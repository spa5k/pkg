//! Spike S4 (PR-6 / DR-004) — EXECUTE slice: an INTERNAL finite-time executor
//! that runs a validated [`CommandSpec`] under `/usr/bin/time` and reports its
//! outcome.
//!
//! # What it is
//! This is an **internal primitive** for the harness. Given a validated
//! [`CommandSpec`] and a `/usr/bin/time` dialect, [`run`] spawns the program in
//! its OWN process group, captures stdout/stderr concurrently under bounded
//! caps, polls the child with non-blocking `try_wait` against the validated
//! wall-clock timeout, and on timeout signals the ENTIRE fresh child group with
//! `SIGKILL` and reaps the `/usr/bin/time` leader. It then maps the reaped
//! status to [`UnixStatus`] and parses the required maximum-RSS line out of the
//! combined stderr.
//!
//! # Pipeline
//! Given a validated spec and a `/usr/bin/time` dialect, [`run`]:
//!   1. validates the spec (absolute program; nonzero caps enforced by type;
//!      timeout in `1 ms..=1 h`);
//!   2. builds `/usr/bin/time <flag> <program> <args...>` with a **fail-closed**
//!      environment (`env_clear()` then EXACTLY `spec.env`, so nothing is
//!      inherited from the parent), a null stdin, piped stdout/stderr, and
//!      `process_group(0)` so the child runs in its OWN process group
//!      (pgid == child pid);
//!   3. records the wall-clock start [`Instant`] immediately BEFORE spawn, then
//!      spawns. The deadline is `start + timeout`, so the enforced budget
//!      includes spawn + `/usr/bin/time` wrapper + child latency;
//!   4. takes both capture pipes — if either is absent it runs the group-kill
//!      cleanup ([`cleanup_child`]); a cleanup `Kill`/`Wait` failure OVERRIDES
//!      the [`CommandError::MissingPipe`], otherwise it returns `MissingPipe`;
//!   5. drains BOTH pipes concurrently (scoped threads +
//!      [`StreamCapture::drain`]) while polling the child with
//!      [`poll_until_deadline`]: non-blocking `try_wait` probes at a 2 ms
//!      monotonic interval until the child exits, the deadline is reached, or a
//!      poll fails. At the deadline exactly ONE final `try_wait` resolves the
//!      final-poll race (see [`poll_until_deadline`]). `wall_ms` is captured at
//!      the observation instant — the `try_wait` that returned `Some`/`Err`, or
//!      the final-poll `None` confirming the deadline — so it is measured from
//!      `start` through the observation and deliberately EXCLUDES the
//!      cleanup/kill and the deterministic stdout-before-stderr reader joins
//!      that follow;
//!   6. resolves the poll event into a lifecycle result. An `Exited` status, or
//!      a `Deadline` whose group signal raced to already-gone (`ESRCH`) and
//!      whose leader reaped successfully, is an HONEST natural completion using
//!      that reaped [`ExitStatus`]. A poll failure surfaces
//!      [`CommandError::Poll`] and a genuine deadline (final `None`) with the
//!      group signal accepted surfaces [`CommandError::Timeout`] {
//!      `killed: true` }; in BOTH cases the whole-group `SIGKILL` + leader reap
//!      of [`cleanup_child`] runs first, and a cleanup `Kill`/`Wait` failure
//!      OVERRIDES the in-flight error. The readers are ALWAYS joined (stdout
//!      before stderr, deterministic) after cleanup;
//!   7. keeps stdout RAW (no decoding), and validates the stderr cap BEFORE
//!      decoding: the cap bounds the COMBINED child stderr plus the
//!      `/usr/bin/time` diagnostics (which are interleaved on the same pipe),
//!      and a cap overflow fails closed as [`CommandError::CapOverflow`] BEFORE
//!      the strict-UTF-8 decode and the required `max_rss_kib` extraction via
//!      [`crate::timeparse::parse_max_rss`];
//!   8. maps the reaped [`ExitStatus`] to [`UnixStatus`] (`code` OR `signal`,
//!      never the `128 + signal` shell anti-pattern).
//!
//! A normal nonzero exit — and likewise a signal termination — is a successful
//! [`CommandOutcome`] carrying the appropriate [`UnixStatus`]; only structural,
//! capture, and parse failures are [`CommandError`].
//!
//! # Timeout semantics
//! The wall budget and the deadline both start at the [`Instant`] captured
//! immediately BEFORE spawn, so they cover spawn + `/usr/bin/time` wrapper + the
//! child's own run latency. `wall_ms` is captured the moment the poll loop
//! resolves — an observed status, a poll failure, or the final-poll `None` at
//! the deadline — and the group-kill/leader reap and the reader joins are
//! excluded. The single final `try_wait` at the deadline prevents
//! misclassifying a child that exited since the last probe. If that final poll
//! is `None` AND the group `SIGKILL` is accepted, the result is a
//! [`CommandError::Timeout`] { `killed: true` }. If instead
//! `kill_process_group` returned `ESRCH` (the group had already exited) and the
//! leader then reaped successfully, that is an HONEST natural completion using
//! the reaped status. Any OTHER cleanup error (a `Kill`/`Wait` failure)
//! OVERRIDES the in-flight poll/timeout condition.
//!
//! # Deterministic error precedence
//! Errors are surfaced in a fixed order so identical conditions always yield the
//! same error. Within the lifecycle step a cleanup `Kill`/`Wait` failure
//! OVERRIDES the triggering [`CommandError::Poll`]/[`CommandError::Timeout`];
//! the resulting lifecycle error then OVERRIDES every reader error. A normal or
//! already-gone completion (no lifecycle error) processes the readers and
//! payload in this order:
//!   stdout reader (`ReaderPanic`/`ReaderIo`) → stderr reader →
//!   stdout `CapOverflow` → stderr `CapOverflow` → stderr `Utf8` → `Rss`.
//! [`CommandError::MissingPipe`] is its own early path: a cleanup `Kill`/`Wait`
//! failure there OVERRIDES it.
//!
//! rustix (`process`/`io`) + std, no `unsafe` (forbidden crate-wide),
//! Unix-targeted.

use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

use crate::command::{
    CaptureError, CommandError, CommandOutcome, CommandSpec, Stream, StreamCapture, TimeFlavor,
    UnixStatus,
};
use crate::timeparse::parse_max_rss;

/// Absolute path to the external `time` wrapper that reports maximum RSS. This
/// primitive invokes `time` directly and never searches `PATH`.
const TIME_BIN: &str = "/usr/bin/time";

/// Run a validated `spec` under `/usr/bin/time` (dialect `flavor`) and return
/// the captured outcome.
///
/// See the [module docs](self) for the full contract. In short: this is an
/// **internal finite-time executor**. It enforces `spec.timeout` as a wall
/// budget starting immediately before spawn (covering spawn + the
/// `/usr/bin/time` wrapper + child latency) by polling the child with
/// non-blocking `try_wait` ([`poll_until_deadline`]) and, on a genuine
/// deadline, signaling the ENTIRE fresh child group with `SIGKILL` and reaping
/// the leader ([`cleanup_child`]).
///
/// # Environment
/// Fail-closed: the child sees EXACTLY `spec.env` (applied via `env_clear()`
/// followed by these entries) and nothing inherited from the parent.
///
/// # Outcome vs. error
/// A normal nonzero exit, a signal termination, or a timeout whose group
/// signal raced to already-gone and reaped successfully, is returned as a
/// successful [`CommandOutcome`] (the `status` field records which). A genuine
/// deadline (final `None`) with the group signal accepted is
/// [`CommandError::Timeout`] { `killed: true` }. Other errors are reserved for
/// structural and capture/parse failures (see the module-level precedence
/// list).
pub(crate) fn run(spec: &CommandSpec, flavor: TimeFlavor) -> Result<CommandOutcome, CommandError> {
    // 1. Validate the spec fully (absolute program, nonzero caps by type,
    //    timeout in `1 ms..=1 h`).
    spec.validate()?;

    // 2. Build `/usr/bin/time <flag> <program> <args...>` with a fail-closed
    //    environment, null stdin, piped stdout/stderr, and its own process
    //    group. Copy the nonzero caps out so the drain closures do not need to
    //    borrow `spec`.
    let stdout_cap = spec.stdout_cap;
    let stderr_cap = spec.stderr_cap;
    let timeout = spec.timeout;

    let mut cmd = Command::new(TIME_BIN);
    cmd.arg(time_flag(flavor));
    cmd.arg(&spec.program);
    cmd.args(&spec.args);
    cmd.env_clear();
    for (key, val) in &spec.env {
        cmd.env(key, val);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Isolate the child in its own process group (pgid == child pid). This is a
    // safe std API (no `unsafe`). The group is targeted both at a genuine
    // deadline (via [`poll_until_deadline`] → [`cleanup_child`]) and on a
    // structural/poll-failure cleanup path.
    cmd.process_group(0);

    // 3. Wall clock + deadline start immediately BEFORE spawn. `wall_ms` is
    //    captured later (step 6) at the instant the poll loop resolves, so the
    //    budget covers spawn + `/usr/bin/time` wrapper + child lifetime but NOT
    //    the cleanup/kill or the reader joins.
    let start = Instant::now();

    // 4. Spawn.
    let mut child = cmd
        .spawn()
        .map_err(|e| CommandError::Spawn { kind: e.kind() })?;

    // 5. Take both capture pipes. Either being `None` is a structural
    //    misconfiguration (we configured both as piped): run the group-kill
    //    cleanup so we do not leak a process — a cleanup `Kill`/`Wait` failure
    //    OVERRIDES the `MissingPipe` we are about to return — then fail with
    //    `MissingPipe`. Stdout takes precedence if both are somehow absent.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout_pipe, stderr_pipe) = match (stdout_pipe, stderr_pipe) {
        (Some(out), Some(err)) => (out, err),
        (None, _) => {
            // Cleanup may surface Kill/Wait; its success payload (signal sent
            // vs. group already gone) is irrelevant here — we only need the
            // leader reaped before reporting the structural MissingPipe.
            let _ = cleanup_child(&mut child)?;
            return Err(CommandError::MissingPipe {
                stream: Stream::Stdout,
            });
        }
        (_, None) => {
            let _ = cleanup_child(&mut child)?;
            return Err(CommandError::MissingPipe {
                stream: Stream::Stderr,
            });
        }
    };

    // 6. Drain BOTH pipes concurrently while polling the child against the
    //    validated timeout. The scoped reader threads own the pipe read-ends;
    //    [`poll_until_deadline`] runs the non-blocking `try_wait` probes on THIS
    //    thread.
    //
    //    `wall_ms` is captured at the instant the poll loop resolves — the
    //    `try_wait` that returned `Some`/`Err`, or the final-poll `None`
    //    confirming the deadline — so a large buffered stdout (e.g. emitted by
    //    the index) cannot inflate the reported process lifetime with
    //    reader-join / drain time, and neither the cleanup/kill nor the reader
    //    joins are counted.
    //
    //    A `Failed` poll event does NOT imply the child exited — it may still
    //    be alive (e.g. an interrupted `waitpid`). Joining the readers while it
    //    is alive could deadlock on a pipe it still holds open, so the `Failed`
    //    and `Deadline` cases run the group-kill cleanup ([`cleanup_child`])
    //    BEFORE joining, closing the group's pipe write-ends so the readers can
    //    reach EOF. We ALWAYS join BOTH readers (stdout before stderr,
    //    deterministic) regardless of the poll outcome. A cleanup `Kill`/`Wait`
    //    failure OVERRIDES the in-flight `Poll`/`Timeout`; otherwise a `Poll`
    //    failure stays `Poll` and a genuine deadline with the group signal
    //    accepted is `Timeout { killed: true }`, while a deadline whose group
    //    had already raced to gone (`ESRCH`) and reaped successfully is an
    //    HONEST natural completion using that status (see step 8).
    let (lifecycle_res, wall_ms, stdout_join, stderr_join) = thread::scope(|s| {
        let stdout_thread = s.spawn(move || StreamCapture::drain(stdout_pipe, stdout_cap));
        let stderr_thread = s.spawn(move || StreamCapture::drain(stderr_pipe, stderr_cap));
        // Poll the child against the validated timeout until it exits, the
        // deadline is reached, or a poll fails.
        let poll_outcome = poll_until_deadline(&mut child, start, timeout);
        let wall_ms = poll_outcome.wall_ms;
        // Convert the poll event into a `Result<ExitStatus, CommandError>`
        // BEFORE joining the readers, so a lifecycle error (Poll/Timeout)
        // takes precedence over reader errors. The Failed and Deadline cases
        // run the group-kill cleanup first — closing the group's pipe
        // write-ends so the readers can reach EOF — and a cleanup Kill/Wait
        // failure OVERRIDES the in-flight error. A genuine Deadline whose
        // group signal is accepted is `Timeout { killed: true }`; a Deadline
        // whose group had already raced to gone (`ESRCH`) and reaped
        // successfully is an HONEST natural completion carrying that reaped
        // status.
        let lifecycle_res: Result<ExitStatus, CommandError> = match poll_outcome.event {
            PollEvent::Exited(status) => Ok(status),
            PollEvent::Failed { kind } => match cleanup_child(&mut child) {
                Err(cleanup_err) => Err(cleanup_err),
                Ok(_) => Err(CommandError::Poll { kind }),
            },
            PollEvent::Deadline => match cleanup_child(&mut child) {
                Err(cleanup_err) => Err(cleanup_err),
                Ok(GroupCleanup::SignalSent(status)) => {
                    let _ = status;
                    Err(CommandError::Timeout { killed: true })
                }
                Ok(GroupCleanup::AlreadyGone(status)) => Ok(status),
            },
        };
        // Always join BOTH readers (stdout before stderr, deterministic)
        // regardless of the lifecycle result.
        let stdout_join = stdout_thread.join();
        let stderr_join = stderr_thread.join();
        (lifecycle_res, wall_ms, stdout_join, stderr_join)
    });

    // 7. `wall_ms` was captured inside the scope at the instant the poll loop
    //    resolved; it deliberately excludes the cleanup/kill and the
    //    reader-join cleanup that follow.

    // 8. Lifecycle errors (timeout / poll failure / cleanup failure) take
    //    precedence over reader errors, so apply the question-mark BEFORE
    //    joining the capture readers.
    let exit_status = lifecycle_res?;
    let stdout_cap = join_capture(stdout_join)?;
    let stderr_cap = join_capture(stderr_join)?;

    // Cap overflow fails closed here, BEFORE the stderr UTF-8/RSS parse below.
    // The stderr cap bounds the COMBINED child stderr + `/usr/bin/time`
    // diagnostics captured on the same pipe; stdout stays RAW (no decode).
    if stdout_cap.is_overflow() {
        return Err(CommandError::CapOverflow {
            stream: Stream::Stdout,
        });
    }
    if stderr_cap.is_overflow() {
        return Err(CommandError::CapOverflow {
            stream: Stream::Stderr,
        });
    }

    // 9. stdout stays RAW (no decode). The stderr cap (which bounds the COMBINED
    //    child stderr + `/usr/bin/time` diagnostics, checked above) already
    //    failed closed; stderr is now decoded strict-UTF-8, then the REQUIRED
    //    max-RSS metric line is parsed out of it.
    let stdout_total_bytes = stdout_cap.total_bytes();
    let stdout = stdout_cap.into_retained();

    let stderr_total_bytes = stderr_cap.total_bytes();
    let stderr_bytes = stderr_cap.into_retained();
    let stderr_str = std::str::from_utf8(&stderr_bytes).map_err(|_| CommandError::Utf8 {
        stream: Stream::Stderr,
    })?;
    let parsed = parse_max_rss(stderr_str).map_err(|_| CommandError::Rss)?;

    // 10. Map the reaped status: code OR signal, never `128 + signal`.
    let status = unix_status(exit_status)?;

    Ok(CommandOutcome {
        status,
        stdout,
        cleaned_stderr: parsed.stderr,
        stdout_total_bytes,
        stderr_total_bytes,
        wall_ms,
        max_rss_kib: parsed.max_rss_kib,
    })
}

/// The `/usr/bin/time` flag selecting the RSS-reporting dialect for `flavor`.
fn time_flag(flavor: TimeFlavor) -> &'static str {
    match flavor {
        TimeFlavor::MacOs => "-l",
        TimeFlavor::Gnu => "-v",
    }
}

/// Outcome of a successful group-kill cleanup ([`cleanup_child`]): the leader
/// was reaped, tagged with whether the group signal was accepted or had
/// already raced to gone. Both variants carry the reaped [`ExitStatus`] of the
/// `/usr/bin/time` leader.
///
/// This enum is private to this module. The `Deadline` handling in [`run`]
/// CONSUMES both variants: [`SignalSent`](GroupCleanup::SignalSent) is a
/// genuine timeout, while [`AlreadyGone`](GroupCleanup::AlreadyGone) is an
/// honest natural completion carrying the reaped status. The `MissingPipe` and
/// poll-`Failed` paths ignore the successful payload — they only need the
/// leader reaped and a cleanup error surfaced (see [`cleanup_child`]).
#[derive(Debug)]
enum GroupCleanup {
    /// `kill_process_group` returned `Ok` (the group signal was accepted),
    /// then the leader was reaped by `wait` carrying this [`ExitStatus`].
    SignalSent(ExitStatus),
    /// `kill_process_group` returned [`Errno::SRCH`] (the group had already
    /// exited — the "already gone" race), then the leader was reaped by `wait`
    /// carrying this [`ExitStatus`].
    AlreadyGone(ExitStatus),
}

/// Structured cleanup of a child whose run we are aborting: signal the ENTIRE
/// fresh child group (the `/usr/bin/time` leader plus any grandchildren it
/// forked) with `SIGKILL` via the group's process-group id (equal to the child
/// pid), then reap the leader with `child.wait()`.
///
/// On success the [`GroupCleanup`] payload distinguishes the two reaped-leader
/// races: [`GroupCleanup::SignalSent`] when `kill_process_group` returned `Ok`
/// (the group signal was accepted), and [`GroupCleanup::AlreadyGone`] when it
/// returned [`Errno::SRCH`] (the group had already exited). Both then reap the
/// leader and carry its [`ExitStatus`].
///
/// Any OTHER signal error (not `SRCH`) is reduced to a stable
/// [`io::ErrorKind`] (via [`io::Error::from`] on the [`Errno`]) and, after a
/// best-effort leader-only `kill`+`wait`, surfaces as [`CommandError::Kill`].
/// A reap (`wait`) failure surfaces as [`CommandError::Wait`].
///
/// The returned cleanup error (Kill/Wait) is intended to OVERRIDE the
/// in-flight error a caller was about to surface (`MissingPipe`, a `Poll`
/// failure, or a genuine `Timeout`): a failed cleanup may have leaked a
/// process, which is strictly worse than the already-diagnosed condition. The
/// success payload is consumed by the `Deadline` path (timeout vs. honest
/// already-gone completion) and ignored by the `MissingPipe` and `Poll` paths,
/// which only need the leader reaped.
fn cleanup_child(child: &mut Child) -> Result<GroupCleanup, CommandError> {
    let pgid = Pid::from_child(child);
    // Did the group signal get accepted (Ok), or had the group already exited
    // (ESRCH)? Both proceed to reap the leader; only the tagging differs.
    let signaled = match kill_process_group(pgid, Signal::KILL) {
        Ok(()) => true,
        Err(Errno::SRCH) => false,
        Err(errno) => {
            let kind = io::Error::from(errno).kind();
            // Best-effort leader-only kill+wait before surfacing the signal
            // error; intentionally ignored (the child may already be gone) —
            // the surfaced `Kill` is the meaningful failure.
            let _ = child.kill();
            let _ = child.wait();
            return Err(CommandError::Kill { kind });
        }
    };
    let status = child
        .wait()
        .map_err(|e| CommandError::Wait { kind: e.kind() })?;
    if signaled {
        Ok(GroupCleanup::SignalSent(status))
    } else {
        Ok(GroupCleanup::AlreadyGone(status))
    }
}

/// Resolve a scoped reader-thread join into a [`StreamCapture`]: a panic in the
/// reader becomes [`CommandError::ReaderPanic`]; a capture read error becomes
/// the mapped [`CommandError::ReaderIo`] (via [`CommandError::from`]).
fn join_capture(
    join: thread::Result<Result<StreamCapture, CaptureError>>,
) -> Result<StreamCapture, CommandError> {
    match join {
        Ok(Ok(cap)) => Ok(cap),
        Ok(Err(capture_err)) => Err(CommandError::from(capture_err)),
        Err(_) => Err(CommandError::ReaderPanic),
    }
}

/// Wall-clock elapsed milliseconds since `start`, saturating into `u64` so a
/// pathologically long run cannot overflow.
fn elapsed_ms_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Map a reaped [`ExitStatus`] to [`UnixStatus`]: the exit code if the child
/// exited normally, otherwise the terminating signal. Never falls back to the
/// `128 + signal` shell convention.
///
/// The `(None, None)` case is unreachable for a Unix child returned by `wait`
/// (exited and signaled are exhaustive), but it is kept panic-free by surfacing
/// a [`CommandError::Wait`] rather than fabricating a status.
fn unix_status(status: ExitStatus) -> Result<UnixStatus, CommandError> {
    if let Some(code) = status.code() {
        Ok(UnixStatus::Exited(code))
    } else if let Some(signal) = status.signal() {
        Ok(UnixStatus::Signaled(signal))
    } else {
        Err(CommandError::Wait {
            kind: io::ErrorKind::Other,
        })
    }
}

// ---------------------------------------------------------------------------
// Timeout-polling primitive (used by `run`)
// ---------------------------------------------------------------------------
//
// Monotonic child-polling primitive used by [`run`] to enforce the validated
// `spec.timeout`. [`poll_until_deadline`] performs NO killing and NO blocking
// `wait` itself: on a genuine deadline it returns [`PollEvent::Deadline`] and
// hands kill/reap ownership back to [`run`], which runs [`cleanup_child`]
// (whole-group `SIGKILL` + leader reap) and then surfaces the result.

/// Monotonic polling interval used by [`poll_until_deadline`]. After each
/// `None` probe (child still running) the loop sleeps for at most this long —
/// small enough that deadline enforcement stays snappy, large enough that the
/// per-iteration `waitpid(WNOHANG)` syscall and wakeup cost is negligible. The
/// loop always sleeps `min(POLL_INTERVAL, remaining)`, so it neither
/// busy-spins nor intentionally sleeps past the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The resolved outcome of one [`poll_until_deadline`] loop: WHAT the loop
/// resolved to ([`PollEvent`]) tagged with the wall time elapsed since the
/// caller-supplied `start` and captured at the observation/decision instant.
///
/// `wall_ms` is metadata about the observation — when we noticed — kept on the
/// struct so the three-way [`PollEvent`] stays focused on what happened. It
/// uses the same saturating definition as [`elapsed_ms_since`].
#[derive(Debug)]
struct PollOutcome {
    /// Wall-clock ms since `start`, captured at the instant the loop resolved
    /// (the `try_wait` that returned `Some`/`Err`, or the final-poll `None`
    /// that confirmed a genuine deadline).
    wall_ms: u64,
    /// What the poll loop resolved to.
    event: PollEvent,
}

/// What a [`poll_until_deadline`] loop resolved to. The three cases are
/// disjoint and exhaustive, so a caller cannot silently forget the "still
/// running at the deadline" case.
#[derive(Debug)]
enum PollEvent {
    /// `try_wait` returned an observed [`ExitStatus`]: the child has been
    /// reaped and its pid is reclaimed. Produced either mid-loop or by the
    /// single final-poll race check performed at the deadline.
    Exited(ExitStatus),
    /// The deadline was reached and the final confirming poll STILL observed
    /// the child running (`try_wait` returned `None`). The child is NOT reaped;
    /// the caller owns killing/reaping it (and any reader cleanup).
    Deadline,
    /// A `try_wait` returned `Err`; `kind` is the stable mapped
    /// [`io::ErrorKind`]. The child may STILL BE ALIVE — a poll error does NOT
    /// imply the child exited — and the caller owns cleanup.
    Failed {
        /// The stable, mapped I/O error kind (deterministic; not the
        /// platform-localized message).
        kind: io::ErrorKind,
    },
}

/// Monotonically poll `child` with non-blocking [`Child::try_wait`] until it
/// exits, the `start + timeout` deadline is reached, or a poll fails.
///
/// [`run`] calls this to enforce the validated `spec.timeout`. It performs NO
/// killing and NO blocking `wait` itself: on a genuine deadline it returns
/// [`PollEvent::Deadline`] (the child is NOT reaped) and hands kill/reap
/// ownership back to [`run`], which runs [`cleanup_child`] — whole-group
/// `SIGKILL` + leader reap — and then surfaces the result. The 2 ms
/// [`POLL_INTERVAL`] keeps deadline enforcement snappy without busy-spinning.
///
/// # Time model
/// `start` is supplied by the caller from **immediately before** the child was
/// spawned, so the deadline is `start + timeout` and every reported
/// [`PollOutcome::wall_ms`] is `elapsed_ms_since(start)` captured at the
/// observation/decision instant — the moment `try_wait` resolved to
/// `Some`/`Err`, or (for the deadline case) the moment the final poll
/// confirmed the child was still `None`.
///
/// # The final-poll race (why the deadline result is HONEST)
/// A naive "stop when the clock passes the deadline" rule has a race: the
/// child may exit in the window between the last `None` probe and the deadline
/// check, and a pure-clock timeout would then misreport an already-exited
/// child as a timeout (and, worse, target a dead pid for a kill). So the
/// instant the monotonic clock reaches the deadline this primitive performs
/// **exactly ONE** final non-blocking [`Child::try_wait`]:
///   * `Some(status)` ⇒ the child exited honestly; return [`PollEvent::Exited`].
///   * `Err(e)`        ⇒ the final poll itself failed; return [`PollEvent::Failed`].
///   * `None`          ⇒ the child is genuinely still running; return
///     [`PollEvent::Deadline`].
///
/// "Exactly one" matters: we do not loop after the deadline, since looping
/// would silently extend the timeout. The single final poll is bounded and
/// closes the race without slipping the deadline.
///
/// # Sleep model
/// Before the deadline, each `None` probe is followed by a [`thread::sleep`]
/// of `min(POLL_INTERVAL, remaining)`, where `remaining` is the strictly-
/// positive time left to the deadline. We therefore never busy-spin, and never
/// **intentionally** schedule a sleep past the deadline. (The OS may wake us a
/// scheduler tick late; that is caught on the next iteration's
/// `now >= deadline` check and resolved by the single final poll above — it
/// does not extend the timeout.)
///
/// # Overflow safety (why `start + timeout` is panic-free)
/// [`Instant`] addition panics if the result would overflow the underlying
/// monotonic representation. This primitive does NOT re-validate `timeout`:
/// the contract is that the caller passes a timeout already validated to be at
/// most `MAX_TIMEOUT` (1 h) — as [`CommandSpec::validate`] enforces (see
/// `crate::command::MAX_TIMEOUT`). Adding at most one hour to a recent
/// [`Instant`] is nowhere near the representation ceiling on either supported
/// platform (Linux `CLOCK_MONOTONIC` / Darwin `mach_absolute_time`), so
/// `start + timeout` is panic-free by the validated-range invariant. Likewise
/// `deadline - now` is only evaluated when `now < deadline`, so it cannot
/// underflow.
///
/// # Safety posture
/// std-only: [`Child::try_wait`], [`Instant`], [`Duration`], [`thread::sleep`].
/// No `unsafe`, no `libc`, no shell.
fn poll_until_deadline(child: &mut Child, start: Instant, timeout: Duration) -> PollOutcome {
    // Deadline = start + timeout. Panic-free by the validated-range invariant
    // documented above (timeout <= MAX_TIMEOUT == 1 h, caller-validated).
    let deadline = start + timeout;

    loop {
        // Non-blocking probe. The `None` arm (child still running) falls
        // through to the deadline/sleep logic below.
        match child.try_wait() {
            // Child reaped: record its honest status at the instant `try_wait`
            // resolved.
            Ok(Some(status)) => {
                return PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Exited(status),
                };
            }
            // Poll failure. The child may STILL BE ALIVE (a poll error does
            // not imply exit); surface the stable kind and let the caller own
            // cleanup. Wall time is the instant the failing poll resolved.
            Err(e) => {
                return PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Failed { kind: e.kind() },
                };
            }
            // Still running at this probe; decide against the deadline below.
            Ok(None) => {}
        }

        let now = Instant::now();
        if now >= deadline {
            // Deadline reached. Perform EXACTLY ONE final non-blocking poll to
            // close the final-poll race (see the rustdoc): the child may have
            // exited since the last probe, and reporting a timeout for an
            // already-exited child would be dishonest. We do NOT loop here —
            // looping would silently extend the timeout.
            return match child.try_wait() {
                Ok(Some(status)) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Exited(status),
                },
                Err(e) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Failed { kind: e.kind() },
                },
                // Genuinely still running at the deadline: the honest deadline
                // result. The child is NOT reaped; the caller owns
                // killing/reaping it. Wall time is the instant this final poll
                // confirmed `None`.
                Ok(None) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Deadline,
                },
            };
        }

        // Sleep the SMALLER of POLL_INTERVAL and the time remaining to the
        // deadline: never busy-spin, never intentionally sleep past the
        // deadline. `deadline - now` is strictly positive here (we are past
        // the `now >= deadline` check), so it cannot underflow/panic.
        let remaining = deadline - now;
        let sleep_dur = if remaining < POLL_INTERVAL {
            remaining
        } else {
            POLL_INTERVAL
        };
        thread::sleep(sleep_dur);
    }
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
