//! Narrow atomic filesystem helpers for tap publication.
//!
//! A published tap root is a stable REAL directory, never a symlink: Nix
//! rejects `path:` references whose target is a symlink. Replacing an
//! existing generation uses one atomic directory exchange — Linux
//! `renameat2(RENAME_EXCHANGE)`, macOS `renameatx_np(RENAME_SWAP)` — so the
//! active generation is committed by a single rename with no window that
//! mixes two generations. There is deliberately no multi-rename fallback:
//! a filesystem without the exchange syscall fails publication instead of
//! risking a missing-path window between renames.

use std::io;
use std::path::Path;

/// Atomically exchange two directories on one filesystem.
///
/// Both paths must exist before the call; after a successful exchange each
/// path names the other's former directory. The caller verifies both paths
/// first and holds the per-source lock, so no third party renames between
/// the check and the exchange.
/// # Errors
///
/// Returns an error when either path is not a real directory, contains a NUL byte, or the platform exchange operation fails.
pub fn exchange_directories(a: &Path, b: &Path) -> Result<(), String> {
    // Everything fallible — the real-directory checks and the C string
    // conversions — happens before the syscall. Nothing fallible runs
    // after it: a post-commit check that failed would leave the caller
    // unable to tell whether the exchange happened, and an undo attempt
    // could then delete the wrong generation.
    verify_directory(a)?;
    verify_directory(b)?;
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        a,
        rustix::fs::CWD,
        b,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|error| {
        let error = io::Error::from(error);
        format!(
            "cannot atomically exchange {} and {}: {error}",
            a.display(),
            b.display()
        )
    })
}

/// Refuse anything that is not a real directory (a symlink never counts).
pub(crate) fn verify_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("{} is not readable: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "{} is not a real directory; a published tap root \
             must never be a symlink",
            path.display()
        ));
    }
    Ok(())
}

/// Flush one directory's entries to disk after renames, best effort.
///
/// Durability of the published generation matters more than the latency of
/// the flush, but a filesystem that refuses directory fsync must not fail
/// an otherwise complete publication.
pub fn fsync_directory(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Ok(directory) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
    {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_swaps_two_real_directories_atomically() {
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir(&a).expect("mkdir a");
        std::fs::create_dir(&b).expect("mkdir b");
        std::fs::write(a.join("marker"), "new").expect("write");
        std::fs::write(b.join("marker"), "old").expect("write");
        exchange_directories(&a, &b).expect("exchanges");
        assert_eq!(
            std::fs::read_to_string(a.join("marker")).expect("read"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("marker")).expect("read"),
            "new"
        );

        // Missing paths and non-directories are refused, never exchanged.
        let missing = root.path().join("missing");
        assert!(exchange_directories(&a, &missing).is_err());
        let file = root.path().join("file");
        std::fs::write(&file, "x").expect("write");
        assert!(exchange_directories(&a, &file).is_err());
        // A symlink is not a real directory even when its target is one.
        let link = root.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&a, &link).expect("symlink");
        #[cfg(unix)]
        assert!(exchange_directories(&a, &link).is_err());
    }

    #[test]
    // Only Linux: APFS rejects creating filenames with invalid UTF-8
    // bytes (EILSEQ), so the swap itself cannot run there.
    #[cfg(target_os = "linux")]
    fn exchange_swaps_directories_with_non_utf8_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join(OsString::from_vec(b"g\xffn-new".to_vec()));
        let b = root.path().join(OsString::from_vec(b"g\xffn-old".to_vec()));
        std::fs::create_dir(&a).expect("mkdir a");
        std::fs::create_dir(&b).expect("mkdir b");
        std::fs::write(a.join("marker"), "new").expect("write");
        std::fs::write(b.join("marker"), "old").expect("write");
        exchange_directories(&a, &b).expect("exchanges non-UTF-8 directories");
        assert_eq!(
            std::fs::read_to_string(a.join("marker")).expect("read"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("marker")).expect("read"),
            "new"
        );
        fsync_directory(&a);
    }
}
