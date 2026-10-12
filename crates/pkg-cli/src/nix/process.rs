//! Child process supervision: one signal forward and reap boundary.
//!
//! Every external child of pkg — native Nix commands and the macOS app
//! helper alike — runs through [`run_child`]. Termination signals sent only
//! to pkg are forwarded to the registered child, the child is reaped, and
//! pkg stays alive to re-read state and report. Every reaped run is
//! classified exactly once, into [`Outcome`], by the code that reaped it.

use std::process::{Child, Command, ExitStatus, Output, Stdio};

#[cfg(all(test, unix))]
mod tests;

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
        if let Some(signal) = self.cancelled.or_else(|| unix_signal(self.output.status)) {
            return Outcome::Interrupted { signal };
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
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    /// Signals that cancel a native child and are forwarded to it.
    pub(super) const TERMINATION: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    static RECEIVED: AtomicI32 = AtomicI32::new(0);
    static SIGNAL_COUNTS: [AtomicUsize; 3] = [
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ];
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static HANDLERS: AtomicUsize = AtomicUsize::new(0);
    static RUN_LOCK: Mutex<()> = Mutex::new(());

    extern "C" fn record_cancellation(signum: libc::c_int) {
        // Async-signal-safe: lock-free atomics only. The owner forwards later.
        HANDLERS.fetch_add(1, Ordering::SeqCst);
        if ACTIVE.load(Ordering::SeqCst) {
            let index = TERMINATION.iter().position(|&signal| signal == signum);
            if let Some(index) = index {
                RECEIVED.store(signum, Ordering::SeqCst);
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

    /// Reject dispositions that let the kernel reap children before wait.
    pub(super) fn check_sigchld() -> std::io::Result<()> {
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

    pub(super) fn install() -> std::io::Result<Guard> {
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
                ACTIVE.store(false, Ordering::SeqCst);
                while HANDLERS.load(Ordering::SeqCst) != 0 {
                    std::hint::spin_loop();
                }
                restore(&previous)?;
                return Err(error);
            }
            previous.push((signum, old));
        }
        Ok(Guard(Some(previous)))
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

    pub(super) struct Guard(Option<Vec<(libc::c_int, libc::sigaction)>>);

    impl Guard {
        pub(super) fn restore(mut self) -> std::io::Result<Option<i32>> {
            ACTIVE.store(false, Ordering::SeqCst);
            while HANDLERS.load(Ordering::SeqCst) != 0 {
                std::hint::spin_loop();
            }
            let received = RECEIVED.load(Ordering::SeqCst);
            restore(self.0.as_deref().unwrap_or_default())?;
            self.0 = None;
            Ok((received != 0).then_some(received))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            ACTIVE.store(false, Ordering::SeqCst);
            while HANDLERS.load(Ordering::SeqCst) != 0 {
                std::hint::spin_loop();
            }
            if let Some(previous) = &self.0 {
                let _ = restore(previous);
            }
        }
    }

    pub(super) fn signals_since(seen: &mut [usize; 3]) -> Option<[usize; 3]> {
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

    pub(super) fn block() -> std::io::Result<MaskGuard> {
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

    pub(super) struct MaskGuard(libc::sigset_t);

    impl MaskGuard {
        pub(super) fn prepare_child(&self, command: &mut std::process::Command) {
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
    /// Update terminal progress while the child is quiet.
    fn tick(&mut self) {}
    /// Called once when output processing stops. Normal completion follows
    /// child reap; error cleanup can follow this call.
    fn finish(&mut self, success: bool);
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct ChildLimits {
    resident_bytes: u64,
    wall_time: std::time::Duration,
}

#[cfg(unix)]
impl ChildLimits {
    const fn metadata(resident_bytes: u64) -> Self {
        Self {
            resident_bytes,
            wall_time: std::time::Duration::from_mins(2),
        }
    }
}

#[cfg(unix)]
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const READ_BUDGET: usize = 64 * 1024;
#[cfg(unix)]
const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(unix)]
const POST_EXIT_DRAIN: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(unix)]
const MONITOR_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildState {
    Running,
    Exited,
    Reaping,
    Reaped,
    Lost,
}

#[cfg(unix)]
struct ChildOwner {
    child: Child,
    pid: libc::pid_t,
    process_group: bool,
    state: ChildState,
}

#[cfg(unix)]
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
        let id = libc::id_t::try_from(self.pid).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "child id does not fit id_t",
            )
        })?;
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
        if self.process_group {
            let group_result = self.signal_group(libc::SIGKILL);
            let child_result = self.signal_child(libc::SIGKILL);
            match (group_result, child_result) {
                (Err(error), _) | (_, Err(error)) => Err(error),
                (Ok(()), Ok(())) => Ok(()),
            }
        } else {
            self.signal_child(libc::SIGKILL)
        }
    }

    fn clean_group_after_exit(&mut self) -> std::io::Result<()> {
        self.signal_group(libc::SIGKILL)
    }

    fn reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if self.state != ChildState::Exited {
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
        if self.state == ChildState::Reaping {
            self.finish_reap();
            return;
        }
        if self.state == ChildState::Running {
            match self.observe() {
                Ok(true) => {}
                Ok(false) => {
                    let _ = self.force_stop();
                    self.state = ChildState::Reaping;
                    self.finish_reap();
                    return;
                }
                Err(_) if self.state == ChildState::Lost => return,
                Err(_) => {
                    // Keep the child identity owned while stopping it. We
                    // cannot signal its group after wait may consume its PID.
                    let _ = self.force_stop();
                    self.state = ChildState::Reaping;
                    self.finish_reap();
                    return;
                }
            }
        }
        if self.state == ChildState::Exited {
            let _ = self.clean_group_after_exit();
            let _ = self.reap();
            if self.state == ChildState::Reaping {
                self.finish_reap();
            }
        }
    }

    fn finish_reap(&mut self) {
        loop {
            match self.child.wait() {
                Ok(_) => {
                    self.state = ChildState::Reaped;
                    return;
                }
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    self.state = ChildState::Lost;
                    return;
                }
                Err(_) => std::thread::sleep(MONITOR_INTERVAL),
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
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if read != size {
            return Err(std::io::Error::other(
                "cannot inspect a Darwin process-group member",
            ));
        }
        if info.pbi_pgid == pgid.cast_unsigned() && info.pbi_status != libc::SZOMB {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(unix)]
struct OutputPipes<'a> {
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
    bounded: bool,
    overflow: bool,
    sink: Option<&'a mut dyn StderrSink>,
    last_tick: std::time::Instant,
}

#[cfg(unix)]
impl<'a> OutputPipes<'a> {
    fn new(
        child: &mut Child,
        mode: IoMode,
        bounded: bool,
        sink: Option<&'a mut dyn StderrSink>,
    ) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd as _;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if let Some(pipe) = &stdout {
            set_nonblocking(pipe.as_raw_fd())?;
        }
        if let Some(pipe) = &stderr {
            set_nonblocking(pipe.as_raw_fd())?;
        }
        Ok(Self {
            stdout,
            stderr,
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            bounded: bounded && mode == IoMode::Capture,
            overflow: false,
            sink,
            last_tick: std::time::Instant::now(),
        })
    }

    fn poll_read(&mut self) -> std::io::Result<()> {
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
        ];
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, 10) };
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
            self.read_stdout()?;
            if self.overflow {
                return Ok(());
            }
        }
        if descriptors[1].revents != 0 {
            self.read_stderr()?;
        }
        self.tick();
        Ok(())
    }

    fn read_stdout(&mut self) -> std::io::Result<()> {
        use std::io::Read as _;

        let (stdout, bytes, overflow) =
            (&mut self.stdout, &mut self.stdout_bytes, &mut self.overflow);
        let Some(pipe) = stdout.as_mut() else {
            return Ok(());
        };
        let mut buffer = [0_u8; 8192];
        let mut budget = READ_BUDGET;
        let mut eof = false;
        while budget > 0 {
            let count_limit = budget.min(buffer.len());
            match pipe.read(&mut buffer[..count_limit]) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    budget -= count;
                    append_output(bytes, &buffer[..count], self.bounded, overflow);
                    if *overflow {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        if eof {
            *stdout = None;
        }
        Ok(())
    }

    fn read_stderr(&mut self) -> std::io::Result<()> {
        use std::io::Read as _;

        let (stderr, bytes, overflow, sink) = (
            &mut self.stderr,
            &mut self.stderr_bytes,
            &mut self.overflow,
            &mut self.sink,
        );
        let Some(pipe) = stderr.as_mut() else {
            return Ok(());
        };
        let mut buffer = [0_u8; 8192];
        let mut budget = READ_BUDGET;
        let mut eof = false;
        while budget > 0 {
            let count_limit = budget.min(buffer.len());
            match pipe.read(&mut buffer[..count_limit]) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    budget -= count;
                    append_output(bytes, &buffer[..count], self.bounded, overflow);
                    if let Some(sink) = sink.as_deref_mut() {
                        sink.chunk(&buffer[..count]);
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
        if eof {
            *stderr = None;
        }
        Ok(())
    }

    fn tick(&mut self) {
        if self.last_tick.elapsed() >= std::time::Duration::from_millis(250) {
            if let Some(sink) = self.sink.as_deref_mut() {
                sink.tick();
            }
            self.last_tick = std::time::Instant::now();
        }
    }

    fn all_closed(&self) -> bool {
        self.stdout.is_none() && self.stderr.is_none()
    }

    fn close(&mut self) {
        self.stdout.take();
        self.stderr.take();
    }

    fn finish(&mut self, success: bool) {
        if let Some(sink) = self.sink.take() {
            sink.finish(success);
        }
    }
}

#[cfg(unix)]
struct ChildMonitor {
    started: std::time::Instant,
    unmeasured_since: Option<std::time::Instant>,
    signal_counts: [usize; 3],
    cancellation_deadline: Option<std::time::Instant>,
    cancellation_expired: bool,
    stop_sent: bool,
    limit_reason: Option<&'static str>,
    read_error: Option<std::io::Error>,
    exit_seen: bool,
    drain_deadline: Option<std::time::Instant>,
}

#[cfg(unix)]
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
            read_error: None,
            exit_seen: false,
            drain_deadline: None,
        }
    }

    fn observe_exit(&mut self, owner: &mut ChildOwner) -> std::io::Result<()> {
        if self.exit_seen || !owner.observe()? {
            return Ok(());
        }
        self.exit_seen = true;
        self.drain_deadline = Some(std::time::Instant::now() + POST_EXIT_DRAIN);
        if let Err(error) = owner.clean_group_after_exit() {
            self.read_error.get_or_insert(error);
        }
        Ok(())
    }

    fn drain_exit(&mut self, pipes: &mut OutputPipes<'_>) -> bool {
        if pipes.all_closed()
            || self
                .drain_deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            pipes.close();
            return true;
        }
        if let Err(error) = pipes.poll_read() {
            self.read_error.get_or_insert(error);
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
                    self.read_error.get_or_insert(error);
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
        if self.read_error.is_some() {
            return None;
        }
        if limits.is_some_and(|bounds| now.duration_since(self.started) >= bounds.wall_time) {
            return Some("metadata batch exceeded 120 seconds");
        }
        let bounds = limits?;
        if self.limit_reason.is_some() {
            return None;
        }
        let rss_limit = bounds.resident_bytes.saturating_sub(32 * 1024 * 1024);
        let resident = if let Ok(pid) = u32::try_from(owner.pid) {
            resident_bytes(pid)
        } else {
            self.read_error.get_or_insert_with(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "child process id does not fit u32",
                )
            });
            None
        };
        match resident {
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

    fn enforce_stops(
        &mut self,
        owner: &mut ChildOwner,
        pipes: &mut OutputPipes<'_>,
        limits: Option<ChildLimits>,
    ) {
        let now = std::time::Instant::now();
        let reason = if pipes.overflow {
            Some("metadata output exceeded 16 MiB")
        } else {
            self.stop_reason(owner, limits, now)
        };
        if let Some(reason) = reason
            && self.limit_reason.is_none()
        {
            self.limit_reason = Some(reason);
            self.stop_sent = true;
            if let Err(error) = owner.force_stop() {
                self.read_error.get_or_insert(error);
            }
        }

        if !self.cancellation_expired
            && self
                .cancellation_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            self.cancellation_expired = true;
            self.stop_sent = true;
            if let Err(error) = owner.force_stop() {
                self.read_error.get_or_insert(error);
            }
        }

        if self.read_error.is_some() && !self.stop_sent {
            self.stop_sent = true;
            pipes.close();
            if let Err(error) = owner.force_stop() {
                self.read_error.get_or_insert(error);
            }
        }
    }

    fn poll_running(&mut self, owner: &mut ChildOwner, pipes: &mut OutputPipes<'_>) {
        if let Err(error) = pipes.poll_read() {
            self.read_error.get_or_insert(error);
            pipes.close();
            if !self.stop_sent {
                self.stop_sent = true;
            }
            if let Err(error) = owner.force_stop() {
                self.read_error.get_or_insert(error);
            }
        }
    }

    fn run(
        &mut self,
        owner: &mut ChildOwner,
        pipes: &mut OutputPipes<'_>,
        limits: Option<ChildLimits>,
    ) -> std::io::Result<()> {
        loop {
            self.observe_exit(owner)?;
            if self.exit_seen && pipes.overflow && self.limit_reason.is_none() {
                self.limit_reason = Some("metadata output exceeded 16 MiB");
            }
            if self.exit_seen {
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

#[cfg(unix)]
fn append_output(bytes: &mut Vec<u8>, chunk: &[u8], bounded: bool, overflow: &mut bool) {
    if bounded && (*overflow || bytes.len().saturating_add(chunk.len()) > OUTPUT_LIMIT) {
        *overflow = true;
    } else {
        bytes.extend_from_slice(chunk);
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
impl ChildOwner {
    fn run(
        mut self,
        mode: IoMode,
        sink: Option<&mut dyn StderrSink>,
        limits: Option<ChildLimits>,
    ) -> std::io::Result<(Output, Option<&'static str>)> {
        let mut pipes = OutputPipes::new(&mut self.child, mode, limits.is_some(), sink)?;
        let mut monitor = ChildMonitor::new();
        if let Err(error) = monitor.run(&mut self, &mut pipes, limits) {
            pipes.finish(false);
            return Err(error);
        }

        let status = match self.reap() {
            Ok(status) => status,
            Err(error) => {
                pipes.finish(false);
                return Err(error);
            }
        };
        pipes.finish(status.success() && monitor.read_error.is_none());
        if let Some(error) = monitor.read_error {
            return Err(error);
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

pub(super) fn run_child(
    command: Command,
    mode: IoMode,
    sink: Option<&mut dyn StderrSink>,
) -> std::io::Result<Reaped> {
    run_child_with_limits(command, mode, sink, None)
}

pub(super) fn run_child_bounded(command: Command, bytes: u64) -> std::io::Result<Reaped> {
    run_child_with_limits(
        command,
        IoMode::Capture,
        None,
        Some(ChildLimits::metadata(bytes)),
    )
}

#[cfg(unix)]
fn run_child_with_limits(
    mut command: Command,
    mode: IoMode,
    sink: Option<&mut dyn StderrSink>,
    limits: Option<ChildLimits>,
) -> std::io::Result<Reaped> {
    let _run = forward::lock();
    let mask = forward::block()?;
    let guard = forward::install()?;
    mask.prepare_child(&mut command);
    if limits.is_some() {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    forward::check_sigchld()?;
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
    let child = spawn?;
    let owner = ChildOwner::new(child, limits.is_some())?;
    drop(mask);
    let (output, resource_limit) = owner.run(mode, sink, limits)?;
    let cancelled = guard.restore()?;
    Ok(Reaped {
        output,
        cancelled,
        resource_limit,
    })
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

/// Run one direct external command through the shared signal forward/reap
/// boundary used for native Nix children, capturing stdout and the
/// classified outcome.
///
/// Termination signals sent only to pkg are forwarded to the child, the
/// child is reaped, and pkg stays alive, so an interrupted command is
/// reported as [`Outcome::Interrupted`] instead of killing pkg mid-run.
/// Stderr is captured into the outcome; stdout is returned alongside it.
/// # Errors
///
/// Returns an error if signal setup, child setup, spawning, signal forwarding,
/// exit observation, stream supervision, reaping, or signal restoration fails.
/// Nonzero exits and cancellation are returned as [`Outcome`] values.
pub fn run_direct_captured(
    executable: &std::path::Path,
    args: &[String],
) -> Result<(Outcome, String), String> {
    let mut command = Command::new(executable);
    command.args(args);
    run_child(command, IoMode::Capture, None)
        .map_err(|error| error.to_string())
        .map(|reaped| {
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
/// # Errors
///
/// Returns an error if signal setup, child setup, spawning, signal forwarding,
/// exit observation, stream supervision, reaping, or signal restoration fails.
/// Nonzero exits and cancellation are returned as [`Outcome`] values.
pub fn run_direct(executable: &std::path::Path, args: &[String]) -> Result<Outcome, String> {
    run_direct_captured(executable, args).map(|(outcome, _)| outcome)
}

/// The cancellation signal for a finished child, when the run was
/// cancelled: a signalled child, then a child that exited 130 after
/// handling SIGINT itself.
fn unix_signal(status: ExitStatus) -> Option<i32> {
    if let Some(signal) = platform_signal(status) {
        return Some(signal);
    }
    // A child that exits with status 130 reports a handled SIGINT; treat it
    // as interruption so native state is still re-read.
    (status.code() == Some(130)).then_some(2)
}

fn platform_signal(status: ExitStatus) -> Option<i32> {
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

fn exit_status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => format!("terminated by signal: {status}"),
    }
}
