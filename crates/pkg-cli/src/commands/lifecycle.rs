//! Lifecycle commands: install, remove, update, upgrade, history,
//! rollback, prune.
//!
//! These handlers resolve input and read profile history. Package changes
//! use the shared change owner for locking, native mutation, postflight,
//! launcher refresh, and reporting.

use std::collections::BTreeMap;

use crate::catalog::{self, ParsedId};
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::tap::store;

use super::{CommandError, session};

pub(super) fn install(cli: &Cli, ids: &[String]) -> Result<(), CommandError> {
    if ids.is_empty() {
        return Err(String::from("install needs at least one ID").into());
    }
    let parsed_ids = ids
        .iter()
        .map(|id| catalog::parse_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    eprintln!("Resolving: {}", ids.join(", "));
    let session = session(cli)?;
    let system = session.nix.system()?;
    let (installables, resolved_names) = resolve_installables(&session, parsed_ids, &system)?;
    super::change::execute(
        &session,
        super::change::Request::Install {
            installables,
            resolved_names,
            system: &system,
        },
    )
}

fn resolve_installables(
    session: &super::Session,
    parsed_ids: Vec<ParsedId>,
    system: &str,
) -> Result<(Vec<String>, BTreeMap<String, String>), CommandError> {
    let only_formulae = parsed_ids
        .iter()
        .all(|id| matches!(id, ParsedId::Qualified(catalog::CatalogId::Formula(_))));
    let formula_view = if parsed_ids
        .iter()
        .any(|id| matches!(id, ParsedId::Qualified(catalog::CatalogId::Formula(_))))
    {
        Some(catalog::brew::FormulaView::load(
            &session.nix,
            &session.config.sources.formulae,
        )?)
    } else {
        None
    };
    let mut tap_state = if only_formulae {
        super::TapState::official(session)
    } else {
        super::TapState::load(session)?
    };
    // One index evaluation is shared by every bare resolution and every
    // cask gate in this command run.
    let mut catalog_handle = catalog::CatalogOnce::new(&session.nix, &session.config.sources.casks);
    let mut installables = Vec::new();
    let mut resolved_names = BTreeMap::new();
    for parsed in parsed_ids {
        // Qualified IDs resolve without reading any saved tap: an
        // unrelated broken tap cannot block a request that names its
        // source. Bare names load every source, because bare
        // resolution must stay strict across all of them.
        let mut resolved = match parsed {
            ParsedId::Qualified(qualified) => qualified,
            ParsedId::Bare(name) => {
                let saved = tap_state.saved_all()?;
                catalog::resolve_bare(
                    &session.nix,
                    &session.paths.cache_dir,
                    &tap_state.routing,
                    &saved,
                    &name,
                    system,
                    &mut catalog_handle,
                )
                .map_err(|error| {
                    super::search::install_resolution_error(session, &name, None, error)
                })?
            }
            ParsedId::BareCask(token) => {
                let saved = tap_state.saved_all()?;
                catalog::resolve_bare_cask(
                    &session.nix,
                    &saved,
                    &token,
                    system,
                    &mut catalog_handle,
                )
                .map_err(|error| {
                    super::search::install_resolution_error(session, &token, Some("cask"), error)
                })?
            }
        };
        if let (catalog::CatalogId::Formula(name), Some(view)) = (&resolved, &formula_view) {
            let canonical = view.resolve(system, name)?;
            view.availability(&catalog::CaskHost::default(), system, &canonical)
                .require()
                .map_err(|reason| format!("brew:homebrew/core/{canonical}: {reason}"))?;
            resolved = catalog::CatalogId::Formula(canonical);
        }
        if let catalog::CatalogId::Cask { source, token } = &resolved {
            gate_cask(&mut catalog_handle, &mut tap_state, system, source, token)?;
        }
        let installable = resolved
            .installable(&tap_state.routing, system)
            .map_err(CommandError::Message)?;
        resolved_names.insert(installable.clone(), resolved.qualified());
        installables.push(installable);
    }
    Ok((installables, resolved_names))
}

/// Refuse cask installs the generated or saved catalog excludes or does not
/// list.
///
/// The official source reads the generated index: the platform rule is the
/// envelope's target list, eligibility and exclusion reasons are generated
/// data, and an index failure is a failure — never a platform skip. An
/// unknown token never becomes eligible. An imported tap reads its saved
/// capture the same way; no tap Ruby ever runs.
///
/// Returns `Err(message)` when the token must not be installed on `system`.
fn gate_cask(
    catalog: &mut catalog::CatalogOnce<'_>,
    state: &mut super::TapState,
    system: &str,
    source: &str,
    token: &str,
) -> Result<(), CommandError> {
    let saved_view;
    let view = if source == nix::OFFICIAL_CASK_SOURCE {
        catalog.load()?
    } else {
        // The gate reads exactly the source it judges: an unrelated
        // broken tap cannot block this install.
        let saved = state.saved_one(source)?;
        saved_view = catalog::CatalogView::from_saved(saved);
        &saved_view
    };
    gate_view(view, system, source, token).map_err(Into::into)
}

/// Gate one (source, token) against one already-loaded index view.
fn gate_view(
    view: &catalog::CatalogView,
    system: &str,
    source: &str,
    token: &str,
) -> Result<(), String> {
    catalog::CaskHost::default()
        .availability(view, system, source, token)
        .require()
        .map_err(|reason| format!("cask:{source}/{token} {reason}"))
}

pub(super) fn remove(cli: &Cli, entries: &[String], exact_entry: bool) -> Result<(), CommandError> {
    if entries.is_empty() {
        return Err(String::from("remove needs at least one installed entry ID").into());
    }
    super::change::execute(
        &session(cli)?,
        super::change::Request::Remove {
            entries,
            exact_entry,
        },
    )
}

pub(super) fn upgrade(
    cli: &Cli,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<(), CommandError> {
    super::change::execute(
        &session(cli)?,
        super::change::Request::Upgrade {
            entries,
            all,
            exact_entry,
        },
    )
}

pub(super) fn gate_upgrade_formulae(
    session: &super::Session,
    installed: &BTreeMap<String, ProfileEntry>,
    movable: &[String],
) -> Result<(), CommandError> {
    let mut views = BTreeMap::new();
    let host = catalog::CaskHost::default();
    for key in movable {
        let entry = &installed[key];
        let Some((system, source, name)) = cask_identity(&entry.attr_path) else {
            continue;
        };
        if source != "homebrew/core"
            || catalog::canonical_source(&entry.original_url)
                == catalog::canonical_source(&session.config.sources.nixpkgs)
        {
            continue;
        }
        let host_system = session.nix.system()?;
        if system != host_system {
            return Err(format!(
                "brew:homebrew/core/{name} was installed for {system}; this host is {host_system}"
            )
            .into());
        }
        if !views.contains_key(&entry.original_url) {
            views.insert(
                entry.original_url.clone(),
                catalog::brew::FormulaView::load(&session.nix, &entry.original_url)?,
            );
        }
        let view = &views[&entry.original_url];
        view.availability(&host, &system, &name)
            .require()
            .map_err(|reason| format!("brew:homebrew/core/{name}: {reason}"))?;
    }
    Ok(())
}

/// Re-gate every movable cask entry against its own current catalog
/// record before `profile_upgrade`.
///
/// Identity recovery is exact: the whole native attribute path must be
/// `packages.{system}.{encoded full id}` — the final segment decodes
/// through `decode_package_attribute` and the result is confirmed back
/// through `catalog::native_package_attribute` as one whole identity,
/// never by naive attribute text splitting. A Nixpkgs entry whose
/// attribute merely ends in a cask-shaped id is never gated: an entry
/// whose original URL canonicalizes to the configured Nixpkgs source is
/// left to the plain upgrade unless the tap store verifies a saved
/// source for it. The judged index comes from the entry's OWN original
/// URL — the selected saved generation for a verified tap-store source,
/// the index evaluated at the original reference for any other casks
/// source — so an older installed source is judged by its own catalog
/// even after `sources.casks` names a different source today. Native
/// upgrade follows the original URL; this gate judges the same source,
/// never today's configured one. Tap state loads lazily, only when a
/// movable entry is actually a saved tap cask.
pub(super) fn gate_upgrade_macos_range(
    session: &super::Session,
    installed: &BTreeMap<String, ProfileEntry>,
    movable: &[String],
) -> Result<(), CommandError> {
    // Recover exact cask identities first; Nixpkgs-only upgrades never
    // touch tap state or any index.
    let mut casks: Vec<(&str, String, String, String)> = Vec::new();
    for entry in movable {
        let profile = &installed[entry];
        let saved_source =
            store::source_of_reference(&session.paths.state_home, &profile.original_url);
        // A Nixpkgs entry is never a cask however cask-shaped its final
        // attribute segment looks: only a verified tap-store source
        // overrides the configured-source classification.
        if saved_source.is_none()
            && catalog::canonical_source(&profile.original_url)
                == catalog::canonical_source(&session.config.sources.nixpkgs)
        {
            continue;
        }
        let Some((system, source, token)) = cask_identity(&profile.attr_path) else {
            continue;
        };
        // A saved tap is judged only under its exact verified source; an
        // attribute that names a different source is not that cask.
        if saved_source.as_deref().is_some_and(|saved| saved != source) {
            continue;
        }
        if source == "homebrew/core" {
            continue;
        }
        casks.push((entry, system, source, token));
    }
    if casks.is_empty() {
        return Ok(());
    }
    let host_system = session.nix.system()?;
    let mut tap_state: Option<super::TapState> = None;
    // One index load per distinct original source, cached for the run.
    let mut views: BTreeMap<String, catalog::CatalogView> = BTreeMap::new();
    for (entry, system, source, token) in casks {
        if system != host_system {
            return Err(format!(
                "cask:{source}/{token} was installed for {system}; this host is {host_system}"
            )
            .into());
        }
        let original = installed[entry].original_url.clone();
        let saved_source = store::source_of_reference(&session.paths.state_home, &original);
        let view = if saved_source.as_deref() == Some(source.as_str()) {
            // The saved tap reads exactly its selected saved generation.
            // The tap state loads lazily, once, on the first saved-tap
            // entry; the match keeps that invariant local: the loaded
            // state is used and cached in the same arm, with no
            // impossible state left to assert.
            let state = match &mut tap_state {
                Some(state) => state,
                empty @ None => empty.insert(super::TapState::load(session)?),
            };
            let saved = state.saved_one(&source)?;
            views
                .entry(source.clone())
                .or_insert_with(|| catalog::CatalogView::from_saved(saved))
        } else {
            // Any other casks source is judged by the index of its own
            // original URL, however `sources.casks` is configured today.
            let key = catalog::canonical_source(&original);
            match views.entry(key) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(catalog::CatalogView::load(&session.nix, &original)?)
                }
            }
        };
        gate_view(view, &system, &source, &token)?;
    }
    Ok(())
}

/// The exact cask identity of one native attribute path.
///
/// The path must be exactly `packages.{system}.{encoded full id}`: the
/// final segment decodes through the shared strict inverse, the id
/// splits as one whole `owner/tap/token`, and the result is confirmed
/// back through `catalog::native_package_attribute` — so the identity
/// is the whole recorded path, never a naive final-segment split.
fn cask_identity(attr_path: &str) -> Option<(String, String, String)> {
    let mut parts = attr_path.split('.');
    if parts.next()? != "packages" {
        return None;
    }
    let system = parts.next()?.to_string();
    let segment = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let id = cask_catalog::decode_package_attribute(segment).ok()?;
    let (source, token) = nix::split_entry_id(&id)?;
    if catalog::native_package_attribute(&system, &id) != attr_path {
        return None;
    }
    Some((system, source.to_string(), token.to_string()))
}

pub(super) fn history(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    let text = session.nix.profile_history(&session.paths.profile)?;
    // `--no-color` holds for native text pkg passes through, even if
    // the runtime colorized it anyway.
    let text = if session.config.color_disabled(cli.no_color) {
        crate::output::strip_ansi(&text)
    } else {
        text
    };
    if cli.verbose {
        print!("{text}");
    } else {
        print!(
            "{}",
            render_history(&text, active_generation(&session.paths.profile))
        );
    }
    Ok(())
}

/// Read the active generation from the native profile link when available.
pub(super) fn active_generation(profile: &std::path::Path) -> Option<u64> {
    let link = std::fs::read_link(profile).ok()?;
    let name = link.file_name()?.to_str()?;
    name.strip_suffix("-link")?.rsplit('-').next()?.parse().ok()
}

/// Keep generation numbers and dates; hide per-package native history detail.
fn render_history(text: &str, active: Option<u64>) -> String {
    use std::fmt::Write as _;
    let clean = crate::output::strip_ansi(text);
    let rows: Vec<(u64, &str)> = clean
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Version ")?;
            let (number, rest) = rest.split_once(" (")?;
            let (date, _) = rest.split_once(')')?;
            Some((number.parse().ok()?, date))
        })
        .collect();
    if rows.is_empty() {
        return clean;
    }
    let mut out = String::from("GEN  CREATED  STATUS\n");
    for (number, date) in rows.iter().rev() {
        let status = if Some(*number) == active {
            "current"
        } else {
            ""
        };
        let _ = writeln!(out, "{number}  {date}  {status}");
    }
    if rows.len() == 1 {
        out.push_str("No earlier generation to restore.\n");
    }
    out
}

pub(super) fn rollback(cli: &Cli, generation: Option<u64>) -> Result<(), CommandError> {
    super::change::execute(&session(cli)?, super::change::Request::Rollback(generation))
}

pub(super) fn prune(cli: &Cli, older_than: &str) -> Result<(), CommandError> {
    let days = crate::cli::parse_prune_age(older_than)?;
    super::change::execute(&session(cli)?, super::change::Request::Prune { days })
}
