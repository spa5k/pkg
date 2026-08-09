//! PR-39 FakeNix + in-process broker/helper reference flow.

#![forbid(unsafe_code)]

use std::num::NonZeroU64;

use pkg_nix::{
    BrokerHelperRequest, BrokerOperationKind, GenerationId, InProcessBroker, InProcessCallerPeer,
    InProcessHelper, InProcessPeer, MaintenanceAdapter, MaintenanceErrorCode, NarIntegrity,
    NixAdapter, PathVerifyResult, PolicyVersion, ProductFrameCodec, RepairMode,
    RepairStorePathsRequest, RootName, RootSet, RootSetEntry, StorePath, TrustStatus,
    VerifiedRepairScope, VerifyMode, VerifyReport, VerifyRequest,
};
use pkg_testkit::FakeNix;

const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

fn path(name: &str) -> StorePath {
    StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
}

#[test]
fn phase_zero_to_capability_repair_round_trip_is_closed_and_restart_safe() {
    let damaged = path("hello-1.0");
    let verify_request = VerifyRequest::new(vec![damaged.clone()], VerifyMode::Recursive).unwrap();
    let verify_report = VerifyReport::new(vec![PathVerifyResult::new(
        damaged.clone(),
        NarIntegrity::Corrupt,
        TrustStatus::Trusted,
    )])
    .unwrap();
    let fake = FakeNix::new();
    fake.expect_verify(verify_request.clone(), Ok(verify_report));
    assert_eq!(
        fake.verify(&verify_request).unwrap().results()[0].nar_integrity(),
        NarIntegrity::Corrupt
    );

    let broker = InProcessBroker::new().unwrap();
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap();
    let operation = caller.begin(BrokerOperationKind::Repair).unwrap();
    caller.acquire_gc_inhibit(&operation).unwrap();

    let helper = InProcessHelper::new(991).unwrap();
    let helper_session = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap();
    let maintenance = helper_session.for_caller(1001);
    let generation = GenerationId::new("gen-0007").unwrap();
    maintenance
        .publish_root_set(
            &RootSet::new(
                1001,
                generation.clone(),
                vec![RootSetEntry::new(
                    RootName::new("hello-out").unwrap(),
                    damaged.clone(),
                )],
            )
            .unwrap(),
        )
        .unwrap();
    let scope = VerifiedRepairScope::new(
        1001,
        generation,
        [damaged],
        None,
        PolicyVersion::new(NonZeroU64::new(1).unwrap()),
        RepairMode::CacheOnly,
    )
    .unwrap();
    let capability = maintenance.issue_repair_capability(&scope).unwrap();
    let framed = ProductFrameCodec::encode_helper_request(
        42,
        &BrokerHelperRequest::RepairStorePaths(RepairStorePathsRequest::new(capability)),
    )
    .unwrap();
    let (request_id, request) = ProductFrameCodec::decode_helper_request(&framed).unwrap();
    assert_eq!(request_id, 42);
    let BrokerHelperRequest::RepairStorePaths(request) = request else {
        panic!("typed frame changed method")
    };
    assert_eq!(
        maintenance
            .repair_store_paths(&request)
            .unwrap()
            .outcomes()
            .len(),
        1
    );
    caller.complete(&operation).unwrap();
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);

    let stale = maintenance.issue_repair_capability(&scope).unwrap();
    broker.restart().unwrap();
    helper.broker_restarted().unwrap();
    assert_eq!(
        maintenance
            .repair_store_paths(&RepairStorePathsRequest::new(stale))
            .unwrap_err()
            .code(),
        MaintenanceErrorCode::SessionRestarted
    );
    assert_eq!(broker.admission_snapshot().operation_count(), 0);
    fake.assert_exhausted().unwrap();
}
