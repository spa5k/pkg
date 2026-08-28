//! Strict, secret-free state for macOS install recovery.

use std::{error::Error, fmt, str::FromStr};

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::platform::macos::{MacOsAssetKind, macos_product_install_assets};

const SCHEMA_VERSION: u32 = 3;
const PRODUCT: &str = "pkg";
const MAX_JOURNAL_BYTES: usize = 32 * 1024;

/// Stable macOS install-journal failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsInstallJournalErrorCode {
    /// The snapshot is malformed, oversized, stale, or has unknown data.
    InvalidJournal,
    /// A mutation was repeated, skipped, or completed out of order.
    InvalidTransition,
}

/// Redacted macOS install-journal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsInstallJournalError {
    code: MacOsInstallJournalErrorCode,
}

impl MacOsInstallJournalError {
    const fn new(code: MacOsInstallJournalErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsInstallJournalErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsInstallJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS install recovery state is invalid")
    }
}

impl Error for MacOsInstallJournalError {}

/// One fixed mutation in the macOS installation sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum MacOsInstallMutation {
    /// One compiled account, directory, or file asset.
    Asset { id: String },
    /// The encrypted APFS store transaction.
    StoreVolume,
    /// The authenticated managed runtime transaction.
    ManagedRuntime,
    /// The fixed launchd activation transaction.
    Services,
    /// The authenticated root-owned ownership receipt, published last.
    OwnershipReceipt,
}

/// Durable state for one fixed mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MacOsInstallMutationState {
    /// The write-ahead record is durable. Completion is not yet known.
    Intended,
    /// This attempt created or changed the fixed object.
    Created,
    /// The exact fixed object existed before this attempt.
    PreExisting,
    /// An owned file was replaced and its authenticated prior bytes are retained.
    Replaced,
}

/// Closed macOS product installation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MacOsInstallMode {
    FreshInstall,
    OfflineUpgrade,
    OfflineRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MacOsInstallJournalEntry {
    mutation: MacOsInstallMutation,
    state: MacOsInstallMutationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_digest: Option<String>,
}

/// A strict write-ahead snapshot for one authenticated macOS install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacOsInstallJournal {
    schema_version: u32,
    product: String,
    system: String,
    ownership_manifest_digest: String,
    recovery_context_digest: String,
    mode: MacOsInstallMode,
    committed: bool,
    entries: Vec<MacOsInstallJournalEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uninstall_registered_uids: Option<Vec<u32>>,
}

/// One bounded recovery decision in safe reverse order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacOsInstallRecoveryAction<'a> {
    /// Revalidate an incomplete write-ahead operation. Never remove it blindly.
    RevalidateIntended(&'a MacOsInstallMutation),
    /// Revert a completed mutation that this journal owns.
    RevertCreated(&'a MacOsInstallMutation),
    /// Restores the authenticated prior bytes for an interrupted upgrade.
    RestoreReplaced(&'a MacOsInstallMutation, Digest),
    /// Keeps authenticated release bytes for an interrupted explicit repair.
    RollForwardReplaced(&'a MacOsInstallMutation),
}

impl MacOsInstallJournal {
    /// Creates an empty journal bound to one authenticated product bundle.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for a non-macOS system.
    pub fn new(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, MacOsInstallJournalError> {
        Self::new_with_mode(
            system,
            ownership_manifest_digest,
            recovery_context_digest,
            MacOsInstallMode::FreshInstall,
        )
    }

    /// Creates an empty journal for one exact product operation mode.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for a non-macOS system.
    pub fn new_with_mode(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
        mode: MacOsInstallMode,
    ) -> Result<Self, MacOsInstallJournalError> {
        if !matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
            return Err(invalid_journal());
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            system: system.to_string(),
            ownership_manifest_digest: ownership_manifest_digest.to_string(),
            recovery_context_digest: recovery_context_digest.to_string(),
            mode,
            committed: false,
            entries: Vec::new(),
            uninstall_registered_uids: None,
        })
    }

    #[must_use]
    pub const fn mode(&self) -> MacOsInstallMode {
        self.mode
    }

    /// Decodes and fully validates one bounded strict snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for malformed, oversized, extended, or stale data.
    pub fn decode(bytes: &[u8]) -> Result<Self, MacOsInstallJournalError> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid_journal());
        }
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| invalid_journal())?;
        journal.validate()?;
        Ok(journal)
    }

    /// Encodes one validated deterministic snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` when invariants or the size bound fail.
    pub fn encode(&self) -> Result<Vec<u8>, MacOsInstallJournalError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid_journal())?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid_journal());
        }
        Ok(bytes)
    }

    /// Returns true when this journal belongs to the authenticated bundle.
    #[must_use]
    pub fn matches_binding(
        &self,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> bool {
        self.system == system.as_str()
            && self.ownership_manifest_digest == ownership_manifest_digest.to_string()
            && self.recovery_context_digest == recovery_context_digest.to_string()
    }

    /// Records the next fixed mutation intent after exact absence was verified.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` for an incomplete or out-of-order mutation.
    pub fn intend(
        &mut self,
        mutation: MacOsInstallMutation,
    ) -> Result<(), MacOsInstallJournalError> {
        if self.committed
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.state == MacOsInstallMutationState::Intended)
            || expected_mutation(self.entries.len()).as_ref() != Some(&mutation)
        {
            return Err(invalid_transition());
        }
        self.entries.push(MacOsInstallJournalEntry {
            mutation,
            state: MacOsInstallMutationState::Intended,
            prior_digest: None,
        });
        Ok(())
    }

    /// Records one exact object that existed before this attempt.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` for an incomplete or out-of-order mutation.
    pub fn record_preexisting(
        &mut self,
        mutation: MacOsInstallMutation,
    ) -> Result<(), MacOsInstallJournalError> {
        if self.committed
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.state == MacOsInstallMutationState::Intended)
            || expected_mutation(self.entries.len()).as_ref() != Some(&mutation)
        {
            return Err(invalid_transition());
        }
        self.entries.push(MacOsInstallJournalEntry {
            mutation,
            state: MacOsInstallMutationState::PreExisting,
            prior_digest: None,
        });
        Ok(())
    }

    /// Records the next owned-file replacement before its first write.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` for a non-file, invalid mode, or wrong order.
    pub fn intend_replacement(
        &mut self,
        mutation: MacOsInstallMutation,
        prior_digest: Option<Digest>,
    ) -> Result<(), MacOsInstallJournalError> {
        let replaceable = match &mutation {
            MacOsInstallMutation::Asset { id } => macos_product_install_assets()
                .any(|asset| asset.id() == id && asset.kind() == MacOsAssetKind::File),
            MacOsInstallMutation::OwnershipReceipt => true,
            MacOsInstallMutation::StoreVolume
            | MacOsInstallMutation::ManagedRuntime
            | MacOsInstallMutation::Services => false,
        };
        if !replaceable
            || self.mode == MacOsInstallMode::FreshInstall
            || (self.mode == MacOsInstallMode::OfflineUpgrade && prior_digest.is_none())
            || self.committed
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.state == MacOsInstallMutationState::Intended)
            || expected_mutation(self.entries.len()).as_ref() != Some(&mutation)
        {
            return Err(invalid_transition());
        }
        self.entries.push(MacOsInstallJournalEntry {
            mutation,
            state: MacOsInstallMutationState::Intended,
            prior_digest: prior_digest.map(|digest| digest.to_string()),
        });
        Ok(())
    }

    /// Records that the current absent-before intent created the fixed object.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when no matching intent is current.
    pub fn complete_created(&mut self) -> Result<(), MacOsInstallJournalError> {
        let entry = self.entries.last_mut().ok_or_else(invalid_transition)?;
        if entry.state != MacOsInstallMutationState::Intended || entry.prior_digest.is_some() {
            return Err(invalid_transition());
        }
        entry.state = MacOsInstallMutationState::Created;
        Ok(())
    }

    /// Records that the intended replacement installed new bytes.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless a valid replacement intent is current.
    pub fn complete_replaced(&mut self) -> Result<(), MacOsInstallJournalError> {
        let entry = self.entries.last_mut().ok_or_else(invalid_transition)?;
        if entry.state != MacOsInstallMutationState::Intended
            || self.mode == MacOsInstallMode::FreshInstall
            || (self.mode == MacOsInstallMode::OfflineUpgrade && entry.prior_digest.is_none())
        {
            return Err(invalid_transition());
        }
        entry.state = MacOsInstallMutationState::Replaced;
        Ok(())
    }

    /// Records that the intended replacement found exact bytes and made no change.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless a replacement intent is current.
    pub fn complete_unchanged_replacement(&mut self) -> Result<(), MacOsInstallJournalError> {
        let entry = self.entries.last_mut().ok_or_else(invalid_transition)?;
        if entry.state != MacOsInstallMutationState::Intended
            || self.mode == MacOsInstallMode::FreshInstall
        {
            return Err(invalid_transition());
        }
        entry.state = MacOsInstallMutationState::PreExisting;
        entry.prior_digest = None;
        Ok(())
    }

    /// Commits a complete installation after receipt-last publication.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless every fixed mutation is complete.
    pub fn commit(&mut self) -> Result<(), MacOsInstallJournalError> {
        if self.committed
            || self.entries.len() != install_sequence().len()
            || self
                .entries
                .iter()
                .any(|entry| entry.state == MacOsInstallMutationState::Intended)
        {
            return Err(invalid_transition());
        }
        self.committed = true;
        Ok(())
    }

    /// Returns true after successful receipt-last installation was committed.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        self.committed
    }

    /// Records the bounded ordered user snapshot before any product root deletion.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` for install state, a repeated snapshot, or
    /// zero, duplicate, unsorted, or excessive UIDs.
    pub fn record_uninstall_user_snapshot(
        &mut self,
        uids: &[u32],
    ) -> Result<(), MacOsInstallJournalError> {
        if self.committed
            || !self.entries.is_empty()
            || self.mode != MacOsInstallMode::FreshInstall
            || self.uninstall_registered_uids.is_some()
            || !valid_uninstall_user_snapshot(uids)
        {
            return Err(invalid_transition());
        }
        self.uninstall_registered_uids = Some(uids.to_vec());
        Ok(())
    }

    /// Returns the durable user snapshot for an uninstall retry.
    #[must_use]
    pub fn uninstall_registered_uids(&self) -> Option<&[u32]> {
        self.uninstall_registered_uids.as_deref()
    }

    /// Returns true for the snapshot used only as an uninstall marker.
    #[must_use]
    pub const fn is_uninstall_marker(&self) -> bool {
        !self.committed && self.entries.is_empty()
    }

    /// Returns the durable state for one mutation at the current sequence point.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` for a skipped or unknown mutation.
    pub fn mutation_state(
        &self,
        mutation: &MacOsInstallMutation,
    ) -> Result<Option<MacOsInstallMutationState>, MacOsInstallJournalError> {
        let Some(index) = install_sequence()
            .iter()
            .position(|expected| expected == mutation)
        else {
            return Err(invalid_transition());
        };
        if index > self.entries.len() {
            return Err(invalid_transition());
        }
        self.entries.get(index).map_or(Ok(None), |entry| {
            (entry.mutation == *mutation)
                .then_some(Some(entry.state))
                .ok_or_else(invalid_transition)
        })
    }

    /// Returns bounded recovery actions in reverse mutation order.
    #[must_use]
    pub fn recovery_actions(&self) -> Vec<MacOsInstallRecoveryAction<'_>> {
        if self.committed {
            return Vec::new();
        }
        self.entries
            .iter()
            .rev()
            .filter_map(|entry| match entry.state {
                MacOsInstallMutationState::Intended => match (
                    self.mode,
                    entry
                        .prior_digest
                        .as_deref()
                        .and_then(|digest| Digest::from_str(digest).ok()),
                ) {
                    (MacOsInstallMode::OfflineUpgrade, Some(digest)) => Some(
                        MacOsInstallRecoveryAction::RestoreReplaced(&entry.mutation, digest),
                    ),
                    (MacOsInstallMode::OfflineRepair, Some(_)) => Some(
                        MacOsInstallRecoveryAction::RollForwardReplaced(&entry.mutation),
                    ),
                    (_, None) => Some(MacOsInstallRecoveryAction::RevalidateIntended(
                        &entry.mutation,
                    )),
                    (MacOsInstallMode::FreshInstall, Some(_)) => None,
                },
                MacOsInstallMutationState::Created => {
                    Some(MacOsInstallRecoveryAction::RevertCreated(&entry.mutation))
                }
                MacOsInstallMutationState::Replaced => match self.mode {
                    MacOsInstallMode::OfflineUpgrade => entry
                        .prior_digest
                        .as_deref()
                        .and_then(|digest| Digest::from_str(digest).ok())
                        .map(|digest| {
                            MacOsInstallRecoveryAction::RestoreReplaced(&entry.mutation, digest)
                        }),
                    MacOsInstallMode::OfflineRepair => Some(
                        MacOsInstallRecoveryAction::RollForwardReplaced(&entry.mutation),
                    ),
                    MacOsInstallMode::FreshInstall => None,
                },
                MacOsInstallMutationState::PreExisting => None,
            })
            .collect()
    }

    /// Removes one completed recovery action and later pre-existing entries.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when recovery is out of order.
    pub fn complete_recovery_action(
        &mut self,
        mutation: &MacOsInstallMutation,
    ) -> Result<(), MacOsInstallJournalError> {
        if self.committed {
            return Err(invalid_transition());
        }
        let Some(index) = self
            .entries
            .iter()
            .rposition(|entry| entry.state != MacOsInstallMutationState::PreExisting)
        else {
            return Err(invalid_transition());
        };
        if self.entries[index].mutation != *mutation {
            return Err(invalid_transition());
        }
        self.entries.truncate(index);
        Ok(())
    }

    fn validate(&self) -> Result<(), MacOsInstallJournalError> {
        if self.schema_version != SCHEMA_VERSION
            || self.product != PRODUCT
            || !matches!(
                System::from_str(&self.system),
                Ok(System::X8664Darwin | System::Aarch64Darwin)
            )
            || Digest::from_str(&self.ownership_manifest_digest).is_err()
            || Digest::from_str(&self.recovery_context_digest).is_err()
            || self.entries.len() > install_sequence().len()
            || self
                .uninstall_registered_uids
                .as_deref()
                .is_some_and(|uids| {
                    self.committed
                        || !self.entries.is_empty()
                        || self.mode != MacOsInstallMode::FreshInstall
                        || !valid_uninstall_user_snapshot(uids)
                })
        {
            return Err(invalid_journal());
        }
        let sequence = install_sequence();
        for (index, entry) in self.entries.iter().enumerate() {
            if sequence.get(index) != Some(&entry.mutation)
                || (entry.state == MacOsInstallMutationState::Intended
                    && index + 1 != self.entries.len())
                || entry
                    .prior_digest
                    .as_deref()
                    .is_some_and(|digest| Digest::from_str(digest).is_err())
                || (entry.state != MacOsInstallMutationState::Replaced
                    && entry.state != MacOsInstallMutationState::Intended
                    && entry.prior_digest.is_some())
                || (entry.state == MacOsInstallMutationState::Replaced
                    && (self.mode == MacOsInstallMode::FreshInstall
                        || (self.mode == MacOsInstallMode::OfflineUpgrade
                            && entry.prior_digest.is_none())))
            {
                return Err(invalid_journal());
            }
        }
        if self.committed
            && (self.entries.len() != sequence.len()
                || self
                    .entries
                    .iter()
                    .any(|entry| entry.state == MacOsInstallMutationState::Intended))
        {
            return Err(invalid_journal());
        }
        Ok(())
    }
}

fn valid_uninstall_user_snapshot(uids: &[u32]) -> bool {
    uids.len() <= crate::linux_user_cleanup::MAX_DURABLE_USER_SNAPSHOT
        && !uids.contains(&0)
        && uids.windows(2).all(|pair| pair[0] < pair[1])
}

pub fn install_sequence() -> Vec<MacOsInstallMutation> {
    let mut sequence = macos_product_install_assets()
        .filter(|asset| asset.kind() != MacOsAssetKind::File && asset.id() != "nix-root")
        .map(|asset| asset_mutation(&asset))
        .collect::<Vec<_>>();
    sequence.push(MacOsInstallMutation::ManagedRuntime);
    sequence.push(MacOsInstallMutation::Asset {
        id: "nix-root".to_owned(),
    });
    sequence.extend(
        macos_product_install_assets()
            .filter(|asset| {
                asset.kind() == MacOsAssetKind::File && asset.id() != "uninstall-manifest"
            })
            .map(|asset| asset_mutation(&asset)),
    );
    sequence.push(MacOsInstallMutation::Services);
    sequence.push(MacOsInstallMutation::OwnershipReceipt);
    sequence
}

fn asset_mutation(asset: &crate::MacOsInstallAsset) -> MacOsInstallMutation {
    MacOsInstallMutation::Asset {
        id: asset.id().to_owned(),
    }
}

fn expected_mutation(index: usize) -> Option<MacOsInstallMutation> {
    install_sequence().get(index).cloned()
}

const fn invalid_journal() -> MacOsInstallJournalError {
    MacOsInstallJournalError::new(MacOsInstallJournalErrorCode::InvalidJournal)
}

const fn invalid_transition() -> MacOsInstallJournalError {
    MacOsInstallJournalError::new(MacOsInstallJournalErrorCode::InvalidTransition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Result<MacOsInstallJournal, MacOsInstallJournalError> {
        MacOsInstallJournal::new(
            System::Aarch64Darwin,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        )
    }

    fn advance_to_first_product_file(
        journal: &mut MacOsInstallJournal,
    ) -> Result<MacOsInstallMutation, Box<dyn Error>> {
        for mutation in install_sequence() {
            let is_file = match &mutation {
                MacOsInstallMutation::Asset { id } => macos_product_install_assets()
                    .any(|asset| asset.id() == id && asset.kind() == MacOsAssetKind::File),
                MacOsInstallMutation::StoreVolume
                | MacOsInstallMutation::ManagedRuntime
                | MacOsInstallMutation::Services
                | MacOsInstallMutation::OwnershipReceipt => false,
            };
            if is_file {
                return Ok(mutation);
            }
            journal.record_preexisting(mutation)?;
        }
        Err(std::io::Error::other("missing product file").into())
    }

    #[test]
    fn sequence_matches_the_outer_install_order() -> Result<(), Box<dyn Error>> {
        let sequence = install_sequence();
        let runtime = sequence
            .iter()
            .position(|mutation| mutation == &MacOsInstallMutation::ManagedRuntime)
            .ok_or_else(|| std::io::Error::other("missing runtime"))?;
        let services = sequence
            .iter()
            .position(|mutation| mutation == &MacOsInstallMutation::Services)
            .ok_or_else(|| std::io::Error::other("missing services"))?;
        let nix_root = sequence
            .iter()
            .position(|mutation| {
                mutation
                    == &MacOsInstallMutation::Asset {
                        id: "nix-root".to_owned(),
                    }
            })
            .ok_or_else(|| std::io::Error::other("missing nix root evidence"))?;
        assert!(runtime < nix_root && nix_root < services);
        assert!(
            !sequence
                .iter()
                .any(|mutation| matches!(mutation, MacOsInstallMutation::StoreVolume))
        );
        assert!(!sequence.iter().any(|mutation| {
            matches!(mutation, MacOsInstallMutation::Asset { id } if id == "daemon-plist" || id == "nix-config")
        }));
        assert_eq!(
            sequence.last(),
            Some(&MacOsInstallMutation::OwnershipReceipt)
        );
        Ok(())
    }

    #[test]
    fn upgrade_and_repair_record_distinct_replacement_recovery() -> Result<(), Box<dyn Error>> {
        let digest = Digest::from_bytes([0x7a; 32]);
        for (mode, restore) in [
            (MacOsInstallMode::OfflineUpgrade, true),
            (MacOsInstallMode::OfflineRepair, false),
        ] {
            let mut journal = MacOsInstallJournal::new_with_mode(
                System::Aarch64Darwin,
                Digest::from_bytes([0x5a; 32]),
                Digest::from_bytes([0x6a; 32]),
                mode,
            )?;
            let mutation = advance_to_first_product_file(&mut journal)?;
            journal.intend_replacement(mutation.clone(), Some(digest))?;
            let intended_actions = journal.recovery_actions();
            assert!(if restore {
                intended_actions
                    == vec![MacOsInstallRecoveryAction::RestoreReplaced(
                        &mutation, digest,
                    )]
            } else {
                intended_actions == vec![MacOsInstallRecoveryAction::RollForwardReplaced(&mutation)]
            });
            journal.complete_replaced()?;
            let actions = journal.recovery_actions();
            assert!(if restore {
                actions
                    == vec![MacOsInstallRecoveryAction::RestoreReplaced(
                        &mutation, digest,
                    )]
            } else {
                actions == vec![MacOsInstallRecoveryAction::RollForwardReplaced(&mutation)]
            });
        }
        Ok(())
    }

    #[test]
    fn replacement_state_is_limited_to_owned_files_and_receipt() -> Result<(), Box<dyn Error>> {
        let mut journal = MacOsInstallJournal::new_with_mode(
            System::Aarch64Darwin,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
            MacOsInstallMode::OfflineUpgrade,
        )?;
        assert!(
            journal
                .intend_replacement(MacOsInstallMutation::ManagedRuntime, None)
                .is_err()
        );
        let mutation = advance_to_first_product_file(&mut journal)?;
        journal.intend_replacement(mutation, Some(Digest::from_bytes([0x7a; 32])))?;
        assert!(journal.complete_created().is_err());
        Ok(())
    }

    #[test]
    fn uninstall_user_snapshot_is_bounded_strict_and_marker_only() -> Result<(), Box<dyn Error>> {
        let mut marker = journal()?;
        marker.record_uninstall_user_snapshot(&[501, 1_001])?;
        assert_eq!(marker.uninstall_registered_uids(), Some(&[501, 1_001][..]));
        assert!(marker.is_uninstall_marker());
        assert_eq!(MacOsInstallJournal::decode(&marker.encode()?)?, marker);
        assert!(
            marker
                .record_uninstall_user_snapshot(&[501, 1_001])
                .is_err()
        );

        let mut empty = journal()?;
        empty.record_uninstall_user_snapshot(&[])?;
        assert_eq!(empty.uninstall_registered_uids(), Some(&[][..]));
        assert_eq!(MacOsInstallJournal::decode(&empty.encode()?)?, empty);

        for invalid in [&[0][..], &[1_001, 501], &[501, 501]] {
            assert!(journal()?.record_uninstall_user_snapshot(invalid).is_err());
        }
        let oversized =
            (1..=u32::try_from(crate::linux_user_cleanup::MAX_DURABLE_USER_SNAPSHOT + 1)?)
                .collect::<Vec<_>>();
        assert!(
            journal()?
                .record_uninstall_user_snapshot(&oversized)
                .is_err()
        );

        let mut install = journal()?;
        let first = install_sequence()
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty sequence"))?;
        install.intend(first)?;
        assert!(install.record_uninstall_user_snapshot(&[501]).is_err());

        let mut corrupted = serde_json::to_value(marker)?;
        corrupted["uninstallRegisteredUids"] = serde_json::json!([1_001, 501]);
        assert!(MacOsInstallJournal::decode(&serde_json::to_vec(&corrupted)?).is_err());
        let oversized_bytes = vec![b' '; MAX_JOURNAL_BYTES + 1];
        assert!(MacOsInstallJournal::decode(&oversized_bytes).is_err());
        Ok(())
    }

    #[test]
    fn strict_round_trip_binds_authenticated_identity() -> Result<(), Box<dyn Error>> {
        let mut journal = journal()?;
        let first = install_sequence()
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty sequence"))?;
        journal.intend(first.clone())?;
        journal.complete_created()?;
        let decoded = MacOsInstallJournal::decode(&journal.encode()?)?;
        assert_eq!(decoded, journal);
        assert!(decoded.matches_binding(
            System::Aarch64Darwin,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        ));
        assert!(!decoded.matches_binding(
            System::X8664Darwin,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        ));
        assert_eq!(
            decoded.recovery_actions(),
            vec![MacOsInstallRecoveryAction::RevertCreated(&first)]
        );
        Ok(())
    }

    #[test]
    fn transitions_refuse_skips_and_recover_in_reverse() -> Result<(), Box<dyn Error>> {
        let mut journal = journal()?;
        assert_eq!(
            journal.intend(MacOsInstallMutation::StoreVolume),
            Err(invalid_transition())
        );
        let first = install_sequence()
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty sequence"))?;
        let second = install_sequence()
            .get(1)
            .cloned()
            .ok_or_else(|| std::io::Error::other("short sequence"))?;
        journal.intend(first.clone())?;
        journal.complete_created()?;
        journal.record_preexisting(second)?;
        assert_eq!(
            journal.recovery_actions(),
            vec![MacOsInstallRecoveryAction::RevertCreated(&first)]
        );
        journal.complete_recovery_action(&first)?;
        assert!(journal.recovery_actions().is_empty());
        Ok(())
    }
}
