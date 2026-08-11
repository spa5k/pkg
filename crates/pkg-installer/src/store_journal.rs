//! Secret-free write-ahead state for macOS store provisioning and rollback.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

const SCHEMA_VERSION: u32 = 1;
const PRODUCT: &str = "pkg";
const MAX_JOURNAL_BYTES: usize = 4096;

/// Stable failures for the provisioning rollback journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreJournalErrorCode {
    /// The serialized journal is malformed, oversized, or violates invariants.
    InvalidJournal,
    /// A requested state transition is repeated, skipped, or reordered.
    InvalidTransition,
}

/// Redacted provisioning-journal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsStoreJournalError {
    code: MacOsStoreJournalErrorCode,
}

impl MacOsStoreJournalError {
    const fn new(code: MacOsStoreJournalErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsStoreJournalErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsStoreJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS store rollback journal is invalid")
    }
}

impl Error for MacOsStoreJournalError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SyntheticRollback {
    existed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_sha256: Option<String>,
}

/// The one legal write-ahead provisioning sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MacOsStoreJournalPhase {
    /// No mutation is authorized yet.
    Empty,
    /// Synthetic-file rollback evidence is durable; replacement may run.
    SyntheticIntended,
    /// Synthetic-file replacement completed.
    SyntheticCompleted,
    /// APFS creation is authorized; recovery may discover the fixed new volume.
    VolumeIntended,
    /// APFS creation completed and its canonical UUID is durable.
    VolumeCompleted,
    /// Fixed System-keychain item creation is authorized.
    KeychainIntended,
    /// Fixed System-keychain item creation completed.
    KeychainCompleted,
    /// Mounting the recorded volume at `/nix` is authorized.
    MountIntended,
    /// Mounting completed.
    MountCompleted,
    /// Ownership enablement is authorized on the mounted volume.
    OwnershipIntended,
    /// Ownership enablement completed.
    OwnershipCompleted,
    /// Publishing the fixed volume record is authorized.
    PublicationIntended,
    /// Publishing completed.
    PublicationCompleted,
    /// Complete external state was verified.
    Verified,
    /// Success is durable; recovery must retain the installation.
    Committed,
}

/// One strict secret-free snapshot persisted before and after each mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacOsStoreProvisionJournal {
    schema_version: u32,
    product: String,
    phase: MacOsStoreJournalPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    synthetic: Option<SyntheticRollback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume_uuid: Option<String>,
}

/// One fixed recovery operation, returned in safe reverse order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacOsStoreRollbackAction<'a> {
    /// Remove the fixed dynamic volume record if this attempt published it.
    RemoveRecord,
    /// Unmount only the recorded volume UUID from `/nix` if mounted there.
    UnmountVolume { volume_uuid: &'a str },
    /// Delete only the fixed System-keychain service/account item.
    DeleteKeychainItem,
    /// Delete only the recorded APFS volume UUID.
    DeleteVolume { volume_uuid: &'a str },
    /// Discover and delete the sole exact fixed-identity volume created after preflight.
    DiscoverAndDeleteVolume,
    /// Restore exact pre-install synthetic.conf state from its fixed backup.
    RestoreSynthetic {
        existed: bool,
        backup_sha256: Option<&'a str>,
    },
}

impl MacOsStoreProvisionJournal {
    /// Creates the empty pre-mutation journal snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            phase: MacOsStoreJournalPhase::Empty,
            synthetic: None,
            volume_uuid: None,
        }
    }

    /// Returns the current durable protocol phase.
    #[must_use]
    pub const fn phase(&self) -> MacOsStoreJournalPhase {
        self.phase
    }

    /// Returns the secret-free synthetic rollback evidence for committed cleanup.
    #[must_use]
    pub fn synthetic_rollback(&self) -> Option<(bool, Option<&str>)> {
        self.synthetic
            .as_ref()
            .map(|state| (state.existed, state.backup_sha256.as_deref()))
    }

    /// Returns the recorded canonical volume UUID once creation completed.
    #[must_use]
    pub fn volume_uuid(&self) -> Option<&str> {
        self.volume_uuid.as_deref()
    }

    /// Decodes and fully validates one bounded strict journal snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for oversized, malformed, extended, or inconsistent input.
    pub fn decode(bytes: &[u8]) -> Result<Self, MacOsStoreJournalError> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid_journal());
        }
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| invalid_journal())?;
        journal.validate()?;
        Ok(journal)
    }

    /// Encodes one validated deterministic secret-free journal snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` when invariants or the encoded bound fail.
    pub fn encode(&self) -> Result<Vec<u8>, MacOsStoreJournalError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid_journal())?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid_journal());
        }
        Ok(bytes)
    }

    /// Persists exact synthetic rollback evidence before replacement.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless this is the valid first intent.
    pub fn intend_synthetic(
        &mut self,
        existed: bool,
        backup_sha256: Option<String>,
    ) -> Result<(), MacOsStoreJournalError> {
        if self.phase != MacOsStoreJournalPhase::Empty
            || existed != backup_sha256.is_some()
            || backup_sha256.as_deref().is_some_and(|value| !digest(value))
        {
            return Err(invalid_transition());
        }
        self.synthetic = Some(SyntheticRollback {
            existed,
            backup_sha256,
        });
        self.phase = MacOsStoreJournalPhase::SyntheticIntended;
        Ok(())
    }

    /// Records completed synthetic replacement.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless synthetic intent is current.
    pub fn complete_synthetic(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::SyntheticIntended,
            MacOsStoreJournalPhase::SyntheticCompleted,
        )
    }

    /// Persists APFS-creation intent before invoking `diskutil`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless synthetic replacement completed.
    pub fn intend_volume(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::SyntheticCompleted,
            MacOsStoreJournalPhase::VolumeIntended,
        )
    }

    /// Records the canonical UUID returned by completed APFS creation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless volume intent is current and the UUID is canonical.
    pub fn complete_volume(&mut self, volume_uuid: String) -> Result<(), MacOsStoreJournalError> {
        if self.phase != MacOsStoreJournalPhase::VolumeIntended
            || !crate::store_mount::canonical_uuid(&volume_uuid)
        {
            return Err(invalid_transition());
        }
        self.volume_uuid = Some(volume_uuid);
        self.phase = MacOsStoreJournalPhase::VolumeCompleted;
        Ok(())
    }

    /// Persists fixed keychain-item creation intent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless volume creation completed.
    pub fn intend_keychain(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::VolumeCompleted,
            MacOsStoreJournalPhase::KeychainIntended,
        )
    }

    /// Records completed fixed keychain-item creation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless keychain intent is current.
    pub fn complete_keychain(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::KeychainIntended,
            MacOsStoreJournalPhase::KeychainCompleted,
        )
    }

    /// Persists mount intent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless keychain creation completed.
    pub fn intend_mount(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::KeychainCompleted,
            MacOsStoreJournalPhase::MountIntended,
        )
    }

    /// Records completed mounting.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless mount intent is current.
    pub fn complete_mount(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::MountIntended,
            MacOsStoreJournalPhase::MountCompleted,
        )
    }

    /// Persists ownership-enablement intent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless mounting completed.
    pub fn intend_ownership(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::MountCompleted,
            MacOsStoreJournalPhase::OwnershipIntended,
        )
    }

    /// Records completed ownership enablement.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless ownership intent is current.
    pub fn complete_ownership(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::OwnershipIntended,
            MacOsStoreJournalPhase::OwnershipCompleted,
        )
    }

    /// Persists fixed-record publication intent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless ownership enablement completed.
    pub fn intend_publication(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::OwnershipCompleted,
            MacOsStoreJournalPhase::PublicationIntended,
        )
    }

    /// Records completed fixed-record publication.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless publication intent is current.
    pub fn complete_publication(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::PublicationIntended,
            MacOsStoreJournalPhase::PublicationCompleted,
        )
    }

    /// Records successful verification of all completed external state.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless publication completed.
    pub fn record_verified(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::PublicationCompleted,
            MacOsStoreJournalPhase::Verified,
        )
    }

    /// Durably commits success before journal removal.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless complete external state was verified.
    pub fn commit(&mut self) -> Result<(), MacOsStoreJournalError> {
        self.advance(
            MacOsStoreJournalPhase::Verified,
            MacOsStoreJournalPhase::Committed,
        )
    }

    /// Returns exact recovery actions in reverse mutation order.
    ///
    /// A committed journal deliberately returns no rollback actions. At
    /// `VolumeIntended`, recovery must discover the sole exact fixed-identity volume:
    /// preflight established that none existed before intent became durable.
    #[must_use]
    pub fn rollback_actions(&self) -> Vec<MacOsStoreRollbackAction<'_>> {
        if matches!(
            self.phase,
            MacOsStoreJournalPhase::Empty | MacOsStoreJournalPhase::Committed
        ) {
            return Vec::new();
        }
        let mut actions = Vec::with_capacity(5);
        if self.phase >= MacOsStoreJournalPhase::PublicationIntended {
            actions.push(MacOsStoreRollbackAction::RemoveRecord);
        }
        if self.phase >= MacOsStoreJournalPhase::MountIntended
            && let Some(volume_uuid) = self.volume_uuid.as_deref()
        {
            actions.push(MacOsStoreRollbackAction::UnmountVolume { volume_uuid });
        }
        if self.phase >= MacOsStoreJournalPhase::KeychainIntended {
            actions.push(MacOsStoreRollbackAction::DeleteKeychainItem);
        }
        if self.phase == MacOsStoreJournalPhase::VolumeIntended {
            actions.push(MacOsStoreRollbackAction::DiscoverAndDeleteVolume);
        } else if self.phase >= MacOsStoreJournalPhase::VolumeCompleted
            && let Some(volume_uuid) = self.volume_uuid.as_deref()
        {
            actions.push(MacOsStoreRollbackAction::DeleteVolume { volume_uuid });
        }
        if let Some(synthetic) = &self.synthetic {
            actions.push(MacOsStoreRollbackAction::RestoreSynthetic {
                existed: synthetic.existed,
                backup_sha256: synthetic.backup_sha256.as_deref(),
            });
        }
        actions
    }

    fn advance(
        &mut self,
        expected: MacOsStoreJournalPhase,
        next: MacOsStoreJournalPhase,
    ) -> Result<(), MacOsStoreJournalError> {
        if self.phase != expected {
            return Err(invalid_transition());
        }
        self.phase = next;
        Ok(())
    }

    fn validate(&self) -> Result<(), MacOsStoreJournalError> {
        let synthetic_expected = self.phase >= MacOsStoreJournalPhase::SyntheticIntended;
        let uuid_expected = self.phase >= MacOsStoreJournalPhase::VolumeCompleted;
        if self.schema_version != SCHEMA_VERSION
            || self.product != PRODUCT
            || self.synthetic.is_some() != synthetic_expected
            || self.volume_uuid.is_some() != uuid_expected
            || self.synthetic.as_ref().is_some_and(|synthetic| {
                synthetic.existed != synthetic.backup_sha256.is_some()
                    || synthetic
                        .backup_sha256
                        .as_deref()
                        .is_some_and(|value| !digest(value))
            })
            || self
                .volume_uuid
                .as_deref()
                .is_some_and(|uuid| !crate::store_mount::canonical_uuid(uuid))
        {
            return Err(invalid_journal());
        }
        Ok(())
    }
}

impl Default for MacOsStoreProvisionJournal {
    fn default() -> Self {
        Self::new()
    }
}

fn digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256-")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn invalid_journal() -> MacOsStoreJournalError {
    MacOsStoreJournalError::new(MacOsStoreJournalErrorCode::InvalidJournal)
}

const fn invalid_transition() -> MacOsStoreJournalError {
    MacOsStoreJournalError::new(MacOsStoreJournalErrorCode::InvalidTransition)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "01234567-89AB-CDEF-0123-456789ABCDEF";
    const DIGEST: &str = "sha256-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn through_volume_intent() -> Result<MacOsStoreProvisionJournal, MacOsStoreJournalError> {
        let mut journal = MacOsStoreProvisionJournal::new();
        journal.intend_synthetic(true, Some(DIGEST.to_owned()))?;
        journal.complete_synthetic()?;
        journal.intend_volume()?;
        Ok(journal)
    }

    fn complete_install() -> Result<MacOsStoreProvisionJournal, MacOsStoreJournalError> {
        let mut journal = through_volume_intent()?;
        journal.complete_volume(UUID.to_owned())?;
        journal.intend_keychain()?;
        journal.complete_keychain()?;
        journal.intend_mount()?;
        journal.complete_mount()?;
        journal.intend_ownership()?;
        journal.complete_ownership()?;
        journal.intend_publication()?;
        journal.complete_publication()?;
        journal.record_verified()?;
        Ok(journal)
    }

    #[test]
    fn write_ahead_transitions_round_trip_and_roll_back() -> Result<(), MacOsStoreJournalError> {
        let journal = complete_install()?;
        let bytes = journal.encode()?;
        assert_eq!(MacOsStoreProvisionJournal::decode(&bytes)?, journal);
        assert!(!String::from_utf8_lossy(&bytes).contains("password"));
        assert_eq!(
            journal.rollback_actions(),
            [
                MacOsStoreRollbackAction::RemoveRecord,
                MacOsStoreRollbackAction::UnmountVolume { volume_uuid: UUID },
                MacOsStoreRollbackAction::DeleteKeychainItem,
                MacOsStoreRollbackAction::DeleteVolume { volume_uuid: UUID },
                MacOsStoreRollbackAction::RestoreSynthetic {
                    existed: true,
                    backup_sha256: Some(DIGEST)
                }
            ]
        );
        Ok(())
    }

    #[test]
    fn volume_intent_recovers_creation_before_uuid_snapshot() -> Result<(), MacOsStoreJournalError>
    {
        let journal = through_volume_intent()?;
        assert_eq!(
            journal.rollback_actions(),
            [
                MacOsStoreRollbackAction::DiscoverAndDeleteVolume,
                MacOsStoreRollbackAction::RestoreSynthetic {
                    existed: true,
                    backup_sha256: Some(DIGEST)
                }
            ]
        );
        Ok(())
    }

    #[test]
    fn committed_terminal_state_retains_installation() -> Result<(), MacOsStoreJournalError> {
        let mut journal = complete_install()?;
        journal.commit()?;
        let bytes = journal.encode()?;
        let recovered = MacOsStoreProvisionJournal::decode(&bytes)?;
        assert_eq!(recovered.phase(), MacOsStoreJournalPhase::Committed);
        assert!(recovered.rollback_actions().is_empty());
        assert!(journal.commit().is_err());
        Ok(())
    }

    #[test]
    fn skipped_repeated_and_malformed_transitions_fail_closed() -> Result<(), MacOsStoreJournalError>
    {
        let mut journal = MacOsStoreProvisionJournal::new();
        assert!(journal.intend_volume().is_err());
        assert!(journal.complete_synthetic().is_err());
        assert!(journal.intend_synthetic(true, None).is_err());
        journal.intend_synthetic(false, None)?;
        assert!(journal.intend_synthetic(false, None).is_err());
        journal.complete_synthetic()?;
        journal.intend_volume()?;
        assert!(journal.complete_volume("not-a-uuid".to_owned()).is_err());
        Ok(())
    }

    #[test]
    fn strict_decode_rejects_extension_inconsistency_and_oversize() {
        let extended = br#"{"schemaVersion":1,"product":"pkg","phase":"empty","secret":"no"}"#;
        assert!(MacOsStoreProvisionJournal::decode(extended).is_err());
        let inconsistent = format!(
            r#"{{"schemaVersion":1,"product":"pkg","phase":"volumeCompleted","volumeUuid":"{UUID}"}}"#
        );
        assert!(MacOsStoreProvisionJournal::decode(inconsistent.as_bytes()).is_err());
        assert!(MacOsStoreProvisionJournal::decode(&vec![b'x'; MAX_JOURNAL_BYTES + 1]).is_err());
    }
}
