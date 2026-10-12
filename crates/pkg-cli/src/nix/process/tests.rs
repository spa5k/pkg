use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::{IoMode, Outcome, run_child};

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

#[test]
fn direct_children_receive_each_forwarded_termination_signal() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let temp = tempfile::tempdir().expect("tempdir");
        let ready = temp.path().join("ready");
        let mut supervisor = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "nix::process::tests::signal_supervisor",
                "--nocapture",
            ])
            .env(MODE, "supervisor")
            .env(READY, &ready)
            .env(SIGNAL, signal.to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn supervisor");
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
                // SAFETY: the blocked helper is still owned by the live supervisor.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                supervisor.kill().expect("kill stuck supervisor");
                supervisor.wait().expect("reap stuck supervisor");
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
