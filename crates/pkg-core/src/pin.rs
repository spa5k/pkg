//! Atomic pin and unpin desired-state edits.

use std::collections::BTreeSet;
use std::fmt;

use crate::SelectorId;
use crate::lifecycle::{LifecycleError, LifecycleState};
use crate::state::{LockedState, Manifest};

/// Pin-state edit requested for installed selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    /// Freeze each selector to its current primary realization identity.
    Pin,
    /// Clear the exact realization pin.
    Unpin,
}

/// Successful pin edit with changed and already-satisfied targets separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinResult {
    state: LifecycleState,
    changed: Vec<SelectorId>,
    unchanged: Vec<SelectorId>,
}

impl PinResult {
    /// Returns the coherent next state.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }
    /// Returns targets whose desired pin intent changed.
    #[must_use]
    pub fn changed(&self) -> &[SelectorId] {
        &self.changed
    }
    /// Returns targets already satisfying the requested intent.
    #[must_use]
    pub fn unchanged(&self) -> &[SelectorId] {
        &self.unchanged
    }
    /// Consumes the result and returns the next state.
    #[must_use]
    pub fn into_state(self) -> LifecycleState {
        self.state
    }
}

/// Stable pin/unpin edit failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    /// At least one target is required.
    EmptyRequest,
    /// A target appeared more than once.
    DuplicateTarget,
    /// At least one requested selector is not installed.
    NotInstalled,
    /// The resulting manifest/lock pair violated a lifecycle invariant.
    InvalidState,
}

impl fmt::Display for PinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pin edit refused: {self:?}")
    }
}
impl std::error::Error for PinError {}

/// Applies pin intent to every target atomically without changing exact lock entries.
pub fn edit_pins(
    state: LifecycleState,
    targets: &[SelectorId],
    action: PinAction,
) -> Result<PinResult, PinError> {
    if targets.is_empty() {
        return Err(PinError::EmptyRequest);
    }
    let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
    if target_set.len() != targets.len() {
        return Err(PinError::DuplicateTarget);
    }
    if !target_set
        .iter()
        .all(|id| state.locked().entries().contains_key(id))
    {
        return Err(PinError::NotInstalled);
    }

    let channel_seq = state.manifest().channel_seq();
    let uid = state.manifest().uid();
    let system = state.locked().system();
    let locked_by_id = state.locked().entries().clone();
    let (manifest, locked) = state.into_parts();
    let mut changed_set = BTreeSet::new();
    let manifest_entries = manifest
        .into_lifecycle_entries()
        .into_iter()
        .map(|entry| {
            if !target_set.contains(entry.id()) {
                return entry;
            }
            let next_pin = match action {
                PinAction::Pin => Some(locked_by_id[entry.id()].realization().store_path().clone()),
                PinAction::Unpin => None,
            };
            if entry.pinned_to() != next_pin.as_ref() {
                changed_set.insert(entry.id().clone());
            }
            entry.with_pin(next_pin)
        })
        .collect();
    let manifest = Manifest::from_lifecycle_parts(channel_seq, uid, manifest_entries);
    let locked = LockedState::from_lifecycle_parts(
        channel_seq,
        system,
        uid,
        locked.into_lifecycle_entries(),
    );
    let state = LifecycleState::new(manifest, locked).map_err(map_lifecycle_error)?;
    let changed = targets
        .iter()
        .filter(|id| changed_set.contains(*id))
        .cloned()
        .collect();
    let unchanged = targets
        .iter()
        .filter(|id| !changed_set.contains(*id))
        .cloned()
        .collect();
    Ok(PinResult {
        state,
        changed,
        unchanged,
    })
}

const fn map_lifecycle_error(_: LifecycleError) -> PinError {
    PinError::InvalidState
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_test_support::state;

    fn id(value: &str) -> SelectorId {
        SelectorId::new(value).unwrap()
    }

    #[test]
    fn pin_and_unpin_change_only_manifest_intent() {
        let original = state();
        let original_lock = original.locked().clone();
        let original_outputs = original.selected_output_paths();
        let pinned = edit_pins(original, &[id("sel_a")], PinAction::Pin).unwrap();
        assert_eq!(pinned.changed(), [id("sel_a")]);
        assert_eq!(pinned.state().locked(), &original_lock);
        assert_eq!(pinned.state().selected_output_paths(), original_outputs);
        let entry = &pinned.state().manifest().entries()[0];
        assert!(entry.is_pinned());
        assert_eq!(
            entry.pinned_to(),
            Some(
                pinned.state().locked().entries()[&id("sel_a")]
                    .realization()
                    .store_path()
            )
        );

        let unpinned = edit_pins(pinned.into_state(), &[id("sel_a")], PinAction::Unpin).unwrap();
        assert_eq!(unpinned.changed(), [id("sel_a")]);
        assert!(!unpinned.state().manifest().entries()[0].is_pinned());
    }

    #[test]
    fn already_satisfied_and_missing_targets_are_explicit() {
        let original = state();
        let pinned = edit_pins(original.clone(), &[id("sel_c")], PinAction::Pin).unwrap();
        assert!(pinned.changed().is_empty());
        assert_eq!(pinned.unchanged(), [id("sel_c")]);
        assert_eq!(pinned.into_state(), original);
        assert_eq!(
            edit_pins(original, &[id("sel_missing")], PinAction::Pin),
            Err(PinError::NotInstalled)
        );
    }
}
