//! Child process supervision, signal forwarding, and reaping.
//!
//! [`run_child`] runs Nix commands, tap imports, and app helpers. It forwards
//! termination signals to the child and reaps it. This lets pkg read state
//! again and report the result after cancellation. [`Reaped::classify`]
//! converts the result to an [`Outcome`].

#![cfg(unix)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests fail on broken fixtures"
    )
)]

use std::process::{Child, Command, ExitStatus, Output, Stdio};

#[cfg(test)]
mod tests;

/// Input and output handling for a child process.
pub enum IoMode<'sink> {
    /// Capture stdout/stderr through pipes.
    Capture,
    /// Stream stdio to the user's terminal.
    Stream,
    /// Inherit stdin/stdout, pipe stderr, and feed it live to a sink.
    Filtered(&'sink mut dyn StderrSink),
}

/// A reaped child with output, cancellation, and resource status.
pub struct Reaped {
    /// The child's output. The stdout and stderr buffers are empty for
    /// streamed children.
    pub output: Output,
    /// The first termination signal pkg received during the run, if any.
    ///
    /// The supervisor records this signal before it restores the previous
    /// signal handlers. The exit status alone cannot identify cancellation.
    /// Nix can handle SIGINT and exit with code 1.
    pub cancelled: Option<i32>,
    /// A failed resource bound, after group cleanup.
    pub resource_limit: Option<&'static str>,
}

/// A supervision failure with any cancellation retained through cleanup.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct RunError {
    /// The first forwarded signal, or the child's cancellation signal.
    pub cancelled: Option<i32>,
    /// The original setup, stream, cleanup, or wait failure.
    #[source]
    pub source: std::io::Error,
}

impl From<std::io::Error> for RunError {
    fn from(source: std::io::Error) -> Self {
        Self {
            cancelled: None,
            source,
        }
    }
}

impl Reaped {
    /// Classify this run by cancellation and exit status.
    ///
    /// Cancellation takes priority over the exit status. Without a resource-limit
    /// failure, exit code 130 also counts as interrupted. A resource-limit
    /// failure is a failure even after a successful exit, because the
    /// captured output is incomplete. Callers must read native state again
    /// because the child may have applied part of the operation.
    #[must_use]
    pub fn classify(&self) -> Outcome {
        if let Some(signal) = self.cancelled.or_else(|| {
            self.resource_limit
                .is_none()
                .then(|| unix_signal(self.output.status))
                .flatten()
        }) {
            return Outcome::Interrupted { signal };
        }
        if let Some(limit) = self.resource_limit {
            return Outcome::Failed {
                status: limit.to_string(),
                stderr: String::from_utf8_lossy(&self.output.stderr).into_owned(),
            };
        }
        if self.output.status.success() {
            return Outcome::Success;
        }
        Outcome::Failed {
            status: exit_status_text(self.output.status),
            stderr: String::from_utf8_lossy(&self.output.stderr).into_owned(),
        }
    }
}

/// The result of a child process.
///
/// The Nix adapter adds the command arguments when it converts this result
/// to an error. Direct helper commands return it without conversion.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The command exited successfully.
    Success,
    /// The command was cancelled by a termination signal during the run.
    ///
    /// The supervisor has reaped the child. The operation may have applied
    /// some changes, so callers must check state and report the result.
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

mod forward {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    /// Signals that cancel a native child and are forwarded to it.
    pub const TERMINATION: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    static RECEIVED: AtomicI32 = AtomicI32::new(0);
    static SIGNAL_COUNTS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static HANDLERS: AtomicUsize = AtomicUsize::new(0);
    static RUN_LOCK: Mutex<()> = Mutex::new(());

    extern "C" fn record_cancellation(signum: libc::c_int) {
        // Async-signal-safe: lock-free atomics only. The owner forwards later.
        HANDLERS.fetch_add(1, Ordering::SeqCst);
        if ACTIVE.load(Ordering::SeqCst) {
            let index = TERMINATION.iter().position(|&signal| signal == signum);
            if let Some(index) = index {
                let _ = RECEIVED.compare_exchange(0, signum, Ordering::SeqCst, Ordering::SeqCst);
                SIGNAL_COUNTS[index].fetch_add(1, Ordering::SeqCst);
            }
        }
        HANDLERS.fetch_sub(1, Ordering::SeqCst);
    }

    fn empty_action(handler: usize) -> libc::sigaction {
        // SAFETY: an all-zero `sigaction` is a valid empty mask and no
        // flags; the handler field is then assigned explicitly.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        action
    }

    /// Ignore SIGPIPE until the guard drops. A write to a child that ended
    /// then fails with `BrokenPipe`, also in a program that ends on SIGPIPE.
    pub fn ignore_pipe() -> std::io::Result<PipeGuard> {
        // SAFETY: an all-zero `sigaction` is a valid empty action to receive
        // the old disposition.
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: SIG_IGN is a valid disposition.
        if unsafe { libc::sigaction(libc::SIGPIPE, &empty_action(libc::SIG_IGN), &raw mut old) }
            != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(PipeGuard(old))
    }

    pub struct PipeGuard(libc::sigaction);

    impl Drop for PipeGuard {
        fn drop(&mut self) {
            // SAFETY: the saved disposition came from `sigaction`.
            unsafe { libc::sigaction(libc::SIGPIPE, &raw const self.0, std::ptr::null_mut()) };
        }
    }

    /// Reject dispositions that let the kernel reap children before wait.
    pub fn check_sigchld() -> std::io::Result<()> {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new-action pointer queries the current disposition.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SIGCHLD is configured to reap children automatically",
            ));
        }
        Ok(())
    }

    pub fn install() -> std::io::Result<Guard> {
        let mut previous = Vec::new();
        RECEIVED.store(0, Ordering::SeqCst);
        for count in &SIGNAL_COUNTS {
            count.store(0, Ordering::SeqCst);
        }
        ACTIVE.store(true, Ordering::SeqCst);
        for signum in TERMINATION {
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            let handler = record_cancellation as *const () as usize;
            if unsafe { libc::sigaction(signum, &empty_action(handler), &raw mut old) } != 0 {
                let error = std::io::Error::last_os_error();
                deactivate();
                restore(&previous)?;
                return Err(error);
            }
            previous.push((signum, old));
        }
        Ok(Guard(Some(previous)))
    }

    fn deactivate() {
        ACTIVE.store(false, Ordering::SeqCst);
        while HANDLERS.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
    }

    fn restore(previous: &[(libc::c_int, libc::sigaction)]) -> std::io::Result<()> {
        let mut first_error = None;
        for (signum, old) in previous.iter().rev() {
            if unsafe { libc::sigaction(*signum, &raw const *old, std::ptr::null_mut()) } != 0
                && first_error.is_none()
            {
                first_error = Some(std::io::Error::last_os_error());
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub struct Guard(Option<Vec<(libc::c_int, libc::sigaction)>>);

    impl Guard {
        pub fn restore(mut self) -> std::io::Result<()> {
            deactivate();
            restore(self.0.as_deref().unwrap_or_default())?;
            self.0 = None;
            Ok(())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            deactivate();
            if let Some(previous) = &self.0 {
                let _ = restore(previous);
            }
        }
    }

    pub fn signals_since(seen: &mut [usize; 3]) -> Option<[usize; 3]> {
        let mut new_signals = [0; 3];
        for (index, count) in SIGNAL_COUNTS.iter().enumerate() {
            let current = count.load(Ordering::SeqCst);
            new_signals[index] = current.wrapping_sub(seen[index]);
            seen[index] = current;
        }
        if new_signals.iter().all(|count| *count == 0) {
            return None;
        }
        Some(new_signals)
    }

    pub fn received() -> Option<i32> {
        let signal = RECEIVED.load(Ordering::SeqCst);
        (signal != 0).then_some(signal)
    }

    pub fn block() -> std::io::Result<MaskGuard> {
        let previous = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut set);
            for signum in TERMINATION {
                libc::sigaddset(&raw mut set, signum);
            }
            let status = libc::pthread_sigmask(
                libc::SIG_BLOCK,
                std::ptr::from_ref(&set),
                std::ptr::from_mut(&mut previous),
            );
            if status != 0 {
                return Err(std::io::Error::from_raw_os_error(status));
            }
            previous
        };
        Ok(MaskGuard(previous))
    }

    pub struct MaskGuard(libc::sigset_t);

    impl MaskGuard {
        pub fn prepare_child(&self, command: &mut std::process::Command) {
            use std::os::unix::process::CommandExt as _;
            let previous = self.0;
            // SAFETY: the hook only restores a copied signal mask with the
            // async-signal-safe sigprocmask call. It does not allocate or lock.
            unsafe {
                command.pre_exec(move || {
                    if libc::sigprocmask(
                        libc::SIG_SETMASK,
                        std::ptr::from_ref(&previous),
                        std::ptr::null_mut(),
                    ) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }

    impl Drop for MaskGuard {
        fn drop(&mut self) {
            unsafe {
                libc::pthread_sigmask(
                    libc::SIG_SETMASK,
                    std::ptr::from_ref(&self.0),
                    std::ptr::null_mut(),
                );
            }
        }
    }

    /// Serialize child registration; only one child runs at a time.
    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        RUN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A live consumer of one filtered child's stderr chunks.
pub trait StderrSink {
    /// One raw chunk in arrival order.
    fn chunk(&mut self, bytes: &[u8]);
    /// Update terminal progress while the child is quiet.
    fn tick(&mut self) {}
    /// Called once when output processing stops. Normal completion follows
    /// child reap; error cleanup can follow this call.
    fn finish(&mut self, success: bool);
}

#[derive(Clone, Copy)]
/// Resource bounds for a captured child in its own process group.
pub struct ChildLimits {
    /// Maximum resident memory. None disables memory measurement.
    pub resident_bytes: Option<u64>,
    /// Maximum elapsed time.
    pub wall_time: std::time::Duration,
    /// Maximum captured bytes in each stream.
    pub output_bytes: usize,
}

const READ_BUDGET: usize = 64 * 1024;
/// The captured bytes of each stream when the caller sets no limits.
const DEFAULT_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
/// The end of filtered stderr that stays for the failure report.
const FILTERED_STDERR_BYTES: usize = 4 * 1024 * 1024;
const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
const POST_EXIT_DRAIN: std::time::Duration = std::time::Duration::from_millis(250);
const MONITOR_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildState {
    Running,
    Exited,
    Reaping,
    Reaped,
    Lost,
}

struct ChildOwner {
    child: Child,
    pid: libc::pid_t,
    process_group: bool,
    state: ChildState,
}

impl ChildOwner {
    fn new(mut child: Child, process_group: bool) -> std::io::Result<Self> {
        let Ok(pid) = libc::pid_t::try_from(child.id()) else {
            let kill_result = child.kill();
            let wait_result = child.wait().map(|_| ());
            kill_result.and(wait_result)?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "child process id does not fit pid_t",
            ));
        };
        Ok(Self {
            child,
            pid,
            process_group,
            state: ChildState::Running,
        })
    }

    fn observe(&mut self) -> std::io::Result<bool> {
        if self.state == ChildState::Exited {
            return Ok(true);
        }
        if self.state != ChildState::Running {
            return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
        }
        let id = self.child.id();
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is writable and `id` names this unreaped child.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    id,
                    std::ptr::from_mut(&mut info),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } == 0
            {
                // SAFETY: successful `waitid` initialized the process id field.
                if unsafe { info.si_pid() } == 0 {
                    return Ok(false);
                }
                self.state = ChildState::Exited;
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.state = ChildState::Lost;
            }
            return Err(error);
        }
    }

    fn signal_child(&mut self, signal: libc::c_int) -> std::io::Result<()> {
        if self.state != ChildState::Running {
            return Ok(());
        }
        // SAFETY: wait ownership is retained until `reap` consumes the status.
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn signal_group(&mut self, signal: libc::c_int) -> std::io::Result<()> {
        if !self.process_group || !matches!(self.state, ChildState::Running | ChildState::Exited) {
            return Ok(());
        }
        // SAFETY: the child leads a dedicated group and remains unreaped.
        if unsafe { libc::kill(-self.pid, signal) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EPERM) && signal == libc::SIGKILL {
            if self.state == ChildState::Running && !self.observe()? {
                return Err(error);
            }
            if self.state == ChildState::Exited && !macos_group_has_live_members(self.pid)? {
                return Ok(());
            }
        }
        Err(error)
    }

    fn force_stop(&mut self) -> std::io::Result<()> {
        let group_result = self.signal_group(libc::SIGKILL);
        let child_result = self.signal_child(libc::SIGKILL);
        group_result.and(child_result)
    }

    fn reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !matches!(self.state, ChildState::Exited | ChildState::Reaping) {
            return Err(std::io::Error::other("child exit was not observed"));
        }
        // Stop all signaling before wait can consume the PID and release its
        // process-group identity. A wait error must not restore that identity.
        self.state = ChildState::Reaping;
        match self.child.wait() {
            Ok(status) => {
                self.state = ChildState::Reaped;
                Ok(status)
            }
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.state = ChildState::Lost;
                }
                Err(error)
            }
        }
    }

    fn cleanup(&mut self) {
        if self.state == ChildState::Running {
            match self.observe() {
                Ok(true) => {}
                Err(_) if self.state == ChildState::Lost => return,
                Ok(false) | Err(_) => {
                    // Keep the child identity owned while stopping it. We
                    // cannot signal its group after wait may consume its PID.
                    let _ = self.force_stop();
                    self.state = ChildState::Reaping;
                }
            }
        }
        if self.state == ChildState::Exited {
            let _ = self.signal_group(libc::SIGKILL);
            let _ = self.reap();
        }
        self.finish_reap();
    }

    fn finish_reap(&mut self) {
        while self.state == ChildState::Reaping {
            if self.reap().is_err() && self.state == ChildState::Reaping {
                std::thread::sleep(MONITOR_INTERVAL);
            }
        }
    }
}

/// Check live members after Darwin reports EPERM for a group signal.
///
/// Darwin keeps an exited child in its process group until wait reaps it. A
/// group with only that zombie therefore returns EPERM instead of ESRCH. The
/// process-list APIs include zombies, so skip the leader whose exit we have
/// already observed and require readable state for every other listed PID.
#[cfg(target_os = "macos")]
fn macos_group_has_live_members(pgid: libc::pid_t) -> std::io::Result<bool> {
    // SAFETY: the null buffer asks libproc to size the PID list; no memory is written.
    let estimate = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
    if estimate <= 0 {
        return Err(std::io::Error::other(
            "cannot size Darwin process-group member list",
        ));
    }
    let capacity = usize::try_from(estimate).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Darwin process-group member count does not fit usize",
        )
    })?;
    let buffer_size = capacity
        .checked_mul(std::mem::size_of::<libc::pid_t>())
        .and_then(|size| libc::c_int::try_from(size).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Darwin process-group member buffer is too large",
            )
        })?;
    let mut pids = vec![0; capacity];
    // SAFETY: `pids` has `buffer_size` writable bytes for returned PIDs.
    let count = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), buffer_size) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let count = usize::try_from(count).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Darwin process-group member count does not fit usize",
        )
    })?;
    if count >= capacity {
        return Err(std::io::Error::other(
            "Darwin process-group member list may be truncated",
        ));
    }
    pids.truncate(count);

    for pid in pids {
        if pid == pgid {
            continue;
        }
        // SAFETY: the initialized buffer matches `proc_bsdinfo`; proc_pidinfo
        // writes no more than the supplied size.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of_val(&info)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Darwin process info size does not fit c_int",
            )
        })?;
        // SAFETY: `info` is writable and `pid` came from the process-group list.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            // SAFETY: the buffer remains writable during each inspection attempt.
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    std::ptr::from_mut(&mut info).cast(),
                    size,
                )
            };
            if read == size {
                break;
            }
            // A killed descendant can briefly remain listed while its process
            // information is unavailable. Accept only readable state or absence.
            if unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "cannot inspect a Darwin process-group member",
                ));
            }
            std::thread::sleep(MONITOR_INTERVAL);
        }
        if info.pbi_pgid == pgid.cast_unsigned() && info.pbi_status != libc::SZOMB {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct OutputPipes<'sink, 'input> {
    stdin: Option<StdinPipe<'input>>,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
    output_limit: Option<usize>,
    overflow: bool,
    mode: IoMode<'sink>,
    last_tick: std::time::Instant,
}

struct StdinPipe<'a> {
    stream: std::process::ChildStdin,
    remaining: &'a [u8],
}

impl<'sink, 'input> OutputPipes<'sink, 'input> {
    fn new(
        child: &mut Child,
        mode: IoMode<'sink>,
        output_limit: Option<usize>,
        input: Option<&'input [u8]>,
    ) -> std::io::Result<Self> {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = if let Some(bytes) = input {
            let stream = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("the child has no piped stdin"))?;
            set_nonblocking(&stream)?;
            Some(StdinPipe {
                stream,
                remaining: bytes,
            })
        } else {
            None
        };
        if let Some(pipe) = &stdout {
            set_nonblocking(pipe)?;
        }
        if let Some(pipe) = &stderr {
            set_nonblocking(pipe)?;
        }
        Ok(Self {
            stdin,
            stdout,
            stderr,
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            output_limit: matches!(mode, IoMode::Capture)
                .then(|| output_limit.unwrap_or(DEFAULT_OUTPUT_BYTES)),
            overflow: false,
            mode,
            last_tick: std::time::Instant::now(),
        })
    }

    fn poll_io(&mut self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;

        let mut descriptors = [
            libc::pollfd {
                fd: self.stdout.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: self.stderr.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: self
                    .stdin
                    .as_ref()
                    .map_or(-1, |pipe| pipe.stream.as_raw_fd()),
                events: libc::POLLOUT | libc::POLLHUP,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 3, 10) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                self.tick();
                return Ok(());
            }
            return Err(error);
        }
        if descriptors
            .iter()
            .any(|descriptor| descriptor.revents & libc::POLLNVAL != 0)
        {
            return Err(std::io::Error::from_raw_os_error(libc::EBADF));
        }
        if descriptors[0].revents != 0 {
            read_pipe(
                &mut self.stdout,
                &mut self.stdout_bytes,
                self.output_limit,
                &mut self.overflow,
                &mut IoMode::Capture,
            )?;
            if self.overflow {
                return Ok(());
            }
        }
        if descriptors[1].revents != 0 {
            read_pipe(
                &mut self.stderr,
                &mut self.stderr_bytes,
                self.output_limit,
                &mut self.overflow,
                &mut self.mode,
            )?;
        }
        if descriptors[2].revents != 0 {
            self.write_stdin()?;
        }
        self.tick();
        Ok(())
    }

    fn write_stdin(&mut self) -> std::io::Result<()> {
        use std::io::Write as _;
        let Some(pipe) = &mut self.stdin else {
            return Ok(());
        };
        if !pipe.remaining.is_empty() {
            let end = pipe.remaining.len().min(READ_BUDGET);
            match pipe.stream.write(&pipe.remaining[..end]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "could not deliver child stdin",
                    ));
                }
                Ok(count) => pipe.remaining = &pipe.remaining[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        if pipe.remaining.is_empty() {
            self.stdin.take();
        }
        Ok(())
    }

    fn tick(&mut self) {
        if self.last_tick.elapsed() >= std::time::Duration::from_millis(250) {
            if let IoMode::Filtered(sink) = &mut self.mode {
                sink.tick();
            }
            self.last_tick = std::time::Instant::now();
        }
    }

    fn all_closed(&self) -> bool {
        self.stdin.is_none() && self.stdout.is_none() && self.stderr.is_none()
    }

    fn close(&mut self) {
        self.stdin.take();
        self.stdout.take();
        self.stderr.take();
    }

    fn finish(&mut self, success: bool) {
        if let IoMode::Filtered(sink) = std::mem::replace(&mut self.mode, IoMode::Stream) {
            sink.finish(success);
        }
    }
}

fn read_pipe<T: std::io::Read>(
    pipe: &mut Option<T>,
    bytes: &mut Vec<u8>,
    limit: Option<usize>,
    overflow: &mut bool,
    mode: &mut IoMode<'_>,
) -> std::io::Result<()> {
    let Some(stream) = pipe.as_mut() else {
        return Ok(());
    };
    let mut buffer = [0_u8; 8192];
    let mut budget = READ_BUDGET;
    while budget > 0 {
        let count_limit = budget.min(buffer.len());
        match stream.read(&mut buffer[..count_limit]) {
            Ok(0) => {
                *pipe = None;
                break;
            }
            Ok(count) => {
                budget -= count;
                append_output(bytes, &buffer[..count], limit, overflow);
                if let IoMode::Filtered(sink) = mode {
                    sink.chunk(&buffer[..count]);
                    keep_tail(bytes);
                }
                if *overflow {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct ChildMonitor {
    started: std::time::Instant,
    unmeasured_since: Option<std::time::Instant>,
    signal_counts: [usize; 3],
    cancellation_deadline: Option<std::time::Instant>,
    cancellation_expired: bool,
    stop_sent: bool,
    limit_reason: Option<&'static str>,
    io_error: Option<std::io::Error>,
    drain_deadline: Option<std::time::Instant>,
}

impl ChildMonitor {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            unmeasured_since: None,
            signal_counts: [0; 3],
            cancellation_deadline: None,
            cancellation_expired: false,
            stop_sent: false,
            limit_reason: None,
            io_error: None,
            drain_deadline: None,
        }
    }

    fn observe_exit(&mut self, owner: &mut ChildOwner) -> std::io::Result<()> {
        if self.drain_deadline.is_some() || !owner.observe()? {
            return Ok(());
        }
        self.drain_deadline = Some(std::time::Instant::now() + POST_EXIT_DRAIN);
        if let Err(error) = owner.signal_group(libc::SIGKILL) {
            self.io_error.get_or_insert(error);
        }
        Ok(())
    }

    fn drain_exit(&mut self, pipes: &mut OutputPipes<'_, '_>) -> bool {
        if pipes.all_closed()
            || self
                .drain_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            if pipes.stdin.is_some() {
                self.io_error.get_or_insert_with(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "child ended before stdin was delivered",
                    )
                });
            }
            pipes.close();
            return true;
        }
        if let Err(error) = pipes.poll_io() {
            self.io_error.get_or_insert(error);
            pipes.close();
        }
        false
    }

    fn forward_signals(&mut self, owner: &mut ChildOwner) {
        let Some(counts) = forward::signals_since(&mut self.signal_counts) else {
            return;
        };
        self.cancellation_deadline
            .get_or_insert_with(|| std::time::Instant::now() + CANCELLATION_GRACE);
        for (signal, count) in forward::TERMINATION.into_iter().zip(counts) {
            for _ in 0..count {
                if let Err(error) = owner.signal_child(signal) {
                    self.io_error.get_or_insert(error);
                }
            }
        }
    }

    fn stop_reason(
        &mut self,
        owner: &ChildOwner,
        limits: Option<ChildLimits>,
        now: std::time::Instant,
    ) -> Option<&'static str> {
        if self.io_error.is_some() {
            return None;
        }
        if limits.is_some_and(|bounds| now.duration_since(self.started) >= bounds.wall_time) {
            return Some("child exceeded wall limit");
        }
        let bounds = limits?;
        if self.limit_reason.is_some() {
            return None;
        }
        let rss_limit = bounds.resident_bytes?.saturating_sub(32 * 1024 * 1024);
        match resident_bytes(owner.child.id()) {
            Some(bytes) if bytes > rss_limit => Some("evaluator approached 512 MiB RSS"),
            Some(_) => {
                self.unmeasured_since = None;
                None
            }
            None => {
                let since = self.unmeasured_since.get_or_insert(now);
                (since.elapsed() > std::time::Duration::from_millis(200))
                    .then_some("cannot measure evaluator memory")
            }
        }
    }

    fn force_stop(&mut self, owner: &mut ChildOwner) {
        self.stop_sent = true;
        if let Err(error) = owner.force_stop() {
            self.io_error.get_or_insert(error);
        }
    }

    fn enforce_stops(
        &mut self,
        owner: &mut ChildOwner,
        pipes: &mut OutputPipes<'_, '_>,
        limits: Option<ChildLimits>,
    ) {
        let now = std::time::Instant::now();
        let reason = if pipes.overflow {
            Some("child exceeded log cap")
        } else {
            self.stop_reason(owner, limits, now)
        };
        if let Some(reason) = reason
            && self.limit_reason.is_none()
        {
            self.limit_reason = Some(reason);
            self.force_stop(owner);
        }

        if !self.cancellation_expired
            && self
                .cancellation_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            self.cancellation_expired = true;
            self.force_stop(owner);
        }

        if self.io_error.is_some() && !self.stop_sent {
            self.stop_sent = true;
            pipes.close();
            self.force_stop(owner);
        }
    }

    fn poll_running(&mut self, owner: &mut ChildOwner, pipes: &mut OutputPipes<'_, '_>) {
        if let Err(error) = pipes.poll_io() {
            self.io_error.get_or_insert(error);
            pipes.close();
            self.force_stop(owner);
        }
    }

    fn run(
        &mut self,
        owner: &mut ChildOwner,
        pipes: &mut OutputPipes<'_, '_>,
        limits: Option<ChildLimits>,
    ) -> std::io::Result<()> {
        loop {
            self.observe_exit(owner)?;
            if self.drain_deadline.is_some() && pipes.overflow && self.limit_reason.is_none() {
                self.limit_reason = Some("child exceeded log cap");
            }
            if self.drain_deadline.is_some() {
                if self.drain_exit(pipes) {
                    break;
                }
                continue;
            }
            self.forward_signals(owner);
            self.enforce_stops(owner, pipes, limits);
            self.poll_running(owner, pipes);
        }
        Ok(())
    }
}

fn append_output(
    bytes: &mut Vec<u8>,
    chunk: &[u8],
    output_limit: Option<usize>,
    overflow: &mut bool,
) {
    if output_limit
        .is_some_and(|limit| *overflow || bytes.len().saturating_add(chunk.len()) > limit)
    {
        *overflow = true;
    } else {
        bytes.extend_from_slice(chunk);
    }
}

/// Keep the end of filtered stderr from its last line boundaries.
///
/// The sink already showed the stream, and failure reports read its last
/// lines, so a long log does not grow without a limit.
fn keep_tail(bytes: &mut Vec<u8>) {
    if bytes.len() > 2 * FILTERED_STDERR_BYTES {
        let cut = bytes.len() - FILTERED_STDERR_BYTES;
        let start = bytes[cut..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(cut, |line| cut + line + 1);
        bytes.drain(..start);
    }
}

fn set_nonblocking(fd: impl std::os::fd::AsFd) -> std::io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(&fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

impl ChildOwner {
    fn run(
        mut self,
        mode: IoMode<'_>,
        limits: Option<ChildLimits>,
        input: Option<&[u8]>,
    ) -> Result<(Output, Option<&'static str>), RunError> {
        let mut pipes = OutputPipes::new(
            &mut self.child,
            mode,
            limits.map(|bounds| bounds.output_bytes),
            input,
        )?;
        let mut monitor = ChildMonitor::new();
        if let Err(error) = monitor.run(&mut self, &mut pipes, limits) {
            pipes.finish(false);
            return Err(error.into());
        }

        let status = match self.reap() {
            Ok(status) => status,
            Err(error) => {
                pipes.finish(false);
                return Err(error.into());
            }
        };
        pipes.finish(status.success() && monitor.io_error.is_none());
        if let Some(error) = monitor.io_error {
            return Err(RunError {
                cancelled: unix_signal(status)
                    .filter(|signal| !monitor.stop_sent || forward::TERMINATION.contains(signal)),
                source: error,
            });
        }
        Ok((
            Output {
                status,
                stdout: pipes.stdout_bytes,
                stderr: pipes.stderr_bytes,
            },
            monitor.limit_reason,
        ))
    }
}

/// Run a child and retain cancellation until the caller reports it.
///
/// The mode selects captured or inherited output. Supplied input uses a
/// nonblocking pipe instead of inherited or null stdin. The complete input
/// must be delivered; otherwise the child is stopped and reaped.
/// Limits create a child process group and bound elapsed time and captured
/// output. Memory checks require `resident_bytes`. Without limits, each
/// captured stream stops the child at 256 MiB, and filtered stderr keeps
/// its last 4 MiB.
/// Filtered mode delivers captured stderr to its borrowed sink while the child runs.
/// # Errors
/// Returns an error if signal registration, spawn, input delivery, stream reads,
/// cleanup, or wait fails.
pub fn run_child(
    mut command: Command,
    mode: IoMode<'_>,
    limits: Option<ChildLimits>,
    input: Option<&[u8]>,
) -> Result<Reaped, RunError> {
    let _run = forward::lock();
    let mask = forward::block()?;
    let guard = forward::install().map_err(|source| RunError {
        cancelled: forward::received(),
        source,
    })?;
    mask.prepare_child(&mut command);
    if limits.is_some() {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let result = (|| {
        forward::check_sigchld()?;
        let child = command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else if matches!(mode, IoMode::Capture) {
                Stdio::null()
            } else {
                Stdio::inherit()
            })
            .stdout(if matches!(mode, IoMode::Capture) {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stderr(if matches!(mode, IoMode::Stream) {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .spawn()?;
        // After the spawn, so that the child keeps its own SIGPIPE action.
        let _pipe = input.map(|_| forward::ignore_pipe()).transpose()?;
        let owner = ChildOwner::new(child, limits.is_some())?;
        drop(mask);
        owner.run(mode, limits, input)
    })();
    let restored = guard.restore();
    let cancelled = forward::received();
    let failure = |source| RunError { cancelled, source };
    let (output, resource_limit) = result.map_err(|mut error| {
        error.cancelled = cancelled.or(error.cancelled);
        error
    })?;
    restored.map_err(failure)?;
    Ok(Reaped {
        output,
        cancelled,
        resource_limit,
    })
}

/// Run several captured children at the same time, with the same limits.
///
/// Each child gets the supervision of [`run_child`]. A termination signal
/// reaches every child, and every child is reaped. The results keep the
/// order of `commands`.
/// # Errors
///
/// Returns an error if signal setup or signal restoration fails. A child
/// that cannot start or be supervised gets an error in its own result.
pub fn run_children(
    commands: Vec<Command>,
    limits: Option<ChildLimits>,
) -> Result<Vec<Result<Reaped, RunError>>, RunError> {
    use std::os::unix::process::CommandExt as _;
    let _run = forward::lock();
    let mask = forward::block()?;
    let guard = forward::install().map_err(|source| RunError {
        cancelled: forward::received(),
        source,
    })?;
    let owners: Vec<Result<ChildOwner, RunError>> = commands
        .into_iter()
        .map(|mut command| {
            mask.prepare_child(&mut command);
            if limits.is_some() {
                command.process_group(0);
            }
            forward::check_sigchld()?;
            let child = command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            Ok(ChildOwner::new(child, limits.is_some())?)
        })
        .collect();
    drop(mask);
    let results: Vec<_> = std::thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "every supervisor starts before the first join"
        )]
        let handles: Vec<_> = owners
            .into_iter()
            .map(|owner| {
                scope
                    .spawn(move || owner.and_then(|owner| owner.run(IoMode::Capture, limits, None)))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let restored = guard.restore();
    let cancelled = forward::received();
    restored.map_err(|source| RunError { cancelled, source })?;
    Ok(results
        .into_iter()
        .map(|result| {
            let (output, resource_limit) = result.map_err(|mut error| {
                error.cancelled = cancelled.or(error.cancelled);
                error
            })?;
            Ok(Reaped {
                output,
                cancelled,
                resource_limit,
            })
        })
        .collect())
}

/// Memory that new processes can use, in bytes, when the system reports it.
///
/// Linux reports available memory. macOS reports only the installed
/// memory, so this returns half of it.
#[must_use]
pub fn available_memory() -> Option<u64> {
    available_memory_bytes()
}

#[cfg(target_os = "linux")]
fn available_memory_bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    info.lines()
        .find_map(|line| {
            line.strip_prefix("MemAvailable:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .map(|kib| kib * 1024)
}

#[cfg(target_os = "macos")]
fn available_memory_bytes() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut size = std::mem::size_of_val(&bytes);
    // SAFETY: the name is a NUL-terminated string, and the output buffer
    // and its size describe one initialized u64.
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            std::ptr::from_mut(&mut bytes).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of_val(&bytes)).then_some(bytes / 2)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn available_memory_bytes() -> Option<u64> {
    None
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Run an external command and return its stdout and classified outcome.
///
/// [`run_child`] forwards termination signals and reaps the child.
/// Cancellation returns [`Outcome::Interrupted`]. Failed commands include
/// captured stderr in the outcome.
/// # Errors
///
/// Returns an error if signal setup, child setup, spawning, signal forwarding,
/// exit observation, stream supervision, reaping, or signal restoration fails.
/// Nonzero exits and cancellation return [`Outcome`] values.
pub fn run_direct_captured(
    executable: &std::path::Path,
    args: &[&str],
) -> Result<(Outcome, String), RunError> {
    let mut command = Command::new(executable);
    command.args(args);
    run_child(command, IoMode::Capture, None, None).map(|reaped| {
        (
            reaped.classify(),
            String::from_utf8_lossy(&reaped.output.stdout).into_owned(),
        )
    })
}

/// Run an external command and discard its captured stdout.
///
/// This calls [`run_direct_captured`]. Use that function when the caller
/// needs stdout.
/// # Errors
///
/// Returns an error if signal setup, child setup, spawning, signal forwarding,
/// exit observation, stream supervision, reaping, or signal restoration fails.
/// Nonzero exits and cancellation return [`Outcome`] values.
pub fn run_direct(executable: &std::path::Path, args: &[&str]) -> Result<Outcome, RunError> {
    run_direct_captured(executable, args).map(|(outcome, _)| outcome)
}

/// Read the terminating signal from the exit status.
/// Treat exit code 130 as a handled SIGINT.
fn unix_signal(status: ExitStatus) -> Option<i32> {
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return Some(signal);
    }
    // A child that exits with status 130 reports a handled SIGINT; treat it
    // as interruption so native state is still re-read.
    (status.code() == Some(130)).then_some(2)
}

fn exit_status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => format!("terminated by signal: {status}"),
    }
}
