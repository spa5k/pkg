//! Two-phase, explicitly non-atomic store-repair coordination.
//!
//! Phase 0 remains read-only in the broker. Mutating phases redeem opaque
//! helper capabilities and journal every target before, during, and after the
//! non-atomic Nix operation. Recovery never resumes a build automatically.

use std::collections::BTreeMap;
use std::fmt;

use pkg_core::{PolicyVersion, state::Digest};
use pkg_nix::{
    AuthenticatedCaller, BrokerOperationKind, BuildApprovalReceipt, CallerMaintenance,
    GenerationId, MaintenanceAdapter, NixAdapter, OperationHandle, OperationId, RepairMode,
    RepairStorePathsRequest, StorePath, VerifiedRepairScope, verify_closure,
};

/// Stable repair-coordinator failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairCoordinatorErrorCode {
    /// The broker-derived request or journal violates the closed grammar.
    ValidationFailure,
    /// Read-only Phase 0 or final verification failed.
    VerifyFailure,
    /// Broker admission could not be acquired or released safely.
    AdmissionFailure,
    /// Capability issuance or privileged helper execution failed.
    HelperFailure,
    /// The durable per-path journal could not record a required transition.
    JournalFailure,
    /// A fresh final verify still found damage after the approved phases.
    StillDamaged,
    /// An interrupted build requires a different freshly consumed approval.
    FreshApprovalRequired,
}

/// Redacted repair orchestration error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairCoordinatorError {
    code: RepairCoordinatorErrorCode,
}

impl RepairCoordinatorError {
    const fn new(code: RepairCoordinatorErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a redacted journal-backend failure.
    #[must_use]
    pub const fn journal_failure() -> Self {
        Self::new(RepairCoordinatorErrorCode::JournalFailure)
    }

    /// Returns the stable public category.
    #[must_use]
    pub const fn code(self) -> RepairCoordinatorErrorCode {
        self.code
    }
}

impl fmt::Display for RepairCoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.code {
            RepairCoordinatorErrorCode::ValidationFailure => "the repair request is invalid",
            RepairCoordinatorErrorCode::VerifyFailure => "read-only repair verification failed",
            RepairCoordinatorErrorCode::AdmissionFailure => "repair admission is unavailable",
            RepairCoordinatorErrorCode::HelperFailure => "privileged repair execution failed",
            RepairCoordinatorErrorCode::JournalFailure => "the repair journal is unavailable",
            RepairCoordinatorErrorCode::StillDamaged => "store damage remains after repair",
            RepairCoordinatorErrorCode::FreshApprovalRequired => {
                "a fresh repair-build approval is required"
            }
        })
    }
}

impl std::error::Error for RepairCoordinatorError {}

/// Durable per-path states. `InProgress` explicitly denotes a non-atomic window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairJournalStatus {
    /// Phase 0 found the path damaged and the closure is unhealthy.
    Detected,
    /// One exact mutating attempt is intended but has not started.
    Intended,
    /// Nix may have removed or moved the live path; content is unknown.
    InProgress,
    /// The helper returned and a fresh read-only verify is required.
    PostVerify,
    /// Cache repair could not finish; a new preview and approval are required.
    NeedsApproval,
    /// A fresh full-closure read-only verify found the path clean.
    Repaired,
}

/// One canonical repair-journal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairJournalEntry {
    sequence: u64,
    path: StorePath,
    mode: Option<RepairMode>,
    status: RepairJournalStatus,
    approval_operation: Option<OperationId>,
}

impl RepairJournalEntry {
    /// Returns the contiguous journal sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the typed store path.
    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    /// Returns the helper mode, absent only for detection/final state.
    #[must_use]
    pub const fn mode(&self) -> Option<RepairMode> {
        self.mode
    }

    /// Returns the explicit per-path state.
    #[must_use]
    pub const fn status(&self) -> RepairJournalStatus {
        self.status
    }

    /// Returns the approval operation bound to build-phase rows.
    #[must_use]
    pub const fn approval_operation(&self) -> Option<&OperationId> {
        self.approval_operation.as_ref()
    }
}

/// Durable service-private journal boundary.
pub trait RepairJournal {
    /// Returns the accepted complete journal prefix.
    fn entries(&self) -> &[RepairJournalEntry];

    /// Durably appends one transition before returning success.
    ///
    /// # Errors
    /// Returns a redacted failure if the row cannot be made durable.
    fn append(
        &mut self,
        path: StorePath,
        mode: Option<RepairMode>,
        status: RepairJournalStatus,
        approval_operation: Option<OperationId>,
    ) -> Result<(), RepairCoordinatorError>;
}

/// Deterministic in-memory journal used by the fake/e2e contract lane.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MemoryRepairJournal {
    entries: Vec<RepairJournalEntry>,
}

impl RepairJournal for MemoryRepairJournal {
    fn entries(&self) -> &[RepairJournalEntry] {
        &self.entries
    }

    fn append(
        &mut self,
        path: StorePath,
        mode: Option<RepairMode>,
        status: RepairJournalStatus,
        approval_operation: Option<OperationId>,
    ) -> Result<(), RepairCoordinatorError> {
        validate_mode(status, mode)?;
        validate_approval(mode, approval_operation.as_ref())?;
        let sequence = u64::try_from(self.entries.len())
            .map_err(|_| RepairCoordinatorError::journal_failure())?
            .checked_add(1)
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        let entry = RepairJournalEntry {
            sequence,
            path,
            mode,
            status,
            approval_operation,
        };
        let mut candidate = self.entries.clone();
        candidate.push(entry);
        validate_journal(&candidate)?;
        self.entries = candidate;
        Ok(())
    }
}

/// Broker-derived, user-initiated repair input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRequest {
    owner_uid: u32,
    generation: GenerationId,
    closure: Vec<StorePath>,
    policy_version: PolicyVersion,
    verify_only: bool,
    approved_build: Option<BuildApprovalReceipt>,
}

/// Canonical scope an authoritative approval store must bind and consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairApprovalScope {
    owner_uid: u32,
    generation: GenerationId,
    paths: Vec<StorePath>,
    build_plan_digest: Digest,
    policy_version: PolicyVersion,
}

impl RepairApprovalScope {
    /// Returns the authenticated owner uid.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }
    /// Returns the exact rooted generation.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }
    /// Returns the exact remaining damage set.
    #[must_use]
    pub fn paths(&self) -> &[StorePath] {
        &self.paths
    }
    /// Returns the canonical full-output repair-build plan digest.
    #[must_use]
    pub const fn build_plan_digest(&self) -> Digest {
        self.build_plan_digest
    }
    /// Returns the governing policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }
}

/// Authoritative broker-side single-use approval store.
pub trait RepairApprovalGate {
    /// Atomically validates scope binding and consumes the receipt.
    ///
    /// # Errors
    /// Returns a closed error for unknown, replayed, stale, or mismatched approval.
    fn consume(
        &self,
        receipt: &BuildApprovalReceipt,
        scope: &RepairApprovalScope,
    ) -> Result<(), RepairCoordinatorError>;
}

impl RepairRequest {
    /// Constructs one bounded request from broker-held rooted-generation state.
    ///
    /// # Errors
    /// Returns a closed error for uid zero, an empty/duplicate closure, or an
    /// approval digest on a verify-only request.
    pub fn new(
        owner_uid: u32,
        generation: GenerationId,
        mut closure: Vec<StorePath>,
        policy_version: PolicyVersion,
        verify_only: bool,
        approved_build: Option<BuildApprovalReceipt>,
    ) -> Result<Self, RepairCoordinatorError> {
        closure.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let original_len = closure.len();
        closure.dedup_by(|left, right| left.as_str() == right.as_str());
        if owner_uid == 0
            || closure.is_empty()
            || closure.len() != original_len
            || (verify_only && approved_build.is_some())
            || approved_build
                .as_ref()
                .is_some_and(|approval| approval.policy_version() != policy_version)
        {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            owner_uid,
            generation,
            closure,
            policy_version,
            verify_only,
            approved_build,
        })
    }
}

/// Sanitized terminal repair outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairResult {
    /// Phase 0 found the full closure clean.
    Clean,
    /// Verify-only mode found damage but performed no mutation.
    DamageDetected,
    /// Cache-only helper repair followed by a clean full verification.
    RepairedFromCache,
    /// Damage remains and a fresh full-output build preview is required.
    NeedsApproval,
    /// Explicitly approved local repair build followed by a clean verification.
    RepairedByBuild,
}

/// Crash-recovery action derived only from the accepted journal prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairRecoveryAction {
    /// Re-run Phase 0, then idempotently attempt cache-only repair.
    RetryCacheOnly(StorePath),
    /// Re-run Phase 0 and require a fresh build preview and approval.
    NeedsFreshApproval(StorePath),
}

/// Derives safe restart work without ever auto-resuming a build.
///
/// # Errors
/// Returns a closed error if sequences or mode/status combinations are invalid.
pub fn recover_repair(
    entries: &[RepairJournalEntry],
) -> Result<Vec<RepairRecoveryAction>, RepairCoordinatorError> {
    validate_journal(entries)?;
    let mut latest = BTreeMap::new();
    for entry in entries {
        latest.insert(entry.path.as_str(), entry);
    }
    let mut actions = Vec::new();
    for entry in latest.into_values() {
        if entry.status == RepairJournalStatus::Repaired {
            continue;
        }
        if entry.mode == Some(RepairMode::Build)
            || entry.status == RepairJournalStatus::NeedsApproval
        {
            actions.push(RepairRecoveryAction::NeedsFreshApproval(entry.path.clone()));
        } else {
            actions.push(RepairRecoveryAction::RetryCacheOnly(entry.path.clone()));
        }
    }
    Ok(actions)
}

/// Runs Phase 0, cache-only repair, optional approved build, and final verify.
///
/// The caller must warn about temporary command unavailability before invoking
/// this function in mutating mode.
///
/// # Errors
/// Returns a closed error for verify, admission, journal, capability, helper,
/// or final-integrity failure. Any opened broker operation is cancelled on
/// failure, releasing its build/GC permits.
pub fn repair_generation(
    request: &RepairRequest,
    adapter: &dyn NixAdapter,
    admission: &AuthenticatedCaller,
    maintenance: &CallerMaintenance,
    approval_gate: &dyn RepairApprovalGate,
    journal: &mut dyn RepairJournal,
) -> Result<RepairResult, RepairCoordinatorError> {
    let initial = verify_closure(adapter, request.closure.clone())
        .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::VerifyFailure))?;
    if initial.is_clean() {
        reconcile_clean_journal(initial.closure(), journal)?;
        return Ok(RepairResult::Clean);
    }
    if request.verify_only {
        return Ok(RepairResult::DamageDetected);
    }
    ensure_fresh_approval(
        initial.damaged(),
        journal.entries(),
        request.approved_build.as_ref(),
    )?;
    for path in initial.damaged() {
        journal.append(path.clone(), None, RepairJournalStatus::Detected, None)?;
    }

    let handle = admission
        .begin(BrokerOperationKind::Repair)
        .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure))?;
    if admission.acquire_gc_inhibit(&handle).is_err() {
        return Err(cancel_with_error(
            admission,
            &handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure),
        ));
    }

    if let Err(error) = run_helper_phase(
        request,
        initial.damaged(),
        RepairMode::CacheOnly,
        None,
        None,
        maintenance,
        journal,
    ) {
        return Err(cancel_with_error(admission, &handle, error));
    }
    let after_cache = verify_closure(adapter, request.closure.clone()).map_err(|_| {
        cancel_with_error(
            admission,
            &handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::VerifyFailure),
        )
    })?;
    if after_cache.is_clean() {
        if let Err(error) = mark_repaired(initial.damaged(), journal) {
            return Err(cancel_with_error(admission, &handle, error));
        }
        complete_or_cancel(admission, &handle)?;
        return Ok(RepairResult::RepairedFromCache);
    }

    if let Err(error) = mark_cache_successes(initial.damaged(), after_cache.damaged(), journal) {
        return Err(cancel_with_error(admission, &handle, error));
    }

    let Some(approval) = request.approved_build.as_ref() else {
        return stop_for_approval(after_cache.damaged(), admission, &handle, journal);
    };
    let build_plan_digest = approval.build_plan_digest();
    let approval_operation = approval.operation_id().clone();
    let approval_scope = RepairApprovalScope {
        owner_uid: request.owner_uid,
        generation: request.generation.clone(),
        paths: after_cache.damaged().to_vec(),
        build_plan_digest,
        policy_version: request.policy_version,
    };
    if approval_gate.consume(approval, &approval_scope).is_err() {
        return Err(cancel_with_error(
            admission,
            &handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired),
        ));
    }
    if admission.acquire_build(&handle).is_err() {
        return Err(cancel_with_error(
            admission,
            &handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure),
        ));
    }
    if let Err(error) = run_helper_phase(
        request,
        after_cache.damaged(),
        RepairMode::Build,
        Some(build_plan_digest),
        Some(&approval_operation),
        maintenance,
        journal,
    ) {
        return Err(cancel_with_error(admission, &handle, error));
    }
    finish_build_repair(
        request,
        adapter,
        admission,
        &handle,
        after_cache.damaged(),
        journal,
    )
}

fn stop_for_approval(
    paths: &[StorePath],
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
    journal: &mut dyn RepairJournal,
) -> Result<RepairResult, RepairCoordinatorError> {
    for path in paths {
        if let Err(error) =
            journal.append(path.clone(), None, RepairJournalStatus::NeedsApproval, None)
        {
            return Err(cancel_with_error(admission, handle, error));
        }
    }
    complete_or_cancel(admission, handle)?;
    Ok(RepairResult::NeedsApproval)
}

fn finish_build_repair(
    request: &RepairRequest,
    adapter: &dyn NixAdapter,
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
    repaired_paths: &[StorePath],
    journal: &mut dyn RepairJournal,
) -> Result<RepairResult, RepairCoordinatorError> {
    let final_verify = verify_closure(adapter, request.closure.clone()).map_err(|_| {
        cancel_with_error(
            admission,
            handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::VerifyFailure),
        )
    })?;
    if !final_verify.is_clean() {
        return Err(cancel_with_error(
            admission,
            handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::StillDamaged),
        ));
    }
    if let Err(error) = mark_repaired(repaired_paths, journal) {
        return Err(cancel_with_error(admission, handle, error));
    }
    complete_or_cancel(admission, handle)?;
    Ok(RepairResult::RepairedByBuild)
}

fn complete_or_cancel(
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<(), RepairCoordinatorError> {
    if admission.complete(handle).is_ok() {
        Ok(())
    } else {
        admission.cancel(handle).map_err(|_| {
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure)
        })?;
        Err(RepairCoordinatorError::new(
            RepairCoordinatorErrorCode::AdmissionFailure,
        ))
    }
}

fn cancel_with_error(
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
    original: RepairCoordinatorError,
) -> RepairCoordinatorError {
    if admission.cancel(handle).is_ok() {
        original
    } else {
        RepairCoordinatorError::new(RepairCoordinatorErrorCode::AdmissionFailure)
    }
}

fn run_helper_phase(
    request: &RepairRequest,
    paths: &[StorePath],
    mode: RepairMode,
    build_plan_digest: Option<Digest>,
    approval_operation: Option<&OperationId>,
    maintenance: &CallerMaintenance,
    journal: &mut dyn RepairJournal,
) -> Result<(), RepairCoordinatorError> {
    for path in paths {
        journal.append(
            path.clone(),
            Some(mode),
            RepairJournalStatus::Intended,
            approval_operation.cloned(),
        )?;
    }
    let scope = VerifiedRepairScope::new(
        request.owner_uid,
        request.generation.clone(),
        paths.iter().cloned(),
        build_plan_digest,
        request.policy_version,
        mode,
    )
    .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::ValidationFailure))?;
    let capability = maintenance
        .issue_repair_capability(&scope)
        .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::HelperFailure))?;
    for path in paths {
        journal.append(
            path.clone(),
            Some(mode),
            RepairJournalStatus::InProgress,
            approval_operation.cloned(),
        )?;
    }
    let report = maintenance
        .repair_store_paths(&RepairStorePathsRequest::new(capability))
        .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::HelperFailure))?;
    if report.mode() != mode
        || report.outcomes().len() != paths.len()
        || report
            .outcomes()
            .iter()
            .zip(paths)
            .any(|(outcome, path)| outcome.path() != path)
    {
        return Err(RepairCoordinatorError::new(
            RepairCoordinatorErrorCode::HelperFailure,
        ));
    }
    for path in paths {
        journal.append(
            path.clone(),
            Some(mode),
            RepairJournalStatus::PostVerify,
            approval_operation.cloned(),
        )?;
    }
    Ok(())
}

fn mark_repaired(
    paths: &[StorePath],
    journal: &mut dyn RepairJournal,
) -> Result<(), RepairCoordinatorError> {
    for path in paths {
        journal.append(path.clone(), None, RepairJournalStatus::Repaired, None)?;
    }
    Ok(())
}

fn mark_cache_successes(
    initially_damaged: &[StorePath],
    still_damaged: &[StorePath],
    journal: &mut dyn RepairJournal,
) -> Result<(), RepairCoordinatorError> {
    for path in initially_damaged {
        if still_damaged.iter().all(|candidate| candidate != path) {
            journal.append(path.clone(), None, RepairJournalStatus::Repaired, None)?;
        }
    }
    Ok(())
}

fn reconcile_clean_journal(
    closure: &[StorePath],
    journal: &mut dyn RepairJournal,
) -> Result<(), RepairCoordinatorError> {
    validate_journal(journal.entries())?;
    let mut latest = BTreeMap::new();
    for entry in journal.entries() {
        latest.insert(entry.path.as_str(), entry);
    }
    let repaired = closure
        .iter()
        .filter(|path| {
            latest
                .get(path.as_str())
                .is_some_and(|entry| entry.status != RepairJournalStatus::Repaired)
        })
        .cloned()
        .collect::<Vec<_>>();
    mark_repaired(&repaired, journal)
}

fn ensure_fresh_approval(
    damaged: &[StorePath],
    entries: &[RepairJournalEntry],
    approval: Option<&BuildApprovalReceipt>,
) -> Result<(), RepairCoordinatorError> {
    validate_journal(entries)?;
    let mut latest = BTreeMap::new();
    for entry in entries {
        latest.insert(entry.path.as_str(), entry);
    }
    for path in damaged {
        let Some(previous) = latest.get(path.as_str()) else {
            continue;
        };
        let interrupted_build = previous.mode == Some(RepairMode::Build)
            && previous.status != RepairJournalStatus::Repaired;
        if interrupted_build
            && approval
                .is_none_or(|fresh| previous.approval_operation() == Some(fresh.operation_id()))
        {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::FreshApprovalRequired,
            ));
        }
    }
    Ok(())
}

const fn validate_mode(
    status: RepairJournalStatus,
    mode: Option<RepairMode>,
) -> Result<(), RepairCoordinatorError> {
    let valid = match status {
        RepairJournalStatus::Detected
        | RepairJournalStatus::NeedsApproval
        | RepairJournalStatus::Repaired => mode.is_none(),
        RepairJournalStatus::Intended
        | RepairJournalStatus::InProgress
        | RepairJournalStatus::PostVerify => mode.is_some(),
    };
    if valid {
        Ok(())
    } else {
        Err(RepairCoordinatorError::new(
            RepairCoordinatorErrorCode::ValidationFailure,
        ))
    }
}

fn validate_approval(
    mode: Option<RepairMode>,
    approval_operation: Option<&OperationId>,
) -> Result<(), RepairCoordinatorError> {
    let valid = if mode == Some(RepairMode::Build) {
        approval_operation.is_some()
    } else {
        approval_operation.is_none()
    };
    if valid {
        Ok(())
    } else {
        Err(RepairCoordinatorError::new(
            RepairCoordinatorErrorCode::ValidationFailure,
        ))
    }
}

fn validate_journal(entries: &[RepairJournalEntry]) -> Result<(), RepairCoordinatorError> {
    let mut previous = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let expected = u64::try_from(index)
            .map_err(|_| RepairCoordinatorError::journal_failure())?
            .checked_add(1)
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        if entry.sequence != expected {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::ValidationFailure,
            ));
        }
        validate_mode(entry.status, entry.mode)?;
        validate_approval(entry.mode, entry.approval_operation.as_ref())?;
        if !valid_transition(previous.get(entry.path.as_str()).copied(), entry) {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::ValidationFailure,
            ));
        }
        previous.insert(entry.path.as_str(), entry);
    }
    Ok(())
}

fn valid_transition(previous: Option<&RepairJournalEntry>, next: &RepairJournalEntry) -> bool {
    let Some(previous) = previous else {
        return next.status == RepairJournalStatus::Detected;
    };
    if next.status == RepairJournalStatus::Detected && next.mode.is_none() {
        return true;
    }
    match (previous.status, next.status) {
        (_, RepairJournalStatus::Repaired) => next.mode.is_none(),
        (RepairJournalStatus::Detected, RepairJournalStatus::Intended) => {
            next.mode == Some(RepairMode::CacheOnly)
        }
        (RepairJournalStatus::PostVerify, RepairJournalStatus::NeedsApproval) => {
            next.mode.is_none()
        }
        (RepairJournalStatus::Intended, RepairJournalStatus::InProgress)
        | (RepairJournalStatus::InProgress, RepairJournalStatus::PostVerify) => {
            previous.mode == next.mode && previous.approval_operation == next.approval_operation
        }
        (
            RepairJournalStatus::PostVerify | RepairJournalStatus::NeedsApproval,
            RepairJournalStatus::Intended,
        ) => previous.mode != Some(RepairMode::Build) && next.mode == Some(RepairMode::Build),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_nix::{
        InProcessBroker, InProcessCallerPeer, InProcessHelper, InProcessPeer, NarIntegrity,
        PathVerifyResult, RootName, RootSet, RootSetEntry, TrustStatus, VerifyMode, VerifyReport,
        VerifyRequest,
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
    fn clean_phase_zero_reconciles_interrupted_repair_journal()
    -> Result<(), Box<dyn std::error::Error>> {
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
                &maintenance,
                &TestApprovalGate::default(),
                &mut journal
            )?,
            RepairResult::RepairedByBuild
        );
        assert!(journal.entries().iter().any(|entry| {
            entry.mode() == Some(RepairMode::Build)
                && entry.status() == RepairJournalStatus::InProgress
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
    fn interrupted_build_rejects_the_same_approval_operation()
    -> Result<(), Box<dyn std::error::Error>> {
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
    fn partial_cache_success_is_terminal_before_approval() -> Result<(), Box<dyn std::error::Error>>
    {
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
                    PathVerifyResult::new(
                        fixed.clone(),
                        NarIntegrity::Corrupt,
                        TrustStatus::Trusted,
                    ),
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
                    PathVerifyResult::new(
                        fixed.clone(),
                        NarIntegrity::Intact,
                        TrustStatus::Trusted,
                    ),
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
}
