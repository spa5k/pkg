use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::process::ExitCode;

use crate::catalog;
use crate::nix::{self, NixError, ProfileEntry};

use super::query::{active_generation, optional_history};
use super::{CommandError, Session, installed_entries};

pub(super) enum Request<'a> {
    Profile(ProfileChange<'a>),
    Prune { days: u64 },
}

pub(super) enum ProfileChange<'a> {
    Install {
        installables: IndexMap<String, String>,
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
}

#[derive(strum_macros::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum Prepared {
    Install {
        targets: Vec<String>,
        requested: Vec<String>,
    },
    Remove(Vec<String>),
    Upgrade(UpgradeTargets),
    Rollback {
        generation: Option<u64>,
        previous: Option<u64>,
    },
}

pub(super) fn execute(session: &Session, request: Request<'_>) -> Result<(), CommandError> {
    let profile = super::LockedProfile::acquire(&session.nix, &session.paths)?;
    let request = match request {
        Request::Prune { days } => return prune(session, days),
        Request::Profile(request) => request,
    };
    // A broken current generation blocks every change except the rollback
    // that repairs it.
    let state = if matches!(request, ProfileChange::Rollback(_))
        && super::query::broken_profile(&session.paths.profile).is_some()
    {
        None
    } else {
        super::query::installed_state(session)?
    };
    let empty = BTreeMap::new();
    let before = state.as_ref().map_or(&empty, nix::CheckedProfile::entries);
    let Some(prepared) = prepare(session, request, before)? else {
        return Ok(());
    };
    if let Err(error) = prepared.apply(session, state.as_ref()) {
        return Err(prepared.failed(session, before, &error));
    }
    let after = installed_entries(session)
        .map_err(|error| report_postflight_failure(prepared.action(), &error))?;
    let report = prepared.completed(session, before, &after);
    let apps = crate::apps::refresh_after_profile_change(
        &profile,
        &after,
        prepared.action(),
        &report.names,
    )?;
    print!("{}{apps}", report.text);
    Ok(())
}

fn prepare(
    session: &Session,
    request: ProfileChange<'_>,
    before: &BTreeMap<String, ProfileEntry>,
) -> Result<Option<Prepared>, CommandError> {
    match request {
        ProfileChange::Install {
            installables,
            system,
        } => prepare_install(session, before, installables, system),
        ProfileChange::Remove {
            entries,
            exact_entry,
        } => {
            let targets = super::installed::select(
                before,
                &session.config,
                &session.paths.state_home,
                entries,
                exact_entry,
            )?;
            eprintln!(
                "Removing: {}",
                display_names(session, before, &targets).join(", ")
            );
            Ok(Some(Prepared::Remove(targets)))
        }
        ProfileChange::Upgrade {
            entries,
            all,
            exact_entry,
        } => prepare_upgrade(session, before, entries, all, exact_entry),
        ProfileChange::Rollback(generation) => {
            let generation = match generation {
                Some(number) => Some(number),
                None => match super::query::undo_target(&session.paths.profile) {
                    super::query::UndoTarget::Generation(number) => Some(number),
                    super::query::UndoTarget::Nothing => {
                        if super::query::generation_numbers(&session.paths.profile).len() < 2 {
                            println!("Nothing to undo yet.");
                        } else {
                            println!("Nothing to undo. Older generations have the same packages.");
                        }
                        return Ok(None);
                    }
                    super::query::UndoTarget::Unknown
                        if super::query::generation_numbers(&session.paths.profile).len() < 2
                            && super::query::broken_profile(&session.paths.profile).is_none() =>
                    {
                        println!("Nothing to undo yet.");
                        return Ok(None);
                    }
                    super::query::UndoTarget::Unknown => None,
                },
            };
            let previous = active_generation(&session.paths.profile);
            if let Some(number) = generation {
                if previous == Some(number) {
                    println!("Generation {number} is already active. Nothing changed.");
                    return Ok(None);
                }
                let known = super::query::generation_numbers(&session.paths.profile);
                if !known.is_empty() && !known.contains(&number) {
                    return Err(format!(
                        "generation {number} does not exist. Run `pkg history` to see the generations."
                    )
                    .into());
                }
            }
            report_rollback_target(session, generation, previous)?;
            Ok(Some(Prepared::Rollback {
                generation,
                previous,
            }))
        }
    }
}

fn prepare_install(
    session: &Session,
    before: &BTreeMap<String, ProfileEntry>,
    installables: IndexMap<String, String>,
    system: &str,
) -> Result<Option<Prepared>, CommandError> {
    let checked = batch_installed(&session.nix, before, &installables, system)?;
    let mut targets = Vec::new();
    for (installable, name) in installables {
        let identical = match checked.get(&installable) {
            Some(&identical) => identical,
            None => already_installed(&session.nix, before, &installable, system)?,
        };
        // An active entry from the same source and attribute is the same
        // package, even when the source moved since it was installed. A
        // second entry would make `pkg remove NAME` ambiguous; an upgrade
        // updates the existing entry instead.
        match installed_entry(before, &installable, system) {
            Some(entry) if identical || !installable.contains('^') => {
                let package = super::summary::Package::of(entry, display_name(session, entry));
                println!("Already installed: {}", package.label());
                if !identical {
                    println!(
                        "A newer build is available. To get it, run `{}`.",
                        crate::output::paint(
                            crate::output::Stream::Out,
                            crate::output::Tone::Accent,
                            &format!("pkg upgrade {}", package.name)
                        )
                    );
                }
            }
            _ if identical => {
                println!(
                    "Already installed: {}",
                    crate::output::short_package_name(&name)
                );
            }
            _ => match same_build(&session.nix, before, &installable)? {
                Some(entry) => {
                    let package = super::summary::Package::of(entry, display_name(session, entry));
                    println!("Already installed: {} (the same build)", package.label());
                }
                None => targets.push((installable, name)),
            },
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let (targets, requested): (Vec<_>, Vec<_>) = targets.into_iter().unzip();
    eprintln!(
        "Installing: {}",
        requested
            .iter()
            .map(|name| crate::output::short_package_name(name))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(Some(Prepared::Install { targets, requested }))
}

/// The short names of the selected entries, without repeats.
fn display_names(
    session: &Session,
    entries: &BTreeMap<String, ProfileEntry>,
    targets: &[String],
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for key in targets {
        let name = display_name(session, &entries[key]);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.sort();
    names
}

impl Prepared {
    fn action(&self) -> &'static str {
        self.into()
    }

    fn apply(
        &self,
        session: &Session,
        state: Option<&nix::CheckedProfile<'_>>,
    ) -> Result<(), NixError> {
        let profile = &session.paths.profile;
        match self {
            Self::Install { targets, .. } => state.map_or_else(
                || session.nix.profile_add(profile, targets),
                |state| state.add(targets),
            ),
            Self::Remove(targets) => session.nix.profile_remove(profile, targets),
            Self::Upgrade(targets) => session
                .nix
                .profile_upgrade(profile, Some(targets.as_slice())),
            Self::Rollback { generation, .. } => state.map_or_else(
                || session.nix.profile_rollback(profile, *generation),
                |state| state.rollback(*generation),
            ),
        }
    }

    fn failed(
        &self,
        session: &Session,
        before: &BTreeMap<String, ProfileEntry>,
        error: &NixError,
    ) -> CommandError {
        if let Self::Rollback { previous, .. } = self
            && let Some(number) = active_generation(&session.paths.profile)
                .filter(|number| Some(*number) != *previous)
        {
            eprintln!("Partial: generation {number} is active. App launchers may need an update.");
            super::report_native_cause(error);
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
        let changes = super::summary::diff(before, after, |entry| display_name(session, entry));
        if let Self::Rollback {
            generation,
            previous,
        } = self
        {
            return rolled_back(session, *generation, *previous, &changes);
        }
        let keys: Vec<&String> = match self {
            Self::Install { .. } => after
                .iter()
                .filter(|(key, entry)| before.get(*key) != Some(*entry))
                .map(|(key, _)| key)
                .collect(),
            Self::Remove(targets) => targets
                .iter()
                .filter(|key| !after.contains_key(*key))
                .collect(),
            Self::Upgrade(targets) => targets
                .as_slice()
                .iter()
                .filter(|key| {
                    after
                        .get(*key)
                        .is_some_and(|entry| Some(entry) != before.get(*key))
                })
                .collect(),
            Self::Rollback { .. } => Vec::new(),
        };
        let touched: Vec<&ProfileEntry> = keys
            .iter()
            .filter_map(|key| after.get(*key).or_else(|| before.get(*key)))
            .collect();
        let names: Vec<_> = touched
            .iter()
            .map(|entry| package_id(session, entry))
            .collect();
        let verbs = match self {
            Self::Install { .. } => ["Installed", "Removed", "Upgraded"],
            Self::Remove(_) => ["Added", "Removed", "Changed"],
            _ => ["Added", "Removed", "Upgraded"],
        };
        let mut text = String::new();
        for change in &changes {
            let _ = writeln!(text, "{}", change.line(verbs));
        }
        if text.is_empty() {
            text = match self {
                Self::Upgrade(targets) if targets.all => {
                    String::from("Everything is up to date.\n")
                }
                Self::Upgrade(targets) => {
                    let current: Vec<String> = targets
                        .as_slice()
                        .iter()
                        .filter_map(|key| before.get(key))
                        .map(|entry| {
                            super::summary::Package::of(entry, display_name(session, entry)).label()
                        })
                        .collect();
                    format!("Up to date: {}\n", current.join(", "))
                }
                _ => String::from("Nothing changed.\n"),
            };
        }
        if matches!(self, Self::Install { .. }) {
            text.push_str(&commands_text(session, &touched));
        }
        Report { names, text }
    }
}

/// Report a completed rollback and the package changes it made.
fn rolled_back(
    session: &Session,
    generation: Option<u64>,
    previous: Option<u64>,
    changes: &[super::summary::Change],
) -> Report {
    let current = active_generation(&session.paths.profile);
    let destination = current.or(generation).map_or_else(
        || String::from("the previous generation"),
        |number| format!("generation {number}"),
    );
    if current.is_some() && current == previous {
        return Report {
            names: Vec::new(),
            text: format!(
                "Nothing changed. {} is still active.\n",
                capitalize(&destination)
            ),
        };
    }
    let mut text = format!(
        "{} to {destination}.\n",
        crate::output::paint(
            crate::output::Stream::Out,
            crate::output::Tone::Success,
            "Rolled back"
        )
    );
    for change in changes {
        let _ = writeln!(text, "{}", change.line(["Added", "Removed", "Changed"]));
    }
    if changes.is_empty() {
        text.push_str("No package changes.\n");
    }
    // A repair from a missing generation has no way back.
    if let Some(number) = previous
        && super::query::generation_numbers(&session.paths.profile).contains(&number)
    {
        let _ = writeln!(text, "To go back, run `pkg rollback {number}`.");
    }
    Report {
        names: vec![destination],
        text,
    }
}

/// The commands that new entries add, and a hint when the profile is not
/// on `PATH` yet.
fn commands_text(session: &Session, entries: &[&ProfileEntry]) -> String {
    let mut text = String::new();
    let commands: BTreeSet<_> = entries
        .iter()
        .flat_map(|entry| super::installed::commands(entry, &session.paths.profile))
        .collect();
    if commands.is_empty() {
        // Apps open from the Applications folder. Other packages without
        // commands are often libraries that a profile cannot use.
        let apps = entries.iter().any(|entry| {
            entry
                .store_paths
                .iter()
                .any(|path| std::path::Path::new(path).join("Applications").is_dir())
        });
        if !apps && !entries.is_empty() {
            text.push_str("This package adds no commands.\n");
        }
        return text;
    }
    let shown = commands
        .iter()
        .take(10)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = commands.len().saturating_sub(10);
    if remaining == 0 {
        let _ = writeln!(text, "Commands: {shown}");
    } else {
        let _ = writeln!(text, "Commands: {shown} (+{remaining} more)");
    }
    if !crate::output::on_path(&session.paths.profile.join("bin")) {
        let _ = writeln!(text, "Open a new terminal window to use these commands.");
    }
    text
}

/// `generation 3` becomes `Generation 3`.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

struct Report {
    names: Vec<String>,
    text: String,
}

fn failure_exit(error: &NixError) -> ExitCode {
    error
        .signal()
        .map_or_else(|| ExitCode::from(super::FAILURE), super::error::signal_exit)
}

fn report_postflight_failure(action: &str, error: &NixError) -> CommandError {
    let (state, next, inspect) = if action == "prune" {
        ("generation history", "history", "generations")
    } else {
        ("active profile", "list", "the active profile")
    };
    eprintln!("Partial: native {action} completed. The {state} could not be read.");
    super::report_native_cause(error);
    eprintln!("Next: run `pkg {next}` to inspect {inspect}.");
    CommandError::Reported(failure_exit(error))
}

fn report_rollback_target(
    session: &Session,
    generation: Option<u64>,
    previous: Option<u64>,
) -> Result<(), CommandError> {
    if let Some(number) = generation {
        eprintln!("Rolling back to generation {number}");
    } else {
        let destination = optional_history(session)?
            .and_then(|generations| generations.range(..previous?).next_back().copied());
        if let Some(number) = destination {
            eprintln!("Rolling back to generation {number}");
        } else {
            eprintln!("Rolling back to the previous generation");
        }
    }
    Ok(())
}

fn prune(session: &Session, days: u64) -> Result<(), CommandError> {
    let before = optional_history(session)?;
    eprintln!(
        "Pruning: generations older than {days} {}",
        if days == 1 { "day" } else { "days" }
    );
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| super::mutation_failed(session, "prune", &error, None, None))?;
    let after =
        optional_history(session).map_err(|error| report_postflight_failure("prune", &error))?;
    match (before, after) {
        (Some(before), Some(after)) => match before.difference(&after).count() {
            0 => println!("No old generations to prune."),
            1 => println!("Pruned 1 old generation. The current generation stays."),
            removed => {
                println!("Pruned {removed} old generations. The current generation stays.");
            }
        },
        _ => println!("Prune complete. Run `pkg history` to see the generations."),
    }
    Ok(())
}

struct UpgradeTargets {
    ids: Vec<String>,
    /// Whether the user asked for all packages.
    all: bool,
}

impl UpgradeTargets {
    fn nonempty(ids: Vec<String>, all: bool) -> Option<Self> {
        (!ids.is_empty()).then_some(Self { ids, all })
    }

    fn as_slice(&self) -> &[String] {
        &self.ids
    }
}

fn prepare_upgrade(
    session: &Session,
    installed: &BTreeMap<String, ProfileEntry>,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<Option<Prepared>, CommandError> {
    // `pkg upgrade` without names upgrades all packages.
    let all = all || entries.is_empty();
    let selected: Vec<String> = if all {
        installed
            .iter()
            .filter(|(_, entry)| entry.active)
            .map(|(key, _)| key.clone())
            .collect()
    } else {
        super::installed::select(
            installed,
            &session.config,
            &session.paths.state_home,
            entries,
            exact_entry,
        )?
    };
    if all && selected.is_empty() {
        println!("No packages installed.");
        return Ok(None);
    }
    let taps = crate::tap::Catalogs::load(&session.paths.state_home)?;
    let mut retained = Vec::new();
    for entry in selected {
        let removed_source = taps.withdrawn_source(&installed[&entry].original_url);
        if let Some(source) = removed_source {
            let name = display_name(session, &installed[&entry]);
            if !all {
                return Err(format!(
                    "pkg: {name}: tap {source} is removed; upgrade refuses to follow it"
                )
                .into());
            }
            println!("Skipped: {name} (tap {source} was removed; the installed version stays).");
        } else {
            retained.push(entry);
        }
    }
    if all && retained.is_empty() {
        println!("Nothing to upgrade.");
        return Ok(None);
    }
    let mut movable = Vec::new();
    for entry in retained {
        if nix::is_fixed_reference(&installed[&entry].original_url) {
            println!(
                "Skipped: {} (fixed source revision).",
                display_name(session, &installed[&entry])
            );
        } else {
            movable.push(entry);
        }
    }
    let Some(targets) = UpgradeTargets::nonempty(movable, all) else {
        if all {
            println!("Nothing to upgrade.");
        } else {
            println!("Nothing to upgrade. The selected packages use a fixed source revision.");
        }
        return Ok(None);
    };
    crate::discovery::gate_upgrade(session, installed, targets.as_slice())?;
    eprintln!(
        "Upgrading: {}",
        display_names(session, installed, targets.as_slice()).join(", ")
    );
    Ok(Some(Prepared::Upgrade(targets)))
}

fn matching_entry(entry: &ProfileEntry, reference: &str, attribute: &str, system: &str) -> bool {
    entry.active
        && entry.original_url == reference
        && (entry.attr_path == attribute
            || catalog::exposed_attribute(&entry.attr_path, system) == attribute)
}

fn batch_installed(
    nix: &nix::Nix,
    installed: &BTreeMap<String, ProfileEntry>,
    installables: &IndexMap<String, String>,
    system: &str,
) -> Result<BTreeMap<String, bool>, NixError> {
    let mut groups: BTreeMap<&str, Vec<(&str, Vec<String>)>> = BTreeMap::new();
    for installable in installables.keys() {
        let Some((reference, attribute)) = installable.split_once('#') else {
            continue;
        };
        let plain = attribute.split('.').all(|segment| {
            let mut bytes = segment.bytes();
            bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
        if !plain
            || !installed
                .values()
                .any(|entry| matching_entry(entry, reference, attribute, system))
        {
            continue;
        }
        groups.entry(reference).or_default().push((
            installable,
            attribute.split('.').map(String::from).collect(),
        ));
    }
    let mut checked = BTreeMap::new();
    for (reference, group) in groups {
        if group.len() < 2 {
            continue;
        }
        let (installables, requests): (Vec<_>, Vec<_>) = group.into_iter().unzip();
        let rows = match nix.install_outputs(reference, system, &requests) {
            Ok(rows) => rows,
            Err(NixError::Failed { .. } | NixError::ResourceLimit(_)) => continue,
            Err(error) => return Err(error),
        };
        for (installable, row) in installables.into_iter().zip(rows) {
            let Some(mut row) = row else { continue };
            row.paths.sort();
            let identical = installed.values().any(|entry| {
                if !entry.active
                    || entry.original_url != reference
                    || entry.attr_path != row.attribute
                {
                    return false;
                }
                let mut paths = entry.store_paths.clone();
                paths.sort();
                paths == row.paths
            });
            checked.insert(installable.to_string(), identical);
        }
    }
    Ok(checked)
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
        .filter(|entry| matching_entry(entry, reference, &attribute, system))
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

/// The active entry with the same build as `installable`, for example the
/// same package through another flake reference or output name.
///
/// The check costs one evaluation, so it runs only when an active entry
/// has the name of an attribute segment.
fn same_build<'a>(
    nix: &nix::Nix,
    installed: &'a BTreeMap<String, ProfileEntry>,
    installable: &str,
) -> Result<Option<&'a ProfileEntry>, NixError> {
    let Some((_, attribute)) = installable.split_once('#') else {
        return Ok(None);
    };
    let segments: Vec<&str> = attribute
        .split(['.', '^'])
        .map(|segment| segment.trim_matches('"'))
        .collect();
    let candidates: Vec<_> = installed
        .values()
        .filter(|entry| entry.active && segments.contains(&entry.name().as_str()))
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut paths = match nix.default_output_paths(installable) {
        Ok(paths) => paths,
        // The install reports the cause.
        Err(NixError::Failed { .. } | NixError::ResourceLimit(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    paths.sort();
    Ok(candidates.into_iter().find(|entry| {
        let mut existing = entry.store_paths.clone();
        existing.sort();
        existing == paths
    }))
}

fn package_id(session: &super::Session, entry: &ProfileEntry) -> String {
    super::installed::package_id(entry, &session.config, &session.paths.state_home)
}

fn display_name(session: &super::Session, entry: &ProfileEntry) -> String {
    super::installed::display_name(entry, &session.config, &session.paths.state_home)
}

/// The active entry installed from the same source and attribute as
/// `installable`, ignoring any output selection.
fn installed_entry<'a>(
    installed: &'a BTreeMap<String, ProfileEntry>,
    installable: &str,
    system: &str,
) -> Option<&'a ProfileEntry> {
    let (reference, attribute) = installable.split_once('#')?;
    let attribute = attribute
        .split_once('^')
        .map_or(attribute, |(base, _)| base)
        .replace('"', "");
    installed
        .values()
        .find(|entry| matching_entry(entry, reference, &attribute, system))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_action_names_remain_stable() {
        for (prepared, expected) in [
            (
                Prepared::Install {
                    targets: vec![String::from("nixpkgs:hello")],
                    requested: vec![String::from("hello")],
                },
                "install",
            ),
            (Prepared::Remove(vec![String::from("entry")]), "remove"),
            (
                Prepared::Upgrade(UpgradeTargets {
                    ids: vec![String::from("entry")],
                    all: false,
                }),
                "upgrade",
            ),
            (
                Prepared::Rollback {
                    generation: Some(2),
                    previous: Some(3),
                },
                "rollback",
            ),
        ] {
            assert_eq!(prepared.action(), expected);
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "isolated reporting fixtures fail directly when setup is unavailable"
)]
mod reporting_tests {
    use super::super::mutation_failed;
    use super::*;
    use crate::commands::finish;
    use crate::config::{Config, Paths};
    use std::os::unix::fs::PermissionsExt as _;

    const SIGNAL: &str = "PKG_MUTATION_REPORT_SIGNAL";
    const STAGE: &str = "PKG_MUTATION_REPORT_STAGE";
    const PURE: &str = "PKG_MUTATION_REPORT_PURE";

    #[test]
    fn cancelled_supervision_report_child() {
        let Ok(signal) = std::env::var(SIGNAL) else {
            return;
        };
        let signal = signal.parse::<i32>().expect("cancellation signal");
        let home = tempfile::tempdir().expect("isolated home");
        let paths = Paths::from_bases(
            &home.path().join("state"),
            &home.path().join("config"),
            &home.path().join("cache"),
        );
        let runtime = home.path().join("nix");
        std::fs::write(
            &runtime,
            "#!/bin/sh\ncase \"$*\" in\n *--version*) echo 'nix (Nix) 2.35.2';;\n *'profile list'*) echo '{\"version\":3,\"elements\":{}}';;\n *) exit 9;;\nesac\n",
        )
        .expect("fixture runtime");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755))
            .expect("executable runtime");
        let session = Session {
            nix: nix::discover(runtime.to_str()).expect("discover fixture runtime"),
            config: Config::default(),
            paths,
        };
        let _lock = crate::profile_lock::ProfileLock::acquire(&session.paths.profile)
            .expect("profile lock");
        std::fs::write(&session.paths.profile, "profile").expect("native profile");
        let failure = if std::env::var_os(PURE).is_some() {
            NixError::Interrupted {
                args: vec![String::from("fixture")],
                signal,
            }
        } else {
            NixError::Process(pkg_process::RunError {
                cancelled: Some(signal),
                source: std::io::Error::other("fixture supervisor pipe read failed"),
            })
        };
        let outcome = match std::env::var(STAGE).expect("report stage").as_str() {
            "mutation" => {
                mutation_failed(&session, "install", &failure, Some(&BTreeMap::new()), None)
            }
            "postflight" => report_postflight_failure("install", &failure),
            "prune" => report_postflight_failure("prune", &failure),
            "rollback" => {
                std::fs::remove_file(&session.paths.profile).expect("remove fixture profile");
                std::os::unix::fs::symlink("pkg-2-link", &session.paths.profile)
                    .expect("active generation");
                Prepared::Rollback {
                    generation: Some(2),
                    previous: Some(1),
                }
                .failed(&session, &BTreeMap::new(), &failure)
            }
            stage => panic!("unknown fixture stage {stage}"),
        };
        assert_eq!(
            finish(Err(outcome)),
            super::super::error::signal_exit(signal)
        );
    }

    #[test]
    fn cancelled_supervision_report_keeps_its_io_cause() {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            for stage in ["mutation", "postflight", "prune", "rollback"] {
                for pure in [false, true] {
                    let mut child = std::process::Command::new(
                        std::env::current_exe().expect("test executable"),
                    );
                    child
                        .args([
                            "--exact",
                            "profile::change::reporting_tests::cancelled_supervision_report_child",
                            "--nocapture",
                        ])
                        .env(SIGNAL, signal.to_string())
                        .env(STAGE, stage)
                        .env_remove(PURE);
                    if pure {
                        child.env(PURE, "1");
                    }
                    let output = child.output().expect("isolated report process");
                    assert!(
                        output.status.success(),
                        "{signal}/{stage}/{pure}: {output:?}"
                    );
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let heading = match stage {
                        "mutation" => {
                            "Interrupted: install stopped.\nInstalled entries unchanged.\n"
                        }
                        "postflight" => {
                            "Partial: native install completed. The active profile could not be read.\n"
                        }
                        "prune" => {
                            "Partial: native prune completed. The generation history could not be read.\n"
                        }
                        "rollback" => {
                            "Partial: generation 2 is active. App launchers may need an update.\n"
                        }
                        _ => unreachable!(),
                    };
                    assert!(
                        stderr.contains(heading),
                        "{signal}/{stage}/{pure}: {stderr}"
                    );
                    if pure {
                        assert!(
                            !stderr.contains("Cause:"),
                            "pure interruptions stay concise: {stderr}"
                        );
                        assert!(!stderr.contains("native command `nix fixture`"), "{stderr}");
                    } else {
                        assert!(stderr.contains("Cause: could not supervise the Nix command: fixture supervisor pipe read failed.\n"), "{signal}/{stage}: {stderr}");
                    }
                }
            }
        }
    }
}
