use std::fmt;
use std::fs;
use std::path::Path;

use pkg_core::state::{CollisionPolicy, canonical_digest};
use pkg_core::{RollbackPlan, StorePath};
use pkg_nix::GenerationId;
use pkg_store::StateLease;
use pkg_store::{ActivationInput, ActivationPlan, StateLayout, stage_activation};
use serde_json::{Value, json};

use crate::activation_metadata::{activation_inputs, collision_policy_name, collision_resolutions};
use crate::commit::{discard_staging, strictly_newer};
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
        stage_activation,
    )
}

pub fn prepare_rollback_with<E>(
    layout: StateLayout,
    lease: StateLease,
    rollback: &RollbackPlan,
    generation_id: &str,
    created_at: &str,
    operation_id: &str,
    stage: impl FnOnce(&Path, &[ActivationInput], CollisionPolicy) -> Result<ActivationPlan, E>,
) -> Result<PreparedGeneration, RollbackPrepareError> {
    let generation =
        GenerationId::new(generation_id).map_err(|_| RollbackPrepareError::InvalidGeneration)?;
    if !strictly_newer(generation.as_str(), rollback.active_generation()) {
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
    let inputs = activation_inputs(target.state());
    let activation_plan =
        stage(&staging, &inputs, activation.collision_policy()).map_err(|_| {
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
    activation.insert(
        "collisionPolicy".into(),
        json!(collision_policy_name(
            target.generation().activation().collision_policy()
        )),
    );
    activation.insert(
        "collisionResolutions".into(),
        Value::Array(
            collision_resolutions(plan)
                .ok_or(RollbackPrepareError::Commit(CommitError::InvalidCandidate))?,
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

#[cfg(test)]
mod tests;
