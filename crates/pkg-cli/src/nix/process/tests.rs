use std::fs;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{ChildLimits, IoMode, Outcome, StderrSink, run_child, run_child_with_limits};

const MODE: &str = "PKG_PROCESS_SIGNAL_TEST";
const READY: &str = "PKG_PROCESS_SIGNAL_READY";
const SIGNAL: &str = "PKG_PROCESS_SIGNAL_NUMBER";
const MARKER: &str = "PKG_PROCESS_SPAWN_MARKER";

static MIXED_INT_COUNT: AtomicUsize = AtomicUsize::new(0);
static MIXED_TERM_COUNT: AtomicUsize = AtomicUsize::new(0);
static MIXED_HUP_COUNT: AtomicUsize = AtomicUsize::new(0);

struct MixedSignalSink {
    bytes: Vec<u8>,
    raised: bool,
    raise_failed: bool,
}

impl StderrSink for MixedSignalSink {
    fn chunk(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        if self.raised
            || !self
                .bytes
                .windows(b"READY\n".len())
                .any(|part| part == b"READY\n")
        {
            return;
        }
        self.raised = true;
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: `signal` is a valid, installed process signal handler.
            if unsafe { libc::raise(signal) } != 0 {
                self.raise_failed = true;
                return;
            }
        }
    }

    fn finish(&mut self, _success: bool) {}
}

extern "C" fn count_mixed_signal(signal: libc::c_int) {
    let counter = match signal {
        libc::SIGINT => &MIXED_INT_COUNT,
        libc::SIGTERM => &MIXED_TERM_COUNT,
        libc::SIGHUP => &MIXED_HUP_COUNT,
        _ => return,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn mixed_signal_child() {
    if std::env::var(MODE).as_deref() != Ok("mixed-signal-child") {
        return;
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the action is initialized and the handler only updates an atomic.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = count_mixed_signal as *const () as usize;
        // SAFETY: `action` has an initialized mask and a valid signal handler.
        unsafe {
            libc::sigemptyset(&raw mut action.sa_mask);
            assert_eq!(
                libc::sigaction(signal, &raw const action, std::ptr::null_mut()),
                0
            );
        }
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "READY").expect("publish mixed-signal readiness");
    let deadline = Instant::now() + Duration::from_secs(2);
    while [
        MIXED_INT_COUNT.load(Ordering::Relaxed),
        MIXED_TERM_COUNT.load(Ordering::Relaxed),
        MIXED_HUP_COUNT.load(Ordering::Relaxed),
    ]
    .contains(&0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    writeln!(
        stderr,
        "SIGNALS={},{},{}",
        MIXED_INT_COUNT.load(Ordering::Relaxed),
        MIXED_TERM_COUNT.load(Ordering::Relaxed),
        MIXED_HUP_COUNT.load(Ordering::Relaxed)
    )
    .expect("write mixed-signal counts");
}

#[test]
fn mixed_termination_signals_are_all_forwarded() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::mixed_signal_child",
            "--nocapture",
        ])
        .env(MODE, "mixed-signal-child");
    let mut sink = MixedSignalSink {
        bytes: Vec::new(),
        raised: false,
        raise_failed: false,
    };
    let reaped = run_child_with_limits(
        command,
        IoMode::Filtered,
        Some(&mut sink),
        Some(ChildLimits {
            resident_bytes: u64::MAX,
            wall_time: Duration::from_secs(3),
        }),
    )
    .expect("run mixed-signal child");
    assert!(sink.raised, "child readiness must trigger the signal batch");
    assert!(!sink.raise_failed);
    assert_eq!(
        reaped.classify(),
        Outcome::Interrupted {
            signal: libc::SIGHUP
        }
    );
    assert!(
        String::from_utf8_lossy(&sink.bytes).contains("SIGNALS=1,1,1"),
        "child must receive every signal kind: {}",
        String::from_utf8_lossy(&sink.bytes)
    );
}

#[test]
fn argv_child() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--skip") {
        println!(
            "PKG_ARGV={}",
            serde_json::to_string(&args[index + 1]).unwrap()
        );
    }
}

#[test]
fn spawn_marker_child() {
    if std::env::var(MODE).as_deref() != Ok("spawn-marker-child") {
        return;
    }
    fs::write(std::env::var(MARKER).unwrap(), "spawned").unwrap();
}

#[test]
fn auto_reap_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("auto-reap-supervisor") {
        return;
    }
    let marker = std::env::var(MARKER).unwrap();
    // SAFETY: SIGCHLD is a valid signal and SIG_IGN is a valid disposition.
    assert_ne!(
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) },
        libc::SIG_ERR
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::spawn_marker_child",
            "--nocapture",
        ])
        .env(MODE, "spawn-marker-child")
        .env(MARKER, &marker);
    let error = run_child(command, IoMode::Capture, None)
        .err()
        .expect("auto-reaped children cannot be supervised safely");
    assert!(
        !Path::new(&marker).exists(),
        "the child must not be spawned"
    );
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn setup_signal_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("setup-supervisor") {
        return;
    }
    let signal = std::env::var(SIGNAL).unwrap().parse::<i32>().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::setup_signal_child",
            "--nocapture",
        ])
        .env(MODE, "setup-child")
        .env(SIGNAL, signal.to_string());
    // SAFETY: the hook uses only async-signal-safe calls after fork. It sends
    // the signal before `spawn` returns and before the parent registers the pid.
    unsafe {
        command.pre_exec(move || {
            // SAFETY: `getppid` reads the direct parent's process id.
            let parent = libc::getppid();
            // SAFETY: `kill` sends a valid termination signal to the parent.
            if libc::kill(parent, signal) != 0 {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
    let reaped = run_child(command, IoMode::Capture, None).expect("run setup signal child");
    assert_eq!(reaped.classify(), Outcome::Interrupted { signal });
}

#[test]
fn setup_signal_child() {
    if std::env::var(MODE).as_deref() != Ok("setup-child") {
        return;
    }
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn signals_sent_during_child_setup_are_forwarded() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "nix::process::tests::setup_signal_supervisor",
                "--nocapture",
            ])
            .env(MODE, "setup-supervisor")
            .env(SIGNAL, signal.to_string())
            .status()
            .expect("run setup signal supervisor");
        assert!(status.success(), "setup signal {signal} failed");
    }
}

#[test]
fn grace_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("grace-supervisor") {
        return;
    }
    let signal = std::env::var(SIGNAL).unwrap().parse::<i32>().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::ignoring_signal_child",
            "--nocapture",
        ])
        .env(MODE, "ignoring-child")
        .env(READY, std::env::var(READY).unwrap());
    let reaped = run_child(command, IoMode::Capture, None).expect("run signal-ignoring child");
    assert_eq!(reaped.classify(), Outcome::Interrupted { signal });
}

#[test]
fn ignoring_signal_child() {
    if std::env::var(MODE).as_deref() != Ok("ignoring-child") {
        return;
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_IGN;
        // SAFETY: `action` is initialized and the signal is valid.
        unsafe {
            libc::sigemptyset(&raw mut action.sa_mask);
            assert_eq!(
                libc::sigaction(signal, &raw const action, std::ptr::null_mut()),
                0
            );
        }
    }
    let ready = std::env::var(READY).unwrap();
    fs::write(&ready, format!("{} 0 0 0", std::process::id()))
        .expect("publish signal-ignoring child pid");
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn cancellation_grace_kills_and_reaps_a_signal_ignoring_child() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ready = temp.path().join("ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::grace_supervisor",
            "--nocapture",
        ])
        .env(MODE, "grace-supervisor")
        .env(SIGNAL, libc::SIGTERM.to_string())
        .env(READY, &ready)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut supervisor =
        GroupChildGuard::spawn(command).expect("spawn cancellation grace supervisor");
    let text = wait_ready(&ready);
    let child_pid = text.split_whitespace().next().unwrap().parse().unwrap();
    let started = Instant::now();
    supervisor
        .signal(libc::SIGTERM)
        .expect("signal the owned grace supervisor");
    let status = supervisor
        .wait_for_exit(Duration::from_secs(4))
        .expect("observe grace supervisor")
        .expect("grace supervisor must stop within four seconds");
    assert!(status.success());
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert!(started.elapsed() < Duration::from_secs(4));
    wait_for_process_exit(child_pid);
}

#[test]
fn exit_signal_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("exit-supervisor") {
        return;
    }
    let signal = std::env::var(SIGNAL).unwrap().parse::<i32>().unwrap();
    let ready = std::env::var(READY).unwrap();
    // The supervisor is the direct child under test. Keep a separate process
    // alive after it exits so its inherited pipe tests the post-exit drain.
    let owner = unsafe { libc::getppid() };
    // SAFETY: the child branch below uses only async-signal-safe libc calls
    // and `_exit`; the parent owns and records the child's pid.
    let sender = unsafe { libc::fork() };
    assert!(sender >= 0, "fork exit signal sender");
    if sender == 0 {
        let request = libc::timespec {
            tv_sec: 0,
            tv_nsec: 150_000_000,
        };
        let mut remaining = request;
        // SAFETY: nanosleep uses initialized stack storage and is async-signal-safe.
        if unsafe { libc::nanosleep(&raw const remaining, &raw mut remaining) } != 0 {
            // SAFETY: _exit is async-signal-safe and does not run parent destructors.
            unsafe { libc::_exit(2) };
        }
        // SAFETY: owner is the live test process that launched this supervisor.
        let result = unsafe { libc::kill(owner, signal) };
        // SAFETY: _exit is async-signal-safe and does not run parent destructors.
        unsafe { libc::_exit(i32::from(result != 0)) };
    }
    fs::write(&ready, sender.to_string()).expect("record exit signal sender pid");
}

#[test]
fn exit_signal_test_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("exit-signal-test-supervisor") {
        return;
    }
    let ready_dir = std::env::var(READY).unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let ready = Path::new(&ready_dir).join(format!("sender-{signal}"));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "nix::process::tests::exit_signal_supervisor",
                "--nocapture",
            ])
            .env(MODE, "exit-supervisor")
            .env(SIGNAL, signal.to_string())
            .env(READY, &ready);
        let started = Instant::now();
        let reaped = run_child(command, IoMode::Capture, None).expect("run exit signal child");
        assert_eq!(reaped.classify(), Outcome::Interrupted { signal });
        assert!(started.elapsed() < Duration::from_secs(1));
        let sender_pid = wait_for_marker(&ready).parse::<i32>().unwrap();
        wait_for_process_exit(sender_pid);
    }
}

#[test]
fn signals_after_child_exit_are_classified_without_reaping_pipe_holders() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::exit_signal_test_supervisor",
            "--nocapture",
        ])
        .env(MODE, "exit-signal-test-supervisor")
        .env(READY, temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut supervisor =
        GroupChildGuard::spawn(command).expect("spawn post-exit signal supervisor");
    let status = supervisor
        .wait_for_exit(Duration::from_secs(5))
        .expect("observe post-exit signal supervisor")
        .expect("post-exit signal supervisor must finish within five seconds");
    assert!(
        status.success(),
        "post-exit signal supervisor failed: {status}"
    );
}

#[test]
fn bounded_group_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("bounded-group-supervisor") {
        return;
    }
    let ready = std::env::var(READY).unwrap();
    // The forked process inherits the child's dedicated process group. It
    // stays alive after this test process exits.
    // SAFETY: the child branch loops in the async-signal-safe `pause` call;
    // the parent records its pid and then returns to the test harness.
    let descendant = unsafe { libc::fork() };
    assert!(descendant >= 0, "fork bounded descendant");
    if descendant == 0 {
        loop {
            // SAFETY: pause blocks this process until a signal arrives.
            unsafe { libc::pause() };
        }
    }
    fs::write(&ready, descendant.to_string()).expect("publish bounded descendant pid");
}

#[test]
fn bounded_children_stop_group_descendants_and_preserve_unrelated_sentinels() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ready = temp.path().join("descendant");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::bounded_group_supervisor",
            "--nocapture",
        ])
        .env(MODE, "bounded-group-supervisor")
        .env(READY, &ready);
    let mut sentinels = SentinelChildren(vec![
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn first sentinel"),
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn second sentinel"),
    ]);
    for _ in 0..32 {
        let reaped = super::run_child_bounded(command_for_true(), 512 * 1024 * 1024)
            .expect("run bounded short child");
        assert_eq!(reaped.classify(), Outcome::Success);
        for sentinel in &mut sentinels.0 {
            assert!(sentinel.try_wait().unwrap().is_none());
        }
    }
    let reaped =
        super::run_child_bounded(command, 512 * 1024 * 1024).expect("run bounded group child");
    assert_eq!(reaped.classify(), Outcome::Success);
    wait_for_process_exit(wait_for_marker(&ready).parse().unwrap());
}

#[test]
fn bounded_child_with_no_group_descendants_succeeds_after_exit() {
    let reaped = super::run_child_bounded(command_for_true(), 512 * 1024 * 1024)
        .expect("run bounded child with no descendants");
    assert_eq!(reaped.classify(), Outcome::Success);
}

#[cfg(target_os = "macos")]
#[test]
fn force_stop_observes_exit_after_darwin_zombie_group_eperm() {
    let mut command = command_for_true();
    command.process_group(0);
    let child = command.spawn().expect("spawn owned process-group child");
    let mut owner = super::ChildOwner::new(child, true).expect("own process-group child");
    wait_for_unobserved_exit(owner.pid);

    // SAFETY: the process-group leader is this owned, unreaped child.
    assert_eq!(unsafe { libc::kill(-owner.pid, libc::SIGKILL) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );

    owner
        .force_stop()
        .expect("zombie-only group cleanup is safe");
    assert!(owner.state == super::ChildState::Exited);
    owner.reap().expect("reap observed child");
    assert!(owner.state == super::ChildState::Reaped);
}

#[cfg(target_os = "macos")]
fn wait_for_unobserved_exit(pid: libc::pid_t) {
    let id = libc::id_t::try_from(pid).expect("child id fits id_t");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: waitid uses an initialized output buffer and does not consume status.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `id` names the owned child and WNOWAIT retains its wait identity.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                std::ptr::from_mut(&mut info),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == 0
        {
            // SAFETY: successful waitid initialized the process id field.
            if unsafe { info.si_pid() } != 0 {
                return;
            }
        } else {
            let error = std::io::Error::last_os_error();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "{error}");
        }
        assert!(Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn output_limit_child() {
    if std::env::var(MODE).as_deref() != Ok("output-limit-child") {
        return;
    }
    let bytes = vec![b'x'; 17 * 1024 * 1024];
    std::io::stdout().write_all(&bytes).unwrap();
}

#[test]
fn bounded_output_stops_at_the_existing_budget() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::output_limit_child",
            "--nocapture",
        ])
        .env(MODE, "output-limit-child");
    let reaped = run_child_with_limits(
        command,
        IoMode::Capture,
        None,
        Some(ChildLimits {
            resident_bytes: u64::MAX,
            wall_time: Duration::from_secs(5),
        }),
    )
    .expect("run output-limited child");
    assert_eq!(
        reaped.resource_limit,
        Some("metadata output exceeded 16 MiB")
    );
    assert!(reaped.output.stdout.len() <= 16 * 1024 * 1024);
}

#[test]
fn continuous_output_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("continuous-output-supervisor") {
        return;
    }
    let ready = std::env::var(READY).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::continuous_output_child",
            "--nocapture",
        ])
        .env(MODE, "continuous-output-child")
        .env(READY, &ready);
    let started = Instant::now();
    let reaped = run_child_with_limits(
        command,
        IoMode::Capture,
        None,
        Some(ChildLimits {
            resident_bytes: u64::MAX,
            wall_time: Duration::from_secs(5),
        }),
    )
    .expect("stop continuous output child");
    assert_eq!(
        reaped.resource_limit,
        Some("metadata output exceeded 16 MiB")
    );
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn continuous_output_child() {
    if std::env::var(MODE).as_deref() != Ok("continuous-output-child") {
        return;
    }
    fs::write(
        std::env::var(READY).unwrap(),
        std::process::id().to_string(),
    )
    .expect("publish continuous output child pid");
    let mut stdout = std::io::stdout().lock();
    let bytes = [b'x'; 8192];
    while stdout.write_all(&bytes).is_ok() {}
}

#[test]
fn continuous_output_limit_stops_a_live_producer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ready = temp.path().join("continuous-output-child");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::continuous_output_supervisor",
            "--nocapture",
        ])
        .env(MODE, "continuous-output-supervisor")
        .env(READY, &ready)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut supervisor =
        GroupChildGuard::spawn(command).expect("spawn continuous output supervisor");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if ready.exists() {
            break;
        }
        if let Some(status) = supervisor
            .observe_and_reap()
            .expect("poll output supervisor")
        {
            panic!("output supervisor exited before the child started: {status}");
        }
        if Instant::now() >= deadline {
            supervisor
                .stop_group_and_wait()
                .expect("stop and reap output supervisor");
            if let Some(pid) = fs::read_to_string(&ready)
                .ok()
                .and_then(|text| text.parse().ok())
            {
                wait_for_process_exit(pid);
            }
            panic!("continuous output child was not started in time");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let child_pid: i32 = wait_for_marker(&ready).parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = supervisor
            .observe_and_reap()
            .expect("poll output supervisor")
        {
            wait_for_process_exit(child_pid);
            assert!(status.success(), "continuous output limit failed");
            break;
        }
        if Instant::now() >= deadline {
            supervisor
                .stop_group_and_wait()
                .expect("stop and reap output supervisor");
            wait_for_process_exit(child_pid);
            panic!("continuous output child was not stopped by the limit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn group_guard_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("group-guard-supervisor") {
        return;
    }
    let ready = std::env::var(READY).unwrap();
    // SAFETY: the child branch only calls async-signal-safe pause.
    let descendant = unsafe { libc::fork() };
    assert!(descendant >= 0, "fork group-guard descendant");
    if descendant == 0 {
        loop {
            // SAFETY: pause blocks until the parent group guard sends SIGKILL.
            unsafe { libc::pause() };
        }
    }
    fs::write(&ready, descendant.to_string()).expect("publish group-guard descendant");
    loop {
        // SAFETY: pause blocks until the parent group guard sends SIGKILL.
        unsafe { libc::pause() };
    }
}

#[test]
fn group_guard_stops_and_reaps_an_early_supervisor_group() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ready = temp.path().join("descendant");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::group_guard_supervisor",
            "--nocapture",
        ])
        .env(MODE, "group-guard-supervisor")
        .env(READY, &ready);
    let supervisor = GroupChildGuard::spawn(command).expect("spawn group-guard supervisor");
    let supervisor_pid = supervisor.pid;
    let descendant_pid = wait_for_marker(&ready).parse().unwrap();
    drop(supervisor);
    wait_for_process_exit(descendant_pid);
    wait_for_process_exit(supervisor_pid);
}

#[test]
fn long_running_child() {
    if std::env::var(MODE).as_deref() != Ok("long-running-child") {
        return;
    }
    if let Ok(ready) = std::env::var(READY) {
        fs::write(ready, std::process::id().to_string()).expect("publish running child pid");
    }
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn bounded_timeout_uses_the_private_wall_time_limit() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::long_running_child",
            "--nocapture",
        ])
        .env(MODE, "long-running-child");
    let temp = tempfile::tempdir().expect("tempdir");
    let ready = temp.path().join("running-child");
    command.env(READY, &ready);
    let run = std::thread::spawn(move || {
        run_child_with_limits(
            command,
            IoMode::Capture,
            None,
            Some(ChildLimits {
                resident_bytes: u64::MAX,
                wall_time: Duration::from_millis(60),
            }),
        )
        .expect("run time-limited child")
    });
    let _child_pid = wait_for_marker(&ready).parse::<i32>().unwrap();
    let started = Instant::now();
    let reaped = run.join().expect("join time-limited child owner");
    assert_eq!(
        reaped.resource_limit,
        Some("metadata batch exceeded 120 seconds")
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn bounded_rss_limit_stops_a_running_child() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::long_running_child",
            "--nocapture",
        ])
        .env(MODE, "long-running-child");
    let reaped = run_child_with_limits(
        command,
        IoMode::Capture,
        None,
        Some(ChildLimits {
            resident_bytes: 1,
            wall_time: Duration::from_secs(5),
        }),
    )
    .expect("run RSS-limited child");
    assert_eq!(
        reaped.resource_limit,
        Some("evaluator approached 512 MiB RSS")
    );
}

#[test]
fn automatic_sigchld_reaping_is_rejected_before_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("spawned");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nix::process::tests::auto_reap_supervisor",
            "--nocapture",
        ])
        .env(MODE, "auto-reap-supervisor")
        .env(MARKER, &marker)
        .status()
        .expect("run isolated SIGCHLD supervisor");
    assert!(status.success(), "isolated SIGCHLD check failed");
    assert!(!marker.exists(), "the child must not be spawned");
}

#[test]
fn direct_commands_preserve_arguments_without_shell_interpretation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("shell-was-run");
    let argument = format!("$(touch {}) ; * ? ' \" `echo injected`", marker.display());
    let args = vec![
        String::from("--exact"),
        String::from("nix::process::tests::argv_child"),
        String::from("--nocapture"),
        String::from("--skip"),
        argument.clone(),
    ];
    let (outcome, stdout) = super::run_direct_captured(&std::env::current_exe().unwrap(), &args)
        .expect("run argument helper");
    assert_eq!(outcome, Outcome::Success);
    assert!(stdout.contains(&format!(
        "PKG_ARGV={}",
        serde_json::to_string(&argument).unwrap()
    )));
    assert!(!marker.exists(), "shell expansion must not run");
}

#[test]
fn spawn_failure_restores_the_callers_signal_mask() {
    std::thread::spawn(|| {
        // SAFETY: zeroed sigset_t buffers are initialized below before use.
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: both buffers are initialized and SIGINT is a valid signal.
        unsafe {
            libc::sigemptyset(&raw mut blocked);
            libc::sigaddset(&raw mut blocked, libc::SIGINT);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous),
                0
            );
        }
        let temp = tempfile::tempdir().unwrap();
        let error = run_child(
            Command::new(temp.path().join("missing")),
            IoMode::Capture,
            None,
        )
        .err()
        .expect("missing executable must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        // SAFETY: pthread_sigmask initializes this buffer before it is read.
        let mut current: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: pthread_sigmask reads this thread's mask into a valid buffer.
        unsafe {
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &raw mut current),
                0
            );
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                let expected = if signal == libc::SIGINT {
                    1
                } else {
                    libc::sigismember(&raw const previous, signal)
                };
                assert_eq!(libc::sigismember(&raw const current, signal), expected);
            }
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut()),
                0
            );
        }
    })
    .join()
    .expect("mask test thread");
}

#[test]
fn signal_supervisor() {
    if std::env::var(MODE).as_deref() != Ok("supervisor") {
        return;
    }
    let signal: i32 = std::env::var(SIGNAL).unwrap().parse().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "nix::process::tests::signal_child",
            "--nocapture",
        ])
        .env(MODE, "child");
    let reaped = run_child(command, IoMode::Capture, None).expect("run signal child");
    assert_eq!(reaped.classify(), Outcome::Interrupted { signal });
}

#[test]
fn signal_child() {
    if std::env::var(MODE).as_deref() != Ok("child") {
        return;
    }
    // SAFETY: sigprocmask writes the current mask to this initialized buffer.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    let status = unsafe {
        libc::sigprocmask(
            libc::SIG_SETMASK,
            std::ptr::null(),
            std::ptr::from_mut(&mut mask),
        )
    };
    assert_eq!(status, 0);
    let blocked: Vec<_> = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .into_iter()
        // SAFETY: mask is initialized and each signal is a valid constant.
        .map(|signal| unsafe { libc::sigismember(std::ptr::from_ref(&mask), signal) })
        .collect();
    fs::write(
        std::env::var(READY).unwrap(),
        format!(
            "{} {} {} {}",
            std::process::id(),
            blocked[0],
            blocked[1],
            blocked[2]
        ),
    )
    .expect("publish child readiness");
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_ready(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && text.split_whitespace().count() == 4
        {
            return text;
        }
        assert!(Instant::now() < deadline, "signal child did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_marker(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "child did not publish its marker"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_process_exit(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: signal zero checks whether this recorded process still exists.
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        assert!(Instant::now() < deadline, "process {pid} did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct GroupChildGuard {
    child: Child,
    pid: libc::pid_t,
    state: GroupChildState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupChildState {
    Owned,
    Reaping,
    Reaped,
    Lost,
}

impl GroupChildGuard {
    fn spawn(mut command: Command) -> std::io::Result<Self> {
        command.process_group(0);
        let mut child = command.spawn()?;
        let Ok(pid) = libc::pid_t::try_from(child.id()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "supervisor process id does not fit pid_t",
            ));
        };
        Ok(Self {
            child,
            pid,
            state: GroupChildState::Owned,
        })
    }

    fn signal(&self, signal: libc::c_int) -> std::io::Result<()> {
        if self.state != GroupChildState::Owned {
            return Err(std::io::Error::other(
                "cannot signal a consumed supervisor identity",
            ));
        }
        self.signal_process(signal)
    }

    fn signal_group(&self, signal: libc::c_int) -> std::io::Result<()> {
        if self.state != GroupChildState::Owned {
            return Err(std::io::Error::other(
                "cannot signal a consumed supervisor group",
            ));
        }
        loop {
            // SAFETY: the dedicated-group leader remains an unreaped child.
            if unsafe { libc::kill(-self.pid, signal) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn signal_process(&self, signal: libc::c_int) -> std::io::Result<()> {
        if self.state != GroupChildState::Owned {
            return Err(std::io::Error::other(
                "cannot signal a consumed supervisor identity",
            ));
        }
        loop {
            // SAFETY: wait ownership for the direct supervisor is still held.
            if unsafe { libc::kill(self.pid, signal) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn wait_for_exit(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.observe_and_reap()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn observe_and_reap(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if self.state != GroupChildState::Owned {
            return Err(std::io::Error::other(
                "supervisor wait ownership is no longer active",
            ));
        }
        let id = libc::id_t::try_from(self.pid).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "supervisor id does not fit id_t",
            )
        })?;
        loop {
            // SAFETY: waitid uses initialized storage and WNOWAIT preserves status.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `id` names this owned supervisor and the flags do not reap it.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    id,
                    std::ptr::from_mut(&mut info),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } == 0
            {
                // SAFETY: successful waitid initialized the process id field.
                if unsafe { info.si_pid() } == 0 {
                    return Ok(None);
                }
                return self.stop_group_and_wait().map(Some);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.state = GroupChildState::Lost;
            }
            return Err(error);
        }
    }

    fn stop_group_and_wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let mut group_error = None;
        if self.state == GroupChildState::Owned {
            if let Err(error) = self.signal_group(libc::SIGKILL) {
                let no_live_members = {
                    #[cfg(target_os = "macos")]
                    {
                        error.raw_os_error() == Some(libc::EPERM)
                            && matches!(super::macos_group_has_live_members(self.pid), Ok(false))
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        false
                    }
                };
                if error.raw_os_error() != Some(libc::ESRCH) && !no_live_members {
                    group_error = Some(error);
                    let _ = self.signal_process(libc::SIGKILL);
                }
            }
            self.state = GroupChildState::Reaping;
        }
        let status = self.wait_owned()?;
        if let Some(error) = group_error {
            return Err(error);
        }
        Ok(status)
    }

    fn wait_owned(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if self.state != GroupChildState::Reaping {
            return Err(std::io::Error::other("supervisor is not being reaped"));
        }
        match self.child.wait() {
            Ok(status) => {
                self.state = GroupChildState::Reaped;
                Ok(status)
            }
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.state = GroupChildState::Lost;
                }
                Err(error)
            }
        }
    }
}

impl Drop for GroupChildGuard {
    fn drop(&mut self) {
        if self.state == GroupChildState::Owned {
            if let Err(error) = self.signal_group(libc::SIGKILL)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                let _ = self.signal_process(libc::SIGKILL);
            }
            self.state = GroupChildState::Reaping;
        }
        while self.state == GroupChildState::Reaping {
            match self.wait_owned() {
                Ok(_) => {}
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

fn command_for_true() -> Command {
    Command::new("/usr/bin/true")
}

struct SentinelChildren(Vec<Child>);

impl Drop for SentinelChildren {
    fn drop(&mut self) {
        for child in &mut self.0 {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(error) = child.kill() {
                        eprintln!("failed to stop sentinel: {error}");
                    }
                }
                Err(error) => eprintln!("failed to poll sentinel: {error}"),
            }
            if let Err(error) = child.wait() {
                eprintln!("failed to reap sentinel: {error}");
            }
        }
    }
}

#[test]
fn direct_children_receive_each_forwarded_termination_signal() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let temp = tempfile::tempdir().expect("tempdir");
        let ready = temp.path().join("ready");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "nix::process::tests::signal_supervisor",
                "--nocapture",
            ])
            .env(MODE, "supervisor")
            .env(READY, &ready)
            .env(SIGNAL, signal.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut supervisor = GroupChildGuard::spawn(command).expect("spawn supervisor");
        let text = wait_ready(&ready);
        let pid: i32 = text.split_whitespace().next().unwrap().parse().unwrap();
        supervisor
            .signal(signal)
            .expect("signal the owned supervisor");
        let status = supervisor
            .wait_for_exit(Duration::from_secs(3))
            .expect("observe supervisor")
            .expect("signal supervisor must stop within three seconds");
        assert!(status.success(), "signal {signal}: {text}");
        assert_eq!(
            text.split_whitespace().skip(1).collect::<Vec<_>>(),
            ["0", "0", "0"]
        );
        // SAFETY: a completed supervisor has reaped this helper before returning.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child must be reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
