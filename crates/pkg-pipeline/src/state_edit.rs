//! Fresh-generation preparation for state-only lifecycle edits.

use std::fs;

use pkg_core::state::{CollisionPolicy, body_digest, canonical_digest};
use pkg_core::{GenerationSnapshot, lifecycle::LifecycleState};
use pkg_nix::GenerationId;
use pkg_store::{StateLayout, StateLease, stage_activation};
use serde_json::{Value, json};

use crate::activation_metadata::{activation_inputs, collision_resolutions};
use crate::commit::{discard_staging, strictly_newer};
use crate::{CandidateGeneration, CommitError, PreparedGeneration};

/// Product provenance recorded for a state-only generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEditKind {
    /// Advance accepted metadata without changing any exact realization.
    Update,
    /// Replace selected exact realizations with authenticated current-channel evidence.
    Upgrade,
    /// Remove one or more selectors.
    Remove,
    /// Pin one or more selectors to their current realization.
    Pin,
    /// Release one or more selector pins.
    Unpin,
}

impl StateEditKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Upgrade => "upgrade",
            Self::Remove => "remove",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
        }
    }
}

/// Caller-supplied identity and provenance for one fresh state-edit generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateEditMetadata<'a> {
    generation_id: &'a str,
    created_at: &'a str,
    operation_id: &'a str,
    kind: StateEditKind,
    collision_policy: Option<CollisionPolicy>,
    build_approval: &'a str,
}

impl<'a> StateEditMetadata<'a> {
    /// Groups the exact fresh-generation identity used by state-edit preparation.
    #[must_use]
    pub const fn new(
        generation_id: &'a str,
        created_at: &'a str,
        operation_id: &'a str,
        kind: StateEditKind,
    ) -> Self {
        Self {
            generation_id,
            created_at,
            operation_id,
            kind,
            collision_policy: None,
            build_approval: "not_required",
        }
    }

    /// Selects an explicit activation collision policy for an upgrade.
    #[must_use]
    pub const fn with_collision_policy(mut self, collision_policy: CollisionPolicy) -> Self {
        self.collision_policy = Some(collision_policy);
        self
    }

    /// Records the broker-owned local-build approval source for an upgrade.
    #[must_use]
    pub const fn with_build_approval(mut self, build_approval: &'a str) -> Self {
        self.build_approval = build_approval;
        self
    }
}

/// Closed preparation failures for state-only generation edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEditPrepareError {
    /// The fresh generation id is invalid or not newer than current.
    InvalidGeneration,
    /// Current state changed after the verified source snapshot was selected.
    CurrentChanged,
    /// The destination generation already has durable or staged state.
    GenerationExists,
    /// The activation forest could not be staged.
    Stage,
    /// The resulting immutable generation failed its closed invariants.
    Commit(CommitError),
}

/// Stages and prepares a fresh immutable generation for remove, pin, or unpin.
pub fn prepare_state_edit(
    layout: StateLayout,
    lease: StateLease,
    source: &GenerationSnapshot,
    next: &LifecycleState,
    metadata: StateEditMetadata<'_>,
) -> Result<PreparedGeneration, StateEditPrepareError> {
    let generation = GenerationId::new(metadata.generation_id)
        .map_err(|_| StateEditPrepareError::InvalidGeneration)?;
    if !strictly_newer(generation.as_str(), source.generation().id()) {
        return Err(StateEditPrepareError::InvalidGeneration);
    }
    layout
        .validate()
        .map_err(|_| StateEditPrepareError::CurrentChanged)?;
    if layout
        .current_generation()
        .map_err(|_| StateEditPrepareError::CurrentChanged)?
        .as_ref()
        .map(GenerationId::as_str)
        != Some(source.generation().id())
    {
        return Err(StateEditPrepareError::CurrentChanged);
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
        return Err(StateEditPrepareError::GenerationExists);
    }

    let inputs = activation_inputs(&next);
    let collision_policy = metadata
        .collision_policy
        .unwrap_or(source.generation().activation().collision_policy());
    let plan = stage_activation(&staging, &inputs, collision_policy)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(|_| StateEditPrepareError::Stage)?;
    let candidate = build_candidate(source, &next, &metadata, collision_policy, &plan)
        .inspect_err(|_| discard_staging(&staging))?;
    PreparedGeneration::prepare(layout, candidate, plan, lease)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(StateEditPrepareError::Commit)
}

pub fn build_candidate(
    source: &GenerationSnapshot,
    next: &LifecycleState,
    metadata: &StateEditMetadata<'_>,
    collision_policy: CollisionPolicy,
    plan: &pkg_store::ActivationPlan,
) -> Result<CandidateGeneration, StateEditPrepareError> {
    let generation_id = metadata.generation_id;
    let manifest_bytes = next.manifest().to_json().map_err(invalid_candidate)?;
    let lock_bytes = next.locked().to_json().map_err(invalid_candidate)?;
    let mut generation: Value =
        serde_json::from_slice(&source.generation().to_json().map_err(invalid_candidate)?)
            .map_err(invalid_candidate)?;
    let object = generation
        .as_object_mut()
        .ok_or_else(invalid_candidate_unit)?;
    object.insert("id".into(), json!(generation_id));
    object.insert("parent".into(), json!(source.generation().id()));
    object.insert("createdAt".into(), json!(metadata.created_at));
    object.insert(
        "channelSeq".into(),
        json!(next.manifest().channel_seq().get().get()),
    );
    object.insert(
        "manifestHash".into(),
        json!(body_digest(&manifest_bytes).to_string()),
    );
    object.insert(
        "lockHash".into(),
        json!(body_digest(&lock_bytes).to_string()),
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
        json!({"opId": metadata.operation_id, "kind": metadata.kind.as_str(), "approval": {"build": metadata.build_approval}}),
    );
    let outputs = next
        .manifest()
        .entries()
        .iter()
        .map(|entry| {
            let lock = &next.locked().entries()[entry.id()];
            let realization = lock.realization();
            json!({
                "id": entry.id().as_str(),
                "attribute": lock.attribute().as_str(),
                "nixpkgsRev": realization.nixpkgs_revision().as_str(),
                "storePath": realization.store_path().as_str(),
                "deriver": realization.deriver().as_str(),
                "outputsToInstall": realization.outputs_to_install().iter().map(pkg_core::OutputName::as_str).collect::<Vec<_>>(),
                "narHash": realization.nar_hash().as_str(),
                "closureNarSize": realization.closure_nar_size(),
                "provenance": lock.provenance(),
                "pinned": entry.is_pinned()
            })
        })
        .collect::<Vec<_>>();
    object.insert("outputs".into(), Value::Array(outputs));
    let activation = object
        .get_mut("activation")
        .and_then(Value::as_object_mut)
        .ok_or_else(invalid_candidate_unit)?;
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
                .map(pkg_core::StorePath::as_str)
                .collect::<Vec<_>>()
        ),
    );
    activation.insert(
        "collisionResolutions".into(),
        Value::Array(collision_resolutions(plan).ok_or_else(invalid_candidate_unit)?),
    );
    activation.insert(
        "collisionPolicy".into(),
        json!(crate::activation_metadata::collision_policy_name(
            collision_policy
        )),
    );
    object.remove("generationHash");
    let hash = canonical_digest(&generation).map_err(invalid_candidate)?;
    generation
        .as_object_mut()
        .expect("validated above")
        .insert("generationHash".into(), json!(hash.to_string()));
    let generation_bytes = serde_json::to_vec(&generation).map_err(invalid_candidate)?;
    CandidateGeneration::new(manifest_bytes, lock_bytes, generation_bytes)
        .map_err(StateEditPrepareError::Commit)
}

fn invalid_candidate<E>(_: E) -> StateEditPrepareError {
    StateEditPrepareError::Commit(CommitError::InvalidCandidate)
}

const fn invalid_candidate_unit() -> StateEditPrepareError {
    StateEditPrepareError::Commit(CommitError::InvalidCandidate)
}
