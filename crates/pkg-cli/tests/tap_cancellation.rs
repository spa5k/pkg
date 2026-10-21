#![cfg(unix)]
#![expect(missing_docs, reason = "integration tests have no public API")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail on broken fixtures"
)]

use std::os::fd::FromRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn tap_import_keeps_the_signal_and_does_not_publish() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("gate-pid");
        let nix = dir.path().join("nix");
        std::fs::write(&nix, format!("#!/bin/sh\ncase \"$*\" in\n *--version*) echo 'nix (Nix) 2.35.2';;\n *'config show system'*) echo '{}';;\n *'config show --json'*) echo $$ > '{}'; sleep 300;;\n *) exit 9;;\nesac\n", if cfg!(target_os = "macos") { "aarch64-darwin" } else { "x86_64-linux" }, marker.display())).unwrap();
        std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_pkg"))
            .args(["tap", "add", "example/cancel", "--trust"])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "tap import did not reach the config gate");
        let gate_pid: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(i32::try_from(child.id()).unwrap(), signal) },
            0
        );
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            unsafe { libc::kill(gate_pid, 0) },
            -1,
            "config child survived cancellation"
        );
        assert!(!dir.path().join("state/pkg/taps/registry.json").exists());
        assert_eq!(
            output.status.code(),
            Some(128 + signal),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn doctor_keeps_probe_cancellation() {
    check_probe_cancellation(false);
}

#[cfg(target_os = "linux")]
#[test]
fn sandbox_setup_keeps_probe_cancellation() {
    check_probe_cancellation(true);
}

fn check_probe_cancellation(setup: bool) {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("probe-pid");
        let nix = dir.path().join("nix");
        std::fs::write(
                &nix,
                format!(
                    "#!/bin/sh\ncase \"$*\" in\n *--version*) echo 'nix (Nix) 2.35.2';;\n *'config show system'*) echo '{}';;\n *'config show --json'*) if [ \"$HOME\" = '{}' ]; then echo $$ > '{}'; sleep 300; else echo '{{}}'; fi;;\n *'store info'*) echo reachable;;\n *) exit 9;;\nesac\n",
                    if cfg!(target_os = "macos") { "aarch64-darwin" } else { "x86_64-linux" },
                    dir.path().display(),
                    marker.display(),
                ),
            ).unwrap();
        std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _terminal = unsafe { std::fs::File::from_raw_fd(master) };
        let input = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        if setup {
            command.args(["tap", "add", "example/cancel", "--trust"]);
        } else {
            command.arg("doctor");
        }
        let mut child = command
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !marker.exists() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "setup={setup}: config probe did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let pid: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(i32::try_from(child.id()).unwrap(), signal) },
            0
        );
        let output = child.wait_with_output().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "config probe survived");
        assert_eq!(
            output.status.code(),
            Some(128 + signal),
            "setup={setup}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!dir.path().join("state/pkg/taps/registry.json").exists());
        if setup {
            assert!(String::from_utf8_lossy(&output.stderr).contains("one-time Nix sandbox setup"));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("apply now?"));
        }
    }
}
