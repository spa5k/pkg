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
        let parent = profile.parent().ok_or("profile has no parent directory")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let mut name = profile
            .file_name()
            .ok_or("profile has no file name")?
            .to_os_string();
        name.push(".pkg-lock");
        let path = parent.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| format!("cannot open profile lock {}: {error}", path.display()))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        // SAFETY: geteuid has no arguments or memory effects.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(format!("unsafe profile lock: {}", path.display()));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                eprintln!("Waiting for another pkg command: {}", profile.display());
                file.lock().map_err(|error| error.to_string())?;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
        }
        Ok(Self { _file: file })
    }
}
