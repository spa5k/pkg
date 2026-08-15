//! Production binding for failure-atomic macOS store-volume provisioning.

use crate::{
    MacOsStoreJournalPhase, MacOsStoreProvisionBackend, MacOsStoreProvisionError,
    MacOsStoreProvisionJournal, MacOsStoreProvisionOutcome, MacOsStoreRollbackAction,
    MacOsStoreVolumeContract, MacOsSyntheticFileStorage, MacOsSyntheticFileTransaction,
    publish_macos_store_volume_record,
    store_apfs::MacOsApfsAdapter,
    store_journal_file::MacOsStoreJournalStorage,
    store_mount::production::{receipt_matches, receipt_volume_uuid, remove_receipt},
};
use nix::{
    fcntl::{OFlag, open},
    sys::stat::{Mode, fchmod},
    unistd::{Gid, Uid, fchown, fsync},
};
use pkg_macos_security::{StoreVolumeSecret, SystemKeychainStore};
use std::{
    os::unix::{fs::MetadataExt, process::CommandExt},
    process::{Command, ExitStatus, Stdio},
};

const APFS_UTIL: &str = "/System/Library/Filesystems/apfs.fs/Contents/Resources/apfs.util";
const SEQUOIA_STITCHED_STATUS: i32 = 253;

/// Provisions or verifies the exact product-owned APFS store as root.
///
/// # Errors
///
/// Returns a redacted failure unless the caller is root:wheel and every
/// journaled filesystem, APFS, Keychain, record, and verification step succeeds.
pub fn provision_macos_store_volume_production()
-> Result<MacOsStoreProvisionOutcome, MacOsStoreProvisionError> {
    if nix::unistd::geteuid().as_raw() != 0 || nix::unistd::getegid().as_raw() != 0 {
        return Err(failure());
    }
    crate::provision_macos_store_volume(&mut ProductionBackend::new(
        crate::macos_accounts::BUILD_GID,
    ))
}

/// Classifies the complete preview-owned APFS state without mutation.
///
/// # Errors
///
/// Returns a redacted error for non-root use or partial, foreign, or unreadable state.
pub fn classify_macos_store_volume_production() -> Result<bool, MacOsStoreProvisionError> {
    require_root()?;
    let mut apfs = MacOsApfsAdapter::production();
    let discovered = apfs.discover_volume().map_err(|_| failure())?;
    let receipt_path = "/Library/Application Support/pkg/managed-nix/store-volume-v1.json";
    let receipt = match std::fs::symlink_metadata(receipt_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(failure()),
        Ok(_) => receipt_volume_uuid().map_err(|_| failure())?,
    };
    match (discovered, receipt) {
        (None, None) => {
            if SystemKeychainStore::exists().map_err(|_| failure())?
                || !MacOsSyntheticFileStorage::preview_entry_absent().map_err(|_| failure())?
            {
                Err(failure())
            } else {
                Ok(false)
            }
        }
        (Some(discovered), Some(receipt)) if discovered == receipt => {
            apfs.verify_final(&receipt).map_err(|_| failure())?;
            if !SystemKeychainStore::exists().map_err(|_| failure())?
                || !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
            {
                return Err(failure());
            }
            Ok(true)
        }
        _ => Err(failure()),
    }
}

/// Removes only the exact preview-owned APFS state, with the receipt last.
///
/// # Errors
///
/// Returns a redacted error unless every ownership check and removal succeeds.
pub fn remove_macos_store_volume_production() -> Result<(), MacOsStoreProvisionError> {
    require_root()?;
    let mut apfs = MacOsApfsAdapter::production();
    let receipt = receipt_volume_uuid().map_err(|_| failure())?;
    let discovered = apfs.discover_volume().map_err(|_| failure())?;
    let Some(volume_uuid) = receipt else {
        if discovered.is_some()
            || SystemKeychainStore::exists().map_err(|_| failure())?
            || !MacOsSyntheticFileStorage::preview_entry_absent().map_err(|_| failure())?
        {
            return Err(failure());
        }
        return Ok(());
    };
    match discovered {
        Some(discovered) if discovered == volume_uuid => {
            apfs.verify_for_removal(&volume_uuid)
                .map_err(|_| failure())?;
            if !SystemKeychainStore::exists().map_err(|_| failure())?
                || !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
            {
                return Err(failure());
            }
            apfs.unmount(&volume_uuid).map_err(|_| failure())?;
            apfs.delete(&volume_uuid).map_err(|_| failure())?;
        }
        None => {}
        Some(_) => return Err(failure()),
    }
    SystemKeychainStore::delete().map_err(|_| failure())?;
    MacOsSyntheticFileStorage::remove_preview_owned().map_err(|_| failure())?;
    remove_receipt(&volume_uuid).map_err(|_| failure())?;
    if apfs.discover_volume().map_err(|_| failure())?.is_some()
        || SystemKeychainStore::exists().map_err(|_| failure())?
        || !MacOsSyntheticFileStorage::preview_entry_absent().map_err(|_| failure())?
        || receipt_volume_uuid().map_err(|_| failure())?.is_some()
    {
        return Err(failure());
    }
    Ok(())
}

/// Verifies final absence without requiring product parent directories to remain.
///
/// # Errors
///
/// Returns a redacted error for non-root use, unreadable state, or product residue.
pub fn verify_macos_store_volume_absent_production() -> Result<(), MacOsStoreProvisionError> {
    require_root()?;
    let mut apfs = MacOsApfsAdapter::production();
    if apfs.discover_volume().map_err(|_| failure())?.is_some()
        || SystemKeychainStore::exists().map_err(|_| failure())?
        || [
            "/private/etc/synthetic.conf",
            "/Library/Application Support/pkg/managed-nix/store-volume-v1.json",
            "/Library/Application Support/pkg/managed-nix/synthetic-conf-v1.backup",
        ]
        .iter()
        .any(|path| std::fs::symlink_metadata(path).is_ok())
    {
        Err(failure())
    } else {
        Ok(())
    }
}

/// Verifies a complete store or the exact receipt-last removal prefix.
///
/// # Errors
///
/// Returns a redacted error for foreign, ambiguous, or invalid recovery state.
pub fn verify_macos_store_removal_state_production() -> Result<(), MacOsStoreProvisionError> {
    require_root()?;
    let mut apfs = MacOsApfsAdapter::production();
    let discovered = apfs.discover_volume().map_err(|_| failure())?;
    let receipt_path = "/Library/Application Support/pkg/managed-nix/store-volume-v1.json";
    let receipt = match std::fs::symlink_metadata(receipt_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(failure()),
        Ok(_) => receipt_volume_uuid().map_err(|_| failure())?,
    };
    match (discovered, receipt) {
        (Some(discovered), Some(receipt)) if discovered == receipt => {
            apfs.verify_for_removal(&receipt).map_err(|_| failure())?;
            if !SystemKeychainStore::exists().map_err(|_| failure())?
                || !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
            {
                return Err(failure());
            }
        }
        (None, Some(_)) => {
            let config = std::fs::symlink_metadata("/private/etc/synthetic.conf");
            if config.is_ok()
                && !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
            {
                return Err(failure());
            }
        }
        (None, None) => verify_macos_store_volume_absent_production()?,
        _ => return Err(failure()),
    }
    Ok(())
}

fn require_root() -> Result<(), MacOsStoreProvisionError> {
    if nix::unistd::geteuid().as_raw() == 0 && nix::unistd::getegid().as_raw() == 0 {
        Ok(())
    } else {
        Err(failure())
    }
}

struct ProductionBackend {
    journal: Option<MacOsStoreProvisionJournal>,
    synthetic: Option<MacOsSyntheticFileTransaction>,
    apfs: MacOsApfsAdapter,
    build_gid: u32,
}

impl ProductionBackend {
    const fn new(build_gid: u32) -> Self {
        Self {
            journal: None,
            synthetic: None,
            apfs: MacOsApfsAdapter::production(),
            build_gid,
        }
    }

    fn journal_mut(&mut self) -> Result<&mut MacOsStoreProvisionJournal, MacOsStoreProvisionError> {
        self.journal.as_mut().ok_or_else(failure)
    }

    fn replace_journal(&self) -> Result<(), MacOsStoreProvisionError> {
        MacOsStoreJournalStorage::replace(self.journal.as_ref().ok_or_else(failure)?)
            .map_err(|_| failure())
    }

    fn finish_committed(&mut self) -> Result<(), MacOsStoreProvisionError> {
        let journal = self.journal.as_ref().ok_or_else(failure)?;
        if journal.phase() != MacOsStoreJournalPhase::Committed {
            return Err(failure());
        }
        let (existed, digest) = journal.synthetic_rollback().ok_or_else(failure)?;
        MacOsSyntheticFileStorage::discard_backup(existed, digest).map_err(|_| failure())?;
        MacOsStoreJournalStorage::remove().map_err(|_| failure())?;
        self.journal = None;
        self.synthetic = None;
        Ok(())
    }

    fn rollback_loaded(&mut self) -> Result<(), MacOsStoreProvisionError> {
        let journal = self.journal.as_ref().ok_or_else(failure)?;
        if journal.phase() == MacOsStoreJournalPhase::Committed {
            return self.finish_committed();
        }
        let volume_uuid = journal.volume_uuid().map(ToOwned::to_owned);
        let actions = journal
            .rollback_actions()
            .into_iter()
            .map(OwnedRollbackAction::from)
            .collect::<Vec<_>>();
        let mut failed = false;
        for action in actions {
            let result = match action {
                OwnedRollbackAction::RemoveRecord => volume_uuid
                    .as_deref()
                    .ok_or_else(failure)
                    .and_then(|uuid| remove_receipt(uuid).map_err(|_| failure())),
                OwnedRollbackAction::UnmountVolume(uuid) => {
                    self.apfs.unmount(&uuid).map_err(|_| failure())
                }
                OwnedRollbackAction::DeleteKeychainItem => {
                    SystemKeychainStore::delete().map_err(|_| failure())
                }
                OwnedRollbackAction::DeleteVolume(uuid) => {
                    self.apfs.delete(&uuid).map_err(|_| failure())
                }
                OwnedRollbackAction::DiscoverAndDeleteVolume => {
                    self.apfs.discover_and_delete().map_err(|_| failure())
                }
                OwnedRollbackAction::RestoreSynthetic {
                    existed,
                    backup_sha256,
                } => MacOsSyntheticFileStorage::restore(existed, backup_sha256.as_deref())
                    .map_err(|_| failure()),
            };
            failed |= result.is_err();
        }
        if failed {
            return Err(failure());
        }
        MacOsStoreJournalStorage::remove().map_err(|_| failure())?;
        self.journal = None;
        self.synthetic = None;
        Ok(())
    }
}

impl MacOsStoreProvisionBackend for ProductionBackend {
    fn recover_pending_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
        self.journal = MacOsStoreJournalStorage::load().map_err(|_| failure())?;
        if self.journal.is_some() {
            self.rollback_loaded()?;
        }
        Ok(())
    }

    fn existing_volume_uuid(&mut self) -> Result<Option<String>, MacOsStoreProvisionError> {
        let volume_uuid = self.apfs.discover_volume().map_err(|_| failure())?;
        if volume_uuid.is_none()
            && (SystemKeychainStore::exists().map_err(|_| failure())?
                || receipt_exists()
                || MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?)
        {
            return Err(failure());
        }
        Ok(volume_uuid)
    }

    fn begin_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
        if self.journal.is_some() || self.synthetic.is_some() {
            return Err(failure());
        }
        let journal = MacOsStoreProvisionJournal::new();
        MacOsStoreJournalStorage::create(&journal).map_err(|_| failure())?;
        self.journal = Some(journal);
        Ok(())
    }

    fn ensure_synthetic_entry(&mut self) -> Result<(), MacOsStoreProvisionError> {
        MacOsSyntheticFileStorage::require_preview_clean().map_err(|_| failure())?;
        let transaction = MacOsSyntheticFileStorage::prepare().map_err(|_| failure())?;
        self.journal_mut()?
            .intend_synthetic(
                transaction.existed(),
                transaction.backup_sha256().map(ToOwned::to_owned),
            )
            .map_err(|_| failure())?;
        self.replace_journal()?;
        MacOsSyntheticFileStorage::apply(&transaction).map_err(|_| failure())?;
        stitch_synthetic_root()?;
        self.journal_mut()?
            .complete_synthetic()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        self.synthetic = Some(transaction);
        Ok(())
    }

    fn create_encrypted_volume(&mut self) -> Result<String, MacOsStoreProvisionError> {
        self.journal_mut()?.intend_volume().map_err(|_| failure())?;
        self.replace_journal()?;
        let secret = StoreVolumeSecret::generate().map_err(|_| failure())?;
        let volume_uuid = self
            .apfs
            .create_encrypted_volume(secret.expose_for_stdin())
            .map_err(|_| failure())?;
        self.journal_mut()?
            .complete_volume(volume_uuid.clone())
            .map_err(|_| failure())?;
        self.replace_journal()?;
        self.journal_mut()?
            .intend_keychain()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        SystemKeychainStore::create(&secret).map_err(|_| failure())?;
        self.journal_mut()?
            .complete_keychain()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        Ok(volume_uuid)
    }

    fn enable_ownership(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
        self.journal_mut()?
            .intend_ownership()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        self.apfs
            .enable_ownership(volume_uuid)
            .map_err(|_| failure())?;
        configure_mount_root(self.build_gid)?;
        self.journal_mut()?
            .complete_ownership()
            .map_err(|_| failure())?;
        self.replace_journal()
    }

    fn mount_volume(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
        self.journal_mut()?.intend_mount().map_err(|_| failure())?;
        self.replace_journal()?;
        self.apfs.mount(volume_uuid).map_err(|_| failure())?;
        self.journal_mut()?
            .complete_mount()
            .map_err(|_| failure())?;
        self.replace_journal()
    }

    fn publish_record(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
        self.journal_mut()?
            .intend_publication()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        publish_macos_store_volume_record(volume_uuid).map_err(|_| failure())?;
        self.journal_mut()?
            .complete_publication()
            .map_err(|_| failure())?;
        self.replace_journal()
    }

    fn verify_final(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
        self.apfs.verify_final(volume_uuid).map_err(|_| failure())?;
        verify_mount_root(self.build_gid)?;
        if !SystemKeychainStore::exists().map_err(|_| failure())?
            || !receipt_matches(volume_uuid).map_err(|_| failure())?
            || !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
        {
            return Err(failure());
        }
        Ok(())
    }

    fn commit_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
        self.journal_mut()?
            .record_verified()
            .map_err(|_| failure())?;
        self.replace_journal()?;
        self.journal_mut()?.commit().map_err(|_| failure())?;
        self.replace_journal()?;
        self.finish_committed()
    }

    fn rollback_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
        if self.journal.is_none() {
            self.journal = MacOsStoreJournalStorage::load().map_err(|_| failure())?;
        }
        if self.journal.is_none() {
            return Err(failure());
        }
        self.rollback_loaded()
    }
}

fn stitch_synthetic_root() -> Result<(), MacOsStoreProvisionError> {
    let status = Command::new(APFS_UTIL)
        .arg("-t")
        .env_clear()
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| failure())?;
    if !accepted_stitch_status(status)
        || !MacOsSyntheticFileStorage::entry_present().map_err(|_| failure())?
    {
        return Err(failure());
    }

    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let root = std::fs::File::from(open("/", flags, Mode::empty()).map_err(|_| failure())?);
    let mount = std::fs::File::from(
        open(MacOsStoreVolumeContract::MOUNT_POINT, flags, Mode::empty()).map_err(|_| failure())?,
    );
    let root = root.metadata().map_err(|_| failure())?;
    let mount = mount.metadata().map_err(|_| failure())?;
    if mount.is_dir()
        && mount.uid() == 0
        && mount.gid() == 0
        && mount.mode() & 0o7777 == 0o755
        && mount.dev() == root.dev()
    {
        Ok(())
    } else {
        Err(failure())
    }
}

fn accepted_stitch_status(status: ExitStatus) -> bool {
    status.success() || status.code() == Some(SEQUOIA_STITCHED_STATUS)
}

fn configure_mount_root(build_gid: u32) -> Result<(), MacOsStoreProvisionError> {
    let root = open(
        MacOsStoreVolumeContract::MOUNT_POINT,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure())?;
    let root = std::fs::File::from(root);
    fchown(
        &root,
        Some(Uid::from_raw(0)),
        Some(Gid::from_raw(build_gid)),
    )
    .map_err(|_| failure())?;
    fchmod(&root, Mode::from_bits_truncate(0o755)).map_err(|_| failure())?;
    fsync(&root).map_err(|_| failure())?;
    verify_mount_root_file(&root, build_gid)
}

fn verify_mount_root(build_gid: u32) -> Result<(), MacOsStoreProvisionError> {
    let root = open(
        MacOsStoreVolumeContract::MOUNT_POINT,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure())?;
    let root = std::fs::File::from(root);
    verify_mount_root_file(&root, build_gid)
}

fn verify_mount_root_file(
    root: &std::fs::File,
    build_gid: u32,
) -> Result<(), MacOsStoreProvisionError> {
    let metadata = root.metadata().map_err(|_| failure())?;
    if metadata.is_dir()
        && metadata.uid() == 0
        && metadata.gid() == build_gid
        && metadata.mode() & 0o7777 == 0o755
    {
        Ok(())
    } else {
        Err(failure())
    }
}

enum OwnedRollbackAction {
    RemoveRecord,
    UnmountVolume(String),
    DeleteKeychainItem,
    DeleteVolume(String),
    DiscoverAndDeleteVolume,
    RestoreSynthetic {
        existed: bool,
        backup_sha256: Option<String>,
    },
}

impl From<MacOsStoreRollbackAction<'_>> for OwnedRollbackAction {
    fn from(action: MacOsStoreRollbackAction<'_>) -> Self {
        match action {
            MacOsStoreRollbackAction::RemoveRecord => Self::RemoveRecord,
            MacOsStoreRollbackAction::UnmountVolume { volume_uuid } => {
                Self::UnmountVolume(volume_uuid.to_owned())
            }
            MacOsStoreRollbackAction::DeleteKeychainItem => Self::DeleteKeychainItem,
            MacOsStoreRollbackAction::DeleteVolume { volume_uuid } => {
                Self::DeleteVolume(volume_uuid.to_owned())
            }
            MacOsStoreRollbackAction::DiscoverAndDeleteVolume => Self::DiscoverAndDeleteVolume,
            MacOsStoreRollbackAction::RestoreSynthetic {
                existed,
                backup_sha256,
            } => Self::RestoreSynthetic {
                existed,
                backup_sha256: backup_sha256.map(ToOwned::to_owned),
            },
        }
    }
}

fn receipt_exists() -> bool {
    use std::{fs, io::ErrorKind};
    const PATH: &str = "/Library/Application Support/pkg/managed-nix/store-volume-v1.json";
    match fs::symlink_metadata(PATH) {
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(_) | Ok(_) => true,
    }
}

const fn failure() -> MacOsStoreProvisionError {
    MacOsStoreProvisionError::backend_failure()
}

#[cfg(test)]
mod tests {
    use super::{SEQUOIA_STITCHED_STATUS, accepted_stitch_status};
    use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

    #[test]
    fn stitch_status_accepts_only_success_and_sequoia_created_state() {
        assert!(accepted_stitch_status(ExitStatus::from_raw(0)));
        assert!(accepted_stitch_status(ExitStatus::from_raw(
            SEQUOIA_STITCHED_STATUS << 8,
        )));
        assert!(!accepted_stitch_status(ExitStatus::from_raw(1 << 8)));
        assert!(!accepted_stitch_status(ExitStatus::from_raw(9)));
    }
}
