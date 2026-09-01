//! Tests for the `rollback` module.

use super::*;
use pkg_core::state::{Generation, LockedState, Manifest, body_digest};
use pkg_core::{GenerationSnapshot, RollbackTarget, plan_rollback};
use pkg_nix::{InProcessHelper, InProcessPeer};
use pkg_store::{LeaseIdentity, inspect_staged_activation};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use tempfile::Builder;

const STORE: &str = "/nix/store/00000000000000000000000000000000-demo";
const DRV: &str = "/nix/store/11111111111111111111111111111111-demo.drv";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

#[test]
fn rollback_prepares_and_activates_a_fresh_generation() {
    let temp = Builder::new()
        .prefix("pkg-rollback-")
        .tempdir_in(".")
        .unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state_root = temp.path().join("state");
    for relative in ["", "generations", "journal", "run", "activations/gen-0002"] {
        let path = state_root.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    symlink("activations/gen-0002", state_root.join("current")).unwrap();
    let target = snapshot(uid, "gen-0001", None);
    let active = snapshot(uid, "gen-0002", Some("gen-0001"));
    let rollback = plan_rollback(
        &active,
        std::slice::from_ref(&target),
        RollbackTarget::Parent,
        |_| true,
    )
    .unwrap();
    let layout = StateLayout::open(temp.path(), &state_root, uid).unwrap();
    let lease = StateLease::try_exclusive(
        &layout,
        &LeaseIdentity::new("op_rollback", "nonce1", "2026-08-09T00:00:03Z").unwrap(),
    )
    .unwrap();
    let prepared = prepare_rollback_with(
        layout,
        lease,
        &rollback,
        "gen-0003",
        "2026-08-09T00:00:03Z",
        "op_rollback",
        |staging, inputs, policy| {
            assert_eq!(policy, CollisionPolicy::Abort);
            fs::create_dir(staging)?;
            symlink(format!("{STORE}/bin/demo"), staging.join("demo"))?;
            inspect_staged_activation(
                staging,
                inputs
                    .iter()
                    .map(|input| input.store_path().clone())
                    .collect(),
            )
        },
    )
    .unwrap();
    let record = fs::read(state_root.join("generations/gen-0003.json")).unwrap();
    let generation = Generation::from_json(&record).unwrap();
    assert_eq!(generation.parent(), Some("gen-0002"));
    assert_eq!(generation.operation().kind(), "rollback");
    assert_eq!(generation.activation().tree_path(), "activations/gen-0003");
    assert_eq!(
        StateLayout::open(temp.path(), &state_root, uid)
            .unwrap()
            .current_generation()
            .unwrap()
            .unwrap()
            .as_str(),
        "gen-0002"
    );

    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(uid);
    prepared
        .activate(&maintenance, "rollbacknonce")
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        StateLayout::open(temp.path(), &state_root, uid)
            .unwrap()
            .current_generation()
            .unwrap()
            .unwrap()
            .as_str(),
        "gen-0003"
    );
    assert_eq!(
        fs::read_link(state_root.join("activations/gen-0003/demo")).unwrap(),
        std::path::PathBuf::from(format!("{STORE}/bin/demo"))
    );
}

#[test]
fn rollback_refuses_stale_or_non_monotonic_generation_before_staging() {
    let temp = Builder::new()
        .prefix("pkg-rollback-refusal-")
        .tempdir_in(".")
        .unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state_root = temp.path().join("state");
    for relative in ["", "generations", "journal", "run", "activations/gen-0002"] {
        let path = state_root.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    symlink("activations/gen-0002", state_root.join("current")).unwrap();
    let target = snapshot(uid, "gen-0001", None);
    let active = snapshot(uid, "gen-0002", Some("gen-0001"));
    let rollback = plan_rollback(
        &active,
        std::slice::from_ref(&target),
        RollbackTarget::Parent,
        |_| true,
    )
    .unwrap();
    let layout = StateLayout::open(temp.path(), &state_root, uid).unwrap();
    let lease = StateLease::try_exclusive(
        &layout,
        &LeaseIdentity::new("op_rollback", "nonce1", "2026-08-09T00:00:03Z").unwrap(),
    )
    .unwrap();
    let result = prepare_rollback_with(
        layout,
        lease,
        &rollback,
        "gen-0002",
        "2026-08-09T00:00:03Z",
        "op_rollback",
        |_, _, _| -> Result<ActivationPlan, ()> { panic!("staging must not run") },
    );
    assert_eq!(result.unwrap_err(), RollbackPrepareError::InvalidGeneration);
    assert!(!strictly_newer("gen-00002", "gen-0002"));
    assert!(strictly_newer("gen-0010", "gen-0009"));
}

fn snapshot(uid: u32, id: &str, parent: Option<&str>) -> GenerationSnapshot {
    let manifest_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "uid": uid,
        "entries": [{
            "id": "sel_demo",
            "selector": "demo",
            "attribute": "demo",
            "versionPref": { "kind": "any" },
            "outputs": null,
            "sourceRev": "channel:current",
            "pinned": false,
            "pinnedTo": null,
            "addedAt": "2026-08-09T00:00:00Z",
            "origin": "user:install"
        }],
        "pins": []
    }))
    .unwrap();
    let lock_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "system": "x86_64-linux",
        "uid": uid,
        "entries": {
            "sel_demo": {
                "attribute": "demo",
                "nixpkgsRev": REV,
                "realized": {
                    "storePath": STORE,
                    "deriver": DRV,
                    "outputs": { "out": STORE },
                    "outputsToInstall": ["out"],
                    "system": "x86_64-linux",
                    "narHash": NAR,
                    "closureNarSize": 42,
                    "pname": "demo",
                    "version": "1.0"
                },
                "lockedAt": "2026-08-09T00:00:01Z",
                "provenance": "cache:official",
                "sigsObserved": ["official-1:fixture"]
            }
        }
    }))
    .unwrap();
    let manifest = Manifest::from_json(&manifest_bytes).unwrap();
    let locked = LockedState::from_json(&lock_bytes).unwrap();
    let lifecycle = pkg_core::lifecycle::LifecycleState::new(manifest, locked).unwrap();
    let mut generation = json!({
        "schemaVersion": 1,
        "uid": uid,
        "id": id,
        "parent": parent,
        "createdAt": "2026-08-09T00:00:02Z",
        "channelSeq": 1,
        "manifestHash": body_digest(&manifest_bytes).to_string(),
        "lockHash": body_digest(&lock_bytes).to_string(),
        "manifestSnapshot": format!("generations/{id}.manifest.json"),
        "lockSnapshot": format!("generations/{id}.lock.json"),
        "activation": {
            "kind": "pkg-symlink-forest",
            "treePath": format!("activations/{id}"),
            "treeDigest": body_digest(b"fixture").to_string(),
            "entryCount": 1,
            "collisionPolicy": "abort",
            "outputRoots": [STORE],
            "collisionResolutions": []
        },
        "outputs": [{
            "id": "sel_demo",
            "attribute": "demo",
            "nixpkgsRev": REV,
            "storePath": STORE,
            "deriver": DRV,
            "outputsToInstall": ["out"],
            "narHash": NAR,
            "closureNarSize": 42,
            "provenance": "cache:official",
            "pinned": false
        }],
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
