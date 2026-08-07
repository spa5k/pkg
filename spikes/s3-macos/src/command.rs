//! Spike S3 (PR-7) — COMMAND slice: a small bounded absolute-program command
//! spec/executor for FIXED probes.
//!
//! This module is the spawn primitive shared by the Detect and Preflight lanes.
//! Detect uses it to run a handful of fixed, read-only host probes
//! (`xcode-select -p`, `xcrun --find notarytool`, `security find-identity …`,
//! `dscl …`); Preflight uses it to run the fixed Nix cache-coverage probes
//! (`nix --version`, `nix flake prefetch`, `nix store info`, `nix derivation
//! show`, `nix path-info`). The Detect host probes are read-only; the Preflight
//! Nix probes are build-free and activation-free, but NOT read-only or
//! mutation-free — when a [`RealRunner`] caller executes them, prefetch may add
//! the pinned source to the Nix store/fetch cache and evaluation may populate
//! ordinary Nix-managed state. It is deliberately far smaller than a general
//! executor: there is no `/usr/bin/time` wrapper, no RSS capture, and the child
//! environment is FIXED to exactly `LANG=C`/`LC_ALL=C` (never inherited, never
//! configurable per spec).
//!
//! # Contract
//! [`CommandSpec`] validates an ABSOLUTE non-empty program, a wall-clock
//! timeout in `1 ms..=180 s`, and NONZERO per-stream caps (enforced by type).
//! [`run`] spawns the child with `env_clear()` then EXACTLY `LANG=C`/`LC_ALL=C`,
//! a null stdin, piped stdout/stderr concurrently drained (retaining a bounded
//! prefix while counting the total), the child placed in its OWN process group,
//! and the deadline polled with non-blocking `try_wait`. On a genuine deadline
//! the ENTIRE fresh child group is signaled `SIGKILL` via `rustix` (safe; no
//! `unsafe`/`libc`) and the leader is reaped. [`CommandError`]`s `Display` is
//! deterministic and bounded and NEVER echoes child output or the program path.
//!
//! A normal NONZERO exit — and a signal termination — is a successful
//! [`CommandOutcome`] (the `status` records which); only structural, capture,
//! timeout, and reap failures are [`CommandError`].
//!
//! rustix (`process`/`io`) + std, Unix-targeted, `#![forbid(unsafe_code)]`.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::io::Read;
use std::num::NonZeroU64;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

/// Minimum accepted wall-clock timeout (1 ms).
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);
/// Maximum accepted wall-clock timeout (180 s). Detect probes are read-only and
/// stay at 10 s; the higher ceiling is needed ONLY for Preflight's network +
/// evaluation probes (`nix flake prefetch` / `nix derivation show` of the pinned
/// nixpkgs flake can take minutes on a cold cache). Runaway children are still
/// killed at the validated per-spec deadline well before stalling indefinitely.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(180);

/// Fixed read-chunk used by the bounded drain.
const DRAIN_CHUNK: usize = 8 * 1024;
/// Maximum characters of any caller-controlled path/value included in a bounded
/// `Display` output, so a malicious or absurdly long path cannot bloat messages.
const SNIPPET_MAX: usize = 96;

/// Which child output stream a capture/overflow error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A validated, ready-to-execute command description: an ABSOLUTE program path,
/// its argv, and per-stream NONZERO byte caps plus a bounded wall-clock timeout.
/// The child environment is FIXED (not part of the spec): the executor applies
/// `env_clear()` then EXACTLY `LANG=C`/`LC_ALL=C`, so nothing is inherited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Absolute path to the program (argv[0]). Must be absolute and non-empty;
    /// the executor will NOT search `PATH`.
    pub program: PathBuf,
    /// Trailing argv (argv[1..]); passed verbatim, no shell, no interpolation.
    pub args: Vec<OsString>,
    /// Maximum bytes RETAINED from the child's stdout (total is counted beyond).
    pub stdout_cap: NonZeroU64,
    /// Maximum bytes RETAINED from the child's stderr (total is counted beyond).
    pub stderr_cap: NonZeroU64,
    /// Wall-clock timeout in `MIN_TIMEOUT..=MAX_TIMEOUT` (`1 ms..=180 s`).
    pub timeout: Duration,
}

impl CommandSpec {
    /// Construct a validated [`CommandSpec`]. Returns the first [`SpecError`]
    /// for a non-absolute/empty program or a timeout outside `1 ms..=180 s`; the
    /// nonzero caps are enforced by the argument types.
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        stdout_cap: NonZeroU64,
        stderr_cap: NonZeroU64,
        timeout: Duration,
    ) -> Result<Self, SpecError> {
        let spec = CommandSpec {
            program,
            args,
            stdout_cap,
            stderr_cap,
            timeout,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Validate an already-constructed spec. The program must be absolute and
    /// non-empty; the timeout must be in `MIN_TIMEOUT..=MAX_TIMEOUT`.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.program.as_os_str().is_empty() {
            return Err(SpecError::ProgramEmpty);
        }
        if !self.program.is_absolute() {
            return Err(SpecError::ProgramNotAbsolute {
                got: bound_path_snippet(&self.program),
            });
        }
        if self.timeout < MIN_TIMEOUT {
            return Err(SpecError::TimeoutTooSmall {
                got_nanos: self.timeout.as_nanos(),
            });
        }
        if self.timeout > MAX_TIMEOUT {
            return Err(SpecError::TimeoutTooLarge {
                got_nanos: self.timeout.as_nanos(),
            });
        }
        Ok(())
    }
}

/// A deterministic, length-bounded [`CommandSpec`] validation failure. No
/// variant embeds an unbounded caller value; the program path, when shown, is
/// truncated via [`bound_path_snippet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// The program path was empty.
    ProgramEmpty,
    /// The program path was not absolute.
    ProgramNotAbsolute {
        /// Bounded, lossy snippet of the offending path.
        got: String,
    },
    /// The timeout was below [`MIN_TIMEOUT`] (`< 1 ms`).
    TimeoutTooSmall {
        /// The timeout as nanoseconds.
        got_nanos: u128,
    },
    /// The timeout was above [`MAX_TIMEOUT`] (`> 180 s`).
    TimeoutTooLarge {
        /// The timeout as nanoseconds.
        got_nanos: u128,
    },
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpecError::ProgramEmpty => f.write_str("command: program path must not be empty"),
            SpecError::ProgramNotAbsolute { got } => {
                write!(f, "command: program path must be absolute, got {got:?}")
            }
            SpecError::TimeoutTooSmall { got_nanos } => write!(
                f,
                "command: timeout must be >= 1 ms ({} ns), got {got_nanos} ns",
                MIN_TIMEOUT.as_nanos()
            ),
            SpecError::TimeoutTooLarge { got_nanos } => write!(
                f,
                "command: timeout must be <= 180 s ({} ns), got {got_nanos} ns",
                MAX_TIMEOUT.as_nanos()
            ),
        }
    }
}

impl std::error::Error for SpecError {}

/// The exit status of a Unix child, split into its two DISJOINT cases. This is
/// an ENUM so the impossible "both" and "neither" states are unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// The child exited normally with the given code.
    Exited(i32),
    /// The child was killed by a signal with the given number.
    Signaled(i32),
}

impl ProbeStatus {
    /// `true` iff the child exited normally with code 0.
    #[must_use]
    pub fn is_zero_exit(self) -> bool {
        matches!(self, ProbeStatus::Exited(0))
    }
}

impl fmt::Display for ProbeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeStatus::Exited(c) => write!(f, "exit {c}"),
            ProbeStatus::Signaled(s) => write!(f, "signal {s}"),
        }
    }
}

/// The bounded result of one executed probe. `stdout` AND `stderr` are the
/// RETAINED raw byte prefixes (never decoded here — the caller decodes as
/// needed); the totals are saturating counts of every byte seen (retained +
/// discarded). A nonzero exit or a signal is a successful outcome carrying the
/// appropriate [`ProbeStatus`].
///
/// The retained `stderr` bytes are INTERNAL process evidence for a caller that
/// needs to classify a nonzero exit (e.g. a bounded Nix path-invalid diagnostic
/// that distinguishes a cache MISS from a query failure). They MUST NEVER be
/// copied into a [`crate::report::Report`], rendered Markdown/JSON, or embedded
/// in an error `Display` string — only the caller's bounded, fail-closed parser
/// may inspect them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// How the child exited (a code OR a signal; never both, never neither).
    pub status: ProbeStatus,
    /// Retained raw stdout prefix (bounded by `stdout_cap`).
    pub stdout: Vec<u8>,
    /// Retained raw stderr prefix (bounded by `stderr_cap`). Internal evidence
    /// only; never enters a report/render/error string.
    pub stderr: Vec<u8>,
    /// Total stdout bytes seen (retained + discarded), saturating.
    pub stdout_total_bytes: u64,
    /// Total stderr bytes seen (retained + discarded), saturating.
    pub stderr_total_bytes: u64,
    /// Elapsed wall time in milliseconds.
    pub wall_ms: u64,
}

impl CommandOutcome {
    /// `true` iff [`status`](Self::status) is a zero exit.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.is_zero_exit()
    }
}

/// The single structured error for the command lifecycle. Every variant has a
/// deterministic, length-bounded `Display`: I/O failures are reduced to a stable
/// [`io::ErrorKind`] token (never the platform-localized message), and NO child
/// output and NO program path is ever embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The command spec failed validation.
    Spec(SpecError),
    /// Spawning the child failed (binary missing, permission denied, ...).
    Spawn {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// A configured capture pipe was ABSENT at take-time (structural).
    MissingPipe {
        /// Which stream's configured pipe was absent.
        stream: Stream,
    },
    /// Polling the child failed.
    Poll {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// Sending the group-kill signal failed.
    Kill {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// Reaping the child failed.
    Wait {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// A capture reader panicked before returning its result.
    ReaderPanic,
    /// A capture stream read failed after mapping the error kind.
    ReaderIo {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// A stream capture exceeded its cap; a truncated prefix was retained.
    CapOverflow {
        /// Which stream overflowed.
        stream: Stream,
    },
    /// The child exceeded its wall-clock timeout (killed iff `killed`).
    Timeout {
        /// `true` iff the whole group was actually killed.
        killed: bool,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::Spec(e) => fmt::Display::fmt(e, f),
            CommandError::Spawn { kind } => {
                write!(f, "command: spawn failed ({})", kind_str(*kind))
            }
            CommandError::MissingPipe { stream } => write!(
                f,
                "command: {} capture pipe unavailable (not piped)",
                stream_str(*stream)
            ),
            CommandError::Poll { kind } => write!(f, "command: poll failed ({})", kind_str(*kind)),
            CommandError::Kill { kind } => write!(f, "command: kill failed ({})", kind_str(*kind)),
            CommandError::Wait { kind } => write!(f, "command: wait failed ({})", kind_str(*kind)),
            CommandError::ReaderPanic => f.write_str("command: capture reader panicked"),
            CommandError::ReaderIo { kind } => {
                write!(f, "command: capture read failed ({})", kind_str(*kind))
            }
            CommandError::CapOverflow { stream } => write!(
                f,
                "command: {} capture exceeded cap (retained prefix only)",
                stream_str(*stream)
            ),
            CommandError::Timeout { killed } => {
                write!(f, "command: exceeded timeout (killed={killed})")
            }
        }
    }
}

impl std::error::Error for CommandError {}

impl From<SpecError> for CommandError {
    fn from(e: SpecError) -> Self {
        CommandError::Spec(e)
    }
}

/// The injectable command-runner abstraction. The production implementation is
/// [`RealRunner`]; tests use [`FakeCommandRunner`] to script outcomes without a
/// process. Detect's [`crate::detect::ProbeRunner`] builds on top of this.
pub trait CommandRunner {
    /// Run a validated `spec` and return the captured outcome, or a structured
    /// [`CommandError`]. A nonzero exit / signal is an `Ok` outcome.
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>;
}

/// The production bounded runner: spawns the real child under the fixed
/// fail-closed environment and the validated timeout. See [`run`] for the
/// full contract.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRunner;

impl RealRunner {
    /// Construct the production runner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl CommandRunner for RealRunner {
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        run(spec)
    }
}

/// Convenience wrapper around [`RealRunner::run_probe`].
pub fn run(spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
    // 1. Validate the spec fully (absolute program; nonzero caps by type;
    //    timeout in `1 ms..=180 s`).
    spec.validate()?;

    let stdout_cap = spec.stdout_cap;
    let stderr_cap = spec.stderr_cap;
    let timeout = spec.timeout;

    // 2. Build the child: ABSOLUTE program (no PATH), explicit argv, no shell,
    //    fail-closed environment (env_clear then EXACTLY LANG=C/LC_ALL=C),
    //    null stdin, piped stdout/stderr, and its OWN process group.
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    cmd.env_clear();
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Isolate the child in its own process group (pgid == child pid). Safe std
    // API (no `unsafe`). The group is targeted at a genuine deadline.
    cmd.process_group(0);

    // 3. Wall clock + deadline start immediately BEFORE spawn.
    let start = Instant::now();

    // 4. Spawn.
    let mut child = cmd
        .spawn()
        .map_err(|e| CommandError::Spawn { kind: e.kind() })?;

    // 5. Take both capture pipes; either absent is structural → cleanup then
    //    MissingPipe (a cleanup Kill/Wait failure overrides it).
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout_pipe, stderr_pipe) = match (stdout_pipe, stderr_pipe) {
        (Some(o), Some(e)) => (o, e),
        (None, _) => {
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
    //    the poll runs on THIS thread. A Failed/Deadline poll runs the group
    //    kill BEFORE joining the readers (closing the group's pipe write-ends so
    //    the readers reach EOF). Readers are ALWAYS joined (stdout before
    //    stderr, deterministic). A cleanup Kill/Wait failure OVERRIDES the
    //    in-flight Poll/Timeout; a Deadline whose group had already raced to
    //    gone (`ESRCH`) and reaped successfully is an HONEST completion.
    let (lifecycle_res, wall_ms, stdout_join, stderr_join) = thread::scope(|s| {
        let stdout_thread = s.spawn(move || StreamCapture::drain(stdout_pipe, stdout_cap));
        let stderr_thread = s.spawn(move || StreamCapture::drain(stderr_pipe, stderr_cap));
        let poll_outcome = poll_until_deadline(&mut child, start, timeout);
        let wall_ms = poll_outcome.wall_ms;
        let lifecycle_res: Result<ExitStatus, CommandError> = match poll_outcome.event {
            PollEvent::Exited(status) => Ok(status),
            PollEvent::Failed { kind } => match cleanup_child(&mut child) {
                Err(cleanup_err) => Err(cleanup_err),
                Ok(_) => Err(CommandError::Poll { kind }),
            },
            PollEvent::Deadline => match cleanup_child(&mut child) {
                Err(cleanup_err) => Err(cleanup_err),
                Ok(GroupCleanup::SignalSent) => Err(CommandError::Timeout { killed: true }),
                Ok(GroupCleanup::AlreadyGone(status)) => Ok(status),
            },
        };
        let stdout_join = stdout_thread.join();
        let stderr_join = stderr_thread.join();
        (lifecycle_res, wall_ms, stdout_join, stderr_join)
    });

    // 7. Lifecycle errors take precedence over reader errors.
    let exit_status = lifecycle_res?;
    let stdout_capture = join_capture(stdout_join)?;
    let stderr_capture = join_capture(stderr_join)?;

    // 8. Cap overflow fails closed, BEFORE handing bytes to the caller.
    if stdout_capture.is_overflow() {
        return Err(CommandError::CapOverflow {
            stream: Stream::Stdout,
        });
    }
    if stderr_capture.is_overflow() {
        return Err(CommandError::CapOverflow {
            stream: Stream::Stderr,
        });
    }

    // 9. Map the reaped status: code OR signal, never `128 + signal`. Extract
    //    the totals BEFORE `into_bytes()` consumes the captures.
    let status = unix_status(exit_status)?;
    let stdout_total_bytes = stdout_capture.total_bytes();
    let stderr_total_bytes = stderr_capture.total_bytes();
    let stdout = stdout_capture.into_bytes();
    let stderr = stderr_capture.into_bytes();
    Ok(CommandOutcome {
        status,
        stdout,
        stderr,
        stdout_total_bytes,
        stderr_total_bytes,
        wall_ms,
    })
}

/// Outcome of a successful group-kill cleanup: the leader was reaped, tagged
/// with whether the group signal was accepted or had already raced to gone.
#[derive(Debug)]
enum GroupCleanup {
    /// `kill_process_group` returned `Ok` (signal accepted); the leader was then
    /// reaped (a signal death). The reaped status is treated as a timeout, not
    /// an honest completion, so it is NOT carried.
    SignalSent,
    /// `kill_process_group` returned `ESRCH` (group already gone), then the
    /// leader reaped by `wait` carrying this honest status.
    AlreadyGone(ExitStatus),
}

/// Signal the ENTIRE fresh child group with `SIGKILL` (via `rustix`, safe), then
/// reap the leader. A cleanup Kill/Wait failure is intended to OVERRIDE the
/// in-flight error a caller was about to surface.
fn cleanup_child(child: &mut Child) -> Result<GroupCleanup, CommandError> {
    let pgid = Pid::from_child(child);
    let signaled = match kill_process_group(pgid, Signal::KILL) {
        Ok(()) => true,
        Err(Errno::SRCH) => false,
        Err(errno) => {
            let kind = io::Error::from(errno).kind();
            let _ = child.kill();
            let _ = child.wait();
            return Err(CommandError::Kill { kind });
        }
    };
    let status = child
        .wait()
        .map_err(|e| CommandError::Wait { kind: e.kind() })?;
    if signaled {
        Ok(GroupCleanup::SignalSent)
    } else {
        Ok(GroupCleanup::AlreadyGone(status))
    }
}

/// Resolve a scoped reader-thread join into a [`StreamCapture`].
fn join_capture(
    join: thread::Result<Result<StreamCapture, CaptureError>>,
) -> Result<StreamCapture, CommandError> {
    match join {
        Ok(Ok(cap)) => Ok(cap),
        Err(_) => Err(CommandError::ReaderPanic),
        Ok(Err(kind)) => Err(CommandError::ReaderIo { kind }),
    }
}

/// Wall-clock elapsed milliseconds since `start`, saturating into `u64`.
fn elapsed_ms_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Map a reaped [`ExitStatus`] to [`ProbeStatus`]. Never `128 + signal`.
fn unix_status(status: ExitStatus) -> Result<ProbeStatus, CommandError> {
    if let Some(code) = status.code() {
        Ok(ProbeStatus::Exited(code))
    } else if let Some(signal) = status.signal() {
        Ok(ProbeStatus::Signaled(signal))
    } else {
        Err(CommandError::Wait {
            kind: io::ErrorKind::Other,
        })
    }
}

// ---------------------------------------------------------------------------
// Bounded streaming capture
// ---------------------------------------------------------------------------

/// A read error during capture, reported by stable [`io::ErrorKind`].
type CaptureError = io::ErrorKind;

/// The bounded result of capturing one child stream: the RETAINED byte prefix
/// (at most the cap) plus a saturating TOTAL and an overflow flag. Built by
/// [`StreamCapture::drain`]. Never decodes text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCapture {
    retained: Vec<u8>,
    total: u64,
    overflow: bool,
}

impl StreamCapture {
    /// Drain `reader` in fixed chunks until EOF, retaining at most `cap` bytes
    /// but KEEPING reading (and discarding, still counted) after the cap so the
    /// producer can never block on a full pipe. An `Interrupted` read is
    /// retried in place; any other read error is mapped by `kind()`.
    pub fn drain<R: Read>(mut reader: R, cap: NonZeroU64) -> Result<Self, CaptureError> {
        let cap = cap.get();
        let cap_usize = usize::try_from(cap).unwrap_or(usize::MAX);
        let mut retained: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        let mut overflow = false;
        let mut buf = [0u8; DRAIN_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total = total.saturating_add(n as u64);
                    let room = cap_usize.saturating_sub(retained.len());
                    let take = room.min(n);
                    if take > 0 {
                        retained.extend_from_slice(&buf[..take]);
                    }
                    if take < n {
                        overflow = true;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.kind()),
            }
        }
        Ok(StreamCapture {
            retained,
            total,
            overflow,
        })
    }

    /// `true` iff more bytes were pushed than the cap allows.
    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.overflow
    }

    /// Saturating total of EVERY byte ever pushed (retained + discarded).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Consume the capture and return the retained prefix.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.retained
    }
}

// ---------------------------------------------------------------------------
// Timeout polling
// ---------------------------------------------------------------------------

/// Monotonic polling interval: small enough to keep deadline enforcement
/// snappy, large enough that the per-iteration `waitpid(WNOHANG)` cost is
/// negligible.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The resolved outcome of one poll loop, tagged with the wall time elapsed.
#[derive(Debug)]
struct PollOutcome {
    wall_ms: u64,
    event: PollEvent,
}

/// What a poll loop resolved to.
#[derive(Debug)]
enum PollEvent {
    /// `try_wait` observed an [`ExitStatus`]: the child is reaped.
    Exited(ExitStatus),
    /// The deadline was reached and the final poll STILL saw the child running.
    Deadline,
    /// A `try_wait` returned `Err`; the child may still be alive.
    Failed { kind: io::ErrorKind },
}

/// Poll `child` with non-blocking `try_wait` until it exits, the
/// `start + timeout` deadline is reached, or a poll fails. Performs NO killing
/// and NO blocking `wait`; on a genuine deadline it returns [`PollEvent::Deadline`]
/// (child NOT reaped) and hands kill/reap ownership back to [`run`]. At the
/// deadline exactly ONE final `try_wait` closes the final-poll race without
/// slipping the deadline.
fn poll_until_deadline(child: &mut Child, start: Instant, timeout: Duration) -> PollOutcome {
    let deadline = start + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Exited(status),
                };
            }
            Err(e) => {
                return PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Failed { kind: e.kind() },
                };
            }
            Ok(None) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return match child.try_wait() {
                Ok(Some(status)) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Exited(status),
                },
                Err(e) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Failed { kind: e.kind() },
                },
                Ok(None) => PollOutcome {
                    wall_ms: elapsed_ms_since(start),
                    event: PollEvent::Deadline,
                },
            };
        }
        let remaining = deadline - now;
        let sleep_dur = if remaining < POLL_INTERVAL {
            remaining
        } else {
            POLL_INTERVAL
        };
        thread::sleep(sleep_dur);
    }
}

// ---------------------------------------------------------------------------
// bounded-display helpers
// ---------------------------------------------------------------------------

/// Map an [`io::ErrorKind`] to a short, stable token for bounded `Display`.
fn kind_str(k: io::ErrorKind) -> &'static str {
    match k {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::ConnectionReset => "connection_reset",
        io::ErrorKind::ConnectionAborted => "connection_aborted",
        io::ErrorKind::NotConnected => "not_connected",
        io::ErrorKind::AddrInUse => "addr_in_use",
        io::ErrorKind::AddrNotAvailable => "addr_not_available",
        io::ErrorKind::BrokenPipe => "broken_pipe",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::WouldBlock => "would_block",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::OutOfMemory => "out_of_memory",
        io::ErrorKind::Other => "other",
        _ => "other",
    }
}

fn stream_str(s: Stream) -> &'static str {
    match s {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

/// Truncate a caller-controlled string for safe, bounded inclusion in `Display`.
fn bound_str(s: &str) -> String {
    if s.len() <= SNIPPET_MAX {
        return s.to_string();
    }
    let mut end = SNIPPET_MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&s[..end]);
    out.push_str("...");
    out
}

fn bound_path_snippet(path: &Path) -> String {
    bound_str(&path.to_string_lossy())
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub(crate) struct FakeCommandRunner {
    /// Scripted outcomes keyed by EXACT `(program, argv)`. A probe whose
    /// `(program, argv)` is absent yields a synthetic `Spawn { NotFound }`
    /// (mirroring a missing binary), so unmapped probes behave like a real
    /// missing tool. Exact-argv matching is required so a single binary (e.g.
    /// the Nix executable) driven with MANY distinct subcommands can be
    /// scripted independently.
    scripts: Vec<(PathBuf, Vec<OsString>, Result<CommandOutcome, CommandError>)>,
}

#[cfg(test)]
impl FakeCommandRunner {
    pub(crate) fn new() -> Self {
        Self {
            scripts: Vec::new(),
        }
    }
    /// Script the outcome for a probe with EXACTLY this `(program, argv)`. A
    /// repeated script for the same `(program, argv)` OVERRIDES the prior one.
    pub(crate) fn set(
        &mut self,
        program: &Path,
        args: &[&str],
        result: Result<CommandOutcome, CommandError>,
    ) {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        for entry in &mut self.scripts {
            if entry.0 == program && entry.1 == args {
                entry.2 = result;
                return;
            }
        }
        self.scripts.push((program.to_path_buf(), args, result));
    }
    /// Script the outcome for a probe whose `(program, argv)` equals `spec`'s.
    /// Convenience over [`Self::set`] for callers that already hold a
    /// [`CommandSpec`] (the Preflight argv tests build specs via the
    /// `pub(crate)` spec builders and script them here).
    pub(crate) fn set_spec(
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
impl CommandRunner for FakeCommandRunner {
    fn run_probe(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        // Still validate the spec so fake-driven tests exercise validation too.
        spec.validate()?;
        for (program, args, result) in &self.scripts {
            if program == &spec.program && args == &spec.args {
                return result.clone();
            }
        }
        Err(CommandError::Spawn {
            kind: io::ErrorKind::NotFound,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn nz(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("test cap must be nonzero")
    }

    fn valid_spec(program: &str) -> CommandSpec {
        CommandSpec::new(
            PathBuf::from(program),
            vec![OsString::from("arg")],
            nz(1024),
            nz(1024),
            Duration::from_secs(1),
        )
        .expect("valid spec")
    }

    // ---- spec validation ----------------------------------------------------

    #[test]
    fn accepts_absolute_program_and_in_range_timeout() {
        let s = valid_spec("/bin/echo");
        assert_eq!(s.program, PathBuf::from("/bin/echo"));
        assert!(s.validate().is_ok());
    }

    #[test]
    fn accepts_timeout_boundaries_inclusive() {
        let mk =
            |t: Duration| CommandSpec::new(PathBuf::from("/bin/echo"), vec![], nz(1), nz(1), t);
        assert!(mk(MIN_TIMEOUT).is_ok());
        assert!(mk(MAX_TIMEOUT).is_ok());
    }

    #[test]
    fn rejects_empty_program() {
        let err = CommandSpec::new(
            PathBuf::from(""),
            vec![],
            nz(1),
            nz(1),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(err, SpecError::ProgramEmpty);
    }

    #[test]
    fn rejects_relative_program() {
        for bad in ["echo", "./echo", "bin/echo", "../echo", "foo/bar", "."] {
            let err = CommandSpec::new(
                PathBuf::from(bad),
                vec![],
                nz(1),
                nz(1),
                Duration::from_secs(1),
            )
            .unwrap_err();
            assert!(
                matches!(err, SpecError::ProgramNotAbsolute { .. }),
                "expected ProgramNotAbsolute for {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_timeout_out_of_range() {
        for bad in [Duration::ZERO, Duration::from_nanos(999_999)] {
            let err = CommandSpec::new(PathBuf::from("/bin/echo"), vec![], nz(1), nz(1), bad)
                .unwrap_err();
            assert_eq!(
                err,
                SpecError::TimeoutTooSmall {
                    got_nanos: bad.as_nanos()
                }
            );
        }
        let too_big = MAX_TIMEOUT + Duration::from_nanos(1);
        let err = CommandSpec::new(PathBuf::from("/bin/echo"), vec![], nz(1), nz(1), too_big)
            .unwrap_err();
        assert_eq!(
            err,
            SpecError::TimeoutTooLarge {
                got_nanos: too_big.as_nanos()
            }
        );
    }

    #[test]
    fn nonzero_caps_type_enforced() {
        assert!(NonZeroU64::new(0).is_none());
    }

    // ---- StreamCapture ------------------------------------------------------

    #[test]
    fn drain_exact_cap_no_overflow() {
        let feed = b"hello, capture!";
        let c = StreamCapture::drain(Cursor::new(&feed[..]), nz(feed.len() as u64)).unwrap();
        assert_eq!(c.into_bytes(), feed);
    }

    #[test]
    fn drain_empty_is_empty_not_overflow() {
        let c = StreamCapture::drain(Cursor::new(&b""[..]), nz(8)).unwrap();
        assert!(!c.is_overflow());
        assert_eq!(c.total_bytes(), 0);
    }

    #[test]
    fn drain_overflow_retains_prefix_keeps_total() {
        let feed: Vec<u8> = (0..250u8).collect();
        let c = StreamCapture::drain(Cursor::new(&feed[..]), nz(100)).unwrap();
        assert_eq!(c.total_bytes(), 250);
        assert!(c.is_overflow());
        assert_eq!(c.into_bytes(), &feed[..100]);
    }

    #[test]
    fn drain_retries_interrupted() {
        // A reader that fails Interrupted twice then yields the feed then EOF.
        let feed = b"after-eintr";
        let c = StreamCapture::drain(InterruptedReader::new(feed, 2), nz(64)).unwrap();
        assert_eq!(c.total_bytes(), feed.len() as u64);
        assert_eq!(c.into_bytes(), feed);
    }

    #[test]
    fn drain_maps_read_error_by_kind() {
        let err = StreamCapture::drain(
            FailingReader {
                kind: io::ErrorKind::BrokenPipe,
            },
            nz(64),
        )
        .unwrap_err();
        assert_eq!(err, io::ErrorKind::BrokenPipe);
    }

    struct InterruptedReader<'a> {
        data: &'a [u8],
        pos: usize,
        left: u32,
    }
    impl<'a> InterruptedReader<'a> {
        fn new(data: &'a [u8], n: u32) -> Self {
            Self {
                data,
                pos: 0,
                left: n,
            }
        }
    }
    impl Read for InterruptedReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.left > 0 {
                self.left -= 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "x"));
            }
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = buf.len().min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    struct FailingReader {
        kind: io::ErrorKind,
    }
    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "x"))
        }
    }

    // ---- ProbeStatus / outcome --------------------------------------------

    #[test]
    fn probe_status_success_only_zero_exit() {
        assert!(ProbeStatus::Exited(0).is_zero_exit());
        assert!(!ProbeStatus::Exited(1).is_zero_exit());
        assert!(!ProbeStatus::Signaled(9).is_zero_exit());
        let out = CommandOutcome {
            status: ProbeStatus::Exited(3),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            wall_ms: 1,
        };
        // Nonzero exit is an outcome (Ok), not an executor error.
        assert!(!out.is_success());
    }

    // ---- FakeCommandRunner outcome/error mapping ---------------------------

    #[test]
    fn fake_runner_returns_scripted_success_and_nonzero_outcomes() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            Path::new("/bin/true"),
            &["arg"],
            Ok(CommandOutcome {
                status: ProbeStatus::Exited(0),
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
                stdout_total_bytes: 2,
                stderr_total_bytes: 0,
                wall_ms: 5,
            }),
        );
        fake.set(
            Path::new("/bin/false"),
            &["arg"],
            Ok(CommandOutcome {
                status: ProbeStatus::Exited(1),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
                wall_ms: 4,
            }),
        );
        let ok = fake.run_probe(&valid_spec("/bin/true")).unwrap();
        assert!(ok.is_success());
        assert_eq!(ok.stdout, b"ok");
        // Nonzero exit is an Ok outcome, not an executor error.
        let nz = fake.run_probe(&valid_spec("/bin/false")).unwrap();
        assert!(!nz.is_success());
    }

    #[test]
    fn fake_runner_returns_scripted_cap_and_timeout_errors() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            Path::new("/chatty"),
            &["arg"],
            Err(CommandError::CapOverflow {
                stream: Stream::Stdout,
            }),
        );
        fake.set(
            Path::new("/slow"),
            &["arg"],
            Err(CommandError::Timeout { killed: true }),
        );
        let err = fake.run_probe(&valid_spec("/chatty")).unwrap_err();
        assert_eq!(
            err,
            CommandError::CapOverflow {
                stream: Stream::Stdout
            }
        );
        let err = fake.run_probe(&valid_spec("/slow")).unwrap_err();
        assert_eq!(err, CommandError::Timeout { killed: true });
    }

    #[test]
    fn fake_runner_unmapped_program_is_not_found_spawn_error() {
        let fake = FakeCommandRunner::new();
        let err = fake.run_probe(&valid_spec("/nope")).unwrap_err();
        assert!(matches!(
            err,
            CommandError::Spawn {
                kind: io::ErrorKind::NotFound
            }
        ));
    }

    // ---- bounded display ----------------------------------------------------

    #[test]
    fn spec_error_display_bounded_for_huge_path() {
        let path = PathBuf::from(format!("x{}", "Y".repeat(50_000)));
        let err = CommandSpec::new(path, vec![], nz(1), nz(1), Duration::from_secs(1)).unwrap_err();
        let s = err.to_string();
        assert!(s.len() <= SNIPPET_MAX + 80, "was {}: {s:?}", s.len());
        assert!(s.contains("must be absolute"));
        assert!(s.contains("..."));
    }

    #[test]
    fn command_error_display_bounded_and_deterministic_no_output_no_path() {
        // I/O variants map a kind to a stable token; no platform text leaks and
        // no child output/path is ever embedded.
        for (kind, tok) in [
            (io::ErrorKind::NotFound, "not_found"),
            (io::ErrorKind::BrokenPipe, "broken_pipe"),
            (io::ErrorKind::TimedOut, "timed_out"),
            (io::ErrorKind::Other, "other"),
        ] {
            let s = CommandError::Spawn { kind }.to_string();
            assert!(s.contains(tok));
            assert!(s.contains("spawn failed"));
        }
        assert_eq!(
            CommandError::ReaderPanic.to_string(),
            "command: capture reader panicked"
        );
        assert!(
            CommandError::Timeout { killed: true }
                .to_string()
                .contains("killed=true")
        );
        assert!(
            CommandError::CapOverflow {
                stream: Stream::Stdout
            }
            .to_string()
            .contains("stdout")
        );
        // No child output or program path is ever embedded in any variant.
        // The path is intentionally NON-absolute so validation fails and yields
        // a `ProgramNotAbsolute` SpecError (-> CommandError::Spec).
        let path = PathBuf::from(format!("very/long/{}", "Z".repeat(60_000)));
        let cerr: CommandError =
            CommandSpec::new(path, vec![], nz(1), nz(1), Duration::from_secs(1))
                .unwrap_err()
                .into();
        let s = cerr.to_string();
        assert!(s.len() <= SNIPPET_MAX + 80, "too long ({}): {s:?}", s.len());
    }

    #[test]
    fn from_impls_wire_spec_error() {
        let se: CommandError = SpecError::ProgramEmpty.into();
        assert!(matches!(se, CommandError::Spec(SpecError::ProgramEmpty)));
    }

    // ---- stderr retention + raised timeout ceiling ------------------------

    #[test]
    fn outcome_carries_retained_stderr_bytes_and_total() {
        // stderr is now RETAINED (bounded prefix) alongside its saturating
        // total, mirroring stdout. Both are internal evidence and never reach a
        // report; here we only assert the struct round-trips them.
        let out = CommandOutcome {
            status: ProbeStatus::Exited(0),
            stdout: b"out".to_vec(),
            stderr: b"err".to_vec(),
            stdout_total_bytes: 3,
            stderr_total_bytes: 3,
            wall_ms: 9,
        };
        assert!(out.is_success());
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.stdout_total_bytes, 3);
        assert_eq!(out.stderr_total_bytes, 3);
    }

    #[test]
    fn max_timeout_raised_to_180s_for_preflight_probes() {
        // The ceiling was raised from 30 s to 180 s ONLY so Preflight's
        // network/evaluation probes fit; Detect stays at 10 s. The boundary is
        // still inclusive.
        assert_eq!(MAX_TIMEOUT, Duration::from_secs(180));
        assert!(
            CommandSpec::new(
                PathBuf::from("/bin/echo"),
                vec![],
                nz(1),
                nz(1),
                Duration::from_secs(180),
            )
            .is_ok()
        );
        // 181 s is rejected.
        assert!(
            CommandSpec::new(
                PathBuf::from("/bin/echo"),
                vec![],
                nz(1),
                nz(1),
                Duration::from_secs(181),
            )
            .is_err()
        );
        // The bounded Display names the new ceiling, not the stale 30 s.
        let s = SpecError::TimeoutTooLarge { got_nanos: 0 }.to_string();
        assert!(s.contains("180 s"));
        assert!(!s.contains("30 s"));
    }
}
