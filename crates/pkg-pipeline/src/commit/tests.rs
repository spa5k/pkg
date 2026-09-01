//! Tests for the `commit` module.

use super::*;
use pkg_nix::{InProcessHelper, InProcessPeer};
use pkg_store::{LeaseIdentity, inspect_staged_activation};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use tempfile::{Builder, TempDir};

const STORE: &str = "/nix/store/00000000000000000000000000000000-demo";
const DRV: &str = "/nix/store/11111111111111111111111111111111-demo.drv";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

struct Fixture {
    _temp: TempDir,
    layout: StateLayout,
    candidate: CandidateGeneration,
    plan: ActivationPlan,
    generation_id: GenerationId,
    maintenance: pkg_nix::CallerMaintenance,
}

struct RootLastMaintenance<'a> {
    layout: &'a StateLayout,
    inner: &'a dyn MaintenanceAdapter,
    fail_removal: bool,
    fail_after_removal: bool,
}

impl MaintenanceAdapter for RootLastMaintenance<'_> {
    fn publish_root_set(
        &self,
        root_set: &pkg_nix::RootSet,
    ) -> Result<RootSetReport, pkg_nix::MaintenanceError> {
        self.inner.publish_root_set(root_set)
    }

    fn attest_root_set(
        &self,
        request: &pkg_nix::RootSetAttestationRequest,
    ) -> Result<RootSetReport, pkg_nix::MaintenanceError> {
        self.inner.attest_root_set(request)
    }

    fn remove_root_set(
        &self,
        request: &RemoveRootSetRequest,
    ) -> Result<(), pkg_nix::MaintenanceError> {
        assert_eq!(request.owner_uid(), self.layout.owner_uid());
        assert!(
            authorize_generation_root_removal(self.layout, request.generation()).is_ok(),
            "root removal must follow user-state deletion"
        );
        if self.fail_removal {
            return Err(pkg_nix::MaintenanceError::backend_failure());
        }
        self.inner.remove_root_set(request)?;
        if self.fail_after_removal {
            return Err(pkg_nix::MaintenanceError::backend_failure());
        }
        Ok(())
    }

    fn repair_store_paths(
        &self,
        request: &pkg_nix::RepairStorePathsRequest,
    ) -> Result<pkg_nix::RepairStorePathsReport, pkg_nix::MaintenanceError> {
        self.inner.repair_store_paths(request)
    }
}

fn fixture() -> Fixture {
    fixture_with_outputs(true)
}

fn empty_fixture() -> Fixture {
    fixture_with_outputs(false)
}

fn fixture_with_outputs(has_output: bool) -> Fixture {
    let temp = Builder::new()
        .prefix("pkg-pipeline-")
        .tempdir_in(".")
        .unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    for relative in ["", "generations", "journal", "activations", "run"] {
        let path = state.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let staging = state.join("activations/gen-0001.staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    let output_roots = if has_output {
        symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
        vec![pkg_core::StorePath::new(STORE).unwrap()]
    } else {
        Vec::new()
    };
    let plan = inspect_staged_activation(&staging, output_roots).unwrap();

    let manifest_entries = if has_output {
        vec![json!({
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
        })]
    } else {
        Vec::new()
    };
    let manifest_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "uid": uid,
        "entries": manifest_entries,
        "pins": []
    }))
    .unwrap();
    let lock_entries = if has_output {
        json!({
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
        })
    } else {
        json!({})
    };
    let lock_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "system": "x86_64-linux",
        "uid": uid,
        "entries": lock_entries
    }))
    .unwrap();
    let generation_outputs = if has_output {
        vec![json!({
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
        })]
    } else {
        Vec::new()
    };
    let mut generation = json!({
        "schemaVersion": 1,
        "uid": uid,
        "id": "gen-0001",
        "parent": null,
        "createdAt": "2026-08-09T00:00:00Z",
        "channelSeq": 1,
        "manifestHash": body_digest(&manifest_bytes).to_string(),
        "lockHash": body_digest(&lock_bytes).to_string(),
        "manifestSnapshot": "generations/gen-0001.manifest.json",
        "lockSnapshot": "generations/gen-0001.lock.json",
        "activation": {
            "kind": "pkg-symlink-forest",
            "treePath": "activations/gen-0001",
            "treeDigest": plan.tree_digest().to_string(),
            "entryCount": plan.entry_count(),
            "collisionPolicy": "abort",
            "outputRoots": plan.output_roots().iter().map(pkg_core::StorePath::as_str).collect::<Vec<_>>(),
            "collisionResolutions": []
        },
        "outputs": generation_outputs,
        "operation": {
            "opId": "op_fixture",
            "kind": "install",
            "approval": { "build": "not_required" }
        }
    });
    let generation_hash = canonical_digest(&generation).unwrap().to_string();
    generation
        .as_object_mut()
        .unwrap()
        .insert("generationHash".into(), json!(generation_hash));
    let generation_bytes = serde_json::to_vec(&generation).unwrap();
    let candidate = CandidateGeneration::new(manifest_bytes, lock_bytes, generation_bytes).unwrap();
    let generation_id = GenerationId::new("gen-0001").unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(uid);
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    Fixture {
        _temp: temp,
        layout,
        candidate,
        plan,
        generation_id,
        maintenance,
    }
}

fn mutation_lease(layout: &StateLayout) -> StateLease {
    StateLease::try_exclusive(
        layout,
        &LeaseIdentity::new("op_fixture", "nonce1", "2026-08-09T00:00:00Z").unwrap(),
    )
    .unwrap()
}

#[test]
fn prepared_fault_discards_record_snapshots_and_staging() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::DiscardedUnactivated
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("activations/gen-0001.staging")
            .exists()
    );
}

#[test]
fn prepared_and_aborted_without_intent_are_not_generic_prunes() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();

    let maintenance = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: false,
        fail_after_removal: false,
    };
    assert!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );

    append_phase(
        &fixture.layout,
        &prepared.lease,
        "op_fixture",
        "commit",
        "aborted",
        [
            ("generationId", json!("gen-0001")),
            ("operationKind", json!("install")),
        ],
    )
    .unwrap();
    assert!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        Some(fixture.generation_id.clone())
    );
    assert_eq!(
        pending_install_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );

    assert_eq!(
        recover_generation(
            &fixture.layout,
            &prepared.lease,
            &fixture.generation_id,
            &maintenance,
        )
        .unwrap(),
        RecoveryResult::DiscardedUnactivated
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );
}

#[test]
fn failed_root_last_discard_converges_through_generic_recovery() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
    let failing = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: true,
        fail_after_removal: false,
    };
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &prepared.lease,
            &fixture.generation_id,
            &failing,
        ),
        Err(CommitError::ActivationFailed)
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        Some(fixture.generation_id.clone())
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );

    let maintenance = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: false,
        fail_after_removal: false,
    };
    assert_eq!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
        vec!["gen-0001".to_owned()]
    );
    assert!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );
}

#[test]
fn cross_operation_prune_is_terminal_for_aborted_install() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
    append_phase(
        &fixture.layout,
        &prepared.lease,
        "op_fixture",
        "commit",
        "aborted",
        [
            ("generationId", json!("gen-0001")),
            ("operationKind", json!("install")),
        ],
    )
    .unwrap();
    append_phase(
        &fixture.layout,
        &prepared.lease,
        "op_other_gc",
        "prune",
        "intended",
        [
            ("generationId", json!("gen-0001")),
            ("outputRoots", json!([STORE])),
        ],
    )
    .unwrap();
    let maintenance = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: false,
        fail_after_removal: false,
    };
    assert!(
        fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert_eq!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
        vec!["gen-0001".to_owned()]
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );
    assert!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn helper_removed_before_terminal_row_retries_idempotently() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
    let interrupted = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: false,
        fail_after_removal: true,
    };
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &prepared.lease,
            &fixture.generation_id,
            &interrupted,
        ),
        Err(CommitError::ActivationFailed)
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        Some(fixture.generation_id.clone())
    );
    let maintenance = RootLastMaintenance {
        layout: &fixture.layout,
        inner: &fixture.maintenance,
        fail_removal: false,
        fail_after_removal: false,
    };

    assert_eq!(
        pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
        vec!["gen-0001".to_owned()]
    );
    assert_eq!(
        pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
        None
    );
}

#[test]
fn generation_order_is_numeric_and_discard_staging_removes_debris() {
    assert!(strictly_newer("gen-0010", "gen-0009"));
    assert!(!strictly_newer("gen-0009", "gen-0010"));
    assert!(!strictly_newer("gen-00002", "gen-0002"));
    assert!(!strictly_newer("generation-11", "gen-0010"));
    assert!(!strictly_newer("gen-0010", "generation-11"));
    let temp = Builder::new()
        .prefix("pkg-discard-staging-")
        .tempdir_in(".")
        .unwrap();
    let staging = temp.path().join("gen-0009.staging");
    fs::create_dir(&staging).unwrap();
    symlink(STORE, staging.join("demo")).unwrap();
    discard_staging(&staging);
    assert!(!staging.exists());
    let file = temp.path().join("gen-0010.staging");
    fs::write(&file, b"staging debris").unwrap();
    discard_staging(&file);
    assert!(!file.exists());
    discard_staging(&temp.path().join("missing.staging"));
}

#[test]
fn prepare_requires_and_holds_exclusive_state_lease() {
    let fixture = fixture();
    drop(mutation_lease(&fixture.layout));
    let shared = StateLease::try_shared(&fixture.layout).unwrap();
    assert!(matches!(
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            shared
        ),
        Err(CommitError::LeaseRequired)
    ));
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
}

#[test]
fn rooted_fault_removes_roots_and_leaves_current_unchanged() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
    append_phase(
        &fixture.layout,
        &prepared.lease,
        "op_fixture",
        "commit",
        "rooted",
        [],
    )
    .unwrap();
    drop(prepared);
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::DiscardedUnactivated
    );
    assert_eq!(fixture.layout.current_generation().unwrap(), None);
}

#[test]
fn activated_fault_restores_views_and_commits_forward() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    prepared.activate(&fixture.maintenance, "n1").unwrap();
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::FinishedActivated
    );
    assert!(fixture.layout.state_root().join("manifest.json").is_file());
    assert!(
        journal_has_status(
            &fixture.layout,
            &recovery_lease,
            "op_fixture",
            "commit",
            "committed"
        )
        .unwrap()
    );
}

#[test]
fn committed_recovery_is_idempotent() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    prepared
        .activate(&fixture.maintenance, "n1")
        .unwrap()
        .finish()
        .unwrap();
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::AlreadyCommitted
    );
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn committed_generation_loads_as_active_verified_history() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap()
    .activate(&fixture.maintenance, "read1")
    .unwrap()
    .finish()
    .unwrap();

    let lease = StateLease::try_shared(&fixture.layout).unwrap();
    let active = load_active_snapshot(&fixture.layout, &lease)
        .unwrap()
        .unwrap();
    assert_eq!(active.generation().id(), "gen-0001");
    assert_eq!(active.state().manifest().entries().len(), 1);
    let history = load_retained_history(&fixture.layout, &lease).unwrap();
    assert_eq!(history.snapshots().len(), 1);
    assert!(history.summaries()[0].is_active());
}

#[test]
fn broker_transition_receipt_finishes_prepared_generation_without_republication() {
    let fixture = fixture();
    let roots = prepare_root_set(
        fixture.layout.owner_uid(),
        fixture.generation_id.clone(),
        [RootCandidate::from_output_root(
            pkg_core::StorePath::new(STORE).unwrap(),
        )],
    )
    .unwrap();
    let report = RootSetTransitionReport::new(
        publish_root_set(&roots, &fixture.maintenance).unwrap(),
        roots
            .request()
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect(),
        roots.request().mapping_digest(),
    )
    .unwrap();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    let intent = prepared
        .root_transition_intent(GenerationId::new("gen-0000").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(intent.destination_generation(), &fixture.generation_id);
    assert_eq!(intent.retained_names().len(), 1);
    prepared
        .activate_transitioned(Some(&report), "transitioned1")
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn broker_publication_receipt_finishes_prepared_generation_without_republication() {
    let fixture = fixture();
    let roots = prepare_root_set(
        fixture.layout.owner_uid(),
        fixture.generation_id.clone(),
        [RootCandidate::from_output_root(
            pkg_core::StorePath::new(STORE).unwrap(),
        )],
    )
    .unwrap();
    let report = publish_root_set(&roots, &fixture.maintenance).unwrap();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    let intent = prepared.root_intent().unwrap().unwrap();
    assert_eq!(intent.generation(), &fixture.generation_id);
    assert_eq!(intent.entries().len(), 1);
    let extended = prepared
        .root_intent_from_source(
            GenerationId::new("gen-0000").unwrap(),
            &[pkg_core::StorePath::new(STORE).unwrap()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(extended.source_generation().unwrap().as_str(), "gen-0000");
    assert_eq!(extended.added_names().len(), 1);
    prepared
        .activate_published(Some(&report), "published1")
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn broker_attestation_receipt_recovers_prepared_generation_without_republication() {
    let fixture = fixture();
    let roots = prepare_root_set(
        fixture.layout.owner_uid(),
        fixture.generation_id.clone(),
        [RootCandidate::from_output_root(
            pkg_core::StorePath::new(STORE).unwrap(),
        )],
    )
    .unwrap();
    publish_root_set(&roots, &fixture.maintenance).unwrap();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    let report = fixture
        .maintenance
        .attest_root_set(&pkg_nix::RootSetAttestationRequest::new(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
        ))
        .unwrap();
    prepared
        .activate_published(Some(&report), "attested1")
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn journaled_install_is_discovered_and_resumed_after_restart() {
    let fixture = fixture();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
    drop(prepared);

    let resume_lease = mutation_lease(&fixture.layout);
    let pending = pending_install_generation(&fixture.layout, &resume_lease)
        .unwrap()
        .unwrap();
    assert_eq!(pending, fixture.generation_id);
    let resumed = resume_prepared_install(fixture.layout.clone(), resume_lease, &pending).unwrap();
    let attested = fixture
        .maintenance
        .attest_root_set(&pkg_nix::RootSetAttestationRequest::new(
            fixture.layout.owner_uid(),
            pending,
        ))
        .unwrap();
    resumed
        .activate_published(Some(&attested), "resumeinstall1")
        .unwrap()
        .finish()
        .unwrap();

    let finished_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        pending_install_generation(&fixture.layout, &finished_lease).unwrap(),
        None
    );
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn published_activation_refuses_a_same_size_wrong_root_mapping() {
    let fixture = fixture();
    let wrong_roots = prepare_root_set(
        fixture.layout.owner_uid(),
        fixture.generation_id.clone(),
        [RootCandidate::from_output_root(
            pkg_core::StorePath::new("/nix/store/22222222222222222222222222222222-other").unwrap(),
        )],
    )
    .unwrap();
    let report = publish_root_set(&wrong_roots, &fixture.maintenance).unwrap();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    assert!(matches!(
        prepared.activate_published(Some(&report), "mismatch1"),
        Err(CommitError::ActivationFailed)
    ));
    assert_eq!(fixture.layout.current_generation().unwrap(), None);
}

#[test]
fn empty_prepared_generation_needs_neither_transition_nor_receipt() {
    let fixture = empty_fixture();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    assert!(
        prepared
            .root_transition_intent(GenerationId::new("gen-0000").unwrap())
            .unwrap()
            .is_none()
    );
    prepared
        .activate_transitioned(None, "empty1")
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fixture.layout.current_generation().unwrap(),
        Some(fixture.generation_id)
    );
}

#[test]
fn transitioned_activation_refuses_a_mismatched_broker_receipt() {
    let fixture = fixture();
    let roots = prepare_root_set(
        fixture.layout.owner_uid(),
        fixture.generation_id.clone(),
        [RootCandidate::from_output_root(
            pkg_core::StorePath::new(STORE).unwrap(),
        )],
    )
    .unwrap();
    let published = publish_root_set(&roots, &fixture.maintenance).unwrap();
    let mapping_digest = published.mapping_digest();
    let report = RootSetTransitionReport::new(
        published,
        vec![pkg_nix::RootName::new("wrong-output").unwrap()],
        mapping_digest,
    )
    .unwrap();
    let prepared = PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap();
    assert_eq!(
        prepared
            .activate_transitioned(Some(&report), "wrong1")
            .unwrap_err(),
        CommitError::ActivationFailed
    );
    assert_eq!(fixture.layout.current_generation().unwrap(), None);
}

#[test]
fn retained_history_refuses_unknown_or_oversized_files() {
    let fixture = empty_fixture();
    drop(mutation_lease(&fixture.layout));
    fs::write(
        fixture.layout.state_root().join("generations/unmanaged"),
        b"foreign",
    )
    .unwrap();
    let lease = StateLease::try_shared(&fixture.layout).unwrap();
    assert_eq!(
        load_retained_history(&fixture.layout, &lease).unwrap_err(),
        CommitError::StateIo
    );
    drop(lease);

    fs::remove_file(fixture.layout.state_root().join("generations/unmanaged")).unwrap();
    let orphan = fixture
        .layout
        .state_root()
        .join("generations/gen-9998.lock.json.sha256");
    fs::write(&orphan, b"sha256:orphan\n").unwrap();
    let lease = StateLease::try_shared(&fixture.layout).unwrap();
    assert_eq!(
        load_retained_history(&fixture.layout, &lease).unwrap_err(),
        CommitError::InvalidCandidate
    );
    drop(lease);
    fs::remove_file(orphan).unwrap();

    let oversized = fixture
        .layout
        .state_root()
        .join("generations/gen-9999.json");
    let file = fs::File::create(&oversized).unwrap();
    file.set_len(MAX_STATE_FILE_BYTES + 1).unwrap();
    fs::write(
        fixture
            .layout
            .state_root()
            .join("generations/gen-9999.json.sha256"),
        b"sha256:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();
    let lease = StateLease::try_shared(&fixture.layout).unwrap();
    assert_eq!(
        load_retained_history(&fixture.layout, &lease).unwrap_err(),
        CommitError::StateIo
    );
}

#[test]
fn empty_generation_commits_and_recovers_without_publishing_roots() {
    let fixture = empty_fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap()
    .activate(&fixture.maintenance, "empty1")
    .unwrap()
    .finish()
    .unwrap();
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::AlreadyCommitted
    );
}

#[test]
fn candidate_hash_or_snapshot_binding_tamper_fails_closed() {
    let fixture = fixture();
    let mut generation = fixture.candidate.generation_bytes.clone();
    let position = generation.iter().position(|byte| *byte == b'2').unwrap();
    generation[position] = b'3';
    assert!(matches!(
        CandidateGeneration::new(
            fixture.candidate.manifest_bytes,
            fixture.candidate.lock_bytes,
            generation
        ),
        Err(CommitError::InvalidCandidate)
    ));
}

#[test]
fn candidate_refuses_activation_roots_not_selected_by_lock() {
    let fixture = fixture();
    let mut generation: Value =
        serde_json::from_slice(&fixture.candidate.generation_bytes).unwrap();
    generation["activation"]["outputRoots"] = json!([]);
    generation.as_object_mut().unwrap().remove("generationHash");
    let generation_hash = canonical_digest(&generation).unwrap().to_string();
    generation
        .as_object_mut()
        .unwrap()
        .insert("generationHash".into(), json!(generation_hash));
    assert!(matches!(
        CandidateGeneration::new(
            fixture.candidate.manifest_bytes,
            fixture.candidate.lock_bytes,
            serde_json::to_vec(&generation).unwrap()
        ),
        Err(CommitError::InvalidCandidate)
    ));
}

#[test]
fn candidate_refuses_lock_from_another_channel_sequence() {
    let fixture = fixture();
    let mismatched_lock = format!(
        r#"{{"schemaVersion":1,"channelSeq":2,"system":"x86_64-linux","uid":{},"entries":{{}}}}"#,
        fixture.candidate.generation().uid()
    )
    .into_bytes();
    assert!(matches!(
        CandidateGeneration::new(
            fixture.candidate.manifest_bytes,
            mismatched_lock,
            fixture.candidate.generation_bytes
        ),
        Err(CommitError::InvalidCandidate)
    ));
}

#[test]
fn preprepared_orphans_and_current_temp_are_cleaned_without_publishing() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap();
    fs::remove_file(
        fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json"),
    )
    .unwrap();
    fs::remove_file(
        fixture
            .layout
            .state_root()
            .join("generations/gen-0001.json.sha256"),
    )
    .unwrap();
    symlink(
        "activations/gen-0001",
        fixture.layout.state_root().join("current.tmp.crash"),
    )
    .unwrap();
    let recovery_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_generation(
            &fixture.layout,
            &recovery_lease,
            &fixture.generation_id,
            &fixture.maintenance
        )
        .unwrap(),
        RecoveryResult::DiscardedUnactivated
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("current.tmp.crash")
            .exists()
    );
    assert!(
        !fixture
            .layout
            .state_root()
            .join("generations/gen-0001.manifest.json")
            .exists()
    );
}

#[test]
fn interrupted_state_edit_resumes_before_and_after_the_current_switch() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap()
    .activate(&fixture.maintenance, "source1")
    .unwrap()
    .finish()
    .unwrap();

    let edit_lease = mutation_lease(&fixture.layout);
    let source = load_active_snapshot(&fixture.layout, &edit_lease)
        .unwrap()
        .unwrap();
    let next = pkg_core::remove::remove_selectors(
        source.state().clone(),
        &[pkg_core::SelectorId::new("sel_demo").unwrap()],
    )
    .unwrap()
    .into_state();
    let prepared = crate::prepare_state_edit(
        fixture.layout.clone(),
        edit_lease,
        &source,
        &next,
        crate::StateEditMetadata::new(
            "gen-0002",
            "2026-08-11T00:00:00Z",
            "op_remove",
            crate::StateEditKind::Remove,
        ),
    )
    .unwrap();
    drop(prepared);

    let resume_lease = mutation_lease(&fixture.layout);
    let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_str(), "gen-0002");
    let resumed =
        resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
    let activated = resumed.activate_transitioned(None, "resume1").unwrap();
    drop(activated);

    let finish_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
        RecoveryResult::FinishedActivated
    );
    assert_eq!(
        pending_state_edit_generation(&fixture.layout, &finish_lease).unwrap(),
        None
    );
    assert!(
        load_active_snapshot(&fixture.layout, &finish_lease)
            .unwrap()
            .unwrap()
            .state()
            .manifest()
            .entries()
            .is_empty()
    );
}

#[test]
fn interrupted_update_resumes_before_and_after_the_current_switch() {
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap()
    .activate(&fixture.maintenance, "updsource1")
    .unwrap()
    .finish()
    .unwrap();

    let edit_lease = mutation_lease(&fixture.layout);
    let source = load_active_snapshot(&fixture.layout, &edit_lease)
        .unwrap()
        .unwrap();
    let next = pkg_core::advance_channel(
        source.state().clone(),
        pkg_core::ChannelSequence::from_u64(2).unwrap(),
    )
    .unwrap();
    let staging = fixture
        .layout
        .state_root()
        .join("activations/gen-0002.staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
    let plan = inspect_staged_activation(&staging, vec![pkg_core::StorePath::new(STORE).unwrap()])
        .unwrap();
    let metadata = crate::StateEditMetadata::new(
        "gen-0002",
        "2026-08-11T00:00:00Z",
        "op_update",
        crate::StateEditKind::Update,
    );
    let candidate = crate::state_edit::build_candidate(
        &source,
        &next,
        &metadata,
        pkg_core::state::CollisionPolicy::Abort,
        &plan,
    )
    .unwrap();
    let prepared =
        PreparedGeneration::prepare(fixture.layout.clone(), candidate, plan, edit_lease).unwrap();
    drop(prepared);

    let resume_lease = mutation_lease(&fixture.layout);
    let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_str(), "gen-0002");
    let resumed =
        resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
    let roots = resumed.roots.as_ref().unwrap();
    let report = RootSetTransitionReport::new(
        publish_root_set(roots, &fixture.maintenance).unwrap(),
        roots
            .request()
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect(),
        roots.request().mapping_digest(),
    )
    .unwrap();
    let activated = resumed
        .activate_transitioned(Some(&report), "updresume1")
        .unwrap();
    drop(activated);

    let finish_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
        RecoveryResult::FinishedActivated
    );
    assert_eq!(
        pending_state_edit_generation(&fixture.layout, &finish_lease).unwrap(),
        None
    );
    let recovered = load_active_snapshot(&fixture.layout, &finish_lease)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.generation().operation().kind(), "update");
    assert_eq!(recovered.state().manifest().channel_seq().get().get(), 2);
    assert_eq!(recovered.state().manifest().entries().len(), 1);
}

#[test]
fn upgrade_generation_rebinds_channel_outputs_collision_and_approval() {
    const NEXT_STORE: &str = "/nix/store/22222222222222222222222222222222-demo";
    const NEXT_DRV: &str = "/nix/store/33333333333333333333333333333333-demo.drv";
    const NEXT_REV: &str = "89abcdef0123456789abcdef0123456789abcdef";
    let fixture = fixture();
    let lease = mutation_lease(&fixture.layout);
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        lease,
    )
    .unwrap()
    .activate(&fixture.maintenance, "upgradesource1")
    .unwrap()
    .finish()
    .unwrap();

    let lease = mutation_lease(&fixture.layout);
    let source = load_active_snapshot(&fixture.layout, &lease)
        .unwrap()
        .unwrap();
    drop(lease);
    let mut manifest: Value =
        serde_json::from_slice(&source.state().manifest().to_json().unwrap()).unwrap();
    manifest["channelSeq"] = json!(2);
    let mut lock: Value =
        serde_json::from_slice(&source.state().locked().to_json().unwrap()).unwrap();
    lock["channelSeq"] = json!(2);
    lock["entries"]["sel_demo"]["nixpkgsRev"] = json!(NEXT_REV);
    let realized = &mut lock["entries"]["sel_demo"]["realized"];
    realized["storePath"] = json!(NEXT_STORE);
    realized["deriver"] = json!(NEXT_DRV);
    realized["outputs"]["out"] = json!(NEXT_STORE);
    realized["version"] = json!("2.0");
    let next = pkg_core::lifecycle::LifecycleState::new(
        pkg_core::state::Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        pkg_core::state::LockedState::from_json(&serde_json::to_vec(&lock).unwrap()).unwrap(),
    )
    .unwrap();
    let staging = fixture
        .layout
        .state_root()
        .join("upgrade-candidate.staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(format!("{NEXT_STORE}/bin/demo"), staging.join("demo")).unwrap();
    let plan = inspect_staged_activation(
        &staging,
        vec![pkg_core::StorePath::new(NEXT_STORE).unwrap()],
    )
    .unwrap();
    let metadata = crate::StateEditMetadata::new(
        "gen-0002",
        "2026-08-12T00:00:00Z",
        "op_upgrade",
        crate::StateEditKind::Upgrade,
    )
    .with_collision_policy(pkg_core::state::CollisionPolicy::KeepLast)
    .with_build_approval("yes");
    let candidate = crate::state_edit::build_candidate(
        &source,
        &next,
        &metadata,
        pkg_core::state::CollisionPolicy::KeepLast,
        &plan,
    )
    .unwrap();
    let generation: Value =
        serde_json::from_slice(&candidate.generation().to_json().unwrap()).unwrap();
    assert_eq!(generation["channelSeq"], 2);
    assert_eq!(generation["outputs"][0]["storePath"], NEXT_STORE);
    assert_eq!(generation["outputs"][0]["nixpkgsRev"], NEXT_REV);
    assert_eq!(generation["activation"]["collisionPolicy"], "keep-last");
    assert_eq!(generation["operation"]["kind"], "upgrade");
    assert_eq!(generation["operation"]["approval"]["build"], "yes");
}

#[test]
fn interrupted_rollback_resumes_from_the_retained_target() {
    let fixture = fixture();
    PreparedGeneration::prepare(
        fixture.layout.clone(),
        fixture.candidate,
        fixture.plan,
        mutation_lease(&fixture.layout),
    )
    .unwrap()
    .activate(&fixture.maintenance, "rbsrc1")
    .unwrap()
    .finish()
    .unwrap();

    let edit_lease = mutation_lease(&fixture.layout);
    let source = load_active_snapshot(&fixture.layout, &edit_lease)
        .unwrap()
        .unwrap();
    let empty = pkg_core::remove::remove_selectors(
        source.state().clone(),
        &[pkg_core::SelectorId::new("sel_demo").unwrap()],
    )
    .unwrap()
    .into_state();
    crate::prepare_state_edit(
        fixture.layout.clone(),
        edit_lease,
        &source,
        &empty,
        crate::StateEditMetadata::new(
            "gen-0002",
            "2026-08-11T00:00:00Z",
            "op_remove_before_rollback",
            crate::StateEditKind::Remove,
        ),
    )
    .unwrap()
    .activate_transitioned(None, "rbempty1")
    .unwrap()
    .finish()
    .unwrap();

    let rollback_lease = mutation_lease(&fixture.layout);
    let active = load_active_snapshot(&fixture.layout, &rollback_lease)
        .unwrap()
        .unwrap();
    let history = load_retained_history(&fixture.layout, &rollback_lease).unwrap();
    let retained = history
        .snapshots()
        .iter()
        .filter(|snapshot| snapshot.generation().id() != active.generation().id())
        .cloned()
        .collect::<Vec<_>>();
    let rollback = pkg_core::plan_rollback(
        &active,
        &retained,
        pkg_core::RollbackTarget::Named("gen-0001".to_owned()),
        |_| true,
    )
    .unwrap();
    crate::rollback::prepare_rollback_with(
        fixture.layout.clone(),
        rollback_lease,
        &rollback,
        "gen-0003",
        "2026-08-11T00:00:01Z",
        "op_rollback",
        |staging, inputs, _| {
            fs::create_dir(staging).unwrap();
            fs::set_permissions(staging, fs::Permissions::from_mode(0o700)).unwrap();
            symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
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

    let resume_lease = mutation_lease(&fixture.layout);
    let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_str(), "gen-0003");
    assert_eq!(
        pending_state_transition_source(&fixture.layout, &resume_lease, &pending)
            .unwrap()
            .as_str(),
        "gen-0001"
    );
    let resumed =
        resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
    let roots = resumed.roots.as_ref().unwrap();
    let published = publish_root_set(roots, &fixture.maintenance).unwrap();
    let report = RootSetTransitionReport::new(
        published,
        roots
            .request()
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect(),
        roots.request().mapping_digest(),
    )
    .unwrap();
    let activated = resumed
        .activate_transitioned(Some(&report), "rbresume1")
        .unwrap();
    drop(activated);

    let finish_lease = mutation_lease(&fixture.layout);
    assert_eq!(
        recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
        RecoveryResult::FinishedActivated
    );
    let recovered = load_active_snapshot(&fixture.layout, &finish_lease)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.generation().operation().kind(), "rollback");
    assert_eq!(recovered.state().manifest().entries().len(), 1);
}

#[test]
fn journal_symlink_is_refused_without_touching_target() {
    let fixture = fixture();
    let outside = fixture._temp.path().join("outside");
    fs::write(&outside, b"unchanged").unwrap();
    symlink(
        &outside,
        fixture.layout.state_root().join("journal/journal.ndjson"),
    )
    .unwrap();
    let lease = mutation_lease(&fixture.layout);
    assert!(
        append_phase(
            &fixture.layout,
            &lease,
            "op_fixture",
            "resolve",
            "started",
            []
        )
        .is_err()
    );
    assert_eq!(fs::read(outside).unwrap(), b"unchanged");
}
