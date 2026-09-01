//! Tests for the `upgrade` module.

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
    let selection = select_upgrade(state(), UpgradeScope::Named(vec![id("sel_a")]), false).unwrap();
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
