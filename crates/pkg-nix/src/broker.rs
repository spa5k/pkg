//! In-process broker reference for authenticated operation lifecycle and admission.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use pkg_core::PolicyVersion;
use sha2::{Digest as _, Sha256};

use crate::build::BuildExecutionRuntime;
use crate::{
    ApprovalJournal, ApprovalSource, BuildApprovalReceipt, BuildEngineError, BuildEngineErrorCode,
    BuildPlan, BuildPreview, BuildProgressEstimate, BuildReport, CancellationToken, Digest,
    InstallEvidence, LocalBuildEngine, NixAdapter, NixAdapterError, OperationId, ResourceProbe,
    RootSet, RootSetAttestationRequest, RootSetIntent, RootSetReport, RootSetTransitionIntent,
    RootSetTransitionReport, RootSetTransitionRequest, VolatileBuildEstimate,
    maintenance::{MaintenanceError, random_secret},
};

const OPERATION_TTL: Duration = Duration::from_secs(30 * 60);
const ADMISSION_WAIT_POLL: Duration = Duration::from_millis(25);

pub(crate) struct BuildExecutionIo<'a> {
    resources: &'a dyn ResourceProbe,
    adapter: &'a dyn NixAdapter,
    progress: &'a mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
}

/// Stable in-process broker failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerErrorCode {
    /// Claimed caller identity differed from transport-authenticated identity.
    UnauthenticatedCaller,
    /// The session predates the latest broker restart.
    SessionRestarted,
    /// An operation handle was unknown or belonged to another caller.
    InvalidOperationHandle,
    /// The operation handle exceeded its fixed lifetime.
    OperationExpired,
    /// A machine-global admission gate is currently held by another operation.
    AdmissionBusy,
    /// Admission waiting was cancelled by the caller or operation lifecycle.
    AdmissionCancelled,
    /// The requested gate transition violated the operation lifecycle.
    InvalidAdmissionTransition,
    /// The trusted cache acquisition callback failed after admission.
    CacheAcquisitionFailed,
    /// The broker could not derive a valid private build plan or preview.
    InvalidBuildPlan,
    /// The approved digest did not identify the broker-held private plan.
    BuildApprovalMismatch,
    /// The private build plan was not approved or was already consumed.
    BuildApprovalUnavailable,
    /// Admission-time replanning invalidated the approved private plan.
    BuildApprovalInvalidated,
    /// Volatile disk or load checks refused execution under admission.
    BuildResourcePreflightFailed,
    /// The managed Nix adapter failed or returned inconsistent build outputs.
    BuildExecutionFailed,
    /// Durable root publication failed after successful build execution.
    RootPublicationFailed,
    /// A managed child policy contained a non-canonical runtime path.
    InvalidChildPolicy,
    /// Fresh session entropy could not be obtained from the operating system.
    EntropyUnavailable,
}

/// Redacted broker boundary error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerError {
    code: BrokerErrorCode,
}

impl BrokerError {
    const fn new(code: BrokerErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> BrokerErrorCode {
        self.code
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "broker request refused: {:?}", self.code)
    }
}

impl std::error::Error for BrokerError {}

/// Closed operation classes accepted by the broker lifecycle layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerOperationKind {
    /// Readiness and ownership diagnosis.
    Doctor,
    /// Signed channel/source/index refresh.
    Refresh,
    /// Catalog resolution and evaluation.
    Resolve,
    /// Substitution-only acquisition.
    Acquire,
    /// Explicitly approved local build.
    Build,
    /// Activation/generation mutation.
    Activate,
    /// Garbage collection.
    Gc,
    /// User-initiated store repair.
    Repair,
}

/// Trusted in-process result of one cache-first install attempt.
pub enum CacheInstallAttempt {
    /// Every selected output was substituted and verified.
    Acquired(InstallEvidence),
    /// At least one selected output requires the approved local-build path.
    BuildRequired,
}

/// Public cache-first outcome. Install evidence stays behind the operation handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheInstallOutcome {
    /// Verified evidence is retained until root publication or cancellation.
    Acquired,
    /// No evidence was retained and local-build approval is required.
    BuildRequired,
}

impl BrokerOperationKind {
    const ALL: [Self; 8] = [
        Self::Doctor,
        Self::Refresh,
        Self::Resolve,
        Self::Acquire,
        Self::Build,
        Self::Activate,
        Self::Gc,
        Self::Repair,
    ];
}

/// Opaque broker-issued operation handle.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationHandle(pub(crate) String);

impl OperationHandle {
    /// Returns the opaque transport token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OperationHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OperationHandle(<opaque>)")
    }
}

/// Publicly observable operation lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    /// The broker is still processing the operation.
    Running,
    /// The operation completed and released all admission.
    Completed,
    /// Cancellation or caller disconnect released all admission.
    Cancelled,
}

#[derive(Debug, Clone)]
struct OperationRecord {
    owner_uid: u32,
    kind: BrokerOperationKind,
    status: OperationStatus,
    expires_at: Instant,
    build_prepared: bool,
    build_operation_id: Option<OperationId>,
    prepared_build: Option<PreparedBuild>,
    cancellation: Arc<CancellationToken>,
    build_executing: bool,
    cache_acquiring: bool,
    root_transition_executing: bool,
    awaiting_root_outputs: Option<BTreeSet<String>>,
    install_evidence: Option<InstallEvidence>,
}

impl OperationRecord {
    const fn authority_executing(&self) -> bool {
        self.build_executing || self.cache_acquiring || self.root_transition_executing
    }
}

#[derive(Clone)]
struct PreparedBuild {
    plan: BuildPlan,
    preview: BuildPreview,
    estimate: Option<VolatileBuildEstimate>,
    digest: Digest,
    approval: PreparedBuildApproval,
    replanner: Option<Arc<dyn TrustedBuildReplanner>>,
}

impl fmt::Debug for PreparedBuild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedBuild")
            .field("digest", &self.digest)
            .field("has_estimate", &self.estimate.is_some())
            .field("has_replanner", &self.replanner.is_some())
            .finish_non_exhaustive()
    }
}

/// Broker-retained authority for reconstructing a private plan at admission.
///
/// Implementations are installed only by trusted in-process orchestration. No
/// implementation, error detail, target, or callback crosses product framing.
pub trait TrustedBuildReplanner: Send + Sync {
    /// Reconstructs the plan from current authenticated source and host facts.
    fn replan(&self) -> Result<BuildPlan, TrustedReplanError>;
}

/// Redacted refusal from a broker-retained trusted replanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedReplanError;

impl fmt::Display for TrustedReplanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("trusted build replan refused")
    }
}

impl std::error::Error for TrustedReplanError {}

#[derive(Debug, Clone)]
enum PreparedBuildApproval {
    Unapproved,
    Recording,
    Approved { receipt: BuildApprovalReceipt },
    Executing,
}

#[derive(Debug)]
struct BrokerState {
    epoch: u64,
    secret: [u8; 32],
    next_operation: u64,
    operations: BTreeMap<OperationHandle, OperationRecord>,
    gc_holder: Option<OperationHandle>,
    gc_inhibitors: BTreeSet<OperationHandle>,
}

#[derive(Debug, Default)]
struct BuildGateState {
    holder: Option<OperationHandle>,
    waiting: VecDeque<OperationHandle>,
}

#[derive(Debug, Default)]
struct FairBuildGate {
    state: Mutex<BuildGateState>,
    changed: Condvar,
}

impl FairBuildGate {
    fn try_acquire(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.lock();
        if state.holder.as_ref() == Some(handle) {
            return Ok(());
        }
        if state.holder.is_none() && state.waiting.is_empty() {
            state.holder = Some(handle.clone());
            Ok(())
        } else {
            Err(BrokerError::new(BrokerErrorCode::AdmissionBusy))
        }
    }

    fn enqueue(&self, handle: &OperationHandle) -> Result<bool, BrokerError> {
        let mut state = self.lock();
        if state.holder.as_ref() == Some(handle) {
            return Ok(true);
        }
        if state.waiting.contains(handle) {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        if state.holder.is_none() && state.waiting.is_empty() {
            state.holder = Some(handle.clone());
            Ok(true)
        } else {
            state.waiting.push_back(handle.clone());
            Ok(false)
        }
    }

    fn wait(
        &self,
        handle: &OperationHandle,
        caller_cancellation: &CancellationToken,
        operation_cancellation: &CancellationToken,
    ) -> Result<(), BrokerError> {
        let mut state = self.lock();
        if state.holder.as_ref() == Some(handle) {
            return Ok(());
        }
        if !state.waiting.contains(handle) {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        loop {
            if caller_cancellation.is_cancelled() || operation_cancellation.is_cancelled() {
                state.waiting.retain(|waiting| waiting != handle);
                self.changed.notify_all();
                return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
            }
            if state.holder.is_none() && state.waiting.front() == Some(handle) {
                state.waiting.pop_front();
                state.holder = Some(handle.clone());
                return Ok(());
            }
            if !state.waiting.contains(handle) {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
            }
            state = self
                .changed
                .wait_timeout(state, ADMISSION_WAIT_POLL)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn release(&self, handle: &OperationHandle) {
        let mut state = self.lock();
        if state.holder.as_ref() == Some(handle) {
            state.holder = None;
        }
        state.waiting.retain(|waiting| waiting != handle);
        self.changed.notify_all();
    }

    fn reset(&self) {
        let mut state = self.lock();
        state.holder = None;
        state.waiting.clear();
        self.changed.notify_all();
    }

    fn held(&self) -> bool {
        self.lock().holder.is_some()
    }

    fn blocks_gc(&self) -> bool {
        let state = self.lock();
        state.holder.is_some() || !state.waiting.is_empty()
    }

    #[cfg(test)]
    fn waiting_count(&self) -> usize {
        self.lock().waiting.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BuildGateState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Read-only admission snapshot for tests, diagnostics, and restart handshakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    operation_count: usize,
    build_held: bool,
    gc_held: bool,
    gc_inhibitor_count: usize,
}

impl AdmissionSnapshot {
    /// Returns the count of in-flight or retained terminal operation handles.
    #[must_use]
    pub const fn operation_count(self) -> usize {
        self.operation_count
    }

    /// Returns whether the machine-wide build lease is held.
    #[must_use]
    pub const fn build_held(self) -> bool {
        self.build_held
    }

    /// Returns whether exclusive garbage collection is admitted.
    #[must_use]
    pub const fn gc_held(self) -> bool {
        self.gc_held
    }

    /// Returns the number of shared GC-inhibit permits.
    #[must_use]
    pub const fn gc_inhibitor_count(self) -> usize {
        self.gc_inhibitor_count
    }
}

/// Simulated CLI transport peer with authenticated and claimed uid lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InProcessCallerPeer {
    authenticated_uid: u32,
    claimed_uid: u32,
}

impl InProcessCallerPeer {
    /// Constructs an honest in-process peer.
    #[must_use]
    pub const fn authenticated(uid: u32) -> Self {
        Self {
            authenticated_uid: uid,
            claimed_uid: uid,
        }
    }

    /// Constructs a peer used to prove identity-claim mismatch rejection.
    #[must_use]
    pub const fn with_claim(authenticated_uid: u32, claimed_uid: u32) -> Self {
        Self {
            authenticated_uid,
            claimed_uid,
        }
    }
}

/// In-process reference broker with memory-only operation and admission state.
pub struct InProcessBroker {
    state: Mutex<BrokerState>,
    build_gate: FairBuildGate,
    build_engine: LocalBuildEngine,
}

impl fmt::Debug for InProcessBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InProcessBroker(<private-state>)")
    }
}

impl InProcessBroker {
    /// Creates a new broker epoch with fresh handle entropy.
    pub fn new() -> Result<Arc<Self>, BrokerError> {
        Ok(Arc::new(Self {
            state: Mutex::new(BrokerState {
                epoch: 1,
                secret: random_secret()
                    .map_err(|_| BrokerError::new(BrokerErrorCode::EntropyUnavailable))?,
                next_operation: 0,
                operations: BTreeMap::new(),
                gc_holder: None,
                gc_inhibitors: BTreeSet::new(),
            }),
            build_gate: FairBuildGate::default(),
            build_engine: LocalBuildEngine::new(),
        }))
    }

    /// Authenticates the CLI peer and establishes a restart-bound session.
    pub fn connect(
        self: &Arc<Self>,
        peer: InProcessCallerPeer,
    ) -> Result<AuthenticatedCaller, BrokerError> {
        if peer.authenticated_uid != peer.claimed_uid {
            return Err(BrokerError::new(BrokerErrorCode::UnauthenticatedCaller));
        }
        let epoch = self.lock().epoch;
        Ok(AuthenticatedCaller {
            broker: Arc::clone(self),
            epoch,
            uid: peer.authenticated_uid,
        })
    }

    /// Restarts the broker and empties all handles and admission state.
    pub fn restart(&self) -> Result<(), BrokerError> {
        let secret =
            random_secret().map_err(|_| BrokerError::new(BrokerErrorCode::EntropyUnavailable))?;
        let mut state = self.lock();
        for record in state.operations.values() {
            record.cancellation.cancel();
            if let Some(operation_id) = build_operation_id(record) {
                self.build_engine.cancel_approval(operation_id);
            }
        }
        state.epoch = state.epoch.saturating_add(1);
        state.secret = secret;
        state.next_operation = 0;
        state.operations.clear();
        self.build_gate.reset();
        state.gc_holder = None;
        state.gc_inhibitors.clear();
        Ok(())
    }

    /// Returns a non-authoritative diagnostic snapshot of in-memory gates.
    #[must_use]
    pub fn admission_snapshot(&self) -> AdmissionSnapshot {
        let mut state = self.lock();
        purge_expired(
            &mut state,
            Instant::now(),
            &self.build_gate,
            &self.build_engine,
        );
        AdmissionSnapshot {
            operation_count: state.operations.len(),
            build_held: self.build_gate.held(),
            gc_held: state.gc_holder.is_some(),
            gc_inhibitor_count: state.gc_inhibitors.len(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Authenticated CLI-to-broker session bound to one uid and broker epoch.
#[derive(Clone)]
pub struct AuthenticatedCaller {
    broker: Arc<InProcessBroker>,
    epoch: u64,
    uid: u32,
}

impl fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticatedCaller(<authenticated-session>)")
    }
}

impl AuthenticatedCaller {
    /// Opens a fresh opaque operation with the fixed broker lifetime.
    pub fn begin(&self, kind: BrokerOperationKind) -> Result<OperationHandle, BrokerError> {
        self.begin_with_deadline(kind, Instant::now() + OPERATION_TTL)
    }

    /// Polls a caller-bound handle without exposing internal operation data.
    pub fn poll(&self, handle: &OperationHandle) -> Result<OperationStatus, BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        let record = self.record_mut(&mut state, handle)?;
        if Instant::now() >= record.expires_at {
            record.cancellation.cancel();
            if let Some(operation_id) = build_operation_id(record) {
                self.broker.build_engine.cancel_approval(operation_id);
            }
            record.status = OperationStatus::Cancelled;
            record.prepared_build = None;
            record.awaiting_root_outputs = None;
            record.install_evidence = None;
            let executing = record.authority_executing();
            if !executing {
                release_admission(&mut state, &self.broker.build_gate, handle);
                state.operations.remove(handle);
            }
            return Err(BrokerError::new(BrokerErrorCode::OperationExpired));
        }
        Ok(record.status)
    }

    /// Authorizes one typed adapter call against a running caller-owned handle.
    ///
    /// This never executes Nix and accepts no argv or configuration. It only
    /// proves that the operation class may invoke the closed adapter method.
    pub fn authorize_adapter_call(
        &self,
        handle: &OperationHandle,
        method: crate::MethodKind,
    ) -> Result<(), BrokerError> {
        let allowed = match method {
            crate::MethodKind::Version => &BrokerOperationKind::ALL[..],
            crate::MethodKind::EvaluateDerivation => &[BrokerOperationKind::Resolve],
            crate::MethodKind::PathInfo => &[
                BrokerOperationKind::Doctor,
                BrokerOperationKind::Resolve,
                BrokerOperationKind::Acquire,
                BrokerOperationKind::Build,
                BrokerOperationKind::Activate,
                BrokerOperationKind::Gc,
                BrokerOperationKind::Repair,
            ],
            crate::MethodKind::Substitute => &[BrokerOperationKind::Acquire],
            crate::MethodKind::Build => &[BrokerOperationKind::Build],
            crate::MethodKind::Verify => &[
                BrokerOperationKind::Doctor,
                BrokerOperationKind::Acquire,
                BrokerOperationKind::Build,
                BrokerOperationKind::Repair,
            ],
            crate::MethodKind::Gc => &[BrokerOperationKind::Gc],
        };
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, allowed)
    }

    /// Verifies one caller-owned live repair handle and returns its transport UID.
    ///
    /// This is a broker-internal intent boundary. The UID is never accepted from
    /// a serialized repair request.
    pub fn authorize_repair(&self, handle: &OperationHandle) -> Result<u32, BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Repair])?;
        Ok(self.uid)
    }

    /// Durably approves one broker-derived repair plan for this repair operation.
    pub fn approve_repair_subject(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        policy_version: PolicyVersion,
        source: ApprovalSource,
        timestamp: &str,
        journal: &dyn ApprovalJournal,
    ) -> Result<BuildApprovalReceipt, BrokerError> {
        let operation_id = {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Repair])?;
            build_operation_id(self.record_mut(&mut state, handle)?)
                .cloned()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?
        };
        self.broker
            .build_engine
            .approve_subject(
                operation_id,
                digest,
                policy_version,
                source,
                timestamp,
                journal,
            )
            .map_err(|error| map_build_engine_error(error.code()))
    }

    /// Atomically consumes one repair approval after exact admission-time replan.
    pub fn consume_repair_subject(
        &self,
        handle: &OperationHandle,
        receipt: &BuildApprovalReceipt,
        digest: Digest,
        policy_version: PolicyVersion,
    ) -> Result<(), BrokerError> {
        {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Repair])?;
            let expected = build_operation_id(self.record_mut(&mut state, handle)?)
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            if expected != receipt.operation_id() {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
            }
        }
        self.broker
            .build_engine
            .consume_subject(receipt, digest, policy_version)
            .map_err(|error| map_build_engine_error(error.code()))
    }

    /// Authorizes one broker-owned signed-channel refresh operation.
    ///
    /// This capability accepts no channel, URL, target, system, or trust
    /// input. It only proves that the authenticated caller owns a live Refresh
    /// handle before the service uses its compiled trust configuration.
    pub fn authorize_channel_refresh(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Refresh])
    }

    /// Authorizes one read-only query against broker-owned catalog authority.
    ///
    /// The request can contain only bounded product query text. It cannot
    /// select an index, channel, system, source, URL, or Nix evaluation input.
    pub fn authorize_catalog_query(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Resolve])
    }

    /// Retains one broker-derived private plan and returns only its public preview.
    ///
    /// This is an in-broker API, not an RPC shape: callers never provide raw
    /// targets or a [`crate::BuildRequest`]. A build operation may prepare only
    /// one plan; a changed plan requires a fresh operation and fresh approval.
    #[cfg(test)]
    fn prepare_build(
        &self,
        handle: &OperationHandle,
        plan: BuildPlan,
    ) -> Result<BuildPreview, BrokerError> {
        self.prepare_build_inner(
            handle,
            plan,
            crate::BuildPreviewEstimates::unavailable(),
            None,
        )
    }

    /// Retains a private plan together with its in-process trusted replanner.
    ///
    /// The replanner is a capability installed by broker orchestration, never a
    /// framed request value. It is consumed only after exact approval and build
    /// admission to reconstruct current plan authority.
    pub fn prepare_build_with_replanner(
        &self,
        handle: &OperationHandle,
        plan: BuildPlan,
        replanner: Arc<dyn TrustedBuildReplanner>,
    ) -> Result<BuildPreview, BrokerError> {
        self.prepare_build_inner(
            handle,
            plan,
            crate::BuildPreviewEstimates::unavailable(),
            Some(replanner),
        )
    }

    /// Retains trusted heuristic estimates with the private plan and replanner.
    ///
    /// The estimate is installed only by in-process broker orchestration. It is
    /// shown in the public preview, retained for admission, and never accepted
    /// from the later execution request.
    pub fn prepare_build_with_replanner_and_estimates(
        &self,
        handle: &OperationHandle,
        plan: BuildPlan,
        estimates: crate::BuildPreviewEstimates,
        replanner: Arc<dyn TrustedBuildReplanner>,
    ) -> Result<BuildPreview, BrokerError> {
        self.prepare_build_inner(handle, plan, estimates, Some(replanner))
    }

    fn prepare_build_inner(
        &self,
        handle: &OperationHandle,
        plan: BuildPlan,
        estimates: crate::BuildPreviewEstimates,
        replanner: Option<Arc<dyn TrustedBuildReplanner>>,
    ) -> Result<BuildPreview, BrokerError> {
        let digest = plan
            .digest()
            .map_err(|_| BrokerError::new(BrokerErrorCode::InvalidBuildPlan))?;
        let preview = plan
            .preview_with_estimates(estimates.clone())
            .map_err(|_| BrokerError::new(BrokerErrorCode::InvalidBuildPlan))?;
        let estimate = estimates.execution_disk_estimate();
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
        let record = self.record_mut(&mut state, handle)?;
        if record.build_prepared {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        record.prepared_build = Some(PreparedBuild {
            plan,
            preview: preview.clone(),
            estimate,
            digest,
            approval: PreparedBuildApproval::Unapproved,
            replanner,
        });
        record.build_prepared = true;
        Ok(preview)
    }

    /// Returns only the sanitized preview of the private plan retained under
    /// this exact caller-bound build handle.
    pub fn build_preview(&self, handle: &OperationHandle) -> Result<BuildPreview, BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
        let record = self.record_mut(&mut state, handle)?;
        Ok(record
            .prepared_build
            .as_ref()
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?
            .preview
            .clone())
    }

    /// Durably approves the exact broker-held plan for one later execution.
    ///
    /// The journal write happens inside the broker authority boundary before a
    /// private receipt is retained. The public IPC request will carry only the
    /// opaque operation handle and digest pointer; it never carries a receipt
    /// or build targets.
    pub fn approve_build(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        source: ApprovalSource,
        timestamp: &str,
        journal: &dyn ApprovalJournal,
    ) -> Result<(), BrokerError> {
        let (plan, operation_id) = {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
            let record = self.record_mut(&mut state, handle)?;
            let operation_id = build_operation_id(record)
                .cloned()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            let prepared = record
                .prepared_build
                .as_mut()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            if prepared.digest != digest {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
            }
            if !matches!(prepared.approval, PreparedBuildApproval::Unapproved) {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable));
            }
            prepared.approval = PreparedBuildApproval::Recording;
            (prepared.plan.clone(), operation_id)
        };

        let receipt = match self.broker.build_engine.approve(
            operation_id.clone(),
            &plan,
            source,
            timestamp,
            journal,
        ) {
            Ok(receipt) => receipt,
            Err(_) => {
                let mut state = self.broker.lock();
                if self.check_epoch(&state).is_ok()
                    && let Ok(record) = self.record_mut(&mut state, handle)
                    && let Some(prepared) = record.prepared_build.as_mut()
                    && prepared.digest == digest
                    && matches!(prepared.approval, PreparedBuildApproval::Recording)
                {
                    prepared.approval = PreparedBuildApproval::Unapproved;
                }
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable));
            }
        };

        let retained = {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])
                .and_then(|()| {
                    let record = self.record_mut(&mut state, handle)?;
                    let prepared = record.prepared_build.as_mut().ok_or_else(|| {
                        BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable)
                    })?;
                    if prepared.digest != digest {
                        return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
                    }
                    if !matches!(prepared.approval, PreparedBuildApproval::Recording) {
                        return Err(BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable));
                    }
                    prepared.approval = PreparedBuildApproval::Approved { receipt };
                    Ok(())
                })
        };
        if retained.is_err() {
            self.broker.build_engine.cancel_approval(&operation_id);
        }
        retained
    }

    /// Executes the exact broker-held approved plan under build and GC admission.
    ///
    /// This remains an in-broker seam: the caller supplies a trusted replanning
    /// closure and typed adapter, while the future IPC request carries only the
    /// opaque handle and digest. The private receipt and raw targets never cross
    /// the broker boundary. Successful output remains admitted until the caller
    /// roots it and completes the operation.
    #[cfg(test)]
    pub(crate) fn execute_build(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        replan: impl FnOnce() -> Result<BuildPlan, BuildEngineError>,
        estimate: VolatileBuildEstimate,
        resources: &dyn ResourceProbe,
        adapter: &dyn NixAdapter,
    ) -> Result<BuildReport, BrokerError> {
        self.execute_build_with_progress(
            handle,
            digest,
            replan,
            estimate,
            BuildExecutionIo {
                resources,
                adapter,
                progress: &mut |_| Ok(()),
            },
        )
    }

    pub(crate) fn execute_build_with_progress(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        replan: impl FnOnce() -> Result<BuildPlan, BuildEngineError>,
        estimate: VolatileBuildEstimate,
        io: BuildExecutionIo<'_>,
    ) -> Result<BuildReport, BrokerError> {
        let (acquired, operation_cancellation) = {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
            if state.gc_holder.is_some() {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
            }
            let record = self.record_mut(&mut state, handle)?;
            let prepared = record
                .prepared_build
                .as_ref()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            if prepared.digest != digest {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
            }
            if !matches!(prepared.approval, PreparedBuildApproval::Approved { .. }) {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable));
            }
            let cancellation = Arc::clone(&record.cancellation);
            (self.broker.build_gate.enqueue(handle)?, cancellation)
        };
        if !acquired {
            self.broker.build_gate.wait(
                handle,
                &CancellationToken::default(),
                &operation_cancellation,
            )?;
        }

        let reservation = (|| {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
            if state.gc_holder.is_some() || operation_cancellation.is_cancelled() {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
            }
            let record = self.record_mut(&mut state, handle)?;
            let prepared = record
                .prepared_build
                .as_mut()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            if prepared.digest != digest {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
            }
            let receipt = match &prepared.approval {
                PreparedBuildApproval::Approved { receipt } => receipt.clone(),
                _ => return Err(BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable)),
            };
            prepared.approval = PreparedBuildApproval::Executing;
            record.build_executing = true;
            state.gc_inhibitors.insert(handle.clone());
            Ok(receipt)
        })();
        let receipt = match reservation {
            Ok(receipt) => receipt,
            Err(error) => {
                self.broker.build_gate.release(handle);
                return Err(error);
            }
        };

        let result = self.broker.build_engine.execute_with_evidence_and_progress(
            receipt,
            replan,
            estimate,
            BuildExecutionRuntime {
                resources: io.resources,
                cancellation: &operation_cancellation,
                adapter: io.adapter,
                progress: io.progress,
            },
        );
        match result {
            Ok((report, evidence)) => self.finish_build_execution(handle, report, evidence),
            Err(error) => {
                self.fail_build_execution(handle);
                Err(map_build_engine_error(error.code()))
            }
        }
    }

    /// Runs one trusted cache-first acquisition while preventing concurrent GC.
    ///
    /// The callback is installed only by broker orchestration. A cache hit
    /// retains its evidence and GC inhibitor until exact roots are published.
    /// A cache miss releases admission and returns `BuildRequired` without
    /// creating evidence or approving a build.
    ///
    /// # Errors
    ///
    /// Refuses the wrong operation class, concurrent GC, repeated acquisition,
    /// lifecycle cancellation, invalid evidence, or a failed trusted callback.
    pub fn acquire_cache_install(
        &self,
        handle: &OperationHandle,
        acquire: impl FnOnce() -> Result<CacheInstallAttempt, ()>,
    ) -> Result<CacheInstallOutcome, BrokerError> {
        {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Acquire])?;
            if state.gc_holder.is_some() {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
            }
            let record = self.record_mut(&mut state, handle)?;
            if record.cache_acquiring
                || record.awaiting_root_outputs.is_some()
                || record.install_evidence.is_some()
            {
                return Err(BrokerError::new(
                    BrokerErrorCode::InvalidAdmissionTransition,
                ));
            }
            record.cache_acquiring = true;
            state.gc_inhibitors.insert(handle.clone());
        }

        match acquire() {
            Ok(CacheInstallAttempt::Acquired(evidence)) => {
                self.finish_cache_acquisition(handle, evidence)
            }
            Ok(CacheInstallAttempt::BuildRequired) => match self.finish_cache_miss(handle) {
                Ok(()) => Ok(CacheInstallOutcome::BuildRequired),
                Err(error) => {
                    self.fail_cache_acquisition(handle);
                    Err(error)
                }
            },
            Err(()) => {
                self.fail_cache_acquisition(handle);
                Err(BrokerError::new(BrokerErrorCode::CacheAcquisitionFailed))
            }
        }
    }

    /// Returns the immutable post-acquisition evidence retained for this install.
    ///
    /// Evidence exists only after successful cache acquisition or build
    /// execution and before root publication completes the operation.
    pub fn install_evidence(
        &self,
        handle: &OperationHandle,
    ) -> Result<InstallEvidence, BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(
            &mut state,
            handle,
            &[BrokerOperationKind::Acquire, BrokerOperationKind::Build],
        )?;
        let record = self.record_mut(&mut state, handle)?;
        if record.build_executing
            || record.cache_acquiring
            || record.awaiting_root_outputs.is_none()
        {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        record
            .install_evidence
            .clone()
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidAdmissionTransition))
    }

    /// Executes using only the replanner retained with the approved plan.
    ///
    /// This is the dispatcher-facing path: no caller-provided replan closure is
    /// accepted. A missing or failed retained capability invalidates approval
    /// before the managed adapter can build.
    pub fn execute_prepared_build(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        resources: &dyn ResourceProbe,
        adapter: &dyn NixAdapter,
    ) -> Result<BuildReport, BrokerError> {
        self.execute_prepared_build_with_progress(handle, digest, resources, adapter, &mut |_| {
            Ok(())
        })
    }

    /// Executes a retained approved plan and emits sanitized live estimates.
    ///
    /// The callback receives only fixed-point completion values. Private Nix
    /// activity identifiers, derivations, store paths, and log text remain
    /// inside the managed adapter.
    pub fn execute_prepared_build_with_progress(
        &self,
        handle: &OperationHandle,
        digest: Digest,
        resources: &dyn ResourceProbe,
        adapter: &dyn NixAdapter,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, BrokerError> {
        let (replanner, estimate) = {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Build])?;
            let prepared = self
                .record_mut(&mut state, handle)?
                .prepared_build
                .as_ref()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            if prepared.digest != digest {
                return Err(BrokerError::new(BrokerErrorCode::BuildApprovalMismatch));
            }
            let replanner = prepared
                .replanner
                .clone()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildApprovalUnavailable))?;
            let estimate = prepared
                .estimate
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::BuildResourcePreflightFailed))?;
            (replanner, estimate)
        };
        self.execute_build_with_progress(
            handle,
            digest,
            move || {
                replanner
                    .replan()
                    .map_err(|_| BuildEngineError::approval_invalidated())
            },
            estimate,
            BuildExecutionIo {
                resources,
                adapter,
                progress,
            },
        )
    }

    /// Publishes a complete generation root set while retaining GC protection.
    ///
    /// The root set must belong to the transport-authenticated caller and
    /// match every output returned by successful cache acquisition or build
    /// execution exactly once. The helper callback runs outside the broker
    /// mutex, but cancellation and disconnect defer admission release until it
    /// returns, preventing GC from racing root publication.
    ///
    /// # Errors
    ///
    /// Refuses a non-install handle, an install without retained acquired
    /// outputs, caller/output mismatch, lifecycle cancellation, or helper
    /// failure.
    pub fn publish_built_root_set(
        &self,
        handle: &OperationHandle,
        root_set: &RootSet,
        publish: impl FnOnce(&RootSet) -> Result<RootSetReport, MaintenanceError>,
    ) -> Result<RootSetReport, BrokerError> {
        {
            let mut state = self.broker.lock();
            self.require_running_kind(
                &mut state,
                handle,
                &[BrokerOperationKind::Acquire, BrokerOperationKind::Build],
            )?;
            if root_set.owner_uid() != self.uid {
                return Err(BrokerError::new(
                    BrokerErrorCode::InvalidAdmissionTransition,
                ));
            }
            let root_targets = root_set
                .entries()
                .iter()
                .map(|entry| entry.target().as_str().to_owned())
                .collect::<BTreeSet<_>>();
            let record = self.record_mut(&mut state, handle)?;
            let required = record
                .awaiting_root_outputs
                .as_ref()
                .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidAdmissionTransition))?;
            if required != &root_targets
                || root_set.entries().len() != required.len()
                || record.build_executing
                || record.cache_acquiring
            {
                return Err(BrokerError::new(
                    BrokerErrorCode::InvalidAdmissionTransition,
                ));
            }
            record.build_executing = true;
        }

        let publication = publish(root_set).and_then(|report| {
            let expected = format!(
                "/nix/var/nix/gcroots/pkg/users/{}/{}",
                root_set.owner_uid(),
                root_set.generation().as_str()
            );
            if report.reference().as_str() != expected
                || report.entry_count() != root_set.entries().len()
                || report.mapping_digest() != root_set.mapping_digest()
            {
                return Err(MaintenanceError::backend_failure());
            }
            Ok(report)
        });
        self.finish_root_publication(handle, publication)
    }

    /// Injects the authenticated caller uid before protected root publication.
    ///
    /// # Errors
    ///
    /// Refuses invalid promoted intent or any protected publication failure.
    pub fn publish_built_root_intent(
        &self,
        handle: &OperationHandle,
        intent: RootSetIntent,
        publish: impl FnOnce(&RootSet) -> Result<RootSetReport, MaintenanceError>,
    ) -> Result<RootSetReport, BrokerError> {
        let root_set = intent
            .into_root_set(self.uid)
            .map_err(|_| BrokerError::new(BrokerErrorCode::InvalidAdmissionTransition))?;
        self.publish_built_root_set(handle, &root_set, publish)
    }

    /// Promotes an ownerless generation transition and executes it under Activate authority.
    ///
    /// The shared GC inhibitor remains held after a successful helper transition. The caller
    /// must commit its local generation state and then call [`Self::complete`] to release it.
    /// A successful transition callback adds the destination roots but retains
    /// the source generation roots for rollback. Cancellation, expiry,
    /// disconnect, and restart release only admission and never remove either
    /// retained generation; generation pruning owns eventual root removal.
    pub fn transition_root_intent(
        &self,
        handle: &OperationHandle,
        intent: RootSetTransitionIntent,
        transition: impl FnOnce(
            RootSetTransitionRequest,
        ) -> Result<RootSetTransitionReport, MaintenanceError>,
    ) -> Result<RootSetTransitionReport, BrokerError> {
        let request = intent
            .into_request(self.uid)
            .map_err(|_| BrokerError::new(BrokerErrorCode::InvalidAdmissionTransition))?;
        {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Activate])?;
            if state.gc_holder.is_some() || state.gc_inhibitors.contains(handle) {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
            }
            let record = self.record_mut(&mut state, handle)?;
            if record.authority_executing() {
                return Err(BrokerError::new(
                    BrokerErrorCode::InvalidAdmissionTransition,
                ));
            }
            record.root_transition_executing = true;
            state.gc_inhibitors.insert(handle.clone());
        }

        let result = transition(request);
        self.finish_root_transition(handle, result)
    }

    /// Attests an already durable generation under fresh Activate authority.
    ///
    /// The request contains only the authenticated uid and typed generation.
    /// A shared GC inhibitor remains held after success until local state is
    /// committed and [`Self::complete`] releases admission.
    pub fn attest_generation_root_intent(
        &self,
        handle: &OperationHandle,
        generation: crate::GenerationId,
        attest: impl FnOnce(&RootSetAttestationRequest) -> Result<RootSetReport, MaintenanceError>,
    ) -> Result<RootSetReport, BrokerError> {
        let request = RootSetAttestationRequest::new(self.uid, generation);
        {
            let mut state = self.broker.lock();
            self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Activate])?;
            if state.gc_holder.is_some() || state.gc_inhibitors.contains(handle) {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
            }
            let record = self.record_mut(&mut state, handle)?;
            if record.authority_executing() {
                return Err(BrokerError::new(
                    BrokerErrorCode::InvalidAdmissionTransition,
                ));
            }
            record.root_transition_executing = true;
            state.gc_inhibitors.insert(handle.clone());
        }

        let result = attest(&request).and_then(|report| {
            let expected = format!(
                "/nix/var/nix/gcroots/pkg/users/{}/{}",
                request.owner_uid(),
                request.generation().as_str()
            );
            if report.reference().as_str() != expected || report.entry_count() == 0 {
                return Err(MaintenanceError::backend_failure());
            }
            Ok(report)
        });
        self.finish_root_attestation(handle, result)
    }

    /// Acquires the machine-wide local-build lease for this operation.
    pub fn acquire_build(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(
            &mut state,
            handle,
            &[BrokerOperationKind::Build, BrokerOperationKind::Repair],
        )?;
        if state.gc_holder.is_some() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        self.broker.build_gate.try_acquire(handle)
    }

    /// Waits in FIFO order for machine-wide build admission or cancellation.
    pub fn acquire_build_wait(
        &self,
        handle: &OperationHandle,
        cancellation: &CancellationToken,
    ) -> Result<(), BrokerError> {
        let (acquired, operation_cancellation) = {
            let mut state = self.broker.lock();
            self.require_running_kind(
                &mut state,
                handle,
                &[BrokerOperationKind::Build, BrokerOperationKind::Repair],
            )?;
            if state.gc_holder.is_some() {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
            }
            if cancellation.is_cancelled() {
                return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
            }
            let operation_cancellation =
                Arc::clone(&self.record_mut(&mut state, handle)?.cancellation);
            (
                self.broker.build_gate.enqueue(handle)?,
                operation_cancellation,
            )
        };
        if !acquired {
            self.broker
                .build_gate
                .wait(handle, cancellation, &operation_cancellation)?;
        } else if cancellation.is_cancelled() || operation_cancellation.is_cancelled() {
            self.broker.build_gate.release(handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let validation = {
            let mut state = self.broker.lock();
            self.require_running_kind(
                &mut state,
                handle,
                &[BrokerOperationKind::Build, BrokerOperationKind::Repair],
            )
            .and_then(|()| {
                if state.gc_holder.is_some() {
                    Err(BrokerError::new(BrokerErrorCode::AdmissionBusy))
                } else {
                    Ok(())
                }
            })
        };
        if validation.is_err() {
            self.broker.build_gate.release(handle);
        }
        validation
    }

    /// Acquires one shared permit that prevents garbage collection.
    pub fn acquire_gc_inhibit(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(
            &mut state,
            handle,
            &[
                BrokerOperationKind::Build,
                BrokerOperationKind::Activate,
                BrokerOperationKind::Repair,
            ],
        )?;
        if state.gc_holder.is_some() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        state.gc_inhibitors.insert(handle.clone());
        Ok(())
    }

    /// Acquires exclusive garbage-collection admission.
    pub fn acquire_gc(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Gc])?;
        if state.gc_holder.as_ref() == Some(handle) {
            return Ok(());
        }
        if state.gc_holder.is_some()
            || !state.gc_inhibitors.is_empty()
            || self.broker.build_gate.blocks_gc()
        {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        state.gc_holder = Some(handle.clone());
        Ok(())
    }

    /// Waits until exclusive GC admission is available for this live handle.
    ///
    /// The short bounded polling interval releases the broker state mutex on
    /// every retry, allowing other connections to finish or cancel their
    /// realize-to-root inhibitors. Operation expiry and cancellation are
    /// re-checked on every attempt.
    pub fn acquire_gc_wait(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        loop {
            match self.acquire_gc(handle) {
                Ok(()) => return Ok(()),
                Err(error) if error.code() == BrokerErrorCode::AdmissionBusy => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Removes one caller-owned generation root set while retaining exclusive
    /// GC admission on the operation until completion or cancellation.
    ///
    /// The public request supplies only a typed generation id. Caller identity
    /// is injected from the authenticated session and raw root paths never
    /// cross the CLI boundary.
    pub fn remove_generation_root_intent(
        &self,
        handle: &OperationHandle,
        generation: crate::GenerationId,
        remove: impl FnOnce(&crate::RemoveRootSetRequest) -> Result<(), MaintenanceError>,
    ) -> Result<(), BrokerError> {
        self.acquire_gc_wait(handle)?;
        let request = crate::RemoveRootSetRequest::new(self.uid, generation);
        remove(&request).map_err(|_| BrokerError::new(BrokerErrorCode::RootPublicationFailed))
    }

    /// Completes an operation and releases every admission gate it holds.
    pub fn complete(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        self.finish(handle, OperationStatus::Completed)
    }

    /// Cancels an operation and releases every admission gate it holds.
    pub fn cancel(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        self.finish(handle, OperationStatus::Cancelled)
    }

    /// Simulates CLI disconnect: every running operation owned by this caller
    /// is cancelled and all of its admission is released.
    pub fn disconnect(&self) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        let handles = state
            .operations
            .iter()
            .filter(|(_, record)| {
                record.owner_uid == self.uid && record.status == OperationStatus::Running
            })
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in handles {
            let executing = if let Some(record) = state.operations.get_mut(&handle) {
                record.cancellation.cancel();
                if let Some(operation_id) = build_operation_id(record) {
                    self.broker.build_engine.cancel_approval(operation_id);
                }
                record.status = OperationStatus::Cancelled;
                record.prepared_build = None;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
                record.authority_executing()
            } else {
                false
            };
            if !executing {
                release_admission(&mut state, &self.broker.build_gate, &handle);
            }
        }
        Ok(())
    }

    fn begin_with_deadline(
        &self,
        kind: BrokerOperationKind,
        expires_at: Instant,
    ) -> Result<OperationHandle, BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        purge_expired(
            &mut state,
            Instant::now(),
            &self.broker.build_gate,
            &self.broker.build_engine,
        );
        state.next_operation = state.next_operation.saturating_add(1);
        let handle = mint_handle(&state, self.uid, kind);
        let build_operation_id = if matches!(
            kind,
            BrokerOperationKind::Build | BrokerOperationKind::Repair
        ) {
            Some(mint_build_operation_id(&handle)?)
        } else {
            None
        };
        state.operations.insert(
            handle.clone(),
            OperationRecord {
                owner_uid: self.uid,
                kind,
                status: OperationStatus::Running,
                expires_at,
                build_prepared: false,
                build_operation_id,
                prepared_build: None,
                cancellation: Arc::new(CancellationToken::default()),
                build_executing: false,
                cache_acquiring: false,
                root_transition_executing: false,
                awaiting_root_outputs: None,
                install_evidence: None,
            },
        );
        Ok(handle)
    }

    fn finish(&self, handle: &OperationHandle, status: OperationStatus) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running(&mut state, handle)?;
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        if status == OperationStatus::Completed
            && (record.authority_executing() || record.awaiting_root_outputs.is_some())
        {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        record.cancellation.cancel();
        if let Some(operation_id) = build_operation_id(record) {
            self.broker.build_engine.cancel_approval(operation_id);
        }
        record.status = status;
        record.prepared_build = None;
        record.awaiting_root_outputs = None;
        record.install_evidence = None;
        let executing = record.authority_executing();
        if !executing {
            release_admission(&mut state, &self.broker.build_gate, handle);
        }
        Ok(())
    }

    fn finish_build_execution(
        &self,
        handle: &OperationHandle,
        report: BuildReport,
        evidence: InstallEvidence,
    ) -> Result<BuildReport, BrokerError> {
        let output_paths = report
            .outputs()
            .iter()
            .map(|output| output.store_path().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            self.broker.build_gate.release(handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let running = state.operations.get(handle).is_some_and(|record| {
            record.owner_uid == self.uid
                && record.status == OperationStatus::Running
                && record.build_executing
                && !record.cancellation.is_cancelled()
        });
        if !running {
            if let Some(record) = state.operations.get_mut(handle) {
                record.build_executing = false;
                record.prepared_build = None;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
            }
            release_admission(&mut state, &self.broker.build_gate, handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        record.build_executing = false;
        record.prepared_build = None;
        if output_paths.is_empty() {
            record.cancellation.cancel();
            record.status = OperationStatus::Completed;
            release_admission(&mut state, &self.broker.build_gate, handle);
        } else {
            record.awaiting_root_outputs = Some(output_paths);
            record.install_evidence = Some(evidence);
        }
        Ok(report)
    }

    fn finish_cache_acquisition(
        &self,
        handle: &OperationHandle,
        evidence: InstallEvidence,
    ) -> Result<CacheInstallOutcome, BrokerError> {
        let output_paths = evidence
            .targets()
            .iter()
            .flat_map(|target| target.acquired())
            .map(|output| output.path_info().store_path().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let cache_only = evidence.targets().iter().all(|target| {
            target
                .acquired()
                .iter()
                .all(|output| output.provenance() == crate::BuildOutputProvenance::CacheSigned)
        });
        if output_paths.is_empty() || !cache_only {
            self.fail_cache_acquisition(handle);
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            release_admission(&mut state, &self.broker.build_gate, handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let running = state.operations.get(handle).is_some_and(|record| {
            record.owner_uid == self.uid
                && record.status == OperationStatus::Running
                && record.cache_acquiring
                && !record.cancellation.is_cancelled()
        });
        if !running {
            if let Some(record) = state.operations.get_mut(handle) {
                record.cache_acquiring = false;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
            }
            release_admission(&mut state, &self.broker.build_gate, handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        record.cache_acquiring = false;
        record.awaiting_root_outputs = Some(output_paths);
        record.install_evidence = Some(evidence);
        Ok(CacheInstallOutcome::Acquired)
    }

    fn finish_cache_miss(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Acquire])?;
        let record = self.record_mut(&mut state, handle)?;
        if !record.cache_acquiring || record.cancellation.is_cancelled() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        record.cache_acquiring = false;
        release_admission(&mut state, &self.broker.build_gate, handle);
        Ok(())
    }

    fn fail_cache_acquisition(&self, handle: &OperationHandle) {
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_ok()
            && let Some(record) = state.operations.get_mut(handle)
            && record.owner_uid == self.uid
        {
            record.cache_acquiring = false;
            record.awaiting_root_outputs = None;
            record.install_evidence = None;
        }
        release_admission(&mut state, &self.broker.build_gate, handle);
    }

    fn finish_root_publication(
        &self,
        handle: &OperationHandle,
        publication: Result<RootSetReport, MaintenanceError>,
    ) -> Result<RootSetReport, BrokerError> {
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            self.broker.build_gate.release(handle);
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let completed = state.operations.get(handle).is_some_and(|record| {
            record.owner_uid == self.uid
                && record.status == OperationStatus::Running
                && record.build_executing
                && !record.cancellation.is_cancelled()
                && publication.is_ok()
        });
        if let Some(record) = state.operations.get_mut(handle)
            && record.owner_uid == self.uid
        {
            record.cancellation.cancel();
            record.status = if completed {
                OperationStatus::Completed
            } else {
                OperationStatus::Cancelled
            };
            record.prepared_build = None;
            record.build_executing = false;
            record.awaiting_root_outputs = None;
            record.install_evidence = None;
        }
        release_admission(&mut state, &self.broker.build_gate, handle);
        if !completed {
            return Err(BrokerError::new(if publication.is_err() {
                BrokerErrorCode::RootPublicationFailed
            } else {
                BrokerErrorCode::AdmissionCancelled
            }));
        }
        publication.map_err(|_| BrokerError::new(BrokerErrorCode::RootPublicationFailed))
    }

    fn finish_root_transition(
        &self,
        handle: &OperationHandle,
        transition: Result<RootSetTransitionReport, MaintenanceError>,
    ) -> Result<RootSetTransitionReport, BrokerError> {
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let succeeded = state.operations.get(handle).is_some_and(|record| {
            record.owner_uid == self.uid
                && record.status == OperationStatus::Running
                && record.root_transition_executing
                && !record.cancellation.is_cancelled()
                && transition.is_ok()
        });
        if let Some(record) = state.operations.get_mut(handle)
            && record.owner_uid == self.uid
        {
            record.root_transition_executing = false;
            if !succeeded {
                record.cancellation.cancel();
                record.status = OperationStatus::Cancelled;
                record.prepared_build = None;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
            }
        }
        if !succeeded {
            release_admission(&mut state, &self.broker.build_gate, handle);
            return Err(BrokerError::new(if transition.is_err() {
                BrokerErrorCode::RootPublicationFailed
            } else {
                BrokerErrorCode::AdmissionCancelled
            }));
        }
        transition.map_err(|_| BrokerError::new(BrokerErrorCode::RootPublicationFailed))
    }

    fn finish_root_attestation(
        &self,
        handle: &OperationHandle,
        attestation: Result<RootSetReport, MaintenanceError>,
    ) -> Result<RootSetReport, BrokerError> {
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionCancelled));
        }
        let succeeded = state.operations.get(handle).is_some_and(|record| {
            record.owner_uid == self.uid
                && record.status == OperationStatus::Running
                && record.root_transition_executing
                && !record.cancellation.is_cancelled()
                && attestation.is_ok()
        });
        if let Some(record) = state.operations.get_mut(handle)
            && record.owner_uid == self.uid
        {
            record.root_transition_executing = false;
            if !succeeded {
                record.cancellation.cancel();
                record.status = OperationStatus::Cancelled;
                record.prepared_build = None;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
            }
        }
        if !succeeded {
            release_admission(&mut state, &self.broker.build_gate, handle);
            return Err(BrokerError::new(if attestation.is_err() {
                BrokerErrorCode::RootPublicationFailed
            } else {
                BrokerErrorCode::AdmissionCancelled
            }));
        }
        attestation.map_err(|_| BrokerError::new(BrokerErrorCode::RootPublicationFailed))
    }

    fn fail_build_execution(&self, handle: &OperationHandle) {
        let mut state = self.broker.lock();
        if self.check_epoch(&state).is_err() {
            self.broker.build_gate.release(handle);
            return;
        }
        if let Some(record) = state.operations.get_mut(handle)
            && record.owner_uid == self.uid
        {
            record.cancellation.cancel();
            if let Some(operation_id) = build_operation_id(record) {
                self.broker.build_engine.cancel_approval(operation_id);
            }
            record.status = OperationStatus::Cancelled;
            record.prepared_build = None;
            record.build_executing = false;
            record.awaiting_root_outputs = None;
            record.install_evidence = None;
        }
        release_admission(&mut state, &self.broker.build_gate, handle);
    }

    fn check_epoch(&self, state: &BrokerState) -> Result<(), BrokerError> {
        if state.epoch == self.epoch {
            Ok(())
        } else {
            Err(BrokerError::new(BrokerErrorCode::SessionRestarted))
        }
    }

    fn record_mut<'a>(
        &self,
        state: &'a mut BrokerState,
        handle: &OperationHandle,
    ) -> Result<&'a mut OperationRecord, BrokerError> {
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        if record.owner_uid != self.uid {
            return Err(BrokerError::new(BrokerErrorCode::InvalidOperationHandle));
        }
        Ok(record)
    }

    fn require_running(
        &self,
        state: &mut BrokerState,
        handle: &OperationHandle,
    ) -> Result<(), BrokerError> {
        self.check_epoch(state)?;
        purge_expired(
            state,
            Instant::now(),
            &self.broker.build_gate,
            &self.broker.build_engine,
        );
        let record = self.record_mut(state, handle)?;
        if record.status != OperationStatus::Running {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        Ok(())
    }

    fn require_running_kind(
        &self,
        state: &mut BrokerState,
        handle: &OperationHandle,
        allowed: &[BrokerOperationKind],
    ) -> Result<(), BrokerError> {
        self.require_running(state, handle)?;
        let record = self.record_mut(state, handle)?;
        if allowed.contains(&record.kind) {
            Ok(())
        } else {
            Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ))
        }
    }
}

fn release_admission(
    state: &mut BrokerState,
    build_gate: &FairBuildGate,
    handle: &OperationHandle,
) {
    build_gate.release(handle);
    if state.gc_holder.as_ref() == Some(handle) {
        state.gc_holder = None;
    }
    state.gc_inhibitors.remove(handle);
}

fn purge_expired(
    state: &mut BrokerState,
    now: Instant,
    build_gate: &FairBuildGate,
    build_engine: &LocalBuildEngine,
) {
    let expired = state
        .operations
        .iter()
        .filter(|(_, record)| now >= record.expires_at)
        .map(|(handle, _)| handle.clone())
        .collect::<Vec<_>>();
    for handle in expired {
        let executing = state
            .operations
            .get(&handle)
            .is_some_and(OperationRecord::authority_executing);
        if executing {
            if let Some(record) = state.operations.get_mut(&handle) {
                record.cancellation.cancel();
                if let Some(operation_id) = build_operation_id(record) {
                    build_engine.cancel_approval(operation_id);
                }
                record.status = OperationStatus::Cancelled;
                record.prepared_build = None;
                record.awaiting_root_outputs = None;
                record.install_evidence = None;
            }
            continue;
        }
        release_admission(state, build_gate, &handle);
        if let Some(record) = state.operations.remove(&handle) {
            record.cancellation.cancel();
            if let Some(operation_id) = build_operation_id(&record) {
                build_engine.cancel_approval(operation_id);
            }
        }
    }
}

fn map_build_engine_error(code: BuildEngineErrorCode) -> BrokerError {
    let code = match code {
        BuildEngineErrorCode::Cancelled => BrokerErrorCode::AdmissionCancelled,
        BuildEngineErrorCode::ApprovalInvalidated => BrokerErrorCode::BuildApprovalInvalidated,
        BuildEngineErrorCode::ApprovalRequired | BuildEngineErrorCode::ApprovalUnavailable => {
            BrokerErrorCode::BuildApprovalUnavailable
        }
        BuildEngineErrorCode::ResourcePreflightFailed => {
            BrokerErrorCode::BuildResourcePreflightFailed
        }
        BuildEngineErrorCode::BuildFailed | BuildEngineErrorCode::AcquireNoBinary => {
            BrokerErrorCode::BuildExecutionFailed
        }
        BuildEngineErrorCode::BuildDenied
        | BuildEngineErrorCode::InvalidPlan
        | BuildEngineErrorCode::ReadinessFailed
        | BuildEngineErrorCode::JournalFailed => BrokerErrorCode::InvalidBuildPlan,
    };
    BrokerError::new(code)
}

fn build_operation_id(record: &OperationRecord) -> Option<&OperationId> {
    record.build_operation_id.as_ref()
}

fn mint_build_operation_id(handle: &OperationHandle) -> Result<OperationId, BrokerError> {
    let token = handle
        .as_str()
        .strip_prefix("op_")
        .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
    let token = token
        .get(..58)
        .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
    OperationId::new(&format!("build_{token}"))
        .map_err(|_| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))
}

fn mint_handle(state: &BrokerState, uid: u32, kind: BrokerOperationKind) -> OperationHandle {
    let mut hasher = Sha256::new();
    hasher.update(state.secret);
    hasher.update(state.epoch.to_be_bytes());
    hasher.update(state.next_operation.to_be_bytes());
    hasher.update(uid.to_be_bytes());
    hasher.update([kind as u8]);
    let bytes: [u8; 32] = hasher.finalize().into();
    let mut token = String::from("op_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    OperationHandle(token)
}

/// Fixed environment and absolute executable policy for bundled Nix children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildContainmentPolicy {
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    cancel_grace: Duration,
}

impl ChildContainmentPolicy {
    /// Constructs the fixed child policy for one bundled Nix executable.
    pub fn new(
        executable: impl Into<PathBuf>,
        private_home: impl Into<PathBuf>,
    ) -> Result<Self, BrokerError> {
        let executable = executable.into();
        let private_home = private_home.into();
        if !is_managed_nix_executable(&executable) || !is_managed_broker_home(&private_home) {
            return Err(BrokerError::new(BrokerErrorCode::InvalidChildPolicy));
        }
        let private_home = private_home
            .to_str()
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidChildPolicy))?;
        let environment = BTreeMap::from([
            ("HOME".to_owned(), private_home.to_owned()),
            (
                "NIX_CONFIG".to_owned(),
                "include /opt/pkg/etc/pkg/nix.conf".to_owned(),
            ),
            (
                "NIX_DAEMON_SOCKET_PATH".to_owned(),
                "/nix/var/nix/daemon-socket/socket".to_owned(),
            ),
            ("NIX_REMOTE".to_owned(), "daemon".to_owned()),
            ("NIX_STATE_DIR".to_owned(), "/nix/var/nix".to_owned()),
            ("NIX_USER_CONF_FILES".to_owned(), String::new()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("TMPDIR".to_owned(), format!("{private_home}/tmp")),
        ]);
        Ok(Self {
            executable,
            environment,
            cancel_grace: Duration::from_secs(5),
        })
    }

    /// Returns the absolute bundled executable; callers cannot replace it per operation.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the complete child environment after `env_clear`.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns the fixed SIGTERM-before-SIGKILL grace for the child group.
    #[must_use]
    pub const fn cancel_grace(&self) -> Duration {
        self.cancel_grace
    }

    /// Returns whether launchers must create and terminate a distinct process group.
    #[must_use]
    pub const fn terminate_process_group(&self) -> bool {
        true
    }
}

fn is_managed_broker_home(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some("/var/lib/pkg/broker-home" | "/Library/Application Support/pkg/broker-home")
    )
}

fn is_managed_nix_executable(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => components.push(value),
            _ => return false,
        }
    }
    components.len() == 6
        && components[0] == "opt"
        && components[1] == "pkg"
        && components[2] == "nix"
        && components[3]
            .to_str()
            .is_some_and(|version| crate::NixVersion::new(version).is_ok())
        && components[4] == "bin"
        && components[5] == "nix"
}

#[cfg(test)]
mod tests {
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
        DerivationPath, DerivationPlanReport, EvaluateDerivationRequest, EvaluatedDerivation,
        GcReport, GenerationId, InProcessHelper, InProcessPeer, MaintenanceAdapter, NarIntegrity,
        NixAdapterError, NixVersion, PathInfoReport, PathVerifyResult, ResourceSnapshot, RootName,
        RootRef, RootSetEntry, SubstituteReport, TrustStatus, VerifyReport, VerifyRequest,
        VersionInfo,
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
                .unwrap_or_else(|poisoned| poisoned.into_inner())
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
                .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let preview = caller.prepare_build(&handle, plan.clone()).unwrap();
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
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let wrong_schema =
            encoded_evidence.replacen("\"schemaVersion\":1", "\"schemaVersion\":2", 1);
        assert!(InstallEvidence::from_json_bytes(wrong_schema.as_bytes()).is_err());
        let extended = encoded_evidence.replacen("{", "{\"futureField\":true,", 1);
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
        caller
            .publish_built_root_set(&handle, &built_root_set(1001), |roots| {
                maintenance.publish_root_set(roots)
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
            .attest_generation_root_intent(
                &handle,
                GenerationId::new("gen-0007").unwrap(),
                |request| {
                    assert_eq!(request.owner_uid(), 1001);
                    assert_eq!(request.generation().as_str(), "gen-0007");
                    Ok(RootSetReport::new(
                        RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
                        1,
                        Digest::from_bytes([0x35; 32]),
                    ))
                },
            )
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
                crate::BuildPreviewEstimates::new(None, Some(100), None).unwrap(),
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
                crate::BuildPreviewEstimates::new(None, Some(100), None).unwrap(),
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
}
