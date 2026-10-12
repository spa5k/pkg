//! Staged launcher publication with exclusive rename and retained recovery.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::destination::{
    DestinationState, ensure_owned_destination, inspect_destination, write_marker, write_source_map,
};
use super::error::AppError;
use super::helper::run_sync_trampolines;

pub(super) fn prepare_parent(destination: &Path) -> Result<&Path, AppError> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::message("launcher destination has no parent"))?;
    match fs::create_dir(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("could not create launcher parent: {error}").into()),
    }
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "could not inspect launcher parent {}: {error}",
            parent.display()
        )
    })?;
    // SAFETY: geteuid takes no arguments and has no memory effects.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(AppError::message(format!(
            "refusing to publish launchers through an unsafe parent: {}",
            parent.display()
        )));
    }
    Ok(parent)
}

pub(super) fn refresh_staged(
    helper: &Path,
    source_view: &Path,
    destination: &Path,
    apps: &BTreeMap<String, PathBuf>,
) -> Result<(), AppError> {
    let parent = prepare_parent(destination)?;
    let staging = tempfile::tempdir_in(parent)
        .map_err(|error| format!("could not create launcher staging directory: {error}"))?;
    let staged = staging.path().join("launchers");
    run_sync_trampolines(helper, source_view, &staged)?;
    let publish = || {
        let metadata = fs::symlink_metadata(&staged)
            .map_err(|error| format!("could not inspect staged launchers: {error}"))?;
        if !metadata.is_dir() {
            return Err(AppError::message(
                "mac-app-util did not create a plain launcher directory",
            ));
        }
        write_marker(&staged)?;
        write_source_map(&staged, apps)?;
        ensure_owned_destination(destination)?;
        publish_staged(&staged, destination, staging)
    };
    publish().map_err(AppError::with_dock_effect_possible)
}

fn publish_staged(
    staged: &Path,
    destination: &Path,
    staging: tempfile::TempDir,
) -> Result<(), AppError> {
    prepare_parent(destination)?;
    let had_destination = match inspect_destination(destination)? {
        DestinationState::Missing => false,
        DestinationState::Owned => true,
        DestinationState::Unowned { detail } => {
            return Err(format!("{detail}; refusing to publish launchers").into());
        }
    };
    if had_destination {
        rename_exclusive(destination, &staging.path().join("previous-launchers")).map_err(
            |error| format!("could not move the previous launcher folder aside: {error}"),
        )?;
    }
    if let Err(error) = rename_exclusive(staged, destination) {
        let publish_error =
            AppError::message(format!("could not publish staged launchers: {error}"));
        if had_destination {
            return restore_previous(destination, staging, publish_error);
        }
        return Err(publish_error);
    }
    Ok(())
}

fn restore_previous(
    destination: &Path,
    staging: tempfile::TempDir,
    publish_error: AppError,
) -> Result<(), AppError> {
    let backup = staging.path().join("previous-launchers");
    if let Err(error) = rename_exclusive(&backup, destination) {
        let recovery = staging.keep().join("previous-launchers");
        return Err(format!(
            "{publish_error}; could not restore the previous launcher folder: {error}; previous launchers were retained at {}",
            recovery.display()
        ).into());
    }
    Err(publish_error)
}

// Publication and recovery must not overwrite an entry created after inspection.
fn rename_exclusive(source: &Path, destination: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both paths are valid C strings for the call. The syscall reads them only.
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    // SAFETY: both paths are valid C strings for the call. The syscall reads them only.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn exclusive_rename_keeps_existing_empty_directories_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("marker"), "keep source").unwrap();
        fs::create_dir(&destination).unwrap();
        assert_eq!(
            rename_exclusive(&source, &destination).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(source.join("marker").exists());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        fs::remove_dir(&destination).unwrap();
        symlink(&source, &destination).unwrap();
        assert!(rename_exclusive(&source, &destination).is_err());
        assert_eq!(fs::read_link(&destination).unwrap(), source);
        fs::remove_file(&destination).unwrap();
        rename_exclusive(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("marker")).unwrap(),
            "keep source"
        );
        assert!(!source.exists());
    }

    #[test]
    fn failed_publication_restores_the_previous_folder() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("pkg");
        fs::create_dir(&destination).unwrap();
        write_marker(&destination).unwrap();
        fs::write(destination.join("Old.app"), "old").unwrap();
        let staging = tempfile::tempdir_in(root.path()).unwrap();
        let missing = staging.path().join("missing");
        let error = publish_staged(&missing, &destination, staging).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("could not publish staged launchers")
        );
        assert_eq!(
            fs::read_to_string(destination.join("Old.app")).unwrap(),
            "old"
        );
        assert!(matches!(
            inspect_destination(&destination),
            Ok(DestinationState::Owned)
        ));
    }

    #[test]
    fn blocked_recovery_retains_the_old_folder_and_preserves_the_new_entry() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("pkg");
        fs::create_dir(&destination).unwrap();
        let staging = tempfile::tempdir_in(root.path()).unwrap();
        let backup = staging.path().join("previous-launchers");
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("Old.app"), "old").unwrap();
        let error = restore_previous(&destination, staging, AppError::message("publish failed"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("previous launchers were retained at")
        );
        assert_eq!(fs::read_to_string(backup.join("Old.app")).unwrap(), "old");
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert!(matches!(
            inspect_destination(&destination),
            Ok(DestinationState::Unowned { .. })
        ));
    }

    #[test]
    fn staging_refuses_a_symlinked_parent() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let parent = root.path().join("Applications");
        symlink(&actual, &parent).unwrap();
        assert!(prepare_parent(&parent.join("pkg")).is_err());
        assert_eq!(fs::read_dir(actual).unwrap().count(), 0);
    }
}
