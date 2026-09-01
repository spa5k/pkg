//! Atomic desired-state removal.

use std::collections::BTreeSet;
use std::fmt;

use crate::SelectorId;
use crate::lifecycle::{LifecycleError, LifecycleState};
use crate::state::{LockedState, Manifest};

/// Successful removal with a coherent next manifest/lock pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveResult {
    state: LifecycleState,
    removed: Vec<SelectorId>,
}

impl RemoveResult {
    /// Returns the next exact lifecycle state.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }

    /// Returns removed selector ids in request order.
    #[must_use]
    pub fn removed(&self) -> &[SelectorId] {
        &self.removed
    }

    /// Consumes the result and returns the next state.
    #[must_use]
    pub fn into_state(self) -> LifecycleState {
        self.state
    }
}

/// Stable atomic removal failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveError {
    /// At least one target is required.
    EmptyRequest,
    /// A selector id appeared more than once.
    DuplicateTarget,
    /// At least one requested selector is not installed.
    NotInstalled,
    /// The resulting manifest/lock pair violated a lifecycle invariant.
    InvalidState,
}

impl fmt::Display for RemoveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remove refused: {self:?}")
    }
}

impl std::error::Error for RemoveError {}

/// Removes all named selectors from manifest and lock atomically.
pub fn remove_selectors(
    state: LifecycleState,
    targets: &[SelectorId],
) -> Result<RemoveResult, RemoveError> {
    if targets.is_empty() {
        return Err(RemoveError::EmptyRequest);
    }
    let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
    if target_set.len() != targets.len() {
        return Err(RemoveError::DuplicateTarget);
    }
    if !target_set
        .iter()
        .all(|id| state.locked().entries().contains_key(id))
    {
        return Err(RemoveError::NotInstalled);
    }

    let channel_seq = state.manifest().channel_seq();
    let uid = state.manifest().uid();
    let system = state.locked().system();
    let (manifest, locked) = state.into_parts();
    let manifest_entries = manifest
        .into_lifecycle_entries()
        .into_iter()
        .filter(|entry| !target_set.contains(entry.id()))
        .collect();
    let mut locked_entries = locked.into_lifecycle_entries();
    locked_entries.retain(|id, _| !target_set.contains(id));
    let manifest = Manifest::from_lifecycle_parts(channel_seq, uid, manifest_entries);
    let locked = LockedState::from_lifecycle_parts(channel_seq, system, uid, locked_entries);
    let state = LifecycleState::new(manifest, locked).map_err(map_lifecycle_error)?;
    Ok(RemoveResult {
        state,
        removed: targets.to_vec(),
    })
}

const fn map_lifecycle_error(_: LifecycleError) -> RemoveError {
    RemoveError::InvalidState
}

#[cfg(test)]
mod tests;
