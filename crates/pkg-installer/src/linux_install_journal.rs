//! Strict, secret-free state for Linux install recovery.

use std::{error::Error, fmt, str::FromStr};

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::{
    LinuxAssetKind,
    assets::{is_linux_product_gcroots_asset, linux_product_mutation_assets},
};

const SCHEMA_VERSION: u32 = 6;
const PRODUCT: &str = "pkg";
const MAX_JOURNAL_BYTES: usize = 16 * 1024;

/// Stable Linux install-journal failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxInstallJournalErrorCode {
    /// The snapshot belongs to a journal schema this binary does not support.
    UnsupportedSchema,
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
        formatter.write_str(match self.code {
            LinuxInstallJournalErrorCode::UnsupportedSchema => {
                "linux install recovery state uses an unsupported schema; keep it unchanged and use the matching installer"
            }
            LinuxInstallJournalErrorCode::InvalidJournal
            | LinuxInstallJournalErrorCode::InvalidTransition => {
                "linux install recovery state is invalid"
            }
        })
    }
}

impl Error for LinuxInstallJournalError {}

/// One fixed mutation in the Linux installation sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum LinuxInstallMutation {
    /// One compiled account, directory, or file asset.
    Asset {
        /// The exact compiled asset id.
        id: String,
    },
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
    /// Create a new installation and activate its product service set.
    FreshInstall,
    /// Replace an existing installation while its product service set stays offline.
    OfflineUpgrade,
    /// Keep authenticated same-release candidate bytes during offline repair.
    OfflineRepair,
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
    fresh_services_deactivated: bool,
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
        mode: LinuxInstallMode,
        system: System,
        ownership_manifest_digest: Digest,
        recovery_context_digest: Digest,
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
            fresh_services_deactivated: false,
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
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| invalid_journal())?;
        let schema = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(invalid_journal)?;
        if schema != u64::from(SCHEMA_VERSION) {
            return Err(LinuxInstallJournalError::new(
                LinuxInstallJournalErrorCode::UnsupportedSchema,
            ));
        }
        let journal: Self = serde_json::from_value(value).map_err(|_| invalid_journal())?;
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

    /// Records fresh-install service activation intent before any systemd mutation.
    pub(crate) fn intend_services(&mut self) -> Result<(), LinuxInstallJournalError> {
        if self.mode != LinuxInstallMode::FreshInstall {
            return Err(invalid_transition());
        }
        self.intend(LinuxInstallMutation::Services)
    }

    pub(crate) fn mark_fresh_services_deactivated(
        &mut self,
    ) -> Result<(), LinuxInstallJournalError> {
        if self.committed
            || self.mode != LinuxInstallMode::FreshInstall
            || self.fresh_services_deactivated
            || !self.recovery_actions().iter().any(|action| {
                matches!(
                    action,
                    LinuxInstallRecoveryAction::RevalidateIntended(LinuxInstallMutation::Services)
                        | LinuxInstallRecoveryAction::RevertCreated(LinuxInstallMutation::Services)
                )
            })
        {
            return Err(invalid_transition());
        }
        self.fresh_services_deactivated = true;
        Ok(())
    }

    pub(crate) const fn fresh_services_deactivated(&self) -> bool {
        self.fresh_services_deactivated
    }

    pub(crate) fn records_asset(&self, id: &str) -> bool {
        self.entries.iter().any(|entry| {
            matches!(&entry.mutation, LinuxInstallMutation::Asset { id: recorded } if recorded == id)
        })
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

    pub(crate) fn finish_recovery(&mut self) -> Result<bool, LinuxInstallJournalError> {
        if self.committed || !self.recovery_actions().is_empty() {
            return Err(invalid_transition());
        }
        let changed = !self.entries.is_empty() || self.fresh_services_deactivated;
        self.entries.clear();
        self.fresh_services_deactivated = false;
        Ok(changed)
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
        {
            return Err(invalid_journal());
        }
        let changed_services = self.entries.iter().any(|entry| {
            entry.mutation == LinuxInstallMutation::Services
                && entry.state != LinuxInstallMutationState::PreExisting
        });
        let service_state_valid = match self.mode {
            LinuxInstallMode::FreshInstall => true,
            LinuxInstallMode::OfflineUpgrade | LinuxInstallMode::OfflineRepair => !changed_services,
        };
        if !service_state_valid
            || (self.fresh_services_deactivated
                && (self.mode != LinuxInstallMode::FreshInstall || self.committed))
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
        .filter(|asset| {
            asset.kind() != LinuxAssetKind::File && !is_linux_product_gcroots_asset(*asset)
        })
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
                is_linux_product_gcroots_asset(*asset)
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
#[allow(clippy::unwrap_used, reason = "tests may unwrap")]
mod tests {
    use super::*;

    fn journal() -> LinuxInstallJournal {
        LinuxInstallJournal::new(
            LinuxInstallMode::FreshInstall,
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
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0x7a; 32]),
            Digest::from_bytes([0x7b; 32]),
        )
        .unwrap();
        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded.mode(), LinuxInstallMode::OfflineRepair);

        journal
            .record_preexisting(install_sequence()[0].clone())
            .unwrap();
        let decoded = LinuxInstallJournal::decode(&journal.encode().unwrap()).unwrap();
        assert!(decoded.recovery_actions().is_empty());
        assert_eq!(journal.intend_services(), Err(invalid_transition()));
    }

    #[test]
    fn schema_six_persists_each_distinct_operation_mode() {
        for mode in [
            LinuxInstallMode::FreshInstall,
            LinuxInstallMode::OfflineUpgrade,
            LinuxInstallMode::OfflineRepair,
        ] {
            let journal = LinuxInstallJournal::new(
                mode,
                System::X8664Linux,
                Digest::from_bytes([0x81; 32]),
                Digest::from_bytes([0x82; 32]),
            )
            .unwrap();
            assert_eq!(
                LinuxInstallJournal::decode(&journal.encode().unwrap())
                    .unwrap()
                    .mode(),
                mode
            );
        }

        let old = journal()
            .encode()
            .unwrap()
            .windows(b"\"schemaVersion\":6".len())
            .position(|bytes| bytes == b"\"schemaVersion\":6")
            .unwrap();
        let mut bytes = journal().encode().unwrap();
        bytes[old + b"\"schemaVersion\":".len()] = b'5';
        assert_eq!(
            LinuxInstallJournal::decode(&bytes).map_err(LinuxInstallJournalError::code),
            Err(LinuxInstallJournalErrorCode::UnsupportedSchema)
        );

        let encoded = journal().encode().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("schemaVersion");
        for invalid in [
            missing,
            {
                let mut invalid = value.clone();
                invalid["schemaVersion"] = serde_json::json!("6");
                invalid
            },
            {
                let mut invalid = value;
                invalid["schemaVersion"] = serde_json::json!(6.5);
                invalid
            },
        ] {
            assert_eq!(
                LinuxInstallJournal::decode(&serde_json::to_vec(&invalid).unwrap())
                    .map_err(LinuxInstallJournalError::code),
                Err(LinuxInstallJournalErrorCode::InvalidJournal)
            );
        }

        let mut fresh = journal();
        for mutation in install_sequence()
            .into_iter()
            .take_while(|mutation| *mutation != LinuxInstallMutation::Services)
        {
            fresh.record_preexisting(mutation).unwrap();
        }
        assert_eq!(fresh.intend_services(), Ok(()));

        let mut upgrade = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineUpgrade,
            System::X8664Linux,
            Digest::from_bytes([0x83; 32]),
            Digest::from_bytes([0x84; 32]),
        )
        .unwrap();
        for mutation in install_sequence()
            .into_iter()
            .take_while(|mutation| *mutation != LinuxInstallMutation::Services)
        {
            upgrade.record_preexisting(mutation).unwrap();
        }
        assert_eq!(upgrade.intend_services(), Err(invalid_transition()));
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
        let gcroots_users = sequence.iter().position(|mutation| {
            mutation
                == &LinuxInstallMutation::Asset {
                    id: "nix-gcroots-users".to_owned(),
                }
        });
        let services = sequence
            .iter()
            .position(|mutation| mutation == &LinuxInstallMutation::Services);
        assert!(
            runtime
                .zip(gcroots)
                .zip(gcroots_users)
                .zip(services)
                .is_some_and(|(((runtime, gcroots), users), services)| {
                    runtime < gcroots && gcroots < users && users < services
                })
        );
    }
}
