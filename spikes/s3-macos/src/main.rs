//! Spike S3 (PR-7) — the `s3-probe` binary entry point.
//!
//! A thin, hardened process shell over the library. It does four things, all
//! with bounded deterministic behavior and `#![forbid(unsafe_code)]`:
//!
//! 1. **Hidden fixture-child dispatch.** When the first real argument is
//!    [`FIXTURE_CHILD_MARKER`], the binary has been re-invoked as its OWN
//!    deterministic fixture child (by an integration test driving the real
//!    [`pkg_spike_s3_macos::command::run`] executor). It parses the remaining
//!    child arguments, writes the requested stdout/stderr bytes, optionally
//!    dumps its (clean) environment, sleeps, and exits with the selected code.
//!    Malformed child input exits 64; an execution failure exits 70.
//!
//! 2. **Normal CLI dispatch.** Otherwise [`pkg_spike_s3_macos::cli::parse`]
//!    resolves the action. `--help`/`-h` (and bare invocation) print the usage
//!    banner to stdout and exit 0. Any [`pkg_spike_s3_macos::cli::CliError`]
//!    prints a bounded deterministic message to stderr and exits 64.
//!
//! 3. **Fake mode.** Runs [`pkg_spike_s3_macos::runner::fake_report`] (no
//!    process, no network, never accesses the keychain), writes `report.json`
//!    + `summary.md` into the output dir, prints one fixed success line, exits 0.
//!
//! 4. **Detect mode.** Runs [`pkg_spike_s3_macos::runner::detect_report`] via
//!    the production [`pkg_spike_s3_macos::detect::BoundedProbeRunner`] (fixed
//!    read-only host probes). A capability-absence Complete report still writes
//!    both artifacts and exits 0. Only an internal probe failure yields an
//!    Incomplete report — both artifacts are STILL written — and exits 69. Any
//!    internal/artifact failure exits 70. Child output/path is NEVER printed.
//!
//! 5. **Preflight mode.** Runs [`pkg_spike_s3_macos::runner::preflight_report`]
//!    via the production [`pkg_spike_s3_macos::command::RealRunner`] against the
//!    caller-supplied absolute Nix binary. The runner spawns the fixed
//!    build-free Nix probe specs directly (no shell, no PATH lookup). A Complete
//!    Preflight report writes both artifacts and exits 0; an Incomplete report
//!    (e.g. the Nix binary is missing → `FailureKind::NixMissing`) STILL writes
//!    both artifacts and exits 69. Any internal/artifact failure exits 70.
//!    Static bounded stderr only; argv/Nix output/store paths/hashes/credentials
//!    are NEVER printed.
//!
//! The binary never calls a shell, performs a `PATH` lookup, touches the
//! network (outside the fixed Preflight cache probes), runs build/sign/notarize
//! execution, or accepts credentials. Detect mode DOES run the read-only
//! `/usr/bin/security find-identity` probe, so when explicitly invoked it reads
//! identity metadata/counts from the default keychain; it never
//! unlocks/signs/notarizes and never writes keychain data. Fake mode never
//! accesses the keychain, and the repo-root validation lanes never run a live
//! Detect. `#![forbid(unsafe_code)]`.

#![forbid(unsafe_code)]

use pkg_spike_s3_macos as s3;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Hidden marker re-invoking this binary as its own deterministic fixture child.
/// Pure ASCII so the byte-exact `OsStr` comparison never panics.
const FIXTURE_CHILD_MARKER: &str = "s3-probe-fixture-child";

/// Exit code for usage / malformed input (`EX_USAGE`).
const EXIT_USAGE: u8 = 64;
/// Exit code for an internal/artifact failure (`EX_SOFTWARE`).
const EXIT_SOFTWARE: u8 = 70;
/// Exit code for a Detect run that produced an Incomplete report
/// (`EX_UNAVAILABLE`): both artifacts were written, but the recorded data is
/// NOT a complete detection.
const EXIT_UNAVAILABLE: u8 = 69;

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let _program = args.next(); // argv[0]; dropped, never trusted or echoed.
    let rest: Vec<OsString> = args.collect();

    // Hidden fixture-child dispatch.
    let is_fixture_child = rest
        .first()
        .map(|t| t.as_os_str() == OsStr::new(FIXTURE_CHILD_MARKER))
        .unwrap_or(false);
    if is_fixture_child {
        return run_fixture_child(&rest);
    }

    match s3::parse_cli(rest) {
        Ok(s3::Action::Help) => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(s3::USAGE.as_bytes());
            let _ = lock.flush();
            ExitCode::SUCCESS
        }
        Ok(s3::Action::Run(run_args)) => match run_args.mode {
            s3::RunMode::Fake => run_fake_mode(&run_args.out_dir),
            s3::RunMode::Detect { nix_bin } => {
                run_detect_mode(nix_bin.as_deref(), &run_args.out_dir)
            }
            s3::RunMode::Preflight { nix_bin } => run_preflight_mode(&nix_bin, &run_args.out_dir),
        },
        Err(err) => {
            fail_stderr(&err.to_string());
            ExitCode::from(EXIT_USAGE)
        }
    }
}

// ---------------------------------------------------------------------------
// Hidden fixture-child path
// ---------------------------------------------------------------------------

/// Drive the hidden fixture-child protocol. Recognized flags (all optional,
/// space-separated, integers): `--exit N` (default 0), `--stdout N` (write N
/// `A` bytes to stdout), `--stderr N` (write N `B` bytes to stderr), `--sleep-ms
/// N` (sleep before exiting), `--dump-env` (write sorted `KEY=VALUE` lines to
/// stdout). Used ONLY by integration tests exercising the real executor.
fn run_fixture_child(rest: &[OsString]) -> ExitCode {
    let mut exit_code: i32 = 0;
    let mut stdout_bytes: u64 = 0;
    let mut stderr_bytes: u64 = 0;
    let mut sleep_ms: u64 = 0;
    let mut dump_env = false;

    // Skip the marker itself.
    let mut iter = rest.iter().skip(1);
    while let Some(tok) = iter.next() {
        let s = match tok.to_str() {
            Some(s) => s,
            None => {
                fail_stderr("fixture child: non-UTF-8 argument");
                return ExitCode::from(EXIT_USAGE);
            }
        };
        let mut take_int = || -> Result<u64, ()> {
            iter.next()
                .and_then(|v| v.to_str())
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or(())
        };
        match s {
            "--exit" => match take_int() {
                Ok(n) => exit_code = n.min(125) as i32,
                Err(()) => {
                    fail_stderr("fixture child: bad --exit");
                    return ExitCode::from(EXIT_USAGE);
                }
            },
            "--stdout" => match take_int() {
                Ok(n) => stdout_bytes = n,
                Err(()) => {
                    fail_stderr("fixture child: bad --stdout");
                    return ExitCode::from(EXIT_USAGE);
                }
            },
            "--stderr" => match take_int() {
                Ok(n) => stderr_bytes = n,
                Err(()) => {
                    fail_stderr("fixture child: bad --stderr");
                    return ExitCode::from(EXIT_USAGE);
                }
            },
            "--sleep-ms" => match take_int() {
                Ok(n) => sleep_ms = n,
                Err(()) => {
                    fail_stderr("fixture child: bad --sleep-ms");
                    return ExitCode::from(EXIT_USAGE);
                }
            },
            "--dump-env" => dump_env = true,
            other => {
                fail_stderr("fixture child: unrecognized argument");
                let _ = other;
                return ExitCode::from(EXIT_USAGE);
            }
        }
    }

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    if dump_env {
        // Sorted KEY=VALUE lines of the (clean) child environment.
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        env.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in env {
            let _ = out.write_all(k.as_bytes());
            let _ = out.write_all(b"=");
            let _ = out.write_all(v.as_bytes());
            let _ = out.write_all(b"\n");
        }
    }
    if write_repeated(&mut out, b'A', stdout_bytes).is_err() {
        return ExitCode::from(EXIT_SOFTWARE);
    }
    if write_repeated(&mut err, b'B', stderr_bytes).is_err() {
        return ExitCode::from(EXIT_SOFTWARE);
    }
    let _ = out.flush();
    let _ = err.flush();

    if sleep_ms > 0 {
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }
    ExitCode::from(exit_code as u8)
}

/// Write `n` copies of `byte` to `w` in chunks (bounded memory).
fn write_repeated<W: Write>(w: &mut W, byte: u8, n: u64) -> io::Result<()> {
    const CHUNK: usize = 8 * 1024;
    let buf = [byte; CHUNK];
    let mut remaining = n;
    while remaining > 0 {
        let take = (CHUNK as u64).min(remaining) as usize;
        w.write_all(&buf[..take])?;
        remaining -= take as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fake mode
// ---------------------------------------------------------------------------

fn run_fake_mode(out_dir: &Path) -> ExitCode {
    let report = match s3::fake_report() {
        Ok(r) => r,
        Err(err) => {
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };
    if let Err(msg) = write_report_artifacts(&report, out_dir) {
        fail_stderr(&msg);
        return ExitCode::from(EXIT_SOFTWARE);
    }
    print_success_line();
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Detect mode
// ---------------------------------------------------------------------------

fn run_detect_mode(nix_bin: Option<&Path>, out_dir: &Path) -> ExitCode {
    let runner = s3::BoundedProbeRunner::new();
    let report = match s3::detect_report(&runner, nix_bin) {
        Ok(r) => r,
        Err(err) => {
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    // Both artifacts are ALWAYS written, including for an Incomplete report.
    if let Err(msg) = write_report_artifacts(&report, out_dir) {
        fail_stderr(&msg);
        return ExitCode::from(EXIT_SOFTWARE);
    }

    // Completeness is read off the lane state (the active lane).
    let complete = report.lanes.detect.state == s3::LaneState::Complete;
    if complete {
        print_success_line();
        ExitCode::SUCCESS
    } else {
        fail_stderr("detect run was incomplete; wrote report.json and summary.md");
        ExitCode::from(EXIT_UNAVAILABLE)
    }
}

// ---------------------------------------------------------------------------
// Preflight mode
// ---------------------------------------------------------------------------

fn run_preflight_mode(nix_bin: &Path, out_dir: &Path) -> ExitCode {
    let runner = s3::RealRunner::new();
    let report = match s3::preflight_report(&runner, nix_bin) {
        Ok(r) => r,
        Err(err) => {
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    // Both artifacts are ALWAYS written, including for an Incomplete report.
    if let Err(msg) = write_report_artifacts(&report, out_dir) {
        fail_stderr(&msg);
        return ExitCode::from(EXIT_SOFTWARE);
    }

    // Completeness is read off the Preflight lane state (the active lane).
    let complete = report.lanes.preflight.state == s3::LaneState::Complete;
    if complete {
        print_success_line();
        ExitCode::SUCCESS
    } else {
        fail_stderr("preflight run was incomplete; wrote report.json and summary.md");
        ExitCode::from(EXIT_UNAVAILABLE)
    }
}

// ---------------------------------------------------------------------------
// Atomic report-artifact writing (Fake + Detect)
// ---------------------------------------------------------------------------

fn write_report_artifacts(report: &s3::Report, out_dir: &Path) -> Result<(), String> {
    if let Err(e) = fs::create_dir_all(out_dir) {
        return Err(io_msg("could not create output directory", &e));
    }
    let mut json = match serde_json::to_string_pretty(report) {
        Ok(t) => t,
        Err(e) => return Err(format!("could not serialize report: {e}")),
    };
    json.push('\n');
    let markdown = s3::render_markdown(report);
    write_artifact_atomic(out_dir, "report.json", json.as_bytes())?;
    write_artifact_atomic(out_dir, "summary.md", markdown.as_bytes())?;
    Ok(())
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_CREATE_RETRIES: u32 = 64;

/// Write `bytes` to `out_dir/name` via a uniquely-named same-directory temp file
/// followed by an atomic rename. The temp file is opened with `create_new`
/// (`O_CREAT|O_EXCL`), so a pre-existing path — regular file OR symlink — fails
/// cleanly with `AlreadyExists` instead of being followed or truncated. After a
/// successful open the bytes are written, `fsync`'d, and renamed into place; any
/// failure after the open removes ONLY this call's temp file.
fn write_artifact_atomic(out_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let target = out_dir.join(name);
    let pid = std::process::id();
    let mut collisions = 0u32;
    loop {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = out_dir.join(format!(".{name}.{pid}.{counter}.tmp"));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                collisions += 1;
                if collisions >= TEMP_CREATE_RETRIES {
                    return Err(io_msg(&format!("could not create {name}"), &e));
                }
                continue;
            }
            Err(e) => return Err(io_msg(&format!("could not create {name}"), &e)),
        };
        if let Err(e) = file.write_all(bytes) {
            let _ = fs::remove_file(&temp);
            return Err(io_msg(&format!("could not write {name}"), &e));
        }
        if let Err(e) = file.sync_all() {
            let _ = fs::remove_file(&temp);
            return Err(io_msg(&format!("could not sync {name}"), &e));
        }
        drop(file);
        match fs::rename(&temp, &target) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&temp);
                return Err(io_msg(&format!("could not rename {name}"), &e));
            }
        }
    }
}

fn print_success_line() {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "s3-probe: wrote report.json and summary.md");
    let _ = lock.flush();
}

fn fail_stderr(message: &str) {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    let _ = writeln!(lock, "s3-probe: {message}");
    let _ = lock.flush();
}

/// Bounded deterministic I/O message: fixed label + stable `io::ErrorKind` token
/// (never the OS-localized message, never a caller-controlled path).
fn io_msg(operation: &str, err: &io::Error) -> String {
    format!("{operation}: {}", err.kind())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch {
        dir: std::path::PathBuf,
    }
    impl Scratch {
        fn new(label: &str) -> Scratch {
            let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("s3-main-tests-{label}-{}-{n}", std::process::id()));
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

    #[test]
    fn write_atomic_writes_exact_bytes() {
        let s = Scratch::new("happy");
        let body = b"{\n  \"ok\": true\n}\n";
        write_artifact_atomic(s.path(), "report.json", body).unwrap();
        assert_eq!(fs::read(s.path().join("report.json")).unwrap(), body);
    }

    #[test]
    fn write_atomic_replaces_existing_and_leaves_no_temp() {
        let s = Scratch::new("replace");
        fs::write(s.path().join("report.json"), b"OLD").unwrap();
        write_artifact_atomic(s.path(), "report.json", b"new").unwrap();
        assert_eq!(fs::read(s.path().join("report.json")).unwrap(), b"new");
        assert!(
            !fs::read_dir(s.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
    }

    #[test]
    fn write_atomic_removes_temp_when_rename_fails() {
        let s = Scratch::new("rename-fail");
        let target = s.path().join("report.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();
        let err = write_artifact_atomic(s.path(), "report.json", b"{}").unwrap_err();
        assert!(err.starts_with("could not rename report.json"));
        assert!(
            !fs::read_dir(s.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
    }

    /// `create_new` must NOT follow or truncate a pre-existing symlink.
    #[cfg(unix)]
    #[test]
    fn create_new_does_not_follow_a_symlink() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new("symlink");
        let canary = s.path().join("canary");
        fs::write(&canary, b"untouched").unwrap();
        let link = s.path().join(".victim.tmp");
        symlink(&canary, &link).unwrap();
        let err = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&link)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&canary).unwrap(), b"untouched");
    }
}
