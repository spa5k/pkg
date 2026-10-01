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
use crate::tap::{self, consent, registry, setup, store};

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

/// The typed failure of one staging attempt.
///
/// Only the importer's typed sandbox refusal is carried as a category;
/// every other failure is already formatted for the CLI boundary.
enum StageError {
    /// The importer refused the import because the effective local nix
    /// config is not fail-closed; the setup offer may answer this.
    SandboxRefused(String),
    /// Any other failure, already a command error.
    Other(CommandError),
}

/// Classify one importer failure for the staging path.
///
/// The category is the importer's typed one: an ordinary failure whose
/// text merely contains the refusal phrase is an ordinary failure.
fn stage_error_for_import_error(source: &str, error: cask_catalog::tap::ImportError) -> StageError {
    match error {
        cask_catalog::tap::ImportError::SandboxRefused(detail) => {
            StageError::SandboxRefused(detail)
        }
        cask_catalog::tap::ImportError::Failed(detail) => StageError::Other(
            format!(
                "importing {source} failed; the published generation and the \
                 registry are unchanged: {detail}"
            )
            .into(),
        ),
    }
}

/// Stage one import; on the importer's TYPED sandbox refusal, offer the
/// one-time consent-gated setup and retry the import exactly once when
/// it verified. A declined or failed setup keeps the refusal short
/// instead of replaying the importer's full setup paragraph. An
/// ordinary failure — even one whose text contains the refusal phrase —
/// never triggers the setup offer.
fn stage_import_with_setup(
    session: &Session,
    source: &str,
    origin: &str,
    revision: Option<&str>,
    now_unix: u64,
) -> Result<
    (
        store::SourceStore,
        std::path::PathBuf,
        store::StagingGuard,
        StagedImport,
    ),
    CommandError,
> {
    match stage_import(session, source, origin, revision, now_unix) {
        Ok(staged) => Ok(staged),
        Err(StageError::Other(error)) => Err(error),
        Err(StageError::SandboxRefused(_)) => {
            eprintln!("pkg: tap import refused: the Nix daemon is not proven sandboxed");
            match setup::offer_and_apply(&session.nix) {
                Ok(true) => {
                    stage_import(session, source, origin, revision, now_unix).map_err(|error| {
                        match error {
                            StageError::SandboxRefused(detail) => format!(
                                "the tap import was refused again after the sandbox \
                             setup: {detail}"
                            )
                            .into(),
                            StageError::Other(error) => error,
                        }
                    })
                }
                Ok(false) => Err(String::from(
                    "the tap import needs the one-time sandbox setup; nothing was imported",
                )
                .into()),
                Err(detail) => Err(format!("the sandbox setup failed: {detail}").into()),
            }
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

/// One source whose update was requested.
struct Target {
    source: String,
    origin: String,
}

/// Resolve the update target set from the registry alone.
///
/// A broken saved catalog must not prevent repair with `pkg tap update`,
/// so targets never load saved catalogs.
fn targets(
    registry: &registry::Registry,
    named: Option<&str>,
) -> Result<Vec<Target>, CommandError> {
    match named {
        Some(name) => {
            let source = canonical_source(name)?;
            let Some(origin) = registry.origin(&source).map(ToString::to_string) else {
                return Err(format!(
                    "`{source}` is not a registered tap; add it with `pkg tap add {source}`"
                )
                .into());
            };
            Ok(vec![Target { source, origin }])
        }
        None => Ok(registry
            .sources
            .iter()
            .map(|(source, entry)| Target {
                source: source.clone(),
                origin: entry.origin.clone(),
            })
            .collect()),
    }
}

/// Run one import into a fresh staging directory and return the validated
/// result together with the staging path and guard.
struct StagedImport {
    result: cask_catalog::tap::ImportResult,
    index: crate::nix::CatalogIndex,
    provenance: store::Provenance,
}

/// Import, validate, and stage one source generation under its lock.
///
/// The importer never publishes. Validation evaluates the `catalogIndex`
/// attribute of the importer's actual generated flake root — the returned
/// `flake_path`, required to be the real `flake` directory directly inside
/// staging — with the client's strict decoder. The decoded envelope must
/// describe exactly the agreed source, revision, and origin, declare
/// exactly the native system, and carry entry counts and a capture hash
/// consistent with the importer's report, all checked against the one
/// provenance this function constructs. Any failure leaves the previous
/// active generation and the registry untouched.
fn stage_import(
    session: &Session,
    source: &str,
    origin: &str,
    revision: Option<&str>,
    now_unix: u64,
) -> Result<
    (
        store::SourceStore,
        std::path::PathBuf,
        store::StagingGuard,
        StagedImport,
    ),
    StageError,
> {
    let locked = store::SourceStore::lock(&session.paths.state_home, source)
        .map_err(|error| StageError::Other(error.into()))?;
    let (staging, guard) = locked
        .new_staging()
        .map_err(|error| StageError::Other(error.into()))?;
    let system = session
        .nix
        .system()
        .map_err(|error| StageError::Other(error.into()))?;
    // The stage boundary: everything above is local state only; from here
    // the approved source's data is fetched and its Ruby runs under the
    // sandboxed Nix build. Name the approved source, origin, and scope
    // before it starts — for `--trust` and updates too, which otherwise
    // print only after success.
    eprintln!("Importing tap: {source}…");
    let request = cask_catalog::tap::ImportRequest {
        source: String::from(source),
        revision: revision.map(ToString::to_string),
        system: system.clone(),
        nix: session.nix.executable().to_path_buf(),
        output_dir: staging.clone(),
    };
    let result = cask_catalog::tap::import(&request)
        .map_err(|error| stage_error_for_import_error(source, error))?;
    if result.source != source || result.origin != origin {
        return Err(StageError::Other(
            format!(
                "the importer returned identity {}/{} for source {source}; \
                 refusing to publish",
                result.source, result.origin
            )
            .into(),
        ));
    }
    // Only the importer's generated flake root — exactly the canonical
    // `staging/flake` directory — is returned for publication. The
    // untrusted checkout and the captures beside it never become the
    // active flake root.
    let flake_path =
        validated_flake_path(&staging, &result.flake_path).map_err(StageError::Other)?;
    let provenance = store::Provenance {
        source: String::from(source),
        origin: String::from(origin),
        revision: result.revision.clone(),
        metadata_sha256: result.metadata_sha256.clone(),
        eligible: result.eligible,
        excluded: result.excluded,
        published_unix: now_unix,
    };
    let index = tap::validate_staging(&session.nix, &flake_path)
        .map_err(|error| StageError::Other(error.into()))?;
    check_staged_index(&index, &provenance, &system).map_err(StageError::Other)?;
    Ok((
        locked,
        flake_path,
        guard,
        StagedImport {
            result,
            index,
            provenance,
        },
    ))
}

/// Require the importer's flake root to be exactly the real directory
/// `staging/flake`.
///
/// The whole untrusted checkout is never assumed to be the flake: only
/// the canonical returned path counts, it must equal the canonical staging
/// directory's `flake` child (so `flake_path.parent()` is always staging
/// itself), and it must be a real directory. Root paths, paths outside
/// staging, and any other in-staging path are refused before Nix
/// evaluation or publication.
fn validated_flake_path(
    staging: &std::path::Path,
    flake_path: &std::path::Path,
) -> Result<std::path::PathBuf, CommandError> {
    let canonical_staging = staging
        .canonicalize()
        .map_err(|error| format!("cannot resolve staging {}: {error}", staging.display()));
    let canonical_flake = flake_path.canonicalize().map_err(|error| {
        format!(
            "the importer reported no usable flake root {}: {error}",
            flake_path.display()
        )
    });
    let (canonical_staging, canonical_flake) = (canonical_staging?, canonical_flake?);
    let expected = canonical_staging.join("flake");
    if canonical_flake != expected {
        return Err(format!(
            "the importer must report the staged flake root {}, not {}",
            expected.display(),
            canonical_flake.display()
        )
        .into());
    }
    if !std::fs::symlink_metadata(&canonical_flake).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(format!(
            "the importer reported flake root {} which is not a real directory",
            canonical_flake.display()
        )
        .into());
    }
    Ok(canonical_flake)
}

/// Check the staged index envelope against the agreed import before
/// publication.
///
/// The one shared binding check ([`tap::validate_binding`]) compares the
/// envelope against the generation's provenance: exactly the agreed
/// source, its origin and revision, the importer's capture hash in the
/// raw provenance, exactly one target, and the importer's entry counts.
/// Staged import adds the one native fact: the single target must be this
/// host's system.
fn check_staged_index(
    index: &crate::nix::CatalogIndex,
    provenance: &store::Provenance,
    system: &str,
) -> Result<(), CommandError> {
    tap::validate_binding(index, provenance).map_err(|detail| {
        format!(
            "the staged catalog for {} is invalid: {detail}",
            provenance.source
        )
    })?;
    if index.targets[0] != system {
        return Err(format!(
            "the staged catalog must target exactly this system ({system}) and \
             nothing else; it targets [{}]",
            index.targets.join(", ")
        )
        .into());
    }
    Ok(())
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
    let state_home = {
        let paths = super::paths_only()?;
        paths.paths.state_home
    };
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
    let (locked, flake, guard, staged) =
        stage_import_with_setup(&session, &source, &origin, revision.as_deref(), now_unix())?;
    let published = locked.publish(&flake, &staged.provenance, &staged.index)?;
    settle_staging_guard(&published, guard);

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
            &source, &locked, &published, &error,
        ));
    }
    // The publication and its registry commit both succeeded: the staging
    // parent now holds only the preserved old generation and this run's
    // fetch scratch. Remove the scratch best effort; the preserved old
    // flake is never touched and a failure is only a warning.
    if let store::Published::Replaced { saved_generation } = &published
        && let Some(parent) = flake.parent()
    {
        cleanup_replaced_staging(parent, saved_generation);
    }
    drop(global);
    report_publication(&source, &staged, &published, cli.verbose);
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

/// Dispose of the staging guard right after a committed publication.
///
/// A first publication renamed the inner flake root to `current`, so the
/// armed guard deletes only the disposable staging parent (the raw
/// checkout and captures). A replacing publication left the preserved
/// old generation inside the staging parent, so the guard is disarmed
/// immediately and the old generation stays retained; its scratch
/// siblings are cleaned up separately after the run fully succeeds.
fn settle_staging_guard(published: &store::Published, guard: store::StagingGuard) {
    match published {
        store::Published::First => drop(guard),
        store::Published::Replaced { .. } => guard.keep(),
    }
}

/// Best-effort cleanup of a replaced generation's staging parent.
///
/// After a successful publication (and, for `add`, its registry commit)
/// the staging parent holds the preserved old generation at `preserved`
/// and this run's fetch scratch (raw checkout, captures) beside it. Only
/// the immediate scratch children are removed; the preserved old flake is
/// never touched. Failure is a warning and never changes the command's
/// success, and nothing is cleaned up when the run failed or rolled back.
fn cleanup_replaced_staging(staging: &std::path::Path, preserved: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(staging) else {
        eprintln!(
            "pkg: warning: cannot inspect the staging directory {} to clean \
             up its fetch scratch",
            staging.display()
        );
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == preserved {
            continue;
        }
        let removed = if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            eprintln!(
                "pkg: warning: cannot remove the staging scratch at {}: {error}",
                path.display()
            );
        }
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
    locked: &store::SourceStore,
    published: &store::Published,
    registry_error: &str,
) -> CommandError {
    match published {
        store::Published::First => match locked.undo_first_publication() {
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
        store::Published::Replaced { saved_generation } => {
            match locked.undo_replaced_publication(saved_generation) {
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
            }
        }
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
    for target in &targets {
        // Recorded consent covers updates of the requested source; an
        // origin change is refused and needs a fresh removal and add.
        let Some(origin) = registry.origin(&target.source) else {
            return Err(format!(
                "tap `{}` was removed while the update ran; add it again \
                 with `pkg tap add {}`",
                target.source, target.source
            )
            .into());
        };
        if origin != target.origin {
            return Err(format!(
                "tap `{}` changed origin from {} to {}; remove the tap and \
                 add it again to approve the new origin",
                target.source, target.origin, origin
            )
            .into());
        }
        let (locked, flake, guard, staged) = stage_import_with_setup(
            &session,
            &target.source,
            &target.origin,
            revision.as_deref(),
            now_unix(),
        )?;
        let published = locked.publish(&flake, &staged.provenance, &staged.index)?;
        settle_staging_guard(&published, guard);
        // An update has no registry write: after the successful exchange
        // only the preserved old generation and this run's fetch scratch
        // remain in the staging parent. Clean the scratch best effort.
        if let store::Published::Replaced { saved_generation } = &published
            && let Some(parent) = flake.parent()
        {
            cleanup_replaced_staging(parent, saved_generation);
        }
        report_publication(&target.source, &staged, &published, cli.verbose);
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
    let session = super::paths_only()?;
    let registry_path = registry::registry_path(&session.paths.state_home);
    let registry = registry::Registry::load(&registry_path)?;
    let rows: Vec<TapRow> = registry
        .sources
        .iter()
        .map(|(source, entry)| {
            let state = tap::SavedCatalogs::load_one(&registry, &session.paths.state_home, source);
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
                    println!("{:<width$}  {}  ready", row.source, eligible)
                }
                _ => println!("{:<width$}  —  needs update", row.source,),
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
    let session = super::paths_only()?;
    let path = registry::registry_path(&session.paths.state_home);
    // The global lock blocks concurrent add or update runs over the same
    // registry for the whole removal.
    let _global = registry::GlobalLock::acquire(&session.paths.state_home)?;
    let mut registry = registry::Registry::load(&path)?;
    if !registry.remove(&source) {
        return Err(
            format!("`{source}` is not a registered tap; `pkg tap list` shows the taps").into(),
        );
    }
    registry.save(&path)?;
    store::remove_current(&session.paths.state_home, &source).map_err(|error| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_refusal_category_survives_text_lookalikes() {
        use cask_catalog::tap::ImportError;
        // Only the typed refusal is carried as a category.
        let refused = stage_error_for_import_error(
            "somebody/apps",
            ImportError::SandboxRefused(String::from("sandbox is false")),
        );
        assert!(matches!(refused, StageError::SandboxRefused(_)));
        // An ordinary failure whose text contains the refusal phrase is
        // an ordinary failure: the setup offer must never trigger.
        let lookalike = stage_error_for_import_error(
            "somebody/apps",
            ImportError::Failed(String::from(
                "effective local nix config refuses the raw tap import: \
                 the settings could not be verified",
            )),
        );
        let StageError::Other(error) = lookalike else {
            panic!("a coincidental refusal phrase must not become a refusal");
        };
        let CommandError::Message(text) = error else {
            panic!("the ordinary failure is a message");
        };
        assert!(text.contains("importing somebody/apps failed"), "{text}");
        assert!(text.contains("could not be verified"), "{text}");
        // The refusal detail stays available for the retry path.
        let StageError::SandboxRefused(detail) = stage_error_for_import_error(
            "somebody/apps",
            ImportError::SandboxRefused(String::from("added host path /etc")),
        ) else {
            panic!("the typed refusal is carried");
        };
        assert_eq!(detail, "added host path /etc");
    }

    #[test]
    fn revision_pins_must_be_bare_full_commits() {
        assert_eq!(validate_revision(None).ok(), Some(None));
        assert_eq!(
            validate_revision(Some("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a0")).ok(),
            Some(Some(String::from(
                "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a0"
            )))
        );
        // A qualified URL that merely contains a full commit is a
        // reference, not a revision pin; a 39-character id and a non-hex
        // string are not full commit ids either.
        for refused in [
            "github:owner/repo?rev=0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a",
            "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1 ",
            "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1g",
            "main",
            "",
        ] {
            assert!(validate_revision(Some(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn sources_canonicalize_and_the_official_source_is_reserved() {
        assert_eq!(
            canonical_source("Somebody/homebrew-Apps").ok(),
            Some(String::from("somebody/apps"))
        );
        // The actual backend contract refuses a trailing slash: the URL
        // must be exactly https://github.com/owner/repo.
        let error = canonical_source("https://github.com/somebody/homebrew-apps/")
            .expect_err("a trailing slash is outside the accepted contract");
        let CommandError::Message(text) = error else {
            panic!("the trailing-slash error is a message");
        };
        assert!(text.contains("exactly"), "{text}");
        assert_eq!(
            canonical_source("https://github.com/somebody/homebrew-apps").ok(),
            Some(String::from("somebody/apps"))
        );
        let error = canonical_source("homebrew/cask").expect_err("the official source is reserved");
        let CommandError::Message(text) = error else {
            panic!("the reserved-source error is a message");
        };
        assert!(text.contains("reserved"), "{text}");
        assert!(canonical_source("git@gitlab.com:a/b").is_err());
        assert!(canonical_source("somebody/apps/extra").is_err());
    }

    const ORIGIN: &str = "https://github.com/somebody/homebrew-apps";
    const CAPTURE_SHA: &str = "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251";

    /// The text of one unprinted command error.
    fn message(error: CommandError) -> String {
        match error {
            CommandError::Message(text) => text,
            CommandError::Reported(_) => panic!("the error is a message"),
        }
    }

    fn registered_registry() -> registry::Registry {
        let mut registry = registry::Registry::empty();
        registry.approve("somebody/apps", ORIGIN, 1);
        registry
    }

    #[test]
    fn consent_recheck_refuses_a_concurrently_removed_source() {
        // Remembered consent still stands after the lock re-read.
        let registry = registered_registry();
        assert!(recheck_consent(&registry, "somebody/apps", ORIGIN, false).is_ok());
        // Fresh approval also passes when the entry stayed or appeared.
        assert!(
            recheck_consent(&registry::Registry::empty(), "somebody/apps", ORIGIN, true).is_ok()
        );
        // The race: the entry is gone under the lock and this invocation
        // did not explicitly approve — refuse before anything runs.
        let error = recheck_consent(&registry::Registry::empty(), "somebody/apps", ORIGIN, false)
            .expect_err("a vanished approval must be refused");
        assert!(message(error).contains("removed"));
        // An origin change under the lock is still refused, fresh or not.
        let error = recheck_consent(
            &registered_registry(),
            "somebody/apps",
            "https://github.com/somebody/homebrew-moved",
            true,
        )
        .expect_err("an origin change must be refused");
        assert!(message(error).contains("origin"));
    }

    /// The one provenance `stage_import` builds for a chosen capture hash.
    fn staged_provenance(metadata_sha256: &str) -> store::Provenance {
        store::Provenance {
            source: String::from("somebody/apps"),
            origin: String::from(ORIGIN),
            revision: String::from("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"),
            metadata_sha256: String::from(metadata_sha256),
            eligible: 1,
            excluded: 0,
            published_unix: 17,
        }
    }

    /// One staged index envelope for the agreed source, with a chosen raw
    /// capture hash and target list.
    fn staged_index(capture_sha256: &str, targets: &serde_json::Value) -> crate::nix::CatalogIndex {
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let raw = serde_json::json!({
            "url": ORIGIN,
            "revision": revision,
            "sha256": CAPTURE_SHA,
            "license": "Homebrew data license",
            "raw": {
                "archiveUrl": format!("{ORIGIN}/archive/{revision}.tar.gz"),
                "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                "captureSha256": capture_sha256,
            },
        });
        let mut systems = serde_json::Map::new();
        for target in targets.as_array().expect("targets list") {
            systems.insert(
                target.as_str().expect("system").to_string(),
                serde_json::json!({"entries": {
                    "somebody/apps/tool": {
                        "source": "somebody/apps", "token": "tool", "name": null,
                        "description": null, "version": "1.0", "homepage": null,
                        "status": "eligible", "kind": "binary", "reason": null,
                        "detail": null}}}),
            );
        }
        let raw_index = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"somebody/apps": raw},
            "targets": targets.clone(),
            "macosBaseline": "15.7.7",
            "systems": systems,
        });
        crate::nix::decode_catalog_index(&raw_index).expect("decodes")
    }

    #[test]
    fn staged_index_checks_accept_exact_provenance_and_refuse_mismatches() {
        let provenance = staged_provenance(CAPTURE_SHA);
        // The exact contract passes: one input, raw provenance with the
        // importer's capture hash, exactly this system as the only target.
        let index = staged_index(CAPTURE_SHA, &serde_json::json!(["x86_64-linux"]));
        assert!(check_staged_index(&index, &provenance, "x86_64-linux").is_ok());

        // A capture hash that disagrees with the provenance is a
        // publication refusal.
        let mismatch = staged_index(
            "3f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529253",
            &serde_json::json!(["x86_64-linux"]),
        );
        let error = check_staged_index(&mismatch, &provenance, "x86_64-linux")
            .expect_err("a capture-hash mismatch must refuse publication");
        let error = message(error);
        assert!(error.contains("capture hash"), "{error}");

        // Extra targets are refused, even when this system is among them.
        let extra = staged_index(
            CAPTURE_SHA,
            &serde_json::json!(["x86_64-linux", "aarch64-darwin"]),
        );
        let error = check_staged_index(&extra, &provenance, "x86_64-linux")
            .expect_err("extra targets must refuse publication");
        assert!(message(error).contains("exactly"));

        // Duplicated targets are refused the same way.
        let duplicate = staged_index(
            CAPTURE_SHA,
            &serde_json::json!(["x86_64-linux", "x86_64-linux"]),
        );
        assert!(check_staged_index(&duplicate, &provenance, "x86_64-linux").is_err());

        // A local capture without raw provenance is refused.
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let bare = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"somebody/apps": {
                "url": ORIGIN, "revision": revision, "sha256": CAPTURE_SHA,
                "license": "Homebrew data license"}},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {}}},
        });
        let bare = crate::nix::decode_catalog_index(&bare).expect("decodes");
        let error = check_staged_index(&bare, &provenance, "x86_64-linux")
            .expect_err("a capture without raw provenance must refuse publication");
        assert!(message(error).contains("raw"));

        // An envelope describing a different source than the provenance is
        // refused by the shared binding check.
        let wrong_source = staged_index(CAPTURE_SHA, &serde_json::json!(["x86_64-linux"]));
        let mut other = staged_provenance(CAPTURE_SHA);
        other.source = String::from("other/apps");
        assert!(check_staged_index(&wrong_source, &other, "x86_64-linux").is_err());
    }

    #[test]
    fn flake_root_must_be_exactly_the_staging_flake_directory() {
        let staging = tempfile::tempdir().expect("tempdir");
        let staging_path = staging.path();
        let flake = staging_path.join("flake");
        std::fs::create_dir(&flake).expect("mkdir flake");
        assert!(
            validated_flake_path(staging_path, &flake).is_ok(),
            "the exact staging/flake layout must be accepted"
        );

        // The staging root itself is refused: `flake == staging` would make
        // later cleanup target the generations parent.
        assert!(validated_flake_path(staging_path, staging_path).is_err());
        // Any other sibling inside staging is refused.
        let other = staging_path.join("other");
        std::fs::create_dir(&other).expect("mkdir other");
        assert!(validated_flake_path(staging_path, &other).is_err());
        // An outside and a root path are refused.
        assert!(validated_flake_path(staging_path, std::path::Path::new("/")).is_err());
        let outside = tempfile::tempdir().expect("tempdir");
        assert!(validated_flake_path(staging_path, outside.path()).is_err());
        // The exact path must be a real directory, not a file.
        std::fs::remove_dir(&flake).expect("rmdir flake");
        std::fs::write(&flake, "x").expect("write file");
        assert!(validated_flake_path(staging_path, &flake).is_err());
        // A missing path is refused.
        std::fs::remove_file(&flake).expect("remove file");
        assert!(validated_flake_path(staging_path, &flake).is_err());
    }
}
