//! macOS launchers derived from the active native profile.
//!
//! Sync scans app bundles, assesses Cask and tap apps, verifies destination
//! ownership, and runs the pinned helper. Private payloads use one generated
//! layout. Status reads the same derived source map without writing.
mod assessment;
mod command;
pub(crate) use command::{command, refresh_after_profile_change};
mod destination;
pub(crate) mod error;
mod helper;
mod payload;
mod publication;
mod scan;
mod storage;

use crate::config::Paths;
use crate::nix::Nix;
use assessment::assess_vendor_apps;
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

/// Refresh macOS launchers and Dock entries for active profile outputs.
///
/// Reads the active profile, collects intact `.app` bundles from its store
/// outputs, prepares externally signed copies when required, and builds a
/// directory-symlink source view. It runs the pinned helper into a private
/// staging folder beside the destination. It publishes launchers, marker,
/// and source map only after helper success and a second ownership check.
/// A failed helper or publication keeps the previous launcher folder and
/// source map. Dock state may change after helper work starts. With no
/// active apps, sync only removes stale launchers from an owned destination.
/// # Errors
///
/// Returns an error if the host is not macOS, a profile lock or read fails, app assessment fails, or helper, destination, or launcher operations fail.
pub(crate) fn sync(nix: &Nix, paths: &Paths) -> Result<(), AppError> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::message(
            "pkg apps sync supports macOS only; .app bundles are not exposed on this platform",
        ));
    }
    let profile = crate::profile::LockedProfile::acquire(nix, paths)?;
    let entries = list_profile(nix, &paths.profile)?;
    sync_locked(&profile, &entries).map(drop)
}

/// The launchers that one sync added and removed, by bundle name.
#[derive(Default)]
pub(crate) struct LauncherChanges {
    pub(crate) added: Vec<String>,
    pub(crate) removed: Vec<String>,
}

/// Refresh launchers from the active entries read under this profile lock.
pub(crate) fn sync_locked(
    profile: &crate::profile::LockedProfile<'_>,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<LauncherChanges, AppError> {
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
    let apps = scan_apps(entries.iter())?;
    // Assess vendor bundles under the sync lock before any launcher writes.
    // A private cache copy does not run vendor code or change the profile.
    assess_vendor_apps(&paths.state_home, entries)?;
    let dest = destination_path()?;
    let state = inspect_destination(&dest)?;
    let before = match state {
        DestinationState::Owned => read_source_map(&dest)?.unwrap_or_default(),
        DestinationState::Missing | DestinationState::Unowned { .. } => BTreeMap::new(),
    };
    let changes = LauncherChanges {
        added: apps
            .keys()
            .filter(|name| !before.contains_key(*name))
            .cloned()
            .collect(),
        removed: before
            .keys()
            .filter(|name| !apps.contains_key(*name))
            .cloned()
            .collect(),
    };

    if apps.is_empty() {
        return match state {
            // Remove stale launchers from the owned folder without the helper.
            DestinationState::Owned => {
                clear_launchers(&dest)?;
                write_marker(&dest)?;
                write_source_map(&dest, &apps)?;
                Ok(changes)
            }
            // No owned launchers need removal.
            DestinationState::Missing | DestinationState::Unowned { .. } => Ok(changes),
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
    refresh_staged(nix, paths, &helper, &source_view, &dest, &apps)?;
    Ok(changes)
}

pub(crate) enum Health {
    Healthy,
    Unhealthy,
}

impl Health {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
        }
    }
}

pub(crate) struct AppStatus {
    pub(crate) health: Health,
    pub(crate) message: String,
}

impl AppStatus {
    fn new(health: Health, detail: &str) -> Self {
        Self {
            health,
            message: detail.to_string(),
        }
    }
}

pub(crate) fn status(nix: &Nix, paths: &Paths) -> Result<AppStatus, AppError> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err("pkg apps status supports macOS only".to_string().into());
    }
    let entries = list_profile(nix, &paths.profile)?;
    let apps = scan_apps(entries.iter())?;
    let dest = destination_path()?;
    let state = inspect_destination(&dest)?;
    if apps.is_empty() && !matches!(state, DestinationState::Owned) {
        return Ok(AppStatus::new(Health::Healthy, "no apps installed"));
    }
    match state {
        DestinationState::Missing => {
            return Ok(AppStatus::new(
                Health::Unhealthy,
                &format!(
                    "{} active .app bundle(s) but no launcher folder {}; \
                 run `pkg apps sync`",
                    apps.len(),
                    dest.display()
                ),
            ));
        }
        DestinationState::Unowned { detail } => {
            return Ok(AppStatus::new(
                Health::Unhealthy,
                &format!("{detail}; remove the unowned folder and run `pkg apps sync`"),
            ));
        }
        DestinationState::Owned => {}
    }
    let recorded = read_source_map(&dest)?;
    let Some(recorded) = recorded else {
        return Ok(AppStatus::new(
            Health::Unhealthy,
            &format!(
                "{} has no recorded launcher source map; run `pkg apps sync`",
                dest.display()
            ),
        ));
    };
    let mut missing: Vec<&str> = Vec::new();
    let mut stale_sources: Vec<&str> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    for (name, source) in &apps {
        match recorded.get(name) {
            None => missing.push(name.as_str()),
            Some(old) if old != source => changed.push(name.as_str()),
            Some(_) => {
                if !dest.join(name).exists()
                    || restored_path(source)?.is_some_and(|path| {
                        !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
                    })
                {
                    missing.push(name.as_str());
                }
            }
        }
    }
    for name in recorded.keys() {
        if !apps.contains_key(name) {
            stale_sources.push(name.as_str());
        }
    }
    if missing.is_empty() && stale_sources.is_empty() && changed.is_empty() {
        return Ok(AppStatus::new(
            Health::Healthy,
            &match apps.len() {
                0 => String::from("no apps installed"),
                1 => format!("1 launcher in {}", dest.display()),
                count => format!("{count} launchers in {}", dest.display()),
            },
        ));
    }
    Ok(AppStatus::new(
        Health::Unhealthy,
        &format!(
            "app launcher drift in {} (missing: [{}]; stale: [{}]; changed source: [{}]); \
         run `pkg apps sync`",
            dest.display(),
            missing.join(", "),
            stale_sources.join(", "),
            changed.join(", ")
        ),
    ))
}

/// Lists a profile through the Nix facade, with one error shape.
fn list_profile(
    nix: &Nix,
    profile: &Path,
) -> Result<BTreeMap<String, crate::nix::ProfileEntry>, AppError> {
    crate::profile::entries(nix, profile)
        .map_err(|e| AppError::native(format!("could not list profile {}", profile.display()), e))
}
