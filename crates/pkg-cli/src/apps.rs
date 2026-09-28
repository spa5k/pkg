//! macOS application exposure for installed packages (NN-06 / design D7).
//!
//! This module turns intact `.app` bundles found in the *active* outputs of
//! the user's native package profile into Spotlight- and Dock-visible
//! launchers under `~/Applications/pkg/`, using the pinned external
//! `mac-app-util` helper from a separate Nix profile. The helper's
//! `sync-trampolines` command runs through the shared signal forward/reap
//! boundary, so launcher and Dock refresh happen together and a cancelled
//! run is reaped and reported. This module never mutates `/Applications`, any
//! other user application folder, or package state; every failure is
//! reported to the caller and nothing is retried here.
//!
//! Platform policy: `.app` exposure is macOS-only. On other platforms
//! [`sync`] fails with an explicit unsupported message when invoked
//! manually, and callers skip automatic refresh. No helper source is
//! bundled; the helper is installed on demand from one exact pinned flake
//! reference and verified by its native profile identity.
//!
//! Requires the `tempfile` crate for the unique temporary source view
//! (a normal workspace dependency).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::config::Paths;
use crate::nix::{Nix, Outcome, run_direct};

/// The only helper used for launcher and Dock sync, pinned to one exact
/// commit. Verified at this commit: builds with import-from-derivation
/// disabled, and `mac-app-util --help` lists `mktrampoline`,
/// `sync-trampolines`, and `sync-dock`. License AGPL-3.0; the helper is
/// fetched by Nix into the helper profile, never bundled by `pkg`.
const HELPER_FLAKE_REF: &str =
    "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887";
/// Repository prefix of [`HELPER_FLAKE_REF`], matched against the helper
/// profile entry's native `original_url`.
const HELPER_REPO_PREFIX: &str = "github:hraban/mac-app-util/";
/// Exact locked revision required in the helper profile entry.
const HELPER_REVISION: &str = "039f33deef21782d4db97087f426504951239887";

/// Location of the helper binary inside one of its store outputs.
const HELPER_BINARY_RELATIVE: &str = "bin/mac-app-util";

/// Launcher destination, relative to the user's home directory. The only
/// directory this module ever writes to.
const DEST_RELATIVE: &str = "Applications/pkg";
/// Marker file that claims the destination for `pkg`; its exact payload
/// must be present before any destination content is replaced.
const MARKER_NAME: &str = ".pkg-owned";
/// Payload required inside [`MARKER_NAME`].
const MARKER_PAYLOAD: &str = "pkg-apps-owned/1\n";
/// Derived record of launcher name to source bundle, written after a
/// successful sync and compared by [`status`]. It is derived state, not
/// package inventory; it is preserved on failed syncs.
const SOURCE_MAP_NAME: &str = ".pkg-launcher-sources";

/// Name of the advisory lock file inside the state home.
const SYNC_LOCK_NAME: &str = "app-sync.lock";

/// Refreshes macOS launchers (and Dock entries) for active profile outputs.
///
/// Reads the active profile, collects intact `.app` bundles from its store
/// outputs, builds a unique temporary source view of symlinks to them, and
/// runs the pinned helper's `sync-trampolines` against the verified
/// pkg-owned destination. The destination is only accepted for writing when
/// missing or owned (exact marker payload); the marker is restored after a
/// partial or interrupted helper failure so a retry can proceed. With no
/// active apps, sync is a no-op unless an owned destination still holds
/// stale launchers to remove; an unowned folder is never touched then.
pub fn sync(nix: &Nix, paths: &Paths) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        let _ = (nix.executable(), &paths.helper_profile);
        return Err(
            "pkg apps sync supports macOS only; .app bundles are not exposed on this platform"
                .to_string(),
        );
    }
    let _lock = SyncLock::acquire(paths)?;
    let entries = list_profile(nix, &paths.profile)?;
    let apps = scan_apps(&entries)?;
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
        symlink(source, view.path().join(name)).map_err(|e| {
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
    let apps = scan_apps(&entries)?;
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
                if !dest.join(name).exists() {
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

/// The user's launcher destination: `$HOME/Applications/pkg`.
fn destination_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        "HOME is not set; cannot locate ~/Applications for app launchers".to_string()
    })?;
    let home = PathBuf::from(home);
    if !home.is_absolute() || home == Path::new("/") {
        return Err(format!(
            "HOME ({}) must be an absolute user directory; refusing to touch app folders",
            home.display()
        ));
    }
    let dest = home.join(DEST_RELATIVE);
    // A hostile or broken HOME must never point the launcher destination at
    // the system application folder.
    if dest.parent() == Some(Path::new("/Applications")) {
        return Err("refusing to use /Applications as the launcher destination".to_string());
    }
    Ok(dest)
}

/// The inspected ownership state of the launcher destination.
///
/// Read-only classification for sync and status; the write guard is
/// [`ensure_owned_destination`].
#[derive(Debug)]
enum DestinationState {
    /// The destination does not exist.
    Missing,
    /// The destination exists with a valid ownership marker.
    Owned,
    /// The destination exists but pkg does not own it; `detail` says why
    /// in one phrase.
    Unowned {
        /// Why the existing destination is not pkg-owned.
        detail: String,
    },
}

/// Classify the destination without changing anything.
///
/// Every pre-existing directory without a valid marker — empty or not,
/// symlinked or not — classifies as [`DestinationState::Unowned`], so
/// read-only paths can report it and the write path can refuse it.
fn inspect_destination(dest: &Path) -> Result<DestinationState, String> {
    match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DestinationState::Missing),
        Err(e) => Err(format!("could not inspect {}: {e}", dest.display())),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Ok(DestinationState::Unowned {
                    detail: format!("app launcher destination {} is a symlink", dest.display()),
                });
            }
            if !meta.is_dir() {
                return Ok(DestinationState::Unowned {
                    detail: format!(
                        "app launcher destination {} exists and is not a directory",
                        dest.display()
                    ),
                });
            }
            if !marker_is_valid(dest) {
                return Ok(DestinationState::Unowned {
                    detail: format!(
                        "app launcher destination {} exists without a valid {} marker",
                        dest.display(),
                        MARKER_NAME
                    ),
                });
            }
            Ok(DestinationState::Owned)
        }
    }
}

/// Validates the destination for writing and reports whether it is pkg-owned.
///
/// Only two states are acceptable: the destination does not exist, or it
/// exists with a regular, non-symlink [`MARKER_NAME`] carrying exactly
/// [`MARKER_PAYLOAD`]. Any other pre-existing directory — empty or not,
/// symlinked or not — is refused, so this module never replaces a folder
/// it does not own.
fn ensure_owned_destination(dest: &Path) -> Result<bool, String> {
    match inspect_destination(dest)? {
        DestinationState::Missing => Ok(false),
        DestinationState::Owned => Ok(true),
        DestinationState::Unowned { detail } => Err(format!(
            "{detail}; refusing to replace a folder pkg does not own"
        )),
    }
}

/// A valid marker is a regular file (not a symlink) with the exact payload.
fn marker_is_valid(dest: &Path) -> bool {
    let marker = dest.join(MARKER_NAME);
    match fs::symlink_metadata(&marker) {
        Ok(meta) => {
            meta.file_type().is_file()
                && fs::read_to_string(&marker).is_ok_and(|text| text == MARKER_PAYLOAD)
        }
        Err(_) => false,
    }
}

/// Collects intact `.app` bundles from all *active* store outputs.
///
/// A bundle counts as intact when it is a directory whose name ends in
/// `.app` and that contains a `Contents` directory. Inactive profile
/// entries are skipped. Two active packages providing the same basename
/// are a hard error, because the launcher name would be ambiguous.
fn scan_apps(
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut apps: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (entry_id, entry) in entries {
        if !entry.active {
            continue;
        }
        let mut store_paths = entry.store_paths.clone();
        store_paths.sort();
        for store_path in &store_paths {
            let applications = Path::new(store_path).join("Applications");
            let read = match fs::read_dir(&applications) {
                Ok(read) => read,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "could not read {} (profile entry {entry_id}): {e}",
                        applications.display()
                    ));
                }
            };
            for item in read {
                let item =
                    item.map_err(|e| format!("could not read {}: {e}", applications.display()))?;
                let path = item.path();
                let name = match item.file_name().into_string() {
                    Ok(name) => name,
                    Err(_) => continue,
                };
                if !name.ends_with(".app") || name == ".app" {
                    continue;
                }
                let is_bundle_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && path.join("Contents").is_dir();
                if !is_bundle_dir {
                    continue;
                }
                if let Some(existing) = apps.get(&name) {
                    if existing != &path {
                        return Err(format!(
                            "two active packages provide Applications/{name}: {} and {}; \
                             remove one before syncing app launchers",
                            existing.display(),
                            path.display()
                        ));
                    }
                    continue;
                }
                apps.insert(name, path);
            }
        }
    }
    Ok(apps)
}

/// Returns the helper binary, installing the pinned reference first if the
/// helper profile is empty.
///
/// The profile is read, installed into and re-read only when empty, and
/// then checked by one verifier: exactly one active entry pinned to the
/// helper repository and revision, with the helper binary present. Native
/// entry names are opaque (Determinate macOS uses `originalUrl#attrPath`
/// keys), so identity is verified from the entry's own native fields
/// instead of its map key. Anything else is an error for the user to
/// resolve; an existing profile is never blindly reused and never silently
/// modified.
fn ensure_helper(nix: &Nix, paths: &Paths) -> Result<PathBuf, String> {
    let profile = &paths.helper_profile;
    let mut entries = nix
        .profile_list(profile)
        .map_err(|e| format!("could not list helper profile {}: {e}", profile.display()))?;
    if entries.is_empty() {
        nix.profile_add(profile, &[HELPER_FLAKE_REF.to_string()])
            .map_err(|e| format!("could not install helper {HELPER_FLAKE_REF}: {e}"))?;
        entries = nix.profile_list(profile).map_err(|e| {
            format!(
                "could not re-read helper profile {}: {e}",
                profile.display()
            )
        })?;
        if entries.is_empty() {
            return Err(format!(
                "installing {HELPER_FLAKE_REF} produced no profile entry"
            ));
        }
    }
    verify_pinned_helper(profile, &entries)
}

/// Verify the helper profile holds exactly one active entry pinned to the
/// helper repository and revision, and return its binary.
///
/// One verifier serves the freshly installed and the pre-existing profile
/// alike; the entry-count check applies to both.
fn verify_pinned_helper(
    profile: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<PathBuf, String> {
    if entries.len() != 1 {
        let ids: Vec<String> = entries.keys().cloned().collect();
        return Err(format!(
            "helper profile {} contains unexpected entries: [{}]; \
             remove what you do not want and run `pkg apps sync` again",
            profile.display(),
            ids.join(", ")
        ));
    }
    let Some(entry) = entries.values().next() else {
        return Err(format!(
            "installing {HELPER_FLAKE_REF} produced no profile entry"
        ));
    };
    if !entry.active {
        return Err(format!(
            "helper profile {} holds an inactive entry; remove it and rerun \
             `pkg apps sync`",
            profile.display()
        ));
    }
    if !entry.original_url.starts_with(HELPER_REPO_PREFIX)
        || entry.locked_revision() != Some(HELPER_REVISION)
    {
        return Err(format!(
            "helper profile {} does not hold the pinned helper {HELPER_FLAKE_REF} \
             (found original {}, locked revision {}); remove the entry and rerun \
             `pkg apps sync`",
            profile.display(),
            entry.original_url,
            entry.locked_revision().unwrap_or("none")
        ));
    }
    helper_binary_in(&entry.store_paths).ok_or_else(|| {
        format!(
            "helper profile entry in {} has no {HELPER_BINARY_RELATIVE} binary in its \
             store paths",
            profile.display()
        )
    })
}

/// Finds the helper executable inside the entry's store outputs.
fn helper_binary_in(store_paths: &[String]) -> Option<PathBuf> {
    store_paths.iter().map(Path::new).find_map(|p| {
        let candidate = p.join(HELPER_BINARY_RELATIVE);
        candidate.is_file().then_some(candidate)
    })
}

/// Runs the one helper command this module needs. `sync-trampolines`
/// rebuilds the destination from the source view and refreshes Dock
/// entries for the trampolined apps. The command goes through the shared
/// signal forward/reap boundary, so an interrupted run is reaped and
/// reported here instead of killing pkg.
fn run_sync_trampolines(helper: &Path, from: &Path, to: &Path) -> Result<(), String> {
    let args = [
        String::from("sync-trampolines"),
        from.to_string_lossy().into_owned(),
        to.to_string_lossy().into_owned(),
    ];
    match run_direct(helper, &args) {
        Ok(Outcome::Success) => Ok(()),
        Ok(Outcome::Interrupted { signal }) => Err(format!(
            "mac-app-util sync-trampolines was interrupted by signal {signal} \
             ({} -> {}); the launcher folder may be partially replaced; \
             run `pkg apps sync` again",
            from.display(),
            to.display()
        )),
        Ok(Outcome::Failed { status, stderr }) => Err(format!(
            "mac-app-util sync-trampolines failed ({} -> {}; {}): {}",
            from.display(),
            to.display(),
            status,
            stderr.trim()
        )),
        Err(detail) => Err(format!("could not run {}: {detail}", helper.display())),
    }
}

/// Removes stale trampolines from the marker-verified owned destination.
///
/// Dock entries that pointed at removed launchers are not refreshed here;
/// Dock cleanup is the helper's behavior, not pkg's.
fn clear_launchers(dest: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(dest).map_err(|e| format!("could not read {}: {e}", dest.display()))?
    {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", dest.display()))?;
        if let Ok(name) = entry.file_name().into_string()
            && name.ends_with(".app")
        {
            let path = entry.path();
            fs::remove_dir_all(&path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Writes the ownership marker into the destination.
fn write_marker(dest: &Path) -> Result<(), String> {
    let marker = dest.join(MARKER_NAME);
    let mut file =
        File::create(&marker).map_err(|e| format!("could not create {}: {e}", marker.display()))?;
    file.write_all(MARKER_PAYLOAD.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", marker.display()))
}

/// Best-effort marker restoration after a partial helper failure, so the
/// destination stays recognized as pkg-owned and a retry can proceed.
fn restore_marker(dest: &Path) {
    if dest.is_dir() && !marker_is_valid(dest) {
        let _ = write_marker(dest);
    }
}

/// Records the derived launcher-name to source-bundle map.
fn write_source_map(dest: &Path, apps: &BTreeMap<String, PathBuf>) -> Result<(), String> {
    let mut text = String::new();
    for (name, source) in apps {
        text.push_str(&format!("{name}\t{}\n", source.display()));
    }
    fs::write(dest.join(SOURCE_MAP_NAME), text).map_err(|e| {
        format!(
            "could not write {}: {e}",
            dest.join(SOURCE_MAP_NAME).display()
        )
    })
}

/// Reads the derived launcher-name to source-bundle map, if present.
fn read_source_map(dest: &Path) -> Result<Option<BTreeMap<String, PathBuf>>, String> {
    let path = dest.join(SOURCE_MAP_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((name, source)) = line.split_once('\t') {
            map.insert(name.to_string(), PathBuf::from(source));
        }
    }
    Ok(Some(map))
}

/// Advisory lock that serializes `apps sync` runs within one state home.
///
/// Uses the standard-library file lock (stable since Rust 1.89; the
/// workspace pins 1.96.1); no new dependency beyond `tempfile`. The lock
/// is released when the guard is dropped, including on error paths.
struct SyncLock {
    file: File,
}

impl SyncLock {
    fn acquire(paths: &Paths) -> Result<Self, String> {
        let lock_path = paths.state_home.join(SYNC_LOCK_NAME);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let file = File::create(&lock_path)
            .map_err(|e| format!("could not open {}: {e}", lock_path.display()))?;
        file.lock()
            .map_err(|e| format!("could not lock {}: {e}", lock_path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_inspection_separates_missing_owned_and_unowned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("pkg");

        assert!(matches!(
            inspect_destination(&dest),
            Ok(DestinationState::Missing)
        ));

        // A folder without the marker is unowned, not an operational error.
        fs::create_dir(&dest).expect("create destination");
        assert!(matches!(
            inspect_destination(&dest),
            Ok(DestinationState::Unowned { .. })
        ));

        // The write guard still refuses the unowned shape.
        assert!(ensure_owned_destination(&dest).is_err());

        // A valid marker claims the folder for pkg.
        write_marker(&dest).expect("write marker");
        assert!(matches!(
            inspect_destination(&dest),
            Ok(DestinationState::Owned)
        ));
        assert_eq!(ensure_owned_destination(&dest), Ok(true));
    }
}
