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
mod uninstall;

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
    // `pkg help [COMMAND]` is `pkg [COMMAND] --help`.
    let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    if args.get(1).is_some_and(|arg| arg == "help") {
        args.remove(1);
        args.push(std::ffi::OsString::from("--help"));
    }
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        // `pkg --json` without a command gets the same help as `pkg`. The
        // clap error would list the hidden commands.
        Err(error) if error.kind() == clap::error::ErrorKind::MissingSubcommand => {
            use clap::CommandFactory as _;
            let help = Cli::command().render_help();
            if crate::output::color(crate::output::Stream::Err) {
                eprint!("{}", help.ansi());
            } else {
                eprint!("{help}");
            }
            return ExitCode::from(USAGE);
        }
        Err(mut error) => {
            // Clap lists the closest subcommand last. Show it first.
            use clap::error::{ContextKind, ContextValue};
            if let Some(ContextValue::Strings(names)) =
                error.get(ContextKind::SuggestedSubcommand).cloned()
            {
                error.insert(
                    ContextKind::SuggestedSubcommand,
                    ContextValue::Strings(names.into_iter().rev().collect()),
                );
            }
            error.exit()
        }
    };
    let outcome = dispatch(&cli);
    if cli.json
        && let Err(error) = &outcome
        && !matches!(error, CommandError::Reported(_))
        && !error.is_interrupted()
        && let Some(command) = json_command(&cli.command)
    {
        // Scripts read stdout; the human line still goes to stderr.
        let _ = print_json(&serde_json::json!({
            "schema": crate::output::JSON_SCHEMA,
            "command": command,
            "error": error_text(error),
        }));
    }
    finish(outcome)
}

/// Whether an error message holds a full native error report.
fn native_report(text: &str) -> bool {
    text.contains("native command `nix ") && text.lines().nth(1).is_some()
}

/// The text of a final error. A native report becomes its one plain cause,
/// unless `--verbose` asks for the full report.
fn error_text(error: &CommandError) -> String {
    let text = error.to_string();
    if native_report(&text) && !crate::output::verbose() {
        sentence(plain_detail(&text))
    } else {
        text
    }
}

/// The JSON command name of a query command, or `None` for other commands.
fn json_command(command: &Command) -> Option<&'static str> {
    Some(match command {
        Command::Search(..) => "search",
        Command::Info { .. } => "info",
        Command::List => "list",
        Command::Doctor => "doctor",
        Command::History => "history",
        Command::Tap(crate::cli::TapCommand::List) => "tap-list",
        _ => return None,
    })
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
            eprintln!("Discovery cancelled. The complete search data did not change.");
            error::signal_exit(signal)
        }
        Err(error) => {
            eprintln!(
                "{} {}",
                crate::output::paint(
                    crate::output::Stream::Err,
                    crate::output::Tone::Error,
                    "pkg:"
                ),
                error_text(&error)
            );
            if native_report(&error.to_string()) && !crate::output::verbose() {
                eprintln!("Run the command again with --verbose to see the Nix output.");
            }
            error
                .signal()
                .map_or_else(|| ExitCode::from(FAILURE), error::signal_exit)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<(), CommandError> {
    crate::output::plain(cli.verbose);
    crate::output::disable_color(
        cli.no_color
            || config::paths()
                .ok()
                .and_then(|paths| Config::load(&paths.config_file).ok())
                .is_some_and(|config| config.color_disabled(false)),
    );
    // `--json` is a query option only; mutating commands must reject it
    // before touching the profile.
    if cli.json && json_command(&cli.command).is_none() {
        eprintln!(
            "pkg: `--json` works only with search, info, list, history, doctor, and tap list."
        );
        return Err(CommandError::Reported(ExitCode::from(USAGE)));
    }
    match &cli.command {
        Command::Search(options) => discovery::search(cli, options),
        Command::Info { id } => discovery::info(cli, id),
        Command::Install { ids } => profile::install(cli, ids),
        Command::List => profile::list(cli),
        Command::Remove { entries, entry } => profile::remove(cli, entries, *entry),
        Command::Update {
            scheduled,
            skip_client,
        } => maintenance::update(cli, *scheduled, *skip_client),
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
        Command::Client(crate::cli::ClientCommand::Uninstall { yes }) => uninstall::run(*yes),
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
/// End `text` with a period unless it already ends a sentence.
fn sentence(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?', ':']) {
        text.push('.');
    }
    text
}

pub(crate) fn report_cause(detail: &str) {
    let cause = plain_detail(detail);
    if !cause.is_empty() {
        eprintln!("Cause: {}", sentence(cause));
    }
    if let Some(log) = detail
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("nix log "))
    {
        eprintln!("Details: {log}");
    }
}

/// One actionable cause from a nested failure report, in plain words.
pub(crate) fn plain_detail(detail: &str) -> String {
    let cause = concise_cause(detail);
    // Some causes need the whole report, such as an unfree license.
    if detail.contains("because it has an unfree license") {
        return String::from(
            "this package has an unfree license, and pkg installs only free Nixpkgs packages",
        );
    }
    if detail.contains("is not available on the requested hostPlatform") {
        return String::from("this package does not run on this system");
    }
    if detail.contains("because it has problems:") && detail.contains("- broken:")
        || detail.contains("is marked as broken")
    {
        return String::from("Nixpkgs marks this package as broken on this system");
    }
    if detail.contains("because it is marked as insecure") {
        return String::from(
            "this package has known security problems, and pkg installs only packages without them",
        );
    }
    plain_cause(cause).into_owned()
}

/// Replace a known native message with plain words.
fn plain_cause(cause: &str) -> std::borrow::Cow<'_, str> {
    // Nix lists every attribute path that it tried for a missing package;
    // the last one is the package name.
    if let Some((_, tried)) = cause.split_once("does not provide attribute ")
        && let Some(name) = tried.rsplit('\'').nth(1)
    {
        return format!("the source has no package named `{name}`").into();
    }
    if cause.contains("HTTP error 403") && cause.contains("github.com") {
        return "GitHub refused the download (HTTP error 403). This is often a rate limit. Try again later".into();
    }
    // Nix and the client update both download with curl.
    if (cause.contains("unable to download") || cause.contains("curl"))
        && [
            "Couldn't connect",
            "Failed to connect",
            "Could not connect",
            "Could not resolve",
            "Timeout",
            "timed out",
        ]
        .iter()
        .any(|text| cause.contains(text))
    {
        return "could not connect to the network. Check the connection, then try again".into();
    }
    if cause.contains("cannot connect to socket")
        || cause.contains("opening a connection to remote store")
    {
        return "pkg cannot reach the Nix daemon. Run `pkg doctor`".into();
    }
    cause.into()
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

#[cfg(test)]
mod tests {
    #[test]
    fn nixpkgs_refusals_have_plain_causes() {
        for (detail, cause) in [
            (
                "error:\n       error: Refusing to evaluate package 'SDL_stretch-0.3.1' in /nix/store/x-source/pkgs/by-name/sd/SDL_stretch/package.nix:31 because it has problems:\n       - broken: This package is broken.",
                "Nixpkgs marks this package as broken on this system",
            ),
            (
                "error:\n       error: Refusing to evaluate package 'alist-3.63.0' in /nix/store/x-source/pkgs/by-name/al/alist/package.nix:106 because it is marked as insecure\n       Known issues:",
                "this package has known security problems, and pkg installs only packages without them",
            ),
            (
                "error:\n       error: Refusing to evaluate package 'vscode-1.142.0' in /nix/store/x-source/vscode.nix:104 because it has an unfree license (‘unfree’)",
                "this package has an unfree license, and pkg installs only free Nixpkgs packages",
            ),
        ] {
            assert_eq!(super::plain_detail(detail), cause);
        }
    }
}
