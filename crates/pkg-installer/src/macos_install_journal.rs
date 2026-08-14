//! Strict, secret-free state for macOS install recovery.

use std::{error::Error, fmt, str::FromStr};

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::platform::macos::{MacOsAssetKind, macos_install_assets};

const SCHEMA_VERSION: u32 = 1;
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MacOsInstallJournalEntry {
    mutation: MacOsInstallMutation,
    state: MacOsInstallMutationState,
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
    committed: bool,
    entries: Vec<MacOsInstallJournalEntry>,
}

/// One bounded recovery decision in safe reverse order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacOsInstallRecoveryAction<'a> {
    /// Revalidate an incomplete write-ahead operation. Never remove it blindly.
    RevalidateIntended(&'a MacOsInstallMutation),
    /// Revert a completed mutation that this journal owns.
    RevertCreated(&'a MacOsInstallMutation),
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
        if !matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
            return Err(invalid_journal());
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            system: system.to_string(),
            ownership_manifest_digest: ownership_manifest_digest.to_string(),
            recovery_context_digest: recovery_context_digest.to_string(),
            committed: false,
            entries: Vec::new(),
        })
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
        if entry.state != MacOsInstallMutationState::Intended {
            return Err(invalid_transition());
        }
        entry.state = MacOsInstallMutationState::Created;
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
                MacOsInstallMutationState::Intended => Some(
                    MacOsInstallRecoveryAction::RevalidateIntended(&entry.mutation),
                ),
                MacOsInstallMutationState::Created => {
                    Some(MacOsInstallRecoveryAction::RevertCreated(&entry.mutation))
                }
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
        {
            return Err(invalid_journal());
        }
        let sequence = install_sequence();
        for (index, entry) in self.entries.iter().enumerate() {
            if sequence.get(index) != Some(&entry.mutation)
                || (entry.state == MacOsInstallMutationState::Intended
                    && index + 1 != self.entries.len())
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

pub(crate) fn install_sequence() -> Vec<MacOsInstallMutation> {
    let mut sequence = macos_install_assets()
        .iter()
        .filter(|asset| store_prerequisite(asset.id()))
        .map(asset_mutation)
        .collect::<Vec<_>>();
    sequence.push(MacOsInstallMutation::StoreVolume);
    sequence.extend(
        macos_install_assets()
            .iter()
            .filter(|asset| {
                asset.kind() != MacOsAssetKind::File && !store_prerequisite(asset.id())
            })
            .map(asset_mutation),
    );
    sequence.push(MacOsInstallMutation::Asset {
        id: "nix-config".to_owned(),
    });
    sequence.push(MacOsInstallMutation::ManagedRuntime);
    sequence.extend(
        macos_install_assets()
            .iter()
            .filter(|asset| {
                asset.kind() == MacOsAssetKind::File
                    && asset.id() != "nix-config"
                    && !store_prerequisite(asset.id())
            })
            .map(asset_mutation),
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

fn store_prerequisite(id: &str) -> bool {
    matches!(
        id,
        "product-root" | "product-bin" | "service-root" | "managed-nix-state" | "helper-binary"
    )
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

    #[test]
    fn sequence_matches_the_outer_install_order() -> Result<(), Box<dyn Error>> {
        let sequence = install_sequence();
        let store = sequence
            .iter()
            .position(|mutation| mutation == &MacOsInstallMutation::StoreVolume)
            .ok_or_else(|| std::io::Error::other("missing store"))?;
        let runtime = sequence
            .iter()
            .position(|mutation| mutation == &MacOsInstallMutation::ManagedRuntime)
            .ok_or_else(|| std::io::Error::other("missing runtime"))?;
        let services = sequence
            .iter()
            .position(|mutation| mutation == &MacOsInstallMutation::Services)
            .ok_or_else(|| std::io::Error::other("missing services"))?;
        assert!(store < runtime && runtime < services);
        assert_eq!(
            sequence.last(),
            Some(&MacOsInstallMutation::OwnershipReceipt)
        );
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
