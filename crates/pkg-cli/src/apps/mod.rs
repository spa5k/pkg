//! macOS launchers derived from the active native profile.
//!
//! Sync scans app bundles, assesses public tap apps, verifies destination
//! ownership, and runs the pinned helper. Private payloads use one generated
//! layout. Status reads the same derived source map without writing.
mod assessment;
mod destination;
pub(crate) mod error;
mod helper;
mod payload;
mod scan;
mod storage;

use crate::config::Paths;
use crate::nix::Nix;
use assessment::assess_public_tap_apps;
use destination::{
    DestinationState, clear_launchers, destination_path, ensure_owned_destination,
    inspect_destination, read_source_map, write_marker, write_source_map,
};
use error::AppError;
use helper::{ensure_helper, run_sync_trampolines};
use payload::{prepare_bundle, restored_path};
use scan::scan_apps;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
pub(crate) use storage::setup_storage;

/// Refreshes macOS launchers (and Dock entries) for active profile outputs.
///
/// Reads the active profile, collects intact `.app` bundles from its store
/// outputs, prepares externally signed copies when required, and builds a
/// directory-symlink source view. It runs the pinned helper into a private
/// staging folder beside the destination. It publishes launchers, marker,
/// and source map only after helper success and a second ownership check.
/// A failed helper or publication keeps the previous launcher folder and
/// source map. Dock state may change after helper work starts. With no
/// active apps, sync is a no-op unless an owned destination still holds
/// stale launchers to remove; an unowned folder is never touched.
/// # Errors
///
/// Returns an error if the host is not macOS, a profile lock or read fails, app assessment fails, or helper, destination, or launcher operations fail.
pub(crate) fn sync(nix: &Nix, paths: &Paths) -> Result<(), AppError> {
    if !cfg!(target_os = "macos") {
        return sync_locked(nix, paths);
    }
    let _profile_lock = crate::profile_lock::ProfileLock::acquire(&paths.profile)?;
    sync_locked(nix, paths)
}

/// Refresh launchers while the caller holds the profile lock.
pub(crate) fn sync_locked(nix: &Nix, paths: &Paths) -> Result<(), AppError> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err(
            "pkg apps sync supports macOS only; .app bundles are not exposed on this platform"
                .to_string()
                .into(),
        );
    }
    let _lock = crate::profile_lock::ProfileLock::acquire(&paths.state_home.join("apps"))?;
    let entries = list_profile(nix, &paths.profile)?;
    let apps = scan_apps(entries.iter())?;
    // Vendor assessment runs under the sync lock before launcher writes:
    // the destination guard, the marker, the helper profile, and any
    // launcher change all come later, so a failed check leaves the
    // previous launchers and source map untouched. Preparing a private
    // cache copy does not run vendor code or change the package profile.
    assess_public_tap_apps(&paths.state_home, &entries)?;
    let dest = destination_path()?;
    let state = inspect_destination(&dest)?;

    if apps.is_empty() {
        return match state {
            // Owned folder, no apps left: drop stale trampolines in place.
            // The folder is marker-verified pkg property; nothing outside it
            // is touched, and no helper is needed for removal.
            DestinationState::Owned => {
                clear_launchers(&dest)?;
                write_marker(&dest)?;
                write_source_map(&dest, &apps)?;
                Ok(())
            }
            // No destination, or one pkg does not own: nothing of pkg's to
            // manage, in particular no helper download and no folder touched.
            DestinationState::Missing | DestinationState::Unowned { .. } => Ok(()),
        };
    }

    // Writing requires a missing or owned destination; an unowned folder is
    // refused before the helper is fetched.
    ensure_owned_destination(&dest)?;
    let helper = ensure_helper(nix, paths)?;
    let view = tempfile::tempdir()
        .map_err(|e| format!("could not create a temporary source view: {e}"))?;
    let source_dir = view.path().join("sources");
    fs::create_dir(&source_dir)
        .map_err(|error| format!("could not create app source directory: {error}"))?;
    for (name, source) in &apps {
        let prepared = prepare_bundle(source)?;
        symlink(&prepared, source_dir.join(name)).map_err(|e| {
            format!(
                "could not link {} into the temporary source view: {e}",
                source.display()
            )
        })?;
    }
    let source_view = view.path().join("source-view");
    symlink(&source_dir, &source_view)
        .map_err(|error| format!("could not create the helper source-view link: {error}"))?;
    refresh_staged(&helper, &source_view, &dest, &apps)?;
    Ok(())
}

/// Reports launcher drift for doctor without changing anything.
///
/// Read-only: lists the active profile, scans its store outputs, inspects
/// the destination, and compares both launcher names *and* their recorded
/// source targets with the derived map written by the last successful sync.
/// A destination pkg needs but does not own is reported as `unhealthy`,
/// not as an operational failure. Returns a diagnostic string; lines are
/// prefixed with `healthy:` or `unhealthy:` so callers can classify without
/// parsing. Operational failures (an unreadable profile or destination)
/// are returned as errors.
/// # Errors
///
/// Returns an error if the host is not macOS, the profile cannot be read, app scanning fails, or destination state cannot be inspected or read.
pub(crate) fn status(nix: &Nix, paths: &Paths) -> Result<String, AppError> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err("pkg apps status supports macOS only".to_string().into());
    }
    let entries = list_profile(nix, &paths.profile)?;
    let apps = scan_apps(entries.iter())?;
    let dest = destination_path()?;
    let state = inspect_destination(&dest)?;
    // With no active apps pkg needs no destination; an unowned folder is
    // not a defect then.
    if apps.is_empty() && !matches!(state, DestinationState::Owned) {
        return Ok("healthy: no active .app bundles; app launchers not needed".to_string());
    }
    match state {
        DestinationState::Missing => {
            return Ok(format!(
                "unhealthy: {} active .app bundle(s) but no launcher folder {}; \
                 run `pkg apps sync`",
                apps.len(),
                dest.display()
            ));
        }
        DestinationState::Unowned { detail } => {
            return Ok(format!(
                "unhealthy: {detail}; remove the unowned folder and run `pkg apps sync`"
            ));
        }
        DestinationState::Owned => {}
    }
    let recorded = read_source_map(&dest)?;
    let Some(recorded) = recorded else {
        return Ok(format!(
            "unhealthy: {} has no recorded launcher source map; run `pkg apps sync`",
            dest.display()
        ));
    };
    let mut missing: Vec<&String> = Vec::new();
    let mut stale_sources: Vec<&String> = Vec::new();
    let mut changed: Vec<&String> = Vec::new();
    for (name, source) in &apps {
        match recorded.get(name) {
            None => missing.push(name),
            Some(old) if old != source => changed.push(name),
            Some(_) => {
                if !dest.join(name).exists()
                    || restored_path(source)?.is_some_and(|path| {
                        !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
                    })
                {
                    missing.push(name);
                }
            }
        }
    }
    for name in recorded.keys() {
        if !apps.contains_key(name) {
            stale_sources.push(name);
        }
    }
    if missing.is_empty() && stale_sources.is_empty() && changed.is_empty() {
        return Ok(format!(
            "healthy: {} launcher(s) in sync (folder {})",
            apps.len(),
            dest.display()
        ));
    }
    let list = |names: &[&String]| {
        names
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(format!(
        "unhealthy: app launcher drift in {} (missing: [{}]; stale: [{}]; changed source: [{}]); \
         run `pkg apps sync`",
        dest.display(),
        list(&missing),
        list(&stale_sources),
        list(&changed)
    ))
}

/// Lists a profile through the Nix facade, with one error shape.
fn list_profile(
    nix: &Nix,
    profile: &Path,
) -> Result<BTreeMap<String, crate::nix::ProfileEntry>, AppError> {
    nix.profile_list(profile)
        .map_err(|e| AppError::native(format!("could not list profile {}", profile.display()), e))
}

fn refresh_staged(
    helper: &Path,
    source_view: &Path,
    destination: &Path,
    apps: &BTreeMap<String, std::path::PathBuf>,
) -> Result<(), AppError> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::message("launcher destination has no parent"))?;
    let staging = tempfile::tempdir_in(parent)
        .map_err(|error| format!("could not create launcher staging directory: {error}"))?;
    let staged = staging.path().join("launchers");
    run_sync_trampolines(helper, source_view, &staged)?;
    let publish = || {
        let metadata = fs::symlink_metadata(&staged)
            .map_err(|error| format!("could not inspect staged launchers: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AppError::message(
                "mac-app-util did not create a plain launcher directory",
            ));
        }
        write_marker(&staged)?;
        write_source_map(&staged, apps)?;
        ensure_owned_destination(destination)?;
        publish_staged(&staged, destination, staging)
    };
    publish().map_err(AppError::mark_dock_effect_possible)
}

fn publish_staged(
    staged: &Path,
    destination: &Path,
    staging: tempfile::TempDir,
) -> Result<(), AppError> {
    use std::os::unix::fs::MetadataExt as _;

    let parent = destination
        .parent()
        .ok_or_else(|| AppError::message("launcher destination has no parent"))?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "could not inspect launcher parent {}: {error}",
            parent.display()
        )
    })?;
    // A symlinked or foreign-owned parent can redirect publication outside
    // the user's Applications directory.
    // SAFETY: getuid takes no arguments and cannot fail.
    let current_uid = unsafe { libc::getuid() };
    if !parent_metadata.is_dir() || parent_metadata.uid() != current_uid {
        return Err(AppError::message(format!(
            "refusing to publish launchers through an unsafe parent: {}",
            parent.display()
        )));
    }
    let backup = staging.path().join("previous-launchers");
    let had_destination = match inspect_destination(destination)? {
        DestinationState::Missing => false,
        DestinationState::Owned => true,
        DestinationState::Unowned { detail } => {
            return Err(AppError::message(format!(
                "{detail}; refusing to publish launchers"
            )));
        }
    };
    if had_destination {
        fs::rename(destination, &backup).map_err(|error| {
            format!("could not move the previous launcher folder aside: {error}")
        })?;
    }
    if let Err(error) = fs::rename(staged, destination) {
        let publish_error =
            AppError::message(format!("could not publish staged launchers: {error}"));
        if had_destination && let Err(restore_error) = fs::rename(&backup, destination) {
            let recovery_dir = staging.keep();
            return Err(AppError::message(format!(
                "{publish_error}; could not restore the previous launcher folder: {restore_error}; previous launchers were retained at {}",
                recovery_dir.join("previous-launchers").display()
            )));
        }
        return Err(publish_error);
    }
    Ok(())
}
