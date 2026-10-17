//! Command execution for the reduced grammar (design D4).
//!
//! Mutating commands delegate package realization to Nix, show native
//! progress, and print a short final result. Nothing here retries silently
//! or writes product state.
//!
//! Error rule: every handler returns `Result<(), CommandError>` and uses
//! `?` for ordinary diagnostics; [`run`] converts the result to an exit
//! code in exactly one place. The change owner reports mutation failures
//! — re-read native state included — and end with the reported variant of
//! `CommandError`, so diagnostics are never duplicated.

mod change;
mod conflict;
mod doctor;
mod error;
mod info;
mod lifecycle;
mod list;
mod maintenance;
mod resolution;
#[cfg(test)]
#[path = "tests/saved.rs"]
mod saved_tests;
mod search;
mod support;
mod tap;

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::Parser as _;

use crate::catalog;
use crate::cli::{Cli, Command};
use crate::config::{self, Config, Paths};
use crate::nix::{self, Nix, NixError, ProfileEntry};

use error::CommandError;

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
fn finish(outcome: Result<(), CommandError>) -> ExitCode {
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
        Command::Search(options) => search::run(cli, options),
        Command::Info { id } => info::run(cli, id),
        Command::Install { ids } => lifecycle::install(cli, ids),
        Command::List => list::run(cli),
        Command::Remove { entries, entry } => lifecycle::remove(cli, entries, *entry),
        Command::Update { catalogs_only } => maintenance::update(cli, *catalogs_only),
        Command::Setup { no_background } => maintenance::setup(cli, *no_background),
        Command::Background(command) => maintenance::background(command),
        Command::Upgrade {
            entries,
            all,
            entry,
        } => lifecycle::upgrade(cli, entries, *all, *entry),
        Command::History => lifecycle::history(cli),
        Command::Rollback { generation } => lifecycle::rollback(cli, *generation),
        Command::Prune { older_than } => lifecycle::prune(cli, older_than),
        Command::Doctor => doctor::doctor(cli),
        Command::Shellenv => support::shellenv(),
        Command::Completion { shell } => support::completion(*shell),
        Command::Apps(crate::cli::AppsCommand::Sync) => support::apps_sync(cli),
        Command::Apps(crate::cli::AppsCommand::Setup) => support::apps_setup(),
        Command::Tap(command) => tap::dispatch(cli, command),
    }
}

/// One command run: resolved paths, configuration, and runtime.
struct Session {
    paths: Paths,
    config: Config,
    nix: Nix,
}

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

/// The tap state one query or mutation run uses: the registry, the
/// routing it implies, and saved catalogs loaded per source on demand.
///
/// Qualified requests are isolated from unrelated broken taps: a
/// source's saved catalog loads only when that source is actually read
/// ([`TapState::saved_one`]). Bare-name resolution is the strict
/// exception: it must see every source to fail closed on ambiguity, so
/// it loads all of them ([`TapState::saved_all`]) and the first broken
/// source fails the resolution.
struct TapState {
    /// Source routing for every catalog source.
    routing: catalog::Routing,
    /// The loaded tap registry.
    registry: crate::tap::Registry,
    /// The state home the saved catalogs live under.
    state_home: std::path::PathBuf,
    /// Saved catalogs loaded so far, keyed by source.
    saved: BTreeMap<String, crate::tap::SavedCatalog>,
}

impl TapState {
    /// Load the tap state for one command run: the registry and the
    /// routing it implies. A malformed registry fails the command before
    /// any query runs; saved generations are deliberately not touched
    /// here, so a request that names its source never reads an
    /// unrelated one.
    fn load(session: &Session) -> Result<Self, CommandError> {
        let registry = crate::tap::Registry::load(&crate::tap::registry::registry_path(
            &session.paths.state_home,
        ))
        .map_err(CommandError::Message)?;
        let routing = catalog::Routing::from_registry(
            &session.config.sources,
            &registry,
            &session.paths.state_home,
        )
        .map_err(CommandError::Message)?;
        Ok(Self {
            routing,
            registry,
            state_home: session.paths.state_home.clone(),
            saved: BTreeMap::new(),
        })
    }

    /// One registered source's saved catalog, loaded on first use.
    ///
    /// Loading is strict and reads exactly this source: a broken
    /// unrelated tap cannot fail a request that never reads it, and an
    /// unregistered source is refused like routing refuses it.
    fn saved_one(&mut self, source: &str) -> Result<&crate::tap::SavedCatalog, String> {
        use std::collections::btree_map::Entry;
        match self.saved.entry(source.to_string()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                if !self.registry.sources.contains_key(source) {
                    return Err(format!(
                        "unknown cask source `{source}`; \
                         add it first with `pkg tap add {source}`"
                    ));
                }
                let catalog =
                    crate::tap::SavedCatalogs::load_one(&self.registry, &self.state_home, source)?;
                Ok(entry.insert(catalog))
            }
        }
    }

    /// Every registered source's saved catalog, loaded strictly.
    ///
    /// Bare-name resolution needs the complete source set to stay
    /// fail-closed: one unreadable or malformed capture fails the whole
    /// load before any package choice is made.
    fn saved_all(&mut self) -> Result<crate::tap::SavedCatalogs, String> {
        let sources: Vec<_> = self.registry.sources.keys().cloned().collect();
        for source in sources {
            self.saved_one(&source)?;
        }
        crate::tap::SavedCatalogs::from_catalogs(self.saved.values().cloned())
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
fn mutation_failed(
    session: &Session,
    action: &str,
    error: &nix::NixError,
    before: Option<&BTreeMap<String, ProfileEntry>>,
    requested: Option<&[String]>,
) -> CommandError {
    let current = installed_entries(session);
    let signal = error
        .signal()
        .or_else(|| current.as_ref().err().and_then(NixError::signal));
    let interrupted = signal.is_some();
    let profile_changed = current
        .as_ref()
        .is_ok_and(|entries| before.is_some_and(|previous| previous != entries));
    let (status, detail) = match (interrupted, profile_changed) {
        (true, true) => ("Interrupted", "stopped after the profile changed."),
        (true, false) => ("Interrupted", "stopped."),
        (false, true) => ("Partial", "changed the profile before it stopped."),
        (false, false) => ("Failed", "did not complete."),
    };
    eprintln!("{status}: {action} {detail}");
    if let Ok(entries) = &current {
        match before {
            Some(_) if !profile_changed => {
                eprintln!("Installed entries unchanged.");
            }
            Some(previous) => {
                let added = entries
                    .keys()
                    .filter(|key| !previous.contains_key(*key))
                    .count();
                let removed = previous
                    .keys()
                    .filter(|key| !entries.contains_key(*key))
                    .count();
                let changed = entries
                    .keys()
                    .filter(|key| {
                        previous
                            .get(*key)
                            .is_some_and(|old| Some(old) != entries.get(*key))
                    })
                    .count();
                eprintln!("Profile: {added} added, {removed} removed, {changed} updated.");
                eprintln!("Next: run `pkg list` to inspect the active profile.");
            }
            None => {
                eprintln!("Profile checked: {} entries are active.", entries.len());
            }
        }
    } else {
        eprintln!("Package state is unknown. Next: run `pkg doctor`.");
    }
    if interrupted || !conflict::report(session, error, current.as_ref().ok(), requested) {
        report_native_cause(error);
    }
    if let Err(read_error @ NixError::Process(_)) = &current
        && read_error.signal().is_some()
    {
        eprintln!("Profile check failed: {read_error}");
    }
    CommandError::Reported(signal.map_or_else(|| ExitCode::from(FAILURE), error::signal_exit))
}

fn report_native_cause(error: &NixError) {
    if !matches!(error, NixError::Interrupted { .. }) {
        report_cause(&error.to_string());
    }
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

/// Read installed entries, treating a not-yet-created profile as empty.
///
/// A fresh install has no profile on disk; that state is empty, not an error.
fn installed_entries(session: &Session) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    match std::fs::symlink_metadata(&session.paths.profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(NixError::Io {
                what: "profile state",
                source: error,
            });
        }
        Ok(_) => {}
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
    profile: &crate::profile_lock::LockedProfile<'_>,
    action: &str,
    names: &[String],
) -> Result<(), CommandError> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    match crate::apps::sync_locked(profile) {
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
            let detail_text = detail.to_string();
            report_cause(&detail_text);
            eprintln!("Next: Resolve the cause, then run `pkg apps sync`.");
            if detail.dock_effect_possible() {
                eprintln!("Dock state may have changed.");
            } else {
                eprintln!("Dock state did not change.");
            }
            Err(detail.signal().map_or_else(
                || CommandError::Reported(ExitCode::from(FAILURE)),
                |signal| CommandError::Reported(error::signal_exit(signal)),
            ))
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
