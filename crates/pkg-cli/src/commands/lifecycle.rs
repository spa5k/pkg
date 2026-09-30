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
    CommandError, apps_refresh_after_mutation, configured_sources, installed_entries,
    mutation_failed, session,
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
    let tap_state = super::TapState::load(&session)?;
    // One index evaluation is shared by every bare resolution and every
    // cask gate in this command run.
    let mut catalog_handle = catalog::CatalogOnce::new(&session.nix, &session.config.sources.casks);
    let mut installables = Vec::new();
    for parsed in parsed_ids {
        // Qualified IDs resolve without reading any saved tap: an
        // unrelated broken tap cannot block a request that names its
        // source. Bare names load every source, because bare
        // resolution must stay strict across all of them.
        let resolved = match parsed {
            ParsedId::Qualified(qualified) => qualified,
            ParsedId::Bare(name) => {
                let saved = tap_state.saved_all()?;
                catalog::resolve_bare(
                    &session.nix,
                    &tap_state.routing,
                    &saved,
                    &name,
                    &system,
                    &mut catalog_handle,
                )?
            }
            ParsedId::BareCask(token) => {
                let saved = tap_state.saved_all()?;
                catalog::resolve_bare_cask(
                    &session.nix,
                    &saved,
                    &token,
                    &system,
                    &mut catalog_handle,
                )?
            }
        };
        if let catalog::CatalogId::Cask { source, token } = &resolved {
            gate_cask(&mut catalog_handle, &tap_state, &system, source, token)
                .map_err(CommandError::Message)?;
        }
        installables.push(
            resolved
                .installable(&tap_state.routing, &system)
                .map_err(CommandError::Message)?,
        );
    }
    let before = installed_entries(&session)?;
    eprintln!("Installing: {}", ids.join(", "));
    session
        .nix
        .profile_add(&session.paths.profile, &installables)
        .map_err(|error| mutation_failed(&session, "install", &error, Some(&before)))?;
    let after = installed_entries(&session)?;
    let names = changed_names(&before, &after);
    apps_refresh_after_mutation(&session, "install", &names)?;
    report_names("Installed", &names);
    Ok(())
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
    if !view.targeted(system) {
        return Err(format!(
            "cask:{source}/{token} needs a catalog target system ({}); this system is {system}",
            view.targets().join(", ")
        ));
    }
    let version = view
        .record(system, source, token)
        .and_then(|entry| entry.version.clone())
        .unwrap_or_else(|| String::from("unknown version"));
    match view.entry(system, source, token) {
        catalog::CaskStatus::Eligible => gate_host_macos_range(view, system, source, token),
        catalog::CaskStatus::Excluded { reason, detail } => Err(format!(
            "cask:{source}/{token} {version} is excluded on {system}: {reason}: {}",
            detail.as_deref().unwrap_or("no detail")
        )),
        catalog::CaskStatus::Unknown => Err(format!(
            "cask:{source}/{token} is not in the generated catalog for {system}"
        )),
    }
}

/// Refuse an eligible cask whose declared macOS range excludes the host.
///
/// The bounds come from the same decoded index entry: `minMacos` and
/// `maxMacos` are null on Linux records, so the host version is read only
/// when a cask actually declares a macOS range on this target — never for
/// Linux, Nixpkgs, or an untargeted catalog. The read goes through the
/// absolute `/usr/bin/sw_vers` path and the shared child signal boundary;
/// a failed read fails the install closed. The refusal happens before any
/// `profile_add` mutation.
fn gate_host_macos_range(
    view: &catalog::CatalogView,
    system: &str,
    source: &str,
    token: &str,
) -> Result<(), String> {
    let Some(entry) = view.record(system, source, token) else {
        return Ok(());
    };
    let (min, max) = (&entry.min_macos, &entry.max_macos);
    if min.is_none() && max.is_none() {
        return Ok(());
    }
    // Explicit platform guard: the host read happens only on macOS
    // hosts. Even an imported malformed index that carries macOS fields
    // on a Linux record never triggers `/usr/bin/sw_vers` here.
    if !system.ends_with("-darwin") {
        return Ok(());
    }
    let host = nix::macos_product_version().map_err(|detail| {
        format!(
            "cask:{source}/{token} declares a macOS range, but the host version cannot be read: {detail}"
        )
    })?;
    if !nix::in_range(&host, min.as_deref(), max.as_deref()) {
        let range = match (min.as_deref(), max.as_deref()) {
            (Some(min), Some(max)) => format!("macOS >= {min} and <= {max}"),
            (Some(min), None) => format!("macOS >= {min}"),
            (None, Some(max)) => format!("macOS <= {max}"),
            (None, None) => unreachable!("at least one bound is declared"),
        };
        return Err(format!(
            "cask:{source}/{token} requires {range}; this host runs macOS {host}"
        ));
    }
    Ok(())
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

pub(super) fn remove(cli: &Cli, entries: &[String]) -> Result<(), CommandError> {
    let session = session(cli)?;
    if entries.is_empty() {
        return Err(String::from("remove needs at least one installed entry ID").into());
    }
    let installed = installed_entries(&session)?;
    for entry in entries {
        if !installed.contains_key(entry) {
            return Err(format!(
                "`{entry}` is not an installed entry; run `pkg list` for exact entry IDs"
            )
            .into());
        }
    }
    eprintln!("Removing: {}", entries.join(", "));
    session
        .nix
        .profile_remove(&session.paths.profile, entries)
        .map_err(|error| mutation_failed(&session, "remove", &error, Some(&installed)))?;
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

pub(super) fn update(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    // Refresh metadata natively (--refresh) so TTL-cached metadata is not
    // reported as current, then drop the discovery cache. Both sources are
    // refreshed on every system: source metadata is not per-system, and the
    // catalog decides per-system reach from its own target list.
    eprintln!("Refreshing package search data…");
    for (_kind, source) in configured_sources(&session.config) {
        match session.nix.refresh_source_identity(source) {
            Ok(identity) => {
                let revision = identity
                    .revision
                    .as_deref()
                    .unwrap_or("moving, no revision");
                if cli.verbose {
                    eprintln!("pkg: {source} -> {revision}");
                }
            }
            Err(error) => {
                eprintln!("Failed: Could not refresh package search data.");
                eprintln!("Source: {}", crate::output::short_source(source));
                super::report_cause(&error.to_string());
                eprintln!("Installed packages did not change.");
                return Err(CommandError::Reported(ExitCode::from(super::FAILURE)));
            }
        }
    }
    let removed = catalog::invalidate_cache(&session.paths.cache_dir)?;
    if cli.verbose {
        eprintln!("pkg: removed {removed} discovery cache entries");
    }
    println!("Updated: package search data. Installed packages did not change.");
    Ok(())
}

pub(super) fn upgrade(cli: &Cli, entries: &[String], all: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    let installed = installed_entries(&session)?;
    // `--all` combined with entry names is a usage error rejected by the
    // parser; here the target set is either explicit names or everything.
    let targets: Vec<String> = if all {
        installed.keys().cloned().collect()
    } else {
        if entries.is_empty() {
            return Err(String::from("upgrade needs entry IDs or --all").into());
        }
        for entry in entries {
            if !installed.contains_key(entry) {
                return Err(format!("`{entry}` is not an installed entry; run `pkg list`").into());
            }
        }
        entries.to_vec()
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
    gate_upgrade_macos_range(&session, &installed, &movable).map_err(CommandError::Message)?;
    eprintln!("Upgrading: {}", movable.join(", "));
    session
        .nix
        .profile_upgrade(&session.paths.profile, Some(&movable))
        .map_err(|error| mutation_failed(&session, "upgrade", &error, Some(&installed)))?;
    let after = installed_entries(&session)?;
    let names: Vec<String> = movable
        .iter()
        .filter(|entry| after.get(*entry) != installed.get(*entry))
        .map(|entry| short_name(entry))
        .collect();
    apps_refresh_after_mutation(&session, "upgrade", &names)?;
    report_names("Upgraded", &names);
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
            if tap_state.is_none() {
                tap_state = Some(super::TapState::load(session).map_err(|error| match error {
                    super::CommandError::Message(message) => message,
                    super::CommandError::Reported(_) => {
                        String::from("tap state could not be loaded for the upgrade gate")
                    }
                })?);
            }
            let saved = tap_state
                .as_ref()
                .expect("tap state was loaded above")
                .saved_one(&source)?;
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
    let before = installed_entries(&session)?;
    let previous_generation = active_generation(&session.paths.profile);
    match generation {
        Some(number) => eprintln!("Restoring: generation {number}"),
        None => eprintln!("Restoring: previous generation"),
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
        return Err(mutation_failed(&session, "rollback", &error, Some(&before)));
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
    let before = session
        .nix
        .profile_history(&session.paths.profile)
        .ok()
        .map(|text| history_numbers(&text));
    eprintln!("Pruning: generations older than {days} days");
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| mutation_failed(&session, "prune", &error, None))?;
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
