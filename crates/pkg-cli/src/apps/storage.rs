//! Private app storage on the Nix volume.
use crate::nix::{Outcome, run_direct};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
/// Root-owned parent of private per-user app views on the Nix volume.
const APP_CACHE_PARENT: &str = "/nix/var/pkg/cask-apps";

/// The real user's identity, matching `os.getuid()` in the app materializer.
fn app_uid() -> u32 {
    // SAFETY: getuid has no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Shared with the app runtime. Custom roots must already be private and
/// on the payload's volume; preparation validates them before any write.
pub(super) fn app_cache_root() -> Result<PathBuf, String> {
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
/// # Errors
///
/// Returns an error when the host is not macOS, the caller is root, storage is unsafe or off-volume, or setup commands and file operations fail or are interrupted.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
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
}
