//! Command execution for the reduced grammar (design D4).
//!
//! Mutating commands delegate package realization to Nix, show native
//! progress, and print a short final result. Nothing here retries silently
//! or writes product state.
//!
//! Layout: query commands live in `query`, lifecycle commands in
//! `lifecycle`, doctor in `doctor`, and the remaining utility commands
//! in `support`. This module holds the shared session, dispatch, and
//! failure-reporting rules.
//!
//! Error rule: every handler returns `Result<(), CommandError>` and uses
//! `?` for ordinary diagnostics; [`run`] converts the result to an exit
//! code in exactly one place. Handlers report mutation failures themselves
//! — re-read native state included — and end with the reported variant of
//! `CommandError`, so diagnostics are never duplicated.

mod doctor;
mod lifecycle;
mod query;
mod support;
mod tap;

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser as _;

use crate::catalog;
use crate::cli::{Cli, Command};
use crate::config::{self, Config, ConfigError, Paths};
use crate::nix::{self, Nix, NixError, ProfileEntry};

/// Exit status for a runtime failure with diagnostics already printed.
pub const FAILURE: u8 = 1;

/// Exit status for a CLI usage error, matching the parser's own exits.
pub const USAGE: u8 = 2;

/// Exit status used when a native operation was interrupted by a signal.
///
/// The child was reaped and native state was re-read before exiting.
pub const INTERRUPTED: u8 = 130;

/// How a command ended without success.
pub(super) enum CommandError {
    /// Not yet printed: the runner prints it with the `pkg:` prefix and
    /// exits with the ordinary failure status.
    Message(String),
    /// Already printed, including re-read native state where it applies;
    /// the runner exits with this status unchanged.
    Reported(ExitCode),
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<NixError> for CommandError {
    fn from(error: NixError) -> Self {
        Self::Message(error.to_string())
    }
}

impl From<catalog::CatalogError> for CommandError {
    fn from(error: catalog::CatalogError) -> Self {
        Self::Message(error.to_string())
    }
}

impl From<ConfigError> for CommandError {
    fn from(error: ConfigError) -> Self {
        Self::Message(error.to_string())
    }
}

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
fn finish(outcome: Result<(), CommandError>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(CommandError::Message(message)) => {
            eprintln!("pkg: {message}");
            ExitCode::from(FAILURE)
        }
        Err(CommandError::Reported(code)) => code,
    }
}

fn dispatch(cli: &Cli) -> Result<(), CommandError> {
    // `--json` is a query option only; mutating commands must reject it
    // before touching the profile.
    if cli.json
        && !matches!(
            cli.command,
            Command::Search { .. }
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
        Command::Search { query } => query::search(cli, query),
        Command::Info { id } => query::info(cli, id),
        Command::Install { ids } => lifecycle::install(cli, ids),
        Command::List => query::list(cli),
        Command::Remove { entries } => lifecycle::remove(cli, entries),
        Command::Update => lifecycle::update(cli),
        Command::Upgrade { entries, all } => lifecycle::upgrade(cli, entries, *all),
        Command::History => lifecycle::history(cli),
        Command::Rollback { generation } => lifecycle::rollback(cli, *generation),
        Command::Prune { older_than } => lifecycle::prune(cli, older_than),
        Command::Doctor => doctor::doctor(cli),
        Command::Shellenv => support::shellenv(),
        Command::Completion { shell } => support::completion(*shell),
        Command::Apps(crate::cli::AppsCommand::Sync) => support::apps_sync(cli),
        Command::Tap(command) => tap::dispatch(cli, command),
    }
}

/// One command run: resolved paths, configuration, and runtime.
struct Session {
    paths: Paths,
    config: Config,
    nix: Nix,
}

/// Discover the external runtime with the user's output options applied.
///
/// `--verbose` traces every native call in the adapter; `--no-color` and
/// the config color policy are forwarded to native children as NO_COLOR.
/// Every command that runs native children goes through this one setup,
/// doctor included.
fn discover_runtime(cli: &Cli, config: &Config) -> Result<Nix, NixError> {
    let no_color = config.color_disabled(cli.no_color);
    Ok(nix::discover(config.runtime.nix.as_deref())?
        .verbose(cli.verbose)
        .no_color(no_color))
}

fn session(cli: &Cli) -> Result<Session, CommandError> {
    let paths = config::paths()?;
    let config = Config::load(&paths.config_file)?;
    let nix = discover_runtime(cli, &config)?;
    Ok(Session { paths, config, nix })
}

/// One command run that needs no Nix runtime: paths and configuration only.
struct PathsOnly {
    paths: Paths,
}

/// Resolve paths and configuration without discovering a Nix runtime.
fn paths_only() -> Result<PathsOnly, CommandError> {
    Ok(PathsOnly {
        paths: config::paths()?,
    })
}

/// The tap state one query or mutation run uses: the strictly loaded
/// registry, the routing it implies, and every saved catalog.
struct TapState {
    /// Source routing for every catalog source.
    routing: catalog::Routing,
    /// The saved catalog of every registered tap, loaded strictly.
    saved: crate::tap::SavedCatalogs,
}

impl TapState {
    /// Load the tap state for one command run; malformed state fails the
    /// command before any query runs.
    fn load(session: &Session) -> Result<Self, CommandError> {
        let (registry, saved) =
            crate::tap::load_all(&session.paths.state_home).map_err(CommandError::Message)?;
        let routing = catalog::Routing::from_registry(
            &session.config.sources,
            &registry,
            &session.paths.state_home,
        )
        .map_err(CommandError::Message)?;
        Ok(Self { routing, saved })
    }
}

/// The configured sources in query order: Nixpkgs first, then casks.
fn configured_sources(config: &Config) -> [(catalog::SourceKind, &str); 2] {
    [
        (
            catalog::SourceKind::Nixpkgs,
            config.sources.nixpkgs.as_str(),
        ),
        (catalog::SourceKind::Cask, config.sources.casks.as_str()),
    ]
}

/// Report native state after a failed or interrupted mutation.
///
/// The child was reaped inside the adapter; pkg stays alive to re-read the
/// authoritative profile and report it instead of exiting silently. This
/// runs after every failed mutation, not only cancellations, because a
/// failed native operation may also have partially applied. The state is
/// stated, not guessed.
fn report_failed_mutation(
    session: &Session,
    action: &str,
    error: &nix::NixError,
    interrupted: bool,
    before: Option<&BTreeMap<String, ProfileEntry>>,
) -> CommandError {
    match installed_entries(session) {
        Ok(entries) => match before {
            Some(previous) if previous == &entries => {
                if interrupted {
                    eprintln!("Interrupted: {action} stopped.");
                } else {
                    eprintln!("Failed: {action} did not complete.");
                }
                eprintln!("Installed entries unchanged.");
            }
            Some(previous) => {
                if interrupted {
                    eprintln!("Interrupted: {action} stopped after the profile changed.");
                } else {
                    eprintln!("Partial: {action} changed the profile before it stopped.");
                }
                let added: Vec<_> = entries
                    .keys()
                    .filter(|key| !previous.contains_key(*key))
                    .collect();
                let removed: Vec<_> = previous
                    .keys()
                    .filter(|key| !entries.contains_key(*key))
                    .collect();
                let changed: Vec<_> = entries
                    .keys()
                    .filter(|key| {
                        previous
                            .get(*key)
                            .is_some_and(|old| Some(old) != entries.get(*key))
                    })
                    .collect();
                eprintln!(
                    "Profile: {} added, {} removed, {} updated.",
                    added.len(),
                    removed.len(),
                    changed.len()
                );
                eprintln!("Next: run `pkg list` to inspect the active profile.");
            }
            None => {
                if interrupted {
                    eprintln!("Interrupted: {action} stopped.");
                } else {
                    eprintln!("Failed: {action} did not complete.");
                }
                eprintln!("Profile checked: {} entries are active.", entries.len());
            }
        },
        Err(_) => {
            if interrupted {
                eprintln!("Interrupted: {action} stopped.");
            } else {
                eprintln!("Failed: {action} did not complete.");
            }
            eprintln!("Package state is unknown. Next: run `pkg doctor`.");
        }
    }
    if !interrupted {
        report_cause(&error.to_string());
    }
    CommandError::Reported(ExitCode::from(if interrupted {
        INTERRUPTED
    } else {
        FAILURE
    }))
}

/// Classify a mutation error, report re-read native state, and end the
/// command with the matching exit status.
fn mutation_failed(
    session: &Session,
    action: &str,
    error: &nix::NixError,
    before: Option<&BTreeMap<String, ProfileEntry>>,
) -> CommandError {
    let interrupted = matches!(error, nix::NixError::Interrupted { .. });
    report_failed_mutation(session, action, error, interrupted, before)
}

/// Report one useful native cause and one log command from a nested error.
fn report_cause(detail: &str) {
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
fn concise_cause(detail: &str) -> &str {
    detail
        .lines()
        .map(str::trim)
        .find(|line| line.contains("failed to allocate"))
        .or_else(|| {
            detail
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("error:"))
        })
        .or_else(|| detail.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("unknown cause")
        .trim_start_matches("error: ")
}

/// Read installed entries, treating a not-yet-created profile as empty.
///
/// A fresh install has no profile on disk; that state is empty, not an error.
fn installed_entries(session: &Session) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    if !session.paths.profile.exists() {
        return Ok(BTreeMap::new());
    }
    session.nix.profile_list(&session.paths.profile)
}

/// Refresh derived macOS launchers after a successful profile mutation.
///
/// The package change has already completed in the native profile. When the
/// launcher sync fails, the failure is reported as partial completion with a
/// nonzero exit and `pkg apps sync` named as the retry (design D4). On other
/// systems app exposure does not apply and this is a no-op.
fn apps_refresh_after_mutation(
    session: &Session,
    action: &str,
    names: &[String],
) -> Result<(), CommandError> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    match crate::apps::sync(&session.nix, &session.paths) {
        Ok(()) => Ok(()),
        Err(detail) => {
            if names.is_empty() {
                eprintln!(
                    "Partial: {action} left installed entries unchanged. App launcher setup failed."
                );
            } else {
                eprintln!(
                    "Partial: {action} changed {}. App launcher setup failed.",
                    names.join(", ")
                );
            }
            report_cause(&detail);
            eprintln!("Next: Resolve the cause, then run `pkg apps sync`.");
            Err(CommandError::Reported(ExitCode::from(FAILURE)))
        }
    }
}

/// Print one query result as the versioned JSON envelope.
fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CommandError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| CommandError::Message(format!("cannot encode JSON output: {error}")))?;
    println!("{text}");
    Ok(())
}
