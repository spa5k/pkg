//! Typed command dispatch across the private engine boundary.

use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::cli::{
    Cli, Command, GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs,
    RepairArgs, RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use crate::exit::ExitCode;
use crate::log::PublicOperationLog;
use crate::progress::PublicEvent;
use crate::ux::{
    CommandError, OutputMode, PUBLIC_SCHEMA_VERSION, json_line_bytes, terminal_error_ndjson_line,
    write_error_with_operation, write_json_line,
};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        if summary.is_empty() {
            return Err(PublicResultError::PrivateValue);
        }
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

    /// Execute one command while reporting sanitized progress events.
    fn execute_with_progress(
        &mut self,
        request: &CommandRequest,
        _progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.execute(request)
    }
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
    /// Install while reporting sanitized phase progress.
    fn install_with_progress(
        &mut self,
        args: &InstallArgs,
        policy: OperationPolicy,
        _progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.install(args, policy)
    }
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
    #[cfg(test)]
    pub(crate) const fn for_test(yes: bool, dry_run: bool) -> Self {
        Self { yes, dry_run }
    }

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
        self.execute_with_progress(request, &mut |_| Ok(()))
    }

    fn execute_with_progress(
        &mut self,
        request: &CommandRequest,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        let policy = OperationPolicy {
            yes: request.yes(),
            dry_run: request.dry_run(),
        };
        match request.command() {
            Command::Search(args) => self.operations.search(args),
            Command::Info(args) => self.operations.info(args),
            Command::Install(args) => self
                .operations
                .install_with_progress(args, policy, progress),
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
            Command::Doctor(_) | Command::Uninstall | Command::Completion(_) => {
                Err(CommandError::new(
                    ExitCode::Config,
                    "bootstrap command reached the private engine",
                    "report this product integration error",
                ))
            }
        }
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
    stdout: impl Write,
    stderr: impl Write,
) -> io::Result<ExitCode> {
    execute_command_inner(cli, engine, None, stdout, stderr)
}

/// Execute one command while durably mirroring its sanitized public operation stream.
pub fn execute_command_with_operation_log(
    cli: &Cli,
    engine: &mut dyn CommandEngine,
    log_directory: &Path,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<ExitCode> {
    let log = match PublicOperationLog::open(log_directory) {
        Ok(log) => log,
        Err(error) => {
            let error = public_stream_unavailable(error);
            write_error_with_operation(
                &mut stdout,
                &mut stderr,
                OutputMode::from_flags(cli.json(), cli.jsonl()),
                cli.command_name(),
                &error,
                None,
            )?;
            return Ok(error.exit_code());
        }
    };
    execute_command_inner(cli, engine, Some(log), stdout, stderr)
}

fn execute_command_inner(
    cli: &Cli,
    engine: &mut dyn CommandEngine,
    operation_log: Option<PublicOperationLog>,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<ExitCode> {
    let mode = OutputMode::from_flags(cli.json(), cli.jsonl());
    let mut journal = operation_log.map(PublicOperationJournal::new);
    let result = {
        let mut progress = |event: PublicEvent| -> Result<(), CommandError> {
            let bytes = event.to_ndjson_line().map_err(public_stream_unavailable)?;
            if let Some(journal) = journal.as_mut() {
                journal
                    .append_event(event.op_id(), &bytes)
                    .map_err(public_stream_unavailable)?;
            }
            match mode {
                OutputMode::Human if !cli.quiet() => event.write_human(&mut stderr),
                OutputMode::JsonLines => stdout.write_all(&bytes),
                OutputMode::Human | OutputMode::Json => Ok(()),
            }
            .map_err(public_stream_unavailable)
        };
        engine.execute_with_progress(&CommandRequest::from_cli(cli), &mut progress)
    };
    let operation_id = journal
        .as_ref()
        .and_then(PublicOperationJournal::operation_id)
        .map(str::to_owned);
    match result {
        Ok(result) => {
            let lines = success_jsonl_lines(cli.command_name(), &result, operation_id.as_deref())?;
            if let Some(journal) = journal.as_mut() {
                let _ = journal.append_records(&lines);
            }
            write_success_lines(
                &mut stdout,
                mode,
                cli.command_name(),
                &result,
                operation_id.as_deref(),
                &lines,
            )?;
            Ok(ExitCode::Ok)
        }
        Err(error) => {
            let terminal =
                terminal_error_ndjson_line(cli.command_name(), &error, operation_id.as_deref())?;
            if let Some(journal) = journal.as_mut() {
                let _ = journal.append_record(&terminal);
            }
            if mode == OutputMode::JsonLines {
                stdout.write_all(&terminal)?;
            } else {
                write_error_with_operation(
                    &mut stdout,
                    &mut stderr,
                    mode,
                    cli.command_name(),
                    &error,
                    operation_id.as_deref(),
                )?;
            }
            Ok(error.exit_code())
        }
    }
}

struct PublicOperationJournal {
    log: PublicOperationLog,
    operation_id: Option<String>,
}

impl PublicOperationJournal {
    const fn new(log: PublicOperationLog) -> Self {
        Self {
            log,
            operation_id: None,
        }
    }

    fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    fn append_event(&mut self, operation_id: &str, record: &[u8]) -> io::Result<()> {
        match self.operation_id.as_deref() {
            Some(bound) if bound != operation_id => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "public progress operation id changed",
            )),
            Some(_) => self.log.append(operation_id, record),
            None => {
                self.log.append(operation_id, record)?;
                self.operation_id = Some(operation_id.to_owned());
                Ok(())
            }
        }
    }

    fn append_record(&mut self, record: &[u8]) -> io::Result<()> {
        let Some(operation_id) = self.operation_id.as_deref() else {
            return Ok(());
        };
        self.log.append(operation_id, record)
    }

    fn append_records(&mut self, records: &[Vec<u8>]) -> io::Result<()> {
        if self.operation_id.is_some() {
            for record in records {
                self.append_record(record)?;
            }
        }
        Ok(())
    }
}

fn public_stream_unavailable(_: io::Error) -> CommandError {
    CommandError::new(
        ExitCode::Config,
        "the sanitized operation stream is unavailable",
        "check the private user-state log directory and retry",
    )
}

#[cfg(test)]
pub(crate) fn write_success(
    mut writer: impl Write,
    mode: OutputMode,
    command: &str,
    result: &CommandResult,
) -> io::Result<()> {
    let lines = success_jsonl_lines(command, result, None)?;
    write_success_lines(&mut writer, mode, command, result, None, &lines)
}

fn write_success_lines(
    mut writer: impl Write,
    mode: OutputMode,
    command: &str,
    result: &CommandResult,
    operation_id: Option<&str>,
    jsonl_lines: &[Vec<u8>],
) -> io::Result<()> {
    match mode {
        OutputMode::Human if command == "search" => write_search_result(&mut writer, result),
        OutputMode::Human => writeln!(writer, "{}", result.summary()),
        OutputMode::Json => {
            let mut value = result.fields().clone();
            bind_operation_id(&mut value, operation_id)?;
            value.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
            value.insert("ok".into(), json!(true));
            value.insert("command".into(), json!(command));
            write_json_line(writer, &value)
        }
        OutputMode::JsonLines => {
            for line in jsonl_lines {
                writer.write_all(line)?;
            }
            Ok(())
        }
    }
}

fn write_search_result(mut writer: impl Write, result: &CommandResult) -> io::Result<()> {
    if !result.records().is_empty() {
        let package_width = result
            .records()
            .iter()
            .filter_map(|record| record.get("package").and_then(Value::as_str))
            .map(str::len)
            .max()
            .unwrap_or(7)
            .max(7);
        let version_width = result
            .records()
            .iter()
            .filter_map(|record| record.get("version").and_then(Value::as_str))
            .map(str::len)
            .max()
            .unwrap_or(7)
            .max(7);
        writeln!(
            writer,
            "{:<package_width$}  {:<version_width$}  {:<11}  DESCRIPTION",
            "PACKAGE", "VERSION", "STATUS"
        )?;
        for record in result.records() {
            let package = record.get("package").and_then(Value::as_str).unwrap_or("-");
            let version = record
                .get("version")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("-");
            let description = record
                .get("description")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("-");
            let status = match (
                record.get("broken").and_then(Value::as_bool),
                record.get("available").and_then(Value::as_bool),
            ) {
                (Some(true), _) => "broken",
                (Some(false), Some(true)) => "ready",
                _ => "unsupported",
            };
            writeln!(
                writer,
                "{package:<package_width$}  {version:<version_width$}  {status:<11}  {description}"
            )?;
        }
        writeln!(writer)?;
    }
    writeln!(writer, "{}", result.summary())?;
    if let Some(generated_at) = result
        .fields()
        .get("catalogGeneratedAt")
        .and_then(Value::as_str)
    {
        writeln!(writer, "Catalog updated: {generated_at}")?;
    }
    if result.fields().get("stale").and_then(Value::as_bool) == Some(true) {
        writeln!(writer, "Catalog data is stale.")?;
    }
    Ok(())
}

fn success_jsonl_lines(
    command: &str,
    result: &CommandResult,
    operation_id: Option<&str>,
) -> io::Result<Vec<Vec<u8>>> {
    let mut lines = Vec::with_capacity(result.records().len() + 1);
    for record in result.records() {
        let mut value = record.clone();
        bind_operation_id(&mut value, operation_id)?;
        value.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
        lines.push(json_line_bytes(&value)?);
    }
    let mut terminal = result.fields().clone();
    bind_operation_id(&mut terminal, operation_id)?;
    terminal.insert("schemaVersion".into(), json!(PUBLIC_SCHEMA_VERSION));
    terminal.insert("type".into(), json!("result"));
    terminal.insert("ok".into(), json!(true));
    terminal.insert("command".into(), json!(command));
    lines.push(json_line_bytes(&terminal)?);
    Ok(lines)
}

fn bind_operation_id(value: &mut Map<String, Value>, operation_id: Option<&str>) -> io::Result<()> {
    let Some(operation_id) = operation_id else {
        return Ok(());
    };
    match value.get("opId") {
        Some(existing) if existing.as_str() == Some(operation_id) => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal result operation id does not match progress stream",
        )),
        None => {
            value.insert("opId".into(), json!(operation_id));
            Ok(())
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
mod tests;
