//! Immutable generation/state bindings and rollback planning.

use std::collections::BTreeSet;
use std::fmt;

use crate::lifecycle::LifecycleState;
use crate::state::Generation;
use crate::{ChannelSequence, StorePath};

/// One immutable generation record proven to match its manifest/lock snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSnapshot {
    generation: Generation,
    state: LifecycleState,
}

/// Stable cross-file generation binding failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationSnapshotError {
    /// Generation ownership differs from lifecycle state.
    OwnerMismatch,
    /// Generation channel sequence differs from lifecycle state.
    ChannelMismatch,
    /// The activation root list differs from selected lock outputs.
    ActivationRootsMismatch,
    /// The generation output set differs from desired selector ids.
    OutputSetMismatch,
    /// At least one generation output differs from its manifest/lock binding.
    OutputMismatch,
}

impl fmt::Display for GenerationSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "generation snapshot refused: {self:?}")
    }
}

impl std::error::Error for GenerationSnapshotError {}

impl GenerationSnapshot {
    /// Validates ownership, channel, selected roots, and every documented output.
    pub fn new(
        generation: Generation,
        state: LifecycleState,
    ) -> Result<Self, GenerationSnapshotError> {
        if generation.uid() != state.manifest().uid() {
            return Err(GenerationSnapshotError::OwnerMismatch);
        }
        if generation.channel_seq() != state.manifest().channel_seq() {
            return Err(GenerationSnapshotError::ChannelMismatch);
        }
        if state.selected_output_paths() != generation.activation().output_roots() {
            return Err(GenerationSnapshotError::ActivationRootsMismatch);
        }
        let expected_ids = state
            .manifest()
            .entries()
            .iter()
            .map(super::state::schema::ManifestEntry::id)
            .collect::<BTreeSet<_>>();
        let output_ids = generation
            .outputs()
            .iter()
            .map(super::state::schema::GenerationOutput::id)
            .collect::<BTreeSet<_>>();
        if expected_ids != output_ids || generation.outputs().len() != expected_ids.len() {
            return Err(GenerationSnapshotError::OutputSetMismatch);
        }
        for manifest_entry in state.manifest().entries() {
            let locked_entry = &state.locked().entries()[manifest_entry.id()];
            let realization = locked_entry.realization();
            let output = generation
                .outputs()
                .iter()
                .find(|output| output.id() == manifest_entry.id())
                .ok_or(GenerationSnapshotError::OutputSetMismatch)?;
            if output.attribute() != locked_entry.attribute()
                || output.nixpkgs_revision() != realization.nixpkgs_revision()
                || output.store_path() != realization.store_path()
                || output.deriver() != realization.deriver()
                || output.outputs_to_install() != realization.outputs_to_install()
                || output.nar_hash() != realization.nar_hash()
                || output.closure_nar_size() != realization.closure_nar_size()
                || output.provenance() != locked_entry.provenance()
                || output.is_pinned() != manifest_entry.is_pinned()
            {
                return Err(GenerationSnapshotError::OutputMismatch);
            }
        }
        Ok(Self { generation, state })
    }

    /// Returns the immutable generation record.
    #[must_use]
    pub const fn generation(&self) -> &Generation {
        &self.generation
    }

    /// Returns the coherent point-in-time manifest/lock state.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }

    /// Returns selected output paths to re-stage for a fresh rollback generation.
    #[must_use]
    pub fn selected_output_paths(&self) -> Vec<StorePath> {
        self.state.selected_output_paths()
    }
}

/// User-selected rollback destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackTarget {
    /// Roll back to the active generation's recorded parent.
    Parent,
    /// Roll back to this exact retained generation id.
    Named(String),
}

/// A verified prior snapshot ready to become a fresh monotonic generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    active_generation: String,
    target: GenerationSnapshot,
}

impl RollbackPlan {
    /// Returns the generation active when this plan was derived.
    #[must_use]
    pub fn active_generation(&self) -> &str {
        &self.active_generation
    }
    /// Returns the verified prior snapshot to copy into a fresh generation.
    #[must_use]
    pub const fn target(&self) -> &GenerationSnapshot {
        &self.target
    }
    /// Returns the target channel sequence for compatibility/audit output.
    #[must_use]
    pub const fn target_channel_seq(&self) -> ChannelSequence {
        self.target.generation.channel_seq()
    }
}

/// Stable rollback selection failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackError {
    /// A named generation id violated the closed grammar.
    InvalidTarget,
    /// The active generation has no retained parent or the named id is absent.
    MissingTarget,
    /// The requested generation is already active.
    AlreadyActive,
    /// Duplicate generation ids made the archive ambiguous.
    DuplicateGeneration,
    /// Target ownership or platform differs from the active generation.
    IncompatibleState,
    /// Retained channel metadata reports an incompatible managed runtime.
    IncompatibleRuntime,
}

impl fmt::Display for RollbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "rollback refused: {self:?}")
    }
}

impl std::error::Error for RollbackError {}

/// Selects a verified rollback target without touching current state.
///
/// `is_runtime_compatible` is derived by the caller from retained,
/// authenticated channel metadata for the target's `channelSeq`.
pub fn plan_rollback(
    active: &GenerationSnapshot,
    retained: &[GenerationSnapshot],
    target: RollbackTarget,
    is_runtime_compatible: impl FnOnce(ChannelSequence) -> bool,
) -> Result<RollbackPlan, RollbackError> {
    let requested = match target {
        RollbackTarget::Parent => active
            .generation
            .parent()
            .map(str::to_owned)
            .ok_or(RollbackError::MissingTarget)?,
        RollbackTarget::Named(id) => {
            if !valid_generation_id(&id) {
                return Err(RollbackError::InvalidTarget);
            }
            id
        }
    };
    if requested == active.generation.id() {
        return Err(RollbackError::AlreadyActive);
    }
    let mut ids = BTreeSet::new();
    if !ids.insert(active.generation.id())
        || retained
            .iter()
            .any(|snapshot| !ids.insert(snapshot.generation.id()))
    {
        return Err(RollbackError::DuplicateGeneration);
    }
    let target = retained
        .iter()
        .find(|snapshot| snapshot.generation.id() == requested)
        .cloned()
        .ok_or(RollbackError::MissingTarget)?;
    if target.state.manifest().uid() != active.state.manifest().uid()
        || target.state.locked().system() != active.state.locked().system()
    {
        return Err(RollbackError::IncompatibleState);
    }
    if !is_runtime_compatible(target.generation.channel_seq()) {
        return Err(RollbackError::IncompatibleRuntime);
    }
    Ok(RollbackPlan {
        active_generation: active.generation.id().to_owned(),
        target,
    })
}

fn valid_generation_id(value: &str) -> bool {
    value.len() <= 64
        && value.strip_prefix("gen-").is_some_and(|digits| {
            digits.len() >= 4 && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_test_support::{snapshot, state};
    use crate::{PinAction, SelectorId, edit_pins};

    #[test]
    fn rollback_selects_parent_for_fresh_rematerialization() {
        let previous = snapshot("gen-0001", None, state(), "install");
        let pinned_state = edit_pins(
            state(),
            &[SelectorId::new("sel_a").unwrap()],
            PinAction::Pin,
        )
        .unwrap()
        .into_state();
        let active = snapshot("gen-0002", Some("gen-0001"), pinned_state, "pin");
        let plan = plan_rollback(
            &active,
            std::slice::from_ref(&previous),
            RollbackTarget::Parent,
            |_| true,
        )
        .unwrap();
        assert_eq!(plan.active_generation(), "gen-0002");
        assert_eq!(plan.target().generation().id(), "gen-0001");
        assert_eq!(
            plan.target().selected_output_paths(),
            previous.selected_output_paths()
        );
    }

    #[test]
    fn rollback_refuses_missing_invalid_duplicate_and_incompatible_targets() {
        let previous = snapshot("gen-0001", None, state(), "install");
        let active = snapshot("gen-0002", Some("gen-0001"), state(), "install");
        assert_eq!(
            plan_rollback(
                &active,
                std::slice::from_ref(&previous),
                RollbackTarget::Named("../gen-0001".into()),
                |_| true
            ),
            Err(RollbackError::InvalidTarget)
        );
        assert_eq!(
            plan_rollback(&active, &[], RollbackTarget::Parent, |_| true),
            Err(RollbackError::MissingTarget)
        );
        assert_eq!(
            plan_rollback(
                &active,
                &[previous.clone(), previous.clone()],
                RollbackTarget::Parent,
                |_| true
            ),
            Err(RollbackError::DuplicateGeneration)
        );
        assert_eq!(
            plan_rollback(&active, &[previous], RollbackTarget::Parent, |_| false),
            Err(RollbackError::IncompatibleRuntime)
        );
    }
}
