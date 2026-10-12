//! One owner for locked native package changes and derived app refresh.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use crate::catalog;
use crate::nix::{self, NixError, ProfileEntry};
use crate::tap::{registry, store};

use super::{CommandError, Session, apps_refresh_after_mutation, installed_entries, lifecycle};

pub(super) enum Request<'a> {
    Install {
        installables: Vec<String>,
        resolved_names: BTreeMap<String, String>,
        system: &'a str,
    },
    Remove {
        entries: &'a [String],
        exact_entry: bool,
    },
    Upgrade {
        entries: &'a [String],
        all: bool,
        exact_entry: bool,
    },
    Rollback(Option<u64>),
    Prune {
        days: u64,
    },
}

enum Prepared {
    Install {
        targets: Vec<String>,
        requested: Vec<String>,
    },
    Remove(Vec<String>),
    Upgrade(Vec<String>),
    Rollback {
        generation: Option<u64>,
        previous: Option<u64>,
    },
}

pub(super) fn execute(session: &Session, request: Request<'_>) -> Result<(), CommandError> {
    let profile = crate::profile_lock::LockedProfile::acquire(&session.nix, &session.paths)?;
    if let Request::Prune { days } = request {
        return prune(session, days);
    }
    let before = installed_entries(session)?;
    let Some(prepared) = prepare(session, request, &before)? else {
        return Ok(());
    };
    if let Err(error) = prepared.apply(session) {
        return Err(prepared.failed(session, &before, &error));
    }
    let after = installed_entries(session)
        .map_err(|error| report_postflight_failure(prepared.action(), &error))?;
    let report = prepared.completed(session, &before, &after);
    apps_refresh_after_mutation(&profile, prepared.action(), &report.names)?;
    report.render();
    Ok(())
}

fn prepare(
    session: &Session,
    request: Request<'_>,
    before: &BTreeMap<String, ProfileEntry>,
) -> Result<Option<Prepared>, CommandError> {
    match request {
        Request::Install {
            installables,
            resolved_names,
            system,
        } => prepare_install(session, before, installables, &resolved_names, system),
        Request::Remove {
            entries,
            exact_entry,
        } => {
            let targets = crate::installed::select(
                before,
                &session.config,
                &session.paths.state_home,
                entries,
                exact_entry,
            )?;
            eprintln!(
                "Removing: {}",
                selected_names(session, before, &targets).join(", ")
            );
            Ok(Some(Prepared::Remove(targets)))
        }
        Request::Upgrade {
            entries,
            all,
            exact_entry,
        } => prepare_upgrade(session, before, entries, all, exact_entry),
        Request::Rollback(generation) => {
            let previous = lifecycle::active_generation(&session.paths.profile);
            report_rollback_target(session, generation, previous)?;
            Ok(Some(Prepared::Rollback {
                generation,
                previous,
            }))
        }
        Request::Prune { days } => prune(session, days).map(|()| None),
    }
}

fn prepare_install(
    session: &Session,
    before: &BTreeMap<String, ProfileEntry>,
    installables: Vec<String>,
    resolved_names: &BTreeMap<String, String>,
    system: &str,
) -> Result<Option<Prepared>, CommandError> {
    let mut selected = BTreeSet::new();
    let mut targets = Vec::new();
    for installable in installables {
        if !selected.insert(installable.clone()) {
            continue;
        }
        if already_installed(&session.nix, before, &installable, system)? {
            println!("Already installed: {}", resolved_names[&installable]);
        } else {
            targets.push(installable);
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let requested = targets
        .iter()
        .map(|target| resolved_names[target].clone())
        .collect::<Vec<_>>();
    eprintln!("Installing: {}", requested.join(", "));
    Ok(Some(Prepared::Install { targets, requested }))
}

fn selected_names(
    session: &Session,
    entries: &BTreeMap<String, ProfileEntry>,
    targets: &[String],
) -> Vec<String> {
    targets
        .iter()
        .map(|key| package_id(session, &entries[key]))
        .collect()
}

impl Prepared {
    fn action(&self) -> &'static str {
        match self {
            Self::Install { .. } => "install",
            Self::Remove(_) => "remove",
            Self::Upgrade(_) => "upgrade",
            Self::Rollback { .. } => "rollback",
        }
    }

    fn apply(&self, session: &Session) -> Result<(), NixError> {
        let profile = &session.paths.profile;
        match self {
            Self::Install { targets, .. } => session.nix.profile_add(profile, targets),
            Self::Remove(targets) => session.nix.profile_remove(profile, targets),
            Self::Upgrade(targets) => session.nix.profile_upgrade(profile, Some(targets)),
            Self::Rollback { generation, .. } => session.nix.profile_rollback(profile, *generation),
        }
    }

    fn failed(
        &self,
        session: &Session,
        before: &BTreeMap<String, ProfileEntry>,
        error: &NixError,
    ) -> CommandError {
        if let Self::Rollback { previous, .. } = self
            && let Some(number) = lifecycle::active_generation(&session.paths.profile)
                .filter(|number| Some(*number) != *previous)
        {
            eprintln!("Partial: generation {number} is active. App launchers may need an update.");
            if !matches!(error, NixError::Interrupted { .. }) {
                super::report_cause(&error.to_string());
            }
            if cfg!(target_os = "macos") {
                eprintln!("Next: run `pkg apps sync`.");
            } else {
                eprintln!("Next: run `pkg history` to inspect generations.");
            }
            return CommandError::Reported(failure_exit(error));
        }
        let requested = match self {
            Self::Install { requested, .. } => Some(requested.as_slice()),
            _ => None,
        };
        super::mutation_failed(session, self.action(), error, Some(before), requested)
    }

    fn completed(
        &self,
        session: &Session,
        before: &BTreeMap<String, ProfileEntry>,
        after: &BTreeMap<String, ProfileEntry>,
    ) -> Report {
        match self {
            Self::Install { .. } => {
                let changed = after
                    .iter()
                    .filter(|(key, entry)| before.get(*key) != Some(*entry))
                    .map(|(_, entry)| entry)
                    .collect::<Vec<_>>();
                Report {
                    names: changed
                        .iter()
                        .map(|entry| package_id(session, entry))
                        .collect(),
                    completion: Completion::Packages {
                        verb: "Installed",
                        commands: changed
                            .iter()
                            .flat_map(|entry| {
                                crate::installed::commands(entry, &session.paths.profile)
                            })
                            .collect(),
                    },
                }
            }
            Self::Remove(targets) => Report {
                names: targets
                    .iter()
                    .filter(|key| !after.contains_key(*key))
                    .map(|key| package_id(session, &before[key]))
                    .collect(),
                completion: Completion::Packages {
                    verb: "Removed",
                    commands: BTreeSet::new(),
                },
            },
            Self::Upgrade(targets) => Report {
                names: targets
                    .iter()
                    .filter_map(|key| {
                        after
                            .get(key)
                            .filter(|entry| Some(*entry) != before.get(key))
                    })
                    .map(|entry| package_id(session, entry))
                    .collect(),
                completion: Completion::Packages {
                    verb: "Upgraded",
                    commands: BTreeSet::new(),
                },
            },
            Self::Rollback {
                generation,
                previous,
            } => {
                let current = lifecycle::active_generation(&session.paths.profile);
                let destination = current.or(*generation).map_or_else(
                    || String::from("the previous generation"),
                    |number| format!("generation {number}"),
                );
                let unchanged = current.is_some() && current == *previous;
                Report {
                    names: if unchanged {
                        Vec::new()
                    } else {
                        vec![destination.clone()]
                    },
                    completion: Completion::Rollback {
                        destination,
                        unchanged,
                    },
                }
            }
        }
    }
}

struct Report {
    names: Vec<String>,
    completion: Completion,
}

enum Completion {
    Packages {
        verb: &'static str,
        commands: BTreeSet<String>,
    },
    Rollback {
        destination: String,
        unchanged: bool,
    },
}

impl Report {
    fn render(self) {
        match self.completion {
            Completion::Packages { verb, commands } => {
                report_names(verb, &self.names);
                if !commands.is_empty() {
                    let shown = commands
                        .iter()
                        .take(10)
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let remaining = commands.len().saturating_sub(10);
                    if remaining == 0 {
                        println!("Commands: {shown}");
                    } else {
                        println!("Commands: {shown} (+{remaining} more)");
                    }
                }
            }
            Completion::Rollback {
                destination,
                unchanged,
            } => {
                if unchanged {
                    println!("Unchanged: {destination} is still active.");
                } else {
                    println!("Restored: {destination}.");
                }
            }
        }
    }
}

fn failure_exit(error: &NixError) -> ExitCode {
    match error {
        NixError::Interrupted { signal, .. } => super::error::signal_exit(*signal),
        _ => ExitCode::from(super::FAILURE),
    }
}

fn report_postflight_failure(action: &str, error: &NixError) -> CommandError {
    eprintln!("Partial: native {action} completed. The active profile could not be read.");
    if !matches!(error, NixError::Interrupted { .. }) {
        super::report_cause(&error.to_string());
    }
    eprintln!("Next: run `pkg list` to inspect the active profile.");
    CommandError::Reported(failure_exit(error))
}

fn report_rollback_target(
    session: &Session,
    generation: Option<u64>,
    previous: Option<u64>,
) -> Result<(), CommandError> {
    if let Some(number) = generation {
        eprintln!("Restoring: generation {number}");
    } else {
        let destination = optional_history(session)?.and_then(|text| {
            let current = previous?;
            history_numbers(&text)
                .into_iter()
                .filter(|number| *number < current)
                .max()
        });
        if let Some(number) = destination {
            eprintln!("Restoring: generation {number} (native previous generation)");
        } else {
            eprintln!("Restoring: native previous generation");
        }
    }
    Ok(())
}

fn prune(session: &Session, days: u64) -> Result<(), CommandError> {
    let before = optional_history(session)?.map(|text| history_numbers(&text));
    eprintln!("Pruning: generations older than {days} days");
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| super::mutation_failed(session, "prune", &error, None, None))?;
    let after = optional_history(session)
        .map_err(|error| {
            eprintln!("Partial: native prune completed. The generation history could not be read.");
            eprintln!("Next: run `pkg history` to inspect generations.");
            CommandError::Reported(failure_exit(&error))
        })?
        .map(|text| history_numbers(&text));
    match (before, after) {
        (Some(before), Some(after)) if !before.is_empty() && !after.is_empty() => {
            let removed = before.difference(&after).count();
            if removed == 0 {
                println!("No old generations to prune.");
            } else {
                println!("Pruned: {removed} old generation(s). Current generation kept.");
            }
        }
        _ => println!("Prune complete: generations older than {days} days were checked."),
    }
    Ok(())
}

fn prepare_upgrade(
    session: &Session,
    installed: &BTreeMap<String, ProfileEntry>,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<Option<Prepared>, CommandError> {
    // `--all` combined with entry names is a usage error rejected by the
    // parser; here the target set is either explicit names or everything.
    let targets: Vec<String> = if all {
        installed
            .iter()
            .filter(|(_, entry)| entry.active)
            .map(|(key, _)| key.clone())
            .collect()
    } else {
        if entries.is_empty() {
            return Err(String::from("upgrade needs entry IDs or --all").into());
        }
        crate::installed::select(
            installed,
            &session.config,
            &session.paths.state_home,
            entries,
            exact_entry,
        )?
    };
    if all && targets.is_empty() {
        println!("No installed entries; nothing to upgrade.");
        return Ok(None);
    }
    // Entries installed from a tap that has since been removed keep their
    // locked outputs; an upgrade must never follow a reference the registry
    // no longer holds. The local public source of an entry is identified
    // solely from its original URL through the tap store - the attribute
    // path is never parsed - so this works even when saved catalogs or the
    // profile metadata are broken, and it works for a removed entry too.
    // Only the registry is loaded here, never `TapState` or saved
    // catalogs, so unrelated broken metadata cannot block the decision.
    let registry = registry::Registry::load(&registry::registry_path(&session.paths.state_home))?;
    let removed_source = |entry: &str| -> Option<String> {
        let source =
            store::source_of_reference(&session.paths.state_home, &installed[entry].original_url)?;
        (source != nix::OFFICIAL_CASK_SOURCE && registry.origin(&source).is_none())
            .then_some(source)
    };
    let targets = if all {
        let mut remaining = targets;
        remaining.retain(|entry| match removed_source(entry) {
            None => true,
            Some(source) => {
                println!("Skipped: {entry} (tap {source} was removed; keeping locked outputs).");
                false
            }
        });
        if remaining.is_empty() {
            println!("Nothing eligible to upgrade.");
            return Ok(None);
        }
        remaining
    } else {
        for entry in &targets {
            if let Some(source) = removed_source(entry) {
                return Err(format!(
                    "pkg: {entry}: tap {source} is removed; upgrade refuses to follow it"
                )
                .into());
            }
        }
        targets
    };
    // Only explicit full-commit references count as fixed; tags, branches,
    // and hex-looking branch names stay movable (design D4). Fixed entries
    // are reported explicitly in both modes.
    let mut movable: Vec<String> = Vec::new();
    for entry in &targets {
        if nix::is_fixed_reference(installed[entry].original_url.as_str()) {
            println!("Skipped: {entry} (fixed source reference).");
        } else {
            movable.push(entry.clone());
        }
    }
    if !all && movable.is_empty() {
        println!("Unchanged: selected entries use fixed source references.");
        return Ok(None);
    }
    if movable.is_empty() {
        println!("Unchanged: no movable entries to upgrade.");
        return Ok(None);
    }
    // The remaining IDs are passed explicitly so fixed sources and removed
    // taps are never followed.
    //
    // Each movable CASK entry is re-gated against its OWN current
    // catalog record before `profile_upgrade`: the installed source is
    // the entry's original URL — saved taps through the verified tap
    // store, every other casks source loaded directly from that URL —
    // and the cask identity is confirmed as the whole native attribute
    // through `catalog::native_package_attribute`. Today's configured
    // casks source never judges an entry installed from another one;
    // Nixpkgs entries are never gated as casks. Unsupported, excluded,
    // and unknown targets are gated with the install policy.
    lifecycle::gate_upgrade_formulae(session, installed, &movable)?;
    lifecycle::gate_upgrade_macos_range(session, installed, &movable)?;
    eprintln!(
        "Upgrading: {}",
        selected_names(session, installed, &movable).join(", ")
    );
    Ok(Some(Prepared::Upgrade(movable)))
}

fn already_installed(
    nix: &nix::Nix,
    installed: &BTreeMap<String, ProfileEntry>,
    installable: &str,
    system: &str,
) -> Result<bool, nix::NixError> {
    let Some((reference, attribute)) = installable.split_once('#') else {
        return Ok(false);
    };
    let attribute = attribute
        .split_once('^')
        .map_or(attribute, |(base, _)| base)
        .replace('"', "");
    let candidates: Vec<_> = installed
        .values()
        .filter(|entry| {
            entry.active
                && entry.original_url == reference
                && (entry.attr_path == attribute
                    || catalog::exposed_attribute(&entry.attr_path, system) == attribute)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(false);
    }
    let selected = installable
        .split_once('^')
        .map_or(installable, |(base, _)| base);
    let exact = nix.search(selected, ".")?;
    if exact.len() != 1 {
        return Ok(false);
    }
    let mut paths = nix.default_output_paths(installable)?;
    paths.sort();
    Ok(candidates.iter().any(|entry| {
        let mut existing = entry.store_paths.clone();
        existing.sort();
        exact.contains_key(&entry.attr_path) && existing == paths
    }))
}

fn package_id(session: &super::Session, entry: &ProfileEntry) -> String {
    crate::installed::package_id(entry, &session.config, &session.paths.state_home)
}

fn report_names(verb: &str, names: &[String]) {
    match names {
        [] => println!("Unchanged: profile did not change."),
        [one] => println!("{verb}: {one}"),
        many => println!("{verb}: {}", many.join(", ")),
    }
}

fn optional_history(session: &super::Session) -> Result<Option<String>, NixError> {
    match session.nix.profile_history(&session.paths.profile) {
        Ok(text) => Ok(Some(text)),
        Err(error @ nix::NixError::Interrupted { .. }) => Err(error),
        Err(_) => Ok(None),
    }
}

fn history_numbers(text: &str) -> std::collections::BTreeSet<u64> {
    crate::output::strip_ansi(text)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Version ")?
                .split_once(" (")?
                .0
                .parse()
                .ok()
        })
        .collect()
}
