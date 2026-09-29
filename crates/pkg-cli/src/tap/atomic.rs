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

use std::ffi::CString;
use std::io;
use std::path::Path;

/// Atomically exchange two directories on one filesystem.
///
/// Both paths must exist before the call; after a successful exchange each
/// path names the other's former directory. The caller verifies both paths
/// first and holds the per-source lock, so no third party renames between
/// the check and the exchange.
pub fn exchange_directories(a: &Path, b: &Path) -> Result<(), String> {
    // Everything fallible — the real-directory checks and the C string
    // conversions — happens before the syscall. Nothing fallible runs
    // after it: a post-commit check that failed would leave the caller
    // unable to tell whether the exchange happened, and an undo attempt
    // could then delete the wrong generation.
    verify_directory(a)?;
    verify_directory(b)?;
    let a_raw = cpath(a)?;
    let b_raw = cpath(b)?;
    // SAFETY: both pointers are valid C strings for the whole call and the
    // syscall only reads them; no state outlives the call.
    let result = unsafe { exchange_syscall(&a_raw, &b_raw) };
    if result != 0 {
        let error = io::Error::last_os_error();
        return Err(format!(
            "cannot atomically exchange {} and {}: {error}",
            a.display(),
            b.display()
        ));
    }
    Ok(())
}

/// The platform exchange call; zero on success.
///
/// SAFETY: `a` and `b` must be valid NUL-terminated paths for the length
/// of the call.
#[cfg(target_os = "linux")]
unsafe fn exchange_syscall(a: &CString, b: &CString) -> i32 {
    unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    }
}

/// The platform exchange call; zero on success.
///
/// SAFETY: `a` and `b` must be valid NUL-terminated paths for the length
/// of the call.
#[cfg(target_os = "macos")]
unsafe fn exchange_syscall(a: &CString, b: &CString) -> i32 {
    // `renameatx_np` takes its flags as `c_uint` on macOS.
    unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    }
}

/// Convert one path to a C string without NUL bytes inside.
///
/// On Unix the path's raw OS bytes are used, so a path with arbitrary
/// non-UTF-8 bytes converts exactly like a UTF-8 one.
fn cpath(path: &Path) -> Result<CString, String> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let bytes = path
        .to_str()
        .map(|text| text.as_bytes())
        .ok_or_else(|| format!("path {} is not usable as a C path", path.display()))?;
    CString::new(bytes).map_err(|_| format!("path {} contains a NUL byte", path.display()))
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
    if let Ok(raw) = cpath(path) {
        // SAFETY: the pointer is a valid C string for the whole call; the fd
        // is closed immediately after, whatever the fsync result.
        unsafe {
            let fd = libc::open(raw.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY);
            if fd >= 0 {
                libc::fsync(fd);
                libc::close(fd);
            }
        }
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
