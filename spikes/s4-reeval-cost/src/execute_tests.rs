//! Integration tests for the [`crate::execute`] slice.
//!
//! This file is included into `crate::execute` as its `tests` submodule
//! (via `#[path = "execute_tests.rs"] mod tests;`), hence `use super::*;` to
//! pull in [`run`] and the re-imported command types
//! ([`CommandSpec`], [`TimeFlavor`], [`UnixStatus`], …).
//!
//! [`run_success_smoke`] re-execs the test binary itself — via
//! [`std::env::current_exe`] restricted to [`fixture_child`] by the test
//! harness's `--exact` selector — so the same deterministic child is driven
//! through the REAL `/usr/bin/time` spawn → drain → reap → RSS-parse pipeline.
//! Under an ordinary `cargo test` run `S4_EXEC_MODE` is unset, so
//! [`fixture_child`] is a no-op; the smoke test arms it with `success`, and the
//! `run_timeout_*` tests arm `timeout-leader` / `timeout-group-leader` to force
//! a wall-clock timeout and exercise whole-group `SIGKILL` cleanup.

use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A deterministic fixture child.
///
/// The `S4_EXEC_MODE` environment variable selects a mode:
///
/// - unset, or any unrecognized value: a harmless no-op (the default during a
///   normal `cargo test` run, where the variable is absent);
/// - `"success"`: writes `b"hello\n"` to stdout and `b"child-stderr\n"` to
///   stderr, flushing both — the payload consumed by [`run_success_smoke`];
/// - `"timeout-leader"`: sleeps 20 s, forcing [`run`] to signal the fresh child
///   group at the wall-clock deadline (used by [`run_timeout_kills_leader`]);
/// - `"timeout-group-leader"`: spawns a grandchild — a re-exec of `current_exe`
///   armed with `"timeout-group-grandchild"` whose stdout/stderr are EXPLICITLY
///   INHERITED so it holds the same capture pipe write-ends as this leader —
///   then sleeps 20 s (used by
///   [`run_timeout_kills_entire_group_and_closes_inherited_pipes`] to prove the
///   whole group, not just the `/usr/bin/time` leader, is signaled: a
///   leader-only kill would strand the grandchild's inherited pipe writers and
///   block the reader joins for ~20 s);
/// - `"timeout-group-grandchild"`: sleeps 20 s; the leaf of the group variant.
/// - `"nonzero"`: calls [`std::process::exit`] with code `7` — a nonzero-exit
///   fixture for callers asserting on the raw exit status, emitting no I/O;
/// - `"env-check"`: asserts [`std::env::var_os`] for both `"PATH"` and
///   `"NIX_PATH"` is `None` (the fail-closed child env must not leak either),
///   then writes `b"env-clean\n"` to stdout and flushes — a violation panics
///   this child test;
/// - `"dual-streams"`: writes a fixed 8192-byte `O` chunk and a fixed
///   8192-byte `E` chunk 128 times apiece, alternating one stdout chunk then
///   one stderr chunk per iteration and flushing both at the end — exactly
///   1 MiB of fixture payload per stream, stressing concurrent drain of both
///   capture pipes;
/// - `"invalid-stdout"`: writes the single raw byte `0xff` to stdout via
///   [`Write::write_all`] and flushes stdout, emitting NO child stderr — a
///   non-UTF-8 stdout fixture for callers asserting on retained raw bytes;
/// - `"invalid-stderr"`: writes the single raw byte `0xff` to stderr via
///   [`Write::write_all`] and flushes stderr, emitting NO child stdout — a
///   non-UTF-8 stderr fixture proving strict stderr UTF-8 rejection (stderr
///   is decoded strict-UTF-8, never retained raw).
///
/// Only [`std::process::Command`]/[`std::process::Stdio`] is used to spawn the
/// grandchild — no shell, no `unsafe`, no `libc`, no external helpers — and the
/// grandchild is left in the SAME process group (no `process_group` call) so the
/// runner's whole-group `SIGKILL` reaches it.
#[test]
fn fixture_child() {
    match std::env::var("S4_EXEC_MODE") {
        // Unset (the default during a normal `cargo test` run, where the
        // variable is absent): do nothing.
        Err(_) => {}
        Ok(mode) => match mode.as_str() {
            // Existing success payload consumed by `run_success_smoke`.
            "success" => {
                let mut out = std::io::stdout();
                let mut err = std::io::stderr();
                out.write_all(b"hello\n").expect("fixture stdout write_all");
                err.write_all(b"child-stderr\n")
                    .expect("fixture stderr write_all");
                out.flush().expect("fixture stdout flush");
                err.flush().expect("fixture stderr flush");
            }
            // Force a timeout: sleep well past any validated budget.
            "timeout-leader" => {
                std::thread::sleep(Duration::from_secs(20));
            }
            // Leaf of the group variant: sleep well past any validated budget.
            "timeout-group-grandchild" => {
                std::thread::sleep(Duration::from_secs(20));
            }
            // Spawn a pipe-inheriting grandchild, then sleep. The grandchild is
            // left in THIS process group (no `process_group` call) so a
            // whole-group `SIGKILL` reaches it; a leader-only kill would strand
            // its inherited pipe writers and block the reader joins.
            "timeout-group-leader" => {
                use std::process::{Command, Stdio};
                let exe = std::env::current_exe().expect("current_exe");
                let mut cmd = Command::new(exe);
                cmd.arg("--exact")
                    .arg("execute::tests::fixture_child")
                    .arg("--nocapture");
                // Fail-closed grandchild env: EXACTLY these three entries,
                // nothing inherited. The leaf mode arms the 20 s sleep.
                cmd.env_clear();
                cmd.env("S4_EXEC_MODE", "timeout-group-grandchild");
                cmd.env("LC_ALL", "C");
                cmd.env("LANG", "C");
                // stdin null; stdout/stderr EXPLICITLY inherited so the
                // grandchild holds the same capture pipes the `/usr/bin/time`
                // wrapper set up.
                cmd.stdin(Stdio::null());
                cmd.stdout(Stdio::inherit());
                cmd.stderr(Stdio::inherit());
                let mut grandchild = cmd.spawn().expect("grandchild spawn");
                std::thread::sleep(Duration::from_secs(20));
                // Non-timeout fallback: if the leader somehow survives its own
                // sleep (it will not under the 250 ms budget used here), reap
                // the grandchild to avoid leaving a zombie.
                let _ = grandchild.wait();
            }
            // Nonzero exit: a fixture for callers asserting on the raw exit
            // status. Exit BEFORE any I/O so no partial payload is emitted.
            "nonzero" => {
                std::process::exit(7);
            }
            // Fail-closed env sanity: the child env must not carry PATH or
            // NIX_PATH. A leak panics this child; a clean env emits the marker
            // line on stdout.
            "env-check" => {
                assert!(
                    std::env::var_os("PATH").is_none(),
                    "PATH leaked into fail-closed child env"
                );
                assert!(
                    std::env::var_os("NIX_PATH").is_none(),
                    "NIX_PATH leaked into fail-closed child env"
                );
                let mut out = std::io::stdout();
                out.write_all(b"env-clean\n")
                    .expect("fixture stdout write_all");
                out.flush().expect("fixture stdout flush");
            }
            // Concurrent drain stress: 128 iterations × (8192-byte O chunk on
            // stdout then 8192-byte E chunk on stderr) = exactly 1 MiB per
            // stream, flushing both once at the end.
            "dual-streams" => {
                let mut out = std::io::stdout();
                let mut err = std::io::stderr();
                let o_chunk = vec![b'O'; 8192];
                let e_chunk = vec![b'E'; 8192];
                for _ in 0..128 {
                    out.write_all(&o_chunk).expect("fixture stdout write_all");
                    err.write_all(&e_chunk).expect("fixture stderr write_all");
                }
                out.flush().expect("fixture stdout flush");
                err.flush().expect("fixture stderr flush");
            }
            // Non-UTF-8 stdout fixture: one raw 0xff byte on stdout, no stderr
            // payload.
            "invalid-stdout" => {
                let mut out = std::io::stdout();
                out.write_all(&[0xff]).expect("fixture stdout write_all");
                out.flush().expect("fixture stdout flush");
            }
            // Non-UTF-8 stderr fixture: one raw 0xff byte on stderr, no stdout
            // payload.
            "invalid-stderr" => {
                let mut err = std::io::stderr();
                err.write_all(&[0xff]).expect("fixture stderr write_all");
                err.flush().expect("fixture stderr flush");
            }
            // Unrecognized value: harmless no-op (fail safe).
            _ => {}
        },
    }
}

/// Select the `/usr/bin/time` RSS dialect for the host: Darwin/BSD `-l` on
/// macOS, GNU `time -v` on Linux.
fn host_flavor() -> TimeFlavor {
    #[cfg(target_os = "macos")]
    {
        TimeFlavor::MacOs
    }
    #[cfg(target_os = "linux")]
    {
        TimeFlavor::Gnu
    }
}

/// Drive a known-good child through [`run`] and assert the captured
/// [`CommandOutcome`].
///
/// The child is this test binary (`current_exe`) restricted to
/// [`fixture_child`] via `--exact`/`--nocapture`, run fail-closed with a minimal
/// but locale-stable environment (`LC_ALL=C`/`LANG=C` so `/usr/bin/time` emits
/// the canonical English RSS line), 1 MiB per-stream retention caps, and a 2 s
/// timeout. The child re-execs the libtest binary, so stdout interleaves the
/// harness's own running/ok text with the fixture payload; a well-behaved run
/// must therefore exit 0 with a stdout whose retained bytes still contain the
/// contiguous `b"hello\n"` fixture window (NOT exact-equal it), and a stderr
/// whose cleaned form still carries the child diagnostic, and the required
/// `max_rss_kib` must have been captured and parsed. On macOS a sandbox that
/// denies `sysctl kern.clockrate` makes the spawn unrunnable, so this test
/// preflights that EXACT condition and skips it (every other failure still
/// surfaces); unsandboxed verification is required to actually exercise the
/// pipeline.
#[test]
fn run_success_smoke() {
    // macOS-only sandbox preflight. `/usr/bin/time` reads `kern.clockrate` via
    // sysctl; under a macOS sandbox that denies sysctl, the spawn emits the
    // EXACT substring "sysctl kern.clockrate: Operation not permitted" to
    // stderr and the fixture cannot run. Skip IFF that substring is present;
    // every other error/parse condition still falls through to [`run`]
    // and surfaces as a real failure. (Linux has no such sysctl gate.)
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_success_smoke: SKIPPED on macOS — sandbox denies \
                 `sysctl kern.clockrate`; unsandboxed verification required."
            );
            return;
        }
    }

    let program: PathBuf = std::env::current_exe().expect("current_exe");

    // Fail-closed environment: the child sees EXACTLY these three entries and
    // nothing inherited from the host. `S4_EXEC_MODE=success` arms the fixture;
    // `LC_ALL=C`/`LANG=C` pin the `/usr/bin/time` metric line to English.
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();
    env.insert(OsString::from("S4_EXEC_MODE"), OsString::from("success"));
    env.insert(OsString::from("LC_ALL"), OsString::from("C"));
    env.insert(OsString::from("LANG"), OsString::from("C"));

    // 1 MiB per-stream retention cap; nonzero by construction.
    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");

    let spec = CommandSpec::new(
        program,
        vec![
            OsString::from("--exact"),
            OsString::from("execute::tests::fixture_child"),
            OsString::from("--nocapture"),
        ],
        env,
        mib,
        mib,
        Duration::from_secs(2),
    )
    .expect("CommandSpec should validate");

    let outcome = run(&spec, host_flavor()).expect("run should succeed");

    // Normal exit with code 0.
    assert_eq!(outcome.status, UnixStatus::Exited(0));

    // stdout is the child's raw bytes, verbatim and never decoded here. The
    // child re-execs the libtest binary, so stdout interleaves the harness's
    // running/ok text with the fixture payload: assert the contiguous
    // `b"hello\n"` window survives, NOT an exact equality on the whole buffer.
    assert!(
        outcome.stdout.windows(6).any(|w| w == b"hello\n"),
        "stdout is missing the contiguous b\"hello\\n\" window: {:?}",
        outcome.stdout,
    );

    // No cap overflow: every stdout byte seen was retained, so the saturating
    // total equals the retained-buffer length.
    assert_eq!(
        outcome.stdout_total_bytes,
        outcome.stdout.len() as u64,
        "stdout_total_bytes should equal retained stdout len (no cap overflow)",
    );

    // At least the 6 bytes of `b"hello\n"` (plus any harness text).
    assert!(
        outcome.stdout_total_bytes >= 6,
        "stdout_total_bytes: expected >= 6, got {}",
        outcome.stdout_total_bytes,
    );

    // The child's own stderr survives the RSS-line excision performed by
    // `parse_max_rss`: the `/usr/bin/time` metric line is stripped, not the
    // child diagnostic.
    assert!(
        outcome.cleaned_stderr.contains("child-stderr"),
        "cleaned_stderr is missing the child diagnostic: {:?}",
        outcome.cleaned_stderr,
    );

    // stderr carries at least the 13 bytes of `child-stderr\n`, plus the
    // `/usr/bin/time` diagnostics that are parsed away.
    assert!(
        outcome.stderr_total_bytes >= 13,
        "stderr_total_bytes: expected >= 13, got {}",
        outcome.stderr_total_bytes,
    );

    // `max_rss_kib` is a REQUIRED plain `u64` (never `Option<u64>`): an outcome
    // exists only after `/usr/bin/time` reported RSS, so consume it directly as
    // a `u64` and confirm a real value was parsed for the process.
    let rss: u64 = outcome.max_rss_kib;
    assert!(
        rss > 0,
        "max_rss_kib should be a real nonzero value, got {rss}"
    );

    // `run` DOES enforce the 2 s `CommandSpec` timeout; this success smoke is
    // not the dedicated timeout/kill test. The broad `wall_ms < 30_000` bound
    // below is only a corruption/sanity check, since a successful outcome is
    // normally observed before the deadline; scheduling/race cleanup is tested
    // separately.
    assert!(
        outcome.wall_ms < 30_000,
        "wall_ms should be < 30_000, got {}",
        outcome.wall_ms,
    );
}

/// Build a fail-closed [`CommandSpec`] that re-execs `current_exe` restricted
/// to [`fixture_child`] under the supplied `S4_EXEC_MODE`, with the same
/// absolute program, argv, and locale-stable environment used by the success
/// smoke: 1 MiB per-stream retention caps and a 250 ms wall budget — short
/// enough that a sleeping fixture (20 s) is guaranteed to time out, and (for
/// the group variant) short enough that a stranded inherited-pipe writer would
/// still be blocked when the budget elapses.
fn timeout_spec(mode: &str) -> CommandSpec {
    let program: PathBuf = std::env::current_exe().expect("current_exe");
    // Fail-closed: the child sees EXACTLY these three entries and nothing
    // inherited from the host. `mode` arms the chosen fixture behavior; the
    // locale pins stabilize the `/usr/bin/time` metric line.
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();
    env.insert(OsString::from("S4_EXEC_MODE"), OsString::from(mode));
    env.insert(OsString::from("LC_ALL"), OsString::from("C"));
    env.insert(OsString::from("LANG"), OsString::from("C"));
    // 1 MiB per-stream retention cap; nonzero by construction.
    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    CommandSpec::new(
        program,
        vec![
            OsString::from("--exact"),
            OsString::from("execute::tests::fixture_child"),
            OsString::from("--nocapture"),
        ],
        env,
        mib,
        mib,
        Duration::from_millis(250),
    )
    .expect("CommandSpec should validate")
}

/// Build a fail-closed [`CommandSpec`] that re-execs `current_exe` restricted
/// to [`fixture_child`] via `--exact`/`--nocapture` under the supplied
/// `S4_EXEC_MODE`, applying `env_clear` semantics: the child sees EXACTLY three
/// entries (`S4_EXEC_MODE`, `LC_ALL=C`, `LANG=C`) and nothing inherited from the
/// host (the locale pins stabilize the `/usr/bin/time` metric line to English),
/// with caller-supplied nonzero per-stream caps and a finite wall budget. Shared
/// by the `nonzero`/`env-check`/`dual-streams` normal-fixture tests below.
fn fixture_spec(
    mode: &str,
    timeout: Duration,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
) -> CommandSpec {
    let program: PathBuf = std::env::current_exe().expect("current_exe");
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();
    env.insert(OsString::from("S4_EXEC_MODE"), OsString::from(mode));
    env.insert(OsString::from("LC_ALL"), OsString::from("C"));
    env.insert(OsString::from("LANG"), OsString::from("C"));
    CommandSpec::new(
        program,
        vec![
            OsString::from("--exact"),
            OsString::from("execute::tests::fixture_child"),
            OsString::from("--nocapture"),
        ],
        env,
        stdout_cap,
        stderr_cap,
        timeout,
    )
    .expect("CommandSpec should validate")
}

/// Force a wall-clock timeout against a sleeping leader and assert that [`run`]
/// (a) reports EXACTLY [`CommandError::Timeout`] { `killed: true` } and (b)
/// returns well under the fixture's 20 s sleep.
///
/// The child is this test binary restricted to [`fixture_child`] via
/// `--exact`/`--nocapture`, run fail-closed with a locale-stable environment,
/// 1 MiB caps, and a 250 ms timeout; under `S4_EXEC_MODE=timeout-leader` the
/// fixture sleeps 20 s, so [`run`] polls to the deadline, signals the ENTIRE
/// fresh child group with `SIGKILL`, reaps the `/usr/bin/time` leader, and
/// surfaces `Timeout { killed: true }`. The OUTER elapsed (measured around
/// [`run`], so it includes the cleanup + reader joins) must be under 10 s — a
/// coarse bound, NOT an exact-millisecond assertion, that nonetheless catches a
/// regression: if the leader were reaped but the group were NOT signaled, a
/// surviving fixture child would keep the capture-pipe reader joins blocked
/// until its ~20 s sleep elapsed. On macOS a sandbox that denies
/// `sysctl kern.clockrate` makes the spawn unrunnable; this test preflights
/// that EXACT condition and skips it (every other failure still surfaces) —
/// unsandboxed verification is required to actually exercise the kill path.
#[test]
fn run_timeout_kills_leader() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_timeout_kills_leader: SKIPPED on macOS — sandbox denies \
                 `sysctl kern.clockrate`; unsandboxed verification required."
            );
            return;
        }
    }

    let spec = timeout_spec("timeout-leader");
    let start = Instant::now();
    let err = run(&spec, host_flavor()).expect_err("run should time out, not succeed");
    let elapsed = start.elapsed();

    assert_eq!(err, CommandError::Timeout { killed: true });

    // Coarse bound (NOT exact-ms): the fixture sleeps 20 s, so returning under
    // 10 s confirms the group was signaled promptly rather than the leader
    // merely reaped while a survivor blocked the reader joins.
    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s (fixture sleeps 20 s), got {elapsed:?}",
    );
}

/// Force a wall-clock timeout against a leader that has spawned a
/// pipe-inheriting grandchild, and assert [`run`] returns
/// [`CommandError::Timeout`] { `killed: true` } under 10 s — proving the ENTIRE
/// fresh process group was signaled, not just the `/usr/bin/time` leader.
///
/// Under `S4_EXEC_MODE=timeout-group-leader` the fixture spawns a grandchild
/// (re-exec of `current_exe` in `timeout-group-grandchild`) whose stdout/stderr
/// are EXPLICITLY INHERITED, so the grandchild holds the SAME capture pipe
/// write-ends as the `/usr/bin/time` wrapper and the leader. Both then sleep
/// 20 s. If [`run`] killed ONLY the `/usr/bin/time` leader, the surviving
/// fixture child and grandchild would keep those inherited pipe writers open
/// and the reader joins (performed inside [`run`] before it returns the
/// timeout) would block until the ~20 s sleeps elapsed. Returning under 10 s
/// therefore proves the whole fresh process group was signaled with `SIGKILL` —
/// the inherited pipe writers were closed promptly, so the readers reached EOF
/// without waiting for the sleeps. (As above, the bound is coarse, NOT
/// exact-ms.) On macOS a sandbox that denies `sysctl kern.clockrate` makes the
/// spawn unrunnable; this test preflights that EXACT condition and skips it —
/// unsandboxed verification is required to actually exercise the group kill.
#[test]
fn run_timeout_kills_entire_group_and_closes_inherited_pipes() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_timeout_kills_entire_group_and_closes_inherited_pipes: \
                 SKIPPED on macOS — sandbox denies `sysctl kern.clockrate`; \
                 unsandboxed verification required."
            );
            return;
        }
    }

    let spec = timeout_spec("timeout-group-leader");
    let start = Instant::now();
    let err = run(&spec, host_flavor()).expect_err("run should time out, not succeed");
    let elapsed = start.elapsed();

    assert_eq!(err, CommandError::Timeout { killed: true });

    // Coarse bound (NOT exact-ms): a leader-only kill would leave the
    // inherited-pipe writers open and block the reader joins for ~20 s
    // (fixture sleeps 20 s); returning under 10 s proves the whole group was
    // signaled.
    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s; a leader-only kill would block the \
         inherited-pipe readers for ~20 s, got {elapsed:?}",
    );
}

/// Run the `nonzero` fixture child (which calls [`std::process::exit`] with code
/// `7` BEFORE any I/O) and assert that [`run`] returns a SUCCESSFUL
/// [`CommandOutcome`] whose status is [`UnixStatus::Exited`] (7) — a nonzero
/// exit is the child's honest outcome, NOT an execution error ([`run`] only
/// fails on structural/capture/parse failures) — and that the REQUIRED
/// `max_rss_kib` is a real positive value. On macOS a sandbox that denies
/// `sysctl kern.clockrate` makes the spawn unrunnable; this test preflights that
/// EXACT condition and skips it (every other failure still surfaces) —
/// unsandboxed verification is required to actually exercise the path.
#[test]
fn run_nonzero_exit_is_outcome_not_error() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_nonzero_exit_is_outcome_not_error: SKIPPED on macOS — \
                 sandbox denies `sysctl kern.clockrate`; unsandboxed \
                 verification required."
            );
            return;
        }
    }

    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    let spec = fixture_spec("nonzero", Duration::from_secs(2), mib, mib);

    let start = Instant::now();
    let outcome = run(&spec, host_flavor())
        .expect("nonzero exit is a successful CommandOutcome, not a CommandError");
    let elapsed = start.elapsed();

    // The child's nonzero exit is its honest outcome: NOT converted into an
    // execution error.
    assert_eq!(outcome.status, UnixStatus::Exited(7));

    // `max_rss_kib` is REQUIRED and must reflect a real process.
    let rss: u64 = outcome.max_rss_kib;
    assert!(rss > 0, "max_rss_kib should be positive, got {rss}");

    // Coarse outer bound: a no-I/O 7-exit returns well under the fixture's own
    // lifetime budget.
    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s, got {elapsed:?}",
    );
}

/// Run the `env-check` fixture child and assert that the fail-closed
/// environment carried NEITHER `PATH` nor `NIX_PATH`: the child asserts both are
/// [`None`] (panicking this child test on a leak, which would surface as a
/// nonzero exit) and, on a clean env, writes `b"env-clean\n"` to stdout. A
/// successful exit 0 whose retained stdout still contains the contiguous
/// `env-clean\n` window therefore PROVES both vars were absent. On macOS a
/// sandbox that denies `sysctl kern.clockrate` makes the spawn unrunnable; this
/// test preflights that EXACT condition and skips it — unsandboxed verification
/// is required.
#[test]
fn run_env_check_clean_environment() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_env_check_clean_environment: SKIPPED on macOS — sandbox \
                 denies `sysctl kern.clockrate`; unsandboxed verification \
                 required."
            );
            return;
        }
    }

    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    let spec = fixture_spec("env-check", Duration::from_secs(2), mib, mib);

    let start = Instant::now();
    let outcome =
        run(&spec, host_flavor()).expect("env-check child should run to a normal completion");
    let elapsed = start.elapsed();

    // The fail-closed env kept PATH and NIX_PATH absent, so the child reached
    // its marker write and exited 0.
    assert_eq!(outcome.status, UnixStatus::Exited(0));

    // Retained stdout contains the contiguous `env-clean\n` marker window. (The
    // child re-execs the libtest binary, so stdout interleaves the harness's
    // running/ok text with the marker — assert the window survives, NOT an
    // exact equality on the whole buffer.)
    assert!(
        outcome
            .stdout
            .windows(b"env-clean\n".len())
            .any(|w| w == b"env-clean\n"),
        "stdout is missing the contiguous b\"env-clean\\n\" window: {:?}",
        outcome.stdout,
    );

    // No cap overflow: every stdout byte seen was retained, so the saturating
    // total equals the retained-buffer length.
    assert_eq!(
        outcome.stdout_total_bytes,
        outcome.stdout.len() as u64,
        "stdout_total_bytes should equal retained stdout len (no cap overflow)",
    );

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s, got {elapsed:?}",
    );
}

/// Run the `dual-streams` fixture child under a 5 s budget with 2 MiB per-stream
/// caps and assert concurrent drain of BOTH capture pipes retains the FULL 1 MiB
/// `O`/`E` payload per stream WITHOUT cap overflow. The child writes 128 ×
/// 8192-byte chunks alternating one stdout `O` chunk then one stderr `E` chunk
/// per iteration, flushing both once at the end — exactly 1 MiB per stream.
/// Because the child re-execs the libtest binary, stdout is the harness's
/// running/ok text PLUS the `O` payload and stderr is the `E` payload PLUS the
/// `/usr/bin/time` metric line (the latter excised into `cleaned_stderr`); we
/// therefore account for the wrapper and libtest bytes rather than asserting
/// exact total equality, and confirm both totals stay within their caps (so each
/// retained length equals its total — no discarded bytes). On macOS a sandbox
/// that denies `sysctl kern.clockrate` makes the spawn unrunnable; this test
/// preflights that EXACT condition and skips it — unsandboxed verification is
/// required.
#[test]
fn run_dual_streams_concurrent_drain() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_dual_streams_concurrent_drain: SKIPPED on macOS — sandbox \
                 denies `sysctl kern.clockrate`; unsandboxed verification \
                 required."
            );
            return;
        }
    }

    let two_mib = NonZeroU64::new(2 * 1024 * 1024).expect("2 MiB is nonzero");
    // Exactly 1 MiB of fixture bytes per stream (128 × 8192).
    let payload: usize = 128 * 8192;
    let spec = fixture_spec("dual-streams", Duration::from_secs(5), two_mib, two_mib);

    let start = Instant::now();
    let outcome =
        run(&spec, host_flavor()).expect("dual-streams child should run to a normal completion");
    let elapsed = start.elapsed();

    assert_eq!(outcome.status, UnixStatus::Exited(0));

    // stdout = harness text + the full 1 MiB `O` payload; the 2 MiB cap is
    // never approached, so the contiguous payload survives intact and contains
    // a run of `O` bytes.
    assert!(
        outcome.stdout.len() >= payload,
        "retained stdout should hold the full {payload}-byte O payload, got {}",
        outcome.stdout.len(),
    );
    assert!(
        outcome
            .stdout
            .windows(8192)
            .any(|w| w.iter().all(|&b| b == b'O')),
        "stdout should contain a contiguous O run",
    );

    // `cleaned_stderr` (the `/usr/bin/time` metric line excised) holds the full
    // 1 MiB `E` payload and contains a run of `E` bytes.
    let cleaned_bytes = outcome.cleaned_stderr.len();
    assert!(
        cleaned_bytes >= payload,
        "cleaned stderr should hold the full {payload}-byte E payload, got \
         {cleaned_bytes}",
    );
    assert!(
        outcome
            .cleaned_stderr
            .as_bytes()
            .windows(8192)
            .any(|w| w.iter().all(|&b| b == b'E')),
        "cleaned stderr should contain a contiguous E run",
    );

    // The `/usr/bin/time` metric line WAS on the pipe (counted in the
    // saturating total) and was parsed into the REQUIRED `max_rss_kib`, proving
    // the wrapper metric round-tripped even though it was excised from
    // `cleaned_stderr`.
    let rss: u64 = outcome.max_rss_kib;
    assert!(rss > 0, "max_rss_kib should be positive, got {rss}");

    // No cap overflow on EITHER stream, so each retained length equals its
    // saturating total (no discarded bytes). stdout retained == raw retained
    // (no excision): assert the equality directly. stderr total counts the raw
    // pipe (including the excised metric line), so it is >= the cleaned length;
    // being within the cap proves retained == total there too.
    assert_eq!(
        outcome.stdout_total_bytes,
        outcome.stdout.len() as u64,
        "stdout_total_bytes should equal retained stdout len (no cap overflow)",
    );
    assert!(
        outcome.stdout_total_bytes <= two_mib.get(),
        "stdout_total_bytes should be within the 2 MiB cap, got {}",
        outcome.stdout_total_bytes,
    );
    assert!(
        outcome.stderr_total_bytes <= two_mib.get(),
        "stderr_total_bytes should be within the 2 MiB cap, got {}",
        outcome.stderr_total_bytes,
    );
    assert!(
        outcome.stderr_total_bytes >= cleaned_bytes as u64,
        "stderr_total_bytes ({}) should be >= cleaned stderr len ({}) — the \
         excised `/usr/bin/time` metric line is counted in the total",
        outcome.stderr_total_bytes,
        cleaned_bytes,
    );

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s (5 s budget), got {elapsed:?}",
    );
}

/// Run the `invalid-stdout` fixture child (which writes the single raw byte
/// `0xff` to stdout via [`Write::write_all`] and flushes, emitting NO child
/// stderr) under 1 MiB per-stream caps and a 2 s budget, and assert that
/// [`run`] returns a SUCCESSFUL [`CommandOutcome`] whose status is
/// [`UnixStatus::Exited`] (0) — stdout is NEVER decoded, so a stray non-UTF-8
/// byte is honest payload, NOT an execution error — that the retained raw
/// stdout STILL contains byte `0xff`, that `stdout_total_bytes` equals the
/// retained stdout length (no cap overflow on a tiny payload), and that the
/// REQUIRED `max_rss_kib` is a real positive value. On macOS a sandbox that
/// denies `sysctl kern.clockrate` makes the spawn unrunnable; this test
/// preflights that EXACT condition and skips it — unsandboxed verification is
/// required.
#[test]
fn run_invalid_stdout_is_retained_raw_bytes() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_invalid_stdout_is_retained_raw_bytes: SKIPPED on macOS \
                 — sandbox denies `sysctl kern.clockrate`; unsandboxed \
                 verification required."
            );
            return;
        }
    }

    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    let spec = fixture_spec("invalid-stdout", Duration::from_secs(2), mib, mib);

    let start = Instant::now();
    let outcome = run(&spec, host_flavor())
        .expect("invalid-stdout is honest raw payload (stdout is never decoded)");
    let elapsed = start.elapsed();

    // stdout is NEVER decoded: a stray 0xff byte is honest payload, so the run
    // succeeds with a normal exit 0.
    assert_eq!(outcome.status, UnixStatus::Exited(0));

    // The retained raw stdout still contains the child's 0xff byte (the harness
    // text surrounds it, but the byte survives within the 1 MiB cap).
    assert!(
        outcome.stdout.contains(&0xff),
        "retained stdout should contain byte 0xff, got {:?}",
        outcome.stdout,
    );

    // No cap overflow: every stdout byte seen was retained, so the saturating
    // total equals the retained-buffer length.
    assert_eq!(
        outcome.stdout_total_bytes,
        outcome.stdout.len() as u64,
        "stdout_total_bytes should equal retained stdout len (no cap overflow)",
    );

    // `max_rss_kib` is REQUIRED and must reflect a real process.
    let rss: u64 = outcome.max_rss_kib;
    assert!(rss > 0, "max_rss_kib should be positive, got {rss}");

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s, got {elapsed:?}",
    );
}

/// Run the `invalid-stderr` fixture child (which writes the single raw byte
/// `0xff` to stderr via [`Write::write_all`] and flushes, emitting NO child
/// stdout) under 1 MiB per-stream caps and a 2 s budget, and assert that
/// [`run`] returns EXACTLY [`CommandError::Utf8`] { `stream: Stream::Stderr` }
/// — stderr IS decoded strict-UTF-8, so a stray non-UTF-8 byte on the combined
/// stderr pipe (the child's payload plus the `/usr/bin/time` metric line) is
/// rejected BEFORE the REQUIRED `max_rss_kib` parse, and never surfaces as
/// retained raw bytes. On macOS a sandbox that denies `sysctl kern.clockrate`
/// makes the spawn unrunnable; this test preflights that EXACT condition and
/// skips it — unsandboxed verification is required.
#[test]
fn run_invalid_stderr_rejects_strict_utf8() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_invalid_stderr_rejects_strict_utf8: SKIPPED on macOS — \
                 sandbox denies `sysctl kern.clockrate`; unsandboxed \
                 verification required."
            );
            return;
        }
    }

    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    let spec = fixture_spec("invalid-stderr", Duration::from_secs(2), mib, mib);

    let start = Instant::now();
    let err = run(&spec, host_flavor())
        .expect_err("invalid-stderr should be rejected as non-UTF-8, not succeed");
    let elapsed = start.elapsed();

    // stderr is decoded strict-UTF-8: a stray 0xff byte on the combined pipe
    // (child payload + `/usr/bin/time` metric line) is rejected, BEFORE the
    // RSS parse, as exactly Utf8 { stream: Stream::Stderr }.
    assert_eq!(
        err,
        CommandError::Utf8 {
            stream: Stream::Stderr
        }
    );

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s, got {elapsed:?}",
    );
}

/// Run the `dual-streams` fixture child (exactly 1 MiB of `O` on stdout and
/// 1 MiB of `E` on stderr) under 32 KiB per-stream caps and a 5 s budget, and
/// assert that [`run`] returns EXACTLY [`CommandError::CapOverflow`] {
/// `stream: Stream::Stdout` } — BOTH streams overflow their 32 KiB caps, but
/// stdout-cap precedence is checked BEFORE stderr-cap, so the deterministic
/// error names stdout (the stderr overflow never surfaces even though it also
/// occurred). On macOS a sandbox that denies `sysctl kern.clockrate` makes the
/// spawn unrunnable; this test preflights that EXACT condition and skips it —
/// unsandboxed verification is required.
#[test]
fn run_dual_streams_stdout_cap_precedence() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_dual_streams_stdout_cap_precedence: SKIPPED on macOS — \
                 sandbox denies `sysctl kern.clockrate`; unsandboxed \
                 verification required."
            );
            return;
        }
    }

    let kib32 = NonZeroU64::new(32 * 1024).expect("32 KiB is nonzero");
    let spec = fixture_spec("dual-streams", Duration::from_secs(5), kib32, kib32);

    let start = Instant::now();
    let err =
        run(&spec, host_flavor()).expect_err("both 32 KiB caps overflow against 1 MiB payloads");
    let elapsed = start.elapsed();

    // BOTH streams overflow, but stdout-cap precedence (checked before
    // stderr-cap) yields EXACTLY CapOverflow { stream: Stream::Stdout }.
    assert_eq!(
        err,
        CommandError::CapOverflow {
            stream: Stream::Stdout
        }
    );

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s (5 s budget), got {elapsed:?}",
    );
}

/// Run the `invalid-stderr` fixture child (single raw byte `0xff` on stderr)
/// under a 1 MiB stdout cap, a 1-byte stderr cap, and a 2 s budget, and assert
/// that [`run`] returns EXACTLY [`CommandError::CapOverflow`] {
/// `stream: Stream::Stderr` } — the combined stderr pipe (the child's `0xff`
/// plus the `/usr/bin/time` metric line) exceeds the 1-byte stderr cap, and
/// cap overflow is checked BEFORE the strict-UTF-8 decode, so this proves cap
/// overflow PRECEDES strict stderr UTF-8 rejection (the sibling
/// [`run_invalid_stderr_rejects_strict_utf8`] test, at a 1 MiB cap, would
/// otherwise surface `Utf8` and mask the precedence). On macOS a sandbox that
/// denies `sysctl kern.clockrate` makes the spawn unrunnable; this test
/// preflights that EXACT condition and skips it — unsandboxed verification is
/// required.
#[test]
fn run_invalid_stderr_cap_precedes_utf8() {
    #[cfg(target_os = "macos")]
    {
        if macos_sysctl_sandbox_denied() {
            eprintln!(
                "run_invalid_stderr_cap_precedes_utf8: SKIPPED on macOS — \
                 sandbox denies `sysctl kern.clockrate`; unsandboxed \
                 verification required."
            );
            return;
        }
    }

    let mib = NonZeroU64::new(1024 * 1024).expect("1 MiB is nonzero");
    let one_byte = NonZeroU64::new(1).expect("1 is nonzero");
    let spec = fixture_spec("invalid-stderr", Duration::from_secs(2), mib, one_byte);

    let start = Instant::now();
    let err =
        run(&spec, host_flavor()).expect_err("1-byte stderr cap overflows on the combined pipe");
    let elapsed = start.elapsed();

    // Cap overflow is checked BEFORE the strict-UTF-8 decode: the combined
    // stderr pipe exceeds the 1-byte cap, so EXACTLY CapOverflow {
    // stream: Stream::Stderr } — NOT Utf8 (which the sibling
    // run_invalid_stderr_rejects_strict_utf8 test exercises at a 1 MiB cap) —
    // proving the precedence.
    assert_eq!(
        err,
        CommandError::CapOverflow {
            stream: Stream::Stderr
        }
    );

    assert!(
        elapsed < Duration::from_secs(10),
        "outer elapsed should be < 10 s, got {elapsed:?}",
    );
}

/// macOS-only sandbox preflight shared by [`run_success_smoke`] and the
/// `run_timeout_*` tests: spawn the absolute `/usr/bin/time -l /usr/bin/true`
/// under a fail-closed, locale-stable environment and capture its stderr.
/// Returns `true` IFF the stderr contains the EXACT known sandbox failure
/// substring `"sysctl kern.clockrate: Operation not permitted"`.
///
/// No other spawn error or stderr content is treated as skippable: those cases
/// return `false` so the caller proceeds to [`run`] and the real failure
/// surfaces.
#[cfg(target_os = "macos")]
fn macos_sysctl_sandbox_denied() -> bool {
    use std::process::{Command, Stdio};
    let skip_marker = "sysctl kern.clockrate: Operation not permitted";
    let mut cmd = Command::new("/usr/bin/time");
    cmd.arg("-l").arg("/usr/bin/true");
    cmd.env_clear();
    cmd.env("LC_ALL", "C");
    cmd.env("LANG", "C");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    let output = match cmd.output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    String::from_utf8_lossy(&output.stderr).contains(skip_marker)
}
