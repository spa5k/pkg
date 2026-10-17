//! Execute `pkg` commands.
//!
//! Package commands call Nix, show its progress, and print a final result.
//!
//! Handlers return `Result<(), CommandError>`. [`run`] converts the result
//! to an exit code. A handler that reports a mutation failure first reads
//! native state again, then returns the reported error variant to prevent
//! duplicate diagnostics.

mod doctor;
pub(crate) mod error;
mod maintenance;
mod support;

use std::process::ExitCode;

use clap::Parser as _;

use crate::catalog;
use crate::cli::{Cli, Command};
use crate::config::{self, Config, Paths};
use crate::nix::{self, Nix, NixError};

use crate::{apps, discovery, profile, tap};
pub(crate) use error::CommandError;

/// Exit status for a runtime failure with diagnostics already printed.
pub const FAILURE: u8 = 1;

/// Exit status for a CLI usage error, matching the parser's own exits.
pub const USAGE: u8 = 2;

/// Exit status used when a native operation was interrupted by a signal.
///
/// The child was reaped and native state was re-read before exiting.
pub const INTERRUPTED: u8 = 130;

/// Parse arguments and run the requested command.
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    finish(dispatch(&cli))
}

/// Convert one command outcome to an exit code, printing ordinary
/// diagnostics exactly once.
pub(crate) fn finish(outcome: Result<(), CommandError>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(CommandError::Reported(code)) => code,
        Err(CommandError::Native(NixError::Interrupted { signal, .. })) => {
            error::signal_exit(signal)
        }
        Err(CommandError::Catalog(catalog::CatalogError::Interrupted(signal))) => {
            eprintln!("Discovery cancelled. The previous complete cache was retained.");
            error::signal_exit(signal)
        }
        Err(error) => {
            eprintln!("pkg: {error}");
            error
                .signal()
                .map_or_else(|| ExitCode::from(FAILURE), error::signal_exit)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<(), CommandError> {
    // `--json` is a query option only; mutating commands must reject it
    // before touching the profile.
    if cli.json
        && !matches!(
            cli.command,
            Command::Search(..)
                | Command::Info { .. }
                | Command::List
                | Command::Doctor
                | Command::Tap(crate::cli::TapCommand::List)
        )
    {
        eprintln!(
            "pkg: `--json` is a query option (search, info, list, doctor, tap list); \
             it is not accepted here"
        );
        return Err(CommandError::Reported(ExitCode::from(USAGE)));
    }
    match &cli.command {
        Command::Search(options) => discovery::search(cli, options),
        Command::Info { id } => discovery::info(cli, id),
        Command::Install { ids } => profile::install(cli, ids),
        Command::List => profile::list(cli),
        Command::Remove { entries, entry } => profile::remove(cli, entries, *entry),
        Command::Update { catalogs_only } => maintenance::update(cli, *catalogs_only),
        Command::Setup { no_background } => maintenance::setup(cli, *no_background),
        Command::Background(command) => maintenance::background(command),
        Command::Upgrade {
            entries,
            all,
            entry,
        } => profile::upgrade(cli, entries, *all, *entry),
        Command::History => profile::history(cli),
        Command::Rollback { generation } => profile::rollback(cli, *generation),
        Command::Prune { older_than } => profile::prune(cli, older_than),
        Command::Doctor => doctor::doctor(cli),
        Command::Shellenv => support::shellenv(),
        Command::Completion { shell } => support::completion(*shell),
        Command::Apps(command) => apps::command(cli, command),
        Command::Tap(command) => tap::dispatch(cli, command),
    }
}

/// One command run: resolved paths, configuration, and runtime.
pub(crate) struct Session {
    pub(crate) paths: Paths,
    pub(crate) config: Config,
    pub(crate) nix: Nix,
}

fn discover_runtime(cli: &Cli, config: &Config) -> Result<Nix, NixError> {
    let no_color = config.color_disabled(cli.no_color);
    Ok(nix::discover(config.runtime.nix.as_deref())?
        .verbose(cli.verbose)
        .no_color(no_color))
}

pub(crate) fn session(cli: &Cli) -> Result<Session, CommandError> {
    let paths = config::paths()?;
    let config = Config::load(&paths.config_file)?;
    let nix = discover_runtime(cli, &config)?;
    Ok(Session { paths, config, nix })
}

pub(crate) fn report_native_cause(error: &NixError) {
    if !matches!(error, NixError::Interrupted { .. }) {
        report_cause(&error.to_string());
    }
}

/// Report one useful native cause and one log command from a nested error.
pub(crate) fn report_cause(detail: &str) {
    let cause = concise_cause(detail);
    if !cause.is_empty() {
        eprintln!("Cause: {cause}");
    }
    if let Some(log) = detail
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("nix log "))
    {
        eprintln!("Details: {log}");
    }
}

/// Select one actionable native error from a nested failure report.
pub(crate) fn concise_cause(detail: &str) -> &str {
    detail
        .lines()
        .map(str::trim)
        .find(|line| line.contains("failed to allocate"))
        .or_else(|| {
            detail
                .lines()
                .map(str::trim)
                .find(|line| line.trim_start_matches("> ").starts_with("cask-build:"))
        })
        .or_else(|| {
            detail.lines().map(str::trim).find(|line| {
                line.starts_with("error:") && !line.trim_start_matches("error:").trim().is_empty()
            })
        })
        .or_else(|| detail.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("unknown cause")
        .trim_start_matches("> ")
        .trim_start_matches("cask-build: ")
        .trim_start_matches("error: ")
}

/// Print one query result as the versioned JSON envelope.
pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CommandError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| CommandError::Message(format!("cannot encode JSON output: {error}")))?;
    println!("{text}");
    Ok(())
}
