//! Two-phase, explicitly non-atomic store-repair coordination.
//!
//! Phase 0 remains read-only in the broker. Mutating phases redeem opaque
//! helper capabilities and journal every target before, during, and after the
//! non-atomic Nix operation. Recovery never resumes a build automatically.

use std::collections::BTreeMap;
use std::fmt;

use pkg_core::{PolicyVersion, state::Digest};
use pkg_nix::{
    AuthenticatedCaller, BuildApprovalReceipt, CallerMaintenance, GenerationId, MaintenanceAdapter,
    MaintenanceCapability, NixAdapter, OperationHandle, OperationId, RepairMode,
    RepairStorePathsReport, RepairStorePathsRequest, StorePath, VerifiedRepairScope,
    verify_closure,
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
    pub(crate) const fn new(code: RepairCoordinatorErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a redacted journal-backend failure.
    #[must_use]
    pub const fn journal_failure() -> Self {
        Self::new(RepairCoordinatorErrorCode::JournalFailure)
    }

    /// Constructs a redacted privileged-helper failure.
    #[must_use]
    pub const fn helper_failure() -> Self {
        Self::new(RepairCoordinatorErrorCode::HelperFailure)
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
    pub(crate) fn from_parts(
        sequence: u64,
        path: StorePath,
        mode: Option<RepairMode>,
        status: RepairJournalStatus,
        approval_operation: Option<OperationId>,
    ) -> Result<Self, RepairCoordinatorError> {
        if sequence == 0 {
            return Err(RepairCoordinatorError::journal_failure());
        }
        validate_mode(status, mode)?;
        validate_approval(mode, approval_operation.as_ref())?;
        Ok(Self {
            sequence,
            path,
            mode,
            status,
            approval_operation,
        })
    }

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

/// Broker-private capability issuance and execution seam used by repair.
pub trait RepairMaintenance: Send + Sync {
    /// Issues one opaque capability for the exact broker-derived scope.
    ///
    /// # Errors
    ///
    /// Returns a closed helper failure if capability issuance is refused.
    fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, RepairCoordinatorError>;

    /// Redeems one capability through the fixed privileged repair executor.
    ///
    /// # Errors
    ///
    /// Returns a closed helper failure if the capability or repair is refused.
    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, RepairCoordinatorError>;
}

impl RepairMaintenance for CallerMaintenance {
    fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, RepairCoordinatorError> {
        Self::issue_repair_capability(self, scope)
            .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::HelperFailure))
    }

    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, RepairCoordinatorError> {
        MaintenanceAdapter::repair_store_paths(self, request)
            .map_err(|_| RepairCoordinatorError::new(RepairCoordinatorErrorCode::HelperFailure))
    }
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
#[allow(
    clippy::too_many_lines,
    reason = "one generation repair walks a closed recoverable state machine; splitting it would spread the invariant checks"
)]
pub fn repair_generation(
    request: &RepairRequest,
    adapter: &dyn NixAdapter,
    admission: &AuthenticatedCaller,
    handle: &OperationHandle,
    maintenance: &dyn RepairMaintenance,
    approval_gate: &dyn RepairApprovalGate,
    journal: &mut dyn RepairJournal,
) -> Result<RepairResult, RepairCoordinatorError> {
    if admission.authorize_repair(handle).is_err() {
        return Err(RepairCoordinatorError::new(
            RepairCoordinatorErrorCode::AdmissionFailure,
        ));
    }
    let initial = verify_closure(adapter, request.closure.clone()).map_err(|_| {
        cancel_with_error(
            admission,
            handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::VerifyFailure),
        )
    })?;
    if initial.is_clean() {
        if request.verify_only {
            complete_or_cancel(admission, handle)?;
            return Ok(RepairResult::Clean);
        }
        if let Err(error) = reconcile_clean_journal(initial.closure(), journal) {
            return Err(cancel_with_error(admission, handle, error));
        }
        complete_or_cancel(admission, handle)?;
        return Ok(RepairResult::Clean);
    }
    if request.verify_only {
        complete_or_cancel(admission, handle)?;
        return Ok(RepairResult::DamageDetected);
    }
    if let Err(error) = ensure_fresh_approval(
        initial.damaged(),
        journal.entries(),
        request.approved_build.as_ref(),
    ) {
        return Err(cancel_with_error(admission, handle, error));
    }
    for path in initial.damaged() {
        if let Err(error) = journal.append(path.clone(), None, RepairJournalStatus::Detected, None)
        {
            return Err(cancel_with_error(admission, handle, error));
        }
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
        return Err(cancel_with_error(admission, handle, error));
    }
    let after_cache = verify_closure(adapter, request.closure.clone()).map_err(|_| {
        cancel_with_error(
            admission,
            handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::VerifyFailure),
        )
    })?;
    if after_cache.is_clean() {
        if let Err(error) = mark_repaired(initial.damaged(), journal) {
            return Err(cancel_with_error(admission, handle, error));
        }
        complete_or_cancel(admission, handle)?;
        return Ok(RepairResult::RepairedFromCache);
    }

    if let Err(error) = mark_cache_successes(initial.damaged(), after_cache.damaged(), journal) {
        return Err(cancel_with_error(admission, handle, error));
    }

    let Some(approval) = request.approved_build.as_ref() else {
        return stop_for_approval(after_cache.damaged(), admission, handle, journal);
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
            handle,
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired),
        ));
    }
    if admission.acquire_build(handle).is_err() {
        return Err(cancel_with_error(
            admission,
            handle,
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
        return Err(cancel_with_error(admission, handle, error));
    }
    finish_build_repair(
        request,
        adapter,
        admission,
        handle,
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
    if admission.complete_repair_dispatch(handle).is_ok() {
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
    maintenance: &dyn RepairMaintenance,
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
    let capability = maintenance.issue_repair_capability(&scope)?;
    for path in paths {
        journal.append(
            path.clone(),
            Some(mode),
            RepairJournalStatus::InProgress,
            approval_operation.cloned(),
        )?;
    }
    let report = maintenance.repair_store_paths(&RepairStorePathsRequest::new(capability))?;
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

pub fn validate_journal(entries: &[RepairJournalEntry]) -> Result<(), RepairCoordinatorError> {
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
mod tests;
