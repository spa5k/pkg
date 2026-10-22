//! `pkg tap` commands: add, list, update, remove.
//!
//! `add` and `update` require consent before Nix discovery, directory creation,
//! fetching, or Ruby execution. They stage an import and validate its generated
//! flake with the client decoder before publishing it with an atomic rename.
//!
//! `add` prepares the registry replacement before publication and renames it
//! afterward. A registry failure triggers a publication rollback attempt.
//! A crash between the two renames can leave an active generation without
//! a registry entry.
//!
//! Registry changes take the global lock before the per-source publication lock.
//! `list` and `remove` use saved state. `remove` needs only the registry, so a
//! broken catalog cannot block removal. Tap commands do not change packages
//! in the native profile.

use crate::cli::{Cli, TapCommand};
use crate::nix::OFFICIAL_CASK_SOURCE;
use crate::tap::{consent, registry, store};

mod staging;
use staging::{StagedImport, stage_import_with_setup};

use crate::commands::{CommandError, Session, print_json, session};

/// Validate one requested revision pin, when present.
///
/// A pin is exactly one bare 40-hex commit id. A qualified reference that
/// merely contains a commit (`github:owner/repo?rev=<commit>`) is refused:
/// it is a reference, not a revision.
fn validate_revision(revision: Option<&str>) -> Result<Option<String>, CommandError> {
    match revision {
        None => Ok(None),
        Some(value) if crate::nix::is_full_commit_id(value) => Ok(Some(value.to_string())),
        Some(value) => Err(format!(
            "`{value}` is not a commit. Use the full commit ID: 40 characters, 0-9 and a-f."
        )
        .into()),
    }
}

/// Resolve and validate one user-supplied source.
///
/// The canonical form is `owner/tap`; explicit GitHub tap URLs are
/// accepted and canonicalized. The reserved official source is refused.
fn canonical_source(source: &str) -> Result<String, CommandError> {
    let canonical = cask_catalog::tap::canonicalize_source(source)?;
    if canonical == OFFICIAL_CASK_SOURCE {
        return Err(format!(
            "`{canonical}` is the reserved official source; it is built in \
             and cannot be added as a tap"
        )
        .into());
    }
    Ok(canonical)
}

/// Resolve the update target set from the registry alone.
///
/// A broken saved catalog must not prevent repair with `pkg tap update`,
/// so targets never load saved catalogs.
fn targets<'a>(
    registry: &'a registry::Registry,
    named: Option<&str>,
) -> Result<Vec<(&'a str, &'a str)>, CommandError> {
    match named {
        Some(name) => {
            let source = canonical_source(name)?;
            let Some((source, entry)) = registry.sources.get_key_value(&source) else {
                return Err(format!(
                    "`{source}` is not a saved tap. Add it with `pkg tap add {source}`."
                )
                .into());
            };
            Ok(vec![(source, entry.origin.as_str())])
        }
        None => Ok(registry
            .sources
            .iter()
            .map(|(source, entry)| (source.as_str(), entry.origin.as_str()))
            .collect()),
    }
}

/// `pkg tap add SOURCE [--revision SHA] [--trust]`
pub(super) fn add(
    cli: &Cli,
    source: &str,
    revision: Option<&str>,
    trusted: bool,
) -> Result<(), CommandError> {
    let revision = validate_revision(revision)?;
    let source = canonical_source(source)?;
    let origin = cask_catalog::tap::source_origin(&source)?;

    // Paths and the strict registry only: no Nix discovery, no created
    // directory, no fetch, and no Ruby before consent.
    let state_home = crate::config::paths()?.state_home;
    let registry_path = registry::registry_path(&state_home);
    let existing = registry::Registry::load(&registry_path)?;
    // Keep explicit approval separate from saved consent. A concurrent remove
    // can delete saved consent before this command takes the registry lock.
    let mut approved_now = trusted;
    if let Some(recorded) = existing.origin(&source) {
        if recorded != origin {
            return Err(format!(
                "tap `{source}` is registered with origin {recorded} but the \
                 repository now resolves to {origin}; remove the tap and add it \
                 again to approve the new origin"
            )
            .into());
        }
        // Recorded consent covers this source and its updates.
    } else {
        match consent::ask(&source, &origin, trusted) {
            consent::Decision::Approved => approved_now = true,
            consent::Decision::Refused => {
                return Err(String::from("the tap was not approved. Nothing changed.").into());
            }
            consent::Decision::NoTerminal => {
                return Err(format!(
                    "the tap runs code from {origin}, so it needs your approval. \
                     Run this command in a terminal, or add `--trust`."
                )
                .into());
            }
        }
    }

    // Registry mutations serialize on the one global lock; consent is
    // re-read under it. Lock order is fixed: registry first, then source.
    let global = registry::GlobalLock::acquire(&state_home)?;
    let mut registry = registry::Registry::load(&registry_path)?;
    recheck_consent(&registry, &source, &origin, approved_now)?;
    // Flush the prepared registry before publishing. Commit it with one rename.
    let prepared = if registry.origin(&source).is_none() {
        registry.approve(&source, &origin, now_unix());
        Some(registry.prepare(&registry_path)?)
    } else {
        None
    };

    // Only now do the runtime and the importer run.
    let session = session(cli)?;
    let (staged_generation, staged) =
        stage_import_with_setup(&session, &source, &origin, revision.as_deref(), now_unix())?;
    let mut publication = staged_generation.publish(&staged.provenance, &staged.index)?;

    // Roll back publication if the registry rename fails. Report rollback
    // failures with the known state. A crash between the renames is not covered.
    if let Some(prepared) = prepared
        && let Err(error) = registry::commit_prepared(prepared, &registry_path)
    {
        return Err(undo_publication_after_registry_failure(
            &source,
            publication,
            &error,
        ));
    }
    // Remove this run's fetch files after both commits. Cleanup failures warn
    // and leave the preserved old flake intact.
    for warning in publication.finish() {
        eprintln!("pkg: warning: {warning}");
    }
    drop(global);
    report_publication(&source, &staged, publication.kind(), cli.verbose);
    Ok(())
}

/// Re-read consent under the global registry lock.
///
/// Require saved consent or this invocation's explicit approval before running
/// Nix, importing, or publishing. A concurrent `remove` can revoke saved consent.
fn recheck_consent(
    registry: &registry::Registry,
    source: &str,
    origin: &str,
    approved_now: bool,
) -> Result<(), CommandError> {
    match registry.origin(source) {
        Some(recorded) if recorded != origin => Err(format!(
            "tap `{source}` is registered with origin {recorded} but the \
             repository now resolves to {origin}; remove the tap and add it \
             again to approve the new origin"
        )
        .into()),
        None if !approved_now => Err(format!(
            "tap `{source}` was removed while this add waited for the registry \
             lock, so its recorded approval is gone; approve it again with \
             `pkg tap add {source}`"
        )
        .into()),
        _ => Ok(()),
    }
}

/// Attempt publication rollback after a registry commit failure and report the result.
///
/// A failed withdrawal may leave the new generation active. A failed restore
/// leaves the old generation at its immutable path. Report these states separately.
fn undo_publication_after_registry_failure(
    source: &str,
    publication: store::Publication,
    registry_error: &str,
) -> CommandError {
    let published = publication.kind().clone();
    match &published {
        store::Published::First => match publication.undo() {
            Ok(()) => format!(
                "cannot record consent for {source}; the first publication was \
                 undone: {registry_error}"
            )
            .into(),
            Err(undo) => format!(
                "cannot record consent for {source}, and undoing the first \
                 publication failed: {undo}; the registry does not hold the \
                 approval and the generation may still be active"
            )
            .into(),
        },
        store::Published::Replaced { saved_generation } => match publication.undo() {
            Ok(()) => format!(
                "cannot record consent for {source}; the previous generation \
                 was restored: {registry_error}"
            )
            .into(),
            Err(undo) => format!(
                "cannot record consent for {source}, and restoring the \
                 previous generation failed: {undo}; the previous generation \
                 stays preserved at {} and the withdrawn generation may \
                 still be active",
                saved_generation.display()
            )
            .into(),
        },
    }
}

/// `pkg tap update [SOURCE] [--revision SHA]`
pub(super) fn update(
    cli: &Cli,
    source: Option<&str>,
    revision: Option<&str>,
) -> Result<(), CommandError> {
    let revision = validate_revision(revision)?;
    if revision.is_some() && source.is_none() {
        return Err(String::from(
            "name one tap to use a revision: `pkg tap update OWNER/TAP --revision SHA`",
        )
        .into());
    }
    let session = session(cli)?;
    // Targets come from the registry alone: a broken saved catalog must
    // not prevent repair with this command.
    let registry_path = registry::registry_path(&session.paths.state_home);
    // The global lock blocks concurrent remove or add runs over the same
    // registry for the whole update; recorded consent is re-read under it.
    let global = registry::GlobalLock::acquire(&session.paths.state_home)?;
    let registry = registry::Registry::load(&registry_path)?;
    let targets = targets(&registry, source)?;
    if targets.is_empty() {
        println!("No saved taps to update.\nNext: pkg tap add OWNER/TAP");
        return Ok(());
    }
    // The global lock covers this registry snapshot and all publications.
    for (source, origin) in targets {
        let (staged_generation, staged) =
            stage_import_with_setup(&session, source, origin, revision.as_deref(), now_unix())?;
        let mut publication = staged_generation.publish(&staged.provenance, &staged.index)?;
        // An update has no registry write: finish the publication by
        // clearing this run's fetch scratch best effort.
        for warning in publication.finish() {
            eprintln!("pkg: warning: {warning}");
        }
        report_publication(source, &staged, publication.kind(), cli.verbose);
    }
    drop(global);
    println!("Installed packages did not change.");
    Ok(())
}

/// Report one published generation with its exact revision.
fn report_publication(
    source: &str,
    staged: &StagedImport,
    published: &store::Published,
    verbose: bool,
) {
    let action = match published {
        store::Published::First => "Added",
        store::Published::Replaced { .. } => "Updated",
    };
    println!(
        "{action} tap: {source} ({} packages available).",
        staged.result.eligible
    );
    if verbose {
        eprintln!(
            "pkg: revision {}; {} eligible, {} excluded",
            staged.result.revision, staged.result.eligible, staged.result.excluded
        );
    }
}

/// `pkg tap list [--json]`
///
/// A malformed registry fails the command. Report saved catalog errors per
/// source. `remove` still works, and `pkg tap update SOURCE` can repair the catalog.
pub(super) fn list(cli: &Cli) -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    let rows = super::query::rows(&paths.state_home)?;
    if cli.json {
        print_json(&crate::output::envelope("tap-list", &rows))?;
    } else if rows.is_empty() {
        println!("No saved taps.\nNext: pkg tap add OWNER/TAP");
    } else {
        let width = rows
            .iter()
            .map(|row| row.source.len())
            .max()
            .unwrap_or(6)
            .max(6);
        println!("{:<width$}  PACKAGES  STATUS", "SOURCE");
        for row in &rows {
            match (row.eligible, row.excluded) {
                (Some(eligible), Some(_excluded)) => {
                    println!("{:<width$}  {}  ready", row.source, eligible);
                }
                _ => println!("{:<width$}  unavailable  needs update", row.source),
            }
            if cli.verbose {
                if let Some(error) = &row.error {
                    eprintln!("pkg: {}: {error}", row.source);
                }
            } else if row.error.is_some() {
                eprintln!("Next: pkg tap update {}", row.source);
            }
        }
    }
    Ok(())
}

/// `pkg tap remove SOURCE`
///
/// Registry first: the moment the entry is gone, future source use is
/// disabled. The active generation is then removed so native upgrades that
/// would follow the tap are blocked; installed packages, native profile
/// generations, and the immutable tap generations are deliberately kept,
/// so rollback keeps working.
pub(super) fn remove(_cli: &Cli, source: &str) -> Result<(), CommandError> {
    let source = canonical_source(source)?;
    let paths = crate::config::paths()?;
    let path = registry::registry_path(&paths.state_home);
    // The global lock blocks concurrent add or update runs over the same
    // registry for the whole removal.
    let _global = registry::GlobalLock::acquire(&paths.state_home)?;
    let mut registry = registry::Registry::load(&path)?;
    if !registry.remove(&source) {
        return Err(
            format!("`{source}` is not a saved tap. Run `pkg tap list` to see the taps.").into(),
        );
    }
    registry.save(&path)?;
    store::remove_current(&paths.state_home, &source).map_err(|error| {
        format!(
            "Partial: tap {source} was unregistered, but its active catalog could not be removed: {error}"
        )
    })?;
    println!("Removed tap: {source}. Installed packages stayed installed.");
    Ok(())
}

/// Dispatch one `pkg tap` subcommand.
pub fn dispatch(cli: &Cli, command: &TapCommand) -> Result<(), CommandError> {
    match command {
        TapCommand::Add {
            source,
            revision,
            trust,
        } => add(cli, source, revision.as_deref(), *trust),
        TapCommand::List => list(cli),
        TapCommand::Update { source, revision } => {
            update(cli, source.as_deref(), revision.as_deref())
        }
        TapCommand::Remove { source } => remove(cli, source),
    }
}

/// Wall-clock Unix seconds for publication timestamps.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// Refresh registered sources without reading possibly damaged saved catalogs.
pub fn refresh_registered(cli: &Cli, state_home: &std::path::Path) -> Result<bool, CommandError> {
    let registry = registry::Registry::load(&registry::registry_path(state_home))?;
    let mut failed = false;
    for source in registry.sources.keys() {
        match update(cli, Some(source), None) {
            Ok(()) => {}
            Err(error) if error.is_interrupted() || matches!(error, CommandError::Reported(_)) => {
                return Err(error);
            }
            Err(error) => {
                failed = true;
                eprintln!("Failed: {source}: {error}");
            }
        }
    }
    Ok(failed)
}
