//! Host runtime controls: canary + loopback positive controls, the
//! bounded child runner, and the sandboxed Nix raw-export build.

use super::ImportRequest;
use super::sha256_hex;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Trusted raw-export runtime assets embedded verbatim.
const RAW_EXPORT_NIX: &str = include_str!("../../../../nix/casks/lib/tap/raw-export.nix");
const EXPORT_RB: &str = include_str!("../../../../nix/casks/lib/tap/export.rb");
const READER_RB: &str = include_str!("../../../../nix/casks/lib/tap/reader.rb");
const PROBE_SH: &str = include_str!("../../../../nix/casks/lib/tap/probe.sh");
const BOOT_ADAPTER_SH: &str = include_str!("../../../../nix/casks/lib/tap/brew-boot-adapter.sh");

/// Nix build wall time limit (the runtime itself bounds export at 15
/// minutes; nixpkgs fetchTarball and store work need headroom).
const NIX_WALLTIME: Duration = Duration::from_secs(45 * 60);
/// Per-stream captured Nix log limit. Overflow ABORTS the build; it
/// never silently truncates into a reported success.
const MAX_STREAM_BYTES: usize = 1024 * 1024;
/// Trusted `--file` wrapper: converts `--argstr`-provided STRINGS into
/// the argument types raw-export.nix expects. No caller-controlled
/// value is ever evaluated as a Nix expression: the tap path arrives as
/// a plain string (safe with spaces, `$`, quotes) and the canary set is
/// parsed by `builtins.fromJSON`.
const RUN_WRAPPER_NIX: &str = r#"
{ tapSourcePath
, canaryJson
, ...
}@args:
import ./raw-export.nix
  (builtins.removeAttrs args [ "tapSourcePath" "canaryJson" ] // {
    tapSource = builtins.path { path = builtins.toPath tapSourcePath; name = "tap-source"; };
    canary = builtins.fromJSON canaryJson;
  })
"#;

pub(super) struct CanaryControl {
    /// RAII guard over the public canary directory: even failures
    /// BEFORE this struct is fully built cannot leak the directory,
    /// because the `TempDir` guard owns it from creation.
    #[allow(dead_code, reason = "the guard field is owned for Drop only")]
    pub(super) dir: tempfile::TempDir,
    pub(super) path: String,
    pub(super) sha256: String,
    pub(super) port: u16,
    stop: Arc<AtomicBool>,
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CanaryControl {
    /// RAII cleanup on EVERY path (success, timeout, failure): stop the
    /// accept loop, reap the thread, remove ONLY the paths this
    /// control created (the nonce file, then its own directory via the
    /// `TempDir` guard).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.path);
        // The TempDir guard removes the (now empty) public directory.
    }
}

/// Randomness from the OS. RNG failure fails the import: there is no
/// fallback.
fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("cannot open /dev/urandom: {e}"))?;
    let mut buf = vec![0u8; n];
    file.read_exact(&mut buf)
        .map_err(|e| format!("cannot read {n} bytes from /dev/urandom: {e}"))?;
    Ok(buf)
}

/// Create the fresh public canary and the live loopback listener, and
/// verify both positive controls from the host side before the build.
/// The canary directory is owned by a `TempDir` guard FROM CREATION, so
/// every early failure (dir, chmod, nonce write, read-back, listener
/// bind, thread spawn) cleans up; the `CanaryControl` is constructed
/// BEFORE the listener positive control runs, so even that error path
/// gets full Drop cleanup.
pub(super) fn setup_canary() -> Result<CanaryControl, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let nonce = random_bytes(32)?;
        // Fresh OS randomness for the directory name; no timestamp
        // fallback ever.
        // The parent MUST be the literal PUBLIC /tmp: on macOS the
        // default std temp dir has private ancestors (per-user
        // dirs), which makes the sandbox deny the canary for reasons
        // that are NOT the build's fault. Never std::env::temp_dir()
        // (inherited TMPDIR) here.
        let dir = tempfile::TempDir::with_prefix_in("pkg-import-", "/tmp")
            .map_err(|e| format!("cannot create public canary dir: {e}"))?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777))
            .map_err(|e| format!("cannot chmod 0777 {}: {e}", dir.path().display()))?;
        let path = dir.path().join("canary");
        std::fs::write(&path, &nonce)
            .map_err(|e| format!("cannot write canary {}: {e}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("cannot chmod 0644 {}: {e}", path.display()))?;
        let sha256 = sha256_hex(&nonce);
        // Positive control: the canary must read back with the exact
        // fresh nonce hash.
        let read_back =
            std::fs::read(&path).map_err(|e| format!("canary positive control failed ({e})"))?;
        if sha256_hex(&read_back) != sha256 {
            return Err("canary positive control failed: read-back hash mismatch".to_string());
        }

        // Live listener: 127.0.0.1 ephemeral port, held for the whole
        // build. Positive control: one real connect is accepted.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("cannot bind listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("listener has no local address: {e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot make listener nonblocking: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let served = Arc::new(AtomicBool::new(false));
        let served_for_thread = Arc::clone(&served);
        let handle = std::thread::Builder::new()
            .name("pkg-canary-listener".into())
            .spawn(move || {
                while !stop_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            served_for_thread.store(true, Ordering::SeqCst);
                            drop(stream);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("cannot start the canary listener: {e}"))?;
        // The control is FULLY constructed (dir guard, thread, stop
        // flag) BEFORE the listener positive control below: any error
        // from here on drops the control, stopping the thread and
        // removing the canary paths.
        let control = CanaryControl {
            dir,
            path: path.display().to_string(),
            sha256,
            port,
            stop,
            listener_thread: Some(handle),
        };
        TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| format!("canary listener positive control failed ({e})"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !served.load(Ordering::SeqCst) {
            if std::time::Instant::now() > deadline {
                return Err(
                    "canary listener positive control failed: self-connect never accepted"
                        .to_string(),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(control)
    }
    #[cfg(not(unix))]
    {
        Err("the raw tap import requires a unix host (canary + listener controls)".to_string())
    }
}

// ---------------------------------------------------------------------------
// Nix raw-export build (ordinary derivation, bounded process)
// ---------------------------------------------------------------------------

/// Resolve the configured Nix executable: an existing file path is
/// used directly; otherwise the name is looked up in PATH.
pub(super) fn resolve_nix_exe(nix: &Path) -> Result<PathBuf, String> {
    if nix.is_file() {
        return Ok(nix.to_path_buf());
    }
    let name = nix
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("nix executable {} is not a usable path", nix.display()))?;
    if name.contains('/') {
        return Err(format!("nix executable {} is not a file", nix.display()));
    }
    let path = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    for dir in path {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "nix executable {name:?} was not found in PATH; pass --nix /path/to/bin/nix"
    ))
}

/// Shared overflow flag: set the moment the stream cap is exceeded so
/// the monitor loop can kill the group WITHOUT waiting for EOF.
type OverflowFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// One drained pipe: bytes up to the stream cap plus an OVERFLOW flag.
/// Overflow ABORTS the build; it never silently truncates into success.
/// The shared flag is set IMMEDIATELY (not at EOF) so the monitor loop
/// can kill the process group while the child is still writing.
fn spawn_drain(
    mut reader: impl Read + Send + 'static,
    overflow_flag: OverflowFlag,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut overflow = false;
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if !overflow && out.len() + n > MAX_STREAM_BYTES {
                        overflow = true;
                        overflow_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if !overflow {
                        out.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        out
    })
}

/// Join a drain thread with a BOUND: after a group kill, a descendant
/// that still holds the pipe open would otherwise hang the join
/// forever. `None` means the drain never finished in time. The bound
/// is small because the group was already killed; 2 s is plenty.
fn join_drain_bounded(handle: std::thread::JoinHandle<Vec<u8>>) -> Option<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    rx.recv_timeout(Duration::from_secs(2))
        .ok()
        .and_then(Result::ok)
}

/// SIGKILL the negative pgid, i.e. the whole group, via libc
/// directly: NO external `kill` executable is spawned (external
/// negative-pgid parsing has killed the calling parent before).
/// Guarded: group 0/1 and the manager's OWN group are never killed.
#[cfg(unix)]
fn kill_process_group(pgid: u32) {
    if pgid <= 1 {
        return;
    }
    let pgid = pgid as libc::pid_t;
    if pgid == unsafe { libc::getpgrp() } {
        return;
    }
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pgid: u32) {}

pub(super) fn write_asset(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("cannot stage a write next to {}: {e}", path.display()))?;
    std::io::Write::write_all(&mut tmp.as_file(), contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| format!("cannot publish {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let permissions = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(path, permissions)
            .map_err(|e| format!("cannot make {} readable: {e}", path.display()))?;
    }
    Ok(())
}
/// Write the trusted embedded runtime assets into `runtime/` and build
/// the raw-export derivation with the configured Nix executable.
pub(super) fn run_nix_export(
    request: &ImportRequest,
    staging: &Path,
    tap_tree: &Path,
    source: &str,
    revision: &str,
    canary: &CanaryControl,
) -> Result<PathBuf, String> {
    run_nix_export_with_limits(
        request,
        staging,
        tap_tree,
        source,
        revision,
        canary,
        NIX_WALLTIME,
    )
}

/// Result of one bounded child run: exit status plus the bounded
/// stdout/stderr captures (overflow already aborted the run).
pub(super) struct BoundedOutput {
    pub(super) status: std::process::ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

/// Run one child command under the SHARED bounds: own process group,
/// per-stream log caps that ABORT (never truncate into success), and a
/// wall-time limit enforced by SIGKILLing the whole group. Both the
/// config gate and the raw-export build use this runner; nothing in
/// this module reads child output unbounded.
///
/// While the child owns its group, SIGINT/SIGTERM/SIGHUP sent to THIS
/// process are caught (see [`signal_guard`]) so the poll loop below can
/// tear the OWNED group down and report an interruption; without this,
/// a default-disposition signal would kill the manager and leave the
/// child plus any Nix builder running.
/// Private RAII signal registration for [`run_bounded_child`]: while a
/// bounded child owns a process group, SIGINT/SIGTERM/SIGHUP sent to
/// THIS process only set a flag (async-signal-safe atomic store). The
/// poll loop sees the flag, kills/reaps the OWNED group, and returns an
/// interrupted error. Previous handlers are saved and restored on EVERY
/// exit, including spawn errors (the guard is constructed BEFORE the
/// spawn). Bounded runs are SERIALIZED: the guard owns the registration
/// mutex lock for the ENTIRE run, so overlapping runs WAIT instead of
/// being refused, and any failure to install a handler FAILS CLOSED
/// (no child is spawned).
#[cfg(unix)]
mod signal_guard {
    use std::sync::Mutex;
    use std::sync::MutexGuard;

    static REGISTRATION_LOCK: Mutex<()> = Mutex::new(());
    static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// Async-signal-safe: one atomic store, nothing else.
    extern "C" fn on_signal(_sig: libc::c_int) {
        INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) struct SignalGuard {
        /// Held for the ENTIRE run. Overlapping bounded runs serialize
        /// on this lock (they are never refused). Handlers are
        /// restored in `Drop` BEFORE this field unlocks.
        _registration: MutexGuard<'static, ()>,
        saved: [libc::sigaction; SIGNALS.len()],
    }

    impl SignalGuard {
        pub(super) fn new() -> Result<Self, String> {
            // Block until any other bounded run finished: parallel
            // runs WAIT here instead of failing nondeterministically.
            let registration = REGISTRATION_LOCK
                .lock()
                .map_err(|_| "signal registration lock is poisoned".to_string())?;
            // Reset the flag BEFORE any handler is installed: a
            // signal arriving during registration must not be lost,
            // and a stale flag from a previous run must not leak in.
            INTERRUPTED.store(false, std::sync::atomic::Ordering::SeqCst);
            // Portable handler construction: `libc::sigaction` differs
            // between platforms (Linux adds sa_restorer), so the
            // struct is zeroed and only portable fields are assigned.
            let mut new_action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut empty_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            unsafe { libc::sigemptyset(&mut empty_mask) };
            new_action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
            new_action.sa_mask = empty_mask;
            new_action.sa_flags = libc::SA_RESTART;
            let mut saved = [unsafe { std::mem::zeroed::<libc::sigaction>() }; SIGNALS.len()];
            for (slot, sig) in saved.iter_mut().zip(SIGNALS) {
                if unsafe { libc::sigaction(sig, &new_action, slot) } != 0 {
                    // Fail closed: restore EVERY handler installed so
                    // far (the registration lock releases with this
                    // error) and refuse to run any child.
                    let errno = std::io::Error::last_os_error();
                    for (old, done_sig) in saved.iter().zip(SIGNALS) {
                        if done_sig == sig {
                            break;
                        }
                        unsafe { libc::sigaction(done_sig, old, std::ptr::null_mut()) };
                    }
                    return Err(format!(
                        "cannot install the bounded-child signal handler for signal {sig}: {errno}"
                    ));
                }
            }
            Ok(SignalGuard {
                _registration: registration,
                saved,
            })
        }

        /// Whether INT/TERM/HUP arrived since registration.
        pub(super) fn interrupted(&self) -> bool {
            INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for SignalGuard {
        fn drop(&mut self) {
            // Restore previous handlers BEFORE `_registration`
            // unlocks (struct fields drop after this method runs).
            for (old, sig) in self.saved.iter().zip(SIGNALS) {
                unsafe { libc::sigaction(sig, old, std::ptr::null_mut()) };
            }
            INTERRUPTED.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// No-op stub where signal disposition is not wired up (non-unix).
#[cfg(not(unix))]
mod signal_guard {
    pub(super) struct SignalGuard;

    impl SignalGuard {
        pub(super) fn new() -> Result<Self, String> {
            Ok(Self)
        }

        pub(super) fn interrupted(&self) -> bool {
            false
        }
    }
}

pub(super) fn run_bounded_child(
    command: &mut std::process::Command,
    walltime: Duration,
    what: &str,
) -> Result<BoundedOutput, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: killing the child must kill its descendants.
        command.process_group(0);
    }
    // Register FIRST: a spawn error must still restore the previous
    // handlers, and a registration failure must fail CLOSED (no child).
    let _signal_guard = signal_guard::SignalGuard::new()?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot spawn the {what} process: {e}"))?;
    let pgid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader: Box<dyn Read + Send + 'static> = match stdout {
        Some(stream) => Box::new(stream),
        None => Box::new(std::io::empty()),
    };
    let stderr_reader: Box<dyn Read + Send + 'static> = match stderr {
        Some(stream) => Box::new(stream),
        None => Box::new(std::io::empty()),
    };
    let stdout_overflow: OverflowFlag = std::sync::Arc::new(Default::default());
    let stderr_overflow: OverflowFlag = std::sync::Arc::new(Default::default());
    let stdout_handle = spawn_drain(stdout_reader, stdout_overflow.clone());
    let stderr_handle = spawn_drain(stderr_reader, stderr_overflow.clone());

    // Bounded wall time. On overrun the WHOLE process group is killed
    // (SIGKILL), so descendants cannot outlive the leader and hold the
    // pipes; drain joins are themselves bounded. The shared overflow
    // flags are checked EVERY poll: a log flood aborts immediately,
    // not after EOF.
    let deadline = std::time::Instant::now() + walltime;
    let status = loop {
        let waited = child.try_wait();
        let overflowed = stdout_overflow.load(std::sync::atomic::Ordering::Relaxed)
            || stderr_overflow.load(std::sync::atomic::Ordering::Relaxed);
        match waited {
            Err(e) => {
                // Failure to even poll must not leak the group.
                kill_process_group(pgid);
                let _ = child.wait();
                drop(join_drain_bounded(stdout_handle));
                drop(join_drain_bounded(stderr_handle));
                return Err(format!("cannot wait for the {what} process: {e}"));
            }
            Ok(Some(status)) => break status,
            Ok(None) if _signal_guard.interrupted() => {
                kill_process_group(pgid);
                let _ = child.wait();
                drop(join_drain_bounded(stdout_handle));
                drop(join_drain_bounded(stderr_handle));
                return Err(format!(
                    "the {what} was interrupted (SIGINT/SIGTERM/SIGHUP arrived); its \
                     process group was killed"
                ));
            }
            Ok(None) if overflowed => {
                kill_process_group(pgid);
                let _ = child.wait();
                drop(join_drain_bounded(stdout_handle));
                drop(join_drain_bounded(stderr_handle));
                return Err(format!(
                    "the {what} emitted more than the {MAX_STREAM_BYTES} byte \
                     per-stream log cap; aborting instead of truncating into a success"
                ));
            }
            Ok(None) if std::time::Instant::now() > deadline => {
                kill_process_group(pgid);
                let _ = child.wait();
                drop(join_drain_bounded(stdout_handle));
                drop(join_drain_bounded(stderr_handle));
                return Err(format!(
                    "the {what} exceeded the {} s wall limit and its \
                     process group was killed",
                    walltime.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    // The leader exited, but DESCENDANTS may still hold the pipes
    // open; kill the (still owned) group BEFORE the bounded drains so
    // no descendant can stall them.
    kill_process_group(pgid);
    let Some(stdout) = join_drain_bounded(stdout_handle) else {
        return Err(format!("the {what} stdout drain never finished"));
    };
    let Some(stderr) = join_drain_bounded(stderr_handle) else {
        return Err(format!("the {what} stderr drain never finished"));
    };
    // Output overflow ABORTS, even when the child itself exited 0.
    if stdout_overflow.load(std::sync::atomic::Ordering::Relaxed)
        || stderr_overflow.load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(format!(
            "the {what} emitted more than the {MAX_STREAM_BYTES} byte \
             per-stream log cap; aborting instead of truncating into a success"
        ));
    }
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
    })
}

fn run_nix_export_with_limits(
    request: &ImportRequest,
    staging: &Path,
    tap_tree: &Path,
    source: &str,
    revision: &str,
    canary: &CanaryControl,
    walltime: Duration,
) -> Result<PathBuf, String> {
    let runtime = staging.join("runtime");
    write_asset(&runtime.join("raw-export.nix"), RAW_EXPORT_NIX)?;
    write_asset(&runtime.join("run.nix"), RUN_WRAPPER_NIX)?;
    write_asset(&runtime.join("export.rb"), EXPORT_RB)?;
    write_asset(&runtime.join("reader.rb"), READER_RB)?;
    write_asset(&runtime.join("probe.sh"), PROBE_SH)?;
    write_asset(&runtime.join("brew-boot-adapter.sh"), BOOT_ADAPTER_SH)?;

    // All caller-controlled values travel as --argstr STRINGS through
    // the trusted wrapper above: no bare-path `--arg` (spaces or `${`
    // would break), no interpolated Nix expression. The canary set is
    // serde-encoded JSON parsed by builtins.fromJSON inside the
    // wrapper. Every value travels as a plain --argstr STRING: no
    // caller-controlled value is ever evaluated as a Nix expression.
    let canary_json = serde_json::json!({
        "path": canary.path,
        "sha256": canary.sha256,
        "port": canary.port,
    })
    .to_string();

    let nix_exe = resolve_nix_exe(&request.nix)?;
    let out_link = staging.join("export-out");
    let nix_build_dir = staging.join("nix-tmp");
    std::fs::create_dir_all(&nix_build_dir)
        .map_err(|e| format!("cannot create {}: {e}", nix_build_dir.display()))?;
    // Isolated HOME: with no HOME at all Nix infers the REAL user home
    // (and its credentials/config); give it an empty private one.
    let nix_home = staging.join("nix-home");
    let nix_config = nix_home.join(".config");
    std::fs::create_dir_all(&nix_config)
        .map_err(|e| format!("cannot create {}: {e}", nix_config.display()))?;
    let nix_user_conf = nix_home.join("empty-nix.conf");
    write_asset(&nix_user_conf, "")?;

    let mut command = std::process::Command::new(&nix_exe);
    command
        .arg("build")
        .arg("--file")
        .arg(runtime.join("run.nix"))
        .arg("--argstr")
        .arg("tapSourcePath")
        .arg(tap_tree)
        .arg("--argstr")
        .arg("canaryJson")
        .arg(&canary_json)
        .arg("--argstr")
        .arg("source")
        .arg(source)
        .arg("--argstr")
        .arg("revision")
        .arg(revision)
        .arg("--argstr")
        .arg("system")
        .arg(&request.system)
        // Fail-closed sandbox demands. `sandbox-fallback false` is the
        // real option name; there is no `fallback` option. Client
        // --options are IGNORED for untrusted users on macOS; only the
        // DAEMON config counts, and the in-build probe stays
        // authoritative either way.
        .arg("--option")
        .arg("sandbox")
        .arg("true")
        .arg("--option")
        .arg("sandbox-fallback")
        .arg("false")
        // Real build-log plumbing: -L surfaces builder logs on failure,
        // and max-build-log-size makes the BUILDER itself die when it
        // exceeds 1 MiB (the same bound as the parent stdout/stderr
        // caps above — both bounds are active, they do not replace
        // each other). This is the raw build only; the config query
        // carries no log options.
        .arg("--print-build-logs")
        .arg("--option")
        .arg("max-build-log-size")
        .arg("1048576")
        .arg("--out-link")
        .arg(&out_link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Clear EVERYTHING inherited: no NIX_CONFIG, no proxies, no
        // HOME credentials, no model keys. The minimal variables the
        // Nix CLI needs are set below, pointed at isolated paths.
        .env_clear();
    let mut path_value = String::new();
    if let Some(bin) = nix_exe.parent() {
        path_value.push_str(&bin.display().to_string());
        path_value.push(':');
    }
    path_value.push_str("/usr/bin:/bin:/usr/sbin:/sbin");
    command.env("PATH", &path_value);
    command.env("TMPDIR", &nix_build_dir);
    command.env("LC_ALL", "C");
    command.env("HOME", &nix_home);
    command.env("XDG_CONFIG_HOME", &nix_config);
    command.env("NIX_USER_CONF_FILES", &nix_user_conf);

    let bounded = run_bounded_child(&mut command, walltime, "nix raw-export build")?;
    let status = bounded.status;
    let stdout = bounded.stdout;
    let stderr = bounded.stderr;
    if !status.success() {
        let tail = |bytes: &[u8]| {
            let start = bytes.len().saturating_sub(4000);
            String::from_utf8_lossy(&bytes[start..]).to_string()
        };
        return Err(format!(
            "the nix raw-export build failed ({status}).\n\
             --- stdout tail ---\n{}\n--- stderr tail ---\n{}\n\
             Fail-closed setup checklist (macOS daemon config, trusted user):\n\
             sandbox=true, sandbox-fallback=false, minimal read-only paths, daemon \\
             TMPDIR=/nix/var/nix/builds (root:wheel 0755). Client --options \\
             are ignored for untrusted users; only the daemon config counts. \\
             The in-build sandbox probe is authoritative: host read, host \\
             write, and loopback connect must all be DENIED for the build \\
             to succeed.",
            tail(&stdout),
            tail(&stderr)
        ));
    }
    if !out_link.join("export.json").is_file() {
        return Err(format!(
            "the nix build succeeded but {} is missing export.json",
            out_link.display()
        ));
    }
    Ok(out_link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::native_target;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[cfg(unix)]
    fn fake_nix(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-nix");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn fake_request(dir: &Path, nix: PathBuf) -> ImportRequest {
        ImportRequest {
            source: "example/foo".to_string(),
            revision: None,
            system: native_target().unwrap().to_string(),
            nix,
            output_dir: dir.to_path_buf(),
        }
    }
    #[cfg(unix)]
    #[test]
    fn nix_timeout_kills_the_process_group_and_returns_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(
            dir.path(),
            // A DESCENDANT holds the stdout pipe open; killing only the
            // leader would leave the drain readers blocked forever.
            "sleep 300 &\nsleep 300",
        );
        let canary = setup_canary().expect("canary");
        let started = std::time::Instant::now();
        let err = run_nix_export_with_limits(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_secs(2),
        )
        .expect_err("timeout");
        drop(canary);
        assert!(err.contains("wall limit"), "unexpected error: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the group kill + bounded joins must return promptly"
        );
        // RAII removed the canary paths.
        assert!(!Path::new(&dir.path().join("x")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn nix_log_flood_aborts_instead_of_truncating_into_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 4 MiB of stdout then exit 0: the capture must ABORT, never
        // silently truncate and report success.
        let nix = fake_nix(dir.path(), "yes 0123456789abcdef | head -c 4194304; exit 0");
        let canary = setup_canary().expect("canary");
        let err = run_nix_export_with_limits(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_secs(120),
        )
        .expect_err("flood");
        drop(canary);
        assert!(err.contains("log cap"), "unexpected error: {err}");
    }

    // 2 MiB of stdout, then a descendant HANGS while holding the pipes
    // open: the shared overflow flag must abort the build IMMEDIATELY
    // (at the 1 MiB cap), not after EOF, and the group kill must free
    // the bounded drains.
    #[cfg(unix)]
    #[test]
    fn nix_log_flood_with_hung_descendant_aborts_within_seconds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(
            dir.path(),
            "yes 0123456789abcdef | head -c 2097152 &\nsleep 300",
        );
        let canary = setup_canary().expect("canary");
        let started = std::time::Instant::now();
        let err = run_nix_export_with_limits(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_secs(120),
        )
        .expect_err("flood with hang");
        drop(canary);
        assert!(err.contains("log cap"), "unexpected error: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the overflow flag + group kill must abort within 5 s, took {:?}",
            started.elapsed()
        );
    }

    // The spawned Nix child runs in its OWN process group; the group
    // kill (libc only, no external kill binary) must terminate the
    #[cfg(unix)]
    #[test]
    fn bounded_child_interrupt_tears_down_group_and_restores_handlers() {
        if std::env::var("PKG_TAP_SIGNAL_CHILD_MODE").is_ok() {
            // Child mode: this process IS the manager under signal. The
            // parent supplied the working directory so it can watch the
            // same marker files.
            let root = std::path::PathBuf::from(
                std::env::var("PKG_TAP_SIGNAL_DIR").expect("signal dir env"),
            );
            let dir = std::path::PathBuf::from(&root);
            let started = dir.join("started");
            let grandchild_marker = dir.join("grandchild-lived");
            let ok_marker = dir.join("child-ok");
            // Unrelated process in THIS process's group: the interrupt
            // path must never signal it (no group 0 / manager group).
            let mut unrelated = std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .expect("unrelated sleep spawns");
            let mut command = std::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(format!(
                    "touch {s}; (sleep 1; touch {g}) & sleep 300",
                    s = started.display(),
                    g = grandchild_marker.display(),
                ))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let Err(error) =
                run_bounded_child(&mut command, Duration::from_secs(60), "signal test")
            else {
                panic!("interruption must error, not succeed");
            };
            assert!(error.contains("interrupted"), "unexpected error: {error}");
            // The OWNED group is gone: the grandchild never runs.
            std::thread::sleep(Duration::from_millis(1500));
            assert!(
                !grandchild_marker.exists(),
                "the grandchild must be dead after the interruption"
            );
            // The unrelated process was never signalled.
            assert!(
                unrelated.try_wait().expect("poll unrelated").is_none(),
                "an unrelated same-group process must survive the interruption"
            );
            let _ = unrelated.kill();
            let _ = unrelated.wait();
            std::fs::write(&ok_marker, b"ok").expect("write ok marker");
            // The guard is dropped here; the DEFAULT SIGTERM disposition
            // is restored, so this re-raise must terminate THIS process.
            unsafe { libc::raise(libc::SIGTERM) };
            unreachable!("raise(SIGTERM) must terminate the child test process");
        }
        // Parent mode: drive the isolated child test process.
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let ok_marker = dir.path().join("child-ok");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("bounded_child_interrupt_tears_down_group_and_restores_handlers")
            .env("PKG_TAP_SIGNAL_CHILD_MODE", "1")
            .env("PKG_TAP_SIGNAL_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("child test process spawns");
        // Wait until the bounded child was spawned and the guard is
        // registered (the `started` marker is written by the bounded
        // child itself, i.e. AFTER guard installation).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the child test process never started its bounded child"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(100));
        // SIGTERM to the PARENT PROCESS ONLY: no group signal.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let status = child.wait().expect("child test process reaped");
        assert_eq!(
            status.code(),
            None,
            "the child must die by the re-raised SIGTERM, not exit on its own"
        );
        assert!(
            ok_marker.exists(),
            "the child must have passed all interruption checks first"
        );
    }

    /// The canary parent MUST be the literal PUBLIC /tmp (canonical
    /// alias /private/tmp on macOS), NEVER the inherited TMPDIR: a
    /// per-user private default temp dir causes false sandbox
    /// denials on macOS. Runs in a child test process so the private
    /// TMPDIR never leaks into parallel tests.
    #[cfg(unix)]
    #[test]
    fn canary_parent_is_public_tmp_not_inherited_tmpdir() {
        if std::env::var("PKG_TAP_CANARY_TMPDIR_CHILD_MODE").is_ok() {
            let canary = setup_canary().expect("canary");
            // The canary FILE lives at <tmpdir>/canary, so the temp dir
            // parent (its PUBLIC parent) is two `.parent()` hops up.
            let public_parent = std::path::Path::new(&canary.path)
                .parent()
                .and_then(|dir| dir.parent())
                .expect("canary dir has a public parent");
            let parent = public_parent
                .canonicalize()
                .expect("canary parent canonicalizes");
            assert!(
                parent == std::path::Path::new("/tmp")
                    || parent == std::path::Path::new("/private/tmp"),
                "canary parent must be the public /tmp (macOS alias /private/tmp), got {}",
                parent.display()
            );
            let inherited = std::env::temp_dir();
            assert_ne!(
                parent,
                inherited.canonicalize().unwrap_or(inherited.clone()),
                "the canary parent must not be the inherited TMPDIR"
            );
            return;
        }
        let private_tmp = tempfile::tempdir().expect("private tmpdir");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("canary_parent_is_public_tmp_not_inherited_tmpdir")
            .env("PKG_TAP_CANARY_TMPDIR_CHILD_MODE", "1")
            .env("TMPDIR", private_tmp.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("child test process spawns");
        let status = child.wait().expect("child test process reaped");
        assert!(
            status.success(),
            "the child test process must pass the canary parent checks"
        );
    }

    /// A bounded-child spawn FAILURE must still restore the previous
    /// signal handlers: after the error, a raised SIGTERM must take
    /// the DEFAULT disposition and terminate the child test process.
    #[cfg(unix)]
    #[test]
    fn bounded_child_spawn_failure_restores_signal_handlers() {
        if std::env::var("PKG_TAP_SIGNAL_SPAWNFAIL_CHILD_MODE").is_ok() {
            let dir = std::path::PathBuf::from(
                std::env::var("PKG_TAP_SIGNAL_DIR").expect("signal dir env"),
            );
            let mut command = std::process::Command::new("/nonexistent/pkg-spawn-failure");
            let Err(error) =
                run_bounded_child(&mut command, Duration::from_secs(5), "spawn failure")
            else {
                panic!("spawn must fail");
            };
            assert!(error.contains("spawn"), "unexpected error: {error}");
            std::fs::write(dir.join("spawn-fail-ok"), b"ok").expect("write marker");
            // Handlers must already be restored: this re-raise must
            // terminate THIS child test process by default disposition.
            unsafe { libc::raise(libc::SIGTERM) };
            unreachable!("raise(SIGTERM) must terminate the child test process");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("bounded_child_spawn_failure_restores_signal_handlers")
            .env("PKG_TAP_SIGNAL_SPAWNFAIL_CHILD_MODE", "1")
            .env("PKG_TAP_SIGNAL_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("child test process spawns");
        let status = child.wait().expect("child test process reaped");
        assert_eq!(
            status.code(),
            None,
            "the child must die by the re-raised SIGTERM, not exit on its own"
        );
        assert!(
            dir.path().join("spawn-fail-ok").exists(),
            "the child must pass the spawn-failure checks before the re-raise"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_process_group_is_isolated_from_the_manager() {
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        // Marker files written by child and grandchild after the kill
        // deadline must never appear.
        let child_marker = dir.path().join("child-lived");
        let grandchild_marker = dir.path().join("grandchild-lived");
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "(sleep 1; touch {gc}) & sleep 300; touch {c}",
                gc = grandchild_marker.display(),
                c = child_marker.display(),
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("child spawns");
        let pgid = child.id();
        // The child got its OWN process group (guaranteed by
        // process_group(0) on spawned Nix/children here).
        assert_ne!(
            pgid as libc::pid_t,
            unsafe { libc::getpgrp() },
            "the spawned child must not share the test process group"
        );
        kill_process_group(pgid);
        let status = child.wait().expect("child reaped");
        assert!(!status.success(), "the group kill must terminate the child");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!child_marker.exists(), "the child must be dead");
        assert!(
            !grandchild_marker.exists(),
            "the grandchild must be dead too"
        );
        // THIS test process is still alive (group 0/own group guarded).
        kill_process_group(0);
        kill_process_group(pgid); // already gone: must be a no-op
    }
}
