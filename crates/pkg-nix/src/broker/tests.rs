//! Tests for the `broker` module.

use super::*;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

use pkg_channel::BuildMode;
use pkg_core::{
    AttributePath, ChannelSequence, NarHash, NixpkgsRevision, OutputName, PackageVersion,
    PolicyVersion, SelectorId, SelectorInput, StorePath, System, VersionPreference,
};

use crate::{
    ApprovalJournalError, ApprovalJournalRecord, BuildOutput, BuildOutputProvenance,
    BuildPlanTarget, BuildReadiness, BuildRequest, BuildStatus, CacheClassification,
    DerivationPath, DerivationPlanReport, EvaluateDerivationRequest, EvaluatedDerivation, GcReport,
    GenerationId, InProcessHelper, InProcessPeer, MaintenanceAdapter, NarIntegrity,
    NixAdapterError, NixVersion, PathInfoReport, PathVerifyResult, ResourceSnapshot, RootName,
    RootRef, RootSetEntry, SubstituteReport, TrustStatus, VerifyReport, VerifyRequest, VersionInfo,
};

const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

#[derive(Default)]
struct Journal {
    rows: Mutex<Vec<ApprovalJournalRecord>>,
}

impl ApprovalJournal for Journal {
    fn record(&self, record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError> {
        self.rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record.clone());
        Ok(())
    }
}

struct FailingJournal;

impl ApprovalJournal for FailingJournal {
    fn record(&self, _record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError> {
        Err(ApprovalJournalError::new())
    }
}

struct ExecutionProbe;

impl ResourceProbe for ExecutionProbe {
    fn measure(&self) -> Result<ResourceSnapshot, BuildEngineError> {
        Ok(ResourceSnapshot {
            free_bytes: 10_000,
            load_average: 1.0,
        })
    }
}

struct ExecutionAdapter {
    calls: AtomicUsize,
    entered: Option<mpsc::Sender<()>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

struct RetainedReplanner {
    plan: BuildPlan,
    calls: AtomicUsize,
    refuse: bool,
}

impl RetainedReplanner {
    fn new(plan: BuildPlan, refuse: bool) -> Self {
        Self {
            plan,
            calls: AtomicUsize::new(0),
            refuse,
        }
    }
}

impl TrustedBuildReplanner for RetainedReplanner {
    fn replan(&self) -> Result<BuildPlan, TrustedReplanError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            Err(TrustedReplanError)
        } else {
            Ok(self.plan.clone())
        }
    }
}

impl ExecutionAdapter {
    fn immediate() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            entered: None,
            release: Mutex::new(None),
        }
    }

    fn blocking(entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            entered: Some(entered),
            release: Mutex::new(Some(release)),
        }
    }
}

impl NixAdapter for ExecutionAdapter {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }

    fn evaluate_derivation(
        &self,
        _: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        PathInfoReport::new(
            path.clone(),
            NarHash::new(NAR_HASH).unwrap(),
            Vec::new(),
            Vec::new(),
            Some(
                DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello-1.0.drv"))
                    .unwrap(),
            ),
            0,
            0,
        )
    }

    fn substitute(&self, _: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }

    fn build(&self, _: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = &self.entered {
            entered.send(()).map_err(|_| NixAdapterError::Unavailable)?;
        }
        if let Some(release) = self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            release.recv().map_err(|_| NixAdapterError::Unavailable)?;
        }
        BuildReport::new(
            BuildStatus::Built,
            vec![BuildOutput::new(
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
                BuildOutputProvenance::LocalBuild,
            )],
        )
    }

    fn verify(&self, request: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        let mut results = request
            .paths()
            .iter()
            .cloned()
            .map(|path| PathVerifyResult::new(path, NarIntegrity::Intact, TrustStatus::Trusted))
            .collect::<Vec<_>>();
        results.push(PathVerifyResult::new(
            StorePath::new(&format!("/nix/store/{STORE_HASH}-dependency-1.0")).unwrap(),
            NarIntegrity::Intact,
            TrustStatus::Trusted,
        ));
        VerifyReport::new(results)
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
}

fn build_plan(document_byte: u8) -> BuildPlan {
    let derivation =
        DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello-1.0.drv")).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(
        OutputName::new("out").unwrap(),
        StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
    );
    let evaluated = EvaluatedDerivation::new(
        derivation.clone(),
        "hello-1.0".to_owned(),
        System::X8664Linux,
        outputs,
        Digest::from_bytes([document_byte; 32]),
        false,
    )
    .unwrap();
    let report = DerivationPlanReport::new(
        4,
        derivation.clone(),
        vec![OutputName::new("out").unwrap()],
        vec![evaluated],
        Digest::from_bytes([document_byte.wrapping_add(1); 32]),
        "hello".to_owned(),
        PackageVersion::new("1.0"),
    )
    .unwrap();
    BuildPlan::new(
        &NixVersion::new("2.34.8").unwrap(),
        Digest::from_bytes([3; 32]),
        PolicyVersion::from_u64(7).unwrap(),
        ChannelSequence::from_u64(42).unwrap(),
        &NixpkgsRevision::new(REVISION).unwrap(),
        &NarHash::new(NAR_HASH).unwrap(),
        System::X8664Linux,
        System::X8664Linux,
        BuildMode::AllowWithGates,
        vec![BuildPlanTarget::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            AttributePath::new("hello").unwrap(),
            VersionPreference::Any,
            pkg_core::OutputSelection::default_selection(),
            pkg_core::SourceRevision::CurrentChannel,
            report,
        )],
        vec![derivation],
        CacheClassification::new(Digest::from_bytes([4; 32]), 2, 1, 100, 200).unwrap(),
        BuildReadiness::new(true, false, true, true, true),
        4,
    )
    .unwrap()
}

fn built_root_set(uid: u32) -> RootSet {
    RootSet::new(
        uid,
        GenerationId::new("gen-0001").unwrap(),
        vec![RootSetEntry::new(
            RootName::new("hello").unwrap(),
            StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
        )],
    )
    .unwrap()
}

fn cache_install_evidence() -> InstallEvidence {
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
                    "signatures": ["cache.nixos.org-1:AAAA"],
                    "references": [],
                    "deriver": derivation,
                    "narSize": 20,
                    "closureSize": 42,
                    "provenance": "cacheSigned"
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn caller_identity_is_transport_bound() {
    let broker = InProcessBroker::new().unwrap();
    assert_eq!(
        broker
            .connect(InProcessCallerPeer::with_claim(1001, 1002))
            .unwrap_err()
            .code(),
        BrokerErrorCode::UnauthenticatedCaller
    );
    assert!(
        broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .is_ok()
    );
}

#[test]
fn build_and_gc_admission_release_on_cancel_and_disconnect() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let build = caller.begin(BrokerOperationKind::Build).unwrap();
    caller.acquire_build(&build).unwrap();
    caller.acquire_gc_inhibit(&build).unwrap();
    let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
    assert_eq!(
        caller.acquire_gc(&gc).unwrap_err().code(),
        BrokerErrorCode::AdmissionBusy
    );
    caller.cancel(&build).unwrap();
    caller.acquire_gc(&gc).unwrap();
    caller.disconnect().unwrap();
    let snapshot = broker.admission_snapshot();
    assert!(!snapshot.build_held());
    assert!(!snapshot.gc_held());
    assert_eq!(snapshot.gc_inhibitor_count(), 0);
}

#[test]
fn gc_admission_waits_for_realize_to_root_inhibitors() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let build = caller.begin(BrokerOperationKind::Build).unwrap();
    caller.acquire_build(&build).unwrap();
    caller.acquire_gc_inhibit(&build).unwrap();
    let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
    let (tx, rx) = mpsc::channel();
    let waiting_caller = caller.clone();
    let waiting_gc = gc.clone();
    let waiter = thread::spawn(move || {
        tx.send(waiting_caller.acquire_gc_wait(&waiting_gc))
            .unwrap();
    });

    assert!(rx.recv_timeout(Duration::from_millis(75)).is_err());
    caller.cancel(&build).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    waiter.join().unwrap();
    assert!(broker.admission_snapshot().gc_held());
    caller.complete(&gc).unwrap();
}

#[test]
fn build_admission_waits_fifo_and_lifecycle_cancel_removes_waiter() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let first = caller.begin(BrokerOperationKind::Build).unwrap();
    let second = caller.begin(BrokerOperationKind::Build).unwrap();
    let third = caller.begin(BrokerOperationKind::Repair).unwrap();
    caller.acquire_build(&first).unwrap();

    let (second_tx, second_rx) = mpsc::channel();
    let second_caller = caller.clone();
    let second_handle = second.clone();
    let second_waiter = thread::spawn(move || {
        let result = second_caller
            .acquire_build_wait(&second_handle, &CancellationToken::default())
            .map_err(BrokerError::code);
        let _ = second_tx.send(result);
    });
    wait_for_build_queue(&broker, 1);

    let (third_tx, third_rx) = mpsc::channel();
    let third_caller = caller.clone();
    let third_handle = third.clone();
    let third_waiter = thread::spawn(move || {
        let result = third_caller
            .acquire_build_wait(&third_handle, &CancellationToken::default())
            .map_err(BrokerError::code);
        let _ = third_tx.send(result);
    });
    wait_for_build_queue(&broker, 2);

    caller.cancel(&first).unwrap();
    assert_eq!(
        second_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(())
    );
    assert!(matches!(
        third_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    caller.cancel(&third).unwrap();
    assert_eq!(
        third_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(BrokerErrorCode::AdmissionCancelled)
    );
    caller.cancel(&second).unwrap();
    second_waiter.join().unwrap();
    third_waiter.join().unwrap();

    let fourth = caller.begin(BrokerOperationKind::Build).unwrap();
    caller.acquire_build(&fourth).unwrap();
    let fifth = caller.begin(BrokerOperationKind::Build).unwrap();
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(
        caller
            .acquire_build_wait(&fifth, &cancelled)
            .unwrap_err()
            .code(),
        BrokerErrorCode::AdmissionCancelled
    );
    assert_eq!(broker.build_gate.waiting_count(), 0);
    caller.cancel(&fourth).unwrap();
    caller.cancel(&fifth).unwrap();
}

#[test]
fn queued_build_reservation_blocks_gc_during_handoff() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let holder = caller.begin(BrokerOperationKind::Build).unwrap();
    let queued = caller.begin(BrokerOperationKind::Repair).unwrap();
    let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
    caller.acquire_build(&holder).unwrap();
    assert!(!broker.build_gate.enqueue(&queued).unwrap());

    caller.cancel(&holder).unwrap();
    assert!(!broker.build_gate.held());
    assert_eq!(broker.build_gate.waiting_count(), 1);
    assert_eq!(
        caller.acquire_gc(&gc).unwrap_err().code(),
        BrokerErrorCode::AdmissionBusy
    );

    caller.cancel(&queued).unwrap();
    caller.acquire_gc(&gc).unwrap();
    caller.cancel(&gc).unwrap();
}

fn wait_for_build_queue(broker: &InProcessBroker, expected: usize) {
    for _ in 0..200 {
        if broker.build_gate.waiting_count() == expected {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("build admission queue did not reach expected size");
}

fn operation_cancellation(
    broker: &InProcessBroker,
    handle: &OperationHandle,
) -> Arc<CancellationToken> {
    Arc::clone(
        &broker
            .lock()
            .operations
            .get(handle)
            .expect("test operation must exist")
            .cancellation,
    )
}

#[test]
fn terminal_lifecycle_signals_private_operation_cancellation() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();

    let completed = caller.begin(BrokerOperationKind::Resolve).unwrap();
    let completed_token = operation_cancellation(&broker, &completed);
    caller.complete(&completed).unwrap();
    assert!(completed_token.is_cancelled());

    let cancelled = caller.begin(BrokerOperationKind::Build).unwrap();
    let cancelled_token = operation_cancellation(&broker, &cancelled);
    caller.cancel(&cancelled).unwrap();
    assert!(cancelled_token.is_cancelled());

    let disconnected = caller.begin(BrokerOperationKind::Acquire).unwrap();
    let disconnected_token = operation_cancellation(&broker, &disconnected);
    caller.disconnect().unwrap();
    assert!(disconnected_token.is_cancelled());

    let fresh = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let expired = fresh
        .begin_with_deadline(BrokerOperationKind::Build, Instant::now())
        .unwrap();
    let expired_token = operation_cancellation(&broker, &expired);
    assert_eq!(
        fresh.poll(&expired).unwrap_err().code(),
        BrokerErrorCode::OperationExpired
    );
    assert!(expired_token.is_cancelled());

    let restarted = fresh.begin(BrokerOperationKind::Build).unwrap();
    let restarted_token = operation_cancellation(&broker, &restarted);
    broker.restart().unwrap();
    assert!(restarted_token.is_cancelled());
}

#[test]
fn handles_are_uid_bound_expiring_and_restart_bound() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let other = broker
        .connect(InProcessCallerPeer::authenticated(1002))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    assert_eq!(
        other.poll(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidOperationHandle
    );
    let expired = caller
        .begin_with_deadline(BrokerOperationKind::Doctor, Instant::now())
        .unwrap();
    assert_eq!(
        caller.poll(&expired).unwrap_err().code(),
        BrokerErrorCode::OperationExpired
    );
    broker.restart().unwrap();
    assert_eq!(
        caller.poll(&handle).unwrap_err().code(),
        BrokerErrorCode::SessionRestarted
    );
    assert_eq!(broker.admission_snapshot().operation_count(), 0);
}

#[test]
fn private_build_plan_approval_is_uid_bound_exact_and_journaled() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let other = broker
        .connect(InProcessCallerPeer::authenticated(1002))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    let journal = Journal::default();
    let preview = caller.prepare_build(&handle, plan).unwrap();
    assert_eq!(preview.build_plan_digest().len(), 71);

    assert_eq!(
        other
            .approve_build(
                &handle,
                digest,
                ApprovalSource::Interactive,
                "2026-08-11T00:00:00Z",
                &journal,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidOperationHandle
    );
    assert_eq!(
        caller
            .approve_build(
                &handle,
                Digest::from_bytes([9; 32]),
                ApprovalSource::Interactive,
                "2026-08-11T00:00:00Z",
                &journal,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalMismatch
    );
    assert_eq!(
        caller
            .approve_build(
                &handle,
                digest,
                ApprovalSource::Interactive,
                "2026-08-11T00:00:00Z",
                &FailingJournal,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalUnavailable
    );
    assert_eq!(broker.build_engine.approval_count(), 0);
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::AssumeYes,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    assert_eq!(
        caller
            .approve_build(
                &handle,
                digest,
                ApprovalSource::Interactive,
                "2026-08-11T00:00:01Z",
                &journal,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalUnavailable
    );
    let rows = journal
        .rows
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].build_plan_digest(), digest);
    assert_eq!(rows[0].source(), ApprovalSource::AssumeYes);
    drop(rows);
    assert_eq!(broker.build_engine.approval_count(), 1);
    assert_eq!(
        caller
            .prepare_build(&handle, build_plan(2))
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
}

#[test]
fn repair_approval_is_operation_bound_journaled_and_single_use() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    let digest = pkg_core::state::body_digest(b"repair plan");
    let policy = PolicyVersion::from_u64(7).unwrap();
    let journal = Journal::default();
    let receipt = caller
        .approve_repair_subject(
            &handle,
            digest,
            policy,
            ApprovalSource::Interactive,
            "unix-ms:1",
            &journal,
        )
        .unwrap();

    caller
        .consume_repair_subject(&handle, &receipt, digest, policy)
        .unwrap();
    assert!(
        caller
            .consume_repair_subject(&handle, &receipt, digest, policy)
            .is_err()
    );
    assert_eq!(journal.rows.lock().unwrap().len(), 1);
    caller.complete(&handle).unwrap();
}

#[test]
fn broker_executes_private_receipt_once_and_retains_gc_inhibit_for_rooting() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let journal = Journal::default();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    caller.prepare_build(&handle, plan.clone()).unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::AssumeYes,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    let adapter = ExecutionAdapter::immediate();
    assert_eq!(
        caller.install_evidence(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );

    let report = caller
        .execute_build(
            &handle,
            digest,
            || Ok(plan.clone()),
            VolatileBuildEstimate::new(100),
            &ExecutionProbe,
            &adapter,
        )
        .unwrap();
    assert_eq!(report.status(), BuildStatus::Built);
    let evidence = caller.install_evidence(&handle).unwrap();
    assert_eq!(evidence.channel_sequence().get().get(), 42);
    assert_eq!(evidence.policy_version().get().get(), 7);
    assert_eq!(evidence.targets().len(), 1);
    assert_eq!(evidence.targets()[0].selector().as_str(), "hello");
    assert_eq!(evidence.targets()[0].acquired().len(), 1);
    let debug = format!("{evidence:?}");
    assert!(!debug.contains("/nix/") && !debug.contains("hello"));
    assert_eq!(
        evidence.targets()[0].acquired()[0].provenance(),
        BuildOutputProvenance::LocalBuild
    );
    assert_eq!(
        InstallEvidence::from_json_bytes(&evidence.to_json_bytes().unwrap()).unwrap(),
        evidence
    );
    let encoded_evidence = String::from_utf8(evidence.to_json_bytes().unwrap()).unwrap();
    let wrong_schema = encoded_evidence.replacen("\"schemaVersion\":1", "\"schemaVersion\":2", 1);
    assert!(InstallEvidence::from_json_bytes(wrong_schema.as_bytes()).is_err());
    let extended = encoded_evidence.replacen('{', "{\"futureField\":true,", 1);
    assert!(InstallEvidence::from_json_bytes(extended.as_bytes()).is_err());
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    assert_eq!(broker.build_engine.approval_count(), 0);
    let admitted = broker.admission_snapshot();
    assert!(admitted.build_held());
    assert_eq!(admitted.gc_inhibitor_count(), 1);
    assert_eq!(
        caller
            .execute_build(
                &handle,
                digest,
                || Ok(plan),
                VolatileBuildEstimate::new(100),
                &ExecutionProbe,
                &adapter,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalUnavailable
    );

    assert_eq!(
        caller.complete(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(
        caller
            .publish_built_root_set(&handle, &built_root_set(1002), |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    let unrelated_roots = RootSet::new(
        1001,
        GenerationId::new("gen-0002").unwrap(),
        vec![RootSetEntry::new(
            RootName::new("other").unwrap(),
            StorePath::new(&format!("/nix/store/{STORE_HASH}-other-1.0")).unwrap(),
        )],
    )
    .unwrap();
    assert_eq!(
        caller
            .publish_built_root_set(&handle, &unrelated_roots, |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    let roots_with_extra = RootSet::new(
        1001,
        GenerationId::new("gen-0002").unwrap(),
        vec![
            RootSetEntry::new(
                RootName::new("demo-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            ),
            RootSetEntry::new(
                RootName::new("other-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-other-1.0")).unwrap(),
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        caller
            .publish_built_root_set(&handle, &roots_with_extra, |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    let duplicate_aliases = RootSet::new(
        1001,
        GenerationId::new("gen-0002").unwrap(),
        vec![
            RootSetEntry::new(
                RootName::new("demo-a").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            ),
            RootSetEntry::new(
                RootName::new("demo-b").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        caller
            .publish_built_root_set(&handle, &duplicate_aliases, |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(1001);
    let intent = RootSetIntent::from_source(
        GenerationId::new("gen-0001").unwrap(),
        GenerationId::new("gen-0002").unwrap(),
        vec![
            RootSetEntry::new(
                RootName::new("demo-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            ),
            RootSetEntry::new(
                RootName::new("retained-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-retained-1.0")).unwrap(),
            ),
        ],
        vec![RootName::new("demo-out").unwrap()],
    )
    .unwrap();
    caller
        .publish_built_root_intent(&handle, intent, |request| {
            assert_eq!(request.source_generation().unwrap().as_str(), "gen-0001");
            assert_eq!(request.added_names().len(), 1);
            maintenance.publish_root_set(request.root_set())
        })
        .unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    assert_eq!(
        caller.install_evidence(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    let released = broker.admission_snapshot();
    assert!(!released.build_held());
    assert_eq!(released.gc_inhibitor_count(), 0);
}

#[test]
fn cache_hit_retains_gc_inhibition_until_exact_roots_are_published() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
    let evidence = cache_install_evidence();

    assert_eq!(
        caller
            .acquire_cache_install(&handle, || {
                Ok(CacheInstallAttempt::Acquired(evidence.clone()))
            })
            .unwrap(),
        CacheInstallOutcome::Acquired
    );
    assert_eq!(caller.install_evidence(&handle).unwrap(), evidence);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    assert_eq!(
        caller.complete(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );

    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(1001);
    caller
        .publish_built_root_set(&handle, &built_root_set(1001), |roots| {
            maintenance.publish_root_set(roots)
        })
        .unwrap();

    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn cache_miss_releases_gc_inhibition_and_permits_completion() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();

    assert_eq!(
        caller
            .acquire_cache_install(&handle, || Ok(CacheInstallAttempt::BuildRequired))
            .unwrap(),
        CacheInstallOutcome::BuildRequired
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    assert_eq!(
        caller.install_evidence(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    caller.complete(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
}

#[test]
fn cache_failure_has_a_distinct_code_and_releases_gc_inhibition() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();

    assert_eq!(
        caller
            .acquire_cache_install(&handle, || Err(()))
            .unwrap_err()
            .code(),
        BrokerErrorCode::CacheAcquisitionFailed
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    caller.complete(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
}

#[test]
fn cancellation_during_cache_acquisition_defers_inhibitor_release() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
    let worker_caller = caller.clone();
    let worker_handle = handle.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        worker_caller.acquire_cache_install(&worker_handle, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(CacheInstallAttempt::Acquired(cache_install_evidence()))
        })
    });

    entered_rx.recv().unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    caller.cancel(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    release_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err().code(),
        BrokerErrorCode::AdmissionCancelled
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn cancellation_during_root_publication_defers_admission_release() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    caller.prepare_build(&handle, plan.clone()).unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::AssumeYes,
            "2026-08-11T00:00:00Z",
            &Journal::default(),
        )
        .unwrap();
    caller
        .execute_build(
            &handle,
            digest,
            || Ok(plan),
            VolatileBuildEstimate::new(100),
            &ExecutionProbe,
            &ExecutionAdapter::immediate(),
        )
        .unwrap();

    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(1001);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let rooting_caller = caller.clone();
    let rooting_handle = handle.clone();
    let worker = thread::spawn(move || {
        rooting_caller.publish_built_root_set(&rooting_handle, &built_root_set(1001), |roots| {
            entered_tx
                .send(())
                .map_err(|_| MaintenanceError::backend_failure())?;
            release_rx
                .recv()
                .map_err(|_| MaintenanceError::backend_failure())?;
            maintenance.publish_root_set(roots)
        })
    });
    entered_rx.recv().unwrap();
    caller.cancel(&handle).unwrap();
    let protected = broker.admission_snapshot();
    assert!(protected.build_held());
    assert_eq!(protected.gc_inhibitor_count(), 1);

    release_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err().code(),
        BrokerErrorCode::AdmissionCancelled
    );
    let released = broker.admission_snapshot();
    assert!(!released.build_held());
    assert_eq!(released.gc_inhibitor_count(), 0);
}

#[test]
fn mismatched_root_receipt_is_terminal_and_releases_admission() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    caller.prepare_build(&handle, plan.clone()).unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::AssumeYes,
            "2026-08-11T00:00:00Z",
            &Journal::default(),
        )
        .unwrap();
    caller
        .execute_build(
            &handle,
            digest,
            || Ok(plan),
            VolatileBuildEstimate::new(100),
            &ExecutionProbe,
            &ExecutionAdapter::immediate(),
        )
        .unwrap();

    assert_eq!(
        caller
            .publish_built_root_set(&handle, &built_root_set(1001), |_| {
                Ok(RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
                    1,
                    Digest::from_bytes([0xff; 32]),
                ))
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::RootPublicationFailed
    );
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    let released = broker.admission_snapshot();
    assert!(!released.build_held());
    assert_eq!(released.gc_inhibitor_count(), 0);
}

#[test]
fn root_transition_injects_uid_and_holds_inhibit_until_state_commit() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
    let intent = RootSetTransitionIntent::new(
        GenerationId::new("gen-0007").unwrap(),
        GenerationId::new("gen-0008").unwrap(),
        vec![RootName::new("ripgrep-out").unwrap()],
    )
    .unwrap();

    let report = caller
        .transition_root_intent(&handle, intent, |request| {
            assert_eq!(request.owner_uid(), 1001);
            assert_eq!(request.source_generation().as_str(), "gen-0007");
            assert_eq!(request.destination_generation().as_str(), "gen-0008");
            assert_eq!(request.retained_names()[0].as_str(), "ripgrep-out");
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                    Digest::from_bytes([0x31; 32]),
                ),
                request.retained_names().to_vec(),
                Digest::from_bytes([0x31; 32]),
            )
        })
        .unwrap();
    assert_eq!(report.root_set().entry_count(), 1);
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.complete(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn root_attestation_injects_uid_and_holds_inhibit_until_recovery_commit() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
    let report = caller
        .attest_generation_root_intent(&handle, GenerationId::new("gen-0007").unwrap(), |request| {
            assert_eq!(request.owner_uid(), 1001);
            assert_eq!(request.generation().as_str(), "gen-0007");
            Ok(RootSetReport::new(
                RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
                1,
                Digest::from_bytes([0x35; 32]),
            ))
        })
        .unwrap();
    assert_eq!(report.mapping_digest(), Digest::from_bytes([0x35; 32]));
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.complete(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn successful_root_transition_is_not_compensated_by_later_cancellation() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
    let intent = RootSetTransitionIntent::new(
        GenerationId::new("gen-0007").unwrap(),
        GenerationId::new("gen-0008").unwrap(),
        vec![RootName::new("ripgrep-out").unwrap()],
    )
    .unwrap();
    let privileged_commits = AtomicUsize::new(0);
    caller
        .transition_root_intent(&handle, intent, |request| {
            privileged_commits.fetch_add(1, Ordering::SeqCst);
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                    Digest::from_bytes([0x41; 32]),
                ),
                request.retained_names().to_vec(),
                Digest::from_bytes([0x41; 32]),
            )
        })
        .unwrap();
    caller.cancel(&handle).unwrap();
    assert_eq!(privileged_commits.load(Ordering::SeqCst), 1);
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn root_transition_requires_activate_and_helper_failure_is_terminal() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let intent = || {
        RootSetTransitionIntent::new(
            GenerationId::new("gen-0007").unwrap(),
            GenerationId::new("gen-0008").unwrap(),
            vec![RootName::new("ripgrep-out").unwrap()],
        )
        .unwrap()
    };
    let wrong_handle = caller.begin(BrokerOperationKind::Resolve).unwrap();
    let calls = AtomicUsize::new(0);
    assert_eq!(
        caller
            .transition_root_intent(&wrong_handle, intent(), |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
    assert_eq!(
        caller
            .transition_root_intent(&handle, intent(), |_| {
                Err(MaintenanceError::backend_failure())
            })
            .unwrap_err()
            .code(),
        BrokerErrorCode::RootPublicationFailed
    );
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn cancellation_during_root_transition_defers_inhibit_release() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
    let intent = RootSetTransitionIntent::new(
        GenerationId::new("gen-0007").unwrap(),
        GenerationId::new("gen-0008").unwrap(),
        vec![RootName::new("ripgrep-out").unwrap()],
    )
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_caller = caller.clone();
    let worker_handle = handle.clone();
    let worker = thread::spawn(move || {
        worker_caller.transition_root_intent(&worker_handle, intent, |request| {
            entered_tx
                .send(())
                .map_err(|_| MaintenanceError::backend_failure())?;
            release_rx
                .recv()
                .map_err(|_| MaintenanceError::backend_failure())?;
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new(&format!(
                        "/nix/var/nix/gcroots/pkg/users/{}/{}",
                        request.owner_uid(),
                        request.destination_generation().as_str()
                    ))
                    .unwrap(),
                    request.retained_names().len(),
                    Digest::from_bytes([0x32; 32]),
                ),
                request.retained_names().to_vec(),
                Digest::from_bytes([0x32; 32]),
            )
        })
    });
    entered_rx.recv().unwrap();
    caller.cancel(&handle).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    release_tx.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap().unwrap_err().code(),
        BrokerErrorCode::AdmissionCancelled
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn dispatcher_execution_uses_only_the_replanner_retained_before_approval() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let journal = Journal::default();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    let replanner = Arc::new(RetainedReplanner::new(plan.clone(), false));
    caller
        .prepare_build_with_replanner_and_estimates(
            &handle,
            plan,
            &crate::BuildPreviewEstimates::new(None, Some(100), None).unwrap(),
            replanner.clone(),
        )
        .unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    let adapter = ExecutionAdapter::immediate();

    assert_eq!(
        caller
            .execute_prepared_build(&handle, digest, &ExecutionProbe, &adapter,)
            .unwrap()
            .status(),
        BuildStatus::Built
    );
    assert_eq!(replanner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn dispatcher_execution_refuses_an_unavailable_preparation_estimate() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    let preview = caller
        .prepare_build_with_replanner(
            &handle,
            plan.clone(),
            Arc::new(RetainedReplanner::new(plan, false)),
        )
        .unwrap();
    assert_eq!(caller.build_preview(&handle).unwrap(), preview);
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &Journal::default(),
        )
        .unwrap();
    let adapter = ExecutionAdapter::immediate();

    assert_eq!(
        caller
            .execute_prepared_build(&handle, digest, &ExecutionProbe, &adapter)
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildResourcePreflightFailed
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    caller.cancel(&handle).unwrap();
    assert_eq!(broker.build_engine.approval_count(), 0);
}

#[test]
fn refused_retained_replan_consumes_approval_before_adapter_build() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    caller
        .prepare_build_with_replanner_and_estimates(
            &handle,
            plan.clone(),
            &crate::BuildPreviewEstimates::new(None, Some(100), None).unwrap(),
            Arc::new(RetainedReplanner::new(plan, true)),
        )
        .unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &Journal::default(),
        )
        .unwrap();
    let adapter = ExecutionAdapter::immediate();

    assert_eq!(
        caller
            .execute_prepared_build(&handle, digest, &ExecutionProbe, &adapter,)
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalInvalidated
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert_eq!(broker.build_engine.approval_count(), 0);
}

#[test]
fn lifecycle_cancel_during_build_defers_admission_release_until_adapter_returns() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let journal = Journal::default();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let plan = build_plan(1);
    let digest = plan.digest().unwrap();
    caller.prepare_build(&handle, plan.clone()).unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let adapter = Arc::new(ExecutionAdapter::blocking(entered_tx, release_rx));
    let executing_caller = caller.clone();
    let executing_handle = handle.clone();
    let executing_adapter = Arc::clone(&adapter);
    let execution = thread::spawn(move || {
        executing_caller
            .execute_build(
                &executing_handle,
                digest,
                || Ok(plan),
                VolatileBuildEstimate::new(100),
                &ExecutionProbe,
                executing_adapter.as_ref(),
            )
            .map_err(BrokerError::code)
    });
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    caller.cancel(&handle).unwrap();
    let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
    assert_eq!(
        caller.acquire_gc(&gc).unwrap_err().code(),
        BrokerErrorCode::AdmissionBusy
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        execution.join().unwrap(),
        Err(BrokerErrorCode::AdmissionCancelled)
    );
    caller.acquire_gc(&gc).unwrap();
    caller.cancel(&gc).unwrap();
}

#[test]
fn admission_replan_failure_consumes_approval_and_releases_every_gate() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let journal = Journal::default();
    let handle = caller.begin(BrokerOperationKind::Build).unwrap();
    let approved = build_plan(1);
    let digest = approved.digest().unwrap();
    caller.prepare_build(&handle, approved).unwrap();
    caller
        .approve_build(
            &handle,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    let adapter = ExecutionAdapter::immediate();
    assert_eq!(
        caller
            .execute_build(
                &handle,
                digest,
                || Ok(build_plan(2)),
                VolatileBuildEstimate::new(100),
                &ExecutionProbe,
                &adapter,
            )
            .unwrap_err()
            .code(),
        BrokerErrorCode::BuildApprovalInvalidated
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert_eq!(broker.build_engine.approval_count(), 0);
    let snapshot = broker.admission_snapshot();
    assert!(!snapshot.build_held());
    assert_eq!(snapshot.gc_inhibitor_count(), 0);
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
}

#[test]
fn private_build_plan_is_invalidated_by_cancel_disconnect_and_restart() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let journal = Journal::default();

    let wrong_kind = caller.begin(BrokerOperationKind::Acquire).unwrap();
    assert_eq!(
        caller
            .prepare_build(&wrong_kind, build_plan(0))
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );

    let expired = caller
        .begin_with_deadline(BrokerOperationKind::Build, Instant::now())
        .unwrap();
    assert_eq!(
        caller
            .prepare_build(&expired, build_plan(0))
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidOperationHandle
    );

    let cancelled = caller.begin(BrokerOperationKind::Build).unwrap();
    let cancelled_plan = build_plan(1);
    let cancelled_digest = cancelled_plan.digest().unwrap();
    caller.prepare_build(&cancelled, cancelled_plan).unwrap();
    caller
        .approve_build(
            &cancelled,
            cancelled_digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:00Z",
            &journal,
        )
        .unwrap();
    caller.cancel(&cancelled).unwrap();
    assert_eq!(broker.build_engine.approval_count(), 0);

    let disconnected = caller.begin(BrokerOperationKind::Build).unwrap();
    let disconnected_plan = build_plan(2);
    let disconnected_digest = disconnected_plan.digest().unwrap();
    caller
        .prepare_build(&disconnected, disconnected_plan)
        .unwrap();
    caller
        .approve_build(
            &disconnected,
            disconnected_digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:01Z",
            &journal,
        )
        .unwrap();
    caller.disconnect().unwrap();
    assert_eq!(broker.build_engine.approval_count(), 0);

    let fresh = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let restarted = fresh.begin(BrokerOperationKind::Build).unwrap();
    let restarted_plan = build_plan(3);
    let restarted_digest = restarted_plan.digest().unwrap();
    fresh.prepare_build(&restarted, restarted_plan).unwrap();
    fresh
        .approve_build(
            &restarted,
            restarted_digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:02Z",
            &journal,
        )
        .unwrap();
    broker.restart().unwrap();
    assert_eq!(broker.build_engine.approval_count(), 0);
}

#[test]
fn expiry_releases_every_admission_gate() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let build = caller
        .begin_with_deadline(
            BrokerOperationKind::Build,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    let plan = build_plan(4);
    let digest = plan.digest().unwrap();
    caller.prepare_build(&build, plan).unwrap();
    caller
        .approve_build(
            &build,
            digest,
            ApprovalSource::Interactive,
            "2026-08-11T00:00:03Z",
            &Journal::default(),
        )
        .unwrap();
    caller.acquire_build(&build).unwrap();
    caller.acquire_gc_inhibit(&build).unwrap();

    {
        let mut state = broker.lock();
        purge_expired(
            &mut state,
            Instant::now() + Duration::from_secs(61),
            &broker.build_gate,
            &broker.build_engine,
        );
    }

    let snapshot = broker.admission_snapshot();
    assert_eq!(snapshot.operation_count(), 0);
    assert!(!snapshot.build_held());
    assert!(!snapshot.gc_held());
    assert_eq!(snapshot.gc_inhibitor_count(), 0);
    assert_eq!(broker.build_engine.approval_count(), 0);
}

#[test]
fn begin_repair_acquires_gc_inhibitor_and_refuses_while_gc_admitted() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();

    let repair = caller.begin(BrokerOperationKind::Repair).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    caller.cancel(&repair).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);

    let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
    caller.acquire_gc(&gc).unwrap();
    assert_eq!(
        caller
            .begin(BrokerOperationKind::Repair)
            .unwrap_err()
            .code(),
        BrokerErrorCode::AdmissionBusy
    );
    caller.cancel(&gc).unwrap();
}

#[test]
fn repair_dispatch_completion_retains_inhibitor_until_finish() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.begin_repair_dispatch(&handle).unwrap();
    caller.complete_repair_dispatch(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.finish_repair_dispatch(&handle, true).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn repair_dispatch_rejects_external_complete_and_cancel_retains_until_finish() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    caller.begin_repair_dispatch(&handle).unwrap();

    assert_eq!(
        caller.complete(&handle).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.cancel(&handle).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.finish_repair_dispatch(&handle, false).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn repair_dispatch_expiry_retains_admission_and_finish_rejects_late_success() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller
        .begin_with_deadline(
            BrokerOperationKind::Repair,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    caller.begin_repair_dispatch(&handle).unwrap();

    {
        let mut state = broker.lock();
        purge_expired(
            &mut state,
            Instant::now() + Duration::from_secs(61),
            &broker.build_gate,
            &broker.build_engine,
        );
    }

    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    assert_eq!(broker.admission_snapshot().operation_count(), 1);
    assert_eq!(
        caller
            .finish_repair_dispatch(&handle, true)
            .unwrap_err()
            .code(),
        BrokerErrorCode::AdmissionCancelled
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn finish_repair_dispatch_cancels_a_still_running_failure() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    caller.begin_repair_dispatch(&handle).unwrap();

    // An authority error that never cancelled still exits with a terminal,
    // unprotected-free operation.
    caller.finish_repair_dispatch(&handle, false).unwrap();
    assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Cancelled);
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn repair_dispatch_disconnect_retains_inhibitor_until_finish() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    caller.begin_repair_dispatch(&handle).unwrap();

    caller.disconnect().unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
    assert_eq!(
        caller
            .finish_repair_dispatch(&handle, true)
            .unwrap_err()
            .code(),
        BrokerErrorCode::AdmissionCancelled
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn finish_repair_dispatch_rejects_foreign_handle_without_releasing() {
    let broker = InProcessBroker::new().unwrap();
    let owner = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let other = broker
        .connect(InProcessCallerPeer::authenticated(1002))
        .unwrap();
    let handle = owner.begin(BrokerOperationKind::Repair).unwrap();
    owner.begin_repair_dispatch(&handle).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    assert_eq!(
        other
            .finish_repair_dispatch(&handle, true)
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidOperationHandle
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    owner.finish_repair_dispatch(&handle, false).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn finish_repair_dispatch_rejects_non_dispatching_repair_without_releasing() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    assert_eq!(
        caller
            .finish_repair_dispatch(&handle, true)
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);

    caller.cancel(&handle).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
}

#[test]
fn operation_class_cannot_widen_admission_authority() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let doctor = caller.begin(BrokerOperationKind::Doctor).unwrap();
    assert_eq!(
        caller.acquire_build(&doctor).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(
        caller.acquire_gc_inhibit(&doctor).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(
        caller.acquire_gc(&doctor).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    assert_eq!(
        caller
            .authorize_adapter_call(&doctor, crate::MethodKind::Build)
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
    caller
        .authorize_adapter_call(&doctor, crate::MethodKind::Version)
        .unwrap();
    assert_eq!(
        caller
            .authorize_channel_refresh(&doctor)
            .unwrap_err()
            .code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );

    let refresh = caller.begin(BrokerOperationKind::Refresh).unwrap();
    caller.authorize_channel_refresh(&refresh).unwrap();
    assert_eq!(
        caller.authorize_catalog_query(&refresh).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );

    let resolve = caller.begin(BrokerOperationKind::Resolve).unwrap();
    caller.authorize_catalog_query(&resolve).unwrap();

    let build = caller.begin(BrokerOperationKind::Build).unwrap();
    assert_eq!(
        caller.acquire_gc(&build).unwrap_err().code(),
        BrokerErrorCode::InvalidAdmissionTransition
    );
}

#[test]
fn operation_handle_debug_is_redacted() {
    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let handle = caller.begin(BrokerOperationKind::Doctor).unwrap();
    assert!(!format!("{handle:?}").contains(handle.as_str()));
}

#[test]
fn contained_child_policy_is_absolute_fixed_and_scrubbed() {
    let policy =
        ChildContainmentPolicy::new("/opt/pkg/nix/2.34.8/bin/nix", "/var/lib/pkg/broker-home")
            .unwrap();
    assert_eq!(
        policy.executable(),
        Path::new("/opt/pkg/nix/2.34.8/bin/nix")
    );
    assert_eq!(
        policy.environment().keys().cloned().collect::<Vec<_>>(),
        [
            "HOME",
            "NIX_CONFIG",
            "NIX_DAEMON_SOCKET_PATH",
            "NIX_REMOTE",
            "NIX_STATE_DIR",
            "NIX_USER_CONF_FILES",
            "PATH",
            "TMPDIR",
        ]
    );
    assert_eq!(policy.environment()["HOME"], "/var/lib/pkg/broker-home");
    assert_eq!(
        policy.environment()["TMPDIR"],
        "/var/lib/pkg/broker-home/tmp"
    );
    assert!(policy.environment()["NIX_USER_CONF_FILES"].is_empty());
    for forbidden in ["NIX_PATH", "NIXPKGS_CONFIG", "http_proxy", "SSH_AUTH_SOCK"] {
        assert!(!policy.environment().contains_key(forbidden));
    }
    assert!(ChildContainmentPolicy::new("nix", "/var/lib/pkg/broker-home").is_err());
    assert!(ChildContainmentPolicy::new("/usr/bin/nix", "/var/lib/pkg/broker-home").is_err());
    assert!(
        ChildContainmentPolicy::new("/opt/pkg/nix/../evil/nix", "/var/lib/pkg/broker-home")
            .is_err()
    );
    assert!(ChildContainmentPolicy::new("/opt/pkg/nix/2.34.8/bin/nix", "/var/empty").is_err());
    assert_eq!(policy.cancel_grace(), Duration::from_secs(5));
    assert!(policy.terminate_process_group());
}
