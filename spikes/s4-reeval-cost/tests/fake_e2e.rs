//! Black-box end-to-end tests for the declared `s4-runner` binary behavior.
//!
//! These tests drive the *compiled* `s4-runner` binary only: they spawn it as a
//! subprocess and assert on its observable effects (process exit codes, the
//! stdout/stderr byte streams, and the `report.json` / `summary.md` artifacts it
//! writes). They do NOT reach into private crate internals. The public
//! [`Report`] type, [`benchmark_manifest`], and [`fake::MARKER`](pkg_spike_s4_reeval_cost::fake::MARKER)
//! are used solely to *interpret* the emitted artifacts and to derive the
//! expected scenario set that `benchmark.json` declares.
//!
//! No test here touches the network or invokes Nix. The Fake pipeline re-invokes
//! this exact binary as deterministic fixture children (no shell, no `PATH`,
//! no network, no Nix), and the hidden fake-child protocol is exercised here
//! directly with fixed sizes and a selected exit status.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_spike_s4_reeval_cost::fake::MARKER;
use pkg_spike_s4_reeval_cost::manifest::benchmark_manifest;
use pkg_spike_s4_reeval_cost::report::{
    CacheLabel, Completeness, Mode, REPORT_SCHEMA_VERSION, Record, Report, render_markdown,
};

/// Absolute path to the compiled `s4-runner` binary, as injected by Cargo for
/// integration tests (`CARGO_BIN_EXE_<name>` for `[[bin]] name = "s4-runner"`).
const BIN: &str = env!("CARGO_BIN_EXE_s4-runner");

/// Known fixture stdout payloads emitted by the Fake runner. These mirror the
/// crate-private constants `SINGLE_ATTR_STDOUT_PAYLOAD` / `INDEX_STDOUT_PAYLOAD`
/// in `runner.rs` (which are `pub(crate)` and therefore unreachable from an
/// integration test). The single-attribute child emits 4096 bytes; every
/// index-meta child emits 131072 bytes.
const SINGLE_ATTR_PAYLOAD_FLOOR: u64 = 4096;
const INDEX_PAYLOAD_FLOOR: u64 = 131_072;

/// Monotonic counter guaranteeing every temp directory is unique even when two
/// tests allocate one within the same nanosecond (e.g. under parallel threads).
static UNIQUIFIER: AtomicU64 = AtomicU64::new(0);

/// RAII guard owning a unique temp directory; removes it (best-effort) on drop
/// so a passing/failing/panicking test never leaves litter behind. Built with
/// `std` only — no `tempfile` dev-dependency exists in this spike.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> TempDir {
        let mut dir = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = UNIQUIFIER.fetch_add(1, Ordering::Relaxed);
        dir.push(format!("s4-fake-e2e-{label}-{pid}-{nanos}-{counter}"));
        fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("could not create temp dir {}: {e}", dir.display()));
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a failed cleanup must never mask the real test failure.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The captured outcome of one `s4-runner` invocation.
struct Outcome {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Outcome {
    /// The numeric exit code (panics if the process was killed by a signal,
    /// which none of these paths are).
    fn code(&self) -> i32 {
        self.status
            .code()
            .expect("s4-runner should exit with a numeric code, not a signal")
    }

    fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Spawn `s4-runner` per `cmd` and capture its exit status + streams.
fn capture(cmd: &mut Command) -> Outcome {
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn s4-runner ({BIN}): {e}"));
    Outcome {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

/// Invoke the hidden fake-child protocol directly:
/// `s4-runner s4-fake-child --stdout-bytes N --stderr-bytes M --sleep-ms S --exit-code C`.
fn fake_child(stdout: u64, stderr: u64, sleep_ms: u64, exit: i32) -> Outcome {
    capture(
        Command::new(BIN)
            .arg(MARKER)
            .arg("--stdout-bytes")
            .arg(stdout.to_string())
            .arg("--stderr-bytes")
            .arg(stderr.to_string())
            .arg("--sleep-ms")
            .arg(sleep_ms.to_string())
            .arg("--exit-code")
            .arg(exit.to_string()),
    )
}

/// Run `s4-runner fake --out-dir <dir>` and return the captured outcome.
fn run_fake(out_dir: &Path) -> Outcome {
    capture(Command::new(BIN).arg("fake").arg("--out-dir").arg(out_dir))
}

// ---------------------------------------------------------------------------
// Fake run: artifacts, validated Report, five scenarios, per-scenario metrics
// ---------------------------------------------------------------------------

#[test]
fn fake_run_writes_validated_fixture_only_report_and_summary() {
    let dir = TempDir::new("fake");
    let out = run_fake(dir.path());

    // The full Fake run exits zero.
    assert!(
        out.status.success(),
        "fake run must exit zero; stderr: {}",
        out.stderr_str(),
    );

    // Both artifacts exist under the requested output directory.
    let report_path = dir.path().join("report.json");
    let summary_path = dir.path().join("summary.md");
    assert!(report_path.exists(), "report.json must exist");
    assert!(summary_path.exists(), "summary.md must exist");

    // report.json deserializes to the public Report type and passes validate().
    let json = fs::read_to_string(&report_path).expect("report.json must be readable");
    let report: Report =
        serde_json::from_str(&json).expect("report.json must deserialize into the public Report");
    report
        .validate()
        .expect("report must pass Report::validate");

    // Headline honesty invariants for a Fake run.
    assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
    assert_eq!(report.mode, Mode::Fake);
    assert_eq!(report.completeness, Completeness::FakeOnly);
    assert!(
        report.harness_only,
        "a Fake report must set harnessOnly = true",
    );
    assert!(
        report.nix_version.is_none(),
        "a Fake report must carry NO detected nixVersion",
    );
    assert!(
        report.failures.is_empty(),
        "a Fake report must record no failures"
    );

    // Exactly five scenarios: one host single-attr + one index-meta per manifest
    // system, in manifest order.
    let manifest = benchmark_manifest();
    assert_eq!(manifest.systems.len(), 4, "manifest declares four systems");
    let expected_single = format!("single-attr:{}", manifest.attr);
    assert_eq!(report.scenarios.len(), 5, "1 single-attr + 4 index-meta");

    let single = &report.scenarios[0];
    assert_eq!(
        single.name, expected_single,
        "scenario 0 is the host single-attr"
    );
    assert_eq!(
        single.system, report.host.system,
        "single-attr targets the host system",
    );

    for (i, system) in manifest.systems.iter().enumerate() {
        let scen = &report.scenarios[1 + i];
        assert_eq!(
            scen.name,
            format!("index-meta:{system}"),
            "index-meta scenarios follow manifest.systems order",
        );
        assert_eq!(scen.system, *system);
    }

    // Per-scenario deep checks: declared + actual warmup/measured counts from
    // benchmark.json, contiguous 0..N-1 indices, Fixture cache labels, complete
    // wall/rss/output metrics, and outputBytes at least the known fixture
    // payload (wall and RSS are NOT asserted for stable values — only presence).
    for scen in &report.scenarios {
        let is_single = scen.name == expected_single;
        let expected_warmup = manifest.sampling.warmup;
        let expected_measured = if is_single {
            manifest.sampling.single_attr_samples
        } else {
            manifest.sampling.index_samples
        };

        // Declared counts match benchmark.json.
        assert_eq!(
            scen.warmup, expected_warmup,
            "{}: declared warmup",
            scen.name,
        );
        assert_eq!(
            scen.measured, expected_measured,
            "{}: declared measured",
            scen.name,
        );

        // Actual record counts match the declared counts.
        let warmup_found = scen
            .samples
            .iter()
            .filter(|s| s.record == Record::Warmup)
            .count() as u32;
        let measured_found = scen
            .samples
            .iter()
            .filter(|s| s.record == Record::Measured)
            .count() as u32;
        assert_eq!(
            warmup_found, expected_warmup,
            "{}: actual warmup record count",
            scen.name,
        );
        assert_eq!(
            measured_found, expected_measured,
            "{}: actual measured record count",
            scen.name,
        );
        assert_eq!(
            scen.samples.len(),
            (expected_warmup + expected_measured) as usize,
            "{}: total sample count",
            scen.name,
        );

        // Contiguous in-order indices, Fixture labels, complete metrics.
        let payload_floor = if is_single {
            SINGLE_ATTR_PAYLOAD_FLOOR
        } else {
            INDEX_PAYLOAD_FLOOR
        };
        for (pos, sample) in scen.samples.iter().enumerate() {
            assert_eq!(
                sample.index, pos as u32,
                "{}: contiguous 0..N-1 sample index",
                scen.name,
            );
            assert_eq!(
                sample.cache,
                CacheLabel::Fixture,
                "{}: every sample is labelled Fixture",
                scen.name,
            );
            assert!(!sample.skipped, "{}: no skipped sample", scen.name);
            assert_eq!(sample.exit, 0, "{}: sample exits 0", scen.name);
            // Wall and RSS are present (NOT compared for stable values).
            assert!(sample.wall_ms.is_some(), "{}: wall_ms present", scen.name);
            assert!(sample.rss_kb.is_some(), "{}: rss_kb present", scen.name);
            // Output is at least the known fixture payload for this scenario
            // type (the runner reports stdout + the `/usr/bin/time` stderr
            // statistics, so this is a floor, not an exact value).
            let output_bytes = sample.output_bytes.expect("output_bytes present");
            assert!(
                output_bytes >= payload_floor,
                "{}: outputBytes {} must be >= fixture payload {}",
                scen.name,
                output_bytes,
                payload_floor,
            );
        }

        // Statistics blocks exist for both metrics over the measured samples.
        let wall_stats = scen
            .statistics
            .wall
            .as_ref()
            .expect("{}: wall statistics present");
        assert_eq!(
            wall_stats.count, expected_measured,
            "{}: wall statistics count",
            scen.name,
        );
        let rss_stats = scen
            .statistics
            .rss
            .as_ref()
            .expect("{}: rss statistics present");
        assert_eq!(
            rss_stats.count, expected_measured,
            "{}: rss statistics count",
            scen.name,
        );
    }

    // The Markdown summary names all five scenarios and clearly labels the
    // report as fixture-only / non-Real evidence (essential phrases only).
    let summary = fs::read_to_string(&summary_path).expect("summary.md must be readable");
    assert!(
        summary.contains(&expected_single),
        "summary must name the single-attr scenario",
    );
    for system in &manifest.systems {
        assert!(
            summary.contains(&format!("index-meta:{system}")),
            "summary must name the index-meta:{system} scenario",
        );
    }
    assert!(summary.contains("fake"), "summary labels mode = fake");
    assert!(
        summary.contains("fakeOnly"),
        "summary labels completeness = fakeOnly",
    );
    assert!(
        summary.contains("fixture"),
        "summary labels every sample cache = fixture",
    );
    assert!(
        summary.contains("harnessOnly | true"),
        "summary labels harnessOnly = true (non-Real evidence)",
    );
}

// ---------------------------------------------------------------------------
// Hidden fake-child: exact stdout/stderr lengths + selected exit status
// ---------------------------------------------------------------------------

#[test]
fn hidden_fake_child_exact_output_lengths_and_exit_status() {
    // Distinct stdout and stderr sizes, exit 0: both streams exact, success.
    let o = fake_child(1000, 500, 0, 0);
    assert!(o.status.success(), "exit 0 child should succeed");
    assert_eq!(o.code(), 0);
    assert_eq!(o.stdout.len(), 1000, "stdout byte count is exact");
    assert_eq!(o.stderr.len(), 500, "stderr byte count is exact");

    // The single-attribute fixture payload size, exit 0.
    let o = fake_child(SINGLE_ATTR_PAYLOAD_FLOOR, 0, 0, 0);
    assert!(o.status.success());
    assert_eq!(o.code(), 0);
    assert_eq!(o.stdout.len() as u64, SINGLE_ATTR_PAYLOAD_FLOOR);
    assert_eq!(o.stderr.len(), 0);

    // The index fixture payload size, exit 0.
    let o = fake_child(INDEX_PAYLOAD_FLOOR, 0, 0, 0);
    assert!(o.status.success());
    assert_eq!(o.code(), 0);
    assert_eq!(o.stdout.len() as u64, INDEX_PAYLOAD_FLOOR);
    assert_eq!(o.stderr.len(), 0);

    // A selected nonzero exit status is propagated verbatim, with no output.
    let o = fake_child(0, 0, 0, 42);
    assert!(!o.status.success(), "nonzero exit child should not succeed");
    assert_eq!(o.code(), 42, "selected exit status is propagated exactly");
    assert_eq!(o.stdout.len(), 0);
    assert_eq!(o.stderr.len(), 0);
}

// ---------------------------------------------------------------------------
// CLI: standalone help succeeds
// ---------------------------------------------------------------------------

#[test]
fn standalone_help_succeeds() {
    let o = capture(Command::new(BIN).arg("--help"));
    assert!(o.status.success(), "--help must exit 0");
    let usage = o.stdout_str();
    assert!(
        usage.contains("s4-runner"),
        "usage banner mentions the program",
    );
    assert!(usage.contains("fake"), "usage documents the fake mode");

    // The short form is also standalone help.
    let o = capture(Command::new(BIN).arg("-h"));
    assert!(o.status.success(), "-h must exit 0");
}

// ---------------------------------------------------------------------------
// CLI: malformed input exits EX_USAGE (64)
// ---------------------------------------------------------------------------

#[test]
fn malformed_cli_exits_64() {
    // An unknown token (no mode recognized first).
    let o = capture(Command::new(BIN).arg("bogus"));
    assert_eq!(o.code(), 64, "unknown token exits EX_USAGE (64)");

    // No mode token at all.
    let mut empty = Command::new(BIN);
    let o = capture(&mut empty);
    assert_eq!(o.code(), 64, "missing mode exits EX_USAGE (64)");
}

// ---------------------------------------------------------------------------
// Real mode: wired diagnostic-incomplete contract when Nix cannot execute
// ---------------------------------------------------------------------------
//
// Real mode is now fully wired. Driving it with a guaranteed-missing absolute
// `nix` binary (under a private temp dir, so the test never depends on a real
// Nix installation and never assumes a platform-local fixed path) MUST fold
// into a validated Incomplete Real report: exit `EX_UNAVAILABLE` (69), one
// bounded caller-data-free stderr line, both artifacts written, zero scenarios,
// and exactly one global `detect-nix` failure whose fixed message names the
// command failure (never the path).

#[test]
fn real_mode_missing_nix_is_diagnostic_incomplete_exit_69() {
    let dir = TempDir::new("real");
    // An absolute, guaranteed-missing `nix` binary path UNDER the temp dir: it
    // never exists (the temp dir is freshly created and we never create this
    // child), so the test never depends on a real Nix installation and never
    // assumes a fixed platform path like `/nonexistent`.
    let nix_bin = dir.path().join("definitely-missing-s4-nix-binary");
    assert!(nix_bin.is_absolute(), "precondition: nix_bin is absolute");
    assert!(!nix_bin.exists(), "precondition: nix_bin must not exist");

    let o = capture(
        Command::new(BIN)
            .arg("real")
            .arg("--nix-bin")
            .arg(&nix_bin)
            .arg("--out-dir")
            .arg(dir.path()),
    );

    // Exact exit code: EX_UNAVAILABLE (69), the Incomplete-Real diagnostic.
    assert_eq!(
        o.code(),
        69,
        "an Incomplete Real run must exit EX_UNAVAILABLE (69); \
         got status {:?}; stdout: {}; stderr: {}",
        o.status,
        o.stdout_str(),
        o.stderr_str(),
    );

    // stdout is byte-empty (no success line is printed for an Incomplete run).
    assert!(
        o.stdout.is_empty(),
        "stdout must be empty for an Incomplete Real run; got {:?}",
        o.stdout_str(),
    );

    // stderr is EXACTLY the one fixed, bounded, caller-data-free diagnostic
    // line (verbatim, with its single trailing newline) — never more.
    let expected_stderr = "s4-runner: real run was incomplete; wrote report.json and summary.md\n";
    assert_eq!(
        o.stderr.as_slice(),
        expected_stderr.as_bytes(),
        "stderr must be exactly the fixed bounded diagnostic line",
    );

    // Neither stream leaks the missing nix path (the contract is caller-data-
    // free; the runner never echoes `nix_bin`).
    let path_lossy = nix_bin.to_string_lossy();
    assert!(
        !o.stdout_str().contains(&*path_lossy),
        "stdout must not leak the missing nix path",
    );
    assert!(
        !o.stderr_str().contains(&*path_lossy),
        "stderr must not leak the missing nix path",
    );

    // Both artifacts ALWAYS exist for an Incomplete Real run (written through
    // the SAME shared atomic writer as Fake mode).
    let report_path = dir.path().join("report.json");
    let summary_path = dir.path().join("summary.md");
    assert!(
        report_path.exists(),
        "report.json must exist for an Incomplete Real run"
    );
    assert!(
        summary_path.exists(),
        "summary.md must exist for an Incomplete Real run"
    );

    // No leftover sibling `.tmp` files: every atomic write either renamed its
    // temp into place or removed it on failure.
    let mut leftover_tmp: Vec<String> = fs::read_dir(dir.path())
        .expect("output directory is readable")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    leftover_tmp.sort();
    assert!(
        leftover_tmp.is_empty(),
        "no sibling .tmp files may remain after the run; found {leftover_tmp:?}",
    );

    // report.json has EXACTLY one trailing newline (not zero, not two).
    let report_bytes = fs::read(&report_path).expect("report.json must be readable");
    assert!(!report_bytes.is_empty(), "report.json must not be empty");
    assert_eq!(
        *report_bytes.last().unwrap(),
        b'\n',
        "report.json must end with a trailing newline",
    );
    assert_ne!(
        report_bytes[report_bytes.len() - 2],
        b'\n',
        "report.json must end with exactly one newline (not two)",
    );

    // report.json deserializes into the public Report and passes validate().
    let report: Report =
        serde_json::from_slice(&report_bytes).expect("report.json must deserialize into Report");
    report
        .validate()
        .expect("report must pass Report::validate");

    // Headline honesty invariants for a missing-Nix Real Incomplete report.
    assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
    assert_eq!(report.mode, Mode::Real);
    assert_eq!(report.completeness, Completeness::Incomplete);
    assert!(
        !report.harness_only,
        "a Real report must set harnessOnly = false"
    );
    assert!(
        report.nix_version.is_none(),
        "a missing-Nix Real report must carry NO detected nixVersion",
    );
    assert!(
        report.scenarios.is_empty(),
        "a missing-Nix Real report must record zero scenarios",
    );

    // Exactly one failure: the overall run, the detect-nix stage, the fixed
    // command-failure message (never the path, never dynamic Nix output).
    assert_eq!(
        report.failures.len(),
        1,
        "a missing-Nix Real report must record exactly one failure",
    );
    let failure = &report.failures[0];
    assert_eq!(
        failure.scenario, "run",
        "failure scenario is the overall run"
    );
    assert_eq!(
        failure.stage, "detect-nix",
        "failure stage is the nix probe"
    );
    assert_eq!(
        failure.message, "failed to execute the pinned Nix binary",
        "failure message is the fixed bounded detect-nix command message",
    );

    // summary.md is byte-equal to render_markdown(&report).
    let summary_bytes = fs::read(&summary_path).expect("summary.md must be readable");
    let expected_markdown = render_markdown(&report);
    assert_eq!(
        summary_bytes,
        expected_markdown.as_bytes(),
        "summary.md must equal render_markdown(&report)",
    );

    // The summary carries clear Real / Incomplete / detect-nix diagnostic
    // evidence: these are the fixed, human-readable renderings of the report's
    // own fields (not dynamic Nix output, and never the missing path).
    let summary = String::from_utf8(summary_bytes).expect("summary.md is valid UTF-8");
    assert!(
        summary.contains("| mode | real |"),
        "summary labels mode = real"
    );
    assert!(
        summary.contains("| completeness | incomplete |"),
        "summary labels completeness = incomplete",
    );
    assert!(
        summary.contains("| harnessOnly | false |"),
        "summary labels harnessOnly = false (Real evidence)",
    );
    assert!(
        summary.contains("| run | detect-nix |"),
        "summary records the detect-nix failure for the overall run",
    );
    assert!(
        summary.contains("failed to execute the pinned Nix binary"),
        "summary records the fixed detect-nix failure message",
    );
    assert!(
        !summary.contains(&*path_lossy),
        "summary must not leak the missing nix path",
    );
}
