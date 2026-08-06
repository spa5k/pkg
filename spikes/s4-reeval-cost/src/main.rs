//! Spike S4 (PR-6 / DR-004) — the `s4-runner` binary entry point.
//!
//! This binary is the thin process shell over the library. It does only four
//! things, all with bounded, deterministic behavior and `#![forbid(unsafe_code)]`:
//!
//! 1. **Hidden fake-child dispatch.** When the first real argument is
//!    [`fake::MARKER`] (`s4-fake-child`), the binary has been re-invoked as its
//!    OWN deterministic fixture child by [`runner::run_fake`]. In that case the
//!    remaining child arguments are parsed with [`fake::parse`] (never
//!    panicking, even on non-UTF-8 input), the resulting [`fake::ChildPlan`] is
//!    executed against the real stdout/stderr handles with [`fake::execute`],
//!    the EXACT stdout/stderr bytes are written, and the process exits with the
//!    EXACT exit code the plan selected. Malformed hidden-child input exits 64;
//!    any execution failure exits 70. In both cases a bounded deterministic
//!    message is written to stderr first.
//!
//! 2. **Normal CLI dispatch.** Otherwise the existing [`cli::parse`] parser
//!    resolves the action. `--help`/`-h` prints the usage banner to stdout and
//!    exits 0. Any [`cli::CliError`] prints a bounded deterministic message to
//!    stderr and exits 64.
//!
//! 3. **Fake mode.** `fake` resolves the current executable via
//!    [`std::env::current_exe`] (the runner re-invokes this EXACT binary, by
//!    absolute path, as its own fixture child — NO shell, NO `PATH` lookup, NO
//!    network, NO Nix), calls the public [`runner::run_fake`], then writes
//!    `report.json` (pretty JSON with exactly one trailing newline) and
//!    `summary.md` (rendered by [`report::render_markdown`]) under the
//!    requested output directory. Each artifact is written to a sibling temp
//!    file then atomically renamed into place, so a crash mid-write never leaves
//!    a partial artifact. On success exactly one fixed concise line is printed
//!    to stdout. Any failure uses safe deterministic error handling: a bounded
//!    message to stderr and a nonzero exit (70).
//!
//! 4. **Real mode.** `real --nix-bin ABSOLUTE_PATH` runs via
//!    [`s4::real::run_real`], whose hardened command executor launches the
//!    FIXED host `/usr/bin/time` wrapper with the EXACT caller-provided
//!    absolute `nix_bin` path as an argument (NO shell, NO `PATH` lookup — the
//!    OS child is the time wrapper, NOT the `nix` binary directly), which
//!    returns a validated [`s4::report::Report`] (Complete on full success,
//!    Incomplete when Nix was missing / wrong version, or any scenario or
//!    command failed). Both `report.json` (pretty JSON with exactly one
//!    trailing newline) and `summary.md` (rendered by
//!    [`s4::report::render_markdown`]) are ALWAYS written — including for an
//!    Incomplete report — through the SAME shared atomic writer as Fake mode.
//!    A [`s4::real::RealRunError`] (private-home / preparation / fallback
//!    failure) prints ONE bounded deterministic line to stderr and exits
//!    [`EXIT_SOFTWARE`](EXIT_SOFTWARE) (70); this invocation does NOT call the
//!    artifact writer and does NOT create or replace any artifacts, but
//!    pre-existing output-directory contents may remain. A Complete report
//!    prints the fixed success line to
//!    stdout and exits 0. An Incomplete report prints ONE fixed concise
//!    caller-data-free line to stderr noting the run was incomplete and both
//!    artifacts were written, then exits [`EXIT_UNAVAILABLE`](EXIT_UNAVAILABLE)
//!    (69); no dynamic Nix output is ever printed.
//!
//! The binary never calls a shell, performs a `PATH` lookup, touches the
//! network, or invokes Nix in fake mode. `#![forbid(unsafe_code)]` is inherited
//! by declaration here (the binary is its own crate root).

#![forbid(unsafe_code)]

use pkg_spike_s4_reeval_cost as s4;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

/// Exit code for usage / malformed input errors (`EX_USAGE`).
const EXIT_USAGE: u8 = 64;
/// Exit code for internal software / execution errors (`EX_SOFTWARE`).
const EXIT_SOFTWARE: u8 = 70;
/// Exit code for a Real run that returned an Incomplete diagnostic report
/// (`EX_UNAVAILABLE`): a recoverable command / scenario failure was folded
/// into a validated [`s4::report::Report`] whose completeness is
/// [`s4::report::Completeness::Incomplete`], both artifacts were written, but
/// the recorded data is NOT a complete measurement.
const EXIT_UNAVAILABLE: u8 = 69;

fn main() -> ExitCode {
    // `args_os()` begins with `argv[0]` (the program path); every meaningful
    // argument follows it. The program path is intentionally dropped and never
    // trusted or echoed.
    let mut args = std::env::args_os();
    let _program = args.next();
    let rest: Vec<OsString> = args.collect();

    // Hidden fake-child dispatch: the binary was re-invoked as its own
    // deterministic fixture child (`<self> <MARKER> --stdout-bytes N ...`).
    // The marker is pure ASCII, so this byte-exact `OsStr` comparison never
    // panics and matches before any UTF-8 decoding is attempted.
    let is_fake_child = rest
        .first()
        .map(|token| token.as_os_str() == OsStr::new(s4::fake::MARKER))
        .unwrap_or(false);
    if is_fake_child {
        return run_fake_child(&rest);
    }

    // Normal CLI path.
    match s4::cli::parse(rest) {
        Ok(s4::cli::Action::Help) => {
            // Help prints the usage banner to stdout and exits 0.
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(s4::cli::USAGE.as_bytes());
            let _ = lock.flush();
            ExitCode::SUCCESS
        }
        Ok(s4::cli::Action::Run(run_args)) => match run_args.mode {
            s4::cli::RunMode::Fake => run_fake_mode(&run_args.out_dir),
            s4::cli::RunMode::Real { nix_bin } => run_real_mode(&nix_bin, &run_args.out_dir),
        },
        Err(err) => {
            // `CliError` Display is bounded by construction (`MAX_TOKEN_CHARS`).
            fail_stderr(&err.to_string());
            ExitCode::from(EXIT_USAGE)
        }
    }
}

// ---------------------------------------------------------------------------
// Hidden fake-child path
// ---------------------------------------------------------------------------

/// Drive the hidden fake-child protocol.
///
/// The remaining arguments (beginning with [`s4::fake::MARKER`]) are converted
/// to `String`s — a non-UTF-8 token is treated as malformed hidden-child input
/// and exits [`EXIT_USAGE`](EXIT_USAGE) without panicking — then parsed with
/// [`s4::fake::parse`]. A parse failure is likewise malformed input and exits
/// 64. On success [`s4::fake::execute`] writes the EXACT stdout/stderr bytes
/// through the real handles (it flushes both streams on success) and returns the
/// selected exit code, which the process exits with verbatim. An
/// [`s4::fake::ExecError`] flushes any partial output, writes a bounded
/// deterministic message to stderr, and exits
/// [`EXIT_SOFTWARE`](EXIT_SOFTWARE).
fn run_fake_child(rest: &[OsString]) -> ExitCode {
    // `fake::parse` works on `&[String]`; convert each token. A non-UTF-8 token
    // is malformed hidden-child input → exit 64 (never a panic).
    let string_args: Vec<String> = match rest
        .iter()
        .map(|token| token.clone().into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(strings) => strings,
        Err(_non_utf8) => {
            fail_stderr("fake child: non-UTF-8 argument");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let plan = match s4::fake::parse(&string_args) {
        Ok(plan) => plan,
        Err(err) => {
            // `ParseError` Display is bounded by construction (`snip`).
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_USAGE);
        }
    };

    // Execute against the real stdout/stderr handles. `execute` writes the
    // deterministic bytes directly, flushes both streams on success, and returns
    // the plan's exit code (validated to the portable 0..=125 range, so it fits
    // in `u8`). On failure a structured `ExecError` (bounded Display) is
    // reported and we exit EX_SOFTWARE.
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    match s4::fake::execute(&plan, &mut stdout, &mut stderr) {
        Ok(code) => {
            // `execute` already flushed on success; flush once more in case the
            // stdout handle was block-buffered before any newline was emitted.
            let _ = stdout.flush();
            let _ = stderr.flush();
            ExitCode::from(code as u8)
        }
        Err(err) => {
            // Flush whatever partial output survived, then report the bounded
            // error on stderr and exit EX_SOFTWARE.
            let _ = stdout.flush();
            let _ = stderr.flush();
            fail_stderr(&err.to_string());
            ExitCode::from(EXIT_SOFTWARE)
        }
    }
}

// ---------------------------------------------------------------------------
// Fake mode
// ---------------------------------------------------------------------------

/// Run the full Fake pipeline and write both report artifacts.
///
/// Resolves the current executable with [`std::env::current_exe`] (the runner
/// re-invokes this EXACT binary, by absolute path, as its own fixture child),
/// calls [`s4::runner::run_fake`], then writes `report.json` (pretty JSON +
/// exactly one trailing newline) and `summary.md` (via
/// [`s4::report::render_markdown`]) under `out_dir`. Each artifact is written to
/// a sibling temp file then atomically renamed. On success exactly one fixed
/// concise line is printed to stdout. Any failure uses safe deterministic error
/// handling: a bounded message to stderr and exit
/// [`EXIT_SOFTWARE`](EXIT_SOFTWARE).
fn run_fake_mode(out_dir: &Path) -> ExitCode {
    // Resolve the current executable: NO shell, NO PATH lookup, NO network, NO
    // Nix. The runner spawns this exact binary as its fixture child.
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            fail_stderr(&io_msg("could not resolve current executable", &err));
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    let report = match s4::runner::run_fake(&executable) {
        Ok(report) => report,
        Err(err) => {
            // `RunnerError` Display is bounded by design.
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    // Write both report artifacts via the shared helper (idempotent directory
    // creation + pretty JSON with exactly one trailing newline +
    // render_markdown + atomic sibling-temp rename). On failure a bounded
    // deterministic message is reported and we exit EX_SOFTWARE.
    if let Err(message) = write_report_artifacts(&report, out_dir) {
        fail_stderr(&message);
        return ExitCode::from(EXIT_SOFTWARE);
    }

    print_success_line();
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Real mode
// ---------------------------------------------------------------------------

/// Run the full Real pipeline and write both report artifacts.
///
/// Runs via [`s4::real::run_real`], whose hardened command executor launches
/// the FIXED host `/usr/bin/time` wrapper with the EXACT caller-provided
/// absolute `nix_bin` path as an argument (NO shell, NO `PATH` lookup — the OS
/// child is the time wrapper, NOT the `nix` binary directly), which returns a
/// validated [`s4::report::Report`] — Complete on full success, Incomplete when
/// Nix was missing / wrong version, or any scenario or command failed. Both
/// `report.json` and `summary.md` are ALWAYS written (including for an
/// Incomplete report) through the SAME shared atomic writer as Fake mode. A
/// [`s4::real::RealRunError`] (private-home / preparation / fallback failure)
/// prints ONE bounded deterministic line to stderr and exits
/// [`EXIT_SOFTWARE`](EXIT_SOFTWARE) (70); this invocation does NOT call the
/// artifact writer and does NOT create or replace any artifacts, but
/// pre-existing output-directory contents may remain. A Complete report prints
/// the fixed success line to stdout and
/// exits 0. An Incomplete report prints ONE fixed concise caller-data-free
/// line to stderr noting the run was incomplete and both artifacts were
/// written, then exits [`EXIT_UNAVAILABLE`](EXIT_UNAVAILABLE) (69). No dynamic
/// Nix output is ever printed.
fn run_real_mode(nix_bin: &Path, out_dir: &Path) -> ExitCode {
    let report = match s4::real::run_real(nix_bin) {
        Ok(report) => report,
        Err(err) => {
            // `RealRunError` Display is a fixed ASCII message with no fields,
            // so this is one bounded deterministic line. This invocation does
            // NOT call the artifact writer and does NOT create or replace any
            // artifacts; pre-existing output-directory contents may remain.
            fail_stderr(&err.to_string());
            return ExitCode::from(EXIT_SOFTWARE);
        }
    };

    // After a Real report is produced, ALWAYS write both artifacts — including
    // when the report is Incomplete. A write failure exits EX_SOFTWARE.
    if let Err(message) = write_report_artifacts(&report, out_dir) {
        fail_stderr(&message);
        return ExitCode::from(EXIT_SOFTWARE);
    }

    if report.completeness == s4::report::Completeness::Complete {
        // A finished Real run: exactly the existing fixed success line, exit 0.
        print_success_line();
        ExitCode::SUCCESS
    } else {
        // Incomplete (a validated Real report is never FakeOnly): one fixed,
        // concise, caller-data-free line. No dynamic Nix output is printed.
        fail_stderr("real run was incomplete; wrote report.json and summary.md");
        ExitCode::from(EXIT_UNAVAILABLE)
    }
}

// ---------------------------------------------------------------------------
// Shared report-artifact writing (Fake + Real)
// ---------------------------------------------------------------------------

/// Serialize a [`s4::report::Report`] and write BOTH artifacts — `report.json`
/// (pretty JSON with exactly one trailing newline) and `summary.md` (rendered
/// by [`s4::report::render_markdown`]) — under `out_dir`, via the shared atomic
/// sibling-temp writer [`write_artifact_atomic`].
///
/// The output directory is created idempotently first. Each artifact is
/// written to a uniquely-named sibling temp file then atomically renamed into
/// place, so a crash mid-write never leaves a partial artifact. Returns a
/// bounded deterministic error string on any failure (a fixed operation label
/// plus, for I/O, the stable [`io::ErrorKind`] token via [`io_msg`] — never the
/// OS-localized message and never a caller-controlled path); the caller maps
/// that to a stderr line + nonzero exit.
fn write_report_artifacts(report: &s4::report::Report, out_dir: &Path) -> Result<(), String> {
    // Ensure the output directory exists (idempotent).
    if let Err(err) = fs::create_dir_all(out_dir) {
        return Err(io_msg("could not create output directory", &err));
    }

    // Pretty JSON with exactly one trailing newline.
    let mut json = match serde_json::to_string_pretty(report) {
        Ok(text) => text,
        Err(err) => return Err(format!("could not serialize report: {err}")),
    };
    json.push('\n');

    let markdown = s4::report::render_markdown(report);

    write_artifact_atomic(out_dir, "report.json", json.as_bytes())?;
    write_artifact_atomic(out_dir, "summary.md", markdown.as_bytes())?;
    Ok(())
}

/// Print the one fixed, deterministic, caller-data-free success line to
/// stdout: `s4-runner: wrote report.json and summary.md\n`. It is a literal
/// with no path or any other caller-controlled content (the output directory is
/// intentionally NOT echoed), so the line is byte-stable regardless of where
/// the artifacts were written. Never panics on write failure (errors are
/// explicitly ignored — there is no useful recovery if stdout is broken).
fn print_success_line() {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "s4-runner: wrote report.json and summary.md");
    let _ = lock.flush();
}

/// Monotonic counter baked into each temp-file name so that two writes — even
/// of the same artifact name within one process — never pick the same temp
/// path. Combined with the PID this makes a temp-path collision effectively
/// impossible; the bounded retry loop below is purely defensive.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Upper bound on how many distinct temp-file names we attempt before giving
/// up. With the PID + 64-bit counter the loop normally exits on the first
/// iteration; this only guards against a pathological flood of pre-existing
/// `.name.<pid>.*.tmp` siblings.
const TEMP_CREATE_RETRIES: u32 = 64;

/// Write `bytes` to `out_dir/name` via a uniquely-named same-directory temp
/// file followed by an atomic rename, so a crash mid-write never leaves a
/// partial artifact.
///
/// Each temp file is a sibling of the target (so the rename stays on one
/// filesystem and is atomic on macOS/Linux) with a unique name of the form
/// `.{name}.{pid}.{counter}.tmp`. It is opened with [`fs::OpenOptions::create_new`] set
/// to `true` (Unix `O_CREAT | O_EXCL`): a pre-existing path at that name — whether a
/// regular file OR a symlink — causes a clean [`io::ErrorKind::AlreadyExists`]
/// failure rather than being followed or truncated. A collision simply advances
/// the counter and retries, up to [`TEMP_CREATE_RETRIES`] times. After a
/// successful open the bytes are written, `fsync`'d, and renamed into place; any
/// failure AFTER the open (write, sync, or rename) removes ONLY the temp file
/// this call created (never any other path) before returning.
///
/// Returns a bounded deterministic error string on failure (operation label +
/// stable [`io::ErrorKind`] token — never the OS-localized message or a path).
fn write_artifact_atomic(out_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let target = out_dir.join(name);
    let pid = std::process::id();

    let mut collisions = 0u32;
    loop {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = out_dir.join(format!(".{name}.{pid}.{counter}.tmp"));

        // `create_new` (O_CREAT|O_EXCL) never follows an existing symlink and
        // never truncates an existing file: it fails with `AlreadyExists`
        // instead. We open `write` only (no `truncate`, no `append`), so
        // nothing existing is ever modified by the open itself.
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // Collision with a pre-existing file or symlink at this unique
                // name (effectively impossible given pid + counter). Nothing
                // was created by this call, so there is nothing to clean up;
                // advance the counter and retry, bounded.
                collisions += 1;
                if collisions >= TEMP_CREATE_RETRIES {
                    return Err(io_msg(&format!("could not create {name}"), &err));
                }
                continue;
            }
            Err(err) => return Err(io_msg(&format!("could not create {name}"), &err)),
        };

        // From here on this call OWNS the freshly created temp file. Any
        // failure below removes ONLY this temp file (never any other path).
        if let Err(err) = file.write_all(bytes) {
            let _ = fs::remove_file(&temp);
            return Err(io_msg(&format!("could not write {name}"), &err));
        }
        if let Err(err) = file.sync_all() {
            let _ = fs::remove_file(&temp);
            return Err(io_msg(&format!("could not sync {name}"), &err));
        }
        // Drop the handle before renaming so the fsync'd contents are fully
        // flushed on every platform and to release any handle state.
        drop(file);

        match fs::rename(&temp, &target) {
            Ok(()) => return Ok(()),
            Err(err) => {
                // Best-effort cleanup of the orphaned temp file THIS call
                // created; its own failure is not reported (the rename failure
                // is the actionable error).
                let _ = fs::remove_file(&temp);
                return Err(io_msg(&format!("could not rename {name}"), &err));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic stderr helper
// ---------------------------------------------------------------------------

/// Write one bounded deterministic line to stderr: `s4-runner: <message>\n`.
/// Never panics on write failure (errors are explicitly ignored — there is no
/// useful recovery if stderr itself is broken). Callers are responsible for
/// passing a bounded `message`; every error type in this crate has a bounded
/// `Display`, and I/O failures are reduced to a stable [`io::ErrorKind`] token
/// via [`io_msg`].
fn fail_stderr(message: &str) {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    let _ = writeln!(lock, "s4-runner: {message}");
    let _ = lock.flush();
}

/// Render a bounded deterministic message for an I/O failure: a fixed
/// `operation` label followed by the stable, non-localized [`io::ErrorKind`]
/// token (never the OS-localized `Display` of the underlying error, and never a
/// caller-controlled path). `operation` should be a fixed literal.
fn io_msg(operation: &str, err: &io::Error) -> String {
    format!("{operation}: {}", err.kind())
}

// ---------------------------------------------------------------------------
// Unit tests (pure `std`; the bin crate is its own test target, so run with
// `cargo test --bin s4-runner`).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// An RAII scratch directory unique across processes and threads, so
    /// concurrent test runs and parallel tests never collide on a path. It is
    /// created under the OS temp dir on construction and removed — recursively,
    /// and ONLY this guard's own unique directory — on `Drop`, so tests never
    /// leave junk behind even on assertion failure.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(label: &str) -> Scratch {
            let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("s4-main-tests-{label}-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).expect("scratch dir");
            Scratch { dir }
        }

        /// The scratch directory's unique path.
        fn path(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Remove ONLY this guard's own unique directory (recursively).
            // Best-effort: a `Drop` impl must never panic, so errors are ignored.
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn has_tmp(dir: &Path) -> bool {
        fs::read_dir(dir)
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
    }

    #[test]
    fn write_atomic_writes_exact_bytes_to_target() {
        let scratch = Scratch::new("happy");
        let dir = scratch.path();
        let body = b"{\n  \"hello\": \"world\"\n}\n";
        write_artifact_atomic(dir, "report.json", body).unwrap();
        assert_eq!(fs::read(dir.join("report.json")).unwrap(), body);
    }

    #[test]
    fn write_atomic_replaces_an_existing_target_wholesale() {
        let scratch = Scratch::new("replace");
        let dir = scratch.path();
        fs::write(dir.join("report.json"), b"OLD CONTENT").unwrap();
        let body = b"new";
        write_artifact_atomic(dir, "report.json", body).unwrap();
        // The target was fully replaced; no partial "OLD CONTENT" remains.
        assert_eq!(fs::read(dir.join("report.json")).unwrap(), body);
        assert!(!has_tmp(dir), "leftover temp file after success");
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_after_success() {
        let scratch = Scratch::new("notemp");
        let dir = scratch.path();
        write_artifact_atomic(dir, "summary.md", b"# hi\n").unwrap();
        assert!(!has_tmp(dir), "leftover temp file after success");
    }

    #[test]
    fn write_atomic_succeeds_across_repeated_writes_of_same_target() {
        // Repeatedly writing the same artifact must not accumulate temp files
        // and must keep the target current (rename replaces in place).
        let scratch = Scratch::new("repeated");
        let dir = scratch.path();
        for i in 0..16u32 {
            write_artifact_atomic(dir, "report.json", format!("v{i}").as_bytes()).unwrap();
        }
        assert_eq!(fs::read(dir.join("report.json")).unwrap(), b"v15");
        assert!(!has_tmp(dir));
    }

    /// Security primitive that `write_artifact_atomic` relies on: opening an
    /// existing symlink with `create_new` must NOT follow the link, truncate it,
    /// or create through it — it must fail cleanly with `AlreadyExists`.
    #[cfg(unix)]
    #[test]
    fn create_new_does_not_follow_or_truncate_a_symlink() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("create-new-symlink");
        let dir = scratch.path();
        let canary = dir.join("canary");
        fs::write(&canary, b"untouched").unwrap();
        let link = dir.join(".victim.tmp");
        symlink(&canary, &link).unwrap();

        let err = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&link)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // The link target was never opened, truncated, or written through.
        assert_eq!(fs::read(&canary).unwrap(), b"untouched");
    }

    /// End-to-end symlink safety: even if many candidate temp names already
    /// exist as symlinks pointing at a canary, `write_artifact_atomic` must
    /// succeed (its retry walks past the planted field) and must NEVER write
    /// through any of those symlinks — the canary stays intact.
    #[cfg(unix)]
    #[test]
    fn write_atomic_never_writes_through_pre_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("symlink-field");
        let dir = scratch.path();
        let canary = dir.join("canary");
        fs::write(&canary, b"sentinel").unwrap();

        // Plant a field of symlinks at plausible candidate temp names. The
        // width MUST stay below `TEMP_CREATE_RETRIES` (64) so the retry loop
        // can walk PAST the planted field instead of exhausting its budget
        // inside it. The global counter is shared across tests, so concurrent
        // drift can only make the call collide fewer times (never more) — it is
        // still symlink-proof either way.
        let pid = std::process::id();
        let start = TEMP_COUNTER.load(Ordering::Relaxed);
        for c in start..start.saturating_add(32) {
            let link = dir.join(format!(".report.json.{pid}.{c}.tmp"));
            let _ = symlink(&canary, &link);
        }

        let body = b"{\n  \"ok\": true\n}\n";
        write_artifact_atomic(dir, "report.json", body).unwrap();

        // The real target has the new bytes...
        assert_eq!(fs::read(dir.join("report.json")).unwrap(), body);
        // ...and the canary was NEVER written through any symlink.
        assert_eq!(fs::read(&canary).unwrap(), b"sentinel");
    }

    /// Failure-path cleanup: when the final rename fails, the temp file created
    /// by THIS call must be removed and nothing else touched. We force a rename
    /// failure by making the target an existing non-empty directory (renaming a
    /// regular file onto a non-empty directory fails with ENOTEMPTY on
    /// macOS/Linux).
    #[test]
    fn write_atomic_removes_its_temp_when_rename_fails() {
        let scratch = Scratch::new("rename-fail");
        let dir = scratch.path();
        let target = dir.join("report.json");
        fs::create_dir(&target).expect("mkdir target");
        fs::write(target.join("sentinel"), b"keep").expect("seed target");

        let err = write_artifact_atomic(dir, "report.json", b"{}").unwrap_err();
        assert!(
            err.starts_with("could not rename report.json"),
            "unexpected error: {err}"
        );

        // No temp file of ours is left behind.
        assert!(!has_tmp(dir), "temp file leaked after rename failure");
        // The non-empty target directory is untouched.
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
    }
}
