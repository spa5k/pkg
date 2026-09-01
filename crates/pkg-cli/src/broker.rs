//! Fail-closed client for the private broker lifecycle protocol.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use pkg_core::PackageSelector;
use pkg_nix::{
    ApprovalSource, BrokerOperationKind, BuildApprovalRequest, BuildExecutionErrorCode,
    BuildPreparationErrorCode, BuildPreview, BuildProgressEstimate, BuildReport, BuildRequest,
    BuildRootPublicationErrorCode, CacheInstallErrorCode, CacheInstallOutcome,
    ChannelRefreshReport, CliBrokerRequest, CliBrokerResponse, DerivationPlanReport, Digest,
    EvaluateDerivationRequest, GcReport, GenerationId, GenerationRootAttestationErrorCode,
    GenerationRootRemovalErrorCode, GenerationRootTransitionErrorCode, InstallDownloadProgress,
    MethodKind, NixAdapter, NixAdapterError, NixAdapterErrorCode, OperationHandle, OperationStatus,
    PathInfoReport, ProductFrameCodec, RepairGenerationErrorCode, RepairGenerationReport,
    RepairGenerationRequest, RootSetIntent, RootSetReport, RootSetTransitionIntent,
    RootSetTransitionReport, StorePath, SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use socket2::{Domain, SockAddr, Socket, Type};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const LONG_RUNNING_RESPONSE_TIMEOUT: Duration = Duration::from_secs(31 * 60);

#[cfg(target_os = "linux")]
const DEFAULT_BROKER_SOCKET: &str = "/run/pkg/broker.sock";
#[cfg(target_os = "macos")]
const DEFAULT_BROKER_SOCKET: &str = "/Library/Application Support/pkg/run/broker/broker.sock";

/// Stable private-broker connector failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerClientErrorCode {
    /// This operating system has no V1 broker endpoint.
    UnsupportedPlatform,
    /// The installed broker endpoint could not be connected.
    Unavailable,
    /// Bounded stream I/O failed or ended unexpectedly.
    TransportFailure,
    /// The peer returned an invalid product frame.
    InvalidFrame,
    /// The response id or response kind did not match the request.
    UnexpectedResponse,
    /// The connection exhausted its nonzero request-id space.
    RequestIdExhausted,
    /// A prior protocol or transport failure made reuse unsafe.
    ConnectionFailed,
    /// The authenticated broker returned a closed adapter failure code.
    AdapterFailure,
    /// Broker-owned build execution returned a stable refusal code.
    BuildRefused,
    /// Broker-owned build preparation returned a stable refusal code.
    BuildPreparationRefused,
    /// Protected post-build root publication returned a stable refusal code.
    BuildRootRefused,
    /// Protected generation root transition returned a stable refusal code.
    GenerationRootTransitionRefused,
    /// Protected generation root removal returned a stable refusal code.
    GenerationRootRemovalRefused,
    /// Protected generation root attestation returned a stable refusal code.
    GenerationRootAttestationRefused,
    /// Cache-first installation returned a stable refusal code.
    InstallAcquisitionRefused,
    /// Signed channel/index bytes could not be acquired.
    ChannelRefreshNetwork,
    /// Signed channel/index verification refused publication.
    ChannelRefreshVerification,
    /// Another process owns the durable channel writer lease.
    ChannelRefreshBusy,
    /// Durable channel state or atomic authority publication is unavailable.
    ChannelRefreshServiceUnavailable,
    /// The broker-owned authenticated catalog was unavailable or refused the query.
    CatalogQueryRefused,
    /// The broker rejected the selected generation or derived repair scope.
    RepairInvalidScope,
    /// Read-only repair verification failed.
    RepairVerifyFailed,
    /// Repair admission was unavailable or invalid.
    RepairAdmissionFailed,
    /// The privileged repair helper failed or refused the request.
    RepairHelperFailed,
    /// Durable repair journaling failed.
    RepairJournalFailed,
    /// Damage remained after a reported repair.
    RepairStillDamaged,
    /// Repair needs a new explicit local-build approval.
    RepairFreshApprovalRequired,
    /// The broker has no usable production repair authority.
    RepairAuthorityUnavailable,
}

/// Redacted failure from the private broker connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerClientError {
    code: BrokerClientErrorCode,
    build_preparation_code: Option<BuildPreparationErrorCode>,
    adapter_code: Option<NixAdapterErrorCode>,
    build_execution_code: Option<BuildExecutionErrorCode>,
    build_root_code: Option<BuildRootPublicationErrorCode>,
    generation_root_transition_code: Option<GenerationRootTransitionErrorCode>,
    generation_root_removal_code: Option<GenerationRootRemovalErrorCode>,
    generation_root_attestation_code: Option<GenerationRootAttestationErrorCode>,
    cache_install_code: Option<CacheInstallErrorCode>,
}

impl BrokerClientError {
    const fn new(code: BrokerClientErrorCode) -> Self {
        Self {
            code,
            build_preparation_code: None,
            adapter_code: None,
            build_execution_code: None,
            build_root_code: None,
            generation_root_transition_code: None,
            generation_root_removal_code: None,
            generation_root_attestation_code: None,
            cache_install_code: None,
        }
    }

    const fn adapter_failure(adapter_code: NixAdapterErrorCode) -> Self {
        Self {
            adapter_code: Some(adapter_code),
            ..Self::new(BrokerClientErrorCode::AdapterFailure)
        }
    }

    const fn build_preparation_refused(code: BuildPreparationErrorCode) -> Self {
        Self {
            build_preparation_code: Some(code),
            ..Self::new(BrokerClientErrorCode::BuildPreparationRefused)
        }
    }

    const fn build_refused(build_execution_code: BuildExecutionErrorCode) -> Self {
        Self {
            build_execution_code: Some(build_execution_code),
            ..Self::new(BrokerClientErrorCode::BuildRefused)
        }
    }

    const fn build_root_refused(build_root_code: BuildRootPublicationErrorCode) -> Self {
        Self {
            build_root_code: Some(build_root_code),
            ..Self::new(BrokerClientErrorCode::BuildRootRefused)
        }
    }

    const fn generation_root_transition_refused(
        generation_root_transition_code: GenerationRootTransitionErrorCode,
    ) -> Self {
        Self {
            generation_root_transition_code: Some(generation_root_transition_code),
            ..Self::new(BrokerClientErrorCode::GenerationRootTransitionRefused)
        }
    }

    const fn generation_root_removal_refused(
        generation_root_removal_code: GenerationRootRemovalErrorCode,
    ) -> Self {
        Self {
            generation_root_removal_code: Some(generation_root_removal_code),
            ..Self::new(BrokerClientErrorCode::GenerationRootRemovalRefused)
        }
    }

    const fn generation_root_attestation_refused(
        generation_root_attestation_code: GenerationRootAttestationErrorCode,
    ) -> Self {
        Self {
            generation_root_attestation_code: Some(generation_root_attestation_code),
            ..Self::new(BrokerClientErrorCode::GenerationRootAttestationRefused)
        }
    }

    const fn install_acquisition_refused(cache_install_code: CacheInstallErrorCode) -> Self {
        Self {
            cache_install_code: Some(cache_install_code),
            ..Self::new(BrokerClientErrorCode::InstallAcquisitionRefused)
        }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> BrokerClientErrorCode {
        self.code
    }

    /// Returns the closed refusal code from authenticated build preparation.
    #[must_use]
    pub const fn build_preparation_code(self) -> Option<BuildPreparationErrorCode> {
        self.build_preparation_code
    }

    /// Returns the redacted adapter code when the broker completed the RPC with
    /// an adapter failure rather than a transport or protocol failure.
    #[must_use]
    pub const fn adapter_code(self) -> Option<NixAdapterErrorCode> {
        self.adapter_code
    }

    /// Returns the closed build refusal code after a completed execution RPC.
    #[must_use]
    pub const fn build_execution_code(self) -> Option<BuildExecutionErrorCode> {
        self.build_execution_code
    }

    /// Returns the closed refusal code from protected root publication.
    #[must_use]
    pub const fn build_root_code(self) -> Option<BuildRootPublicationErrorCode> {
        self.build_root_code
    }

    /// Returns the closed refusal code from a protected generation root transition.
    #[must_use]
    pub const fn generation_root_transition_code(
        self,
    ) -> Option<GenerationRootTransitionErrorCode> {
        self.generation_root_transition_code
    }

    /// Returns the closed refusal code from protected generation root removal.
    #[must_use]
    pub const fn generation_root_removal_code(self) -> Option<GenerationRootRemovalErrorCode> {
        self.generation_root_removal_code
    }

    /// Returns the closed refusal code from protected generation root attestation.
    #[must_use]
    pub const fn generation_root_attestation_code(
        self,
    ) -> Option<GenerationRootAttestationErrorCode> {
        self.generation_root_attestation_code
    }

    /// Returns the closed refusal code from cache-first installation.
    #[must_use]
    pub const fn cache_install_code(self) -> Option<CacheInstallErrorCode> {
        self.cache_install_code
    }
}

impl fmt::Display for BrokerClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private package broker failed")
    }
}

impl Error for BrokerClientError {}

/// One authenticated connection to the private broker lifecycle API.
///
/// Caller identity is never serialized. The broker derives it from kernel peer
/// credentials on this Unix stream.
#[derive(Debug)]
pub struct BrokerLifecycleClient {
    stream: UnixStream,
    next_request_id: u64,
    healthy: bool,
}

impl BrokerLifecycleClient {
    /// Connects to the single fixed broker endpoint for the current V1 host.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the platform is unsupported, the endpoint is
    /// unavailable, or finite I/O deadlines cannot be configured.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn connect_default() -> Result<Self, BrokerClientError> {
        Self::connect(Path::new(DEFAULT_BROKER_SOCKET))
    }

    /// Refuses unsupported Unix hosts without accepting an alternate endpoint.
    ///
    /// # Errors
    ///
    /// Always returns `UnsupportedPlatform` outside Linux and macOS.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub const fn connect_default() -> Result<Self, BrokerClientError> {
        Err(BrokerClientError::new(
            BrokerClientErrorCode::UnsupportedPlatform,
        ))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn connect(path: &Path) -> Result<Self, BrokerClientError> {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        let address = SockAddr::unix(path)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        socket
            .connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        let stream: UnixStream = socket.into();
        Ok(Self::from_stream(stream))
    }

    pub(crate) const fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            next_request_id: 1,
            healthy: true,
        }
    }

    /// Opens one caller-bound lifecycle operation.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn begin(
        &mut self,
        kind: BrokerOperationKind,
    ) -> Result<OperationHandle, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Begin(kind))? {
            CliBrokerResponse::Started(handle) => Ok(handle),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Reads the sanitized status of one operation handle.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn poll(&mut self, handle: OperationHandle) -> Result<OperationStatus, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Poll(handle))? {
            CliBrokerResponse::Status(status) => Ok(status),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Cancels one operation and waits for its acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn cancel(&mut self, handle: OperationHandle) -> Result<(), BrokerClientError> {
        match self.transact(&CliBrokerRequest::Cancel(handle))? {
            CliBrokerResponse::Cancelled => Ok(()),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Acknowledges local state commit and releases admission after an already
    /// irreversible privileged root transition. This call does not persist or
    /// remove roots; disconnect is an equivalent admission cleanup fallback.
    pub fn complete(&mut self, handle: OperationHandle) -> Result<(), BrokerClientError> {
        match self.transact(&CliBrokerRequest::Complete(handle))? {
            CliBrokerResponse::Completed => Ok(()),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Queries the validated pinned managed-runtime version under one handle.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// authorization, adapter, or response-kind failures.
    pub fn version(&mut self, handle: OperationHandle) -> Result<VersionInfo, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Version(handle))? {
            CliBrokerResponse::Version(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::Version, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Requests a privileged exact-ownership check under one live handle.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// authorization, or response-kind failures.
    pub fn verify_managed_ownership(
        &mut self,
        handle: OperationHandle,
    ) -> Result<bool, BrokerClientError> {
        match self.transact(&CliBrokerRequest::VerifyManagedOwnership(handle))? {
            CliBrokerResponse::ManagedOwnership(verified) => Ok(verified),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Evaluates one closed derivation request under a live resolve handle.
    pub fn evaluate_derivation(
        &mut self,
        handle: OperationHandle,
        request: EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::EvaluateDerivation(handle, request))? {
            CliBrokerResponse::DerivationPlan(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::EvaluateDerivation, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Queries validated metadata for one promoted store path.
    pub fn path_info(
        &mut self,
        handle: OperationHandle,
        path: StorePath,
    ) -> Result<PathInfoReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::PathInfo(handle, path))? {
            CliBrokerResponse::PathInfo(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::PathInfo, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Attempts substitution for one promoted store path.
    pub fn substitute(
        &mut self,
        handle: OperationHandle,
        path: StorePath,
    ) -> Result<SubstituteReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Substitute(handle, path))? {
            CliBrokerResponse::Substitute(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::Substitute, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Durably approves the broker-held private plan matching the displayed digest.
    pub fn approve_build(
        &mut self,
        handle: OperationHandle,
        digest: Digest,
        source: ApprovalSource,
    ) -> Result<(), BrokerClientError> {
        let approval = BuildApprovalRequest::new(digest, source);
        match self.transact(&CliBrokerRequest::ApproveBuild(handle, approval))? {
            CliBrokerResponse::BuildApproved => Ok(()),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Fetches the only public view of a broker-held private build plan.
    pub fn build_preview(
        &mut self,
        handle: OperationHandle,
    ) -> Result<BuildPreview, BrokerClientError> {
        match self.transact(&CliBrokerRequest::GetBuildPreview(handle))? {
            CliBrokerResponse::BuildPreview(preview) => Ok(preview),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Prepares a broker-private plan from typed selectors and returns its public preview.
    pub fn prepare_build(
        &mut self,
        handle: OperationHandle,
        selectors: Vec<PackageSelector>,
    ) -> Result<BuildPreview, BrokerClientError> {
        match self.transact_with_timeout(
            &CliBrokerRequest::PrepareBuild(handle, selectors),
            LONG_RUNNING_RESPONSE_TIMEOUT,
        )? {
            CliBrokerResponse::BuildPrepared(preview) => Ok(preview),
            CliBrokerResponse::BuildPreparationRefused(code) => {
                Err(BrokerClientError::build_preparation_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Runs cache-first acquisition through broker-owned authority.
    pub fn acquire_install(
        &mut self,
        handle: OperationHandle,
        selectors: Vec<PackageSelector>,
    ) -> Result<CacheInstallOutcome, BrokerClientError> {
        self.acquire_install_with_progress(handle, selectors, &mut |_| Ok(()))
    }

    /// Runs cache-first acquisition and consumes validated intermediate bytes.
    pub fn acquire_install_with_progress(
        &mut self,
        handle: OperationHandle,
        selectors: Vec<PackageSelector>,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CacheInstallOutcome, BrokerClientError> {
        let response = self.transact_acquire_with_progress(
            &CliBrokerRequest::AcquireInstall(handle, selectors),
            progress,
        )?;
        match response {
            CliBrokerResponse::InstallAcquired => Ok(CacheInstallOutcome::Acquired),
            CliBrokerResponse::InstallBuildRequired => Ok(CacheInstallOutcome::BuildRequired),
            CliBrokerResponse::InstallAcquisitionRefused(code) => {
                Err(BrokerClientError::install_acquisition_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    fn transact_acquire_with_progress(
        &mut self,
        request: &CliBrokerRequest,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        if !self.healthy {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::ConnectionFailed,
            ));
        }
        let result = self.transact_acquire_healthy(request, progress);
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn transact_acquire_healthy(
        &mut self,
        request: &CliBrokerRequest,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        let CliBrokerRequest::AcquireInstall(_, selectors) = request else {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::UnexpectedResponse,
            ));
        };
        let allowed_selectors = selectors
            .iter()
            .map(|selector| selector.selector().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::RequestIdExhausted))?;
        let frame = ProductFrameCodec::encode_cli_request(request_id, request)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        let deadline = Instant::now()
            .checked_add(LONG_RUNNING_RESPONSE_TIMEOUT)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        write_all_until(&mut self.stream, &frame, deadline)?;
        let mut counters = BTreeMap::<String, (u64, u64)>::new();
        loop {
            let response = read_frame(&mut self.stream, deadline)?;
            let (response_id, response) = ProductFrameCodec::decode_cli_response(&response)
                .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
            if response_id != request_id {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::UnexpectedResponse,
                ));
            }
            if let CliBrokerResponse::InstallDownloadProgress(update) = response {
                let key = update.selector().as_str().to_owned();
                if !allowed_selectors.contains(&key) {
                    return Err(BrokerClientError::new(
                        BrokerClientErrorCode::UnexpectedResponse,
                    ));
                }
                match counters.get_mut(&key) {
                    None if update.done() == 0 => {
                        counters.insert(key, (update.total(), 0));
                    }
                    Some((total, done)) if *total == update.total() && update.done() > *done => {
                        *done = update.done();
                    }
                    _ => {
                        return Err(BrokerClientError::new(
                            BrokerClientErrorCode::UnexpectedResponse,
                        ));
                    }
                }
                if progress(update).is_err() {
                    let _ = self.stream.shutdown(Shutdown::Both);
                    return Err(BrokerClientError::new(
                        BrokerClientErrorCode::UnexpectedResponse,
                    ));
                }
                continue;
            }
            match response {
                CliBrokerResponse::InstallAcquired => {
                    if counters.values().any(|(total, done)| total != done) {
                        return Err(BrokerClientError::new(
                            BrokerClientErrorCode::UnexpectedResponse,
                        ));
                    }
                    return Ok(CliBrokerResponse::InstallAcquired);
                }
                CliBrokerResponse::InstallBuildRequired => {
                    return Ok(CliBrokerResponse::InstallBuildRequired);
                }
                CliBrokerResponse::InstallAcquisitionRefused(code) => {
                    return Ok(CliBrokerResponse::InstallAcquisitionRefused(code));
                }
                _ => {
                    return Err(BrokerClientError::new(
                        BrokerClientErrorCode::UnexpectedResponse,
                    ));
                }
            }
        }
    }

    /// Executes the exact approved private plan identified by its displayed digest.
    pub fn execute_build(
        &mut self,
        handle: OperationHandle,
        digest: Digest,
    ) -> Result<BuildReport, BrokerClientError> {
        self.execute_build_with_progress(handle, digest, &mut |_| Ok(()))
    }

    /// Executes one approved build and consumes validated live estimates.
    pub fn execute_build_with_progress(
        &mut self,
        handle: OperationHandle,
        digest: Digest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
    ) -> Result<BuildReport, BrokerClientError> {
        let response = self.transact_build_with_progress(
            &CliBrokerRequest::ExecuteBuild(handle, digest),
            progress,
        )?;
        match response {
            CliBrokerResponse::BuildExecuted(report) => Ok(report),
            CliBrokerResponse::BuildExecutionRefused(code) => {
                Err(BrokerClientError::build_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    fn transact_build_with_progress(
        &mut self,
        request: &CliBrokerRequest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        if !self.healthy {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::ConnectionFailed,
            ));
        }
        let result = self.transact_build_healthy(request, progress);
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn transact_build_healthy(
        &mut self,
        request: &CliBrokerRequest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        if !matches!(request, CliBrokerRequest::ExecuteBuild(_, _)) {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::UnexpectedResponse,
            ));
        }
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::RequestIdExhausted))?;
        let frame = ProductFrameCodec::encode_cli_request(request_id, request)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        let deadline = Instant::now()
            .checked_add(LONG_RUNNING_RESPONSE_TIMEOUT)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        write_all_until(&mut self.stream, &frame, deadline)?;
        let mut last = 0;
        loop {
            let response = read_frame(&mut self.stream, deadline)?;
            let (response_id, response) = ProductFrameCodec::decode_cli_response(&response)
                .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
            if response_id != request_id {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::UnexpectedResponse,
                ));
            }
            if let CliBrokerResponse::BuildExecutionProgress(update) = response {
                if update.millionths() <= last {
                    return Err(BrokerClientError::new(
                        BrokerClientErrorCode::UnexpectedResponse,
                    ));
                }
                last = update.millionths();
                if progress(update).is_err() {
                    let _ = self.stream.shutdown(Shutdown::Both);
                    return Err(BrokerClientError::new(
                        BrokerClientErrorCode::UnexpectedResponse,
                    ));
                }
                continue;
            }
            return match response {
                CliBrokerResponse::BuildExecuted(report) => {
                    Ok(CliBrokerResponse::BuildExecuted(report))
                }
                CliBrokerResponse::BuildExecutionRefused(code) => {
                    Ok(CliBrokerResponse::BuildExecutionRefused(code))
                }
                _ => Err(BrokerClientError::new(
                    BrokerClientErrorCode::UnexpectedResponse,
                )),
            };
        }
    }

    /// Fetches authoritative post-build lifecycle evidence before rooting.
    pub fn install_evidence(
        &mut self,
        handle: OperationHandle,
    ) -> Result<pkg_nix::InstallEvidence, BrokerClientError> {
        match self.transact(&CliBrokerRequest::GetInstallEvidence(handle))? {
            CliBrokerResponse::InstallEvidence(evidence) => Ok(evidence),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Attests one already durable caller-owned generation under Activate authority.
    pub fn attest_generation_roots(
        &mut self,
        handle: OperationHandle,
        generation: GenerationId,
    ) -> Result<RootSetReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::AttestGenerationRoots(handle, generation))? {
            CliBrokerResponse::GenerationRootsAttested(report) => Ok(report),
            CliBrokerResponse::GenerationRootAttestationRefused(code) => {
                Err(BrokerClientError::generation_root_attestation_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Publishes a complete generation root intent after successful build execution.
    pub fn publish_build_roots(
        &mut self,
        handle: OperationHandle,
        intent: RootSetIntent,
    ) -> Result<RootSetReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::PublishBuildRoots(handle, intent))? {
            CliBrokerResponse::BuildRootsPublished(report) => Ok(report),
            CliBrokerResponse::BuildRootPublicationRefused(code) => {
                Err(BrokerClientError::build_root_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Transitions generation roots using only ownerless, path-free intent.
    pub fn transition_generation_roots(
        &mut self,
        handle: OperationHandle,
        intent: RootSetTransitionIntent,
    ) -> Result<RootSetTransitionReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::TransitionGenerationRoots(handle, intent))? {
            CliBrokerResponse::GenerationRootsTransitioned(report) => Ok(report),
            CliBrokerResponse::GenerationRootTransitionRefused(code) => {
                Err(BrokerClientError::generation_root_transition_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Removes one caller-owned generation root set without accepting any raw path.
    pub fn remove_generation_roots(
        &mut self,
        handle: OperationHandle,
        generation: GenerationId,
    ) -> Result<(), BrokerClientError> {
        match self.transact_with_timeout(
            &CliBrokerRequest::RemoveGenerationRoots(handle, generation),
            LONG_RUNNING_RESPONSE_TIMEOUT,
        )? {
            CliBrokerResponse::GenerationRootsRemoved => Ok(()),
            CliBrokerResponse::GenerationRootRemovalRefused(code) => {
                Err(BrokerClientError::generation_root_removal_refused(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Waits for and retains exclusive GC admission until completion or cancellation.
    pub fn acquire_gc(&mut self, handle: OperationHandle) -> Result<(), BrokerClientError> {
        match self.transact_with_timeout(
            &CliBrokerRequest::AcquireGc(handle),
            LONG_RUNNING_RESPONSE_TIMEOUT,
        )? {
            CliBrokerResponse::GcAdmissionAcquired => Ok(()),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Verifies one validated closed request.
    pub fn verify(
        &mut self,
        handle: OperationHandle,
        request: VerifyRequest,
    ) -> Result<VerifyReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Verify(handle, request))? {
            CliBrokerResponse::Verify(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::Verify, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Verifies or cache-repairs one authenticated rooted generation.
    pub fn repair_generation(
        &mut self,
        handle: OperationHandle,
        request: RepairGenerationRequest,
    ) -> Result<RepairGenerationReport, BrokerClientError> {
        match self.transact_with_timeout(
            &CliBrokerRequest::RepairGeneration(handle, request),
            LONG_RUNNING_RESPONSE_TIMEOUT,
        )? {
            CliBrokerResponse::RepairGeneration(report) => Ok(report),
            CliBrokerResponse::RepairGenerationRefused(code) => {
                Err(BrokerClientError::new(match code {
                    RepairGenerationErrorCode::InvalidScope => {
                        BrokerClientErrorCode::RepairInvalidScope
                    }
                    RepairGenerationErrorCode::VerifyFailed => {
                        BrokerClientErrorCode::RepairVerifyFailed
                    }
                    RepairGenerationErrorCode::AdmissionFailed => {
                        BrokerClientErrorCode::RepairAdmissionFailed
                    }
                    RepairGenerationErrorCode::HelperFailed => {
                        BrokerClientErrorCode::RepairHelperFailed
                    }
                    RepairGenerationErrorCode::JournalFailed => {
                        BrokerClientErrorCode::RepairJournalFailed
                    }
                    RepairGenerationErrorCode::StillDamaged => {
                        BrokerClientErrorCode::RepairStillDamaged
                    }
                    RepairGenerationErrorCode::FreshApprovalRequired => {
                        BrokerClientErrorCode::RepairFreshApprovalRequired
                    }
                    RepairGenerationErrorCode::AuthorityUnavailable => {
                        BrokerClientErrorCode::RepairAuthorityUnavailable
                    }
                }))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Collects unreachable paths using only the managed on-disk roots.
    pub fn gc(&mut self, handle: OperationHandle) -> Result<GcReport, BrokerClientError> {
        match self
            .transact_with_timeout(&CliBrokerRequest::Gc(handle), LONG_RUNNING_RESPONSE_TIMEOUT)?
        {
            CliBrokerResponse::Gc(report) => Ok(report),
            CliBrokerResponse::AdapterFailure(MethodKind::Gc, code) => {
                Err(BrokerClientError::adapter_failure(code))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Refreshes the broker-owned signed channel and native index.
    ///
    /// The request contains only a caller-bound Refresh handle. No trust,
    /// origin, system, target, or raw Nix input crosses this boundary.
    pub fn refresh_channel(
        &mut self,
        handle: OperationHandle,
        mode: pkg_nix::ChannelRefreshMode,
    ) -> Result<ChannelRefreshReport, BrokerClientError> {
        match self.transact_with_timeout(
            &CliBrokerRequest::RefreshChannel(handle, mode),
            LONG_RUNNING_RESPONSE_TIMEOUT,
        )? {
            CliBrokerResponse::ChannelRefreshed(report) => Ok(report),
            CliBrokerResponse::ChannelRefreshRefused(code) => {
                Err(BrokerClientError::new(match code {
                    pkg_nix::ChannelRefreshErrorCode::Network => {
                        BrokerClientErrorCode::ChannelRefreshNetwork
                    }
                    pkg_nix::ChannelRefreshErrorCode::Verification => {
                        BrokerClientErrorCode::ChannelRefreshVerification
                    }
                    pkg_nix::ChannelRefreshErrorCode::Busy => {
                        BrokerClientErrorCode::ChannelRefreshBusy
                    }
                    pkg_nix::ChannelRefreshErrorCode::ServiceUnavailable => {
                        BrokerClientErrorCode::ChannelRefreshServiceUnavailable
                    }
                }))
            }
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Searches the broker-owned authenticated native catalog.
    pub fn search_catalog(
        &mut self,
        handle: OperationHandle,
        request: pkg_nix::CatalogSearchRequest,
    ) -> Result<pkg_nix::CatalogSearchReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::SearchCatalog(handle, request))? {
            CliBrokerResponse::CatalogSearch(report) => Ok(report),
            CliBrokerResponse::CatalogSearchRefused => Err(BrokerClientError::new(
                BrokerClientErrorCode::CatalogQueryRefused,
            )),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Inspects selectors through one broker-owned authenticated catalog snapshot.
    pub fn info_catalog(
        &mut self,
        handle: OperationHandle,
        requests: Vec<pkg_nix::CatalogInfoRequest>,
    ) -> Result<Vec<pkg_nix::CatalogInfoReport>, BrokerClientError> {
        match self.transact(&CliBrokerRequest::InfoCatalog(handle, requests))? {
            CliBrokerResponse::CatalogInfo(reports) => Ok(reports),
            CliBrokerResponse::CatalogInfoRefused => Err(BrokerClientError::new(
                BrokerClientErrorCode::CatalogQueryRefused,
            )),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    fn transact(
        &mut self,
        request: &CliBrokerRequest,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        self.transact_with_timeout(request, RESPONSE_TIMEOUT)
    }

    fn transact_with_timeout(
        &mut self,
        request: &CliBrokerRequest,
        timeout: Duration,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        if !self.healthy {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::ConnectionFailed,
            ));
        }
        let result = self.transact_healthy(request, timeout);
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn transact_healthy(
        &mut self,
        request: &CliBrokerRequest,
        timeout: Duration,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::RequestIdExhausted))?;
        let frame = ProductFrameCodec::encode_cli_request(request_id, request)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        write_all_until(&mut self.stream, &frame, deadline)?;
        let response = read_frame(&mut self.stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_cli_response(&response)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        if response_id != request_id {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::UnexpectedResponse,
            ));
        }
        Ok(response)
    }

    const fn fail(&mut self, code: BrokerClientErrorCode) -> BrokerClientError {
        self.healthy = false;
        BrokerClientError::new(code)
    }
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, BrokerClientError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    read_exact_until(stream, &mut header, deadline)?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(BrokerClientError::new(BrokerClientErrorCode::InvalidFrame));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
    frame.extend_from_slice(&header);
    frame.resize(FRAME_HEADER_BYTES + payload_length, 0);
    read_exact_until(stream, &mut frame[FRAME_HEADER_BYTES..], deadline)?;
    Ok(frame)
}

fn write_all_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), BrokerClientError> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
        }
    }
    Ok(())
}

fn read_exact_until(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), BrokerClientError> {
    while !bytes.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
        }
    }
    Ok(())
}

fn remaining(deadline: Instant) -> Result<Duration, BrokerClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))
}

/// `NixAdapter` proxy backed only by the fixed authenticated product broker.
#[derive(Debug, Clone, Default)]
pub struct BrokerNixAdapter {
    endpoint: Option<PathBuf>,
}

impl BrokerNixAdapter {
    /// Constructs the production proxy for the platform's fixed broker endpoint.
    #[must_use]
    pub const fn new() -> Self {
        Self { endpoint: None }
    }

    #[cfg(test)]
    const fn at(endpoint: PathBuf) -> Self {
        Self {
            endpoint: Some(endpoint),
        }
    }

    fn connect(&self) -> Result<BrokerLifecycleClient, BrokerClientError> {
        match &self.endpoint {
            Some(endpoint) => BrokerLifecycleClient::connect(endpoint),
            None => BrokerLifecycleClient::connect_default(),
        }
    }

    fn call<T>(
        &self,
        operation: BrokerOperationKind,
        invoke: impl FnOnce(&mut BrokerLifecycleClient, OperationHandle) -> Result<T, BrokerClientError>,
    ) -> Result<T, NixAdapterError> {
        let mut client = self.connect().map_err(map_broker_error)?;
        let handle = client.begin(operation).map_err(map_broker_error)?;
        let result = invoke(&mut client, handle.clone()).map_err(map_broker_error);
        let _ = client.cancel(handle);
        result
    }
}

impl NixAdapter for BrokerNixAdapter {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        self.call(BrokerOperationKind::Doctor, BrokerLifecycleClient::version)
    }

    fn evaluate_derivation(
        &self,
        request: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        self.call(BrokerOperationKind::Resolve, |client, handle| {
            client.evaluate_derivation(handle, request.clone())
        })
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        self.call(BrokerOperationKind::Doctor, |client, handle| {
            client.path_info(handle, path.clone())
        })
    }

    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        self.call(BrokerOperationKind::Acquire, |client, handle| {
            client.substitute(handle, path.clone())
        })
    }

    fn build(&self, _request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        Err(NixAdapterError::PermissionDenied)
    }

    fn verify(&self, request: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        self.call(BrokerOperationKind::Doctor, |client, handle| {
            client.verify(handle, request.clone())
        })
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        self.call(BrokerOperationKind::Gc, BrokerLifecycleClient::gc)
    }
}

const fn map_broker_error(error: BrokerClientError) -> NixAdapterError {
    if let Some(code) = error.adapter_code() {
        return NixAdapterError::remote(code);
    }
    match error.code() {
        BrokerClientErrorCode::UnsupportedPlatform
        | BrokerClientErrorCode::Unavailable
        | BrokerClientErrorCode::ConnectionFailed => NixAdapterError::Unavailable,
        BrokerClientErrorCode::TransportFailure
        | BrokerClientErrorCode::InvalidFrame
        | BrokerClientErrorCode::UnexpectedResponse
        | BrokerClientErrorCode::RequestIdExhausted
        | BrokerClientErrorCode::AdapterFailure
        | BrokerClientErrorCode::BuildPreparationRefused
        | BrokerClientErrorCode::BuildRefused
        | BrokerClientErrorCode::BuildRootRefused
        | BrokerClientErrorCode::GenerationRootTransitionRefused
        | BrokerClientErrorCode::GenerationRootRemovalRefused
        | BrokerClientErrorCode::GenerationRootAttestationRefused
        | BrokerClientErrorCode::InstallAcquisitionRefused
        | BrokerClientErrorCode::ChannelRefreshNetwork
        | BrokerClientErrorCode::ChannelRefreshVerification
        | BrokerClientErrorCode::ChannelRefreshBusy
        | BrokerClientErrorCode::ChannelRefreshServiceUnavailable
        | BrokerClientErrorCode::CatalogQueryRefused
        | BrokerClientErrorCode::RepairInvalidScope
        | BrokerClientErrorCode::RepairVerifyFailed
        | BrokerClientErrorCode::RepairAdmissionFailed
        | BrokerClientErrorCode::RepairHelperFailed
        | BrokerClientErrorCode::RepairJournalFailed
        | BrokerClientErrorCode::RepairStillDamaged
        | BrokerClientErrorCode::RepairFreshApprovalRequired
        | BrokerClientErrorCode::RepairAuthorityUnavailable => NixAdapterError::OperationFailed,
    }
}

#[cfg(test)]
mod tests;
