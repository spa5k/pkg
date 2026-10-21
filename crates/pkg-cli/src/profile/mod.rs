//! Profile commands, queries, and post-change launcher refresh.
//!
//! This feature owns the lock, native mutation order, and state reports.
//! Nix remains the owner of installed entries and generations.

mod change;
mod conflict;
mod installed;
mod lock;
pub use lock::LockedProfile;
mod query;
mod summary;

pub use installed::package_id;
pub use query::{entries, history, installed_entries, list, status};

use crate::catalog;
use crate::cli::Cli;

use self::change::{ProfileChange, Request};
use crate::commands::{CommandError, FAILURE, Session, error, report_native_cause, session};
use crate::nix::{NixError, ProfileEntry};
use std::collections::BTreeMap;
use std::process::ExitCode;

pub fn install(cli: &Cli, ids: &[String]) -> Result<(), CommandError> {
    if ids.is_empty() {
        return Err(String::from("install needs at least one ID").into());
    }
    let mut unique: Vec<&str> = Vec::new();
    for id in ids {
        if !unique.contains(&id.as_str()) {
            unique.push(id);
        }
    }
    let parsed_ids = unique
        .iter()
        .map(|id| catalog::parse_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    eprintln!("Resolving: {}", unique.join(", "));
    let session = session(cli)?;
    let batch = crate::discovery::resolve_install(&session, parsed_ids)?;
    change::execute(
        &session,
        Request::Profile(ProfileChange::Install {
            installables: batch.installables,
            system: &batch.system,
        }),
    )
}

pub fn remove(cli: &Cli, entries: &[String], exact_entry: bool) -> Result<(), CommandError> {
    if entries.is_empty() {
        return Err(String::from("remove needs at least one installed entry ID").into());
    }
    change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Remove {
            entries,
            exact_entry,
        }),
    )
}

pub fn upgrade(
    cli: &Cli,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<(), CommandError> {
    change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Upgrade {
            entries,
            all,
            exact_entry,
        }),
    )
}

pub fn rollback(cli: &Cli, generation: Option<u64>) -> Result<(), CommandError> {
    change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Rollback(generation)),
    )
}

pub fn prune(cli: &Cli, older_than: &str) -> Result<(), CommandError> {
    let days = crate::cli::parse_prune_age(older_than)?;
    change::execute(&session(cli)?, Request::Prune { days })
}

/// Report native state after a failed or interrupted mutation.
///
/// The adapter has reaped the child. Read and report the profile after
/// every failed mutation because errors and cancellation can leave partial changes.
fn mutation_failed(
    session: &Session,
    action: &str,
    error: &NixError,
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
