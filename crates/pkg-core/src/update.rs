//! Metadata-only accepted-channel advancement.

use std::fmt;

use crate::ChannelSequence;
use crate::lifecycle::LifecycleState;
use crate::state::{LockedState, Manifest};

/// Closed failures for a metadata-only channel state update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelUpdateError {
    /// The authenticated target sequence is older than the local state.
    SequenceRollback,
    /// Rebinding the validated manifest and lock pair failed.
    InvalidState,
}

impl fmt::Display for ChannelUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("metadata-only channel update refused")
    }
}

impl std::error::Error for ChannelUpdateError {}

/// Advances only the accepted channel sequence in a validated lifecycle state.
///
/// Per-package realization revisions and all desired package intent remain
/// unchanged. A later `upgrade` decides which exact realizations move.
///
/// # Errors
///
/// Refuses sequence rollback or a lifecycle pair that cannot be reconstructed.
pub fn advance_channel(
    state: LifecycleState,
    sequence: ChannelSequence,
) -> Result<LifecycleState, ChannelUpdateError> {
    if sequence.get() < state.manifest().channel_seq().get() {
        return Err(ChannelUpdateError::SequenceRollback);
    }
    let (manifest, locked) = state.into_parts();
    let manifest =
        Manifest::from_lifecycle_parts(sequence, manifest.uid(), manifest.into_lifecycle_entries());
    let locked = LockedState::from_lifecycle_parts(
        sequence,
        locked.system(),
        locked.uid(),
        locked.into_lifecycle_entries(),
    );
    LifecycleState::new(manifest, locked).map_err(|_| ChannelUpdateError::InvalidState)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_test_support::state;

    #[test]
    fn metadata_update_changes_only_the_top_level_channel_binding() {
        let current = state();
        let before_manifest = current.manifest().entries().to_vec();
        let before_lock = current.locked().entries().clone();

        let updated = advance_channel(current, ChannelSequence::from_u64(3).unwrap()).unwrap();

        assert_eq!(updated.manifest().channel_seq().get().get(), 3);
        assert_eq!(updated.locked().channel_seq().get().get(), 3);
        assert_eq!(updated.manifest().entries(), before_manifest);
        assert_eq!(updated.locked().entries(), &before_lock);
    }

    #[test]
    fn metadata_update_refuses_sequence_rollback() {
        assert_eq!(
            advance_channel(state(), ChannelSequence::from_u64(1).unwrap()),
            Err(ChannelUpdateError::SequenceRollback)
        );
    }
}
