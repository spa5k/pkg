//! Typed command dispatch across the private engine boundary.

use std::fmt;
use std::io::{self, Write};

use serde_json::{Map, Value, json};

use crate::cli::{
    Cli, Command, GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs,
    RepairArgs, RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use crate::exit::ExitCode;
use crate::ux::{CommandError, OutputMode, PUBLIC_SCHEMA_VERSION, write_error, write_json_line};

const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PUBLIC_DEPTH: usize = 16;

/// One already-parsed product command presented to the private engine connector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    command: Command,
    yes: bool,
    dry_run: bool,
}

impl CommandRequest {
    /// Copies the validated command payload and operation-wide mutation policy.
    #[must_use]
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            command: cli.parsed_command().clone(),
            yes: cli.yes(),
            dry_run: cli.dry_run(),
        }
    }

    /// Parsed command and arguments.
    #[must_use]
    pub const fn command(&self) -> &Command {
        &self.command
    }

    /// Whether ordinary confirmation was pre-approved for this operation.
    #[must_use]
    pub const fn yes(&self) -> bool {
        self.yes
    }

    /// Whether mutation must stop after planning.
    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }
}

/// Sanitized successful command result.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandResult {
    summary: String,
    fields: Map<String, Value>,
    records: Vec<Map<String, Value>>,
}

impl CommandResult {
    /// Validates one bounded terminal result and optional JSONL row/progress records.
    pub fn new(
        summary: impl Into<String>,
        fields: Map<String, Value>,
        records: Vec<Map<String, Value>>,
    ) -> Result<Self, PublicResultError> {
        let summary = summary.into();
        validate_public_string(&summary)?;
        validate_map(&fields, 0)?;
        for record in &records {
            validate_record_map(record)?;
        }
        let encoded = serde_json::to_vec(&(summary.as_str(), &fields, &records))
            .map_err(|_| PublicResultError::InvalidValue)?;
        if encoded.len() > MAX_RESULT_BYTES {
            return Err(PublicResultError::TooLarge);
        }
        Ok(Self {
            summary,
            fields,
            records,
        })
    }

    /// Human-readable final summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Product-owned terminal fields.
    #[must_use]
    pub const fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// Product-owned nonterminal JSONL records.
    #[must_use]
    pub fn records(&self) -> &[Map<String, Value>] {
        &self.records
    }
}

/// A connector that executes typed product commands without exposing Nix controls.
pub trait CommandEngine {
    /// Execute one command or return a sanitized stable failure.
    fn execute(&mut self, request: &CommandRequest) -> Result<CommandResult, CommandError>;
}

/// Typed product operations implemented by the private broker connector.
///
/// The boundary deliberately carries parsed product arguments and sanitized
/// results only. Implementations call `pkg-core`, `pkg-index`, `pkg-pipeline`,
/// and `pkg-store`; no raw Nix command or configuration knob crosses it.
pub trait CoreOperations {
    /// Search the locally authenticated disposable index.
    fn search(&mut self, args: &SearchArgs) -> Result<CommandResult, CommandError>;
    /// Inspect package metadata without realization unless explicitly exact.
    fn info(&mut self, args: &InfoArgs) -> Result<CommandResult, CommandError>;
    /// Resolve, acquire, verify, stage, activate, and commit packages.
    fn install(
        &mut self,
        args: &InstallArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Remove selectors through a fresh generation.
    fn remove(
        &mut self,
        args: &RemoveArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Read installed selectors from the active generation.
    fn list(&mut self, args: &ListArgs) -> Result<CommandResult, CommandError>;
    /// Compare installed versions with accepted metadata.
    fn outdated(&mut self) -> Result<CommandResult, CommandError>;
    /// Refresh authenticated metadata without changing packages.
    fn update(
        &mut self,
        args: &UpdateArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Upgrade the selected lifecycle state through a fresh generation.
    fn upgrade(
        &mut self,
        args: &UpgradeArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Change exact pin intent through a byte-equivalent fresh activation.
    fn pin(
        &mut self,
        args: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Clear exact pin intent through a byte-equivalent fresh activation.
    fn unpin(
        &mut self,
        args: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Read, diff, or explicitly prune retained generations.
    fn history(
        &mut self,
        args: &HistoryArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Re-materialize a retained state as a fresh monotonic generation.
    fn rollback(
        &mut self,
        args: &RollbackArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
    /// Plan or perform retention-aware generation and store collection.
    fn gc(&mut self, args: &GcArgs, policy: OperationPolicy)
    -> Result<CommandResult, CommandError>;
    /// Verify or repair one generation through the privileged two-phase flow.
    fn repair(
        &mut self,
        args: &RepairArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError>;
}

/// Global confirmation and mutation policy attached to one parsed command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationPolicy {
    yes: bool,
    dry_run: bool,
}

impl OperationPolicy {
    /// Whether ordinary confirmation is pre-approved.
    #[must_use]
    pub const fn yes(self) -> bool {
        self.yes
    }
    /// Whether the implementation must stop before mutation.
    #[must_use]
    pub const fn dry_run(self) -> bool {
        self.dry_run
    }
}

/// Dispatch adapter over one concrete implementation of all internal operations.
#[derive(Debug)]
pub struct CoreEngine<C> {
    operations: C,
}

impl<C> CoreEngine<C> {
    /// Wraps a product-operation implementation for CLI dispatch.
    #[must_use]
    pub const fn new(operations: C) -> Self {
        Self { operations }
    }
    /// Returns shared access to the wrapped implementation, primarily for assertions.
    #[must_use]
    pub const fn operations(&self) -> &C {
        &self.operations
    }
    /// Consumes the engine and returns the wrapped implementation.
    #[must_use]
    pub fn into_operations(self) -> C {
        self.operations
    }
}

impl<C: CoreOperations> CommandEngine for CoreEngine<C> {
    fn execute(&mut self, request: &CommandRequest) -> Result<CommandResult, CommandError> {
        let policy = OperationPolicy {
            yes: request.yes(),
            dry_run: request.dry_run(),
        };
        match request.command() {
            Command::Search(args) => self.operations.search(args),
            Command::Info(args) => self.operations.info(args),
            Command::Install(args) => self.operations.install(args, policy),
            Command::Remove(args) => self.operations.remove(args, policy),
            Command::List(args) => self.operations.list(args),
            Command::Outdated => self.operations.outdated(),
            Command::Update(args) => self.operations.update(args, policy),
            Command::Upgrade(args) => self.operations.upgrade(args, policy),
            Command::Pin(args) => self.operations.pin(args, policy),
            Command::Unpin(args) => self.operations.unpin(args, policy),
            Command::History(args) => self.operations.history(args, policy),
            Command::Rollback(args) => self.operations.rollback(args, policy),
            Command::Gc(args) => self.operations.gc(args, policy),
            Command::Repair(args) => self.operations.repair(args, policy),
            Command::Doctor | Command::Completion(_) => Err(CommandError::new(
                ExitCode::Config,
                "bootstrap command reached the private engine",
                "report this product integration error",
            )),
        }
    }
}

/// Fail-closed connector used until the private broker transport lands.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableEngine;

impl CommandEngine for UnavailableEngine {
    fn execute(&mut self, _request: &CommandRequest) -> Result<CommandResult, CommandError> {
        Err(CommandError::new(
            ExitCode::EngineUnavailable,
            "the private package engine is not available",
            "run `pkg doctor` to inspect managed runtime readiness",
        ))
    }
}

/// Stable refusal while constructing a public success result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicResultError {
    /// A public key or string was empty, controlled, reserved, or runtime-private.
    PrivateValue,
    /// Nested output exceeded the supported shape depth.
    TooDeep,
    /// Serialized public output exceeded the bounded response size.
    TooLarge,
    /// A non-finite or unsupported JSON value was supplied.
    InvalidValue,
}

impl fmt::Display for PublicResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "public command result refused: {self:?}")
    }
}
impl std::error::Error for PublicResultError {}

/// Executes one non-bootstrap command and renders its stable public result.
pub fn execute_command(
    cli: &Cli,
    engine: &mut dyn CommandEngine,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<ExitCode> {
    let mode = OutputMode::from_flags(cli.json(), cli.jsonl());
    match engine.execute(&CommandRequest::from_cli(cli)) {
        Ok(result) => {
            write_success(&mut stdout, mode, cli.command_name(), &result)?;
            Ok(ExitCode::Ok)
        }
        Err(error) => {
            write_error(&mut stdout, &mut stderr, mode, cli.command_name(), &error)?;
            Ok(error.exit_code())
        }
    }
}

fn write_success(
    mut writer: impl Write,
    mode: OutputMode,
    command: &str,
    result: &CommandResult,
) -> io::Result<()> {
    match mode {
        OutputMode::Human => writeln!(writer, "{}", result.summary()),
        OutputMode::Json => {
            let mut value = result.fields().clone();
            value.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
            value.insert("ok".into(), json!(true));
            value.insert("command".into(), json!(command));
            write_json_line(writer, &value)
        }
        OutputMode::JsonLines => {
            for record in result.records() {
                let mut value = record.clone();
                value.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
                write_json_line(&mut writer, &value)?;
            }
            let mut value = result.fields().clone();
            value.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
            value.insert("type".into(), json!("result"));
            value.insert("ok".into(), json!(true));
            value.insert("command".into(), json!(command));
            write_json_line(writer, &value)
        }
    }
}

fn validate_map(map: &Map<String, Value>, depth: usize) -> Result<(), PublicResultError> {
    if depth > MAX_PUBLIC_DEPTH {
        return Err(PublicResultError::TooDeep);
    }
    for (key, value) in map {
        validate_key(key)?;
        validate_value(value, depth + 1)?;
    }
    Ok(())
}

fn validate_record_map(map: &Map<String, Value>) -> Result<(), PublicResultError> {
    if map.get("type").and_then(Value::as_str).is_none() {
        return Err(PublicResultError::InvalidValue);
    }
    for (key, value) in map {
        if key != "type" {
            validate_key(key)?;
        }
        validate_value(value, 1)?;
    }
    Ok(())
}

fn validate_value(value: &Value, depth: usize) -> Result<(), PublicResultError> {
    if depth > MAX_PUBLIC_DEPTH {
        return Err(PublicResultError::TooDeep);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => validate_public_string(value),
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_value(value, depth + 1)),
        Value::Object(map) => validate_map(map, depth + 1),
    }
}

fn validate_key(key: &str) -> Result<(), PublicResultError> {
    const RESERVED: [&str; 5] = ["schemaVersion", "ok", "command", "error", "type"];
    const PRIVATE: [&str; 10] = [
        "storepath",
        "drvpath",
        "derivation",
        "attribute",
        "flakeref",
        "nixargs",
        "nixargv",
        "substituter",
        "trustedkey",
        "trustedpublickey",
    ];
    let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
    if key.is_empty()
        || key.chars().any(char::is_control)
        || RESERVED.contains(&key)
        || PRIVATE.iter().any(|private| normalized.contains(private))
    {
        return Err(PublicResultError::PrivateValue);
    }
    Ok(())
}

fn validate_public_string(value: &str) -> Result<(), PublicResultError> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.contains("/nix/")
        || value.contains(".drv")
        || value.contains("github:")
        || value.contains("flake:")
        || [
            "x86_64-linux",
            "aarch64-linux",
            "x86_64-darwin",
            "aarch64-darwin",
        ]
        .iter()
        .any(|private| value.contains(private))
    {
        return Err(PublicResultError::PrivateValue);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        fn gc(
            &mut self,
            _: &GcArgs,
            policy: OperationPolicy,
        ) -> Result<CommandResult, CommandError> {
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
}
