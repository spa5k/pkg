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
            .source_commit()
            .as_str(),
        REV2
    );
    assert_eq!(
        result.state().locked().entries()[&id("sel_b")]
            .realization()
            .source_commit()
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
            .source_commit()
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

fn flake_lock(revision: &str) -> crate::LockedFlake {
    crate::LockedFlake::new(
        crate::PublicFlakeRef::new("github:example/tools/main#alpha").unwrap(),
        NixpkgsRevision::new(revision).unwrap(),
        crate::NarHash::new("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap(),
        r#"{"version":7,"root":"root","nodes":{"root":{}}}"#,
    )
    .unwrap()
}

fn flake_entry(hash: char, revision: &str) -> LockEntry {
    let entry = replacement("alpha", hash, revision, "1.0");
    LockEntry::new(
        entry.attribute().clone(),
        entry.realization().clone().with_flake(flake_lock(revision)),
        "2026-09-23T00:00:00Z".into(),
        "build:local".into(),
        vec![],
    )
    .unwrap()
}

#[test]
fn flake_lifecycle_keeps_source_intent_exact_locks_and_rollback() {
    use crate::state::{LockedState, Manifest};
    use crate::{
        ChannelSequence, InstallPackage, OutputSelection, PackageSelector, SelectorInput,
        SourceRevision, System, VersionPreference,
    };
    let selector = PackageSelector::new(
        id("sel_flake"),
        SelectorInput::new("github:example/tools/main#alpha").unwrap(),
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::PublicFlake(flake_lock(REV1)),
    )
    .with_attribute(crate::AttributePath::new("alpha").unwrap())
    .unwrap();
    let initial = crate::install_packages(
        None,
        ChannelSequence::from_u64(2).unwrap(),
        System::X8664Linux,
        1001,
        vec![
            InstallPackage::new(
                selector,
                flake_entry('3', REV1),
                "2026-09-23T00:00:00Z",
                "user:install",
            )
            .unwrap(),
        ],
    )
    .unwrap()
    .into_state();
    let initial = LifecycleState::new(
        Manifest::from_json(&initial.manifest().to_json().unwrap()).unwrap(),
        LockedState::from_json(&initial.locked().to_json().unwrap()).unwrap(),
    )
    .unwrap();
    let pinned = crate::edit_pins(initial.clone(), &[id("sel_flake")], crate::PinAction::Pin)
        .unwrap()
        .into_state();
    assert!(
        select_upgrade(pinned, UpgradeScope::All, false)
            .unwrap()
            .selectors()
            .is_empty()
    );
    let plan = plan_upgrade(
        initial.clone(),
        UpgradeScope::All,
        false,
        NixpkgsRevision::new(REV1).unwrap(),
    )
    .unwrap();
    assert_eq!(
        plan.selectors()[0].selector().as_str(),
        "github:example/tools/main#alpha"
    );
    let upgraded = plan
        .apply(
            vec![UpgradeOutcome::resolved(
                id("sel_flake"),
                flake_entry('4', REV2),
            )],
            RemovedUpstreamPolicy::Refuse,
        )
        .unwrap();
    assert!(upgraded.changed());
    let upgraded = upgraded.into_state();
    assert_eq!(
        upgraded.locked().entries()[&id("sel_flake")]
            .realization()
            .flake(),
        Some(&flake_lock(REV2))
    );
    assert_eq!(
        upgraded.manifest().entries()[0].source_revision(),
        &SourceRevision::PublicFlake(flake_lock(REV2))
    );
    let old = crate::lifecycle_test_support::snapshot("gen-0001", None, initial, "install");
    let current = crate::lifecycle_test_support::snapshot(
        "gen-0002",
        Some("gen-0001"),
        upgraded.clone(),
        "upgrade",
    );
    let rollback =
        crate::plan_rollback(&current, &[old], crate::RollbackTarget::Parent, |_| true).unwrap();
    assert_eq!(
        rollback.target().state().locked().entries()[&id("sel_flake")]
            .realization()
            .flake(),
        Some(&flake_lock(REV1))
    );
    let removed = crate::remove::remove_selectors(upgraded, &[id("sel_flake")])
        .unwrap()
        .into_state();
    assert!(removed.manifest().entries().is_empty());
    let bad = crate::lifecycle_test_support::state();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&bad.manifest().to_json().unwrap()).unwrap();
    manifest["entries"][0]["selector"] = serde_json::json!("github:example/tools#alpha");
    assert!(
        LifecycleState::new(
            Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
            bad.locked().clone()
        )
        .is_err()
    );
}
