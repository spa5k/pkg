//! Spike S3 (PR-7) — DETECT slice: read-only macOS host capability detection.
//!
//! This module defines an injectable [`ProbeRunner`] trait and a production
//! [`BoundedProbeRunner`] over [`crate::command`], plus the pure parsers and the
//! [`detect`] orchestration that assembles a [`DetectObservation`].
//!
//! # Hardening contract
//! `detect` uses ONLY fixed absolute paths and EXACT argv — never `command -v`,
//! `grep`, a shell, a `PATH` lookup, or any inherited environment. Subprocess
//! probes run through [`crate::command::run`], which clears the environment,
//! sets only `LANG=C`/`LC_ALL=C`, drains stdout/stderr under bounded caps, and
//! kills the whole child process group on timeout.
//!
//! Missing fixed tools and most nonzero "not found" outcomes are capability
//! ABSENCE (an honest Complete observation), NOT an internal failure. There are
//! two deliberate exceptions:
//!   * the `security find-identity` probes — the real tool exits 0 even when
//!     ZERO identities exist (it prints "0 valid identities found"), so a
//!     nonzero exit or signal is an internal Detect/Unknown failure (the count
//!     for that class stays zero), NOT identity absence;
//!   * a live Detect off-macOS (`host_system()` is `None`) NEVER reaches
//!     Complete — it records the partial Observed observation PLUS a closed
//!     Detect/Unknown failure so the lane is Incomplete (the CLI exits 69 after
//!     writing both artifacts). No Linux host system value is ever invented.
//!
//! Only a timeout, cap overflow, malformed (non-UTF-8) output, a nonzero
//! `security` exit, or an off-macOS host is an Incomplete failure with a closed
//! [`Stage`]/[`FailureKind`].
//!
//! `hostSystem` comes from a compile-time OS/ARCH mapping (injectable into
//! [`detect`] so the capability-absence path can still construct a Complete
//! observation using a canonical Darwin system). `nixPresent` is true only if
//! the optional `--nix-bin` is an ABSOLUTE EXISTING FILE — it is never searched
//! on `PATH` and never executed. No identity NAME, tool PATH, hostname,
//! username, keychain profile, timestamp, or raw tool output is ever stored: the
//! observation carries only booleans, the closed Xcode enum, and bounded counts.
//!
//! `#![forbid(unsafe_code)]`; the production [`BoundedProbeRunner`] DOES run
//! the read-only `/usr/bin/security find-identity` probe, so a live Detect reads
//! identity metadata/counts from the default keychain (never credentials, never
//! unlocks/signs/notarizes, never writes keychain data). No unit test accesses
//! the keychain: every one drives a [`FakeProbeRunner`] with injected
//! transcripts, and the repo-root validation lanes never run a live Detect.

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use crate::command::{CommandError, CommandOutcome, CommandRunner, CommandSpec, RealRunner};

use crate::report::{
    DetectObservation, EvidenceSource, Failure, FailureKind, MAX_IDENTITY_COUNT, Stage,
    ToolCapabilities, XcodeSelection,
};
#[cfg(test)]
use std::path::PathBuf;

/// Per-stream byte cap for every read-only probe (8 KiB). Generous for normal
/// hosts yet bounded so a runaway child cannot flood memory.
const PROBE_CAP: u64 = 8 * 1024;
/// Wall-clock timeout for every probe (10 s, well within the 1 ms..=30 s range).
/// `xcrun --find` can be slow on a cold Spotlight index.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

// ---- fixed absolute program paths (NO PATH lookup, NO shell) ---------------
//
// Held as `&'static str` because `Path::new` is not const-stable on the pinned
// toolchain (1.96.1). Each use site converts to `&Path` via `Path::new`.

/// `/usr/bin/xcode-select`.
pub const XCODE_SELECT: &str = "/usr/bin/xcode-select";
/// `/usr/bin/xcrun`.
pub const XCRUN: &str = "/usr/bin/xcrun";
/// `/usr/bin/security`.
pub const SECURITY: &str = "/usr/bin/security";
/// `/usr/bin/dscl`.
pub const DSCL: &str = "/usr/bin/dscl";

/// `/usr/bin/codesign`.
pub const TOOL_CODESIGN: &str = "/usr/bin/codesign";
/// `/usr/bin/stapler`.
pub const TOOL_STAPLER: &str = "/usr/bin/stapler";
/// `/usr/bin/productbuild`.
pub const TOOL_PRODUCTBUILD: &str = "/usr/bin/productbuild";
/// `/usr/bin/productsign`.
pub const TOOL_PRODUCTSIGN: &str = "/usr/bin/productsign";
/// `/usr/bin/pkgbuild`.
pub const TOOL_PKGBUILD: &str = "/usr/bin/pkgbuild";
/// `/usr/sbin/spctl`.
pub const TOOL_SPCTL: &str = "/usr/sbin/spctl";

/// Identity-name needles parsed as COUNTS ONLY (never the name is stored).
const APPLICATION_NEEDLE: &[u8] = b"Developer ID Application";
const INSTALLER_NEEDLE: &[u8] = b"Developer ID Installer";

/// The result of one Detect run: a (possibly partial) observation plus any
/// internal failures recorded along the way. Empty `failures` ⇒ Complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectOutcome {
    /// The gathered observation (always `source == Observed`). Fields whose probe
    /// failed internally keep their default (false/0); the `failures` list
    /// explains why.
    pub observation: DetectObservation,
    /// Internal failures (timeout / cap overflow / malformed output). Capability
    /// absence is NEVER a failure.
    pub failures: Vec<Failure>,
}

/// Injectable surface for the Detect lane: run a fixed command probe and test
/// filesystem presence of a fixed absolute path. The production implementation
/// is [`BoundedProbeRunner`]; tests use [`FakeProbeRunner`].
pub trait ProbeRunner {
    /// Run a validated read-only command probe. A nonzero exit / signal is an
    /// `Ok` outcome (the caller interprets the exit code).
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>;
    /// `true` iff `path` exists as a file (following symlinks, like `Path::is_file`).
    fn probe_is_file(&self, path: &Path) -> bool;
}

/// The production bounded runner: subprocess probes go through [`run`] (fixed
/// `LANG=C`/`LC_ALL=C` env, bounded caps, process-group kill on timeout);
/// filesystem presence uses `Path::is_file`.
#[derive(Debug, Default, Clone, Copy)]
pub struct BoundedProbeRunner {
    runner: RealRunner,
}

impl BoundedProbeRunner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: RealRunner::new(),
        }
    }
}

impl ProbeRunner for BoundedProbeRunner {
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        self.runner.run_probe(spec)
    }
    fn probe_is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

fn nz_cap() -> NonZeroU64 {
    NonZeroU64::new(PROBE_CAP).expect("PROBE_CAP nonzero")
}

/// Build a fixed probe spec: a fixed absolute `program` path constant + exact
/// `argv`, standard caps and timeout. The executor supplies the fail-closed
/// environment.
fn probe(program: &str, args: &[&str]) -> CommandSpec {
    CommandSpec::new(
        Path::new(program).to_path_buf(),
        args.iter().map(|a| OsString::from(*a)).collect(),
        nz_cap(),
        nz_cap(),
        PROBE_TIMEOUT,
    )
    .expect("fixed probe spec is valid")
}

/// `/usr/bin/xcode-select -p`.
pub(crate) fn xcode_select_spec() -> CommandSpec {
    probe(XCODE_SELECT, &["-p"])
}

/// `/usr/bin/xcrun --find notarytool`.
pub(crate) fn notarytool_spec() -> CommandSpec {
    probe(XCRUN, &["--find", "notarytool"])
}

/// `/usr/bin/security find-identity -v -p codesigning` (Developer ID Application).
pub(crate) fn security_application_spec() -> CommandSpec {
    probe(SECURITY, &["find-identity", "-v", "-p", "codesigning"])
}

/// `/usr/bin/security find-identity -v` (Developer ID Installer; Apple does NOT
/// support `-p productsign`, so the unrestricted list is parsed instead).
pub(crate) fn security_installer_spec() -> CommandSpec {
    probe(SECURITY, &["find-identity", "-v"])
}

/// `/usr/bin/dscl . -read /Groups/_nixbld GroupMembership`.
pub(crate) fn dscl_spec() -> CommandSpec {
    probe(DSCL, &[".", "-read", "/Groups/_nixbld", "GroupMembership"])
}

/// Compile-time host system mapping. `Some` only on macOS for the two pinned
/// Darwin architectures; `None` otherwise (so a non-Darwin build never reports
/// a host system outside the pin). Exposed `pub(crate)` so [`detect_report`](
/// crate::runner::detect_report) can pass it to [`detect`]; tests pass a
/// canonical Darwin system to keep the capability-absence path Complete
/// independent of the real test host.
pub(crate) fn host_system() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Some("aarch64-darwin")
        } else if cfg!(target_arch = "x86_64") {
            Some("x86_64-darwin")
        } else {
            None
        }
    } else {
        None
    }
}

/// `nixPresent`: true only if `nix_bin` is an ABSOLUTE EXISTING FILE. Never
/// searched on `PATH`, never executed.
fn nix_present(runner: &dyn ProbeRunner, nix_bin: Option<&Path>) -> bool {
    match nix_bin {
        Some(p) if p.is_absolute() => runner.probe_is_file(p),
        _ => false,
    }
}

/// Detect all nine filesystem capability flags. `notarytool` is detected via the
/// `xcrun` command below, not here.
fn detect_tool_capabilities(runner: &dyn ProbeRunner) -> ToolCapabilities {
    ToolCapabilities {
        codesign: runner.probe_is_file(Path::new(TOOL_CODESIGN)),
        xcrun: runner.probe_is_file(Path::new(XCRUN)),
        // Set by the xcrun probe below; default false.
        notarytool: false,
        stapler: runner.probe_is_file(Path::new(TOOL_STAPLER)),
        productbuild: runner.probe_is_file(Path::new(TOOL_PRODUCTBUILD)),
        productsign: runner.probe_is_file(Path::new(TOOL_PRODUCTSIGN)),
        pkgbuild: runner.probe_is_file(Path::new(TOOL_PKGBUILD)),
        spctl: runner.probe_is_file(Path::new(TOOL_SPCTL)),
        security: runner.probe_is_file(Path::new(SECURITY)),
    }
}

/// Map a [`CommandError`] to a closed Detect-lane [`Failure`]. Timeout keeps its
/// own kind; every other internal failure is `Unknown` (no free-form text).
fn failure_for(err: CommandError) -> Failure {
    let kind = match err {
        CommandError::Timeout { .. } => FailureKind::Timeout,
        _ => FailureKind::Unknown,
    };
    Failure {
        stage: Stage::Detect,
        kind,
    }
}

/// Run the Detect lane against `runner`, returning a (possibly partial)
/// [`DetectOutcome`]. See the module docs for the full hardening contract.
///
/// `host_system` is the host's pinned Darwin system (or `None` off-macOS);
/// production callers pass [`host_system`], tests pass a canonical Darwin
/// system. When it is `None` a closed Detect/Unknown failure is recorded so the
/// lane is Incomplete (a live Detect must NEVER reach Complete off-macOS); the
/// partial Observed observation is still returned.
pub fn detect(
    runner: &dyn ProbeRunner,
    nix_bin: Option<&Path>,
    host_system: Option<&str>,
) -> DetectOutcome {
    let tc = detect_tool_capabilities(runner);
    // Read the probe gates BEFORE `..tc` moves `tc` into the observation.
    let xcrun_present = tc.xcrun;
    let security_present = tc.security;
    let mut obs = DetectObservation {
        source: EvidenceSource::Observed,
        host_system: host_system.map(String::from),
        nix_present: nix_present(runner, nix_bin),
        tool_capabilities: ToolCapabilities {
            notarytool: false,
            ..tc
        },
        xcode_selection: XcodeSelection::Absent,
        application_identity_count: 0,
        installer_identity_count: 0,
        nixbld_group_present: false,
        nixbld_user_count: 0,
    };
    let mut failures = Vec::new();

    // Off-macOS (host outside the pinned Darwin set): a live Detect must NEVER
    // become Complete/success, because the macOS signing/notarization
    // capability evidence is meaningless off-macOS. We do NOT invent a host
    // system value; instead the partial Observed observation is retained AND a
    // closed Detect/Unknown failure is recorded so detect_report builds an
    // Incomplete lane (CLI exits 69 after writing both artifacts).
    if host_system.is_none() {
        failures.push(Failure {
            stage: Stage::Detect,
            kind: FailureKind::Unknown,
        });
    }

    // xcode-select -p (only if the fixed tool exists). Nonzero/Absent is a
    // capability, not a failure.
    if runner.probe_is_file(Path::new(XCODE_SELECT)) {
        match runner.run_probe(&xcode_select_spec()) {
            Ok(outcome) => {
                obs.xcode_selection = parse_xcode_selection(&outcome.stdout, outcome.is_success())
            }
            Err(e) => failures.push(failure_for(e)),
        }
    }

    // xcrun --find notarytool (only if xcrun exists). Nonzero ⇒ notarytool
    // absent (capability); only timeout/cap is a failure.
    if xcrun_present {
        match runner.run_probe(&notarytool_spec()) {
            Ok(outcome) => obs.tool_capabilities.notarytool = outcome.is_success(),
            Err(e) => {
                obs.tool_capabilities.notarytool = false;
                failures.push(failure_for(e));
            }
        }
    }

    // security find-identity (only if security exists). ONLY a zero exit is
    // parsed: real `security find-identity -v` exits 0 even with ZERO identities
    // (it prints "0 valid identities found"), so a nonzero exit or a signal is
    // an internal failure, NOT identity absence. Non-UTF-8 on a zero-exit output
    // is a malformed failure. In every failure case the count for that class
    // stays zero.
    if security_present {
        match runner.run_probe(&security_application_spec()) {
            Ok(outcome) if outcome.is_success() => {
                match parse_identity_count(&outcome.stdout, APPLICATION_NEEDLE) {
                    Ok(c) => obs.application_identity_count = c,
                    Err(()) => failures.push(malformed_failure()),
                }
            }
            Ok(_) => failures.push(identity_probe_failure()),
            Err(e) => failures.push(failure_for(e)),
        }
        match runner.run_probe(&security_installer_spec()) {
            Ok(outcome) if outcome.is_success() => {
                match parse_identity_count(&outcome.stdout, INSTALLER_NEEDLE) {
                    Ok(c) => obs.installer_identity_count = c,
                    Err(()) => failures.push(malformed_failure()),
                }
            }
            Ok(_) => failures.push(identity_probe_failure()),
            Err(e) => failures.push(failure_for(e)),
        }
    }

    // dscl . -read /Groups/_nixbld GroupMembership (only if dscl exists).
    // Nonzero/signal ⇒ group absent (capability, NO failure); zero-exit + valid
    // UTF-8 ⇒ present + bounded count; zero-exit + non-UTF-8 ⇒ a closed
    // Detect/Unknown malformed failure with presence conservatively false. The
    // closed [`NixbldOutcome`] makes the malformed case impossible to silently
    // accept: the parser cannot return a bare `(false, 0)` tuple.
    if runner.probe_is_file(Path::new(DSCL)) {
        match runner.run_probe(&dscl_spec()) {
            Ok(outcome) => match parse_nixbld(&outcome.stdout, outcome.is_success()) {
                NixbldOutcome::Absent => {}
                NixbldOutcome::Present(count) => {
                    obs.nixbld_group_present = true;
                    obs.nixbld_user_count = count;
                }
                NixbldOutcome::Malformed => failures.push(malformed_failure()),
            },
            Err(e) => failures.push(failure_for(e)),
        }
    }

    DetectOutcome {
        observation: obs,
        failures,
    }
}

/// A malformed-output failure (non-UTF-8 tool output): closed kind, no raw text.
fn malformed_failure() -> Failure {
    Failure {
        stage: Stage::Detect,
        kind: FailureKind::Unknown,
    }
}

/// A `security find-identity` probe that exited NONZERO or was signaled: a
/// closed Detect/Unknown failure. The real tool exits 0 even for zero identities
/// ("0 valid identities found"), so a nonzero/signal outcome is an internal
/// failure, NOT identity absence; the count for that class stays zero. No raw
/// output is retained (same Failure shape as [`malformed_failure`]).
fn identity_probe_failure() -> Failure {
    Failure {
        stage: Stage::Detect,
        kind: FailureKind::Unknown,
    }
}

// ---- pure parsers (no I/O; unit-tested with transcripts) -------------------

/// Classify the active developer directory from bounded `xcode-select -p`
/// stdout WITHOUT storing the path. A nonzero exit, or an exit-0 stdout that
/// matches neither marker, is `Absent` (conservative). Only byte-substring
/// matching is used, so non-UTF-8 never matters here.
pub(crate) fn parse_xcode_selection(stdout: &[u8], zero_exit: bool) -> XcodeSelection {
    if !zero_exit {
        return XcodeSelection::Absent;
    }
    if contains_substring(stdout, b"CommandLineTools") {
        XcodeSelection::CommandLineTools
    } else if contains_substring(stdout, b"Xcode.app") {
        XcodeSelection::FullXcode
    } else {
        XcodeSelection::Absent
    }
}

/// Count valid `security find-identity -v` identity RECORD lines of the given
/// `class` (a `Developer ID …` needle), saturating at [`MAX_IDENTITY_COUNT`].
///
/// A line counts ONLY if it matches the bounded valid record shape emitted by
/// the real `security` tool (see [`is_identity_record`]): optional leading
/// whitespace, a decimal ordinal, `')'`, whitespace, EXACTLY 40 ASCII-hex
/// fingerprint characters, whitespace, then a QUOTED identity whose content
/// begins EXACTLY with `needle` followed by `':'` AND ends with a closing
/// double quote (optional trailing whitespace only). Arbitrary text — error
/// diagnostics, the trailing "N valid identities found" summary, a truncated
/// record missing its closing quote, or prose that merely contains the class
/// name — NEVER matches, so a count can only be inflated by genuine,
/// well-shaped identity records.
///
/// No identity NAME, fingerprint, or raw output is retained: only the count.
/// Returns `Err(())` if the stdout is not valid UTF-8.
pub(crate) fn parse_identity_count(stdout: &[u8], needle: &[u8]) -> Result<u16, ()> {
    if std::str::from_utf8(stdout).is_err() {
        return Err(());
    }
    let mut count: u32 = 0;
    for line in stdout.split(|b| *b == b'\n') {
        if is_identity_record(line, needle) {
            count = count.saturating_add(1);
        }
    }
    Ok(count.min(MAX_IDENTITY_COUNT) as u16)
}

/// `true` iff `line` is a bounded valid `security find-identity -v` record of
/// the given `class`. Grammar (byte-level, no allocation, no retained text; the
/// caller has already validated UTF-8):
///
/// ```text
///   <optional ws> <decimal ordinal> ')' <ws+> <exactly 40 ascii-hex> <ws+>
///   '"' <identity beginning exactly with `class` ':'> ... '"' <optional ws>
/// ```
///
/// The identity NAME and fingerprint are matched only to establish the bounded
/// shape; they are never copied out. A missing opening OR closing quote, a
/// non-hex or wrong-length fingerprint, the wrong class, no ordinal, or any
/// trailing non-whitespace garbage after the closing quote all reject the line.
fn is_identity_record(line: &[u8], needle: &[u8]) -> bool {
    let mut rest = line;
    // optional leading whitespace
    while let Some((&b, r)) = rest.split_first() {
        if b.is_ascii_whitespace() {
            rest = r;
        } else {
            break;
        }
    }
    // decimal ordinal (one or more ASCII digits)
    let mut digits = 0;
    while let Some((&b, r)) = rest.split_first() {
        if b.is_ascii_digit() {
            digits += 1;
            rest = r;
        } else {
            break;
        }
    }
    if digits == 0 {
        return false;
    }
    // literal ')'
    match rest.split_first() {
        Some((b')', r)) => rest = r,
        _ => return false,
    }
    // required whitespace, then EXACTLY 40 ASCII-hex fingerprint characters
    rest = match strip_required_ws(rest) {
        Some(r) => r,
        None => return false,
    };
    if rest.len() < 40 || !rest[..40].iter().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    rest = &rest[40..];
    // required whitespace, then a quoted identity beginning EXACTLY with
    // `needle` + ':'
    rest = match strip_required_ws(rest) {
        Some(r) => r,
        None => return false,
    };
    match rest.split_first() {
        Some((b'"', r)) => rest = r,
        _ => return false,
    }
    if rest.len() < needle.len() + 1 {
        return false;
    }
    if !rest.starts_with(needle) || rest[needle.len()] != b':' {
        return false;
    }
    rest = &rest[needle.len() + 1..];
    // A quoted identity record MUST include a closing double quote somewhere
    // after the exact `class`+`':'` prefix; the bytes between the colon and the
    // closing quote are the identity NAME (matched only to bound the shape,
    // never copied out). A truncated/unclosed record is rejected.
    match rest.iter().position(|&b| b == b'"') {
        Some(idx) => rest = &rest[idx + 1..],
        None => return false,
    }
    // After the closing quote ONLY optional ASCII whitespace is accepted: any
    // trailing garbage rejects the line.
    rest.iter().all(|b| b.is_ascii_whitespace())
}

/// Strip one-or-more leading ASCII whitespace; `None` if none was present (the
/// grammar requires whitespace in those positions).
fn strip_required_ws(mut bytes: &[u8]) -> Option<&[u8]> {
    let mut consumed = 0;
    while let Some((&b, r)) = bytes.split_first() {
        if b.is_ascii_whitespace() {
            bytes = r;
            consumed += 1;
        } else {
            break;
        }
    }
    if consumed == 0 { None } else { Some(bytes) }
}

/// The closed outcome of parsing a `_nixbld` `GroupMembership` probe. This is
/// the SINGLE source of truth for how `dscl` output maps to the
/// `(present, count)` pair on the observation AND whether the Detect lane must
/// record a closed failure: the parser never returns a bare tuple the
/// orchestrator could silently accept as complete on malformed output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NixbldOutcome {
    /// Capability absence: a nonzero exit OR a signal termination. The group is
    /// absent, the member count is zero, and NO failure is recorded (a host
    /// without `_nixbld` simply lacks the build-user group).
    Absent,
    /// Group present with a bounded member count. Covers both a real
    /// `GroupMembership:` line and the deliberate exit-0-but-no-line case (the
    /// group record exists, the attribute is empty ⇒ `count == 0`).
    Present(u16),
    /// Zero-exit output that was NOT valid UTF-8. A closed Detect/Unknown
    /// failure: presence is conservatively `false` and the count is zero.
    Malformed,
}

/// Parse `_nixbld` group presence and member COUNT from bounded `dscl …
/// GroupMembership` stdout into a closed [`NixbldOutcome`]:
///   * nonzero exit / signal ⇒ [`NixbldOutcome::Absent`] (group absent, zero
///     members, NO failure — capability absence);
///   * zero exit + valid UTF-8 `GroupMembership:` line ⇒ [`NixbldOutcome::Present`]
///     with the whitespace-separated token count after the colon, saturating at
///     [`MAX_IDENTITY_COUNT`];
///   * zero exit + valid UTF-8 with NO `GroupMembership:` line ⇒
///     [`NixbldOutcome::Present`]`(0)` (deliberate existing behavior: the group
///     record exists, the attribute is empty);
///   * zero exit + NON-UTF-8 ⇒ [`NixbldOutcome::Malformed`] (the orchestrator
///     records a closed Detect/Unknown failure and conservatively reports
///     presence `false`, count zero).
pub(crate) fn parse_nixbld(stdout: &[u8], zero_exit: bool) -> NixbldOutcome {
    if !zero_exit {
        return NixbldOutcome::Absent;
    }
    if std::str::from_utf8(stdout).is_err() {
        return NixbldOutcome::Malformed;
    }
    // Find the GroupMembership: line and count tokens after the colon.
    let mut count: u32 = 0;
    for line in stdout.split(|b| *b == b'\n') {
        if let Some(idx) = find_substring(line, b"GroupMembership:") {
            let rest = &line[idx + b"GroupMembership:".len()..];
            for tok in rest.split(|b| b.is_ascii_whitespace()) {
                if !tok.is_empty() {
                    count = count.saturating_add(1);
                }
            }
            return NixbldOutcome::Present(count.min(MAX_IDENTITY_COUNT) as u16);
        }
    }
    // Exit 0 but no GroupMembership line: the group record exists but the
    // attribute is absent/empty — present true, zero members.
    NixbldOutcome::Present(0)
}

/// Needle-in-haystack substring search over raw bytes (no allocation).
fn contains_substring(haystack: &[u8], needle: &[u8]) -> bool {
    find_substring(haystack, needle).is_some()
}

fn find_substring(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub(crate) struct FakeProbeRunner {
    /// Scripted probe results keyed by `(program, args)`.
    scripts: Vec<(PathBuf, Vec<OsString>, Result<CommandOutcome, CommandError>)>,
    /// Paths reported as existing files by `probe_is_file`.
    files: std::collections::BTreeSet<PathBuf>,
}

#[cfg(test)]
impl FakeProbeRunner {
    pub(crate) fn new() -> Self {
        Self {
            scripts: Vec::new(),
            files: std::collections::BTreeSet::new(),
        }
    }

    /// Mark all fixed tool/program paths as existing files (the happy-path
    /// default). Tests override individual paths with [`Self::set_file`].
    pub(crate) fn mark_all_tools_present(&mut self) {
        for p in [
            TOOL_CODESIGN,
            XCRUN,
            TOOL_STAPLER,
            TOOL_PRODUCTBUILD,
            TOOL_PRODUCTSIGN,
            TOOL_PKGBUILD,
            TOOL_SPCTL,
            SECURITY,
            XCODE_SELECT,
            DSCL,
        ] {
            self.files.insert(Path::new(p).to_path_buf());
        }
    }

    pub(crate) fn set_file(&mut self, path: &Path, exists: bool) {
        if exists {
            self.files.insert(path.to_path_buf());
        } else {
            self.files.remove(path);
        }
    }

    /// Script the result of one probe, matched by the spec's `(program, args)`.
    /// A repeated script for the same `(program, args)` OVERRIDES the prior one,
    /// so a test can fully script a runner and then flip a single probe.
    pub(crate) fn set_probe(
        &mut self,
        spec: &CommandSpec,
        result: Result<CommandOutcome, CommandError>,
    ) {
        for entry in &mut self.scripts {
            if entry.0 == spec.program && entry.1 == spec.args {
                entry.2 = result;
                return;
            }
        }
        self.scripts
            .push((spec.program.clone(), spec.args.clone(), result));
    }
}

#[cfg(test)]
impl ProbeRunner for FakeProbeRunner {
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        spec.validate()
            .map_err(crate::command::CommandError::Spec)?;
        for (program, args, result) in &self.scripts {
            if program == &spec.program && args == &spec.args {
                return result.clone();
            }
        }
        Err(CommandError::Spawn {
            kind: std::io::ErrorKind::NotFound,
        })
    }

    fn probe_is_file(&self, path: &Path) -> bool {
        self.files.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::ProbeStatus;
    use crate::report::Observation;

    fn ok_zero(stdout: &[u8]) -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Exited(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            stdout_total_bytes: stdout.len() as u64,
            stderr_total_bytes: 0,
            wall_ms: 1,
        }
    }
    fn nonzero(code: i32) -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Exited(code),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            wall_ms: 1,
        }
    }

    // ---- test fixtures: canonical 40-char ASCII-hex fingerprints --------
    //
    // The real `security find-identity -v` emits 40-char SHA-1 fingerprints.
    // Tests use exactly that shape so the hardened parser is exercised
    // faithfully. These values are INPUT only: parse_identity_count retains
    // nothing but the count, and the PII assertions below confirm that.
    const APP_FP: &str = "0123456789abcdef0123456789abcdef01234567";
    const INST_FP: &str = "fedcba9876543210fedcba9876543210fedcba98";

    /// Build one `security find-identity -v` record line with a 40-char hex
    /// fingerprint and a quoted identity of the given `class`.
    fn identity_line(fp: &str, class: &str, name: &str) -> Vec<u8> {
        format!("  1) {fp} \"{class}: {name}\"\n").into_bytes()
    }

    /// A canonical pinned Darwin host passed into [`detect`] so the injected
    /// (FakeProbeRunner) capability-absence path constructs a Complete
    /// observation regardless of the real test host platform.
    const TEST_HOST: Option<&str> = Some("aarch64-darwin");

    // ---- pure parsers -------------------------------------------------------

    #[test]
    fn xcode_select_parses_markers_and_absent() {
        assert_eq!(
            parse_xcode_selection(b"/Library/Developer/CommandLineTools\n", true),
            XcodeSelection::CommandLineTools
        );
        assert_eq!(
            parse_xcode_selection(b"/Applications/Xcode.app/Contents/Developer\n", true),
            XcodeSelection::FullXcode
        );
        // Nonzero exit is Absent regardless of stdout.
        assert_eq!(
            parse_xcode_selection(b"/Applications/Xcode.app/Contents/Developer", false),
            XcodeSelection::Absent
        );
        // Unmatched exit-0 is conservatively Absent.
        assert_eq!(
            parse_xcode_selection(b"garbage\n", true),
            XcodeSelection::Absent
        );
        assert_eq!(parse_xcode_selection(b"", true), XcodeSelection::Absent);
    }

    #[test]
    fn identity_count_counts_only_well_shaped_records() {
        // Two genuine records (one Application, one Installer) + the trailing
        // summary line. Each class counts exactly its own records.
        let mixed = format!(
            "  1) {APP_FP} \"Developer ID Application: Acme (TEAM1)\"\n\
             2) {INST_FP} \"Developer ID Installer: Acme (TEAM1)\"\n\
             2 valid identities found\n"
        );
        assert_eq!(
            parse_identity_count(mixed.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            1
        );
        assert_eq!(
            parse_identity_count(mixed.as_bytes(), INSTALLER_NEEDLE).unwrap(),
            1
        );
        // An installer-only list counts zero Application.
        let only_inst = identity_line(INST_FP, "Developer ID Installer", "Acme (TEAM1)");
        assert_eq!(
            parse_identity_count(&only_inst, APPLICATION_NEEDLE).unwrap(),
            0
        );
        // Empty input and a zero-identity summary both count zero.
        assert_eq!(parse_identity_count(b"", APPLICATION_NEEDLE).unwrap(), 0);
        assert_eq!(
            parse_identity_count(b"     0 valid identities found\n", APPLICATION_NEEDLE).unwrap(),
            0
        );
    }

    #[test]
    fn identity_count_accepts_uppercase_and_lowercase_hex() {
        // Both cases are valid ASCII-hex; both records count.
        let upper = identity_line(
            "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
            "Developer ID Application",
            "Acme (TEAM1)",
        );
        let lower = identity_line(
            "0123456789abcdef0123456789abcdef01234567",
            "Developer ID Application",
            "Acme (TEAM2)",
        );
        let both = [&upper[..], &lower[..]].concat();
        assert_eq!(parse_identity_count(&both, APPLICATION_NEEDLE).unwrap(), 2);
    }

    #[test]
    fn identity_count_rejects_non_record_text_and_wrong_shapes() {
        // (a) Error/diagnostic prose that merely MENTIONS the class name: the
        // old substring parser would have counted this; the bounded parser must
        // not.
        let err = b"security: SecKeychainSearchList returned -25300\n\
             note: no 'Developer ID Application' identities usable\n";
        assert_eq!(parse_identity_count(err, APPLICATION_NEEDLE).unwrap(), 0);

        // (b) Unquoted identity (no opening quote after the fingerprint).
        let unquoted = format!("  1) {APP_FP} Developer ID Application: Acme (TEAM1)\n");
        assert_eq!(
            parse_identity_count(unquoted.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (c) Wrong-length fingerprint (4 hex chars, not 40).
        let short_fp = b"  1) AAAA \"Developer ID Application: Acme (TEAM1)\"\n";
        assert_eq!(
            parse_identity_count(short_fp, APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (d) 40 chars but NOT all hex.
        let non_hex = b"  1) zzzz56789abcdef0123456789abcdef01234567 \
\"Developer ID Application: Acme (TEAM1)\"\n";
        assert_eq!(
            parse_identity_count(non_hex, APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (e) Right shape but the WRONG class (Installer record vs Application
        // needle) and a missing colon after the class.
        let wrong_class = identity_line(INST_FP, "Developer ID Installer", "Acme (TEAM1)");
        assert_eq!(
            parse_identity_count(&wrong_class, APPLICATION_NEEDLE).unwrap(),
            0
        );
        let no_colon = format!("  1) {APP_FP} \"Developer ID Application Acme\"\n");
        assert_eq!(
            parse_identity_count(no_colon.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (f) Missing ordinal / ')' / whitespace around the fingerprint.
        assert_eq!(
            parse_identity_count(b"Developer ID Application: Acme\n", APPLICATION_NEEDLE).unwrap(),
            0
        );
    }

    #[test]
    fn identity_count_saturates_at_max() {
        // Far more valid records than MAX_IDENTITY_COUNT saturate there.
        let one = identity_line(APP_FP, "Developer ID Application", "Acme (TEAM1)");
        let mut huge = Vec::new();
        for _ in 0..(MAX_IDENTITY_COUNT + 10) {
            huge.extend_from_slice(&one);
        }
        assert_eq!(
            parse_identity_count(&huge, APPLICATION_NEEDLE).unwrap(),
            MAX_IDENTITY_COUNT as u16
        );
    }

    #[test]
    fn identity_count_rejects_non_utf8() {
        let bad: &[u8] = b"\xff\xfe Developer ID Application";
        assert!(parse_identity_count(bad, APPLICATION_NEEDLE).is_err());
    }

    #[test]
    fn identity_count_requires_closing_quote_and_rejects_trailing_garbage() {
        // (a) Truncated/unclosed: opening quote + exact class+colon prefix but
        // NO closing double quote. The old parser accepted this; the hardened
        // parser must not (a truncated transcript could otherwise inflate the
        // count).
        let unclosed = format!("  1) {APP_FP} \"Developer ID Application: Acme (TEAM1)\n");
        assert_eq!(
            parse_identity_count(unclosed.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );
        // Even a long body with no closing quote is rejected.
        let unclosed_long = format!("  1) {APP_FP} \"Developer ID Application: Acme (TEAM1) (more");
        assert_eq!(
            parse_identity_count(unclosed_long.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (b) Trailing garbage: any non-whitespace byte after the closing quote
        // rejects the line.
        let trailing_byte = format!("  1) {APP_FP} \"Developer ID Application: Acme\"X\n");
        assert_eq!(
            parse_identity_count(trailing_byte.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );
        let trailing_word = format!("  1) {APP_FP} \"Developer ID Application: Acme\" extra\n");
        assert_eq!(
            parse_identity_count(trailing_word.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            0
        );

        // (c) Control: well-formed records still count, with optional trailing
        // whitespace, and both Developer ID classes are preserved.
        let clean = format!("  1) {APP_FP} \"Developer ID Application: Acme\"\n");
        assert_eq!(
            parse_identity_count(clean.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            1
        );
        let clean_ws = format!("  1) {APP_FP} \"Developer ID Application: Acme\"   \n");
        assert_eq!(
            parse_identity_count(clean_ws.as_bytes(), APPLICATION_NEEDLE).unwrap(),
            1
        );
        let inst = identity_line(INST_FP, "Developer ID Installer", "Acme (TEAM1)");
        assert_eq!(parse_identity_count(&inst, INSTALLER_NEEDLE).unwrap(), 1);

        // (d) No raw identity name/team/fingerprint is retained.
        let blob = format!(
            "{:?}",
            parse_identity_count(clean.as_bytes(), APPLICATION_NEEDLE).unwrap()
        );
        assert!(!blob.contains("Developer ID"));
        assert!(!blob.contains("Acme"));
        assert!(!blob.contains(APP_FP));
    }

    #[test]
    fn nixbld_parses_presence_and_member_count() {
        let out = b"GroupMembership: _nixbld1 _nixbld2 _nixbld3\n";
        assert_eq!(parse_nixbld(out, true), NixbldOutcome::Present(3));
        // Extra whitespace collapses to the same token count.
        let out = b"GroupMembership:   _nixbld1   _nixbld2  \n";
        assert_eq!(parse_nixbld(out, true), NixbldOutcome::Present(2));
        // No members line but exit 0: present, zero members (deliberate
        // existing behavior — the group record exists, attribute is empty).
        assert_eq!(parse_nixbld(b"nope\n", true), NixbldOutcome::Present(0));
        // Nonzero exit / signal: capability absence, NOT a failure.
        assert_eq!(parse_nixbld(b"error\n", false), NixbldOutcome::Absent);
    }

    #[test]
    fn nixbld_zero_exit_non_utf8_is_malformed() {
        // Zero-exit NON-UTF-8 is a closed Malformed outcome (presence false,
        // count zero) — NOT a silent (false, 0) acceptance.
        assert_eq!(parse_nixbld(&[0xff, 0xfe], true), NixbldOutcome::Malformed);
        // Malformed even when a GroupMembership-looking prefix appears, so a
        // hostile/truncated transcript can never inflate the count or flip
        // presence on non-UTF-8 output.
        let bad = b"GroupMembership: _nixbld1 \xff\xfe\n";
        assert_eq!(parse_nixbld(bad, true), NixbldOutcome::Malformed);
        // Non-UTF-8 on a NONZERO exit stays Absent: capability absence is the
        // first gate, and non-UTF-8 is only a failure on a zero exit.
        assert_eq!(parse_nixbld(&[0xff, 0xfe], false), NixbldOutcome::Absent);
    }

    // ---- orchestration via FakeProbeRunner ---------------------------------

    fn runner_with_tools() -> FakeProbeRunner {
        let mut r = FakeProbeRunner::new();
        r.mark_all_tools_present();
        r
    }

    #[test]
    fn detect_complete_happy_path_records_capabilities_and_counts() {
        let mut r = runner_with_tools();
        r.set_probe(
            &xcode_select_spec(),
            Ok(ok_zero(b"/Applications/Xcode.app/Contents/Developer\n")),
        );
        r.set_probe(&notarytool_spec(), Ok(ok_zero(b"/path/to/notarytool\n")));
        r.set_probe(
            &security_application_spec(),
            Ok(ok_zero(&identity_line(
                APP_FP,
                "Developer ID Application",
                "Acme (TEAM1)",
            ))),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(&identity_line(
                INST_FP,
                "Developer ID Installer",
                "Acme (TEAM1)",
            ))),
        );
        r.set_probe(
            &dscl_spec(),
            Ok(ok_zero(b"GroupMembership: _nixbld1 _nixbld2\n")),
        );
        r.set_file(Path::new("/abs/nix"), true);

        let out = detect(&r, Some(Path::new("/abs/nix")), TEST_HOST);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        let obs = &out.observation;
        assert!(obs.nix_present);
        assert_eq!(obs.xcode_selection, XcodeSelection::FullXcode);
        assert!(obs.tool_capabilities.notarytool);
        assert!(obs.tool_capabilities.codesign);
        assert_eq!(obs.application_identity_count, 1);
        assert_eq!(obs.installer_identity_count, 1);
        assert!(obs.nixbld_group_present);
        assert_eq!(obs.nixbld_user_count, 2);
        assert_eq!(obs.source, EvidenceSource::Observed);
        // No identity NAME, team id, or fingerprint reaches the observation.
        let blob = format!("{obs:?}");
        assert!(!blob.contains("Developer ID"));
        assert!(!blob.contains("TEAM1"));
        assert!(!blob.contains("0123456789abcdef"));
        assert!(!blob.contains("fedcba9876543"));
    }

    #[test]
    fn detect_capability_absence_is_complete_not_failure() {
        // xcode-select nonzero (no Xcode) ⇒ Absent (not a failure).
        // notarytool nonzero ⇒ notarytool absent (not a failure).
        // security ZERO-exit "0 valid identities found" ⇒ zero identities, a
        //   genuine Complete capability-absence observation (NOT a failure).
        // dscl nonzero ⇒ group absent (not a failure).
        let mut r = runner_with_tools();
        r.set_probe(&xcode_select_spec(), Ok(nonzero(2)));
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(
            &security_application_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(&dscl_spec(), Ok(nonzero(1)));

        let out = detect(&r, None, TEST_HOST);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.observation.xcode_selection, XcodeSelection::Absent);
        assert!(!out.observation.tool_capabilities.notarytool);
        assert_eq!(out.observation.application_identity_count, 0);
        assert_eq!(out.observation.installer_identity_count, 0);
        assert!(!out.observation.nixbld_group_present);
        assert_eq!(out.observation.nixbld_user_count, 0);
    }

    #[test]
    fn detect_missing_fixed_tools_are_absence_not_failure() {
        // Remove xcrun, security, xcode-select, dscl entirely. The dependent
        // probes are SKIPPED (capability absence), never spawn, never fail.
        let mut r = FakeProbeRunner::new();
        r.set_file(Path::new(TOOL_CODESIGN), true);
        r.set_file(Path::new(TOOL_STAPLER), true);
        // xcrun absent ⇒ notarytool probe skipped.
        // security absent ⇒ security probes skipped.
        // xcode-select absent ⇒ xcode-select probe skipped.
        // dscl absent ⇒ dscl probe skipped.
        let out = detect(&r, None, TEST_HOST);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert!(!out.observation.tool_capabilities.xcrun);
        assert!(!out.observation.tool_capabilities.security);
        assert!(!out.observation.tool_capabilities.notarytool);
        assert_eq!(out.observation.xcode_selection, XcodeSelection::Absent);
        assert!(!out.observation.nixbld_group_present);
        assert!(out.observation.tool_capabilities.codesign);
    }

    #[test]
    fn detect_timeout_is_incomplete_failure() {
        let mut r = runner_with_tools();
        r.set_probe(
            &xcode_select_spec(),
            Err(CommandError::Timeout { killed: true }),
        );
        // Script the remaining probes as benign absence so ONLY the xcode-select
        // timeout surfaces as a failure. The security probes use ZERO-exit "0
        // valid identities" (genuine absence), NOT nonzero — a nonzero security
        // exit is itself a failure under the hardened identity-probe contract.
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(
            &security_application_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(&dscl_spec(), Ok(nonzero(1)));
        let out = detect(&r, None, TEST_HOST);
        assert_eq!(out.failures.len(), 1);
        assert_eq!(
            out.failures[0],
            Failure {
                stage: Stage::Detect,
                kind: FailureKind::Timeout
            }
        );
        // Partial observation still validates (defaults).
        out.observation.validate().unwrap();
    }

    #[test]
    fn detect_cap_overflow_and_malformed_are_incomplete() {
        let mut r = runner_with_tools();
        // Script the non-target probes as benign absence so ONLY the two
        // security-probe failures (cap overflow + malformed output) surface.
        r.set_probe(&xcode_select_spec(), Ok(nonzero(2)));
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(
            &security_application_spec(),
            Err(CommandError::CapOverflow {
                stream: crate::command::Stream::Stdout,
            }),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(&[0xff, 0xfe])), // non-UTF-8 ⇒ malformed
        );
        r.set_probe(&dscl_spec(), Ok(nonzero(1)));
        let out = detect(&r, None, TEST_HOST);
        assert_eq!(out.failures.len(), 2);
        for f in &out.failures {
            assert_eq!(f.stage, Stage::Detect);
            assert_eq!(f.kind, FailureKind::Unknown);
        }
    }

    #[test]
    fn detect_off_macos_is_incomplete_even_on_full_happy_path() {
        // On a host outside the pinned Darwin set (`host_system` is `None`), a
        // live Detect must NEVER reach Complete. Even with every probe happy,
        // a single closed Detect/Unknown failure is recorded so the lane is
        // Incomplete; the partial Observed observation is retained and still
        // validates. No Linux host system value is invented.
        let mut r = runner_with_tools();
        r.set_probe(
            &xcode_select_spec(),
            Ok(ok_zero(b"/Applications/Xcode.app/Contents/Developer\n")),
        );
        r.set_probe(&notarytool_spec(), Ok(ok_zero(b"/path/to/notarytool\n")));
        r.set_probe(
            &security_application_spec(),
            Ok(ok_zero(&identity_line(
                APP_FP,
                "Developer ID Application",
                "Acme (TEAM1)",
            ))),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(&identity_line(
                INST_FP,
                "Developer ID Installer",
                "Acme (TEAM1)",
            ))),
        );
        r.set_probe(
            &dscl_spec(),
            Ok(ok_zero(b"GroupMembership: _nixbld1 _nixbld2\n")),
        );

        let out = detect(&r, None, None);
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert_eq!(
            out.failures[0],
            Failure {
                stage: Stage::Detect,
                kind: FailureKind::Unknown
            }
        );
        // The partial Observed observation is retained, with NO host system.
        let obs = &out.observation;
        assert_eq!(obs.source, EvidenceSource::Observed);
        assert!(obs.host_system.is_none());
        // The happy-path capabilities/counts were still gathered.
        assert_eq!(obs.xcode_selection, XcodeSelection::FullXcode);
        assert_eq!(obs.application_identity_count, 1);
        assert_eq!(obs.installer_identity_count, 1);
        // And it still validates as an Incomplete-payload observation.
        obs.validate().unwrap();
    }

    #[test]
    fn detect_security_nonzero_exit_is_failure_not_identity_absence() {
        // `security find-identity -v` exits 0 even for zero identities; a nonzero
        // exit (or signal) is therefore an internal failure for BOTH identity
        // probes, and the counts stay zero. This is distinct from genuine
        // identity absence (zero-exit "0 valid identities found"), which remains
        // a Complete capability-absence observation.
        let mut r = runner_with_tools();
        r.set_probe(&xcode_select_spec(), Ok(nonzero(2)));
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(&security_application_spec(), Ok(nonzero(1)));
        r.set_probe(&security_installer_spec(), Ok(nonzero(1)));
        r.set_probe(&dscl_spec(), Ok(nonzero(1)));

        let out = detect(&r, None, TEST_HOST);
        assert_eq!(out.failures.len(), 2, "{:?}", out.failures);
        for f in &out.failures {
            assert_eq!(f.stage, Stage::Detect);
            assert_eq!(f.kind, FailureKind::Unknown);
        }
        // Counts stay zero on the nonzero-exit path.
        assert_eq!(out.observation.application_identity_count, 0);
        assert_eq!(out.observation.installer_identity_count, 0);
    }

    #[test]
    fn detect_security_signal_is_failure_not_identity_absence() {
        // A signal termination (not a nonzero exit) of a security probe is the
        // same internal failure; the count stays zero.
        let signaled = CommandOutcome {
            status: ProbeStatus::Signaled(9),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            wall_ms: 1,
        };
        let mut r = runner_with_tools();
        r.set_probe(&xcode_select_spec(), Ok(nonzero(2)));
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(&security_application_spec(), Ok(signaled.clone()));
        r.set_probe(&security_installer_spec(), Ok(signaled));
        r.set_probe(&dscl_spec(), Ok(nonzero(1)));

        let out = detect(&r, None, TEST_HOST);
        assert_eq!(out.failures.len(), 2, "{:?}", out.failures);
        for f in &out.failures {
            assert_eq!(f.kind, FailureKind::Unknown);
        }
        assert_eq!(out.observation.application_identity_count, 0);
        assert_eq!(out.observation.installer_identity_count, 0);
    }

    #[test]
    fn detect_dscl_zero_exit_non_utf8_is_malformed_failure() {
        // A zero-exit dscl probe with NON-UTF-8 output yields exactly one closed
        // Detect/Unknown malformed failure; presence stays false and the count
        // stays zero. The closed NixbldOutcome makes this impossible to silently
        // accept as a (false, 0) capability absence.
        let mut r = runner_with_tools();
        r.set_probe(&xcode_select_spec(), Ok(nonzero(2)));
        r.set_probe(&notarytool_spec(), Ok(nonzero(1)));
        r.set_probe(
            &security_application_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(
            &security_installer_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(&dscl_spec(), Ok(ok_zero(&[0xff, 0xfe, b'A', b'B'])));
        let out = detect(&r, None, TEST_HOST);
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert_eq!(
            out.failures[0],
            Failure {
                stage: Stage::Detect,
                kind: FailureKind::Unknown,
            }
        );
        assert!(!out.observation.nixbld_group_present);
        assert_eq!(out.observation.nixbld_user_count, 0);
    }

    #[test]
    fn detect_nix_present_only_for_absolute_existing_file() {
        let mut r = FakeProbeRunner::new();
        // Relative / empty / missing ⇒ false even if a file is scripted.
        assert!(!nix_present(&r, None));
        assert!(!nix_present(&r, Some(Path::new("nix"))));
        assert!(!nix_present(&r, Some(Path::new(""))));
        r.set_file(Path::new("/abs/nix"), true);
        assert!(nix_present(&r, Some(Path::new("/abs/nix"))));
        // A relative path is never consulted even if it would match.
        r.set_file(Path::new("rel"), true);
        assert!(!nix_present(&r, Some(Path::new("rel"))));
    }

    #[test]
    fn host_system_is_a_pinned_darwin_string_or_none() {
        if let Some(s) = host_system() {
            assert!(s == "aarch64-darwin" || s == "x86_64-darwin");
        }
    }

    #[test]
    fn fixed_probe_specs_use_absolute_programs() {
        for spec in [
            xcode_select_spec(),
            notarytool_spec(),
            security_application_spec(),
            security_installer_spec(),
            dscl_spec(),
        ] {
            assert!(spec.program.is_absolute(), "{:?}", spec.program);
            assert!(spec.timeout <= Duration::from_secs(30));
        }
    }
}
