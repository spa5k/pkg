//! End-to-end CLI contract tests over the public command surface.

use std::process::Command;

fn pkg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pkg"))
}

#[test]
fn help_guides_common_tasks_and_short_flags_match_long_flags() {
    let short = pkg().arg("-h").output().unwrap();
    let short = String::from_utf8(short.stdout).unwrap();
    assert!(short.contains("pkg upgrade --all"));
    assert!(!short.contains("--state"));
    let install = pkg().args(["install", "--help"]).output().unwrap();
    let install = String::from_utf8(install.stdout).unwrap();
    assert!(install.contains("github:casey/just#default"));
    assert!(install.contains("--state"));
    let short_flags = pkg_cli::cli::Cli::try_parse(["pkg", "-vy", "install", "just"]).unwrap();
    let long_flags =
        pkg_cli::cli::Cli::try_parse(["pkg", "--verbose", "--yes", "install", "just"]).unwrap();
    assert_eq!(short_flags, long_flags);
}

#[test]
fn clap_usage_failures_exit_two() {
    for args in [
        &["--json", "--jsonl", "doctor"][..],
        &["install"][..],
        &["install", "x", "--on-collision", "keep-all"][..],
    ] {
        let output = pkg().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "args={args:?}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn configuration_failure_obeys_json_and_jsonl_terminal_contracts() {
    for (flag, expected_type) in [("--json", None), ("--jsonl", Some("result"))] {
        let output = pkg()
            .args([flag, "--state", "relative", "install", "ripgrep"])
            .env_remove("PKG_STATE_DIR")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["command"], "install");
        assert_eq!(value["error"]["symbol"], "CONFIG");
        assert_eq!(
            value.get("type").and_then(|value| value.as_str()),
            expected_type
        );
    }
}

#[test]
fn history_uses_the_system_home_boundary_despite_spoofed_or_absent_home() {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
        .unwrap()
        .unwrap();
    let temporary = tempfile::tempdir_in(user.dir).unwrap();
    let spoofed = tempfile::tempdir().unwrap();
    for (label, home) in [("spoofed", Some(spoofed.path())), ("absent", None)] {
        let state = temporary.path().join(label);
        let mut command = pkg();
        command
            .args(["--json", "--state", state.to_str().unwrap(), "history"])
            .env_remove("PKG_STATE_DIR");
        if let Some(home) = home {
            command.env("HOME", home);
        } else {
            command.env_remove("HOME");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{label}: {output:?}");
        assert!(output.stderr.is_empty());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["command"], "history");
        assert_eq!(result["ok"], true);
        assert_eq!(result["entries"], serde_json::json!([]));
        assert!(state.join("logs/pkg.log").is_file());
        assert_eq!(std::fs::read_dir(spoofed.path()).unwrap().count(), 0);
    }
}

#[test]
fn completion_is_real_static_source_and_doctor_reports_verified_host_state() {
    let completion = pkg().args(["completion", "bash"]).output().unwrap();
    assert!(completion.status.success());
    assert!(
        String::from_utf8(completion.stdout)
            .unwrap()
            .contains("_pkg")
    );

    let state = std::env::temp_dir().join(format!("pkg-cli-doctor-{}", std::process::id()));
    let expected_bin = state.join("current/bin");
    let doctor = pkg()
        .args(["--json", "--state", state.to_str().unwrap(), "doctor"])
        .env("PATH", &expected_bin)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    // Both required production rows must exist exactly once before any status
    // or exit decision is read; a missing report row is a report defect.
    let checks = value["checks"].as_array().unwrap();
    let required_row = |id: &str| {
        let matches: Vec<&serde_json::Value> =
            checks.iter().filter(|check| check["id"] == id).collect();
        assert_eq!(matches.len(), 1, "{id} must appear exactly once");
        matches[0]
    };
    let managed = required_row("runtime.managed");
    let channel = required_row("channel.signed");
    // The developer machine can have a working authenticated broker. An
    // alternate user-state directory does not isolate the system service.
    if doctor.status.success() {
        assert_eq!(value["overall"], "healthy");
        for check in [managed, channel] {
            assert_eq!(check["status"], "pass");
        }
        return;
    }
    // A clean CI host reaches the failed production broker checks (78). A host
    // with an installed managed-Nix spike but no authenticated production
    // receipt must fail earlier at the PR-9 ownership gate (74). Both are
    // honest, fail-closed outcomes; this integration test must not pretend the
    // machine's global /nix state is part of its temporary --state directory.
    assert!(matches!(doctor.status.code(), Some(74 | 78)));
    assert!(matches!(
        value["overall"].as_str(),
        Some("needs_attention" | "nix_ownership_unknown")
    ));
    for check in [managed, channel] {
        assert_eq!(check["status"], "fail");
    }
}

#[test]
fn doctor_support_is_preview_only_and_available_on_an_unhealthy_host() {
    let state = std::env::temp_dir().join(format!(
        "pkg-cli-support-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let output = pkg()
        .args(["--state", state.to_str().unwrap(), "doctor", "--support"])
        .env("PATH", state.join("current/bin"))
        .env("PKG_TEST_SECRET", "must-not-leak")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["type"], "support_bundle");
    assert_eq!(value["privacy"]["previewOnly"], true);
    assert_eq!(value["privacy"]["uploaded"], false);
    assert_eq!(value["privacy"]["packageNamesIncluded"], false);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("must-not-leak"));
    assert!(!text.contains(state.to_str().unwrap()));
}

#[test]
fn doctor_support_refuses_competing_machine_output_modes() {
    for args in [
        ["--json", "doctor", "--support"],
        ["doctor", "--support", "--json"],
        ["--jsonl", "doctor", "--support"],
        ["doctor", "--support", "--jsonl"],
    ] {
        let output = pkg().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["symbol"], "USAGE");
        assert_eq!(value["command"], "doctor");
    }
}

#[test]
fn command_logging_records_only_the_command_not_package_arguments() {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
        .unwrap()
        .unwrap();
    let temporary = tempfile::tempdir_in(user.dir).unwrap();
    let state = temporary.path().join("state");
    let output = pkg()
        .args([
            "install",
            "super-secret-package-name",
            "--state",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    // A caller-selected state root is read-only for broker-backed mutations.
    // The command must fail at that CONFIG boundary before broker access.
    assert_eq!(output.status.code(), Some(78));
    let text = std::fs::read_to_string(state.join("logs/pkg.log")).unwrap();
    assert!(text.contains("command_finished"));
    assert!(text.contains("install"));
    assert!(!text.contains("super-secret-package-name"));
    assert!(!text.contains("argv"));
    assert!(!text.contains("environment"));
}

#[test]
fn semantic_usage_failure_uses_the_selected_machine_format() {
    let output = pkg()
        .args(["--json", "upgrade"])
        .env_remove("PKG_UPGRADE_DEFAULT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["symbol"], "USAGE");
}

#[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn live_structured_uninstall_refuses_before_privilege_or_mutation() {
    for flag in ["--json", "--jsonl"] {
        let output = pkg()
            .args([flag, "system", "uninstall", "--yes"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["error"]["message"],
            "live uninstall requires plain output"
        );
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn uninstall_refuses_an_unsupported_host_before_privilege() {
    let output = pkg()
        .args(["--json", "--dry-run", "system", "uninstall"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["error"]["message"],
        "uninstall is not available on this system"
    );
}

#[test]
fn shellenv_is_usable_before_state_exists_and_exports_the_package_path() {
    let output = pkg().arg("shellenv").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let snippet = String::from_utf8(output.stdout).unwrap();
    let script = format!("{snippet}\n{snippet}\nprintf '%s' \"$PATH\"");
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("HOME", "/tmp/pkg shellenv test")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = String::from_utf8(output.stdout).unwrap();
    assert_eq!(path.matches("/current/bin").count(), 1);
    assert!(path.ends_with(":/usr/bin:/bin"));
}

#[test]
fn every_command_has_examples_and_remains_discoverable() {
    use clap::CommandFactory;
    let grammar = pkg_cli::cli::Cli::command();
    let home = pkg().env("COLUMNS", "80").output().unwrap();
    assert!(home.status.success());
    let home = String::from_utf8(home.stdout).unwrap();
    assert!(
        !home.contains("--state"),
        "the home guide keeps advanced options in full help"
    );
    let help = pkg().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for verb in [
        "doctor",
        "install",
        "upgrade",
        "rollback",
        "repair",
        "uninstall",
    ] {
        assert!(help.contains(verb), "missing {verb} from --help");
    }
    for command in grammar.get_subcommands() {
        let name = command.get_name();
        assert!(
            home.contains(&format!("  {name} ")),
            "missing {name} from home"
        );
        for width in [40, 80, 120] {
            let output = pkg()
                .args([name, "--help", "--no-color"])
                .env("COLUMNS", width.to_string())
                .output()
                .unwrap();
            assert!(output.status.success(), "{name}: {output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("Examples"), "missing examples for {name}");
            assert!(
                text.contains(&format!("pkg {name}")),
                "missing usage for {name}"
            );
            assert!(!text.contains('\x1b'));
        }
    }
    let typo = pkg().args(["instal", "ripgrep"]).output().unwrap();
    assert!(!typo.status.success());
    assert!(String::from_utf8(typo.stderr).unwrap().contains("install"));
}

#[test]
fn package_uninstall_is_a_remove_alias_and_never_reaches_product_uninstall() {
    use pkg_cli::cli::Cli;
    for flags in [vec![], vec!["--yes"], vec!["--dry-run"], vec!["--json"]] {
        let mut remove = vec!["pkg", "remove", "just"];
        remove.extend(flags.iter().copied());
        let mut uninstall = vec!["pkg", "uninstall", "just"];
        uninstall.extend(flags);
        assert_eq!(
            Cli::try_parse(remove).unwrap(),
            Cli::try_parse(uninstall).unwrap()
        );
    }
    for args in [
        vec!["uninstall"],
        vec!["system"],
        vec!["system", "uninstall", "just"],
    ] {
        let output = pkg().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let output = pkg()
        .args(["system", "uninstall", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("sudo pkg system uninstall"));
    assert!(help.contains("pkg uninstall <package>"));
}
