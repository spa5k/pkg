//! Root-only durable storage for macOS install recovery snapshots.

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

use nix::unistd::{Gid, Uid, fchown};
use pkg_core::{System, state::Digest};
use rustix::{
    fs::{
        AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fsync,
        mkdirat, open, openat, renameat, renameat_with, statat, unlinkat,
    },
    io::Errno,
};

use crate::MacOsInstallJournal;

const DIRECTORY_NAME: &str = "pkg-install";
const JOURNAL_NAME: &str = "macos-transaction-v1.json";
const TEMP_NAME: &str = ".macos-transaction-v1.json.tmp";
const MAX_JOURNAL_BYTES: u64 = 32 * 1024;

/// Stable failures at the fixed macOS install-journal boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsInstallJournalFileError {
    /// Existing storage is unsafe, malformed, foreign, or ambiguous.
    InvalidState,
    /// A durable create, replace, sync, or removal failed.
    PersistenceFailed,
}

impl fmt::Display for MacOsInstallJournalFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS install recovery storage failed")
    }
}

impl Error for MacOsInstallJournalFileError {}

/// Locked access to `/var/db/pkg-install/macos-transaction-v1.json`.
pub struct MacOsInstallJournalStorage {
    base: File,
    directory: File,
    expected_user_id: u32,
    expected_group_id: u32,
    system: System,
    ownership_manifest_digest: Digest,
    recovery_context_digest: Digest,
}

impl fmt::Debug for MacOsInstallJournalStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MacOsInstallJournalStorage")
            .finish_non_exhaustive()
    }
}

impl MacOsInstallJournalStorage {
    /// Opens and locks existing production recovery storage.
    pub fn open_existing(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Option<Self>, MacOsInstallJournalFileError> {
        Self::open_existing_at(
            open_production_base()?,
            0,
            0,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    /// Creates or opens and locks production recovery storage.
    pub fn prepare(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, MacOsInstallJournalFileError> {
        Self::prepare_at(
            open_production_base()?,
            0,
            0,
            system,
            ownership_manifest_digest,
            recovery_context_digest,
        )
    }

    /// Loads and validates the current bound snapshot.
    pub fn load(&self) -> Result<Option<MacOsInstallJournal>, MacOsInstallJournalFileError> {
        self.validate_directory_binding()?;
        self.reconcile_temporary()?;
        self.load_current()
    }

    /// Creates the first durable snapshot without replacement.
    pub fn create(
        &self,
        journal: &MacOsInstallJournal,
    ) -> Result<(), MacOsInstallJournalFileError> {
        self.persist(journal, true)
    }

    /// Atomically replaces the validated current snapshot.
    pub fn replace(
        &self,
        journal: &MacOsInstallJournal,
    ) -> Result<(), MacOsInstallJournalFileError> {
        self.persist(journal, false)
    }

    /// Durably removes the snapshot and empty private directory.
    pub fn remove(self) -> Result<(), MacOsInstallJournalFileError> {
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
    fn prepare_for_test(
        base: &Path,
        expected_user_id: u32,
        expected_group_id: u32,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, MacOsInstallJournalFileError> {
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
    ) -> Result<Option<Self>, MacOsInstallJournalFileError> {
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
    ) -> Result<Self, MacOsInstallJournalFileError> {
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

    fn lock_and_validate(&self) -> Result<(), MacOsInstallJournalFileError> {
        flock(&self.directory, FlockOperation::LockExclusive).map_err(|_| persistence_failed())?;
        self.validate_directory_binding()
    }

    fn validate_directory_binding(&self) -> Result<(), MacOsInstallJournalFileError> {
        validate_private_directory(
            &self.directory,
            self.expected_user_id,
            self.expected_group_id,
        )?;
        let linked = statat(&self.base, DIRECTORY_NAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| invalid_state())?;
        let opened = self.directory.metadata().map_err(|_| invalid_state())?;
        if FileType::from_raw_mode(linked.st_mode) != FileType::Directory
            || u64::try_from(linked.st_dev).ok() != Some(opened.dev())
            || linked.st_ino != opened.ino()
        {
            return Err(invalid_state());
        }
        Ok(())
    }

    fn load_current(&self) -> Result<Option<MacOsInstallJournal>, MacOsInstallJournalFileError> {
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
        journal: &MacOsInstallJournal,
        create: bool,
    ) -> Result<(), MacOsInstallJournalFileError> {
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

    fn write_temporary(&self, bytes: &[u8]) -> Result<(), MacOsInstallJournalFileError> {
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

    fn reconcile_temporary(&self) -> Result<(), MacOsInstallJournalFileError> {
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
) -> Result<MacOsInstallJournal, MacOsInstallJournalFileError> {
    validate_private_file(&file, expected_user_id, expected_group_id)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_state())?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(invalid_state());
    }
    MacOsInstallJournal::decode(&bytes).map_err(|_| invalid_state())
}

fn open_production_base() -> Result<File, MacOsInstallJournalFileError> {
    let root = open_directory(Path::new("/"))?;
    validate_trusted_directory(&root, 0)?;
    let var = open_child_directory(&root, "var")?;
    validate_trusted_directory(&var, 0)?;
    let db = open_child_directory(&var, "db")?;
    validate_trusted_directory(&db, 0)?;
    Ok(db)
}

#[cfg(test)]
fn open_trusted_base(
    path: &Path,
    expected_user_id: u32,
) -> Result<File, MacOsInstallJournalFileError> {
    let base = open_directory(path)?;
    validate_trusted_directory(&base, expected_user_id)?;
    Ok(base)
}

fn open_directory(path: &Path) -> Result<File, MacOsInstallJournalFileError> {
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| invalid_state())
}

fn open_child_directory(parent: &File, name: &str) -> Result<File, MacOsInstallJournalFileError> {
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
) -> Result<(), MacOsInstallJournalFileError> {
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
) -> Result<(), MacOsInstallJournalFileError> {
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
) -> Result<(), MacOsInstallJournalFileError> {
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

fn sync(file: &File) -> Result<(), MacOsInstallJournalFileError> {
    fsync(file).map_err(|_| persistence_failed())
}

const fn invalid_state() -> MacOsInstallJournalFileError {
    MacOsInstallJournalFileError::InvalidState
}

const fn persistence_failed() -> MacOsInstallJournalFileError {
    MacOsInstallJournalFileError::PersistenceFailed
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::TempDir;

    use super::*;

    fn temporary() -> Result<TempDir, Box<dyn Error>> {
        let temporary = TempDir::new()?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        Ok(temporary)
    }

    #[test]
    fn create_replace_load_and_remove_are_private_and_bound() -> Result<(), Box<dyn Error>> {
        let temporary = temporary()?;
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();
        let ownership = Digest::from_bytes([1; 32]);
        let recovery = Digest::from_bytes([2; 32]);
        let storage = MacOsInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::Aarch64Darwin,
            ownership,
            recovery,
        )?;
        let mut journal = MacOsInstallJournal::new(System::Aarch64Darwin, ownership, recovery)?;
        storage.create(&journal)?;
        assert_eq!(storage.load()?, Some(journal.clone()));
        let first = crate::macos_install_journal::install_sequence()
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty sequence"))?;
        journal.intend(first)?;
        storage.replace(&journal)?;
        assert_eq!(storage.load()?, Some(journal));
        assert_eq!(
            fs::metadata(temporary.path().join(DIRECTORY_NAME).join(JOURNAL_NAME))?.mode() & 0o7777,
            0o600
        );
        storage.remove()?;
        assert!(!temporary.path().join(DIRECTORY_NAME).exists());
        Ok(())
    }

    #[test]
    fn foreign_binding_and_unsafe_directory_are_refused() -> Result<(), Box<dyn Error>> {
        let temporary = temporary()?;
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();
        let ownership = Digest::from_bytes([3; 32]);
        let recovery = Digest::from_bytes([4; 32]);
        let storage = MacOsInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::X8664Darwin,
            ownership,
            recovery,
        )?;
        let journal = MacOsInstallJournal::new(System::X8664Darwin, ownership, recovery)?;
        storage.create(&journal)?;
        drop(storage);
        assert!(
            MacOsInstallJournalStorage::prepare_for_test(
                temporary.path(),
                uid,
                gid,
                System::X8664Darwin,
                Digest::from_bytes([9; 32]),
                recovery,
            )?
            .load()
            .is_err()
        );
        fs::set_permissions(
            temporary.path().join(DIRECTORY_NAME),
            fs::Permissions::from_mode(0o755),
        )?;
        assert!(
            MacOsInstallJournalStorage::prepare_for_test(
                temporary.path(),
                uid,
                gid,
                System::X8664Darwin,
                ownership,
                recovery,
            )
            .is_err()
        );
        Ok(())
    }
}
