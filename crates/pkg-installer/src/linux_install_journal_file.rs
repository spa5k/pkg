//! Root-only durable storage for Linux install recovery snapshots.

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

use nix::unistd::{Gid, Uid, fchown};
use rustix::{
    fs::{
        AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fsync,
        mkdirat, open, openat, renameat, renameat_with, statat, unlinkat,
    },
    io::Errno,
};

use pkg_core::{System, state::Digest};

use crate::LinuxInstallJournal;

const DIRECTORY_NAME: &str = "pkg-install";
const JOURNAL_NAME: &str = "transaction-v1.json";
const TEMP_NAME: &str = ".transaction-v1.json.tmp";
const MAX_JOURNAL_BYTES: u64 = 16 * 1024;

/// Stable failures at the fixed Linux install-journal boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxInstallJournalFileErrorCode {
    /// Existing directory or journal state is unsafe or malformed.
    InvalidState,
    /// A durable create, replace, sync, or removal failed.
    PersistenceFailed,
}

/// Redacted Linux install-journal storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxInstallJournalFileError {
    code: LinuxInstallJournalFileErrorCode,
}

impl LinuxInstallJournalFileError {
    const fn new(code: LinuxInstallJournalFileErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxInstallJournalFileErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxInstallJournalFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux install recovery storage failed")
    }
}

impl Error for LinuxInstallJournalFileError {}

/// Locked access to the fixed root-only Linux install journal.
pub struct LinuxInstallJournalStorage {
    base: File,
    directory: File,
    expected_user_id: u32,
    expected_group_id: u32,
    system: System,
    ownership_manifest_digest: Digest,
    recovery_context_digest: Digest,
}

impl fmt::Debug for LinuxInstallJournalStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxInstallJournalStorage")
            .finish_non_exhaustive()
    }
}

impl LinuxInstallJournalStorage {
    /// Opens and locks an existing production recovery directory without creating it.
    ///
    /// # Errors
    ///
    /// Returns `InvalidState` for unsafe ancestors, ownership, or modes.
    pub fn open_existing(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Option<Self>, LinuxInstallJournalFileError> {
        Self::open_existing_at(
            open_production_base()?,
            0,
            0,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    /// Creates or opens and locks the production recovery directory.
    ///
    /// This must run only after privileged clean-host preflight.
    ///
    /// # Errors
    ///
    /// Returns a stable failure when exact private storage cannot be established.
    pub fn prepare(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalFileError> {
        Self::prepare_at(
            open_production_base()?,
            0,
            0,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    /// Loads the current strict snapshot, or `None` when no snapshot exists.
    ///
    /// # Errors
    ///
    /// Returns `InvalidState` for unsafe or malformed state.
    pub fn load(&self) -> Result<Option<LinuxInstallJournal>, LinuxInstallJournalFileError> {
        self.validate_directory_binding()?;
        self.reconcile_temporary()?;
        self.load_current()
    }

    /// Creates the first durable snapshot without overwriting existing state.
    ///
    /// # Errors
    ///
    /// Returns a stable failure unless no snapshot exists and publication is durable.
    pub fn create(
        &self,
        journal: &LinuxInstallJournal,
    ) -> Result<(), LinuxInstallJournalFileError> {
        self.persist(journal, true)
    }

    /// Atomically replaces an existing validated snapshot.
    ///
    /// # Errors
    ///
    /// Returns a stable failure unless old and new state satisfy the closed contract.
    pub fn replace(
        &self,
        journal: &LinuxInstallJournal,
    ) -> Result<(), LinuxInstallJournalFileError> {
        self.persist(journal, false)
    }

    /// Durably removes the validated snapshot and its empty private directory.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for unsafe state or incomplete removal.
    pub fn remove(self) -> Result<(), LinuxInstallJournalFileError> {
        self.validate_directory_binding()?;
        self.reconcile_temporary()?;
        if self.load_current()?.is_some() {
            unlinkat(&self.directory, JOURNAL_NAME, AtFlags::empty())
                .map_err(|_| persistence_failed())?;
            sync(&self.directory)?;
        }
        self.validate_directory_binding()?;
        unlinkat(&self.base, DIRECTORY_NAME, AtFlags::REMOVEDIR)
            .map_err(|_| persistence_failed())?;
        sync(&self.base)
    }

    #[cfg(test)]
    fn open_existing_for_test(
        base: &Path,
        expected_user_id: u32,
        expected_group_id: u32,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Option<Self>, LinuxInstallJournalFileError> {
        Self::open_existing_at(
            open_trusted_base(base, expected_user_id)?,
            expected_user_id,
            expected_group_id,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    #[cfg(test)]
    fn prepare_for_test(
        base: &Path,
        expected_user_id: u32,
        expected_group_id: u32,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalFileError> {
        Self::prepare_at(
            open_trusted_base(base, expected_user_id)?,
            expected_user_id,
            expected_group_id,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    fn open_existing_at(
        base: File,
        expected_user_id: u32,
        expected_group_id: u32,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Option<Self>, LinuxInstallJournalFileError> {
        let directory = match open_private_directory(&base) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(invalid_state()),
        };
        let storage = Self {
            base,
            directory,
            expected_user_id,
            expected_group_id,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        };
        storage.lock_and_validate()?;
        Ok(Some(storage))
    }

    fn prepare_at(
        base: File,
        expected_user_id: u32,
        expected_group_id: u32,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalFileError> {
        let (directory, created) = match open_private_directory(&base) {
            Ok(directory) => (directory, false),
            Err(Errno::NOENT) => {
                let created = match mkdirat(&base, DIRECTORY_NAME, Mode::from_raw_mode(0o700)) {
                    Ok(()) => true,
                    Err(Errno::EXIST) => false,
                    Err(_) => return Err(persistence_failed()),
                };
                sync(&base)?;
                (
                    open_private_directory(&base).map_err(|_| invalid_state())?,
                    created,
                )
            }
            Err(_) => return Err(invalid_state()),
        };
        let storage = Self {
            base,
            directory,
            expected_user_id,
            expected_group_id,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        };
        if created {
            fchown(
                &storage.directory,
                Some(Uid::from_raw(expected_user_id)),
                Some(Gid::from_raw(expected_group_id)),
            )
            .map_err(|_| persistence_failed())?;
            fchmod(&storage.directory, Mode::from_raw_mode(0o700))
                .map_err(|_| persistence_failed())?;
            sync(&storage.directory)?;
        }
        storage.lock_and_validate()?;
        Ok(storage)
    }

    fn lock_and_validate(&self) -> Result<(), LinuxInstallJournalFileError> {
        flock(&self.directory, FlockOperation::LockExclusive).map_err(|_| persistence_failed())?;
        self.validate_directory_binding()
    }

    fn validate_directory_binding(&self) -> Result<(), LinuxInstallJournalFileError> {
        validate_private_directory(
            &self.directory,
            self.expected_user_id,
            self.expected_group_id,
        )?;
        let linked = statat(&self.base, DIRECTORY_NAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| invalid_state())?;
        let opened = self.directory.metadata().map_err(|_| invalid_state())?;
        #[cfg(target_os = "linux")]
        let same_device = linked.st_dev == opened.dev();
        #[cfg(not(target_os = "linux"))]
        let same_device = u64::try_from(linked.st_dev).ok() == Some(opened.dev());
        if FileType::from_raw_mode(linked.st_mode) != FileType::Directory
            || !same_device
            || linked.st_ino != opened.ino()
        {
            return Err(invalid_state());
        }
        Ok(())
    }

    fn load_current(&self) -> Result<Option<LinuxInstallJournal>, LinuxInstallJournalFileError> {
        let descriptor = match openat(
            &self.directory,
            JOURNAL_NAME,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(invalid_state()),
        };
        let journal = decode_file(
            File::from(descriptor),
            self.expected_user_id,
            self.expected_group_id,
        )?;
        if !journal.matches_binding(
            self.system,
            self.ownership_manifest_digest,
            self.recovery_context_digest,
        ) {
            return Err(invalid_state());
        }
        Ok(Some(journal))
    }

    fn persist(
        &self,
        journal: &LinuxInstallJournal,
        create: bool,
    ) -> Result<(), LinuxInstallJournalFileError> {
        if !journal.matches_binding(
            self.system,
            self.ownership_manifest_digest,
            self.recovery_context_digest,
        ) {
            return Err(invalid_state());
        }
        self.validate_directory_binding()?;
        self.reconcile_temporary()?;
        if create != self.load_current()?.is_none() {
            return Err(invalid_state());
        }
        let bytes = journal.encode().map_err(|_| invalid_state())?;
        if let Err(error) = self.write_temporary(&bytes) {
            let _ = unlinkat(&self.directory, TEMP_NAME, AtFlags::empty());
            return Err(error);
        }
        let publication = if create {
            renameat_with(
                &self.directory,
                TEMP_NAME,
                &self.directory,
                JOURNAL_NAME,
                RenameFlags::NOREPLACE,
            )
        } else {
            renameat(&self.directory, TEMP_NAME, &self.directory, JOURNAL_NAME)
        };
        if publication.is_err() {
            let _ = unlinkat(&self.directory, TEMP_NAME, AtFlags::empty());
            return Err(persistence_failed());
        }
        sync(&self.directory)?;
        if self.load_current()?.as_ref() != Some(journal) {
            return Err(persistence_failed());
        }
        Ok(())
    }

    fn write_temporary(&self, bytes: &[u8]) -> Result<(), LinuxInstallJournalFileError> {
        let descriptor = openat(
            &self.directory,
            TEMP_NAME,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| persistence_failed())?;
        let mut file = File::from(descriptor);
        fchown(
            &file,
            Some(Uid::from_raw(self.expected_user_id)),
            Some(Gid::from_raw(self.expected_group_id)),
        )
        .map_err(|_| persistence_failed())?;
        fchmod(&file, Mode::from_raw_mode(0o600)).map_err(|_| persistence_failed())?;
        file.write_all(bytes).map_err(|_| persistence_failed())?;
        sync(&file)?;
        validate_private_file(&file, self.expected_user_id, self.expected_group_id)
    }

    fn reconcile_temporary(&self) -> Result<(), LinuxInstallJournalFileError> {
        let descriptor = match openat(
            &self.directory,
            TEMP_NAME,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return Ok(()),
            Err(_) => return Err(invalid_state()),
        };
        let file = File::from(descriptor);
        validate_private_file(&file, self.expected_user_id, self.expected_group_id)?;
        unlinkat(&self.directory, TEMP_NAME, AtFlags::empty()).map_err(|_| persistence_failed())?;
        sync(&self.directory)
    }
}

fn decode_file(
    mut file: File,
    expected_user_id: u32,
    expected_group_id: u32,
) -> Result<LinuxInstallJournal, LinuxInstallJournalFileError> {
    validate_private_file(&file, expected_user_id, expected_group_id)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_state())?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_state());
    }
    LinuxInstallJournal::decode(&bytes).map_err(|_| invalid_state())
}

fn open_production_base() -> Result<File, LinuxInstallJournalFileError> {
    let root = open_directory(Path::new("/"))?;
    validate_trusted_directory(&root, 0)?;
    let var = open_child_directory(&root, "var")?;
    validate_trusted_directory(&var, 0)?;
    let lib = open_child_directory(&var, "lib")?;
    validate_trusted_directory(&lib, 0)?;
    Ok(lib)
}

#[cfg(test)]
fn open_trusted_base(
    path: &Path,
    expected_user_id: u32,
) -> Result<File, LinuxInstallJournalFileError> {
    let base = open_directory(path)?;
    validate_trusted_directory(&base, expected_user_id)?;
    Ok(base)
}

fn open_directory(path: &Path) -> Result<File, LinuxInstallJournalFileError> {
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| invalid_state())
}

fn open_child_directory(parent: &File, name: &str) -> Result<File, LinuxInstallJournalFileError> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| invalid_state())
}

fn open_private_directory(parent: &File) -> Result<File, Errno> {
    openat(
        parent,
        DIRECTORY_NAME,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
}

fn validate_trusted_directory(
    directory: &File,
    expected_user_id: u32,
) -> Result<(), LinuxInstallJournalFileError> {
    let metadata = directory.metadata().map_err(|_| invalid_state())?;
    if !metadata.is_dir() || metadata.uid() != expected_user_id || metadata.mode() & 0o022 != 0 {
        return Err(invalid_state());
    }
    Ok(())
}

fn validate_private_directory(
    directory: &File,
    expected_user_id: u32,
    expected_group_id: u32,
) -> Result<(), LinuxInstallJournalFileError> {
    let metadata = directory.metadata().map_err(|_| invalid_state())?;
    if !metadata.is_dir()
        || metadata.uid() != expected_user_id
        || metadata.gid() != expected_group_id
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(invalid_state());
    }
    Ok(())
}

fn validate_private_file(
    file: &File,
    expected_user_id: u32,
    expected_group_id: u32,
) -> Result<(), LinuxInstallJournalFileError> {
    let metadata = file.metadata().map_err(|_| invalid_state())?;
    if !metadata.is_file()
        || metadata.uid() != expected_user_id
        || metadata.gid() != expected_group_id
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > MAX_JOURNAL_BYTES
    {
        return Err(invalid_state());
    }
    Ok(())
}

fn sync(file: &File) -> Result<(), LinuxInstallJournalFileError> {
    fsync(file).map_err(|_| persistence_failed())
}

const fn invalid_state() -> LinuxInstallJournalFileError {
    LinuxInstallJournalFileError::new(LinuxInstallJournalFileErrorCode::InvalidState)
}

const fn persistence_failed() -> LinuxInstallJournalFileError {
    LinuxInstallJournalFileError::new(LinuxInstallJournalFileErrorCode::PersistenceFailed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use pkg_core::{System, state::Digest};
    use tempfile::TempDir;

    use super::*;

    fn identity() -> (u32, u32) {
        (Uid::current().as_raw(), Gid::current().as_raw())
    }

    fn journal(byte: u8) -> LinuxInstallJournal {
        LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([byte; 32]),
            recovery_context(byte),
        )
        .unwrap()
    }

    fn recovery_context(byte: u8) -> Digest {
        Digest::from_bytes([byte.wrapping_add(1); 32])
    }

    fn temporary() -> TempDir {
        let temporary = TempDir::new().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        temporary
    }

    #[test]
    fn create_replace_load_and_remove_are_private_and_durable() {
        let temporary = temporary();
        let (uid, gid) = identity();
        assert!(
            LinuxInstallJournalStorage::open_existing_for_test(
                temporary.path(),
                uid,
                gid,
                System::X8664Linux,
                Digest::from_bytes([1; 32]),
                recovery_context(1),
            )
            .unwrap()
            .is_none()
        );
        let storage = LinuxInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::X8664Linux,
            Digest::from_bytes([1; 32]),
            recovery_context(1),
        )
        .unwrap();
        let first = journal(1);
        storage.create(&first).unwrap();
        assert_eq!(storage.load().unwrap(), Some(first));
        let mut second = journal(1);
        second
            .intend(crate::LinuxInstallMutation::Asset {
                id: "broker-group".to_owned(),
            })
            .unwrap();
        second.complete_created().unwrap();
        storage.replace(&second).unwrap();
        assert_eq!(storage.load().unwrap(), Some(second));

        let directory = temporary.path().join(DIRECTORY_NAME);
        let metadata = fs::metadata(directory.join(JOURNAL_NAME)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        storage.remove().unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn no_overwrite_hardlink_and_symlink_contracts_fail_closed() {
        let temporary = temporary();
        let (uid, gid) = identity();
        let storage = LinuxInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::X8664Linux,
            Digest::from_bytes([3; 32]),
            recovery_context(3),
        )
        .unwrap();
        let first = journal(3);
        storage.create(&first).unwrap();
        assert!(storage.create(&journal(3)).is_err());
        assert_eq!(storage.replace(&journal(4)), Err(invalid_state()));
        assert_eq!(storage.load().unwrap(), Some(first));

        let path = temporary.path().join(DIRECTORY_NAME).join(JOURNAL_NAME);
        fs::hard_link(&path, temporary.path().join("journal-link")).unwrap();
        assert_eq!(storage.load(), Err(invalid_state()));

        fs::remove_file(temporary.path().join("journal-link")).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(temporary.path().join("missing"), &path).unwrap();
        assert_eq!(storage.load(), Err(invalid_state()));
    }

    #[test]
    fn unsafe_directory_and_stale_temporary_are_bounded() {
        let temporary = temporary();
        let (uid, gid) = identity();
        let directory = temporary.path().join(DIRECTORY_NAME);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            LinuxInstallJournalStorage::open_existing_for_test(
                temporary.path(),
                uid,
                gid,
                System::X8664Linux,
                Digest::from_bytes([7; 32]),
                recovery_context(7),
            )
            .is_err()
        );

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let storage = LinuxInstallJournalStorage::open_existing_for_test(
            temporary.path(),
            uid,
            gid,
            System::X8664Linux,
            Digest::from_bytes([7; 32]),
            recovery_context(7),
        )
        .unwrap()
        .unwrap();
        fs::write(directory.join(TEMP_NAME), journal(7).encode().unwrap()).unwrap();
        fs::set_permissions(directory.join(TEMP_NAME), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(storage.load().unwrap(), None);
        assert!(!directory.join(TEMP_NAME).exists());

        fs::write(directory.join(TEMP_NAME), b"invalid").unwrap();
        fs::set_permissions(directory.join(TEMP_NAME), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(storage.load().unwrap(), None);
        assert!(!directory.join(TEMP_NAME).exists());

        std::os::unix::fs::symlink("missing", directory.join(TEMP_NAME)).unwrap();
        assert_eq!(storage.load(), Err(invalid_state()));
        assert!(directory.join(TEMP_NAME).is_symlink());
    }
}
