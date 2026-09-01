//! Tests for the `repair` module.

use super::*;
use pkg_nix::{
    BrokerOperationKind, InProcessBroker, InProcessCallerPeer, InProcessHelper, InProcessPeer,
    NarIntegrity, PathVerifyResult, RootName, RootSet, RootSetEntry, TrustStatus, VerifyMode,
    VerifyReport, VerifyRequest,
};
use pkg_testkit::FakeNix;

#[derive(Default)]
struct TestApprovalGate {
    consumed: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl RepairApprovalGate for TestApprovalGate {
    fn consume(
        &self,
        receipt: &BuildApprovalReceipt,
        scope: &RepairApprovalScope,
    ) -> Result<(), RepairCoordinatorError> {
        if receipt.build_plan_digest() != scope.build_plan_digest()
            || receipt.policy_version() != scope.policy_version()
        {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::FreshApprovalRequired,
            ));
        }
        let key = format!(
            "{}:{}:{}:{:?}:{:?}",
            receipt.operation_id().as_str(),
            scope.owner_uid(),
            scope.generation().as_str(),
            scope.paths(),
            scope.build_plan_digest()
        );
        let mut consumed = self
            .consumed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if consumed.insert(key) {
            Ok(())
        } else {
            Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::FreshApprovalRequired,
            ))
        }
    }
}

fn path(name: &str) -> Result<StorePath, pkg_core::IdentityError> {
    StorePath::new(&format!(
        "/nix/store/00000000000000000000000000000000-{name}"
    ))
}

fn repair_handle(admission: &AuthenticatedCaller) -> Result<OperationHandle, pkg_nix::BrokerError> {
    admission.begin(BrokerOperationKind::Repair)
}

/// Test-local shadow of the coordinator that wraps it with the same
/// repair-execution lifecycle as the production dispatch wrapper.
fn repair_generation(
    request: &RepairRequest,
    adapter: &dyn NixAdapter,
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
    maintenance: &dyn RepairMaintenance,
    approval_gate: &dyn RepairApprovalGate,
    journal: &mut dyn RepairJournal,
) -> Result<RepairResult, RepairCoordinatorError> {
    crate::broker::run_repair_dispatch(
        admission,
        handle,
        || {
            super::repair_generation(
                request,
                adapter,
                admission,
                handle,
                maintenance,
                approval_gate,
                journal,
            )
        },
        || RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure),
    )
}

type Harness = (
    std::sync::Arc<InProcessBroker>,
    AuthenticatedCaller,
    CallerMaintenance,
    GenerationId,
);

#[test]
fn recovery_retries_cache_but_never_build() -> Result<(), Box<dyn std::error::Error>> {
    let cache = path("cache")?;
    let build = path("build")?;
    let done = path("done")?;
    let build_operation = OperationId::new("interrupted-build")?;
    let mut journal = MemoryRepairJournal::default();
    journal.append(cache.clone(), None, RepairJournalStatus::Detected, None)?;
    journal.append(
        cache.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::Intended,
        None,
    )?;
    journal.append(
        cache.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::InProgress,
        None,
    )?;
    journal.append(build.clone(), None, RepairJournalStatus::Detected, None)?;
    journal.append(
        build.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::Intended,
        None,
    )?;
    journal.append(
        build.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::InProgress,
        None,
    )?;
    journal.append(
        build.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::PostVerify,
        None,
    )?;
    journal.append(
        build.clone(),
        None,
        RepairJournalStatus::NeedsApproval,
        None,
    )?;
    journal.append(
        build.clone(),
        Some(RepairMode::Build),
        RepairJournalStatus::Intended,
        Some(build_operation.clone()),
    )?;
    journal.append(
        build.clone(),
        Some(RepairMode::Build),
        RepairJournalStatus::InProgress,
        Some(build_operation),
    )?;
    journal.append(done.clone(), None, RepairJournalStatus::Detected, None)?;
    journal.append(
        done.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::Intended,
        None,
    )?;
    journal.append(
        done.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::InProgress,
        None,
    )?;
    journal.append(
        done.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::PostVerify,
        None,
    )?;
    journal.append(done, None, RepairJournalStatus::Repaired, None)?;
    assert_eq!(
        recover_repair(journal.entries())?,
        vec![
            RepairRecoveryAction::NeedsFreshApproval(build),
            RepairRecoveryAction::RetryCacheOnly(cache),
        ]
    );
    Ok(())
}

#[test]
fn journal_rejects_mode_status_confusion() -> Result<(), Box<dyn std::error::Error>> {
    let mut journal = MemoryRepairJournal::default();
    let result = journal.append(
        path("bad")?,
        Some(RepairMode::Build),
        RepairJournalStatus::Repaired,
        None,
    );
    assert_eq!(
        result.err().map(RepairCoordinatorError::code),
        Some(RepairCoordinatorErrorCode::ValidationFailure)
    );
    Ok(())
}

fn harness(target: &StorePath) -> Result<Harness, Box<dyn std::error::Error>> {
    let broker = InProcessBroker::new()?;
    let admission = broker.connect(InProcessCallerPeer::authenticated(1000))?;
    let helper = InProcessHelper::new(2000)?;
    let session = helper.connect(InProcessPeer::authenticated_uid(2000))?;
    let maintenance = session.for_caller(1000);
    let generation = GenerationId::new("gen-0001")?;
    maintenance.publish_root_set(&RootSet::new(
        1000,
        generation.clone(),
        vec![RootSetEntry::new(RootName::new("main")?, target.clone())],
    )?)?;
    Ok((broker, admission, maintenance, generation))
}

fn verify_request(target: &StorePath) -> Result<VerifyRequest, Box<dyn std::error::Error>> {
    Ok(VerifyRequest::new(
        vec![target.clone()],
        VerifyMode::Recursive,
    )?)
}

fn verify_report(
    target: &StorePath,
    integrity: NarIntegrity,
) -> Result<VerifyReport, Box<dyn std::error::Error>> {
    Ok(VerifyReport::new(vec![PathVerifyResult::new(
        target.clone(),
        integrity,
        TrustStatus::Trusted,
    )])?)
}

#[test]
fn clean_phase_zero_reconciles_interrupted_repair_journal() -> Result<(), Box<dyn std::error::Error>>
{
    let target = path("already-repaired")?;
    let (_, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    adapter.expect_verify(
        verify_request(&target)?,
        Ok(verify_report(&target, NarIntegrity::Intact)?),
    );
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target.clone()],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        false,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    journal.append(target.clone(), None, RepairJournalStatus::Detected, None)?;
    journal.append(
        target.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::Intended,
        None,
    )?;
    journal.append(
        target,
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::InProgress,
        None,
    )?;

    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal,
        )?,
        RepairResult::Clean
    );
    assert!(recover_repair(journal.entries())?.is_empty());
    assert_eq!(
        journal.entries().last().map(RepairJournalEntry::status),
        Some(RepairJournalStatus::Repaired)
    );
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn verify_only_clean_never_reconciles_durable_repair_state()
-> Result<(), Box<dyn std::error::Error>> {
    let target = path("verify-clean")?;
    let (_, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    adapter.expect_verify(
        verify_request(&target)?,
        Ok(verify_report(&target, NarIntegrity::Intact)?),
    );
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target.clone()],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        true,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    journal.append(target, None, RepairJournalStatus::Detected, None)?;
    let before = journal.entries().to_vec();

    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal,
        )?,
        RepairResult::Clean
    );
    assert_eq!(journal.entries(), before);
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn cache_repair_is_final_verify_gated_and_releases_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let target = path("cache-repair")?;
    let (broker, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    adapter
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        )
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Intact)?),
        );
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        false,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::RepairedFromCache
    );
    assert_eq!(
        journal.entries().last().map(RepairJournalEntry::status),
        Some(RepairJournalStatus::Repaired)
    );
    assert!(!broker.admission_snapshot().build_held());
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn verify_only_damage_never_creates_recoverable_mutation_state()
-> Result<(), Box<dyn std::error::Error>> {
    let target = path("verify-only")?;
    let (_broker, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    adapter.expect_verify(
        verify_request(&target)?,
        Ok(verify_report(&target, NarIntegrity::Corrupt)?),
    );
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        true,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::DamageDetected
    );
    assert!(journal.entries().is_empty());
    assert!(recover_repair(journal.entries())?.is_empty());
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn cache_miss_stops_before_build_without_approval() -> Result<(), Box<dyn std::error::Error>> {
    let target = path("cache-miss")?;
    let (broker, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    for _ in 0..2 {
        adapter.expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Missing)?),
        );
    }
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target.clone()],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        false,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::NeedsApproval
    );
    assert_eq!(
        recover_repair(journal.entries())?,
        vec![RepairRecoveryAction::NeedsFreshApproval(target)]
    );
    assert!(!broker.admission_snapshot().build_held());
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn approved_build_is_single_operation_and_final_verify_gated()
-> Result<(), Box<dyn std::error::Error>> {
    let target = path("build-repair")?;
    let (broker, admission, maintenance, generation) = harness(&target)?;
    let adapter = FakeNix::new();
    adapter
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        )
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        )
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Intact)?),
        );
    let policy = PolicyVersion::new(std::num::NonZeroU64::MIN);
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target],
        policy,
        false,
        Some(BuildApprovalReceipt::new(
            OperationId::new("approved-build")?,
            Digest::from_bytes([9; 32]),
            policy,
        )),
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::RepairedByBuild
    );
    assert!(journal.entries().iter().any(|entry| {
        entry.mode() == Some(RepairMode::Build) && entry.status() == RepairJournalStatus::InProgress
    }));
    assert!(!broker.admission_snapshot().build_held());
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn approval_follow_up_reenters_phase_zero_with_the_same_journal()
-> Result<(), Box<dyn std::error::Error>> {
    let target = path("approval-follow-up")?;
    let (broker, admission, maintenance, generation) = harness(&target)?;
    let first = FakeNix::new();
    for _ in 0..2 {
        first.expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        );
    }
    let initial = RepairRequest::new(
        1000,
        generation.clone(),
        vec![target.clone()],
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        false,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &initial,
            &first,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::NeedsApproval
    );

    let approved = FakeNix::new();
    approved
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        )
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Corrupt)?),
        )
        .expect_verify(
            verify_request(&target)?,
            Ok(verify_report(&target, NarIntegrity::Intact)?),
        );
    let policy = PolicyVersion::new(std::num::NonZeroU64::MIN);
    let follow_up = RepairRequest::new(
        1000,
        generation,
        vec![target],
        policy,
        false,
        Some(BuildApprovalReceipt::new(
            OperationId::new("follow-up-build")?,
            Digest::from_bytes([4; 32]),
            policy,
        )),
    )?;
    assert_eq!(
        repair_generation(
            &follow_up,
            &approved,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::RepairedByBuild
    );
    assert_eq!(
        journal.entries().last().map(RepairJournalEntry::status),
        Some(RepairJournalStatus::Repaired)
    );
    assert!(!broker.admission_snapshot().build_held());
    assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    first.assert_exhausted()?;
    approved.assert_exhausted()?;
    Ok(())
}

#[test]
fn interrupted_build_rejects_the_same_approval_operation() -> Result<(), Box<dyn std::error::Error>>
{
    let target = path("stale-approval")?;
    let (_broker, admission, maintenance, generation) = harness(&target)?;
    let policy = PolicyVersion::new(std::num::NonZeroU64::MIN);
    let operation = OperationId::new("stale-build-operation")?;
    let approval =
        BuildApprovalReceipt::new(operation.clone(), Digest::from_bytes([5; 32]), policy);
    let mut journal = MemoryRepairJournal::default();
    journal.append(target.clone(), None, RepairJournalStatus::Detected, None)?;
    journal.append(
        target.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::Intended,
        None,
    )?;
    journal.append(
        target.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::InProgress,
        None,
    )?;
    journal.append(
        target.clone(),
        Some(RepairMode::CacheOnly),
        RepairJournalStatus::PostVerify,
        None,
    )?;
    journal.append(
        target.clone(),
        None,
        RepairJournalStatus::NeedsApproval,
        None,
    )?;
    journal.append(
        target.clone(),
        Some(RepairMode::Build),
        RepairJournalStatus::Intended,
        Some(operation.clone()),
    )?;
    journal.append(
        target.clone(),
        Some(RepairMode::Build),
        RepairJournalStatus::InProgress,
        Some(operation),
    )?;
    let adapter = FakeNix::new();
    adapter.expect_verify(
        verify_request(&target)?,
        Ok(verify_report(&target, NarIntegrity::Corrupt)?),
    );
    let request = RepairRequest::new(
        1000,
        generation,
        vec![target],
        policy,
        false,
        Some(approval),
    )?;
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )
        .err()
        .map(RepairCoordinatorError::code),
        Some(RepairCoordinatorErrorCode::FreshApprovalRequired)
    );
    adapter.assert_exhausted()?;
    Ok(())
}

#[test]
fn partial_cache_success_is_terminal_before_approval() -> Result<(), Box<dyn std::error::Error>> {
    let fixed = path("fixed-by-cache")?;
    let remaining = path("remaining-damage")?;
    let (_broker, admission, maintenance, generation) = harness(&fixed)?;
    let closure = vec![fixed.clone(), remaining.clone()];
    let verify = VerifyRequest::new(closure.clone(), VerifyMode::Recursive)?;
    let adapter = FakeNix::new();
    adapter
        .expect_verify(
            verify.clone(),
            Ok(VerifyReport::new(vec![
                PathVerifyResult::new(fixed.clone(), NarIntegrity::Corrupt, TrustStatus::Trusted),
                PathVerifyResult::new(
                    remaining.clone(),
                    NarIntegrity::Corrupt,
                    TrustStatus::Trusted,
                ),
            ])?),
        )
        .expect_verify(
            verify,
            Ok(VerifyReport::new(vec![
                PathVerifyResult::new(fixed.clone(), NarIntegrity::Intact, TrustStatus::Trusted),
                PathVerifyResult::new(
                    remaining.clone(),
                    NarIntegrity::Corrupt,
                    TrustStatus::Trusted,
                ),
            ])?),
        );
    let request = RepairRequest::new(
        1000,
        generation,
        closure,
        PolicyVersion::new(std::num::NonZeroU64::MIN),
        false,
        None,
    )?;
    let mut journal = MemoryRepairJournal::default();
    assert_eq!(
        repair_generation(
            &request,
            &adapter,
            &admission,
            &repair_handle(&admission)?,
            &maintenance,
            &TestApprovalGate::default(),
            &mut journal
        )?,
        RepairResult::NeedsApproval
    );
    assert_eq!(
        recover_repair(journal.entries())?,
        vec![RepairRecoveryAction::NeedsFreshApproval(remaining)]
    );
    assert!(journal.entries().iter().any(|entry| {
        entry.path() == &fixed && entry.status() == RepairJournalStatus::Repaired
    }));
    adapter.assert_exhausted()?;
    Ok(())
}
