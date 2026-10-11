//! `pkg tap` commands: add, list, update, remove.
//!
//! `add` and `update` are the only paths that run tap imports. The order is
//! fixed and fail-safe: consent comes first — before Nix discovery, before
//! any directory is created, before any fetch or Ruby runs — then the
//! importer stages a generation, validation evaluates the generated staging
//! flake with the client decoder, and publication commits the whole active
//! generation with one atomic rename. The registry replacement is prepared
//! *before* publication and committed by one rename after it: publication
//! and registry commit stay two renames, so a registry failure triggers an
//! attempted rollback of the publication, and a process crash between the
//! two renames can leave an active generation without a committed registry
//! entry. All registry
//! mutations serialize on the global registry lock first, then the
//! per-source publication lock. `list` and `remove` touch only the registry
//! and the saved state; `remove` works from the registry alone so a broken
//! catalog never blocks repair. No command here installs or removes
//! packages: the native profile stays authoritative.

use crate::cli::{Cli, TapCommand};
use crate::nix::OFFICIAL_CASK_SOURCE;
use crate::tap::{self, consent, registry, store};

mod staging;
use staging::{StagedImport, stage_import_with_setup};

use super::{CommandError, Session, print_json};

/// Validate one requested revision pin, when present.
///
/// A pin is exactly one bare 40-hex commit id. A qualified reference that
/// merely contains a commit (`github:owner/repo?rev=<commit>`) is refused:
/// it is a reference, not a revision.
fn validate_revision(revision: Option<&str>) -> Result<Option<String>, CommandError> {
    match revision {
        None => Ok(None),
        Some(value) if crate::nix::is_full_commit_id(value) => Ok(Some(value.to_string())),
        Some(value) => {
            Err(format!("`{value}` is not an exact revision; pass a full 40-hex commit id").into())
        }
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
                    "`{source}` is not a registered tap; add it with `pkg tap add {source}`"
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
    // Whether THIS invocation explicitly approved the source. Remembered
    // consent from an earlier `add` covers it, and an explicit `--trust`
    // counts as this invocation's approval even when remembered consent
    // exists: the registry entry can disappear before the lock is held,
    // and a publication without either remembered or fresh approval must
    // be refused under the lock.
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
                return Err(String::from("tap add was not approved").into());
            }
        }
    }

    // Registry mutations serialize on the one global lock; consent is
    // re-read under it. Lock order is fixed: registry first, then source.
    let global = registry::GlobalLock::acquire(&state_home)?;
    let mut registry = registry::Registry::load(&registry_path)?;
    recheck_consent(&registry, &source, &origin, approved_now)?;
    // The next registry state is prepared before publication, so the
    // publication is committed only when the registry replacement is one
    // already-fsynced rename away.
    let prepared = if registry.origin(&source).is_none() {
        registry.approve(&source, &origin, now_unix());
        Some(registry.prepare(&registry_path)?)
    } else {
        None
    };

    // Only now do the runtime and the importer run.
    let session = super::session(cli)?;
    let (locked, staged_generation, staged) =
        stage_import_with_setup(&session, &source, &origin, revision.as_deref(), now_unix())?;
    let mut publication = locked.publish(staged_generation, &staged.provenance, &staged.index)?;

    // One prepared rename records consent. A failure triggers a rollback
    // of the publication — first or replacing — so the previous generation
    // stays active when the rollback succeeds; when the rollback itself
    // fails, the error reports the exact known state instead of claiming
    // an undo that did not happen. Publication and registry commit stay
    // two renames; a crash between them is not covered.
    if let Some(prepared) = prepared
        && let Err(error) = registry::commit_prepared(prepared, &registry_path)
    {
        return Err(undo_publication_after_registry_failure(
            &source,
            publication,
            &error,
        ));
    }
    // The publication and its registry commit both succeeded: finish the
    // publication by clearing this run's fetch scratch best effort; the
    // preserved old flake is never touched and a failure is only a
    // warning.
    for warning in publication.finish() {
        eprintln!("pkg: warning: {warning}");
    }
    drop(global);
    report_publication(&source, &staged, publication.kind(), cli.verbose);
    Ok(())
}

/// Re-read consent under the global registry lock.
///
/// The approval must still be there — or this invocation must have given
/// it explicitly — before anything runs Nix, imports, or publishes state.
/// A concurrent `remove` that deletes the entry between the first read
/// and the lock must not turn this `add` into an implicit approval.
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

/// Build the honest error for a registry commit failure after a committed
/// publication, undoing the publication when that is possible.
///
/// The message reports the precise known state in every branch: an undo
/// that ran, an undo that failed (the generation may still be active), and
/// a restore that failed (the old generation stays preserved at its
/// immutable path). Nothing is ever claimed undone when it was not.
fn undo_publication_after_registry_failure(
    source: &str,
    publication: store::Publication<'_>,
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
            "pin a revision for one named tap: `pkg tap update SOURCE --revision SHA`",
        )
        .into());
    }
    let session = super::session(cli)?;
    // Targets come from the registry alone: a broken saved catalog must
    // not prevent repair with this command.
    let registry_path = registry::registry_path(&session.paths.state_home);
    // The global lock blocks concurrent remove or add runs over the same
    // registry for the whole update; recorded consent is re-read under it.
    let global = registry::GlobalLock::acquire(&session.paths.state_home)?;
    let registry = registry::Registry::load(&registry_path)?;
    let targets = targets(&registry, source)?;
    if targets.is_empty() {
        println!("No saved taps to update.\nNext: pkg tap add SOURCE");
        return Ok(());
    }
    // The global lock covers this registry snapshot and all publications.
    for (source, origin) in targets {
        let (locked, staged_generation, staged) =
            stage_import_with_setup(&session, source, origin, revision.as_deref(), now_unix())?;
        let mut publication =
            locked.publish(staged_generation, &staged.provenance, &staged.index)?;
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
    match published {
        store::Published::First => println!(
            "Added tap: {source} ({} packages available).",
            staged.result.eligible
        ),
        store::Published::Replaced { .. } => println!(
            "Updated tap: {source} ({} packages available).",
            staged.result.eligible
        ),
    }
    if verbose {
        eprintln!(
            "pkg: revision {}; {} eligible, {} excluded",
            staged.result.revision, staged.result.eligible, staged.result.excluded
        );
    }
}

/// `pkg tap list [--json]`
///
/// The registry is read strictly: a malformed registry is a hard error. A
/// broken saved catalog of one source is reported per source instead —
/// `remove` keeps working, and `pkg tap update SOURCE` can repair.
pub(super) fn list(cli: &Cli) -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    let registry_path = registry::registry_path(&paths.state_home);
    let registry = registry::Registry::load(&registry_path)?;
    let rows: Vec<TapRow> = registry
        .sources
        .iter()
        .map(|(source, entry)| {
            let state = tap::SavedCatalogs::load_one(&registry, &paths.state_home, source);
            match state {
                Ok(catalog) => TapRow {
                    source: source.clone(),
                    origin: entry.origin.clone(),
                    revision: Some(catalog.provenance.revision.clone()),
                    eligible: Some(catalog.provenance.eligible),
                    excluded: Some(catalog.provenance.excluded),
                    added_unix: entry.added_unix,
                    error: None,
                },
                Err(error) => TapRow {
                    source: source.clone(),
                    origin: entry.origin.clone(),
                    revision: None,
                    eligible: None,
                    excluded: None,
                    added_unix: entry.added_unix,
                    error: Some(error),
                },
            }
        })
        .collect();
    if cli.json {
        print_json(&crate::output::envelope("tap-list", &rows))?;
    } else if rows.is_empty() {
        println!("No saved taps.\nNext: pkg tap add SOURCE");
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
                _ => println!("{:<width$}  —  needs update", row.source),
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

/// One `pkg tap list` row.
#[derive(serde::Serialize)]
struct TapRow {
    source: String,
    origin: String,
    revision: Option<String>,
    eligible: Option<usize>,
    excluded: Option<usize>,
    added_unix: u64,
    error: Option<String>,
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
            format!("`{source}` is not a registered tap; `pkg tap list` shows the taps").into(),
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
pub(super) fn dispatch(cli: &Cli, command: &TapCommand) -> Result<(), CommandError> {
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
