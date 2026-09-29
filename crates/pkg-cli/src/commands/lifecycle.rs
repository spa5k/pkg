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
        let resolved = match parsed {
            ParsedId::Qualified(qualified) => qualified,
            ParsedId::Bare(name) => catalog::resolve_bare(
                &session.nix,
                &tap_state.routing,
                &tap_state.saved,
                &name,
                &system,
                &mut catalog_handle,
            )?,
            ParsedId::BareCask(token) => catalog::resolve_bare_cask(
                &session.nix,
                &tap_state.saved,
                &token,
                &system,
                &mut catalog_handle,
            )?,
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
    session
        .nix
        .profile_add(&session.paths.profile, &installables)
        .map_err(|error| mutation_failed(&session, "install", &error))?;
    let entries = installed_entries(&session)?;
    report_entries(&entries, installables.len());
    apps_refresh_after_mutation(&session)
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
        let saved = state
            .saved
            .get(source)
            .ok_or_else(|| format!("tap source `{source}` has no saved catalog"))?;
        saved_view = catalog::CatalogView::from_saved(saved);
        &saved_view
    };
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
        catalog::CaskStatus::Eligible => Ok(()),
        catalog::CaskStatus::Excluded { reason, detail } => Err(format!(
            "cask:{source}/{token} {version} is excluded on {system}: {reason}: {}",
            detail.as_deref().unwrap_or("no detail")
        )),
        catalog::CaskStatus::Unknown => Err(format!(
            "cask:{source}/{token} is not in the generated catalog for {system}"
        )),
    }
}

fn report_entries(entries: &BTreeMap<String, ProfileEntry>, expected: usize) {
    println!(
        "Installed {expected} entr{} in the pkg profile:",
        if expected == 1 { "y" } else { "ies" }
    );
    for (entry_id, entry) in entries {
        let revision = entry.locked_revision().unwrap_or("no revision");
        println!("  {entry_id}  {}  ({revision})", entry.name());
    }
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
    session
        .nix
        .profile_remove(&session.paths.profile, entries)
        .map_err(|error| mutation_failed(&session, "remove", &error))?;
    println!("Removed {} entries from the pkg profile.", entries.len());
    apps_refresh_after_mutation(&session)
}

pub(super) fn update(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    // Refresh metadata natively (--refresh) so TTL-cached metadata is not
    // reported as current, then drop the discovery cache. Both sources are
    // refreshed on every system: source metadata is not per-system, and the
    // catalog decides per-system reach from its own target list.
    for (_kind, source) in configured_sources(&session.config) {
        match session.nix.refresh_source_identity(source) {
            Ok(identity) => {
                let revision = identity
                    .revision
                    .as_deref()
                    .unwrap_or("moving, no revision");
                println!("{source} -> {revision} (metadata refreshed)");
            }
            Err(error) => {
                eprintln!("pkg: source failure for {source}: {error}");
                return Err(CommandError::Reported(ExitCode::from(super::FAILURE)));
            }
        }
    }
    let removed = catalog::invalidate_cache(&session.paths.cache_dir)?;
    println!(
        "Discovery cache refreshed ({removed} cached catalog{} dropped).",
        if removed == 1 { "" } else { "s" }
    );
    println!("Installed packages and their original references are unchanged.");
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
                    "`{entry}` was installed from tap source {source}, which is no \
                     longer registered; upgrade refuses to follow it. Re-add the tap \
                     (`pkg tap add {source}`) or remove the entry to change it."
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
                println!(
                    "pkg: {entry} was installed from tap source {source}, which is no longer \
                     registered; upgrade keeps its locked outputs. Re-add the tap or remove \
                     the entry to change it."
                );
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
            println!(
                "pkg: {entry} uses an explicit full-commit reference; upgrade does not replace it. \
                 Remove and install the new reference to change it."
            );
        } else {
            movable.push(entry.clone());
        }
    }
    if !all && movable.is_empty() {
        return Err(String::from("nothing to upgrade").into());
    }
    // With `--all`, eligibility stays with Nix for the entries that remain:
    // fixed references re-lock to the same commit and path sources
    // re-evaluate natively. The remaining IDs are passed explicitly so a
    // removed tap's locked outputs are never followed.
    let target = if all {
        Some(targets.as_slice())
    } else {
        Some(movable.as_slice())
    };
    session
        .nix
        .profile_upgrade(&session.paths.profile, target)
        .map_err(|error| mutation_failed(&session, "upgrade", &error))?;
    let entries = installed_entries(&session)?;
    report_entries(&entries, entries.len());
    apps_refresh_after_mutation(&session)
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
    print!("{text}");
    Ok(())
}

pub(super) fn rollback(cli: &Cli, generation: Option<u64>) -> Result<(), CommandError> {
    let session = session(cli)?;
    session
        .nix
        .profile_rollback(&session.paths.profile, generation)
        .map_err(|error| mutation_failed(&session, "rollback", &error))?;
    match generation {
        Some(number) => println!("Rolled back to native generation {number}."),
        None => println!("Rolled back to the previous native generation."),
    }
    apps_refresh_after_mutation(&session)
}

pub(super) fn prune(cli: &Cli, older_than: &str) -> Result<(), CommandError> {
    // The age is validated before the runtime is touched.
    let days = crate::cli::parse_prune_age(older_than)?;
    let session = session(cli)?;
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| mutation_failed(&session, "prune", &error))?;
    // Eligibility stays with Nix: which generations are old enough is native
    // behavior and is not assumed by pkg.
    println!("Requested deletion of non-current generations older than {days} days.");
    println!("Nix decides which generations are eligible; run `pkg history` to verify.");
    Ok(())
}
