// Spike S3 (PR-7 / DR-003) — REPORT slice: the closed evidence model + its serde
// DTOs + a deterministic JSON/Markdown renderer.
//
// The report is the machine-readable + human-readable record of one spike run.
// It MUST be deterministic: given the same `Report` value, both the JSON form
// (via [`render_json`]) and the Markdown form (via [`render_markdown`]) are
// byte-identical. Field order is fixed by struct declaration order; lane order
// is fixed (fake, detect, preflight, buildProbe, signPlan); coverage rows follow
// their `Vec` order. No hashing or pointer identity participates in the output.
// The report carries NO hostname, user name, credential/profile value,
// free-form diagnostic, or timestamp.
//
// Classification and consistency invariants enforced by
// [`Report::validate`]. These classify and cross-check fields; they are NOT
// provenance attestation (see `EvidenceSource` and `Report::validate` below):
//   * `schemaVersion` is exactly 1.
//   * mode/harness consistency: `harnessOnly` is true iff `mode == Fake`. It is
//     the Fake fixture marker (Fake is the pure-harness lane), not a claim that
//     every other mode executes: `SignPlan` is `Designed`-only and never
//     executes, and `BuildProbe` cannot be selected by this spike CLI.
//   * the embedded pin summary is the exact canonical pin
//     ([`crate::validate::validate_pin`]).
//   * there are exactly five lanes, one per mode, each labeled with its own mode.
//   * the ACTIVE lane (whose mode equals `report.mode`) is Complete or
//     Incomplete (Fake may legitimately reach Complete via fixtures); every
//     inactive lane is Pending.
//   * each lane's `Lane<T>` state invariants hold (see [`Lane::validate`]).
//   * every observation carries an explicit `EvidenceSource`; a Fake lane may
//     only carry `Fixture` evidence, a SignPlan lane may only carry `Designed`
//     evidence, and every other lane (Detect/Preflight/BuildProbe) may only
//     carry `Observed` evidence. This is a CLASSIFICATION rule the validator
//     cross-checks, NOT provenance attestation: it ensures a lane's source
//     label is consistent with its mode, but it does NOT prove who produced
//     the report, that the runner/binary/host is genuine, or that values
//     labeled `Observed` were truly observed. The public builders
//     [`crate::runner::preflight_report`] / [`crate::runner::detect_report`]
//     accept any `dyn CommandRunner` / `dyn ProbeRunner`; a unit or custom
//     runner can return fabricated observations that still carry the `Observed`
//     label and still validate. Only the `s3-probe` CLI wires the built-in
//     [`crate::command::RealRunner`], so an injected-runner report — including
//     every unit-test report — is a simulation, never admissible evidence.
//     A real evidence judgment is a separate reviewed-process step (see
//     `FINDINGS.md`), not something `validate` establishes.
//   * a `SignPlan` observation, whenever present, always has `executed == false`
//     (this slice plans signing but never performs it).

use serde::{Deserialize, Serialize};

use crate::manifest::PinSummary;
use crate::validate::{ATTRS, DARWIN_SYSTEMS, PinError, validate_pin};

/// The one-and-only report schema revision this slice understands.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// The five lanes a spike run can be in. Each mode is also the identity of one
/// lane in [`Lanes`]; `report.mode` selects the ACTIVE lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Pure-harness lane: exercises the pipeline with fixtures, no network/Nix.
    Fake,
    /// Detect macOS host/tool/signing/build-user capabilities and optional Nix
    /// path existence; the optional Nix binary is never executed by Detect.
    Detect,
    /// Probe cache.nixos.org binary coverage for the pinned attrs/systems.
    Preflight,
    /// Attempt a real native sandboxed Darwin build.
    BuildProbe,
    /// Plan Apple signing/notarization (never executed in this slice).
    SignPlan,
}

/// How complete a lane's recorded data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaneState {
    /// Not exercised this run. Requires a [`PendingReason`].
    Pending,
    /// Attempted but not finished cleanly. Requires ≥1 failure.
    Incomplete,
    /// Finished with a recorded observation.
    Complete,
}

/// The closed set of reasons a lane may be [`LaneState::Pending`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingReason {
    /// Not the active mode this run (the natural state of inactive lanes).
    NotSelected,
    /// Needs real host access (Detect/BuildProbe) that was not available.
    RequiresHost,
    /// Needs network access (Preflight) that was not available.
    RequiresNetwork,
    /// Needs the signing/notarization toolchain (SignPlan) which was absent.
    RequiresSigning,
    /// Explicitly deferred to a later run.
    Deferred,
}

/// The evidence-classification label on every observation. Each mode admits
/// exactly one source: `Fake` → [`Fixture`], `SignPlan` → [`Designed`], and
/// `Detect`/`Preflight`/`BuildProbe` → [`Observed`].
///
/// This is a CLASSIFICATION, not an attestation. [`Report::validate`] only
/// checks that a lane's `source` label is consistent with its mode and that the
/// lane/state invariants hold. It does NOT authenticate the runner, the
/// `s3-probe` binary, or the host, does NOT verify that values labeled
/// `Observed` were genuinely observed, and does NOT make `report.json` /
/// `summary.md` trustworthy. The public [`crate::runner::preflight_report`] and
/// [`crate::runner::detect_report`] builders accept a `dyn CommandRunner` /
/// `dyn ProbeRunner`; a unit or custom runner can return fabricated observations
/// that still carry the `Observed` label and still pass validation. Only the
/// `s3-probe` CLI wires the built-in [`crate::command::RealRunner`]. An
/// injected-runner report is therefore a simulation, never admissible evidence,
/// even when its schema label is `Observed`. Treating a report as evidence is a
/// separate, reviewed-process judgment, not a property `validate` establishes;
/// see `FINDINGS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceSource {
    /// Synthetic fixture-driven value (Fake lane only).
    Fixture,
    /// Value classified as observed on the host/cache/toolchain
    /// (Detect/Preflight/BuildProbe lanes only). The label is declared by the
    /// report builder, not proven by the validator, so an injected-runner
    /// report labeled `Observed` is still only a simulation.
    Observed,
    /// A designed (planned, not executed) value (SignPlan lane only).
    Designed,
}

/// The pipeline stage a failure occurred in (closed enum; no free-form text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// Detect lane.
    Detect,
    /// Preflight lane.
    Preflight,
    /// BuildProbe lane.
    Build,
    /// SignPlan lane.
    Sign,
    /// Shared harness machinery.
    Harness,
}

/// The closed failure taxonomy. Carries NO raw child output, path, or message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// No Nix detected on the host.
    NixMissing,
    /// Detected Nix version did not match the pin.
    VersionMismatch,
    /// A cache query/protocol failure. A cache miss is an availability
    /// observation (a `false` `CoverageEntry`), not a harness failure.
    CacheQueryFailed,
    /// A native build did not succeed.
    BuildFailed,
    /// The build sandbox/build users were not ready.
    SandboxUnavailable,
    /// The signing/notarization toolchain was not available.
    SigningUnavailable,
    /// A phase exceeded its wall-clock budget.
    Timeout,
    /// The run was cancelled.
    Cancelled,
    /// An otherwise-uncategorized bounded failure.
    Unknown,
}

/// A recorded failure: enum `stage` + enum `kind` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Failure {
    pub stage: Stage,
    pub kind: FailureKind,
}

/// One row of the Preflight cache-coverage availability matrix. A row records
/// whether the output path is available and, independently, whether its closure
/// is available; `closureAvailable` may be true only when `outputAvailable` is
/// true. A `false` result is honest evidence of a miss, NOT a harness failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CoverageEntry {
    pub attr: String,
    pub system: String,
    pub output_available: bool,
    pub closure_available: bool,
}

/// What a SignPlan lane would sign (closed; carries no identity/profile value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SignTarget {
    /// The installer bundle.
    Installer,
    /// The runtime agent/binary.
    Runtime,
}

// ===== Lane errors (bounded Display) ========================================

/// A bounded, snippet-safe lane-validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneError {
    /// A lane's inner `mode` did not match its field position.
    ModeFieldMismatch { found: Mode, expected: Mode },
    /// A Pending lane lacked a reason.
    PendingRequiresReason,
    /// A Pending lane carried an observation.
    PendingForbidsObservation,
    /// A Pending lane carried failures.
    PendingForbidsFailures,
    /// An Incomplete lane carried a reason.
    IncompleteForbidsReason,
    /// An Incomplete lane recorded no failure.
    IncompleteRequiresFailure,
    /// A Complete lane carried a reason.
    CompleteForbidsReason,
    /// A Complete lane carried failures.
    CompleteForbidsFailures,
    /// A Complete lane lacked an observation.
    CompleteRequiresObservation,
    /// An observation's `source` did not match its lane's mode.
    EvidenceSourceMismatch {
        lane: Mode,
        found: EvidenceSource,
        expected: EvidenceSource,
    },
    /// A SignPlan observation had `executed == true` (never permitted).
    SignPlanExecutedMustBeFalse,
    /// A Fake observation's `fixturePairs` was out of range.
    FakeFixturePairsOutOfRange { got: u32 },
    /// A Preflight coverage row referenced an attribute outside the pin.
    PreflightCoverageAttrUnknown { got: String },
    /// A Preflight coverage row referenced a system outside the pin.
    PreflightCoverageSystemUnknown { got: String },
    /// A Detect observation's `hostSystem` was outside the pin.
    DetectHostSystemUnknown { got: String },
    /// A Detect observation's identity count was out of range (> 4096).
    DetectIdentityCountOutOfRange { got: u32 },
    /// A Detect observation's nixbld user count was out of range (> 4096).
    DetectNixbldUserCountOutOfRange { got: u32 },
    /// A BuildProbe observation's `builtSystem` was outside the pin.
    BuildSystemUnknown { got: String },
    /// A SignPlan observation named no targets.
    SignPlanTargetsEmpty,
    /// A Preflight coverage row had closure available but output unavailable.
    PreflightClosureWithoutOutput { attr: String, system: String },
    /// A Preflight coverage row repeated an (attr, system) pair.
    PreflightDuplicatePair { attr: String, system: String },
    /// Preflight coverage was not a prefix of the canonical system-major order.
    PreflightNonCanonicalOrder,
    /// A Complete Preflight had `nixVersionExact == false`.
    PreflightNixVersionNotExact,
    /// A Complete Preflight had `flakePrefetchVerified == false`.
    PreflightFlakePrefetchNotVerified,
    /// A Complete Preflight's coverage was not exactly the six canonical cells.
    PreflightCoverageNotCanonical,
    /// A Complete BuildProbe lacked a `builtSystem`.
    BuildSystemMissing,
    /// A Complete BuildProbe had `sandboxEnforced == false`.
    SandboxEnforcedFalse,
    /// A Complete BuildProbe had `sandboxFallbackDisabled == false`.
    SandboxFallbackDisabledFalse,
    /// A Complete BuildProbe had `buildUsersReady == false`.
    BuildUsersReadyFalse,
    /// A Complete BuildProbe had `networkDenied == false`.
    NetworkDeniedFalse,
    /// A Complete BuildProbe had `approvalRecorded == false`.
    ApprovalRecordedFalse,
    /// A SignPlan observation's targets were not exactly [Runtime, Installer].
    SignPlanTargetsNotRuntimeInstaller,
}

impl std::fmt::Display for LaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaneError::ModeFieldMismatch { found, expected } => {
                write!(f, "lane mode must be {expected:?}, found {found:?}")
            }
            LaneError::PendingRequiresReason => f.write_str("a Pending lane requires a reason"),
            LaneError::PendingForbidsObservation => {
                f.write_str("a Pending lane must carry no observation")
            }
            LaneError::PendingForbidsFailures => {
                f.write_str("a Pending lane must carry no failures")
            }
            LaneError::IncompleteForbidsReason => {
                f.write_str("an Incomplete lane must carry no reason")
            }
            LaneError::IncompleteRequiresFailure => {
                f.write_str("an Incomplete lane requires at least one failure")
            }
            LaneError::CompleteForbidsReason => f.write_str("a Complete lane must carry no reason"),
            LaneError::CompleteForbidsFailures => {
                f.write_str("a Complete lane must carry no failures")
            }
            LaneError::CompleteRequiresObservation => {
                f.write_str("a Complete lane requires an observation")
            }
            LaneError::EvidenceSourceMismatch {
                lane,
                found,
                expected,
            } => write!(
                f,
                "{lane:?} lane observation source must be {expected:?}, found {found:?}"
            ),
            LaneError::SignPlanExecutedMustBeFalse => {
                f.write_str("a SignPlan observation must have executed == false")
            }
            LaneError::FakeFixturePairsOutOfRange { got } => {
                write!(f, "fake fixturePairs must be 1..=4096, got {got}")
            }
            LaneError::PreflightCoverageAttrUnknown { got } => write!(
                f,
                "preflight coverage attr must be one of {ATTRS:?}, got {:?}",
                crate::validate::bound_snippet(got)
            ),
            LaneError::PreflightCoverageSystemUnknown { got } => write!(
                f,
                "preflight coverage system must be one of {DARWIN_SYSTEMS:?}, got {:?}",
                crate::validate::bound_snippet(got)
            ),
            LaneError::DetectHostSystemUnknown { got } => write!(
                f,
                "detect hostSystem must be one of {DARWIN_SYSTEMS:?}, got {:?}",
                crate::validate::bound_snippet(got)
            ),
            LaneError::DetectIdentityCountOutOfRange { got } => {
                write!(f, "detect identity count must be <= 4096, got {got}")
            }
            LaneError::DetectNixbldUserCountOutOfRange { got } => {
                write!(f, "detect nixbld user count must be <= 4096, got {got}")
            }
            LaneError::BuildSystemUnknown { got } => write!(
                f,
                "buildProbe builtSystem must be one of {DARWIN_SYSTEMS:?}, got {:?}",
                crate::validate::bound_snippet(got)
            ),
            LaneError::SignPlanTargetsEmpty => {
                f.write_str("a signPlan observation requires >=1 target")
            }
            LaneError::PreflightClosureWithoutOutput { attr, system } => write!(
                f,
                "preflight coverage closureAvailable requires outputAvailable for {:?}/{:?}",
                crate::validate::bound_snippet(attr),
                crate::validate::bound_snippet(system),
            ),
            LaneError::PreflightDuplicatePair { attr, system } => write!(
                f,
                "preflight coverage has a duplicate (attr,system) pair {:?}/{:?}",
                crate::validate::bound_snippet(attr),
                crate::validate::bound_snippet(system),
            ),
            LaneError::PreflightNonCanonicalOrder => f.write_str(
                "preflight coverage must be a prefix of the canonical system-major order",
            ),
            LaneError::PreflightNixVersionNotExact => f.write_str(
                "a complete preflight observation requires nixVersionExact == true",
            ),
            LaneError::PreflightFlakePrefetchNotVerified => f.write_str(
                "a complete preflight observation requires flakePrefetchVerified == true",
            ),
            LaneError::PreflightCoverageNotCanonical => f.write_str(
                "a complete preflight observation requires exactly the six canonical coverage cells",
            ),
            LaneError::BuildSystemMissing => {
                f.write_str("a complete buildProbe observation requires a builtSystem")
            }
            LaneError::SandboxEnforcedFalse => {
                f.write_str("a complete buildProbe observation requires sandboxEnforced == true")
            }
            LaneError::SandboxFallbackDisabledFalse => f.write_str(
                "a complete buildProbe observation requires sandboxFallbackDisabled == true",
            ),
            LaneError::BuildUsersReadyFalse => {
                f.write_str("a complete buildProbe observation requires buildUsersReady == true")
            }
            LaneError::NetworkDeniedFalse => {
                f.write_str("a complete buildProbe observation requires networkDenied == true")
            }
            LaneError::ApprovalRecordedFalse => {
                f.write_str("a complete buildProbe observation requires approvalRecorded == true")
            }
            LaneError::SignPlanTargetsNotRuntimeInstaller => f.write_str(
                "a signPlan observation's targets must be exactly [runtime, installer]",
            )
        }
    }
}

impl std::error::Error for LaneError {}

// ===== Observations =========================================================

/// Every observation exposes its honesty [`EvidenceSource`] and a bounded
/// content validation. The lane (which knows its mode) checks the source against
/// the mode; this trait checks only mode-agnostic observation content.
pub trait Observation {
    /// The explicit evidence source declared on this observation.
    fn evidence_source(&self) -> EvidenceSource;
    /// Validate this observation's own content (NOT its source — the lane does
    /// that, since it knows the mode). Called whenever an observation is
    /// present, regardless of lane state.
    fn validate(&self) -> Result<(), LaneError>;
    /// Validate the invariants that hold only when the carrying lane is
    /// [`LaneState::Complete`]. The default is a no-op so observations with no
    /// complete-specific requirement need not override it. [`Lane::validate`]
    /// calls this only for `Complete` lanes, so `Incomplete` lanes may retain
    /// valid partial observations while `Complete` enforces full evidence.
    fn validate_complete(&self) -> Result<(), LaneError> {
        Ok(())
    }
    /// Append this observation's detail rows to a deterministic Markdown dump.
    fn render_md(&self, out: &mut String);
}

/// Fake-lane evidence: the harness exercised `fixturePairs` (attr×system) pairs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FakeObservation {
    pub source: EvidenceSource,
    pub fixture_pairs: u32,
}

impl Observation for FakeObservation {
    fn evidence_source(&self) -> EvidenceSource {
        self.source
    }
    fn validate(&self) -> Result<(), LaneError> {
        const MAX_PAIRS: u32 = 4_096;
        if self.fixture_pairs == 0 || self.fixture_pairs > MAX_PAIRS {
            return Err(LaneError::FakeFixturePairsOutOfRange {
                got: self.fixture_pairs,
            });
        }
        Ok(())
    }
    fn render_md(&self, out: &mut String) {
        md_kv(
            out,
            "observation.fixturePairs",
            &self.fixture_pairs.to_string(),
        );
    }
}

/// The closed set of macOS signing/notarization/packaging tools S3 probes for.
/// Each flag is a filesystem-presence capability (the fixed absolute path
/// exists); NONE carries a tool PATH, version, or any identity value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolCapabilities {
    /// `/usr/bin/codesign` present.
    pub codesign: bool,
    /// `/usr/bin/xcrun` present.
    pub xcrun: bool,
    /// `notarytool` resolvable via `xcrun --find notarytool`.
    pub notarytool: bool,
    /// `/usr/bin/stapler` present.
    pub stapler: bool,
    /// `/usr/bin/productbuild` present.
    pub productbuild: bool,
    /// `/usr/bin/productsign` present.
    pub productsign: bool,
    /// `/usr/bin/pkgbuild` present.
    pub pkgbuild: bool,
    /// `/usr/sbin/spctl` present.
    pub spctl: bool,
    /// `/usr/bin/security` present.
    pub security: bool,
}

/// The closed classification of the active Xcode developer directory, derived
/// from `/usr/bin/xcode-select -p` WITHOUT storing the path. Carries no path,
/// version, or free-form text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum XcodeSelection {
    /// No Xcode and no Command Line Tools selected/installed.
    Absent,
    /// Command Line Tools only (`/Library/Developer/CommandLineTools`).
    CommandLineTools,
    /// Full Xcode selected (`/Applications/Xcode.app`).
    FullXcode,
}

/// Maximum value accepted for any identity / user COUNT on a Detect observation.
/// Keeps counts honest and bounded; a value above this is a malformed capture.
pub const MAX_IDENTITY_COUNT: u32 = 4_096;

/// Detect-lane evidence: host Nix presence, detected host system (optional),
/// the macOS signing/notarization tool capabilities, the active Xcode
/// classification, the Developer ID identity COUNTS (application/installer),
/// and the `_nixbld` build-user group presence/member count.
///
/// Carries NO identity name, tool path, hostname, username, keychain profile,
/// timestamp, or raw tool output — only booleans, the closed Xcode enum, and
/// bounded counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DetectObservation {
    pub source: EvidenceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_system: Option<String>,
    pub nix_present: bool,
    pub tool_capabilities: ToolCapabilities,
    pub xcode_selection: XcodeSelection,
    /// Number of "Developer ID Application" identities (from
    /// `security find-identity -v -p codesigning`). A COUNT only.
    pub application_identity_count: u16,
    /// Number of "Developer ID Installer" identities (from
    /// `security find-identity -v`). A COUNT only.
    pub installer_identity_count: u16,
    /// `_nixbld` build-user group present (from `dscl . -read /Groups/_nixbld`).
    pub nixbld_group_present: bool,
    /// Number of `_nixbld` group members. A COUNT only.
    pub nixbld_user_count: u16,
}

impl Observation for DetectObservation {
    fn evidence_source(&self) -> EvidenceSource {
        self.source
    }
    fn validate(&self) -> Result<(), LaneError> {
        if let Some(sys) = &self.host_system
            && !DARWIN_SYSTEMS.contains(&sys.as_str())
        {
            return Err(LaneError::DetectHostSystemUnknown { got: sys.clone() });
        }
        if self.application_identity_count as u32 > MAX_IDENTITY_COUNT {
            return Err(LaneError::DetectIdentityCountOutOfRange {
                got: self.application_identity_count as u32,
            });
        }
        if self.installer_identity_count as u32 > MAX_IDENTITY_COUNT {
            return Err(LaneError::DetectIdentityCountOutOfRange {
                got: self.installer_identity_count as u32,
            });
        }
        if self.nixbld_user_count as u32 > MAX_IDENTITY_COUNT {
            return Err(LaneError::DetectNixbldUserCountOutOfRange {
                got: self.nixbld_user_count as u32,
            });
        }
        Ok(())
    }
    fn render_md(&self, out: &mut String) {
        md_kv(out, "observation.nixPresent", bool_str(self.nix_present));
        md_kv(
            out,
            "observation.hostSystem",
            self.host_system.as_deref().unwrap_or("—"),
        );
        md_kv(
            out,
            "observation.xcodeSelection",
            xcode_selection_str(self.xcode_selection),
        );
        md_kv(
            out,
            "observation.applicationIdentityCount",
            &self.application_identity_count.to_string(),
        );
        md_kv(
            out,
            "observation.installerIdentityCount",
            &self.installer_identity_count.to_string(),
        );
        md_kv(
            out,
            "observation.nixbldGroupPresent",
            bool_str(self.nixbld_group_present),
        );
        md_kv(
            out,
            "observation.nixbldUserCount",
            &self.nixbld_user_count.to_string(),
        );
        let tc = &self.tool_capabilities;
        md_kv(out, "observation.tools.codesign", bool_str(tc.codesign));
        md_kv(out, "observation.tools.xcrun", bool_str(tc.xcrun));
        md_kv(out, "observation.tools.notarytool", bool_str(tc.notarytool));
        md_kv(out, "observation.tools.stapler", bool_str(tc.stapler));
        md_kv(
            out,
            "observation.tools.productbuild",
            bool_str(tc.productbuild),
        );
        md_kv(
            out,
            "observation.tools.productsign",
            bool_str(tc.productsign),
        );
        md_kv(out, "observation.tools.pkgbuild", bool_str(tc.pkgbuild));
        md_kv(out, "observation.tools.spctl", bool_str(tc.spctl));
        md_kv(out, "observation.tools.security", bool_str(tc.security));
    }
}

/// Preflight-lane evidence: the cache-coverage availability matrix plus the
/// two host/flake checks that gate whether the matrix is even meaningful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PreflightObservation {
    pub source: EvidenceSource,
    /// Detected Nix version matched the pin exactly.
    pub nix_version_exact: bool,
    /// The pinned flake prefetched (NAR hash verified).
    pub flake_prefetch_verified: bool,
    pub coverage: Vec<CoverageEntry>,
}

/// The six canonical Preflight coverage cells in system-major order: for each
/// Darwin system (x86_64-darwin then aarch64-darwin), each attr
/// (hello/ripgrep/git). Returned as `(system, attr)` pairs. A Complete
/// preflight observation must carry exactly these cells in exactly this order;
/// a partial (Incomplete) observation must be a prefix of this sequence.
fn canonical_coverage_cells() -> [(&'static str, &'static str); 6] {
    [
        (DARWIN_SYSTEMS[0], ATTRS[0]),
        (DARWIN_SYSTEMS[0], ATTRS[1]),
        (DARWIN_SYSTEMS[0], ATTRS[2]),
        (DARWIN_SYSTEMS[1], ATTRS[0]),
        (DARWIN_SYSTEMS[1], ATTRS[1]),
        (DARWIN_SYSTEMS[1], ATTRS[2]),
    ]
}

impl Observation for PreflightObservation {
    fn evidence_source(&self) -> EvidenceSource {
        self.source
    }
    fn validate(&self) -> Result<(), LaneError> {
        // NOTE: an EMPTY coverage vec is VALID here. An Incomplete Preflight may
        // have failed before ANY canonical cell completed (version/prefetch/
        // store-info gate, or on the very first cell), so the partial
        // observation legitimately carries zero rows. The Complete-only
        // requirement (exactly six canonical cells) is enforced in
        // [`Self::validate_complete`]. A non-empty coverage must still be a
        // canonical system-major prefix (checked below).
        let canonical = canonical_coverage_cells();
        let mut seen: Vec<(&str, &str)> = Vec::with_capacity(self.coverage.len());
        for (i, entry) in self.coverage.iter().enumerate() {
            if !ATTRS.contains(&entry.attr.as_str()) {
                return Err(LaneError::PreflightCoverageAttrUnknown {
                    got: entry.attr.clone(),
                });
            }
            if !DARWIN_SYSTEMS.contains(&entry.system.as_str()) {
                return Err(LaneError::PreflightCoverageSystemUnknown {
                    got: entry.system.clone(),
                });
            }
            if entry.closure_available && !entry.output_available {
                return Err(LaneError::PreflightClosureWithoutOutput {
                    attr: entry.attr.clone(),
                    system: entry.system.clone(),
                });
            }
            let pair = (entry.attr.as_str(), entry.system.as_str());
            if seen.contains(&pair) {
                return Err(LaneError::PreflightDuplicatePair {
                    attr: entry.attr.clone(),
                    system: entry.system.clone(),
                });
            }
            // Canonical prefix ordering: position i must equal canonical[i]
            // (system, attr). A row past the end or a cell out of place is
            // non-canonical. Partial Incomplete rows are a prefix of this.
            match canonical.get(i) {
                Some(&(sys, attr)) if sys == pair.1 && attr == pair.0 => {}
                _ => return Err(LaneError::PreflightNonCanonicalOrder),
            }
            seen.push(pair);
        }
        Ok(())
    }
    fn validate_complete(&self) -> Result<(), LaneError> {
        if !self.nix_version_exact {
            return Err(LaneError::PreflightNixVersionNotExact);
        }
        if !self.flake_prefetch_verified {
            return Err(LaneError::PreflightFlakePrefetchNotVerified);
        }
        let canonical = canonical_coverage_cells();
        if self.coverage.len() != canonical.len() {
            return Err(LaneError::PreflightCoverageNotCanonical);
        }
        for (entry, &(sys, attr)) in self.coverage.iter().zip(canonical.iter()) {
            if entry.system.as_str() != sys || entry.attr.as_str() != attr {
                return Err(LaneError::PreflightCoverageNotCanonical);
            }
        }
        Ok(())
    }
    fn render_md(&self, out: &mut String) {
        md_kv(
            out,
            "observation.nixVersionExact",
            bool_str(self.nix_version_exact),
        );
        md_kv(
            out,
            "observation.flakePrefetchVerified",
            bool_str(self.flake_prefetch_verified),
        );
        md_kv(
            out,
            "observation.coverageRows",
            &self.coverage.len().to_string(),
        );
        out.push_str("\n| attr | system | output | closure |\n|---|---|---|---|\n");
        for entry in &self.coverage {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                escape_md(&entry.attr),
                escape_md(&entry.system),
                bool_str(entry.output_available),
                bool_str(entry.closure_available),
            ));
        }
    }
}

/// BuildProbe-lane evidence: the native system built for plus the closed set
/// of sandbox/build-user/network/approval invariants. `Incomplete` lanes may
/// carry partial (false) values alongside a failure; `Complete` requires a
/// present `builtSystem` and every invariant true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BuildProbeObservation {
    pub source: EvidenceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built_system: Option<String>,
    pub sandbox_enforced: bool,
    pub sandbox_fallback_disabled: bool,
    pub build_users_ready: bool,
    pub network_denied: bool,
    pub approval_recorded: bool,
}

impl Observation for BuildProbeObservation {
    fn evidence_source(&self) -> EvidenceSource {
        self.source
    }
    fn validate(&self) -> Result<(), LaneError> {
        if let Some(sys) = &self.built_system
            && !DARWIN_SYSTEMS.contains(&sys.as_str())
        {
            return Err(LaneError::BuildSystemUnknown { got: sys.clone() });
        }
        Ok(())
    }
    fn validate_complete(&self) -> Result<(), LaneError> {
        if self.built_system.is_none() {
            return Err(LaneError::BuildSystemMissing);
        }
        if !self.sandbox_enforced {
            return Err(LaneError::SandboxEnforcedFalse);
        }
        if !self.sandbox_fallback_disabled {
            return Err(LaneError::SandboxFallbackDisabledFalse);
        }
        if !self.build_users_ready {
            return Err(LaneError::BuildUsersReadyFalse);
        }
        if !self.network_denied {
            return Err(LaneError::NetworkDeniedFalse);
        }
        if !self.approval_recorded {
            return Err(LaneError::ApprovalRecordedFalse);
        }
        Ok(())
    }
    fn render_md(&self, out: &mut String) {
        md_kv(
            out,
            "observation.builtSystem",
            self.built_system.as_deref().unwrap_or("—"),
        );
        md_kv(
            out,
            "observation.sandboxEnforced",
            bool_str(self.sandbox_enforced),
        );
        md_kv(
            out,
            "observation.sandboxFallbackDisabled",
            bool_str(self.sandbox_fallback_disabled),
        );
        md_kv(
            out,
            "observation.buildUsersReady",
            bool_str(self.build_users_ready),
        );
        md_kv(
            out,
            "observation.networkDenied",
            bool_str(self.network_denied),
        );
        md_kv(
            out,
            "observation.approvalRecorded",
            bool_str(self.approval_recorded),
        );
    }
}

/// SignPlan-lane evidence: the signing plan. Carries NO identity/profile value.
/// `executed` must always be false in this slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignPlanObservation {
    pub source: EvidenceSource,
    pub executed: bool,
    pub targets: Vec<SignTarget>,
}

impl Observation for SignPlanObservation {
    fn evidence_source(&self) -> EvidenceSource {
        self.source
    }
    fn validate(&self) -> Result<(), LaneError> {
        if self.executed {
            return Err(LaneError::SignPlanExecutedMustBeFalse);
        }
        if self.targets.is_empty() {
            return Err(LaneError::SignPlanTargetsEmpty);
        }
        if self.targets != [SignTarget::Runtime, SignTarget::Installer] {
            return Err(LaneError::SignPlanTargetsNotRuntimeInstaller);
        }
        Ok(())
    }
    fn render_md(&self, out: &mut String) {
        md_kv(out, "observation.executed", bool_str(self.executed));
        let mut joined = String::new();
        for (i, t) in self.targets.iter().enumerate() {
            if i > 0 {
                joined.push_str(", ");
            }
            joined.push_str(sign_target_str(*t));
        }
        md_kv(out, "observation.targets", &joined);
    }
}

// ===== Lane<T> ==============================================================

/// One lane of the report, generic over its observation type `T`. The lane
/// carries its own `mode` (which must match its field position in [`Lanes`]),
/// a [`LaneState`], an optional `reason` (Pending only), an optional
/// observation (Complete requires it), and a list of failures (Incomplete
/// requires ≥1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lane<T> {
    pub mode: Mode,
    pub state: LaneState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<PendingReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<T>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<Failure>,
}

impl<T: Observation> Lane<T> {
    /// Validate this lane's state invariants, mode label, observation content,
    /// and the observation's `EvidenceSource` against the lane's mode. Returns
    /// `Ok(())` if every invariant holds.
    pub fn validate(&self, expected_mode: Mode) -> Result<(), LaneError> {
        if self.mode != expected_mode {
            return Err(LaneError::ModeFieldMismatch {
                found: self.mode,
                expected: expected_mode,
            });
        }
        match self.state {
            LaneState::Pending => {
                if self.reason.is_none() {
                    return Err(LaneError::PendingRequiresReason);
                }
                if self.observation.is_some() {
                    return Err(LaneError::PendingForbidsObservation);
                }
                if !self.failures.is_empty() {
                    return Err(LaneError::PendingForbidsFailures);
                }
            }
            LaneState::Incomplete => {
                if self.reason.is_some() {
                    return Err(LaneError::IncompleteForbidsReason);
                }
                if self.failures.is_empty() {
                    return Err(LaneError::IncompleteRequiresFailure);
                }
                // observation optional
            }
            LaneState::Complete => {
                if self.reason.is_some() {
                    return Err(LaneError::CompleteForbidsReason);
                }
                if !self.failures.is_empty() {
                    return Err(LaneError::CompleteForbidsFailures);
                }
                if self.observation.is_none() {
                    return Err(LaneError::CompleteRequiresObservation);
                }
            }
        }
        if let Some(obs) = &self.observation {
            obs.validate()?;
            if self.state == LaneState::Complete {
                obs.validate_complete()?;
            }
            let found = obs.evidence_source();
            let expected = match self.mode {
                Mode::Fake => EvidenceSource::Fixture,
                Mode::SignPlan => EvidenceSource::Designed,
                _ => EvidenceSource::Observed,
            };
            if found != expected {
                return Err(LaneError::EvidenceSourceMismatch {
                    lane: self.mode,
                    found,
                    expected,
                });
            }
        }
        Ok(())
    }

    /// Append this lane's common metadata rows + observation detail to a
    /// deterministic Markdown dump.
    fn render_md(&self, out: &mut String, name: &str) {
        out.push_str(&format!("## Lane {name}\n\n| field | value |\n|---|---|\n"));
        md_kv(out, "mode", mode_str(self.mode));
        md_kv(out, "state", lane_state_str(self.state));
        md_kv(out, "reason", self.reason.map(reason_str).unwrap_or("—"));
        match &self.observation {
            Some(obs) => {
                md_kv(
                    out,
                    "observation.source",
                    evidence_source_str(obs.evidence_source()),
                );
                obs.render_md(out);
            }
            None => md_kv(out, "observation", "—"),
        }
        md_kv(out, "failures", &self.failures.len().to_string());
        if !self.failures.is_empty() {
            out.push_str("\n| stage | kind |\n|---|---|\n");
            for f in &self.failures {
                out.push_str(&format!(
                    "| {} | {} |\n",
                    stage_str(f.stage),
                    failure_kind_str(f.kind),
                ));
            }
        }
        out.push('\n');
    }
}

// ===== Lanes + Report =======================================================

/// The five lanes, one per mode, in fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lanes {
    pub fake: Lane<FakeObservation>,
    pub detect: Lane<DetectObservation>,
    pub preflight: Lane<PreflightObservation>,
    pub build_probe: Lane<BuildProbeObservation>,
    pub sign_plan: Lane<SignPlanObservation>,
}

impl Lanes {
    /// Validate every lane (invariants + mode label + source) and the
    /// active/inactive state rules relative to `active`.
    pub fn validate(&self, active: Mode) -> Result<(), ReportError> {
        self.fake
            .validate(Mode::Fake)
            .map_err(|e| ReportError::Lane {
                lane: Mode::Fake,
                error: e,
            })?;
        self.detect
            .validate(Mode::Detect)
            .map_err(|e| ReportError::Lane {
                lane: Mode::Detect,
                error: e,
            })?;
        self.preflight
            .validate(Mode::Preflight)
            .map_err(|e| ReportError::Lane {
                lane: Mode::Preflight,
                error: e,
            })?;
        self.build_probe
            .validate(Mode::BuildProbe)
            .map_err(|e| ReportError::Lane {
                lane: Mode::BuildProbe,
                error: e,
            })?;
        self.sign_plan
            .validate(Mode::SignPlan)
            .map_err(|e| ReportError::Lane {
                lane: Mode::SignPlan,
                error: e,
            })?;

        for (lane, state) in self.states() {
            if lane == active {
                if !matches!(state, LaneState::Complete | LaneState::Incomplete) {
                    return Err(ReportError::ActiveLaneMustBeCompleteOrIncomplete {
                        lane,
                        found: state,
                    });
                }
            } else if state != LaneState::Pending {
                return Err(ReportError::InactiveLaneMustBePending { lane, found: state });
            }
        }
        Ok(())
    }

    fn states(&self) -> [(Mode, LaneState); 5] {
        [
            (Mode::Fake, self.fake.state),
            (Mode::Detect, self.detect.state),
            (Mode::Preflight, self.preflight.state),
            (Mode::BuildProbe, self.build_probe.state),
            (Mode::SignPlan, self.sign_plan.state),
        ]
    }
}

/// The top-level spike run report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub mode: Mode,
    pub harness_only: bool,
    pub pin: PinSummary,
    pub lanes: Lanes,
}

/// Error returned by [`Report::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// `schemaVersion` did not match [`REPORT_SCHEMA_VERSION`].
    SchemaVersion { found: u32, expected: u32 },
    /// `harnessOnly` was inconsistent with `mode` (must equal `mode == Fake`).
    HarnessInconsistent { mode: Mode, harness_only: bool },
    /// The embedded pin summary failed validation.
    Pin(PinError),
    /// The active lane was neither Complete nor Incomplete.
    ActiveLaneMustBeCompleteOrIncomplete { lane: Mode, found: LaneState },
    /// An inactive lane was not Pending.
    InactiveLaneMustBePending { lane: Mode, found: LaneState },
    /// A lane failed its own validation.
    Lane { lane: Mode, error: LaneError },
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportError::SchemaVersion { found, expected } => {
                write!(f, "schemaVersion must be {expected}, got {found}")
            }
            ReportError::HarnessInconsistent { mode, harness_only } => write!(
                f,
                "harnessOnly must equal (mode == Fake) for mode {mode:?}, got harnessOnly={harness_only}"
            ),
            ReportError::Pin(e) => write!(f, "pin invalid: {e}"),
            ReportError::ActiveLaneMustBeCompleteOrIncomplete { lane, found } => write!(
                f,
                "active lane {lane:?} must be Complete or Incomplete, found {found:?}"
            ),
            ReportError::InactiveLaneMustBePending { lane, found } => {
                write!(f, "inactive lane {lane:?} must be Pending, found {found:?}")
            }
            ReportError::Lane { lane, error } => {
                write!(f, "lane {lane:?} invalid: {error}")
            }
        }
    }
}

impl std::error::Error for ReportError {}

impl Report {
    /// Validate the report's classification and lane/state consistency
    /// invariants (see the module docs). These are NOT provenance attestation;
    /// a valid report may still carry fabricated values (see `EvidenceSource`).
    pub fn validate(&self) -> Result<(), ReportError> {
        if self.schema_version != REPORT_SCHEMA_VERSION {
            return Err(ReportError::SchemaVersion {
                found: self.schema_version,
                expected: REPORT_SCHEMA_VERSION,
            });
        }
        let want_harness = self.mode == Mode::Fake;
        if self.harness_only != want_harness {
            return Err(ReportError::HarnessInconsistent {
                mode: self.mode,
                harness_only: self.harness_only,
            });
        }
        validate_pin(&self.pin).map_err(ReportError::Pin)?;
        self.lanes.validate(self.mode)?;
        Ok(())
    }
}

// ===== Deterministic rendering ==============================================

/// Render `report` as deterministic pretty JSON with a trailing newline.
#[must_use]
pub fn render_json(report: &Report) -> String {
    let mut s = serde_json::to_string_pretty(report).expect("report serializes");
    s.push('\n');
    s
}

/// Render `report` as a deterministic Markdown document.
#[must_use]
pub fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# S3 macOS spike report\n\n");
    out.push_str("| field | value |\n|---|---|\n");
    md_kv(
        &mut out,
        "schemaVersion",
        &report.schema_version.to_string(),
    );
    md_kv(&mut out, "mode", mode_str(report.mode));
    md_kv(&mut out, "harnessOnly", bool_str(report.harness_only));
    md_kv(&mut out, "pin.nix.version", &report.pin.nix.version);
    md_kv(&mut out, "pin.nixpkgs.owner", &report.pin.nixpkgs.owner);
    md_kv(&mut out, "pin.nixpkgs.repo", &report.pin.nixpkgs.repo);
    md_kv(&mut out, "pin.nixpkgs.rev", &report.pin.nixpkgs.rev);
    md_kv(
        &mut out,
        "pin.nixpkgs.narHash",
        &report.pin.nixpkgs.nar_hash,
    );
    md_kv(&mut out, "pin.systems", &report.pin.systems.join(", "));
    md_kv(&mut out, "pin.attrs", &report.pin.attrs.join(", "));
    md_kv(&mut out, "pin.cacheStoreUrl", &report.pin.cache_store_url);
    out.push('\n');

    report.lanes.fake.render_md(&mut out, "fake");
    report.lanes.detect.render_md(&mut out, "detect");
    report.lanes.preflight.render_md(&mut out, "preflight");
    report.lanes.build_probe.render_md(&mut out, "buildProbe");
    report.lanes.sign_plan.render_md(&mut out, "signPlan");

    out
}

/// Append `| key | value |` row, escaping both cells.
fn md_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(&format!("| {} | {} |\n", escape_md(key), escape_md(value)));
}

/// Escape a caller-controlled string for a Markdown table cell: escape the
/// table delimiter `|`, flatten newlines to a single space (which also prevents
/// a cell from injecting a new block), and drop CR.
fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' => out.push(' '),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Fake => "fake",
        Mode::Detect => "detect",
        Mode::Preflight => "preflight",
        Mode::BuildProbe => "buildProbe",
        Mode::SignPlan => "signPlan",
    }
}

fn lane_state_str(s: LaneState) -> &'static str {
    match s {
        LaneState::Pending => "pending",
        LaneState::Incomplete => "incomplete",
        LaneState::Complete => "complete",
    }
}

fn reason_str(r: PendingReason) -> &'static str {
    match r {
        PendingReason::NotSelected => "notSelected",
        PendingReason::RequiresHost => "requiresHost",
        PendingReason::RequiresNetwork => "requiresNetwork",
        PendingReason::RequiresSigning => "requiresSigning",
        PendingReason::Deferred => "deferred",
    }
}

fn evidence_source_str(s: EvidenceSource) -> &'static str {
    match s {
        EvidenceSource::Fixture => "fixture",
        EvidenceSource::Observed => "observed",
        EvidenceSource::Designed => "designed",
    }
}

fn stage_str(s: Stage) -> &'static str {
    match s {
        Stage::Detect => "detect",
        Stage::Preflight => "preflight",
        Stage::Build => "build",
        Stage::Sign => "sign",
        Stage::Harness => "harness",
    }
}

fn failure_kind_str(k: FailureKind) -> &'static str {
    match k {
        FailureKind::NixMissing => "nixMissing",
        FailureKind::VersionMismatch => "versionMismatch",
        FailureKind::CacheQueryFailed => "cacheQueryFailed",
        FailureKind::BuildFailed => "buildFailed",
        FailureKind::SandboxUnavailable => "sandboxUnavailable",
        FailureKind::SigningUnavailable => "signingUnavailable",
        FailureKind::Timeout => "timeout",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Unknown => "unknown",
    }
}

fn sign_target_str(t: SignTarget) -> &'static str {
    match t {
        SignTarget::Installer => "installer",
        SignTarget::Runtime => "runtime",
    }
}

fn xcode_selection_str(x: XcodeSelection) -> &'static str {
    match x {
        XcodeSelection::Absent => "absent",
        XcodeSelection::CommandLineTools => "commandLineTools",
        XcodeSelection::FullXcode => "fullXcode",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;
    use serde_json::{Value, json};

    // ---- test builders -----------------------------------------------------

    fn pin() -> PinSummary {
        manifest::pin_summary().clone()
    }

    fn pending_fake() -> Lane<FakeObservation> {
        Lane {
            mode: Mode::Fake,
            state: LaneState::Pending,
            reason: Some(PendingReason::NotSelected),
            observation: None,
            failures: vec![],
        }
    }

    fn pending_detect() -> Lane<DetectObservation> {
        Lane {
            mode: Mode::Detect,
            state: LaneState::Pending,
            reason: Some(PendingReason::NotSelected),
            observation: None,
            failures: vec![],
        }
    }

    fn pending_preflight() -> Lane<PreflightObservation> {
        Lane {
            mode: Mode::Preflight,
            state: LaneState::Pending,
            reason: Some(PendingReason::NotSelected),
            observation: None,
            failures: vec![],
        }
    }

    fn pending_build_probe() -> Lane<BuildProbeObservation> {
        Lane {
            mode: Mode::BuildProbe,
            state: LaneState::Pending,
            reason: Some(PendingReason::NotSelected),
            observation: None,
            failures: vec![],
        }
    }

    fn pending_sign_plan() -> Lane<SignPlanObservation> {
        Lane {
            mode: Mode::SignPlan,
            state: LaneState::Pending,
            reason: Some(PendingReason::NotSelected),
            observation: None,
            failures: vec![],
        }
    }

    fn complete_fake() -> Lane<FakeObservation> {
        Lane {
            mode: Mode::Fake,
            state: LaneState::Complete,
            reason: None,
            observation: Some(FakeObservation {
                source: EvidenceSource::Fixture,
                fixture_pairs: 6,
            }),
            failures: vec![],
        }
    }

    fn complete_detect() -> Lane<DetectObservation> {
        Lane {
            mode: Mode::Detect,
            state: LaneState::Complete,
            reason: None,
            observation: Some(DetectObservation {
                source: EvidenceSource::Observed,
                host_system: Some("aarch64-darwin".to_string()),
                nix_present: true,
                tool_capabilities: ToolCapabilities {
                    codesign: true,
                    xcrun: true,
                    notarytool: true,
                    stapler: true,
                    productbuild: true,
                    productsign: true,
                    pkgbuild: true,
                    spctl: true,
                    security: true,
                },
                xcode_selection: XcodeSelection::FullXcode,
                application_identity_count: 1,
                installer_identity_count: 1,
                nixbld_group_present: true,
                nixbld_user_count: 32,
            }),
            failures: vec![],
        }
    }

    fn full_coverage() -> Vec<CoverageEntry> {
        // Canonical system-major order: x86_64-darwin then aarch64-darwin,
        // each hello/ripgrep/git. Availability fully true for the happy path.
        let mut rows = Vec::new();
        for sys in DARWIN_SYSTEMS {
            for attr in ATTRS {
                rows.push(CoverageEntry {
                    attr: (*attr).to_string(),
                    system: (*sys).to_string(),
                    output_available: true,
                    closure_available: true,
                });
            }
        }
        rows
    }

    fn complete_preflight() -> Lane<PreflightObservation> {
        Lane {
            mode: Mode::Preflight,
            state: LaneState::Complete,
            reason: None,
            observation: Some(PreflightObservation {
                source: EvidenceSource::Observed,
                nix_version_exact: true,
                flake_prefetch_verified: true,
                coverage: full_coverage(),
            }),
            failures: vec![],
        }
    }

    fn complete_build_probe() -> Lane<BuildProbeObservation> {
        Lane {
            mode: Mode::BuildProbe,
            state: LaneState::Complete,
            reason: None,
            observation: Some(BuildProbeObservation {
                source: EvidenceSource::Observed,
                built_system: Some("aarch64-darwin".to_string()),
                sandbox_enforced: true,
                sandbox_fallback_disabled: true,
                build_users_ready: true,
                network_denied: true,
                approval_recorded: true,
            }),
            failures: vec![],
        }
    }

    fn complete_sign_plan() -> Lane<SignPlanObservation> {
        Lane {
            mode: Mode::SignPlan,
            state: LaneState::Complete,
            reason: None,
            observation: Some(SignPlanObservation {
                source: EvidenceSource::Designed,
                executed: false,
                targets: vec![SignTarget::Runtime, SignTarget::Installer],
            }),
            failures: vec![],
        }
    }

    /// A valid Complete report for `active`: the active lane Complete, all
    /// others Pending, harnessOnly consistent with the mode.
    fn valid_report(active: Mode) -> Report {
        let (fake, detect, preflight, build_probe, sign_plan) = match active {
            Mode::Fake => (
                complete_fake(),
                pending_detect(),
                pending_preflight(),
                pending_build_probe(),
                pending_sign_plan(),
            ),
            Mode::Detect => (
                pending_fake(),
                complete_detect(),
                pending_preflight(),
                pending_build_probe(),
                pending_sign_plan(),
            ),
            Mode::Preflight => (
                pending_fake(),
                pending_detect(),
                complete_preflight(),
                pending_build_probe(),
                pending_sign_plan(),
            ),
            Mode::BuildProbe => (
                pending_fake(),
                pending_detect(),
                pending_preflight(),
                complete_build_probe(),
                pending_sign_plan(),
            ),
            Mode::SignPlan => (
                pending_fake(),
                pending_detect(),
                pending_preflight(),
                pending_build_probe(),
                complete_sign_plan(),
            ),
        };
        Report {
            schema_version: REPORT_SCHEMA_VERSION,
            mode: active,
            harness_only: active == Mode::Fake,
            pin: pin(),
            lanes: Lanes {
                fake,
                detect,
                preflight,
                build_probe,
                sign_plan,
            },
        }
    }

    // ---- happy paths: every mode validates with an active Complete lane ----
    #[test]
    fn validates_fake_complete() {
        valid_report(Mode::Fake).validate().unwrap();
    }
    #[test]
    fn validates_detect_complete() {
        valid_report(Mode::Detect).validate().unwrap();
    }
    #[test]
    fn validates_preflight_complete() {
        valid_report(Mode::Preflight).validate().unwrap();
    }
    #[test]
    fn validates_build_probe_complete() {
        valid_report(Mode::BuildProbe).validate().unwrap();
    }
    #[test]
    fn validates_sign_plan_complete() {
        valid_report(Mode::SignPlan).validate().unwrap();
    }

    // ---- active lane may be Incomplete (with a failure) --------------------
    #[test]
    fn validates_active_incomplete_with_failure() {
        for active in [
            Mode::Fake,
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let mut r = valid_report(active);
            set_active_incomplete(&mut r, active);
            r.validate().unwrap();
        }
    }

    /// Force the active lane into an Incomplete state with one failure and no
    /// observation (an honest "attempted but failed" record).
    fn set_active_incomplete(r: &mut Report, active: Mode) {
        let fail = Failure {
            stage: stage_for(active),
            kind: FailureKind::Cancelled,
        };
        match active {
            Mode::Fake => {
                r.lanes.fake.state = LaneState::Incomplete;
                r.lanes.fake.reason = None;
                r.lanes.fake.observation = None;
                r.lanes.fake.failures = vec![fail];
            }
            Mode::Detect => {
                r.lanes.detect.state = LaneState::Incomplete;
                r.lanes.detect.reason = None;
                r.lanes.detect.observation = None;
                r.lanes.detect.failures = vec![fail];
            }
            Mode::Preflight => {
                r.lanes.preflight.state = LaneState::Incomplete;
                r.lanes.preflight.reason = None;
                r.lanes.preflight.observation = None;
                r.lanes.preflight.failures = vec![fail];
            }
            Mode::BuildProbe => {
                r.lanes.build_probe.state = LaneState::Incomplete;
                r.lanes.build_probe.reason = None;
                r.lanes.build_probe.observation = None;
                r.lanes.build_probe.failures = vec![fail];
            }
            Mode::SignPlan => {
                r.lanes.sign_plan.state = LaneState::Incomplete;
                r.lanes.sign_plan.reason = None;
                r.lanes.sign_plan.observation = None;
                r.lanes.sign_plan.failures = vec![fail];
            }
        }
    }

    fn stage_for(m: Mode) -> Stage {
        match m {
            Mode::Detect => Stage::Detect,
            Mode::Preflight => Stage::Preflight,
            Mode::BuildProbe => Stage::Build,
            Mode::SignPlan => Stage::Sign,
            Mode::Fake => Stage::Harness,
        }
    }

    // ---- active lane must NOT be Pending -----------------------------------
    #[test]
    fn active_lane_pending_is_rejected_for_every_mode() {
        for active in [
            Mode::Fake,
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let mut r = valid_report(active);
            // Force the active lane to Pending with a reason; this violates the
            // active-must-be-attempted rule.
            force_pending(&mut r, active);
            let err = r.validate().unwrap_err();
            assert!(
                matches!(
                    err,
                    ReportError::ActiveLaneMustBeCompleteOrIncomplete { lane, .. } if lane == active
                ),
                "mode {active:?}: {err:?}"
            );
        }
    }

    fn force_pending(r: &mut Report, active: Mode) {
        match active {
            Mode::Fake => r.lanes.fake = pending_fake(),
            Mode::Detect => r.lanes.detect = pending_detect(),
            Mode::Preflight => r.lanes.preflight = pending_preflight(),
            Mode::BuildProbe => r.lanes.build_probe = pending_build_probe(),
            Mode::SignPlan => r.lanes.sign_plan = pending_sign_plan(),
        }
    }

    // ---- inactive lanes must be Pending ------------------------------------
    #[test]
    fn inactive_lane_non_pending_is_rejected() {
        // Detect is active; Fake (inactive) is set to Complete -> rejected.
        let mut r = valid_report(Mode::Detect);
        r.lanes.fake = complete_fake();
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            ReportError::InactiveLaneMustBePending {
                lane: Mode::Fake,
                ..
            }
        ));

        // BuildProbe is active; SignPlan (inactive) is set to Incomplete.
        let mut r = valid_report(Mode::BuildProbe);
        r.lanes.sign_plan.state = LaneState::Incomplete;
        r.lanes.sign_plan.reason = None;
        r.lanes.sign_plan.observation = None;
        r.lanes.sign_plan.failures = vec![Failure {
            stage: Stage::Sign,
            kind: FailureKind::SigningUnavailable,
        }];
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            ReportError::InactiveLaneMustBePending {
                lane: Mode::SignPlan,
                ..
            }
        ));
    }

    // ---- mode/harness consistency ------------------------------------------
    #[test]
    fn fake_requires_harness_only_true() {
        let mut r = valid_report(Mode::Fake);
        r.harness_only = false;
        assert!(matches!(
            r.validate().unwrap_err(),
            ReportError::HarnessInconsistent {
                mode: Mode::Fake,
                ..
            }
        ));
    }

    #[test]
    fn real_requires_harness_only_false() {
        for active in [
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let mut r = valid_report(active);
            r.harness_only = true;
            assert!(matches!(
                r.validate().unwrap_err(),
                ReportError::HarnessInconsistent { mode, harness_only: true } if mode == active
            ));
        }
    }

    // ---- schema version ----------------------------------------------------
    #[test]
    fn rejects_wrong_schema_version() {
        let mut r = valid_report(Mode::Fake);
        r.schema_version = 2;
        assert_eq!(
            r.validate().unwrap_err(),
            ReportError::SchemaVersion {
                found: 2,
                expected: REPORT_SCHEMA_VERSION,
            }
        );
    }

    // ---- pin validation wired into the report ------------------------------
    #[test]
    fn report_rejects_invalid_pin() {
        let mut r = valid_report(Mode::Fake);
        r.pin.nix.version = "2.34.9".to_string();
        assert!(matches!(r.validate().unwrap_err(), ReportError::Pin(_)));
    }

    // ---- lane mode label must match field position -------------------------
    #[test]
    fn lane_mode_mismatch_is_rejected() {
        let mut r = valid_report(Mode::Fake);
        // Tamper the inner mode label of the detect lane.
        r.lanes.detect.mode = Mode::Preflight;
        assert!(matches!(
            r.validate().unwrap_err(),
            ReportError::Lane {
                lane: Mode::Detect,
                error: LaneError::ModeFieldMismatch { .. }
            }
        ));
    }

    // ---- Lane<T> state-invariant truth table -------------------------------
    #[test]
    fn pending_requires_reason() {
        let mut lane = pending_fake();
        lane.reason = None;
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::PendingRequiresReason
        );
    }

    #[test]
    fn pending_forbids_observation_and_failures() {
        let mut lane = pending_fake();
        lane.observation = Some(FakeObservation {
            source: EvidenceSource::Fixture,
            fixture_pairs: 1,
        });
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::PendingForbidsObservation
        );

        let mut lane = pending_fake();
        lane.failures = vec![Failure {
            stage: Stage::Harness,
            kind: FailureKind::Unknown,
        }];
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::PendingForbidsFailures
        );
    }

    #[test]
    fn incomplete_forbids_reason_and_requires_failure() {
        // Isolate each invariant. The reason check is evaluated first, so a
        // lane carrying BOTH a reason and no failure surfaces the reason error.
        {
            let mut lane = complete_fake();
            lane.state = LaneState::Incomplete;
            lane.reason = Some(PendingReason::Deferred);
            lane.observation = None;
            lane.failures = vec![Failure {
                stage: Stage::Harness,
                kind: FailureKind::Unknown,
            }];
            assert_eq!(
                lane.validate(Mode::Fake).unwrap_err(),
                LaneError::IncompleteForbidsReason
            );
        }
        {
            let mut lane = complete_fake();
            lane.state = LaneState::Incomplete;
            lane.reason = None;
            lane.observation = None;
            lane.failures = vec![];
            assert_eq!(
                lane.validate(Mode::Fake).unwrap_err(),
                LaneError::IncompleteRequiresFailure
            );
        }
    }

    #[test]
    fn incomplete_allows_observation_optional() {
        // Incomplete with an observation + a failure is valid.
        let mut lane = complete_fake();
        lane.state = LaneState::Incomplete;
        lane.failures = vec![Failure {
            stage: Stage::Harness,
            kind: FailureKind::Unknown,
        }];
        lane.validate(Mode::Fake).unwrap();

        // Incomplete with NO observation + a failure is also valid.
        let mut lane = complete_fake();
        lane.state = LaneState::Incomplete;
        lane.observation = None;
        lane.failures = vec![Failure {
            stage: Stage::Harness,
            kind: FailureKind::Unknown,
        }];
        lane.validate(Mode::Fake).unwrap();
    }

    #[test]
    fn complete_forbids_reason_and_failures_and_requires_observation() {
        let mut lane = complete_fake();
        lane.reason = Some(PendingReason::Deferred);
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::CompleteForbidsReason
        );

        let mut lane = complete_fake();
        lane.failures = vec![Failure {
            stage: Stage::Harness,
            kind: FailureKind::Unknown,
        }];
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::CompleteForbidsFailures
        );

        let mut lane = complete_fake();
        lane.observation = None;
        assert_eq!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::CompleteRequiresObservation
        );
    }

    // ---- EvidenceSource honesty --------------------------------------------
    #[test]
    fn fake_lane_rejects_observed_source() {
        let mut lane = complete_fake();
        lane.observation.as_mut().unwrap().source = EvidenceSource::Observed;
        assert!(matches!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::EvidenceSourceMismatch {
                lane: Mode::Fake,
                found: EvidenceSource::Observed,
                expected: EvidenceSource::Fixture
            }
        ));
    }

    #[test]
    fn real_lane_rejects_fixture_source() {
        let mut lane = complete_detect();
        lane.observation.as_mut().unwrap().source = EvidenceSource::Fixture;
        assert!(matches!(
            lane.validate(Mode::Detect).unwrap_err(),
            LaneError::EvidenceSourceMismatch {
                lane: Mode::Detect,
                found: EvidenceSource::Fixture,
                expected: EvidenceSource::Observed
            }
        ));
    }

    #[test]
    fn fixture_source_is_rejected_in_real_lane_end_to_end() {
        // A Detect-active report whose detect observation is Fixture-sourced is
        // rejected at the report level: synthetic Fake data cannot be accepted
        // as Real evidence.
        let mut r = valid_report(Mode::Detect);
        r.lanes.detect.observation.as_mut().unwrap().source = EvidenceSource::Fixture;
        assert!(matches!(
            r.validate().unwrap_err(),
            ReportError::Lane {
                lane: Mode::Detect,
                error: LaneError::EvidenceSourceMismatch { .. }
            }
        ));
    }

    // ---- SignPlan executed must be false -----------------------------------
    #[test]
    fn sign_plan_executed_true_is_rejected() {
        let mut lane = complete_sign_plan();
        lane.observation.as_mut().unwrap().executed = true;
        assert_eq!(
            lane.validate(Mode::SignPlan).unwrap_err(),
            LaneError::SignPlanExecutedMustBeFalse
        );

        // Also at the report level (SignPlan active + executed=true).
        let mut r = valid_report(Mode::SignPlan);
        r.lanes.sign_plan.observation.as_mut().unwrap().executed = true;
        assert!(matches!(
            r.validate().unwrap_err(),
            ReportError::Lane {
                lane: Mode::SignPlan,
                error: LaneError::SignPlanExecutedMustBeFalse
            }
        ));
    }

    #[test]
    fn sign_plan_targets_empty_is_rejected() {
        let mut lane = complete_sign_plan();
        lane.observation.as_mut().unwrap().targets.clear();
        assert_eq!(
            lane.validate(Mode::SignPlan).unwrap_err(),
            LaneError::SignPlanTargetsEmpty
        );
    }

    // ---- observation content validation ------------------------------------
    #[test]
    fn fake_fixture_pairs_range() {
        let mut lane = complete_fake();
        lane.observation.as_mut().unwrap().fixture_pairs = 0;
        assert!(matches!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::FakeFixturePairsOutOfRange { got: 0 }
        ));
        let mut lane = complete_fake();
        lane.observation.as_mut().unwrap().fixture_pairs = 4_097;
        assert!(matches!(
            lane.validate(Mode::Fake).unwrap_err(),
            LaneError::FakeFixturePairsOutOfRange { got: 4_097 }
        ));
    }

    #[test]
    fn preflight_coverage_empty_valid_for_incomplete_rejected_for_complete() {
        // Empty coverage is now VALID for an Incomplete Preflight (the engine
        // may fail before any canonical cell completes). It is still rejected
        // for a Complete lane, which must carry exactly six canonical cells.
        {
            let mut lane = complete_preflight();
            lane.state = LaneState::Incomplete;
            lane.observation.as_mut().unwrap().coverage.clear();
            lane.failures = vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::NixMissing,
            }];
            lane.validate(Mode::Preflight).unwrap();
        }
        // Complete + empty coverage -> PreflightCoverageNotCanonical (the
        // six-cell requirement is the complete-only hook).
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage.clear();
        assert_eq!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightCoverageNotCanonical
        );
    }

    #[test]
    fn preflight_coverage_unknown_attr_and_system_rejected() {
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage[0].attr = "curl".to_string();
        assert!(matches!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightCoverageAttrUnknown { .. }
        ));
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage[0].system = "x86_64-linux".to_string();
        assert!(matches!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightCoverageSystemUnknown { .. }
        ));
    }

    #[test]
    fn detect_and_build_unknown_system_rejected() {
        let mut lane = complete_detect();
        lane.observation.as_mut().unwrap().host_system = Some("x86_64-linux".to_string());
        assert!(matches!(
            lane.validate(Mode::Detect).unwrap_err(),
            LaneError::DetectHostSystemUnknown { .. }
        ));
        let mut lane = complete_build_probe();
        lane.observation.as_mut().unwrap().built_system = Some("wasm32-wasi".to_string());
        assert!(matches!(
            lane.validate(Mode::BuildProbe).unwrap_err(),
            LaneError::BuildSystemUnknown { .. }
        ));
    }

    // ---- JSON round trips + determinism ------------------------------------
    #[test]
    fn json_round_trips_every_mode() {
        for active in [
            Mode::Fake,
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let r = valid_report(active);
            r.validate().unwrap();
            let json = serde_json::to_string(&r).unwrap();
            let back: Report = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back, "round trip for {active:?}");
        }
    }

    #[test]
    fn render_json_is_deterministic_with_trailing_newline() {
        let r = valid_report(Mode::Fake);
        let a = render_json(&r);
        let b = render_json(&r);
        assert_eq!(a, b);
        assert!(a.ends_with('\n'));
        // Pretty (multi-line) form.
        assert!(a.contains("\n  \"mode\""));
    }

    #[test]
    fn json_renders_trailing_newline_for_all_modes() {
        for active in [
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            assert!(render_json(&valid_report(active)).ends_with('\n'));
        }
    }

    #[test]
    fn camel_case_keys_present_and_no_pii_or_timestamp() {
        // Exercise a DETECT report so the new nested capability/identity-count
        // fields are actually serialized and audited for PII leakage.
        let r = valid_report(Mode::Detect);
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("schemaVersion"));
        assert!(obj.contains_key("harnessOnly"));
        let obs = &v["lanes"]["detect"]["observation"];
        assert_eq!(obs["applicationIdentityCount"], json!(1));
        assert_eq!(obs["installerIdentityCount"], json!(1));
        assert_eq!(obs["nixbldUserCount"], json!(32));
        assert_eq!(obs["xcodeSelection"], json!("fullXcode"));
        assert!(obs["toolCapabilities"].is_object());

        let blob = serde_json::to_string(&v).unwrap();
        // Clear PII KEY names that must NEVER appear (none are field names in
        // this model). Note the bare substrings "user"/"identity" are
        // intentionally NOT used here: legitimate count field names
        // (nixbldUserCount, *IdentityCount) contain them; the value-based checks
        // below guard against actual leaked identity NAMES/fingerprints.
        for forbidden in [
            "hostname",
            "hostName",
            "machine",
            "username",
            "userName",
            "credential",
            "profile",
            "teamId",
            "timestamp",
            "diagnostic",
            "rawArchive",
        ] {
            assert!(
                !blob.contains(forbidden),
                "forbidden key {forbidden:?} present"
            );
        }
        // Value-based PII guards: no identity NAME prefix ("Developer ID") and
        // no 40-char lowercase-hex SHA-1 fingerprint (as `security find-identity`
        // emits) may ever reach the report.
        assert!(!blob.contains("Developer ID"), "identity name leaked");
        assert!(
            !blob.contains("abcdef0123456789"),
            "fingerprint-shaped hex leaked"
        );
    }

    // ---- deny_unknown_fields on report/lane/observation --------------------
    fn report_value() -> Value {
        serde_json::from_str(&serde_json::to_string(&valid_report(Mode::Fake)).unwrap()).unwrap()
    }

    fn with_unknown(path: &[&str]) -> String {
        let mut v = report_value();
        if path.is_empty() {
            v["bogusKey"] = json!(1);
        } else {
            let mut cur = &mut v;
            for seg in path {
                cur = &mut cur[*seg];
            }
            cur["bogusKey"] = json!(1);
        }
        v.to_string()
    }

    #[test]
    fn rejects_unknown_keys_at_every_level() {
        // top-level report
        assert!(serde_json::from_str::<Report>(&with_unknown(&[])).is_err());
        // lanes container
        assert!(serde_json::from_str::<Report>(&with_unknown(&["lanes"])).is_err());
        // a lane
        assert!(serde_json::from_str::<Report>(&with_unknown(&["lanes", "fake"])).is_err());
        // an observation
        assert!(
            serde_json::from_str::<Report>(&with_unknown(&["lanes", "fake", "observation"]))
                .is_err()
        );
        // the pin + nested pin containers
        assert!(serde_json::from_str::<Report>(&with_unknown(&["pin"])).is_err());
        assert!(serde_json::from_str::<Report>(&with_unknown(&["pin", "nixpkgs"])).is_err());
    }

    #[test]
    fn lane_skips_empty_optional_fields_on_serialize() {
        // A Pending lane serializes without observation/failures keys.
        let lane = pending_detect();
        let v: Value = serde_json::from_str(&serde_json::to_string(&lane).unwrap()).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("observation"));
        assert!(!obj.contains_key("failures"));
        assert!(obj.contains_key("reason"));
    }

    // ---- Markdown render determinism + escaping ----------------------------
    #[test]
    fn render_markdown_is_deterministic() {
        for active in [
            Mode::Fake,
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let r = valid_report(active);
            assert_eq!(render_markdown(&r), render_markdown(&r), "{active:?}");
        }
    }

    #[test]
    fn render_markdown_contains_expected_sections_and_no_pii() {
        let md = render_markdown(&valid_report(Mode::Preflight));
        assert!(md.contains("# S3 macOS spike report"));
        assert!(md.contains("| mode | preflight |"));
        assert!(md.contains("## Lane fake"));
        assert!(md.contains("## Lane buildProbe"));
        assert!(md.contains("## Lane signPlan"));
        assert!(md.contains("https://cache.nixos.org/"));
        // Coverage table for the active preflight lane (output/closure split).
        assert!(md.contains("| attr | system | output | closure |"));
        assert!(md.contains("| observation.nixVersionExact | true |"));
        assert!(md.contains("| observation.flakePrefetchVerified | true |"));
        // No PII / timestamp / raw-archive markers.
        for forbidden in [
            "hostname",
            "machine",
            "username",
            "credential",
            "profile",
            "timestamp",
            "rawArchive",
        ] {
            assert!(!md.contains(forbidden), "markdown leaked {forbidden:?}");
        }
    }

    #[test]
    fn escape_md_handles_pipe_and_newlines() {
        // Pipe is escaped; newline flattened to space; CR dropped.
        assert_eq!(escape_md("a|b"), "a\\|b");
        assert_eq!(escape_md("a\nb"), "a b");
        assert_eq!(escape_md("a\r\nb"), "a b");
        assert_eq!(escape_md("plain"), "plain");
        // Every pipe is escaped, including those flanking the flattened newline.
        assert_eq!(escape_md("|x|\n|y|"), "\\|x\\| \\|y\\|");
    }

    #[test]
    fn markdown_table_survives_pipe_in_value() {
        // Although validated reports never carry pipes, escape_md guarantees the
        // table delimiter count is stable. Render a fake report and confirm the
        // separator rows are the only standalone 2-col separators.
        let md = render_markdown(&valid_report(Mode::Fake));
        assert!(md.contains("| field | value |"));
    }

    // ---- Failure deny_unknown_fields + enum round trip ---------------------
    #[test]
    fn failure_round_trips_and_rejects_unknown_fields() {
        let f = Failure {
            stage: Stage::Build,
            kind: FailureKind::BuildFailed,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v, json!({ "stage": "build", "kind": "buildFailed" }));
        let back: Failure = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, f);
        let mut bad = v.clone();
        bad["bogus"] = json!(1);
        assert!(serde_json::from_str::<Failure>(&bad.to_string()).is_err());
    }

    #[test]
    fn coverage_entry_round_trips_camel_case() {
        let e = CoverageEntry {
            attr: "hello".to_string(),
            system: "aarch64-darwin".to_string(),
            output_available: true,
            closure_available: true,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({
                "attr": "hello",
                "system": "aarch64-darwin",
                "outputAvailable": true,
                "closureAvailable": true
            })
        );
        let back: CoverageEntry = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, e);
    }

    // ---- bounded Display errors --------------------------------------------
    #[test]
    fn lane_and_report_errors_have_bounded_display() {
        // LaneError carrying a huge "got" string stays bounded.
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage[0].attr = "x".repeat(10_000);
        let err = lane.validate(Mode::Preflight).unwrap_err();
        let s = err.to_string();
        assert!(s.len() < 256, "lane error display was {}: {s:?}", s.len());

        // ReportError Display also bounded.
        let mut r = valid_report(Mode::Fake);
        r.schema_version = 99;
        let s = r.validate().unwrap_err().to_string();
        assert!(s.len() < 256);
    }

    #[test]
    fn report_error_display_names_pin_and_lane() {
        let mut r = valid_report(Mode::Fake);
        r.pin.nix.version = "bad".to_string();
        assert!(
            r.validate()
                .unwrap_err()
                .to_string()
                .starts_with("pin invalid:")
        );

        let mut r = valid_report(Mode::Fake);
        r.lanes.detect.mode = Mode::Preflight;
        let s = r.validate().unwrap_err().to_string();
        assert!(s.starts_with("lane Detect invalid:"), "{s}");
    }

    // =======================================================================
    // PR-7/S3 contract-slice repair: evidence source, coverage split, complete
    // hooks, build-probe invariants, sign-plan target order.
    // =======================================================================

    fn expected_source(m: Mode) -> EvidenceSource {
        match m {
            Mode::Fake => EvidenceSource::Fixture,
            Mode::SignPlan => EvidenceSource::Designed,
            _ => EvidenceSource::Observed,
        }
    }

    /// Set the active lane's observation evidence source. Every observation
    /// type stores a `source` field.
    fn set_active_source(r: &mut Report, active: Mode, src: EvidenceSource) {
        match active {
            Mode::Fake => r.lanes.fake.observation.as_mut().unwrap().source = src,
            Mode::Detect => r.lanes.detect.observation.as_mut().unwrap().source = src,
            Mode::Preflight => r.lanes.preflight.observation.as_mut().unwrap().source = src,
            Mode::BuildProbe => r.lanes.build_probe.observation.as_mut().unwrap().source = src,
            Mode::SignPlan => r.lanes.sign_plan.observation.as_mut().unwrap().source = src,
        }
    }

    /// A fresh Complete BuildProbe lane with one invariant mutated.
    fn build_probe_with(f: impl FnOnce(&mut BuildProbeObservation)) -> Lane<BuildProbeObservation> {
        let mut lane = complete_build_probe();
        f(lane.observation.as_mut().unwrap());
        lane
    }

    // ---- (1) EvidenceSource: each mode admits exactly one source ----------
    #[test]
    fn every_mode_rejects_every_wrong_evidence_source_end_to_end() {
        for active in [
            Mode::Fake,
            Mode::Detect,
            Mode::Preflight,
            Mode::BuildProbe,
            Mode::SignPlan,
        ] {
            let expected = expected_source(active);
            for wrong in [
                EvidenceSource::Fixture,
                EvidenceSource::Observed,
                EvidenceSource::Designed,
            ] {
                if wrong == expected {
                    continue;
                }
                let mut r = valid_report(active);
                set_active_source(&mut r, active, wrong);
                assert!(
                    matches!(
                        r.validate().unwrap_err(),
                        ReportError::Lane {
                            lane,
                            error: LaneError::EvidenceSourceMismatch { found, .. }
                        } if lane == active && found == wrong
                    ),
                    "mode {active:?} must reject source {wrong:?}"
                );
            }
        }
    }

    #[test]
    fn sign_plan_complete_never_counts_as_observed_signing_evidence() {
        // The headline honesty invariant: a designed signing plan must never be
        // accepted as observed signing evidence.
        let mut r = valid_report(Mode::SignPlan);
        r.lanes.sign_plan.observation.as_mut().unwrap().source = EvidenceSource::Observed;
        assert!(matches!(
            r.validate().unwrap_err(),
            ReportError::Lane {
                lane: Mode::SignPlan,
                error: LaneError::EvidenceSourceMismatch {
                    lane: Mode::SignPlan,
                    found: EvidenceSource::Observed,
                    expected: EvidenceSource::Designed
                }
            }
        ));
    }

    // ---- (2) FailureKind::CacheQueryFailed; a miss is evidence, not failure
    #[test]
    fn cache_query_failed_round_trips_and_miss_is_not_a_failure() {
        let f = Failure {
            stage: Stage::Preflight,
            kind: FailureKind::CacheQueryFailed,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({ "stage": "preflight", "kind": "cacheQueryFailed" })
        );
        let back: Failure = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, f);
        // A Complete preflight whose coverage is entirely miss is valid: misses
        // are availability evidence, never a harness failure.
        let mut r = valid_report(Mode::Preflight);
        for e in &mut r.lanes.preflight.observation.as_mut().unwrap().coverage {
            e.output_available = false;
            e.closure_available = false;
        }
        r.validate().unwrap();
    }

    // ---- (3) CoverageEntry output/closure split ---------------------------
    #[test]
    fn preflight_closure_without_output_is_rejected() {
        let mut lane = complete_preflight();
        let obs = lane.observation.as_mut().unwrap();
        obs.coverage[5].output_available = false;
        obs.coverage[5].closure_available = true;
        assert!(matches!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightClosureWithoutOutput { .. }
        ));
    }

    // ---- (4)+(5) Preflight canonical ordering, partials, complete hooks ---
    #[test]
    fn preflight_duplicate_pair_is_rejected() {
        let mut lane = complete_preflight();
        // Replace the 6th cell with a duplicate of the 1st.
        let obs = lane.observation.as_mut().unwrap();
        obs.coverage[5] = CoverageEntry {
            attr: "hello".to_string(),
            system: "x86_64-darwin".to_string(),
            output_available: true,
            closure_available: true,
        };
        assert!(matches!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightDuplicatePair { .. }
        ));
    }

    #[test]
    fn preflight_non_canonical_order_is_rejected() {
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage.swap(1, 2);
        assert_eq!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightNonCanonicalOrder
        );
    }

    #[test]
    fn preflight_wrong_pair_in_position_is_rejected() {
        let mut lane = complete_preflight();
        // A valid attr+system, but the wrong canonical slot (aarch64 at pos 0).
        let obs = lane.observation.as_mut().unwrap();
        obs.coverage[0] = CoverageEntry {
            attr: "hello".to_string(),
            system: "aarch64-darwin".to_string(),
            output_available: true,
            closure_available: true,
        };
        assert_eq!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightNonCanonicalOrder
        );
    }

    #[test]
    fn preflight_complete_missing_cells_rejected_partial_incomplete_accepted() {
        // Complete with only the first 3 canonical cells -> complete-state
        // rejection (missing evidence).
        let mut lane = complete_preflight();
        lane.observation.as_mut().unwrap().coverage.truncate(3);
        assert_eq!(
            lane.validate(Mode::Preflight).unwrap_err(),
            LaneError::PreflightCoverageNotCanonical
        );

        // The SAME partial observation is accepted when the lane is Incomplete
        // and carries a failure: validate_complete is not called for Incomplete.
        let mut partial = complete_preflight();
        partial.state = LaneState::Incomplete;
        partial.observation.as_mut().unwrap().coverage.truncate(3);
        partial.failures = vec![Failure {
            stage: Stage::Preflight,
            kind: FailureKind::CacheQueryFailed,
        }];
        partial.validate(Mode::Preflight).unwrap();
    }

    #[test]
    fn preflight_version_and_prefetch_flags_rejected_only_when_complete() {
        // Complete rejects nixVersionExact == false.
        {
            let mut lane = complete_preflight();
            lane.observation.as_mut().unwrap().nix_version_exact = false;
            assert_eq!(
                lane.validate(Mode::Preflight).unwrap_err(),
                LaneError::PreflightNixVersionNotExact
            );
        }
        // Complete rejects flakePrefetchVerified == false.
        {
            let mut lane = complete_preflight();
            lane.observation.as_mut().unwrap().flake_prefetch_verified = false;
            assert_eq!(
                lane.validate(Mode::Preflight).unwrap_err(),
                LaneError::PreflightFlakePrefetchNotVerified
            );
        }
        // Incomplete accepts both false alongside a failure (partial evidence).
        {
            let mut lane = complete_preflight();
            lane.state = LaneState::Incomplete;
            let obs = lane.observation.as_mut().unwrap();
            obs.nix_version_exact = false;
            obs.flake_prefetch_verified = false;
            lane.failures = vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::CacheQueryFailed,
            }];
            lane.validate(Mode::Preflight).unwrap();
        }
    }

    #[test]
    fn preflight_complete_false_availability_matrix_accepted() {
        // All six canonical cells present and ordered, with a mix of true/false
        // output and closure. Every combination is valid in a Complete preflight
        // as long as closure implies output.
        let mut lane = complete_preflight();
        let obs = lane.observation.as_mut().unwrap();
        for (i, e) in obs.coverage.iter_mut().enumerate() {
            e.output_available = i % 2 == 0;
            e.closure_available = e.output_available && i != 2;
        }
        lane.validate(Mode::Preflight).unwrap();
    }

    // ---- (6) BuildProbe complete invariants ------------------------------
    #[test]
    fn build_probe_complete_rejects_each_missing_or_false_invariant() {
        let mut lane = complete_build_probe();
        lane.observation.as_mut().unwrap().built_system = None;
        assert_eq!(
            lane.validate(Mode::BuildProbe).unwrap_err(),
            LaneError::BuildSystemMissing
        );
        assert_eq!(
            build_probe_with(|o| o.sandbox_enforced = false)
                .validate(Mode::BuildProbe)
                .unwrap_err(),
            LaneError::SandboxEnforcedFalse
        );
        assert_eq!(
            build_probe_with(|o| o.sandbox_fallback_disabled = false)
                .validate(Mode::BuildProbe)
                .unwrap_err(),
            LaneError::SandboxFallbackDisabledFalse
        );
        assert_eq!(
            build_probe_with(|o| o.build_users_ready = false)
                .validate(Mode::BuildProbe)
                .unwrap_err(),
            LaneError::BuildUsersReadyFalse
        );
        assert_eq!(
            build_probe_with(|o| o.network_denied = false)
                .validate(Mode::BuildProbe)
                .unwrap_err(),
            LaneError::NetworkDeniedFalse
        );
        assert_eq!(
            build_probe_with(|o| o.approval_recorded = false)
                .validate(Mode::BuildProbe)
                .unwrap_err(),
            LaneError::ApprovalRecordedFalse
        );
    }

    #[test]
    fn build_probe_incomplete_partial_with_failure_accepted() {
        // Incomplete may carry false values + no built system alongside a
        // failure: validate_complete is not invoked for Incomplete lanes.
        let mut lane = complete_build_probe();
        lane.state = LaneState::Incomplete;
        let obs = lane.observation.as_mut().unwrap();
        obs.built_system = None;
        obs.sandbox_enforced = false;
        obs.sandbox_fallback_disabled = false;
        obs.build_users_ready = false;
        obs.network_denied = false;
        obs.approval_recorded = false;
        lane.failures = vec![Failure {
            stage: Stage::Build,
            kind: FailureKind::BuildFailed,
        }];
        lane.validate(Mode::BuildProbe).unwrap();
    }

    // ---- (7) SignPlan targets exactly [Runtime, Installer] ---------------
    #[test]
    fn sign_plan_targets_must_be_exactly_runtime_then_installer() {
        // Reversed.
        {
            let mut lane = complete_sign_plan();
            lane.observation.as_mut().unwrap().targets =
                vec![SignTarget::Installer, SignTarget::Runtime];
            assert_eq!(
                lane.validate(Mode::SignPlan).unwrap_err(),
                LaneError::SignPlanTargetsNotRuntimeInstaller
            );
        }
        // Duplicate.
        {
            let mut lane = complete_sign_plan();
            lane.observation.as_mut().unwrap().targets =
                vec![SignTarget::Runtime, SignTarget::Runtime];
            assert_eq!(
                lane.validate(Mode::SignPlan).unwrap_err(),
                LaneError::SignPlanTargetsNotRuntimeInstaller
            );
        }
        // Too many.
        {
            let mut lane = complete_sign_plan();
            lane.observation.as_mut().unwrap().targets = vec![
                SignTarget::Runtime,
                SignTarget::Installer,
                SignTarget::Runtime,
            ];
            assert_eq!(
                lane.validate(Mode::SignPlan).unwrap_err(),
                LaneError::SignPlanTargetsNotRuntimeInstaller
            );
        }
        // Too few (non-empty).
        {
            let mut lane = complete_sign_plan();
            lane.observation.as_mut().unwrap().targets = vec![SignTarget::Runtime];
            assert_eq!(
                lane.validate(Mode::SignPlan).unwrap_err(),
                LaneError::SignPlanTargetsNotRuntimeInstaller
            );
        }
        // Missing entirely stays its own distinct error.
        {
            let mut lane = complete_sign_plan();
            lane.observation.as_mut().unwrap().targets.clear();
            assert_eq!(
                lane.validate(Mode::SignPlan).unwrap_err(),
                LaneError::SignPlanTargetsEmpty
            );
        }
    }

    // ---- (8) New fields render deterministically in camelCase JSON -------
    #[test]
    fn designed_source_and_new_fields_render_in_camel_case() {
        // SignPlan: source "designed", targets [runtime, installer].
        let r = valid_report(Mode::SignPlan);
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(
            v["lanes"]["signPlan"]["observation"]["source"],
            json!("designed")
        );
        assert_eq!(
            v["lanes"]["signPlan"]["observation"]["targets"],
            json!(["runtime", "installer"])
        );

        // BuildProbe: the five camelCase invariant booleans.
        let r = valid_report(Mode::BuildProbe);
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        let obs = &v["lanes"]["buildProbe"]["observation"];
        assert_eq!(obs["sandboxEnforced"], json!(true));
        assert_eq!(obs["sandboxFallbackDisabled"], json!(true));
        assert_eq!(obs["buildUsersReady"], json!(true));
        assert_eq!(obs["networkDenied"], json!(true));
        assert_eq!(obs["approvalRecorded"], json!(true));

        // Preflight: the two camelCase flags + output/closure coverage split.
        let r = valid_report(Mode::Preflight);
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        let obs = &v["lanes"]["preflight"]["observation"];
        assert_eq!(obs["nixVersionExact"], json!(true));
        assert_eq!(obs["flakePrefetchVerified"], json!(true));
        assert_eq!(obs["coverage"][0]["outputAvailable"], json!(true));
        assert_eq!(obs["coverage"][0]["closureAvailable"], json!(true));
    }

    // =======================================================================
    // PR-7/S3 Detect extension: nested tool capabilities, Xcode enum, counts,
    // nixbld group, and their serde/validation/render contract.
    // =======================================================================

    #[test]
    fn detect_observation_serializes_nested_camel_case() {
        let r = valid_report(Mode::Detect);
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        let obs = &v["lanes"]["detect"]["observation"];
        assert_eq!(obs["nixPresent"], json!(true));
        assert_eq!(obs["hostSystem"], json!("aarch64-darwin"));
        assert_eq!(obs["xcodeSelection"], json!("fullXcode"));
        assert_eq!(obs["applicationIdentityCount"], json!(1));
        assert_eq!(obs["installerIdentityCount"], json!(1));
        assert_eq!(obs["nixbldGroupPresent"], json!(true));
        assert_eq!(obs["nixbldUserCount"], json!(32));
        let tc = &obs["toolCapabilities"];
        assert_eq!(tc["codesign"], json!(true));
        assert_eq!(tc["productsign"], json!(true));
        assert_eq!(tc["security"], json!(true));
        assert_eq!(tc["spctl"], json!(true));
    }

    #[test]
    fn detect_observation_round_trips_through_serde() {
        let r = valid_report(Mode::Detect);
        let json = serde_json::to_string(&r).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn detect_observation_rejects_unknown_keys_in_nested_types() {
        let mut v: Value =
            serde_json::from_str(&serde_json::to_string(&valid_report(Mode::Detect)).unwrap())
                .unwrap();
        // Unknown key inside the observation itself.
        v["lanes"]["detect"]["observation"]["bogus"] = json!(1);
        assert!(serde_json::from_str::<Report>(&v.to_string()).is_err());
        // Unknown key inside toolCapabilities.
        let mut v2: Value =
            serde_json::from_str(&serde_json::to_string(&valid_report(Mode::Detect)).unwrap())
                .unwrap();
        v2["lanes"]["detect"]["observation"]["toolCapabilities"]["bogus"] = json!(1);
        assert!(serde_json::from_str::<Report>(&v2.to_string()).is_err());
    }

    #[test]
    fn detect_identity_and_user_counts_bounded_at_4096() {
        // Exactly 4096 is accepted; 4097 is rejected for each count field.
        let mut lane = complete_detect();
        lane.observation
            .as_mut()
            .unwrap()
            .application_identity_count = 4_096;
        lane.validate(Mode::Detect).unwrap();
        lane.observation
            .as_mut()
            .unwrap()
            .application_identity_count = 4_097;
        assert!(matches!(
            lane.validate(Mode::Detect).unwrap_err(),
            LaneError::DetectIdentityCountOutOfRange { got: 4_097 }
        ));
        let mut lane = complete_detect();
        lane.observation.as_mut().unwrap().installer_identity_count = 4_097;
        assert!(matches!(
            lane.validate(Mode::Detect).unwrap_err(),
            LaneError::DetectIdentityCountOutOfRange { got: 4_097 }
        ));
        let mut lane = complete_detect();
        lane.observation.as_mut().unwrap().nixbld_user_count = 4_097;
        assert!(matches!(
            lane.validate(Mode::Detect).unwrap_err(),
            LaneError::DetectNixbldUserCountOutOfRange { got: 4_097 }
        ));
    }

    #[test]
    fn detect_xcode_selection_all_variants_round_trip() {
        for x in [
            XcodeSelection::Absent,
            XcodeSelection::CommandLineTools,
            XcodeSelection::FullXcode,
        ] {
            let mut lane = complete_detect();
            lane.observation.as_mut().unwrap().xcode_selection = x;
            lane.validate(Mode::Detect).unwrap();
            let json = serde_json::to_string(&lane.observation).unwrap();
            let back: DetectObservation = serde_json::from_str(&json).unwrap();
            assert_eq!(back.xcode_selection, x);
        }
    }

    #[test]
    fn detect_markdown_renders_counts_and_capabilities_without_pii() {
        let md = render_markdown(&valid_report(Mode::Detect));
        assert!(md.contains("| observation.xcodeSelection | fullXcode |"));
        assert!(md.contains("| observation.applicationIdentityCount | 1 |"));
        assert!(md.contains("| observation.nixbldUserCount | 32 |"));
        assert!(md.contains("| observation.tools.security | true |"));
        // No identity NAME / fingerprint value ever reaches the markdown.
        assert!(!md.contains("Developer ID"));
        for forbidden in [
            "hostname",
            "machine",
            "username",
            "credential",
            "profile",
            "timestamp",
        ] {
            assert!(!md.contains(forbidden), "markdown leaked {forbidden:?}");
        }
    }

    // =======================================================================
    // PR-7/S3 Detect repair: off-macOS live Detect is Incomplete (never
    // Complete/success). The closed report schema must accept an Incomplete
    // Detect lane carrying `hostSystem: null` + a partial Observed observation +
    // one closed Detect/Unknown failure, so the artifact is writable and the CLI
    // exits 69 after writing it. No host system value is invented.
    // =======================================================================

    #[test]
    fn detect_off_macos_incomplete_lane_with_null_host_validates_and_renders() {
        let mut r = valid_report(Mode::Detect);
        let lane = &mut r.lanes.detect;
        lane.state = LaneState::Incomplete;
        lane.reason = None;
        lane.failures = vec![Failure {
            stage: Stage::Detect,
            kind: FailureKind::Unknown,
        }];
        let obs = lane.observation.as_mut().unwrap();
        obs.host_system = None;
        // Retain a partial Observed observation (defaults): no host system, no
        // identities, but the probe fields may have been gathered.
        obs.application_identity_count = 0;
        obs.installer_identity_count = 0;
        r.validate()
            .expect("off-macOS Incomplete detect lane validates");

        // The JSON form carries hostSystem: null and the one closed failure.
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        let detect = &v["lanes"]["detect"];
        assert_eq!(detect["state"], json!("incomplete"));
        assert_eq!(detect["observation"]["hostSystem"], json!(null));
        assert_eq!(
            detect["failures"],
            json!([{ "stage": "detect", "kind": "unknown" }])
        );
        // The partial observation still round-trips.
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
