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
    let state = super::query::installed_state(session)?;
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
    crate::apps::refresh_after_profile_change(&profile, &after, prepared.action(), &report.names)?;
    print!("{}", report.text);
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
                selected_names(session, before, &targets).join(", ")
            );
            Ok(Some(Prepared::Remove(targets)))
        }
        ProfileChange::Upgrade {
            entries,
            all,
            exact_entry,
        } => prepare_upgrade(session, before, entries, all, exact_entry),
        ProfileChange::Rollback(generation) => {
            let previous = active_generation(&session.paths.profile);
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
        let installed = match checked.get(&installable) {
            Some(&installed) => installed,
            None => already_installed(&session.nix, before, &installable, system)?,
        };
        if installed {
            println!("Already installed: {name}");
        } else {
            targets.push((installable, name));
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let (targets, requested): (Vec<_>, Vec<_>) = targets.into_iter().unzip();
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
        let (verb, changed): (&str, Vec<&ProfileEntry>) = match self {
            Self::Install { .. } => (
                "Installed",
                after
                    .iter()
                    .filter(|(key, entry)| before.get(*key) != Some(*entry))
                    .map(|(_, entry)| entry)
                    .collect(),
            ),
            Self::Remove(targets) => (
                "Removed",
                targets
                    .iter()
                    .filter(|key| !after.contains_key(*key))
                    .map(|key| &before[key])
                    .collect(),
            ),
            Self::Upgrade(targets) => (
                "Upgraded",
                targets
                    .as_slice()
                    .iter()
                    .filter_map(|key| {
                        after
                            .get(key)
                            .filter(|entry| Some(*entry) != before.get(key))
                    })
                    .collect(),
            ),
            Self::Rollback {
                generation,
                previous,
            } => {
                let current = active_generation(&session.paths.profile);
                let destination = current.or(*generation).map_or_else(
                    || String::from("the previous generation"),
                    |number| format!("generation {number}"),
                );
                let unchanged = current.is_some() && current == *previous;
                return Report {
                    names: if unchanged {
                        Vec::new()
                    } else {
                        vec![destination.clone()]
                    },
                    text: if unchanged {
                        format!("Unchanged: {destination} is still active.\n")
                    } else {
                        format!("Restored: {destination}.\n")
                    },
                };
            }
        };
        let names: Vec<_> = changed
            .iter()
            .map(|entry| package_id(session, entry))
            .collect();
        let mut text = if names.is_empty() {
            String::from("Unchanged: profile did not change.\n")
        } else {
            format!("{verb}: {}\n", names.join(", "))
        };
        if matches!(self, Self::Install { .. }) {
            let commands: BTreeSet<_> = changed
                .iter()
                .flat_map(|entry| super::installed::commands(entry, &session.paths.profile))
                .collect();
            if !commands.is_empty() {
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
            }
        }
        Report { names, text }
    }
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
        eprintln!("Restoring: generation {number}");
    } else {
        let destination = optional_history(session)?
            .and_then(|generations| generations.range(..previous?).next_back().copied());
        if let Some(number) = destination {
            eprintln!("Restoring: generation {number} (native previous generation)");
        } else {
            eprintln!("Restoring: native previous generation");
        }
    }
    Ok(())
}

fn prune(session: &Session, days: u64) -> Result<(), CommandError> {
    let before = optional_history(session)?;
    eprintln!("Pruning: generations older than {days} days");
    session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
        .map_err(|error| super::mutation_failed(session, "prune", &error, None, None))?;
    let after =
        optional_history(session).map_err(|error| report_postflight_failure("prune", &error))?;
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

struct UpgradeTargets(Vec<String>);

impl UpgradeTargets {
    fn nonempty(ids: Vec<String>) -> Option<Self> {
        (!ids.is_empty()).then_some(Self(ids))
    }

    fn as_slice(&self) -> &[String] {
        &self.0
    }
}

fn prepare_upgrade(
    session: &Session,
    installed: &BTreeMap<String, ProfileEntry>,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<Option<Prepared>, CommandError> {
    let selected: Vec<String> = if all {
        installed
            .iter()
            .filter(|(_, entry)| entry.active)
            .map(|(key, _)| key.clone())
            .collect()
    } else {
        if entries.is_empty() {
            return Err(String::from("upgrade needs entry IDs or --all").into());
        }
        super::installed::select(
            installed,
            &session.config,
            &session.paths.state_home,
            entries,
            exact_entry,
        )?
    };
    if all && selected.is_empty() {
        println!("No installed entries; nothing to upgrade.");
        return Ok(None);
    }
    let taps = crate::tap::Catalogs::load(&session.paths.state_home)?;
    let mut retained = Vec::new();
    for entry in selected {
        let removed_source = taps.withdrawn_source(&installed[&entry].original_url);
        if let Some(source) = removed_source {
            if !all {
                return Err(format!(
                    "pkg: {entry}: tap {source} is removed; upgrade refuses to follow it"
                )
                .into());
            }
            println!("Skipped: {entry} (tap {source} was removed; keeping locked outputs).");
        } else {
            retained.push(entry);
        }
    }
    if all && retained.is_empty() {
        println!("Nothing eligible to upgrade.");
        return Ok(None);
    }
    let mut movable = Vec::new();
    for entry in retained {
        if nix::is_fixed_reference(&installed[&entry].original_url) {
            println!("Skipped: {entry} (fixed source reference).");
        } else {
            movable.push(entry);
        }
    }
    let Some(targets) = UpgradeTargets::nonempty(movable) else {
        if all {
            println!("Unchanged: no movable entries to upgrade.");
        } else {
            println!("Unchanged: selected entries use fixed source references.");
        }
        return Ok(None);
    };
    crate::discovery::gate_upgrade(session, installed, targets.as_slice())?;
    eprintln!(
        "Upgrading: {}",
        selected_names(session, installed, targets.as_slice()).join(", ")
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

fn package_id(session: &super::Session, entry: &ProfileEntry) -> String {
    super::installed::package_id(entry, &session.config, &session.paths.state_home)
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
                Prepared::Upgrade(UpgradeTargets(vec![String::from("entry")])),
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
                        assert!(stderr.contains("Cause: could not supervise the Nix command: fixture supervisor pipe read failed\n"), "{signal}/{stage}: {stderr}");
                    }
                }
            }
        }
    }
}
