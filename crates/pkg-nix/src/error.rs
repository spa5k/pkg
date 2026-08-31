//! The closed, redacted error type for the [`NixAdapter`](crate::NixAdapter)
//! trait, plus its stable codes.
//!
//! [`NixAdapterError`] is the **only** error type that crosses the adapter
//! boundary (`plans/09` §4.1). It carries **stable closed codes** (via
//! [`NixAdapterErrorCode`]) and **bounded, redacted context** only. It **never**
//! carries raw wire JSON, stdout/stderr, credentials, or unbounded paths
//! (T-DAEMON-2). Every variant that the architecture lists as an adapter error
//! is represented here: oversized input, malformed payload, unsupported upstream
//! JSON format, validation failure, timeout, unavailable backend, the
//! transcript-mismatch variants [`NixAdapterError::UnexpectedCall`] and
//! [`NixAdapterError::UnexpectedExtraCall`], and four coarse
//! operation-failure categories — [`NixAdapterError::TrustFailure`],
//! [`NixAdapterError::IntegrityFailure`],
//! [`NixAdapterError::PermissionDenied`], and
//! [`NixAdapterError::OperationFailed`] (`plans/09` §4.1/§4.4).
//! [`NixAdapterError::UnsupportedSchemaVersion`] is the pkg-contract schema
//! mismatch, deliberately distinct from the upstream Nix format mismatch
//! [`NixAdapterError::UnsupportedUpstreamFormat`].

use crate::contract::MethodKind;
use std::fmt;

/// The maximum byte length of any bounded redacted summary carried by an error.
///
/// Summaries are truncated at a UTF-8 character boundary to this length. By
/// contract they only ever hold **redacted, non-secret, bounded** content
/// (category names such as `"invalid store path"`), never raw inputs, paths,
/// or JSON.
const SUMMARY_MAX_BYTES: usize = 128;

/// A bounded, redacted text summary carried inside [`NixAdapterError`].
///
/// This is the only kind of free-form text an error may carry, and it is
/// capped at [`BoundedSummary::MAX`] bytes. **Construction is crate-private**
/// (`BoundedSummary::new`): the *only* way text enters an error is
/// crate-internal construction from a `&'static str` static category name. In
/// particular the [`NixAdapterError::unexpected_call`] transcript-mismatch
/// constructor takes **no** external text argument: it selects between two
/// crate-owned static summaries (`"method mismatch"` / `"request mismatch"`)
/// based solely on the `expected`/`actual` [`MethodKind`]s, and the sibling
/// [`NixAdapterError::unexpected_extra_call`] no-head constructor uses the
/// single crate-owned static summary `"extra call"`. No external free-text
/// input may ever enter [`NixAdapterError`]. External code may only *read* a
/// summary (via [`BoundedSummary::as_str`]) after the fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedSummary(String);

impl BoundedSummary {
    /// The hard byte cap on summary text.
    pub const MAX: usize = SUMMARY_MAX_BYTES;

    /// Constructs a bounded summary from `text`, truncating at a UTF-8
    /// character boundary if it exceeds [`BoundedSummary::MAX`].
    ///
    /// This is **crate-private**: it is the redaction boundary. The only
    /// callers are crate-internal, and they pass only **static, redacted**,
    /// non-secret category strings (including the two static summaries the
    /// [`NixAdapterError::unexpected_call`] constructor selects between). This
    /// type enforces the byte bound, not the redaction; the redaction is a
    /// property of what the (crate-internal) callers choose to store.
    pub(crate) fn new(text: &str) -> Self {
        Self(truncate_at_char_boundary(text, Self::MAX))
    }

    /// Returns the (already-bounded) summary text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Truncates `s` to at most `max_bytes`, backing up to the nearest UTF-8
/// character boundary so the result is always valid UTF-8. Inputs already at or
/// under the cap are copied unchanged.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// A coarse, stable classification of a malformed-payload failure.
///
/// It carries **no raw bytes**; it only distinguishes JSON-shape problems from
/// excessive-nesting problems (the latter surfaced by serde_json's default
/// recursion protection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedKind {
    /// JSON that was malformed, had trailing data, or failed strict
    /// (deny-unknown-field) parsing. No input bytes are retained.
    Json,
    /// Reserved for future recursive DTOs: input nested deeper than
    /// serde_json's default recursion limit. The non-recursive DTOs defined
    /// here surface deep nesting as a JSON type-mismatch
    /// ([`MalformedKind::Json`]) before reaching the recursion limit, so this
    /// variant is **not currently produced**; it is kept so a recursive DTO
    /// added later has a stable, distinct classification.
    ExcessiveNesting,
}

impl MalformedKind {
    /// Returns the stable, lowercase classification name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MalformedKind::Json => "json",
            MalformedKind::ExcessiveNesting => "excessive-nesting",
        }
    }
}

impl fmt::Display for MalformedKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable, closed error codes for [`NixAdapterError`].
///
/// These are the public, stable identifiers callers match or log on; they are
/// deliberately decoupled from the (richer, internal) enum variants so the code
/// set can be documented as a stable contract even if internal detail evolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NixAdapterErrorCode {
    /// A transcript mismatch: either a call that did not match an existing
    /// head expectation ([`NixAdapterError::UnexpectedCall`]), or a call that
    /// arrived with no remaining head expectation
    /// ([`NixAdapterError::UnexpectedExtraCall`]). Both variants map to this
    /// single code (`plans/09` §4.4).
    UnexpectedCall,
    /// Decoded input exceeded the codec's byte cap before parsing.
    OversizedInput,
    /// Input was malformed JSON, had trailing data, was excessively nested, or
    /// failed strict unknown-field rejection.
    MalformedPayload,
    /// A `pkg`-owned serialized payload reported a `schemaVersion` this decoder
    /// does not accept.
    UnsupportedSchemaVersion,
    /// A real Nix backend returned an upstream JSON format version the adapter
    /// does not accept (`plans/01` §11).
    UnsupportedUpstreamFormat,
    /// A promoted value failed domain validation.
    ValidationFailure,
    /// The operation exceeded its time budget.
    Timeout,
    /// The Nix backend (e.g. the managed daemon) was unavailable.
    Unavailable,
    /// A cache/signature trust rejection surfaced by a fetching or building
    /// lane (`substitute`, `build`), e.g. a downloaded object whose signatures
    /// or trusted cache could not be verified. Distinct from a read-only
    /// [`crate::TrustStatus::Untrusted`] observation, which verify reports
    /// normally.
    TrustFailure,
    /// Verified NAR/hash corruption observed as an operation failure on a
    /// mutating/fetching lane. Distinct from a read-only
    /// [`crate::NarIntegrity::Corrupt`] observation, which verify reports
    /// normally.
    IntegrityFailure,
    /// A build-approval or root-helper authorization rejection (e.g. an
    /// invalid/already-consumed approval receipt, or the root helper refusing
    /// the caller).
    PermissionDenied,
    /// A bounded generic backend process/IO failure that is neither a timeout
    /// ([`NixAdapterErrorCode::Timeout`]) nor an unavailable backend
    /// ([`NixAdapterErrorCode::Unavailable`]).
    OperationFailed,
}

impl NixAdapterErrorCode {
    /// Returns the stable snake_case code string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            NixAdapterErrorCode::UnexpectedCall => "unexpected_call",
            NixAdapterErrorCode::OversizedInput => "oversized_input",
            NixAdapterErrorCode::MalformedPayload => "malformed_payload",
            NixAdapterErrorCode::UnsupportedSchemaVersion => "unsupported_schema_version",
            NixAdapterErrorCode::UnsupportedUpstreamFormat => "unsupported_upstream_format",
            NixAdapterErrorCode::ValidationFailure => "validation_failure",
            NixAdapterErrorCode::Timeout => "timeout",
            NixAdapterErrorCode::Unavailable => "unavailable",
            NixAdapterErrorCode::TrustFailure => "trust_failure",
            NixAdapterErrorCode::IntegrityFailure => "integrity_failure",
            NixAdapterErrorCode::PermissionDenied => "permission_denied",
            NixAdapterErrorCode::OperationFailed => "operation_failed",
        }
    }
}

impl fmt::Display for NixAdapterErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The only error type returned by [`NixAdapter`](crate::NixAdapter) methods.
///
/// It is **closed** (a fixed enum), **redacted** (carries only bounded summaries
/// and stable codes — never raw JSON, stdout/stderr, credentials, or unbounded
/// paths), and [`Clone`] + [`Eq`] so tests and transcript matchers can compare
/// errors by value.
///
/// # Variants and the architecture
///
/// Every adapter failure named in `plans/09` §4.1/§4.4 maps to a variant here:
/// oversized input, malformed payload, unsupported upstream JSON format,
/// validation failure, timeout, unavailable backend, the two transcript-
/// mismatch variants `UnexpectedCall` and `UnexpectedExtraCall`, and four coarse
/// operation-failure categories —
/// [`Self::TrustFailure`], [`Self::IntegrityFailure`], [`Self::PermissionDenied`],
/// and [`Self::OperationFailed`]. [`Self::UnsupportedSchemaVersion`] is the
/// pkg-contract schema mismatch, separate from the upstream format mismatch
/// [`Self::UnsupportedUpstreamFormat`].
///
/// The four operation-failure categories are deliberately distinct from the
/// read-only `verify` report observations: a read-only verify **normally
/// reports** an [`crate::Untrusted`](crate::TrustStatus::Untrusted) trust
/// status and a [`crate::Corrupt`](crate::NarIntegrity) NAR-integrity status
/// in its [`crate::VerifyReport`]; `TrustFailure`/`IntegrityFailure` instead
/// represent the same underlying conditions surfacing as an operation failure
/// on a lane where trust/integrity is a precondition (substitute or build,
/// including a downloaded object that failed its trust/integrity checks). The
/// contract is closed and exhaustive (no `non_exhaustive`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixAdapterError {
    /// A trait call popped a head transcript expectation of the wrong method,
    /// or whose request did not equal the head matcher (`plans/09` §4.4).
    ///
    /// Carries the `expected` and `actual` [`MethodKind`] plus a **bounded,
    /// redacted, crate-owned static** `mismatch` summary. The summary is never
    /// caller-supplied free text: it is one of two static strings selected by
    /// [`NixAdapterError::unexpected_call`] based only on whether the two
    /// method kinds differ. It deliberately carries **no raw request data**
    /// and never a `Vec` of expectations. This variant is the `pkg-nix`-side
    /// half of the transcript contract; a `pkg-testkit` `FakeNix` constructs
    /// it (via [`NixAdapterError::unexpected_call`]); `pkg-nix` never depends
    /// on `pkg-testkit`. The no-head case (an empty or fully-consumed
    /// transcript, where no honest `expected` exists) uses the sibling
    /// [`Self::UnexpectedExtraCall`].
    UnexpectedCall {
        /// The method kind the head expectation required.
        expected: MethodKind,
        /// The method kind of the call that actually arrived.
        actual: MethodKind,
        /// A bounded, redacted, crate-owned static summary of how the call
        /// missed the matcher (one of two fixed strings; never free text).
        mismatch: BoundedSummary,
    },
    /// A trait call arrived when the transcript held **no head expectation** —
    /// the call was extra: the transcript was initially empty, or every
    /// expectation had already been consumed (`plans/09` §4.4).
    ///
    /// This is the honest sibling of [`UnexpectedCall`](Self::UnexpectedCall)
    /// for the no-head case: there is no `expected: MethodKind` to name,
    /// because no expectation remained. It carries only the `actual` method
    /// kind that arrived and a **bounded, redacted, crate-owned static**
    /// `summary` — the single fixed string `"extra call"`. Like
    /// [`UnexpectedCall`](Self::UnexpectedCall), it carries **no raw request
    /// data** and never a `Vec` of expectations, and it reuses the single
    /// [`NixAdapterErrorCode::UnexpectedCall`] code: the two transcript-
    /// mismatch variants are one *code*, distinguished by whether an
    /// expectation existed (a head-bearing mismatch versus a no-head extra
    /// call). A `pkg-testkit` `FakeNix` constructs it via
    /// [`NixAdapterError::unexpected_extra_call`]; `pkg-nix` never depends on
    /// `pkg-testkit`.
    UnexpectedExtraCall {
        /// The method kind of the call that arrived with no expectation to
        /// match.
        actual: MethodKind,
        /// A bounded, redacted, crate-owned static summary (the fixed string
        /// `"extra call"`; never free text).
        summary: BoundedSummary,
    },
    /// Decoded input exceeded the codec's byte cap. The check happens **before**
    /// parsing, so oversized input is rejected without materializing it.
    OversizedInput {
        /// The byte limit that was exceeded.
        limit_bytes: usize,
    },
    /// Input was malformed JSON, carried trailing data, was excessively nested,
    /// or failed strict unknown-field rejection. Only a coarse, redacted
    /// [`MalformedKind`] is retained — never the payload.
    MalformedPayload {
        /// The coarse, redacted classification.
        kind: MalformedKind,
    },
    /// A `pkg`-owned serialized payload carried a `schemaVersion` this decoder
    /// does not accept.
    UnsupportedSchemaVersion {
        /// The schema version observed in the payload.
        observed: u32,
    },
    /// A real Nix backend returned an upstream JSON format version the pinned
    /// adapter does not accept (`plans/01` §11). Normalization into the
    /// `pkg`-owned contract happens before this can surface.
    UnsupportedUpstreamFormat {
        /// The command whose upstream JSON format version was unsupported.
        command: MethodKind,
        /// The upstream format version observed in the response.
        observed: u32,
    },
    /// A promoted value failed pkg-domain validation (e.g. an invalid store
    /// path, a duplicate in a collection, or an inconsistent status/payload
    /// combination). Carries only a bounded, redacted `summary` — never the
    /// offending value.
    ValidationFailure {
        /// A bounded, redacted summary of which validation failed.
        summary: BoundedSummary,
    },
    /// The operation exceeded its time budget.
    Timeout,
    /// The Nix backend (e.g. the managed daemon) was unavailable.
    Unavailable,
    /// A cache/signature trust rejection surfaced by a fetching or building
    /// lane (`substitute`, `build`), e.g. a downloaded object whose signatures
    /// or trusted cache could not be verified. Read-only `verify` still
    /// reports [`crate::TrustStatus::Untrusted`] as a normal observation; this
    /// variant is the **operation failure** when trust is a precondition for
    /// substituting or building a path.
    TrustFailure,
    /// Verified NAR/hash corruption observed as an operation failure on a
    /// fetching/building lane. Read-only `verify` still reports
    /// [`crate::NarIntegrity::Corrupt`] as a normal observation; this variant
    /// is the **operation failure** when integrity is a precondition for
    /// proceeding (e.g. a downloaded object did not match its expected hash, or
    /// a substituted/built path failed its NAR verification).
    IntegrityFailure,
    /// A build-approval or root-helper authorization rejection: an invalid or
    /// already-consumed approval receipt, or the authenticated root helper
    /// refusing the caller (`plans/09` §4.1). Carries no data.
    PermissionDenied,
    /// A bounded generic backend process/IO failure that is neither a timeout
    /// ([`Self::Timeout`]) nor an unavailable backend ([`Self::Unavailable`]).
    /// Carries no data: the operation exited non-zero, a file failed to read,
    /// a child process was killed, etc.
    OperationFailed,
    /// A broker transported only this stable error code and intentionally
    /// omitted variant-specific private metadata.
    RemoteFailure {
        /// The exact redacted code returned by the authenticated broker.
        code: NixAdapterErrorCode,
    },
}

impl NixAdapterError {
    /// Constructs the [`NixAdapterError::UnexpectedCall`] transcript-mismatch
    /// variant from the expected/actual method kinds **only**.
    ///
    /// This is the constructor a `pkg-testkit` `FakeNix` uses; `pkg-nix` itself
    /// never depends on `pkg-testkit`. No external free-text argument is
    /// accepted: the `mismatch` summary is a **crate-owned static** string
    /// selected by this constructor based only on the method kinds —
    /// `"method mismatch"` when `expected != actual`, and `"request
    /// mismatch"` when they are equal — so no raw request bytes or arbitrary
    /// text can ever enter the error.
    #[must_use]
    pub fn unexpected_call(expected: MethodKind, actual: MethodKind) -> Self {
        let summary = if expected == actual {
            "request mismatch"
        } else {
            "method mismatch"
        };
        Self::UnexpectedCall {
            expected,
            actual,
            mismatch: BoundedSummary::new(summary),
        }
    }

    /// Constructs the [`NixAdapterError::UnexpectedExtraCall`] no-head variant
    /// from the actual method kind **only**.
    ///
    /// This is the constructor a `pkg-testkit` `FakeNix` uses for a call
    /// against an empty or fully-consumed transcript, where there is no honest
    /// `expected: MethodKind`; `pkg-nix` itself never depends on `pkg-testkit`.
    /// No external free-text argument is accepted: the `summary` is the single
    /// **crate-owned static** string `"extra call"`, so no raw request bytes or
    /// arbitrary text can ever enter the error. It reuses the single
    /// [`NixAdapterErrorCode::UnexpectedCall`] code; `expected_method` returns
    /// `None`, `actual_method` returns `Some(actual)`, and `mismatch_summary`
    /// returns `Some("extra call")`.
    #[must_use]
    pub fn unexpected_extra_call(actual: MethodKind) -> Self {
        Self::UnexpectedExtraCall {
            actual,
            summary: BoundedSummary::new("extra call"),
        }
    }

    /// Reconstructs a redacted adapter failure from the broker's closed
    /// error-code envelope without inventing omitted private metadata.
    #[must_use]
    pub const fn remote(code: NixAdapterErrorCode) -> Self {
        Self::RemoteFailure { code }
    }

    /// Returns the stable [`NixAdapterErrorCode`] for this error.
    #[must_use]
    pub const fn code(&self) -> NixAdapterErrorCode {
        match self {
            Self::UnexpectedCall { .. } => NixAdapterErrorCode::UnexpectedCall,
            Self::UnexpectedExtraCall { .. } => NixAdapterErrorCode::UnexpectedCall,
            Self::OversizedInput { .. } => NixAdapterErrorCode::OversizedInput,
            Self::MalformedPayload { .. } => NixAdapterErrorCode::MalformedPayload,
            Self::UnsupportedSchemaVersion { .. } => NixAdapterErrorCode::UnsupportedSchemaVersion,
            Self::UnsupportedUpstreamFormat { .. } => {
                NixAdapterErrorCode::UnsupportedUpstreamFormat
            }
            Self::ValidationFailure { .. } => NixAdapterErrorCode::ValidationFailure,
            Self::Timeout => NixAdapterErrorCode::Timeout,
            Self::Unavailable => NixAdapterErrorCode::Unavailable,
            Self::TrustFailure => NixAdapterErrorCode::TrustFailure,
            Self::IntegrityFailure => NixAdapterErrorCode::IntegrityFailure,
            Self::PermissionDenied => NixAdapterErrorCode::PermissionDenied,
            Self::OperationFailed => NixAdapterErrorCode::OperationFailed,
            Self::RemoteFailure { code } => *code,
        }
    }

    /// Returns the expected [`MethodKind`], if this is an `UnexpectedCall`.
    #[must_use]
    pub const fn expected_method(&self) -> Option<MethodKind> {
        match self {
            Self::UnexpectedCall { expected, .. } => Some(*expected),
            // No expectation existed for an extra call, so there is no honest
            // expected method to report.
            Self::UnexpectedExtraCall { .. } => None,
            _ => None,
        }
    }

    /// Returns the actual [`MethodKind`], if this is an `UnexpectedCall` or
    /// `UnexpectedExtraCall`.
    #[must_use]
    pub const fn actual_method(&self) -> Option<MethodKind> {
        match self {
            Self::UnexpectedCall { actual, .. } => Some(*actual),
            Self::UnexpectedExtraCall { actual, .. } => Some(*actual),
            _ => None,
        }
    }

    /// Returns the bounded, redacted mismatch summary, if this is an
    /// `UnexpectedCall` or `UnexpectedExtraCall`.
    #[must_use]
    pub fn mismatch_summary(&self) -> Option<&str> {
        match self {
            Self::UnexpectedCall { mismatch, .. } => Some(mismatch.as_str()),
            Self::UnexpectedExtraCall { summary, .. } => Some(summary.as_str()),
            _ => None,
        }
    }
}

impl fmt::Display for NixAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedCall {
                expected,
                actual,
                mismatch,
            } => write!(
                f,
                "unexpected adapter call: expected {}, actual {} ({})",
                expected.as_str(),
                actual.as_str(),
                mismatch.as_str()
            ),
            Self::UnexpectedExtraCall { actual, summary } => write!(
                f,
                "unexpected adapter call: no expectation remained, actual {} ({})",
                actual.as_str(),
                summary.as_str()
            ),
            Self::OversizedInput { limit_bytes } => {
                write!(f, "oversized input: exceeds {limit_bytes} bytes")
            }
            Self::MalformedPayload { kind } => {
                write!(f, "malformed payload: {kind}")
            }
            Self::UnsupportedSchemaVersion { observed } => {
                write!(f, "unsupported schema version {observed}")
            }
            Self::UnsupportedUpstreamFormat { command, observed } => write!(
                f,
                "unsupported upstream {} format version {observed}",
                command.as_str()
            ),
            Self::ValidationFailure { summary } => {
                write!(f, "validation failure: {}", summary.as_str())
            }
            Self::Timeout => f.write_str("operation timed out"),
            Self::Unavailable => f.write_str("nix backend unavailable"),
            Self::TrustFailure => f.write_str("trust failure"),
            Self::IntegrityFailure => f.write_str("integrity failure"),
            Self::PermissionDenied => f.write_str("permission denied"),
            Self::OperationFailed => f.write_str("backend operation failed"),
            Self::RemoteFailure { code } => write!(f, "remote adapter failure: {code}"),
        }
    }
}

impl std::error::Error for NixAdapterError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_strings() {
        // Stable snake_case codes; the set is closed and ordered.
        assert_eq!(
            NixAdapterErrorCode::UnexpectedCall.as_str(),
            "unexpected_call"
        );
        assert_eq!(
            NixAdapterErrorCode::OversizedInput.as_str(),
            "oversized_input"
        );
        assert_eq!(
            NixAdapterErrorCode::MalformedPayload.as_str(),
            "malformed_payload"
        );
        assert_eq!(
            NixAdapterErrorCode::UnsupportedSchemaVersion.as_str(),
            "unsupported_schema_version"
        );
        assert_eq!(
            NixAdapterErrorCode::UnsupportedUpstreamFormat.as_str(),
            "unsupported_upstream_format"
        );
        assert_eq!(
            NixAdapterErrorCode::ValidationFailure.as_str(),
            "validation_failure"
        );
        assert_eq!(NixAdapterErrorCode::Timeout.as_str(), "timeout");
        assert_eq!(NixAdapterErrorCode::Unavailable.as_str(), "unavailable");
        assert_eq!(NixAdapterErrorCode::TrustFailure.as_str(), "trust_failure");
        assert_eq!(
            NixAdapterErrorCode::IntegrityFailure.as_str(),
            "integrity_failure"
        );
        assert_eq!(
            NixAdapterErrorCode::PermissionDenied.as_str(),
            "permission_denied"
        );
        assert_eq!(
            NixAdapterErrorCode::OperationFailed.as_str(),
            "operation_failed"
        );
    }

    #[test]
    fn code_maps_each_variant() {
        assert_eq!(
            NixAdapterError::Timeout.code(),
            NixAdapterErrorCode::Timeout
        );
        assert_eq!(
            NixAdapterError::Unavailable.code(),
            NixAdapterErrorCode::Unavailable
        );
        assert_eq!(
            NixAdapterError::OversizedInput { limit_bytes: 7 }.code(),
            NixAdapterErrorCode::OversizedInput
        );
        assert_eq!(
            NixAdapterError::MalformedPayload {
                kind: MalformedKind::Json
            }
            .code(),
            NixAdapterErrorCode::MalformedPayload
        );
        assert_eq!(
            NixAdapterError::MalformedPayload {
                kind: MalformedKind::ExcessiveNesting
            }
            .code(),
            NixAdapterErrorCode::MalformedPayload
        );
        assert_eq!(
            NixAdapterError::UnsupportedSchemaVersion { observed: 2 }.code(),
            NixAdapterErrorCode::UnsupportedSchemaVersion
        );
        assert_eq!(
            NixAdapterError::TrustFailure.code(),
            NixAdapterErrorCode::TrustFailure
        );
        assert_eq!(
            NixAdapterError::IntegrityFailure.code(),
            NixAdapterErrorCode::IntegrityFailure
        );
        assert_eq!(
            NixAdapterError::PermissionDenied.code(),
            NixAdapterErrorCode::PermissionDenied
        );
        assert_eq!(
            NixAdapterError::OperationFailed.code(),
            NixAdapterErrorCode::OperationFailed
        );
        assert_eq!(
            NixAdapterError::remote(NixAdapterErrorCode::TrustFailure).code(),
            NixAdapterErrorCode::TrustFailure
        );
    }

    #[test]
    fn unexpected_call_emits_only_static_summaries() {
        // The constructor takes only the expected/actual MethodKind — no free
        // text argument. The summary is a crate-owned static selected by the
        // constructor itself: "method mismatch" when the kinds differ …
        let e = NixAdapterError::unexpected_call(MethodKind::Version, MethodKind::Build);
        assert_eq!(e.expected_method(), Some(MethodKind::Version));
        assert_eq!(e.actual_method(), Some(MethodKind::Build));
        let summary = e.mismatch_summary().expect("mismatch present");
        assert_eq!(summary, "method mismatch");
        assert!(summary.len() <= BoundedSummary::MAX);
        // Display carries the method names and the static summary only.
        let msg = e.to_string();
        assert!(msg.contains("version"));
        assert!(msg.contains("build"));
        assert!(msg.contains("method mismatch"));

        // … and "request mismatch" when the kinds are equal.
        let e = NixAdapterError::unexpected_call(MethodKind::Substitute, MethodKind::Substitute);
        assert_eq!(e.expected_method(), Some(MethodKind::Substitute));
        assert_eq!(e.actual_method(), Some(MethodKind::Substitute));
        assert_eq!(e.mismatch_summary(), Some("request mismatch"));

        // The only two reachable summaries are these two static strings; no
        // external free text can ever appear (the constructor signature
        // accepts no text argument), so any injected text is impossible.
        for pair in [
            (MethodKind::Version, MethodKind::Build),
            (MethodKind::Substitute, MethodKind::Substitute),
        ] {
            let e = NixAdapterError::unexpected_call(pair.0, pair.1);
            let s = e.mismatch_summary().unwrap();
            assert!(s == "method mismatch" || s == "request mismatch");
        }
    }

    #[test]
    fn bounded_summary_truncates_at_char_boundary() {
        let s = BoundedSummary::new(&"a".repeat(BoundedSummary::MAX + 10));
        assert_eq!(s.as_str().len(), BoundedSummary::MAX);
        // Truncation lands on a char boundary for ASCII.
        // For multibyte, it backs up rather than splitting a code point.
        let mb = "é".repeat(BoundedSummary::MAX); // 2 bytes each
        let bounded = BoundedSummary::new(&mb);
        assert!(bounded.as_str().len() <= BoundedSummary::MAX);
        assert!(bounded.as_str().chars().all(|c| c == 'é'));
    }

    #[test]
    fn display_is_redacted_and_bounded() {
        let e = NixAdapterError::ValidationFailure {
            summary: BoundedSummary::new("invalid store path"),
        };
        let msg = e.to_string();
        assert!(msg.contains("invalid store path"));
        assert_eq!(e.code(), NixAdapterErrorCode::ValidationFailure);
    }

    #[test]
    fn unexpected_extra_call_represents_no_head_honestly() {
        // The no-head sibling: constructed from the actual method kind only.
        let e = NixAdapterError::unexpected_extra_call(MethodKind::Gc);
        // Reuses the single UnexpectedCall code.
        assert_eq!(e.code(), NixAdapterErrorCode::UnexpectedCall);
        // No expectation existed -> no honest expected method.
        assert_eq!(e.expected_method(), None);
        // The actual method that arrived is named.
        assert_eq!(e.actual_method(), Some(MethodKind::Gc));
        // The single crate-owned static summary.
        assert_eq!(e.mismatch_summary(), Some("extra call"));
        assert!(e.mismatch_summary().unwrap().len() <= BoundedSummary::MAX);

        // Display is bounded/redacted and truthfully says no expectation
        // remained, names the actual method, and carries the static summary.
        let msg = e.to_string();
        assert!(msg.contains("no expectation remained"), "msg={msg}");
        assert!(msg.contains("gc"), "actual method named: msg={msg}");
        assert!(msg.contains("extra call"), "summary present: msg={msg}");

        // Equal construction is value-equal; a different actual is not.
        assert_eq!(
            NixAdapterError::unexpected_extra_call(MethodKind::Gc),
            NixAdapterError::unexpected_extra_call(MethodKind::Gc)
        );
        assert_ne!(
            NixAdapterError::unexpected_extra_call(MethodKind::Gc),
            NixAdapterError::unexpected_extra_call(MethodKind::Build)
        );

        // It is distinct from the head-bearing UnexpectedCall even when actual
        // matches, because there is no expected and the summary differs.
        assert_ne!(
            NixAdapterError::unexpected_extra_call(MethodKind::Gc),
            NixAdapterError::unexpected_call(MethodKind::Version, MethodKind::Gc)
        );
    }

    #[test]
    fn unexpected_extra_call_works_for_every_method_kind() {
        for m in MethodKind::ALL {
            let e = NixAdapterError::unexpected_extra_call(m);
            assert_eq!(e.code(), NixAdapterErrorCode::UnexpectedCall);
            assert_eq!(e.expected_method(), None);
            assert_eq!(e.actual_method(), Some(m));
            assert_eq!(e.mismatch_summary(), Some("extra call"));
            let msg = e.to_string();
            assert!(msg.contains(m.as_str()));
            assert!(msg.contains("no expectation remained"));
        }
    }
}
