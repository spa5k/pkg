use std::fs;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::{ChildLimits, IoMode, Outcome, run_child, run_child_with_limits};

const MODE: &str = "PKG_PROCESS_SIGNAL_TEST";
const READY: &str = "PKG_PROCESS_SIGNAL_READY";
const SIGNAL: &str = "PKG_PROCESS_SIGNAL_NUMBER";
const MARKER: &str = "PKG_PROCESS_SPAWN_MARKER";

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
    let mut supervisor = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nix::process::tests::grace_supervisor",
            "--nocapture",
        ])
        .env(MODE, "grace-supervisor")
        .env(SIGNAL, libc::SIGTERM.to_string())
        .env(READY, &ready)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn cancellation grace supervisor");
    let text = wait_ready(&ready);
    let child_pid = text.split_whitespace().next().unwrap().parse().unwrap();
    let started = Instant::now();
    // SAFETY: the supervisor is a live child that this test has not reaped.
    assert_eq!(
        unsafe { libc::kill(supervisor.id().try_into().unwrap(), libc::SIGTERM) },
        0
    );
    let status = supervisor.wait().expect("wait for grace supervisor");
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
fn signals_after_child_exit_are_classified_without_reaping_pipe_holders() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let temp = tempfile::tempdir().expect("tempdir");
        let ready = temp.path().join("sender");
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
    let mut supervisor = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nix::process::tests::continuous_output_supervisor",
            "--nocapture",
        ])
        .env(MODE, "continuous-output-supervisor")
        .env(READY, &ready)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn continuous output supervisor");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if ready.exists() {
            break;
        }
        if let Some(status) = supervisor.try_wait().expect("poll output supervisor") {
            panic!("output supervisor exited before the child started: {status}");
        }
        if Instant::now() >= deadline {
            supervisor.kill().expect("stop stuck output supervisor");
            supervisor.wait().expect("reap output supervisor");
            if let Ok(text) = fs::read_to_string(&ready)
                && let Ok(pid) = text.parse()
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
        if let Some(status) = supervisor.try_wait().expect("poll output supervisor") {
            assert!(status.success(), "continuous output limit failed");
            break;
        }
        if Instant::now() >= deadline {
            supervisor.kill().expect("stop stuck output supervisor");
            supervisor.wait().expect("reap output supervisor");
            wait_for_process_exit(child_pid);
            panic!("continuous output child was not stopped by the limit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_for_process_exit(child_pid);
}

#[test]
fn long_running_child() {
    if std::env::var(MODE).as_deref() != Ok("long-running-child") {
        return;
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
    let started = Instant::now();
    let reaped = run_child_with_limits(
        command,
        IoMode::Capture,
        None,
        Some(ChildLimits {
            resident_bytes: u64::MAX,
            wall_time: Duration::from_millis(60),
        }),
    )
    .expect("run time-limited child");
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
        command.process_group(0);
        let mut supervisor = command.spawn().expect("spawn supervisor");
        let text = wait_ready(&ready);
        let pid: i32 = text.split_whitespace().next().unwrap().parse().unwrap();
        let supervisor_pid = i32::try_from(supervisor.id()).unwrap();
        // SAFETY: supervisor is a live child that this test has not reaped.
        assert_eq!(unsafe { libc::kill(supervisor_pid, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = supervisor.try_wait().expect("poll supervisor") {
                break Some(status);
            }
            if Instant::now() >= deadline {
                // SAFETY: supervisor leads a dedicated group, and remains an
                // unreaped child while the group signal is sent.
                assert_eq!(unsafe { libc::kill(-supervisor_pid, libc::SIGKILL) }, 0);
                supervisor.wait().expect("reap stuck supervisor");
                wait_for_process_exit(pid);
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.is_some_and(|status| status.success()),
            "signal {signal}: {text}"
        );
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
