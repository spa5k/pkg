//! Production binding for failure-atomic macOS store-volume provisioning.

use crate::{
    MacOsStoreJournalPhase, MacOsStoreProvisionBackend, MacOsStoreProvisionError,
    MacOsStoreProvisionJournal, MacOsStoreProvisionOutcome, MacOsStoreRollbackAction,
    MacOsSyntheticFileStorage, MacOsSyntheticFileTransaction, publish_macos_store_volume_record,
    store_apfs::MacOsApfsAdapter,
    store_journal_file::MacOsStoreJournalStorage,
    store_mount::production::{receipt_matches, remove_receipt},
};
use pkg_macos_security::{StoreVolumeSecret, SystemKeychainStore};

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
    crate::provision_macos_store_volume(&mut ProductionBackend::new())
}

struct ProductionBackend {
    journal: Option<MacOsStoreProvisionJournal>,
    synthetic: Option<MacOsSyntheticFileTransaction>,
    apfs: MacOsApfsAdapter,
}

impl ProductionBackend {
    const fn new() -> Self {
        Self {
            journal: None,
            synthetic: None,
            apfs: MacOsApfsAdapter::production(),
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
        let transaction = MacOsSyntheticFileStorage::prepare().map_err(|_| failure())?;
        self.journal_mut()?
            .intend_synthetic(
                transaction.existed(),
                transaction.backup_sha256().map(ToOwned::to_owned),
            )
            .map_err(|_| failure())?;
        self.replace_journal()?;
        MacOsSyntheticFileStorage::apply(&transaction).map_err(|_| failure())?;
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
