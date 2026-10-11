//! Tap import staging, sandbox setup, and publication validation.

use super::{CommandError, Session};
use crate::tap::{self, setup, store};

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

/// Retry once after approved sandbox setup. Only a typed refusal offers setup.
pub(super) fn stage_import_with_setup(
    session: &Session,
    source: &str,
    origin: &str,
    revision: Option<&str>,
    now_unix: u64,
) -> Result<(store::SourceStore, store::StagedGeneration, StagedImport), CommandError> {
    match stage_import(session, source, origin, revision, now_unix) {
        Ok(staged) => return Ok(staged),
        Err(StageError::Other(error)) => return Err(error),
        Err(StageError::SandboxRefused(_)) => {}
    }
    eprintln!("pkg: tap import refused: the Nix daemon is not proven sandboxed");
    if !setup::offer_and_apply(&session.nix)
        .map_err(|detail| format!("the sandbox setup failed: {detail}"))?
    {
        return Err(String::from(
            "the tap import needs the one-time sandbox setup; nothing was imported",
        )
        .into());
    }
    stage_import(session, source, origin, revision, now_unix).map_err(|error| match error {
        StageError::SandboxRefused(detail) => {
            format!("the tap import was refused again after the sandbox setup: {detail}").into()
        }
        StageError::Other(error) => error,
    })
}

/// Run one import into a fresh staging directory and return the validated
/// result together with the owned staged generation.
pub(super) struct StagedImport {
    pub(super) result: cask_catalog::tap::ImportResult,
    pub(super) index: crate::nix::CatalogIndex,
    pub(super) provenance: store::Provenance,
}

/// Import and validate under the source lock. Publication stays with the caller.
///
/// Evaluate only `staging/flake`. Bind its index to the approved origin,
/// revision, capture hash, entry counts, and native system before publication.
fn stage_import(
    session: &Session,
    source: &str,
    origin: &str,
    revision: Option<&str>,
    now_unix: u64,
) -> Result<(store::SourceStore, store::StagedGeneration, StagedImport), StageError> {
    let locked = store::SourceStore::lock(&session.paths.state_home, source)
        .map_err(|error| StageError::Other(error.into()))?;
    let staged = locked
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
        output_dir: staged.path().to_path_buf(),
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
        validated_flake_path(staged.path(), &result.flake_path).map_err(StageError::Other)?;
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
        staged,
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
