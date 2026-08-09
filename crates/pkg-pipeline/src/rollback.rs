use std::fmt;
use std::fs;
use std::path::Path;

use pkg_core::state::{CollisionPolicy, canonical_digest};
use pkg_core::{RollbackPlan, StorePath};
use pkg_nix::GenerationId;
use pkg_store::StateLease;
use pkg_store::{ActivationError, ActivationInput, ActivationPlan, StateLayout, stage_activation};
use serde_json::{Value, json};

use crate::{CandidateGeneration, CommitError, PreparedGeneration};

/// Stable failures while turning a verified rollback target into a fresh generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackPrepareError {
    /// The new generation id was invalid or was not newer than the active id.
    InvalidGeneration,
    /// The active generation changed after rollback planning.
    CurrentChanged,
    /// A path reserved for the new generation already exists.
    GenerationExists,
    /// Rust could not materialize the fresh activation forest.
    Stage,
    /// The candidate generation or durable prepare transaction was refused.
    Commit(CommitError),
}

impl fmt::Display for RollbackPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "rollback preparation refused: {self:?}")
    }
}

impl std::error::Error for RollbackPrepareError {}

/// Re-materializes a rollback target and durably prepares a fresh generation.
///
/// The caller must activate and finish the returned transaction using the same
/// broker-backed maintenance path as install, remove, and upgrade, while
/// holding the per-user state-mutation lease introduced by the GC/lifecycle
/// layer.
pub fn prepare_rollback(
    layout: StateLayout,
    lease: StateLease,
    plan: &RollbackPlan,
    generation_id: &str,
    created_at: &str,
    operation_id: &str,
) -> Result<PreparedGeneration, RollbackPrepareError> {
    prepare_rollback_with(
        layout,
        lease,
        plan,
        generation_id,
        created_at,
        operation_id,
        |staging, outputs, policy| {
            let inputs = outputs
                .iter()
                .cloned()
                .map(ActivationInput::new)
                .collect::<Vec<_>>();
            stage_activation(staging, &inputs, policy)
        },
    )
}

fn prepare_rollback_with(
    layout: StateLayout,
    lease: StateLease,
    rollback: &RollbackPlan,
    generation_id: &str,
    created_at: &str,
    operation_id: &str,
    stage: impl FnOnce(&Path, &[StorePath], CollisionPolicy) -> Result<ActivationPlan, ActivationError>,
) -> Result<PreparedGeneration, RollbackPrepareError> {
    let generation =
        GenerationId::new(generation_id).map_err(|_| RollbackPrepareError::InvalidGeneration)?;
    if !is_strictly_newer(generation.as_str(), rollback.active_generation()) {
        return Err(RollbackPrepareError::InvalidGeneration);
    }
    layout
        .validate()
        .map_err(|_| RollbackPrepareError::CurrentChanged)?;
    let current = layout
        .current_generation()
        .map_err(|_| RollbackPrepareError::CurrentChanged)?;
    if current.as_ref().map(GenerationId::as_str) != Some(rollback.active_generation()) {
        return Err(RollbackPrepareError::CurrentChanged);
    }

    let root = layout.state_root();
    let staging = root
        .join("activations")
        .join(format!("{}.staging", generation.as_str()));
    let reserved = [
        staging.clone(),
        root.join("activations").join(generation.as_str()),
        root.join("generations")
            .join(format!("{}.json", generation.as_str())),
        root.join("generations")
            .join(format!("{}.manifest.json", generation.as_str())),
        root.join("generations")
            .join(format!("{}.lock.json", generation.as_str())),
    ];
    if reserved
        .iter()
        .any(|path| fs::symlink_metadata(path).is_ok())
    {
        return Err(RollbackPrepareError::GenerationExists);
    }

    let target = rollback.target();
    let activation = target.generation().activation();
    let activation_plan = stage(
        &staging,
        &target.selected_output_paths(),
        activation.collision_policy(),
    )
    .map_err(|_| {
        discard_staging(&staging);
        RollbackPrepareError::Stage
    })?;
    let candidate = build_candidate(
        rollback,
        generation.as_str(),
        created_at,
        operation_id,
        &activation_plan,
    )
    .inspect_err(|_| discard_staging(&staging))?;
    PreparedGeneration::prepare(layout, candidate, activation_plan, lease)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(RollbackPrepareError::Commit)
}

fn build_candidate(
    rollback: &RollbackPlan,
    generation_id: &str,
    created_at: &str,
    operation_id: &str,
    plan: &ActivationPlan,
) -> Result<CandidateGeneration, RollbackPrepareError> {
    let target = rollback.target();
    let manifest_bytes = target
        .state()
        .manifest()
        .to_json()
        .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    let lock_bytes = target
        .state()
        .locked()
        .to_json()
        .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    let mut generation: Value = serde_json::from_slice(
        &target
            .generation()
            .to_json()
            .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?,
    )
    .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    let object = generation
        .as_object_mut()
        .ok_or(RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    object.insert("id".into(), json!(generation_id));
    object.insert("parent".into(), json!(rollback.active_generation()));
    object.insert("createdAt".into(), json!(created_at));
    object.insert(
        "manifestHash".into(),
        json!(pkg_core::state::body_digest(&manifest_bytes).to_string()),
    );
    object.insert(
        "lockHash".into(),
        json!(pkg_core::state::body_digest(&lock_bytes).to_string()),
    );
    object.insert(
        "manifestSnapshot".into(),
        json!(format!("generations/{generation_id}.manifest.json")),
    );
    object.insert(
        "lockSnapshot".into(),
        json!(format!("generations/{generation_id}.lock.json")),
    );
    object.insert(
        "operation".into(),
        json!({
            "opId": operation_id,
            "kind": "rollback",
            "approval": { "build": "not_required" }
        }),
    );
    let activation = object
        .get_mut("activation")
        .and_then(Value::as_object_mut)
        .ok_or(RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    activation.insert(
        "treePath".into(),
        json!(format!("activations/{generation_id}")),
    );
    activation.insert("treeDigest".into(), json!(plan.tree_digest().to_string()));
    activation.insert("entryCount".into(), json!(plan.entry_count()));
    activation.insert(
        "outputRoots".into(),
        json!(
            plan.output_roots()
                .iter()
                .map(StorePath::as_str)
                .collect::<Vec<_>>()
        ),
    );
    object.remove("generationHash");
    let generation_hash = canonical_digest(&generation)
        .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    generation
        .as_object_mut()
        .expect("generation object was validated above")
        .insert("generationHash".into(), json!(generation_hash.to_string()));
    let generation_bytes = serde_json::to_vec(&generation)
        .map_err(|_| RollbackPrepareError::Commit(CommitError::InvalidCandidate))?;
    CandidateGeneration::new(manifest_bytes, lock_bytes, generation_bytes)
        .map_err(RollbackPrepareError::Commit)
}

fn is_strictly_newer(candidate: &str, active: &str) -> bool {
    let Some(candidate) = candidate.strip_prefix("gen-") else {
        return false;
    };
    let Some(active) = active.strip_prefix("gen-") else {
        return false;
    };
    let candidate = candidate.trim_start_matches('0');
    let active = active.trim_start_matches('0');
    let candidate = if candidate.is_empty() { "0" } else { candidate };
    let active = if active.is_empty() { "0" } else { active };
    candidate.len() > active.len() || (candidate.len() == active.len() && candidate > active)
}

fn discard_staging(staging: &Path) {
    let Ok(metadata) = fs::symlink_metadata(staging) else {
        return;
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        let _ = fs::remove_dir_all(staging);
    } else {
        let _ = fs::remove_file(staging);
    }
}

#[cfg(test)]
mod tests {
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
            |staging, outputs, policy| {
                assert_eq!(policy, CollisionPolicy::Abort);
                fs::create_dir(staging)?;
                symlink(format!("{STORE}/bin/demo"), staging.join("demo"))?;
                inspect_staged_activation(staging, outputs.to_vec())
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
            |_, _, _| panic!("staging must not run"),
        );
        assert_eq!(result.unwrap_err(), RollbackPrepareError::InvalidGeneration);
        assert!(!is_strictly_newer("gen-00002", "gen-0002"));
        assert!(is_strictly_newer("gen-0010", "gen-0009"));
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
}
