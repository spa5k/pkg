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
mod publication;
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
use helper::ensure_helper;
use payload::{prepare_bundle, restored_path};
use publication::{prepare_parent, refresh_staged};
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
        return Err(AppError::message(
            "pkg apps sync supports macOS only; .app bundles are not exposed on this platform",
        ));
    }
    let profile = crate::profile_lock::LockedProfile::acquire(nix, paths)?;
    sync_locked(&profile)
}

/// Refresh launchers while the caller holds the profile lock.
pub(crate) fn sync_locked(
    profile: &crate::profile_lock::LockedProfile<'_>,
) -> Result<(), AppError> {
    let nix = profile.nix();
    let paths = profile.paths();
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
    prepare_parent(&dest)?;
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
