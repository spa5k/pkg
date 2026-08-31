//! Offline generation history and sanitized desired-state diffs.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{GenerationSnapshot, OutputName, PackageVersion, SelectorId, SelectorInput};

/// One sanitized package-level history change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A selector was added.
    Added,
    /// A selector was removed.
    Removed,
    /// Desired intent or exact realization changed.
    Changed,
}

/// One product-facing package delta with no store or derivation identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageChange {
    id: SelectorId,
    selector: SelectorInput,
    kind: ChangeKind,
    before_version: Option<PackageVersion>,
    after_version: Option<PackageVersion>,
    before_outputs: Vec<OutputName>,
    after_outputs: Vec<OutputName>,
    before_pinned: Option<bool>,
    after_pinned: Option<bool>,
}

impl PackageChange {
    /// Returns the stable selector id.
    #[must_use]
    pub const fn id(&self) -> &SelectorId {
        &self.id
    }
    /// Returns the original product-facing selector.
    #[must_use]
    pub const fn selector(&self) -> &SelectorInput {
        &self.selector
    }
    /// Returns the change class.
    #[must_use]
    pub const fn kind(&self) -> ChangeKind {
        self.kind
    }
    /// Returns the prior display version, when present.
    #[must_use]
    pub const fn before_version(&self) -> Option<&PackageVersion> {
        self.before_version.as_ref()
    }
    /// Returns the next display version, when present.
    #[must_use]
    pub const fn after_version(&self) -> Option<&PackageVersion> {
        self.after_version.as_ref()
    }
    /// Returns prior selected output names.
    #[must_use]
    pub fn before_outputs(&self) -> &[OutputName] {
        &self.before_outputs
    }
    /// Returns next selected output names.
    #[must_use]
    pub fn after_outputs(&self) -> &[OutputName] {
        &self.after_outputs
    }
    /// Returns prior pin state, when present.
    #[must_use]
    pub const fn before_pinned(&self) -> Option<bool> {
        self.before_pinned
    }
    /// Returns next pin state, when present.
    #[must_use]
    pub const fn after_pinned(&self) -> Option<bool> {
        self.after_pinned
    }
}

/// Counts rendered as history's `+added ~changed -removed` summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    /// Added selector count.
    pub added: usize,
    /// Changed selector count.
    pub changed: usize,
    /// Removed selector count.
    pub removed: usize,
}

/// Deterministically ordered package changes between two generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDiff {
    changes: Vec<PackageChange>,
    counts: ChangeCounts,
}

impl HistoryDiff {
    /// Returns changes sorted by stable selector id.
    #[must_use]
    pub fn changes(&self) -> &[PackageChange] {
        &self.changes
    }
    /// Returns aggregate added/changed/removed counts.
    #[must_use]
    pub const fn counts(&self) -> ChangeCounts {
        self.counts
    }
}

/// One product-facing generation history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySummary {
    id: String,
    created_at: String,
    operation: String,
    active: bool,
    changes_from_parent: Option<ChangeCounts>,
}

impl HistorySummary {
    /// Returns the generation id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the persisted creation timestamp.
    #[must_use]
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
    /// Returns the product operation kind.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }
    /// Returns whether this row is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }
    /// Returns the diff count when the parent is still retained.
    #[must_use]
    pub const fn changes_from_parent(&self) -> Option<ChangeCounts> {
        self.changes_from_parent
    }
}

/// Verified retained generation history, newest generation id first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    snapshots: Vec<GenerationSnapshot>,
    summaries: Vec<HistorySummary>,
}

/// Stable retained-history failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    /// An empty archive unexpectedly claimed an active generation.
    ActiveWithoutHistory,
    /// A nonempty archive omitted or misnamed the active generation.
    MissingActive,
    /// Duplicate generation ids made ordering ambiguous.
    DuplicateGeneration,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "history refused: {self:?}")
    }
}
impl std::error::Error for HistoryError {}

impl History {
    /// Validates active membership and deterministically orders retained snapshots.
    pub fn new(
        mut snapshots: Vec<GenerationSnapshot>,
        active_id: Option<&str>,
    ) -> Result<Self, HistoryError> {
        if snapshots.is_empty() {
            return if active_id.is_some() {
                Err(HistoryError::ActiveWithoutHistory)
            } else {
                Ok(Self {
                    snapshots,
                    summaries: Vec::new(),
                })
            };
        }
        let active_id = active_id.ok_or(HistoryError::MissingActive)?;
        let mut ids = BTreeSet::new();
        if snapshots
            .iter()
            .any(|snapshot| !ids.insert(snapshot.generation().id()))
        {
            return Err(HistoryError::DuplicateGeneration);
        }
        if !ids.contains(active_id) {
            return Err(HistoryError::MissingActive);
        }
        snapshots.sort_by(|left, right| {
            compare_generation_ids(right.generation().id(), left.generation().id())
        });
        let by_id = snapshots
            .iter()
            .map(|snapshot| (snapshot.generation().id(), snapshot))
            .collect::<BTreeMap<_, _>>();
        let summaries = snapshots
            .iter()
            .map(|snapshot| HistorySummary {
                id: snapshot.generation().id().to_owned(),
                created_at: snapshot.generation().created_at().to_owned(),
                operation: snapshot.generation().operation().kind().to_owned(),
                active: snapshot.generation().id() == active_id,
                changes_from_parent: snapshot
                    .generation()
                    .parent()
                    .and_then(|parent| by_id.get(parent))
                    .map(|parent| Self::diff(parent, snapshot).counts()),
            })
            .collect();
        Ok(Self {
            snapshots,
            summaries,
        })
    }

    /// Returns verified snapshots newest first.
    #[must_use]
    pub fn snapshots(&self) -> &[GenerationSnapshot] {
        &self.snapshots
    }
    /// Returns sanitized rows newest first.
    #[must_use]
    pub fn summaries(&self) -> &[HistorySummary] {
        &self.summaries
    }

    /// Computes a deterministic product-facing diff between two snapshots.
    #[must_use]
    pub fn diff(from: &GenerationSnapshot, to: &GenerationSnapshot) -> HistoryDiff {
        let from_manifest = from
            .state()
            .manifest()
            .entries()
            .iter()
            .map(|entry| (entry.id(), entry))
            .collect::<BTreeMap<_, _>>();
        let to_manifest = to
            .state()
            .manifest()
            .entries()
            .iter()
            .map(|entry| (entry.id(), entry))
            .collect::<BTreeMap<_, _>>();
        let ids = from_manifest
            .keys()
            .chain(to_manifest.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let mut changes = Vec::new();
        for id in ids {
            let before_manifest = from_manifest.get(id).copied();
            let after_manifest = to_manifest.get(id).copied();
            let before_lock = from.state().locked().entries().get(id);
            let after_lock = to.state().locked().entries().get(id);
            let kind = match (before_manifest, after_manifest) {
                (None, Some(_)) => ChangeKind::Added,
                (Some(_), None) => ChangeKind::Removed,
                (Some(before), Some(after)) if before != after || before_lock != after_lock => {
                    ChangeKind::Changed
                }
                _ => continue,
            };
            let Some(selector_entry) = after_manifest.or(before_manifest) else {
                continue;
            };
            let selector = selector_entry.selector().clone();
            changes.push(PackageChange {
                id: id.clone(),
                selector,
                kind,
                before_version: before_lock.map(|entry| entry.realization().version().clone()),
                after_version: after_lock.map(|entry| entry.realization().version().clone()),
                before_outputs: before_lock
                    .map(|entry| entry.realization().outputs_to_install().to_vec())
                    .unwrap_or_default(),
                after_outputs: after_lock
                    .map(|entry| entry.realization().outputs_to_install().to_vec())
                    .unwrap_or_default(),
                before_pinned: before_manifest.map(|entry| entry.is_pinned()),
                after_pinned: after_manifest.map(|entry| entry.is_pinned()),
            });
        }
        let counts = ChangeCounts {
            added: changes
                .iter()
                .filter(|change| change.kind == ChangeKind::Added)
                .count(),
            changed: changes
                .iter()
                .filter(|change| change.kind == ChangeKind::Changed)
                .count(),
            removed: changes
                .iter()
                .filter(|change| change.kind == ChangeKind::Removed)
                .count(),
        };
        HistoryDiff { changes, counts }
    }
}

fn compare_generation_ids(left: &str, right: &str) -> Ordering {
    let left = left.strip_prefix("gen-").unwrap_or(left);
    let right = right.strip_prefix("gen-").unwrap_or(right);
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_test_support::{snapshot, state};
    use crate::remove::remove_selectors;
    use crate::{PinAction, SelectorId, edit_pins};

    fn id(value: &str) -> SelectorId {
        SelectorId::new(value).unwrap()
    }

    fn three_generations() -> Vec<GenerationSnapshot> {
        let first_state = state();
        let first = snapshot("gen-0001", None, first_state.clone(), "install");
        let second_state = edit_pins(first_state, &[id("sel_a")], PinAction::Pin)
            .unwrap()
            .into_state();
        let second = snapshot("gen-0002", Some("gen-0001"), second_state.clone(), "pin");
        let third_state = remove_selectors(second_state, &[id("sel_b")])
            .unwrap()
            .into_state();
        let third = snapshot("gen-0010", Some("gen-0002"), third_state, "remove");
        vec![first, second, third]
    }

    #[test]
    fn history_is_numeric_newest_first_with_parent_change_counts() {
        let history = History::new(three_generations(), Some("gen-0010")).unwrap();
        assert_eq!(
            history
                .summaries()
                .iter()
                .map(HistorySummary::id)
                .collect::<Vec<_>>(),
            ["gen-0010", "gen-0002", "gen-0001"]
        );
        assert!(history.summaries()[0].is_active());
        assert_eq!(
            history.summaries()[0].changes_from_parent(),
            Some(ChangeCounts {
                added: 0,
                changed: 0,
                removed: 1
            })
        );
        assert_eq!(
            history.summaries()[1].changes_from_parent(),
            Some(ChangeCounts {
                added: 0,
                changed: 1,
                removed: 0
            })
        );
    }

    #[test]
    fn diff_is_sanitized_stable_and_detects_pin_plus_remove() {
        let generations = three_generations();
        let diff = History::diff(&generations[0], &generations[2]);
        assert_eq!(
            diff.counts(),
            ChangeCounts {
                added: 0,
                changed: 1,
                removed: 1
            }
        );
        assert_eq!(
            diff.changes()
                .iter()
                .map(|change| (change.id().as_str(), change.kind()))
                .collect::<Vec<_>>(),
            [
                ("sel_a", ChangeKind::Changed),
                ("sel_b", ChangeKind::Removed)
            ]
        );
        assert_eq!(diff.changes()[0].before_version().unwrap().as_str(), "1.0");
        assert_eq!(diff.changes()[0].after_version().unwrap().as_str(), "1.0");
        assert_eq!(diff.changes()[0].before_pinned(), Some(false));
        assert_eq!(diff.changes()[0].after_pinned(), Some(true));
    }

    #[test]
    fn history_requires_exact_active_membership() {
        assert_eq!(
            History::new(three_generations(), Some("gen-9999")),
            Err(HistoryError::MissingActive)
        );
        assert_eq!(
            History::new(Vec::new(), Some("gen-0001")),
            Err(HistoryError::ActiveWithoutHistory)
        );
    }
}
