//! Tests for the `local` module.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use super::*;
use crate::broker::BrokerLifecycleClient;
use crate::cli::Cli;
use crate::commands::execute::{
    CommandEngine, CommandRequest, CoreEngine, OperationPolicy, write_success,
};
use crate::ux::OutputMode;
use pkg_core::state::{body_digest, canonical_digest};
use pkg_core::{AttributePath, ChannelSequence, NixpkgsRevision, PackageVersion};
use pkg_nix::{
    BuildOutput, BuildOutputProvenance, BuildPreview, BuildReport, BuildStatus, CatalogInfoLookup,
    CatalogInfoReport, CatalogPackageInfo, CatalogPackageSummary, CatalogSearchReport,
    ChannelRefreshReport, CliBrokerRequest, CliBrokerResponse, InProcessBroker,
    InProcessCallerPeer, InProcessHelper, InProcessPeer, MaintenanceErrorCode, ProductFrameCodec,
    RepairGenerationReport, RepairGenerationStatus, StorePath,
};
use pkg_pipeline::{CandidateGeneration, PreparedGeneration};
use pkg_store::inspect_staged_activation;

const FRAME_HEADER_BYTES: usize = 20;
const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn read_request(stream: &mut UnixStream) -> (u64, CliBrokerRequest) {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + length);
    frame.extend_from_slice(&header);
    frame.resize(FRAME_HEADER_BYTES + length, 0);
    stream.read_exact(&mut frame[FRAME_HEADER_BYTES..]).unwrap();
    ProductFrameCodec::decode_cli_request(&frame).unwrap()
}

fn write_response(stream: &mut UnixStream, request_id: u64, response: &CliBrokerResponse) {
    let frame = ProductFrameCodec::encode_cli_response(request_id, response).unwrap();
    stream.write_all(&frame).unwrap();
}

fn repair_fixture() -> (TempDir, StateLayout, LocalStateOperations, u32) {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let layout = StateLayout::initialize(home.path(), &home.path().join("pkg"), uid).unwrap();
    let operations = LocalStateOperations {
        source: layout.clone(),
        broker_state_compatible: true,
    };
    (home, layout, operations, uid)
}

fn repair_args(verify_only: bool, generation: &str) -> crate::cli::RepairArgs {
    let mut argv = vec!["pkg".to_owned(), "repair".to_owned()];
    if verify_only {
        argv.push("--verify-only".to_owned());
    }
    argv.push(generation.to_owned());
    let cli = Cli::try_parse(argv).unwrap();
    let crate::cli::Command::Repair(args) = cli.parsed_command() else {
        panic!("expected repair command");
    };
    args.clone()
}

fn install_evidence(provenance: &str) -> InstallEvidence {
    let store_path = format!("/nix/store/{STORE_HASH}-hello-1.0");
    let derivation = format!("{store_path}.drv");
    InstallEvidence::from_json_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "descriptorHash": format!("sha256-{}", "0".repeat(64)),
            "channelSequence": 42,
            "policyVersion": 7,
            "revision": REVISION,
            "sourceNarHash": NAR_HASH,
            "system": "x86_64-linux",
            "targets": [{
                "selectorId": "sel_hello",
                "selector": "hello",
                "attribute": "hello",
                "versionPreference": { "kind": "any" },
                "requestedOutputs": null,
                "sourceRevision": "channel:current",
                "rootDerivation": derivation,
                "rootOutputs": [{ "name": "out", "storePath": store_path }],
                "outputsToInstall": ["out"],
                "packageName": "hello",
                "packageVersion": "1.0",
                "acquired": [{
                    "outputName": "out",
                    "storePath": store_path,
                    "narHash": NAR_HASH,
                    "signatures": if provenance == "cacheSigned" {
                        vec!["cache.nixos.org-1:AAAA"]
                    } else {
                        Vec::new()
                    },
                    "references": [],
                    "deriver": derivation,
                    "narSize": 20,
                    "closureSize": 42,
                    "provenance": provenance
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap()
}

fn build_preview() -> BuildPreview {
    BuildPreview::from_json_bytes(
            &serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "purpose": "build",
                "platform": { "os": "linux", "arch": "x86_64" },
                "policyVersion": 7,
                "buildPlanDigest": format!("sha256:{}", "1".repeat(64)),
                "targets": [{
                    "selector": "hello",
                    "packageName": "hello",
                    "version": "1.0",
                    "outputsToInstall": ["out"],
                    "localBuildRequired": true
                }],
                "build": { "count": 1, "names": ["hello"], "hasFixedOutput": false },
                "cache": { "knownDownloadBytes": 0, "knownContentBytes": 0 },
                "unknownLocalOutputs": 1,
                "estimates": {
                    "approxBuildMinutes": null,
                    "approxNewDiskBytes": 1073741824,
                    "approxTotalClosureBytes": null
                },
                "readiness": {
                    "sandboxed": true,
                    "buildIsolationReady": true,
                    "nativeBuild": true,
                    "resourceBoundary": {
                        "isolation": "sandbox",
                        "perBuildResourceCap": false,
                        "notice": "Builds run sandboxed. Determinate controls daemon limits and build parallelism. pkg admits one machine-global build operation and applies no hard per-build memory/CPU/IO cap."
                    }
                },
                "approvalRequired": true
            }))
            .unwrap(),
        )
        .unwrap()
}

fn hello_selectors() -> Vec<PackageSelector> {
    let cli = Cli::try_parse(["pkg", "install", "hello"]).unwrap();
    let crate::cli::Command::Install(args) = cli.parsed_command() else {
        panic!("expected install command");
    };
    install_selectors(args, "00112233445566778899aabbccddeeff").unwrap()
}

#[test]
fn gc_wait_does_not_hold_the_local_state_lease() {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let layout = StateLayout::initialize(home.path(), &home.path().join("pkg"), uid).unwrap();
    let operations = LocalStateOperations {
        source: layout.clone(),
        broker_state_compatible: true,
    };

    let broker = InProcessBroker::new().unwrap();
    let build_caller = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .unwrap();
    let build = build_caller.begin(BrokerOperationKind::Build).unwrap();
    build_caller.acquire_build(&build).unwrap();
    build_caller.acquire_gc_inhibit(&build).unwrap();

    let (mut server_stream, client_stream) = UnixStream::pair().unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let server_broker = broker.clone();
    let server = thread::spawn(move || {
        let gc_caller = server_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut server_stream);
        assert_eq!(request, CliBrokerRequest::Begin(BrokerOperationKind::Gc));
        let gc = gc_caller.begin(BrokerOperationKind::Gc).unwrap();
        write_response(
            &mut server_stream,
            request_id,
            &CliBrokerResponse::Started(gc.clone()),
        );

        let (request_id, request) = read_request(&mut server_stream);
        assert_eq!(request, CliBrokerRequest::AcquireGc(gc.clone()));
        waiting_tx.send(()).unwrap();
        gc_caller.acquire_gc_wait(&gc).unwrap();
        write_response(
            &mut server_stream,
            request_id,
            &CliBrokerResponse::GcAdmissionAcquired,
        );

        let (request_id, request) = read_request(&mut server_stream);
        assert_eq!(request, CliBrokerRequest::Complete(gc.clone()));
        gc_caller.complete(&gc).unwrap();
        write_response(
            &mut server_stream,
            request_id,
            &CliBrokerResponse::Completed,
        );
    });

    let recovery_layout = layout.clone();
    let recovery = thread::spawn(move || {
        let mut client = BrokerLifecycleClient::from_stream(client_stream);
        operations.recover_pending_prunes(&recovery_layout, &mut client)
    });

    waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let identity = LeaseIdentity::new("op_probe", "nonce_probe", "2026-08-21T00:00:00Z").unwrap();
    let probe = StateLease::try_exclusive(&layout, &identity);
    let lease_was_available = probe.is_ok();
    drop(probe);
    build_caller.cancel(&build).unwrap();

    assert!(lease_was_available, "GC admission wait held StateLease");
    assert_eq!(recovery.join().unwrap().unwrap(), Vec::<String>::new());
    server.join().unwrap();
    let admissions = broker.admission_snapshot();
    assert!(!admissions.build_held());
    assert!(!admissions.gc_held());
    assert_eq!(admissions.gc_inhibitor_count(), 0);
}

#[test]
fn verify_only_repair_allows_state_mutation_and_blocks_selected_history_prune_until_finish() {
    let (_home, layout, uid) = prepared_pending_install_fixture();
    let broker = InProcessBroker::new().unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(uid);

    let generation_one = GenerationId::new("gen-0001").unwrap();
    let setup_nonce = "00112233445566778899aabbccddeeff";
    let setup_identity =
        LeaseIdentity::new("op_setup", setup_nonce, "2026-08-09T00:00:00Z").unwrap();
    let setup_lease = StateLease::try_exclusive(&layout, &setup_identity).unwrap();
    assert_eq!(
        pending_install_generation(&layout, &setup_lease).unwrap(),
        Some(generation_one.clone())
    );
    let prepared = resume_prepared_install(layout.clone(), setup_lease, &generation_one).unwrap();
    let intent = prepared.root_intent().unwrap().unwrap();
    let generation_one_roots =
        RootSet::new(uid, generation_one.clone(), intent.entries().to_vec()).unwrap();
    let generation_one_report = maintenance.publish_root_set(&generation_one_roots).unwrap();
    prepared
        .activate_published(Some(&generation_one_report), setup_nonce)
        .unwrap()
        .finish()
        .unwrap();

    let manifest_bytes = fs::read(
        layout
            .state_root()
            .join("generations/gen-0001.manifest.json"),
    )
    .unwrap();
    let lock_bytes = fs::read(layout.state_root().join("generations/gen-0001.lock.json")).unwrap();
    let mut generation: Value = serde_json::from_slice(
        &fs::read(layout.state_root().join("generations/gen-0001.json")).unwrap(),
    )
    .unwrap();
    {
        let generation = generation.as_object_mut().unwrap();
        generation.remove("generationHash");
        generation.insert("id".into(), json!("gen-0002"));
        generation.insert("parent".into(), json!("gen-0001"));
        generation.insert("createdAt".into(), json!("2026-08-10T00:00:00Z"));
        generation.insert(
            "manifestSnapshot".into(),
            json!("generations/gen-0002.manifest.json"),
        );
        generation.insert(
            "lockSnapshot".into(),
            json!("generations/gen-0002.lock.json"),
        );
        generation
            .get_mut("activation")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("treePath".into(), json!("activations/gen-0002"));
        generation
            .get_mut("operation")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("opId".into(), json!("op_setup_two"));
    }
    let generation_hash = canonical_digest(&generation).unwrap().to_string();
    generation
        .as_object_mut()
        .unwrap()
        .insert("generationHash".into(), json!(generation_hash));
    let staging = layout.state_root().join("activations/gen-0002.staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    let store_path = format!("/nix/store/{STORE_HASH}-hello-1.0");
    symlink(format!("{store_path}/bin/hello"), staging.join("hello")).unwrap();
    let plan = inspect_staged_activation(
        &staging,
        vec![pkg_core::StorePath::new(&store_path).unwrap()],
    )
    .unwrap();
    let candidate = CandidateGeneration::new(
        manifest_bytes,
        lock_bytes,
        serde_json::to_vec(&generation).unwrap(),
    )
    .unwrap();
    let setup_two_nonce = "11112222333344445555666677778888";
    let setup_two_identity =
        LeaseIdentity::new("op_setup_two", setup_two_nonce, "2026-08-10T00:00:00Z").unwrap();
    let setup_two_lease = StateLease::try_exclusive(&layout, &setup_two_identity).unwrap();
    let prepared =
        PreparedGeneration::prepare(layout.clone(), candidate, plan, setup_two_lease).unwrap();
    let intent = prepared.root_intent().unwrap().unwrap();
    let generation_two = GenerationId::new("gen-0002").unwrap();
    let generation_two_roots =
        RootSet::new(uid, generation_two.clone(), intent.entries().to_vec()).unwrap();
    let generation_two_report = maintenance.publish_root_set(&generation_two_roots).unwrap();
    prepared
        .activate_published(Some(&generation_two_report), setup_two_nonce)
        .unwrap()
        .finish()
        .unwrap();
    let initial_identity =
        LeaseIdentity::new("op_initial", "nonce_initial", "2026-08-10T01:00:00Z").unwrap();
    let initial_lease = StateLease::try_exclusive(&layout, &initial_identity).unwrap();
    let initial_active = load_active_snapshot(&layout, &initial_lease)
        .unwrap()
        .unwrap();
    let initial_history = load_retained_history(&layout, &initial_lease).unwrap();
    assert_eq!(initial_active.generation().id(), "gen-0002");
    assert_eq!(initial_active.state().manifest().entries().len(), 1);
    assert_eq!(
        initial_history
            .snapshots()
            .iter()
            .map(|snapshot| snapshot.generation().id())
            .collect::<Vec<_>>(),
        vec!["gen-0002", "gen-0001"]
    );
    drop(initial_lease);
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(uid, generation_one.clone(),))
            .unwrap(),
        generation_one_report
    );
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(uid, generation_two))
            .unwrap(),
        generation_two_report
    );

    let (mut repair_server_stream, repair_client_stream) = UnixStream::pair().unwrap();
    let (repair_started_tx, repair_started_rx) = mpsc::channel();
    let (release_verification_tx, release_verification_rx) = mpsc::channel();
    let (repair_returned_tx, repair_returned_rx) = mpsc::channel();
    let repair_broker = broker.clone();
    let repair_server = thread::spawn(move || {
        let caller = repair_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut repair_server_stream);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Repair)
        );
        let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
        write_response(
            &mut repair_server_stream,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut repair_server_stream);
        let CliBrokerRequest::RepairGeneration(actual, repair_request) = request else {
            panic!("expected repair generation");
        };
        assert_eq!(actual, handle);
        assert_eq!(repair_request.generation().as_str(), "gen-0001");
        assert!(repair_request.verify_only());
        caller.begin_repair_dispatch(&handle).unwrap();
        repair_started_tx.send(handle.clone()).unwrap();
        release_verification_rx.recv().unwrap();
        caller.complete_repair_dispatch(&handle).unwrap();
        caller.finish_repair_dispatch(&handle, true).unwrap();
        let report = RepairGenerationReport::new(RepairGenerationStatus::Clean, 0).unwrap();
        write_response(
            &mut repair_server_stream,
            request_id,
            &CliBrokerResponse::RepairGeneration(report),
        );
        repair_returned_rx.recv().unwrap();
        handle
    });
    let repair_operations = LocalStateOperations {
        source: layout.clone(),
        broker_state_compatible: true,
    };
    let repair = thread::spawn(move || {
        let mut client = BrokerLifecycleClient::from_stream(repair_client_stream);
        let result = repair_operations.repair_with_broker(
            &mut client,
            &repair_args(true, "gen-0001"),
            OperationPolicy::for_test(true, false),
        );
        repair_returned_tx.send(()).unwrap();
        result
    });
    let repair_handle = repair_started_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    let mutation_nonce = "9999aaaabbbbccccddddeeeeffff0000";
    let mutation_identity =
        LeaseIdentity::new("op_gen-0003", mutation_nonce, "2026-08-11T00:00:00Z").unwrap();
    let mutation_lease = StateLease::try_exclusive(&layout, &mutation_identity).unwrap();
    let mutation_source = load_active_snapshot(&layout, &mutation_lease)
        .unwrap()
        .unwrap();
    let remove = Cli::try_parse(["pkg", "remove", "hello"]).unwrap();
    let crate::cli::Command::Remove(remove_args) = remove.parsed_command() else {
        panic!("expected remove state edit");
    };
    let next = remove_state(mutation_source.state().clone(), remove_args)
        .unwrap()
        .into_parts()
        .0;
    let source_generation = GenerationId::new(mutation_source.generation().id()).unwrap();
    let prepared = prepare_state_edit(
        layout.clone(),
        mutation_lease,
        &mutation_source,
        &next,
        StateEditMetadata::new(
            "gen-0003",
            "2026-08-11T00:00:00Z",
            "op_gen-0003",
            StateEditKind::Remove,
        ),
    )
    .unwrap();
    assert!(
        prepared
            .root_transition_intent(source_generation)
            .unwrap()
            .is_none()
    );
    let mutation_caller = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .unwrap();
    let mutation_handle = mutation_caller
        .begin(BrokerOperationKind::Activate)
        .unwrap();
    prepared
        .activate_transitioned(None, mutation_nonce)
        .unwrap()
        .finish()
        .unwrap();
    mutation_caller.complete(&mutation_handle).unwrap();
    let snapshot_identity =
        LeaseIdentity::new("op_snapshot", "nonce_snapshot", "2026-08-11T01:00:00Z").unwrap();
    let snapshot_lease = StateLease::try_exclusive(&layout, &snapshot_identity).unwrap();
    let generation_snapshot = load_retained_history(&layout, &snapshot_lease).unwrap();
    let active_snapshot = load_active_snapshot(&layout, &snapshot_lease)
        .unwrap()
        .unwrap();
    assert_eq!(active_snapshot.generation().id(), "gen-0003");
    assert!(active_snapshot.state().manifest().entries().is_empty());
    assert_eq!(
        generation_snapshot
            .snapshots()
            .iter()
            .map(|snapshot| snapshot.generation().id())
            .collect::<Vec<_>>(),
        vec!["gen-0003", "gen-0002", "gen-0001"]
    );
    drop(snapshot_lease);
    let root_snapshot = [
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(uid, generation_one.clone()))
            .unwrap(),
        generation_two_report,
    ];
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(
                uid,
                GenerationId::new("gen-0003").unwrap(),
            ))
            .unwrap_err()
            .code(),
        MaintenanceErrorCode::GenerationNotRooted
    );

    let (mut prune_server_stream, prune_client_stream) = UnixStream::pair().unwrap();
    let (gc_waiting_tx, gc_waiting_rx) = mpsc::channel();
    let (gc_admitted_tx, gc_admitted_rx) = mpsc::channel();
    let (prune_returned_tx, prune_returned_rx) = mpsc::channel();
    let prune_broker = broker.clone();
    let prune_maintenance = maintenance.clone();
    let prune_server = thread::spawn(move || {
        let caller = prune_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut prune_server_stream);
        assert_eq!(request, CliBrokerRequest::Begin(BrokerOperationKind::Gc));
        let handle = caller.begin(BrokerOperationKind::Gc).unwrap();
        write_response(
            &mut prune_server_stream,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut prune_server_stream);
        assert_eq!(request, CliBrokerRequest::AcquireGc(handle.clone()));
        gc_waiting_tx.send(()).unwrap();
        caller.acquire_gc_wait(&handle).unwrap();
        gc_admitted_tx.send(()).unwrap();
        write_response(
            &mut prune_server_stream,
            request_id,
            &CliBrokerResponse::GcAdmissionAcquired,
        );

        let (request_id, request) = read_request(&mut prune_server_stream);
        assert_eq!(
            request,
            CliBrokerRequest::RemoveGenerationRoots(
                handle.clone(),
                GenerationId::new("gen-0001").unwrap()
            )
        );
        caller
            .remove_generation_root_intent(
                &handle,
                GenerationId::new("gen-0001").unwrap(),
                |request| prune_maintenance.remove_root_set(request),
            )
            .unwrap();
        write_response(
            &mut prune_server_stream,
            request_id,
            &CliBrokerResponse::GenerationRootsRemoved,
        );

        let (request_id, request) = read_request(&mut prune_server_stream);
        assert_eq!(request, CliBrokerRequest::Complete(handle.clone()));
        caller.complete(&handle).unwrap();
        write_response(
            &mut prune_server_stream,
            request_id,
            &CliBrokerResponse::Completed,
        );
        prune_returned_rx.recv().unwrap();
        handle
    });
    let prune_layout = layout.clone();
    let prune = thread::spawn(move || {
        let mut client = BrokerLifecycleClient::from_stream(prune_client_stream);
        let handle = client.begin(BrokerOperationKind::Gc).unwrap();
        client.acquire_gc(handle.clone()).unwrap();
        let identity =
            LeaseIdentity::new("op_prune", "nonce_prune", "2026-08-12T00:00:00Z").unwrap();
        let lease = StateLease::try_exclusive(&prune_layout, &identity).unwrap();
        let active = load_active_snapshot(&prune_layout, &lease)
            .unwrap()
            .unwrap();
        let history = load_retained_history(&prune_layout, &lease).unwrap();
        ensure_generation_deletable(&active, &history, "gen-0001").unwrap();
        let candidate = plan_generation_prune(
            &active,
            history.snapshots(),
            "gen-0001",
            unix_now().unwrap(),
        )
        .unwrap();
        let maintenance = BrokerGcMaintenance {
            broker: Mutex::new(&mut client),
            handle: handle.clone(),
        };
        let outcome =
            prune_generation(&prune_layout, &lease, &candidate, &maintenance, "op_prune").unwrap();
        drop(maintenance);
        client.complete(handle).unwrap();
        prune_returned_tx.send(()).unwrap();
        outcome
    });

    gc_waiting_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        gc_admitted_rx.recv_timeout(Duration::from_millis(75)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    assert!(!prune.is_finished());
    let waiting_admission = broker.admission_snapshot();
    assert!(!waiting_admission.gc_held());
    assert_eq!(waiting_admission.gc_inhibitor_count(), 1);

    let waiting_identity =
        LeaseIdentity::new("op_waiting", "nonce_waiting", "2026-08-11T02:00:00Z").unwrap();
    let waiting_lease = StateLease::try_exclusive(&layout, &waiting_identity).unwrap();
    assert_eq!(
        load_active_snapshot(&layout, &waiting_lease)
            .unwrap()
            .unwrap(),
        active_snapshot
    );
    assert_eq!(
        load_retained_history(&layout, &waiting_lease)
            .unwrap()
            .snapshots(),
        generation_snapshot.snapshots()
    );
    drop(waiting_lease);
    assert_eq!(
        [
            maintenance
                .attest_root_set(&RootSetAttestationRequest::new(uid, generation_one.clone(),))
                .unwrap(),
            maintenance
                .attest_root_set(&RootSetAttestationRequest::new(
                    uid,
                    GenerationId::new("gen-0002").unwrap(),
                ))
                .unwrap(),
        ],
        root_snapshot
    );
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(
                uid,
                GenerationId::new("gen-0003").unwrap(),
            ))
            .unwrap_err()
            .code(),
        MaintenanceErrorCode::GenerationNotRooted
    );

    release_verification_tx.send(()).unwrap();
    repair.join().unwrap().unwrap();
    assert_eq!(repair_server.join().unwrap(), repair_handle);
    assert_eq!(
        broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap()
            .poll(&repair_handle)
            .unwrap(),
        OperationStatus::Completed
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    gc_admitted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(prune.join().unwrap(), PruneOutcome::Pruned);
    let prune_handle = prune_server.join().unwrap();
    assert_eq!(
        broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap()
            .poll(&prune_handle)
            .unwrap(),
        OperationStatus::Completed
    );

    let final_identity =
        LeaseIdentity::new("op_final", "nonce_final", "2026-08-12T01:00:00Z").unwrap();
    let final_lease = StateLease::try_exclusive(&layout, &final_identity).unwrap();
    assert_eq!(
        load_retained_history(&layout, &final_lease)
            .unwrap()
            .snapshots()
            .iter()
            .map(|snapshot| snapshot.generation().id())
            .collect::<Vec<_>>(),
        vec!["gen-0003", "gen-0002"]
    );
    assert!(
        !layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
    drop(final_lease);
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(uid, generation_one))
            .unwrap_err()
            .code(),
        MaintenanceErrorCode::GenerationNotRooted
    );
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(
                uid,
                GenerationId::new("gen-0002").unwrap(),
            ))
            .unwrap(),
        root_snapshot[1]
    );
    assert_eq!(
        maintenance
            .attest_root_set(&RootSetAttestationRequest::new(
                uid,
                GenerationId::new("gen-0003").unwrap(),
            ))
            .unwrap_err()
            .code(),
        MaintenanceErrorCode::GenerationNotRooted
    );
    let final_admission = broker.admission_snapshot();
    assert!(!final_admission.build_held());
    assert!(!final_admission.gc_held());
    assert_eq!(final_admission.gc_inhibitor_count(), 0);
}

#[test]
fn mutating_repair_keeps_the_exclusive_state_lease() {
    let (_home, layout, operations, uid) = repair_fixture();
    let (mut server_stream, client_stream) = UnixStream::pair().unwrap();
    let (probed_tx, probed_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let server_layout = layout;
    let server = thread::spawn(move || {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut server_stream);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Repair)
        );
        let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
        write_response(
            &mut server_stream,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut server_stream);
        let CliBrokerRequest::RepairGeneration(actual, repair_request) = request else {
            panic!("expected repair generation");
        };
        assert_eq!(actual, handle);
        assert_eq!(repair_request.generation().as_str(), "gen-0001");
        assert!(!repair_request.verify_only());

        let lease_held = matches!(
            StateLease::try_exclusive(
                &server_layout,
                &LeaseIdentity::new("op_probe", "nonce_probe", "2026-08-21T00:00:00Z").unwrap(),
            ),
            Err(LeaseError::Locked)
        );
        probed_tx.send(lease_held).unwrap();

        let report = RepairGenerationReport::new(RepairGenerationStatus::Clean, 0).unwrap();
        write_response(
            &mut server_stream,
            request_id,
            &CliBrokerResponse::RepairGeneration(report),
        );
        done_rx.recv().unwrap();
    });

    let mut client = BrokerLifecycleClient::from_stream(client_stream);
    let args = repair_args(false, "gen-0001");
    let result =
        operations.repair_with_broker(&mut client, &args, OperationPolicy::for_test(true, false));
    done_tx.send(()).unwrap();
    server.join().unwrap();
    assert!(result.is_ok());
    assert!(probed_rx.recv_timeout(Duration::from_secs(1)).unwrap());
}

#[test]
fn repair_begin_failure_holds_the_lease_through_failure_and_releases_after() {
    let (_home, layout, operations, _uid) = repair_fixture();
    let (mut server_stream, client_stream) = UnixStream::pair().unwrap();
    let (probed_tx, probed_rx) = mpsc::channel();
    let server_layout = layout.clone();
    let server = thread::spawn(move || {
        let (_request_id, request) = read_request(&mut server_stream);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Repair)
        );
        let lease_held = matches!(
            StateLease::try_exclusive(
                &server_layout,
                &LeaseIdentity::new("op_probe", "nonce_probe", "2026-08-21T00:00:00Z").unwrap(),
            ),
            Err(LeaseError::Locked)
        );
        probed_tx.send(lease_held).unwrap();
        drop(server_stream);
    });

    let mut client = BrokerLifecycleClient::from_stream(client_stream);
    let args = repair_args(true, "gen-0001");
    let result =
        operations.repair_with_broker(&mut client, &args, OperationPolicy::for_test(true, false));
    server.join().unwrap();
    assert!(result.is_err());
    assert!(probed_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    let released = StateLease::try_exclusive(
        &layout,
        &LeaseIdentity::new("op_probe2", "nonce_probe2", "2026-08-21T00:00:00Z").unwrap(),
    )
    .is_ok();
    assert!(released);
}

#[test]
fn install_preview_uses_the_outer_public_schema() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let preview = build_preview();
    let worker = thread::spawn(move || {
        let caller = InProcessBroker::new()
            .unwrap()
            .connect(InProcessCallerPeer::authenticated(501))
            .unwrap();
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Begin(BrokerOperationKind::Build));
        let handle = caller.begin(BrokerOperationKind::Build).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );
        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::PrepareBuild(actual, selectors) = request else {
            panic!("expected build preparation");
        };
        assert_eq!(actual, handle);
        assert_eq!(selectors[0].selector().as_str(), "hello");
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::BuildPrepared(preview),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Cancel(handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Cancelled);
    });

    let result = preview_install(
        &mut BrokerLifecycleClient::from_stream(client),
        hello_selectors(),
    )
    .unwrap();
    worker.join().unwrap();
    assert_eq!(result.fields()["dryRun"], true);
    assert!(result.fields()["preflight"].get("schemaVersion").is_none());
    assert_eq!(result.fields()["preflight"]["approvalRequired"], true);
}

#[test]
fn cache_hit_uses_the_closed_acquire_protocol_and_returns_evidence() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let expected = install_evidence("cacheSigned");
    let server_evidence = expected.clone();
    let worker = thread::spawn(move || {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(501))
            .unwrap();

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::Begin(BrokerOperationKind::Acquire) = request else {
            panic!("expected acquire begin");
        };
        let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::AcquireInstall(actual, selectors) = request else {
            panic!("expected cache acquisition");
        };
        assert_eq!(actual, handle);
        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0].selector().as_str(), "hello");
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallDownloadProgress(
                pkg_nix::InstallDownloadProgress::new(SelectorInput::new("hello").unwrap(), 0, 42)
                    .unwrap(),
            ),
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallDownloadProgress(
                pkg_nix::InstallDownloadProgress::new(SelectorInput::new("hello").unwrap(), 42, 42)
                    .unwrap(),
            ),
        );
        write_response(&mut server, request_id, &CliBrokerResponse::InstallAcquired);

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::GetInstallEvidence(actual) = request else {
            panic!("expected private install evidence request");
        };
        assert_eq!(actual, handle);
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallEvidence(server_evidence),
        );
        let mut eof = [0_u8; 1];
        assert_eq!(server.read(&mut eof).unwrap(), 0);
    });

    let mut broker = BrokerLifecycleClient::from_stream(client);
    let mut events = Vec::new();
    let (handle, public_operation_id, actual, approval) = acquire_install_evidence(
        &mut broker,
        hello_selectors(),
        OperationPolicy::for_test(true, false),
        true,
        &mut |event| {
            events.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert!(!handle.as_str().is_empty());
    assert_ne!(public_operation_id, handle.as_str());
    assert_eq!(actual, expected);
    assert_eq!(approval, "not_required");
    assert_eq!(events.len(), 4);
    let rendered = events
        .iter()
        .map(|event| String::from_utf8(event.to_ndjson_line().unwrap()).unwrap())
        .collect::<String>();
    assert!(rendered.contains(r#""type":"download_started""#));
    assert!(rendered.contains(r#""type":"download_progress""#));
    assert!(rendered.contains(r#""done":42,"total":42"#));
    assert!(
        events
            .iter()
            .all(|event| event.op_id() == public_operation_id)
    );
    drop(broker);
    worker.join().unwrap();
}

#[test]
fn local_install_reports_each_build_preparation_refusal_and_cancels() {
    for code in [
        pkg_nix::BuildPreparationErrorCode::HostRefused,
        pkg_nix::BuildPreparationErrorCode::IntentRefused,
        pkg_nix::BuildPreparationErrorCode::PlanningRefused,
        pkg_nix::BuildPreparationErrorCode::BrokerRefused,
    ] {
        let (mut server, client) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let caller = InProcessBroker::new()
                .unwrap()
                .connect(InProcessCallerPeer::authenticated(501))
                .unwrap();
            let (request_id, request) = read_request(&mut server);
            assert_eq!(
                request,
                CliBrokerRequest::Begin(BrokerOperationKind::Acquire)
            );
            let acquire = caller.begin(BrokerOperationKind::Acquire).unwrap();
            write_response(
                &mut server,
                request_id,
                &CliBrokerResponse::Started(acquire.clone()),
            );
            let (request_id, request) = read_request(&mut server);
            assert!(matches!(request, CliBrokerRequest::AcquireInstall(_, _)));
            write_response(
                &mut server,
                request_id,
                &CliBrokerResponse::InstallBuildRequired,
            );
            let (request_id, request) = read_request(&mut server);
            assert_eq!(request, CliBrokerRequest::Complete(acquire.clone()));
            caller.complete(&acquire).unwrap();
            write_response(&mut server, request_id, &CliBrokerResponse::Completed);

            let (request_id, request) = read_request(&mut server);
            assert_eq!(request, CliBrokerRequest::Begin(BrokerOperationKind::Build));
            let build = caller.begin(BrokerOperationKind::Build).unwrap();
            write_response(
                &mut server,
                request_id,
                &CliBrokerResponse::Started(build.clone()),
            );
            let (request_id, request) = read_request(&mut server);
            assert!(matches!(request, CliBrokerRequest::PrepareBuild(_, _)));
            write_response(
                &mut server,
                request_id,
                &CliBrokerResponse::BuildPreparationRefused(code),
            );
            let (request_id, request) = read_request(&mut server);
            assert_eq!(request, CliBrokerRequest::Cancel(build.clone()));
            caller.cancel(&build).unwrap();
            write_response(&mut server, request_id, &CliBrokerResponse::Cancelled);
            assert_eq!(caller.poll(&build).unwrap(), OperationStatus::Cancelled);
        });

        let mut events = Vec::new();
        let error = acquire_install_evidence(
            &mut BrokerLifecycleClient::from_stream(client),
            hello_selectors(),
            OperationPolicy::for_test(true, false),
            true,
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap_err();
        worker.join().unwrap();
        assert_eq!(error.exit_code(), ExitCode::EngineUnavailable);
        let event = serde_json::to_value(events.last().unwrap()).unwrap();
        assert_eq!(event["type"], "phase");
        assert_eq!(event["schemaVersion"], 1);
        assert_eq!(event["phase"], "build_prepare");
        assert_eq!(event["status"], code.as_str());
        assert!(event["opId"].as_str().unwrap().starts_with("op_"));
    }
}

#[test]
fn cache_miss_uses_one_digest_bound_build_and_returns_local_evidence() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let preview = build_preview();
    let digest = parse_build_plan_digest(preview.build_plan_digest()).unwrap();
    let expected = install_evidence("localBuild");
    let server_evidence = expected.clone();
    let worker = thread::spawn(move || {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(501))
            .unwrap();

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::Begin(BrokerOperationKind::Acquire) = request else {
            panic!("expected acquire begin");
        };
        let acquire_handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(acquire_handle.clone()),
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::AcquireInstall(actual, selectors) = request else {
            panic!("expected cache acquisition");
        };
        assert_eq!(actual, acquire_handle);
        assert_eq!(selectors[0].selector().as_str(), "hello");
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallDownloadProgress(
                pkg_nix::InstallDownloadProgress::new(
                    SelectorInput::new("hello").unwrap(),
                    0,
                    17_072,
                )
                .unwrap(),
            ),
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallBuildRequired,
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::Complete(actual) = request else {
            panic!("expected cache operation completion");
        };
        assert_eq!(actual, acquire_handle);
        write_response(&mut server, request_id, &CliBrokerResponse::Completed);

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::Begin(BrokerOperationKind::Build) = request else {
            panic!("expected build begin");
        };
        let build_handle = caller.begin(BrokerOperationKind::Build).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(build_handle.clone()),
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::PrepareBuild(actual, selectors) = request else {
            panic!("expected private build preparation");
        };
        assert_eq!(actual, build_handle);
        assert_eq!(selectors[0].selector().as_str(), "hello");
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::BuildPrepared(preview),
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::ApproveBuild(actual, approval) = request else {
            panic!("expected exact build approval");
        };
        assert_eq!(actual, build_handle);
        assert_eq!(approval.build_plan_digest(), digest);
        assert_eq!(approval.source(), ApprovalSource::AssumeYes);
        write_response(&mut server, request_id, &CliBrokerResponse::BuildApproved);

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::ExecuteBuild(actual, actual_digest) = request else {
            panic!("expected exact build execution");
        };
        assert_eq!(actual, build_handle);
        assert_eq!(actual_digest, digest);
        let report = BuildReport::new(
            BuildStatus::Built,
            vec![BuildOutput::new(
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
                BuildOutputProvenance::LocalBuild,
            )],
        )
        .unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::BuildExecuted(report),
        );

        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::GetInstallEvidence(actual) = request else {
            panic!("expected post-build install evidence");
        };
        assert_eq!(actual, build_handle);
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallEvidence(server_evidence),
        );
        let mut eof = [0_u8; 1];
        assert_eq!(server.read(&mut eof).unwrap(), 0);
    });

    let mut broker = BrokerLifecycleClient::from_stream(client);
    let mut events = Vec::new();
    let result = acquire_install_evidence(
        &mut broker,
        hello_selectors(),
        OperationPolicy::for_test(true, false),
        true,
        &mut |event| {
            events.push(event);
            Ok(())
        },
    );
    let (handle, public_operation_id, actual, approval) = match result {
        Ok(result) => result,
        Err(error) => {
            drop(broker);
            let server_result = worker.join();
            panic!("client failed: {error:?}; server: {server_result:?}");
        }
    };
    assert!(!handle.as_str().is_empty());
    assert_eq!(actual, expected);
    assert_eq!(approval, "yes");
    assert_eq!(
        events,
        vec![
            PublicEvent::phase(&public_operation_id, "acquire", "started").unwrap(),
            PublicEvent::download_started(&public_operation_id, "hello", 17_072).unwrap(),
            PublicEvent::phase(&public_operation_id, "acquire", "completed").unwrap(),
            PublicEvent::phase(&public_operation_id, "build", "started").unwrap(),
            PublicEvent::build_started(&public_operation_id, "hello", "hello", "1.0",).unwrap(),
            PublicEvent::build_progress(&public_operation_id, "hello", 0.0).unwrap(),
            PublicEvent::build_progress(&public_operation_id, "hello", 1.0).unwrap(),
            PublicEvent::phase(&public_operation_id, "build", "completed").unwrap(),
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| event.op_id() == public_operation_id)
    );
    drop(broker);
    worker.join().unwrap();
}

#[test]
fn no_build_stops_after_cache_miss_without_opening_build_authority() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(501))
            .unwrap();
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Acquire)
        );
        let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );
        let (request_id, request) = read_request(&mut server);
        let CliBrokerRequest::AcquireInstall(actual, _) = request else {
            return;
        };
        assert_eq!(actual, handle);
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::InstallBuildRequired,
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Complete(handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Completed);
        let mut eof = [0_u8; 1];
        assert_eq!(server.read(&mut eof).unwrap(), 0);
    });
    let mut broker = BrokerLifecycleClient::from_stream(client);

    let error = acquire_install_evidence(
        &mut broker,
        hello_selectors(),
        OperationPolicy::for_test(true, false),
        false,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.exit_code(), ExitCode::AcquireNoBinary);
    drop(broker);
    worker.join().unwrap();
}

#[test]
fn install_success_output_matches_the_v1_golden() {
    let result = install_result(
        "op_fixture",
        "gen-0001",
        None,
        &install_evidence("cacheSigned"),
    )
    .unwrap();
    assert_eq!(result.summary(), "Installed 1 package(s) as gen-0001.");

    let mut output = Vec::new();
    write_success(&mut output, OutputMode::Json, "install", &result).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        include_str!("../../../../../fixtures/cli-v1/install-success.json")
    );
}

#[test]
fn repeated_install_is_not_reported_as_state_corruption() {
    let error = map_install_generation_error(&InstallGenerationError::InvalidEvidence(
        InstallStateError::AlreadyInstalled,
    ));

    assert_eq!(error.exit_code(), ExitCode::PreflightFail);
    assert_eq!(
        error.message(),
        "one or more requested packages are already installed"
    );
}

#[test]
fn missing_state_is_initialized_as_empty_history() {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let cli = Cli::try_parse(["pkg", "history"]).unwrap();
    let location = StateLocation::alternate(home.path().join("pkg"), home.path().to_path_buf());
    let mut engine = CoreEngine::new(LocalStateOperations::open(&location, uid).unwrap());
    let result = engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
    assert_eq!(result.fields()["entries"], Value::Array(vec![]));
    assert_eq!(
        fs::symlink_metadata(home.path().join("pkg"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn initialized_empty_state_reports_no_active_generation() {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let state = home.path().join("pkg");
    let identity =
        pkg_store::LeaseIdentity::new("op_initialize", "nonce1", "2026-08-11T00:00:00Z").unwrap();
    let layout = StateLayout::initialize(home.path(), &state, uid).unwrap();
    drop(StateLease::try_exclusive(&layout, &identity).unwrap());

    let cli = Cli::try_parse(["pkg", "history"]).unwrap();
    let location = StateLocation::alternate(state, home.path().to_path_buf());
    let mut engine = CoreEngine::new(LocalStateOperations::open(&location, uid).unwrap());
    let result = engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
    assert_eq!(result.fields()["entries"], Value::Array(vec![]));
}

#[test]
fn mutation_identity_helpers_are_canonical_and_overflow_safe() {
    assert_eq!(next_generation_id("gen-0009").unwrap(), "gen-0010");
    assert_eq!(next_generation_id("gen-9999").unwrap(), "gen-10000");
    assert!(next_generation_id("generation-1").is_err());
    assert_eq!(format_utc(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    assert_eq!(
        format_utc(951_782_400).as_deref(),
        Some("2000-02-29T00:00:00Z")
    );
    assert_eq!(
        format_utc(1_787_528_645).as_deref(),
        Some("2026-08-23T23:44:05Z")
    );
}

#[test]
fn install_arguments_become_closed_current_channel_selectors() {
    let cli = Cli::try_parse([
        "pkg",
        "install",
        "ripgrep",
        "fd",
        "--with-outputs",
        "out,man",
    ])
    .unwrap();
    let crate::cli::Command::Install(args) = cli.parsed_command() else {
        panic!("expected install command");
    };
    require_supported_install_options(args).unwrap();
    let selectors = install_selectors(args, "00112233445566778899aabbccddeeff").unwrap();

    assert_eq!(selectors.len(), 2);
    assert_eq!(selectors[0].selector().as_str(), "ripgrep");
    assert_eq!(
        selectors[0]
            .outputs()
            .explicit_outputs()
            .unwrap()
            .iter()
            .map(OutputName::as_str)
            .collect::<Vec<_>>(),
        ["out", "man"]
    );
    assert!(matches!(
        selectors[0].source_revision(),
        SourceRevision::CurrentChannel
    ));
    assert_ne!(selectors[0].id(), selectors[1].id());
}

#[test]
fn install_argument_widening_is_refused_before_broker_access() {
    for argv in [
        vec!["pkg", "install", "ripgrep", "ripgrep"],
        vec!["pkg", "install", "ripgrep", "--channel", "other"],
    ] {
        let cli = Cli::try_parse(argv).unwrap();
        let crate::cli::Command::Install(args) = cli.parsed_command() else {
            panic!("expected install command");
        };
        assert!(
            require_supported_install_options(args).is_err()
                || install_selectors(args, "00112233445566778899aabbccddeeff").is_err()
        );
    }
}

#[test]
fn install_collision_policy_reaches_the_state_boundary() {
    for (value, expected) in [
        ("abort", StateCollisionPolicy::Abort),
        ("keep-first", StateCollisionPolicy::KeepFirst),
        ("keep-last", StateCollisionPolicy::KeepLast),
    ] {
        let cli = Cli::try_parse(["pkg", "install", "ripgrep", "--on-collision", value]).unwrap();
        let crate::cli::Command::Install(args) = cli.parsed_command() else {
            panic!("expected install command");
        };
        require_supported_install_options(args).unwrap();
        assert_eq!(state_collision_policy(args.collision_policy()), expected);
    }
}

#[test]
fn channel_refresh_runs_one_authenticated_transaction() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let handle = InProcessBroker::new()
        .unwrap()
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap()
        .begin(BrokerOperationKind::Refresh)
        .unwrap();
    let server_handle = handle;
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Refresh)
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(server_handle.clone()),
        );

        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::RefreshChannel(server_handle.clone(), ChannelRefreshMode::Apply,)
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::ChannelRefreshed(ChannelRefreshReport::new(
                true,
                ChannelSequence::from_u64(43).unwrap(),
            )),
        );

        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Complete(server_handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Completed);
        release_rx.recv().unwrap();
    });
    let mut broker = BrokerLifecycleClient::from_stream(client);

    let result = refresh_channel_metadata(&mut broker, ChannelRefreshMode::Apply);
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let report = result.unwrap();
    let result = channel_refresh_result(report, ChannelRefreshMode::Apply, false).unwrap();

    assert_eq!(result.fields()["updated"], Value::Bool(true));
    assert_eq!(result.fields()["channelSequence"], Value::from(43));
}

#[test]
fn channel_refresh_failure_classes_keep_stable_exit_codes() {
    for (code, expected) in [
        (
            BrokerClientErrorCode::ChannelRefreshNetwork,
            ExitCode::AcquireNetwork,
        ),
        (
            BrokerClientErrorCode::ChannelRefreshVerification,
            ExitCode::VerifyFail,
        ),
        (
            BrokerClientErrorCode::ChannelRefreshBusy,
            ExitCode::StateLocked,
        ),
        (
            BrokerClientErrorCode::ChannelRefreshServiceUnavailable,
            ExitCode::EngineUnavailable,
        ),
    ] {
        assert_eq!(channel_refresh_error_fields(code).0, expected);
    }
}

#[test]
fn catalog_search_runs_one_closed_resolve_transaction() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let handle = InProcessBroker::new()
        .unwrap()
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap()
        .begin(BrokerOperationKind::Resolve)
        .unwrap();
    let server_handle = handle;
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Resolve)
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(server_handle.clone()),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::SearchCatalog(
                server_handle.clone(),
                CatalogSearchRequest::new("ripgrep", 25, false, None).unwrap(),
            )
        );
        let summary = CatalogPackageSummary::new(
            "ripgrep",
            "ripgrep",
            "14.1.1",
            "fast search",
            vec![String::from("MIT")],
            true,
            false,
        )
        .unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::CatalogSearch(
                CatalogSearchReport::new(
                    ChannelSequence::from_u64(42).unwrap(),
                    "2026-08-19T00:00:00Z",
                    vec![summary],
                )
                .unwrap(),
            ),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Complete(server_handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Completed);
        release_rx.recv().unwrap();
    });
    let mut broker = BrokerLifecycleClient::from_stream(client);

    let result = run_catalog_search(
        &mut broker,
        CatalogSearchRequest::new("ripgrep", 25, false, None).unwrap(),
    );
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let result = result.unwrap();
    assert_eq!(result.fields()["stale"], Value::Bool(false));
    assert_eq!(result.fields()["entries"][0]["package"], "ripgrep");
}

#[test]
fn install_failure_diagnosis_lists_ambiguous_catalog_ids() {
    let (mut server, client) = UnixStream::pair().unwrap();
    let handle = InProcessBroker::new()
        .unwrap()
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap()
        .begin(BrokerOperationKind::Resolve)
        .unwrap();
    let server_handle = handle;
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Resolve)
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(server_handle.clone()),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::InfoCatalog(
                server_handle.clone(),
                vec![CatalogInfoRequest::new("requests").unwrap()],
            )
        );
        let candidates = ["python3Packages.requests", "pythonPackages.requests"]
            .map(|package| {
                CatalogPackageSummary::new(
                    package,
                    "requests",
                    "2.32.4",
                    "Python HTTP library",
                    vec![String::from("Apache-2.0")],
                    true,
                    false,
                )
                .unwrap()
            })
            .to_vec();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::CatalogInfo(vec![
                CatalogInfoReport::new(
                    ChannelSequence::from_u64(42).unwrap(),
                    CatalogInfoLookup::Ambiguous(candidates),
                )
                .unwrap(),
            ]),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Cancel(server_handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Cancelled);
        release_rx.recv().unwrap();
    });
    let selector = PackageSelector::new(
        SelectorId::new("sel_test_0").unwrap(),
        SelectorInput::new("requests").unwrap(),
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    );
    let mut broker = BrokerLifecycleClient::from_stream(client);

    let error = diagnose_install_selector_error(&mut broker, &[selector]);
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let error = error.unwrap();
    assert_eq!(error.exit_code(), ExitCode::ResolveFailed);
    assert_eq!(
        error.hint(),
        "choose one: python3Packages.requests, pythonPackages.requests"
    );
}

#[test]
fn catalog_info_renders_only_product_metadata() {
    let summary = CatalogPackageSummary::new(
        "ripgrep",
        "ripgrep",
        "14.1.1",
        "fast search",
        vec![String::from("MIT")],
        true,
        false,
    )
    .unwrap();
    let info = CatalogPackageInfo::new(
        summary,
        "https://example.invalid/ripgrep",
        vec![String::from("out")],
        vec![String::from("linux-x86-64")],
        REVISION,
        "2026-08-12T00:00:00Z",
    )
    .unwrap();
    let report = CatalogInfoReport::new(
        ChannelSequence::from_u64(42).unwrap(),
        CatalogInfoLookup::Found(Box::new(info)),
    )
    .unwrap();

    let result = info_catalog_reports(&[report]).unwrap();
    let encoded = serde_json::to_string(result.fields()).unwrap();
    assert!(encoded.contains("ripgrep"));
    assert!(!encoded.contains("/nix/store/"));
    assert!(!encoded.contains("drvPath"));
    assert!(!encoded.contains("narHash"));
}

#[test]
fn catalog_outdated_uses_one_closed_resolve_transaction() {
    const NEW_REVISION: &str = "89abcdef0123456789abcdef0123456789abcdef";
    let (mut server, client) = UnixStream::pair().unwrap();
    let handle = InProcessBroker::new()
        .unwrap()
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap()
        .begin(BrokerOperationKind::Resolve)
        .unwrap();
    let server_handle = handle;
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || {
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Resolve)
        );
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(server_handle.clone()),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::InfoCatalog(
                server_handle.clone(),
                vec![CatalogInfoRequest::new("ripgrep").unwrap()],
            )
        );
        let summary = CatalogPackageSummary::new(
            "ripgrep",
            "ripgrep",
            "15.0.0",
            "fast search",
            vec![String::from("MIT")],
            true,
            false,
        )
        .unwrap();
        let info = CatalogPackageInfo::new(
            summary,
            "https://example.invalid/ripgrep",
            vec![String::from("out")],
            vec![String::from("linux-x86-64")],
            NEW_REVISION,
            "2026-08-12T00:00:00Z",
        )
        .unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::CatalogInfo(vec![
                CatalogInfoReport::new(
                    ChannelSequence::from_u64(43).unwrap(),
                    CatalogInfoLookup::Found(Box::new(info)),
                )
                .unwrap(),
            ]),
        );
        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Complete(server_handle));
        write_response(&mut server, request_id, &CliBrokerResponse::Completed);
        release_rx.recv().unwrap();
    });
    let installed = vec![InstalledCatalogPackage::new(
        AttributePath::new("ripgrep").unwrap(),
        String::from("ripgrep"),
        PackageVersion::new("14.1.1"),
        NixpkgsRevision::new(REVISION).unwrap(),
        true,
    )];
    let mut broker = BrokerLifecycleClient::from_stream(client);

    let result = run_catalog_outdated(
        &mut broker,
        ChannelSequence::from_u64(42).unwrap(),
        &installed,
    );
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let result = result.unwrap();
    assert_eq!(result.fields()["channelSequence"], 43);
    assert_eq!(result.fields()["entries"][0]["kind"], "major");
    assert_eq!(result.fields()["entries"][0]["pinned"], true);
}

#[test]
fn empty_catalog_outdated_skips_broker_access() {
    let (_server, client) = UnixStream::pair().unwrap();
    let mut broker = BrokerLifecycleClient::from_stream(client);
    let result = run_catalog_outdated(
        &mut broker,
        ChannelSequence::from_u64(42).unwrap(),
        &Vec::new(),
    )
    .unwrap();
    assert_eq!(result.fields()["channelSequence"], 42);
    assert_eq!(result.fields()["entries"], serde_json::json!([]));
}

#[test]
fn alternate_state_roots_are_read_only_for_broker_backed_mutations() {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let state = home.path().join("alternate");
    let cli = Cli::try_parse(["pkg", "gc", "--yes", "--state", state.to_str().unwrap()]).unwrap();
    let location = StateLocation::alternate(state.clone(), home.path().to_path_buf());
    let mut engine = CoreEngine::new(LocalStateOperations::open(&location, uid).unwrap());

    let error = engine.execute(&CommandRequest::from_cli(&cli)).unwrap_err();
    assert_eq!(error.exit_code(), ExitCode::Config);
    assert!(!state.join("journal/operations.jsonl").exists());
}

#[test]
fn upgrade_re_resolves_attributes_inside_the_broker() {
    let selector = PackageSelector::new(
        SelectorId::new("sel_hello").unwrap(),
        SelectorInput::new("hello").unwrap(),
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    )
    .with_attribute(AttributePath::new("hello").unwrap())
    .unwrap();

    let broker = broker_upgrade_selectors(std::slice::from_ref(&selector));

    assert_eq!(broker.len(), 1);
    assert_eq!(broker[0].id(), selector.id());
    assert_eq!(broker[0].selector(), selector.selector());
    assert_eq!(
        broker[0].version_preference(),
        selector.version_preference()
    );
    assert_eq!(broker[0].outputs(), selector.outputs());
    assert!(selector.attribute().is_some());
    assert!(broker[0].attribute().is_none());
    assert!(matches!(
        broker[0].source_revision(),
        SourceRevision::CurrentChannel
    ));
}

#[test]
fn outdated_attributes_are_exact_and_fail_closed() {
    let result = CommandResult::new(
        "1 package(s) outdated",
        Map::from_iter([("entries".into(), json!([{"package": "hello"}]))]),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        outdated_attributes(&result).unwrap(),
        BTreeSet::from(["hello".to_owned()])
    );

    let malformed = CommandResult::new(
        "invalid",
        Map::from_iter([("entries".into(), json!([{}]))]),
        Vec::new(),
    )
    .unwrap();
    assert!(outdated_attributes(&malformed).is_err());
}

fn prepared_pending_install_fixture() -> (TempDir, StateLayout, u32) {
    let home = TempDir::new().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::symlink_metadata(home.path()).unwrap().uid();
    let layout = StateLayout::initialize(home.path(), &home.path().join("pkg"), uid).unwrap();

    // Fixture: one prepared-but-uncommitted install generation.
    let store_path = format!("/nix/store/{STORE_HASH}-hello-1.0");
    let staging = layout.state_root().join("activations/gen-0001.staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(format!("{store_path}/bin/hello"), staging.join("hello")).unwrap();
    let plan = inspect_staged_activation(
        &staging,
        vec![pkg_core::StorePath::new(&store_path).unwrap()],
    )
    .unwrap();
    let manifest_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "channelSeq": 1,
        "uid": uid,
        "entries": [{
            "id": "sel_hello",
            "selector": "hello",
            "attribute": "hello",
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
            "sel_hello": {
                "attribute": "hello",
                "nixpkgsRev": REVISION,
                "realized": {
                    "storePath": store_path,
                    "deriver": format!("{store_path}.drv"),
                    "outputs": { "out": store_path },
                    "outputsToInstall": ["out"],
                    "system": "x86_64-linux",
                    "narHash": NAR_HASH,
                    "closureNarSize": 42,
                    "pname": "hello",
                    "version": "1.0"
                },
                "lockedAt": "2026-08-09T00:00:01Z",
                "provenance": "cache:official",
                "sigsObserved": ["official-1:fixture"]
            }
        }
    }))
    .unwrap();
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
        "outputs": [{
            "id": "sel_hello",
            "attribute": "hello",
            "nixpkgsRev": REVISION,
            "storePath": store_path,
            "deriver": format!("{store_path}.drv"),
            "outputsToInstall": ["out"],
            "narHash": NAR_HASH,
            "closureNarSize": 42,
            "provenance": "cache:official",
            "pinned": false
        }],
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
    let candidate = CandidateGeneration::new(
        manifest_bytes,
        lock_bytes,
        serde_json::to_vec(&generation).unwrap(),
    )
    .unwrap();
    let identity =
        LeaseIdentity::new("op_fixture", "nonce_fixture", "2026-08-09T00:00:00Z").unwrap();
    let lease = StateLease::try_exclusive(&layout, &identity).unwrap();
    let prepared = PreparedGeneration::prepare(layout.clone(), candidate, plan, lease).unwrap();
    drop(prepared);
    (home, layout, uid)
}

#[test]
fn attestation_failure_reconciles_cancelled_activate_then_discards_with_gc() {
    let (_home, layout, uid) = prepared_pending_install_fixture();

    let broker = InProcessBroker::new().unwrap();
    let activate_broker = broker.clone();
    let (mut activate_server, client) = UnixStream::pair().unwrap();
    let activate_worker = thread::spawn(move || {
        let caller = activate_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut activate_server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Activate)
        );
        let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
        write_response(
            &mut activate_server,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut activate_server);
        assert_eq!(
            request,
            CliBrokerRequest::AttestGenerationRoots(
                handle.clone(),
                GenerationId::new("gen-0001").unwrap()
            )
        );
        let error = caller
            .attest_generation_root_intent(&handle, GenerationId::new("gen-0001").unwrap(), |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err();
        assert_eq!(
            error.code(),
            pkg_nix::BrokerErrorCode::RootPublicationFailed
        );
        write_response(
            &mut activate_server,
            request_id,
            &CliBrokerResponse::GenerationRootAttestationRefused(
                GenerationRootAttestationErrorCode::AttestationFailed,
            ),
        );

        let (_, request) = read_request(&mut activate_server);
        assert_eq!(request, CliBrokerRequest::Cancel(handle.clone()));
        assert_eq!(
            caller.cancel(&handle).unwrap_err().code(),
            pkg_nix::BrokerErrorCode::InvalidAdmissionTransition
        );
        // Lose the error reply after attestation made the handle terminal.
        handle
    });

    let recovery_broker = broker.clone();
    let (mut recovery_server, recovery_client) = UnixStream::pair().unwrap();
    let recovery_worker = thread::spawn(move || {
        let caller = recovery_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut recovery_server);
        let CliBrokerRequest::Poll(activate) = request else {
            panic!("expected Activate status reconciliation");
        };
        let status = caller.poll(&activate).unwrap();
        assert_eq!(status, OperationStatus::Cancelled);
        write_response(
            &mut recovery_server,
            request_id,
            &CliBrokerResponse::Status(status),
        );

        let (request_id, request) = read_request(&mut recovery_server);
        assert_eq!(request, CliBrokerRequest::Begin(BrokerOperationKind::Gc));
        let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
        write_response(
            &mut recovery_server,
            request_id,
            &CliBrokerResponse::Started(gc.clone()),
        );

        let (request_id, request) = read_request(&mut recovery_server);
        assert_eq!(request, CliBrokerRequest::AcquireGc(gc.clone()));
        caller.acquire_gc_wait(&gc).unwrap();
        write_response(
            &mut recovery_server,
            request_id,
            &CliBrokerResponse::GcAdmissionAcquired,
        );

        let (request_id, request) = read_request(&mut recovery_server);
        assert_eq!(
            request,
            CliBrokerRequest::RemoveGenerationRoots(
                gc.clone(),
                GenerationId::new("gen-0001").unwrap()
            )
        );
        caller
            .remove_generation_root_intent(&gc, GenerationId::new("gen-0001").unwrap(), |request| {
                assert_eq!(request.owner_uid(), uid);
                Ok(())
            })
            .unwrap();
        write_response(
            &mut recovery_server,
            request_id,
            &CliBrokerResponse::GenerationRootsRemoved,
        );

        let (request_id, request) = read_request(&mut recovery_server);
        assert_eq!(request, CliBrokerRequest::Complete(gc.clone()));
        caller.complete(&gc).unwrap();
        write_response(
            &mut recovery_server,
            request_id,
            &CliBrokerResponse::Completed,
        );
        let mut eof = [0_u8; 1];
        let _ = recovery_server.read(&mut eof);
        (activate, gc)
    });

    let operations = LocalStateOperations {
        source: layout.clone(),
        broker_state_compatible: true,
    };
    let mut client = BrokerLifecycleClient::from_stream(client);
    let mut recovery_client = Some(BrokerLifecycleClient::from_stream(recovery_client));
    let mut reconnect = || {
        Ok(recovery_client
            .take()
            .expect("recovery opened an unexpected fresh connection"))
    };
    operations
        .recover_pending_install_with(&layout, &mut client, &mut reconnect)
        .unwrap();
    drop(client);
    let activate = activate_worker.join().unwrap();
    let (reconciled, gc) = recovery_worker.join().unwrap();
    assert_eq!(activate, reconciled);

    let probe = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .unwrap();
    assert_eq!(probe.poll(&activate).unwrap(), OperationStatus::Cancelled);
    assert_eq!(probe.poll(&gc).unwrap(), OperationStatus::Completed);
    assert_eq!(layout.current_generation().unwrap(), None);
    let probe_identity =
        LeaseIdentity::new("op_probe", "nonce_probe", "2026-08-21T00:00:00Z").unwrap();
    let probe_lease = StateLease::try_exclusive(&layout, &probe_identity).unwrap();
    assert_eq!(
        pending_install_generation(&layout, &probe_lease).unwrap(),
        None
    );
    assert!(
        !layout
            .state_root()
            .join("generations/gen-0001.json")
            .exists()
    );
}

#[test]
fn generic_resume_failure_preserves_first_error_and_cancels_running_activate() {
    let (_home, layout, uid) = prepared_pending_install_fixture();
    fs::remove_dir_all(layout.state_root().join("activations/gen-0001.staging")).unwrap();

    let broker = InProcessBroker::new().unwrap();
    let server_broker = broker.clone();
    let (mut server, client) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        let caller = server_broker
            .connect(InProcessCallerPeer::authenticated(uid))
            .unwrap();
        let (request_id, request) = read_request(&mut server);
        assert_eq!(
            request,
            CliBrokerRequest::Begin(BrokerOperationKind::Activate)
        );
        let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
        write_response(
            &mut server,
            request_id,
            &CliBrokerResponse::Started(handle.clone()),
        );

        let (request_id, request) = read_request(&mut server);
        assert_eq!(request, CliBrokerRequest::Cancel(handle.clone()));
        assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
        caller.cancel(&handle).unwrap();
        write_response(&mut server, request_id, &CliBrokerResponse::Cancelled);
        let mut eof = [0_u8; 1];
        let _ = server.read(&mut eof);
        handle
    });

    let operations = LocalStateOperations {
        source: layout.clone(),
        broker_state_compatible: true,
    };
    let mut client = BrokerLifecycleClient::from_stream(client);
    let mut reconnect = || -> Result<BrokerLifecycleClient, BrokerClientError> {
        unreachable!("the primary cancellation opened a fresh connection");
    };
    let error = operations
        .recover_pending_install_with(&layout, &mut client, &mut reconnect)
        .unwrap_err();
    drop(client);
    let handle = worker.join().unwrap();

    assert_eq!(error, install_commit_failed());
    let probe = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .unwrap();
    assert_eq!(probe.poll(&handle).unwrap(), OperationStatus::Cancelled);
    let probe_identity =
        LeaseIdentity::new("op_probe", "nonce_probe", "2026-08-21T00:00:00Z").unwrap();
    let probe_lease = StateLease::try_exclusive(&layout, &probe_identity).unwrap();
    assert_eq!(
        pending_install_generation(&layout, &probe_lease).unwrap(),
        Some(GenerationId::new("gen-0001").unwrap())
    );
}

fn scripted_server(
    workers: &mut Vec<thread::JoinHandle<()>>,
    script: impl FnOnce(&mut UnixStream) + Send + 'static,
) -> BrokerLifecycleClient {
    let (mut server, client) = UnixStream::pair().unwrap();
    workers.push(thread::spawn(move || script(&mut server)));
    BrokerLifecycleClient::from_stream(client)
}

#[test]
fn cancel_operation_falls_back_to_a_fresh_connection() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let (server, client) = UnixStream::pair().unwrap();
    drop(server);
    let mut client = BrokerLifecycleClient::from_stream(client);

    let mut workers = Vec::new();
    let mut fresh_clients = VecDeque::new();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            write_response(
                server,
                request_id,
                &CliBrokerResponse::Status(caller.poll(&handle).unwrap()),
            );
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Cancel(handle.clone()));
            caller.cancel(&handle).unwrap();
            write_response(server, request_id, &CliBrokerResponse::Cancelled);
            let mut eof = [0_u8; 1];
            let _ = server.read(&mut eof);
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("cancel fallback opened an unexpected fresh connection"))
    };

    cancel_operation(&mut client, &mut reconnect, handle.clone());
    drop(client);

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn cancel_operation_returns_false_when_fresh_poll_transport_fails() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let (server, client) = UnixStream::pair().unwrap();
    drop(server);
    let mut client = BrokerLifecycleClient::from_stream(client);

    let mut workers = Vec::new();
    let handle_for_poll = handle.clone();
    let mut fresh_client = Some(scripted_server(&mut workers, move |server| {
        let (_, request) = read_request(server);
        assert_eq!(request, CliBrokerRequest::Poll(handle_for_poll));
        // Drop without responding so the exact-handle poll is unreadable.
    }));
    let mut reconnects = 0;
    {
        let mut reconnect = || -> Result<BrokerLifecycleClient, BrokerClientError> {
            reconnects += 1;
            Ok(fresh_client
                .take()
                .expect("cancellation opened an unexpected fresh connection"))
        };

        assert!(!cancel_operation(
            &mut client,
            &mut reconnect,
            handle.clone()
        ));
    }
    assert_eq!(reconnects, 1);
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
    caller.cancel(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn complete_operation_reconciles_completed_on_fresh_connection() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let mut workers = Vec::new();
    let (mut main_server, main_client) = UnixStream::pair().unwrap();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        workers.push(thread::spawn(move || {
            let (_, request) = read_request(&mut main_server);
            assert_eq!(request, CliBrokerRequest::Complete(handle.clone()));
            caller.complete(&handle).unwrap();
            // Drop the completion reply without responding.
        }));
    }

    let mut fresh_clients = VecDeque::new();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            let status = caller.poll(&handle).unwrap();
            write_response(server, request_id, &CliBrokerResponse::Status(status));
            let mut eof = [0_u8; 1];
            let _ = server.read(&mut eof);
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("reconciliation opened an unexpected fresh connection"))
    };

    let mut client = BrokerLifecycleClient::from_stream(main_client);
    complete_operation(&mut client, &mut reconnect, handle.clone()).unwrap();

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
}

#[test]
fn complete_operation_preserves_error_when_reconciled_cancelled() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let mut workers = Vec::new();
    let (mut main_server, main_client) = UnixStream::pair().unwrap();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        workers.push(thread::spawn(move || {
            let (_, request) = read_request(&mut main_server);
            assert_eq!(request, CliBrokerRequest::Complete(handle.clone()));
            caller.cancel(&handle).unwrap();
            // Lose the completion response after the handle is Cancelled.
        }));
    }

    let mut fresh_clients = VecDeque::new();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            let status = caller.poll(&handle).unwrap();
            assert_eq!(status, OperationStatus::Cancelled);
            write_response(server, request_id, &CliBrokerResponse::Status(status));
            let mut eof = [0_u8; 1];
            let _ = server.read(&mut eof);
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("completion reconciliation opened an unexpected fresh connection"))
    };

    let mut client = BrokerLifecycleClient::from_stream(main_client);
    let error = complete_operation(&mut client, &mut reconnect, handle.clone()).unwrap_err();

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        error,
        CommandError::new(
            ExitCode::EngineUnavailable,
            "the managed package service refused the transaction",
            "run `pkg doctor` to inspect managed broker readiness",
        )
    );
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn complete_operation_cancels_after_uncertain_completion() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let mut workers = Vec::new();
    let (mut main_server, main_client) = UnixStream::pair().unwrap();
    {
        let handle = handle.clone();
        workers.push(thread::spawn(move || {
            let (_, request) = read_request(&mut main_server);
            assert_eq!(request, CliBrokerRequest::Complete(handle.clone()));
            // Leave the operation Running and drop the transport.
        }));
    }

    let mut fresh_clients = VecDeque::new();
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            let status = caller.poll(&handle).unwrap();
            assert_eq!(status, OperationStatus::Running);
            write_response(server, request_id, &CliBrokerResponse::Status(status));
            // Read the Cancel request, then drop without responding so the
            // cancel transport fails and the fallback opens a second fresh
            // connection.
            let (_, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Cancel(handle));
        }));
    }
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            let status = caller.poll(&handle).unwrap();
            assert_eq!(status, OperationStatus::Running);
            write_response(server, request_id, &CliBrokerResponse::Status(status));
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Cancel(handle.clone()));
            caller.cancel(&handle).unwrap();
            write_response(server, request_id, &CliBrokerResponse::Cancelled);
            let mut eof = [0_u8; 1];
            let _ = server.read(&mut eof);
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("reconciliation opened an unexpected fresh connection"))
    };

    let mut client = BrokerLifecycleClient::from_stream(main_client);
    let error = complete_operation(&mut client, &mut reconnect, handle.clone()).unwrap_err();

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(error.exit_code(), ExitCode::EngineUnavailable);
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn complete_operation_retries_cancel_after_poll_failure() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    let mut workers = Vec::new();
    let (mut main_server, main_client) = UnixStream::pair().unwrap();
    {
        let handle = handle.clone();
        workers.push(thread::spawn(move || {
            let (_, request) = read_request(&mut main_server);
            assert_eq!(request, CliBrokerRequest::Complete(handle.clone()));
            // Leave the operation Running and drop the transport.
        }));
    }

    let mut fresh_clients = VecDeque::new();
    {
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (_, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle));
            // Drop without responding: the poll transport fails.
        }));
    }
    {
        let caller = caller.clone();
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            let status = caller.poll(&handle).unwrap();
            assert_eq!(status, OperationStatus::Running);
            write_response(server, request_id, &CliBrokerResponse::Status(status));
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Cancel(handle.clone()));
            caller.cancel(&handle).unwrap();
            write_response(server, request_id, &CliBrokerResponse::Cancelled);
            let mut eof = [0_u8; 1];
            let _ = server.read(&mut eof);
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("reconciliation opened an unexpected fresh connection"))
    };

    let mut client = BrokerLifecycleClient::from_stream(main_client);
    let error = complete_operation(&mut client, &mut reconnect, handle.clone()).unwrap_err();

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(error.exit_code(), ExitCode::EngineUnavailable);
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn cancel_operation_rejects_running_after_failed_reconciliation() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(501))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();

    // The primary connection is already dead, so its cancel fails first.
    let (server, client) = UnixStream::pair().unwrap();
    drop(server);
    let mut client = BrokerLifecycleClient::from_stream(client);

    let mut workers = Vec::new();
    let mut fresh_clients = VecDeque::new();
    {
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle.clone()));
            write_response(
                server,
                request_id,
                &CliBrokerResponse::Status(OperationStatus::Running),
            );
            let (_, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Cancel(handle));
            // Drop without responding so the fresh cancellation also fails.
        }));
    }
    {
        let handle = handle.clone();
        fresh_clients.push_back(scripted_server(&mut workers, move |server| {
            let (request_id, request) = read_request(server);
            assert_eq!(request, CliBrokerRequest::Poll(handle));
            write_response(
                server,
                request_id,
                &CliBrokerResponse::Status(OperationStatus::Running),
            );
        }));
    }
    let mut reconnect = move || -> Result<BrokerLifecycleClient, BrokerClientError> {
        Ok(fresh_clients
            .pop_front()
            .expect("cancel fallback opened an unexpected fresh connection"))
    };

    assert!(!cancel_operation(
        &mut client,
        &mut reconnect,
        handle.clone()
    ));

    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
    caller.cancel(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}
