//! Strict, secret-free state for Linux install recovery.

use std::{error::Error, fmt, str::FromStr};

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::{LinuxAssetKind, linux_install_assets};

const SCHEMA_VERSION: u32 = 1;
const PRODUCT: &str = "pkg";
const MAX_JOURNAL_BYTES: usize = 16 * 1024;

/// Stable Linux install-journal failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxInstallJournalErrorCode {
    /// The snapshot is malformed, oversized, stale, or has unknown data.
    InvalidJournal,
    /// A mutation was repeated, skipped, or completed out of order.
    InvalidTransition,
}

/// Redacted Linux install-journal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxInstallJournalError {
    code: LinuxInstallJournalErrorCode,
}

impl LinuxInstallJournalError {
    const fn new(code: LinuxInstallJournalErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxInstallJournalErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxInstallJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux install recovery state is invalid")
    }
}

impl Error for LinuxInstallJournalError {}

/// One fixed mutation in the Linux installation sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum LinuxInstallMutation {
    /// One compiled account, directory, or file asset.
    Asset { id: String },
    /// The authenticated managed runtime transaction.
    ManagedRuntime,
    /// The fixed systemd activation transaction.
    Services,
}

/// Durable state for one fixed mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinuxInstallMutationState {
    /// The write-ahead record is durable. Completion is not yet known.
    Intended,
    /// This attempt created or changed the fixed object.
    Created,
    /// The exact fixed object existed before this attempt.
    PreExisting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinuxInstallJournalEntry {
    mutation: LinuxInstallMutation,
    state: LinuxInstallMutationState,
}

/// A strict write-ahead snapshot for one authenticated Linux install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinuxInstallJournal {
    schema_version: u32,
    product: String,
    system: String,
    ownership_manifest_digest: String,
    committed: bool,
    entries: Vec<LinuxInstallJournalEntry>,
}

/// One bounded recovery decision in safe reverse order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxInstallRecoveryAction<'a> {
    /// Revalidate an incomplete write-ahead operation. Never remove it blindly.
    RevalidateIntended(&'a LinuxInstallMutation),
    /// Revert a completed mutation that this journal owns.
    RevertCreated(&'a LinuxInstallMutation),
}

impl LinuxInstallJournal {
    /// Creates an empty journal bound to one authenticated product bundle.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for a non-Linux system.
    pub fn new(
        system: System,
        ownership_manifest_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalError> {
        if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
            return Err(invalid_journal());
        }
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            system: system.to_string(),
            ownership_manifest_digest: ownership_manifest_digest.to_string(),
            committed: false,
            entries: Vec::new(),
        })
    }

    /// Decodes and fully validates one bounded strict snapshot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for malformed, oversized, extended, or stale data.
    pub fn decode(bytes: &[u8]) -> Result<Self, LinuxInstallJournalError> {
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
    pub fn encode(&self) -> Result<Vec<u8>, LinuxInstallJournalError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid_journal())?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(invalid_journal());
        }
        Ok(bytes)
    }

    /// Returns true when this journal belongs to the authenticated bundle.
    #[must_use]
    pub fn matches_binding(&self, system: System, digest: Digest) -> bool {
        self.system == system.as_str() && self.ownership_manifest_digest == digest.to_string()
    }

    /// Records the next fixed mutation intent after exact absence was verified.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when a prior intent is incomplete or the
    /// mutation does not match the compiled install sequence.
    pub fn intend(
        &mut self,
        mutation: LinuxInstallMutation,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.state == LinuxInstallMutationState::Intended)
            || expected_mutation(self.entries.len()).as_ref() != Some(&mutation)
        {
            return Err(invalid_transition());
        }
        self.entries.push(LinuxInstallJournalEntry {
            mutation,
            state: LinuxInstallMutationState::Intended,
        });
        Ok(())
    }

    /// Records one exact object that existed before this attempt.
    ///
    /// No intent is needed because this operation does not mutate the host.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when an intent is pending or the mutation
    /// does not match the compiled install sequence.
    pub fn record_preexisting(
        &mut self,
        mutation: LinuxInstallMutation,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed
            || self
                .entries
                .last()
                .is_some_and(|entry| entry.state == LinuxInstallMutationState::Intended)
            || expected_mutation(self.entries.len()).as_ref() != Some(&mutation)
        {
            return Err(invalid_transition());
        }
        self.entries.push(LinuxInstallJournalEntry {
            mutation,
            state: LinuxInstallMutationState::PreExisting,
        });
        Ok(())
    }

    /// Records that the current absent-before intent created the fixed object.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when no matching intent is current.
    pub fn complete_created(&mut self) -> Result<(), LinuxInstallJournalError> {
        let entry = self.entries.last_mut().ok_or_else(invalid_transition)?;
        if entry.state != LinuxInstallMutationState::Intended {
            return Err(invalid_transition());
        }
        entry.state = LinuxInstallMutationState::Created;
        Ok(())
    }

    /// Commits a complete installation after receipt-last publication.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` unless every fixed mutation is complete.
    pub fn commit(&mut self) -> Result<(), LinuxInstallJournalError> {
        if self.committed
            || self.entries.len() != install_sequence().len()
            || self
                .entries
                .iter()
                .any(|entry| entry.state == LinuxInstallMutationState::Intended)
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
    /// Returns `InvalidTransition` when the caller skips an earlier mutation or
    /// names a mutation outside the compiled sequence.
    pub fn mutation_state(
        &self,
        mutation: &LinuxInstallMutation,
    ) -> Result<Option<LinuxInstallMutationState>, LinuxInstallJournalError> {
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
    pub fn recovery_actions(&self) -> Vec<LinuxInstallRecoveryAction<'_>> {
        if self.committed {
            return Vec::new();
        }
        self.entries
            .iter()
            .rev()
            .filter_map(|entry| match entry.state {
                LinuxInstallMutationState::Intended => Some(
                    LinuxInstallRecoveryAction::RevalidateIntended(&entry.mutation),
                ),
                LinuxInstallMutationState::Created => {
                    Some(LinuxInstallRecoveryAction::RevertCreated(&entry.mutation))
                }
                LinuxInstallMutationState::PreExisting => None,
            })
            .collect()
    }

    fn validate(&self) -> Result<(), LinuxInstallJournalError> {
        if self.schema_version != SCHEMA_VERSION
            || self.product != PRODUCT
            || !matches!(
                System::from_str(&self.system),
                Ok(System::X8664Linux | System::Aarch64Linux)
            )
            || Digest::from_str(&self.ownership_manifest_digest).is_err()
            || self.entries.len() > install_sequence().len()
        {
            return Err(invalid_journal());
        }
        let sequence = install_sequence();
        for (index, entry) in self.entries.iter().enumerate() {
            if sequence.get(index) != Some(&entry.mutation)
                || (entry.state == LinuxInstallMutationState::Intended
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
                    .any(|entry| entry.state == LinuxInstallMutationState::Intended))
        {
            return Err(invalid_journal());
        }
        Ok(())
    }
}

fn install_sequence() -> Vec<LinuxInstallMutation> {
    let mut sequence = linux_install_assets()
        .iter()
        .filter(|asset| asset.kind() != LinuxAssetKind::File)
        .map(|asset| LinuxInstallMutation::Asset {
            id: asset.id().to_owned(),
        })
        .collect::<Vec<_>>();
    sequence.push(LinuxInstallMutation::Asset {
        id: "nix-config".to_owned(),
    });
    sequence.push(LinuxInstallMutation::ManagedRuntime);
    sequence.extend(
        linux_install_assets()
            .iter()
            .filter(|asset| {
                asset.kind() == LinuxAssetKind::File
                    && !matches!(asset.id(), "nix-config" | "uninstall-manifest")
            })
            .map(|asset| LinuxInstallMutation::Asset {
                id: asset.id().to_owned(),
            }),
    );
    sequence.push(LinuxInstallMutation::Services);
    sequence.push(LinuxInstallMutation::Asset {
        id: "uninstall-manifest".to_owned(),
    });
    sequence
}

fn expected_mutation(index: usize) -> Option<LinuxInstallMutation> {
    install_sequence().get(index).cloned()
}

const fn invalid_journal() -> LinuxInstallJournalError {
    LinuxInstallJournalError::new(LinuxInstallJournalErrorCode::InvalidJournal)
}

const fn invalid_transition() -> LinuxInstallJournalError {
    LinuxInstallJournalError::new(LinuxInstallJournalErrorCode::InvalidTransition)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn journal() -> LinuxInstallJournal {
        LinuxInstallJournal::new(System::X8664Linux, Digest::from_bytes([0x5a; 32])).unwrap()
    }

    #[test]
    fn strict_round_trip_binds_authenticated_identity() {
        let mut journal = journal();
        let first = install_sequence().first().cloned().unwrap();
        journal.intend(first.clone()).unwrap();
        journal.complete_created().unwrap();

        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded, journal);
        assert!(decoded.matches_binding(System::X8664Linux, Digest::from_bytes([0x5a; 32])));
        assert!(!decoded.matches_binding(System::Aarch64Linux, Digest::from_bytes([0x5a; 32])));
        assert_eq!(
            decoded.recovery_actions(),
            vec![LinuxInstallRecoveryAction::RevertCreated(&first)]
        );
    }

    #[test]
    fn sequence_refuses_skips_repeats_and_incomplete_completion() {
        let mut journal = journal();
        assert_eq!(
            journal.intend(LinuxInstallMutation::ManagedRuntime),
            Err(invalid_transition())
        );
        let first = install_sequence().first().cloned().unwrap();
        journal.intend(first.clone()).unwrap();
        assert_eq!(journal.intend(first), Err(invalid_transition()));
        assert_eq!(
            journal.record_preexisting(install_sequence()[1].clone()),
            Err(invalid_transition())
        );
        assert_eq!(journal.commit(), Err(invalid_transition()));
    }

    #[test]
    fn state_lookup_refuses_skips_and_distinguishes_pending_state() {
        let mut journal = journal();
        let sequence = install_sequence();
        assert_eq!(journal.mutation_state(&sequence[0]).unwrap(), None);
        assert_eq!(
            journal.mutation_state(&sequence[1]),
            Err(invalid_transition())
        );
        journal.record_preexisting(sequence[0].clone()).unwrap();
        assert_eq!(
            journal.mutation_state(&sequence[0]).unwrap(),
            Some(LinuxInstallMutationState::PreExisting)
        );
        assert_eq!(journal.mutation_state(&sequence[1]).unwrap(), None);
    }

    #[test]
    fn recovery_preserves_preexisting_and_flags_uncertain_intent() {
        let mut journal = journal();
        let sequence = install_sequence();
        journal.record_preexisting(sequence[0].clone()).unwrap();
        journal.intend(sequence[1].clone()).unwrap();
        assert_eq!(
            journal.recovery_actions(),
            vec![LinuxInstallRecoveryAction::RevalidateIntended(&sequence[1])]
        );
    }

    #[test]
    fn commit_requires_the_complete_receipt_last_sequence() {
        let mut journal = journal();
        for mutation in install_sequence() {
            journal.intend(mutation).unwrap();
            journal.complete_created().unwrap();
        }
        journal.commit().unwrap();
        assert!(journal.is_committed());
        assert!(journal.recovery_actions().is_empty());
        assert_eq!(journal.commit(), Err(invalid_transition()));
    }

    #[test]
    fn decode_rejects_unknown_fields_invalid_bindings_and_impossible_state() {
        let encoded = journal().encode().unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["extra"] = serde_json::json!(true);
        assert_eq!(
            LinuxInstallJournal::decode(&serde_json::to_vec(&value).unwrap()),
            Err(invalid_journal())
        );

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["system"] = serde_json::json!("x86_64-darwin");
        assert_eq!(
            LinuxInstallJournal::decode(&serde_json::to_vec(&value).unwrap()),
            Err(invalid_journal())
        );

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["committed"] = serde_json::json!(true);
        assert_eq!(
            LinuxInstallJournal::decode(&serde_json::to_vec(&value).unwrap()),
            Err(invalid_journal())
        );
    }

    #[test]
    fn sequence_contains_each_fixed_asset_once() {
        let sequence = install_sequence();
        let actual = sequence
            .iter()
            .filter_map(|mutation| match mutation {
                LinuxInstallMutation::Asset { id } => Some(id.as_str()),
                LinuxInstallMutation::ManagedRuntime | LinuxInstallMutation::Services => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let expected = linux_install_assets()
            .iter()
            .map(|asset| asset.id())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert_eq!(sequence.len(), linux_install_assets().len() + 2);
    }
}
