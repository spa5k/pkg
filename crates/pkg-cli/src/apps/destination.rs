//! Launcher destination ownership and derived source records.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
/// Launcher destination, relative to the user's home directory. The only
/// launcher directory this module manages.
const DEST_RELATIVE: &str = "Applications/pkg";
/// Marker file that claims the destination for `pkg`; its exact payload
/// must be present before any destination content is replaced.
const MARKER_NAME: &str = ".pkg-owned";
/// Payload required inside [`MARKER_NAME`].
const MARKER_PAYLOAD: &str = "pkg-apps-owned/1\n";
/// Derived record of launcher name to source bundle, written after a
/// successful sync and compared by [`super::status`]. It is derived state, not
/// package inventory; it is preserved on failed syncs.
const SOURCE_MAP_NAME: &str = ".pkg-launcher-sources";
/// The user's launcher destination: `$HOME/Applications/pkg`.
pub(super) fn destination_path() -> Result<PathBuf, String> {
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
pub(super) enum DestinationState {
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
pub(super) fn inspect_destination(dest: &Path) -> Result<DestinationState, String> {
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

/// Require a missing or pkg-owned destination before writing.
///
/// Only two states are acceptable: the destination does not exist, or it
/// exists with a regular, non-symlink [`MARKER_NAME`] carrying exactly
/// [`MARKER_PAYLOAD`]. Any other pre-existing directory — empty or not,
/// symlinked or not — is refused, so this module never replaces a folder
/// it does not own.
pub(super) fn ensure_owned_destination(dest: &Path) -> Result<(), String> {
    match inspect_destination(dest)? {
        DestinationState::Missing | DestinationState::Owned => Ok(()),
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

/// Removes stale trampolines from the marker-verified owned destination.
///
/// Dock entries that pointed at removed launchers are not refreshed here;
/// Dock cleanup is the helper's behavior, not pkg's.
pub(super) fn clear_launchers(dest: &Path) -> Result<(), String> {
    for entry in
        fs::read_dir(dest).map_err(|e| format!("could not read {}: {e}", dest.display()))?
    {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", dest.display()))?;
        if let Ok(name) = entry.file_name().into_string()
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension == "app")
        {
            let path = entry.path();
            fs::remove_dir_all(&path)
                .map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Writes the ownership marker into the destination.
pub(super) fn write_marker(dest: &Path) -> Result<(), String> {
    let marker = dest.join(MARKER_NAME);
    fs::write(&marker, MARKER_PAYLOAD)
        .map_err(|e| format!("could not write {}: {e}", marker.display()))
}

/// Best-effort marker restoration after a partial helper failure, so the
/// destination stays recognized as pkg-owned and a retry can proceed.
pub(super) fn restore_marker(dest: &Path) {
    if dest.is_dir() && !marker_is_valid(dest) {
        let _ = write_marker(dest);
    }
}

/// Records the derived launcher-name to source-bundle map.
pub(super) fn write_source_map(
    dest: &Path,
    apps: &BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    let mut text = String::new();
    for (name, source) in apps {
        use std::fmt::Write as _;
        let _ = writeln!(text, "{name}\t{}", source.display());
    }
    fs::write(dest.join(SOURCE_MAP_NAME), text).map_err(|e| {
        format!(
            "could not write {}: {e}",
            dest.join(SOURCE_MAP_NAME).display()
        )
    })
}

/// Reads the derived launcher-name to source-bundle map, if present.
pub(super) fn read_source_map(dest: &Path) -> Result<Option<BTreeMap<String, PathBuf>>, String> {
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
        assert_eq!(ensure_owned_destination(&dest), Ok(()));
    }
}
