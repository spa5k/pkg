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
mod tests;
