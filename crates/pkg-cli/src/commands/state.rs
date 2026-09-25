//! Offline active-state/history views and deterministic lifecycle edits.

use serde_json::{Map, Value, json};

use pkg_core::lifecycle::LifecycleState;
use pkg_core::remove::remove_selectors;
use pkg_core::{
    GenerationSnapshot, History, PinAction, RollbackPlan, RollbackTarget, SelectorId, edit_pins,
    plan_rollback,
};

use crate::cli::{ListArgs, PackageArgs, RemoveArgs, RollbackArgs};

mod history;
use crate::commands::execute::CommandResult;
use crate::commands::query::optional_text;
use crate::exit::ExitCode;
use crate::ux::CommandError;
pub use history::read_history;

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
    outdated_attributes: Option<&std::collections::BTreeSet<String>>,
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
        let is_outdated = realization.flake().is_none()
            && outdated_attributes
                .is_some_and(|attributes| attributes.contains(desired.attribute().as_str()));
        if args.outdated() && !is_outdated {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("selector".into(), json!(desired.selector().as_str()));
        entry.insert("name".into(), json!(realization.pname()));
        if !args.name_only() {
            entry.insert(
                "version".into(),
                optional_text(realization.version().as_str()),
            );
            entry.insert("pinned".into(), json!(desired.is_pinned()));
            entry.insert(
                "sourceRevision".into(),
                json!(realization.source_commit().as_str()),
            );
            if args.with_outputs() {
                entry.insert(
                    "outputsToInstall".into(),
                    json!(
                        realization
                            .outputs_to_install()
                            .iter()
                            .map(pkg_core::OutputName::as_str)
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
    entries.sort_by(|left, right| {
        left["name"]
            .as_str()
            .cmp(&right["name"].as_str())
            .then_with(|| left["selector"].as_str().cmp(&right["selector"].as_str()))
    });
    let records = row_records(&entries, "installed_package");
    result(
        format!("Installed packages: {}", entries.len()),
        Map::from_iter([
            ("entries".into(), Value::Array(entries)),
            ("nameOnly".into(), json!(args.name_only())),
        ]),
        records,
    )
}

/// Applies an atomic remove edit in memory; the caller commits it as a fresh generation.
pub fn remove_state(
    state: LifecycleState,
    args: &RemoveArgs,
) -> Result<LifecycleEdit, CommandError> {
    let labels = package_labels(&state);
    let targets = resolve_targets(&state, args.packages())?;
    let packages = installed_packages(&state, &targets)?;
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
            ("packages".into(), packages),
            ("orphanCheckRequested".into(), json!(args.orphan_check())),
        ]),
        vec![],
    )?;
    Ok(LifecycleEdit {
        state: removed.into_state(),
        result: command_result
            .with_package_labels(labels)
            .map_err(|_| state_error())?,
    })
}

/// Applies an atomic pin/unpin edit in memory; the caller commits a fresh generation.
pub fn edit_pin_state(
    state: LifecycleState,
    args: &PackageArgs,
    action: PinAction,
) -> Result<LifecycleEdit, CommandError> {
    let labels = package_labels(&state);
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
            (
                "packages".into(),
                installed_packages(edited.state(), edited.changed())?,
            ),
            ("unchanged".into(), json!(unchanged)),
        ]),
        vec![],
    )?;
    Ok(LifecycleEdit {
        state: edited.into_state(),
        result: command_result
            .with_package_labels(labels)
            .map_err(|_| state_error())?,
    })
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
                "packageChanges".into(),
                json!(history::package_changes(active, plan.target())),
            ),
            (
                "packageCount".into(),
                json!(plan.target().state().manifest().entries().len()),
            ),
        ]),
        vec![],
    )?;
    Ok(RollbackEdit { plan, result })
}

pub(super) fn installed_packages(
    state: &LifecycleState,
    ids: &[SelectorId],
) -> Result<Value, CommandError> {
    ids.iter().map(|id| {
        let desired = state.manifest().entries().iter().find(|entry| entry.id() == id)
            .ok_or_else(state_error)?;
        let locked = state.locked().entries().get(id).ok_or_else(state_error)?;
        Ok(json!({"name": locked.realization().pname(), "selector": desired.selector().as_str(),
            "version": optional_text(locked.realization().version().as_str()), "pinned": desired.is_pinned()}))
    }).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

pub(super) fn package_labels(state: &LifecycleState) -> Map<String, Value> {
    state
        .manifest()
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.id().as_str().to_owned(),
                json!(entry.selector().as_str()),
            )
        })
        .collect()
}

/// Resolve installed intent before any approval. Exact selectors take priority;
/// realized names are accepted only when they identify one installed selector.
pub(super) fn resolve_targets(
    state: &LifecycleState,
    names: &[String],
) -> Result<Vec<SelectorId>, CommandError> {
    names
        .iter()
        .map(|name| resolve_target(state, name))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map(|ids| ids.into_iter().collect())
}

fn resolve_target(state: &LifecycleState, name: &str) -> Result<SelectorId, CommandError> {
    let exact = state
        .manifest()
        .entries()
        .iter()
        .filter(|entry| entry.id().as_str() == name || entry.selector().as_str() == name)
        .collect::<Vec<_>>();
    let matches = if exact.is_empty() {
        state
            .manifest()
            .entries()
            .iter()
            .filter(|entry| {
                state
                    .locked()
                    .entries()
                    .get(entry.id())
                    .is_some_and(|entry| entry.realization().pname() == name)
            })
            .collect::<Vec<_>>()
    } else {
        exact
    };
    match matches.as_slice() {
        [entry] => Ok(entry.id().clone()),
        [] => Err(CommandError::new(
            ExitCode::ResolveFailed,
            format!("package '{name}' is not installed"),
            "run `pkg list` to see installed names and selectors",
        )),
        _ => {
            let choices = matches
                .iter()
                .map(|entry| entry.id().as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(CommandError::new(
                ExitCode::ResolveFailed,
                format!("installed name '{name}' matches more than one package"),
                format!("use an exact selector from `pkg list`, or one of these IDs: {choices}"),
            ))
        }
    }
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
mod tests;
