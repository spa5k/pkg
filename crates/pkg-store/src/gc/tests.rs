//! Tests for the `gc` module.

use super::*;
use pkg_core::state::{Generation, LockedState, Manifest, body_digest, canonical_digest};
use pkg_nix::{GcStatus, InProcessHelper, InProcessPeer};
use pkg_testkit::FakeNix;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use tempfile::{Builder, TempDir};

#[test]
fn retention_requires_both_count_and_age_expiry_and_never_selects_active() {
    let old = snapshot("gen-0001", None, "2026-06-01T00:00:00Z");
    let recent = snapshot("gen-0002", Some("gen-0001"), "2026-08-09T00:00:00Z");
    let active = snapshot("gen-0003", Some("gen-0002"), "2026-08-10T00:00:00Z");
    let archive = vec![old, active.clone(), recent];
    let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
    let plan = plan_gc(&active, &archive, GcPolicy::new(1, 30).unwrap(), now).unwrap();
    assert_eq!(
        plan.candidates()
            .iter()
            .map(PruneCandidate::generation_id)
            .collect::<Vec<_>>(),
        ["gen-0001"]
    );
    assert_eq!(plan.active_generation(), "gen-0003");
    assert_eq!(plan.estimated_reclaimable_bytes(), 0);

    let age_only = plan_gc(&active, &archive, GcPolicy::new(0, 30).unwrap(), now).unwrap();
    assert_eq!(age_only.candidates()[0].generation_id(), "gen-0001");
    assert!(
        !age_only
            .candidates()
            .iter()
            .any(|candidate| candidate.generation_id() == "gen-0002")
    );
}

#[test]
fn manual_prune_selects_only_a_retired_retained_generation() {
    let retired = snapshot("gen-0001", None, "2026-08-09T00:00:00Z");
    let active = snapshot("gen-0002", Some("gen-0001"), "2026-08-10T00:00:00Z");
    let archive = vec![retired.clone(), active.clone()];
    let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();

    let candidate = plan_generation_prune(&active, &archive, "gen-0001", now).unwrap();
    assert_eq!(candidate.snapshot(), &retired);
    assert_eq!(
        plan_generation_prune(&active, &archive, "gen-0002", now),
        Err(GcError::CurrentChanged)
    );
    assert_eq!(
        plan_generation_prune(&active, &archive, "gen-9999", now),
        Err(GcError::InvalidArchive)
    );
}

#[test]
fn root_removal_authority_requires_root_last_state_and_refuses_active() {
    let (_temp, layout) = layout_fixture();
    let retired = GenerationId::new("gen-0001").unwrap();
    let active = GenerationId::new("gen-0002").unwrap();
    write_generation_assets(layout.state_root(), retired.as_str());

    assert_eq!(
        authorize_generation_root_removal(&layout, &retired),
        Err(GcError::PruneNotAuthorized)
    );
    assert_eq!(
        authorize_generation_root_removal(&layout, &active),
        Err(GcError::PruneNotAuthorized)
    );

    delete_user_generation(layout.state_root(), layout.owner_uid(), retired.as_str()).unwrap();
    assert_eq!(authorize_generation_root_removal(&layout, &retired), Ok(()));
}

#[test]
fn planning_rejects_duplicate_missing_active_and_future_timestamps() {
    let active = snapshot("gen-0002", None, "2026-08-10T00:00:00Z");
    let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
    assert_eq!(
        plan_gc(
            &active,
            &[active.clone(), active.clone()],
            GcPolicy::new(0, 0).unwrap(),
            now
        ),
        Err(GcError::InvalidArchive)
    );
    assert_eq!(
        plan_gc(&active, &[], GcPolicy::new(0, 0).unwrap(), now),
        Err(GcError::InvalidArchive)
    );
    let future = snapshot("gen-0003", None, "2026-08-11T00:00:00Z");
    assert_eq!(
        plan_gc(
            &active,
            &[active.clone(), future],
            GcPolicy::new(0, 0).unwrap(),
            now
        ),
        Err(GcError::InvalidTimestamp)
    );
}

#[test]
fn execute_prunes_metadata_root_last_then_calls_fake_nix_once() {
    let (temp, layout) = layout_fixture();
    let old = snapshot("gen-0001", None, "2026-06-01T00:00:00Z");
    let active = snapshot("gen-0002", Some("gen-0001"), "2026-08-10T00:00:00Z");
    write_generation_assets(layout.state_root(), "gen-0001");
    let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
    let plan = plan_gc(
        &active,
        &[old, active.clone()],
        GcPolicy::new(0, 0).unwrap(),
        now,
    )
    .unwrap();
    let lease = StateLease::try_exclusive(
        &layout,
        &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
    )
    .unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(layout.owner_uid());
    let report = GcReport::new(GcStatus::Collected, Vec::new(), 4096).unwrap();
    let fake = FakeNix::new();
    fake.expect_gc(Ok(report));
    let result = execute_gc(&layout, &lease, &plan, &maintenance, &fake, "op_gc").unwrap();
    assert_eq!(result.pruned_generations(), ["gen-0001"]);
    assert_eq!(result.nix_report().freed_bytes(), 4096);
    assert!(
        !layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert!(!layout.state_root().join("activations/gen-0001").exists());
    assert_eq!(
        layout.current_generation().unwrap().unwrap().as_str(),
        "gen-0002"
    );
    let rows = StateJournal::open(&layout).unwrap().rows(&lease).unwrap();
    assert_eq!(
        rows.iter()
            .filter_map(|row| row.payload().fields().get("status"))
            .filter_map(Value::as_str)
            .collect::<Vec<_>>(),
        ["intended", "pruned"]
    );
    fake.assert_exhausted().unwrap();
    drop(temp);
}

#[test]
fn active_target_and_shared_lease_are_refused_before_deletion() {
    let (_temp, layout) = layout_fixture();
    write_generation_assets(layout.state_root(), "gen-0002");
    let active = snapshot("gen-0002", None, "2026-08-10T00:00:00Z");
    let candidate = PruneCandidate { snapshot: active };
    drop(
        StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_seed", "nonce0", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap(),
    );
    let shared = StateLease::try_shared(&layout).unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(layout.owner_uid());
    assert_eq!(
        prune_generation(&layout, &shared, &candidate, &maintenance, "op_gc"),
        Err(GcError::LeaseRequired)
    );
    drop(shared);
    let exclusive = StateLease::try_exclusive(
        &layout,
        &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
    )
    .unwrap();
    assert_eq!(
        prune_generation(&layout, &exclusive, &candidate, &maintenance, "op_gc"),
        Err(GcError::CurrentChanged)
    );
    assert!(
        layout
            .state_root()
            .join("generations/gen-0002.json")
            .exists()
    );
}

#[test]
fn recovery_finishes_an_intended_prune_idempotently() {
    let (_temp, layout) = layout_fixture();
    write_generation_assets(layout.state_root(), "gen-0001");
    let lease = StateLease::try_exclusive(
        &layout,
        &crate::LeaseIdentity::new("op_recover", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
    )
    .unwrap();
    let journal = StateJournal::open(&layout).unwrap();
    journal
        .append(
            &lease,
            "op_gc",
            "prune",
            "intended",
            [
                ("kind".into(), json!("gc")),
                ("generationId".into(), json!("gen-0001")),
                ("outputRoots".into(), json!([])),
            ],
        )
        .unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(layout.owner_uid());
    assert_eq!(
        recover_prunes(&layout, &lease, &maintenance).unwrap(),
        ["gen-0001"]
    );
    assert!(
        recover_prunes(&layout, &lease, &maintenance)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn root_removal_failure_leaves_only_a_recoverable_rooted_intent() {
    let (_temp, layout) = layout_fixture();
    write_generation_assets(layout.state_root(), "gen-0001");
    let lease = StateLease::try_exclusive(
        &layout,
        &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
    )
    .unwrap();
    let candidate = PruneCandidate {
        snapshot: snapshot("gen-0001", None, "2026-06-01T00:00:00Z"),
    };
    let helper = InProcessHelper::new(991).unwrap();
    let authenticated = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap();
    let wrong_caller = authenticated.for_caller(layout.owner_uid().saturating_add(1));
    assert_eq!(
        prune_generation(&layout, &lease, &candidate, &wrong_caller, "op_gc"),
        Err(GcError::RootRemoval)
    );
    assert!(
        !layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert_eq!(
        layout.current_generation().unwrap().unwrap().as_str(),
        "gen-0002"
    );

    let maintenance = authenticated.for_caller(layout.owner_uid());
    assert_eq!(
        recover_prunes(&layout, &lease, &maintenance).unwrap(),
        ["gen-0001"]
    );
}

#[test]
fn symlinked_generation_parent_is_refused_before_cross_tree_deletion() {
    let (temp, layout) = layout_fixture();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("gen-0001.json"), b"keep").unwrap();
    fs::remove_dir(layout.state_root().join("generations")).unwrap();
    symlink(&outside, layout.state_root().join("generations")).unwrap();
    let lease = StateLease::try_exclusive(
        &layout,
        &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
    )
    .unwrap();
    let candidate = PruneCandidate {
        snapshot: snapshot("gen-0001", None, "2026-06-01T00:00:00Z"),
    };
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(layout.owner_uid());
    assert_eq!(
        prune_generation(&layout, &lease, &candidate, &maintenance, "op_gc"),
        Err(GcError::UnsafeState)
    );
    assert_eq!(fs::read(outside.join("gen-0001.json")).unwrap(), b"keep");
}

fn layout_fixture() -> (TempDir, StateLayout) {
    let temp = Builder::new().prefix("pkg-gc-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    for relative in [
        "",
        "run",
        "journal",
        "generations",
        "activations",
        "activations/gen-0002",
    ] {
        let path = state.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    symlink("activations/gen-0002", state.join("current")).unwrap();
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    (temp, layout)
}

fn write_generation_assets(root: &Path, id: &str) {
    let activation = root.join("activations").join(id);
    fs::create_dir_all(&activation).unwrap();
    fs::set_permissions(&activation, fs::Permissions::from_mode(0o700)).unwrap();
    for suffix in [
        ".json",
        ".json.sha256",
        ".manifest.json",
        ".manifest.json.sha256",
        ".lock.json",
        ".lock.json.sha256",
    ] {
        fs::write(
            root.join("generations").join(format!("{id}{suffix}")),
            b"fixture",
        )
        .unwrap();
    }
}

fn snapshot(id: &str, parent: Option<&str>, created_at: &str) -> GenerationSnapshot {
    let uid = fs::symlink_metadata(".").unwrap().uid();
    let manifest_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "uid": uid,
        "entries": [],
        "pins": []
    }))
    .unwrap();
    let lock_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "system": "x86_64-linux",
        "uid": uid,
        "entries": {}
    }))
    .unwrap();
    let lifecycle = pkg_core::lifecycle::LifecycleState::new(
        Manifest::from_json(&manifest_bytes).unwrap(),
        LockedState::from_json(&lock_bytes).unwrap(),
    )
    .unwrap();
    let mut generation = json!({
        "schemaVersion": 1,
        "uid": uid,
        "id": id,
        "parent": parent,
        "createdAt": created_at,
        "channelSeq": 1,
        "manifestHash": body_digest(&manifest_bytes).to_string(),
        "lockHash": body_digest(&lock_bytes).to_string(),
        "manifestSnapshot": format!("generations/{id}.manifest.json"),
        "lockSnapshot": format!("generations/{id}.lock.json"),
        "activation": {
            "kind": "pkg-symlink-forest",
            "treePath": format!("activations/{id}"),
            "treeDigest": body_digest(b"").to_string(),
            "entryCount": 0,
            "collisionPolicy": "abort",
            "outputRoots": [],
            "collisionResolutions": []
        },
        "outputs": [],
        "operation": {
            "opId": format!("op_{id}"),
            "kind": "install",
            "approval": { "build": "not_required" }
        }
    });
    let hash = canonical_digest(&generation).unwrap().to_string();
    generation
        .as_object_mut()
        .unwrap()
        .insert("generationHash".into(), json!(hash));
    let generation = Generation::from_json(&serde_json::to_vec(&generation).unwrap()).unwrap();
    GenerationSnapshot::new(generation, lifecycle).unwrap()
}
