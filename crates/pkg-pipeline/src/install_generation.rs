//! Crash-safe preparation of a broker-evidenced install generation.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pkg_core::state::{CollisionPolicy, body_digest, canonical_digest};
use pkg_core::{GenerationSnapshot, StorePath, lifecycle::LifecycleState};
use pkg_nix::InstallEvidence;
use pkg_store::{LeaseMode, StateLayout, StateLease, stage_activation};
use serde_json::json;

use crate::activation_metadata::{activation_inputs, collision_policy_name, collision_resolutions};
use crate::{
    CandidateGeneration, CommitError, PreparedGeneration, assemble_install_evidence_state,
};

/// Immutable metadata assigned to one install generation.
#[derive(Debug, Clone, Copy)]
pub struct InstallGenerationMetadata<'a> {
    generation_id: &'a str,
    created_at: &'a str,
    operation_id: &'a str,
    build_approval: &'a str,
}

impl<'a> InstallGenerationMetadata<'a> {
    /// Creates the durable identity and approval summary for an install.
    #[must_use]
    pub const fn new(
        generation_id: &'a str,
        created_at: &'a str,
        operation_id: &'a str,
        build_approval: &'a str,
    ) -> Self {
        Self {
            generation_id,
            created_at,
            operation_id,
            build_approval,
        }
    }
}

/// Stable refusal at the evidence-to-prepared-generation boundary.
#[derive(Debug)]
pub enum InstallGenerationError {
    /// The reserved generation id is invalid or not monotonic.
    InvalidGeneration,
    /// The current pointer does not match the supplied source snapshot.
    CurrentChanged,
    /// Destination files already exist.
    GenerationExists,
    /// Broker evidence could not form coherent lifecycle state.
    InvalidEvidence,
    /// The activation forest could not be staged.
    Stage,
    /// The immutable candidate or journal transition failed.
    Commit(CommitError),
}

impl std::fmt::Display for InstallGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "install generation preparation refused: {self:?}"
        )
    }
}

impl std::error::Error for InstallGenerationError {}

/// Converts retained broker evidence into a durable prepared generation before
/// any generation root is published.
pub fn prepare_install_generation(
    layout: StateLayout,
    lease: StateLease,
    current: Option<&GenerationSnapshot>,
    evidence: &InstallEvidence,
    uid: u32,
    collision_policy: CollisionPolicy,
    metadata: InstallGenerationMetadata<'_>,
) -> Result<PreparedGeneration, InstallGenerationError> {
    if !lease.authorizes(&layout, LeaseMode::Exclusive) || layout.owner_uid() != uid {
        return Err(InstallGenerationError::CurrentChanged);
    }
    layout
        .validate()
        .map_err(|_| InstallGenerationError::CurrentChanged)?;
    let observed = layout
        .current_generation()
        .map_err(|_| InstallGenerationError::CurrentChanged)?;
    if observed.as_ref().map(pkg_nix::GenerationId::as_str)
        != current.map(|snapshot| snapshot.generation().id())
    {
        return Err(InstallGenerationError::CurrentChanged);
    }
    pkg_nix::GenerationId::new(metadata.generation_id)
        .map_err(|_| InstallGenerationError::InvalidGeneration)?;
    match current {
        None if metadata.generation_id != "gen-0001" => {
            return Err(InstallGenerationError::InvalidGeneration);
        }
        Some(snapshot) if !strictly_newer(metadata.generation_id, snapshot.generation().id()) => {
            return Err(InstallGenerationError::InvalidGeneration);
        }
        _ => {}
    }

    let next = assemble_install_evidence_state(
        current.map(|snapshot| snapshot.state().clone()),
        evidence,
        uid,
        metadata.created_at,
    )
    .map_err(|_| InstallGenerationError::InvalidEvidence)?
    .into_state();
    let root = layout.state_root();
    let staging = root
        .join("activations")
        .join(format!("{}.staging", metadata.generation_id));
    let reserved = [
        staging.clone(),
        root.join("activations").join(metadata.generation_id),
        root.join("generations")
            .join(format!("{}.json", metadata.generation_id)),
        root.join("generations")
            .join(format!("{}.manifest.json", metadata.generation_id)),
        root.join("generations")
            .join(format!("{}.lock.json", metadata.generation_id)),
    ];
    if reserved
        .iter()
        .any(|path| fs::symlink_metadata(path).is_ok())
    {
        return Err(InstallGenerationError::GenerationExists);
    }

    let inputs = activation_inputs(&next);
    let plan = stage_activation(&staging, &inputs, collision_policy)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(|_| InstallGenerationError::Stage)?;
    let candidate = build_candidate(current, next, collision_policy, metadata, &plan)
        .inspect_err(|_| discard_staging(&staging))?;
    PreparedGeneration::prepare(layout, candidate, plan, lease)
        .inspect_err(|_| discard_staging(&staging))
        .map_err(InstallGenerationError::Commit)
}

fn build_candidate(
    current: Option<&GenerationSnapshot>,
    next: LifecycleState,
    collision_policy: CollisionPolicy,
    metadata: InstallGenerationMetadata<'_>,
    plan: &pkg_store::ActivationPlan,
) -> Result<CandidateGeneration, InstallGenerationError> {
    let manifest_bytes = next.manifest().to_json().map_err(|_| invalid_candidate())?;
    let lock_bytes = next.locked().to_json().map_err(|_| invalid_candidate())?;
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
                "outputsToInstall": realization.outputs_to_install().iter().map(|name| name.as_str()).collect::<Vec<_>>(),
                "narHash": realization.nar_hash().as_str(),
                "closureNarSize": realization.closure_nar_size(),
                "provenance": lock.provenance(),
                "pinned": entry.is_pinned()
            })
        })
        .collect::<Vec<_>>();
    let collision_resolutions = collision_resolutions(plan).ok_or_else(invalid_candidate)?;
    let collision_policy = collision_policy_name(collision_policy);
    let mut generation = json!({
        "schemaVersion": 1,
        "uid": next.manifest().uid(),
        "id": metadata.generation_id,
        "parent": current.map(|snapshot| snapshot.generation().id()),
        "createdAt": metadata.created_at,
        "channelSeq": next.manifest().channel_seq().get(),
        "manifestHash": body_digest(&manifest_bytes).to_string(),
        "lockHash": body_digest(&lock_bytes).to_string(),
        "manifestSnapshot": format!("generations/{}.manifest.json", metadata.generation_id),
        "lockSnapshot": format!("generations/{}.lock.json", metadata.generation_id),
        "activation": {
            "kind": "pkg-symlink-forest",
            "treePath": format!("activations/{}", metadata.generation_id),
            "treeDigest": plan.tree_digest().to_string(),
            "entryCount": plan.entry_count(),
            "collisionPolicy": collision_policy,
            "outputRoots": plan.output_roots().iter().map(StorePath::as_str).collect::<Vec<_>>(),
            "collisionResolutions": collision_resolutions
        },
        "outputs": outputs,
        "operation": {
            "opId": metadata.operation_id,
            "kind": "install",
            "approval": { "build": metadata.build_approval }
        }
    });
    let hash = canonical_digest(&generation).map_err(|_| invalid_candidate())?;
    generation
        .as_object_mut()
        .ok_or_else(invalid_candidate)?
        .insert("generationHash".into(), json!(hash.to_string()));
    let generation_bytes = serde_json::to_vec(&generation).map_err(|_| invalid_candidate())?;
    CandidateGeneration::new(manifest_bytes, lock_bytes, generation_bytes)
        .map_err(InstallGenerationError::Commit)
}

fn invalid_candidate() -> InstallGenerationError {
    InstallGenerationError::Commit(CommitError::InvalidCandidate)
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

fn discard_staging(staging: &std::path::Path) {
    let Ok(metadata) = fs::symlink_metadata(staging) else {
        return;
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        let _ = fs::set_permissions(staging, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(staging);
    } else {
        let _ = fs::remove_file(staging);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use pkg_nix::InstallEvidence;
    use pkg_store::inspect_staged_activation;
    use serde_json::json;
    use tempfile::Builder;

    use super::{InstallGenerationMetadata, build_candidate, strictly_newer};
    use crate::assemble_install_evidence_state;

    const STORE: &str = "/nix/store/00000000000000000000000000000000-demo";
    const DRV: &str = "/nix/store/11111111111111111111111111111111-demo.drv";
    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn broker_evidence_builds_a_closed_initial_candidate() {
        let evidence = InstallEvidence::from_json_bytes(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "descriptorHash": format!("sha256-{}", "0".repeat(64)),
                "channelSequence": 1,
                "policyVersion": 1,
                "revision": REV,
                "sourceNarHash": NAR,
                "system": "x86_64-linux",
                "targets": [{
                    "selectorId": "sel_demo",
                    "selector": "demo",
                    "attribute": "demo",
                    "versionPreference": { "kind": "any" },
                    "requestedOutputs": null,
                    "sourceRevision": "channel:current",
                    "rootDerivation": DRV,
                    "rootOutputs": [{ "name": "out", "storePath": STORE }],
                    "outputsToInstall": ["out"],
                    "packageName": "demo",
                    "packageVersion": "1.0",
                    "acquired": [{
                        "outputName": "out",
                        "storePath": STORE,
                        "narHash": NAR,
                        "signatures": ["cache.nixos.org-1:AAAA"],
                        "references": [],
                        "deriver": DRV,
                        "narSize": 20,
                        "closureSize": 42,
                        "provenance": "cacheSigned"
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let next = assemble_install_evidence_state(None, &evidence, 501, "2026-08-11T00:00:00Z")
            .unwrap()
            .into_state();
        let temp = Builder::new()
            .prefix("pkg-install-candidate-")
            .tempdir_in(".")
            .unwrap();
        let staging = temp.path().join("gen-0001.staging");
        std::fs::create_dir(&staging).unwrap();
        symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
        let plan =
            inspect_staged_activation(&staging, vec![pkg_core::StorePath::new(STORE).unwrap()])
                .unwrap();
        let candidate = build_candidate(
            None,
            next,
            pkg_core::state::CollisionPolicy::Abort,
            InstallGenerationMetadata::new(
                "gen-0001",
                "2026-08-11T00:00:00Z",
                "op_install",
                "not_required",
            ),
            &plan,
        )
        .unwrap();
        assert_eq!(candidate.generation().id(), "gen-0001");
        assert_eq!(candidate.generation().parent(), None);
        assert_eq!(candidate.generation().outputs().len(), 1);
        assert_eq!(candidate.generation().activation().output_roots().len(), 1);
    }

    #[test]
    fn generation_order_is_numeric_and_never_wraps_by_text_order() {
        assert!(strictly_newer("gen-0010", "gen-0009"));
        assert!(!strictly_newer("gen-0009", "gen-0010"));
        assert!(!strictly_newer("generation-11", "gen-0010"));
    }
}
