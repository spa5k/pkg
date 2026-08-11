//! Fresh-generation preparation for state-only lifecycle edits.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pkg_core::state::{body_digest, canonical_digest};
use pkg_core::{GenerationSnapshot, lifecycle::LifecycleState};
use pkg_nix::GenerationId;
use pkg_store::{StateLayout, StateLease, stage_activation};
use serde_json::{Value, json};

use crate::activation_metadata::{activation_inputs, collision_resolutions};
use crate::{CandidateGeneration, CommitError, PreparedGeneration};

/// Product provenance recorded for a state-only generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEditKind {
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
        }
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
    next: LifecycleState,
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
    let plan = stage_activation(
        &staging,
        &inputs,
        source.generation().activation().collision_policy(),
    )
    .inspect_err(|_| discard_staging(&staging))
    .map_err(|_| StateEditPrepareError::Stage)?;
    let candidate = build_candidate(
        source,
        next,
        generation.as_str(),
        metadata.created_at,
        metadata.operation_id,
        metadata.kind,
        &plan,
    )
    .inspect_err(|_| discard_staging(&staging))?;
    PreparedGeneration::prepare(layout, candidate, plan, lease)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(StateEditPrepareError::Commit)
}

fn build_candidate(
    source: &GenerationSnapshot,
    next: LifecycleState,
    generation_id: &str,
    created_at: &str,
    operation_id: &str,
    kind: StateEditKind,
    plan: &pkg_store::ActivationPlan,
) -> Result<CandidateGeneration, StateEditPrepareError> {
    let pinned = next
        .manifest()
        .entries()
        .iter()
        .map(|entry| (entry.id().as_str().to_owned(), entry.is_pinned()))
        .collect::<BTreeMap<_, _>>();
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
    object.insert("createdAt".into(), json!(created_at));
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
        json!({"opId": operation_id, "kind": kind.as_str(), "approval": {"build": "not_required"}}),
    );
    let outputs = object
        .get_mut("outputs")
        .and_then(Value::as_array_mut)
        .ok_or_else(invalid_candidate_unit)?;
    outputs.retain_mut(|output| {
        let Some(id) = output.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(is_pinned) = pinned.get(id) else {
            return false;
        };
        output
            .as_object_mut()
            .expect("generation output is an object")
            .insert("pinned".into(), json!(is_pinned));
        true
    });
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

fn invalid_candidate_unit() -> StateEditPrepareError {
    StateEditPrepareError::Commit(CommitError::InvalidCandidate)
}

fn strictly_newer(candidate: &str, active: &str) -> bool {
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
