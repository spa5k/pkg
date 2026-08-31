//! Versioned Real-Nix capture and exact `FakeNix` replay support.
//!
//! Only validated [`NixAdapter`] requests and outcomes cross this boundary.
//! Raw argv, process output, environment values, and private runtime paths do
//! not enter the artifact (`plans/09` §4.3).

use std::fmt;
use std::sync::Mutex;

use pkg_nix::{
    BuildReport, BuildRequest, DerivationPlanReport, EvaluateDerivationRequest, GcReport,
    JsonCodec, NixAdapter, NixAdapterError, NixAdapterErrorCode, PathInfoReport, StorePath,
    SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use serde::{Deserialize, Serialize};

use crate::FakeNix;

const SCHEMA_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 64;
// This is an aggregate test-artifact limit, not the 64 MiB limit for one
// production adapter payload. Current full-closure platform goldens are below
// 0.5 MiB each. The separate cap keeps a malformed or accidental multi-call
// capture from multiplying the per-message allowance.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// A bounded error from capture, strict decoding, or replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityError {
    /// The artifact exceeded the fixed byte limit.
    Oversized,
    /// The artifact or a nested typed contract was malformed.
    Malformed,
    /// The artifact used an unsupported parity schema version.
    UnsupportedSchemaVersion,
    /// A capture exceeded the fixed call-count limit.
    TooManyEntries,
    /// Fake replay did not reproduce a captured outcome.
    ReplayMismatch,
    /// Fake replay left one or more expectations unused.
    ReplayIncomplete,
}

impl fmt::Display for ParityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Oversized => "parity artifact exceeds the byte limit",
            Self::Malformed => "parity artifact is malformed",
            Self::UnsupportedSchemaVersion => "unsupported parity schema version",
            Self::TooManyEntries => "parity artifact has too many entries",
            Self::ReplayMismatch => "FakeNix parity replay mismatch",
            Self::ReplayIncomplete => "FakeNix parity replay is incomplete",
        })
    }
}

impl std::error::Error for ParityError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedCall {
    Version(Result<VersionInfo, NixAdapterError>),
    Evaluate(
        EvaluateDerivationRequest,
        Result<DerivationPlanReport, NixAdapterError>,
    ),
    PathInfo(StorePath, Result<PathInfoReport, NixAdapterError>),
    Substitute(StorePath, Result<SubstituteReport, NixAdapterError>),
    Build(BuildRequest, Result<BuildReport, NixAdapterError>),
    Verify(VerifyRequest, Result<VerifyReport, NixAdapterError>),
    Gc(Result<GcReport, NixAdapterError>),
}

#[derive(Debug, Default)]
struct CaptureState {
    calls: Vec<RecordedCall>,
    overflowed: bool,
}

/// A test adapter wrapper that records validated typed calls and outcomes.
///
/// The fixed in-memory limit prevents an unbounded capture. Overflow makes the
/// snapshot fail. It does not change the wrapped adapter result after a real
/// mutating call has already completed.
pub struct CapturingNix<A> {
    inner: A,
    state: Mutex<CaptureState>,
}

impl<A> CapturingNix<A> {
    /// Wraps one adapter with an empty capture.
    #[must_use]
    pub fn new(inner: A) -> Self {
        Self {
            inner,
            state: Mutex::new(CaptureState::default()),
        }
    }

    fn record(&self, call: RecordedCall) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.calls.len() == MAX_ENTRIES {
            state.overflowed = true;
        } else if !state.overflowed {
            state.calls.push(call);
        }
    }

    /// Returns a stable snapshot of the calls recorded so far.
    pub fn transcript(&self) -> Result<ParityTranscript, ParityError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.overflowed {
            return Err(ParityError::TooManyEntries);
        }
        Ok(ParityTranscript {
            calls: state.calls.clone(),
        })
    }

    /// Returns the wrapped adapter.
    #[must_use]
    pub const fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A: NixAdapter> NixAdapter for CapturingNix<A> {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        let result = self.inner.version();
        self.record(RecordedCall::Version(result.clone()));
        result
    }

    fn evaluate_derivation(
        &self,
        request: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        let result = self.inner.evaluate_derivation(request);
        self.record(RecordedCall::Evaluate(request.clone(), result.clone()));
        result
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        let result = self.inner.path_info(path);
        self.record(RecordedCall::PathInfo(path.clone(), result.clone()));
        result
    }

    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        let result = self.inner.substitute(path);
        self.record(RecordedCall::Substitute(path.clone(), result.clone()));
        result
    }

    fn build(&self, request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        let result = self.inner.build(request);
        self.record(RecordedCall::Build(request.clone(), result.clone()));
        result
    }

    fn verify(&self, request: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        let result = self.inner.verify(request);
        self.record(RecordedCall::Verify(request.clone(), result.clone()));
        result
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        let result = self.inner.gc();
        self.record(RecordedCall::Gc(result.clone()));
        result
    }
}

/// One immutable, ordered Real-Nix adapter session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityTranscript {
    calls: Vec<RecordedCall>,
}

impl ParityTranscript {
    /// Returns the number of captured adapter calls.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.calls.len()
    }

    /// Returns `true` when the capture contains no calls.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Encodes deterministic, versioned golden JSON.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ParityError> {
        let mut entries = Vec::with_capacity(self.calls.len());
        let mut encoded_entries = 0_usize;
        for call in &self.calls {
            let entry = EntryWire::try_from(call)?;
            let entry_len = serde_json::to_vec(&entry)
                .map_err(|_| ParityError::Malformed)?
                .len();
            encoded_entries = encoded_entries
                .checked_add(entry_len)
                .ok_or(ParityError::Oversized)?;
            if encoded_entries > MAX_BYTES {
                return Err(ParityError::Oversized);
            }
            entries.push(entry);
        }
        let mut bytes = serde_json::to_vec_pretty(&TranscriptWire {
            schema_version: SCHEMA_VERSION,
            entries,
        })
        .map_err(|_| ParityError::Malformed)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_BYTES {
            return Err(ParityError::Oversized);
        }
        Ok(bytes)
    }

    /// Strictly decodes and validates one bounded golden artifact.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ParityError> {
        if bytes.len() > MAX_BYTES {
            return Err(ParityError::Oversized);
        }
        let wire: TranscriptWire =
            serde_json::from_slice(bytes).map_err(|_| ParityError::Malformed)?;
        if wire.schema_version != SCHEMA_VERSION {
            return Err(ParityError::UnsupportedSchemaVersion);
        }
        if wire.entries.len() > MAX_ENTRIES {
            return Err(ParityError::TooManyEntries);
        }
        let calls = wire
            .entries
            .into_iter()
            .map(RecordedCall::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { calls })
    }

    /// Replays the same requests through `FakeNix` and compares typed results.
    pub fn assert_fake_parity(&self) -> Result<(), ParityError> {
        let fake = FakeNix::new();
        for call in &self.calls {
            add_expectation(&fake, call);
        }
        for call in &self.calls {
            let matches = match call {
                RecordedCall::Version(expected) => same_outcome(&fake.version(), expected),
                RecordedCall::Evaluate(request, expected) => {
                    same_outcome(&fake.evaluate_derivation(request), expected)
                }
                RecordedCall::PathInfo(request, expected) => {
                    same_outcome(&fake.path_info(request), expected)
                }
                RecordedCall::Substitute(request, expected) => {
                    same_outcome(&fake.substitute(request), expected)
                }
                RecordedCall::Build(request, expected) => {
                    same_outcome(&fake.build(request), expected)
                }
                RecordedCall::Verify(request, expected) => {
                    same_outcome(&fake.verify(request), expected)
                }
                RecordedCall::Gc(expected) => same_outcome(&fake.gc(), expected),
            };
            if !matches {
                return Err(ParityError::ReplayMismatch);
            }
        }
        fake.assert_exhausted()
            .map_err(|_| ParityError::ReplayIncomplete)
    }
}

fn same_outcome<T: PartialEq>(
    actual: &Result<T, NixAdapterError>,
    expected: &Result<T, NixAdapterError>,
) -> bool {
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => actual == expected,
        (Err(actual), Err(expected)) => actual.code() == expected.code(),
        _ => false,
    }
}

fn add_expectation(fake: &FakeNix, call: &RecordedCall) {
    match call {
        RecordedCall::Version(result) => {
            fake.expect_version(result.clone());
        }
        RecordedCall::Evaluate(request, result) => {
            fake.expect_evaluate_derivation(request.clone(), result.clone());
        }
        RecordedCall::PathInfo(request, result) => {
            fake.expect_path_info(request.clone(), result.clone());
        }
        RecordedCall::Substitute(request, result) => {
            fake.expect_substitute(request.clone(), result.clone());
        }
        RecordedCall::Build(request, result) => {
            fake.expect_build(request.clone(), result.clone());
        }
        RecordedCall::Verify(request, result) => {
            fake.expect_verify(request.clone(), result.clone());
        }
        RecordedCall::Gc(result) => {
            fake.expect_gc(result.clone());
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranscriptWire {
    schema_version: u32,
    entries: Vec<EntryWire>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum EntryWire {
    Version {
        outcome: OutcomeWire,
    },
    EvaluateDerivation {
        #[serde(rename = "requestJson")]
        request_json: String,
        outcome: OutcomeWire,
    },
    PathInfo {
        #[serde(rename = "requestJson")]
        request_json: String,
        outcome: OutcomeWire,
    },
    Substitute {
        #[serde(rename = "requestJson")]
        request_json: String,
        outcome: OutcomeWire,
    },
    Build {
        #[serde(rename = "requestJson")]
        request_json: String,
        outcome: OutcomeWire,
    },
    Verify {
        #[serde(rename = "requestJson")]
        request_json: String,
        outcome: OutcomeWire,
    },
    Gc {
        outcome: OutcomeWire,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum OutcomeWire {
    Ok {
        #[serde(rename = "responseJson")]
        response_json: String,
    },
    Error {
        #[serde(rename = "errorCode")]
        error_code: String,
    },
}

impl TryFrom<&RecordedCall> for EntryWire {
    type Error = ParityError;

    fn try_from(call: &RecordedCall) -> Result<Self, Self::Error> {
        Ok(match call {
            RecordedCall::Version(outcome) => Self::Version {
                outcome: encode_outcome(outcome, VersionInfo::encode)?,
            },
            RecordedCall::Evaluate(request, outcome) => Self::EvaluateDerivation {
                request_json: encode_contract(request, EvaluateDerivationRequest::encode)?,
                outcome: encode_outcome(outcome, DerivationPlanReport::encode)?,
            },
            RecordedCall::PathInfo(request, outcome) => Self::PathInfo {
                request_json: encode_store_path(request)?,
                outcome: encode_outcome(outcome, PathInfoReport::encode)?,
            },
            RecordedCall::Substitute(request, outcome) => Self::Substitute {
                request_json: encode_store_path(request)?,
                outcome: encode_outcome(outcome, SubstituteReport::encode)?,
            },
            RecordedCall::Build(request, outcome) => Self::Build {
                request_json: encode_contract(request, BuildRequest::encode)?,
                outcome: encode_outcome(outcome, BuildReport::encode)?,
            },
            RecordedCall::Verify(request, outcome) => Self::Verify {
                request_json: encode_contract(request, VerifyRequest::encode)?,
                outcome: encode_outcome(outcome, VerifyReport::encode)?,
            },
            RecordedCall::Gc(outcome) => Self::Gc {
                outcome: encode_outcome(outcome, GcReport::encode)?,
            },
        })
    }
}

impl TryFrom<EntryWire> for RecordedCall {
    type Error = ParityError;

    fn try_from(entry: EntryWire) -> Result<Self, Self::Error> {
        let codec = JsonCodec::production();
        Ok(match entry {
            EntryWire::Version { outcome } => Self::Version(decode_outcome(outcome, |bytes| {
                VersionInfo::decode(&codec, bytes)
            })?),
            EntryWire::EvaluateDerivation {
                request_json,
                outcome,
            } => Self::Evaluate(
                decode_contract(&request_json, |bytes| {
                    EvaluateDerivationRequest::decode(&codec, bytes)
                })?,
                decode_outcome(outcome, |bytes| DerivationPlanReport::decode(&codec, bytes))?,
            ),
            EntryWire::PathInfo {
                request_json,
                outcome,
            } => Self::PathInfo(
                decode_store_path(&request_json)?,
                decode_outcome(outcome, |bytes| PathInfoReport::decode(&codec, bytes))?,
            ),
            EntryWire::Substitute {
                request_json,
                outcome,
            } => Self::Substitute(
                decode_store_path(&request_json)?,
                decode_outcome(outcome, |bytes| SubstituteReport::decode(&codec, bytes))?,
            ),
            EntryWire::Build {
                request_json,
                outcome,
            } => Self::Build(
                decode_contract(&request_json, |bytes| BuildRequest::decode(&codec, bytes))?,
                decode_outcome(outcome, |bytes| BuildReport::decode(&codec, bytes))?,
            ),
            EntryWire::Verify {
                request_json,
                outcome,
            } => Self::Verify(
                decode_contract(&request_json, |bytes| VerifyRequest::decode(&codec, bytes))?,
                decode_outcome(outcome, |bytes| VerifyReport::decode(&codec, bytes))?,
            ),
            EntryWire::Gc { outcome } => Self::Gc(decode_outcome(outcome, |bytes| {
                GcReport::decode(&codec, bytes)
            })?),
        })
    }
}

fn encode_contract<T>(
    value: &T,
    encode: impl FnOnce(&T) -> Result<Vec<u8>, NixAdapterError>,
) -> Result<String, ParityError> {
    let bytes = encode(value).map_err(|_| ParityError::Malformed)?;
    if bytes.len() > MAX_BYTES {
        return Err(ParityError::Oversized);
    }
    String::from_utf8(bytes).map_err(|_| ParityError::Malformed)
}

fn decode_contract<T>(
    json: &str,
    decode: impl FnOnce(&[u8]) -> Result<T, NixAdapterError>,
) -> Result<T, ParityError> {
    if json.len() > JsonCodec::PRODUCTION_LIMIT {
        return Err(ParityError::Oversized);
    }
    decode(json.as_bytes()).map_err(|_| ParityError::Malformed)
}

fn encode_store_path(path: &StorePath) -> Result<String, ParityError> {
    serde_json::to_string(path.as_str()).map_err(|_| ParityError::Malformed)
}

fn decode_store_path(json: &str) -> Result<StorePath, ParityError> {
    if json.len() > JsonCodec::PRODUCTION_LIMIT {
        return Err(ParityError::Oversized);
    }
    let path: String = serde_json::from_str(json).map_err(|_| ParityError::Malformed)?;
    StorePath::new(&path).map_err(|_| ParityError::Malformed)
}

fn encode_outcome<T>(
    outcome: &Result<T, NixAdapterError>,
    encode: impl FnOnce(&T) -> Result<Vec<u8>, NixAdapterError>,
) -> Result<OutcomeWire, ParityError> {
    match outcome {
        Ok(value) => Ok(OutcomeWire::Ok {
            response_json: encode_contract(value, encode)?,
        }),
        Err(error) => Ok(OutcomeWire::Error {
            error_code: error.code().as_str().to_owned(),
        }),
    }
}

fn decode_outcome<T>(
    outcome: OutcomeWire,
    decode: impl FnOnce(&[u8]) -> Result<T, NixAdapterError>,
) -> Result<Result<T, NixAdapterError>, ParityError> {
    match outcome {
        OutcomeWire::Ok { response_json } => decode_contract(&response_json, decode).map(Ok),
        OutcomeWire::Error { error_code } => decode_error_code(&error_code)
            .map(NixAdapterError::remote)
            .map(Err),
    }
}

fn decode_error_code(value: &str) -> Result<NixAdapterErrorCode, ParityError> {
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
        _ => Err(ParityError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use pkg_nix::{AcceptedFormats, FormatVersion, NixVersion};

    use super::*;

    fn version() -> VersionInfo {
        VersionInfo::new(
            NixVersion::new("2.34.8").expect("fixture version"),
            AcceptedFormats::new(FormatVersion::new(2).expect("fixture format")),
        )
    }

    #[test]
    fn capture_round_trips_and_replays_exact_typed_result() {
        let fake = FakeNix::new();
        fake.expect_version(Ok(version()));
        let capture = CapturingNix::new(fake);
        assert_eq!(capture.version(), Ok(version()));
        let transcript = capture.transcript().expect("capture");
        let bytes = transcript.to_json_bytes().expect("encode");
        let decoded = ParityTranscript::from_json_bytes(&bytes).expect("decode");
        assert_eq!(decoded, transcript);
        decoded.assert_fake_parity().expect("parity");
    }

    #[test]
    fn error_capture_replays_the_same_stable_code() {
        let fake = FakeNix::new();
        fake.expect_version(Err(NixAdapterError::Timeout));
        let capture = CapturingNix::new(fake);
        assert_eq!(capture.version(), Err(NixAdapterError::Timeout));
        let bytes = capture
            .transcript()
            .expect("capture")
            .to_json_bytes()
            .expect("encode");
        let decoded = ParityTranscript::from_json_bytes(&bytes).expect("decode");
        decoded.assert_fake_parity().expect("parity");
        assert!(String::from_utf8(bytes).expect("utf8").contains("timeout"));
    }

    #[test]
    fn strict_decoders_reject_widening_and_duplicates() {
        for bytes in [
            br#"{"schemaVersion":1,"entries":[],"private":"x"}"#.as_slice(),
            br#"{"schemaVersion":1,"entries":[{"method":"version","outcome":{"status":"error","errorCode":"timeout","private":"x"}}]}"#.as_slice(),
            br#"{"schemaVersion":1,"schemaVersion":1,"entries":[]}"#.as_slice(),
        ] {
            assert_eq!(
                ParityTranscript::from_json_bytes(bytes),
                Err(ParityError::Malformed)
            );
        }
    }

    #[test]
    fn schema_and_size_limits_fail_closed() {
        assert_eq!(
            ParityTranscript::from_json_bytes(br#"{"schemaVersion":2,"entries":[]}"#),
            Err(ParityError::UnsupportedSchemaVersion)
        );
        assert_eq!(
            ParityTranscript::from_json_bytes(&vec![b' '; MAX_BYTES + 1]),
            Err(ParityError::Oversized)
        );
    }

    #[test]
    fn capture_overflow_does_not_change_wrapped_results() {
        let fake = FakeNix::new();
        for _ in 0..=MAX_ENTRIES {
            fake.expect_version(Ok(version()));
        }
        let capture = CapturingNix::new(fake);
        for _ in 0..=MAX_ENTRIES {
            assert_eq!(capture.version(), Ok(version()));
        }
        assert_eq!(capture.transcript(), Err(ParityError::TooManyEntries));
    }
}
