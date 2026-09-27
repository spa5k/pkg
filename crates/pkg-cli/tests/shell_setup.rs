//! Shell process contracts run separately from the library's state-lease tests.
//!
//! A spawned child can retain another thread's file locks until exec.
//! Keep shell launches in this test binary so they cannot inherit those leases.

use std::path::Path;

use pkg_cli::path::{HostFamily, production_state_root, shell_init};

#[test]
fn repeated_shell_setup_keeps_one_manpath_entry_and_preserves_system_defaults() {
    let home = std::env::var("HOME").unwrap();
    for host in [HostFamily::Linux, HostFamily::MacOs] {
        let man = production_state_root(host, Path::new(&home))
            .join("current/share/man")
            .to_str()
            .unwrap()
            .to_owned();
        for shell in ["/bin/bash", "/bin/zsh"] {
            // Linux runners do not all include zsh; native macOS covers both.
            if !Path::new(shell).exists() {
                assert_eq!(shell, "/bin/zsh");
                continue;
            }
            for initial in [
                None,
                Some(""),
                Some("/custom/man"),
                Some(":/custom/man:"),
                Some(man.as_str()),
            ] {
                let mut command = std::process::Command::new(shell);
                if shell.ends_with("bash") {
                    command.args(["--noprofile", "--norc"]);
                } else {
                    command.arg("-f");
                }
                command
                    .env_remove("BASH_ENV")
                    .env_remove("ENV")
                    .env_remove("MANPATH");
                if let Some(initial) = initial {
                    command.env("MANPATH", initial);
                }
                let snippet = shell_init(host);
                let output = command
                    .args([
                        "-c",
                        &format!("{snippet}{snippet}{snippet}printf '%s' \"$MANPATH\""),
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{shell}: {:?}", output.stderr);
                let actual = String::from_utf8(output.stdout).unwrap();
                let expected = if initial == Some(man.as_str()) {
                    man.clone()
                } else {
                    format!("{man}:{}", initial.unwrap_or_default())
                };
                assert_eq!(actual, expected, "{shell} {host:?} {initial:?}");
                assert_eq!(actual.split(':').filter(|entry| *entry == man).count(), 1);
            }
        }
    }
}
