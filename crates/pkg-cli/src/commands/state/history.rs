//! Readable projections of verified generation snapshots. No second history store.

use super::{find_snapshot, installed_packages, result, row_records};
use crate::cli::HistoryArgs;
use crate::commands::execute::CommandResult;
use crate::commands::query::optional_text;
use crate::exit::ExitCode;
use crate::ux::CommandError;
use pkg_core::{ChangeKind, GenerationSnapshot, History};
use serde_json::{Map, Value, json};

/// Read a saved environment, the retained history, or a two-generation diff.
///
/// # Errors
/// Returns an error for a missing generation, an engine-bound deletion, or an invalid public result.
pub fn read_history(history: &History, args: &HistoryArgs) -> Result<CommandResult, CommandError> {
    if args.delete().is_some() {
        return Err(CommandError::new(
            ExitCode::EngineUnavailable,
            "generation deletion requires the private package engine",
            "use `pkg history` without `--delete` for an offline view",
        ));
    }
    if let Some(id) = args.generation() {
        return snapshot_result(find_snapshot(history, id)?);
    }
    if let [from, to] = args.diff() {
        let from = find_snapshot(history, from)?;
        let to = find_snapshot(history, to)?;
        let changes = package_changes(from, to);
        let records = row_records(&changes, "generation_change");
        return result(
            format!("{} generation change(s)", changes.len()),
            Map::from_iter([
                ("from".into(), json!(from.generation().id())),
                ("to".into(), json!(to.generation().id())),
                ("changes".into(), json!(changes)),
            ]),
            records,
        );
    }
    let entries = history_entries(history)?;
    let records = row_records(&entries, "generation");
    result(
        format!("{} generation(s) retained", entries.len()),
        Map::from_iter([("entries".into(), json!(entries))]),
        records,
    )
}

fn snapshot_result(snapshot: &GenerationSnapshot) -> Result<CommandResult, CommandError> {
    let id = snapshot.generation().id();
    let packages = snapshot_packages(snapshot)?;
    result(
        format!(
            "{} package(s) in {id}",
            snapshot.state().manifest().entries().len()
        ),
        Map::from_iter([
            ("generation".into(), json!(id)),
            ("packages".into(), packages),
            (
                "createdAt".into(),
                json!(snapshot.generation().created_at()),
            ),
            (
                "operation".into(),
                json!(snapshot.generation().operation().kind()),
            ),
        ]),
        vec![],
    )
}

fn snapshot_packages(snapshot: &GenerationSnapshot) -> Result<Value, CommandError> {
    let ids = snapshot
        .state()
        .manifest()
        .entries()
        .iter()
        .map(|entry| entry.id().clone())
        .collect::<Vec<_>>();
    installed_packages(snapshot.state(), &ids)
}

fn history_entries(history: &History) -> Result<Vec<Value>, CommandError> {
    let by_id = history
        .snapshots()
        .iter()
        .map(|snapshot| (snapshot.generation().id(), snapshot))
        .collect::<std::collections::BTreeMap<_, _>>();
    history.summaries().iter().map(|summary| {
        let snapshot = by_id[summary.id()];
        let parent = snapshot.generation().parent().and_then(|id| by_id.get(id));
        let changes = parent.map(|parent| package_changes(parent, snapshot));
        let counts = summary.changes_from_parent();
        Ok(json!({
            "id": summary.id(), "createdAt": summary.created_at(), "operation": summary.operation(),
            "active": summary.is_active(),
            "changes": counts.map(|counts| json!({"added": counts.added, "changed": counts.changed, "removed": counts.removed})),
            "packageChanges": changes,
            "parent": snapshot.generation().parent(),
            "packages": if parent.is_none() { Some(snapshot_packages(snapshot)?) } else { None },
        }))
    }).collect()
}

pub(super) fn package_changes(from: &GenerationSnapshot, to: &GenerationSnapshot) -> Vec<Value> {
    History::diff(from, to).changes().iter().map(|change| {
        let before = from.state().locked().entries().get(change.id()).map(pkg_core::state::LockEntry::realization);
        let after = to.state().locked().entries().get(change.id()).map(pkg_core::state::LockEntry::realization);
        json!({
            "selector": change.selector().as_str(),
            "name": after.or(before).map(pkg_core::Realization::pname),
            "kind": match change.kind() { ChangeKind::Added => "added", ChangeKind::Removed => "removed", ChangeKind::Changed => "changed" },
            "beforeVersion": change.before_version().map(|version| optional_text(version.as_str())),
            "afterVersion": change.after_version().map(|version| optional_text(version.as_str())),
            "beforeOutputs": change.before_outputs().iter().map(pkg_core::OutputName::as_str).collect::<Vec<_>>(),
            "afterOutputs": change.after_outputs().iter().map(pkg_core::OutputName::as_str).collect::<Vec<_>>(),
            "beforePinned": change.before_pinned(), "afterPinned": change.after_pinned(),
            "beforeRevision": before.map(|realization| realization.source_commit().as_str()),
            "afterRevision": after.map(|realization| realization.source_commit().as_str()),
        })
    }).collect()
}
