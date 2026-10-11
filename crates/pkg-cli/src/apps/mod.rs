//! macOS launchers derived from the active native profile.
//!
//! Sync scans app bundles, assesses public tap apps, verifies destination
//! ownership, and runs the pinned helper. Private payloads use one generated
//! layout. Status reads the same derived source map without writing.
mod assessment;
mod destination;
mod helper;
mod payload;
mod scan;
mod storage;

use crate::config::Paths;
use crate::nix::Nix;
use assessment::assess_public_tap_apps;
use destination::{
    DestinationState, clear_launchers, destination_path, ensure_owned_destination,
    inspect_destination, read_source_map, restore_marker, write_marker, write_source_map,
};
use helper::{ensure_helper, run_sync_trampolines};
use payload::{prepare_bundle, restored_path};
use scan::scan_apps;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
pub use storage::setup_storage;

/// Refreshes macOS launchers (and Dock entries) for active profile outputs.
///
/// Reads the active profile, collects intact `.app` bundles from its store
/// outputs, prepares externally signed copies when required, builds a
/// unique temporary source view of symlinks, and
/// runs the pinned helper's `sync-trampolines` against the verified
/// pkg-owned destination. The destination is only accepted for writing when
/// missing or owned (exact marker payload); the marker is restored after a
/// partial or interrupted helper failure so a retry can proceed. With no
/// active apps, sync is a no-op unless an owned destination still holds
/// stale launchers to remove; an unowned folder is never touched then.
pub fn sync(nix: &Nix, paths: &Paths) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return sync_locked(nix, paths);
    }
    let _profile_lock = crate::profile_lock::ProfileLock::acquire(&paths.profile)?;
    sync_locked(nix, paths)
}

/// Refresh launchers while the caller holds the profile lock.
pub(crate) fn sync_locked(nix: &Nix, paths: &Paths) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err(
            "pkg apps sync supports macOS only; .app bundles are not exposed on this platform"
                .to_string(),
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
    for (name, source) in &apps {
        let prepared = prepare_bundle(source)?;
        symlink(&prepared, view.path().join(name)).map_err(|e| {
            format!(
                "could not link {} into the temporary source view: {e}",
                source.display()
            )
        })?;
    }
    if let Err(error) = run_sync_trampolines(&helper, view.path(), &dest) {
        // `sync-trampolines` replaces the destination; restore the
        // ownership marker on whatever remains so a retry is possible
        // and the folder is never left looking unowned. Interrupted runs
        // arrive here too: the child was reaped and pkg stayed alive.
        restore_marker(&dest);
        return Err(error);
    }

    write_marker(&dest)?;
    write_source_map(&dest, &apps)?;
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
pub fn status(nix: &Nix, paths: &Paths) -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err("pkg apps status supports macOS only".to_string());
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
    let mut stale: Vec<&String> = Vec::new();
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
            stale.push(name);
        }
    }
    if missing.is_empty() && stale.is_empty() && changed.is_empty() {
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
        list(&stale),
        list(&changed)
    ))
}

/// Lists a profile through the Nix facade, with one error shape.
fn list_profile(
    nix: &Nix,
    profile: &Path,
) -> Result<BTreeMap<String, crate::nix::ProfileEntry>, String> {
    nix.profile_list(profile)
        .map_err(|e| format!("could not list profile {}: {e}", profile.display()))
}
