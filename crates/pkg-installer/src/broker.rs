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
    AuthenticatedCaller, BrokerErrorCode, BuildExecutionErrorCode, BuildPreparationErrorCode,
    BuildPreview, BuildProgressEstimate, BuildReport, BuildRootPublicationErrorCode,
    CacheInstallErrorCode, CacheInstallOutcome, CatalogInfoReport, CatalogInfoRequest,
    CatalogSearchReport, CatalogSearchRequest, ChannelRefreshErrorCode, ChannelRefreshMode,
    ChannelRefreshReport, CliBrokerRequest, CliBrokerResponse, Digest, GenerationId,
    GenerationRootAttestationErrorCode, GenerationRootRemovalErrorCode,
    GenerationRootTransitionErrorCode, HostResourceProbe, InProcessBroker, InProcessCallerPeer,
    InstallDownloadProgress, MaintenanceError, MethodKind, NixAdapter, NixAdapterError,
    OperationHandle, ProductFrameCodec, RepairGenerationErrorCode, RepairGenerationReport,
    RepairGenerationRequest, RootSetIntent, RootSetReport, RootSetTransitionIntent,
    RootSetTransitionReport,
};
use pkg_pipeline::{AuthenticatedBuildAuthority, BuildAuthorityErrorCode};
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::fd::AsFd,
    os::unix::net::UnixStream,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const FRAME_READ_TIMEOUT: Duration = Duration::from_mins(5);
const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const BROKER_READINESS_TIMEOUT: Duration = Duration::from_mins(2);

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

/// Completes one fixed doctor lifecycle against the installed broker endpoint.
pub fn probe_broker_readiness(path: &Path) -> Result<(), BrokerTransportError> {
    let mut stream = UnixStream::connect(path)
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    probe_broker_stream(&mut stream)
}

fn probe_broker_stream(stream: &mut UnixStream) -> Result<(), BrokerTransportError> {
    let CliBrokerResponse::Started(handle) = readiness_transaction(
        stream,
        1,
        &CliBrokerRequest::Begin(pkg_nix::BrokerOperationKind::Doctor),
    )?
    else {
        return Err(BrokerTransportError::new(
            BrokerTransportErrorCode::BrokerFailure,
        ));
    };
    match readiness_transaction(stream, 2, &CliBrokerRequest::Version(handle.clone()))? {
        CliBrokerResponse::Version(_) => {}
        _ => {
            return Err(BrokerTransportError::new(
                BrokerTransportErrorCode::BrokerFailure,
            ));
        }
    }
    match readiness_transaction(stream, 3, &CliBrokerRequest::Complete(handle))? {
        CliBrokerResponse::Completed => Ok(()),
        _ => Err(BrokerTransportError::new(
            BrokerTransportErrorCode::BrokerFailure,
        )),
    }
}

fn readiness_transaction(
    stream: &mut UnixStream,
    request_id: u64,
    request: &CliBrokerRequest,
) -> Result<CliBrokerResponse, BrokerTransportError> {
    let frame = ProductFrameCodec::encode_cli_request(request_id, request)
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
    write_all_until(stream, &frame, deadline_after(FRAME_WRITE_TIMEOUT)?)?;
    let frame = read_frame_with_timeout(stream, BROKER_READINESS_TIMEOUT)?
        .ok_or_else(|| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    let (response_id, response) = ProductFrameCodec::decode_cli_response(&frame)
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
    if response_id != request_id {
        return Err(BrokerTransportError::new(
            BrokerTransportErrorCode::InvalidFrame,
        ));
    }
    Ok(response)
}

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
    serve_broker_connection_inner(&mut stream, broker, ConnectionAuthorities::default())
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
    serve_broker_connection_inner(
        &mut stream,
        broker,
        ConnectionAuthorities {
            adapter: Some(adapter),
            ..ConnectionAuthorities::default()
        },
    )
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
        ConnectionAuthorities {
            adapter: Some(adapter),
            approval_audit: Some(approval_audit),
            ..ConnectionAuthorities::default()
        },
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
        ConnectionAuthorities {
            adapter: Some(&adapter),
            approval_audit: Some(approval_audit),
            build: Some(authority.as_ref()),
            ..ConnectionAuthorities::default()
        },
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
        ConnectionAuthorities {
            adapter: Some(&adapter),
            approval_audit: Some(approval_audit),
            build: Some(authority.as_ref()),
            roots: Some(roots),
            ..ConnectionAuthorities::default()
        },
    )
}

/// Serves the complete production authority, including authenticated channel refresh.
///
/// The refresh request carries only a caller-bound operation handle. The
/// implementation owns every channel origin, trust root, system, target, and
/// publication decision.
///
/// # Errors
///
/// Refuses unauthenticated peers, invalid frames, unavailable authorities,
/// helper failures, or bounded transport failures.
pub fn serve_broker_connection_with_product_authority(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
    approval_audit: &BrokerApprovalAudit,
    authority: &Arc<AuthenticatedBuildAuthority>,
    roots: &RootHelperClient,
    refresh: &dyn ChannelRefreshDispatch,
    repair: &dyn RepairAuthorityDispatch,
) -> Result<(), BrokerTransportError> {
    let adapter = authority.adapter();
    serve_broker_connection_inner(
        &mut stream,
        broker,
        ConnectionAuthorities {
            adapter: Some(&adapter),
            approval_audit: Some(approval_audit),
            build: Some(authority.as_ref()),
            roots: Some(roots),
            refresh: Some(refresh),
            repair: Some(repair),
        },
    )
}

/// Closed production channel-refresh capability installed by the broker service.
pub trait ChannelRefreshDispatch: Send + Sync {
    /// Authenticates and atomically publishes the current signed channel/index pair.
    ///
    /// Returns one closed failure category when refresh is refused.
    ///
    /// # Errors
    ///
    /// Returns only sanitized network, verification, contention, or service
    /// failure classes.
    fn refresh(
        &self,
        mode: ChannelRefreshMode,
    ) -> Result<ChannelRefreshReport, ChannelRefreshErrorCode>;
}

/// Closed production repair capability installed by the broker service.
pub trait RepairAuthorityDispatch: Send + Sync {
    /// Repairs one authenticated rooted generation without accepting paths or Nix controls.
    ///
    /// A successful authority must close its repair through
    /// [`AuthenticatedCaller::complete_repair_dispatch`].
    ///
    /// # Errors
    ///
    /// Returns only a stable sanitized repair failure category.
    fn repair(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        request: &RepairGenerationRequest,
        approval_journal: Option<&BrokerCallerApprovalJournal>,
    ) -> Result<RepairGenerationReport, RepairGenerationErrorCode>;
}

trait BuildAuthorityDispatch: Send + Sync {
    fn runtime_asset_manifest_digest(&self) -> Result<Digest, ()> {
        Err(())
    }

    fn search(&self, request: &CatalogSearchRequest) -> Result<CatalogSearchReport, ()>;

    fn info(&self, requests: &[CatalogInfoRequest]) -> Result<Vec<CatalogInfoReport>, ()>;

    fn acquire(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CacheInstallOutcome, CacheInstallErrorCode>;

    fn prepare(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, BuildPreparationErrorCode>;

    fn execute(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        digest: Digest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
    ) -> Result<BuildReport, BrokerErrorCode>;
}

trait RootAuthorityDispatch: Send + Sync {
    fn verify_managed_ownership(&self, _digest: Digest) -> Result<bool, ()> {
        Err(())
    }

    fn publish(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        intent: RootSetIntent,
    ) -> Result<RootSetReport, BrokerErrorCode>;

    fn transition(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        intent: RootSetTransitionIntent,
    ) -> Result<RootSetTransitionReport, BrokerErrorCode>;

    fn remove(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        generation: GenerationId,
    ) -> Result<(), BrokerErrorCode>;

    fn attest(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        generation: GenerationId,
    ) -> Result<RootSetReport, BrokerErrorCode>;
}

#[derive(Clone, Copy)]
struct DispatchAuthorities<'a> {
    adapter: Option<&'a Arc<dyn NixAdapter>>,
    approval_journal: Option<&'a BrokerCallerApprovalJournal>,
    build: Option<&'a dyn BuildAuthorityDispatch>,
    roots: Option<&'a dyn RootAuthorityDispatch>,
    refresh: Option<&'a dyn ChannelRefreshDispatch>,
    repair: Option<&'a dyn RepairAuthorityDispatch>,
}

#[derive(Clone, Copy, Default)]
struct ConnectionAuthorities<'a> {
    adapter: Option<&'a Arc<dyn NixAdapter>>,
    approval_audit: Option<&'a BrokerApprovalAudit>,
    build: Option<&'a dyn BuildAuthorityDispatch>,
    roots: Option<&'a dyn RootAuthorityDispatch>,
    refresh: Option<&'a dyn ChannelRefreshDispatch>,
    repair: Option<&'a dyn RepairAuthorityDispatch>,
}

impl RootAuthorityDispatch for RootHelperClient {
    fn verify_managed_ownership(&self, digest: Digest) -> Result<bool, ()> {
        self.verify_managed_ownership(digest).map_err(|_| ())
    }

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

    fn transition(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        intent: RootSetTransitionIntent,
    ) -> Result<RootSetTransitionReport, BrokerErrorCode> {
        caller
            .transition_root_intent(handle, intent, |request| {
                self.transition_root_set(&request)
                    .map_err(|_| MaintenanceError::backend_failure())
            })
            .map_err(pkg_nix::BrokerError::code)
    }

    fn remove(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        generation: GenerationId,
    ) -> Result<(), BrokerErrorCode> {
        caller
            .remove_generation_root_intent(handle, generation, |request| {
                self.remove_root_set(request)
                    .map_err(|_| MaintenanceError::backend_failure())
            })
            .map_err(pkg_nix::BrokerError::code)
    }

    fn attest(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        generation: GenerationId,
    ) -> Result<RootSetReport, BrokerErrorCode> {
        caller
            .attest_generation_root_intent(handle, generation, |request| {
                self.attest_root_set(request)
                    .map_err(|_| MaintenanceError::backend_failure())
            })
            .map_err(pkg_nix::BrokerError::code)
    }
}

impl BuildAuthorityDispatch for AuthenticatedBuildAuthority {
    fn runtime_asset_manifest_digest(&self) -> Result<Digest, ()> {
        self.runtime_asset_manifest_digest().map_err(|_| ())
    }

    fn search(&self, request: &CatalogSearchRequest) -> Result<CatalogSearchReport, ()> {
        self.search_catalog(request).map_err(|_| ())
    }

    fn info(&self, requests: &[CatalogInfoRequest]) -> Result<Vec<CatalogInfoReport>, ()> {
        self.info_catalog(requests).map_err(|_| ())
    }

    fn acquire(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CacheInstallOutcome, CacheInstallErrorCode> {
        self.acquire_install_with_progress(&selectors, caller, handle, progress)
            .map_err(|error| match error.code() {
                BuildAuthorityErrorCode::AcquisitionCancelled => CacheInstallErrorCode::Cancelled,
                BuildAuthorityErrorCode::AcquisitionIntentRefused => {
                    CacheInstallErrorCode::InvalidIntent
                }
                BuildAuthorityErrorCode::AcquisitionRefused => {
                    CacheInstallErrorCode::AcquisitionFailed
                }
                _ => CacheInstallErrorCode::AuthorityUnavailable,
            })
    }

    fn prepare(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, BuildPreparationErrorCode> {
        self.prepare_and_install(selectors, caller, handle)
            .map_err(pkg_pipeline::BuildPreparationError::code)
    }

    fn execute(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        digest: Digest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
    ) -> Result<BuildReport, BrokerErrorCode> {
        let adapter = self.adapter();
        caller
            .execute_prepared_build_with_progress(
                handle,
                digest,
                &HostResourceProbe::new(),
                adapter.as_ref(),
                &mut |estimate| progress(estimate).map_err(|()| NixAdapterError::OperationFailed),
            )
            .map_err(pkg_nix::BrokerError::code)
    }
}

fn serve_broker_connection_inner(
    stream: &mut UnixStream,
    broker: &Arc<InProcessBroker>,
    authorities: ConnectionAuthorities<'_>,
) -> Result<(), BrokerTransportError> {
    let uid = peer_uid(stream)
        .map_err(|()| BrokerTransportError::new(BrokerTransportErrorCode::UnauthenticatedPeer))?;
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
    let approval_journal = approval_journal_for_peer(authorities.approval_audit, uid)?;
    stream
        .set_nonblocking(true)
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    let result = serve_frames(stream, |request, progress| {
        dispatch_request_with_progress(
            &caller,
            request,
            DispatchAuthorities {
                adapter: authorities.adapter,
                approval_journal: approval_journal.as_ref(),
                build: authorities.build,
                roots: authorities.roots,
                refresh: authorities.refresh,
                repair: authorities.repair,
            },
            progress,
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

fn approval_journal_for_peer(
    audit: Option<&BrokerApprovalAudit>,
    uid: u32,
) -> Result<Option<BrokerCallerApprovalJournal>, BrokerTransportError> {
    if uid == 0 {
        return Ok(None);
    }
    audit
        .map(|audit| audit.for_caller(uid))
        .transpose()
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))
}

#[cfg(test)]
fn dispatch_request(
    caller: &AuthenticatedCaller,
    request: CliBrokerRequest,
    adapter: Option<&Arc<dyn NixAdapter>>,
    approval_journal: Option<&BrokerCallerApprovalJournal>,
    authority: Option<&dyn BuildAuthorityDispatch>,
    roots: Option<&dyn RootAuthorityDispatch>,
) -> Result<CliBrokerResponse, ()> {
    dispatch_request_with_progress(
        caller,
        request,
        DispatchAuthorities {
            adapter,
            approval_journal,
            build: authority,
            roots,
            refresh: None,
            repair: None,
        },
        &mut |_| Ok(()),
    )
}

#[cfg(test)]
fn dispatch_request_with_refresh(
    caller: &AuthenticatedCaller,
    request: CliBrokerRequest,
    refresh: &dyn ChannelRefreshDispatch,
) -> Result<CliBrokerResponse, ()> {
    dispatch_request_with_progress(
        caller,
        request,
        DispatchAuthorities {
            adapter: None,
            approval_journal: None,
            build: None,
            roots: None,
            refresh: Some(refresh),
            repair: None,
        },
        &mut |_| Ok(()),
    )
}

#[allow(clippy::too_many_lines)]
fn dispatch_request_with_progress(
    caller: &AuthenticatedCaller,
    request: CliBrokerRequest,
    authorities: DispatchAuthorities<'_>,
    progress: &mut dyn FnMut(CliBrokerResponse) -> Result<(), ()>,
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
        CliBrokerRequest::Cancel(handle) => cancel_response(caller, &handle),
        CliBrokerRequest::Complete(handle) => complete_response(caller, &handle),
        CliBrokerRequest::Version(handle) => dispatch_version(caller, authorities.adapter, &handle),
        CliBrokerRequest::VerifyManagedOwnership(handle) => {
            caller.poll(&handle).map_err(|_| ())?;
            let digest = authorities
                .build
                .ok_or(())?
                .runtime_asset_manifest_digest()?;
            authorities
                .roots
                .ok_or(())?
                .verify_managed_ownership(digest)
                .map(CliBrokerResponse::ManagedOwnership)
        }
        CliBrokerRequest::EvaluateDerivation(handle, request) => {
            dispatch_evaluation(caller, authorities.adapter, &handle, &request)
        }
        CliBrokerRequest::PathInfo(handle, path) => dispatch_path_query(
            caller,
            authorities.adapter,
            &handle,
            MethodKind::PathInfo,
            &path,
        ),
        CliBrokerRequest::Substitute(handle, path) => dispatch_path_query(
            caller,
            authorities.adapter,
            &handle,
            MethodKind::Substitute,
            &path,
        ),
        CliBrokerRequest::ApproveBuild(handle, approval) => {
            let timestamp = broker_timestamp()?;
            caller
                .approve_build(
                    &handle,
                    approval.build_plan_digest(),
                    approval.source(),
                    &timestamp,
                    authorities.approval_journal.ok_or(())?,
                )
                .map_err(|_| ())?;
            Ok(CliBrokerResponse::BuildApproved)
        }
        CliBrokerRequest::Verify(handle, request) => {
            dispatch_verify(caller, authorities.adapter, &handle, &request)
        }
        CliBrokerRequest::Gc(handle) => dispatch_gc(caller, authorities.adapter, &handle),
        CliBrokerRequest::AcquireGc(handle) => gc_admission_response(caller, &handle),
        CliBrokerRequest::GetInstallEvidence(handle) => install_evidence_response(caller, &handle),
        CliBrokerRequest::GetBuildPreview(handle) => build_preview_response(caller, &handle),
        CliBrokerRequest::PrepareBuild(handle, selectors) => Ok(authorities.build.map_or(
            CliBrokerResponse::BuildPreparationRefused(BuildPreparationErrorCode::BrokerRefused),
            |authority| match authority.prepare(selectors, caller, &handle) {
                Ok(preview) => CliBrokerResponse::BuildPrepared(preview),
                Err(code) => CliBrokerResponse::BuildPreparationRefused(code),
            },
        )),
        CliBrokerRequest::ExecuteBuild(handle, digest) => Ok(build_execution_response(
            authorities.build,
            caller,
            &handle,
            digest,
            progress,
        )),
        CliBrokerRequest::PublishBuildRoots(handle, intent) => Ok(build_root_response(
            authorities.roots,
            caller,
            &handle,
            intent,
        )),
        CliBrokerRequest::TransitionGenerationRoots(handle, intent) => Ok(
            generation_root_transition_response(authorities.roots, caller, &handle, intent),
        ),
        CliBrokerRequest::RemoveGenerationRoots(handle, generation) => Ok(
            generation_root_removal_response(authorities.roots, caller, &handle, generation),
        ),
        CliBrokerRequest::AttestGenerationRoots(handle, generation) => Ok(
            generation_root_attestation_response(authorities.roots, caller, &handle, generation),
        ),
        CliBrokerRequest::AcquireInstall(handle, selectors) => {
            let mut download_progress =
                |download| progress(CliBrokerResponse::InstallDownloadProgress(download));
            Ok(install_acquisition_response(
                authorities.build,
                caller,
                &handle,
                selectors,
                &mut download_progress,
            ))
        }
        CliBrokerRequest::RefreshChannel(handle, mode) => {
            dispatch_channel_refresh(caller, authorities.refresh, &handle, mode)
        }
        CliBrokerRequest::SearchCatalog(handle, request) => {
            dispatch_catalog_search(caller, authorities.build, &handle, &request)
        }
        CliBrokerRequest::InfoCatalog(handle, requests) => {
            dispatch_catalog_info(caller, authorities.build, &handle, &requests)
        }
        CliBrokerRequest::RepairGeneration(handle, request) => Ok(repair_generation_response(
            authorities.repair,
            caller,
            &handle,
            &request,
            authorities.approval_journal,
        )),
    }
}

/// Runs one repair authority body inside the repair-execution lifecycle.
///
/// A begin or finish failure always maps to `admission_failure`, including when
/// the body itself already failed, so a failed cleanup never reports a body
/// result as if admission were healthy.
pub fn run_repair_dispatch<T, E>(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    body: impl FnOnce() -> Result<T, E>,
    admission_failure: impl Fn() -> E,
) -> Result<T, E> {
    caller
        .begin_repair_dispatch(handle)
        .map_err(|_| admission_failure())?;
    let result = body();
    let success = result.is_ok();
    if caller.finish_repair_dispatch(handle, success).is_err() {
        return Err(admission_failure());
    }
    result
}

fn repair_generation_response(
    authority: Option<&dyn RepairAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    request: &RepairGenerationRequest,
    approval_journal: Option<&BrokerCallerApprovalJournal>,
) -> CliBrokerResponse {
    let Some(repair) = authority else {
        return CliBrokerResponse::RepairGenerationRefused(
            RepairGenerationErrorCode::AuthorityUnavailable,
        );
    };
    match run_repair_dispatch(
        caller,
        handle,
        || repair.repair(caller, handle, request, approval_journal),
        || RepairGenerationErrorCode::AdmissionFailed,
    ) {
        Ok(report) => CliBrokerResponse::RepairGeneration(report),
        Err(error) => CliBrokerResponse::RepairGenerationRefused(error),
    }
}

fn dispatch_evaluation(
    caller: &AuthenticatedCaller,
    adapter: Option<&Arc<dyn NixAdapter>>,
    handle: &OperationHandle,
    request: &pkg_nix::EvaluateDerivationRequest,
) -> Result<CliBrokerResponse, ()> {
    caller
        .authorize_adapter_call(handle, MethodKind::EvaluateDerivation)
        .map_err(|_| ())?;
    Ok(adapter_response(
        MethodKind::EvaluateDerivation,
        adapter.ok_or(())?.evaluate_derivation(request),
        CliBrokerResponse::DerivationPlan,
    ))
}

fn dispatch_path_query(
    caller: &AuthenticatedCaller,
    adapter: Option<&Arc<dyn NixAdapter>>,
    handle: &OperationHandle,
    method: MethodKind,
    path: &pkg_nix::StorePath,
) -> Result<CliBrokerResponse, ()> {
    caller
        .authorize_adapter_call(handle, method)
        .map_err(|_| ())?;
    match method {
        MethodKind::PathInfo => Ok(adapter_response(
            method,
            adapter.ok_or(())?.path_info(path),
            CliBrokerResponse::PathInfo,
        )),
        MethodKind::Substitute => Ok(adapter_response(
            method,
            adapter.ok_or(())?.substitute(path),
            CliBrokerResponse::Substitute,
        )),
        _ => Err(()),
    }
}

fn dispatch_verify(
    caller: &AuthenticatedCaller,
    adapter: Option<&Arc<dyn NixAdapter>>,
    handle: &OperationHandle,
    request: &pkg_nix::VerifyRequest,
) -> Result<CliBrokerResponse, ()> {
    caller
        .authorize_adapter_call(handle, MethodKind::Verify)
        .map_err(|_| ())?;
    Ok(adapter_response(
        MethodKind::Verify,
        adapter.ok_or(())?.verify(request),
        CliBrokerResponse::Verify,
    ))
}

fn dispatch_channel_refresh(
    caller: &AuthenticatedCaller,
    refresh: Option<&dyn ChannelRefreshDispatch>,
    handle: &OperationHandle,
    mode: ChannelRefreshMode,
) -> Result<CliBrokerResponse, ()> {
    caller.authorize_channel_refresh(handle).map_err(|_| ())?;
    Ok(match refresh.ok_or(())?.refresh(mode) {
        Ok(report) => CliBrokerResponse::ChannelRefreshed(report),
        Err(code) => CliBrokerResponse::ChannelRefreshRefused(code),
    })
}

fn dispatch_catalog_search(
    caller: &AuthenticatedCaller,
    authority: Option<&dyn BuildAuthorityDispatch>,
    handle: &OperationHandle,
    request: &CatalogSearchRequest,
) -> Result<CliBrokerResponse, ()> {
    caller.authorize_catalog_query(handle).map_err(|_| ())?;
    Ok(authority
        .ok_or(())?
        .search(request)
        .map_or(CliBrokerResponse::CatalogSearchRefused, |report| {
            CliBrokerResponse::CatalogSearch(report)
        }))
}

fn dispatch_catalog_info(
    caller: &AuthenticatedCaller,
    authority: Option<&dyn BuildAuthorityDispatch>,
    handle: &OperationHandle,
    requests: &[CatalogInfoRequest],
) -> Result<CliBrokerResponse, ()> {
    caller.authorize_catalog_query(handle).map_err(|_| ())?;
    Ok(authority
        .ok_or(())?
        .info(requests)
        .map_or(CliBrokerResponse::CatalogInfoRefused, |reports| {
            CliBrokerResponse::CatalogInfo(reports)
        }))
}

fn dispatch_version(
    caller: &AuthenticatedCaller,
    adapter: Option<&Arc<dyn NixAdapter>>,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller
        .authorize_adapter_call(handle, MethodKind::Version)
        .map_err(|_| ())?;
    Ok(adapter_response(
        MethodKind::Version,
        adapter.ok_or(())?.version(),
        CliBrokerResponse::Version,
    ))
}

fn install_acquisition_response(
    authority: Option<&dyn BuildAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    selectors: Vec<PackageSelector>,
    progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
) -> CliBrokerResponse {
    let Some(authority) = authority else {
        return CliBrokerResponse::InstallAcquisitionRefused(
            CacheInstallErrorCode::AuthorityUnavailable,
        );
    };
    match authority.acquire(selectors, caller, handle, progress) {
        Ok(CacheInstallOutcome::Acquired) => CliBrokerResponse::InstallAcquired,
        Ok(CacheInstallOutcome::BuildRequired) => CliBrokerResponse::InstallBuildRequired,
        Err(code) => CliBrokerResponse::InstallAcquisitionRefused(code),
    }
}

fn cancel_response(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller.cancel(handle).map_err(|_| ())?;
    Ok(CliBrokerResponse::Cancelled)
}

fn generation_root_attestation_response(
    roots: Option<&dyn RootAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    generation: GenerationId,
) -> CliBrokerResponse {
    roots.map_or(
        CliBrokerResponse::GenerationRootAttestationRefused(
            GenerationRootAttestationErrorCode::AuthorityUnavailable,
        ),
        |roots| match roots.attest(caller, handle, generation) {
            Ok(report) => CliBrokerResponse::GenerationRootsAttested(report),
            Err(code) => CliBrokerResponse::GenerationRootAttestationRefused(
                map_generation_root_attestation_error(code),
            ),
        },
    )
}

const fn map_generation_root_attestation_error(
    code: BrokerErrorCode,
) -> GenerationRootAttestationErrorCode {
    match code {
        BrokerErrorCode::InvalidAdmissionTransition => {
            GenerationRootAttestationErrorCode::InvalidIntent
        }
        BrokerErrorCode::RootPublicationFailed => {
            GenerationRootAttestationErrorCode::AttestationFailed
        }
        BrokerErrorCode::AdmissionCancelled => GenerationRootAttestationErrorCode::Cancelled,
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
        | BrokerErrorCode::CacheAcquisitionFailed
        | BrokerErrorCode::InvalidChildPolicy
        | BrokerErrorCode::EntropyUnavailable => {
            GenerationRootAttestationErrorCode::AuthorityUnavailable
        }
    }
}

fn install_evidence_response(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller
        .install_evidence(handle)
        .map(CliBrokerResponse::InstallEvidence)
        .map_err(|_| ())
}

fn complete_response(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller.complete(handle).map_err(|_| ())?;
    Ok(CliBrokerResponse::Completed)
}

fn gc_admission_response(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller
        .acquire_gc_wait(handle)
        .map(|()| CliBrokerResponse::GcAdmissionAcquired)
        .map_err(|_| ())
}

fn build_preview_response(
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
) -> Result<CliBrokerResponse, ()> {
    caller
        .build_preview(handle)
        .map(CliBrokerResponse::BuildPreview)
        .map_err(|_| ())
}

fn generation_root_removal_response(
    roots: Option<&dyn RootAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    generation: GenerationId,
) -> CliBrokerResponse {
    roots.map_or(
        CliBrokerResponse::GenerationRootRemovalRefused(
            GenerationRootRemovalErrorCode::AuthorityUnavailable,
        ),
        |roots| match roots.remove(caller, handle, generation) {
            Ok(()) => CliBrokerResponse::GenerationRootsRemoved,
            Err(code) => CliBrokerResponse::GenerationRootRemovalRefused(
                map_generation_root_removal_error(code),
            ),
        },
    )
}

const fn map_generation_root_removal_error(
    code: BrokerErrorCode,
) -> GenerationRootRemovalErrorCode {
    match code {
        BrokerErrorCode::InvalidAdmissionTransition => {
            GenerationRootRemovalErrorCode::InvalidIntent
        }
        BrokerErrorCode::RootPublicationFailed => GenerationRootRemovalErrorCode::RemovalFailed,
        BrokerErrorCode::AdmissionCancelled => GenerationRootRemovalErrorCode::Cancelled,
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
        | BrokerErrorCode::CacheAcquisitionFailed
        | BrokerErrorCode::InvalidChildPolicy
        | BrokerErrorCode::EntropyUnavailable => {
            GenerationRootRemovalErrorCode::AuthorityUnavailable
        }
    }
}

fn generation_root_transition_response(
    roots: Option<&dyn RootAuthorityDispatch>,
    caller: &AuthenticatedCaller,
    handle: &OperationHandle,
    intent: RootSetTransitionIntent,
) -> CliBrokerResponse {
    roots.map_or(
        CliBrokerResponse::GenerationRootTransitionRefused(
            GenerationRootTransitionErrorCode::AuthorityUnavailable,
        ),
        |roots| match roots.transition(caller, handle, intent) {
            Ok(report) => CliBrokerResponse::GenerationRootsTransitioned(report),
            Err(code) => CliBrokerResponse::GenerationRootTransitionRefused(
                map_generation_root_transition_error(code),
            ),
        },
    )
}

const fn map_generation_root_transition_error(
    code: BrokerErrorCode,
) -> GenerationRootTransitionErrorCode {
    match code {
        BrokerErrorCode::InvalidAdmissionTransition => {
            GenerationRootTransitionErrorCode::InvalidIntent
        }
        BrokerErrorCode::RootPublicationFailed => {
            GenerationRootTransitionErrorCode::TransitionFailed
        }
        BrokerErrorCode::AdmissionCancelled => GenerationRootTransitionErrorCode::Cancelled,
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
        | BrokerErrorCode::CacheAcquisitionFailed
        | BrokerErrorCode::InvalidChildPolicy
        | BrokerErrorCode::EntropyUnavailable => {
            GenerationRootTransitionErrorCode::AuthorityUnavailable
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
        | BrokerErrorCode::CacheAcquisitionFailed
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
    progress: &mut dyn FnMut(CliBrokerResponse) -> Result<(), ()>,
) -> CliBrokerResponse {
    authority.map_or(
        CliBrokerResponse::BuildExecutionRefused(BuildExecutionErrorCode::AuthorityUnavailable),
        |authority| match authority.execute(caller, handle, digest, &mut |estimate| {
            progress(CliBrokerResponse::BuildExecutionProgress(estimate))
        }) {
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
        | BrokerErrorCode::CacheAcquisitionFailed
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
    mut dispatch: impl FnMut(
        CliBrokerRequest,
        &mut dyn FnMut(CliBrokerResponse) -> Result<(), ()>,
    ) -> Result<CliBrokerResponse, ()>,
) -> Result<(), BrokerTransportError> {
    loop {
        let Some(frame) = read_frame(stream)? else {
            return Ok(());
        };
        let (request_id, request) = ProductFrameCodec::decode_cli_request(&frame)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
        let mut progress_error = None;
        let response = {
            let mut progress = |response| {
                let encoded = ProductFrameCodec::encode_cli_response(request_id, &response)
                    .map_err(|_| {
                        progress_error = Some(BrokerTransportError::new(
                            BrokerTransportErrorCode::InvalidFrame,
                        ));
                    })?;
                let deadline = deadline_after(FRAME_WRITE_TIMEOUT).map_err(|error| {
                    progress_error = Some(error);
                })?;
                write_all_until(stream, &encoded, deadline).map_err(|error| {
                    progress_error = Some(error);
                })
            };
            dispatch(request, &mut progress)
        };
        if let Some(error) = progress_error {
            return Err(error);
        }
        let response = response
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
        AcceptedFormats, ApprovalSource, BrokerOperationKind, BuildApprovalRequest, BuildPlan,
        BuildPlanTarget, BuildReadiness, CacheClassification, DerivationPath, DerivationPlanReport,
        Digest, EvaluatedDerivation, FormatVersion, GenerationId, NarHash, NixVersion,
        NixpkgsRevision, OperationStatus, OutputName, PackageVersion, PolicyVersion, RootName,
        RootSetEntry, StorePath, System, TrustedBuildReplanner, TrustedReplanError, VersionInfo,
    };
    use pkg_testkit::FakeNix;
    use std::{
        collections::BTreeMap, io, net::Shutdown, os::unix::fs::PermissionsExt, str::FromStr,
        thread,
    };
    use tempfile::TempDir;

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    #[test]
    fn readiness_probe_completes_a_real_broker_lifecycle() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let broker = Arc::new(InProcessBroker::new().unwrap());
        let fake = Arc::new(FakeNix::new());
        fake.expect_version(Ok(VersionInfo::new(
            NixVersion::new("2.34.8").unwrap(),
            AcceptedFormats::new(FormatVersion::new(1).unwrap()),
        )));
        let adapter: Arc<dyn NixAdapter> = fake.clone();
        let worker =
            thread::spawn(move || serve_broker_connection_with_nix(server, &broker, &adapter));

        assert_eq!(probe_broker_stream(&mut client), Ok(()));
        client.shutdown(Shutdown::Both).unwrap();
        assert_eq!(worker.join().unwrap(), Ok(()));
        assert_eq!(fake.assert_exhausted(), Ok(()));
    }

    #[test]
    fn root_readiness_has_no_build_approval_journal() {
        let temporary = TempDir::new().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let audit =
            BrokerApprovalAudit::open(temporary.path(), nix::unistd::Uid::effective().as_raw())
                .unwrap();

        assert!(
            approval_journal_for_peer(Some(&audit), 0)
                .unwrap()
                .is_none()
        );
        assert!(
            approval_journal_for_peer(Some(&audit), 1001)
                .unwrap()
                .is_some()
        );
    }

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
                OutputSelection::default_selection(),
                SourceRevision::CurrentChannel,
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
        fn runtime_asset_manifest_digest(&self) -> Result<Digest, ()> {
            Ok(Digest::from_bytes([0x5a; 32]))
        }

        fn search(&self, request: &CatalogSearchRequest) -> Result<CatalogSearchReport, ()> {
            if request.query() != "ripgrep" {
                return Err(());
            }
            CatalogSearchReport::new(
                ChannelSequence::from_u64(42).unwrap(),
                "2026-08-19T00:00:00Z",
                vec![
                    pkg_nix::CatalogPackageSummary::new(
                        "ripgrep",
                        "ripgrep",
                        "14.1.1",
                        "fast search",
                        vec![String::from("MIT")],
                        true,
                        false,
                    )
                    .unwrap(),
                ],
            )
            .ok_or(())
        }

        fn info(&self, requests: &[CatalogInfoRequest]) -> Result<Vec<CatalogInfoReport>, ()> {
            requests
                .iter()
                .map(|request| {
                    let lookup = if request.selector() == "ripgrep" {
                        let summary = pkg_nix::CatalogPackageSummary::new(
                            "ripgrep",
                            "ripgrep",
                            "14.1.1",
                            "fast search",
                            vec![String::from("MIT")],
                            true,
                            false,
                        )
                        .unwrap();
                        let package = pkg_nix::CatalogPackageInfo::new(
                            summary,
                            "https://example.invalid/ripgrep",
                            vec![String::from("out")],
                            vec![String::from("linux-x86-64")],
                            REVISION,
                            "2026-08-12T00:00:00Z",
                        )
                        .unwrap();
                        pkg_nix::CatalogInfoLookup::Found(Box::new(package))
                    } else {
                        pkg_nix::CatalogInfoLookup::NotFound
                    };
                    CatalogInfoReport::new(ChannelSequence::from_u64(42).unwrap(), lookup).ok_or(())
                })
                .collect()
        }

        fn acquire(
            &self,
            selectors: Vec<PackageSelector>,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
        ) -> Result<CacheInstallOutcome, CacheInstallErrorCode> {
            if selectors.len() != 1 || selectors[0].selector().as_str() != "hello" {
                return Err(CacheInstallErrorCode::InvalidIntent);
            }
            Ok(CacheInstallOutcome::BuildRequired)
        }

        fn prepare(
            &self,
            selectors: Vec<PackageSelector>,
            caller: &AuthenticatedCaller,
            handle: &OperationHandle,
        ) -> Result<BuildPreview, BuildPreparationErrorCode> {
            if selectors.len() != 1 {
                return Err(BuildPreparationErrorCode::IntentRefused);
            }
            match selectors[0].selector().as_str() {
                "host-refusal" => return Err(BuildPreparationErrorCode::HostRefused),
                "planning-refusal" => return Err(BuildPreparationErrorCode::PlanningRefused),
                "broker-refusal" => return Err(BuildPreparationErrorCode::BrokerRefused),
                "hello" => {}
                _ => return Err(BuildPreparationErrorCode::IntentRefused),
            }
            let plan = self.0.clone();
            let preview = plan
                .preview()
                .map_err(|_| BuildPreparationErrorCode::PlanningRefused)?;
            caller
                .prepare_build_with_replanner(handle, plan.clone(), Arc::new(TestReplanner(plan)))
                .map_err(|_| BuildPreparationErrorCode::BrokerRefused)?;
            Ok(preview)
        }

        fn execute(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _digest: Digest,
            _progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), ()>,
        ) -> Result<BuildReport, BrokerErrorCode> {
            Err(BrokerErrorCode::BuildResourcePreflightFailed)
        }
    }

    struct TestChannelRefresh(Result<ChannelRefreshReport, ChannelRefreshErrorCode>);

    impl ChannelRefreshDispatch for TestChannelRefresh {
        fn refresh(
            &self,
            _mode: ChannelRefreshMode,
        ) -> Result<ChannelRefreshReport, ChannelRefreshErrorCode> {
            self.0
        }
    }

    struct TestRepairAuthority {
        seen: std::sync::Mutex<Option<(u32, String, bool)>>,
    }

    impl RepairAuthorityDispatch for TestRepairAuthority {
        fn repair(
            &self,
            caller: &AuthenticatedCaller,
            handle: &OperationHandle,
            request: &RepairGenerationRequest,
            _approval_journal: Option<&BrokerCallerApprovalJournal>,
        ) -> Result<RepairGenerationReport, RepairGenerationErrorCode> {
            let uid = caller
                .authorize_repair(handle)
                .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
            *self
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
                uid,
                request.generation().as_str().to_owned(),
                request.verify_only(),
            ));
            caller
                .complete_repair_dispatch(handle)
                .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
            RepairGenerationReport::new(pkg_nix::RepairGenerationStatus::DamageDetected, 2)
                .map_err(|_| RepairGenerationErrorCode::VerifyFailed)
        }
    }

    #[test]
    fn repair_dispatch_uses_authenticated_uid_and_path_free_intent() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
        let authority = TestRepairAuthority {
            seen: std::sync::Mutex::new(None),
        };
        let request = RepairGenerationRequest::new(GenerationId::new("gen-0042").unwrap(), true);
        assert_eq!(
            dispatch_request_with_progress(
                &caller,
                CliBrokerRequest::RepairGeneration(handle.clone(), request),
                DispatchAuthorities {
                    adapter: None,
                    approval_journal: None,
                    build: None,
                    roots: None,
                    refresh: None,
                    repair: Some(&authority),
                },
                &mut |_| Ok(()),
            ),
            Ok(CliBrokerResponse::RepairGeneration(
                RepairGenerationReport::new(pkg_nix::RepairGenerationStatus::DamageDetected, 2,)
                    .unwrap()
            ))
        );
        assert_eq!(
            *authority
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some((1001, String::from("gen-0042"), true))
        );
        assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
        assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
    }

    struct ObservingRepairAuthority {
        broker: Arc<InProcessBroker>,
        inhibited_during: std::sync::Mutex<usize>,
        gc_blocked_during: std::sync::Mutex<bool>,
    }

    impl RepairAuthorityDispatch for ObservingRepairAuthority {
        fn repair(
            &self,
            caller: &AuthenticatedCaller,
            handle: &OperationHandle,
            _request: &RepairGenerationRequest,
            _approval_journal: Option<&BrokerCallerApprovalJournal>,
        ) -> Result<RepairGenerationReport, RepairGenerationErrorCode> {
            caller
                .authorize_repair(handle)
                .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
            *self
                .inhibited_during
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                self.broker.admission_snapshot().gc_inhibitor_count();
            let gc = caller
                .begin(BrokerOperationKind::Gc)
                .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
            *self
                .gc_blocked_during
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                caller.acquire_gc(&gc).is_err();
            let _ = caller.cancel(&gc);
            caller
                .complete_repair_dispatch(handle)
                .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
            RepairGenerationReport::new(pkg_nix::RepairGenerationStatus::DamageDetected, 2)
                .map_err(|_| RepairGenerationErrorCode::VerifyFailed)
        }
    }

    #[test]
    fn repair_dispatch_holds_gc_inhibitor_across_authority_and_releases_after() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
        let authority = ObservingRepairAuthority {
            broker: broker.clone(),
            inhibited_during: std::sync::Mutex::new(0),
            gc_blocked_during: std::sync::Mutex::new(false),
        };
        let request = RepairGenerationRequest::new(GenerationId::new("gen-0043").unwrap(), true);

        let response = dispatch_request_with_progress(
            &caller,
            CliBrokerRequest::RepairGeneration(handle.clone(), request),
            DispatchAuthorities {
                adapter: None,
                approval_journal: None,
                build: None,
                roots: None,
                refresh: None,
                repair: Some(&authority),
            },
            &mut |_| Ok(()),
        );

        assert!(matches!(
            response,
            Ok(CliBrokerResponse::RepairGeneration(_))
        ));
        assert_eq!(
            *authority
                .inhibited_during
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );
        assert!(
            *authority
                .gc_blocked_during
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );
        assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
        assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    }

    #[test]
    fn channel_refresh_requires_refresh_authority_and_returns_only_the_report() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let refresh_handle = caller.begin(BrokerOperationKind::Refresh).unwrap();
        let report = ChannelRefreshReport::new(true, ChannelSequence::from_u64(43).unwrap());
        let refresh = TestChannelRefresh(Ok(report));

        assert_eq!(
            dispatch_request_with_refresh(
                &caller,
                CliBrokerRequest::RefreshChannel(refresh_handle.clone(), ChannelRefreshMode::Apply,),
                &refresh,
            ),
            Ok(CliBrokerResponse::ChannelRefreshed(report))
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Complete(refresh_handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Completed)
        );

        let doctor_handle = caller.begin(BrokerOperationKind::Doctor).unwrap();
        assert_eq!(
            dispatch_request_with_refresh(
                &caller,
                CliBrokerRequest::RefreshChannel(doctor_handle.clone(), ChannelRefreshMode::Apply,),
                &refresh,
            ),
            Err(())
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Cancel(doctor_handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Cancelled)
        );
    }

    #[test]
    fn channel_refresh_refusal_is_typed() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Refresh).unwrap();

        assert_eq!(
            dispatch_request_with_refresh(
                &caller,
                CliBrokerRequest::RefreshChannel(handle.clone(), ChannelRefreshMode::Apply),
                &TestChannelRefresh(Err(ChannelRefreshErrorCode::Verification)),
            ),
            Ok(CliBrokerResponse::ChannelRefreshRefused(
                ChannelRefreshErrorCode::Verification
            ))
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Cancel(handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Cancelled)
        );
    }

    #[test]
    fn catalog_queries_require_resolve_authority_and_return_product_metadata() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let authority = TestAuthority(build_plan());
        let handle = caller.begin(BrokerOperationKind::Resolve).unwrap();

        let response = dispatch_request(
            &caller,
            CliBrokerRequest::SearchCatalog(
                handle.clone(),
                CatalogSearchRequest::new("ripgrep", 25, false, None).unwrap(),
            ),
            None,
            None,
            Some(&authority),
            None,
        )
        .unwrap();
        assert!(matches!(response, CliBrokerResponse::CatalogSearch(_)));
        let CliBrokerResponse::CatalogSearch(report) = response else {
            return;
        };
        assert_eq!(report.results()[0].package(), "ripgrep");

        let response = dispatch_request(
            &caller,
            CliBrokerRequest::InfoCatalog(
                handle.clone(),
                vec![
                    CatalogInfoRequest::new("ripgrep").unwrap(),
                    CatalogInfoRequest::new("missing").unwrap(),
                ],
            ),
            None,
            None,
            Some(&authority),
            None,
        )
        .unwrap();
        assert!(matches!(
            response,
            CliBrokerResponse::CatalogInfo(ref reports)
                if reports.len() == 2
                    && matches!(reports[0].lookup(), pkg_nix::CatalogInfoLookup::Found(_))
                    && matches!(reports[1].lookup(), pkg_nix::CatalogInfoLookup::NotFound)
        ));

        let doctor = caller.begin(BrokerOperationKind::Doctor).unwrap();
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::InfoCatalog(
                    doctor.clone(),
                    vec![CatalogInfoRequest::new("ripgrep").unwrap()],
                ),
                None,
                None,
                Some(&authority),
                None,
            ),
            Err(())
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Cancel(doctor),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Cancelled)
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Complete(handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Completed)
        );
    }

    #[test]
    fn frame_server_streams_progress_before_the_correlated_terminal_response() {
        let (mut server, mut client) = UnixStream::pair().unwrap();
        let handle = InProcessBroker::new()
            .unwrap()
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap()
            .begin(BrokerOperationKind::Acquire)
            .unwrap();
        let selector = PackageSelector::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        );
        let request = CliBrokerRequest::AcquireInstall(handle, vec![selector]);
        client
            .write_all(&ProductFrameCodec::encode_cli_request(7, &request).unwrap())
            .unwrap();
        let worker = thread::spawn(move || {
            server.set_nonblocking(true).unwrap();
            serve_frames(&mut server, |actual, progress| {
                assert_eq!(actual, request);
                progress(CliBrokerResponse::InstallDownloadProgress(
                    InstallDownloadProgress::new(SelectorInput::new("hello").unwrap(), 0, 42)
                        .unwrap(),
                ))?;
                progress(CliBrokerResponse::InstallDownloadProgress(
                    InstallDownloadProgress::new(SelectorInput::new("hello").unwrap(), 42, 42)
                        .unwrap(),
                ))?;
                Ok(CliBrokerResponse::InstallAcquired)
            })
        });
        client.set_nonblocking(true).unwrap();
        let responses = (0..3)
            .map(|_| {
                let frame = read_frame_with_timeout(&mut client, Duration::from_secs(2))?
                    .ok_or_else(|| {
                        BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure)
                    })?;
                ProductFrameCodec::decode_cli_response(&frame)
                    .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(responses.iter().all(|(request_id, _)| *request_id == 7));
        assert!(matches!(
            responses[0].1,
            CliBrokerResponse::InstallDownloadProgress(_)
        ));
        assert!(matches!(
            responses[1].1,
            CliBrokerResponse::InstallDownloadProgress(_)
        ));
        assert_eq!(responses[2].1, CliBrokerResponse::InstallAcquired);
        client.shutdown(Shutdown::Both).unwrap();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn frame_server_streams_build_estimates_before_the_terminal_response() {
        let (mut server, mut client) = UnixStream::pair().unwrap();
        let handle = InProcessBroker::new()
            .unwrap()
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap()
            .begin(BrokerOperationKind::Build)
            .unwrap();
        let request = CliBrokerRequest::ExecuteBuild(handle, Digest::from_bytes([0x42; 32]));
        client
            .write_all(&ProductFrameCodec::encode_cli_request(9, &request).unwrap())
            .unwrap();
        let worker = thread::spawn(move || {
            server.set_nonblocking(true).unwrap();
            serve_frames(&mut server, |actual, progress| {
                assert_eq!(actual, request);
                progress(CliBrokerResponse::BuildExecutionProgress(
                    BuildProgressEstimate::new(250_000).unwrap(),
                ))?;
                progress(CliBrokerResponse::BuildExecutionProgress(
                    BuildProgressEstimate::new(750_000).unwrap(),
                ))?;
                Ok(CliBrokerResponse::BuildExecutionRefused(
                    BuildExecutionErrorCode::ExecutionFailed,
                ))
            })
        });
        client.set_nonblocking(true).unwrap();
        let responses = (0..3)
            .map(|_| {
                let frame = read_frame_with_timeout(&mut client, Duration::from_secs(2))?
                    .ok_or_else(|| {
                        BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure)
                    })?;
                ProductFrameCodec::decode_cli_response(&frame)
                    .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(responses.iter().all(|(request_id, _)| *request_id == 9));
        assert_eq!(
            responses[0].1,
            CliBrokerResponse::BuildExecutionProgress(BuildProgressEstimate::new(250_000).unwrap())
        );
        assert_eq!(
            responses[1].1,
            CliBrokerResponse::BuildExecutionProgress(BuildProgressEstimate::new(750_000).unwrap())
        );
        assert_eq!(
            responses[2].1,
            CliBrokerResponse::BuildExecutionRefused(BuildExecutionErrorCode::ExecutionFailed)
        );
        client.shutdown(Shutdown::Both).unwrap();
        worker.join().unwrap().unwrap();
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

        fn transition(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _intent: RootSetTransitionIntent,
        ) -> Result<RootSetTransitionReport, BrokerErrorCode> {
            Err(self.0)
        }

        fn remove(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _generation: GenerationId,
        ) -> Result<(), BrokerErrorCode> {
            Err(self.0)
        }

        fn attest(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _generation: GenerationId,
        ) -> Result<RootSetReport, BrokerErrorCode> {
            Err(self.0)
        }
    }

    struct AcceptingRootRemoval;

    impl RootAuthorityDispatch for AcceptingRootRemoval {
        fn verify_managed_ownership(&self, digest: Digest) -> Result<bool, ()> {
            Ok(digest == Digest::from_bytes([0x5a; 32]))
        }

        fn publish(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _intent: RootSetIntent,
        ) -> Result<RootSetReport, BrokerErrorCode> {
            Err(BrokerErrorCode::InvalidAdmissionTransition)
        }

        fn transition(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _intent: RootSetTransitionIntent,
        ) -> Result<RootSetTransitionReport, BrokerErrorCode> {
            Err(BrokerErrorCode::InvalidAdmissionTransition)
        }

        fn remove(
            &self,
            caller: &AuthenticatedCaller,
            handle: &OperationHandle,
            generation: GenerationId,
        ) -> Result<(), BrokerErrorCode> {
            caller
                .remove_generation_root_intent(handle, generation, |request| {
                    assert_eq!(request.owner_uid(), 1001);
                    assert_eq!(request.generation().as_str(), "gen-0007");
                    Ok(())
                })
                .map_err(pkg_nix::BrokerError::code)
        }

        fn attest(
            &self,
            _caller: &AuthenticatedCaller,
            _handle: &OperationHandle,
            _generation: GenerationId,
        ) -> Result<RootSetReport, BrokerErrorCode> {
            Err(BrokerErrorCode::InvalidAdmissionTransition)
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

    #[test]
    fn managed_ownership_uses_signed_digest_and_privileged_verifier() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let authority = TestAuthority(build_plan());
        let roots = AcceptingRootRemoval;
        let handle = caller.begin(BrokerOperationKind::Doctor).unwrap();

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::VerifyManagedOwnership(handle),
                None,
                None,
                Some(&authority),
                Some(&roots),
            ),
            Ok(CliBrokerResponse::ManagedOwnership(true))
        );
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
    fn cache_acquisition_dispatch_is_closed_and_reports_build_required() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
        let selector = PackageSelector::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        );
        let authority = TestAuthority(build_plan());

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::AcquireInstall(handle.clone(), vec![selector.clone()]),
                None,
                None,
                Some(&authority),
                None,
            ),
            Ok(CliBrokerResponse::InstallBuildRequired)
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::AcquireInstall(handle, vec![selector]),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::InstallAcquisitionRefused(
                CacheInstallErrorCode::AuthorityUnavailable
            ))
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
    fn broker_disconnect_cancels_running_recovery_handle() -> Result<(), Box<dyn Error>> {
        let broker = InProcessBroker::new()?;
        let (first_server, mut first_client) = UnixStream::pair()?;
        let first_broker = Arc::clone(&broker);
        let first_worker =
            thread::spawn(move || serve_broker_connection(first_server, &first_broker));

        first_client.write_all(&ProductFrameCodec::encode_cli_request(
            1,
            &CliBrokerRequest::Begin(BrokerOperationKind::Activate),
        )?)?;
        let (_, started) =
            ProductFrameCodec::decode_cli_response(&read_response(&mut first_client)?)?;
        let CliBrokerResponse::Started(handle) = started else {
            return Err(io::Error::other("expected started response").into());
        };
        drop(first_client);
        first_worker
            .join()
            .map_err(|_| io::Error::other("broker thread panicked"))??;

        let (second_server, mut second_client) = UnixStream::pair()?;
        let second_broker = Arc::clone(&broker);
        let second_worker =
            thread::spawn(move || serve_broker_connection(second_server, &second_broker));
        second_client.write_all(&ProductFrameCodec::encode_cli_request(
            1,
            &CliBrokerRequest::Poll(handle),
        )?)?;
        let (_, status) =
            ProductFrameCodec::decode_cli_response(&read_response(&mut second_client)?)?;
        assert_eq!(
            status,
            CliBrokerResponse::Status(OperationStatus::Cancelled)
        );
        second_client.shutdown(Shutdown::Write)?;
        second_worker
            .join()
            .map_err(|_| io::Error::other("broker thread panicked"))??;
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
    fn preparation_dispatch_returns_every_refusal_without_closing_the_operation() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let authority = TestAuthority(build_plan());
        for (selector, code) in [
            ("host-refusal", BuildPreparationErrorCode::HostRefused),
            ("intent-refusal", BuildPreparationErrorCode::IntentRefused),
            (
                "planning-refusal",
                BuildPreparationErrorCode::PlanningRefused,
            ),
            ("broker-refusal", BuildPreparationErrorCode::BrokerRefused),
        ] {
            let handle = caller.begin(BrokerOperationKind::Build).unwrap();
            let selector = PackageSelector::new(
                SelectorId::new(&format!("sel_{}", selector.replace('-', "_"))).unwrap(),
                SelectorInput::new(selector).unwrap(),
                VersionPreference::Any,
                OutputSelection::default_selection(),
                SourceRevision::CurrentChannel,
            );
            assert_eq!(
                dispatch_request(
                    &caller,
                    CliBrokerRequest::PrepareBuild(handle.clone(), vec![selector]),
                    None,
                    None,
                    Some(&authority),
                    None,
                ),
                Ok(CliBrokerResponse::BuildPreparationRefused(code))
            );
            assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
            caller.cancel(&handle).unwrap();
        }

        let handle = caller.begin(BrokerOperationKind::Build).unwrap();
        let selector = PackageSelector::new(
            SelectorId::new("sel_missing_authority").unwrap(),
            SelectorInput::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        );
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::PrepareBuild(handle.clone(), vec![selector]),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::BuildPreparationRefused(
                BuildPreparationErrorCode::BrokerRefused
            ))
        );
        assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Running);
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

    #[test]
    fn generation_root_transition_dispatch_is_closed_and_complete_releases_admission() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
        let intent = || {
            RootSetTransitionIntent::new(
                pkg_nix::GenerationId::new("gen-0007").unwrap(),
                pkg_nix::GenerationId::new("gen-0008").unwrap(),
                vec![pkg_nix::RootName::new("hello-out").unwrap()],
            )
            .unwrap()
        };

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::TransitionGenerationRoots(handle.clone(), intent()),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::GenerationRootTransitionRefused(
                GenerationRootTransitionErrorCode::AuthorityUnavailable,
            ))
        );
        for (broker_code, wire_code) in [
            (
                BrokerErrorCode::InvalidAdmissionTransition,
                GenerationRootTransitionErrorCode::InvalidIntent,
            ),
            (
                BrokerErrorCode::RootPublicationFailed,
                GenerationRootTransitionErrorCode::TransitionFailed,
            ),
            (
                BrokerErrorCode::AdmissionCancelled,
                GenerationRootTransitionErrorCode::Cancelled,
            ),
            (
                BrokerErrorCode::SessionRestarted,
                GenerationRootTransitionErrorCode::AuthorityUnavailable,
            ),
        ] {
            let authority = RefusingRootAuthority(broker_code);
            assert_eq!(
                dispatch_request(
                    &caller,
                    CliBrokerRequest::TransitionGenerationRoots(handle.clone(), intent()),
                    None,
                    None,
                    None,
                    Some(&authority),
                ),
                Ok(CliBrokerResponse::GenerationRootTransitionRefused(
                    wire_code
                ))
            );
        }

        caller.acquire_gc_inhibit(&handle).unwrap();
        assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 1);
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Complete(handle.clone()),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Completed)
        );
        assert_eq!(broker.admission_snapshot().gc_inhibitor_count(), 0);
        assert_eq!(caller.poll(&handle).unwrap(), OperationStatus::Completed);
    }

    #[test]
    fn generation_root_attestation_dispatch_has_closed_failures() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Activate).unwrap();
        let generation = || GenerationId::new("gen-0007").unwrap();

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::AttestGenerationRoots(handle.clone(), generation()),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::GenerationRootAttestationRefused(
                GenerationRootAttestationErrorCode::AuthorityUnavailable,
            ))
        );
        for (broker_code, wire_code) in [
            (
                BrokerErrorCode::InvalidAdmissionTransition,
                GenerationRootAttestationErrorCode::InvalidIntent,
            ),
            (
                BrokerErrorCode::RootPublicationFailed,
                GenerationRootAttestationErrorCode::AttestationFailed,
            ),
            (
                BrokerErrorCode::AdmissionCancelled,
                GenerationRootAttestationErrorCode::Cancelled,
            ),
            (
                BrokerErrorCode::SessionRestarted,
                GenerationRootAttestationErrorCode::AuthorityUnavailable,
            ),
        ] {
            let authority = RefusingRootAuthority(broker_code);
            assert_eq!(
                dispatch_request(
                    &caller,
                    CliBrokerRequest::AttestGenerationRoots(handle.clone(), generation()),
                    None,
                    None,
                    None,
                    Some(&authority),
                ),
                Ok(CliBrokerResponse::GenerationRootAttestationRefused(
                    wire_code
                ))
            );
        }
    }

    #[test]
    fn generation_root_removal_injects_uid_and_retains_gc_admission() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Gc).unwrap();
        let generation = || GenerationId::new("gen-0007").unwrap();

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::RemoveGenerationRoots(handle.clone(), generation()),
                None,
                None,
                None,
                Some(&AcceptingRootRemoval),
            ),
            Ok(CliBrokerResponse::GenerationRootsRemoved)
        );
        assert!(broker.admission_snapshot().gc_held());
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::RemoveGenerationRoots(handle.clone(), generation()),
                None,
                None,
                None,
                Some(&AcceptingRootRemoval),
            ),
            Ok(CliBrokerResponse::GenerationRootsRemoved)
        );
        assert!(broker.admission_snapshot().gc_held());
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Complete(handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Completed)
        );
        assert!(!broker.admission_snapshot().gc_held());
    }

    #[test]
    fn explicit_gc_admission_is_retained_until_completion() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Gc).unwrap();

        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::AcquireGc(handle.clone()),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::GcAdmissionAcquired)
        );
        assert!(broker.admission_snapshot().gc_held());
        assert_eq!(
            dispatch_request(
                &caller,
                CliBrokerRequest::Complete(handle),
                None,
                None,
                None,
                None,
            ),
            Ok(CliBrokerResponse::Completed)
        );
        assert!(!broker.admission_snapshot().gc_held());
    }
}
