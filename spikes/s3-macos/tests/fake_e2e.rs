//! Spike S3 (PR-7) — black-box integration tests for the `s3-probe` binary and
//! the real [`pkg_spike_s3_macos::command`] executor.
//!
//! These tests:
//!   * run the binary end-to-end for **Fake** mode and assert the artifacts +
//!     exit code + determinism;
//!   * exercise the closed CLI grammar's rejections (exit 64, bounded stderr,
//!     no credential echoed);
//!   * drive the REAL command executor ([`pkg_spike_s3_macos::command::run`])
//!     through the binary's hidden fixture-child protocol to verify success,
//!     nonzero-as-outcome, cap overflow, timeout group-kill, and the clean
//!     fail-closed environment.
//!
//! The **live Detect** CLI is NEVER executed here (it may read the user
//! keychain). Detect-transcript injection lives in the `detect` unit tests.

#![forbid(unsafe_code)]

use pkg_spike_s3_macos as s3;
use pkg_spike_s3_macos::command::{CommandError, CommandSpec, ProbeStatus, run};

use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The hidden fixture-child marker (must match `main.rs`).
const FIXTURE_CHILD_MARKER: &str = "s3-probe-fixture-child";

/// Resolve the built `s3-probe` binary path (Cargo sets
/// `CARGO_BIN_EXE_s3-probe` for integration tests).
fn s3_probe() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_s3-probe") {
        return PathBuf::from(p);
    }
    if let Some(p) = option_env!("CARGO_BIN_EXE_s3_probe") {
        return PathBuf::from(p);
    }
    panic!("CARGO_BIN_EXE_s3-probe not set; run via `cargo test`");
}

/// An RAII scratch directory unique across processes and threads.
struct Scratch {
    dir: PathBuf,
}
impl Scratch {
    fn new(label: &str) -> Scratch {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("s3-e2e-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("scratch dir");
        Scratch { dir }
    }
    fn path(&self) -> &Path {
        &self.dir
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn run_bin(args: &[&str]) -> (std::process::Output, PathBuf) {
    let out = Command::new(s3_probe())
        .args(args)
        .output()
        .expect("spawn s3-probe");
    (out, s3_probe())
}

// ---------------------------------------------------------------------------
// Fake mode end-to-end
// ---------------------------------------------------------------------------

#[test]
fn fake_mode_writes_valid_artifacts_and_exits_zero() {
    let scratch = Scratch::new("fake");
    let (out, _) = run_bin(&["fake", "--out-dir", scratch.path().to_str().unwrap()]);
    assert!(out.status.success(), "exit {:?}", out.status);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout, "s3-probe: wrote report.json and summary.md\n");

    let json = fs::read_to_string(scratch.path().join("report.json")).unwrap();
    assert!(json.ends_with('\n'));
    let report: s3::Report = serde_json::from_str(&json).unwrap();
    report.validate().expect("report validates");
    assert_eq!(report.mode, s3::Mode::Fake);
    assert!(report.harness_only);
    assert_eq!(report.lanes.fake.state, s3::LaneState::Complete);

    let md = fs::read_to_string(scratch.path().join("summary.md")).unwrap();
    assert!(md.contains("# S3 macOS spike report"));
    assert!(md.contains("| mode | fake |"));
}

#[test]
fn fake_mode_artifacts_are_deterministic_across_runs() {
    let a = Scratch::new("det-a");
    let b = Scratch::new("det-b");
    run_bin(&["fake", "--out-dir", a.path().to_str().unwrap()]);
    run_bin(&["fake", "--out-dir", b.path().to_str().unwrap()]);
    let ja = fs::read_to_string(a.path().join("report.json")).unwrap();
    let jb = fs::read_to_string(b.path().join("report.json")).unwrap();
    assert_eq!(ja, jb);
    let ma = fs::read_to_string(a.path().join("summary.md")).unwrap();
    let mb = fs::read_to_string(b.path().join("summary.md")).unwrap();
    assert_eq!(ma, mb);
}

#[test]
fn no_args_prints_help_and_exits_zero() {
    let (out, _) = run_bin(&[]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("USAGE:"));
    assert!(stdout.contains("s3-probe fake"));
}

#[test]
fn help_flag_prints_usage() {
    let (out, _) = run_bin(&["--help"]);
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("USAGE:"));
    let (out, _) = run_bin(&["-h"]);
    assert!(out.status.success());
}

// ---------------------------------------------------------------------------
// CLI rejections (exit 64, bounded stderr, no secrets)
// ---------------------------------------------------------------------------

#[test]
fn cli_errors_exit_64_with_bounded_stderr() {
    for bad in [
        &["bogus"][..],
        &["--out-dir", "x", "fake"][..],
        &["fake", "--out-dir=/tmp"][..],
        &["fake", "--out-dir", "a", "--out-dir", "b"][..],
        &["fake", "extra"][..],
        &["fake", "--out-dir"][..],
        &["detect", "--nix-bin", "rel"][..],
        &["fake", "--nix-bin", "/nix"][..],
    ] {
        let (out, _) = run_bin(bad);
        assert_eq!(out.status.code(), Some(64), "args {bad:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.starts_with("s3-probe: cli:"));
        // Bounded: no huge token dump.
        assert!(
            stderr.len() < 512,
            "stderr too long for {bad:?}: {stderr:?}"
        );
    }
}

#[test]
fn cli_signing_options_exit_64_without_echoing_credential() {
    // A credential-shaped option is denied; the offered VALUE must never appear.
    let (out, _) = run_bin(&["detect", "--identity", "SUPERSECRETVALUE"]);
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("signing credential option not accepted"));
    assert!(!stderr.contains("SUPERSECRETVALUE"));
    assert!(!stderr.contains("identity"));
}

#[test]
fn cli_preflight_grammar_rejections_exit_64_with_bounded_stderr() {
    // Preflight keeps the same closed grammar as the other modes.
    for bad in [
        &["preflight"][..],
        &["preflight", "--out-dir", "/tmp"][..],
        &["preflight", "--nix-bin", "/nix", "--nix-bin", "/nix"][..],
        &["preflight", "--nix-bin", "rel"][..],
        &["preflight", "--nix-bin", ""][..],
        &["preflight", "--nix-bin=/nix"][..],
        &["preflight", "--nix-bin", "/nix", "extra"][..],
        &["preflight", "--nix-bin"][..],
        &["preflight", "--out-dir"][..],
        &["preflight", "--verbose", "/nix"][..],
        &["--nix-bin", "/nix", "preflight"][..],
    ] {
        let (out, _) = run_bin(bad);
        assert_eq!(out.status.code(), Some(64), "args {bad:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(
            stderr.starts_with("s3-probe: cli:"),
            "bad stderr for {bad:?}: {stderr:?}"
        );
        assert!(
            stderr.len() < 512,
            "stderr too long for {bad:?}: {stderr:?}"
        );
        // Never echoes the supplied path value.
        assert!(!stderr.contains("/nix"));
    }
}

#[test]
fn cli_preflight_signing_option_denied_without_echoing_credential() {
    let (out, _) = run_bin(&["preflight", "--identity", "HUSH", "--nix-bin", "/nix"]);
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("signing credential option not accepted"));
    assert!(!stderr.contains("HUSH"));
}

#[test]
fn cli_help_must_be_standalone_exit_64_no_secret_echoed() {
    // --help/-h with ANY trailing token is a closed, bounded error (exit 64).
    // The trailing token is NEVER echoed — not even a credential value — and
    // the help text is NOT printed on the error path.
    for bad in [
        &["--help", "extra"][..],
        &["-h", "extra"][..],
        &["--help", "--help"][..],
        &["--help", "--keychain-password", "SECRET"][..],
        &["--help", "--out-dir"][..],
    ] {
        let (out, _) = run_bin(bad);
        assert_eq!(out.status.code(), Some(64), "args {bad:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(
            stderr.starts_with("s3-probe: cli:"),
            "bad stderr for {bad:?}: {stderr:?}"
        );
        assert!(
            stderr.len() < 512,
            "stderr too long for {bad:?}: {stderr:?}"
        );
        // Never echoes the trailing token or a credential value.
        assert!(!stderr.contains("extra"), "{bad:?}: {stderr:?}");
        assert!(!stderr.contains("SECRET"), "{bad:?}: {stderr:?}");
        assert!(!stderr.contains("keychain"), "{bad:?}: {stderr:?}");
        assert!(!stderr.contains("password"), "{bad:?}: {stderr:?}");
        // The help text is NOT printed on the error path (stdout stays empty).
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(!stdout.contains("USAGE:"), "{bad:?}: help leaked to stdout");
        assert!(stdout.is_empty(), "{bad:?}: stdout leaked: {stdout:?}");
    }
}

// ---------------------------------------------------------------------------
// Preflight mode end-to-end (missing Nix binary => NixMissing, no child starts)
// ---------------------------------------------------------------------------

/// An absolute `--nix-bin` path UNDER `scratch` that is asserted NOT to exist
/// before invocation, so the production RealRunner spawn fails with `NotFound`
/// BEFORE any child is exec'd. No real Nix runs, no network is touched, and the
/// Nix store is never mutated: the only artifact is the bounded
/// `FailureKind::NixMissing` report. The path is per-test (under the test's
/// fresh, process-unique Scratch dir), so it can never collide with a real Nix
/// install or another test; the file is NEVER created, and no fake Nix
/// interpreter is supplied.
fn missing_nix_bin(scratch: &Scratch) -> PathBuf {
    let p = scratch.path().join("no-such-nix-binary");
    assert!(
        !p.exists(),
        "preflight --nix-bin path must not exist before invocation: {}",
        p.display()
    );
    p
}

#[test]
fn preflight_missing_nix_bin_exits_69_with_both_valid_artifacts() {
    let scratch = Scratch::new("preflight-missing");
    let nix_bin = missing_nix_bin(&scratch);
    let start = std::time::Instant::now();
    let (out, _) = run_bin(&[
        "preflight",
        "--nix-bin",
        nix_bin.to_str().unwrap(),
        "--out-dir",
        scratch.path().to_str().unwrap(),
    ]);
    // The spawn failure is immediate (no child, no network): bounded.
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "preflight against a missing binary took too long: {:?}",
        start.elapsed()
    );

    // Incomplete Preflight (NixMissing) still writes both artifacts, then
    // exits 69 (EX_UNAVAILABLE).
    assert_eq!(out.status.code(), Some(69), "exit {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        stderr,
        "s3-probe: preflight run was incomplete; wrote report.json and summary.md\n"
    );
    // No success line on stdout for an incomplete run.
    assert!(out.stdout.is_empty(), "stdout leaked: {:?}", out.stdout);

    let json = fs::read_to_string(scratch.path().join("report.json")).unwrap();
    assert!(json.ends_with('\n'));
    let report: s3::Report = serde_json::from_str(&json).unwrap();
    report.validate().expect("report validates");
    assert_eq!(report.mode, s3::Mode::Preflight);
    assert!(!report.harness_only);
    let lane = &report.lanes.preflight;
    assert_eq!(lane.state, s3::LaneState::Incomplete);
    assert_eq!(lane.reason, None);
    assert_eq!(
        lane.failures,
        vec![s3::Failure {
            stage: s3::Stage::Preflight,
            kind: s3::FailureKind::NixMissing,
        }]
    );
    // The other lanes are Pending/NotSelected.
    for (mode, state) in [
        (s3::Mode::Fake, report.lanes.fake.state),
        (s3::Mode::Detect, report.lanes.detect.state),
        (s3::Mode::BuildProbe, report.lanes.build_probe.state),
        (s3::Mode::SignPlan, report.lanes.sign_plan.state),
    ] {
        assert_eq!(state, s3::LaneState::Pending, "{mode:?}");
    }

    // Both artifacts are present and the failure kind serializes camelCase.
    assert!(json.contains("\"nixMissing\""));
    assert!(json.contains("\"preflight\""));
    let md = fs::read_to_string(scratch.path().join("summary.md")).unwrap();
    assert!(md.contains("# S3 macOS spike report"));
    assert!(md.contains("| mode | preflight |"));
    assert!(md.contains("incomplete"));
}

#[test]
fn preflight_missing_nix_bin_artifacts_leak_no_secrets_or_paths() {
    let scratch = Scratch::new("preflight-leak");
    let nix_bin = missing_nix_bin(&scratch);
    let nix_bin_s = nix_bin.to_str().unwrap();
    let (out, _) = run_bin(&[
        "preflight",
        "--nix-bin",
        nix_bin_s,
        "--out-dir",
        scratch.path().to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(69));
    // argv / program path / store paths / hashes never reach stderr or stdout.
    let stderr = String::from_utf8(out.stderr).unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    for stream in [&stderr, &stdout] {
        assert!(!stream.contains(nix_bin_s));
        assert!(!stream.contains("--nix-bin"));
        assert!(!stream.contains("/nix/store/"));
        assert!(!stream.contains("--version"));
    }

    let json = fs::read_to_string(scratch.path().join("report.json")).unwrap();
    let md = fs::read_to_string(scratch.path().join("summary.md")).unwrap();
    for s in [&json, &md] {
        // No store path, argv, offered program path, or raw child output.
        assert!(!s.contains(nix_bin_s), "program path leaked: {s:?}");
        assert!(!s.contains("/nix/store/"), "store path leaked: {s:?}");
        assert!(!s.contains("--version"), "argv leaked: {s:?}");
        assert!(!s.contains("derivation show"), "argv leaked: {s:?}");
        assert!(!s.contains("path-info"), "argv leaked: {s:?}");
    }
}

#[test]
fn preflight_missing_nix_bin_report_is_deterministic_across_runs() {
    let a = Scratch::new("preflight-det-a");
    let b = Scratch::new("preflight-det-b");
    let a_bin = missing_nix_bin(&a);
    let b_bin = missing_nix_bin(&b);
    for (scratch, bin) in [(&a, a_bin.to_str().unwrap()), (&b, b_bin.to_str().unwrap())] {
        let (out, _) = run_bin(&[
            "preflight",
            "--nix-bin",
            bin,
            "--out-dir",
            scratch.path().to_str().unwrap(),
        ]);
        assert_eq!(out.status.code(), Some(69));
    }
    let ja = fs::read_to_string(a.path().join("report.json")).unwrap();
    let jb = fs::read_to_string(b.path().join("report.json")).unwrap();
    assert_eq!(ja, jb);
    let ma = fs::read_to_string(a.path().join("summary.md")).unwrap();
    let mb = fs::read_to_string(b.path().join("summary.md")).unwrap();
    assert_eq!(ma, mb);
}

#[test]
fn preflight_default_out_dir_writes_to_cwd() {
    // --out-dir defaults to dot: run in a scratch cwd and confirm both
    // artifacts land there. Exit 69 (still NixMissing), no child starts.
    let scratch = Scratch::new("preflight-cwd");
    let nix_bin = missing_nix_bin(&scratch);
    let out = Command::new(s3_probe())
        .args(["preflight", "--nix-bin", nix_bin.to_str().unwrap()])
        .current_dir(scratch.path())
        .output()
        .expect("spawn s3-probe");
    assert_eq!(out.status.code(), Some(69));
    assert!(scratch.path().join("report.json").exists());
    assert!(scratch.path().join("summary.md").exists());
}

// ---------------------------------------------------------------------------
// Real command executor via the hidden fixture child
// ---------------------------------------------------------------------------

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}

/// Build a CommandSpec that re-invokes the `s3-probe` binary as its fixture
/// child with `child_args`, under given caps/timeout.
fn fixture_spec(child_args: &[&str], stdout_cap: u64, timeout_ms: u64) -> CommandSpec {
    let mut args: Vec<OsString> = Vec::with_capacity(child_args.len() + 1);
    args.push(OsString::from(FIXTURE_CHILD_MARKER));
    for a in child_args {
        args.push(OsString::from(*a));
    }
    CommandSpec::new(
        s3_probe(),
        args,
        nz(stdout_cap),
        nz(64 * 1024),
        Duration::from_millis(timeout_ms),
    )
    .expect("fixture spec valid")
}

#[test]
fn executor_success_captures_stdout_and_zero_exit() {
    let spec = fixture_spec(&["--stdout", "5", "--exit", "0"], 1024, 5_000);
    let outcome = run(&spec).expect("run ok");
    assert!(outcome.is_success());
    assert_eq!(outcome.status, ProbeStatus::Exited(0));
    assert_eq!(outcome.stdout, b"AAAAA");
    assert_eq!(outcome.stdout_total_bytes, 5);
}

#[test]
fn executor_nonzero_exit_is_outcome_not_error() {
    let spec = fixture_spec(&["--exit", "3"], 1024, 5_000);
    let outcome = run(&spec).expect("nonzero is Ok outcome");
    assert!(!outcome.is_success());
    assert_eq!(outcome.status, ProbeStatus::Exited(3));
}

#[test]
fn executor_cap_overflow_is_structured_error() {
    // Emit far more than the cap → CapOverflow (fail closed), no panic.
    let spec = fixture_spec(&["--stdout", "100000"], 32, 5_000);
    let err = run(&spec).unwrap_err();
    assert!(matches!(err, CommandError::CapOverflow { .. }));
    // Display is bounded and names no child output.
    let s = err.to_string();
    assert!(s.contains("stdout"));
    assert!(s.contains("exceeded cap"));
    assert!(s.len() < 256);
}

#[test]
fn executor_timeout_kills_process_group() {
    // Sleep 30s but cap the run at 300ms → Timeout{killed:true}.
    let spec = fixture_spec(&["--sleep-ms", "30000"], 1024, 300);
    let start = std::time::Instant::now();
    let err = run(&spec).unwrap_err();
    let elapsed = start.elapsed();
    assert!(
        matches!(err, CommandError::Timeout { killed: true }),
        "{err:?}"
    );
    // The group kill reaps promptly: well under the 30s sleep.
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
}

#[test]
fn executor_environment_is_fail_closed_lang_c_lc_all_c_only() {
    let spec = fixture_spec(&["--dump-env"], 4096, 5_000);
    let outcome = run(&spec).expect("run ok");
    let env = String::from_utf8(outcome.stdout).unwrap();
    // The ONLY inherited variables are LANG=C and LC_ALL=C (sorted).
    let mut lines: Vec<&str> = env.lines().collect();
    lines.sort();
    assert_eq!(lines, vec!["LANG=C", "LC_ALL=C"]);
}

#[test]
fn executor_stderr_is_captured_and_counted() {
    let spec = fixture_spec(&["--stderr", "7"], 1024, 5_000);
    let outcome = run(&spec).expect("run ok");
    assert_eq!(outcome.stderr_total_bytes, 7);
}
