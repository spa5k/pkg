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

use super::{
    CommandError, Session, apps_refresh_after_mutation, configured_sources, installed_entries,
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
    let mut installables = Vec::new();
    for parsed in parsed_ids {
        let resolved = match parsed {
            ParsedId::Qualified(qualified) => qualified,
            ParsedId::Bare(name) => {
                catalog::resolve_bare(&session.nix, &session.config.sources, &name, &system)?
            }
        };
        if let catalog::CatalogId::Cask(token) = &resolved {
            gate_cask(&session, &system, token).map_err(CommandError::Message)?;
        }
        installables.push(resolved.installable(&session.config.sources));
    }
    session
        .nix
        .profile_add(&session.paths.profile, &installables)
        .map_err(|error| mutation_failed(&session, "install", &error))?;
    let entries = installed_entries(&session)?;
    report_entries(&entries, installables.len());
    apps_refresh_after_mutation(&session)
}

/// Refuse cask installs that the platform or the support manifest excludes.
///
/// Returns `Err(message)` when the token must not be installed on `system`.
fn gate_cask(session: &Session, system: &str, token: &str) -> Result<(), String> {
    if !catalog::casks_supported_on(system) {
        return Err(format!(
            "cask:{token} requires macOS ({}); this system is {system}",
            catalog::CASK_SYSTEM
        ));
    }
    match catalog::cask_status(&session.nix, &session.config.sources.casks, system, token) {
        Ok(lookup) => match lookup.status {
            catalog::CaskStatus::Supported { .. } => Ok(()),
            catalog::CaskStatus::Excluded {
                version,
                reason,
                detail,
                ..
            } => Err(format!(
                "cask:{token} {} is excluded on {system}: {reason}: {}",
                version.as_deref().unwrap_or("unknown version"),
                detail.as_deref().unwrap_or("no detail")
            )),
            catalog::CaskStatus::Unlisted { .. } => Err(format!(
                "cask:{token} is not in the cask support manifest for {system}"
            )),
        },
        Err(error) => Err(error.to_string()),
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
    let system = session.nix.system()?;
    // Refresh metadata natively (--refresh) so TTL-cached metadata is not
    // reported as current, then drop the discovery cache.
    for (kind, source) in configured_sources(&session.config) {
        // The cask source is macOS-only; on other systems it is skipped,
        // not fetched or failed.
        if kind == catalog::SourceKind::Cask && !catalog::casks_supported_on(&system) {
            println!(
                "{source} -> skipped (cask source supports {} only)",
                catalog::CASK_SYSTEM
            );
            continue;
        }
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
        "Discovery cache refreshed ({removed} cached result set{} dropped).",
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
    // With --all, eligibility stays with Nix: fixed references re-lock to the
    // same commit and path sources re-evaluate natively.
    let target = if all { None } else { Some(movable.as_slice()) };
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
