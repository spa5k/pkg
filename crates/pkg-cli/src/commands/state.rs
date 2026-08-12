//! Offline active-state/history views and deterministic lifecycle edits.

use serde_json::{Map, Value, json};

use pkg_core::lifecycle::LifecycleState;
use pkg_core::remove::remove_selectors;
use pkg_core::{
    ChangeKind, GenerationSnapshot, History, NixpkgsRevision, PinAction, RollbackPlan,
    RollbackTarget, SelectorId, edit_pins, plan_rollback,
};

use crate::cli::{HistoryArgs, ListArgs, PackageArgs, RemoveArgs, RollbackArgs};
use crate::commands::execute::CommandResult;
use crate::exit::ExitCode;
use crate::ux::CommandError;

/// A validated lifecycle edit paired with its sanitized public preview/result.
#[derive(Debug)]
pub struct LifecycleEdit {
    state: LifecycleState,
    result: CommandResult,
}

/// Verified rollback plan paired with its sanitized public preview/result.
#[derive(Debug)]
pub struct RollbackEdit {
    plan: RollbackPlan,
    result: CommandResult,
}

impl RollbackEdit {
    /// Consumes the rollback edit into its private plan and public result.
    #[must_use]
    pub fn into_parts(self) -> (RollbackPlan, CommandResult) {
        (self.plan, self.result)
    }
}

impl LifecycleEdit {
    /// Pairs one validated lifecycle state with its public transaction result.
    #[must_use]
    pub(crate) const fn new(state: LifecycleState, result: CommandResult) -> Self {
        Self { state, result }
    }
    /// Coherent next manifest/lock state that still requires generation commit.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }
    /// Sanitized product result for preview or post-commit rendering.
    #[must_use]
    pub const fn result(&self) -> &CommandResult {
        &self.result
    }
    /// Consumes the edit into state and result parts.
    #[must_use]
    pub fn into_parts(self) -> (LifecycleState, CommandResult) {
        (self.state, self.result)
    }
}

/// Renders installed packages from one already-verified active lifecycle state.
pub fn list_state(
    state: &LifecycleState,
    args: &ListArgs,
    accepted_revision: Option<&NixpkgsRevision>,
) -> Result<CommandResult, CommandError> {
    let mut entries = Vec::new();
    for desired in state.manifest().entries() {
        if args.pinned() && !desired.is_pinned() {
            continue;
        }
        let locked = state
            .locked()
            .entries()
            .get(desired.id())
            .ok_or_else(state_error)?;
        let realization = locked.realization();
        let is_outdated =
            accepted_revision.is_some_and(|revision| revision != realization.nixpkgs_revision());
        if args.outdated() && !is_outdated {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("selector".into(), json!(desired.selector().as_str()));
        entry.insert("name".into(), json!(realization.pname()));
        if !args.name_only() {
            entry.insert("version".into(), json!(realization.version().as_str()));
            entry.insert("pinned".into(), json!(desired.is_pinned()));
            entry.insert(
                "sourceRevision".into(),
                json!(realization.nixpkgs_revision().as_str()),
            );
            if args.with_outputs() {
                entry.insert(
                    "outputsToInstall".into(),
                    json!(
                        realization
                            .outputs_to_install()
                            .iter()
                            .map(|output| output.as_str())
                            .collect::<Vec<_>>()
                    ),
                );
            }
            if args.size() {
                entry.insert("closureBytes".into(), json!(realization.closure_nar_size()));
            }
            if args.outdated() {
                entry.insert("outdated".into(), json!(is_outdated));
            }
        }
        entries.push(Value::Object(entry));
    }
    let records = row_records(&entries, "installed_package");
    result(
        format!("{} package(s) installed", entries.len()),
        Map::from_iter([("entries".into(), Value::Array(entries))]),
        records,
    )
}

/// Applies an atomic remove edit in memory; the caller commits it as a fresh generation.
pub fn remove_state(
    state: LifecycleState,
    args: &RemoveArgs,
) -> Result<LifecycleEdit, CommandError> {
    let targets = resolve_targets(&state, args.packages())?;
    let removed = remove_selectors(state, &targets).map_err(|_| {
        CommandError::new(
            ExitCode::ResolveFailed,
            "one or more packages are not installed",
            "run `pkg list` and use an installed selector",
        )
    })?;
    let names = removed
        .removed()
        .iter()
        .map(SelectorId::as_str)
        .collect::<Vec<_>>();
    let command_result = result(
        format!("{} package(s) ready to remove", names.len()),
        Map::from_iter([
            ("removed".into(), json!(names)),
            ("orphanCheckRequested".into(), json!(args.orphan_check())),
        ]),
        vec![],
    )?;
    Ok(LifecycleEdit {
        state: removed.into_state(),
        result: command_result,
    })
}

/// Applies an atomic pin/unpin edit in memory; the caller commits a fresh generation.
pub fn edit_pin_state(
    state: LifecycleState,
    args: &PackageArgs,
    action: PinAction,
) -> Result<LifecycleEdit, CommandError> {
    let targets = resolve_targets(&state, args.packages())?;
    let edited = edit_pins(state, &targets, action).map_err(|_| {
        CommandError::new(
            ExitCode::ResolveFailed,
            "one or more packages are not installed",
            "run `pkg list` and use an installed selector",
        )
    })?;
    let changed = edited
        .changed()
        .iter()
        .map(SelectorId::as_str)
        .collect::<Vec<_>>();
    let unchanged = edited
        .unchanged()
        .iter()
        .map(SelectorId::as_str)
        .collect::<Vec<_>>();
    let verb = match action {
        PinAction::Pin => "pinned",
        PinAction::Unpin => "unpinned",
    };
    let command_result = result(
        format!("{} package(s) {verb}", changed.len()),
        Map::from_iter([
            ("changed".into(), json!(changed)),
            ("unchanged".into(), json!(unchanged)),
        ]),
        vec![],
    )?;
    Ok(LifecycleEdit {
        state: edited.into_state(),
        result: command_result,
    })
}

/// Renders retained generation rows or one sanitized two-generation diff.
pub fn read_history(history: &History, args: &HistoryArgs) -> Result<CommandResult, CommandError> {
    if args.delete().is_some() {
        return Err(CommandError::new(
            ExitCode::EngineUnavailable,
            "generation deletion requires the private package engine",
            "use `pkg history` without `--delete` for an offline view",
        ));
    }
    if let [from, to] = args.diff() {
        let from = find_snapshot(history, from)?;
        let to = find_snapshot(history, to)?;
        let diff = History::diff(from, to);
        let changes = diff.changes().iter().map(|change| json!({
            "selector": change.selector().as_str(),
            "kind": match change.kind() { ChangeKind::Added => "added", ChangeKind::Removed => "removed", ChangeKind::Changed => "changed" },
            "beforeVersion": change.before_version().map(|version| version.as_str()),
            "afterVersion": change.after_version().map(|version| version.as_str()),
            "beforeOutputs": change.before_outputs().iter().map(|output| output.as_str()).collect::<Vec<_>>(),
            "afterOutputs": change.after_outputs().iter().map(|output| output.as_str()).collect::<Vec<_>>(),
            "beforePinned": change.before_pinned(),
            "afterPinned": change.after_pinned()
        })).collect::<Vec<_>>();
        let records = row_records(&changes, "generation_change");
        return result(
            format!("{} generation change(s)", changes.len()),
            Map::from_iter([
                ("from".into(), json!(from.generation().id())),
                ("to".into(), json!(to.generation().id())),
                ("changes".into(), Value::Array(changes)),
            ]),
            records,
        );
    }
    let entries = history.summaries().iter().map(|summary| {
        let counts = summary.changes_from_parent();
        json!({
            "id": summary.id(), "createdAt": summary.created_at(), "operation": summary.operation(),
            "active": summary.is_active(),
            "changes": counts.map(|counts| json!({"added": counts.added, "changed": counts.changed, "removed": counts.removed}))
        })
    }).collect::<Vec<_>>();
    let records = row_records(&entries, "generation");
    result(
        format!("{} generation(s) retained", entries.len()),
        Map::from_iter([("entries".into(), Value::Array(entries))]),
        records,
    )
}

/// Plans a rollback from one verified active snapshot and complete retained history.
pub fn rollback_state(
    active: &GenerationSnapshot,
    history: &History,
    args: &RollbackArgs,
) -> Result<RollbackEdit, CommandError> {
    let target = args
        .generation()
        .map(|id| RollbackTarget::Named(id.to_owned()))
        .unwrap_or(RollbackTarget::Parent);
    let retained = history
        .snapshots()
        .iter()
        .filter(|snapshot| snapshot.generation().id() != active.generation().id())
        .cloned()
        .collect::<Vec<_>>();
    // V1 pins one managed Nix runtime for every accepted channel sequence. A
    // future runtime migration must replace this with an authenticated
    // compatibility capability before it can publish such a channel.
    let plan = plan_rollback(active, &retained, target, |_| true).map_err(|_| {
        CommandError::new(
            ExitCode::ResolveFailed,
            "the requested rollback generation is unavailable or incompatible",
            "run `pkg history` and choose a retained generation",
        )
    })?;
    let target_id = plan.target().generation().id();
    let result = result(
        format!("rollback to {target_id} ready"),
        Map::from_iter([
            ("sourceGeneration".into(), json!(active.generation().id())),
            ("targetGeneration".into(), json!(target_id)),
            (
                "packageCount".into(),
                json!(plan.target().state().manifest().entries().len()),
            ),
        ]),
        vec![],
    )?;
    Ok(RollbackEdit { plan, result })
}

fn resolve_targets(
    state: &LifecycleState,
    names: &[String],
) -> Result<Vec<SelectorId>, CommandError> {
    names
        .iter()
        .map(|name| {
            let matches = state
                .manifest()
                .entries()
                .iter()
                .filter(|entry| entry.id().as_str() == name || entry.selector().as_str() == name)
                .map(|entry| entry.id().clone())
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [id] => Ok(id.clone()),
                [] => Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "package is not installed",
                    "run `pkg list` and use an installed selector",
                )),
                _ => Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "installed selector is ambiguous",
                    "use the stable selector id from machine output",
                )),
            }
        })
        .collect()
}

fn find_snapshot<'a>(
    history: &'a History,
    id: &str,
) -> Result<&'a GenerationSnapshot, CommandError> {
    history
        .snapshots()
        .iter()
        .find(|snapshot| snapshot.generation().id() == id)
        .ok_or_else(|| {
            CommandError::new(
                ExitCode::ResolveFailed,
                "generation was not found",
                "run `pkg history` to list retained generations",
            )
        })
}

fn row_records(entries: &[Value], kind: &str) -> Vec<Map<String, Value>> {
    entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!(kind));
            record
        })
        .collect()
}

fn result(
    summary: String,
    fields: Map<String, Value>,
    records: Vec<Map<String, Value>>,
) -> Result<CommandResult, CommandError> {
    CommandResult::new(summary, fields, records).map_err(|_| state_error())
}

fn state_error() -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "active package state is inconsistent",
        "run `pkg doctor` before making changes",
    )
}

#[cfg(test)]
mod tests {
    use pkg_core::state::{LockedState, Manifest};

    use super::*;
    use crate::cli::{Cli, Command};

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn store(hash: char, name: &str) -> String {
        format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
    }
    fn drv(hash: char, name: &str) -> String {
        format!("/nix/store/{}-{name}.drv", hash.to_string().repeat(32))
    }

    fn state() -> LifecycleState {
        let alpha = store('0', "alpha");
        let beta = store('1', "beta");
        let manifest = json!({
            "schemaVersion": 1, "channelSeq": 2, "uid": 1001,
            "entries": [
                {"id":"sel_alpha","selector":"alpha","attribute":"alpha","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"channel:current","pinned":false,"pinnedTo":null,"addedAt":"2026-08-09T00:00:00Z","origin":"user:install"},
                {"id":"sel_beta","selector":"beta","attribute":"beta","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"channel:current","pinned":false,"pinnedTo":null,"addedAt":"2026-08-09T00:00:00Z","origin":"user:install"}
            ], "pins": []
        });
        let locked = json!({
            "schemaVersion":1,"channelSeq":2,"system":"x86_64-linux","uid":1001,
            "entries": {
                "sel_alpha":{"attribute":"alpha","nixpkgsRev":REVISION,"realized":{"storePath":alpha,"deriver":drv('0',"alpha"),"outputs":{"out":store('0',"alpha")},"outputsToInstall":["out"],"system":"x86_64-linux","narHash":NAR,"closureNarSize":42,"pname":"alpha","version":"1.0"},"lockedAt":"2026-08-09T00:00:01Z","provenance":"cache:official","sigsObserved":["official-1:fixture"]},
                "sel_beta":{"attribute":"beta","nixpkgsRev":REVISION,"realized":{"storePath":beta,"deriver":drv('1',"beta"),"outputs":{"out":store('1',"beta")},"outputsToInstall":["out"],"system":"x86_64-linux","narHash":NAR,"closureNarSize":84,"pname":"beta","version":"2.0"},"lockedAt":"2026-08-09T00:00:01Z","provenance":"cache:official","sigsObserved":["official-1:fixture"]}
            }
        });
        LifecycleState::new(
            Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
            LockedState::from_json(&serde_json::to_vec(&locked).unwrap()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn list_maps_active_state_without_private_identity() {
        let cli = Cli::try_parse(["pkg", "list", "--with-outputs", "--size"]).unwrap();
        let Command::List(args) = cli.parsed_command() else {
            unreachable!()
        };
        let result = list_state(&state(), args, None).unwrap();
        assert_eq!(result.fields()["entries"].as_array().unwrap().len(), 2);
        assert_eq!(result.fields()["entries"][0]["closureBytes"], 42);
        let encoded = serde_json::to_string(result.fields()).unwrap();
        assert!(!encoded.contains("/nix/store/"));
        assert!(!encoded.contains("x86_64-linux"));
    }

    #[test]
    fn remove_and_pin_use_core_atomic_lifecycle_editors() {
        let remove = Cli::try_parse(["pkg", "remove", "beta", "--orphan-check"]).unwrap();
        let Command::Remove(args) = remove.parsed_command() else {
            unreachable!()
        };
        let removed = remove_state(state(), args).unwrap();
        assert_eq!(removed.state().manifest().entries().len(), 1);
        assert_eq!(removed.result().fields()["orphanCheckRequested"], true);

        let pin = Cli::try_parse(["pkg", "pin", "alpha"]).unwrap();
        let Command::Pin(args) = pin.parsed_command() else {
            unreachable!()
        };
        let pinned = edit_pin_state(state(), args, PinAction::Pin).unwrap();
        assert!(pinned.state().manifest().entries()[0].is_pinned());
        assert_eq!(pinned.result().fields()["changed"][0], "sel_alpha");
    }

    #[test]
    fn empty_history_is_offline_but_delete_stays_engine_bound() {
        let history = History::new(vec![], None).unwrap();
        let list = Cli::try_parse(["pkg", "history"]).unwrap();
        let Command::History(args) = list.parsed_command() else {
            unreachable!()
        };
        assert!(
            read_history(&history, args).unwrap().fields()["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let delete = Cli::try_parse(["pkg", "history", "--delete", "gen-0001"]).unwrap();
        let Command::History(args) = delete.parsed_command() else {
            unreachable!()
        };
        assert_eq!(
            read_history(&history, args).unwrap_err().exit_code(),
            ExitCode::EngineUnavailable
        );
    }
}
