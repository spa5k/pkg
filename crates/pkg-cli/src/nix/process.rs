//! Child process supervision: one signal forward and reap boundary.
//!
//! Every external child of pkg — native Nix commands and the macOS app
//! helper alike — runs through [`run_child`]. Termination signals sent only
//! to pkg are forwarded to the registered child, the child is reaped, and
//! pkg stays alive to re-read state and report. Every reaped run is
//! classified exactly once, into [`Outcome`], by the code that reaped it.

use std::process::{Child, Command, ExitStatus, Output, Stdio};

/// How a native child's stdio is wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IoMode {
    /// Capture stdout/stderr through pipes.
    Capture,
    /// Stream stdio to the user's terminal.
    Stream,
    /// Inherit stdin/stdout, pipe stderr, and feed it live to a sink.
    Filtered,
}

/// One reaped child: its complete output record and cancellation record.
///
/// The same shape serves captured and streamed children. `wait_with_output`
/// waits identically when the streams are inherited; the buffers are then
/// empty, and the exit status is what streamed callers need.
pub(super) struct Reaped {
    /// The child's output record; stdout and stderr buffers are empty for
    /// streamed children.
    pub(super) output: Output,
    /// A termination signal pkg received and forwarded to the child during
    /// the run, when one was.
    ///
    /// It is captured before the forwarding dispositions are restored,
    /// because classification happens after restoration and the exit status
    /// alone cannot prove cancellation: the verified runtime handles SIGINT
    /// itself and then exits with a plain code 1.
    pub(super) cancelled: Option<i32>,
    pub(super) resource_limit: Option<&'static str>,
}

impl Reaped {
    /// Classify this run into the one shared outcome.
    ///
    /// Cancellation wins over exit status: a child that was signalled — or
    /// that handled SIGINT and exited 130 — is an interruption, not a plain
    /// failure, so callers re-read native state instead of trusting the
    /// exit code.
    pub(super) fn classify(&self) -> Outcome {
        if let Some(signal) = self.cancelled.or_else(|| unix_signal(&self.output.status)) {
            return Outcome::Interrupted { signal };
        }
        if self.output.status.success() {
            return Outcome::Success;
        }
        Outcome::Failed {
            status: exit_status_text(&self.output.status),
            stderr: String::from_utf8_lossy(&self.output.stderr).into_owned(),
        }
    }
}

/// The classified outcome of one external child through the boundary.
///
/// One representation serves native Nix commands (wrapped into
/// [`super::NixError`] with their argument vector) and direct helper
/// commands (returned as is).
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The command exited successfully.
    Success,
    /// The command was cancelled by a termination signal during the run.
    ///
    /// The child was reaped and pkg stayed alive; the operation may have
    /// partially applied, so callers must finish their own bookkeeping
    /// and report.
    Interrupted {
        /// The terminating signal number.
        signal: i32,
    },
    /// The command exited with a failure.
    Failed {
        /// The exit status text.
        status: String,
        /// Captured stderr, when the caller captured it.
        stderr: String,
    },
}

#[cfg(unix)]
mod forward {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Signals that cancel a native child and are forwarded to it.
    const TERMINATION: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The pid of the child signals are currently forwarded to (0 = none).
    static CHILD_PID: AtomicI32 = AtomicI32::new(0);
    /// The last termination signal received by pkg during a run (0 = none).
    static RECEIVED: AtomicI32 = AtomicI32::new(0);
    /// Whether forwarding dispositions are installed right now.
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    /// Only one child may be registered at a time.
    static RUN_LOCK: Mutex<()> = Mutex::new(());

    extern "C" fn forward_to_child(signum: libc::c_int) {
        // Async-signal-safe: atomic loads/stores and `kill` only.
        let pid = CHILD_PID.load(Ordering::Relaxed);
        if pid > 0 {
            // SAFETY: `kill` sends `signum` to exactly the registered pid.
            unsafe { libc::kill(pid, signum) };
        }
        RECEIVED.store(signum, Ordering::Relaxed);
    }

    fn empty_action(handler: usize) -> libc::sigaction {
        // SAFETY: an all-zero `sigaction` is a valid empty mask and no
        // flags; the handler field is then assigned explicitly.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        action
    }

    /// Install forwarding dispositions for the termination signals.
    ///
    /// A signal sent only to the pkg pid is delivered to the handler and
    /// forwarded to the registered child, so the child is cancelled while
    /// pkg stays alive to reap it and re-read state.
    pub(super) fn install() -> Option<Guard> {
        let mut previous = Vec::new();
        for signum in TERMINATION {
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: registers `forward_to_child` for `signum` and captures
            // the previous disposition in `old`.
            let handler = forward_to_child as *const () as usize;
            if unsafe { libc::sigaction(signum, &empty_action(handler), &mut old) } != 0 {
                for (changed, old) in &previous {
                    // SAFETY: restores dispositions captured above.
                    unsafe { libc::sigaction(*changed, old, std::ptr::null_mut()) };
                }
                return None;
            }
            previous.push((signum, old));
        }
        RECEIVED.store(0, Ordering::Relaxed);
        ACTIVE.store(true, Ordering::Relaxed);
        Some(Guard(previous))
    }

    /// Restores the previous dispositions when the run finishes.
    pub(super) struct Guard(Vec<(libc::c_int, libc::sigaction)>);

    impl Guard {
        pub(super) fn restore(self) {
            for (signum, old) in self.0 {
                // SAFETY: restores a previously captured disposition.
                unsafe { libc::sigaction(signum, &old, std::ptr::null_mut()) };
            }
            ACTIVE.store(false, Ordering::Relaxed);
            RECEIVED.store(0, Ordering::Relaxed);
        }
    }

    /// Block termination signals, closing the spawn/registration race.
    pub(super) fn block() {
        // SAFETY: builds a signal set from constants and blocks it for this
        // thread only; pending signals are delivered on unblock.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signum in TERMINATION {
                libc::sigaddset(&mut set, signum);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
    }

    /// Unblock termination signals after the child is registered.
    pub(super) fn unblock() {
        // SAFETY: unblocks exactly the set `block` blocked.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signum in TERMINATION {
                libc::sigaddset(&mut set, signum);
            }
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        }
    }

    /// Register the child signals are forwarded to. Callers hold the lock.
    pub(super) fn register(pid: u32) {
        CHILD_PID.store(i32::try_from(pid).unwrap_or(-1), Ordering::Relaxed);
    }

    /// Clear the registered child.
    pub(super) fn unregister() {
        CHILD_PID.store(0, Ordering::Relaxed);
    }

    /// The received signal number, when one was received.
    pub(super) fn received_signal() -> Option<i32> {
        let signum = RECEIVED.load(Ordering::Relaxed);
        (signum != 0 && ACTIVE.load(Ordering::Relaxed)).then_some(signum)
    }

    /// Serialize child registration; only one child runs at a time.
    pub(super) fn lock() -> std::sync::MutexGuard<'static, ()> {
        RUN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A live consumer of one filtered child's stderr chunks.
pub(super) trait StderrSink {
    /// One raw chunk in arrival order.
    fn chunk(&mut self, bytes: &[u8]);
    /// Called once after the child has exited and stderr reached EOF.
    fn finish(&mut self, success: bool);
}

/// Drain one filtered child: stdin/stdout are inherited; stderr is read
/// chunk by chunk through the sink while the child runs, and the complete
/// raw stderr is preserved in the output record so failures keep their
/// full native diagnostics.
///
/// An interrupted read is not a failure: a termination signal forwarded to
/// the child also interrupts this thread's `read`, so the read resumes and
/// the child decides when the pipe closes. That is what keeps a cancelled
/// run classifiable instead of collapsing into a spawn-style error; only
/// `ErrorKind::Interrupted` is retried, and a retry re-reads the same pipe,
/// so it never repeats a mutation. A genuinely failed pipe kills the child
/// before the wait, so this drain reaps on every one of its own paths and
/// the wait after a kill cannot hang.
fn read_filtered(
    mut child: Child,
    mut sink: Option<&mut dyn StderrSink>,
) -> Result<Output, std::io::Error> {
    use std::io::Read as _;
    let mut raw = Vec::new();
    let mut pipe_failed: Option<std::io::Error> = None;
    if let Some(mut pipe) = child.stderr.take() {
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    raw.extend_from_slice(&buffer[..read]);
                    if let Some(sink) = sink.as_deref_mut() {
                        sink.chunk(&buffer[..read]);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(error) => {
                    pipe_failed = Some(error);
                    break;
                }
            }
        }
    }
    if pipe_failed.is_some() {
        // A dead pipe must not leave a live child behind the error: kill
        // first, so the wait below cannot block on a running child.
        let _ = child.kill();
    }
    // `Child::wait` resumes interrupted waits itself, so one call reaps.
    let status = child.wait()?;
    if let Some(sink) = sink {
        sink.finish(status.success() && pipe_failed.is_none());
    }
    match pipe_failed {
        Some(error) => Err(error),
        None => Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: raw,
        }),
    }
}

/// Spawn one prepared command through the shared signal forward/reap
/// boundary, wait for it, and reap it. A spawn failure restores the
/// forwarding dispositions before returning; a filtered run reaps on
/// every path inside [`read_filtered`]; captured and streamed runs wait
/// through `wait_with_output`, whose internal read or wait error would
/// propagate here without an extra reap.
///
/// The same boundary serves native Nix children and direct external
/// commands: termination signals sent only to pkg are forwarded to the
/// child, the child is reaped, and pkg stays alive.
pub(super) fn run_child(
    command: Command,
    mode: IoMode,
    sink: Option<&mut dyn StderrSink>,
) -> Result<Reaped, String> {
    run_child_with_limit(command, mode, sink, None)
}

pub(super) fn run_child_bounded(command: Command, bytes: u64) -> Result<Reaped, String> {
    run_child_with_limit(command, IoMode::Capture, None, Some(bytes))
}

fn run_child_with_limit(
    mut command: Command,
    mode: IoMode,
    sink: Option<&mut dyn StderrSink>,
    memory_limit: Option<u64>,
) -> Result<Reaped, String> {
    #[cfg(unix)]
    let _run = forward::lock();
    #[cfg(unix)]
    let guard = forward::install();
    #[cfg(unix)]
    forward::block();
    #[cfg(unix)]
    if memory_limit.is_some() {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let filtered = mode == IoMode::Filtered;
    let spawn = command
        .stdin(if mode == IoMode::Capture {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(if mode == IoMode::Capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(if mode == IoMode::Stream {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .spawn();
    let child = match spawn {
        Ok(child) => child,
        Err(error) => {
            #[cfg(unix)]
            forward::unblock();
            if let Some(guard) = guard {
                guard.restore();
            }
            return Err(error.to_string());
        }
    };
    #[cfg(unix)]
    forward::register(child.id());
    #[cfg(unix)]
    forward::unblock();
    // Unfiltered children wait through wait_with_output; filtered ones
    // drain stderr through the sink while the child runs.
    let mut resource_limit = None;
    let waited = if let Some(bytes) = memory_limit {
        let (output, limit) = read_bounded(child, bytes);
        resource_limit = limit;
        output
    } else if filtered {
        read_filtered(child, sink)
    } else {
        child.wait_with_output()
    };
    #[cfg(unix)]
    forward::unregister();
    // Read the forwarded-signal record before the dispositions are
    // restored: `restore` clears it, and classification happens later.
    #[cfg(unix)]
    let cancelled = forward::received_signal();
    #[cfg(not(unix))]
    let cancelled: Option<i32> = None;
    if let Some(guard) = guard {
        guard.restore();
    }
    waited
        .map(|output| Reaped {
            output,
            cancelled,
            resource_limit,
        })
        .map_err(|error| error.to_string())
}

fn read_bounded(
    mut child: Child,
    limit: u64,
) -> (Result<Output, std::io::Error>, Option<&'static str>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let overflow = AtomicBool::new(false);
    let stop_at = limit.saturating_sub(32 * 1024 * 1024);
    let mut stopped = None;
    let output = std::thread::scope(|scope| {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out = scope.spawn(|| capped_read(stdout, &overflow));
        let err = scope.spawn(|| capped_read(stderr, &overflow));
        let pid = child.id();
        let start = Instant::now();
        let mut unmeasured_since = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(error);
                }
                Ok(None) => {}
            }
            let reason = if overflow.load(Ordering::Relaxed) {
                Some("metadata output exceeded 16 MiB")
            } else if start.elapsed() > Duration::from_secs(120) {
                Some("metadata batch exceeded 120 seconds")
            } else {
                match resident_bytes(child.id()) {
                    Some(bytes) if bytes > stop_at => Some("evaluator approached 512 MiB RSS"),
                    Some(_) => {
                        unmeasured_since = None;
                        None
                    }
                    None => match child.try_wait() {
                        Ok(Some(status)) => break Ok(status),
                        _ => {
                            let since = unmeasured_since.get_or_insert_with(Instant::now);
                            (since.elapsed() > Duration::from_millis(200))
                                .then_some("cannot measure evaluator memory")
                        }
                    },
                }
            };
            if let Some(reason) = reason {
                stopped = Some(reason);
                kill_group(pid);
                let _ = child.kill();
                break child.wait();
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        kill_group(pid);
        let stdout = out
            .join()
            .map_err(|_| std::io::Error::other("stdout reader stopped"))??;
        let stderr = err
            .join()
            .map_err(|_| std::io::Error::other("stderr reader stopped"))??;
        Ok(Output {
            status: status?,
            stdout,
            stderr,
        })
    });
    (output, stopped)
}

fn kill_group(pid: u32) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: bounded children own a separate process group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn capped_read<R: std::io::Read>(
    pipe: Option<R>,
    overflow: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>, std::io::Error> {
    use std::sync::atomic::Ordering;
    let mut result = Vec::new();
    if let Some(mut pipe) = pipe {
        let mut buffer = [0u8; 8192];
        loop {
            let count = match pipe.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                break;
            }
            if result.len() + count > 16 * 1024 * 1024 {
                overflow.store(true, Ordering::Relaxed);
            } else {
                result.extend_from_slice(&buffer[..count]);
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .map(|kib| kib * 1024)
}

#[cfg(target_os = "macos")]
fn resident_bytes(pid: u32) -> Option<u64> {
    // SAFETY: proc_pidinfo writes at most the size of this initialized buffer.
    let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = i32::try_from(std::mem::size_of_val(&task)).ok()?;
    let read = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            libc::PROC_PIDTASKINFO,
            0,
            std::ptr::from_mut(&mut task).cast(),
            size,
        )
    };
    (read == size).then_some(task.pti_resident_size)
}

/// Run one direct external command through the shared signal forward/reap
/// boundary used for native Nix children, capturing stdout and the
/// classified outcome.
///
/// Termination signals sent only to pkg are forwarded to the child, the
/// child is reaped, and pkg stays alive, so an interrupted command is
/// reported as [`Outcome::Interrupted`] instead of killing pkg mid-run.
/// Stderr is captured into the outcome; stdout is returned alongside it.
pub fn run_direct_captured(
    executable: &std::path::Path,
    args: &[String],
) -> Result<(Outcome, String), String> {
    let mut command = Command::new(executable);
    command.args(args);
    run_child(command, IoMode::Capture, None).map(|reaped| {
        (
            reaped.classify(),
            String::from_utf8_lossy(&reaped.output.stdout).into_owned(),
        )
    })
}

/// Run one direct external command through the shared signal forward/reap
/// boundary, discarding captured stdout.
///
/// Termination signals sent only to pkg are forwarded to the child, the
/// child is reaped, and pkg stays alive, so an interrupted command is
/// reported as [`Outcome::Interrupted`] instead of killing pkg mid-run.
/// Stderr is captured for diagnostics; use [`run_direct_captured`] when
/// stdout matters.
pub fn run_direct(executable: &std::path::Path, args: &[String]) -> Result<Outcome, String> {
    run_direct_captured(executable, args).map(|(outcome, _)| outcome)
}

/// The cancellation signal for a finished child, when the run was
/// cancelled: a signalled child, then a child that exited 130 after
/// handling SIGINT itself.
fn unix_signal(status: &ExitStatus) -> Option<i32> {
    if let Some(signal) = platform_signal(status) {
        return Some(signal);
    }
    // A child that exits with status 130 reports a handled SIGINT; treat it
    // as interruption so native state is still re-read.
    (status.code() == Some(130)).then_some(2)
}

fn platform_signal(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

fn exit_status_text(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => format!("terminated by signal: {status}"),
    }
}
