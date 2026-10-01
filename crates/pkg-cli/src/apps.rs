//! macOS application exposure for installed packages (NN-06 / design D7).
//!
//! This module turns intact `.app` bundles found in the *active* outputs of
//! the user's native package profile into Spotlight- and Dock-visible
//! launchers under `~/Applications/pkg/`, using the pinned external
//! `mac-app-util` helper from a separate Nix profile. The helper's
//! `sync-trampolines` command runs through the shared signal forward/reap
//! boundary, so launcher and Dock refresh happen together and a cancelled
//! run is reaped and reported. This module never mutates `/Applications`, any
//! other user application folder, or package state. A verified helper with
//! the known macOS 27 SBCL startup failure is replaced once; other failures
//! are reported to the caller without a retry.
//!
//! Platform policy: `.app` exposure is macOS-only. On other platforms
//! [`sync`] fails with an explicit unsupported message when invoked
//! manually, and callers skip automatic refresh. No helper source is
//! bundled; the helper is installed on demand from one exact pinned flake
//! reference and verified by its native profile identity.
//! Externally signed cask data files need a verified private app copy:
//! their attributes are retained as ordinary Nix data and restored by
//! the generated materializer. The native profile still owns inventory.
//!
//! Requires the `tempfile` crate for the unique temporary source view
//! (a normal workspace dependency).
//!
//! macOS assessment gate: entries installed from the user's local public
//! tap store may carry vendor-built `.app` bundles pkg did not build.
//! Before any launcher is exposed, Gatekeeper assessment must be enabled
//! on this system (confirmed read-only with `spctl --status`), and then
//! every such bundle (restored first when required) must pass `codesign` and `spctl`
//! verification in `assess_public_tap_apps`; a failed, disabled, or
//! interrupted check stops the sync before the destination is touched.
//! Passing these checks does not claim a bundle is malware-free.
//! System policy is never changed here: no bypass, no quarantine
//! stripping, no re-signing, no launch.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::config::Paths;
use crate::nix::{Nix, Outcome, run_direct, run_direct_captured};

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
/// The helper's upstream lock selects SBCL 2.6.4, whose static memory
/// address conflicts with macOS 27 on ARM64. Pin Nixpkgs with SBCL 2.6.8
/// (the fix landed in 2.6.6) while retaining the exact helper source.
const HELPER_INPUTS: &[(&str, &str)] = &[(
    "nixpkgs",
    "github:NixOS/nixpkgs/b6c8664de9b6cc07fe5666a29f91884ba81197c4",
)];

/// Apple's code-signature verifier, run read-only on vendor bundles.
const CODESIGN_BIN: &str = "/usr/bin/codesign";
/// The signature check every public tap vendor bundle must pass.
const CODESIGN_CHECK: &str = "codesign --verify --deep --strict";
/// Apple's Gatekeeper assessor, run read-only on vendor bundles.
const SPCTL_BIN: &str = "/usr/sbin/spctl";
/// The Gatekeeper check every public tap vendor bundle must pass.
const SPCTL_CHECK: &str = "spctl --assess --type execute --verbose=4";
/// The read-only Gatekeeper status probe run before any assessment.
const SPCTL_STATUS_CHECK: &str = "spctl --status";
/// The exact stdout line `spctl --status` prints when assessment is on.
const ASSESSMENTS_ENABLED: &str = "assessments enabled";

/// Location of the helper binary inside one of its store outputs.
const HELPER_BINARY_RELATIVE: &str = "bin/mac-app-util";

/// Launcher destination, relative to the user's home directory. The only
/// launcher directory this module manages.
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
/// Builder data for apps whose external signatures require a private copy.
const CASK_APP_MANIFEST: &str = "share/pkg/cask-apps.json";
/// Root-owned parent of private per-user app views on the Nix volume.
const APP_CACHE_PARENT: &str = "/nix/var/pkg/cask-apps";

/// Name of the advisory lock file inside the state home.
const SYNC_LOCK_NAME: &str = "app-sync.lock";

/// The real user's identity, matching os.getuid() in the app materializer.
fn app_uid() -> u32 {
    // SAFETY: getuid has no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Shared with the app runtime. Custom roots must already be private and
/// on the payload's volume; preparation validates them before any write.
fn app_cache_root() -> Result<PathBuf, String> {
    let root = std::env::var_os("PKG_APP_CACHE_DIR").map_or_else(
        || PathBuf::from(APP_CACHE_PARENT).join(app_uid().to_string()),
        PathBuf::from,
    );
    if !root.is_absolute() {
        return Err(String::from(
            "PKG_APP_CACHE_DIR must be an absolute directory",
        ));
    }
    Ok(root)
}

/// Validate a cache location without following its final link.
fn check_storage_directory(path: &Path, owner: u32, private: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    let forbidden = if private { 0o077 } else { 0o022 };
    if !metadata.is_dir()
        || metadata.uid() != owner
        || metadata.permissions().mode() & forbidden != 0
        || (private && metadata.permissions().mode() & 0o700 != 0o700)
    {
        return Err(format!("refusing unsafe app storage: {}", path.display()));
    }
    Ok(())
}

/// Set up the private same-volume cache. This explicit command is the only
/// administrator operation; normal installation and app use stay unprivileged.
pub fn setup_storage() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err(String::from("shared app storage supports macOS only"));
    }
    let uid = app_uid();
    if uid == 0 {
        return Err(String::from(
            "run `pkg apps setup` as your user; it will request administrator access",
        ));
    }
    let root = app_cache_root()?;
    if std::env::var_os("PKG_APP_CACHE_DIR").is_none() {
        for parent in ["/nix", "/nix/var"] {
            check_storage_directory(Path::new(parent), 0, false)?;
        }
        for (path, mode, owner) in [
            (PathBuf::from("/nix/var/pkg"), "0755", 0),
            (PathBuf::from(APP_CACHE_PARENT), "0755", 0),
            (root.clone(), "0700", uid),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(_) => check_storage_directory(&path, owner, owner == uid)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("Preparing app storage at {}…", path.display());
                    let args = vec![
                        String::from("/usr/bin/install"),
                        String::from("-d"),
                        String::from("-m"),
                        String::from(mode),
                        String::from("-o"),
                        owner.to_string(),
                        path.to_string_lossy().into_owned(),
                    ];
                    match run_direct(Path::new("/usr/bin/sudo"), &args)? {
                        Outcome::Success => check_storage_directory(&path, owner, owner == uid)?,
                        Outcome::Failed { status, stderr } => {
                            return Err(format!(
                                "app storage setup failed ({status}): {}",
                                stderr.trim()
                            ));
                        }
                        Outcome::Interrupted { signal } => {
                            return Err(format!(
                                "app storage setup was interrupted by signal {signal}"
                            ));
                        }
                    }
                }
                Err(error) => return Err(format!("could not inspect {}: {error}", path.display())),
            }
        }
    }
    check_storage_directory(&root, uid, true)?;
    if fs::metadata(&root).map_err(|e| e.to_string())?.dev()
        != fs::metadata("/nix/store").map_err(|e| e.to_string())?.dev()
    {
        return Err(String::from("app storage must be on the Nix store volume"));
    }
    Ok(())
}

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
    let _lock = SyncLock::acquire(paths)?;
    let entries = list_profile(nix, &paths.profile)?;
    let apps = scan_apps(&entries)?;
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

/// Collects intact bundles and declared private payloads from active outputs.
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
            for (name, path) in scan_restorable_apps(Path::new(store_path))? {
                if let Some(existing) = apps.insert(name.clone(), path.clone())
                    && existing != path
                {
                    return Err(format!(
                        "two active packages provide {name}; remove one before syncing app launchers"
                    ));
                }
            }
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

/// Read generated app data without executing vendor code or writing state.
fn scan_restorable_apps(output: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let namespace = output
        .join("libexec/pkg-casks")
        .join(output.file_name().ok_or("app output has no name")?);
    let current = namespace.join("apps.json");
    let modern = current.exists();
    let path = if modern {
        current
    } else {
        output.join(CASK_APP_MANIFEST)
    };
    let metadata = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("could not inspect {}: {e}", path.display())),
        Ok(metadata) => metadata,
    };
    if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
        return Err(format!("invalid cask app data: {}", path.display()));
    }
    #[derive(serde::Deserialize)]
    struct AppData {
        source: String,
    }
    #[derive(serde::Deserialize)]
    struct Manifest {
        schema: String,
        apps: BTreeMap<String, AppData>,
    }
    let data: Manifest = serde_json::from_slice(
        &fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("invalid cask app data {}: {e}", path.display()))?;
    let (schema, helper, source_dir) = if modern {
        (
            "pkg-cask-apps/2",
            namespace.join("prepare"),
            format!(
                "libexec/pkg-casks/{}/app-sources",
                output
                    .file_name()
                    .ok_or("app output has no name")?
                    .to_string_lossy()
            ),
        )
    } else {
        (
            "pkg-cask-apps/1",
            output.join("libexec/pkg-cask-app"),
            String::from("libexec/pkg/app-sources"),
        )
    };
    if data.schema != schema || !helper.is_file() {
        return Err(format!("unsupported cask app data: {}", path.display()));
    }
    let mut apps = BTreeMap::new();
    for (name, entry) in data.apps {
        if name == ".app"
            || !name.ends_with(".app")
            || name.contains(['/', '\0', '\n', '\r'])
            || entry.source != format!("{source_dir}/{name}")
        {
            return Err(format!("invalid cask app payload: {name}"));
        }
        let source = output.join(entry.source);
        if !fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir())
            || !source.join("Contents").is_dir()
        {
            return Err(format!("missing cask app payload: {}", source.display()));
        }
        apps.insert(name, source);
    }
    Ok(apps)
}

/// Locate the shared materializer only for the generated private layout.
fn app_output(bundle: &Path) -> Option<&Path> {
    let sources = bundle.parent()?;
    let pkg = sources.parent()?;
    let libexec = pkg.parent()?;
    if sources.file_name()? == "app-sources" && libexec.file_name()? == "pkg-casks" {
        let directory = libexec.parent()?;
        let output = directory.parent()?;
        if directory.file_name()? == "libexec" && pkg.file_name()? == output.file_name()? {
            return Some(output);
        }
        return None;
    }
    if sources.file_name()? != "app-sources"
        || pkg.file_name()? != "pkg"
        || libexec.file_name()? != "libexec"
    {
        return None;
    }
    libexec.parent()
}

/// Read-only destination calculation, shared with the builder runtime.
fn restored_path(bundle: &Path) -> Result<Option<PathBuf>, String> {
    let Some(output) = app_output(bundle) else {
        return Ok(None);
    };
    Ok(Some(
        app_cache_root()?
            .join(output.file_name().ok_or("app output has no name")?)
            .join(bundle.file_name().ok_or("app bundle has no name")?),
    ))
}

/// Prepare and verify a private app copy through the shared child supervisor.
fn prepare_bundle(bundle: &Path) -> Result<PathBuf, String> {
    let Some(expected) = restored_path(bundle)? else {
        return Ok(bundle.to_path_buf());
    };
    let output = app_output(bundle).ok_or("missing cask app output")?;
    let helper = if bundle
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        == Some(std::ffi::OsStr::new("pkg"))
    {
        output.join("libexec/pkg-cask-app")
    } else {
        bundle
            .parent()
            .and_then(Path::parent)
            .ok_or("missing cask namespace")?
            .join("prepare")
    };
    let name = bundle
        .file_name()
        .ok_or("missing cask app name")?
        .to_string_lossy()
        .into_owned();
    match run_direct_captured(&helper, &[String::from("prepare"), name])? {
        (Outcome::Success, text)
            if text.trim() == expected.to_string_lossy() && expected.is_dir() =>
        {
            Ok(expected)
        }
        (Outcome::Success, _) => Err(String::from("app preparation returned an unexpected path")),
        (Outcome::Failed { status, stderr }, _) => Err(format!(
            "app preparation failed ({status}): {}",
            stderr.trim()
        )),
        (Outcome::Interrupted { signal }, _) => Err(format!(
            "app preparation was interrupted by signal {signal}"
        )),
    }
}

/// Active entries installed from this state home's local public tap store.
///
/// An entry counts when its `original_url` is the store's stable
/// `pkg/taps/src/{owner}/{tap}/current` reference, recognized by
/// [`crate::tap::store::source_of_reference`] on path components alone,
/// even if the source was later removed from the registry. Entries from
/// Nixpkgs, self-built outputs, or any other source are not selected,
/// and inactive entries are skipped. Selected entries are cloned, so
/// the caller's map stays intact.
fn public_tap_entries(
    state_home: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> BTreeMap<String, crate::nix::ProfileEntry> {
    let mut selected = BTreeMap::new();
    for (entry_id, entry) in entries {
        if entry.active
            && crate::tap::store::source_of_reference(state_home, &entry.original_url).is_some()
        {
            selected.insert(entry_id.clone(), entry.clone());
        }
    }
    selected
}

/// macOS vendor assessment gate for local public tap apps.
///
/// The selected entries are re-scanned with [`scan_apps`], and when they
/// expose any `.app` bundle, Gatekeeper assessment must first be enabled
/// ([`require_assessments_enabled`]) and then every resulting vendor
/// bundle must pass [`assess_bundle`] before the sync continues. The gate
/// runs before the destination write guard, marker handling, helper
/// installation, and any launcher change, so a failure leaves the previous
/// launchers and the recorded source map exactly as they were. Entries
/// that expose no `.app` bundle (CLI-only outputs) need no assessment, and
/// bundles from other sources (Nixpkgs, self-built, ad-hoc signed) are
/// never assessed here.
fn assess_public_tap_apps(
    state_home: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<(), String> {
    let public = public_tap_entries(state_home, entries);
    if public.is_empty() {
        return Ok(());
    }
    let apps = scan_apps(&public)?;
    if apps.is_empty() {
        // CLI-only public outputs carry no vendor bundle to expose.
        return Ok(());
    }
    // Gatekeeper assessment must be enabled before any vendor bundle is
    // trusted to pass `spctl --assess`: with assessment disabled, that
    // check cannot mean anything.
    require_assessments_enabled()?;
    for (_name, bundle) in apps {
        assess_bundle(&prepare_bundle(&bundle)?)?;
    }
    Ok(())
}

/// Whether a `spctl --status` stdout line confirms enabled assessment.
///
/// Apple's `spctl` prints exactly `assessments enabled` or
/// `assessments disabled`; anything else — including a disabled system,
/// an unknown string, or tool noise — does not confirm the gate and
/// must refuse.
fn confirms_enabled_assessment(stdout: &str) -> bool {
    stdout.trim() == ASSESSMENTS_ENABLED
}

/// Require, read-only, that Gatekeeper assessment is enabled.
///
/// Runs `/usr/sbin/spctl --status` through the shared signal forward/reap
/// boundary and accepts only a successful exit with the exact
/// `assessments enabled` line. A disabled system, an unknown answer, a
/// nonzero exit, or a failed execution refuses; system policy is never
/// changed, bypassed, or recorded — the user enables assessment in System
/// Settings.
fn require_assessments_enabled() -> Result<(), String> {
    let args = [String::from("--status")];
    match run_direct_captured(Path::new(SPCTL_BIN), &args) {
        Ok((Outcome::Success, stdout)) if confirms_enabled_assessment(&stdout) => Ok(()),
        Ok((Outcome::Success, stdout)) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; {SPCTL_STATUS_CHECK} \
             printed {:?} instead of {ASSESSMENTS_ENABLED:?}; pkg will not expose \
             public tap app bundles; enable assessment in System Settings and \
             run `pkg apps sync` again",
            stdout.trim()
        )),
        Ok((Outcome::Interrupted { signal }, _)) => Err(format!(
            "{SPCTL_STATUS_CHECK} was interrupted by signal {signal}; \
             no launcher was changed; run `pkg apps sync` again"
        )),
        Ok((Outcome::Failed { status, stderr }, _)) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; {SPCTL_STATUS_CHECK} \
             failed ({status}): {}; no launcher was changed",
            stderr.trim()
        )),
        Err(detail) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; could not run \
             {SPCTL_STATUS_CHECK}: {detail}; no launcher was changed"
        )),
    }
}

/// Verifies one vendor bundle with Apple's own read-only tools:
/// signature first, Gatekeeper second.
///
/// Nothing is modified: no quarantine attribute is stripped, no
/// signature is changed or re-created, the bundle is never opened, and
/// a failed check has no bypass. Success records only that both checks
/// passed; it does not claim the bundle is free of malware.
fn assess_bundle(bundle: &Path) -> Result<(), String> {
    let bundle_arg = || bundle.to_string_lossy().into_owned();
    run_assessment_check(
        Path::new(CODESIGN_BIN),
        &[
            "--verify".to_string(),
            "--deep".to_string(),
            "--strict".to_string(),
            bundle_arg(),
        ],
        bundle,
        CODESIGN_CHECK,
    )?;
    run_assessment_check(
        Path::new(SPCTL_BIN),
        &[
            "--assess".to_string(),
            "--type".to_string(),
            "execute".to_string(),
            "--verbose=4".to_string(),
            bundle_arg(),
        ],
        bundle,
        SPCTL_CHECK,
    )
}

/// Runs one assessment tool through the shared signal forward/reap
/// boundary and reports an exact outcome for the named bundle.
fn run_assessment_check(
    tool: &Path,
    args: &[String],
    bundle: &Path,
    check: &str,
) -> Result<(), String> {
    match run_direct(tool, args) {
        Ok(Outcome::Success) => Ok(()),
        Ok(Outcome::Interrupted { signal }) => Err(format!(
            "{check} on {} was interrupted by signal {signal}; \
             no launcher was changed; run `pkg apps sync` again",
            bundle.display()
        )),
        Ok(Outcome::Failed { status, stderr }) => Err(format!(
            "{check} rejected {}: {}; {}; no launcher was changed",
            bundle.display(),
            status,
            stderr.trim()
        )),
        Err(detail) => Err(format!(
            "could not run {check} on {}: {detail}",
            bundle.display()
        )),
    }
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
/// resolve. A verified helper that cannot start because of the macOS 27
/// SBCL address conflict is rebuilt with the fixed dependency pin. Other
/// failures do not trigger a repair.
fn ensure_helper(nix: &Nix, paths: &Paths) -> Result<PathBuf, String> {
    let profile = &paths.helper_profile;
    let mut entries = nix
        .profile_list(profile)
        .map_err(|e| format!("could not list helper profile {}: {e}", profile.display()))?;
    if entries.is_empty() {
        nix.profile_add_with_inputs(profile, &[HELPER_FLAKE_REF.to_string()], HELPER_INPUTS)
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
    let helper = verify_pinned_helper(profile, &entries)?;
    if helper_runtime_compatible(&helper)? {
        return Ok(helper);
    }

    eprintln!("Updating app helper for macOS compatibility…");
    // Locked flake references cannot be upgraded by `nix profile upgrade`.
    // Build a fresh native profile and verify it before replacing the old
    // generation. A build or probe failure leaves that profile intact.
    let staging = tempfile::tempdir()
        .map_err(|e| format!("could not create an app helper staging directory: {e}"))?;
    let staged_profile = staging.path().join("helper");
    nix.profile_add_with_inputs(
        &staged_profile,
        &[HELPER_FLAKE_REF.to_string()],
        HELPER_INPUTS,
    )
    .map_err(|e| format!("could not update app helper runtime: {e}"))?;
    let staged_entries = nix
        .profile_list(&staged_profile)
        .map_err(|e| format!("could not read staged app helper profile: {e}"))?;
    let staged_helper = verify_pinned_helper(&staged_profile, &staged_entries)?;
    if !helper_runtime_compatible(&staged_helper)? {
        return Err("app helper still cannot start after updating its SBCL runtime".to_string());
    }
    let store_profile = fs::canonicalize(&staged_profile)
        .map_err(|e| format!("could not resolve staged app helper profile: {e}"))?;
    nix.profile_replace(profile, &store_profile)
        .map_err(|e| format!("could not activate updated app helper: {e}"))?;
    let entries = nix.profile_list(profile).map_err(|e| {
        format!(
            "could not re-read helper profile {}: {e}",
            profile.display()
        )
    })?;
    verify_pinned_helper(profile, &entries)
}

/// Probe only the verified helper. The exact SBCL static-address failure
/// allows one dependency repair; interruption and unrelated failures stop
/// the sync before any launcher is replaced.
fn helper_runtime_compatible(helper: &Path) -> Result<bool, String> {
    match run_direct(helper, &[String::from("--help")])? {
        Outcome::Success => Ok(true),
        Outcome::Failed { stderr, .. }
            if stderr.contains("failed to allocate 1048576 bytes at 0x300100000") =>
        {
            Ok(false)
        }
        Outcome::Failed { status, stderr } => Err(format!(
            "app helper could not start ({status}): {}",
            stderr.trim()
        )),
        Outcome::Interrupted { signal } => Err(format!(
            "app helper check was interrupted by signal {signal}"
        )),
    }
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
    fn private_storage_rejects_shared_permissions_wrong_owner_and_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        fs::create_dir(&cache).expect("create cache");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).expect("private mode");
        assert!(check_storage_directory(&cache, app_uid(), true).is_ok());
        assert!(check_storage_directory(&cache, app_uid().wrapping_add(1), true).is_err());
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).expect("shared mode");
        assert!(check_storage_directory(&cache, app_uid(), true).is_err());
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).expect("private mode");
        let link = dir.path().join("link");
        symlink(&cache, &link).expect("create link");
        assert!(check_storage_directory(&link, app_uid(), true).is_err());
    }

    /// The fake runtime records promotion separately from building, so
    /// failures prove the old helper remains selected.
    fn check_helper_repair(old_error: Option<&str>, build_fails: bool, new_error: Option<&str>) {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_bases(dir.path(), dir.path(), dir.path());
        let promoted = dir.path().join("promoted");
        let built = dir.path().join("built");
        let calls = dir.path().join("calls");
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        for (root, error) in [(&old, old_error), (&new, new_error)] {
            fs::create_dir_all(root.join("bin")).expect("create bin");
            let binary = root.join(HELPER_BINARY_RELATIVE);
            let body = error.map_or_else(
                || String::from("exit 0"),
                |error| format!("printf '%s\\n' '{error}' >&2; exit 1"),
            );
            fs::write(&binary, format!("#!/bin/sh\n{body}\n")).expect("write helper");
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let manifest = |root: &Path| {
            serde_json::json!({
                "version": 3,
                "elements": {
                    "opaque-helper-id": {
                        "active": true,
                        "attrPath": "packages.aarch64-darwin.default",
                        "originalUrl": HELPER_FLAKE_REF,
                        "url": HELPER_FLAKE_REF,
                        "storePaths": [root.display().to_string()]
                    }
                }
            })
        };
        let runtime = dir.path().join("nix");
        fs::write(
            &runtime,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> '{calls}'\n\
                 args=$*\n\
                 case \"$args\" in *--version*) echo 'nix (Nix) 2.35.2'; exit 0;; esac\n\
                 while [ \"$1\" != '--profile' ]; do shift; done\n\
                 profile=$2\n\
                 case \"$args\" in\n\
                 *'profile list'*)\n\
                   if [ \"$profile\" = '{profile}' ] && [ ! -f '{promoted}' ]; then\n\
                     printf '%s\\n' '{old_manifest}'\n\
                   elif [ -f '{built}' ]; then printf '%s\\n' '{new_manifest}'\n\
                   else echo '{{\"version\":3,\"elements\":{{}}}}'; fi;;\n\
                 *'profile add'*)\n\
                   if {build_fails}; then echo 'error: replacement build failed' >&2; exit 1; fi\n\
                   mkdir -p \"$profile\"; touch '{built}';;\n\
                 *'build --no-link'*) touch '{promoted}';;\n\
                 *) echo \"unexpected nix call: $args\" >&2; exit 9;;\n\
                 esac\n",
                calls = calls.display(),
                profile = paths.helper_profile.display(),
                promoted = promoted.display(),
                built = built.display(),
                old_manifest = manifest(&old),
                new_manifest = manifest(&new),
            ),
        )
        .expect("write runtime");
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).expect("chmod");
        let nix = crate::nix::discover(runtime.to_str()).expect("discover runtime");
        let result = ensure_helper(&nix, &paths);
        let expected_repair = old_error
            .is_some_and(|error| error.contains("failed to allocate 1048576 bytes at 0x300100000"));
        let expected_success =
            old_error.is_none() || expected_repair && !build_fails && new_error.is_none();
        assert_eq!(result.is_ok(), expected_success, "{result:?}");
        assert_eq!(promoted.exists(), expected_success && expected_repair);
        if expected_success {
            let root = if expected_repair { &new } else { &old };
            assert_eq!(
                result.expect("repaired helper"),
                root.join(HELPER_BINARY_RELATIVE)
            );
        }
        let calls = fs::read_to_string(calls).expect("read calls");
        assert_eq!(calls.contains("profile add"), expected_repair);
        if expected_repair {
            assert!(
                calls.contains(&format!(
                    "--override-input nixpkgs {} -- {HELPER_FLAKE_REF}",
                    HELPER_INPUTS[0].1
                )),
                "{calls}"
            );
        }
        assert!(!calls.contains("profile remove"), "{calls}");
        assert!(!calls.contains("profile upgrade"), "{calls}");
    }

    #[test]
    fn helper_address_conflict_repairs_only_after_replacement_is_verified() {
        let error = Some("failed to allocate 1048576 bytes at 0x300100000");
        check_helper_repair(error, false, None);
        check_helper_repair(error, true, None);
        check_helper_repair(error, false, Some("replacement could not start"));
    }

    #[test]
    fn unrelated_helper_startup_failure_does_not_modify_profile() {
        check_helper_repair(Some("permission denied"), false, None);
    }

    #[test]
    fn working_helper_is_reused_without_a_profile_change() {
        check_helper_repair(None, true, None);
    }

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

    #[test]
    fn private_cask_payload_is_discovered_without_executing_its_helper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path();
        let bundle = output.join("libexec/pkg/app-sources/Vendor.app");
        fs::create_dir_all(bundle.join("Contents")).expect("bundle");
        fs::create_dir_all(output.join("share/pkg")).expect("data folder");
        fs::write(output.join("libexec/pkg-cask-app"), "must not run").expect("helper");
        let entry = crate::nix::ProfileEntry {
            active: true,
            attr_path: String::from("packages.aarch64-darwin.vendor"),
            original_url: String::new(),
            locked_url: String::new(),
            store_paths: vec![output.display().to_string()],
        };
        let entries = BTreeMap::from([(String::from("opaque-native-id"), entry)]);
        let manifest = |source: &str| {
            serde_json::json!({
                "schema": "pkg-cask-apps/1", "apps": {"Vendor.app": {"source": source}}
            })
        };
        fs::write(
            output.join(CASK_APP_MANIFEST),
            manifest("libexec/pkg/app-sources/Vendor.app").to_string(),
        )
        .expect("manifest");
        assert_eq!(
            scan_apps(&entries).expect("apps"),
            BTreeMap::from([(String::from("Vendor.app"), bundle)])
        );
        fs::write(
            output.join(CASK_APP_MANIFEST),
            manifest("../outside.app").to_string(),
        )
        .expect("invalid manifest");
        assert!(
            scan_apps(&entries)
                .expect_err("unsafe path")
                .contains("invalid cask app payload")
        );
    }

    #[test]
    fn public_tap_selection_takes_only_active_store_entries() {
        // The state home deliberately contains spaces and is never
        // registered in any tap registry: selection must use the real
        // store reference shape, not a hardcoded path or registry lookup.
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");
        let public_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entry = |active: bool, original_url: &str| crate::nix::ProfileEntry {
            active,
            attr_path: "apps.demo".to_string(),
            original_url: original_url.to_string(),
            locked_url: String::new(),
            store_paths: Vec::new(),
        };
        // A tap reference below a different state home must not match.
        let other_home = state.path().join("other home");
        fs::create_dir_all(&other_home).expect("other home");
        let other_url = crate::tap::store::current_ref(&other_home, "acme/tools").expect("ref");
        let entries = BTreeMap::from([
            ("public#apps.demo".to_string(), entry(true, &public_url)),
            ("inactive#apps.demo".to_string(), entry(false, &public_url)),
            (
                "nixpkgs".to_string(),
                entry(true, "github:NixOS/nixpkgs/nixpkgs-unstable"),
            ),
            ("other-home".to_string(), entry(true, &other_url)),
        ]);

        let selected = public_tap_entries(&home, &entries);

        // Only the active entry below this state home's tap store counts;
        // the shape is recognized even though the source was never
        // registered or published, Nixpkgs is not a tap, and a tap-shaped
        // reference below another state home does not match.
        assert_eq!(selected.len(), 1);
        assert!(selected.contains_key("public#apps.demo"));
    }

    #[test]
    fn status_gate_accepts_only_the_exact_enabled_line() {
        // The exact line, with surrounding whitespace, confirms the gate.
        assert!(confirms_enabled_assessment("assessments enabled"));
        assert!(confirms_enabled_assessment("assessments enabled\n"));
        assert!(confirms_enabled_assessment("  assessments enabled  \n"));
        // A disabled system, an unknown string, tool noise, and an empty
        // answer all refuse.
        assert!(!confirms_enabled_assessment("assessments disabled"));
        assert!(!confirms_enabled_assessment("assessments disabled\n"));
        assert!(!confirms_enabled_assessment("Assessments enabled"));
        assert!(!confirms_enabled_assessment("assessments enabled.\n"));
        assert!(!confirms_enabled_assessment(""));
        assert!(!confirms_enabled_assessment("spctl: note\n"));
    }

    #[test]
    fn assessment_gate_is_read_only_and_names_the_failed_bundle() {
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");

        // One fake store output holding a synthetic vendor bundle,
        // referenced by an active local public tap entry.
        let output = state.path().join("store-output");
        let bundle = output.join("Applications").join("Vendor.app");
        fs::create_dir_all(bundle.join("Contents")).expect("synthetic bundle");
        let original_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entries = BTreeMap::from([(
            "public#apps.vendor".to_string(),
            crate::nix::ProfileEntry {
                active: true,
                attr_path: "apps.vendor".to_string(),
                original_url,
                locked_url: String::new(),
                store_paths: vec![output.display().to_string()],
            },
        )]);

        // A pre-existing owned destination with one launcher and map that
        // a failed assessment must leave untouched.
        let dest = state.path().join("dest");
        fs::create_dir_all(dest.join("Old.app")).expect("old launcher");
        fs::write(dest.join("Old.app").join("trampoline"), b"x").expect("old content");
        write_marker(&dest).expect("marker");
        write_source_map(
            &dest,
            &BTreeMap::from([("Old.app".to_string(), PathBuf::from("/old/store/Old.app"))]),
        )
        .expect("source map");

        // The status gate runs first: where `spctl` is absent the run
        // itself fails, and the refusal names the exact check. On a
        // system where the tool exists but assessment is disabled, the
        // refusal names `assessments disabled` instead. Both refuse.
        let error = assess_public_tap_apps(&home, &entries)
            .expect_err("the vendor gate must refuse the synthetic unsigned bundle");
        // The status gate runs first, so on a host without a usable
        // `spctl` the refusal names the status check; where the tool
        // exists and confirms assessment, the bundle's own signature
        // refusal names the signature check instead. Either way the
        // refusal names an exact read-only check and no bypass.
        assert!(
            error.contains(SPCTL_STATUS_CHECK) || error.contains(CODESIGN_CHECK),
            "{error}"
        );
        assert!(!error.contains("bypass"), "{error}");

        // The bundle check also names the bundle and its check: an
        // unsigned synthetic bundle fails the signature check on macOS;
        // where the tool is absent the run itself fails. Both shapes
        // refuse.
        let error = assess_bundle(&bundle).expect_err("an unsigned synthetic bundle must not pass");
        assert!(error.contains("Vendor.app"), "{error}");
        assert!(error.contains(CODESIGN_CHECK), "{error}");

        // Nothing about the destination changed: this proves the gate
        // itself is read-only. It calls the gate directly and does not
        // prove the full sync ordering, which the parent verifies on a VM.
        assert!(dest.join("Old.app").join("trampoline").exists());
        assert!(fs::read_to_string(dest.join(MARKER_NAME)).is_ok_and(|t| t == MARKER_PAYLOAD));
        assert!(dest.join(SOURCE_MAP_NAME).exists());
    }

    #[test]
    fn assessment_skips_nixpkgs_and_cli_only_public_outputs() {
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");
        let public_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entry = |original_url: &str, store_paths: Vec<String>| crate::nix::ProfileEntry {
            active: true,
            attr_path: "apps.demo".to_string(),
            original_url: original_url.to_string(),
            locked_url: String::new(),
            store_paths,
        };

        // An ordinary Nixpkgs entry with an unsigned bundle: not a public
        // tap source, so its bundle is not assessed here.
        let nixpkgs_output = state.path().join("nixpkgs-output");
        let unsigned = nixpkgs_output.join("Applications").join("Unsigned.app");
        fs::create_dir_all(unsigned.join("Contents")).expect("unsigned bundle");

        // A public tap entry whose output is CLI-only: no `.app` bundle,
        // so nothing to assess even though the entry is selected.
        let cli_output = state.path().join("cli-output");
        fs::create_dir_all(cli_output.join("bin")).expect("cli output");

        let entries = BTreeMap::from([
            (
                "nixpkgs".to_string(),
                entry(
                    "github:NixOS/nixpkgs/nixpkgs-unstable",
                    vec![nixpkgs_output.display().to_string()],
                ),
            ),
            (
                "public#apps.cli".to_string(),
                entry(&public_url, vec![cli_output.display().to_string()]),
            ),
        ]);

        assert_eq!(
            assess_public_tap_apps(&home, &entries),
            Ok(()),
            "no codesign check must run for a Nixpkgs bundle or a CLI-only public output"
        );
    }
}
