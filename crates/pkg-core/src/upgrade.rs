//! Selective and all-package upgrade planning with mixed-revision preservation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::lifecycle::{LifecycleError, LifecycleState};
use crate::state::{LockEntry, LockedState, Manifest};
use crate::{ChannelSequence, NixpkgsRevision, PackageSelector, SelectorId};

/// Which installed selectors an upgrade should reconsider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeScope {
    /// Upgrade exactly these stable selector ids, in caller order.
    Named(Vec<SelectorId>),
    /// Upgrade every eligible selector in manifest order.
    All,
}

/// How an authenticated resolution miss for a formerly installed attribute is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedUpstreamPolicy {
    /// Fail the complete operation without changing desired state.
    Refuse,
    /// Preserve the old exact lock entry and report it as skipped.
    Skip,
}

/// One externally resolved result returned to the pure desired-state editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// Resolution and verified acquisition produced a replacement lock entry.
    Resolved {
        /// Stable selector being replaced.
        id: SelectorId,
        /// New exact, verified lock entry.
        entry: Box<LockEntry>,
    },
    /// The selector's prior attribute no longer exists in the accepted source.
    RemovedUpstream {
        /// Stable selector whose old exact state remains available.
        id: SelectorId,
    },
}

impl UpgradeOutcome {
    /// Constructs a verified replacement outcome.
    #[must_use]
    pub fn resolved(id: SelectorId, entry: LockEntry) -> Self {
        Self::Resolved {
            id,
            entry: Box::new(entry),
        }
    }

    /// Constructs an authenticated removed-upstream outcome.
    #[must_use]
    pub fn removed_upstream(id: SelectorId) -> Self {
        Self::RemovedUpstream { id }
    }

    fn id(&self) -> &SelectorId {
        match self {
            Self::Resolved { id, .. } | Self::RemovedUpstream { id } => id,
        }
    }
}

/// Mutation-free selection of installed packages eligible for re-resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeSelection {
    state: LifecycleState,
    target_ids: Vec<SelectorId>,
    selectors: Vec<PackageSelector>,
    skipped_pinned: Vec<SelectorId>,
    bump_pinned: bool,
}

impl UpgradeSelection {
    /// Resolver requests forced to the authenticated current channel.
    #[must_use]
    pub fn selectors(&self) -> &[PackageSelector] {
        &self.selectors
    }

    /// Pinned selectors deliberately left untouched by the default policy.
    #[must_use]
    pub fn skipped_pinned(&self) -> &[SelectorId] {
        &self.skipped_pinned
    }

    /// Binds this selection to the exact authenticated channel used for acquisition.
    pub fn bind_channel(
        self,
        target_sequence: ChannelSequence,
        target_revision: NixpkgsRevision,
    ) -> Result<UpgradePlan, UpgradeError> {
        if target_sequence.get() < self.state.manifest().channel_seq().get() {
            return Err(UpgradeError::SequenceRollback);
        }
        Ok(UpgradePlan {
            selection: self,
            target_sequence,
            target_revision,
        })
    }
}

/// Upgrade selection bound to one authenticated target channel identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePlan {
    selection: UpgradeSelection,
    target_sequence: ChannelSequence,
    target_revision: NixpkgsRevision,
}

impl UpgradePlan {
    /// Resolver requests, each forced to the currently accepted pinned channel.
    #[must_use]
    pub fn selectors(&self) -> &[PackageSelector] {
        self.selection.selectors()
    }

    /// Pinned selectors deliberately left untouched by the default policy.
    #[must_use]
    pub fn skipped_pinned(&self) -> &[SelectorId] {
        self.selection.skipped_pinned()
    }

    /// Applies one exact outcome per resolver request, atomically.
    pub fn apply(
        self,
        outcomes: Vec<UpgradeOutcome>,
        removed_upstream: RemovedUpstreamPolicy,
    ) -> Result<UpgradeResult, UpgradeError> {
        let mut by_id = BTreeMap::new();
        for outcome in outcomes {
            let id = outcome.id().clone();
            if !self.selection.target_ids.contains(&id) {
                return Err(UpgradeError::UnexpectedOutcome);
            }
            if by_id.insert(id, outcome).is_some() {
                return Err(UpgradeError::DuplicateOutcome);
            }
        }
        if by_id.len() != self.selection.target_ids.len() {
            return Err(UpgradeError::IncompleteOutcomes);
        }
        let removed_ids = self
            .selection
            .target_ids
            .iter()
            .filter(|id| matches!(by_id.get(*id), Some(UpgradeOutcome::RemovedUpstream { .. })))
            .cloned()
            .collect::<Vec<_>>();
        if removed_upstream == RemovedUpstreamPolicy::Refuse && !removed_ids.is_empty() {
            return Err(UpgradeError::RemovedUpstream);
        }

        let original = self.selection.state.clone();
        let uid = self.selection.state.manifest().uid();
        let system = self.selection.state.locked().system();
        for outcome in by_id.values() {
            if let UpgradeOutcome::Resolved { id, entry } = outcome {
                if entry.realization().system() != system {
                    return Err(UpgradeError::SystemMismatch);
                }
                if entry.realization().nixpkgs_revision() != &self.target_revision {
                    return Err(UpgradeError::RevisionMismatch);
                }
                let planned_attribute = self
                    .selection
                    .selectors
                    .iter()
                    .find(|selector| selector.id() == id)
                    .and_then(PackageSelector::attribute)
                    .ok_or(UpgradeError::InvalidState)?;
                if entry.attribute() != planned_attribute {
                    return Err(UpgradeError::AttributeMismatch);
                }
            }
        }
        let changed_ids = self
            .selection
            .target_ids
            .iter()
            .filter(|id| {
                let Some(UpgradeOutcome::Resolved { entry, .. }) = by_id.get(*id) else {
                    return false;
                };
                let clears_pin = self.selection.bump_pinned
                    && self
                        .selection
                        .state
                        .manifest()
                        .entries()
                        .iter()
                        .any(|manifest| manifest.id() == *id && manifest.is_pinned());
                clears_pin
                    || !same_resolved_package(&self.selection.state.locked().entries()[*id], entry)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if changed_ids.is_empty() {
            return Ok(UpgradeResult {
                state: original,
                upgraded: Vec::new(),
                skipped_pinned: self.selection.skipped_pinned,
                removed_upstream: removed_ids,
                changed: false,
            });
        }
        by_id.retain(|id, outcome| {
            changed_ids.contains(id) || matches!(outcome, UpgradeOutcome::RemovedUpstream { .. })
        });
        let (manifest, locked) = self.selection.state.into_parts();
        let manifest_entries = manifest
            .into_lifecycle_entries()
            .into_iter()
            .map(|entry| match by_id.get(entry.id()) {
                Some(UpgradeOutcome::Resolved {
                    entry: replacement, ..
                }) => entry.retarget_for_upgrade(
                    replacement.attribute().clone(),
                    self.selection.bump_pinned,
                ),
                _ => entry,
            })
            .collect();
        let mut locked_entries = locked.into_lifecycle_entries();
        for (id, outcome) in by_id {
            if let UpgradeOutcome::Resolved { entry, .. } = outcome {
                locked_entries.insert(id, *entry);
            }
        }
        let manifest = Manifest::from_lifecycle_parts(self.target_sequence, uid, manifest_entries);
        let locked =
            LockedState::from_lifecycle_parts(self.target_sequence, system, uid, locked_entries);
        let state = LifecycleState::new(manifest, locked).map_err(map_lifecycle_error)?;
        let upgraded = changed_ids.into_iter().collect();
        let changed = state != original;
        Ok(UpgradeResult {
            state,
            upgraded,
            skipped_pinned: self.selection.skipped_pinned,
            removed_upstream: removed_ids,
            changed,
        })
    }
}

fn same_resolved_package(left: &LockEntry, right: &LockEntry) -> bool {
    let left_realization = left.realization();
    let right_realization = right.realization();
    left.attribute() == right.attribute()
        && left_realization.store_path() == right_realization.store_path()
        && left_realization.deriver() == right_realization.deriver()
        && left_realization.outputs() == right_realization.outputs()
        && left_realization.outputs_to_install() == right_realization.outputs_to_install()
        && left_realization.system() == right_realization.system()
        && left_realization.nixpkgs_revision() == right_realization.nixpkgs_revision()
        && left_realization.nar_hash() == right_realization.nar_hash()
        && left_realization.closure_nar_size() == right_realization.closure_nar_size()
        && left_realization.pname() == right_realization.pname()
        && left_realization.version() == right_realization.version()
}

/// Successful upgrade editing result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeResult {
    state: LifecycleState,
    upgraded: Vec<SelectorId>,
    skipped_pinned: Vec<SelectorId>,
    removed_upstream: Vec<SelectorId>,
    changed: bool,
}

impl UpgradeResult {
    /// Returns the coherent next state.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }
    /// Returns selectors whose exact lock entries were replaced.
    #[must_use]
    pub fn upgraded(&self) -> &[SelectorId] {
        &self.upgraded
    }
    /// Returns pinned selectors skipped by policy.
    #[must_use]
    pub fn skipped_pinned(&self) -> &[SelectorId] {
        &self.skipped_pinned
    }
    /// Returns removed-upstream selectors preserved under skip policy.
    #[must_use]
    pub fn removed_upstream(&self) -> &[SelectorId] {
        &self.removed_upstream
    }
    /// Returns whether serialized manifest or lock bytes would change.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }
    /// Consumes the result and returns the next state.
    #[must_use]
    pub fn into_state(self) -> LifecycleState {
        self.state
    }
}

/// Stable upgrade planning or application failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeError {
    /// Named scope must contain at least one selector.
    EmptyScope,
    /// A named selector appeared more than once.
    DuplicateTarget,
    /// At least one named selector is not installed.
    NotInstalled,
    /// Validated state could not produce a safe resolution request.
    InvalidState,
    /// An outcome did not belong to this plan.
    UnexpectedOutcome,
    /// More than one outcome was supplied for a target.
    DuplicateOutcome,
    /// Not every eligible target received an outcome.
    IncompleteOutcomes,
    /// A removed-upstream result requires explicit skip policy.
    RemovedUpstream,
    /// A replacement realization targets another platform.
    SystemMismatch,
    /// A replacement names a different canonical attribute than was planned.
    AttributeMismatch,
    /// A replacement was not realized from the authenticated current revision.
    RevisionMismatch,
    /// The authenticated target channel sequence was older than active state.
    SequenceRollback,
}

impl fmt::Display for UpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "upgrade refused: {self:?}")
    }
}

impl std::error::Error for UpgradeError {}

/// Selects exactly the entries eligible for re-resolution without mutating state.
pub fn plan_upgrade(
    state: LifecycleState,
    scope: UpgradeScope,
    bump_pinned: bool,
    current_revision: NixpkgsRevision,
) -> Result<UpgradePlan, UpgradeError> {
    let current_sequence = state.manifest().channel_seq();
    select_upgrade(state, scope, bump_pinned)?.bind_channel(current_sequence, current_revision)
}

/// Selects upgrade targets without accepting channel identity from the caller.
pub fn select_upgrade(
    state: LifecycleState,
    scope: UpgradeScope,
    bump_pinned: bool,
) -> Result<UpgradeSelection, UpgradeError> {
    let requested = match scope {
        UpgradeScope::Named(ids) => {
            if ids.is_empty() {
                return Err(UpgradeError::EmptyScope);
            }
            let unique = ids.iter().cloned().collect::<BTreeSet<_>>();
            if unique.len() != ids.len() {
                return Err(UpgradeError::DuplicateTarget);
            }
            if !unique
                .iter()
                .all(|id| state.locked().entries().contains_key(id))
            {
                return Err(UpgradeError::NotInstalled);
            }
            ids
        }
        UpgradeScope::All => state
            .manifest()
            .entries()
            .iter()
            .map(|entry| entry.id().clone())
            .collect(),
    };
    let mut target_ids = Vec::new();
    let mut selectors = Vec::new();
    let mut skipped_pinned = Vec::new();
    for id in requested {
        let entry = state
            .manifest()
            .entries()
            .iter()
            .find(|entry| entry.id() == &id)
            .ok_or(UpgradeError::InvalidState)?;
        if entry.is_pinned() && !bump_pinned {
            skipped_pinned.push(id);
            continue;
        }
        selectors.push(
            state
                .selector_for_current_upgrade(&id)
                .map_err(map_lifecycle_error)?,
        );
        target_ids.push(id);
    }
    Ok(UpgradeSelection {
        state,
        target_ids,
        selectors,
        skipped_pinned,
        bump_pinned,
    })
}

const fn map_lifecycle_error(_: LifecycleError) -> UpgradeError {
    UpgradeError::InvalidState
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceRevision;
    use crate::lifecycle_test_support::{REV1, REV2, replacement, state, store};

    fn id(value: &str) -> SelectorId {
        SelectorId::new(value).unwrap()
    }

    #[test]
    fn selective_upgrade_preserves_every_untouched_revision_and_pin() {
        let original = state();
        let old_beta = original.locked().entries()[&id("sel_b")].clone();
        let old_charlie = original.locked().entries()[&id("sel_c")].clone();
        let plan = plan_upgrade(
            original,
            UpgradeScope::Named(vec![id("sel_a")]),
            false,
            NixpkgsRevision::new(REV2).unwrap(),
        )
        .unwrap();
        assert_eq!(plan.selectors().len(), 1);
        assert_eq!(
            plan.selectors()[0].source_revision(),
            &SourceRevision::CurrentChannel
        );
        let result = plan
            .apply(
                vec![UpgradeOutcome::resolved(
                    id("sel_a"),
                    replacement("alpha", '3', REV2, "2.0"),
                )],
                RemovedUpstreamPolicy::Refuse,
            )
            .unwrap();
        assert!(result.changed());
        assert_eq!(result.upgraded(), [id("sel_a")]);
        assert_eq!(result.state().locked().entries()[&id("sel_b")], old_beta);
        assert_eq!(result.state().locked().entries()[&id("sel_c")], old_charlie);
        assert_eq!(
            result.state().locked().entries()[&id("sel_a")]
                .realization()
                .nixpkgs_revision()
                .as_str(),
            REV2
        );
        assert_eq!(
            result.state().locked().entries()[&id("sel_b")]
                .realization()
                .nixpkgs_revision()
                .as_str(),
            REV1
        );
        assert!(result.state().manifest().entries()[2].is_pinned());
    }

    #[test]
    fn all_skips_pins_and_removed_upstream_policy_is_atomic() {
        let plan = plan_upgrade(
            state(),
            UpgradeScope::All,
            false,
            NixpkgsRevision::new(REV2).unwrap(),
        )
        .unwrap();
        assert_eq!(plan.skipped_pinned(), [id("sel_c")]);
        assert_eq!(
            plan.selectors()
                .iter()
                .map(|selector| selector.id().as_str())
                .collect::<Vec<_>>(),
            ["sel_a", "sel_b"]
        );
        let outcomes = vec![
            UpgradeOutcome::resolved(id("sel_a"), replacement("alpha", '3', REV2, "2.0")),
            UpgradeOutcome::removed_upstream(id("sel_b")),
        ];
        assert_eq!(
            plan.clone()
                .apply(outcomes.clone(), RemovedUpstreamPolicy::Refuse),
            Err(UpgradeError::RemovedUpstream)
        );
        let result = plan.apply(outcomes, RemovedUpstreamPolicy::Skip).unwrap();
        assert_eq!(result.removed_upstream(), [id("sel_b")]);
        assert_eq!(result.skipped_pinned(), [id("sel_c")]);
        assert_eq!(
            result.state().locked().entries()[&id("sel_b")]
                .realization()
                .store_path()
                .as_str(),
            store('1', "beta")
        );
    }

    #[test]
    fn bump_pinned_clears_pin_only_after_a_verified_replacement() {
        let plan = plan_upgrade(
            state(),
            UpgradeScope::Named(vec![id("sel_c")]),
            true,
            NixpkgsRevision::new(REV2).unwrap(),
        )
        .unwrap();
        assert!(plan.skipped_pinned().is_empty());
        let result = plan
            .apply(
                vec![UpgradeOutcome::resolved(
                    id("sel_c"),
                    replacement("charlie", '4', REV2, "2.0"),
                )],
                RemovedUpstreamPolicy::Refuse,
            )
            .unwrap();
        let entry = &result.state().manifest().entries()[2];
        assert!(!entry.is_pinned());
        assert_eq!(entry.pinned_to(), None);
        assert!(result.state().manifest().pins().is_empty());
    }

    #[test]
    fn exact_outcome_coverage_and_noop_detection_are_closed() {
        let original = state();
        let old = &original.locked().entries()[&id("sel_a")];
        let reobserved = LockEntry::new(
            old.attribute().clone(),
            old.realization().clone(),
            "2026-08-20T00:00:00Z".into(),
            "cache:refreshed".into(),
            vec!["official-1:refreshed".into()],
        )
        .unwrap();
        let plan = plan_upgrade(
            original.clone(),
            UpgradeScope::Named(vec![id("sel_a")]),
            false,
            NixpkgsRevision::new(REV1).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.clone().apply(vec![], RemovedUpstreamPolicy::Refuse),
            Err(UpgradeError::IncompleteOutcomes)
        );
        let unchanged = plan
            .apply(
                vec![UpgradeOutcome::resolved(id("sel_a"), reobserved)],
                RemovedUpstreamPolicy::Refuse,
            )
            .unwrap();
        assert!(!unchanged.changed());
        assert!(unchanged.upgraded().is_empty());
        assert_eq!(unchanged.state(), &original);
    }

    #[test]
    fn bump_pinned_is_a_change_when_the_package_is_current() {
        let original = state();
        let old = &original.locked().entries()[&id("sel_c")];
        let reobserved = LockEntry::new(
            old.attribute().clone(),
            old.realization().clone(),
            "2026-08-20T00:00:00Z".into(),
            "cache:refreshed".into(),
            Vec::new(),
        )
        .unwrap();
        let result = plan_upgrade(
            original,
            UpgradeScope::Named(vec![id("sel_c")]),
            true,
            NixpkgsRevision::new(REV1).unwrap(),
        )
        .unwrap()
        .apply(
            vec![UpgradeOutcome::resolved(id("sel_c"), reobserved)],
            RemovedUpstreamPolicy::Refuse,
        )
        .unwrap();
        assert!(result.changed());
        assert_eq!(result.upgraded(), [id("sel_c")]);
        assert!(!result.state().manifest().entries()[2].is_pinned());
    }

    #[test]
    fn authenticated_channel_binding_advances_state_and_refuses_rollback() {
        let selection =
            select_upgrade(state(), UpgradeScope::Named(vec![id("sel_a")]), false).unwrap();
        assert_eq!(
            selection
                .clone()
                .bind_channel(
                    ChannelSequence::from_u64(1).unwrap(),
                    NixpkgsRevision::new(REV2).unwrap(),
                )
                .unwrap_err(),
            UpgradeError::SequenceRollback
        );
        let result = selection
            .bind_channel(
                ChannelSequence::from_u64(3).unwrap(),
                NixpkgsRevision::new(REV2).unwrap(),
            )
            .unwrap()
            .apply(
                vec![UpgradeOutcome::resolved(
                    id("sel_a"),
                    replacement("alpha", '3', REV2, "2.0"),
                )],
                RemovedUpstreamPolicy::Refuse,
            )
            .unwrap();
        assert_eq!(result.state().manifest().channel_seq().get().get(), 3);
        assert_eq!(result.state().locked().channel_seq().get().get(), 3);
        assert_eq!(
            result.state().locked().entries()[&id("sel_b")]
                .realization()
                .nixpkgs_revision()
                .as_str(),
            REV1
        );
    }

    #[test]
    fn replacement_is_bound_to_planned_attribute_and_authenticated_revision() {
        let plan = plan_upgrade(
            state(),
            UpgradeScope::Named(vec![id("sel_a")]),
            false,
            NixpkgsRevision::new(REV2).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.clone().apply(
                vec![UpgradeOutcome::resolved(
                    id("sel_a"),
                    replacement("beta", '3', REV2, "2.0"),
                )],
                RemovedUpstreamPolicy::Refuse,
            ),
            Err(UpgradeError::AttributeMismatch)
        );
        assert_eq!(
            plan.apply(
                vec![UpgradeOutcome::resolved(
                    id("sel_a"),
                    replacement("alpha", '3', REV1, "2.0"),
                )],
                RemovedUpstreamPolicy::Refuse,
            ),
            Err(UpgradeError::RevisionMismatch)
        );
    }
}
