//! Human views of validated public command results.

use std::io::{self, Write};

use serde_json::{Map, Value};

use super::execute::CommandResult;
use crate::presentation::table::write_table;
use crate::presentation::{Style, Tone, format_bytes};

mod changes;
mod details;

type Column = (&'static str, &'static str);

#[derive(Clone, Copy, Default)]
pub(super) struct View {
    pub(super) style: Style,
    pub(super) preview: bool,
}

impl View {
    pub(super) fn from_cli(cli: &crate::cli::Cli) -> Self {
        use crate::cli::Command;
        let changes = match cli.parsed_command() {
            Command::Install(_)
            | Command::Remove(_)
            | Command::Upgrade(_)
            | Command::Pin(_)
            | Command::Unpin(_)
            | Command::Rollback(_)
            | Command::Gc(_)
            | Command::Repair(_)
            | Command::System(crate::cli::SystemCommand::Uninstall)
            | Command::Update(_) => true,
            Command::History(args) => args.delete().is_some(),
            _ => false,
        };
        Self {
            style: Style::stdout(cli.no_color()),
            preview: cli.dry_run() && changes,
        }
    }
}

pub(super) fn write_result(
    mut writer: impl Write,
    command: &str,
    result: &CommandResult,
    view: View,
) -> io::Result<()> {
    let style = view.style;
    let preview = view.preview || result.fields().get("dryRun") == Some(&Value::Bool(true));
    let records = result.records();
    if command == "list" && result.fields().get("nameOnly") == Some(&Value::Bool(true)) {
        for record in records {
            writeln!(writer, "{}", cell(record, "name"))?;
        }
        return Ok(());
    }
    let title = if preview {
        format!("pkg · {command} preview")
    } else {
        format!("pkg · {command}")
    };
    style.heading(&mut writer, &title)?;
    if command == "info" {
        write_info(&mut writer, records, style)?;
    } else if !records.is_empty() {
        let columns = columns(command, records);
        if !columns.is_empty() {
            let rows = records
                .iter()
                .map(|record| columns.iter().map(|(_, key)| cell(record, key)).collect())
                .collect::<Vec<_>>();
            let headings = columns.iter().map(|(title, _)| *title).collect::<Vec<_>>();
            write_table(&mut writer, &headings, &rows, style)?;
            writeln!(writer)?;
        }
    }
    if command == "list" {
        write_list_notes(&mut writer, records, style)?;
    }
    details::write_details(&mut writer, command, &result.human_fields(), style)?;
    if preview {
        if (command == "upgrade"
            && result.fields().get("upgraded") == Some(&Value::Array(Vec::new())))
            || (command == "system uninstall"
                && result.fields().get("status").and_then(Value::as_str) == Some("absent"))
        {
            style.text(&mut writer, "Plan: ", &summary_text(result.summary()))?;
        }
        style.text(&mut writer, "Preview: ", "No changes were applied.")?;
    } else {
        style.success(&mut writer, &summary_text(result.summary()))?;
    }
    details::write_next(writer, command, result, view, preview)
}

fn write_list_notes(
    mut writer: impl Write,
    records: &[Map<String, Value>],
    style: Style,
) -> io::Result<()> {
    changes::write_sources(&mut writer, records, style)?;
    if records
        .iter()
        .any(|record| record.contains_key("closureBytes"))
    {
        style.text(
            &mut writer,
            "",
            "Size is the primary output closure, including shared dependencies.",
        )?;
        writeln!(writer)?;
    }
    Ok(())
}

pub(super) fn write_confirmation(
    mut writer: impl Write,
    command: &str,
    result: &CommandResult,
) -> io::Result<()> {
    let style = Style::new(Style::stderr(true).is_terminal(), false);
    style.heading(&mut writer, &format!("pkg · {command}"))?;
    details::write_details(writer, command, &result.human_fields(), style)
}

fn summary_text(summary: &str) -> String {
    let mut count = None;
    let mut text = summary
        .split_whitespace()
        .map(|word| {
            if let Ok(number) = word.parse::<u64>() {
                count = Some(number);
            }
            word.replace("(s)", if count == Some(1) { "" } else { "s" })
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !text.starts_with("pkg ")
        && let Some(first) = text.get_mut(..1)
    {
        first.make_ascii_uppercase();
    }
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

fn columns(command: &str, records: &[Map<String, Value>]) -> Vec<Column> {
    let Some(first) = records.first() else {
        return Vec::new();
    };
    match command {
        "search" => vec![
            ("Package", "package"),
            ("Version", "version"),
            ("Status", "catalogStatus"),
            ("Description", "description"),
        ],
        "list" => list_columns(records, first),
        "outdated" => vec![
            ("Package", "package"),
            ("Installed", "current"),
            ("Available", "available"),
            ("Pinned", "pinned"),
            ("Change", "kind"),
        ],
        "history" if first.get("type").and_then(Value::as_str) == Some("generation_change") => {
            history_columns(records)
        }
        "history" => vec![
            ("Generation", "generationLabel"),
            ("Created", "createdAt"),
            ("Operation", "operation"),
            ("Changes", "historyChanges"),
        ],
        _ => Vec::new(),
    }
}

fn list_columns(records: &[Map<String, Value>], first: &Map<String, Value>) -> Vec<Column> {
    let mut columns = vec![("Package", "name"), ("Version", "version")];
    if records
        .iter()
        .any(|record| record.get("pinned") == Some(&Value::Bool(true)))
    {
        columns.push(("Pinned", "pinned"));
    }
    for column in [
        ("Outputs", "outputsToInstall"),
        ("Size", "closureBytes"),
        ("Outdated", "outdated"),
    ] {
        if first.contains_key(column.1) {
            columns.push(column);
        }
    }
    columns
}

fn history_columns(records: &[Map<String, Value>]) -> Vec<Column> {
    let mut columns = vec![
        ("Package", "selector"),
        ("Change", "kind"),
        ("Before", "beforeVersion"),
        ("After", "afterVersion"),
    ];
    for (before, after, column) in [
        ("beforePinned", "afterPinned", ("Pins", "pinChange")),
        ("beforeOutputs", "afterOutputs", ("Outputs", "outputChange")),
        (
            "beforeRevision",
            "afterRevision",
            ("Source commit", "revisionChange"),
        ),
    ] {
        if records.iter().any(|record| {
            record.get("kind").and_then(Value::as_str) == Some("changed")
                && record.get(before) != record.get(after)
        }) {
            columns.push(column);
        }
    }
    columns
}

fn write_info(
    mut writer: impl Write,
    records: &[Map<String, Value>],
    style: Style,
) -> io::Result<()> {
    for record in records {
        let title = format!("{} {}", cell(record, "package"), cell(record, "version"));
        if style.is_terminal() {
            for line in crate::presentation::wrap(&title, crate::presentation::terminal_width()) {
                writeln!(writer, "{}", style.paint(&line, Tone::Heading))?;
            }
        } else {
            writeln!(writer, "{title}")?;
        }
        for (title, key) in [
            ("Description", "description"),
            ("Homepage", "homepage"),
            ("License", "licenses"),
            ("Outputs", "outputs"),
            ("Available", "available"),
            ("Broken", "broken"),
        ] {
            style.text(&mut writer, &format!("  {title}: "), &cell(record, key))?;
        }
        writeln!(writer)?;
    }
    Ok(())
}

fn cell(record: &Map<String, Value>, key: &str) -> String {
    if key == "generationLabel" {
        return format!(
            "{}{}",
            cell(record, "id"),
            if record.get("active") == Some(&Value::Bool(true)) {
                " (active)"
            } else {
                ""
            }
        );
    }
    if key == "historyChanges" {
        return changes::history_summary(record);
    }
    let change = match key {
        "pinChange" => Some(("beforePinned", "afterPinned")),
        "outputChange" => Some(("beforeOutputs", "afterOutputs")),
        "revisionChange" => Some(("beforeRevision", "afterRevision")),
        _ => None,
    };
    if let Some((before, after)) = change {
        return format!("{} → {}", cell(record, before), cell(record, after));
    }
    if key == "catalogStatus" {
        return match (
            record.get("broken").and_then(Value::as_bool),
            record.get("available").and_then(Value::as_bool),
        ) {
            (Some(true), _) => "broken",
            (Some(false), Some(true)) => "ready",
            _ => "unsupported",
        }
        .to_owned();
    }
    if matches!(key, "beforeRevision" | "afterRevision") {
        return record.get(key).and_then(Value::as_str).map_or_else(
            || "-".into(),
            |revision| revision.chars().take(12).collect(),
        );
    }
    if key == "closureBytes" {
        return record
            .get(key)
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".into(), format_bytes);
    }
    record.get(key).map_or_else(|| "-".into(), value_text)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) if !text.is_empty() => text.clone(),
        Value::Bool(value) => if *value { "yes" } else { "no" }.into(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) if !values.is_empty() => {
            values.iter().map(value_text).collect::<Vec<_>>().join(", ")
        }
        _ => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn render(command: &str, records: &Value) -> String {
        let records = records
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record.as_object().unwrap().clone())
            .collect();
        let result = CommandResult::new("Complete.", Map::new(), records).unwrap();
        let mut output = Vec::new();
        write_result(&mut output, command, &result, View::default()).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn installed_packages_show_selectors_versions_and_requested_details() {
        let output = render(
            "list",
            &json!([{"type":"installed_package", "selector":"github:casey/just#default", "name":"just", "version":"1.58.0", "pinned":true, "outputsToInstall":["out"], "closureBytes":1234}]),
        );
        for expected in [
            "Package",
            "Version",
            "Pinned",
            "Outputs",
            "Size",
            "github:casey/just#default",
            "1.58.0",
            "yes",
            "out",
            "1.2 KiB",
        ] {
            assert!(output.contains(expected), "missing {expected}: {output}");
        }
    }

    #[test]
    fn history_shows_pin_and_output_changes_even_when_versions_match() {
        let output = render(
            "history",
            &json!([
                {"type":"generation_change", "selector":"fzf", "kind":"changed", "beforeVersion":"1.0", "afterVersion":"1.0", "beforePinned":false, "afterPinned":true, "beforeOutputs":["out"], "afterOutputs":["out"]},
                {"type":"generation_change", "selector":"just", "kind":"changed", "beforeVersion":"1.0", "afterVersion":"1.0", "beforePinned":false, "afterPinned":false, "beforeOutputs":["out"], "afterOutputs":["out", "man"]}
            ]),
        );
        for text in ["Pins", "no → yes", "Outputs", "out → out, man"] {
            assert!(output.contains(text), "{output}");
        }
    }

    #[test]
    fn names_only_can_be_used_in_a_pipeline() {
        for (records, expected) in [
            (vec![json!({"type":"installed_package", "name":"just", "selector":"github:casey/just#default"}).as_object().unwrap().clone()], "just\n"),
            (vec![], ""),
        ] {
            let result = CommandResult::new("Installed packages", Map::from_iter([("nameOnly".into(), json!(true))]), records).unwrap();
            let mut output = Vec::new();
            write_result(&mut output, "list", &result, View::default()).unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
    }

    #[test]
    fn history_outdated_and_info_show_the_data_needed_for_the_next_command() {
        let history = render(
            "history",
            &json!([{"type":"generation", "id":"gen-0002", "createdAt":"2026-09-25T00:00:00Z", "operation":"install", "active":true}]),
        );
        assert!(history.contains("gen-0002") && history.contains("(active)"));
        let outdated = render(
            "outdated",
            &json!([{"type":"outdated_package", "package":"just", "current":"1.0", "available":"2.0", "pinned":false, "kind":"upgrade"}]),
        );
        assert!(outdated.contains("1.0") && outdated.contains("2.0"));
        let info = render(
            "info",
            &json!([{"type":"package_info", "package":"just", "version":null, "description":"Command runner", "licenses":["CC0-1.0"], "outputs":["out"], "available":true, "broken":false}]),
        );
        assert!(
            info.contains("just -\n")
                && info.contains("Command runner")
                && info.contains("CC0-1.0")
        );
    }
}

#[cfg(test)]
mod command_views {
    use super::*;
    use crate::cli::Cli;
    use serde_json::json;

    fn output(command: &str, fields: &Value, preview: bool) -> String {
        let result = CommandResult::new(
            "Operation complete.",
            fields.as_object().unwrap().clone(),
            vec![],
        )
        .unwrap();
        let mut out = Vec::new();
        write_result(
            &mut out,
            command,
            &result,
            View {
                style: Style::new(true, false),
                preview,
            },
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn results_show_the_packages_environments_and_disk_values() {
        for (command, fields, expected) in [
            (
                "install",
                json!({"added":[{"package":"just","version":"1.58.0","outputs":["out"]}], "generation":{"id":"gen-0002"}}),
                vec!["just", "1.58.0", "out", "gen-0002"],
            ),
            (
                "upgrade",
                json!({"upgraded":["just"], "skippedPinned":["fzf"]}),
                vec!["Packages: just", "Kept pinned: fzf"],
            ),
            (
                "remove",
                json!({"removed":["just"]}),
                vec!["Packages: just"],
            ),
            (
                "pin",
                json!({"changed":["just"],"unchanged":["fzf"]}),
                vec!["Packages: just", "Already in this state: fzf"],
            ),
            ("unpin", json!({"changed":["just"]}), vec!["Packages: just"]),
            (
                "rollback",
                json!({"sourceGeneration":"gen-0002","targetGeneration":"gen-0001","packageCount":2}),
                vec![
                    "From: gen-0002",
                    "Target environment: gen-0001",
                    "Packages: 2",
                ],
            ),
            (
                "gc",
                json!({"prunedGenerations":["gen-0001"],"freedBytes":1024}),
                vec!["Deleted generations: gen-0001", "Space freed: 1.0 KiB"],
            ),
            (
                "repair",
                json!({"generation":"gen-0002", "damagedPathCount":0}),
                vec!["Generation: gen-0002", "Damaged paths: 0"],
            ),
        ] {
            let text = output(command, &fields, false);
            for value in expected {
                assert!(text.contains(value), "{command}: {text}");
            }
        }
    }

    #[test]
    fn labels_survive_summary_changes_without_changing_machine_ids() {
        let labels = Map::from_iter([("sel_example".into(), json!("fzf"))]);
        let result = CommandResult::new(
            "1 package(s) pinned",
            Map::from_iter([("changed".into(), json!(["sel_example"]))]),
            vec![],
        )
        .unwrap()
        .with_package_labels(labels)
        .unwrap()
        .with_summary("1 package(s) pinned")
        .unwrap();
        assert_eq!(result.fields()["changed"], json!(["sel_example"]));
        assert_eq!(result.human_fields()["changed"], json!(["fzf"]));
        assert_eq!(summary_text(result.summary()), "1 package pinned.");
        assert_eq!(
            summary_text("pruned 1 generation(s); collected 2 store path(s)"),
            "Pruned 1 generation; collected 2 store paths."
        );
        assert!(
            CommandResult::new("Done", Map::new(), vec![])
                .unwrap()
                .with_package_labels(Map::from_iter([(
                    "id".into(),
                    json!("\u{001b}[31mprivate")
                )]))
                .is_err()
        );
    }

    #[test]
    fn mutation_previews_never_claim_a_completed_action() {
        for command in [
            "install",
            "remove",
            "upgrade",
            "pin",
            "unpin",
            "rollback",
            "gc",
            "repair",
            "system uninstall",
        ] {
            let text = output(command, &json!({"dryRun":true}), false);
            assert!(
                text.contains("Preview: No changes were applied."),
                "{command}: {text}"
            );
            assert!(!text.contains('✓'));
            assert!(!text.contains("Operation complete"));
        }
        for args in [
            vec!["pkg", "--dry-run", "remove", "just"],
            vec!["pkg", "--dry-run", "history", "--delete", "gen-0001"],
        ] {
            assert!(View::from_cli(&Cli::try_parse(args).unwrap()).preview);
        }
        for command in ["list", "history", "outdated"] {
            assert!(
                !View::from_cli(&Cli::try_parse(["pkg", "--dry-run", command]).unwrap()).preview
            );
        }
    }

    #[test]
    fn catalog_previews_retain_the_check_result() {
        for (updated, expected) in [(true, "yes"), (false, "no")] {
            let text = output(
                "update",
                &json!({"channelSequence":52,"checkedOnly":true,"updated":updated}),
                true,
            );
            assert!(text.contains("Catalog sequence: 52"));
            assert!(text.contains(&format!("Update available: {expected}")));
            assert!(text.contains("No changes were applied."));
            assert!(!text.contains('✓'));
        }
    }

    #[test]
    fn applied_catalog_refresh_is_not_reported_as_an_available_update() {
        let text = output(
            "update",
            &json!({"channelSequence":52,"checkedOnly":false,"updated":true}),
            false,
        );
        assert!(text.contains("Catalog updated: yes"));
        assert!(!text.contains("Update available"));
    }

    #[test]
    fn build_previews_show_targets_and_do_not_invent_unknown_estimates() {
        let text = output(
            "install",
            &json!({"dryRun":true, "deferredChecks":["new-output-collisions"], "preflight":{
                "targets":[{"packageName":"just", "version":"1.58.0", "localBuildRequired":true}],
                "estimates":{"approxNewDiskBytes":null, "minimumFreeDiskBytes":8589934592_u64}
            }}),
            true,
        );
        for value in [
            "just",
            "1.58.0",
            "Local build",
            "unknown",
            "8.0 GiB",
            "No changes were applied.",
            "The preview does not confirm activation.",
        ] {
            assert!(text.contains(value), "{text}");
        }
        assert!(!text.contains('✓'));
    }
}
