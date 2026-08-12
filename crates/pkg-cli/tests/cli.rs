use std::process::Command;

fn pkg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pkg"))
}

#[test]
fn help_exits_success_and_lists_the_product_commands() {
    let output = pkg().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for verb in ["doctor", "install", "upgrade", "rollback", "repair"] {
        assert!(stdout.contains(verb));
    }
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
fn development_stub_obeys_json_and_jsonl_terminal_contracts() {
    let home = std::env::temp_dir().join(format!(
        "pkg-cli-engine-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    for (flag, expected_type) in [("--json", None), ("--jsonl", Some("result"))] {
        let output = pkg()
            .args([flag, "install", "ripgrep"])
            .env("HOME", &home)
            .env_remove("XDG_DATA_HOME")
            .env_remove("PKG_STATE_DIR")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(79));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["command"], "install");
        assert_eq!(value["error"]["symbol"], "ENGINE_UNAVAILABLE");
        assert_eq!(
            value.get("type").and_then(|value| value.as_str()),
            expected_type
        );
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn completion_is_real_static_source_and_doctor_fails_closed_without_the_broker() {
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
    // A clean CI host reaches the failed production broker checks (78). A host
    // with an installed managed-Nix spike but no authenticated production
    // receipt must fail earlier at the PR-9 ownership gate (74). Both are
    // honest, fail-closed outcomes; this integration test must not pretend the
    // machine's global /nix state is part of its temporary --state directory.
    assert!(matches!(doctor.status.code(), Some(74 | 78)));
    let value: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert!(matches!(
        value["overall"].as_str(),
        Some("needs_attention" | "nix_ownership_unknown")
    ));
    assert!(
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|check| {
                matches!(
                    check["id"].as_str(),
                    Some("runtime.managed" | "channel.signed")
                )
            })
            .all(|check| check["status"] == "fail")
    );
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
    let state = std::env::temp_dir().join(format!(
        "pkg-cli-log-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
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
    std::fs::remove_dir_all(state).unwrap();
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
