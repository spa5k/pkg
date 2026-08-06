//! Spike S4 (PR-6 / DR-004) — RUNNER: pure assembly helpers + the public Fake
//! fixture-subprocess loop.
//!
//! This module holds two things. First, a set of PURE, allocation-only assembly
//! helpers: the compile-time host-target-to-`system` mapping, the deterministic
//! fail-closed child environment, the exact scenario descriptors,
//! [`CommandOutcome`]-to-[`Sample`] / [`Scenario`] folding, and Fake report
//! assembly. Second, the PUBLIC [`run_fake`] entry point that drives those
//! helpers end-to-end by spawning fresh fixture subprocesses (see [`fake`])
//! under `/usr/bin/time` via the internal [`execute::run`] executor.
//!
//! The loop NEVER touches Nix, the network, a shell, or `PATH`: every child is
//! the `s4-runner` binary re-invoked as its own hidden fake child, by absolute
//! path, with only `LANG=C` / `LC_ALL=C` and fixed-size fixture stdout.
//!
//! # Honesty
//!
//! **No value produced by a Fake run is real benchmark evidence.** Every number
//! in a Fake report originates from deterministic fixture children exercising
//! the exact capture/statistics/report pipeline; the report is explicitly
//! marked `mode = fake`, `completeness = fakeOnly`, `harnessOnly = true`,
//! carries NO detected `nixVersion`, and labels every sample
//! [`report::CacheLabel::Fixture`]. These invariants are re-checked by
//! [`report::Report::validate`] before a Fake report leaves this module.
//!
//! # Safety / dependencies
//!
//! `#![forbid(unsafe_code)]` is inherited from the crate root; this module uses
//! only `std` and existing crate modules (`command`, `execute`, `fake`,
//! `flakeref`, `manifest`, `report`, `stats`). Visibility is mixed: the public
//! types and entry point the `s4-runner` binary needs ([`run_fake`],
//! [`RunnerError`], [`ManifestInvariant`], [`ManifestField`], [`ScenarioError`])
//! are `pub`, while every assembly helper is `pub(crate)`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::command::{
    CommandError, CommandOutcome, CommandSpec, MIN_TIMEOUT, TimeFlavor, UnixStatus,
};
use crate::execute;
use crate::fake;
use crate::flakeref;
use crate::manifest::{Manifest, benchmark_manifest};
use crate::report::{
    self, CacheLabel, Completeness, Host, Mode, Pin, Record, Sample, Scenario, Statistics,
};
use crate::stats;

/// Fixture single-attribute stdout payload size, in bytes. The pure descriptor
/// carries this so the later subprocess runner knows how many bytes to emit; the
/// pure helpers here never read it back into a sample (a sample's
/// `output_bytes` comes from the captured outcome totals).
pub(crate) const SINGLE_ATTR_STDOUT_PAYLOAD: u64 = 4096;

/// Fixture index-meta stdout payload size, in bytes.
pub(crate) const INDEX_STDOUT_PAYLOAD: u64 = 131_072;

/// Best-effort maximum length (in `char`s) of the recorded host machine name.
const MAX_MACHINE_CHARS: usize = 128;

// ---------------------------------------------------------------------------
// RunnerError
// ---------------------------------------------------------------------------

/// The single structured error for the runner. Every variant is bounded and
/// deterministic; NO variant embeds captured child stdout/stderr, arbitrary
/// environment values, or platform OS error text. Existing typed errors
/// ([`flakeref::SystemError`], [`stats::StatsError`], [`report::ReportError`],
/// [`command::CommandError`]) are wrapped rather than re-encoded.
///
/// This is a PUBLIC API: the later `s4-runner` binary names it as the error
/// type of [`run_fake`]. Every field of every variant is itself public, so the
/// type is free of `private_in_public` exposure. The detailed internal
/// [`ScenarioError`] is promoted to `pub` (it carries only bounded counts /
/// indices, never child output) precisely so this enum can name it publicly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// The compile-time host target is not one of the four supported
    /// `(arch, os)` pairs. `arch`/`os` are compile-time constants
    /// (`std::env::consts::{ARCH,OS}`), never runtime-guessed.
    UnsupportedHost {
        /// Compile-time target arch string.
        arch: &'static str,
        /// Compile-time target os string.
        os: &'static str,
    },
    /// A system triple was not accepted by the manifest allow-list (e.g. the
    /// host system is absent from `manifest.systems`).
    System(flakeref::SystemError),
    /// A captured command did not exit successfully (nonzero exit or signal).
    /// Stores only the bounded [`UnixStatus`]; never the child output.
    NonSuccessOutcome {
        /// The nonzero-exit / signaled status that was rejected.
        status: UnixStatus,
    },
    /// The internal finite-time executor ([`execute::run`]) failed: an invalid
    /// spec, a spawn/poll/kill/wait failure, a capture/decode/RSS failure, or a
    /// per-command timeout. Wraps [`command::CommandError`], whose `Display`
    /// reduces every I/O failure to a stable [`std::io::ErrorKind`] token — it
    /// never embeds child output, environment values, or OS-localized text.
    Command(CommandError),
    /// The overall wall-clock budget (`manifest.timeouts.overall_seconds`)
    /// was exhausted, or the time left before a child dropped below
    /// [`command::MIN_TIMEOUT`] (1 ms), so no further child could be honestly
    /// measured within the remaining budget. Carries no payload: the report is
    /// abandoned rather than partially fabricated.
    OverallTimeout,
    /// An "impossible" numeric conversion of a strictly-validated embedded
    /// manifest field (a nonzero cap from a value the validator already
    /// guaranteed `>= MIN_CAP_BYTES`). Seconds-to-[`Duration`] is
    /// infallible, so only the cap path can actually fire. Surfaced as a typed
    /// error rather than `unwrap`/`expect` so a mis-authored embedded manifest
    /// never panics in production.
    Manifest(ManifestInvariant),
    /// Statistics could not be computed (e.g. an empty measured-sample set).
    Stats(stats::StatsError),
    /// A scenario could not be assembled from samples (count/order/index
    /// mismatch or a missing measured value).
    Scenario(ScenarioError),
    /// The assembled report failed [`report::Report::validate`].
    Report(report::ReportError),
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::UnsupportedHost { arch, os } => write!(
                f,
                "runner: unsupported host target (arch={arch:?}, os={os:?}); expected one of x86_64/aarch64 on linux/macos"
            ),
            RunnerError::System(e) => write!(f, "runner: system check failed: {e}"),
            RunnerError::NonSuccessOutcome { status } => {
                write!(f, "runner: command did not exit successfully ({status})")
            }
            RunnerError::Command(e) => write!(f, "runner: command execution failed: {e}"),
            RunnerError::OverallTimeout => {
                write!(f, "runner: overall wall-clock budget exceeded")
            }
            RunnerError::Manifest(e) => {
                write!(f, "runner: embedded manifest invariant violated: {e}")
            }
            RunnerError::Stats(e) => write!(f, "runner: statistics error: {e}"),
            RunnerError::Scenario(e) => write!(f, "runner: scenario assembly error: {e}"),
            RunnerError::Report(e) => write!(f, "runner: report validation error: {e}"),
        }
    }
}

impl std::error::Error for RunnerError {}

// ---------------------------------------------------------------------------
// ManifestInvariant (bounded, public; surfaces impossible manifest conversions)
// ---------------------------------------------------------------------------

/// Which strictly-validated manifest cap field an "impossible" conversion
/// failed on. These can only fire for a mis-authored embedded manifest (the
/// validator already guarantees `>= MIN_CAP_BYTES == 1024`), never for a
/// runtime input. A bounded, caller-payload-free identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestField {
    /// `caps.singleAttrStdoutBytes`.
    SingleAttrStdoutCap,
    /// `caps.indexStdoutBytes`.
    IndexStdoutCap,
    /// `caps.stderrBytes`.
    StderrCap,
}

impl std::fmt::Display for ManifestField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ManifestField::SingleAttrStdoutCap => "caps.singleAttrStdoutBytes",
            ManifestField::IndexStdoutCap => "caps.indexStdoutBytes",
            ManifestField::StderrCap => "caps.stderrBytes",
        };
        f.write_str(s)
    }
}

/// An "impossible" numeric conversion of a strictly-validated embedded
/// manifest field. Bounded and deterministic: identifies the offending field,
/// embeds no caller-controlled payload. Only the cap path can actually fire in
/// normal operation (seconds-to-[`Duration`] is infallible); it exists so a
/// mis-authored embedded manifest is surfaced as a typed error rather than a
/// production panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestInvariant {
    /// A validated cap field was zero, so it could not become [`NonZeroU64`].
    CapNotPositive(ManifestField),
}

impl std::fmt::Display for ManifestInvariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestInvariant::CapNotPositive(field) => {
                write!(f, "{field} must be positive (NonZeroU64 conversion failed)")
            }
        }
    }
}

impl std::error::Error for ManifestInvariant {}

/// A structured scenario-assembly failure from [`scenario_from_samples`]. All
/// variants are bounded metadata (counts / indices); none embed sample output.
/// Promoted to `pub` so the public [`RunnerError::Scenario`] variant can name
/// it without `private_in_public` exposure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    /// The warmup-record count did not match the descriptor.
    WarmupCount {
        /// Declared warmup count.
        declared: u32,
        /// Warmup records found.
        found: u32,
    },
    /// The measured-record count did not match the descriptor.
    MeasuredCount {
        /// Declared measured count.
        declared: u32,
        /// Measured records found.
        found: u32,
    },
    /// A warmup record appeared after a measured record (records must be
    /// grouped warmup-first).
    RecordOrder {
        /// Index of the offending warmup record.
        index: u32,
    },
    /// Sample indices are not a contiguous in-order `0..N-1`.
    IndexNotContiguous {
        /// 0-based position where the mismatch was found.
        position: u32,
        /// `index` found at that position.
        found: u32,
    },
    /// A measured sample is missing its wall-time value.
    MissingWall {
        /// Index of the offending sample.
        index: u32,
    },
    /// A measured sample is missing its RSS value.
    MissingRss {
        /// Index of the offending sample.
        index: u32,
    },
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScenarioError::WarmupCount { declared, found } => write!(
                f,
                "warmup count mismatch (declared {declared}, found {found})"
            ),
            ScenarioError::MeasuredCount { declared, found } => write!(
                f,
                "measured count mismatch (declared {declared}, found {found})"
            ),
            ScenarioError::RecordOrder { index } => {
                write!(f, "warmup record after measured record (index {index})")
            }
            ScenarioError::IndexNotContiguous { position, found } => write!(
                f,
                "sample indices not contiguous in-order 0..N-1 (at position {position} found index {found})"
            ),
            ScenarioError::MissingWall { index } => {
                write!(f, "measured sample missing wall_ms (index {index})")
            }
            ScenarioError::MissingRss { index } => {
                write!(f, "measured sample missing rss_kb (index {index})")
            }
        }
    }
}

impl std::error::Error for ScenarioError {}

// ---------------------------------------------------------------------------
// Host target mapping + environment
// ---------------------------------------------------------------------------

/// Pure mapping from a compile-time `(arch, os)` pair to the canonical Nix
/// `system` triple. Returns `None` for any unsupported combination. There is NO
/// runtime guessing: the only caller ([`host_system`]) feeds compile-time
/// constants ([`std::env::consts::ARCH`] / [`std::env::consts::OS`]), so the
/// result is fixed at build time for a given target.
fn map_system(arch: &str, os: &str) -> Option<&'static str> {
    match (arch, os) {
        ("x86_64", "linux") => Some("x86_64-linux"),
        ("aarch64", "linux") => Some("aarch64-linux"),
        ("x86_64", "macos") => Some("x86_64-darwin"),
        ("aarch64", "macos") => Some("aarch64-darwin"),
        _ => None,
    }
}

/// The canonical Nix `system` triple for the compile-time host target.
///
/// Derived purely from [`std::env::consts::ARCH`] and [`std::env::consts::OS`]
/// (compile-time constants baked into the binary, NOT runtime environment
/// probing). Fails with a typed [`RunnerError::UnsupportedHost`] for any target
/// outside the four supported `(arch, os)` pairs; it never guesses.
pub(crate) fn host_system() -> Result<&'static str, RunnerError> {
    match map_system(std::env::consts::ARCH, std::env::consts::OS) {
        Some(s) => Ok(s),
        None => Err(RunnerError::UnsupportedHost {
            arch: std::env::consts::ARCH,
            os: std::env::consts::OS,
        }),
    }
}

/// The `/usr/bin/time` dialect for the compile-time host OS: GNU `-v` on Linux,
/// BSD/macOS `-l` on macOS. Unsupported OSes default to GNU (unreachable in
/// practice because [`host_system`] rejects them first).
///
/// Selected once per [`run_fake`] call and handed to [`execute::run`] so the
/// executor parses `/usr/bin/time` output in the host's dialect.
pub(crate) fn host_time_flavor() -> TimeFlavor {
    match std::env::consts::OS {
        "linux" => TimeFlavor::Gnu,
        "macos" => TimeFlavor::MacOs,
        _ => TimeFlavor::Gnu,
    }
}

/// The COMPLETE, deterministic child process environment: exactly `LANG=C` and
/// `LC_ALL=C`, in a [`BTreeMap`] for deterministic iteration. There is NO
/// `PATH`: [`run_fake`] invokes the `s4-runner` fake child by absolute path
/// only and applies this via `Command::env_clear()` so nothing is inherited.
pub(crate) fn child_env() -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    env.insert(OsString::from("LANG"), OsString::from("C"));
    env.insert(OsString::from("LC_ALL"), OsString::from("C"));
    env
}

// ---------------------------------------------------------------------------
// Scenario descriptors
// ---------------------------------------------------------------------------

/// Internal description of one scenario the (later) subprocess runner will
/// execute: a name, the Nix `system` triple, the pure flake installable, the
/// declared warmup/measured counts, the fixture stdout payload, the
/// per-command timeout, and the per-scenario stdout CAP. Pure data — no
/// spawning, no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScenarioDescriptor {
    /// Human-readable scenario name (e.g. `single-attr:ripgrep`,
    /// `index-meta:x86_64-linux`).
    pub(crate) name: String,
    /// Nix `system` triple this scenario targets.
    pub(crate) system: String,
    /// Pure flake installable string (from [`flakeref`]).
    pub(crate) installable: String,
    /// Declared number of warmup iterations.
    pub(crate) warmup: u32,
    /// Declared number of measured iterations.
    pub(crate) measured: u32,
    /// Fixture stdout payload size in bytes the child should EMIT
    /// (`SINGLE_ATTR_STDOUT_PAYLOAD` / `INDEX_STDOUT_PAYLOAD`).
    pub(crate) stdout_payload: u64,
    /// Maximum stdout bytes RETAINED from the child for this scenario, as a
    /// nonzero cap. Single-attr uses `manifest.caps.single_attr_stdout_bytes`;
    /// index uses `manifest.caps.index_stdout_bytes`. The shared stderr cap
    /// (`manifest.caps.stderr_bytes`) is applied separately in [`run_fake`].
    pub(crate) stdout_cap_bytes: NonZeroU64,
    /// Per-command wall-clock timeout in whole seconds.
    pub(crate) timeout_seconds: u64,
}

/// Enumerate the EXACT scenario set for a manifest, in order:
///
/// 1. ONE host-only `single-attr:<attr>` scenario (using
///    [`flakeref::single_attr_installable`] and `sampling.singleAttrSamples`),
///    targeting the compile-time host system; then
/// 2. ONE `index-meta:<system>` scenario per `manifest.systems` element in
///    manifest order (using [`flakeref::index_installable`] and
///    `sampling.indexSamples`).
///
/// That is exactly `1 + manifest.systems.len()` entries (5 for the four-system
/// manifest). Every scenario uses `sampling.warmup`. Single-attr payload is
/// [`SINGLE_ATTR_STDOUT_PAYLOAD`] with timeout `timeouts.singleAttrSeconds`;
/// index payload is [`INDEX_STDOUT_PAYLOAD`] with timeout `timeouts.indexSeconds`.
///
/// Fails if the host system is unsupported or absent from `manifest.systems`.
pub(crate) fn descriptors(manifest: &Manifest) -> Result<Vec<ScenarioDescriptor>, RunnerError> {
    let mut out = Vec::with_capacity(1 + manifest.systems.len());

    // 1. host-only single-attr scenario.
    let host_sys = host_system()?;
    let host_checked = flakeref::check_system(manifest, host_sys).map_err(RunnerError::System)?;
    out.push(ScenarioDescriptor {
        name: format!("single-attr:{}", manifest.attr),
        system: host_sys.to_owned(),
        installable: flakeref::single_attr_installable(manifest, &host_checked),
        warmup: manifest.sampling.warmup,
        measured: manifest.sampling.single_attr_samples,
        stdout_payload: SINGLE_ATTR_STDOUT_PAYLOAD,
        stdout_cap_bytes: nz_cap(
            manifest.caps.single_attr_stdout_bytes,
            ManifestField::SingleAttrStdoutCap,
        )?,
        timeout_seconds: manifest.timeouts.single_attr_seconds,
    });

    // 2. index-meta scenario for each manifest system, in manifest order.
    for system in &manifest.systems {
        let checked = flakeref::check_system(manifest, system).map_err(RunnerError::System)?;
        out.push(ScenarioDescriptor {
            name: format!("index-meta:{system}"),
            system: system.clone(),
            installable: flakeref::index_installable(manifest, &checked),
            warmup: manifest.sampling.warmup,
            measured: manifest.sampling.index_samples,
            stdout_payload: INDEX_STDOUT_PAYLOAD,
            stdout_cap_bytes: nz_cap(
                manifest.caps.index_stdout_bytes,
                ManifestField::IndexStdoutCap,
            )?,
            timeout_seconds: manifest.timeouts.index_seconds,
        });
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Outcome -> Sample -> Scenario
// ---------------------------------------------------------------------------

/// Convert one captured [`CommandOutcome`] into a [`Sample`].
///
/// Accepts ONLY [`UnixStatus::Exited`]`(0)`; any nonzero exit or signal is
/// rejected with [`RunnerError::NonSuccessOutcome`]. On success the sample is
/// `skipped = false`, carries the EXACT wall-ms and RSS-KiB from the outcome,
/// `output_bytes` = `stdout_total_bytes.saturating_add(stderr_total_bytes)`,
/// `exit = 0`, and is labelled [`CacheLabel::Fixture`] (Fake foundation). The
/// child output bytes themselves are NEVER inspected or embedded.
pub(crate) fn sample_from_outcome(
    index: u32,
    record: Record,
    outcome: CommandOutcome,
) -> Result<Sample, RunnerError> {
    if !outcome.status.is_success() {
        return Err(RunnerError::NonSuccessOutcome {
            status: outcome.status,
        });
    }
    let output_bytes = outcome
        .stdout_total_bytes
        .saturating_add(outcome.stderr_total_bytes);
    Ok(Sample {
        index,
        record,
        skipped: false,
        wall_ms: Some(outcome.wall_ms),
        rss_kb: Some(outcome.max_rss_kib),
        output_bytes: Some(output_bytes),
        exit: 0,
        cache: CacheLabel::Fixture,
    })
}

/// Fold a [`ScenarioDescriptor`] and its captured [`Sample`]s into a validated
/// [`Scenario`].
///
/// Validates the EXACT warmup/measured counts, contiguous in-order `0..N-1`
/// sample indices, warmup-before-measured record ordering, and that every
/// measured sample carries wall + RSS values. Statistics are then computed from
/// the MEASURED samples only (warmup excluded) via [`stats::compute`]. Returns a
/// structured [`RunnerError::Scenario`] on any mismatch or missing value.
pub(crate) fn scenario_from_samples(
    descriptor: &ScenarioDescriptor,
    samples: Vec<Sample>,
) -> Result<Scenario, RunnerError> {
    let warmup_found = samples
        .iter()
        .filter(|s| s.record == Record::Warmup)
        .count() as u32;
    let measured_found = samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .count() as u32;

    if warmup_found != descriptor.warmup {
        return Err(RunnerError::Scenario(ScenarioError::WarmupCount {
            declared: descriptor.warmup,
            found: warmup_found,
        }));
    }
    if measured_found != descriptor.measured {
        return Err(RunnerError::Scenario(ScenarioError::MeasuredCount {
            declared: descriptor.measured,
            found: measured_found,
        }));
    }

    // Contiguous in-order indices: at position `pos` the index MUST equal `pos`.
    for (pos, sample) in samples.iter().enumerate() {
        if sample.index != pos as u32 {
            return Err(RunnerError::Scenario(ScenarioError::IndexNotContiguous {
                position: pos as u32,
                found: sample.index,
            }));
        }
    }

    // Records grouped warmup-first: once a measured record is seen, no later
    // warmup is allowed.
    let mut seen_measured = false;
    for sample in &samples {
        match sample.record {
            Record::Measured => seen_measured = true,
            Record::Warmup if seen_measured => {
                return Err(RunnerError::Scenario(ScenarioError::RecordOrder {
                    index: sample.index,
                }));
            }
            Record::Warmup => {}
        }
    }

    // Every measured sample must carry wall + RSS (needed for statistics).
    for sample in &samples {
        if sample.record == Record::Measured {
            if sample.wall_ms.is_none() {
                return Err(RunnerError::Scenario(ScenarioError::MissingWall {
                    index: sample.index,
                }));
            }
            if sample.rss_kb.is_none() {
                return Err(RunnerError::Scenario(ScenarioError::MissingRss {
                    index: sample.index,
                }));
            }
        }
    }

    // Statistics over MEASURED samples only (warmup excluded).
    let wall_vals: Vec<u64> = samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| {
            s.wall_ms
                .expect("measured sample has wall_ms (validated above)")
        })
        .collect();
    let rss_vals: Vec<u64> = samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| {
            s.rss_kb
                .expect("measured sample has rss_kb (validated above)")
        })
        .collect();

    let wall_stats = stats::compute(&wall_vals).map_err(RunnerError::Stats)?;
    let rss_stats = stats::compute(&rss_vals).map_err(RunnerError::Stats)?;

    Ok(Scenario {
        name: descriptor.name.clone(),
        system: descriptor.system.clone(),
        installable: descriptor.installable.clone(),
        warmup: descriptor.warmup,
        measured: descriptor.measured,
        samples,
        statistics: Statistics {
            wall: Some(to_sample_statistics(wall_stats, wall_vals.len())),
            rss: Some(to_sample_statistics(rss_stats, rss_vals.len())),
        },
    })
}

/// Adapt [`stats::Stats`] + the count into a report [`report::SampleStatistics`].
fn to_sample_statistics(s: stats::Stats, count: usize) -> report::SampleStatistics {
    report::SampleStatistics {
        count: count as u32,
        min: s.min,
        median: s.median,
        p95: s.p95,
        max: s.max,
    }
}

// ---------------------------------------------------------------------------
// Fake report assembly
// ---------------------------------------------------------------------------

/// Assemble a deterministic Fake report from a manifest, the captured host, and
/// the fully-exercised scenarios, then validate it.
///
/// The report is exactly: `schemaVersion = REPORT_SCHEMA_VERSION`,
/// `mode = fake`, `completeness = fakeOnly`, `harnessOnly = true`,
/// `nixVersion = None`, `failures = []`, with the pin fields copied verbatim
/// from the manifest. [`report::Report::validate`] is called before the report
/// is returned; a validation failure becomes [`RunnerError::Report`].
///
/// **No value in this report is real benchmark evidence** — see the module docs.
pub(crate) fn fake_report(
    manifest: &Manifest,
    host: Host,
    scenarios: Vec<Scenario>,
) -> Result<report::Report, RunnerError> {
    let built = report::Report {
        schema_version: report::REPORT_SCHEMA_VERSION,
        mode: Mode::Fake,
        completeness: Completeness::FakeOnly,
        harness_only: true,
        host,
        pin: Pin {
            nix_version: manifest.nix.version.clone(),
            owner: manifest.nixpkgs.owner.clone(),
            repo: manifest.nixpkgs.repo.clone(),
            rev: manifest.nixpkgs.rev.clone(),
            nar_hash: manifest.nixpkgs.nar_hash.clone(),
            attr: manifest.attr.clone(),
        },
        nix_version: None,
        scenarios,
        failures: Vec::new(),
    };
    built.validate().map_err(RunnerError::Report)?;
    Ok(built)
}

// ---------------------------------------------------------------------------
// Subprocess execution loop (run_fake) + its pure, testable helpers
// ---------------------------------------------------------------------------
//
// The PUBLIC entry point [`run_fake`] drives the EXACT pipeline end-to-end:
// for every scenario descriptor it spawns fresh fixture subprocesses (warmups
// first, then measured, contiguous indices), each via [`execute::run`] under
// `/usr/bin/time`, converts each successful outcome with [`sample_from_outcome`],
// folds a descriptor + samples into a validated [`Scenario`] via
// [`scenario_from_samples`], and assembles a deterministic Fake report via
// [`fake_report`]. No partial / complete fabrication: every sample is a real
// fixture invocation, and the report is abandoned (as [`RunnerError`]) if any
// step fails — including a sample that completes only after the overall
// deadline.
//
// The loop is factored into small PURE helpers (no spawning, no I/O) so the
// argv / spec / timeout / iteration-plan contracts are unit-tested in
// isolation with NO process spawning.

/// Convert a strictly-validated manifest cap (`u64`) into a [`NonZeroU64`].
/// The validator already guarantees `>= [`crate::validate::MIN_CAP_BYTES`]`, so
/// this only fails for a mis-authored embedded manifest; that impossible case
/// is surfaced as [`RunnerError::Manifest`] rather than `unwrap`/`expect`, so a
/// production build never panics on a trusted-input invariant.
pub(crate) fn nz_cap(value: u64, field: ManifestField) -> Result<NonZeroU64, RunnerError> {
    NonZeroU64::new(value).ok_or(RunnerError::Manifest(ManifestInvariant::CapNotPositive(
        field,
    )))
}

/// Build the EXACT fake-child argv for a `stdout_payload` byte count. The order
/// is fixed by the hidden fake-child protocol ([`fake`]):
///
/// ```text
/// <MARKER> --stdout-bytes <payload> --stderr-bytes 0 --sleep-ms 0 --exit-code 0
/// ```
///
/// All nine tokens are [`OsString`] values passed VERBATIM as `argv[1..]`; the
/// program itself is supplied separately (the absolute `executable`), so there
/// is NO shell, NO interpolation, and NO `PATH` search. `stderr-bytes`,
/// `sleep-ms`, and `exit-code` are pinned to `0` so every fixture child emits
/// only the requested stdout, sleeps nothing, and exits 0.
pub(crate) fn fake_argv(stdout_payload: u64) -> Vec<OsString> {
    vec![
        OsString::from(fake::MARKER),
        OsString::from("--stdout-bytes"),
        OsString::from(stdout_payload.to_string()),
        OsString::from("--stderr-bytes"),
        OsString::from("0"),
        OsString::from("--sleep-ms"),
        OsString::from("0"),
        OsString::from("--exit-code"),
        OsString::from("0"),
    ]
}

/// The bounded command timeout for one child, given the per-phase budget and
/// the time left to the overall deadline.
///
/// Returns [`RunnerError::OverallTimeout`] when the overall budget is already
/// exhausted (`elapsed >= overall`) OR when the remaining budget is below
/// [`command::MIN_TIMEOUT`] (1 ms) — in either case no child can be honestly
/// measured within the budget. Otherwise the command timeout is the SMALLER of
/// `phase` and `remaining`, so it never exceeds either bound. Because the
/// manifest validates `phase <= [`MAX_PER_COMMAND_TIMEOUT_SECONDS`] == 3600 s`
/// (== [`command::MAX_TIMEOUT`]) and the guard above guarantees
/// `remaining >= [`MIN_TIMEOUT`]`, the result is always inside the spec's
/// accepted `1 ms..=1 h` window.
///
/// Pure (no clock access): the caller supplies `elapsed`, so this is trivially
/// unit-testable across boundary conditions.
pub(crate) fn select_timeout(
    phase: Duration,
    elapsed: Duration,
    overall: Duration,
) -> Result<Duration, RunnerError> {
    if elapsed >= overall {
        return Err(RunnerError::OverallTimeout);
    }
    // Safe: we just proved elapsed < overall, so this cannot underflow.
    let remaining = overall - elapsed;
    if remaining < MIN_TIMEOUT {
        return Err(RunnerError::OverallTimeout);
    }
    Ok(if phase < remaining { phase } else { remaining })
}

/// Enumerate the EXACT iteration plan for one descriptor: ALL warmup iterations
/// first (indices `0..warmup-1`), then ALL measured iterations
/// (`warmup..warmup+measured-1`), as contiguous `u32` indices in order. Pure
/// data — no spawning, no I/O. `warmup + measured` is saturated on the capacity
/// estimate and the index counter to stay panic-free even for a hypothetically
/// huge (unvalidated) descriptor.
pub(crate) fn iteration_plan(descriptor: &ScenarioDescriptor) -> Vec<(Record, u32)> {
    let total = (descriptor.warmup as usize).saturating_add(descriptor.measured as usize);
    let mut out = Vec::with_capacity(total);
    let mut idx: u32 = 0;
    for _ in 0..descriptor.warmup {
        out.push((Record::Warmup, idx));
        idx = idx.saturating_add(1);
    }
    for _ in 0..descriptor.measured {
        out.push((Record::Measured, idx));
        idx = idx.saturating_add(1);
    }
    out
}

/// Build the validated [`CommandSpec`] for one fixture child: the absolute
/// `executable` as the program (passed VERBATIM — NO shell, NO `PATH`), the
/// exact fake-child [`argv`](fake_argv) as `argv[1..]`, the COMPLETE fail-closed
/// environment ([`child_env`]: exactly `LANG=C` / `LC_ALL=C`, nothing inherited),
/// and the per-stream nonzero caps plus the bounded `timeout`.
///
/// A relative or empty `executable` is rejected HERE by [`CommandSpec::new`] →
/// [`command::SpecError`] → [`CommandError::Spec`], surfaced as
/// [`RunnerError::Command`], BEFORE any spawn is attempted (the executor is
/// never reached). Pure validation only — no spawning, no I/O.
pub(crate) fn build_spec(
    executable: &Path,
    descriptor: &ScenarioDescriptor,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
) -> Result<CommandSpec, RunnerError> {
    CommandSpec::new(
        executable.to_path_buf(),
        fake_argv(descriptor.stdout_payload),
        child_env(),
        stdout_cap,
        stderr_cap,
        timeout,
    )
    .map_err(|e| RunnerError::Command(CommandError::from(e)))
}

/// Run the EXACT Fake pipeline end-to-end against `executable` and return a
/// deterministic, validated Fake report.
///
/// `executable` is the absolute path to the `s4-runner` binary re-invoked as
/// its own hidden fake child (see [`fake`]); it is passed VERBATIM to
/// [`CommandSpec::new`] and MUST be absolute (the runner never searches
/// `PATH`, never shells out, never touches the network or Nix).
///
/// # Loop
/// 1. One [`Instant`] deadline spans the WHOLE call, sized by
///    `manifest.timeouts.overall_seconds`.
/// 2. For every [`descriptor`](descriptors) (1 host single-attr + 1 index-meta
///    per manifest system, in order), run EXACTLY `warmup` warmup iterations
///    then `measured` measured iterations — fresh subprocesses each, contiguous
///    `u32` indices — via [`execute::run`] under `/usr/bin/time`.
/// 3. Before each child, the command timeout is [`select_timeout`] of the phase
///    budget and the remaining overall budget; expired or sub-1 ms remaining
///    abandons the run as [`RunnerError::OverallTimeout`].
/// 4. Any [`CommandError`] (spawn/poll/kill/wait/capture/decode/RSS/timeout),
///    nonzero exit, or signal fails IMMEDIATELY. A successful outcome is
///    converted with [`sample_from_outcome`].
/// 5. The overall deadline is re-checked AFTER each child: a completion that
///    lands at or past the deadline fails (via [`RunnerError::OverallTimeout`])
///    rather than being accepted.
/// 6. Each descriptor is folded into a validated [`Scenario`] via
///    [`scenario_from_samples`], and the report is assembled + validated by
///    [`fake_report`]. No partial / complete fabrication occurs.
///
/// # Honesty
/// **No value produced by a Fake run is real benchmark evidence** — see the
/// module docs. The report is `mode = fake`, `completeness = fakeOnly`,
/// `harnessOnly = true`, carries no detected `nixVersion`, and labels every
/// sample [`CacheLabel::Fixture`].
///
/// # Panics
/// Never panics on `executable` / runtime inputs: every numeric conversion of
/// the trusted embedded manifest is surfaced as a typed
/// [`RunnerError::Manifest`], never `unwrap`/`expect`.
pub fn run_fake(executable: &Path) -> Result<report::Report, RunnerError> {
    let start = Instant::now();
    let manifest = benchmark_manifest();
    let host = host()?;
    let descriptors = descriptors(manifest)?;
    let overall = Duration::from_secs(manifest.timeouts.overall_seconds);
    let stderr_cap = nz_cap(manifest.caps.stderr_bytes, ManifestField::StderrCap)?;
    let flavor = host_time_flavor();

    let mut scenarios = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let stdout_cap = descriptor.stdout_cap_bytes;
        let phase_timeout = Duration::from_secs(descriptor.timeout_seconds);
        let plan = iteration_plan(descriptor);
        let mut samples = Vec::with_capacity(plan.len());
        for (record, index) in plan {
            // Before each child: remaining = overall - elapsed. Expired or
            // sub-MIN_TIMEOUT remaining abandons the run. The command timeout
            // is min(phase, remaining) — never above either, always valid.
            let command_timeout = select_timeout(phase_timeout, start.elapsed(), overall)?;
            let spec = build_spec(
                executable,
                descriptor,
                stdout_cap,
                stderr_cap,
                command_timeout,
            )?;
            // One fresh subprocess per iteration. Any CommandError fails
            // immediately; a nonzero/signal outcome is rejected by
            // sample_from_outcome.
            let outcome = execute::run(&spec, flavor).map_err(RunnerError::Command)?;
            samples.push(sample_from_outcome(index, record, outcome)?);
            // After each child: a completion at/after the overall deadline
            // fails rather than being accepted.
            if start.elapsed() >= overall {
                return Err(RunnerError::OverallTimeout);
            }
        }
        scenarios.push(scenario_from_samples(descriptor, samples)?);
    }

    fake_report(manifest, host, scenarios)
}

// ---------------------------------------------------------------------------
// Host machine + cores helpers
// ---------------------------------------------------------------------------

/// Sanitize untrusted `HOSTNAME` input into a bounded machine name: drop control
/// characters, cap at [`MAX_MACHINE_CHARS`] chars, fall back to `"unknown"` when
/// absent or empty. Pure (no env access) so it is trivially testable.
fn sanitize_machine(raw: Option<&OsStr>) -> String {
    let cleaned: String = match raw {
        Some(s) => s
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_MACHINE_CHARS)
            .collect(),
        None => String::new(),
    };
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// Best-effort, UNTRUSTED host machine name, read from the `HOSTNAME` env var
/// only (never spawns `hostname(1)`). Control characters are stripped and the
/// value is capped at [`MAX_MACHINE_CHARS`] chars; an absent/empty value falls
/// back to `"unknown"`. Recorded, never trusted.
pub(crate) fn host_machine() -> String {
    sanitize_machine(std::env::var_os("HOSTNAME").as_deref())
}

/// Usable CPU core count via [`std::thread::available_parallelism`], saturated
/// to `u32`, with a fallback of `1` when detection fails.
pub(crate) fn host_cores() -> u32 {
    match std::thread::available_parallelism() {
        Ok(n) => {
            let n = n.get();
            if n > u32::MAX as usize {
                u32::MAX
            } else {
                n as u32
            }
        }
        Err(_) => 1,
    }
}

/// Build a [`Host`] for the compile-time target (system triple from
/// [`host_system`], machine name from [`host_machine`], cores from
/// [`host_cores`]). Fails only if the host target is unsupported.
pub(crate) fn host() -> Result<Host, RunnerError> {
    Ok(Host {
        system: host_system()?.to_owned(),
        machine: host_machine(),
        cores: host_cores(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandOutcome, SpecError};
    use crate::flakeref;
    use crate::manifest::benchmark_manifest;
    use crate::report::REPORT_SCHEMA_VERSION;

    // ---- test helpers ------------------------------------------------------

    /// Build a [`CommandOutcome`] with the given status/totals and empty
    /// retained output (the pure helpers never inspect retained output).
    fn outcome(
        status: UnixStatus,
        wall_ms: u64,
        max_rss_kib: u64,
        stdout_total_bytes: u64,
        stderr_total_bytes: u64,
    ) -> CommandOutcome {
        CommandOutcome {
            status,
            stdout: Vec::new(),
            cleaned_stderr: String::new(),
            stdout_total_bytes,
            stderr_total_bytes,
            wall_ms,
            max_rss_kib,
        }
    }

    /// Build a fully-exercised [`Scenario`] from a descriptor using synthetic
    /// increasing wall/rss outcomes (warmup excluded from stats by construction
    /// because warmup values differ from measured values).
    fn synth_scenario(descriptor: &ScenarioDescriptor, base_wall: u64, base_rss: u64) -> Scenario {
        let mut samples = Vec::new();
        let mut idx = 0u32;
        for _ in 0..descriptor.warmup {
            samples.push(
                sample_from_outcome(
                    idx,
                    Record::Warmup,
                    outcome(
                        UnixStatus::Exited(0),
                        base_wall,
                        base_rss,
                        descriptor.stdout_payload,
                        0,
                    ),
                )
                .unwrap(),
            );
            idx += 1;
        }
        for i in 0..descriptor.measured {
            let wall = base_wall + i as u64;
            let rss = base_rss + i as u64 * 10;
            samples.push(
                sample_from_outcome(
                    idx,
                    Record::Measured,
                    outcome(
                        UnixStatus::Exited(0),
                        wall,
                        rss,
                        descriptor.stdout_payload,
                        0,
                    ),
                )
                .unwrap(),
            );
            idx += 1;
        }
        scenario_from_samples(descriptor, samples).unwrap()
    }

    // ---- Test 1: host mapping + actual host in manifest --------------------

    #[test]
    fn map_system_covers_exact_four_supported_pairs() {
        assert_eq!(map_system("x86_64", "linux"), Some("x86_64-linux"));
        assert_eq!(map_system("aarch64", "linux"), Some("aarch64-linux"));
        assert_eq!(map_system("x86_64", "macos"), Some("x86_64-darwin"));
        assert_eq!(map_system("aarch64", "macos"), Some("aarch64-darwin"));
    }

    #[test]
    fn map_system_rejects_unsupported_combinations() {
        assert_eq!(map_system("x86_64", "windows"), None);
        assert_eq!(map_system("powerpc64", "linux"), None);
        assert_eq!(map_system("aarch64", "freebsd"), None);
        assert_eq!(map_system("", ""), None);
    }

    #[test]
    fn host_system_returns_a_manifest_supported_triple() {
        let manifest = benchmark_manifest();
        let sys = host_system().expect("host target must be supported");
        assert!(
            manifest.systems.iter().any(|s| s == sys),
            "host system {sys:?} must be one of {:?}",
            manifest.systems
        );
    }

    // ---- Test 2: child env -------------------------------------------------

    #[test]
    fn child_env_is_exactly_lang_and_lc_all_c_with_no_path() {
        let env = child_env();
        assert_eq!(env.len(), 2, "exactly two entries");
        assert_eq!(
            env.get(OsStr::new("LANG")),
            Some(&OsString::from("C")),
            "LANG=C"
        );
        assert_eq!(
            env.get(OsStr::new("LC_ALL")),
            Some(&OsString::from("C")),
            "LC_ALL=C"
        );
        assert!(
            !env.contains_key(OsStr::new("PATH")),
            "no PATH in child env"
        );
        // Deterministic iteration order (BTreeMap): LANG before LC_ALL.
        let keys: Vec<&OsString> = env.keys().collect();
        assert_eq!(
            keys,
            vec![&OsString::from("LANG"), &OsString::from("LC_ALL")]
        );
    }

    // ---- Test 3: descriptors -----------------------------------------------

    #[test]
    fn descriptors_are_exactly_five_in_order_with_exact_fields() {
        let manifest = benchmark_manifest();
        let host_sys = host_system().unwrap();
        let descs = descriptors(manifest).expect("descriptors must assemble");
        assert_eq!(descs.len(), 5, "exactly 1 single-attr + 4 index-meta");

        // Entry 0: host-only single-attr.
        let host_checked = flakeref::check_system(manifest, host_sys).unwrap();
        let d0 = &descs[0];
        assert_eq!(d0.name, format!("single-attr:{}", manifest.attr));
        assert_eq!(d0.system, host_sys, "single-attr is host-only");
        assert_eq!(
            d0.installable,
            flakeref::single_attr_installable(manifest, &host_checked)
        );
        assert_eq!(d0.warmup, manifest.sampling.warmup);
        assert_eq!(d0.measured, manifest.sampling.single_attr_samples);
        assert_eq!(d0.stdout_payload, SINGLE_ATTR_STDOUT_PAYLOAD);
        assert_eq!(d0.timeout_seconds, manifest.timeouts.single_attr_seconds);

        // Entries 1..4: index-meta per manifest system, in manifest order.
        for (i, system) in manifest.systems.iter().enumerate() {
            let d = &descs[1 + i];
            let checked = flakeref::check_system(manifest, system).unwrap();
            assert_eq!(d.name, format!("index-meta:{system}"));
            assert_eq!(d.system, *system);
            assert_eq!(
                d.installable,
                flakeref::index_installable(manifest, &checked)
            );
            assert_eq!(d.warmup, manifest.sampling.warmup);
            assert_eq!(d.measured, manifest.sampling.index_samples);
            assert_eq!(d.stdout_payload, INDEX_STDOUT_PAYLOAD);
            assert_eq!(d.timeout_seconds, manifest.timeouts.index_seconds);
        }

        // Exactly one single-attr entry; its system is the host only.
        let single_count = descs
            .iter()
            .filter(|d| d.name.starts_with("single-attr:"))
            .count();
        assert_eq!(single_count, 1);
    }

    // ---- Test 4: sample_from_outcome ---------------------------------------

    #[test]
    fn sample_from_outcome_success_carries_exact_fields() {
        let s = sample_from_outcome(
            3,
            Record::Measured,
            outcome(UnixStatus::Exited(0), 4242, 9_000, 1_000, 240),
        )
        .unwrap();
        assert_eq!(s.index, 3);
        assert_eq!(s.record, Record::Measured);
        assert!(!s.skipped);
        assert_eq!(s.wall_ms, Some(4242));
        assert_eq!(s.rss_kb, Some(9_000));
        assert_eq!(s.output_bytes, Some(1_240)); // 1000 + 240
        assert_eq!(s.exit, 0);
        assert_eq!(s.cache, CacheLabel::Fixture);
    }

    #[test]
    fn sample_from_outcome_combines_output_bytes_saturating() {
        // Near-u64::MAX totals: a plain `+` would overflow; saturating add caps.
        let s = sample_from_outcome(
            0,
            Record::Warmup,
            outcome(UnixStatus::Exited(0), 1, 1, u64::MAX - 5, 10),
        )
        .unwrap();
        assert_eq!(s.output_bytes, Some(u64::MAX));
    }

    #[test]
    fn sample_from_outcome_rejects_nonzero_exit() {
        let err = sample_from_outcome(
            0,
            Record::Measured,
            outcome(UnixStatus::Exited(1), 1, 1, 0, 0),
        )
        .unwrap_err();
        assert_eq!(
            err,
            RunnerError::NonSuccessOutcome {
                status: UnixStatus::Exited(1)
            }
        );
    }

    #[test]
    fn sample_from_outcome_rejects_signaled() {
        let err = sample_from_outcome(
            0,
            Record::Measured,
            outcome(UnixStatus::Signaled(9), 1, 1, 0, 0),
        )
        .unwrap_err();
        assert_eq!(
            err,
            RunnerError::NonSuccessOutcome {
                status: UnixStatus::Signaled(9)
            }
        );
    }

    #[test]
    fn runner_error_never_embeds_output() {
        // A nonzero outcome with nontrivial retained output must not leak into
        // the error's Display.
        let err = sample_from_outcome(
            0,
            Record::Measured,
            outcome(UnixStatus::Exited(2), 5, 6, 9_999, 8_888),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("9999"));
        assert!(!msg.contains("8888"));
        assert!(msg.contains("exit 2"));
    }

    // ---- Test 5: scenario_from_samples -------------------------------------

    #[test]
    fn scenario_stats_exclude_warmup_and_recompute_exactly() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // warmup=1, measured=5
        let scen = synth_scenario(single, 100, 1_000);

        assert_eq!(scen.warmup, 1);
        assert_eq!(scen.measured, 5);
        // Measured wall values were 100..=104; warmup wall was 100 (excluded).
        let expected_wall = stats::compute(&[100, 101, 102, 103, 104]).unwrap();
        let expected_rss = stats::compute(&[1000, 1010, 1020, 1030, 1040]).unwrap();
        assert_eq!(
            scen.statistics.wall.as_ref().unwrap(),
            &report::SampleStatistics {
                count: 5,
                min: expected_wall.min,
                median: expected_wall.median,
                p95: expected_wall.p95,
                max: expected_wall.max,
            }
        );
        assert_eq!(
            scen.statistics.rss.as_ref().unwrap(),
            &report::SampleStatistics {
                count: 5,
                min: expected_rss.min,
                median: expected_rss.median,
                p95: expected_rss.p95,
                max: expected_rss.max,
            }
        );
    }

    #[test]
    fn scenario_rejects_wrong_warmup_count() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // expects warmup=1
        let samples = vec![
            sample_from_outcome(
                0,
                Record::Measured,
                outcome(UnixStatus::Exited(0), 1, 1, 1, 0),
            )
            .unwrap(),
        ];
        let err = scenario_from_samples(single, samples).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Scenario(ScenarioError::WarmupCount {
                declared: 1,
                found: 0
            })
        );
    }

    #[test]
    fn scenario_rejects_wrong_measured_count() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // expects measured=5
        let mut samples = vec![
            sample_from_outcome(
                0,
                Record::Warmup,
                outcome(UnixStatus::Exited(0), 1, 1, 1, 0),
            )
            .unwrap(),
        ];
        samples.push(
            sample_from_outcome(
                1,
                Record::Measured,
                outcome(UnixStatus::Exited(0), 2, 2, 1, 0),
            )
            .unwrap(),
        );
        let err = scenario_from_samples(single, samples).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Scenario(ScenarioError::MeasuredCount {
                declared: 5,
                found: 1
            })
        );
    }

    #[test]
    fn scenario_rejects_warmup_after_measured() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // warmup=1, measured=5
        let mut samples: Vec<Sample> = Vec::new();
        // measured first (index 0) then warmup (index 1) -> wrong order.
        samples.push(
            sample_from_outcome(
                0,
                Record::Measured,
                outcome(UnixStatus::Exited(0), 1, 1, 1, 0),
            )
            .unwrap(),
        );
        samples.push(
            sample_from_outcome(
                1,
                Record::Warmup,
                outcome(UnixStatus::Exited(0), 2, 2, 1, 0),
            )
            .unwrap(),
        );
        for i in 2..6 {
            samples.push(
                sample_from_outcome(
                    i,
                    Record::Measured,
                    outcome(UnixStatus::Exited(0), i as u64, i as u64, 1, 0),
                )
                .unwrap(),
            );
        }
        let err = scenario_from_samples(single, samples).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Scenario(ScenarioError::RecordOrder { index: 1 })
        );
    }

    #[test]
    fn scenario_rejects_non_contiguous_index() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // warmup=1, measured=5
        let mut samples: Vec<Sample> = Vec::new();
        samples.push(
            sample_from_outcome(
                0,
                Record::Warmup,
                outcome(UnixStatus::Exited(0), 1, 1, 1, 0),
            )
            .unwrap(),
        );
        // Skip index 1: warmup at index 0, then measured indices 2,3,4,5,6
        // (5 measured, correct count) but with a gap at position 1.
        samples.push(
            sample_from_outcome(
                2,
                Record::Measured,
                outcome(UnixStatus::Exited(0), 2, 2, 1, 0),
            )
            .unwrap(),
        );
        for i in 3..=6 {
            samples.push(
                sample_from_outcome(
                    i,
                    Record::Measured,
                    outcome(UnixStatus::Exited(0), i as u64, i as u64, 1, 0),
                )
                .unwrap(),
            );
        }
        assert_eq!(samples.len(), 6); // 1 warmup + 5 measured
        let err = scenario_from_samples(single, samples).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Scenario(ScenarioError::IndexNotContiguous {
                position: 1,
                found: 2
            })
        );
    }

    #[test]
    fn scenario_rejects_missing_measured_values() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        let single = &descs[0]; // warmup=1, measured=5
        let mut samples: Vec<Sample> = Vec::new();
        samples.push(
            sample_from_outcome(
                0,
                Record::Warmup,
                outcome(UnixStatus::Exited(0), 1, 1, 1, 0),
            )
            .unwrap(),
        );
        // Build 4 good measured samples + 1 measured sample missing wall/rss.
        for i in 1..5 {
            samples.push(
                sample_from_outcome(
                    i,
                    Record::Measured,
                    outcome(UnixStatus::Exited(0), i as u64, i as u64, 1, 0),
                )
                .unwrap(),
            );
        }
        samples.push(Sample {
            index: 5,
            record: Record::Measured,
            skipped: true,
            wall_ms: None,
            rss_kb: None,
            output_bytes: None,
            exit: 1,
            cache: CacheLabel::Fixture,
        });
        let err = scenario_from_samples(single, samples).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Scenario(ScenarioError::MissingWall { index: 5 })
        );
    }

    // ---- Test 6: synthetic five-scenario FakeOnly report -------------------

    #[test]
    fn fake_report_validates_with_exact_pin_mode_and_honesty_fields() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).unwrap();
        assert_eq!(descs.len(), 5);

        // Five fully-exercised synthetic scenarios.
        let scenarios: Vec<Scenario> = descs
            .iter()
            .enumerate()
            .map(|(i, d)| synth_scenario(d, 100 + i as u64, 5_000 + i as u64 * 100))
            .collect();

        let host_val = host().unwrap();
        let report = fake_report(manifest, host_val, scenarios).expect("report must validate");

        // Exact honesty / mode fields.
        assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
        assert_eq!(report.mode, Mode::Fake);
        assert_eq!(report.completeness, Completeness::FakeOnly);
        assert!(report.harness_only);
        assert!(report.nix_version.is_none());
        assert!(report.failures.is_empty());

        // Exact pin fields copied verbatim from the manifest.
        assert_eq!(report.pin.nix_version, manifest.nix.version);
        assert_eq!(report.pin.owner, manifest.nixpkgs.owner);
        assert_eq!(report.pin.repo, manifest.nixpkgs.repo);
        assert_eq!(report.pin.rev, manifest.nixpkgs.rev);
        assert_eq!(report.pin.nar_hash, manifest.nixpkgs.nar_hash);
        assert_eq!(report.pin.attr, manifest.attr);

        // Every sample is a Fixture.
        for scen in &report.scenarios {
            for s in &scen.samples {
                assert_eq!(s.cache, CacheLabel::Fixture);
            }
        }
        assert_eq!(report.scenarios.len(), 5);
    }

    // ---- Test 7: machine sanitizer + cores ---------------------------------

    #[test]
    fn sanitize_machine_strips_control_characters() {
        // \x00 and \x1f (unit separator) are control chars; alnum preserved.
        let got = sanitize_machine(Some(OsStr::new("b\x00en\x1fch-01")));
        assert_eq!(got, "bench-01");
    }

    #[test]
    fn sanitize_machine_bounds_to_max_chars() {
        let long: String = "a".repeat(MAX_MACHINE_CHARS + 50);
        let got = sanitize_machine(Some(OsStr::new(&long)));
        assert_eq!(got.len(), MAX_MACHINE_CHARS);
        assert!(long.starts_with(&got));
    }

    #[test]
    fn sanitize_machine_falls_back_to_unknown_when_absent() {
        assert_eq!(sanitize_machine(None), "unknown");
    }

    #[test]
    fn sanitize_machine_falls_back_to_unknown_when_empty() {
        assert_eq!(sanitize_machine(Some(OsStr::new(""))), "unknown");
        assert_eq!(
            sanitize_machine(Some(OsStr::new("\x01\x02"))),
            "unknown",
            "all-control input collapses to unknown"
        );
    }

    #[test]
    fn host_machine_never_panics_and_returns_bounded_string() {
        let m = host_machine();
        assert!(m.chars().count() <= MAX_MACHINE_CHARS);
        // No control characters survive.
        assert!(m.chars().all(|c| !c.is_control()));
        // Nonempty (always at least "unknown").
        assert!(!m.is_empty());
    }

    #[test]
    fn host_cores_is_at_least_one() {
        let cores = host_cores();
        assert!(cores >= 1, "cores must be >= 1 even on detection failure");
    }

    // ---- descriptors: stdout_cap_bytes wiring ------------------------------

    #[test]
    fn descriptors_stdout_cap_bytes_match_manifest_caps_exactly() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).expect("descriptors must assemble");

        // The single host-only descriptor uses the single_attr_stdout_bytes cap.
        let single = &descs[0];
        assert!(single.name.starts_with("single-attr:"));
        assert_eq!(
            single.stdout_cap_bytes.get(),
            manifest.caps.single_attr_stdout_bytes,
            "single-attr descriptor cap must equal caps.single_attr_stdout_bytes"
        );

        // All four index-meta descriptors use the index_stdout_bytes cap.
        let index_descs: Vec<&ScenarioDescriptor> = descs
            .iter()
            .filter(|d| d.name.starts_with("index-meta:"))
            .collect();
        assert_eq!(index_descs.len(), 4, "exactly four index-meta descriptors");
        for d in index_descs {
            assert_eq!(
                d.stdout_cap_bytes.get(),
                manifest.caps.index_stdout_bytes,
                "index descriptor {} cap must equal caps.index_stdout_bytes",
                d.name
            );
        }
    }

    // ---- fake_argv: exact nine-token protocol, no shell/nix/PATH -----------

    #[test]
    fn fake_argv_is_exactly_nine_tokens_for_payload_4096() {
        let argv = fake_argv(4096);
        assert_eq!(argv.len(), 9, "exactly nine OsString tokens");

        let expected: Vec<OsString> = vec![
            OsString::from(fake::MARKER),
            OsString::from("--stdout-bytes"),
            OsString::from("4096"),
            OsString::from("--stderr-bytes"),
            OsString::from("0"),
            OsString::from("--sleep-ms"),
            OsString::from("0"),
            OsString::from("--exit-code"),
            OsString::from("0"),
        ];
        assert_eq!(argv, expected, "exact token order and values");

        // None of the forbidden tokens appear anywhere in argv: no shell, no
        // `sh -c`, no nix, no PATH search — the program is supplied separately
        // by absolute path.
        let forbidden = ["sh", "bash", "-c", "nix", "PATH"];
        for tok in &argv {
            let s = tok.to_string_lossy().into_owned();
            assert!(
                !forbidden.contains(&s.as_str()),
                "forbidden token {s:?} present in argv"
            );
        }
        // First token is the marker, never a shell.
        assert_eq!(argv[0], OsString::from(fake::MARKER));
    }

    // ---- build_spec: absolute success + relative rejection -----------------

    #[test]
    fn build_spec_absolute_executable_succeeds_with_exact_fields() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).expect("descriptors must assemble");
        let descriptor = &descs[0]; // single-attr
        let stdout_cap = descriptor.stdout_cap_bytes;
        let stderr_cap = nz_cap(manifest.caps.stderr_bytes, ManifestField::StderrCap).unwrap();
        let timeout = Duration::from_secs(descriptor.timeout_seconds);
        let exe = Path::new("/opt/s4-runner/s4-runner");

        let spec = build_spec(exe, descriptor, stdout_cap, stderr_cap, timeout)
            .expect("absolute executable must build a valid spec");

        // Program passed verbatim — no PATH search, no shell.
        assert_eq!(spec.program, exe);
        // Args are exactly the fake-child argv.
        assert_eq!(spec.args, fake_argv(descriptor.stdout_payload));
        // Env is exactly the fail-closed child env (LANG=C / LC_ALL=C).
        assert_eq!(spec.env, child_env());
        assert_eq!(spec.env.len(), 2, "exactly two env entries");
        // NonZero caps carried verbatim.
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert!(spec.stdout_cap.get() > 0);
        assert!(spec.stderr_cap.get() > 0);
        // Timeout carried verbatim.
        assert_eq!(spec.timeout, timeout);
    }

    #[test]
    fn build_spec_rejects_relative_executable_as_spec_error_without_spawning() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).expect("descriptors must assemble");
        let descriptor = &descs[0];
        let stdout_cap = descriptor.stdout_cap_bytes;
        let stderr_cap = nz_cap(manifest.caps.stderr_bytes, ManifestField::StderrCap).unwrap();
        let timeout = Duration::from_secs(descriptor.timeout_seconds);

        // A relative executable is rejected by `CommandSpec::validate` BEFORE
        // any spawn is attempted: surfaced as the exact nested error. No process
        // is ever started — `build_spec` performs pure validation only and never
        // touches the executor / spawn / poll / kill / wait path.
        let err = build_spec(
            Path::new("s4-runner"),
            descriptor,
            stdout_cap,
            stderr_cap,
            timeout,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                RunnerError::Command(CommandError::Spec(SpecError::ProgramNotAbsolute { .. }))
            ),
            "expected ProgramNotAbsolute, got {err:?}"
        );

        // Sanity: the rejection is about the path, not the rest of the spec —
        // an absolute path with identical args builds successfully.
        assert!(
            build_spec(
                Path::new("/abs/s4-runner"),
                descriptor,
                stdout_cap,
                stderr_cap,
                timeout,
            )
            .is_ok()
        );
    }

    // ---- select_timeout boundary behavior ----------------------------------

    #[test]
    fn select_timeout_phase_wins_when_smaller_than_remaining() {
        let overall = Duration::from_secs(100);
        let elapsed = Duration::from_secs(10); // remaining = 90 s
        let phase = Duration::from_secs(30); // smaller than remaining
        assert_eq!(select_timeout(phase, elapsed, overall).unwrap(), phase);
    }

    #[test]
    fn select_timeout_remaining_wins_when_smaller_than_phase() {
        let overall = Duration::from_secs(100);
        let elapsed = Duration::from_secs(70); // remaining = 30 s
        let phase = Duration::from_secs(60); // larger than remaining
        let remaining = overall - elapsed;
        assert_eq!(select_timeout(phase, elapsed, overall).unwrap(), remaining);
    }

    #[test]
    fn select_timeout_accepts_exactly_min_timeout_remaining() {
        // remaining == MIN_TIMEOUT is accepted (the guard is strict `<`).
        let overall = MIN_TIMEOUT + Duration::from_secs(10);
        let elapsed = Duration::from_secs(10); // remaining == MIN_TIMEOUT
        let phase = Duration::from_secs(60); // larger than remaining
        assert_eq!(
            select_timeout(phase, elapsed, overall).unwrap(),
            MIN_TIMEOUT
        );
    }

    #[test]
    fn select_timeout_rejects_elapsed_equal_to_overall() {
        let overall = Duration::from_secs(100);
        let phase = Duration::from_secs(10);
        let err = select_timeout(phase, overall, overall).unwrap_err();
        assert_eq!(err, RunnerError::OverallTimeout);
    }

    #[test]
    fn select_timeout_rejects_elapsed_greater_than_overall() {
        let overall = Duration::from_secs(100);
        let phase = Duration::from_secs(10);
        let err = select_timeout(phase, overall + Duration::from_secs(1), overall).unwrap_err();
        assert_eq!(err, RunnerError::OverallTimeout);
    }

    #[test]
    fn select_timeout_rejects_sub_min_timeout_remaining() {
        // remaining = 500 µs < MIN_TIMEOUT (1 ms) -> rejected.
        let overall = Duration::from_secs(1) + Duration::from_micros(500);
        let elapsed = Duration::from_secs(1); // remaining = 500 µs
        let phase = Duration::from_secs(60);
        let err = select_timeout(phase, elapsed, overall).unwrap_err();
        assert_eq!(err, RunnerError::OverallTimeout);
    }

    // ---- iteration_plan: warmup-first, contiguous, exact count -------------

    #[test]
    fn iteration_plan_single_descriptor_is_warmup_first_then_contiguous_measured() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).expect("descriptors must assemble");
        let single = &descs[0]; // single-attr: warmup=1, measured=5
        assert!(single.name.starts_with("single-attr:"));
        assert_eq!(single.warmup, 1);
        assert_eq!(single.measured, 5);

        let plan = iteration_plan(single);
        let expected: Vec<(Record, u32)> = vec![
            (Record::Warmup, 0),
            (Record::Measured, 1),
            (Record::Measured, 2),
            (Record::Measured, 3),
            (Record::Measured, 4),
            (Record::Measured, 5),
        ];
        assert_eq!(plan, expected);

        // Structural invariants: warmup-first grouping, then all measured,
        // with contiguous 0..N indices.
        assert_eq!(plan.len(), (single.warmup + single.measured) as usize);
        assert_eq!(plan[0].0, Record::Warmup);
        assert!(plan[1..].iter().all(|(r, _)| *r == Record::Measured));
        for (pos, (_, idx)) in plan.iter().enumerate() {
            assert_eq!(*idx, pos as u32, "indices must be contiguous 0..N");
        }
    }

    #[test]
    fn iteration_plan_index_descriptor_same_warmup_first_contiguous_count() {
        let manifest = benchmark_manifest();
        let descs = descriptors(manifest).expect("descriptors must assemble");
        let index = &descs[1]; // first index-meta: warmup=1, measured=3
        assert!(index.name.starts_with("index-meta:"));
        assert_eq!(index.warmup, 1);
        assert_eq!(index.measured, manifest.sampling.index_samples);

        let plan = iteration_plan(index);
        let expected: Vec<(Record, u32)> = vec![
            (Record::Warmup, 0),
            (Record::Measured, 1),
            (Record::Measured, 2),
            (Record::Measured, 3),
        ];
        assert_eq!(plan, expected);

        // Same warmup-first / contiguous / exact-count invariants.
        assert_eq!(plan.len(), (index.warmup + index.measured) as usize);
        assert_eq!(plan[0].0, Record::Warmup);
        assert!(plan[1..].iter().all(|(r, _)| *r == Record::Measured));
        for (pos, (_, idx)) in plan.iter().enumerate() {
            assert_eq!(*idx, pos as u32, "indices must be contiguous 0..N");
        }
    }

    // ---- nz_cap: nonzero roundtrip + zero exact invariant ------------------

    #[test]
    fn nz_cap_roundtrips_nonzero_and_reports_cap_not_positive_field_for_zero() {
        // Nonzero manifest caps round-trip through NonZeroU64.
        assert_eq!(
            nz_cap(1_048_576, ManifestField::SingleAttrStdoutCap)
                .unwrap()
                .get(),
            1_048_576
        );
        assert_eq!(
            nz_cap(268_435_456, ManifestField::IndexStdoutCap)
                .unwrap()
                .get(),
            268_435_456
        );
        assert_eq!(
            nz_cap(8_388_608, ManifestField::StderrCap).unwrap().get(),
            8_388_608
        );

        // Zero yields the exact Manifest(ManifestInvariant::CapNotPositive(field))
        // for each field label.
        assert_eq!(
            nz_cap(0, ManifestField::SingleAttrStdoutCap).unwrap_err(),
            RunnerError::Manifest(ManifestInvariant::CapNotPositive(
                ManifestField::SingleAttrStdoutCap
            ))
        );
        assert_eq!(
            nz_cap(0, ManifestField::IndexStdoutCap).unwrap_err(),
            RunnerError::Manifest(ManifestInvariant::CapNotPositive(
                ManifestField::IndexStdoutCap
            ))
        );
        assert_eq!(
            nz_cap(0, ManifestField::StderrCap).unwrap_err(),
            RunnerError::Manifest(ManifestInvariant::CapNotPositive(ManifestField::StderrCap))
        );
    }

    // ---- public error surface: bounded, named, leak-free -------------------

    #[test]
    fn runner_error_public_surface_is_bounded_named_and_leak_free() {
        // Generic name/boundedness check: every RunnerError Display is prefixed
        // "runner:", is length-bounded, and never embeds arbitrary child output.
        fn check(name: &str, err: &RunnerError, poison: &str) {
            let msg = err.to_string();
            assert!(
                msg.starts_with("runner:"),
                "{name}: Display must be prefixed \"runner:\", got {msg:?}"
            );
            assert!(
                msg.len() < 512,
                "{name}: Display must be bounded, was {}: {msg:?}",
                msg.len()
            );
            assert!(
                !msg.contains(poison),
                "{name}: arbitrary child output leaked into Display: {msg:?}"
            );
        }

        // A representative child-output payload that must NEVER appear.
        const POISON: &str = "LEAKED-CHILD-OUTPUT-9999-XYZZY";

        // OverallTimeout: fixed string, no payload.
        check("overall", &RunnerError::OverallTimeout, POISON);
        assert!(
            RunnerError::OverallTimeout
                .to_string()
                .contains("overall wall-clock budget exceeded")
        );

        // Manifest zero-cap: bounded field identifier, no child payload.
        let manifest_err = RunnerError::Manifest(ManifestInvariant::CapNotPositive(
            ManifestField::SingleAttrStdoutCap,
        ));
        check("manifest", &manifest_err, POISON);
        let m = manifest_err.to_string();
        assert!(m.contains("caps.singleAttrStdoutBytes"));
        assert!(m.contains("must be positive"));

        // A Command spec error (relative program) wraps a bounded SpecError; the
        // bounded path snippet is truncated and no child output is embedded.
        let cmd_err = RunnerError::Command(CommandError::Spec(SpecError::ProgramNotAbsolute {
            got: "s4-runner".to_owned(),
        }));
        check("command-spec", &cmd_err, POISON);
        assert!(
            cmd_err
                .to_string()
                .contains("program path must be absolute")
        );
    }
}
