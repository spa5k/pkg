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
    for (flag, expected_type) in [("--json", None), ("--jsonl", Some("result"))] {
        let output = pkg().args([flag, "doctor"]).output().unwrap();
        assert_eq!(output.status.code(), Some(79));
        assert!(output.stderr.is_empty());
        assert_eq!(
            output.stdout.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["command"], "doctor");
        assert_eq!(value["error"]["symbol"], "ENGINE_UNAVAILABLE");
        assert_eq!(
            value.get("type").and_then(|value| value.as_str()),
            expected_type
        );
    }
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
