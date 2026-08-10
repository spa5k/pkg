//! Exact product framing for the CLI↔broker and broker↔helper channels.

use std::fmt;
use std::str::FromStr;

use pkg_core::channel::PolicyVersion;
use pkg_core::identity::StorePath;
use pkg_core::state::Digest;
use serde::{Deserialize, Serialize};

use crate::broker::{BrokerOperationKind, OperationHandle, OperationStatus};
use crate::maintenance::{
    GenerationId, MaintenanceCapability, RemoveRootSetRequest, RepairMode, RepairOutcomeKind,
    RepairPathOutcome, RepairStorePathsReport, RepairStorePathsRequest, RootSet, RootSetEntry,
    RootSetReport, VerifiedRepairScope,
};
use crate::{
    DerivationPlanReport, EvaluateDerivationRequest, GcReport, JsonCodec, PathInfoReport, RootName,
    RootRef, SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use serde_json::value::RawValue;

const MAGIC: [u8; 4] = *b"PKG1";
const PROTOCOL_VERSION: u16 = 1;
const HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_ROOT_SET_WIRE_ENTRIES: usize = 4096;

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
    /// Query the pinned managed runtime under an authorized operation.
    Version(OperationHandle),
    /// Evaluate one validated derivation request under a resolve operation.
    EvaluateDerivation(OperationHandle, EvaluateDerivationRequest),
    /// Query validated metadata for one promoted store path.
    PathInfo(OperationHandle, StorePath),
    /// Attempt substitution for one promoted store path.
    Substitute(OperationHandle, StorePath),
    /// Verify one validated closed request.
    Verify(OperationHandle, VerifyRequest),
    /// Collect unreachable paths using the managed root set.
    Gc(OperationHandle),
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
    /// Validated pinned managed-runtime version information.
    Version(VersionInfo),
    /// Validated derivation evaluation result.
    DerivationPlan(DerivationPlanReport),
    /// Validated path metadata result.
    PathInfo(PathInfoReport),
    /// Validated substitution result.
    Substitute(SubstituteReport),
    /// Validated verification result.
    Verify(VerifyReport),
    /// Validated garbage-collection result.
    Gc(GcReport),
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
            2 | 3 | 10 | 16 => {
                let wire: HandleOwnedWire = decode_json(frame.payload)?;
                let handle = parse_handle(&wire.handle)?;
                match frame.method {
                    2 => CliBrokerRequest::Poll(handle),
                    3 => CliBrokerRequest::Cancel(handle),
                    10 => CliBrokerRequest::Version(handle),
                    16 => CliBrokerRequest::Gc(handle),
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
            CliBrokerResponse::Verify(report) => (15, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::Gc(report) => (16, report.encode().map_err(adapter_payload)?),
        };
        encode_frame(CHANNEL_CLI_BROKER, method, request_id, &payload)
    }

    /// Decodes one exact broker-to-CLI lifecycle response.
    pub fn decode_cli_response(bytes: &[u8]) -> Result<(u64, CliBrokerResponse), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_CLI_BROKER)?;
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
            15 => CliBrokerResponse::Verify(
                VerifyReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            16 => CliBrokerResponse::Gc(
                GcReport::decode(&JsonCodec::default(), frame.payload).map_err(adapter_payload)?,
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
    use crate::{AcceptedFormats, FormatVersion, NixVersion};

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

        let helper = BrokerHelperRequest::PublishRootSet(root_set());
        let encoded = ProductFrameCodec::encode_helper_request(9, &helper).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((9, helper))
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
