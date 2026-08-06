//! Spike S4 (PR-6 / DR-004) — FAKE slice: a strict, hidden, deterministic
//! fake-child protocol that the future `main` binary will use to exercise the
//! EXACT command pipeline (real `/usr/bin/time`, real pipes, real caps, real
//! timeout) without touching Nix or the network.
//!
//! This module is *library* code with two jobs:
//!
//! 1. **Strict parse.** [`parse`] accepts ONLY the exact hidden subcommand marker
//!    followed by a closed set of `--key value` flags:
//!
//!    ```text
//!    <MARKER> [--stdout-bytes N] [--stderr-bytes N] [--sleep-ms N] [--exit-code N]
//!    ```
//!
//!    with hard bounds and no tolerance for duplicates, unknown keys, missing
//!    values, or malformed/out-of-range values. It returns a typed [`ChildPlan`]
//!    and stable, bounded [`ParseError`]s (caller-controlled snippets are
//!    length-capped so a hostile/huge token can never bloat a diagnostic).
//!
//! 2. **Deterministic execute.** [`execute`] writes a deterministic byte pattern
//!    to two GENERIC [`std::io::Write`] sinks using a single fixed-size reusable
//!    chunk — it NEVER allocates a buffer proportional to the requested size —
//!    optionally sleeps, and RETURNS the selected exit code rather than calling
//!    [`std::process::exit`] (the caller owns process termination, so the library
//!    stays unit-testable and side-effect-free). A write failure is reported as a
//!    structured [`ExecError`]; broken-pipe is its own variant.
//!
//! All output bytes are a pure function of their position (`byte[i] = b'a' + (i %
//! 26)`), so chunk boundaries and short writes never change the bytes a sink
//! ultimately receives. `#![forbid(unsafe_code)]` is inherited from the crate
//! root.

use std::fmt;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// The leading marker token that selects fake-child mode (the "hidden
/// subcommand"). The future `main` re-invokes its own binary as
/// `<self> <MARKER> --stdout-bytes N ...`; [`parse`] requires this token first
/// and returns [`ParseError::NoMarker`] otherwise, so a normal benchmark run is
/// cleanly distinguished from a fake-child run.
pub const MARKER: &str = "s4-fake-child";

/// Hard ceiling on requested stdout bytes (64 MiB). Bounds a buggy/hostile
/// caller so a test cannot accidentally request an absurd allocation.
pub const MAX_STDOUT_BYTES: u64 = 64 * 1024 * 1024;
/// Hard ceiling on requested stderr bytes (64 MiB).
pub const MAX_STDERR_BYTES: u64 = 64 * 1024 * 1024;
/// Hard ceiling on requested sleep, in milliseconds (5 s). Keeps the fake child
/// from running unboundedly long while still exercising the timeout path.
pub const MAX_SLEEP_MS: u64 = 5_000;
/// Maximum accepted exit code. 0..=125 mirrors the portable program-exit range
/// (126/127 and 128+N are reserved by shells for "not executable" / "not found"
/// / "killed by signal N"), so a fake child must stay inside 0..=125.
pub const MAX_EXIT_CODE: i32 = 125;

/// Size of the single reusable generation buffer. Output of any requested size
/// is produced by repeatedly writing this fixed chunk, so the executor never
/// allocates a buffer proportional to the requested size.
pub const CHUNK: usize = 8 * 1024;

/// A fully-parsed, validated fake-child plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildPlan {
    /// Number of deterministic bytes to write to stdout (`<= MAX_STDOUT_BYTES`).
    pub stdout_bytes: u64,
    /// Number of deterministic bytes to write to stderr (`<= MAX_STDERR_BYTES`).
    pub stderr_bytes: u64,
    /// Milliseconds to sleep AFTER writing all output (`<= MAX_SLEEP_MS`).
    pub sleep_ms: u64,
    /// Exit code the caller should exit with (`0..=MAX_EXIT_CODE`).
    pub exit_code: i32,
}

impl Default for ChildPlan {
    /// The all-zero plan: writes nothing, sleeps nothing, exits 0.
    fn default() -> Self {
        ChildPlan {
            stdout_bytes: 0,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 0,
        }
    }
}

impl ChildPlan {
    /// The deterministic byte produced at stream position `i` (the fill pattern
    /// is a pure function of position, independent of chunk boundaries).
    #[must_use]
    pub fn byte_at(i: u64) -> u8 {
        // Take the modulo on the FULL index BEFORE narrowing to u8: casting
        // first (`(i as u8) % 26`) silently truncates `i` to its low 8 bits,
        // so the pattern would wrongly reset every 256 bytes (e.g. byte_at(256)
        // collapsed to 'a' instead of the documented 'w'). `i % 26` is always
        // 0..26 and fits in a u8, so the documented period-26 pattern holds at
        // every absolute offset.
        b'a' + ((i % 26) as u8)
    }
}

/// Which output stream an [`ExecError`] occurred on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stream::Stdout => f.write_str("stdout"),
            Stream::Stderr => f.write_str("stderr"),
        }
    }
}

/// A structured execution error from [`execute`]. Every variant identifies the
/// stream; broken-pipe is its own variant so the caller can react to a closed
/// read end distinctly from other write failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
    /// A write or flush failed because the read end of the pipe is closed
    /// (`ErrorKind::BrokenPipe`).
    BrokenPipe {
        /// The stream whose sink is broken.
        stream: Stream,
    },
    /// Any other write/flush failure, identified by its stable `ErrorKind`.
    WriteFailed {
        /// The stream that failed.
        stream: Stream,
        /// The stable I/O error kind.
        kind: io::ErrorKind,
    },
    /// A stream worker PANICKED during generation or flush. Rather than
    /// unwinding through [`execute`] (and through [`thread::scope`]), the panic
    /// is caught at the worker's join and reported as this stable, structured
    /// variant for the offending [`Stream`].
    WorkerPanicked {
        /// The stream whose worker panicked.
        stream: Stream,
    },
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::BrokenPipe { stream } => {
                write!(f, "fake: broken pipe on {stream}")
            }
            ExecError::WriteFailed { stream, kind } => {
                write!(f, "fake: write to {stream} failed: {kind}")
            }
            ExecError::WorkerPanicked { stream } => {
                write!(f, "fake: worker on {stream} panicked")
            }
        }
    }
}

impl std::error::Error for ExecError {}

/// Strict parse error. Every variant is stable and bounded: caller-controlled
/// snippets are funneled through [`snip`] so diagnostics can never grow with the
/// input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The leading [`MARKER`] was absent (the "normal run" case; the helper
    /// should fall through to normal operation).
    NoMarker,
    /// A token after the marker was not one of the four known flags.
    UnknownArgument {
        /// The offending token (snippet-bounded).
        arg: String,
    },
    /// A flag appeared more than once.
    DuplicateKey {
        /// The canonical key name (without `--`).
        key: &'static str,
    },
    /// A flag had no following value token.
    MissingValue {
        /// The canonical key name (without `--`).
        key: &'static str,
    },
    /// A value could not be parsed as the required integer type.
    InvalidValue {
        /// The canonical key name (without `--`).
        key: &'static str,
        /// The offending value (snippet-bounded).
        value: String,
    },
    /// A value parsed fine but was outside the accepted range.
    OutOfRange {
        /// The canonical key name (without `--`).
        key: &'static str,
        /// The offending value (snippet-bounded).
        value: String,
        /// The inclusive maximum for this field.
        max: u64,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NoMarker => write!(f, "fake: marker {MARKER:?} not present"),
            ParseError::UnknownArgument { arg } => {
                write!(f, "fake: unknown argument {:?}", snip(arg))
            }
            ParseError::DuplicateKey { key } => write!(f, "fake: duplicate {key}"),
            ParseError::MissingValue { key } => write!(f, "fake: missing value for {key}"),
            ParseError::InvalidValue { key, value } => {
                write!(f, "fake: invalid {key}={:?}", snip(value))
            }
            ParseError::OutOfRange { key, value, max } => {
                write!(f, "fake: {key}={:?} out of range (0..={max})", snip(value))
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Bound any caller-controlled snippet included in error messages so a huge
/// token can never bloat diagnostics.
const SNIPPET_MAX: usize = 64;

fn snip(s: &str) -> String {
    if s.len() <= SNIPPET_MAX {
        s.to_owned()
    } else {
        // Truncate at the largest UTF-8 character boundary at or below
        // SNIPPET_MAX bytes. A naive `s[..SNIPPET_MAX]` slice would PANIC if
        // byte SNIPPET_MAX lands inside a multibyte codepoint (e.g. emoji or
        // CJK full-width text in a hostile token); `floor_char_boundary` always
        // returns a valid char boundary `<=` its argument, so the body is at
        // most SNIPPET_MAX bytes and never splits a codepoint.
        let cut = s.floor_char_boundary(SNIPPET_MAX);
        let mut t = s[..cut].to_owned();
        t.push('…');
        t
    }
}

/// Parse a fake-child request from the tokens after `argv[0]` (i.e.
/// `std::env::args().skip(1).collect()`). The leading [`MARKER`] is required;
/// everything after it is parsed strictly as `--key value` pairs from a closed
/// set. All flags are optional with safe defaults; duplicates, unknown keys,
/// missing values, and malformed/out-of-range values are rejected as stable
/// structured errors.
pub fn parse(args: &[String]) -> Result<ChildPlan, ParseError> {
    if args.first().map(String::as_str) != Some(MARKER) {
        return Err(ParseError::NoMarker);
    }

    let mut plan = ChildPlan::default();
    let mut seen_out = false;
    let mut seen_err = false;
    let mut seen_sleep = false;
    let mut seen_exit = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--stdout-bytes" => {
                if seen_out {
                    return Err(ParseError::DuplicateKey {
                        key: "stdout-bytes",
                    });
                }
                let v = take_value(args, &mut i, "stdout-bytes")?;
                plan.stdout_bytes = check_u64(&v, "stdout-bytes", MAX_STDOUT_BYTES)?;
                seen_out = true;
            }
            "--stderr-bytes" => {
                if seen_err {
                    return Err(ParseError::DuplicateKey {
                        key: "stderr-bytes",
                    });
                }
                let v = take_value(args, &mut i, "stderr-bytes")?;
                plan.stderr_bytes = check_u64(&v, "stderr-bytes", MAX_STDERR_BYTES)?;
                seen_err = true;
            }
            "--sleep-ms" => {
                if seen_sleep {
                    return Err(ParseError::DuplicateKey { key: "sleep-ms" });
                }
                let v = take_value(args, &mut i, "sleep-ms")?;
                plan.sleep_ms = check_u64(&v, "sleep-ms", MAX_SLEEP_MS)?;
                seen_sleep = true;
            }
            "--exit-code" => {
                if seen_exit {
                    return Err(ParseError::DuplicateKey { key: "exit-code" });
                }
                let v = take_value(args, &mut i, "exit-code")?;
                plan.exit_code = check_exit(&v)?;
                seen_exit = true;
            }
            other => return Err(ParseError::UnknownArgument { arg: snip(other) }),
        }
    }
    Ok(plan)
}

/// Read the value token following a key at `*i`, advancing `*i` past BOTH the
/// key and the value. Returns [`ParseError::MissingValue`] if no value token is
/// present.
fn take_value(args: &[String], i: &mut usize, key: &'static str) -> Result<String, ParseError> {
    let value = match args.get(*i + 1) {
        Some(v) => v.clone(),
        None => return Err(ParseError::MissingValue { key }),
    };
    *i += 2;
    Ok(value)
}

/// Parse a non-negative integer in `0..=max`.
///
/// Requires `value` to be a non-empty run of ASCII digits BEFORE attempting
/// `u64::from_str`. This up-front guard classifies as [`ParseError::InvalidValue`]
/// everything we want to reject on shape rather than magnitude: `+1` (which the
/// std parser would otherwise accept), a leading `-`/`+`, surrounding
/// whitespace, underscores, and non-ASCII (Unicode) digits such as the
/// Arabic-Indic `١` or full-width `１`. A purely-numeric value that overflows
/// `u64` still falls through to `parse`, which reports it as `InvalidValue`.
fn check_u64(value: &str, key: &'static str, max: u64) -> Result<u64, ParseError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::InvalidValue {
            key,
            value: snip(value),
        });
    }
    let v: u64 = value.parse().map_err(|_| ParseError::InvalidValue {
        key,
        value: snip(value),
    })?;
    if v > max {
        return Err(ParseError::OutOfRange {
            key,
            value: snip(value),
            max,
        });
    }
    Ok(v)
}

/// Parse an exit code in the portable `0..=MAX_EXIT_CODE` range.
///
/// Accepts ASCII digits, or exactly one leading `-` followed by ASCII digits,
/// so a negative value PARSES and is then reported as
/// [`ParseError::OutOfRange`] (not a parse failure). Everything else — `+1`,
/// surrounding whitespace, Unicode digits, a stray `-`, doubled signs, and
/// other junk — is [`ParseError::InvalidValue`]. The validated text is then
/// parsed as `i64`; a magnitude that overflows `i64` is `InvalidValue`.
fn check_exit(value: &str) -> Result<i32, ParseError> {
    const KEY: &str = "exit-code";
    // Accept `123` or `-123` only; reject `+1`, whitespace, Unicode digits, and
    // misplaced/doubled signs up front as InvalidValue.
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::InvalidValue {
            key: KEY,
            value: snip(value),
        });
    }
    let v: i64 = value.parse().map_err(|_| ParseError::InvalidValue {
        key: KEY,
        value: snip(value),
    })?;
    if !(0..=i64::from(MAX_EXIT_CODE)).contains(&v) {
        return Err(ParseError::OutOfRange {
            key: KEY,
            value: snip(value),
            max: u64::try_from(MAX_EXIT_CODE).expect("MAX_EXIT_CODE is non-negative"),
        });
    }
    Ok(v as i32)
}

// --- deterministic generation ----------------------------------------------

/// Allocate one reusable [`CHUNK`]-byte generation buffer (zeroed). Each
/// concurrent stream worker in [`execute`] allocates its own and never grows
/// it, keeping peak generation memory `O(2 * CHUNK)` regardless of the
/// requested output sizes. [`write_n`] REFILLS this
/// buffer for each emitted window with the correct absolute-position bytes, so
/// a chunk size that is not a multiple of the 26-byte pattern period
/// (8192 % 26 = 2) can never desynchronize the stream at a chunk seam.
fn make_chunk() -> Vec<u8> {
    vec![0u8; CHUNK]
}

/// Map an [`io::Error`] from a stream write/flush into the structured
/// [`ExecError`], lifting broken-pipe into its own variant.
fn map_write_err(stream: Stream, err: io::Error) -> ExecError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        ExecError::BrokenPipe { stream }
    } else {
        ExecError::WriteFailed {
            stream,
            kind: err.kind(),
        }
    }
}

/// Write exactly `n` deterministic bytes to `w`. The reusable `chunk` buffer
/// is REFILLED for each emitted window with the bytes for that window's
/// absolute stream offsets, so the documented pattern
/// (`byte[i] = b'a' + (i % 26)`) holds at EVERY absolute offset — including
/// past 255 and across every [`CHUNK`]-byte chunk seam — even though [`CHUNK`]
/// (8192) is not a multiple of the 26-byte period (simply repeating one fixed
/// chunk would resync to 'a' at each seam and corrupt offset 8192 onward).
/// Each underlying `write` receives at most `chunk.len()` (`<= CHUNK`) bytes
/// regardless of `n`, so memory stays `O(CHUNK)`; [`Write::write_all`]
/// transparently loops over short writes without changing which bytes the sink
/// ultimately receives. Propagates write errors as structured [`ExecError`]s.
fn write_n<W: Write>(w: &mut W, n: u64, chunk: &mut [u8], stream: Stream) -> Result<(), ExecError> {
    if n == 0 {
        return Ok(());
    }
    let mut remaining = n;
    let mut pos: u64 = 0;
    while remaining > 0 {
        let take = remaining.min(chunk.len() as u64) as usize;
        // Fill this window with the correct ABSOLUTE-position pattern so chunk
        // seams never desync the stream: the byte at offset `pos + j` is always
        // `byte_at(pos + j)`, independent of CHUNK or the 26-byte period.
        for (j, slot) in chunk.iter_mut().enumerate().take(take) {
            *slot = ChildPlan::byte_at(pos + j as u64);
        }
        w.write_all(&chunk[..take])
            .map_err(|e| map_write_err(stream, e))?;
        pos += take as u64;
        remaining -= take as u64;
    }
    Ok(())
}

/// Execute a [`ChildPlan`] against two arbitrary writers (pure / testable) by
/// generating the stdout and stderr streams CONCURRENTLY — one dedicated worker
/// per stream, each owning its writer for the duration of a [`thread::scope`],
/// each writing its deterministic byte pattern from its OWN fixed-size reusable
/// [`CHUNK`] buffer and flushing before it finishes.
///
/// Concurrency is load-bearing: the real child's stdout and stderr are two
/// separate OS pipes, so a downstream command reader that drains the pipes
/// *sequentially* (all of stdout, then all of stderr) can only make progress if
/// the producer fills both pipes at the same time. A fake that writes all of
/// stdout and only then all of stderr would let such a reader succeed and could
/// never exercise simultaneous pipe pressure; this fake fills both concurrently,
/// so a sequentially-draining reader is a genuine deadlock test rather than a
/// no-op.
///
/// Operational guarantees:
///
/// * **Two separate fixed buffers.** Each worker allocates one [`CHUNK`]-byte
///   buffer, so peak generation memory is `O(2 * CHUNK)`, never proportional to
///   the requested sizes.
/// * **Join on every path.** Both workers are joined via their scoped handles
///   before `execute` decides its result, so neither writer is touched again
///   after the scope ends.
/// * **Panic containment.** A worker PANIC is observed at the join and reported
///   as the structured [`ExecError::WorkerPanicked`] for that stream instead of
///   unwinding through `execute` or through [`thread::scope`]; `execute`
///   therefore never propagates a worker panic.
/// * **Deterministic precedence.** If BOTH streams error, stdout wins — the
///   stdout error is returned even when stderr also failed, independent of
///   which worker finished first in wall-clock time.
/// * **Single, success-only sleep.** The optional sleep happens exactly once,
///   AFTER both workers join successfully; no error path sleeps.
///
/// Requires `Send` writers so each `&mut` reference can be handed to its worker
/// thread. Does not call [`std::process::exit`]; the caller owns termination.
pub fn execute<So: Write + Send, Se: Write + Send>(
    plan: &ChildPlan,
    stdout: &mut So,
    stderr: &mut Se,
) -> Result<i32, ExecError> {
    // Snapshot the byte counts (cheap `Copy`) so the worker closures capture
    // plain integers instead of borrowing `plan`.
    let out_bytes = plan.stdout_bytes;
    let err_bytes = plan.stderr_bytes;

    // Run both stream workers concurrently inside a scope. Scoped threads may
    // borrow the (now-`Send`) writers for the duration of the scope, and the
    // scope blocks until both threads have exited, so the borrows are released
    // before `execute` returns. Each worker writes + flushes its own stream
    // with its OWN fixed buffer, so the two pipes are filled at the same time.
    let (out_res, err_res) = thread::scope(|s| {
        let out_handle = s.spawn(move || {
            let mut chunk = make_chunk();
            stream_worker(stdout, out_bytes, &mut chunk, Stream::Stdout)
        });
        let err_handle = s.spawn(move || {
            let mut chunk = make_chunk();
            stream_worker(stderr, err_bytes, &mut chunk, Stream::Stderr)
        });
        // Join both on every path. A join that returns `Err` is a worker panic
        // and is mapped to a structured `WorkerPanicked` error here, leaving scope()
        // with no residual panic to propagate.
        let out = join_worker(out_handle.join(), Stream::Stdout);
        let err = join_worker(err_handle.join(), Stream::Stderr);
        (out, err)
    });

    // Deterministic precedence: a stdout error wins over any stderr error.
    out_res.and(err_res)?;

    // Sleep exactly once, and only after both workers joined successfully.
    if plan.sleep_ms > 0 {
        thread::sleep(Duration::from_millis(plan.sleep_ms));
    }
    Ok(plan.exit_code)
}

/// Generate `n` deterministic bytes into `w` and then flush, mapping any write
/// or flush failure to a structured [`ExecError`] tagged with `stream`. This
/// bundles [`write_n`] plus a flush so each scoped worker is one self-contained
/// fallible unit (write all the bytes, then flush) operating on its OWN fixed
/// `chunk` buffer.
fn stream_worker<W: Write>(
    w: &mut W,
    n: u64,
    chunk: &mut [u8],
    stream: Stream,
) -> Result<(), ExecError> {
    write_n(w, n, chunk, stream)?;
    w.flush().map_err(|e| map_write_err(stream, e))?;
    Ok(())
}

/// Convert a scoped-worker join result into a structured [`ExecError`]-bearing
/// result: a normal worker `Err(e)` flows through unchanged, while a worker
/// PANIC (the join itself returns [`Err`]) is reported as
/// [`ExecError::WorkerPanicked`] for `stream` instead of unwinding.
fn join_worker(
    joined: thread::Result<Result<(), ExecError>>,
    stream: Stream,
) -> Result<(), ExecError> {
    match joined {
        Ok(inner) => inner,
        Err(_panic_payload) => Err(ExecError::WorkerPanicked { stream }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    fn toks(parts: &[&str]) -> Vec<String> {
        parts.iter().copied().map(String::from).collect()
    }

    /// A sink that records the maximum single-`write` byte count it was handed,
    /// proving the executor never presents more than `CHUNK` bytes per write
    /// (i.e. it never builds a requested-size buffer).
    #[derive(Default)]
    struct RecordingSink {
        out: Vec<u8>,
        max_write: usize,
    }

    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.max_write = self.max_write.max(buf.len());
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A sink that only accepts `step` bytes per `write` call, forcing
    /// `write_all` to loop (simulates pipe backpressure / short writes).
    struct ShortWriteSink {
        out: Vec<u8>,
        step: usize,
    }

    impl ShortWriteSink {
        fn new(step: usize) -> Self {
            ShortWriteSink {
                out: Vec::new(),
                step,
            }
        }
    }

    impl Write for ShortWriteSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let k = buf.len().min(self.step).max(1);
            self.out.extend_from_slice(&buf[..k]);
            Ok(k)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A sink whose writes always fail with a chosen error kind.
    struct FailingSink {
        kind: ErrorKind,
    }

    impl Write for FailingSink {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.kind))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(self.kind))
        }
    }

    fn expected_pattern(n: u64) -> Vec<u8> {
        (0..n).map(ChildPlan::byte_at).collect()
    }

    // === parser: happy paths =================================================

    #[test]
    fn marker_alone_yields_defaults() {
        let r = parse(&toks(&[MARKER])).unwrap();
        assert_eq!(r, ChildPlan::default());
    }

    #[test]
    fn all_flags_parse_in_any_order() {
        let r = parse(&toks(&[
            MARKER,
            "--sleep-ms",
            "50",
            "--exit-code",
            "7",
            "--stdout-bytes",
            "100",
            "--stderr-bytes",
            "200",
        ]))
        .unwrap();
        assert_eq!(r.stdout_bytes, 100);
        assert_eq!(r.stderr_bytes, 200);
        assert_eq!(r.sleep_ms, 50);
        assert_eq!(r.exit_code, 7);
    }

    #[test]
    fn missing_individual_flags_keep_defaults() {
        let r = parse(&toks(&[MARKER, "--exit-code", "3"])).unwrap();
        assert_eq!(
            r,
            ChildPlan {
                stdout_bytes: 0,
                stderr_bytes: 0,
                sleep_ms: 0,
                exit_code: 3
            }
        );
    }

    // === parser: marker handling ============================================

    #[test]
    fn missing_marker_is_no_marker() {
        assert_eq!(
            parse(&toks(&["--stdout-bytes", "10"])),
            Err(ParseError::NoMarker)
        );
        assert_eq!(parse(&toks(&[])), Err(ParseError::NoMarker));
    }

    #[test]
    fn marker_must_be_exact_first_token() {
        // A trailing/prefixed look-alike is not the selector.
        assert_eq!(
            parse(&toks(&["--stdout-bytes", "1", MARKER])),
            Err(ParseError::NoMarker)
        );
        assert_eq!(
            parse(&toks(&[&format!("{MARKER}x")])),
            Err(ParseError::NoMarker)
        );
    }

    // === parser: bounds ======================================================

    #[test]
    fn maxima_are_accepted() {
        let r = parse(&toks(&[
            MARKER,
            "--stdout-bytes",
            &MAX_STDOUT_BYTES.to_string(),
            "--stderr-bytes",
            &MAX_STDERR_BYTES.to_string(),
            "--sleep-ms",
            &MAX_SLEEP_MS.to_string(),
            "--exit-code",
            &MAX_EXIT_CODE.to_string(),
        ]))
        .unwrap();
        assert_eq!(r.stdout_bytes, MAX_STDOUT_BYTES);
        assert_eq!(r.stderr_bytes, MAX_STDERR_BYTES);
        assert_eq!(r.sleep_ms, MAX_SLEEP_MS);
        assert_eq!(r.exit_code, MAX_EXIT_CODE);
    }

    #[test]
    fn above_maxima_are_out_of_range() {
        assert!(matches!(
            parse(&toks(&[
                MARKER,
                "--stdout-bytes",
                &(MAX_STDOUT_BYTES + 1).to_string()
            ])),
            Err(ParseError::OutOfRange {
                key: "stdout-bytes",
                max: MAX_STDOUT_BYTES,
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[
                MARKER,
                "--stderr-bytes",
                &(MAX_STDERR_BYTES + 1).to_string()
            ])),
            Err(ParseError::OutOfRange {
                key: "stderr-bytes",
                max: MAX_STDERR_BYTES,
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[
                MARKER,
                "--sleep-ms",
                &(MAX_SLEEP_MS + 1).to_string()
            ])),
            Err(ParseError::OutOfRange {
                key: "sleep-ms",
                max: MAX_SLEEP_MS,
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", &(MAX_EXIT_CODE + 1).to_string()])),
            Err(ParseError::OutOfRange { key: "exit-code", max, .. }) if max == MAX_EXIT_CODE as u64
        ));
    }

    #[test]
    fn exit_code_range_is_portable() {
        // Boundaries accepted.
        parse(&toks(&[MARKER, "--exit-code", "0"])).unwrap();
        parse(&toks(&[MARKER, "--exit-code", "125"])).unwrap();
        // Above the portable ceiling (126/127/128+N are shell-reserved).
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "126"])),
            Err(ParseError::OutOfRange {
                key: "exit-code",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "255"])),
            Err(ParseError::OutOfRange {
                key: "exit-code",
                ..
            })
        ));
        // Negative is out-of-range, not a parse error.
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "-1"])),
            Err(ParseError::OutOfRange {
                key: "exit-code",
                ..
            })
        ));
    }

    // === parser: strictness ==================================================

    #[test]
    fn unknown_argument_rejected() {
        assert!(matches!(
            parse(&toks(&[MARKER, "--bogus", "1"])),
            Err(ParseError::UnknownArgument { arg }) if arg == "--bogus"
        ));
        // A bare positional token is also an unknown argument.
        assert!(matches!(
            parse(&toks(&[MARKER, "stray"])),
            Err(ParseError::UnknownArgument { arg }) if arg == "stray"
        ));
    }

    #[test]
    fn duplicate_flags_rejected() {
        assert!(matches!(
            parse(&toks(&[
                MARKER,
                "--stdout-bytes",
                "1",
                "--stdout-bytes",
                "2"
            ])),
            Err(ParseError::DuplicateKey {
                key: "stdout-bytes"
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "1", "--exit-code", "2"])),
            Err(ParseError::DuplicateKey { key: "exit-code" })
        ));
    }

    #[test]
    fn missing_value_rejected() {
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes"])),
            Err(ParseError::MissingValue {
                key: "stdout-bytes"
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code"])),
            Err(ParseError::MissingValue { key: "exit-code" })
        ));
    }

    #[test]
    fn invalid_numeric_values_rejected() {
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes", "abc"])),
            Err(ParseError::InvalidValue {
                key: "stdout-bytes",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes", "1.5"])),
            Err(ParseError::InvalidValue {
                key: "stdout-bytes",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes", "-5"])),
            Err(ParseError::InvalidValue {
                key: "stdout-bytes",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--sleep-ms", "later"])),
            Err(ParseError::InvalidValue {
                key: "sleep-ms",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "x"])),
            Err(ParseError::InvalidValue {
                key: "exit-code",
                ..
            })
        ));
    }

    #[test]
    fn parse_error_display_is_bounded() {
        // A huge token must not bloat the diagnostic.
        let huge = "x".repeat(1000);
        let e = ParseError::UnknownArgument { arg: snip(&huge) };
        let s = e.to_string();
        assert!(s.len() < 200, "display is bounded, got {} bytes", s.len());
        // {:?} quotes the snippet; the truncation marker is present (not the
        // last char).
        assert!(s.contains('…'));
        // Spot-check the other displays render without panic and stay short.
        assert!(ParseError::NoMarker.to_string().contains(MARKER));
        assert!(
            ParseError::MissingValue { key: "sleep-ms" }
                .to_string()
                .contains("sleep-ms")
        );
        assert!(
            ParseError::DuplicateKey { key: "exit-code" }
                .to_string()
                .contains("duplicate")
        );
        assert!(
            ParseError::OutOfRange {
                key: "stdout-bytes",
                value: snip("99999999999"),
                max: MAX_STDOUT_BYTES,
            }
            .to_string()
            .contains("out of range")
        );
    }

    // === parser safety: snippet truncation & strict numeric forms ===========

    #[test]
    fn snip_never_splits_a_multibyte_codepoint() {
        // Case 1: byte SNIPPET_MAX (64) falls in the MIDDLE of a 3-byte
        // codepoint — 'Ａ' (U+FF21) occupies bytes 62..65. A naive `s[..64]`
        // slice would PANIC; snip must back off to byte 62.
        let s1 = "a".repeat(62) + "ＡＢＣＤＥ";
        assert_eq!(s1.len(), 77);
        let got = snip(&s1);
        let body = got
            .strip_suffix('…')
            .expect("ellipsis appended on truncation");
        assert!(
            body.len() <= SNIPPET_MAX,
            "body must be <= {SNIPPET_MAX} bytes, got {}",
            body.len()
        );
        assert_eq!(body.len(), 62, "cut backs off to the codepoint start");
        assert_eq!(body, &"a".repeat(62));
        assert!(body.is_char_boundary(body.len()));

        // Case 2: a long run of 3-byte codepoints only; byte 64 is mid-codepoint.
        let s2 = "Ａ".repeat(40); // 120 bytes, all 3-byte
        let got2 = snip(&s2);
        let body2 = got2.strip_suffix('…').unwrap();
        assert!(body2.len() <= SNIPPET_MAX);
        assert_eq!(body2.len(), 63); // 21 codepoints * 3 bytes
        assert_eq!(body2.chars().count(), 21);

        // Case 3: 4-byte codepoints (emoji) with a 1-byte prefix so byte 64
        // lands inside a 4-byte sequence (boundaries at 1, 5, …, 61, 65).
        let s3 = format!("{}{}", "z", "😀".repeat(40));
        let got3 = snip(&s3);
        let body3 = got3.strip_suffix('…').unwrap();
        assert!(body3.len() <= SNIPPET_MAX);
        assert_eq!(body3.len(), 61); // 1 + 15 * 4
        assert_eq!(body3.chars().count(), 16);

        // Case 4: a string at or under the cap is returned verbatim (no
        // ellipsis), including pure-multibyte strings sitting at the boundary.
        let exact = "Ａ".repeat(21); // 63 bytes <= SNIPPET_MAX
        assert_eq!(snip(&exact), exact);
        let just_over = "Ａ".repeat(22); // 66 bytes > SNIPPET_MAX
        assert!(snip(&just_over).ends_with('…'));
    }

    #[test]
    fn check_u64_requires_nonempty_ascii_digits() {
        use ParseError::InvalidValue;
        // Plain ASCII digits parse (incl. leading zeros, which are shape-valid).
        assert_eq!(check_u64("0", "k", 10).unwrap(), 0);
        assert_eq!(check_u64("007", "k", 1_000_000).unwrap(), 7);
        // Empty and every non-pure-ASCII-digit form is InvalidValue: `+1` (which
        // std would accept), signs, whitespace, underscores, hex/float syntax,
        // and Unicode digits (Arabic-Indic `١٢٣`, full-width `１２３`, roman `Ⅷ`).
        for bad in [
            "",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1_000",
            "0x10",
            "1.5",
            "abc",
            "١٢٣",
            "１２３",
            "Ⅷ",
        ] {
            assert!(
                matches!(
                    check_u64(bad, "k", u64::MAX),
                    Err(InvalidValue { key: "k", .. })
                ),
                "check_u64({bad:?}) should be InvalidValue"
            );
        }
        // A purely-numeric value that overflows u64 stays InvalidValue (it
        // never becomes a usable magnitude, so OutOfRange would be misleading).
        let overflow = "9".repeat(40);
        assert!(matches!(
            check_u64(&overflow, "k", u64::MAX),
            Err(InvalidValue { key: "k", .. })
        ));
        // Valid digits above the field cap are still OutOfRange.
        assert!(matches!(
            check_u64("11", "k", 10),
            Err(ParseError::OutOfRange {
                key: "k",
                max: 10,
                ..
            })
        ));
    }

    #[test]
    fn check_exit_minus_is_parseable_but_out_of_range() {
        use ParseError::{InvalidValue, OutOfRange};
        // In-range ASCII digits accepted.
        assert_eq!(check_exit("0").unwrap(), 0);
        assert_eq!(check_exit("125").unwrap(), 125);
        // A single leading '-' parses, so genuine negatives are OutOfRange
        // (reported by magnitude, not as a parse failure).
        assert!(matches!(
            check_exit("-1"),
            Err(OutOfRange {
                key: "exit-code",
                ..
            })
        ));
        assert!(matches!(
            check_exit("-125"),
            Err(OutOfRange {
                key: "exit-code",
                ..
            })
        ));
        // Above the portable ceiling is OutOfRange.
        assert!(matches!(
            check_exit("126"),
            Err(OutOfRange {
                key: "exit-code",
                ..
            })
        ));
        // Everything else is InvalidValue: '+1', a stray '-', whitespace, junk,
        // Unicode digits, and misplaced/doubled signs.
        for bad in [
            "", "+1", "-", " 1", "1 ", "x", "١٢٣", "12-3", "--1", "- 1", "-١",
        ] {
            assert!(
                matches!(
                    check_exit(bad),
                    Err(InvalidValue {
                        key: "exit-code",
                        ..
                    })
                ),
                "check_exit({bad:?}) should be InvalidValue"
            );
        }
        // Numeric but overflowing i64 -> InvalidValue (both signs).
        let overflow_pos = "9".repeat(40);
        let overflow_neg = format!("-{}", "9".repeat(40));
        assert!(matches!(
            check_exit(&overflow_pos),
            Err(InvalidValue {
                key: "exit-code",
                ..
            })
        ));
        assert!(matches!(
            check_exit(&overflow_neg),
            Err(InvalidValue {
                key: "exit-code",
                ..
            })
        ));
    }

    #[test]
    fn parse_rejects_plus_sign_whitespace_and_unicode_digits() {
        // Regression: `+1` was previously accepted by the std parser for both
        // unsigned fields and exit-code; it is now InvalidValue everywhere.
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes", "+1"])),
            Err(ParseError::InvalidValue {
                key: "stdout-bytes",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--stderr-bytes", "+1"])),
            Err(ParseError::InvalidValue {
                key: "stderr-bytes",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--sleep-ms", "+1"])),
            Err(ParseError::InvalidValue {
                key: "sleep-ms",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "+1"])),
            Err(ParseError::InvalidValue {
                key: "exit-code",
                ..
            })
        ));
        // Surrounding whitespace and Unicode (Arabic-Indic) digits are
        // InvalidValue for unsigned fields, not silently accepted.
        assert!(matches!(
            parse(&toks(&[MARKER, "--sleep-ms", " 5"])),
            Err(ParseError::InvalidValue {
                key: "sleep-ms",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--sleep-ms", "5 "])),
            Err(ParseError::InvalidValue {
                key: "sleep-ms",
                ..
            })
        ));
        assert!(matches!(
            parse(&toks(&[MARKER, "--stdout-bytes", "١٢٣"])),
            Err(ParseError::InvalidValue {
                key: "stdout-bytes",
                ..
            })
        ));
        // exit-code negative still reaches the range check and is OutOfRange.
        assert!(matches!(
            parse(&toks(&[MARKER, "--exit-code", "-1"])),
            Err(ParseError::OutOfRange {
                key: "exit-code",
                ..
            })
        ));
    }

    // === execute: deterministic bytes & chunk boundaries ====================

    #[test]
    fn output_is_the_pure_positional_pattern() {
        for &n in &[0u64, 1, 2, 25, 26, 27, CHUNK as u64, (CHUNK as u64) * 2 + 7] {
            let plan = ChildPlan {
                stdout_bytes: n,
                stderr_bytes: 0,
                sleep_ms: 0,
                exit_code: 0,
            };
            let mut so = Vec::new();
            let mut se = Vec::new();
            assert_eq!(execute(&plan, &mut so, &mut se).unwrap(), 0);
            assert_eq!(so, expected_pattern(n));
            assert!(se.is_empty());
        }
    }

    #[test]
    fn short_writes_do_not_change_the_bytes() {
        // Force write_all to loop one byte at a time; the result must still be
        // the exact deterministic pattern (chunk-boundary invariance).
        let n: u64 = (CHUNK as u64) * 3 + 11;
        let plan = ChildPlan {
            stdout_bytes: n,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = ShortWriteSink::new(1);
        let mut se = Vec::new();
        execute(&plan, &mut so, &mut se).unwrap();
        assert_eq!(so.out, expected_pattern(n));
        assert!(se.is_empty());

        // And with an awkward 3-byte step.
        let mut so3 = ShortWriteSink::new(3);
        execute(&plan, &mut so3, &mut se).unwrap();
        assert_eq!(so3.out, expected_pattern(n));
    }

    // === execute: literal byte regression at pattern & chunk boundaries =====

    #[test]
    fn byte_at_matches_literals_at_boundary_offsets() {
        // Hand-computed literals (NOT derived from byte_at) for the documented
        // pattern byte[i] = b'a' + (i % 26). These are exactly the offsets
        // where the two regressions hide:
        //   * the cast-before-modulo bug (visible at 256+, where `i as u8 != i`),
        //     and
        //   * the repeat-a-fixed-chunk bug (visible at the 8192-byte chunk seam,
        //     since 8192 % 26 = 2 != 0).
        //   off % 26:  25->25 26->0 255->21 256->22 8191->1 8192->2 8193->3
        let cases: [(u64, u8); 7] = [
            (25, b'z'),
            (26, b'a'),
            (255, b'v'),
            (256, b'w'),
            (8191, b'b'),
            (8192, b'c'),
            (8193, b'd'),
        ];
        for (off, want) in cases {
            assert_eq!(ChildPlan::byte_at(off), want, "byte_at({off})");
        }
    }

    #[test]
    fn emitted_stream_is_correct_at_offsets_and_across_chunk_seams() {
        // Span three full chunks plus a tail so every 8192-byte seam is
        // exercised. The oracles here are INDEPENDENT of ChildPlan::byte_at:
        // hand-computed literals at the boundary offsets and an inline copy of
        // the documented formula for the full scan, so neither a byte_at nor a
        // write_n regression can mask itself.
        let n: u64 = (CHUNK as u64) * 3 + 10;
        let plan = ChildPlan {
            stdout_bytes: n,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = Vec::new();
        let mut se = Vec::new();
        assert_eq!(execute(&plan, &mut so, &mut se).unwrap(), 0);
        assert_eq!(so.len() as u64, n);
        assert!(se.is_empty());

        // Literal assertions at the documented boundary offsets, including the
        // two chunk-seam bytes (8192/8193) inside this multi-chunk stream.
        //   off % 26:  25->25 26->0 255->21 256->22 8191->1 8192->2 8193->3
        let literals: [(u64, u8); 7] = [
            (25, b'z'),
            (26, b'a'),
            (255, b'v'),
            (256, b'w'),
            (8191, b'b'),
            (8192, b'c'),
            (8193, b'd'),
        ];
        for (off, want) in literals {
            assert_eq!(
                so[off as usize], want,
                "emitted byte at absolute offset {off}"
            );
        }

        // Full multi-chunk scan against the inline documented formula — NOT
        // byte_at — so every seam (8192, 16384, 24576) is checked, not just the
        // sampled offsets above.
        for (idx, &b) in so.iter().enumerate() {
            let off = idx as u64;
            assert_eq!(
                b,
                b'a' + ((off % 26) as u8),
                "emitted byte at absolute offset {off}"
            );
        }
    }

    // === execute: simultaneous sink behavior =================================

    #[test]
    fn both_sinks_receive_exact_amounts() {
        let plan = ChildPlan {
            stdout_bytes: 1000,
            stderr_bytes: 500,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = Vec::new();
        let mut se = Vec::new();
        execute(&plan, &mut so, &mut se).unwrap();
        assert_eq!(so.len(), 1000);
        assert_eq!(se.len(), 500);
        assert_eq!(so, expected_pattern(1000));
        assert_eq!(se, expected_pattern(500));
    }

    #[test]
    fn large_plan_to_both_sinks_is_exact() {
        let n: u64 = 2 * 1024 * 1024; // 2 MiB each
        let plan = ChildPlan {
            stdout_bytes: n,
            stderr_bytes: n,
            sleep_ms: 0,
            exit_code: 99,
        };
        let mut so = Vec::with_capacity(n as usize);
        let mut se = Vec::with_capacity(n as usize);
        let code = execute(&plan, &mut so, &mut se).unwrap();
        assert_eq!(code, 99);
        assert_eq!(so.len() as u64, n);
        assert_eq!(se.len() as u64, n);
        // Pattern holds at the seam and at the tail.
        assert_eq!(so[0], ChildPlan::byte_at(0));
        assert_eq!(so[n as usize - 1], ChildPlan::byte_at(n - 1));
    }

    // === execute: no large allocation =======================================

    #[test]
    fn executor_never_hands_more_than_chunk_per_write() {
        // Request several MiB; the executor must emit it via fixed CHUNK-sized
        // writes, never a requested-size buffer.
        let n: u64 = 4 * 1024 * 1024;
        let plan = ChildPlan {
            stdout_bytes: n,
            stderr_bytes: n / 2,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = RecordingSink::default();
        let mut se = RecordingSink::default();
        execute(&plan, &mut so, &mut se).unwrap();
        assert_eq!(so.out.len() as u64, n);
        assert_eq!(se.out.len() as u64, n / 2);
        assert!(
            so.max_write <= CHUNK,
            "stdout max_write={}, CHUNK={}",
            so.max_write,
            CHUNK
        );
        assert!(
            se.max_write <= CHUNK,
            "stderr max_write={}, CHUNK={}",
            se.max_write,
            CHUNK
        );
    }

    // === execute: exit code & sleep =========================================

    #[test]
    fn execute_returns_selected_exit_code_without_process_exit() {
        let plan = ChildPlan {
            stdout_bytes: 0,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 42,
        };
        let mut so = Vec::new();
        let mut se = Vec::new();
        assert_eq!(execute(&plan, &mut so, &mut se).unwrap(), 42);
        assert!(so.is_empty());
        assert!(se.is_empty());
    }

    #[test]
    fn execute_tolerates_a_small_sleep() {
        let plan = ChildPlan {
            stdout_bytes: 4,
            stderr_bytes: 0,
            sleep_ms: 1,
            exit_code: 0,
        };
        let mut so = Vec::new();
        let mut se = Vec::new();
        let start = std::time::Instant::now();
        execute(&plan, &mut so, &mut se).unwrap();
        let elapsed = start.elapsed();
        assert!(so == expected_pattern(4));
        // The sleep actually happened (>= ~1ms) but stayed bounded.
        assert!(elapsed >= Duration::from_millis(1));
        assert!(elapsed < Duration::from_millis(MAX_SLEEP_MS));
    }

    // === execute: structured errors (broken pipe) ===========================

    #[test]
    fn broken_stdout_is_structured() {
        let plan = ChildPlan {
            stdout_bytes: 16,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = FailingSink {
            kind: ErrorKind::BrokenPipe,
        };
        let mut se = Vec::new();
        assert_eq!(
            execute(&plan, &mut so, &mut se),
            Err(ExecError::BrokenPipe {
                stream: Stream::Stdout
            })
        );
    }

    #[test]
    fn broken_stderr_is_structured_and_stdout_already_written() {
        // stdout succeeds fully; stderr breaks -> BrokenPipe on stderr.
        let plan = ChildPlan {
            stdout_bytes: 8,
            stderr_bytes: 8,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = Vec::new();
        let mut se = FailingSink {
            kind: ErrorKind::BrokenPipe,
        };
        assert_eq!(
            execute(&plan, &mut so, &mut se),
            Err(ExecError::BrokenPipe {
                stream: Stream::Stderr
            })
        );
        // stdout was already written before stderr failed.
        assert_eq!(so, expected_pattern(8));
    }

    #[test]
    fn other_write_failure_is_structured_with_kind() {
        let plan = ChildPlan {
            stdout_bytes: 16,
            stderr_bytes: 0,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = FailingSink {
            kind: ErrorKind::PermissionDenied,
        };
        let mut se = Vec::new();
        assert_eq!(
            execute(&plan, &mut so, &mut se),
            Err(ExecError::WriteFailed {
                stream: Stream::Stdout,
                kind: ErrorKind::PermissionDenied
            })
        );
        // ExecError display is meaningful.
        let s = ExecError::WriteFailed {
            stream: Stream::Stdout,
            kind: ErrorKind::Other,
        }
        .to_string();
        assert!(s.contains("stdout") && s.contains("write"));
    }

    #[test]
    fn zero_byte_plan_writes_nothing_and_returns_exit_code() {
        let plan = ChildPlan::default();
        let mut so = Vec::new();
        let mut se = Vec::new();
        assert_eq!(execute(&plan, &mut so, &mut se).unwrap(), 0);
        assert!(so.is_empty());
        assert!(se.is_empty());
    }

    #[test]
    fn flush_failure_is_reported_even_when_no_bytes_written() {
        // No output requested, but the sink still fails on flush: the executor
        // flushes both sinks after writing, so the failure is surfaced as a
        // structured error rather than swallowed.
        let plan = ChildPlan::default();
        let mut so = FailingSink {
            kind: ErrorKind::BrokenPipe,
        };
        let mut se = Vec::new();
        assert_eq!(
            execute(&plan, &mut so, &mut se),
            Err(ExecError::BrokenPipe {
                stream: Stream::Stdout
            })
        );
        assert!(se.is_empty());
    }

    // === parse + execute round trip ==========================================

    #[test]
    fn parse_then_execute_round_trip() {
        let plan = parse(&toks(&[
            MARKER,
            "--stdout-bytes",
            "30000",
            "--stderr-bytes",
            "10000",
            "--exit-code",
            "17",
        ]))
        .unwrap();
        let mut so = Vec::new();
        let mut se = Vec::new();
        let code = execute(&plan, &mut so, &mut se).unwrap();
        assert_eq!(code, 17);
        assert_eq!(so, expected_pattern(30000));
        assert_eq!(se, expected_pattern(10000));
    }

    // === execute: concurrent stream generation (concurrency fix) ============

    /// A sink that, on its FIRST `write`, blocks at a shared 2-way barrier until
    /// the OTHER stream's sink has ALSO begun writing. Under CONCURRENT
    /// [`execute`] the two workers reach the barrier independently and both are
    /// released; under a sequential impl (all stdout, then all stderr) the first
    /// sink's write would block forever waiting for a second sink that never
    /// runs. The barrier's own contract — "all threads have reached the barrier
    /// before any are released" — is the proof that BOTH streams began before
    /// EITHER was released.
    #[test]
    fn both_streams_begin_before_either_is_released() {
        use std::sync::mpsc;
        use std::sync::{Arc, Barrier};

        struct BarrierSink {
            gate: Arc<Barrier>,
            out: Vec<u8>,
            began: bool,
        }
        impl Write for BarrierSink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if !self.began {
                    self.began = true;
                    // Blocks until the other stream's sink has begun too.
                    self.gate.wait();
                }
                self.out.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let gate = Arc::new(Barrier::new(2));
        let plan = ChildPlan {
            stdout_bytes: 13,
            stderr_bytes: 17,
            sleep_ms: 0,
            exit_code: 5,
        };
        let (tx, rx) = mpsc::channel();

        let so_gate = gate.clone();
        let se_gate = gate;
        // Run execute on a helper thread so a sequential-impl deadlock surfaces
        // as a bounded timeout instead of hanging the whole test binary.
        thread::spawn(move || {
            let mut so = BarrierSink {
                gate: so_gate,
                out: Vec::new(),
                began: false,
            };
            let mut se = BarrierSink {
                gate: se_gate,
                out: Vec::new(),
                began: false,
            };
            let r = execute(&plan, &mut so, &mut se);
            let _ = tx.send((r, so.out, se.out));
        });

        let (r, out, err) = rx.recv_timeout(Duration::from_secs(5)).expect(
            "execute deadlocked: streams were not generated concurrently (a \
                 sequential impl blocks forever on the 2-way barrier)",
        );
        assert_eq!(r.unwrap(), 5);
        assert_eq!(out, expected_pattern(13));
        assert_eq!(err, expected_pattern(17));
    }

    /// Both streams fail; the result must be deterministic stdout-first
    /// precedence regardless of which worker errors first in wall-clock time.
    #[test]
    fn both_streams_error_reports_stdout_first() {
        let plan = ChildPlan {
            stdout_bytes: 8,
            stderr_bytes: 8,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = FailingSink {
            kind: ErrorKind::PermissionDenied,
        };
        let mut se = FailingSink {
            kind: ErrorKind::BrokenPipe,
        };
        // stdout's distinct error (PermissionDenied) wins over stderr's
        // (BrokenPipe) by precedence — never the reverse.
        assert_eq!(
            execute(&plan, &mut so, &mut se),
            Err(ExecError::WriteFailed {
                stream: Stream::Stdout,
                kind: ErrorKind::PermissionDenied
            })
        );
    }

    /// A worker panic is caught at the join and surfaced as the stable
    /// structured [`ExecError::WorkerPanicked`] for that stream instead of unwinding
    /// through [`execute`] or [`thread::scope`].
    #[test]
    fn worker_panic_becomes_structured_error() {
        struct PanickingSink;
        impl Write for PanickingSink {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                panic!("synthetic panic inside stream worker");
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let plan = ChildPlan {
            stdout_bytes: 8,
            stderr_bytes: 8,
            sleep_ms: 0,
            exit_code: 0,
        };
        let mut so = PanickingSink;
        let mut se = Vec::new();

        // Silence the default panic hook for just this call so the synthetic
        // worker panic does not clutter test output; the real assertion is that
        // execute RETURNS a structured error rather than unwinding. Restored
        // unconditionally right after.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = execute(&plan, &mut so, &mut se);
        std::panic::set_hook(prev_hook);

        assert_eq!(
            r,
            Err(ExecError::WorkerPanicked {
                stream: Stream::Stdout
            })
        );
        // The non-panicking stderr worker ran independently and completed (join
        // happens on every path), so its deterministic bytes are still present.
        assert_eq!(se, expected_pattern(8));
    }
}
