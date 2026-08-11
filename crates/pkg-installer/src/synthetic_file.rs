//! Closed transactional filesystem adapter for `/private/etc/synthetic.conf`.

use crate::{
    store_journal_file::full_sync,
    synthetic_conf::{MacOsSyntheticConfPlan, plan_macos_synthetic_entry},
};
use exacl::getfacl;
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
    unistd::{Gid, Uid, fchown},
};
use rustix::fs::{RenameFlags, renameat_with};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    ffi::OsStr,
    fmt,
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const CONFIG_PARENT: &str = "/private/etc";
const CONFIG_PATH: &str = "/private/etc/synthetic.conf";
const BACKUP_PARENT: &str = "/Library/Application Support/pkg/managed-nix";
const BACKUP_PATH: &str = "/Library/Application Support/pkg/managed-nix/synthetic-conf-v1.backup";
const EXCHANGE_NAME: &str = ".synthetic.conf.pkg.exchange-v1";
const MAX_CONFIG_BYTES: u64 = 65_536;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Stable failures for closed synthetic-file transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsSyntheticFileError {
    /// Existing config, backup, or parent state was unsafe or conflicting.
    InvalidState,
    /// A durable mutation or verification failed.
    PersistenceFailed,
}

impl fmt::Display for MacOsSyntheticFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS synthetic configuration transaction failed")
    }
}

impl Error for MacOsSyntheticFileError {}

/// Prepared replacement and journal evidence bound to exact pre-state.
#[derive(Clone, PartialEq, Eq)]
pub struct MacOsSyntheticFileTransaction {
    existed: bool,
    backup_sha256: Option<String>,
    original_sha256: Option<String>,
    original: Option<Vec<u8>>,
    changed: bool,
    replacement: Vec<u8>,
}

impl MacOsSyntheticFileTransaction {
    /// Returns whether the config existed before this attempt.
    #[must_use]
    pub const fn existed(&self) -> bool {
        self.existed
    }

    /// Returns the private-backup digest to persist in the write-ahead journal.
    #[must_use]
    pub fn backup_sha256(&self) -> Option<&str> {
        self.backup_sha256.as_deref()
    }

    /// Returns whether applying the transaction replaces the config.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }
}

/// Closed access to the compiled config and private backup paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsSyntheticFileStorage;

impl MacOsSyntheticFileStorage {
    /// Reads exact pre-state, creates a durable private backup when needed, and plans replacement.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for unsafe state, a conflicting plan, or incomplete backup sync.
    pub fn prepare() -> Result<MacOsSyntheticFileTransaction, MacOsSyntheticFileError> {
        prepare_at(
            Path::new(CONFIG_PARENT),
            Path::new(CONFIG_PATH),
            Path::new(BACKUP_PARENT),
            Path::new(BACKUP_PATH),
            0,
            0,
        )
    }

    /// Applies a prepared replacement only when the exact bound pre-state still exists.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for pre-state drift or incomplete replacement durability.
    pub fn apply(
        transaction: &MacOsSyntheticFileTransaction,
    ) -> Result<(), MacOsSyntheticFileError> {
        apply_at(
            Path::new(CONFIG_PARENT),
            Path::new(CONFIG_PATH),
            Path::new(BACKUP_PARENT),
            Path::new(BACKUP_PATH),
            transaction,
            0,
            0,
        )
    }

    /// Restores the exact journaled before-state and durably removes its backup.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for evidence mismatch, foreign current state, or incomplete sync.
    pub fn restore(
        existed: bool,
        backup_sha256: Option<&str>,
    ) -> Result<(), MacOsSyntheticFileError> {
        restore_at(
            Path::new(CONFIG_PARENT),
            Path::new(CONFIG_PATH),
            Path::new(BACKUP_PARENT),
            Path::new(BACKUP_PATH),
            existed,
            backup_sha256,
            0,
            0,
        )
    }

    /// Removes a committed transaction's verified private backup.
    ///
    /// # Errors
    ///
    /// Returns a stable failure unless backup presence and digest match the committed evidence.
    pub fn discard_backup(
        existed: bool,
        backup_sha256: Option<&str>,
    ) -> Result<(), MacOsSyntheticFileError> {
        discard_backup_at(
            Path::new(BACKUP_PARENT),
            Path::new(BACKUP_PATH),
            existed,
            backup_sha256,
            0,
            0,
        )
    }

    /// Returns whether the exact canonical `nix` entry is already installed.
    ///
    /// Unlike `prepare`, this read-only check permits the transaction backup
    /// that remains until the journal's committed success boundary.
    ///
    /// # Errors
    ///
    /// Returns a stable failure for unsafe metadata, unreadable state, or a
    /// malformed/conflicting synthetic configuration.
    pub fn entry_present() -> Result<bool, MacOsSyntheticFileError> {
        validate_directory(Path::new(CONFIG_PARENT), 0, 0, 0o755)?;
        validate_directory(Path::new(BACKUP_PARENT), 0, 0, 0o700)?;
        let bytes = read_optional_file(Path::new(CONFIG_PATH), 0, 0, 0o644, MAX_CONFIG_BYTES)?;
        let plan = plan_macos_synthetic_entry(bytes.as_deref())
            .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
        Ok(!plan.changed())
    }
}

fn prepare_at(
    config_parent: &Path,
    config: &Path,
    backup_parent: &Path,
    backup: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<MacOsSyntheticFileTransaction, MacOsSyntheticFileError> {
    validate_directory(config_parent, expected_owner, expected_group, 0o755)?;
    validate_directory(backup_parent, expected_owner, expected_group, 0o700)?;
    match fs::symlink_metadata(backup) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) | Err(_) => return Err(MacOsSyntheticFileError::InvalidState),
    }
    let original = read_optional_file(
        config,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    )?;
    let plan = plan_macos_synthetic_entry(original.as_deref())
        .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    let original_sha256 = original.as_deref().map(digest);
    let backup_sha256 = original.as_deref().map(digest);
    Ok(transaction(
        original.is_some(),
        backup_sha256,
        original_sha256,
        original,
        &plan,
    ))
}

fn transaction(
    existed: bool,
    backup_sha256: Option<String>,
    original_sha256: Option<String>,
    original: Option<Vec<u8>>,
    plan: &MacOsSyntheticConfPlan,
) -> MacOsSyntheticFileTransaction {
    MacOsSyntheticFileTransaction {
        existed,
        backup_sha256,
        original_sha256,
        original,
        changed: plan.changed(),
        replacement: plan.bytes().to_vec(),
    }
}

fn apply_at(
    config_parent: &Path,
    config: &Path,
    backup_parent: &Path,
    backup: &Path,
    transaction: &MacOsSyntheticFileTransaction,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    validate_directory(config_parent, expected_owner, expected_group, 0o755)?;
    validate_directory(backup_parent, expected_owner, expected_group, 0o700)?;
    let current = read_optional_file(
        config,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    )?;
    if current != transaction.original {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    ensure_backup(
        backup_parent,
        backup,
        transaction,
        expected_owner,
        expected_group,
    )?;
    if !transaction.changed {
        return Ok(());
    }
    replace_file_atomically(
        config_parent,
        config,
        &transaction.replacement,
        transaction.original.as_deref(),
        expected_owner,
        expected_group,
        0o644,
    )?;
    if read_optional_file(
        config,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    )?
    .as_deref()
        != Some(transaction.replacement.as_slice())
    {
        return Err(MacOsSyntheticFileError::PersistenceFailed);
    }
    Ok(())
}

fn ensure_backup(
    backup_parent: &Path,
    backup: &Path,
    transaction: &MacOsSyntheticFileTransaction,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let existing = read_optional_file(
        backup,
        expected_owner,
        expected_group,
        0o600,
        MAX_CONFIG_BYTES,
    )?;
    match (transaction.original.as_deref(), existing.as_deref()) {
        (None, None) => sync_directory(backup_parent),
        (Some(original), None) => {
            write_new_file(backup, original, expected_owner, expected_group, 0o600)?;
            sync_directory(backup_parent)
        }
        (Some(original), Some(bytes)) if bytes == original => sync_directory(backup_parent),
        (Some(original), Some(_)) => {
            remove_backup(backup_parent, backup)?;
            write_new_file(backup, original, expected_owner, expected_group, 0o600)?;
            sync_directory(backup_parent)
        }
        (None, Some(_)) => Err(MacOsSyntheticFileError::InvalidState),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn restore_at(
    config_parent: &Path,
    config: &Path,
    backup_parent: &Path,
    backup: &Path,
    existed: bool,
    backup_sha256: Option<&str>,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    if existed != backup_sha256.is_some() {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    validate_directory(config_parent, expected_owner, expected_group, 0o755)?;
    validate_directory(backup_parent, expected_owner, expected_group, 0o700)?;
    if existed {
        let before = read_optional_file(
            backup,
            expected_owner,
            expected_group,
            0o600,
            MAX_CONFIG_BYTES,
        )?;
        let Some(before) = before else {
            if exchange_exists(config_parent)? {
                return Err(MacOsSyntheticFileError::InvalidState);
            }
            let current = read_optional_file(
                config,
                expected_owner,
                expected_group,
                0o644,
                MAX_CONFIG_BYTES,
            )?;
            return if current.as_deref().map(digest).as_deref() == backup_sha256 {
                sync_directory(config_parent)?;
                sync_directory(backup_parent)
            } else {
                Err(MacOsSyntheticFileError::InvalidState)
            };
        };
        if Some(digest(&before).as_str()) != backup_sha256 {
            if exchange_exists(config_parent)? {
                return Err(MacOsSyntheticFileError::InvalidState);
            }
            let current = read_optional_file(
                config,
                expected_owner,
                expected_group,
                0o644,
                MAX_CONFIG_BYTES,
            )?;
            if current.as_deref().map(digest).as_deref() == backup_sha256 {
                remove_backup(backup_parent, backup)?;
                sync_directory(config_parent)?;
                return Ok(());
            }
            return Err(MacOsSyntheticFileError::InvalidState);
        }
        let expected = plan_macos_synthetic_entry(Some(&before))
            .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
        reconcile_exchange(
            config_parent,
            config,
            Some(&before),
            expected.bytes(),
            expected_owner,
            expected_group,
        )?;
        let current = read_optional_file(
            config,
            expected_owner,
            expected_group,
            0o644,
            MAX_CONFIG_BYTES,
        )?;
        if current.as_deref() != Some(before.as_slice())
            && current.as_deref() != Some(expected.bytes())
        {
            return Err(MacOsSyntheticFileError::InvalidState);
        }
        if current.as_deref() != Some(before.as_slice()) {
            replace_file_atomically(
                config_parent,
                config,
                &before,
                current.as_deref(),
                expected_owner,
                expected_group,
                0o644,
            )?;
        }
        remove_backup(backup_parent, backup)?;
    } else {
        if fs::symlink_metadata(backup).is_ok() {
            return Err(MacOsSyntheticFileError::InvalidState);
        }
        let expected =
            plan_macos_synthetic_entry(None).map_err(|_| MacOsSyntheticFileError::InvalidState)?;
        reconcile_exchange(
            config_parent,
            config,
            None,
            expected.bytes(),
            expected_owner,
            expected_group,
        )?;
        let current = read_optional_file(
            config,
            expected_owner,
            expected_group,
            0o644,
            MAX_CONFIG_BYTES,
        )?;
        match current.as_deref() {
            None => sync_directory(config_parent)?,
            Some(bytes) if bytes == expected.bytes() => {
                remove_expected_atomically(
                    config_parent,
                    config,
                    expected.bytes(),
                    expected_owner,
                    expected_group,
                )?;
            }
            Some(_) => return Err(MacOsSyntheticFileError::InvalidState),
        }
        sync_directory(backup_parent)?;
    }
    Ok(())
}

fn discard_backup_at(
    backup_parent: &Path,
    backup: &Path,
    existed: bool,
    backup_sha256: Option<&str>,
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    if existed != backup_sha256.is_some() {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    validate_directory(backup_parent, expected_owner, expected_group, 0o700)?;
    if !existed {
        return if fs::symlink_metadata(backup)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            sync_directory(backup_parent)
        } else {
            Err(MacOsSyntheticFileError::InvalidState)
        };
    }
    let Some(bytes) = read_optional_file(
        backup,
        expected_owner,
        expected_group,
        0o600,
        MAX_CONFIG_BYTES,
    )?
    else {
        return sync_directory(backup_parent);
    };
    if Some(digest(&bytes).as_str()) != backup_sha256 {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    remove_backup(backup_parent, backup)
}

fn read_optional_file(
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
    maximum: u64,
) -> Result<Option<Vec<u8>>, MacOsSyntheticFileError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(MacOsSyntheticFileError::InvalidState),
        Ok(_) => read_required_file(path, expected_owner, expected_group, mode, maximum).map(Some),
    }
}

fn read_required_file(
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
    maximum: u64,
) -> Result<Vec<u8>, MacOsSyntheticFileError> {
    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    let mut file = File::from(fd);
    validate_file(&file, path, expected_owner, expected_group, mode, maximum)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    if bytes.len() as u64 > maximum {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    Ok(bytes)
}

fn replace_file_atomically(
    parent: &Path,
    path: &Path,
    bytes: &[u8],
    expected_current: Option<&[u8]>,
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let staging = temporary_path(parent);
    let exchange = exchange_path(parent);
    write_new_file(&staging, bytes, expected_owner, expected_group, mode)?;
    let directory = File::open(parent).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    let staging_name = file_name(&staging)?;
    let exchange_name = file_name(&exchange)?;
    let target_name = file_name(path)?;
    if renameat_with(
        &directory,
        staging_name,
        &directory,
        exchange_name,
        RenameFlags::NOREPLACE,
    )
    .is_err()
    {
        let _ = fs::remove_file(&staging);
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    let flags = if expected_current.is_some() {
        RenameFlags::EXCHANGE
    } else {
        RenameFlags::NOREPLACE
    };
    if renameat_with(&directory, exchange_name, &directory, target_name, flags).is_err() {
        let _ = fs::remove_file(&exchange);
        let _ = full_sync(&directory);
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    if let Some(expected) = expected_current {
        let displaced = match read_required_file(
            &exchange,
            expected_owner,
            expected_group,
            mode,
            MAX_CONFIG_BYTES,
        ) {
            Ok(displaced) => displaced,
            Err(error) => {
                restore_exchange(&directory, exchange_name, target_name, &exchange)?;
                return Err(error);
            }
        };
        if displaced != expected {
            restore_exchange(&directory, exchange_name, target_name, &exchange)?;
            return Err(MacOsSyntheticFileError::InvalidState);
        }
        fs::remove_file(&exchange).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    }
    full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
}

fn remove_expected_atomically(
    parent: &Path,
    path: &Path,
    expected: &[u8],
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let quarantine = exchange_path(parent);
    let directory = File::open(parent).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    let target_name = file_name(path)?;
    let quarantine_name = file_name(&quarantine)?;
    renameat_with(
        &directory,
        target_name,
        &directory,
        quarantine_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    let displaced = match read_required_file(
        &quarantine,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    ) {
        Ok(displaced) => displaced,
        Err(error) => {
            restore_quarantine(&directory, quarantine_name, target_name)?;
            return Err(error);
        }
    };
    if displaced != expected {
        restore_quarantine(&directory, quarantine_name, target_name)?;
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    fs::remove_file(quarantine).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
}

fn restore_exchange(
    directory: &File,
    temporary_name: &OsStr,
    target_name: &OsStr,
    temporary: &Path,
) -> Result<(), MacOsSyntheticFileError> {
    renameat_with(
        directory,
        temporary_name,
        directory,
        target_name,
        RenameFlags::EXCHANGE,
    )
    .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    fs::remove_file(temporary).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    full_sync(directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
}

fn restore_quarantine(
    directory: &File,
    quarantine_name: &OsStr,
    target_name: &OsStr,
) -> Result<(), MacOsSyntheticFileError> {
    renameat_with(
        directory,
        quarantine_name,
        directory,
        target_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    full_sync(directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
}

fn reconcile_exchange(
    parent: &Path,
    config: &Path,
    before: Option<&[u8]>,
    expected_product: &[u8],
    expected_owner: u32,
    expected_group: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let exchange = exchange_path(parent);
    match fs::symlink_metadata(&exchange) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(MacOsSyntheticFileError::InvalidState),
        Ok(_) => {}
    }
    let directory = File::open(parent).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    let exchange_name = file_name(&exchange)?;
    let config_name = file_name(config)?;
    let displaced = read_required_file(
        &exchange,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    )?;
    let current = read_optional_file(
        config,
        expected_owner,
        expected_group,
        0o644,
        MAX_CONFIG_BYTES,
    )?;
    match (before, current.as_deref()) {
        (Some(before), Some(current)) if current == before && displaced == expected_product => {
            fs::remove_file(exchange).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
        }
        (Some(before), Some(current)) if current == expected_product && displaced == before => {
            renameat_with(
                &directory,
                exchange_name,
                &directory,
                config_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            fs::remove_file(exchange).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
        }
        (None, None) if displaced == expected_product => {
            fs::remove_file(exchange).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
        }
        (None, None) => {
            renameat_with(
                &directory,
                exchange_name,
                &directory,
                config_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            Err(MacOsSyntheticFileError::InvalidState)
        }
        (None, Some(current)) if current == expected_product && displaced == expected_product => {
            fs::remove_file(exchange).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
            remove_expected_atomically(
                parent,
                config,
                expected_product,
                expected_owner,
                expected_group,
            )
        }
        (Some(_), None | Some(_)) | (None, Some(_)) => Err(MacOsSyntheticFileError::InvalidState),
    }
}

fn write_new_file(
    path: &Path,
    bytes: &[u8],
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    let result = (|| {
        fchown(
            &file,
            Some(Uid::from_raw(expected_owner)),
            Some(Gid::from_raw(expected_group)),
        )
        .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
        file.set_permissions(Permissions::from_mode(mode))
            .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
        file.write_all(bytes)
            .map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
        full_sync(&file).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
        validate_file(
            &file,
            path,
            expected_owner,
            expected_group,
            mode,
            bytes.len() as u64,
        )
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn validate_file(
    file: &File,
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
    maximum: u64,
) -> Result<(), MacOsSyntheticFileError> {
    let metadata = file
        .metadata()
        .map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    if !metadata.is_file()
        || metadata.uid() != expected_owner
        || metadata.gid() != expected_group
        || metadata.mode() & 0o7777 != mode
        || metadata.nlink() != 1
        || metadata.len() > maximum
        || !getfacl(path, None).is_ok_and(|acl| acl.is_empty())
    {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    Ok(())
}

fn validate_directory(
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    mode: u32,
) -> Result<(), MacOsSyntheticFileError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| MacOsSyntheticFileError::InvalidState)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_owner
        || metadata.gid() != expected_group
        || metadata.mode() & 0o7777 != mode
        || !getfacl(path, None).is_ok_and(|acl| acl.is_empty())
    {
        return Err(MacOsSyntheticFileError::InvalidState);
    }
    Ok(())
}

fn remove_backup(parent: &Path, backup: &Path) -> Result<(), MacOsSyntheticFileError> {
    fs::remove_file(backup).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), MacOsSyntheticFileError> {
    let directory = File::open(path).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)?;
    full_sync(&directory).map_err(|_| MacOsSyntheticFileError::PersistenceFailed)
}

fn exchange_path(parent: &Path) -> PathBuf {
    parent.join(EXCHANGE_NAME)
}

fn temporary_path(parent: &Path) -> PathBuf {
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".synthetic.conf.pkg.staging.{}.{sequence}",
        std::process::id()
    ))
}

fn exchange_exists(parent: &Path) -> Result<bool, MacOsSyntheticFileError> {
    match fs::symlink_metadata(exchange_path(parent)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(MacOsSyntheticFileError::InvalidState),
    }
}

fn file_name(path: &Path) -> Result<&OsStr, MacOsSyntheticFileError> {
    path.file_name()
        .ok_or(MacOsSyntheticFileError::InvalidState)
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut value = String::with_capacity(71);
    value.push_str("sha256-");
    for byte in hash {
        use fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        config_parent: PathBuf,
        config: PathBuf,
        backup_parent: PathBuf,
        backup: PathBuf,
        uid: u32,
        gid: u32,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            let root = tempfile::tempdir()?;
            let config_parent = root.path().join("etc");
            let backup_parent = root.path().join("private");
            fs::create_dir(&config_parent)?;
            fs::create_dir(&backup_parent)?;
            fs::set_permissions(&config_parent, Permissions::from_mode(0o755))?;
            fs::set_permissions(&backup_parent, Permissions::from_mode(0o700))?;
            let config = config_parent.join("synthetic.conf");
            let backup = backup_parent.join("synthetic-conf-v1.backup");
            Ok(Self {
                _root: root,
                config_parent,
                config,
                backup_parent,
                backup,
                uid: Uid::effective().as_raw(),
                gid: Gid::effective().as_raw(),
            })
        }

        fn prepare(&self) -> Result<MacOsSyntheticFileTransaction, MacOsSyntheticFileError> {
            prepare_at(
                &self.config_parent,
                &self.config,
                &self.backup_parent,
                &self.backup,
                self.uid,
                self.gid,
            )
        }

        fn apply(
            &self,
            transaction: &MacOsSyntheticFileTransaction,
        ) -> Result<(), MacOsSyntheticFileError> {
            apply_at(
                &self.config_parent,
                &self.config,
                &self.backup_parent,
                &self.backup,
                transaction,
                self.uid,
                self.gid,
            )
        }

        fn restore(
            &self,
            transaction: &MacOsSyntheticFileTransaction,
        ) -> Result<(), MacOsSyntheticFileError> {
            restore_at(
                &self.config_parent,
                &self.config,
                &self.backup_parent,
                &self.backup,
                transaction.existed,
                transaction.backup_sha256(),
                self.uid,
                self.gid,
            )
        }
    }

    #[test]
    fn existing_file_is_backed_up_applied_and_exactly_restored() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"home\tUsers")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        let transaction = fixture.prepare()?;
        assert!(transaction.existed());
        assert!(transaction.changed());
        assert!(!fixture.backup.exists());

        fixture.apply(&transaction)?;
        assert_eq!(fs::read(&fixture.backup)?, b"home\tUsers");
        assert_eq!(fs::read(&fixture.config)?, b"home\tUsers\nnix\n");
        fixture.restore(&transaction)?;
        assert_eq!(fs::read(&fixture.config)?, b"home\tUsers");
        assert!(!fixture.backup.exists());
        Ok(())
    }

    #[test]
    fn absent_file_is_created_and_removed_on_rollback() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let transaction = fixture.prepare()?;
        assert!(!transaction.existed());
        fixture.apply(&transaction)?;
        assert_eq!(fs::read(&fixture.config)?, b"nix\n");
        fixture.restore(&transaction)?;
        assert!(!fixture.config.exists());
        Ok(())
    }

    #[test]
    fn prestate_drift_and_foreign_rollback_state_fail_closed() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"home\tUsers")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        let transaction = fixture.prepare()?;
        fs::write(&fixture.config, b"private\tprivate")?;
        assert!(fixture.apply(&transaction).is_err());
        assert!(fixture.restore(&transaction).is_err());
        assert_eq!(fs::read(&fixture.config)?, b"private\tprivate");
        Ok(())
    }

    #[test]
    fn committed_backup_is_digest_bound_before_removal() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"home\tUsers")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        let transaction = fixture.prepare()?;
        fixture.apply(&transaction)?;
        assert!(
            discard_backup_at(
                &fixture.backup_parent,
                &fixture.backup,
                true,
                Some("sha256-0000000000000000000000000000000000000000000000000000000000000000"),
                fixture.uid,
                fixture.gid,
            )
            .is_err()
        );
        discard_backup_at(
            &fixture.backup_parent,
            &fixture.backup,
            true,
            transaction.backup_sha256(),
            fixture.uid,
            fixture.gid,
        )?;
        discard_backup_at(
            &fixture.backup_parent,
            &fixture.backup,
            true,
            transaction.backup_sha256(),
            fixture.uid,
            fixture.gid,
        )?;
        assert!(!fixture.backup.exists());
        Ok(())
    }

    #[test]
    fn intent_before_apply_and_completed_cleanup_are_replay_safe() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"home\tUsers")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        let transaction = fixture.prepare()?;
        fixture.restore(&transaction)?;
        assert_eq!(fs::read(&fixture.config)?, b"home\tUsers");
        assert!(!fixture.backup.exists());
        Ok(())
    }

    #[test]
    fn incomplete_backup_and_interrupted_exchange_recover() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"home\tUsers")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        let transaction = fixture.prepare()?;

        fs::write(&fixture.backup, b"partial")?;
        fs::set_permissions(&fixture.backup, Permissions::from_mode(0o600))?;
        fixture.restore(&transaction)?;
        assert!(!fixture.backup.exists());

        ensure_backup(
            &fixture.backup_parent,
            &fixture.backup,
            &transaction,
            fixture.uid,
            fixture.gid,
        )?;
        let exchange = exchange_path(&fixture.config_parent);
        write_new_file(
            &exchange,
            &transaction.replacement,
            fixture.uid,
            fixture.gid,
            0o644,
        )?;
        let directory = File::open(&fixture.config_parent)?;
        renameat_with(
            &directory,
            file_name(&exchange)?,
            &directory,
            file_name(&fixture.config)?,
            RenameFlags::EXCHANGE,
        )?;

        fixture.restore(&transaction)?;
        assert_eq!(fs::read(&fixture.config)?, b"home\tUsers");
        assert!(!exchange.exists());
        assert!(!fixture.backup.exists());
        Ok(())
    }

    #[test]
    fn atomic_displacement_preserves_unexpected_current_bytes() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fs::write(&fixture.config, b"foreign")?;
        fs::set_permissions(&fixture.config, Permissions::from_mode(0o644))?;
        assert!(
            replace_file_atomically(
                &fixture.config_parent,
                &fixture.config,
                b"replacement",
                Some(b"expected"),
                fixture.uid,
                fixture.gid,
                0o644,
            )
            .is_err()
        );
        assert_eq!(fs::read(&fixture.config)?, b"foreign");

        fs::set_permissions(&fixture.config, Permissions::from_mode(0o600))?;
        assert!(
            replace_file_atomically(
                &fixture.config_parent,
                &fixture.config,
                b"replacement",
                Some(b"foreign"),
                fixture.uid,
                fixture.gid,
                0o644,
            )
            .is_err()
        );
        assert_eq!(fs::read(&fixture.config)?, b"foreign");
        assert_eq!(fs::metadata(&fixture.config)?.mode() & 0o7777, 0o600);
        assert!(
            remove_expected_atomically(
                &fixture.config_parent,
                &fixture.config,
                b"expected",
                fixture.uid,
                fixture.gid,
            )
            .is_err()
        );
        assert_eq!(fs::read(&fixture.config)?, b"foreign");
        Ok(())
    }
}
