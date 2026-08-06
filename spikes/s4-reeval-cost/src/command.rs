//! Spike S4 (PR-6 / DR-004) — COMMAND slice: the validated command *spec*, the
//! bounded streaming *capture* primitive, and the structured outcome/error types
//! the runner will populate — WITHOUT any child spawning, polling, killing,
//! waiting, timing, or RSS capture yet.
//!
//! This module is the type/validation/capture foundation for the runner. It is
//! deliberately split from execution so that the contract — what a command is,
//! how captured bytes are bounded and accounted, and how every failure is
//! reported — can be unit-tested in isolation with NO process, NO thread, and
//! NO I/O beyond a plain [`std::io::Read`].
//!
//! What this module owns in THIS task:
//!   * [`TimeFlavor`] — selects the `/usr/bin/time` dialect (`-l` macOS/BSD vs
//!     `-v` GNU) the runner will later parse `max-rss` from.
//!   * [`CommandSpec`] — an ABSOLUTE program path + args + an explicit COMPLETE
//!     environment (applied by the runner via `Command::env_clear()` followed by
//!     exactly these entries, so NOTHING is inherited from the parent process —
//!     fail-closed) + per-stream nonzero byte caps + a bounded wall-clock
//!     timeout. [`CommandSpec::new`] / [`CommandSpec::validate`] reject a
//!     non-absolute or empty program and a timeout outside `1 ms..=1 h`, with
//!     deterministic, length-bounded errors. The nonzero caps are enforced at
//!     the type boundary ([`std::num::NonZeroU64`]), so a zero cap is
//!     unrepresentable — the same philosophy as [`ByteCap`].
//!   * [`StreamCapture`] + [`StreamCapture::drain`] — the capture primitive:
//!     [`drain`](StreamCapture::drain) reads a [`std::io::Read`] in fixed chunks
//!     until EOF into a [`ByteCap`] (retaining at most the cap, KEEPING
//!     reading/discarding after overflow so the producer never blocks on a full
//!     pipe), retries an [`io::ErrorKind::Interrupted`] read in place rather
//!     than surfacing it, maps any other [`io::Error`] to a [`CaptureError`] by
//!     `kind()` only, and NEVER decodes text. The result carries the retained
//!     byte prefix and a [`Stats`] total-accounting snapshot.
//!   * [`UnixStatus`] — the exited/signaled split of a Unix child exit. It is an
//!     ENUM, so the "both a code and a signal" and "neither" states are
//!     unrepresentable; exactly one case always holds.
//!   * [`CommandOutcome`] — the field set the runner fills in later (status,
//!     retained stdout, cleaned stderr, per-stream totals, wall-ms, REQUIRED
//!     max-rss). There is deliberately NO stored `success` field — it is derived
//!     from `status` via [`CommandOutcome::is_success`] so the two can never
//!     disagree. `max_rss_kib` is a required `u64`: an outcome is produced only
//!     after the `/usr/bin/time` metric was captured and parsed, and an RSS
//!     failure is reported as [`CommandError::Rss`], never as a missing field.
//!   * [`CommandError`] — the single structured error for the whole lifecycle
//!     (invalid spec / spawn / missing pipe / poll / kill / wait / reader panic
//!     or IO / cap overflow / UTF-8 / RSS / timeout). Its [`std::fmt::Display`]
//!     is deterministic and bounded; later tasks construct variants as they wire
//!     up spawning, but the contract is fixed here. [`CommandError::MissingPipe`]
//!     models a configured pipe that was absent at take-time (a structural
//!     misconfiguration) and identifies the affected [`Stream`] rather than
//!     inventing an [`io::ErrorKind`].
//!
//! What this module does NOT do (later tasks): spawn, fork/exec, pipes, signal
//! handling, timeout enforcement, `/usr/bin/time` invocation/parsing, RSS
//! capture, stderr cleaning, or any I/O other than reading a capture stream.
//!
//! std-only, Unix-targeted. `#![forbid(unsafe_code)]` is inherited from the
//! crate root.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::io::Read;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::caps::{ByteCap, Stats};

/// Minimum accepted wall-clock timeout (1 ms). Below this a child cannot be
/// reliably measured and is almost certainly a misconfiguration.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);
/// Maximum accepted wall-clock timeout (1 h). Above this the harness would stall
/// unreasonably; runaway children are killed well before this.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Fixed read-chunk size used by [`StreamCapture::drain`]. Small enough to stay
/// on the stack and keep capture memory flat, large enough that a typical pipe
/// fill costs negligible per-read overhead.
const DRAIN_CHUNK: usize = 8 * 1024;

/// Maximum bytes of any caller-controlled path/value included in a bounded
/// `Display` output, so a malicious or absurdly long program path cannot bloat
/// error messages, logs, or reports.
const SNIPPET_MAX: usize = 160;

// ---------------------------------------------------------------------------
// TimeFlavor
// ---------------------------------------------------------------------------

/// The `/usr/bin/time` dialect the runner will later parse maximum-RSS from.
///
/// * [`TimeFlavor::MacOs`] — Darwin/BSD `time -l`, which prints a numbered
///   field list; the RSS field (`maximum resident set size`) is reported in
///   BYTES.
/// * [`TimeFlavor::Gnu`] — GNU `time -v`, which prints a labelled block; the
///   RSS line (`Maximum resident set size (kbytes)`) is reported in KiB.
///
/// [`CommandOutcome::max_rss_kib`] is normalized to KiB regardless of dialect;
/// the dialect only decides how the raw value is read and scaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFlavor {
    /// Darwin/BSD `time -l` (RSS reported in bytes).
    MacOs,
    /// GNU `time -v` (RSS reported in KiB).
    Gnu,
}

impl TimeFlavor {
    /// A short, stable lowercase label (`"macos"` / `"gnu"`) suitable for
    /// machine-readable reporting. Deterministic and allocation-free.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TimeFlavor::MacOs => "macos",
            TimeFlavor::Gnu => "gnu",
        }
    }
}

impl fmt::Display for TimeFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// Which child output stream a capture/decode error refers to. Used by the
/// structured [`CommandError`] variants so a failure names the stream without
/// embedding caller bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

// ---------------------------------------------------------------------------
// CommandSpec + SpecError
// ---------------------------------------------------------------------------

/// A validated, ready-to-execute command description: an ABSOLUTE program path,
/// its argv, an explicit COMPLETE environment (the runner applies it via
/// `Command::env_clear()` followed by EXACTLY these entries, so the child sees
/// nothing inherited from the parent process — fail-closed), per-stream NONZERO
/// byte caps, and a bounded wall-clock timeout.
///
/// Construct with [`CommandSpec::new`] (which validates) or build a value and
/// call [`CommandSpec::validate`]. The caps are [`NonZeroU64`] by type, so a
/// zero cap is unrepresentable — there is no "cap = 0" validation case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Absolute path to the program to execute (e.g. `/bin/echo`). Must be
    /// absolute and non-empty; the runner will NOT search `PATH`.
    pub program: PathBuf,
    /// Trailing argv (argv[1..]); `program` is argv[0]. Passed verbatim, no
    /// shell, no interpolation.
    pub args: Vec<OsString>,
    /// The COMPLETE child environment, applied by the runner via
    /// `Command::env_clear()` followed by these entries. Nothing is inherited
    /// from the parent process: the child sees EXACTLY these entries and no
    /// others. A [`BTreeMap`] so iteration is deterministic for diagnostics.
    /// Keys and values are passed verbatim.
    pub env: BTreeMap<OsString, OsString>,
    /// Maximum bytes RETAINED from the child's stdout (total is counted beyond).
    pub stdout_cap: NonZeroU64,
    /// Maximum bytes RETAINED from the child's stderr (total is counted beyond).
    pub stderr_cap: NonZeroU64,
    /// Wall-clock timeout in `MIN_TIMEOUT..=MAX_TIMEOUT` (`1 ms..=1 h`).
    pub timeout: Duration,
}

impl CommandSpec {
    /// Construct a validated [`CommandSpec`]. Returns the first [`SpecError`]
    /// for a non-absolute/empty program or a timeout outside `1 ms..=1 h`; the
    /// nonzero caps are enforced by the argument types. The `env` argument is
    /// the COMPLETE child environment (applied after `env_clear()`).
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        env: BTreeMap<OsString, OsString>,
        stdout_cap: NonZeroU64,
        stderr_cap: NonZeroU64,
        timeout: Duration,
    ) -> Result<Self, SpecError> {
        let spec = CommandSpec {
            program,
            args,
            env,
            stdout_cap,
            stderr_cap,
            timeout,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Validate an already-constructed spec in place. The program must be
    /// absolute and non-empty; the timeout must be in
    /// `MIN_TIMEOUT..=MAX_TIMEOUT`.
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
    /// The program path was not absolute (the runner will not search `PATH`).
    ProgramNotAbsolute {
        /// Bounded, lossy snippet of the offending path.
        got: String,
    },
    /// The timeout was below [`MIN_TIMEOUT`] (`< 1 ms`).
    TimeoutTooSmall {
        /// The timeout as nanoseconds.
        got_nanos: u128,
    },
    /// The timeout was above [`MAX_TIMEOUT`] (`> 1 h`).
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
                "command: timeout must be <= 1 h ({} ns), got {got_nanos} ns",
                MAX_TIMEOUT.as_nanos()
            ),
        }
    }
}

impl std::error::Error for SpecError {}

// ---------------------------------------------------------------------------
// CaptureError + StreamCapture
// ---------------------------------------------------------------------------

/// A failure from [`StreamCapture::drain`]. The only mode is a read error,
/// reported by [`io::ErrorKind`] (deterministic) rather than the full
/// [`io::Error`] (whose message is platform- and locale-dependent and thus
/// neither bounded nor deterministic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    /// A read from the capture stream returned an error; `kind` is the mapped
    /// [`io::Error::kind`].
    Read {
        /// The stable, mapped I/O error kind.
        kind: io::ErrorKind,
    },
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Read { kind } => {
                write!(f, "capture: read failed ({})", kind_str(*kind))
            }
        }
    }
}

impl std::error::Error for CaptureError {}

/// The bounded result of capturing one child stream: the RETAINED byte prefix
/// (at most the cap) plus a [`Stats`] snapshot giving the TOTAL accounting
/// (bytes seen, retained, discarded, and the overflow/saturation flags).
///
/// Built by [`StreamCapture::drain`]. Never decodes text — the bytes are raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCapture {
    /// The retained prefix (`len <= cap`); raw bytes, never decoded.
    retained: Vec<u8>,
    /// Total-accounting snapshot (cap/retained/total/discarded/flags).
    stats: Stats,
}

impl StreamCapture {
    /// Drain `reader` into a bounded collector, reading fixed `DRAIN_CHUNK`
    /// chunks until EOF. Retains at most `cap` bytes but KEEPS reading (and
    /// discarding, still counted) after the cap is hit, so the producer can
    /// never block on a full buffer and a partial capture remains for
    /// diagnostics. An [`io::ErrorKind::Interrupted`] read is RETRIED in place
    /// (the loop continues draining) rather than surfaced, matching `Read`'s
    /// contract that an `EINTR` may be retried by the caller. Any other read
    /// error is mapped — by [`io::Error::kind`] only — to
    /// [`CaptureError::Read`]; no text is ever decoded.
    ///
    /// The returned [`Stats`] is a pure function of `(cap, bytes pushed)` and is
    /// independent of how the reader chunked the feed.
    pub fn drain<R: Read>(mut reader: R, cap: NonZeroU64) -> Result<Self, CaptureError> {
        let mut collector = ByteCap::new(cap);
        let mut buf = [0u8; DRAIN_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => collector.push(&buf[..n]),
                // EINTR is retried: keep draining instead of failing. Bytes
                // already captured/ counted are preserved across the retry.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(CaptureError::Read { kind: e.kind() }),
            }
        }
        let stats = collector.stats();
        let retained = collector.into_bytes();
        Ok(StreamCapture { retained, stats })
    }

    /// The retained byte prefix (`len <= cap`), raw and never decoded.
    #[must_use]
    pub fn retained(&self) -> &[u8] {
        &self.retained
    }

    /// The deterministic total-accounting snapshot.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The configured retention cap (always `> 0`).
    #[must_use]
    pub fn cap(&self) -> u64 {
        self.stats.cap
    }

    /// Saturating total of EVERY byte ever pushed (retained + discarded).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.stats.total
    }

    /// `true` iff more bytes were pushed than the cap allows (bytes discarded).
    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.stats.cap_exceeded
    }

    /// Consume the capture and return the retained prefix.
    #[must_use]
    pub fn into_retained(self) -> Vec<u8> {
        self.retained
    }
}

// ---------------------------------------------------------------------------
// UnixStatus + CommandOutcome
// ---------------------------------------------------------------------------

/// The exit status of a Unix child, split into its two DISJOINT cases: a normal
/// exit with a code, or termination by a signal. This is an ENUM, so the
/// impossible "both a code and a signal" and "neither" states are
/// UNREPRESENTABLE — exactly one case always holds. (A not-yet-reaped child is
/// handled transiently by the runner and is never stored in this type.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixStatus {
    /// The child exited normally (`WIFEXITED`) with the given exit code.
    Exited(i32),
    /// The child was killed by a signal (`WIFSIGNALED`) with the given number.
    Signaled(i32),
}

impl UnixStatus {
    /// `true` iff the child exited normally with code 0 (and was not signaled).
    /// The only success case is [`UnixStatus::Exited`] with a zero code.
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, UnixStatus::Exited(0))
    }
}

impl fmt::Display for UnixStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnixStatus::Exited(c) => write!(f, "exit {c}"),
            UnixStatus::Signaled(s) => write!(f, "signal {s}"),
        }
    }
}

/// The full result of one executed command, as the runner will populate it
/// later.
///
/// `stdout` is the RETAINED raw byte prefix (never decoded here — the caller
/// decodes as needed, e.g. as JSON); `cleaned_stderr` is already decoded +
/// cleaned for diagnostics. `stdout_total_bytes` / `stderr_total_bytes` are the
/// saturating totals (bytes seen, including discarded overflow), so a consumer
/// can tell a capped run from a complete one.
///
/// There is deliberately NO stored `success` flag: success is derived from
/// [`status`](Self::status) via [`is_success`](Self::is_success) so a stored
/// bool could never disagree with the status. `max_rss_kib` is a REQUIRED `u64`:
/// an outcome is produced only after the `/usr/bin/time` metric was captured and
/// parsed, so an RSS failure surfaces as [`CommandError::Rss`] — never as a
/// missing `None` field on a successful-looking outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// How the child exited (a code OR a signal; never both, never neither).
    pub status: UnixStatus,
    /// Retained raw stdout prefix (bounded by `stdout_cap`).
    pub stdout: Vec<u8>,
    /// Cleaned, decoded stderr for human/machine diagnostics.
    pub cleaned_stderr: String,
    /// Total stdout bytes seen (retained + discarded), saturating.
    pub stdout_total_bytes: u64,
    /// Total stderr bytes seen (retained + discarded), saturating.
    pub stderr_total_bytes: u64,
    /// Elapsed wall time in milliseconds.
    pub wall_ms: u64,
    /// Maximum resident set size in KiB. REQUIRED: an outcome exists only after
    /// the `/usr/bin/time` output was captured and parsed; an RSS capture/parse
    /// failure is reported as [`CommandError::Rss`], not as a missing field.
    pub max_rss_kib: u64,
}

impl CommandOutcome {
    /// `true` iff [`status`](Self::status) is a successful normal exit (code 0).
    /// This is DERIVED from the status so it can never disagree with it.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

// ---------------------------------------------------------------------------
// CommandError
// ---------------------------------------------------------------------------

/// The single structured error for the entire command lifecycle. Every variant
/// has a deterministic, length-bounded [`fmt::Display`]: I/O failures are
/// reduced to a stable [`io::ErrorKind`] token (never the platform's localized
/// [`io::Error`] message), and no caller byte payload is embedded.
///
/// Later tasks (spawn/poll/kill/wait/timeout/RSS) construct the variants as the
/// execution path is wired up; the contract is fixed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The command spec failed validation.
    Spec(SpecError),
    /// Spawning the child failed (binary missing, permission denied, ...).
    Spawn {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// A configured capture pipe was ABSENT at take-time. The runner configures
    /// `std::process::Command` to pipe `stdout`/`stderr`, then takes the
    /// resulting `ChildStdout`/`ChildStderr` via the `Option` that `Child`
    /// yields. A `None` there means the stream was not actually piped despite
    /// being configured for capture — a structural misconfiguration, NOT an
    /// I/O error — so this variant identifies the affected [`Stream`] rather
    /// than inventing an [`io::ErrorKind`].
    MissingPipe {
        /// Which stream's configured pipe was absent when taken.
        stream: Stream,
    },
    /// Polling the child or a pipe-readiness descriptor failed.
    Poll {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// Sending a termination/kill signal to the child failed.
    Kill {
        /// Mapped I/O error kind.
        kind: io::ErrorKind,
    },
    /// Reaping the child (`waitpid`) failed.
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
    /// A captured stream could not be decoded as the required output encoding.
    Utf8 {
        /// Which stream failed to decode.
        stream: Stream,
    },
    /// Maximum-RSS could not be captured or parsed.
    Rss,
    /// The child exceeded its wall-clock timeout (and was killed iff `killed`).
    Timeout {
        /// `true` iff the child was actually killed (vs. kill failed/noted).
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
            CommandError::Utf8 { stream } => {
                write!(
                    f,
                    "command: {} decode failed (invalid UTF-8)",
                    stream_str(*stream)
                )
            }
            CommandError::Rss => f.write_str("command: maximum-RSS capture failed"),
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

impl From<CaptureError> for CommandError {
    fn from(e: CaptureError) -> Self {
        match e {
            CaptureError::Read { kind } => CommandError::ReaderIo { kind },
        }
    }
}

// ---------------------------------------------------------------------------
// bounded-display helpers (deterministic, allocation-bounded)
// ---------------------------------------------------------------------------

/// Map an [`io::ErrorKind`] to a short, stable, allocation-free token for use
/// in bounded `Display` output. Only the long-standing, universally-available
/// kinds are named; everything else collapses to `"other"` (still deterministic).
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

/// Map a [`Stream`] to a lowercase token for bounded `Display`.
fn stream_str(s: Stream) -> &'static str {
    match s {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

/// Truncate a caller-controlled string for safe, bounded inclusion in a
/// `Display` output. Slices on a UTF-8 boundary and appends `...` if truncated.
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

/// Bounded, lossy snippet of a path for error messages. [`Path::to_string_lossy`]
/// replaces any non-UTF-8 tail with U+FFFD (deterministic) before truncation.
fn bound_path_snippet(path: &Path) -> String {
    bound_str(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a nonzero cap value, panicking if `n == 0` (a test bug, not a
    /// unit-under-test bug).
    fn nz(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("test cap must be nonzero")
    }

    /// A known-good spec centered on `program`, used by the positive cases.
    fn valid_spec(program: &str) -> CommandSpec {
        CommandSpec::new(
            PathBuf::from(program),
            vec![OsString::from("arg")],
            BTreeMap::new(),
            nz(1024),
            nz(1024),
            Duration::from_secs(1),
        )
        .expect("valid spec")
    }

    // --- spec validation -----------------------------------------------------

    #[test]
    fn accepts_absolute_program_and_in_range_timeout() {
        let s = valid_spec("/bin/echo");
        assert_eq!(s.program, PathBuf::from("/bin/echo"));
        assert!(s.validate().is_ok());
    }

    #[test]
    fn accepts_timeout_boundaries_inclusive() {
        let mk = |t: Duration| {
            CommandSpec::new(
                PathBuf::from("/bin/echo"),
                vec![],
                BTreeMap::new(),
                nz(1),
                nz(1),
                t,
            )
        };
        assert!(mk(MIN_TIMEOUT).is_ok());
        assert!(mk(MAX_TIMEOUT).is_ok());
        // A hair below/above the floor is rejected/accepted respectively.
        assert!(mk(MIN_TIMEOUT + Duration::from_nanos(1)).is_ok());
    }

    #[test]
    fn rejects_empty_program() {
        let err = CommandSpec::new(
            PathBuf::from(""),
            vec![],
            BTreeMap::new(),
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
                BTreeMap::new(),
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
    fn rejects_timeout_below_min() {
        for bad in [
            Duration::ZERO,
            Duration::from_nanos(999_999),
            Duration::from_micros(500),
        ] {
            let err = CommandSpec::new(
                PathBuf::from("/bin/echo"),
                vec![],
                BTreeMap::new(),
                nz(1),
                nz(1),
                bad,
            )
            .unwrap_err();
            assert_eq!(
                err,
                SpecError::TimeoutTooSmall {
                    got_nanos: bad.as_nanos()
                },
                "expected TimeoutTooSmall for {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_timeout_above_max() {
        let too_big = MAX_TIMEOUT + Duration::from_nanos(1);
        let err = CommandSpec::new(
            PathBuf::from("/bin/echo"),
            vec![],
            BTreeMap::new(),
            nz(1),
            nz(1),
            too_big,
        )
        .unwrap_err();
        assert_eq!(
            err,
            SpecError::TimeoutTooLarge {
                got_nanos: too_big.as_nanos()
            }
        );
    }

    #[test]
    fn nonzero_caps_are_type_enforced_not_validated() {
        // A zero cap cannot be expressed at all; the constructor only ever sees
        // nonzero values. Sanity: NonZeroU64 rejects 0.
        assert!(NonZeroU64::new(0).is_none());
        // And an otherwise-valid spec with nonzero caps is accepted.
        assert!(valid_spec("/bin/true").validate().is_ok());
    }

    #[test]
    fn command_spec_env_is_the_complete_fail_closed_environment() {
        // The `env` field is the COMPLETE child environment the runner applies
        // via Command::env_clear() + these entries; nothing is inherited. The
        // field is named `env` (not an "overlay"/"explicit_env") so the
        // fail-closed contract is unmistakable at the type boundary.
        let mut env = BTreeMap::new();
        env.insert(OsString::from("PATH"), OsString::from("/usr/bin:/bin"));
        env.insert(OsString::from("RUST_BACKTRACE"), OsString::from("0"));
        let s = CommandSpec::new(
            PathBuf::from("/bin/echo"),
            vec![],
            env.clone(),
            nz(16),
            nz(16),
            Duration::from_secs(1),
        )
        .expect("valid spec");
        assert_eq!(s.env, env);
        // An empty complete environment is a legitimate (if austere) fail-closed
        // configuration: the child sees nothing at all.
        let empty = valid_spec("/bin/true");
        assert!(empty.env.is_empty());
    }

    // --- drain: exact / empty ------------------------------------------------

    #[test]
    fn drain_exact_cap_retains_all_no_overflow() {
        let feed = b"hello, capture!"; // 14 bytes
        let cap = nz(feed.len() as u64);
        let c = StreamCapture::drain(Cursor::new(&feed[..]), cap).unwrap();
        assert_eq!(c.retained(), feed);
        assert_eq!(c.total_bytes(), feed.len() as u64);
        assert!(!c.is_overflow());
        assert_eq!(c.cap(), feed.len() as u64);
        let s = c.stats();
        assert_eq!(s.retained, feed.len() as u64);
        assert_eq!(s.discarded, 0);
        assert!(!s.cap_exceeded);
    }

    #[test]
    fn drain_empty_reader_is_empty_not_overflow() {
        let c = StreamCapture::drain(Cursor::new(&b""[..]), nz(8)).unwrap();
        assert!(c.retained().is_empty());
        assert_eq!(c.total_bytes(), 0);
        assert!(!c.is_overflow());
    }

    #[test]
    fn drain_into_retained_returns_prefix_only() {
        let c = StreamCapture::drain(Cursor::new(&b"helloworld"[..]), nz(5)).unwrap();
        assert_eq!(c.into_retained(), b"hello");
    }

    // --- drain: overflow keeps draining/counting -----------------------------

    #[test]
    fn drain_overflow_retains_prefix_keeps_total() {
        let feed: Vec<u8> = (0..250u8).collect(); // 250 bytes
        let cap = nz(100);
        let c = StreamCapture::drain(Cursor::new(&feed[..]), cap).unwrap();
        assert_eq!(c.retained(), &feed[..100]);
        assert_eq!(c.total_bytes(), 250);
        assert!(c.is_overflow());
        let s = c.stats();
        assert_eq!(s.retained, 100);
        assert_eq!(s.discarded, 150);
        assert!(s.cap_exceeded);
        assert!(!s.total_saturated);
    }

    // --- drain: chunking invariance (same feed, any boundaries) --------------

    #[test]
    fn drain_chunking_invariant_same_feed_any_boundaries() {
        let feed: Vec<u8> = (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .cycle()
            .take(1000)
            .collect();
        let cap = nz(300);
        // Reference: a Cursor hands the whole 8 KiB drain buffer at once for a
        // 1000-byte feed.
        let reference = StreamCapture::drain(Cursor::new(&feed[..]), cap).unwrap();
        for chunk in [1usize, 2, 7, 13, 128, 8192] {
            let c = StreamCapture::drain(ChunkedReader::new(&feed, chunk), cap).unwrap();
            assert_eq!(
                c.retained(),
                reference.retained(),
                "retained differs at chunk={chunk}"
            );
            assert_eq!(
                c.total_bytes(),
                reference.total_bytes(),
                "total differs at chunk={chunk}"
            );
            assert_eq!(
                c.stats(),
                reference.stats(),
                "stats differ at chunk={chunk}"
            );
        }
        // Sanity: the reference overflowed and retained exactly the cap.
        assert_eq!(reference.retained().len(), 300);
        assert_eq!(reference.total_bytes(), 1000);
        assert!(reference.is_overflow());
    }

    /// A test reader that returns at most `chunk` bytes per `read`, to prove
    /// `drain`'s accounting is independent of feed chunking.
    struct ChunkedReader<'a> {
        data: &'a [u8],
        chunk: usize,
        pos: usize,
    }

    impl<'a> ChunkedReader<'a> {
        fn new(data: &'a [u8], chunk: usize) -> Self {
            assert!(chunk > 0, "chunk must be > 0");
            ChunkedReader {
                data,
                chunk,
                pos: 0,
            }
        }
    }

    impl<'a> Read for ChunkedReader<'a> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = self.chunk.min(buf.len()).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    // --- drain: interrupted reads are retried, not surfaced ------------------

    #[test]
    fn drain_retries_interrupted_then_succeeds() {
        // A reader that fails Interrupted three times, then yields the feed,
        // then EOF: drain must retry the EINTRs and capture the whole feed
        // rather than surfacing CaptureError::Read.
        let feed = b"hello-after-eintr";
        let c = StreamCapture::drain(InterruptedReader::new(feed, 3), nz(64))
            .expect("Interrupted reads must be retried, not surfaced");
        assert_eq!(c.retained(), feed);
        assert_eq!(c.total_bytes(), feed.len() as u64);
        assert!(!c.is_overflow());
    }

    #[test]
    fn drain_retries_interrupted_mid_stream_keeps_counting() {
        // Yields "abc", fails Interrupted once, yields "def", EOF: the retry
        // must NOT lose the already-captured bytes, double-count them, or fail.
        let c = StreamCapture::drain(MidStreamInterrupted::new(b"abc", b"def", 1), nz(64))
            .expect("mid-stream Interrupted must be retried");
        assert_eq!(c.retained(), b"abcdef");
        assert_eq!(c.total_bytes(), 6);
        assert!(!c.is_overflow());
    }

    #[test]
    fn drain_retries_repeated_interrupted_until_eof() {
        // A reader that returns Interrupted on EVERY read would loop forever;
        // instead, interrupt a bounded number of times then signal EOF, proving
        // the retry path terminates once real data/EOF arrives.
        let c = StreamCapture::drain(InterruptedReader::new(b"", 5), nz(64))
            .expect("trailing Interrupted then EOF must complete");
        assert!(c.retained().is_empty());
        assert_eq!(c.total_bytes(), 0);
        assert!(!c.is_overflow());
    }

    /// A reader that fails with `Interrupted` `interrupts` times, then serves
    /// `data` (in one read), then EOF.
    struct InterruptedReader<'a> {
        data: &'a [u8],
        pos: usize,
        interrupts_left: u32,
    }

    impl<'a> InterruptedReader<'a> {
        fn new(data: &'a [u8], interrupts: u32) -> Self {
            assert!(interrupts > 0, "interrupts must be > 0 for this reader");
            InterruptedReader {
                data,
                pos: 0,
                interrupts_left: interrupts,
            }
        }
    }

    impl<'a> Read for InterruptedReader<'a> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupts_left > 0 {
                self.interrupts_left -= 1;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "synthetic EINTR",
                ));
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

    /// A reader that serves `head`, then fails `Interrupted` `interrupts`
    /// times, then serves `tail`, then EOF.
    struct MidStreamInterrupted<'a> {
        head: &'a [u8],
        tail: &'a [u8],
        head_pos: usize,
        tail_pos: usize,
        interrupts_left: u32,
    }

    impl<'a> MidStreamInterrupted<'a> {
        fn new(head: &'a [u8], tail: &'a [u8], interrupts: u32) -> Self {
            assert!(interrupts > 0, "interrupts must be > 0 for this reader");
            MidStreamInterrupted {
                head,
                tail,
                head_pos: 0,
                tail_pos: 0,
                interrupts_left: interrupts,
            }
        }
    }

    impl<'a> Read for MidStreamInterrupted<'a> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.head_pos < self.head.len() {
                let n = buf.len().min(self.head.len() - self.head_pos);
                buf[..n].copy_from_slice(&self.head[self.head_pos..self.head_pos + n]);
                self.head_pos += n;
                return Ok(n);
            }
            if self.interrupts_left > 0 {
                self.interrupts_left -= 1;
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "synthetic EINTR",
                ));
            }
            if self.tail_pos < self.tail.len() {
                let n = buf.len().min(self.tail.len() - self.tail_pos);
                buf[..n].copy_from_slice(&self.tail[self.tail_pos..self.tail_pos + n]);
                self.tail_pos += n;
                Ok(n)
            } else {
                Ok(0)
            }
        }
    }

    // --- drain: read failure mapping -----------------------------------------

    #[test]
    fn drain_maps_read_error_to_capture_error_by_kind() {
        // NOTE: io::ErrorKind::Interrupted is DELIBERATELY absent here — drain
        // retries it (see the drain_retries_* tests) rather than surfacing it.
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::Other,
        ] {
            let err = StreamCapture::drain(FailingReader { kind }, nz(64)).unwrap_err();
            assert_eq!(err, CaptureError::Read { kind });
            // Display is deterministic and names the mapped token.
            let s = err.to_string();
            assert!(s.starts_with("capture: read failed ("));
            assert!(s.ends_with(')'));
        }
    }

    #[test]
    fn drain_returns_err_on_failure_after_partial_read() {
        // Yields some bytes, then fails mid-stream: drain surfaces the error
        // (it does NOT return a partial capture on read failure).
        let err = StreamCapture::drain(
            PartialThenFail::new(b"abc", io::ErrorKind::BrokenPipe),
            nz(64),
        )
        .unwrap_err();
        assert_eq!(
            err,
            CaptureError::Read {
                kind: io::ErrorKind::BrokenPipe
            }
        );
    }

    /// A reader that always fails with a fixed kind.
    struct FailingReader {
        kind: io::ErrorKind,
    }

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "synthetic test failure"))
        }
    }

    /// A reader that yields a fixed head then fails with a fixed kind.
    struct PartialThenFail {
        head: &'static [u8],
        pos: usize,
        kind: io::ErrorKind,
    }

    impl PartialThenFail {
        fn new(head: &'static [u8], kind: io::ErrorKind) -> Self {
            PartialThenFail { head, pos: 0, kind }
        }
    }

    impl Read for PartialThenFail {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos < self.head.len() {
                let n = buf.len().min(self.head.len() - self.pos);
                buf[..n].copy_from_slice(&self.head[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            } else {
                Err(io::Error::new(self.kind, "synthetic test failure"))
            }
        }
    }

    // --- bounded display -----------------------------------------------------

    #[test]
    fn spec_error_display_is_bounded_for_huge_path() {
        let path = PathBuf::from(format!("x{}", "Y".repeat(50_000)));
        let err = CommandSpec::new(
            path,
            vec![],
            BTreeMap::new(),
            nz(1),
            nz(1),
            Duration::from_secs(1),
        )
        .unwrap_err();
        let s = err.to_string();
        assert!(
            s.len() <= SNIPPET_MAX + 80,
            "display must be bounded, was {}: {s:?}",
            s.len()
        );
        assert!(s.contains("must be absolute"));
        assert!(s.contains("...")); // truncated snippet marker
    }

    #[test]
    fn spec_error_timeout_display_is_bounded_for_huge_duration() {
        let s = SpecError::TimeoutTooLarge {
            got_nanos: u128::MAX,
        }
        .to_string();
        // u128::MAX is 39 digits; total message is well under any practical
        // bound and is fully deterministic.
        assert!(s.len() < 200, "too long ({}): {s:?}", s.len());
        assert!(s.contains("must be <= 1 h"));
        assert!(s.contains(&u128::MAX.to_string()));
    }

    #[test]
    fn command_error_display_is_bounded_and_deterministic() {
        // Wrapping a huge-path SpecError stays bounded.
        let path = PathBuf::from(format!("z{}", "Z".repeat(60_000)));
        let spec_err = CommandSpec::new(
            path,
            vec![],
            BTreeMap::new(),
            nz(1),
            nz(1),
            Duration::from_secs(1),
        )
        .unwrap_err();
        let cerr: CommandError = spec_err.into();
        let s = cerr.to_string();
        assert!(s.len() <= SNIPPET_MAX + 80, "too long ({}): {s:?}", s.len());

        // Each I/O variant maps a kind to a stable token; no platform text leaks.
        for (kind, tok) in [
            (io::ErrorKind::NotFound, "not_found"),
            (io::ErrorKind::BrokenPipe, "broken_pipe"),
            (io::ErrorKind::TimedOut, "timed_out"),
            (io::ErrorKind::Other, "other"),
        ] {
            let s = CommandError::Spawn { kind }.to_string();
            assert!(s.contains(tok), "spawn {kind:?} -> {s:?}");
            assert!(s.contains("spawn failed"));
        }
        // Non-IO variants are fixed strings.
        assert_eq!(
            CommandError::ReaderPanic.to_string(),
            "command: capture reader panicked"
        );
        assert_eq!(
            CommandError::Rss.to_string(),
            "command: maximum-RSS capture failed"
        );
        assert!(
            CommandError::Timeout { killed: true }
                .to_string()
                .contains("killed=true")
        );
        assert!(
            CommandError::Timeout { killed: false }
                .to_string()
                .contains("killed=false")
        );
        assert!(
            CommandError::CapOverflow {
                stream: Stream::Stdout
            }
            .to_string()
            .contains("stdout")
        );
        assert!(
            CommandError::Utf8 {
                stream: Stream::Stderr
            }
            .to_string()
            .contains("stderr")
        );
    }

    #[test]
    fn capture_error_display_maps_kind_token() {
        let s = CaptureError::Read {
            kind: io::ErrorKind::BrokenPipe,
        }
        .to_string();
        assert_eq!(s, "capture: read failed (broken_pipe)");
    }

    #[test]
    fn from_impls_wire_structured_errors() {
        let se: CommandError = SpecError::ProgramEmpty.into();
        assert!(matches!(se, CommandError::Spec(SpecError::ProgramEmpty)));
        let ce: CommandError = CaptureError::Read {
            kind: io::ErrorKind::TimedOut,
        }
        .into();
        assert!(matches!(ce, CommandError::ReaderIo { kind } if kind == io::ErrorKind::TimedOut));
    }

    // --- MissingPipe: structural, identifies the stream, no io kind ----------

    #[test]
    fn command_error_missing_pipe_identifies_stream_without_io_kind() {
        // A configured pipe absent at take-time is a structural (not-piped)
        // misconfiguration, not an I/O error: it names the stream and carries no
        // invented io::ErrorKind.
        assert_eq!(
            CommandError::MissingPipe {
                stream: Stream::Stdout
            }
            .to_string(),
            "command: stdout capture pipe unavailable (not piped)"
        );
        assert_eq!(
            CommandError::MissingPipe {
                stream: Stream::Stderr
            }
            .to_string(),
            "command: stderr capture pipe unavailable (not piped)"
        );
        // No I/O kind token leaks into a structural pipe-absence message.
        for s in [
            CommandError::MissingPipe {
                stream: Stream::Stdout,
            }
            .to_string(),
            CommandError::MissingPipe {
                stream: Stream::Stderr,
            }
            .to_string(),
        ] {
            assert!(!s.contains("not_found"));
            assert!(!s.contains("broken_pipe"));
            assert!(!s.contains("other"));
        }
    }

    // --- UnixStatus + CommandOutcome semantics -------------------------------

    #[test]
    fn unix_status_only_two_disjoint_variants_no_impossible_states() {
        // The enum makes "both a code and a signal" and "neither" UNREPRESENTABLE
        // — exactly one case always holds. is_success is true ONLY for Exited(0).
        let cases: [(UnixStatus, &str, bool); 5] = [
            (UnixStatus::Exited(0), "exit 0", true),
            (UnixStatus::Exited(1), "exit 1", false),
            (UnixStatus::Exited(127), "exit 127", false),
            (UnixStatus::Signaled(9), "signal 9", false),
            (UnixStatus::Signaled(15), "signal 15", false),
        ];
        for (st, disp, ok) in cases {
            assert_eq!(st.to_string(), disp, "display for {st:?}");
            assert_eq!(st.is_success(), ok, "is_success for {st:?}");
        }
        // Any signal — even an unusual one — is never a success.
        assert!(!UnixStatus::Signaled(0).is_success());
    }

    #[test]
    fn command_outcome_success_is_derived_from_status() {
        // There is no stored `success` field; is_success() is derived from
        // status so the two can never disagree.
        fn mk(status: UnixStatus) -> CommandOutcome {
            CommandOutcome {
                status,
                stdout: Vec::new(),
                cleaned_stderr: String::new(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
                wall_ms: 7,
                max_rss_kib: 128,
            }
        }
        assert!(mk(UnixStatus::Exited(0)).is_success());
        assert!(!mk(UnixStatus::Exited(2)).is_success());
        assert!(!mk(UnixStatus::Signaled(9)).is_success());
    }

    #[test]
    fn command_outcome_max_rss_is_required_u64() {
        // An outcome exists only after the /usr/bin/time metric was captured and
        // parsed; RSS failure is CommandError::Rss, never a None field. The
        // field is a plain u64, so "missing RSS" is unrepresentable here.
        let out = CommandOutcome {
            status: UnixStatus::Exited(0),
            stdout: Vec::new(),
            cleaned_stderr: String::new(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            wall_ms: 12,
            max_rss_kib: 2048,
        };
        let _: u64 = out.max_rss_kib; // type assertion: not Option<u64>
        assert_eq!(out.max_rss_kib, 2048);
        // The structured failure path is the Rss error, not a None payload.
        assert_eq!(
            CommandError::Rss.to_string(),
            "command: maximum-RSS capture failed"
        );
    }

    #[test]
    fn time_flavor_label_is_stable() {
        assert_eq!(TimeFlavor::MacOs.label(), "macos");
        assert_eq!(TimeFlavor::Gnu.label(), "gnu");
        assert_eq!(TimeFlavor::MacOs.to_string(), "macos");
        assert_eq!(TimeFlavor::Gnu.to_string(), "gnu");
    }
}
