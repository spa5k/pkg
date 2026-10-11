//! Lifecycle commands: install, remove, update, upgrade, history,
//! rollback, prune.
//!
//! These commands mutate or read the native profile. Every mutation goes
//! through one native operation, reports the re-read profile afterwards,
//! and refreshes derived launchers on success.

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::catalog::{self, ParsedId};
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::tap::{registry, store};

use super::{
    CommandError, apps_refresh_after_mutation, installed_entries, mutation_failed, session,
};

pub(super) fn install(cli: &Cli, ids: &[String]) -> Result<(), CommandError> {
    if ids.is_empty() {
        return Err(String::from("install needs at least one ID").into());
    }
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let mut parsed_ids = Vec::new();
    for id in ids {
        parsed_ids.push(catalog::parse_id(id)?);
    }
    let session = session(cli)?;
    let system = session.nix.system()?;
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
    let tap_state = if only_formulae {
        super::TapState::official(&session)
    } else {
        super::TapState::load(&session)?
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
                    &system,
                    &mut catalog_handle,
                )
                .map_err(|error| {
                    super::query::install_resolution_error(&session, &name, None, error)
                })?
            }
            ParsedId::BareCask(token) => {
                let saved = tap_state.saved_all()?;
                catalog::resolve_bare_cask(
                    &session.nix,
                    &saved,
                    &token,
                    &system,
                    &mut catalog_handle,
                )
                .map_err(|error| {
                    super::query::install_resolution_error(&session, &token, Some("cask"), error)
                })?
            }
        };
        if let (catalog::CatalogId::Formula(name), Some(view)) = (&resolved, &formula_view) {
            let canonical = view.resolve(&system, name)?;
            view.availability(&catalog::CaskHost::default(), &system, &canonical)
                .require()
                .map_err(|reason| format!("brew:homebrew/core/{canonical}: {reason}"))?;
            resolved = catalog::CatalogId::Formula(canonical);
        }
        if let catalog::CatalogId::Cask { source, token } = &resolved {
            gate_cask(&mut catalog_handle, &tap_state, &system, source, token)
                .map_err(CommandError::Message)?;
        }
        let installable = resolved
            .installable(&tap_state.routing, &system)
            .map_err(CommandError::Message)?;
        resolved_names.insert(installable.clone(), resolved.qualified());
        installables.push(installable);
    }
    let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)?;
    let before = installed_entries(&session)?;
    let mut selected = std::collections::BTreeSet::new();
    installables.retain(|installable| selected.insert(installable.clone()));
    let mut pending = Vec::new();
    for installable in installables {
        if already_installed(&session.nix, &before, &installable, &system)? {
            println!("Already installed: {}", resolved_names[&installable]);
        } else {
            pending.push(installable);
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    eprintln!("Installing: {}", ids.join(", "));
    session
        .nix
        .profile_add(&session.paths.profile, &pending)
        .map_err(|error| {
            let requested: Vec<_> = pending
                .iter()
                .map(|installable| resolved_names[installable].clone())
                .collect();
            mutation_failed(&session, "install", &error, Some(&before), Some(&requested))
        })?;
    let after = installed_entries(&session)?;
    let names = changed_names(&before, &after);
    apps_refresh_after_mutation(&session, "install", &names)?;
    report_names("Installed", &names);
    Ok(())
}

fn already_installed(
    nix: &nix::Nix,
    installed: &BTreeMap<String, ProfileEntry>,
    installable: &str,
    system: &str,
) -> Result<bool, nix::NixError> {
    let Some((reference, attribute)) = installable.split_once('#') else {
        return Ok(false);
    };
    let attribute = attribute
        .split_once('^')
        .map_or(attribute, |(base, _)| base)
        .replace('"', "");
    let candidates: Vec<_> = installed
        .values()
        .filter(|entry| {
            entry.active
                && entry.original_url == reference
                && (entry.attr_path == attribute
                    || catalog::exposed_attribute(&entry.attr_path, system) == attribute)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(false);
    }
    let selected = installable
        .split_once('^')
        .map_or(installable, |(base, _)| base);
    let exact = nix.search(selected, ".")?;
    if exact.len() != 1 {
        return Ok(false);
    }
    let mut paths = nix.default_output_paths(installable)?;
    paths.sort();
    Ok(candidates.iter().any(|entry| {
        let mut existing = entry.store_paths.clone();
        existing.sort();
        exact.contains_key(&entry.attr_path) && existing == paths
    }))
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
    state: &super::TapState,
    system: &str,
    source: &str,
    token: &str,
) -> Result<(), String> {
    let saved_view;
    let view = if source == nix::OFFICIAL_CASK_SOURCE {
        catalog.load().map_err(|error| error.to_string())?
    } else {
        // The gate reads exactly the source it judges: an unrelated
        // broken tap cannot block this install.
        let saved = state.saved_one(source)?;
        saved_view = catalog::CatalogView::from_saved(&saved);
        &saved_view
    };
    gate_view(view, system, source, token)
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

/// The short display name: the last path segment of the native name.
fn short_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_string()
}

/// Report one completed transaction: one line, only what changed.
fn report_names(verb: &str, names: &[String]) {
    match names {
        [] => println!("Unchanged: profile did not change."),
        [one] => println!("{verb}: {one}"),
        many => println!("{verb}: {}", many.join(", ")),
    }
}

/// The names of entries that were added or changed in the second profile.
fn changed_names(
    before: &BTreeMap<String, ProfileEntry>,
    after: &BTreeMap<String, ProfileEntry>,
) -> Vec<String> {
    after
        .keys()
        .filter(|id| before.get(*id) != after.get(*id))
        .map(|id| short_name(&after[id].name()))
        .collect()
}

pub(super) fn remove(cli: &Cli, entries: &[String], exact_entry: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    if entries.is_empty() {
        return Err(String::from("remove needs at least one installed entry ID").into());
    }
    let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)?;
    let installed = installed_entries(&session)?;
    let entries = crate::installed::select(
        &installed,
        &session.config,
        &session.paths.state_home,
        entries,
        exact_entry,
    )?;
    eprintln!("Removing: {}", entries.join(", "));
    session
        .nix
        .profile_remove(&session.paths.profile, &entries)
        .map_err(|error| mutation_failed(&session, "remove", &error, Some(&installed), None))?;
    let after = installed_entries(&session)?;
    let names: Vec<String> = entries
        .iter()
        .filter(|entry| !after.contains_key(*entry))
        .map(|entry| short_name(&installed[entry].name()))
        .collect();
    apps_refresh_after_mutation(&session, "remove", &names)?;
    report_names("Removed", &names);
    Ok(())
}

pub(super) fn upgrade(
    cli: &Cli,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<(), CommandError> {
    let session = session(cli)?;
    let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)?;
    let installed = installed_entries(&session)?;
    // `--all` combined with entry names is a usage error rejected by the
    // parser; here the target set is either explicit names or everything.
    let targets: Vec<String> = if all {
        installed
            .iter()
            .filter(|(_, entry)| entry.active)
            .map(|(key, _)| key.clone())
            .collect()
    } else {
        if entries.is_empty() {
            return Err(String::from("upgrade needs entry IDs or --all").into());
        }
        crate::installed::select(
            &installed,
            &session.config,
            &session.paths.state_home,
            entries,
            exact_entry,
        )?
    };
    if all && targets.is_empty() {
        println!("No installed entries; nothing to upgrade.");
        return Ok(());
    }
    // Entries installed from a tap that has since been removed keep their
    // locked outputs; an upgrade must never follow a reference the registry
    // no longer holds. The local public source of an entry is identified
    // solely from its original URL through the tap store - the attribute
    // path is never parsed - so this works even when saved catalogs or the
    // profile metadata are broken, and it works for a removed entry too.
    // Only the registry is loaded here, never `TapState` or saved
    // catalogs, so unrelated broken metadata cannot block the decision.
    let registry = registry::Registry::load(&registry::registry_path(&session.paths.state_home))?;
    let removed_source = |entry: &str| -> Option<String> {
        let source =
            store::source_of_reference(&session.paths.state_home, &installed[entry].original_url)?;
        (source != nix::OFFICIAL_CASK_SOURCE && registry.origin(&source).is_none())
            .then_some(source)
    };
    let targets = if !all {
        // An explicitly named entry from a removed tap is a hard refusal
        // before any Nix upgrade runs.
        for entry in &targets {
            if let Some(source) = removed_source(entry) {
                return Err(format!(
                    "pkg: {entry}: tap {source} is removed; upgrade refuses to follow it"
                )
                .into());
            }
        }
        targets
    } else {
        let mut remaining = targets;
        remaining.retain(|entry| match removed_source(entry) {
            None => true,
            Some(source) => {
                println!("Skipped: {entry} (tap {source} was removed; keeping locked outputs).");
                false
            }
        });
        if remaining.is_empty() {
            println!("Nothing eligible to upgrade.");
            return Ok(());
        }
        remaining
    };
    // Only explicit full-commit references count as fixed; tags, branches,
    // and hex-looking branch names stay movable (design D4). Fixed entries
    // are reported explicitly in both modes.
    let mut movable: Vec<String> = Vec::new();
    for entry in &targets {
        if nix::is_fixed_reference(installed[entry].original_url.as_str()) {
            println!("Skipped: {entry} (fixed source reference).");
        } else {
            movable.push(entry.clone());
        }
    }
    if !all && movable.is_empty() {
        println!("Unchanged: selected entries use fixed source references.");
        return Ok(());
    }
    if movable.is_empty() {
        println!("Unchanged: no movable entries to upgrade.");
        return Ok(());
    }
    // The remaining IDs are passed explicitly so fixed sources and removed
    // taps are never followed.
    //
    // Each movable CASK entry is re-gated against its OWN current
    // catalog record before `profile_upgrade`: the installed source is
    // the entry's original URL — saved taps through the verified tap
    // store, every other casks source loaded directly from that URL —
    // and the cask identity is confirmed as the whole native attribute
    // through `catalog::native_package_attribute`. Today's configured
    // casks source never judges an entry installed from another one;
    // Nixpkgs entries are never gated as casks. Unsupported, excluded,
    // and unknown targets are gated with the install policy.
    gate_upgrade_formulae(&session, &installed, &movable)?;
    gate_upgrade_macos_range(&session, &installed, &movable).map_err(CommandError::Message)?;
    eprintln!("Upgrading: {}", movable.join(", "));
    session
        .nix
        .profile_upgrade(&session.paths.profile, Some(&movable))
        .map_err(|error| mutation_failed(&session, "upgrade", &error, Some(&installed), None))?;
    let after = installed_entries(&session)?;
    let names: Vec<String> = movable
        .iter()
        .filter(|entry| after.get(*entry) != installed.get(*entry))
        .map(|entry| short_name(&installed[entry].name()))
        .collect();
    apps_refresh_after_mutation(&session, "upgrade", &names)?;
    report_names("Upgraded", &names);
    Ok(())
}

fn gate_upgrade_formulae(
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
fn gate_upgrade_macos_range(
    session: &super::Session,
    installed: &BTreeMap<String, ProfileEntry>,
    movable: &[String],
) -> Result<(), String> {
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
    let host_system = session.nix.system().map_err(|error| error.to_string())?;
    let mut tap_state: Option<super::TapState> = None;
    // One index load per distinct original source, cached for the run.
    let mut views: BTreeMap<String, catalog::CatalogView> = BTreeMap::new();
    for (entry, system, source, token) in casks {
        if system != host_system {
            return Err(format!(
                "cask:{source}/{token} was installed for {system}; this host is {host_system}"
            ));
        }
        let original = installed[entry].original_url.clone();
        let saved_source = store::source_of_reference(&session.paths.state_home, &original);
        let view = if saved_source.as_deref() == Some(source.as_str()) {
            // The saved tap reads exactly its selected saved generation.
            // The tap state loads lazily, once, on the first saved-tap
            // entry; the match keeps that invariant local: the loaded
            // state is used and cached in the same arm, with no
            // impossible state left to assert.
            let saved = match tap_state.as_ref() {
                Some(state) => state.saved_one(&source)?,
                None => {
                    let state = super::TapState::load(session).map_err(|error| match error {
                        super::CommandError::Message(message) => message,
                        super::CommandError::Reported(_) => {
                            String::from("tap state could not be loaded for the upgrade gate")
                        }
                    })?;
                    let saved = state.saved_one(&source)?;
                    tap_state = Some(state);
                    saved
                }
            };
            views.insert(source.clone(), catalog::CatalogView::from_saved(&saved));
            &views[&source]
        } else {
            // Any other casks source is judged by the index of its own
            // original URL, however `sources.casks` is configured today.
            let key = catalog::canonical_source(&original);
            if !views.contains_key(&key) {
                let view = catalog::CatalogView::load(&session.nix, &original)
                    .map_err(|error| error.to_string())?;
                views.insert(key.clone(), view);
            }
            &views[&key]
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
    Some((system, source, token))
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
fn active_generation(profile: &std::path::Path) -> Option<u64> {
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
    let session = session(cli)?;
    let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)?;
    let before = installed_entries(&session)?;
    let previous_generation = active_generation(&session.paths.profile);
    match generation {
        Some(number) => eprintln!("Restoring: generation {number}"),
        None => {
            let destination = session
                .nix
                .profile_history(&session.paths.profile)
                .ok()
                .and_then(|text| {
                    let current = previous_generation?;
                    history_numbers(&text)
                        .into_iter()
                        .filter(|number| *number < current)
                        .max()
                });
            if let Some(number) = destination {
                eprintln!("Restoring: generation {number} (native previous generation)");
            } else {
                eprintln!("Restoring: native previous generation");
            }
        }
    }
    if let Err(error) = session
        .nix
        .profile_rollback(&session.paths.profile, generation)
    {
        let active = active_generation(&session.paths.profile);
        if let Some(number) = active.filter(|_| active != previous_generation) {
            eprintln!("Partial: generation {number} is active. App launchers may need an update.");
            if !matches!(&error, nix::NixError::Interrupted { .. }) {
                super::report_cause(&error.to_string());
            }
            if cfg!(target_os = "macos") {
                eprintln!("Next: run `pkg apps sync`.");
            } else {
                eprintln!("Next: run `pkg history` to inspect generations.");
            }
            return Err(CommandError::Reported(ExitCode::from(
                if matches!(&error, nix::NixError::Interrupted { .. }) {
                    super::INTERRUPTED
                } else {
                    super::FAILURE
                },
            )));
        }
        return Err(mutation_failed(
            &session,
            "rollback",
            &error,
            Some(&before),
            None,
        ));
    }
    let current_generation = active_generation(&session.paths.profile);
    let destination = match current_generation.or(generation) {
        Some(number) => format!("generation {number}"),
        None => String::from("the previous generation"),
    };
    let affected = if current_generation.is_some() && current_generation == previous_generation {
        Vec::new()
    } else {
        vec![destination.clone()]
    };
    apps_refresh_after_mutation(&session, "rollback", &affected)?;
    if current_generation.is_some() && current_generation == previous_generation {
        println!("Unchanged: {destination} is still active.");
    } else {
        println!("Restored: {destination}.");
    }
    Ok(())
}

pub(super) fn prune(cli: &Cli, older_than: &str) -> Result<(), CommandError> {
    // The age is validated before the runtime is touched.
    let days = crate::cli::parse_prune_age(older_than)?;
    let session = session(cli)?;
    let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)?;
    let before = session
        .nix
        .profile_history(&session.paths.profile)
        .ok()
        .map(|text| history_numbers(&text));
    eprintln!("Pruning: generations older than {days} days");
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| mutation_failed(&session, "prune", &error, None, None))?;
    // Eligibility stays with Nix: which generations are old enough is native
    // behavior and is not assumed by pkg.
    let after = session
        .nix
        .profile_history(&session.paths.profile)
        .ok()
        .map(|text| history_numbers(&text));
    match (before, after) {
        (Some(before), Some(after)) if !before.is_empty() && !after.is_empty() => {
            let removed = before.difference(&after).count();
            if removed == 0 {
                println!("No old generations to prune.");
            } else {
                println!("Pruned: {removed} old generation(s). Current generation kept.");
            }
        }
        _ => println!("Prune complete: generations older than {days} days were checked."),
    }
    Ok(())
}

/// Parse only Nix's `Version N (DATE)` headers.
fn history_numbers(text: &str) -> std::collections::BTreeSet<u64> {
    crate::output::strip_ansi(text)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Version ")?
                .split_once(" (")?
                .0
                .parse()
                .ok()
        })
        .collect()
}
