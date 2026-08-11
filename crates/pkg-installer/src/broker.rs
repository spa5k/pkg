//! Unix CLI-to-broker transport with kernel-derived caller identity.

use crate::{
    BrokerApprovalAudit, BrokerCallerApprovalJournal, RootHelperClient, platform::peer_uid,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};
use pkg_core::PackageSelector;
use pkg_nix::{
    AuthenticatedCaller, BrokerErrorCode, BuildExecutionErrorCode, BuildPreview, BuildReport,
    BuildRootPublicationErrorCode, CliBrokerRequest, CliBrokerResponse, Digest, HostResourceProbe,
    InProcessBroker, InProcessCallerPeer, MaintenanceError, MethodKind, NixAdapter,
    NixAdapterError, OperationHandle, ProductFrameCodec, RootSetIntent, RootSetReport,
};
use pkg_pipeline::AuthenticatedBuildAuthority;
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::fd::AsFd,
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const FRAME_READ_TIMEOUT: Duration = Duration::from_mins(5);
const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable CLI-to-broker transport failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerTransportErrorCode {
    /// Kernel peer credentials were unavailable.
    UnauthenticatedPeer,
    /// The byte stream ended mid-frame or response I/O failed.
    TransportFailure,
    /// The strict product frame was invalid.
    InvalidFrame,
    /// The authenticated operation lifecycle rejected the request.
    BrokerFailure,
}

/// Redacted CLI-to-broker transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerTransportError {
    code: BrokerTransportErrorCode,
}

impl BrokerTransportError {
    const fn new(code: BrokerTransportErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> BrokerTransportErrorCode {
        self.code
    }
}

impl fmt::Display for BrokerTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("package broker transport failed")
    }
}

impl Error for BrokerTransportError {}

/// Authenticates one connection and serves lifecycle frames until disconnect.
///
/// The uid is obtained once from `SO_PEERCRED` (Linux) or `getpeereid`
/// (macOS); no payload identity is read.
/// Disconnect always invokes broker-owned cleanup for this caller session.
///
/// # Errors
///
/// Returns a redacted error for peer-credential, bounded transport, strict
/// framing, or authenticated lifecycle failure.
pub fn serve_broker_connection(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
) -> Result<(), BrokerTransportError> {
    serve_broker_connection_inner(&mut stream, broker, None, None, None, None)
}

/// Serves lifecycle plus typed managed-Nix calls for one authenticated peer.
///
/// # Errors
///
/// Returns a redacted transport error for authentication, framing, lifecycle,
/// adapter, or bounded I/O failures.
pub fn serve_broker_connection_with_nix(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
    adapter: &Arc<dyn NixAdapter>,
) -> Result<(), BrokerTransportError> {
    serve_broker_connection_inner(&mut stream, broker, Some(adapter), None, None, None)
}

/// Serves typed managed-Nix calls plus broker-private durable build approval.
///
/// The audit is bound to the kernel-derived peer uid inside this function.
/// No caller-supplied identity or receipt is accepted.
///
/// # Errors
///
/// Returns a redacted transport error for authentication, framing, lifecycle,
/// adapter, audit, or bounded I/O failures.
pub fn serve_broker_connection_with_nix_and_approval(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
    adapter: &Arc<dyn NixAdapter>,
    approval_audit: &BrokerApprovalAudit,
) -> Result<(), BrokerTransportError> {
    serve_broker_connection_inner(
        &mut stream,
        broker,
        Some(adapter),
        Some(approval_audit),
        None,
        None,
    )
}

/// Serves the complete authenticated build-preparation boundary.
///
/// # Errors
///
/// Refuses unauthenticated peers, invalid frames, broker failures, or bounded
/// transport failures without exposing private authority state.
pub fn serve_broker_connection_with_build_authority(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
    approval_audit: &BrokerApprovalAudit,
    authority: &Arc<AuthenticatedBuildAuthority>,
) -> Result<(), BrokerTransportError> {
    let adapter = authority.adapter();
    serve_broker_connection_inner(
        &mut stream,
        broker,
        Some(&adapter),
        Some(approval_audit),
        Some(authority.as_ref()),
        None,
    )
}

/// Serves authenticated build execution through durable protected rooting.
///
/// # Errors
///
/// Refuses unauthenticated peers, invalid frames, unavailable authority,
/// helper publication failures, or bounded transport failures.
pub fn serve_broker_connection_with_build_and_root_authority(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
    approval_audit: &BrokerApprovalAudit,
    authority: &Arc<AuthenticatedBuildAuthority>,
    roots: &RootHelperClient,
) -> Result<(), BrokerTransportError> {
    let adapter = authority.adapter();
    serve_broker_connection_inner(
        &mut stream,
        broker,
        Some(&adapter),
        Some(approval_audit),
        Some(authority.as_ref()),
        Some(roots),
    )
}

trait BuildAuthorityDispatch: Send + Sync {
    fn prepare(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, ()>;

    fn execute(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        digest: Digest,
    ) -> Result<BuildReport, BrokerErrorCode>;
}

trait RootAuthorityDispatch: Send + Sync {
    fn publish(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        intent: RootSetIntent,
    ) -> Result<RootSetReport, BrokerErrorCode>;
}

impl RootAuthorityDispatch for RootHelperClient {
    fn publish(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        intent: RootSetIntent,
    ) -> Result<RootSetReport, BrokerErrorCode> {
        caller
            .publish_built_root_intent(handle, intent, |roots| {
                self.publish_root_set(roots)
                    .map_err(|_| MaintenanceError::backend_failure())
            })
            .map_err(pkg_nix::BrokerError::code)
    }
}

impl BuildAuthorityDispatch for AuthenticatedBuildAuthority {
    fn prepare(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, ()> {
        self.prepare_and_install(selectors, caller, handle)
            .map_err(|_| ())
    }

    fn execute(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        digest: Digest,
    ) -> Result<BuildReport, BrokerErrorCode> {
        let adapter = self.adapter();
        caller
            .execute_prepared_build(handle, digest, &HostResourceProbe::new(), adapter.as_ref())
            .map_err(pkg_nix::BrokerError::code)
    }
}

fn serve_broker_connection_inner(
    stream: &mut UnixStream,
    broker: &Arc<InProcessBroker>,
    adapter: Option<&Arc<dyn NixAdapter>>,
    approval_audit: Option<&BrokerApprovalAudit>,
    authority: Option<&dyn BuildAuthorityDispatch>,
    roots: Option<&dyn RootAuthorityDispatch>,
) -> Result<(), BrokerTransportError> {
    let uid = peer_uid(stream)
        .map_err(|()| BrokerTransportError::new(BrokerTransportErrorCode::UnauthenticatedPeer))?;
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
    let approval_journal = approval_audit
        .map(|audit| audit.for_caller(uid))
        .transpose()
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
    stream
        .set_nonblocking(true)
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    let result = serve_frames(stream, |request| {
        dispatch_request(
            &caller,
            request,
            adapter,
            approval_journal.as_ref(),
            authority,
            roots,
        )
    });
    let disconnected = caller.disconnect();
    match (result, disconnected) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(_)) => Err(BrokerTransportError::new(
            BrokerTransportErrorCode::BrokerFailure,
        )),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn dispatch_request(
    caller: &AuthenticatedCaller,
    request: CliBrokerRequest,
    adapter: Option<&Arc<dyn NixAdapter>>,
    approval_journal: Option<&BrokerCallerApprovalJournal>,
    authority: Option<&dyn BuildAuthorityDispatch>,
    roots: Option<&dyn RootAuthorityDispatch>,
) -> Result<CliBrokerResponse, ()> {
    match request {
        CliBrokerRequest::Begin(kind) => caller
            .begin(kind)
            .map(CliBrokerResponse::Started)
            .map_err(|_| ()),
        CliBrokerRequest::Poll(handle) => caller
            .poll(&handle)
            .map(CliBrokerResponse::Status)
            .map_err(|_| ()),
        CliBrokerRequest::Cancel(handle) => {
            caller.cancel(&handle).map_err(|_| ())?;
            Ok(CliBrokerResponse::Cancelled)
        }
        CliBrokerRequest::Version(handle) => {
            caller
                .authorize_adapter_call(&handle, MethodKind::Version)
                .map_err(|_| ())?;
            Ok(adapter_response(
                MethodKind::Version,
                adapter.ok_or(())?.version(),
                CliBrokerResponse::Version,
            ))
        }
        CliBrokerRequest::EvaluateDerivation(handle, request) => {
            caller
                .authorize_adapter_call(&handle, MethodKind::EvaluateDerivation)
                .map_err(|_| ())?;
            Ok(adapter_response(
                MethodKind::EvaluateDerivation,
                adapter.ok_or(())?.evaluate_derivation(&request),
                CliBrokerResponse::DerivationPlan,
            ))
        }
        CliBrokerRequest::PathInfo(handle, path) => {
            caller
                .authorize_adapter_call(&handle, MethodKind::PathInfo)
                .map_err(|_| ())?;
            Ok(adapter_response(
                MethodKind::PathInfo,
                adapter.ok_or(())?.path_info(&path),
                CliBrokerResponse::PathInfo,
            ))
        }
        CliBrokerRequest::Substitute(handle, path) => {
            caller
                .authorize_adapter_call(&handle, MethodKind::Substitute)
                .map_err(|_| ())?;
            Ok(adapter_response(
                MethodKind::Substitute,
                adapter.ok_or(())?.substitute(&path),
                CliBrokerResponse::Substitute,
            ))
        }
        CliBrokerRequest::ApproveBuild(handle, approval) => {
            let timestamp = broker_timestamp()?;
            caller
                .approve_build(
                    &handle,
                    approval.build_plan_digest(),
                    approval.source(),
                    &timestamp,
                    approval_journal.ok_or(())?,
                )
                .map_err(|_| ())?;
            Ok(CliBrokerResponse::BuildApproved)
        }
        CliBrokerRequest::Verify(handle, request) => {
            caller
                .authorize_adapter_call(&handle, MethodKind::Verify)
                .map_err(|_| ())?;
            Ok(adapter_response(
                MethodKind::Verify,
                adapter.ok_or(())?.verify(&request),
                CliBrokerResponse::Verify,
            ))
        }
        CliBrokerRequest::Gc(handle) => dispatch_gc(caller, adapter, &handle),
        CliBrokerRequest::GetBuildPreview(handle) => caller
            .build_preview(&handle)
            .map(CliBrokerResponse::BuildPreview)
            .map_err(|_| ()),
        CliBrokerRequest::PrepareBuild(handle, selectors) => authority
            .ok_or(())?
            .prepare(selectors, caller, &handle)
            .map(CliBrokerResponse::BuildPrepared),
        CliBrokerRequest::ExecuteBuild(handle, digest) => {
            Ok(build_execution_response(authority, caller, &handle, digest))
        }
        CliBrokerRequest::PublishBuildRoots(handle, intent) => {
            Ok(build_root_response(roots, caller, &handle, intent))
        }
    }
}

fn dispatch_gc(
    caller: &AuthenticatedCaller,
    adapter: Option<&Arc<dyn NixAdapter>>,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller
        .authorize_adapter_call(handle, MethodKind::Gc)
        .map_err(|_| ())?;
    caller.acquire_gc(handle).map_err(|_| ())?;
    Ok(adapter_response(
        MethodKind::Gc,
        adapter.ok_or(())?.gc(),
        CliBrokerResponse::Gc,
    ))
}

fn build_root_response(
    roots: Option<&dyn RootAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    intent: RootSetIntent,
) -> CliBrokerResponse {
    roots.map_or(
        CliBrokerResponse::BuildRootPublicationRefused(
            BuildRootPublicationErrorCode::AuthorityUnavailable,
        ),
        |roots| match roots.publish(caller, handle, intent) {
            Ok(report) => CliBrokerResponse::BuildRootsPublished(report),
            Err(code) => CliBrokerResponse::BuildRootPublicationRefused(map_build_root_error(code)),
        },
    )
}

const fn map_build_root_error(code: BrokerErrorCode) -> BuildRootPublicationErrorCode {
    match code {
        BrokerErrorCode::InvalidAdmissionTransition => {
            BuildRootPublicationErrorCode::InvalidRootIntent
        }
        BrokerErrorCode::RootPublicationFailed => BuildRootPublicationErrorCode::PublicationFailed,
        BrokerErrorCode::AdmissionCancelled => BuildRootPublicationErrorCode::Cancelled,
        BrokerErrorCode::UnauthenticatedCaller
        | BrokerErrorCode::SessionRestarted
        | BrokerErrorCode::InvalidOperationHandle
        | BrokerErrorCode::OperationExpired
        | BrokerErrorCode::AdmissionBusy
        | BrokerErrorCode::InvalidBuildPlan
        | BrokerErrorCode::BuildApprovalMismatch
        | BrokerErrorCode::BuildApprovalUnavailable
        | BrokerErrorCode::BuildApprovalInvalidated
        | BrokerErrorCode::BuildResourcePreflightFailed
        | BrokerErrorCode::BuildExecutionFailed
        | BrokerErrorCode::InvalidChildPolicy
        | BrokerErrorCode::EntropyUnavailable => {
            BuildRootPublicationErrorCode::AuthorityUnavailable
        }
    }
}

fn build_execution_response(
    authority: Option<&dyn BuildAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    digest: Digest,
) -> CliBrokerResponse {
    authority.map_or(
        CliBrokerResponse::BuildExecutionRefused(BuildExecutionErrorCode::AuthorityUnavailable),
        |authority| match authority.execute(caller, handle, digest) {
            Ok(report) => CliBrokerResponse::BuildExecuted(report),
            Err(code) => CliBrokerResponse::BuildExecutionRefused(map_build_execution_error(code)),
        },
    )
}

const fn map_build_execution_error(code: BrokerErrorCode) -> BuildExecutionErrorCode {
    match code {
        BrokerErrorCode::BuildApprovalMismatch | BrokerErrorCode::BuildApprovalUnavailable => {
            BuildExecutionErrorCode::ApprovalUnavailable
        }
        BrokerErrorCode::BuildApprovalInvalidated => BuildExecutionErrorCode::ApprovalInvalidated,
        BrokerErrorCode::BuildResourcePreflightFailed => {
            BuildExecutionErrorCode::ResourcePreflightFailed
        }
        BrokerErrorCode::BuildExecutionFailed | BrokerErrorCode::RootPublicationFailed => {
            BuildExecutionErrorCode::ExecutionFailed
        }
        BrokerErrorCode::AdmissionCancelled => BuildExecutionErrorCode::Cancelled,
        BrokerErrorCode::UnauthenticatedCaller
        | BrokerErrorCode::SessionRestarted
        | BrokerErrorCode::InvalidOperationHandle
        | BrokerErrorCode::OperationExpired
        | BrokerErrorCode::AdmissionBusy
        | BrokerErrorCode::InvalidAdmissionTransition
        | BrokerErrorCode::InvalidBuildPlan
        | BrokerErrorCode::InvalidChildPolicy
        | BrokerErrorCode::EntropyUnavailable => BuildExecutionErrorCode::AuthorityUnavailable,
    }
}

fn broker_timestamp() -> Result<String, ()> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ())?;
    Ok(format!("unix-ms:{}", elapsed.as_millis()))
}

fn adapter_response<T>(
    method: MethodKind,
    result: Result<T, NixAdapterError>,
    success: impl FnOnce(T) -> CliBrokerResponse,
) -> CliBrokerResponse {
    match result {
        Ok(value) => success(value),
        Err(error) => CliBrokerResponse::AdapterFailure(method, error.code()),
    }
}

fn serve_frames(
    stream: &mut UnixStream,
    mut dispatch: impl FnMut(CliBrokerRequest) -> Result<CliBrokerResponse, ()>,
) -> Result<(), BrokerTransportError> {
    loop {
        let Some(frame) = read_frame(stream)? else {
            return Ok(());
        };
        let (request_id, request) = ProductFrameCodec::decode_cli_request(&frame)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
        let response = dispatch(request)
            .map_err(|()| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
        let encoded = ProductFrameCodec::encode_cli_response(request_id, &response)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(FRAME_WRITE_TIMEOUT)?;
        write_all_until(stream, &encoded, deadline)?;
    }
}

fn read_frame(stream: &mut UnixStream) -> Result<Option<Vec<u8>>, BrokerTransportError> {
    read_frame_with_timeout(stream, FRAME_READ_TIMEOUT)
}

fn read_frame_with_timeout(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, BrokerTransportError> {
    let deadline = deadline_after(timeout)?;
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    loop {
        wait_ready(stream, deadline, PollFlags::POLLIN)?;
        match stream.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            _ => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
    read_exact_until(stream, &mut header[1..], deadline)?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(BrokerTransportError::new(
            BrokerTransportErrorCode::InvalidFrame,
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
    frame.extend_from_slice(&header);
    frame.resize(FRAME_HEADER_BYTES + payload_length, 0);
    read_exact_until(stream, &mut frame[FRAME_HEADER_BYTES..], deadline)?;
    Ok(Some(frame))
}

fn write_all_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), BrokerTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLOUT)?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
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
) -> Result<(), BrokerTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLIN)?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
                ));
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
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
) -> Result<(), BrokerTransportError> {
    loop {
        let timeout = PollTimeout::try_from(remaining(deadline)?)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
        let mut descriptor = [PollFd::new(stream.as_fd(), required)];
        match poll(&mut descriptor, timeout) {
            Ok(0) => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
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
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
}

fn deadline_after(timeout: Duration) -> Result<Instant, BrokerTransportError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))
}

fn remaining(deadline: Instant) -> Result<Duration, BrokerTransportError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    let milliseconds = u64::try_from(remaining.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds != 0)
        .ok_or_else(|| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pkg_channel::BuildMode;
    use pkg_core::{
        channel::{ChannelSequence, SourceRevision},
        selector::{OutputSelection, SelectorId, SelectorInput},
        state::recover_journal,
        version::VersionPreference,
    };
    use pkg_nix::{
        ApprovalSource, BrokerOperationKind, BuildApprovalRequest, BuildPlan, BuildPlanTarget,
        BuildReadiness, CacheClassification, DerivationPath, DerivationPlanReport, Digest,
        EvaluatedDerivation, GenerationId, NarHash, NixVersion, NixpkgsRevision, OperationStatus,
        OutputName, PackageVersion, PolicyVersion, RootName, RootSetEntry, StorePath, System,
        TrustedBuildReplanner, TrustedReplanError,
    };
    use std::{
        collections::BTreeMap, io, net::Shutdown, os::unix::fs::PermissionsExt, str::FromStr,
        thread,
    };
    use tempfile::TempDir;

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    fn build_plan() -> BuildPlan {
        let derivation =
            DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello-1.0.drv")).unwrap();
        let output = StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap();
        let output_name = OutputName::new("out").unwrap();
        let evaluated = EvaluatedDerivation::new(
            derivation.clone(),
            "hello-1.0".to_owned(),
            System::X8664Linux,
            BTreeMap::from([(output_name.clone(), output)]),
            Digest::from_bytes([1; 32]),
            false,
        )
        .unwrap();
        let report = DerivationPlanReport::new(
            4,
            derivation.clone(),
            vec![output_name],
            vec![evaluated],
            Digest::from_bytes([2; 32]),
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
                pkg_nix::AttributePath::new("hello").unwrap(),
                VersionPreference::Any,
                report,
            )],
            vec![derivation],
            CacheClassification::new(Digest::from_bytes([4; 32]), 2, 1, 100, 200).unwrap(),
            BuildReadiness::new(true, false, true, true, true),
            4,
        )
        .unwrap()
    }

    struct TestReplanner(BuildPlan);

    impl TrustedBuildReplanner for TestReplanner {
        fn replan(&self) -> Result<BuildPlan, TrustedReplanError> {
            Ok(self.0.clone())
        }
    }

    struct TestAuthority(BuildPlan);

    impl BuildAuthorityDispatch for TestAuthority {
        fn prepare(
            &self,
            selectors: Vec<PackageSelector>,
            caller: &AuthenticatedCaller,
            handle: &OperationHandle,
        ) -> Result<BuildPreview, ()> {
            if selectors.len() != 1 || selectors[0].selector().as_str() != "hello" {
                return Err(());
            }
            let plan = self.0.clone();
            let preview = plan.preview().map_err(|_| ())?;
            caller
                .prepare_build_with_replanner(handle, plan.clone(), Arc::new(TestReplanner(plan)))
                .map_err(|_| ())?;
            Ok(preview)
        }

        fn execute(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _digest: Digest,
        ) -> Result<BuildReport, BrokerErrorCode> {
            Err(BrokerErrorCode::BuildResourcePreflightFailed)
        }
    }

    struct RefusingRootAuthority(BrokerErrorCode);

    impl RootAuthorityDispatch for RefusingRootAuthority {
        fn publish(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _intent: RootSetIntent,
        ) -> Result<RootSetReport, BrokerErrorCode> {
            Err(self.0)
        }
    }

    fn root_intent() -> RootSetIntent {
        RootSetIntent::new(
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            )],
        )
        .unwrap()
    }

    fn read_response(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        stream.read_exact(&mut header)?;
        let length = u32::from_be_bytes(
            header[16..20]
                .try_into()
                .map_err(|_| io::Error::other("invalid response header"))?,
        ) as usize;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + length);
        frame.extend_from_slice(&header);
        frame.resize(FRAME_HEADER_BYTES + length, 0);
        stream.read_exact(&mut frame[FRAME_HEADER_BYTES..])?;
        Ok(frame)
    }

    fn assert_preview_round_trip(preview: BuildPreview) {
        let encoded = ProductFrameCodec::encode_cli_response(
            17,
            &CliBrokerResponse::BuildPreview(preview.clone()),
        )
        .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((17, CliBrokerResponse::BuildPreview(preview)))
        );
    }

    #[test]
    fn peer_authenticated_connection_serves_lifecycle_until_disconnect()
    -> Result<(), Box<dyn Error>> {
        let broker = InProcessBroker::new()?;
        let (server, mut client) = UnixStream::pair()?;
        let server_broker = Arc::clone(&broker);
        let worker = thread::spawn(move || serve_broker_connection(server, &server_broker));

        client.write_all(&ProductFrameCodec::encode_cli_request(
            1,
            &CliBrokerRequest::Begin(BrokerOperationKind::Resolve),
        )?)?;
        let (_, started) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        let CliBrokerResponse::Started(handle) = started else {
            return Err(io::Error::other("expected started response").into());
        };
        client.write_all(&ProductFrameCodec::encode_cli_request(
            2,
            &CliBrokerRequest::Poll(handle.clone()),
        )?)?;
        let (_, status) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        assert_eq!(status, CliBrokerResponse::Status(OperationStatus::Running));
        client.write_all(&ProductFrameCodec::encode_cli_request(
            3,
            &CliBrokerRequest::Cancel(handle),
        )?)?;
        let (_, cancelled) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        assert_eq!(cancelled, CliBrokerResponse::Cancelled);
        client.shutdown(Shutdown::Write)?;
        worker
            .join()
            .map_err(|_| io::Error::other("broker thread panicked"))??;
        let snapshot = broker.admission_snapshot();
        assert_eq!(snapshot.operation_count(), 1);
        assert!(!snapshot.build_held());
        assert!(!snapshot.gc_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
        Ok(())
    }

    #[test]
    fn partial_authenticated_frame_expires_without_dispatch() -> Result<(), Box<dyn Error>> {
        let (mut server, mut client) = UnixStream::pair()?;
        server.set_nonblocking(true)?;
        client.write_all(b"P")?;
        assert_eq!(
            read_frame_with_timeout(&mut server, Duration::from_millis(50))
                .map_err(BrokerTransportError::code),
            Err(BrokerTransportErrorCode::TransportFailure)
        );
        Ok(())
    }

    #[test]
    fn response_write_expires_when_authenticated_client_stops_reading() -> Result<(), Box<dyn Error>>
    {
        let (mut server, _client) = UnixStream::pair()?;
        socket2::SockRef::from(&server).set_send_buffer_size(4096)?;
        server.set_nonblocking(true)?;
        let bytes = vec![0_u8; MAX_FRAME_PAYLOAD_BYTES];
        assert_eq!(
            write_all_until(
                &mut server,
                &bytes,
                deadline_after(Duration::from_millis(50))?,
            )
            .map_err(BrokerTransportError::code),
            Err(BrokerTransportErrorCode::TransportFailure)
        );
        Ok(())
    }

    #[test]
    fn approval_dispatch_records_authenticated_uid_before_acknowledgement() {
        let temporary = TempDir::new().unwrap();
        let directory = temporary.path().join("broker");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let audit =
            BrokerApprovalAudit::open(&directory, nix::unistd::Uid::effective().as_raw()).unwrap();
        let journal = audit.for_caller(1001).unwrap();
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Build).unwrap();
        let plan = build_plan();
        let digest = plan.digest().unwrap();
        let authority = TestAuthority(plan);
        let selector = PackageSelector::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        );
        let prepared_response = dispatch_request(
            &caller,
            CliBrokerRequest::PrepareBuild(handle.clone(), vec![selector]),
            None,
            Some(&journal),
            Some(&authority),
            None,
        )
        .unwrap();
        assert!(matches!(
            prepared_response,
            CliBrokerResponse::BuildPrepared(_)
        ));
        let preview_response = dispatch_request(
            &caller,
            CliBrokerRequest::GetBuildPreview(handle.clone()),
            None,
            Some(&journal),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            preview_response,
            CliBrokerResponse::BuildPreview(_)
        ));
        let CliBrokerResponse::BuildPreview(preview) = preview_response else {
            return;
        };
        assert_preview_round_trip(preview);
        let request = CliBrokerRequest::ApproveBuild(
            handle.clone(),
            BuildApprovalRequest::new(digest, ApprovalSource::Interactive),
        );
        assert_eq!(
            dispatch_request(&caller, request.clone(), None, Some(&journal), None, None,),
            Ok(CliBrokerResponse::BuildApproved)
        );
        assert!(dispatch_request(&caller, request, None, Some(&journal), None, None).is_err());
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::ExecuteBuild(handle.clone(), digest),
                None,
                Some(&journal),
                Some(&authority),
                None,
            ),
            Ok(CliBrokerResponse::BuildExecutionRefused(
                BuildExecutionErrorCode::ResourcePreflightFailed,
            ))
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Poll(handle),
                None,
                Some(&journal),
                None,
                None,
            ),
            Ok(CliBrokerResponse::Status(OperationStatus::Running))
        );

        let recovery =
            recover_journal(&std::fs::read(directory.join("approvals.ndjson")).unwrap()).unwrap();
        assert!(recovery.quarantined_suffix().is_empty());
        assert_eq!(recovery.accepted().len(), 1);
        assert_eq!(
            recovery.accepted()[0]
                .payload()
                .fields()
                .get("authenticatedUid"),
            Some(&serde_json::json!(1001))
        );
    }

    #[test]
    fn root_publication_dispatch_returns_only_closed_failures_and_remains_usable() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Build).unwrap();

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::PublishBuildRoots(handle.clone(), root_intent()),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::BuildRootPublicationRefused(
                BuildRootPublicationErrorCode::AuthorityUnavailable,
            ))
        );
        for (broker_code, wire_code) in [
            (
                BrokerErrorCode::InvalidAdmissionTransition,
                BuildRootPublicationErrorCode::InvalidRootIntent,
            ),
            (
                BrokerErrorCode::RootPublicationFailed,
                BuildRootPublicationErrorCode::PublicationFailed,
            ),
            (
                BrokerErrorCode::AdmissionCancelled,
                BuildRootPublicationErrorCode::Cancelled,
            ),
            (
                BrokerErrorCode::SessionRestarted,
                BuildRootPublicationErrorCode::AuthorityUnavailable,
            ),
        ] {
            let authority = RefusingRootAuthority(broker_code);
            assert_eq!(
                dispatch_request(
                    &caller,
                    CliBrokerRequest::PublishBuildRoots(handle.clone(), root_intent()),
                    None,
                    None,
                    None,
                    Some(&authority),
                ),
                Ok(CliBrokerResponse::BuildRootPublicationRefused(wire_code))
            );
        }
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Poll(handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Status(OperationStatus::Running))
        );
    }
}
