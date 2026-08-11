//! Root-only durable file adapter for macOS store-provisioning snapshots.

use crate::store_journal::MacOsStoreProvisionJournal;
use exacl::getfacl;
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl, open},
    sys::stat::Mode,
    unistd::{Gid, Uid, fchown},
};
use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const JOURNAL_PARENT: &str = "/Library/Application Support/pkg/managed-nix";
const JOURNAL_PATH: &str = "/Library/Application Support/pkg/managed-nix/store-provision-v1.json";
const MAX_JOURNAL_BYTES: u64 = 4096;
const TEMP_PREFIX: &str = ".store-provision-v1.json.tmp.";
const MAX_PARENT_ENTRIES: usize = 64;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Stable failures at the fixed root-only journal persistence boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreJournalFileError {
    /// Existing parent or journal state was unsafe or malformed.
    InvalidState,
    /// A durable write, replacement, sync, or removal failed.
    PersistenceFailed,
}

impl fmt::Display for MacOsStoreJournalFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS store journal persistence failed")
    }
}

impl Error for MacOsStoreJournalFileError {}

/// Closed access to the compiled root:wheel journal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsStoreJournalStorage;

impl MacOsStoreJournalStorage {
    /// Loads the current strict snapshot, or `None` when no journal exists.
    ///
    /// # Errors
    ///
    /// Returns `InvalidState` for unsafe or malformed filesystem state.
    pub fn load() -> Result<Option<MacOsStoreProvisionJournal>, MacOsStoreJournalFileError> {
        load_at(Path::new(JOURNAL_PARENT), Path::new(JOURNAL_PATH), 0, 0)
    }

    /// Creates the first snapshot without overwriting existing state.
    ///
    /// # Errors
    ///
    /// Returns a stable failure unless the parent is exact and the durable create succeeds.
    pub fn create(journal: &MacOsStoreProvisionJournal) -> Result<(), MacOsStoreJournalFileError> {
        persist_at(
            Path::new(JOURNAL_PARENT),
            Path::new(JOURNAL_PATH),
            journal,
            0,
            0,
            true,
        )
    }

    /// Atomically replaces an existing validated snapshot and syncs its parent.
    ///
    /// # Errors
    ///
    /// Returns a stable failure unless old and new state satisfy the closed contract.
    pub fn replace(journal: &MacOsStoreProvisionJournal) -> Result<(), MacOsStoreJournalFileError> {
        persist_at(
            Path::new(JOURNAL_PARENT),
            Path::new(JOURNAL_PATH),
            journal,
            0,
            0,
            false,
        )
    }

    /// Durably removes a validated snapshot; absence is idempotent.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for unsafe state or an incomplete remove/sync.
    pub fn remove() -> Result<(), MacOsStoreJournalFileError> {
        remove_at(Path::new(JOURNAL_PARENT), Path::new(JOURNAL_PATH), 0, 0)
    }
}

fn load_at(
    parent: &Path,
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<Option<MacOsStoreProvisionJournal>, MacOsStoreJournalFileError> {
    validate_directory(parent, expected_owner, expected_group)?;
    reconcile_linked_temporary(parent, path, expected_owner, expected_group)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(MacOsStoreJournalFileError::InvalidState),
        Ok(_) => {}
    }
    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    let mut file = File::from(fd);
    validate_file(&file, path, expected_owner, expected_group, 1)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(MacOsStoreJournalFileError::InvalidState);
    }
    MacOsStoreProvisionJournal::decode(&bytes)
        .map(Some)
        .map_err(|_| MacOsStoreJournalFileError::InvalidState)
}

fn persist_at(
    parent: &Path,
    path: &Path,
    journal: &MacOsStoreProvisionJournal,
    expected_owner: u32,
    expected_group: u32,
    create: bool,
) -> Result<(), MacOsStoreJournalFileError> {
    validate_directory(parent, expected_owner, expected_group)?;
    let existing = load_at(parent, path, expected_owner, expected_group)?;
    if create != existing.is_none() {
        return Err(MacOsStoreJournalFileError::InvalidState);
    }
    let bytes = journal
        .encode()
        .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    let temporary = temporary_path(parent);
    let result =
        write_private_file(&temporary, &bytes, expected_owner, expected_group).and_then(|()| {
            if create {
                fs::hard_link(&temporary, path)
                    .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
                fs::remove_file(&temporary)
                    .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
            } else {
                fs::rename(&temporary, path)
                    .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
            }
            sync_directory(parent)?;
            if load_at(parent, path, expected_owner, expected_group)?.as_ref() != Some(journal) {
                return Err(MacOsStoreJournalFileError::PersistenceFailed);
            }
            Ok(())
        });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_at(
    parent: &Path,
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsStoreJournalFileError> {
    if load_at(parent, path, expected_owner, expected_group)?.is_none() {
        return Ok(());
    }
    fs::remove_file(path).map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    sync_directory(parent)
}

fn validate_directory(
    parent: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsStoreJournalFileError> {
    let metadata =
        fs::symlink_metadata(parent).map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_owner
        || metadata.gid() != expected_group
        || metadata.mode() & 0o7777 != 0o700
        || !getfacl(parent, None).is_ok_and(|acl| acl.is_empty())
    {
        return Err(MacOsStoreJournalFileError::InvalidState);
    }
    Ok(())
}

fn validate_file(
    file: &File,
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    expected_links: u64,
) -> Result<(), MacOsStoreJournalFileError> {
    let metadata = file
        .metadata()
        .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    if !metadata.is_file()
        || metadata.uid() != expected_owner
        || metadata.gid() != expected_group
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != expected_links
        || metadata.len() > MAX_JOURNAL_BYTES
        || !getfacl(path, None).is_ok_and(|acl| acl.is_empty())
    {
        return Err(MacOsStoreJournalFileError::InvalidState);
    }
    Ok(())
}

fn write_private_file(
    path: &Path,
    bytes: &[u8],
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsStoreJournalFileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    fchown(
        &file,
        Some(Uid::from_raw(expected_owner)),
        Some(Gid::from_raw(expected_group)),
    )
    .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    file.write_all(bytes)
        .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    full_sync(&file)?;
    validate_file(&file, path, expected_owner, expected_group, 1)
}

fn sync_directory(path: &Path) -> Result<(), MacOsStoreJournalFileError> {
    let directory = File::open(path).map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    full_sync(&directory)
}

pub fn full_sync(file: &File) -> Result<(), MacOsStoreJournalFileError> {
    fcntl(file, FcntlArg::F_FULLFSYNC)
        .map(|_| ())
        .map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)
}

fn temporary_path(parent: &Path) -> PathBuf {
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{TEMP_PREFIX}{}.{sequence}", std::process::id()))
}

fn reconcile_linked_temporary(
    parent: &Path,
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsStoreJournalFileError> {
    let target = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 2 => metadata,
        Ok(_) | Err(_) => return Ok(()),
    };
    let mut matching = None;
    for (index, entry) in fs::read_dir(parent)
        .map_err(|_| MacOsStoreJournalFileError::InvalidState)?
        .enumerate()
    {
        if index >= MAX_PARENT_ENTRIES {
            return Err(MacOsStoreJournalFileError::InvalidState);
        }
        let entry = entry.map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let candidate = entry.path();
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
        if metadata.dev() == target.dev()
            && metadata.ino() == target.ino()
            && matching.replace(candidate).is_some()
        {
            return Err(MacOsStoreJournalFileError::InvalidState);
        }
    }
    let temporary = matching.ok_or(MacOsStoreJournalFileError::InvalidState)?;
    let fd = open(
        &temporary,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| MacOsStoreJournalFileError::InvalidState)?;
    let file = File::from(fd);
    validate_file(&file, &temporary, expected_owner, expected_group, 2)?;
    fs::remove_file(temporary).map_err(|_| MacOsStoreJournalFileError::PersistenceFailed)?;
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> Result<(tempfile::TempDir, PathBuf, u32, u32), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        fs::set_permissions(directory.path(), Permissions::from_mode(0o700))?;
        let path = directory.path().join("journal.json");
        Ok((
            directory,
            path,
            Uid::effective().as_raw(),
            Gid::effective().as_raw(),
        ))
    }

    #[test]
    fn create_replace_load_and_remove_are_private_and_durable() -> Result<(), Box<dyn Error>> {
        let (directory, path, uid, gid) = setup()?;
        let mut journal = MacOsStoreProvisionJournal::new();
        persist_at(directory.path(), &path, &journal, uid, gid, true)?;
        assert_eq!(
            load_at(directory.path(), &path, uid, gid)?,
            Some(journal.clone())
        );
        assert_eq!(fs::metadata(&path)?.mode() & 0o7777, 0o600);

        journal.intend_synthetic(false, None)?;
        persist_at(directory.path(), &path, &journal, uid, gid, false)?;
        assert_eq!(load_at(directory.path(), &path, uid, gid)?, Some(journal));
        assert!(
            persist_at(
                directory.path(),
                &path,
                &MacOsStoreProvisionJournal::new(),
                uid,
                gid,
                true
            )
            .is_err()
        );

        remove_at(directory.path(), &path, uid, gid)?;
        remove_at(directory.path(), &path, uid, gid)?;
        assert!(load_at(directory.path(), &path, uid, gid)?.is_none());
        Ok(())
    }

    #[test]
    fn unsafe_parent_target_and_hardlink_fail_closed() -> Result<(), Box<dyn Error>> {
        let (directory, path, uid, gid) = setup()?;
        fs::set_permissions(directory.path(), Permissions::from_mode(0o755))?;
        assert!(load_at(directory.path(), &path, uid, gid).is_err());
        fs::set_permissions(directory.path(), Permissions::from_mode(0o700))?;

        let outside = directory.path().join("outside");
        fs::write(&outside, b"outside")?;
        symlink(&outside, &path)?;
        assert!(load_at(directory.path(), &path, uid, gid).is_err());
        fs::remove_file(&path)?;

        persist_at(
            directory.path(),
            &path,
            &MacOsStoreProvisionJournal::new(),
            uid,
            gid,
            true,
        )?;
        let alias = directory.path().join("alias");
        fs::hard_link(&path, alias)?;
        assert!(load_at(directory.path(), &path, uid, gid).is_err());
        Ok(())
    }

    #[test]
    fn malformed_and_non_private_snapshots_fail_closed() -> Result<(), Box<dyn Error>> {
        let (directory, path, uid, gid) = setup()?;
        fs::write(&path, b"{}")?;
        fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        assert!(load_at(directory.path(), &path, uid, gid).is_err());
        fs::write(&path, MacOsStoreProvisionJournal::new().encode()?)?;
        fs::set_permissions(&path, Permissions::from_mode(0o644))?;
        assert!(load_at(directory.path(), &path, uid, gid).is_err());
        Ok(())
    }

    #[test]
    fn interrupted_initial_link_is_reconciled_before_decode() -> Result<(), Box<dyn Error>> {
        let (directory, path, uid, gid) = setup()?;
        let journal = MacOsStoreProvisionJournal::new();
        let temporary = temporary_path(directory.path());
        write_private_file(&temporary, &journal.encode()?, uid, gid)?;
        fs::hard_link(&temporary, &path)?;

        assert_eq!(fs::metadata(&path)?.nlink(), 2);
        assert_eq!(load_at(directory.path(), &path, uid, gid)?, Some(journal));
        assert_eq!(fs::metadata(&path)?.nlink(), 1);
        assert!(!temporary.exists());
        Ok(())
    }
}
