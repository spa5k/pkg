//! Process locks for logical native profile paths.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

/// An exclusive lock held until the file is closed.
pub struct ProfileLock {
    _file: File,
}

impl ProfileLock {
    /// Lock a profile's sibling file without following its generation link.
    pub fn acquire(profile: &Path) -> Result<Self, String> {
        Self::acquire_waiting(
            profile,
            "Waiting for another pkg command to change your packages. Press Ctrl-C to stop waiting.",
        )
    }

    /// Lock like [`Self::acquire`], and print `waiting` if another process
    /// holds the lock.
    pub fn acquire_waiting(profile: &Path, waiting: &str) -> Result<Self, String> {
        let path = lock_path(profile)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let file = open(&path, true)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                crate::output::note(waiting);
                file.lock().map_err(|error| error.to_string())?;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
        }
        Ok(Self { _file: file })
    }

    /// Check that the lock file for `profile` can be used, without taking
    /// the lock. A missing file is fine: the next change creates it.
    pub fn check(profile: &Path) -> Result<(), String> {
        let path = lock_path(profile)?;
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => open(&path, false).map(drop),
        }
    }
}

fn lock_path(profile: &Path) -> Result<std::path::PathBuf, String> {
    let parent = profile.parent().ok_or("profile has no parent directory")?;
    let mut name = profile
        .file_name()
        .ok_or("profile has no file name")?
        .to_os_string();
    name.push(".pkg-lock");
    Ok(parent.join(name))
}

fn open(path: &Path, create: bool) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            format!(
                "cannot open the lock file {}: {error}. Remove it, then try again",
                path.display()
            )
        })?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no arguments or memory effects.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "the lock file {} has the wrong owner or permissions. Remove it, then try again",
            path.display()
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn check_accepts_a_missing_or_private_lock_and_names_a_broken_one() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("pkg");
        let lock = directory.path().join("pkg.pkg-lock");
        assert_eq!(ProfileLock::check(&profile), Ok(()));
        drop(ProfileLock::acquire(&profile).unwrap());
        assert_eq!(ProfileLock::check(&profile), Ok(()));
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = ProfileLock::check(&profile).unwrap_err();
        assert!(error.contains("wrong owner or permissions"), "{error}");
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        let error = ProfileLock::check(&profile).unwrap_err();
        assert!(
            error.contains(&lock.display().to_string()) && error.contains("Remove it"),
            "{error}"
        );
    }
}
