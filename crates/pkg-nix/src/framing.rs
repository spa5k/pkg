//! Exact product framing for the CLI↔broker and broker↔helper channels.

use std::fmt;
use std::str::FromStr;

use pkg_core::channel::{PolicyVersion, SourceRevision};
use pkg_core::identity::{OutputName, StorePath};
use pkg_core::selector::{OutputSelection, PackageSelector, SelectorId, SelectorInput};
use pkg_core::state::Digest;
use pkg_core::version::{PackageVersion, VersionBound, VersionPreference, VersionRange};
use serde::{Deserialize, Serialize};

use crate::broker::{BrokerOperationKind, OperationHandle, OperationStatus};
use crate::maintenance::{
    GenerationId, MaintenanceCapability, RemoveRootSetRequest, RepairMode, RepairOutcomeKind,
    RepairPathOutcome, RepairStorePathsReport, RepairStorePathsRequest, RootSet, RootSetEntry,
    RootSetIntent, RootSetReport, RootSetTransitionIntent, RootSetTransitionReport,
    RootSetTransitionRequest, VerifiedRepairScope,
};
use crate::{
    ApprovalSource, BuildPreview, BuildReport, DerivationPlanReport, EvaluateDerivationRequest,
    GcReport, JsonCodec, PathInfoReport, RootName, RootRef, SubstituteReport, VerifyReport,
    VerifyRequest, VersionInfo,
};
use crate::{MethodKind, NixAdapterErrorCode};
use serde_json::value::RawValue;

const MAGIC: [u8; 4] = *b"PKG1";
const PROTOCOL_VERSION: u16 = 1;
const HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_ROOT_SET_WIRE_ENTRIES: usize = 4096;
const MAX_BUILD_SELECTOR_WIRE_ENTRIES: usize = 4096;

const CHANNEL_CLI_BROKER: u8 = 1;
const CHANNEL_BROKER_HELPER: u8 = 2;

/// Stable product-frame decoding failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameErrorCode {
    /// The frame exceeded its fixed byte ceiling.
    FrameTooLarge,
    /// The header was truncated or carried the wrong magic.
    MalformedHeader,
    /// The protocol version is not the single supported version.
    UnsupportedVersion,
    /// Channel, method, or request id violated the closed grammar.
    UnsupportedMessage,
    /// Declared and actual payload lengths differed.
    LengthMismatch,
    /// The JSON body was malformed, extended, or invalid after promotion.
    InvalidPayload,
}

/// Redacted product-frame error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameError {
    code: FrameErrorCode,
}

impl FrameError {
    const fn new(code: FrameErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable frame failure category.
    #[must_use]
    pub const fn code(self) -> FrameErrorCode {
        self.code
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "product frame refused: {:?}", self.code)
    }
}

impl std::error::Error for FrameError {}

/// Closed controls on the CLI-to-broker channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliBrokerRequest {
    /// Begin one closed operation class.
    Begin(BrokerOperationKind),
    /// Poll one opaque operation handle.
    Poll(OperationHandle),
    /// Cancel one opaque operation handle.
    Cancel(OperationHandle),
    /// Complete one operation after its local state commit succeeds.
    Complete(OperationHandle),
    /// Query the pinned managed runtime under an authorized operation.
    Version(OperationHandle),
    /// Evaluate one validated derivation request under a resolve operation.
    EvaluateDerivation(OperationHandle, EvaluateDerivationRequest),
    /// Query validated metadata for one promoted store path.
    PathInfo(OperationHandle, StorePath),
    /// Attempt substitution for one promoted store path.
    Substitute(OperationHandle, StorePath),
    /// Approve the exact private plan already held under a build operation.
    ApproveBuild(OperationHandle, BuildApprovalRequest),
    /// Verify one validated closed request.
    Verify(OperationHandle, VerifyRequest),
    /// Collect unreachable paths using the managed root set.
    Gc(OperationHandle),
    /// Fetch the sanitized preview of a broker-held private build plan.
    GetBuildPreview(OperationHandle),
    /// Prepare a private build plan from typed selector intent and broker authority.
    PrepareBuild(OperationHandle, Vec<PackageSelector>),
    /// Execute the exact approved private plan retained under one build handle.
    ExecuteBuild(OperationHandle, Digest),
    /// Publish a complete generation root intent after successful build execution.
    PublishBuildRoots(OperationHandle, RootSetIntent),
    /// Derive a fresh generation root set from an authenticated source generation.
    TransitionGenerationRoots(OperationHandle, RootSetTransitionIntent),
}

/// Closed responses on the broker-to-CLI channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliBrokerResponse {
    /// A fresh caller-bound operation was opened.
    Started(OperationHandle),
    /// Current sanitized operation state.
    Status(OperationStatus),
    /// Cancellation was accepted and admission released.
    Cancelled,
    /// Completion was accepted and admission released.
    Completed,
    /// Validated pinned managed-runtime version information.
    Version(VersionInfo),
    /// Validated derivation evaluation result.
    DerivationPlan(DerivationPlanReport),
    /// Validated path metadata result.
    PathInfo(PathInfoReport),
    /// Validated substitution result.
    Substitute(SubstituteReport),
    /// The exact private plan was durably approved for this operation.
    BuildApproved,
    /// Validated verification result.
    Verify(VerifyReport),
    /// Validated garbage-collection result.
    Gc(GcReport),
    /// Sanitized view of the broker-held private build plan.
    BuildPreview(BuildPreview),
    /// Sanitized view returned after authenticated preparation succeeds.
    BuildPrepared(BuildPreview),
    /// Validated report from broker-owned build execution.
    BuildExecuted(BuildReport),
    /// Stable redacted refusal from broker-owned build execution.
    BuildExecutionRefused(BuildExecutionErrorCode),
    /// Durable root publication completed for the authenticated generation.
    BuildRootsPublished(RootSetReport),
    /// Stable redacted refusal from protected root publication.
    BuildRootPublicationRefused(BuildRootPublicationErrorCode),
    /// Authenticated generation root transition completed.
    GenerationRootsTransitioned(RootSetTransitionReport),
    /// Stable redacted refusal from protected generation root transition.
    GenerationRootTransitionRefused(GenerationRootTransitionErrorCode),
    /// Redacted adapter failure for one exposed typed method.
    AdapterFailure(MethodKind, NixAdapterErrorCode),
}

/// Stable redacted build-execution refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildExecutionErrorCode {
    /// The exact private approval was absent, stale, consumed, or mismatched.
    ApprovalUnavailable,
    /// Admission-time replanning no longer matched the approved plan.
    ApprovalInvalidated,
    /// The broker-owned disk/load preflight refused execution.
    ResourcePreflightFailed,
    /// Managed build execution failed or returned inconsistent outputs.
    ExecutionFailed,
    /// Operation lifecycle cancellation stopped admission or execution.
    Cancelled,
    /// Other private authority state refused the request without more detail.
    AuthorityUnavailable,
}

/// Stable redacted generation-root transition refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRootTransitionErrorCode {
    /// The ownerless path-free transition intent was invalid for this operation.
    InvalidIntent,
    /// The fixed privileged helper failed to derive or publish the destination root set.
    TransitionFailed,
    /// Operation lifecycle cancellation stopped the transition.
    Cancelled,
    /// Other private authority state refused the request without more detail.
    AuthorityUnavailable,
}

impl GenerationRootTransitionErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIntent => "invalid_intent",
            Self::TransitionFailed => "transition_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

impl BuildExecutionErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalUnavailable => "approval_unavailable",
            Self::ApprovalInvalidated => "approval_invalidated",
            Self::ResourcePreflightFailed => "resource_preflight_failed",
            Self::ExecutionFailed => "execution_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

/// Closed protected-root-publication refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildRootPublicationErrorCode {
    /// The handle, generation, or root entries did not match retained authority.
    InvalidRootIntent,
    /// The privileged helper failed to publish the complete durable root set.
    PublicationFailed,
    /// Operation lifecycle cancellation stopped or superseded publication.
    Cancelled,
    /// The broker has no authenticated root-publishing authority available.
    AuthorityUnavailable,
}

impl BuildRootPublicationErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRootIntent => "invalid_root_intent",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

/// Closed user-approval pointer. It carries no receipt, target, derivation, or
/// runtime option; the broker resolves the digest only against its private plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildApprovalRequest {
    build_plan_digest: Digest,
    source: ApprovalSource,
}

impl BuildApprovalRequest {
    /// Constructs an approval pointer for one displayed private-plan digest.
    #[must_use]
    pub const fn new(build_plan_digest: Digest, source: ApprovalSource) -> Self {
        Self {
            build_plan_digest,
            source,
        }
    }

    /// Returns the exact displayed plan digest.
    #[must_use]
    pub const fn build_plan_digest(&self) -> Digest {
        self.build_plan_digest
    }

    /// Returns the explicit one-operation approval source.
    #[must_use]
    pub const fn source(&self) -> ApprovalSource {
        self.source
    }
}

/// Closed privileged requests on the broker-to-helper channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerHelperRequest {
    /// Atomically publish a complete root set.
    PublishRootSet(RootSet),
    /// Remove exactly one generation root set.
    RemoveRootSet(RemoveRootSetRequest),
    /// Ask the helper to issue a capability for a verified scope.
    IssueRepairCapability(VerifiedRepairScope),
    /// Redeem one opaque capability for fixed repair execution.
    RepairStorePaths(RepairStorePathsRequest),
    /// Derive a new root set only from names in a durable source generation.
    TransitionRootSet(RootSetTransitionRequest),
}

/// Closed privileged responses on the helper-to-broker channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerHelperResponse {
    /// A generation root set was durably published.
    RootSetPublished(RootSetReport),
    /// One generation root set was removed or already absent.
    RootSetRemoved,
    /// A fresh opaque repair capability was issued.
    RepairCapabilityIssued(MaintenanceCapability),
    /// Fixed repair execution completed with sanitized outcomes.
    RepairCompleted(RepairStorePathsReport),
    /// A path-free generation transition was durably published.
    RootSetTransitioned(RootSetTransitionReport),
}

/// Exact V1 product frame codec.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProductFrameCodec;

impl ProductFrameCodec {
    /// Encodes one closed CLI-to-broker lifecycle request.
    pub fn encode_cli_request(
        request_id: u64,
        request: &CliBrokerRequest,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match request {
            CliBrokerRequest::Begin(kind) => (
                1,
                encode_json(&BeginWire {
                    operation: operation_name(*kind),
                })?,
            ),
            CliBrokerRequest::Poll(handle) => (
                2,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Cancel(handle) => (
                3,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Complete(handle) => (
                4,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Version(handle) => (
                10,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::EvaluateDerivation(handle, request) => (
                11,
                encode_handle_body(handle, &request.encode().map_err(adapter_payload)?)?,
            ),
            CliBrokerRequest::PathInfo(handle, path) => (
                12,
                encode_json(&HandlePathWire {
                    handle: handle.as_str(),
                    path: path.as_str(),
                })?,
            ),
            CliBrokerRequest::Substitute(handle, path) => (
                13,
                encode_json(&HandlePathWire {
                    handle: handle.as_str(),
                    path: path.as_str(),
                })?,
            ),
            CliBrokerRequest::ApproveBuild(handle, approval) => (
                14,
                encode_json(&BuildApprovalWire {
                    handle: handle.as_str(),
                    build_plan_digest: approval.build_plan_digest.to_string(),
                    source: approval_source_name(approval.source),
                })?,
            ),
            CliBrokerRequest::Verify(handle, request) => (
                15,
                encode_handle_body(handle, &request.encode().map_err(adapter_payload)?)?,
            ),
            CliBrokerRequest::Gc(handle) => (
                16,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::GetBuildPreview(handle) => (
                17,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::PrepareBuild(handle, selectors) => {
                validate_prepare_selectors(selectors)?;
                (
                    18,
                    encode_json(&PrepareBuildWire {
                        handle: handle.as_str(),
                        selectors: selectors.iter().map(SelectorWire::from).collect(),
                    })?,
                )
            }
            CliBrokerRequest::ExecuteBuild(handle, digest) => (
                19,
                encode_json(&BuildExecutionWire {
                    handle: handle.as_str(),
                    build_plan_digest: digest.to_string(),
                })?,
            ),
            CliBrokerRequest::PublishBuildRoots(handle, intent) => (
                20,
                encode_json(&BuildRootIntentWire {
                    handle: handle.as_str(),
                    generation: intent.generation().as_str(),
                    entries: intent
                        .entries()
                        .iter()
                        .map(|entry| RootSetEntryWire {
                            name: entry.name().as_str(),
                            target: entry.target().as_str(),
                        })
                        .collect(),
                })?,
            ),
            CliBrokerRequest::TransitionGenerationRoots(handle, intent) => (
                21,
                encode_json(&GenerationRootTransitionWire {
                    handle: handle.as_str(),
                    source_generation: intent.source_generation().as_str(),
                    destination_generation: intent.destination_generation().as_str(),
                    retained_names: intent
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                })?,
            ),
        };
        encode_frame(CHANNEL_CLI_BROKER, method, request_id, &payload)
    }

    /// Decodes one exact CLI-to-broker lifecycle request.
    pub fn decode_cli_request(bytes: &[u8]) -> Result<(u64, CliBrokerRequest), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_CLI_BROKER)?;
        let request = match frame.method {
            1 => {
                let wire: BeginOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::Begin(parse_operation(&wire.operation)?)
            }
            2 | 3 | 4 | 10 | 16 | 17 => {
                let wire: HandleOwnedWire = decode_json(frame.payload)?;
                let handle = parse_handle(&wire.handle)?;
                match frame.method {
                    2 => CliBrokerRequest::Poll(handle),
                    3 => CliBrokerRequest::Cancel(handle),
                    4 => CliBrokerRequest::Complete(handle),
                    10 => CliBrokerRequest::Version(handle),
                    16 => CliBrokerRequest::Gc(handle),
                    17 => CliBrokerRequest::GetBuildPreview(handle),
                    _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
                }
            }
            11 | 15 => {
                let (handle, body) = decode_handle_body(frame.payload)?;
                let codec = JsonCodec::default();
                match frame.method {
                    11 => CliBrokerRequest::EvaluateDerivation(
                        handle,
                        EvaluateDerivationRequest::decode(&codec, body.get().as_bytes())
                            .map_err(adapter_payload)?,
                    ),
                    15 => CliBrokerRequest::Verify(
                        handle,
                        VerifyRequest::decode(&codec, body.get().as_bytes())
                            .map_err(adapter_payload)?,
                    ),
                    _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
                }
            }
            12 | 13 => {
                let wire: HandlePathOwnedWire = decode_json(frame.payload)?;
                let handle = parse_handle(&wire.handle)?;
                let path = StorePath::new(&wire.path).map_err(adapter_payload)?;
                if frame.method == 12 {
                    CliBrokerRequest::PathInfo(handle, path)
                } else {
                    CliBrokerRequest::Substitute(handle, path)
                }
            }
            14 => {
                let wire: BuildApprovalOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::ApproveBuild(
                    parse_handle(&wire.handle)?,
                    BuildApprovalRequest::new(
                        Digest::from_str(&wire.build_plan_digest)
                            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                        parse_approval_source(&wire.source)?,
                    ),
                )
            }
            18 => {
                let wire: PrepareBuildOwnedWire = decode_json(frame.payload)?;
                if wire.selectors.is_empty()
                    || wire.selectors.len() > MAX_BUILD_SELECTOR_WIRE_ENTRIES
                {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                CliBrokerRequest::PrepareBuild(
                    parse_handle(&wire.handle)?,
                    wire.selectors
                        .into_iter()
                        .map(SelectorOwnedWire::promote)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            19 => {
                let wire: BuildExecutionOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::ExecuteBuild(
                    parse_handle(&wire.handle)?,
                    Digest::from_str(&wire.build_plan_digest)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                )
            }
            20 => {
                let wire: BuildRootIntentOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::PublishBuildRoots(parse_handle(&wire.handle)?, wire.promote()?)
            }
            21 => {
                let wire: GenerationRootTransitionOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::TransitionGenerationRoots(
                    parse_handle(&wire.handle)?,
                    wire.promote()?,
                )
            }
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, request))
    }

    /// Encodes one closed broker-to-CLI lifecycle response.
    pub fn encode_cli_response(
        request_id: u64,
        response: &CliBrokerResponse,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match response {
            CliBrokerResponse::Started(handle) => (
                1,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerResponse::Status(status) => (
                2,
                encode_json(&StatusWire {
                    status: status_name(*status),
                })?,
            ),
            CliBrokerResponse::Cancelled => (3, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Completed => (4, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Version(report) => (
                10,
                report
                    .encode()
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            CliBrokerResponse::DerivationPlan(report) => {
                (11, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::PathInfo(report) => (12, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::Substitute(report) => {
                (13, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::BuildApproved => (14, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Verify(report) => (15, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::Gc(report) => (16, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::BuildPreview(preview) => (
                17,
                preview
                    .to_json_bytes()
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            CliBrokerResponse::BuildPrepared(preview) => (
                18,
                preview
                    .to_json_bytes()
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            CliBrokerResponse::BuildExecuted(report) => {
                (19, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::BuildExecutionRefused(code) => (
                19,
                encode_json(&BuildExecutionFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::BuildRootsPublished(report) => (
                20,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                })?,
            ),
            CliBrokerResponse::BuildRootPublicationRefused(code) => (
                20,
                encode_json(&BuildRootPublicationFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::GenerationRootsTransitioned(report) => (
                21,
                encode_json(&RootSetTransitionReportWire {
                    reference: report.root_set().reference().as_str(),
                    entry_count: report.root_set().entry_count(),
                    retained_names: report
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            CliBrokerResponse::GenerationRootTransitionRefused(code) => (
                21,
                encode_json(&GenerationRootTransitionFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::AdapterFailure(method, code) => (
                cli_adapter_method(*method)
                    .ok_or_else(|| FrameError::new(FrameErrorCode::UnsupportedMessage))?,
                encode_json(&AdapterFailureWire {
                    error: code.as_str(),
                })?,
            ),
        };
        encode_frame(CHANNEL_CLI_BROKER, method, request_id, &payload)
    }

    /// Decodes one exact broker-to-CLI lifecycle response.
    pub fn decode_cli_response(bytes: &[u8]) -> Result<(u64, CliBrokerResponse), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_CLI_BROKER)?;
        if let Some(method) = adapter_method(frame.method)
            && let Some(code) = decode_adapter_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::AdapterFailure(method, code),
            ));
        }
        if frame.method == 19
            && let Some(code) = decode_build_execution_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildExecutionRefused(code),
            ));
        }
        if frame.method == 20
            && let Some(code) = decode_build_root_publication_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildRootPublicationRefused(code),
            ));
        }
        if frame.method == 21
            && let Some(code) = decode_generation_root_transition_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::GenerationRootTransitionRefused(code),
            ));
        }
        let response = match frame.method {
            1 => {
                let wire: HandleOwnedWire = decode_json(frame.payload)?;
                CliBrokerResponse::Started(parse_handle(&wire.handle)?)
            }
            2 => {
                let wire: StatusOwnedWire = decode_json(frame.payload)?;
                CliBrokerResponse::Status(parse_status(&wire.status)?)
            }
            3 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::Cancelled
            }
            4 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::Completed
            }
            10 => CliBrokerResponse::Version(
                VersionInfo::decode(&JsonCodec::default(), frame.payload)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            11 => CliBrokerResponse::DerivationPlan(
                DerivationPlanReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            12 => CliBrokerResponse::PathInfo(
                PathInfoReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            13 => CliBrokerResponse::Substitute(
                SubstituteReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            14 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::BuildApproved
            }
            15 => CliBrokerResponse::Verify(
                VerifyReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            16 => CliBrokerResponse::Gc(
                GcReport::decode(&JsonCodec::default(), frame.payload).map_err(adapter_payload)?,
            ),
            17 => CliBrokerResponse::BuildPreview(
                BuildPreview::from_json_bytes(frame.payload)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            18 => CliBrokerResponse::BuildPrepared(
                BuildPreview::from_json_bytes(frame.payload)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            19 => CliBrokerResponse::BuildExecuted(
                BuildReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            20 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                CliBrokerResponse::BuildRootsPublished(RootSetReport::new(
                    reference,
                    wire.entry_count,
                ))
            }
            21 => CliBrokerResponse::GenerationRootsTransitioned(
                decode_json::<RootSetTransitionReportOwnedWire>(frame.payload)?.promote()?,
            ),
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, response))
    }

    /// Encodes one closed broker-to-helper request.
    pub fn encode_helper_request(
        request_id: u64,
        request: &BrokerHelperRequest,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match request {
            BrokerHelperRequest::PublishRootSet(root_set) => {
                (1, encode_json(&RootSetWire::from_root_set(root_set))?)
            }
            BrokerHelperRequest::RemoveRootSet(request) => (
                2,
                encode_json(&RemoveRootSetWire {
                    owner_uid: request.owner_uid(),
                    generation: request.generation().as_str(),
                })?,
            ),
            BrokerHelperRequest::IssueRepairCapability(scope) => {
                (3, encode_json(&RepairScopeWire::from_scope(scope))?)
            }
            BrokerHelperRequest::RepairStorePaths(request) => (
                4,
                encode_json(&CapabilityWire {
                    capability: request.capability().as_str(),
                })?,
            ),
            BrokerHelperRequest::TransitionRootSet(request) => (
                5,
                encode_json(&RootSetTransitionWire {
                    owner_uid: request.owner_uid(),
                    source_generation: request.source_generation().as_str(),
                    destination_generation: request.destination_generation().as_str(),
                    retained_names: request
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                })?,
            ),
        };
        encode_frame(CHANNEL_BROKER_HELPER, method, request_id, &payload)
    }

    /// Decodes one exact broker-to-helper request.
    pub fn decode_helper_request(bytes: &[u8]) -> Result<(u64, BrokerHelperRequest), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_BROKER_HELPER)?;
        let request = match frame.method {
            1 => BrokerHelperRequest::PublishRootSet(
                decode_json::<RootSetOwnedWire>(frame.payload)?.promote()?,
            ),
            2 => {
                let wire: RemoveRootSetOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::RemoveRootSet(RemoveRootSetRequest::new(
                    wire.owner_uid,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            }
            3 => BrokerHelperRequest::IssueRepairCapability(
                decode_json::<RepairScopeOwnedWire>(frame.payload)?.promote()?,
            ),
            4 => {
                let wire: CapabilityOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::RepairStorePaths(RepairStorePathsRequest::new(
                    parse_capability(&wire.capability)?,
                ))
            }
            5 => BrokerHelperRequest::TransitionRootSet(
                decode_json::<RootSetTransitionOwnedWire>(frame.payload)?.promote()?,
            ),
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, request))
    }

    /// Encodes one closed helper-to-broker response.
    pub fn encode_helper_response(
        request_id: u64,
        response: &BrokerHelperResponse,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match response {
            BrokerHelperResponse::RootSetPublished(report) => (
                1,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                })?,
            ),
            BrokerHelperResponse::RootSetRemoved => (2, encode_json(&EmptyWire {})?),
            BrokerHelperResponse::RepairCapabilityIssued(capability) => (
                3,
                encode_json(&CapabilityWire {
                    capability: capability.as_str(),
                })?,
            ),
            BrokerHelperResponse::RepairCompleted(report) => {
                (4, encode_json(&RepairReportWire::from_report(report))?)
            }
            BrokerHelperResponse::RootSetTransitioned(report) => (
                5,
                encode_json(&RootSetTransitionReportWire {
                    reference: report.root_set().reference().as_str(),
                    entry_count: report.root_set().entry_count(),
                    retained_names: report
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
        };
        encode_frame(CHANNEL_BROKER_HELPER, method, request_id, &payload)
    }

    /// Decodes one exact helper-to-broker response.
    pub fn decode_helper_response(bytes: &[u8]) -> Result<(u64, BrokerHelperResponse), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_BROKER_HELPER)?;
        let response = match frame.method {
            1 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                BrokerHelperResponse::RootSetPublished(RootSetReport::new(
                    reference,
                    wire.entry_count,
                ))
            }
            2 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                BrokerHelperResponse::RootSetRemoved
            }
            3 => {
                let wire: CapabilityOwnedWire = decode_json(frame.payload)?;
                BrokerHelperResponse::RepairCapabilityIssued(parse_capability(&wire.capability)?)
            }
            4 => BrokerHelperResponse::RepairCompleted(
                decode_json::<RepairReportOwnedWire>(frame.payload)?.promote()?,
            ),
            5 => BrokerHelperResponse::RootSetTransitioned(
                decode_json::<RootSetTransitionReportOwnedWire>(frame.payload)?.promote()?,
            ),
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, response))
    }
}

struct DecodedFrame<'a> {
    method: u8,
    request_id: u64,
    payload: &'a [u8],
}

fn encode_frame(
    channel: u8,
    method: u8,
    request_id: u64,
    payload: &[u8],
) -> Result<Vec<u8>, FrameError> {
    if request_id == 0 || payload.len() > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::new(
            if payload.len() > MAX_FRAME_PAYLOAD_BYTES {
                FrameErrorCode::FrameTooLarge
            } else {
                FrameErrorCode::UnsupportedMessage
            },
        ));
    }
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| FrameError::new(FrameErrorCode::FrameTooLarge))?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    frame.push(channel);
    frame.push(method);
    frame.extend_from_slice(&request_id.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_frame(bytes: &[u8], expected_channel: u8) -> Result<DecodedFrame<'_>, FrameError> {
    if bytes.len() < HEADER_BYTES {
        return Err(FrameError::new(FrameErrorCode::MalformedHeader));
    }
    if bytes.len() > HEADER_BYTES + MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::new(FrameErrorCode::FrameTooLarge));
    }
    if bytes[..4] != MAGIC {
        return Err(FrameError::new(FrameErrorCode::MalformedHeader));
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(FrameError::new(FrameErrorCode::UnsupportedVersion));
    }
    if bytes[6] != expected_channel || bytes[7] == 0 {
        return Err(FrameError::new(FrameErrorCode::UnsupportedMessage));
    }
    let request_id = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| FrameError::new(FrameErrorCode::MalformedHeader))?,
    );
    if request_id == 0 {
        return Err(FrameError::new(FrameErrorCode::UnsupportedMessage));
    }
    let payload_len = u32::from_be_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| FrameError::new(FrameErrorCode::MalformedHeader))?,
    ) as usize;
    if payload_len > MAX_FRAME_PAYLOAD_BYTES {
        return Err(FrameError::new(FrameErrorCode::FrameTooLarge));
    }
    if bytes.len() != HEADER_BYTES + payload_len {
        return Err(FrameError::new(FrameErrorCode::LengthMismatch));
    }
    Ok(DecodedFrame {
        method: bytes[7],
        request_id,
        payload: &bytes[HEADER_BYTES..],
    })
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, FrameError> {
    serde_json::to_vec(value).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn decode_json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, FrameError> {
    serde_json::from_slice(bytes).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn adapter_payload<T>(_: T) -> FrameError {
    FrameError::new(FrameErrorCode::InvalidPayload)
}

fn validate_prepare_selectors(selectors: &[PackageSelector]) -> Result<(), FrameError> {
    if selectors.is_empty()
        || selectors.len() > MAX_BUILD_SELECTOR_WIRE_ENTRIES
        || selectors.iter().any(|selector| {
            selector.attribute().is_some()
                || selector.pin_state().is_pinned()
                || !matches!(selector.source_revision(), SourceRevision::CurrentChannel)
        })
    {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    } else {
        Ok(())
    }
}

const fn cli_adapter_method(method: MethodKind) -> Option<u8> {
    match method {
        MethodKind::Version => Some(10),
        MethodKind::EvaluateDerivation => Some(11),
        MethodKind::PathInfo => Some(12),
        MethodKind::Substitute => Some(13),
        MethodKind::Build => None,
        MethodKind::Verify => Some(15),
        MethodKind::Gc => Some(16),
    }
}

const fn adapter_method(method: u8) -> Option<MethodKind> {
    match method {
        10 => Some(MethodKind::Version),
        11 => Some(MethodKind::EvaluateDerivation),
        12 => Some(MethodKind::PathInfo),
        13 => Some(MethodKind::Substitute),
        15 => Some(MethodKind::Verify),
        16 => Some(MethodKind::Gc),
        _ => None,
    }
}

fn decode_adapter_failure(bytes: &[u8]) -> Result<Option<NixAdapterErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<AdapterFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_adapter_error_code(&wire.error).map(Some)
}

fn decode_build_execution_failure(
    bytes: &[u8],
) -> Result<Option<BuildExecutionErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildExecutionFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_build_execution_error_code(&wire.error).map(Some)
}

fn decode_build_root_publication_failure(
    bytes: &[u8],
) -> Result<Option<BuildRootPublicationErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildRootPublicationFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_build_root_publication_error_code(&wire.error).map(Some)
}

fn decode_generation_root_transition_failure(
    bytes: &[u8],
) -> Result<Option<GenerationRootTransitionErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<GenerationRootTransitionFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_generation_root_transition_error_code(&wire.error).map(Some)
}

fn parse_build_execution_error_code(value: &str) -> Result<BuildExecutionErrorCode, FrameError> {
    match value {
        "approval_unavailable" => Ok(BuildExecutionErrorCode::ApprovalUnavailable),
        "approval_invalidated" => Ok(BuildExecutionErrorCode::ApprovalInvalidated),
        "resource_preflight_failed" => Ok(BuildExecutionErrorCode::ResourcePreflightFailed),
        "execution_failed" => Ok(BuildExecutionErrorCode::ExecutionFailed),
        "cancelled" => Ok(BuildExecutionErrorCode::Cancelled),
        "authority_unavailable" => Ok(BuildExecutionErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_build_root_publication_error_code(
    value: &str,
) -> Result<BuildRootPublicationErrorCode, FrameError> {
    match value {
        "invalid_root_intent" => Ok(BuildRootPublicationErrorCode::InvalidRootIntent),
        "publication_failed" => Ok(BuildRootPublicationErrorCode::PublicationFailed),
        "cancelled" => Ok(BuildRootPublicationErrorCode::Cancelled),
        "authority_unavailable" => Ok(BuildRootPublicationErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_generation_root_transition_error_code(
    value: &str,
) -> Result<GenerationRootTransitionErrorCode, FrameError> {
    match value {
        "invalid_intent" => Ok(GenerationRootTransitionErrorCode::InvalidIntent),
        "transition_failed" => Ok(GenerationRootTransitionErrorCode::TransitionFailed),
        "cancelled" => Ok(GenerationRootTransitionErrorCode::Cancelled),
        "authority_unavailable" => Ok(GenerationRootTransitionErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_adapter_error_code(value: &str) -> Result<NixAdapterErrorCode, FrameError> {
    match value {
        "unexpected_call" => Ok(NixAdapterErrorCode::UnexpectedCall),
        "oversized_input" => Ok(NixAdapterErrorCode::OversizedInput),
        "malformed_payload" => Ok(NixAdapterErrorCode::MalformedPayload),
        "unsupported_schema_version" => Ok(NixAdapterErrorCode::UnsupportedSchemaVersion),
        "unsupported_upstream_format" => Ok(NixAdapterErrorCode::UnsupportedUpstreamFormat),
        "validation_failure" => Ok(NixAdapterErrorCode::ValidationFailure),
        "timeout" => Ok(NixAdapterErrorCode::Timeout),
        "unavailable" => Ok(NixAdapterErrorCode::Unavailable),
        "trust_failure" => Ok(NixAdapterErrorCode::TrustFailure),
        "integrity_failure" => Ok(NixAdapterErrorCode::IntegrityFailure),
        "permission_denied" => Ok(NixAdapterErrorCode::PermissionDenied),
        "operation_failed" => Ok(NixAdapterErrorCode::OperationFailed),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn encode_handle_body(handle: &OperationHandle, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    let body: Box<RawValue> = decode_json(body)?;
    encode_json(&HandleBodyWire {
        handle: handle.as_str(),
        request: &body,
    })
}

fn decode_handle_body(bytes: &[u8]) -> Result<(OperationHandle, Box<RawValue>), FrameError> {
    let wire: HandleBodyOwnedWire = decode_json(bytes)?;
    Ok((parse_handle(&wire.handle)?, wire.request))
}

fn operation_name(kind: BrokerOperationKind) -> &'static str {
    match kind {
        BrokerOperationKind::Doctor => "doctor",
        BrokerOperationKind::Refresh => "refresh",
        BrokerOperationKind::Resolve => "resolve",
        BrokerOperationKind::Acquire => "acquire",
        BrokerOperationKind::Build => "build",
        BrokerOperationKind::Activate => "activate",
        BrokerOperationKind::Gc => "gc",
        BrokerOperationKind::Repair => "repair",
    }
}

fn parse_operation(value: &str) -> Result<BrokerOperationKind, FrameError> {
    match value {
        "doctor" => Ok(BrokerOperationKind::Doctor),
        "refresh" => Ok(BrokerOperationKind::Refresh),
        "resolve" => Ok(BrokerOperationKind::Resolve),
        "acquire" => Ok(BrokerOperationKind::Acquire),
        "build" => Ok(BrokerOperationKind::Build),
        "activate" => Ok(BrokerOperationKind::Activate),
        "gc" => Ok(BrokerOperationKind::Gc),
        "repair" => Ok(BrokerOperationKind::Repair),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn status_name(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Completed => "completed",
        OperationStatus::Cancelled => "cancelled",
    }
}

fn parse_status(value: &str) -> Result<OperationStatus, FrameError> {
    match value {
        "running" => Ok(OperationStatus::Running),
        "completed" => Ok(OperationStatus::Completed),
        "cancelled" => Ok(OperationStatus::Cancelled),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn approval_source_name(source: ApprovalSource) -> &'static str {
    source.as_str()
}

fn parse_approval_source(value: &str) -> Result<ApprovalSource, FrameError> {
    match value {
        "interactive" => Ok(ApprovalSource::Interactive),
        "yes" => Ok(ApprovalSource::AssumeYes),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_handle(value: &str) -> Result<OperationHandle, FrameError> {
    let tail = value.strip_prefix("op_").unwrap_or_default();
    if tail.len() == 64
        && tail
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(OperationHandle(value.to_owned()))
    } else {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

fn parse_capability(value: &str) -> Result<MaintenanceCapability, FrameError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(MaintenanceCapability(value.to_owned()))
    } else {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BeginWire<'a> {
    operation: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginOwnedWire {
    operation: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandleWire<'a> {
    handle: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleOwnedWire {
    handle: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AdapterFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutionFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutionFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BuildRootPublicationFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRootPublicationFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootTransitionFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootTransitionFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandleBodyWire<'a> {
    handle: &'a str,
    request: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleBodyOwnedWire {
    handle: String,
    request: Box<RawValue>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandlePathWire<'a> {
    handle: &'a str,
    path: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandlePathOwnedWire {
    handle: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildApprovalWire<'a> {
    handle: &'a str,
    build_plan_digest: String,
    source: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildApprovalOwnedWire {
    handle: String,
    build_plan_digest: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareBuildWire<'a> {
    handle: &'a str,
    selectors: Vec<SelectorWire<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareBuildOwnedWire {
    handle: String,
    selectors: Vec<SelectorOwnedWire>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionWire<'a> {
    handle: &'a str,
    build_plan_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionOwnedWire {
    handle: String,
    build_plan_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildRootIntentWire<'a> {
    handle: &'a str,
    generation: &'a str,
    entries: Vec<RootSetEntryWire<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildRootIntentOwnedWire {
    handle: String,
    generation: String,
    entries: Vec<RootSetEntryOwnedWire>,
}

impl BuildRootIntentOwnedWire {
    fn promote(self) -> Result<RootSetIntent, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let entries = self
            .entries
            .into_iter()
            .map(|entry| {
                Ok(RootSetEntry::new(
                    RootName::new(&entry.name)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                    StorePath::new(&entry.target)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        RootSetIntent::new(generation, entries)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootTransitionWire<'a> {
    handle: &'a str,
    source_generation: &'a str,
    destination_generation: &'a str,
    retained_names: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootTransitionOwnedWire {
    handle: String,
    source_generation: String,
    destination_generation: String,
    retained_names: Vec<String>,
}

impl GenerationRootTransitionOwnedWire {
    fn promote(self) -> Result<RootSetTransitionIntent, FrameError> {
        let source_generation = GenerationId::new(&self.source_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let destination_generation = GenerationId::new(&self.destination_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let retained_names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RootSetTransitionIntent::new(source_generation, destination_generation, retained_names)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectorWire<'a> {
    id: &'a str,
    selector: &'a str,
    version_preference: VersionPreferenceWire<'a>,
    outputs: Option<Vec<&'a str>>,
}

impl<'a> From<&'a PackageSelector> for SelectorWire<'a> {
    fn from(selector: &'a PackageSelector) -> Self {
        Self {
            id: selector.id().as_str(),
            selector: selector.selector().as_str(),
            version_preference: VersionPreferenceWire::from(selector.version_preference()),
            outputs: selector
                .outputs()
                .explicit_outputs()
                .map(|outputs| outputs.iter().map(OutputName::as_str).collect()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectorOwnedWire {
    id: String,
    selector: String,
    version_preference: VersionPreferenceOwnedWire,
    outputs: Option<Vec<String>>,
}

impl SelectorOwnedWire {
    fn promote(self) -> Result<PackageSelector, FrameError> {
        let outputs = match self.outputs {
            None => OutputSelection::default_selection(),
            Some(outputs) => OutputSelection::explicit(
                outputs
                    .into_iter()
                    .map(|output| OutputName::new(&output).map_err(adapter_payload))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .map_err(adapter_payload)?,
        };
        Ok(PackageSelector::new(
            SelectorId::new(&self.id).map_err(adapter_payload)?,
            SelectorInput::new(&self.selector).map_err(adapter_payload)?,
            self.version_preference.promote()?,
            outputs,
            SourceRevision::CurrentChannel,
        ))
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum VersionPreferenceWire<'a> {
    Any,
    Exact {
        version: &'a str,
    },
    Min {
        version: &'a str,
    },
    Range {
        lower: Option<VersionBoundWire<'a>>,
        upper: Option<VersionBoundWire<'a>>,
    },
}

impl<'a> From<&'a VersionPreference> for VersionPreferenceWire<'a> {
    fn from(preference: &'a VersionPreference) -> Self {
        match preference {
            VersionPreference::Any => Self::Any,
            VersionPreference::Exact(version) => Self::Exact {
                version: version.as_str(),
            },
            VersionPreference::Minimum(version) => Self::Min {
                version: version.as_str(),
            },
            VersionPreference::Range(range) => Self::Range {
                lower: range.lower().map(VersionBoundWire::from),
                upper: range.upper().map(VersionBoundWire::from),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum VersionPreferenceOwnedWire {
    Any,
    Exact {
        version: String,
    },
    Min {
        version: String,
    },
    Range {
        lower: Option<VersionBoundOwnedWire>,
        upper: Option<VersionBoundOwnedWire>,
    },
}

impl VersionPreferenceOwnedWire {
    fn promote(self) -> Result<VersionPreference, FrameError> {
        Ok(match self {
            Self::Any => VersionPreference::Any,
            Self::Exact { version } => VersionPreference::Exact(PackageVersion::new(version)),
            Self::Min { version } => VersionPreference::Minimum(PackageVersion::new(version)),
            Self::Range { lower, upper } => VersionPreference::Range(
                VersionRange::new(
                    lower.map(VersionBoundOwnedWire::promote).transpose()?,
                    upper.map(VersionBoundOwnedWire::promote).transpose()?,
                )
                .map_err(adapter_payload)?,
            ),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBoundWire<'a> {
    version: &'a str,
    inclusive: bool,
}

impl<'a> From<&'a VersionBound> for VersionBoundWire<'a> {
    fn from(bound: &'a VersionBound) -> Self {
        Self {
            version: bound.version().as_str(),
            inclusive: bound.is_inclusive(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBoundOwnedWire {
    version: String,
    inclusive: bool,
}

impl VersionBoundOwnedWire {
    fn promote(self) -> Result<VersionBound, FrameError> {
        let version = PackageVersion::new(self.version);
        Ok(if self.inclusive {
            VersionBound::inclusive(version)
        } else {
            VersionBound::exclusive(version)
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyWire {}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StatusWire<'a> {
    status: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusOwnedWire {
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetWire<'a> {
    owner_uid: u32,
    generation: &'a str,
    entries: Vec<RootSetEntryWire<'a>>,
}

impl<'a> RootSetWire<'a> {
    fn from_root_set(root_set: &'a RootSet) -> Self {
        Self {
            owner_uid: root_set.owner_uid(),
            generation: root_set.generation().as_str(),
            entries: root_set
                .entries()
                .iter()
                .map(|entry| RootSetEntryWire {
                    name: entry.name().as_str(),
                    target: entry.target().as_str(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RootSetEntryWire<'a> {
    name: &'a str,
    target: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetOwnedWire {
    owner_uid: u32,
    generation: String,
    entries: Vec<RootSetEntryOwnedWire>,
}

impl RootSetOwnedWire {
    fn promote(self) -> Result<RootSet, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let entries = self
            .entries
            .into_iter()
            .map(|entry| {
                Ok(RootSetEntry::new(
                    RootName::new(&entry.name)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                    StorePath::new(&entry.target)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        RootSet::new(self.owner_uid, generation, entries)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootSetEntryOwnedWire {
    name: String,
    target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveRootSetWire<'a> {
    owner_uid: u32,
    generation: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveRootSetOwnedWire {
    owner_uid: u32,
    generation: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionWire<'a> {
    owner_uid: u32,
    source_generation: &'a str,
    destination_generation: &'a str,
    retained_names: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionOwnedWire {
    owner_uid: u32,
    source_generation: String,
    destination_generation: String,
    retained_names: Vec<String>,
}

impl RootSetTransitionOwnedWire {
    fn promote(self) -> Result<RootSetTransitionRequest, FrameError> {
        let source_generation = GenerationId::new(&self.source_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let destination_generation = GenerationId::new(&self.destination_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let retained_names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RootSetTransitionRequest::new(
            self.owner_uid,
            source_generation,
            destination_generation,
            retained_names,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairScopeWire<'a> {
    owner_uid: u32,
    generation: &'a str,
    paths: Vec<&'a str>,
    build_plan_digest: Option<String>,
    policy_version: u64,
    mode: &'a str,
}

impl<'a> RepairScopeWire<'a> {
    fn from_scope(scope: &'a VerifiedRepairScope) -> Self {
        Self {
            owner_uid: scope.owner_uid(),
            generation: scope.generation().as_str(),
            paths: scope.paths().iter().map(StorePath::as_str).collect(),
            build_plan_digest: scope.build_plan_digest().map(|digest| digest.to_string()),
            policy_version: scope.policy_version().get().get(),
            mode: match scope.mode() {
                RepairMode::CacheOnly => "cacheOnly",
                RepairMode::Build => "build",
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairScopeOwnedWire {
    owner_uid: u32,
    generation: String,
    paths: Vec<String>,
    build_plan_digest: Option<String>,
    policy_version: u64,
    mode: String,
}

impl RepairScopeOwnedWire {
    fn promote(self) -> Result<VerifiedRepairScope, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let paths = self
            .paths
            .into_iter()
            .map(|path| {
                StorePath::new(&path).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let digest = self
            .build_plan_digest
            .map(|digest| {
                Digest::from_str(&digest)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .transpose()?;
        let policy_version = PolicyVersion::from_u64(self.policy_version)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let mode = match self.mode.as_str() {
            "cacheOnly" => RepairMode::CacheOnly,
            "build" => RepairMode::Build,
            _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
        };
        VerifiedRepairScope::new(
            self.owner_uid,
            generation,
            paths,
            digest,
            policy_version,
            mode,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityWire<'a> {
    capability: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityOwnedWire {
    capability: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetReportWire<'a> {
    reference: &'a str,
    entry_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetReportOwnedWire {
    reference: String,
    entry_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionReportWire<'a> {
    reference: &'a str,
    entry_count: usize,
    retained_names: Vec<&'a str>,
    mapping_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionReportOwnedWire {
    reference: String,
    entry_count: usize,
    retained_names: Vec<String>,
    mapping_digest: String,
}

impl RootSetTransitionReportOwnedWire {
    fn promote(self) -> Result<RootSetTransitionReport, FrameError> {
        if self.entry_count == 0 || self.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let reference = RootRef::new(&self.reference)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mapping_digest = Digest::from_str(&self.mapping_digest)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        RootSetTransitionReport::new(
            RootSetReport::new(reference, self.entry_count),
            names,
            mapping_digest,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairReportWire<'a> {
    mode: &'a str,
    outcomes: Vec<RepairOutcomeWire<'a>>,
}

impl<'a> RepairReportWire<'a> {
    fn from_report(report: &'a RepairStorePathsReport) -> Self {
        Self {
            mode: match report.mode() {
                RepairMode::CacheOnly => "cacheOnly",
                RepairMode::Build => "build",
            },
            outcomes: report
                .outcomes()
                .iter()
                .map(|outcome| RepairOutcomeWire {
                    path: outcome.path().as_str(),
                    kind: match outcome.kind() {
                        RepairOutcomeKind::Restored => "restored",
                        RepairOutcomeKind::Unchanged => "unchanged",
                        RepairOutcomeKind::CacheMiss => "cacheMiss",
                    },
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RepairOutcomeWire<'a> {
    path: &'a str,
    kind: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairReportOwnedWire {
    mode: String,
    outcomes: Vec<RepairOutcomeOwnedWire>,
}

impl RepairReportOwnedWire {
    fn promote(self) -> Result<RepairStorePathsReport, FrameError> {
        if self.outcomes.is_empty() || self.outcomes.len() > MAX_ROOT_SET_WIRE_ENTRIES {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let mode = match self.mode.as_str() {
            "cacheOnly" => RepairMode::CacheOnly,
            "build" => RepairMode::Build,
            _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
        };
        let mut outcomes = self
            .outcomes
            .into_iter()
            .map(|outcome| {
                let path = StorePath::new(&outcome.path)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                let kind = match outcome.kind.as_str() {
                    "restored" => RepairOutcomeKind::Restored,
                    "unchanged" => RepairOutcomeKind::Unchanged,
                    "cacheMiss" => RepairOutcomeKind::CacheMiss,
                    _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
                };
                Ok(RepairPathOutcome::new(path, kind))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        outcomes.sort_by(|left, right| left.path().as_str().cmp(right.path().as_str()));
        if outcomes
            .windows(2)
            .any(|pair| pair[0].path() == pair[1].path())
        {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        Ok(RepairStorePathsReport::new(mode, outcomes))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairOutcomeOwnedWire {
    path: String,
    kind: String,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use pkg_core::state::body_digest;

    use super::*;
    use crate::{
        AcceptedFormats, BuildOutput, BuildOutputProvenance, BuildStatus, FormatVersion, NixVersion,
    };

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn path(name: &str) -> StorePath {
        StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
    }

    fn root_set() -> RootSet {
        RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("hello-1.0"),
            )],
        )
        .unwrap()
    }

    #[test]
    fn both_channels_round_trip_with_exact_headers() {
        let cli = CliBrokerRequest::Begin(BrokerOperationKind::Repair);
        let encoded = ProductFrameCodec::encode_cli_request(7, &cli).unwrap();
        assert_eq!(&encoded[..4], b"PKG1");
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((7, cli))
        );

        let handle = OperationHandle(format!("op_{}", "a".repeat(64)));
        let approval = CliBrokerRequest::ApproveBuild(
            handle,
            BuildApprovalRequest::new(Digest::from_bytes([0x42; 32]), ApprovalSource::Interactive),
        );
        let encoded = ProductFrameCodec::encode_cli_request(8, &approval).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("buildPlanDigest"));
        for forbidden in ["receipt", "derivation", "target", "substituter", "maxJobs"] {
            assert!(!wire.contains(forbidden), "approval exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((8, approval))
        );

        let encoded =
            ProductFrameCodec::encode_cli_response(8, &CliBrokerResponse::BuildApproved).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((8, CliBrokerResponse::BuildApproved))
        );
        let handle = OperationHandle(format!("op_{}", "b".repeat(64)));
        let selector = PackageSelector::new(
            SelectorId::new("sel_ripgrep").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Minimum(PackageVersion::new("14.0")),
            OutputSelection::explicit(vec![OutputName::new("out").unwrap()]).unwrap(),
            SourceRevision::CurrentChannel,
        );
        let prepare = CliBrokerRequest::PrepareBuild(handle, vec![selector]);
        let encoded = ProductFrameCodec::encode_cli_request(10, &prepare).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["source", "revision", "attribute", "pinned", "nixpkgs"] {
            assert!(!wire.contains(forbidden), "prepare exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((10, prepare))
        );

        let resolved = PackageSelector::new(
            SelectorId::new("sel_resolved").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        )
        .with_attribute(pkg_core::AttributePath::new("ripgrep").unwrap())
        .unwrap();
        assert_eq!(
            ProductFrameCodec::encode_cli_request(
                10,
                &CliBrokerRequest::PrepareBuild(
                    OperationHandle(format!("op_{}", "b".repeat(64))),
                    vec![resolved],
                ),
            ),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let empty = encode_frame(
            CHANNEL_CLI_BROKER,
            18,
            10,
            format!(r#"{{"handle":"op_{}","selectors":[]}}"#, "b".repeat(64)).as_bytes(),
        )
        .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&empty),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let execute = CliBrokerRequest::ExecuteBuild(
            OperationHandle(format!("op_{}", "c".repeat(64))),
            Digest::from_bytes([0x24; 32]),
        );
        let encoded = ProductFrameCodec::encode_cli_request(11, &execute).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("buildPlanDigest"));
        for forbidden in ["estimate", "resource", "receipt", "target", "derivation"] {
            assert!(!wire.contains(forbidden), "execute exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((11, execute))
        );
        for code in [
            BuildExecutionErrorCode::ApprovalUnavailable,
            BuildExecutionErrorCode::ApprovalInvalidated,
            BuildExecutionErrorCode::ResourcePreflightFailed,
            BuildExecutionErrorCode::ExecutionFailed,
            BuildExecutionErrorCode::Cancelled,
            BuildExecutionErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::BuildExecutionRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(11, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((11, response))
            );
        }
        let report = BuildReport::new(
            BuildStatus::Built,
            vec![BuildOutput::new(
                path("built-1.0"),
                BuildOutputProvenance::LocalBuild,
            )],
        )
        .unwrap();
        let response = CliBrokerResponse::BuildExecuted(report);
        let encoded = ProductFrameCodec::encode_cli_response(11, &response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((11, response))
        );

        let root_intent = RootSetIntent::new(
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("built-1.0"),
            )],
        )
        .unwrap();
        let publish = CliBrokerRequest::PublishBuildRoots(
            OperationHandle(format!("op_{}", "d".repeat(64))),
            root_intent,
        );
        let encoded = ProductFrameCodec::encode_cli_request(12, &publish).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("generation"));
        assert!(wire.contains("entries"));
        assert!(!wire.contains("ownerUid"));
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((12, publish))
        );

        let published = CliBrokerResponse::BuildRootsPublished(RootSetReport::new(
            RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
            1,
        ));
        let encoded = ProductFrameCodec::encode_cli_response(12, &published).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((12, published))
        );
        for code in [
            BuildRootPublicationErrorCode::InvalidRootIntent,
            BuildRootPublicationErrorCode::PublicationFailed,
            BuildRootPublicationErrorCode::Cancelled,
            BuildRootPublicationErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::BuildRootPublicationRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(12, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((12, response))
            );
        }
        let extended = encode_frame(
            CHANNEL_CLI_BROKER,
            20,
            12,
            br#"{"error":"future_extension"}"#,
        )
        .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&extended),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let transition = CliBrokerRequest::TransitionGenerationRoots(
            OperationHandle(format!("op_{}", "e".repeat(64))),
            RootSetTransitionIntent::new(
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                vec![RootName::new("hello-out").unwrap()],
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_request(13, &transition).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("sourceGeneration"));
        assert!(wire.contains("destinationGeneration"));
        assert!(wire.contains("retainedNames"));
        for forbidden in ["ownerUid", "/nix/store", "target"] {
            assert!(!wire.contains(forbidden), "transition exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((13, transition))
        );
        let transitioned = CliBrokerResponse::GenerationRootsTransitioned(
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                ),
                vec![RootName::new("hello-out").unwrap()],
                Digest::from_bytes([0x51; 32]),
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_response(13, &transitioned).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((13, transitioned))
        );
        for code in [
            GenerationRootTransitionErrorCode::InvalidIntent,
            GenerationRootTransitionErrorCode::TransitionFailed,
            GenerationRootTransitionErrorCode::Cancelled,
            GenerationRootTransitionErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::GenerationRootTransitionRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(13, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((13, response))
            );
        }
        let complete =
            CliBrokerRequest::Complete(OperationHandle(format!("op_{}", "f".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_request(14, &complete).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((14, complete))
        );
        let encoded =
            ProductFrameCodec::encode_cli_response(14, &CliBrokerResponse::Completed).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((14, CliBrokerResponse::Completed))
        );

        let helper = BrokerHelperRequest::PublishRootSet(root_set());
        let encoded = ProductFrameCodec::encode_helper_request(9, &helper).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((9, helper))
        );

        let transition = BrokerHelperRequest::TransitionRootSet(
            RootSetTransitionRequest::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                vec![RootName::new("hello-out").unwrap()],
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_helper_request(10, &transition).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("sourceGeneration"));
        assert!(wire.contains("destinationGeneration"));
        assert!(wire.contains("retainedNames"));
        assert!(!wire.contains("/nix/store"));
        assert!(!wire.contains("target"));
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((10, transition))
        );

        let transitioned = BrokerHelperResponse::RootSetTransitioned(
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                ),
                vec![RootName::new("hello-out").unwrap()],
                Digest::from_bytes([0x52; 32]),
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_helper_response(10, &transitioned).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((10, transitioned))
        );

        let started = CliBrokerResponse::Started(OperationHandle(format!("op_{}", "0".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_response(11, &started).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((11, started))
        );

        let handle = OperationHandle(format!("op_{}", "2".repeat(64)));
        let version_request = CliBrokerRequest::Version(handle);
        let encoded = ProductFrameCodec::encode_cli_request(13, &version_request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((13, version_request))
        );

        let version_response = CliBrokerResponse::Version(VersionInfo::new(
            NixVersion::new("2.34.8").unwrap(),
            AcceptedFormats::new(FormatVersion::new(1).unwrap()),
        ));
        let encoded = ProductFrameCodec::encode_cli_response(14, &version_response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((14, version_response))
        );

        let adapter_failure = CliBrokerResponse::AdapterFailure(
            MethodKind::Substitute,
            NixAdapterErrorCode::TrustFailure,
        );
        let encoded = ProductFrameCodec::encode_cli_response(15, &adapter_failure).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((15, adapter_failure))
        );

        let issued =
            BrokerHelperResponse::RepairCapabilityIssued(MaintenanceCapability("1".repeat(64)));
        let encoded = ProductFrameCodec::encode_helper_response(12, &issued).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((12, issued))
        );
    }

    #[test]
    fn typed_adapter_envelopes_preserve_nested_strictness_and_promote_paths() {
        let handle = format!("op_{}", "3".repeat(64));
        let duplicate_nested =
            format!(r#"{{"handle":"{handle}","request":{{"schemaVersion":1,"schemaVersion":1}}}}"#);
        let encoded =
            encode_frame(CHANNEL_CLI_BROKER, 11, 91, duplicate_nested.as_bytes()).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let arbitrary_path = format!(r#"{{"handle":"{handle}","path":"/tmp/not-store"}}"#);
        let encoded = encode_frame(CHANNEL_CLI_BROKER, 12, 92, arbitrary_path.as_bytes()).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );
    }

    #[test]
    fn repair_scope_round_trip_preserves_every_binding() {
        let scope = VerifiedRepairScope::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            [path("hello-1.0"), path("glibc-2.39")],
            Some(body_digest(b"plan")),
            PolicyVersion::new(NonZeroU64::new(3).unwrap()),
            RepairMode::Build,
        )
        .unwrap();
        let request = BrokerHelperRequest::IssueRepairCapability(scope);
        let encoded = ProductFrameCodec::encode_helper_request(1, &request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((1, request))
        );
    }

    #[test]
    fn framing_rejects_wrong_channel_version_length_and_extension() {
        let request = CliBrokerRequest::Begin(BrokerOperationKind::Doctor);
        let encoded = ProductFrameCodec::encode_cli_request(1, &request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded)
                .unwrap_err()
                .code(),
            FrameErrorCode::UnsupportedMessage
        );
        let mut bad_version = encoded.clone();
        bad_version[5] = 2;
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&bad_version)
                .unwrap_err()
                .code(),
            FrameErrorCode::UnsupportedVersion
        );
        let mut bad_length = encoded.clone();
        bad_length.push(b' ');
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&bad_length)
                .unwrap_err()
                .code(),
            FrameErrorCode::LengthMismatch
        );
        let payload = br#"{"operation":"doctor","argv":[]}"#;
        let extended = encode_frame(CHANNEL_CLI_BROKER, 1, 1, payload).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&extended)
                .unwrap_err()
                .code(),
            FrameErrorCode::InvalidPayload
        );
    }

    #[test]
    fn helper_grammar_rejects_raw_paths_options_and_forged_capabilities() {
        for payload in [
            br#"{"capability":"/nix/store/raw"}"#.as_slice(),
            br#"{"capability":"--substituters"}"#.as_slice(),
            br#"{"capability":"github:NixOS/nixpkgs"}"#.as_slice(),
            br#"{"capability":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#
                .as_slice(),
        ] {
            let frame = encode_frame(CHANNEL_BROKER_HELPER, 4, 1, payload).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_helper_request(&frame)
                    .unwrap_err()
                    .code(),
                FrameErrorCode::InvalidPayload
            );
        }
    }

    #[test]
    fn arbitrary_short_frames_never_panic_or_decode_as_valid() {
        for length in 0..HEADER_BYTES {
            for fill in [0_u8, 0x41, 0xff] {
                let bytes = vec![fill; length];
                assert!(ProductFrameCodec::decode_cli_request(&bytes).is_err());
                assert!(ProductFrameCodec::decode_helper_request(&bytes).is_err());
                assert!(ProductFrameCodec::decode_cli_response(&bytes).is_err());
                assert!(ProductFrameCodec::decode_helper_response(&bytes).is_err());
            }
        }
    }
}
