//! Tests for the `execute` module.

use super::*;
use crate::cli::Cli;

struct EchoEngine;
impl CommandEngine for EchoEngine {
    fn execute(&mut self, request: &CommandRequest) -> Result<CommandResult, CommandError> {
        CommandResult::new(
            format!("{} completed", request.command().name()),
            Map::from_iter([("dryRun".into(), json!(request.dry_run()))]),
            vec![Map::from_iter([("type".into(), json!("phase"))])],
        )
        .map_err(|_| CommandError::new(ExitCode::Config, "invalid result", "report a bug"))
    }
}

struct ProgressEngine;
impl CommandEngine for ProgressEngine {
    fn execute(&mut self, request: &CommandRequest) -> Result<CommandResult, CommandError> {
        self.execute_with_progress(request, &mut |_| Ok(()))
    }

    fn execute_with_progress(
        &mut self,
        _request: &CommandRequest,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        for event in [
            PublicEvent::phase("op_fixture", "acquire", "started").unwrap(),
            PublicEvent::phase("op_fixture", "acquire", "completed").unwrap(),
            PublicEvent::phase("op_fixture", "stage", "started").unwrap(),
            PublicEvent::phase("op_fixture", "stage", "completed").unwrap(),
            PublicEvent::phase("op_fixture", "activate", "started").unwrap(),
            PublicEvent::phase("op_fixture", "activate", "completed").unwrap(),
            PublicEvent::committed("op_fixture", "gen-0001").unwrap(),
        ] {
            progress(event)?;
        }
        CommandResult::new(
            "Installed 1 package(s) as gen-0001.",
            Map::from_iter([
                ("opId".into(), json!("op_fixture")),
                (
                    "generation".into(),
                    json!({ "id": "gen-0001", "parent": null }),
                ),
            ]),
            vec![],
        )
        .map_err(|_| CommandError::new(ExitCode::Config, "invalid result", "report a bug"))
    }
}

#[derive(Default)]
struct RecordingOperations {
    calls: Vec<(&'static str, Option<OperationPolicy>)>,
}

impl RecordingOperations {
    fn called(
        &mut self,
        name: &'static str,
        policy: Option<OperationPolicy>,
    ) -> Result<CommandResult, CommandError> {
        self.calls.push((name, policy));
        CommandResult::new("completed", Map::new(), vec![])
            .map_err(|_| CommandError::new(ExitCode::Config, "invalid result", "report a bug"))
    }
}

impl CoreOperations for RecordingOperations {
    fn search(&mut self, _: &SearchArgs) -> Result<CommandResult, CommandError> {
        self.called("search", None)
    }
    fn info(&mut self, _: &InfoArgs) -> Result<CommandResult, CommandError> {
        self.called("info", None)
    }
    fn install(
        &mut self,
        _: &InstallArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("install", Some(policy))
    }
    fn remove(
        &mut self,
        _: &RemoveArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("remove", Some(policy))
    }
    fn list(&mut self, _: &ListArgs) -> Result<CommandResult, CommandError> {
        self.called("list", None)
    }
    fn outdated(&mut self) -> Result<CommandResult, CommandError> {
        self.called("outdated", None)
    }
    fn update(
        &mut self,
        _: &UpdateArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("update", Some(policy))
    }
    fn upgrade(
        &mut self,
        _: &UpgradeArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("upgrade", Some(policy))
    }
    fn pin(
        &mut self,
        _: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("pin", Some(policy))
    }
    fn unpin(
        &mut self,
        _: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("unpin", Some(policy))
    }
    fn history(
        &mut self,
        _: &HistoryArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("history", Some(policy))
    }
    fn rollback(
        &mut self,
        _: &RollbackArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("rollback", Some(policy))
    }
    fn gc(&mut self, _: &GcArgs, policy: OperationPolicy) -> Result<CommandResult, CommandError> {
        self.called("gc", Some(policy))
    }
    fn repair(
        &mut self,
        _: &RepairArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.called("repair", Some(policy))
    }
}

#[test]
fn dispatches_every_engine_command_without_reparsing() {
    for args in [
        vec!["pkg", "search", "ripgrep"],
        vec!["pkg", "info", "ripgrep"],
        vec!["pkg", "install", "ripgrep"],
        vec!["pkg", "remove", "ripgrep"],
        vec!["pkg", "list"],
        vec!["pkg", "outdated"],
        vec!["pkg", "update"],
        vec!["pkg", "upgrade", "ripgrep"],
        vec!["pkg", "pin", "ripgrep"],
        vec!["pkg", "unpin", "ripgrep"],
        vec!["pkg", "history"],
        vec!["pkg", "rollback"],
        vec!["pkg", "gc"],
        vec!["pkg", "repair"],
    ] {
        let cli = Cli::try_parse(args).unwrap();
        let mut stdout = Vec::new();
        assert_eq!(
            execute_command(&cli, &mut EchoEngine, &mut stdout, Vec::new()).unwrap(),
            ExitCode::Ok
        );
        assert!(!stdout.is_empty());
    }
}

#[test]
fn public_result_rejects_private_runtime_material_and_reserved_fields() {
    for fields in [
        Map::from_iter([("storePath".into(), json!("hidden"))]),
        Map::from_iter([("resolved_store_path".into(), json!("hidden"))]),
        Map::from_iter([("path".into(), json!("/nix/store/abc-secret"))]),
        Map::from_iter([("system".into(), json!("aarch64-darwin"))]),
        Map::from_iter([("detail".into(), json!("source github:NixOS/nixpkgs/rev"))]),
        Map::from_iter([("ok".into(), json!(true))]),
    ] {
        assert_eq!(
            CommandResult::new("done", fields, vec![]),
            Err(PublicResultError::PrivateValue)
        );
    }
}

#[test]
fn public_result_requires_null_for_absent_optional_values() {
    assert_eq!(
        CommandResult::new(
            "done",
            Map::from_iter([("description".into(), json!(""))]),
            vec![],
        ),
        Err(PublicResultError::PrivateValue)
    );
    assert!(
        CommandResult::new(
            "done",
            Map::from_iter([("description".into(), Value::Null)]),
            vec![],
        )
        .is_ok()
    );
    assert_eq!(
        CommandResult::new("", Map::new(), vec![]),
        Err(PublicResultError::PrivateValue)
    );
}

#[test]
fn human_search_lists_packages_and_catalog_time() {
    let result = CommandResult::new(
        "2 package(s) found",
        Map::from_iter([
            ("catalogGeneratedAt".into(), json!("2026-08-19T00:00:00Z")),
            ("stale".into(), json!(false)),
        ]),
        vec![
            Map::from_iter([
                ("type".into(), json!("package")),
                ("package".into(), json!("python3Packages.requests")),
                ("version".into(), json!("2.32.4")),
                ("available".into(), json!(true)),
                ("broken".into(), json!(false)),
                ("description".into(), json!("Python HTTP library")),
            ]),
            Map::from_iter([
                ("type".into(), json!("package")),
                ("package".into(), json!("pythonPackages.requests")),
                ("version".into(), Value::Null),
                ("available".into(), json!(false)),
                ("broken".into(), json!(false)),
                ("description".into(), Value::Null),
            ]),
        ],
    )
    .unwrap();
    let mut output = Vec::new();
    write_success(&mut output, OutputMode::Human, "search", &result).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("PACKAGE                   VERSION  STATUS"));
    assert!(output.contains("python3Packages.requests  2.32.4   ready"));
    assert!(output.contains("pythonPackages.requests   -        unsupported"));
    assert!(output.contains("Catalog updated: 2026-08-19T00:00:00Z"));
}

#[test]
fn jsonl_records_are_versioned_and_end_in_one_terminal_result() {
    let cli = Cli::try_parse(["pkg", "list", "--jsonl", "--dry-run"]).unwrap();
    let mut stdout = Vec::new();
    execute_command(&cli, &mut EchoEngine, &mut stdout, Vec::new()).unwrap();
    let rows = String::from_utf8(stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["schemaVersion"] == 1));
    assert_eq!(rows.last().unwrap()["type"], "result");
    assert_eq!(rows.last().unwrap()["ok"], true);
}

#[test]
fn install_progress_stream_and_human_lines_match_v1_goldens() {
    let jsonl = Cli::try_parse(["pkg", "install", "hello", "--yes", "--jsonl"]).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    execute_command(&jsonl, &mut ProgressEngine, &mut stdout, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        include_str!("../../../../../fixtures/cli-v1/install-progress.jsonl")
    );

    let human = Cli::try_parse(["pkg", "install", "hello", "--yes"]).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    execute_command(&human, &mut ProgressEngine, &mut stdout, &mut stderr).unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "Installed 1 package(s) as gen-0001.\n"
    );
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        include_str!("../../../../../fixtures/cli-v1/install-progress.txt")
    );

    let quiet = Cli::try_parse(["pkg", "install", "hello", "--yes", "--quiet"]).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    execute_command(&quiet, &mut ProgressEngine, &mut stdout, &mut stderr).unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "Installed 1 package(s) as gen-0001.\n"
    );
    assert!(stderr.is_empty());

    let json = Cli::try_parse(["pkg", "install", "hello", "--yes", "--json"]).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    execute_command(&json, &mut ProgressEngine, &mut stdout, &mut stderr).unwrap();
    assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert!(stderr.is_empty());
}

#[test]
fn operation_log_is_byte_identical_in_every_output_mode() {
    for flags in [&[][..], &["--quiet"][..], &["--json"][..], &["--jsonl"][..]] {
        let root = tempfile::tempdir().unwrap();
        let mut args = vec!["pkg", "install", "hello", "--yes"];
        args.extend_from_slice(flags);
        let cli = Cli::try_parse(args).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            execute_command_with_operation_log(
                &cli,
                &mut ProgressEngine,
                &root.path().join("logs"),
                &mut stdout,
                &mut stderr,
            )
            .unwrap(),
            ExitCode::Ok
        );
        let journal = std::fs::read(root.path().join("logs/op_fixture.ndjson")).unwrap();
        assert_eq!(
            journal,
            include_bytes!("../../../../../fixtures/cli-v1/install-progress.jsonl")
        );
        if flags == ["--jsonl"] {
            assert_eq!(stdout, journal);
        }
    }
}

struct FailingProgressEngine;

impl CommandEngine for FailingProgressEngine {
    fn execute(&mut self, _: &CommandRequest) -> Result<CommandResult, CommandError> {
        unreachable!()
    }

    fn execute_with_progress(
        &mut self,
        _: &CommandRequest,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        progress(PublicEvent::phase("op_failure", "build", "started").unwrap())?;
        Err(CommandError::new(
            ExitCode::BuildFailed,
            "package build failed",
            "inspect the sanitized operation log",
        ))
    }
}

#[test]
fn failed_jsonl_stream_and_operation_log_share_the_exact_terminal_record() {
    let root = tempfile::tempdir().unwrap();
    let cli = Cli::try_parse(["pkg", "install", "hello", "--yes", "--jsonl"]).unwrap();
    let mut stdout = Vec::new();
    assert_eq!(
        execute_command_with_operation_log(
            &cli,
            &mut FailingProgressEngine,
            &root.path().join("logs"),
            &mut stdout,
            Vec::new(),
        )
        .unwrap(),
        ExitCode::BuildFailed
    );
    let journal = std::fs::read(root.path().join("logs/op_failure.ndjson")).unwrap();
    assert_eq!(stdout, journal);
    let terminal: Value = serde_json::from_slice(
        journal
            .split(|byte| *byte == b'\n')
            .rfind(|row| !row.is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(terminal["type"], "result");
    assert_eq!(terminal["opId"], "op_failure");
    assert_eq!(terminal["error"]["symbol"], "BUILD_FAILED");
}

struct GuardedMutationEngine {
    mutated: bool,
}

impl CommandEngine for GuardedMutationEngine {
    fn execute(&mut self, _: &CommandRequest) -> Result<CommandResult, CommandError> {
        unreachable!()
    }

    fn execute_with_progress(
        &mut self,
        _: &CommandRequest,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        progress(PublicEvent::phase("op_guard", "acquire", "started").unwrap())?;
        self.mutated = true;
        CommandResult::new("completed", Map::new(), vec![])
            .map_err(|_| CommandError::new(ExitCode::Config, "invalid", "report a bug"))
    }
}

#[test]
#[cfg(unix)]
fn unsafe_operation_log_stops_the_engine_before_mutation() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("target");
    std::fs::write(&target, b"unchanged").unwrap();
    symlink(&target, logs.join("op_guard.ndjson")).unwrap();
    let cli = Cli::try_parse(["pkg", "install", "hello", "--yes", "--jsonl"]).unwrap();
    let mut engine = GuardedMutationEngine { mutated: false };
    let mut stdout = Vec::new();
    assert_eq!(
        execute_command_with_operation_log(&cli, &mut engine, &logs, &mut stdout, Vec::new(),)
            .unwrap(),
        ExitCode::Config
    );
    assert!(!engine.mutated);
    assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    let terminal: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(terminal["error"]["symbol"], "CONFIG");
    assert!(terminal.get("opId").is_none());
}

struct TerminalJournalFailureEngine {
    journal_path: std::path::PathBuf,
}

impl CommandEngine for TerminalJournalFailureEngine {
    fn execute(&mut self, _: &CommandRequest) -> Result<CommandResult, CommandError> {
        unreachable!()
    }

    fn execute_with_progress(
        &mut self,
        _: &CommandRequest,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        progress(PublicEvent::phase("op_committed", "activate", "started").unwrap())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.journal_path, std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        CommandResult::new(
            "Installed 1 package.",
            Map::from_iter([("opId".into(), json!("op_committed"))]),
            vec![],
        )
        .map_err(|_| CommandError::new(ExitCode::Config, "invalid", "report a bug"))
    }
}

#[test]
#[cfg(unix)]
fn terminal_journal_fault_does_not_report_a_committed_success_as_failure() {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    let cli = Cli::try_parse(["pkg", "install", "hello", "--yes", "--jsonl"]).unwrap();
    let mut engine = TerminalJournalFailureEngine {
        journal_path: logs.join("op_committed.ndjson"),
    };
    let mut stdout = Vec::new();
    assert_eq!(
        execute_command_with_operation_log(&cli, &mut engine, &logs, &mut stdout, Vec::new(),)
            .unwrap(),
        ExitCode::Ok
    );
    let rows = stdout
        .split(|byte| *byte == b'\n')
        .filter(|row| !row.is_empty())
        .map(|row| serde_json::from_slice::<Value>(row).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.last().unwrap()["ok"], true);
    assert_eq!(rows.last().unwrap()["opId"], "op_committed");
    let journal = std::fs::read(engine.journal_path).unwrap();
    assert_eq!(journal.iter().filter(|byte| **byte == b'\n').count(), 1);
}

#[test]
#[cfg(unix)]
fn unsafe_log_directory_returns_a_stable_public_error_before_engine_entry() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli = Cli::try_parse(["pkg", "install", "hello", "--yes", "--jsonl"]).unwrap();
    let mut engine = GuardedMutationEngine { mutated: false };
    let mut stdout = Vec::new();
    assert_eq!(
        execute_command_with_operation_log(&cli, &mut engine, &logs, &mut stdout, Vec::new(),)
            .unwrap(),
        ExitCode::Config
    );
    assert!(!engine.mutated);
    let terminal: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(terminal["type"], "result");
    assert_eq!(terminal["error"]["symbol"], "CONFIG");
}

#[test]
fn core_engine_routes_every_variant_and_preserves_global_policy() {
    let cases = [
        (vec!["pkg", "search", "ripgrep"], "search", false),
        (vec!["pkg", "info", "ripgrep"], "info", false),
        (
            vec!["pkg", "install", "ripgrep", "--yes", "--dry-run"],
            "install",
            true,
        ),
        (vec!["pkg", "remove", "ripgrep"], "remove", true),
        (vec!["pkg", "list"], "list", false),
        (vec!["pkg", "outdated"], "outdated", false),
        (vec!["pkg", "update"], "update", true),
        (vec!["pkg", "upgrade", "ripgrep"], "upgrade", true),
        (vec!["pkg", "pin", "ripgrep"], "pin", true),
        (vec!["pkg", "unpin", "ripgrep"], "unpin", true),
        (vec!["pkg", "history"], "history", true),
        (vec!["pkg", "rollback"], "rollback", true),
        (vec!["pkg", "gc"], "gc", true),
        (vec!["pkg", "repair"], "repair", true),
    ];
    let mut engine = CoreEngine::new(RecordingOperations::default());
    for (argv, expected, has_policy) in cases {
        let cli = Cli::try_parse(argv).unwrap();
        engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
        let (called, policy) = engine.operations().calls.last().unwrap();
        assert_eq!(*called, expected);
        assert_eq!(policy.is_some(), has_policy);
        if expected == "install" {
            assert_eq!(
                *policy,
                Some(OperationPolicy {
                    yes: true,
                    dry_run: true
                })
            );
        }
    }
}
