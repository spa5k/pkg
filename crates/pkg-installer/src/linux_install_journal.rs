//! Strict, secret-free state for Linux install recovery.

use std::{error::Error, fmt, str::FromStr};

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::{LinuxAssetKind, assets::linux_product_mutation_assets};

const SCHEMA_VERSION: u32 = 4;
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

/// Durable product-asset recovery policy for one Linux invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinuxInstallMode {
    /// Restore only authenticated prior bytes after an interrupted upgrade.
    InstallOrUpgrade,
    /// Keep authenticated same-release candidate bytes during offline repair.
    Repair,
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
    recovery_context_digest: String,
    mode: LinuxInstallMode,
    committed: bool,
    service_prior_active: Option<bool>,
    service_recovery_prepared: bool,
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
        recovery_context_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalError> {
        Self::new_with_mode(
            system,
            ownership_manifest_digest,
            recovery_context_digest,
            LinuxInstallMode::InstallOrUpgrade,
            None,
        )
    }

    /// Creates an offline repair journal.
    ///
    /// # Errors
    ///
    /// Returns `InvalidJournal` for a non-Linux system.
    pub fn new_repair(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Self, LinuxInstallJournalError> {
        Self::new_with_mode(
            system,
            ownership_manifest_digest,
            recovery_context_digest,
            LinuxInstallMode::Repair,
            None,
        )
    }

    fn new_with_mode(
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
        mode: LinuxInstallMode,
        service_prior_active: Option<bool>,
    ) -> Result<Self, LinuxInstallJournalError> {
        if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
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
            service_prior_active,
            service_recovery_prepared: false,
            entries: Vec::new(),
        })
    }

    /// Returns the durable recovery policy.
    #[must_use]
    pub const fn mode(&self) -> LinuxInstallMode {
        self.mode
    }

    /// Returns the Linux target bound into this validated journal.
    pub(crate) fn system(&self) -> Result<System, LinuxInstallJournalError> {
        System::from_str(&self.system).map_err(|_| invalid_journal())
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

    /// Records the service transition and its exact prior active state.
    pub(crate) fn intend_services(
        &mut self,
        prior_active: bool,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.mode == LinuxInstallMode::Repair {
            return Err(invalid_transition());
        }
        self.intend(LinuxInstallMutation::Services)?;
        self.service_prior_active = Some(prior_active);
        Ok(())
    }

    pub(crate) const fn service_prior_active(&self) -> Option<bool> {
        self.service_prior_active
    }

    pub(crate) const fn service_recovery_prepared(&self) -> bool {
        self.service_recovery_prepared
    }

    pub(crate) const fn prepare_service_recovery(
        &mut self,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed || self.service_prior_active.is_none() {
            return Err(invalid_transition());
        }
        self.service_recovery_prepared = true;
        Ok(())
    }

    pub(crate) const fn complete_service_recovery(
        &mut self,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed || !self.service_recovery_prepared {
            return Err(invalid_transition());
        }
        self.service_prior_active = None;
        self.service_recovery_prepared = false;
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
        self.service_prior_active = None;
        self.service_recovery_prepared = false;
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

    /// Removes one completed recovery action and later pre-existing entries.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when the mutation is not the next action in
    /// reverse order or the journal is committed.
    pub(crate) fn complete_recovery_action(
        &mut self,
        mutation: &LinuxInstallMutation,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed {
            return Err(invalid_transition());
        }
        let Some(index) = self
            .entries
            .iter()
            .rposition(|entry| entry.state != LinuxInstallMutationState::PreExisting)
        else {
            return Err(invalid_transition());
        };
        if self.entries[index].mutation != *mutation {
            return Err(invalid_transition());
        }
        self.entries.truncate(index);
        Ok(())
    }

    fn validate(&self) -> Result<(), LinuxInstallJournalError> {
        if self.schema_version != SCHEMA_VERSION
            || self.product != PRODUCT
            || !matches!(
                System::from_str(&self.system),
                Ok(System::X8664Linux | System::Aarch64Linux)
            )
            || Digest::from_str(&self.ownership_manifest_digest).is_err()
            || Digest::from_str(&self.recovery_context_digest).is_err()
            || self.entries.len() > install_sequence().len()
            || (self.service_recovery_prepared && self.service_prior_active.is_none())
        {
            return Err(invalid_journal());
        }
        let has_changed_services = self.entries.iter().any(|entry| {
            entry.mutation == LinuxInstallMutation::Services
                && entry.state != LinuxInstallMutationState::PreExisting
        });
        let service_state_valid = match self.mode {
            LinuxInstallMode::InstallOrUpgrade => {
                self.service_prior_active.is_some()
                    == (has_changed_services || self.service_recovery_prepared)
            }
            LinuxInstallMode::Repair => {
                self.service_prior_active.is_none()
                    && !self.service_recovery_prepared
                    && !has_changed_services
            }
        };
        if (self.committed
            && (self.service_prior_active.is_some() || self.service_recovery_prepared))
            || (!self.committed && !service_state_valid)
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
    let mut sequence = linux_product_mutation_assets()
        .filter(|asset| asset.kind() != LinuxAssetKind::File && asset.id() != "nix-gcroots")
        .map(|asset| LinuxInstallMutation::Asset {
            id: asset.id().to_owned(),
        })
        .collect::<Vec<_>>();
    sequence.push(LinuxInstallMutation::Asset {
        id: "nix-config".to_owned(),
    });
    sequence.push(LinuxInstallMutation::ManagedRuntime);
    sequence.extend(
        linux_product_mutation_assets()
            .filter(|asset| {
                asset.id() == "nix-gcroots"
                    || (asset.kind() == LinuxAssetKind::File
                        && !matches!(asset.id(), "nix-config" | "uninstall-manifest"))
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
        LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        )
        .unwrap()
    }

    #[test]
    fn strict_round_trip_binds_authenticated_identity() {
        let mut journal = journal();
        let first = install_sequence().first().cloned().unwrap();
        journal.intend(first.clone()).unwrap();
        journal.complete_created().unwrap();

        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded, journal);
        assert!(decoded.matches_binding(
            System::X8664Linux,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        ));
        assert!(!decoded.matches_binding(
            System::Aarch64Linux,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6a; 32]),
        ));
        assert!(!decoded.matches_binding(
            System::X8664Linux,
            Digest::from_bytes([0x5a; 32]),
            Digest::from_bytes([0x6b; 32]),
        ));
        assert_eq!(
            decoded.recovery_actions(),
            vec![LinuxInstallRecoveryAction::RevertCreated(&first)]
        );
    }

    #[test]
    fn repair_round_trip_has_no_service_mutation_state() {
        let mut journal = LinuxInstallJournal::new_repair(
            System::X8664Linux,
            Digest::from_bytes([0x7a; 32]),
            Digest::from_bytes([0x7b; 32]),
        )
        .unwrap();
        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded.mode(), LinuxInstallMode::Repair);
        assert_eq!(decoded.service_prior_active(), None);
        assert!(!decoded.service_recovery_prepared());

        journal
            .record_preexisting(install_sequence()[0].clone())
            .unwrap();
        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert!(decoded.recovery_actions().is_empty());
        assert_eq!(journal.intend_services(false), Err(invalid_transition()));
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
    fn completed_recovery_action_discards_only_the_recovered_suffix() {
        let mut journal = journal();
        let sequence = install_sequence();
        journal.record_preexisting(sequence[0].clone()).unwrap();
        journal.intend(sequence[1].clone()).unwrap();
        journal.complete_created().unwrap();
        journal.record_preexisting(sequence[2].clone()).unwrap();

        journal.complete_recovery_action(&sequence[1]).unwrap();

        assert!(journal.recovery_actions().is_empty());
        assert_eq!(
            journal.mutation_state(&sequence[0]).unwrap(),
            Some(LinuxInstallMutationState::PreExisting)
        );
        assert_eq!(journal.mutation_state(&sequence[1]).unwrap(), None);
        assert_eq!(
            journal.complete_recovery_action(&sequence[1]),
            Err(invalid_transition())
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
        let expected = crate::assets::linux_product_mutation_assets()
            .map(crate::LinuxInstallAsset::id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert_eq!(
            sequence.len(),
            crate::assets::linux_product_mutation_assets().count() + 2
        );
        let runtime = sequence
            .iter()
            .position(|mutation| mutation == &LinuxInstallMutation::ManagedRuntime);
        let gcroots = sequence.iter().position(|mutation| {
            mutation
                == &LinuxInstallMutation::Asset {
                    id: "nix-gcroots".to_owned(),
                }
        });
        assert!(
            runtime
                .zip(gcroots)
                .is_some_and(|(runtime, gcroots)| runtime < gcroots)
        );
    }
}
