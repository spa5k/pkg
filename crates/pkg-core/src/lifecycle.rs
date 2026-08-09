//! Coherent manifest/lock state used by lifecycle operations.

use std::collections::BTreeSet;
use std::fmt;

use crate::state::{LockedState, Manifest};
use crate::{PackageSelector, SourceRevision, StorePath};

/// A manifest and lockfile proven to describe the same desired-state set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleState {
    manifest: Manifest,
    locked: LockedState,
}

/// Stable failures when binding desired intent to exact locked state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleError {
    /// Manifest and lock ownership differs.
    OwnerMismatch,
    /// Manifest and lock channel sequence differs.
    ChannelMismatch,
    /// Manifest and lock selector-id sets differ.
    EntrySetMismatch,
    /// A selector's canonical attribute differs across the two files.
    AttributeMismatch,
    /// A pinned selector does not name its locked primary store path.
    PinMismatch,
    /// An exact-revision selector disagrees with its locked realization.
    RevisionMismatch,
    /// A locked realization targets a different system than its lockfile.
    SystemMismatch,
    /// Validated persisted intent could not be promoted to a resolver request.
    InvalidSelector,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lifecycle state refused: {self:?}")
    }
}

impl std::error::Error for LifecycleError {}

impl LifecycleState {
    /// Validates every cross-file ownership, identity, pin, and revision binding.
    pub fn new(manifest: Manifest, locked: LockedState) -> Result<Self, LifecycleError> {
        if manifest.uid() != locked.uid() {
            return Err(LifecycleError::OwnerMismatch);
        }
        if manifest.channel_seq() != locked.channel_seq() {
            return Err(LifecycleError::ChannelMismatch);
        }
        let manifest_ids = manifest
            .entries()
            .iter()
            .map(|entry| entry.id().clone())
            .collect::<BTreeSet<_>>();
        let locked_ids = locked.entries().keys().cloned().collect::<BTreeSet<_>>();
        if manifest_ids != locked_ids {
            return Err(LifecycleError::EntrySetMismatch);
        }
        for entry in manifest.entries() {
            let locked_entry = &locked.entries()[entry.id()];
            if locked_entry.realization().system() != locked.system() {
                return Err(LifecycleError::SystemMismatch);
            }
            if entry.attribute() != locked_entry.attribute() {
                return Err(LifecycleError::AttributeMismatch);
            }
            if entry
                .pinned_to()
                .is_some_and(|path| path != locked_entry.realization().store_path())
            {
                return Err(LifecycleError::PinMismatch);
            }
            if let SourceRevision::ExactRevision(revision) = entry.source_revision()
                && revision != locked_entry.realization().nixpkgs_revision()
            {
                return Err(LifecycleError::RevisionMismatch);
            }
        }
        Ok(Self { manifest, locked })
    }

    /// Returns the validated desired-state manifest.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Returns the validated exact lock state.
    #[must_use]
    pub const fn locked(&self) -> &LockedState {
        &self.locked
    }

    /// Returns selected output paths in deterministic store-path order.
    #[must_use]
    pub fn selected_output_paths(&self) -> Vec<StorePath> {
        let mut paths = self
            .locked
            .entries()
            .values()
            .flat_map(|entry| {
                let realization = entry.realization();
                realization
                    .outputs_to_install()
                    .iter()
                    .map(|output| realization.outputs()[output].clone())
            })
            .collect::<Vec<_>>();
        paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        paths.dedup_by(|left, right| left.as_str() == right.as_str());
        paths
    }

    /// Separates the validated pair for crash-safe generation serialization.
    #[must_use]
    pub fn into_parts(self) -> (Manifest, LockedState) {
        (self.manifest, self.locked)
    }

    pub(crate) fn selector_for_current_upgrade(
        &self,
        id: &crate::SelectorId,
    ) -> Result<PackageSelector, LifecycleError> {
        let entry = self
            .manifest
            .entries()
            .iter()
            .find(|entry| entry.id() == id)
            .ok_or(LifecycleError::EntrySetMismatch)?;
        PackageSelector::new(
            entry.id().clone(),
            entry.selector().clone(),
            entry.version_preference().clone(),
            entry.outputs().clone(),
            SourceRevision::CurrentChannel,
        )
        .with_attribute(entry.attribute().clone())
        .map_err(|_| LifecycleError::InvalidSelector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_test_support::{REV1, state, store};
    use crate::state::{LockedState, Manifest};

    #[test]
    fn binds_cross_file_identity_and_lists_selected_outputs_deterministically() {
        let state = state();
        assert_eq!(
            state
                .selected_output_paths()
                .iter()
                .map(StorePath::as_str)
                .collect::<Vec<_>>(),
            [
                store('0', "alpha"),
                store('1', "beta"),
                store('2', "charlie")
            ]
        );

        let bad_manifest = String::from_utf8(state.manifest().to_json().unwrap())
            .unwrap()
            .replace(REV1, crate::lifecycle_test_support::REV2);
        let manifest = Manifest::from_json(bad_manifest.as_bytes()).unwrap();
        let locked = state.locked().clone();
        assert_eq!(
            LifecycleState::new(manifest, locked),
            Err(LifecycleError::RevisionMismatch)
        );
    }

    #[test]
    fn refuses_manifest_lock_entry_set_mismatch() {
        let state = state();
        let manifest = Manifest::from_json(
            &state
                .manifest()
                .to_json()
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let lock = LockedState::from_json(
            br#"{"schemaVersion":1,"channelSeq":2,"system":"x86_64-linux","uid":1001,"entries":{}}"#,
        )
        .unwrap();
        assert_eq!(
            LifecycleState::new(manifest, lock),
            Err(LifecycleError::EntrySetMismatch)
        );
    }
}
