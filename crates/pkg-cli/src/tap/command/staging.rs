//! Tap import staging, sandbox setup, and publication validation.

use super::{CommandError, Session};
use crate::tap::{self, setup, store};

/// Retry once after approved sandbox setup. Only a typed refusal offers setup.
pub(super) fn stage_import_with_setup(
    session: &Session,
    source: &str,
    origin: &str,
    revision: Option<&str>,
    now_unix: u64,
) -> Result<(store::StagedGeneration, StagedImport), CommandError> {
    match stage_import(session, source, origin, revision, now_unix) {
        Ok(staged) => return Ok(staged),
        Err(CommandError::TapImport {
            error: cask_catalog::tap::ImportError::SandboxRefused(_),
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    if !crate::tap::consent::stdin_is_terminal() {
        return Err(String::from(
            "taps need a one-time Nix sandbox setup. Run this command in a terminal to set it up. Nothing was imported.",
        )
        .into());
    }
    eprintln!("Taps need a one-time Nix sandbox setup.");
    if !setup::offer_and_apply(&session.nix)? {
        return Err(String::from("the sandbox setup did not run. Nothing was imported.").into());
    }
    stage_import(session, source, origin, revision, now_unix).map_err(|error| match error {
        CommandError::TapImport {
            error: cask_catalog::tap::ImportError::SandboxRefused(detail),
            ..
        } => format!("the tap import was refused again after the sandbox setup: {detail}").into(),
        error => error,
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
) -> Result<(store::StagedGeneration, StagedImport), CommandError> {
    let locked = store::SourceStore::lock(&session.paths.state_home, source)?;
    let staged = locked.new_staging()?;
    let system = session.nix.system()?;
    // Report the source and origin before fetching or running sandboxed Ruby.
    // `--trust` and updates need this message because they skip the consent prompt.
    eprintln!("Importing tap: {source}…");
    let request = cask_catalog::tap::ImportRequest {
        source: String::from(source),
        revision: revision.map(ToString::to_string),
        system: system.clone(),
        nix: session.nix.executable().to_path_buf(),
        output_dir: staged.path().to_path_buf(),
    };
    let result = cask_catalog::tap::import(&request).map_err(|error| CommandError::TapImport {
        tap: source.to_owned(),
        error,
    })?;
    if result.source != source || result.origin != origin {
        return Err(format!(
            "the importer returned identity {}/{} for source {source}; \
             refusing to publish",
            result.source, result.origin
        )
        .into());
    }
    // Publish only the generated `staging/flake` directory, not the checkout
    // or captures beside it.
    let flake_path = validated_flake_path(staged.path(), &result.flake_path)?;
    let provenance = store::Provenance {
        source: String::from(source),
        origin: String::from(origin),
        revision: result.revision.clone(),
        metadata_sha256: result.metadata_sha256.clone(),
        eligible: result.eligible,
        excluded: result.excluded,
        published_unix: now_unix,
    };
    let index =
        tap::validate_staging(&session.nix, &flake_path).map_err(CommandError::TapValidation)?;
    check_staged_index(&index, &provenance, &system)?;
    Ok((
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
/// Require a real directory whose canonical path is the canonical staging
/// directory's `flake` child. Reject all other paths before evaluation or publication.
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
/// [`tap::validate_binding`] checks the source, origin, revision, capture hash,
/// entry counts, and single target against the generation's provenance.
/// Staging also requires that target to match this host's system.
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
    if index.targets()[0] != system {
        return Err(format!(
            "the staged catalog must target exactly this system ({system}) and \
             nothing else; it targets [{}]",
            index.targets().join(", ")
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
