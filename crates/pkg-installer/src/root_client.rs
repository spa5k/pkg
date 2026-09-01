//! Root-publication and path-free-transition client for the private helper channel.

use crate::{
    HelperTransportError, HelperTransportErrorCode, RepairCoordinatorError, RepairMaintenance,
    platform::peer_uid,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};
use pkg_nix::{
    BrokerHelperRequest, BrokerHelperResponse, BuildCacheError, BuildCacheProbe, BuildReport,
    BuildRequest, CacheDownloadClosure, CachePathObservation, DerivationPlanReport, Digest,
    EvaluateDerivationRequest, GcReport, HELPER_FRAME_PAYLOAD_LIMIT, MAX_REPAIR_EXECUTION_DURATION,
    MaintenanceCapability, NixAdapter, NixAdapterError, NixpkgsMetadataRunner, NixpkgsPin,
    NixpkgsSourceError, PathInfoReport, ProductFrameCodec, RemoveRootSetRequest,
    RepairStorePathsReport, RepairStorePathsRequest, RootNixFailure, RootNixOperation,
    RootNixRequest, RootNixResponse, RootRepairPlanProof, RootRepairPlanRequest, RootSet,
    RootSetAttestationRequest, RootSetPublicationRequest, RootSetReport, RootSetTransitionReport,
    RootSetTransitionRequest, StorePath, SubstituteReport, VerifiedRepairScope, VerifyReport,
    VerifyRequest, VersionInfo,
};
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    fmt,
    io::{self, Read, Write},
    net::Shutdown,
    os::fd::AsFd,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FRAME_HEADER_BYTES: usize = 20;
const REQUEST_ID: u64 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const REPAIR_RESPONSE_GRACE: Duration = Duration::from_mins(1);

#[cfg(target_os = "linux")]
const DEFAULT_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
#[cfg(target_os = "macos")]
const DEFAULT_HELPER_SOCKET: &str = "/Library/Application Support/pkg/run/helper/root-helper.sock";

/// Fixed-policy client for durable roots and capability-gated repair.
pub struct RootHelperClient {
    endpoint: Option<PathBuf>,
    expected_uid: u32,
}

impl fmt::Debug for RootHelperClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RootHelperClient(<fixed-private-endpoint>)")
    }
}

impl RootHelperClient {
    /// Constructs the production client for the compiled platform endpoint.
    #[must_use]
    pub fn production() -> Self {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let endpoint = Some(PathBuf::from(default_endpoint()));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let endpoint = None;
        Self {
            endpoint,
            expected_uid: 0,
        }
    }

    #[cfg(test)]
    const fn at(endpoint: PathBuf, expected_uid: u32) -> Self {
        Self {
            endpoint: Some(endpoint),
            expected_uid,
        }
    }

    /// Publishes one complete, validated root set over a fresh authenticated
    /// connection and accepts only its exact correlated response.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error if the fixed endpoint is unavailable,
    /// the peer is not root, bounded I/O fails, or the response is not the exact
    /// root-publication response for this request.
    pub fn publish_root_set(
        &self,
        request: &RootSetPublicationRequest,
    ) -> Result<RootSetReport, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::PublishRootSet(request.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RootSetPublished(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Requests one path-free root transition over a fresh authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated transition response.
    pub fn transition_root_set(
        &self,
        request: &RootSetTransitionRequest,
    ) -> Result<RootSetTransitionReport, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::TransitionRootSet(request.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RootSetTransitioned(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Attests one durable root set over a fresh authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated attestation response.
    pub fn attest_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::AttestRootSet(request.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RootSetAttested(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Removes one path-free, authenticated generation root set over a fresh
    /// helper connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated idempotent removal response.
    pub fn remove_root_set(
        &self,
        request: &RemoveRootSetRequest,
    ) -> Result<(), HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::RemoveRootSet(request.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RootSetRemoved => Ok(()),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Loads exact durable roots for broker-private repair planning.
    ///
    /// The request is path-free. The returned paths stay on the authenticated
    /// broker-to-helper channel and never cross the public CLI boundary.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn load_repair_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSet, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::LoadRepairRootSet(request.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RepairRootSetLoaded(root_set) => Ok(root_set),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Verifies the managed runtime against one authenticated manifest digest.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the root helper returns the
    /// exact correlated ownership response.
    pub fn verify_managed_ownership(&self, digest: Digest) -> Result<bool, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::VerifyManagedOwnership(digest),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::ManagedOwnership(verified) => Ok(verified),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Issues one opaque capability for a broker-derived verified repair scope.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, HelperTransportError> {
        match self.round_trip(
            &BrokerHelperRequest::IssueRepairCapability(scope.clone()),
            RESPONSE_TIMEOUT,
        )? {
            BrokerHelperResponse::RepairCapabilityIssued(capability) => Ok(capability),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Redeems one opaque capability for the helper's fixed repair executor.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, HelperTransportError> {
        // The privileged executor owns a bounded process-group deadline. The
        // broker waits beyond that bound so it does not release build or GC
        // admission while the helper can still mutate the store.
        match self.round_trip(
            &BrokerHelperRequest::RepairStorePaths(request.clone()),
            repair_response_timeout()?,
        )? {
            BrokerHelperResponse::RepairCompleted(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    fn round_trip(
        &self,
        request: &BrokerHelperRequest,
        timeout: Duration,
    ) -> Result<BrokerHelperResponse, HelperTransportError> {
        let mut stream = self.connect()?;
        let request = ProductFrameCodec::encode_helper_request(REQUEST_ID, request)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(timeout)?;
        write_all_until(&mut stream, &request, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        Ok(response)
    }

    fn connect(&self) -> Result<UnixStream, HelperTransportError> {
        let endpoint = self
            .endpoint
            .as_deref()
            .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let address = SockAddr::unix(endpoint)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        socket
            .connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let stream: UnixStream = socket.into();
        if peer_uid(&stream).map_err(|()| {
            HelperTransportError::new(HelperTransportErrorCode::UnauthenticatedPeer)
        })? != self.expected_uid
        {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::UnauthenticatedPeer,
            ));
        }
        stream
            .set_nonblocking(true)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        Ok(stream)
    }

    fn root_round_trip(
        &self,
        request: RootNixRequest,
    ) -> Result<RootNixResponse, HelperTransportError> {
        let operation = request.operation();
        let timeout = operation
            .client_budget()
            .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        match self.round_trip(&BrokerHelperRequest::RootNix(request), timeout)? {
            BrokerHelperResponse::RootNix(response) if response.operation() == operation => {
                Ok(*response)
            }
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Resolves the exact typed closure of generation roots through the helper.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error for transport, framing, helper, or validation failure.
    #[allow(
        dead_code,
        reason = "inactive typed proxy contract kept for the DN09 contract test"
    )]
    pub(crate) fn closure_for_roots(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<StorePath>, NixAdapterError> {
        match self.adapter_response(RootNixRequest::ClosureForRoots(roots.to_vec()))? {
            RootNixResponse::ClosureForRoots(closure) => Ok(closure),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    /// Requests the sanitized repair preview and digest proof.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error for transport, framing, helper, or validation failure.
    #[allow(
        dead_code,
        reason = "inactive typed proxy contract kept for the DN09 contract test"
    )]
    pub(crate) fn repair_plan_proof(
        &self,
        request: &RootRepairPlanRequest,
    ) -> Result<RootRepairPlanProof, NixAdapterError> {
        match self.adapter_response(RootNixRequest::RepairPlan(request.clone()))? {
            RootNixResponse::RepairPlan(proof) if request.accepts(&proof) => Ok(proof),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn adapter_response(
        &self,
        request: RootNixRequest,
    ) -> Result<RootNixResponse, NixAdapterError> {
        let operation = request.operation();
        match self
            .root_round_trip(request)
            .map_err(adapter_transport_failure)?
        {
            RootNixResponse::Failed {
                operation: response_operation,
                failure: RootNixFailure::Adapter(code),
            } if response_operation == operation => Err(NixAdapterError::remote(code)),
            RootNixResponse::Failed { .. } => Err(NixAdapterError::Unavailable),
            response if response.operation() == operation => Ok(response),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }
}

impl NixAdapter for RootHelperClient {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        match self.adapter_response(RootNixRequest::Version)? {
            RootNixResponse::Version(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn evaluate_derivation(
        &self,
        request: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        match self.adapter_response(RootNixRequest::Evaluate(request.clone()))? {
            RootNixResponse::Evaluate(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        match self.adapter_response(RootNixRequest::PathInfo(path.clone()))? {
            RootNixResponse::PathInfo(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        match self.adapter_response(RootNixRequest::Substitute(path.clone()))? {
            RootNixResponse::Substitute(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn substitute_many(
        &self,
        paths: &[StorePath],
    ) -> Result<Vec<SubstituteReport>, NixAdapterError> {
        match self.adapter_response(RootNixRequest::SubstituteMany(paths.to_vec()))? {
            RootNixResponse::SubstituteMany(reports) => Ok(reports),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn build(&self, request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        self.build_with_progress(request, &mut |_| Ok(()))
    }

    fn build_with_progress(
        &self,
        request: &BuildRequest,
        progress: &mut dyn FnMut(pkg_nix::BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, NixAdapterError> {
        let mut stream = self.connect().map_err(adapter_transport_failure)?;
        let request = BrokerHelperRequest::RootNix(RootNixRequest::Build(request.clone()));
        let encoded = ProductFrameCodec::encode_helper_request(REQUEST_ID, &request)
            .map_err(|_| NixAdapterError::OperationFailed)?;
        let timeout = RootNixOperation::Build
            .client_budget()
            .ok_or(NixAdapterError::Timeout)?;
        let deadline = deadline_after(timeout).map_err(adapter_transport_failure)?;
        write_all_until(&mut stream, &encoded, deadline).map_err(adapter_transport_failure)?;
        let mut last_progress = 0;
        loop {
            let frame = read_frame(&mut stream, deadline).map_err(adapter_transport_failure)?;
            let (response_id, response) = ProductFrameCodec::decode_helper_response(&frame)
                .map_err(|_| NixAdapterError::OperationFailed)?;
            if response_id != REQUEST_ID {
                return Err(NixAdapterError::OperationFailed);
            }
            let BrokerHelperResponse::RootNix(response) = response else {
                return Err(NixAdapterError::OperationFailed);
            };
            if let Some(report) = handle_build_response(response, &mut last_progress, progress)? {
                return Ok(report);
            }
        }
    }

    fn verify(&self, request: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        match self.adapter_response(RootNixRequest::Verify(request.clone()))? {
            RootNixResponse::Verify(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        match self.adapter_response(RootNixRequest::Gc)? {
            RootNixResponse::Gc(report) => Ok(report),
            _ => Err(NixAdapterError::OperationFailed),
        }
    }
}

impl BuildCacheProbe for RootHelperClient {
    fn inspect(&self, paths: &[StorePath]) -> Result<Vec<CachePathObservation>, BuildCacheError> {
        match self.root_round_trip(RootNixRequest::CacheInspect(paths.to_vec())) {
            Ok(RootNixResponse::CacheInspect(observations)) => Ok(observations),
            Ok(RootNixResponse::Failed {
                operation: RootNixOperation::CacheInspect,
                failure: RootNixFailure::Cache(code),
            }) => Err(BuildCacheError::remote(code)),
            _ => Err(BuildCacheError::remote(
                pkg_nix::BuildCacheErrorCode::ProbeFailed,
            )),
        }
    }

    fn inspect_download_closures(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<CacheDownloadClosure>, BuildCacheError> {
        match self.root_round_trip(RootNixRequest::CacheInspectClosures(roots.to_vec())) {
            Ok(RootNixResponse::CacheInspectClosures(closures)) => Ok(closures),
            Ok(RootNixResponse::Failed {
                operation: RootNixOperation::CacheInspectClosures,
                failure: RootNixFailure::Cache(code),
            }) => Err(BuildCacheError::remote(code)),
            _ => Err(BuildCacheError::remote(
                pkg_nix::BuildCacheErrorCode::ProbeFailed,
            )),
        }
    }
}

impl NixpkgsMetadataRunner for RootHelperClient {
    fn run_metadata(&self, pin: &NixpkgsPin) -> Result<Vec<u8>, NixpkgsSourceError> {
        match self.root_round_trip(RootNixRequest::NixpkgsMetadata(pin.clone())) {
            Ok(RootNixResponse::NixpkgsMetadata(metadata)) => Ok(metadata),
            _ => Err(NixpkgsSourceError::runner_failure()),
        }
    }
}

const fn adapter_transport_failure(error: HelperTransportError) -> NixAdapterError {
    match error.code() {
        HelperTransportErrorCode::UnauthenticatedPeer => NixAdapterError::PermissionDenied,
        HelperTransportErrorCode::TransportFailure => NixAdapterError::Unavailable,
        HelperTransportErrorCode::InvalidFrame | HelperTransportErrorCode::HelperFailure => {
            NixAdapterError::OperationFailed
        }
    }
}

impl RepairMaintenance for RootHelperClient {
    fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, RepairCoordinatorError> {
        Self::issue_repair_capability(self, scope)
            .map_err(|_| RepairCoordinatorError::helper_failure())
    }

    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, RepairCoordinatorError> {
        Self::repair_store_paths(self, request)
            .map_err(|_| RepairCoordinatorError::helper_failure())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn default_endpoint() -> &'static Path {
    Path::new(DEFAULT_HELPER_SOCKET)
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, HelperTransportError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    read_exact_until(stream, &mut header, deadline)?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > HELPER_FRAME_PAYLOAD_LIMIT {
        return Err(HelperTransportError::new(
            HelperTransportErrorCode::InvalidFrame,
        ));
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
) -> Result<(), HelperTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLOUT)?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
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
) -> Result<(), HelperTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLIN)?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
    Ok(())
}

fn wait_ready(
    stream: &UnixStream,
    deadline: Instant,
    required: PollFlags,
) -> Result<(), HelperTransportError> {
    loop {
        let timeout = PollTimeout::try_from(remaining(deadline)?)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let mut descriptor = [PollFd::new(stream.as_fd(), required)];
        match poll(&mut descriptor, timeout) {
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(_)
                if descriptor[0]
                    .revents()
                    .is_some_and(|events| events.contains(required)) =>
            {
                return Ok(());
            }
            Err(Errno::EINTR) => {}
            Ok(_) | Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
}

fn deadline_after(timeout: Duration) -> Result<Instant, HelperTransportError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
}

fn repair_response_timeout() -> Result<Duration, HelperTransportError> {
    MAX_REPAIR_EXECUTION_DURATION
        .checked_add(REPAIR_RESPONSE_GRACE)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
}

fn remaining(deadline: Instant) -> Result<Duration, HelperTransportError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
    let milliseconds = u64::try_from(remaining.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds != 0)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[allow(clippy::unwrap_used, reason = "tests may unwrap")]
mod tests {
    use super::*;
    use crate::{BrokerHelperDispatch, serve_helper_connection};
    use nix::unistd::Uid;
    use pkg_nix::{
        AuthenticatedHelper, BuildApprovalReceipt, BuildOutput, BuildOutputProvenance,
        BuildPreview, BuildReadiness, BuildStatus, DerivationPath, DerivedOutputTarget,
        GenerationId, InProcessHelper, InProcessPeer, MaintenanceAdapter, MaintenanceError,
        OperationId, OutputName, PolicyVersion, RootName, RootRepairPlanProof,
        RootRepairPlanRequest, RootSetAttestationRequest, RootSetEntry, RootSetTransitionRequest,
        StorePath, System,
    };
    use std::{
        error::Error,
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };
    use tempfile::TempDir;

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    struct RootDispatch(AuthenticatedHelper);

    impl BrokerHelperDispatch for RootDispatch {
        fn dispatch(
            &self,
            request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            match request {
                BrokerHelperRequest::PublishRootSet(request) => self
                    .0
                    .for_caller(request.root_set().owner_uid())
                    .publish_root_set(request.root_set())
                    .map(BrokerHelperResponse::RootSetPublished),
                BrokerHelperRequest::TransitionRootSet(request) => {
                    let derived = request.derive_from(&root_set())?;
                    let report = self
                        .0
                        .for_caller(derived.owner_uid())
                        .publish_root_set(&derived)?;
                    let mapping_digest = derived.mapping_digest();
                    RootSetTransitionReport::new(
                        report,
                        request.retained_names().to_vec(),
                        mapping_digest,
                    )
                    .map(BrokerHelperResponse::RootSetTransitioned)
                }
                BrokerHelperRequest::RemoveRootSet(request) => {
                    self.0
                        .for_caller(request.owner_uid())
                        .remove_root_set(&request)?;
                    Ok(BrokerHelperResponse::RootSetRemoved)
                }
                BrokerHelperRequest::AttestRootSet(request) => self
                    .0
                    .for_caller(request.owner_uid())
                    .attest_root_set(&request)
                    .map(BrokerHelperResponse::RootSetAttested),
                BrokerHelperRequest::RootNix(request) => Ok(BrokerHelperResponse::RootNix(
                    Box::new(RootNixResponse::Failed {
                        operation: request.operation(),
                        failure: RootNixFailure::Inactive,
                    }),
                )),
                _ => Err(MaintenanceError::backend_failure()),
            }
        }
    }

    fn root_set() -> RootSet {
        RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            )],
        )
        .unwrap()
    }

    fn build_request() -> BuildRequest {
        BuildRequest::new(
            vec![
                DerivedOutputTarget::new(
                    DerivationPath::new(
                        StorePath::new(&format!("/nix/store/{STORE_HASH}-hello.drv")).unwrap(),
                    )
                    .unwrap(),
                    vec![OutputName::new("out").unwrap()],
                )
                .unwrap(),
            ],
            System::X8664Linux,
            BuildApprovalReceipt::new(
                OperationId::new("op-root-client").unwrap(),
                Digest::from_bytes([0x42; 32]),
                PolicyVersion::from_u64(7).unwrap(),
            ),
        )
        .unwrap()
    }

    fn build_report() -> BuildReport {
        BuildReport::new(
            BuildStatus::Built,
            vec![BuildOutput::new(
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello")).unwrap(),
                BuildOutputProvenance::LocalBuild,
            )],
        )
        .unwrap()
    }

    fn repair_proof() -> RootRepairPlanProof {
        let preview = BuildPreview::from_json_bytes(
            br#"{"schemaVersion":1,"purpose":"repair","platform":{"os":"linux","arch":"x86_64"},"policyVersion":7,"buildPlanDigest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","targets":[{"selector":"repair-1","packageName":"hello-1.0","version":"installed","outputsToInstall":["out"],"localBuildRequired":true}],"build":{"count":1,"names":["hello-1.0"],"hasFixedOutput":false},"cache":{"knownDownloadBytes":0,"knownContentBytes":0},"unknownLocalOutputs":1,"estimates":{"approxBuildMinutes":null,"approxNewDiskBytes":null,"approxTotalClosureBytes":null},"readiness":{"sandboxed":true,"buildIsolationReady":true,"nativeBuild":true,"resourceBoundary":{"isolation":"sandbox","perBuildResourceCap":false,"notice":"Repair builds run sandboxed. pkg fixes repair parallelism to one build job, admits one machine-global build operation, and applies no hard per-build memory/CPU/IO cap. Determinate controls other daemon limits."}},"approvalRequired":true}"#,
        )
        .unwrap();
        RootRepairPlanProof::new(preview).unwrap()
    }

    struct ProgressDispatch {
        estimates: Vec<u32>,
        delay: Duration,
        sink_failed: Arc<AtomicBool>,
    }

    struct RepairPlanningDispatch {
        roots: Vec<StorePath>,
        closure: Vec<StorePath>,
        request: RootRepairPlanRequest,
        proof: RootRepairPlanProof,
        calls: AtomicUsize,
    }

    impl BrokerHelperDispatch for RepairPlanningDispatch {
        fn dispatch(
            &self,
            _request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            Err(MaintenanceError::backend_failure())
        }

        fn dispatch_root_nix(
            &self,
            request: RootNixRequest,
            _deadline: Instant,
        ) -> RootNixResponse {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match request {
                RootNixRequest::ClosureForRoots(roots) if roots == self.roots => {
                    RootNixResponse::ClosureForRoots(self.closure.clone())
                }
                RootNixRequest::RepairPlan(request) if request == self.request => {
                    RootNixResponse::RepairPlan(self.proof.clone())
                }
                request => RootNixResponse::Failed {
                    operation: request.operation(),
                    failure: RootNixFailure::Inactive,
                },
            }
        }
    }

    type ProgressRun = (
        Result<BuildReport, NixAdapterError>,
        bool,
        Result<(), HelperTransportError>,
    );

    impl BrokerHelperDispatch for ProgressDispatch {
        fn dispatch(
            &self,
            _request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            Err(MaintenanceError::backend_failure())
        }

        fn dispatch_build(
            &self,
            _request: &BuildRequest,
            _deadline: Instant,
            cancelled: &AtomicBool,
            progress: &mut dyn FnMut(pkg_nix::BuildProgressEstimate) -> Result<(), NixAdapterError>,
        ) -> RootNixResponse {
            for value in &self.estimates {
                if !self.delay.is_zero() {
                    thread::sleep(self.delay);
                }
                if cancelled.load(Ordering::Acquire) {
                    self.sink_failed.store(true, Ordering::Release);
                    return RootNixResponse::Failed {
                        operation: RootNixOperation::Build,
                        failure: RootNixFailure::Adapter(pkg_nix::NixAdapterErrorCode::Unavailable),
                    };
                }
                if progress(pkg_nix::BuildProgressEstimate::new(*value).unwrap()).is_err() {
                    self.sink_failed.store(true, Ordering::Relaxed);
                    return RootNixResponse::Failed {
                        operation: RootNixOperation::Build,
                        failure: RootNixFailure::Adapter(pkg_nix::NixAdapterErrorCode::Unavailable),
                    };
                }
            }
            RootNixResponse::Build(build_report())
        }
    }

    fn run_progress_build(
        estimates: Vec<u32>,
        delay: Duration,
        progress: &mut dyn FnMut(pkg_nix::BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<ProgressRun, Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let helper_uid = Uid::effective().as_raw();
        let sink_failed = Arc::new(AtomicBool::new(false));
        let observed_failure = Arc::clone(&sink_failed);
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(
                stream,
                helper_uid,
                &ProgressDispatch {
                    estimates,
                    delay,
                    sink_failed: observed_failure,
                },
            )
        });
        let result = RootHelperClient::at(endpoint, helper_uid)
            .build_with_progress(&build_request(), progress);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        Ok((result, sink_failed.load(Ordering::Relaxed), served))
    }

    fn publication() -> RootSetPublicationRequest {
        let root_set = root_set();
        let added_names = root_set
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect();
        RootSetPublicationRequest::new(root_set, None, added_names).unwrap()
    }

    #[test]
    fn fixed_client_authenticates_peer_and_round_trips_only_root_publication()
    -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);

        let report = client.publish_root_set(&publication());
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        let report = report?;
        assert_eq!(report.entry_count(), 1);
        assert!(report.reference().as_str().ends_with("/1001/gen-0007"));
        Ok(())
    }

    #[test]
    fn typed_adapter_maps_the_closed_inactive_result_without_fallback() -> Result<(), Box<dyn Error>>
    {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);

        assert_eq!(
            client.version().unwrap_err().code(),
            pkg_nix::NixAdapterErrorCode::Unavailable
        );
        assert!(worker.join().unwrap().is_ok());
        Ok(())
    }

    #[test]
    fn repair_planning_seam_round_trips_exact_closure_and_proof_requests()
    -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let helper_uid = Uid::effective().as_raw();
        let root = StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?;
        let dependency = StorePath::new(&format!("/nix/store/{STORE_HASH}-glibc"))?;
        let request = RootRepairPlanRequest::new(
            vec![root.clone()],
            PolicyVersion::from_u64(7).unwrap(),
            System::X8664Linux,
            BuildReadiness::new(true, false, true, true, true),
            8,
        )
        .ok_or("invalid repair request")?;
        let proof = repair_proof();
        let dispatcher = Arc::new(RepairPlanningDispatch {
            roots: vec![root.clone()],
            closure: vec![root.clone(), dependency.clone()],
            request: request.clone(),
            proof: proof.clone(),
            calls: AtomicUsize::new(0),
        });
        let worker_dispatcher = Arc::clone(&dispatcher);
        let worker = thread::spawn(move || -> Result<(), HelperTransportError> {
            for _ in 0..2 {
                let (stream, _) = listener.accept().map_err(|_| {
                    HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
                })?;
                serve_helper_connection(stream, helper_uid, worker_dispatcher.as_ref())?;
            }
            Ok(())
        });
        let client = RootHelperClient::at(endpoint, helper_uid);

        assert_eq!(
            client.closure_for_roots(std::slice::from_ref(&root))?,
            vec![root, dependency]
        );
        assert_eq!(client.repair_plan_proof(&request)?, proof);
        worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))??;
        assert_eq!(dispatcher.calls.load(Ordering::Relaxed), 2);
        Ok(())
    }

    #[test]
    fn streaming_build_preserves_order_and_rejects_non_monotonic_progress()
    -> Result<(), Box<dyn Error>> {
        let mut observed = Vec::new();
        let (result, sink_failed, served) =
            run_progress_build(vec![100, 200], Duration::ZERO, &mut |estimate| {
                observed.push(estimate.millionths());
                Ok(())
            })?;
        assert_eq!(result, Ok(build_report()));
        assert_eq!(observed, [100, 200]);
        assert!(!sink_failed);
        assert!(served.is_ok());

        let (result, _, _) = run_progress_build(vec![200, 100], Duration::ZERO, &mut |_| Ok(()))?;
        assert_eq!(result, Err(NixAdapterError::OperationFailed));
        Ok(())
    }

    #[test]
    fn build_callback_failure_disconnects_the_helper_progress_sink() -> Result<(), Box<dyn Error>> {
        let (result, sink_failed, served) =
            run_progress_build(vec![100, 200], Duration::from_millis(50), &mut |_| {
                Err(NixAdapterError::OperationFailed)
            })?;
        assert_eq!(result, Err(NixAdapterError::OperationFailed));
        assert!(sink_failed);
        assert_eq!(
            served.map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::TransportFailure)
        );
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_transition() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RootSetTransitionRequest::new(
            1001,
            GenerationId::new("gen-0007")?,
            GenerationId::new("gen-0008")?,
            vec![RootName::new("hello-out")?],
        )?;

        let report = client.transition_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        let report = report?;
        assert_eq!(report.root_set().entry_count(), 1);
        assert_eq!(report.retained_names()[0].as_str(), "hello-out");
        assert!(
            report
                .root_set()
                .reference()
                .as_str()
                .ends_with("/1001/gen-0008")
        );
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_attestation() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let expected = authenticated
            .for_caller(1001)
            .publish_root_set(&root_set())?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RootSetAttestationRequest::new(1001, GenerationId::new("gen-0007")?);

        let report = client.attest_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        assert_eq!(report?, expected);
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_removal() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        authenticated
            .for_caller(1001)
            .publish_root_set(&root_set())?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RemoveRootSetRequest::new(1001, GenerationId::new("gen-0007")?);

        let removed = client.remove_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        assert_eq!(removed, Ok(()));
        Ok(())
    }

    /// Serves one scripted reply to the exact root-publication request and
    /// returns the mapped client result. The script fixes both the response
    /// frame id and the response kind, so id correlation failures and kind
    /// mismatches share one socket setup.
    fn scripted_reply_result(
        response_id: u64,
        response: BrokerHelperResponse,
    ) -> Result<Result<RootSetReport, HelperTransportErrorCode>, Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let helper_uid = Uid::effective().as_raw();
        let expected = publication();
        let worker = thread::spawn(move || -> Result<(), HelperTransportError> {
            let (mut stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            let mut header = [0_u8; FRAME_HEADER_BYTES];
            stream.read_exact(&mut header).map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            let payload_length =
                u32::from_be_bytes(header[16..20].try_into().map_err(|_| {
                    HelperTransportError::new(HelperTransportErrorCode::InvalidFrame)
                })?) as usize;
            let mut request = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
            request.extend_from_slice(&header);
            request.resize(FRAME_HEADER_BYTES + payload_length, 0);
            stream
                .read_exact(&mut request[FRAME_HEADER_BYTES..])
                .map_err(|_| {
                    HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
                })?;
            assert_eq!(
                ProductFrameCodec::decode_helper_request(&request),
                Ok((REQUEST_ID, BrokerHelperRequest::PublishRootSet(expected)))
            );
            let response = ProductFrameCodec::encode_helper_response(response_id, &response)
                .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
            stream
                .write_all(&response)
                .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
        });
        let client = RootHelperClient::at(endpoint, helper_uid);

        let result = client
            .publish_root_set(&publication())
            .map_err(HelperTransportError::code);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        Ok(result)
    }

    #[test]
    fn wrong_response_id_is_rejected_after_exact_request() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            scripted_reply_result(REQUEST_ID + 1, BrokerHelperResponse::RootSetRemoved)?,
            Err(HelperTransportErrorCode::InvalidFrame)
        );
        Ok(())
    }

    #[test]
    fn wrong_response_kind_is_rejected_after_exact_request() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            scripted_reply_result(REQUEST_ID, BrokerHelperResponse::RootSetRemoved)?,
            Err(HelperTransportErrorCode::InvalidFrame)
        );
        Ok(())
    }

    #[test]
    fn response_timeouts_remain_fixed() -> Result<(), Box<dyn Error>> {
        assert_eq!(RESPONSE_TIMEOUT, Duration::from_secs(30));
        assert_eq!(
            repair_response_timeout()?,
            MAX_REPAIR_EXECUTION_DURATION + REPAIR_RESPONSE_GRACE
        );
        Ok(())
    }

    #[test]
    fn non_root_helper_peer_is_rejected_before_request_bytes() -> Result<(), Box<dyn Error>> {
        if Uid::effective().is_root() {
            return Ok(());
        }
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let worker = thread::spawn(move || listener.accept().map(|_| ()));
        let client = RootHelperClient::at(endpoint, 0);

        assert_eq!(
            client
                .publish_root_set(&publication())
                .map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::UnauthenticatedPeer)
        );
        worker
            .join()
            .map_err(|_| io::Error::other("peer worker panicked"))??;
        Ok(())
    }

    #[test]
    fn debug_never_exposes_the_private_endpoint() {
        let client = RootHelperClient::at(PathBuf::from("/private/secret.sock"), 0);
        assert_eq!(
            format!("{client:?}"),
            "RootHelperClient(<fixed-private-endpoint>)"
        );
    }
}

/// Handles one build-stream response frame.
///
/// Returns `Ok(Some(report))` when the build finished, `Ok(None)` when
/// progress was forwarded and the stream continues.
fn handle_build_response(
    response: Box<RootNixResponse>,
    last_progress: &mut u32,
    progress: &mut dyn FnMut(pkg_nix::BuildProgressEstimate) -> Result<(), NixAdapterError>,
) -> Result<Option<BuildReport>, NixAdapterError> {
    match *response {
        RootNixResponse::BuildProgress(estimate) => {
            if estimate.millionths() <= *last_progress {
                return Err(NixAdapterError::OperationFailed);
            }
            *last_progress = estimate.millionths();
            progress(estimate)?;
            Ok(None)
        }
        RootNixResponse::Build(report) => Ok(Some(report)),
        RootNixResponse::Failed {
            operation: RootNixOperation::Build,
            failure: RootNixFailure::Adapter(code),
        } => Err(NixAdapterError::remote(code)),
        RootNixResponse::Failed {
            operation: RootNixOperation::Build,
            ..
        } => Err(NixAdapterError::Unavailable),
        _ => Err(NixAdapterError::OperationFailed),
    }
}
