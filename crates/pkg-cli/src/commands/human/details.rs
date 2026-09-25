//! Selected public result fields that help users see what changed.

use super::{View, value_text};
use crate::commands::execute::CommandResult;
use crate::presentation::table::write_table;
use crate::presentation::{Style, Tone, format_bytes};
use serde_json::{Map, Value};
use std::io::{self, Write};

pub(super) fn write_details(
    mut writer: impl Write,
    command: &str,
    fields: &Map<String, Value>,
    style: Style,
) -> io::Result<()> {
    write_added(&mut writer, fields, style)?;
    if let Some(plan) = fields
        .get("preflight")
        .or_else(|| fields.get("buildPreview"))
    {
        write_plan(&mut writer, plan, style)?;
    }
    write_fields(&mut writer, fields, style)?;
    if command == "update" {
        for (key, title) in [
            ("channelSequence", "Catalog sequence"),
            ("updated", "Update available"),
        ] {
            if let Some(value) = fields.get(key) {
                style.text(&mut writer, &format!("{title}: "), &value_text(value))?;
            }
        }
    }
    if command == "uninstall" && fields.get("status").and_then(Value::as_str) == Some("planned") {
        style.text(
            &mut writer,
            "Scope: ",
            "pkg and its managed Nix installation will be removed.",
        )?;
    }
    Ok(())
}

fn write_fields(
    mut writer: impl Write,
    fields: &Map<String, Value>,
    style: Style,
) -> io::Result<()> {
    for (key, title) in [
        ("upgraded", "Packages"),
        ("removed", "Packages"),
        ("changed", "Packages"),
        ("unchanged", "Already in this state"),
        ("skippedPinned", "Kept pinned"),
        ("generations", "Generations to delete"),
        ("prunedGenerations", "Deleted generations"),
        ("recoveredGenerations", "Recovered generations"),
    ] {
        if let Some(values) = fields
            .get(key)
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty())
        {
            style.text(
                &mut writer,
                &format!("{title}: "),
                &value_text(&Value::Array(values.clone())),
            )?;
        }
    }
    for (key, title) in [
        ("sourceGeneration", "From"),
        ("targetGeneration", "Target environment"),
        ("packageCount", "Packages"),
        ("damagedPathCount", "Damaged paths"),
        ("collectedPathCount", "Collected store paths"),
    ] {
        if let Some(value) = fields.get(key).filter(|value| !value.is_null()) {
            style.text(&mut writer, &format!("{title}: "), &value_text(value))?;
        }
    }
    if let Some(generation) = fields.get("generation").filter(|value| !value.is_null()) {
        let generation = generation.get("id").unwrap_or(generation);
        style.text(&mut writer, "Generation: ", &value_text(generation))?;
    }
    for (key, title) in [
        ("freedBytes", "Space freed"),
        ("estimatedReclaimableBytes", "Estimated space to free"),
    ] {
        if let Some(value) = fields.get(key) {
            let text = value
                .as_u64()
                .map_or_else(|| "unknown".into(), format_bytes);
            style.text(&mut writer, &format!("{title}: "), &text)?;
        }
    }
    Ok(())
}

fn write_added(
    mut writer: impl Write,
    fields: &Map<String, Value>,
    style: Style,
) -> io::Result<()> {
    if let Some(packages) = fields.get("added").and_then(Value::as_array) {
        let rows = packages
            .iter()
            .map(|package| {
                ["package", "version", "outputs"]
                    .map(|key| package.get(key).map_or_else(|| "-".into(), value_text))
                    .to_vec()
            })
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            write_table(
                &mut writer,
                &["Package", "Version", "Outputs"],
                &rows,
                style,
            )?;
            writeln!(writer)?;
        }
    }
    Ok(())
}

pub(super) fn write_plan(mut writer: impl Write, plan: &Value, style: Style) -> io::Result<()> {
    if let Some(targets) = plan.get("targets").and_then(Value::as_array) {
        let rows = targets
            .iter()
            .map(|target| {
                let action =
                    if target.get("localBuildRequired").and_then(Value::as_bool) == Some(true) {
                        "Local build"
                    } else {
                        "Available output"
                    };
                vec![
                    target
                        .get("packageName")
                        .map_or_else(|| "-".into(), value_text),
                    target.get("version").map_or_else(|| "-".into(), value_text),
                    action.into(),
                ]
            })
            .collect::<Vec<_>>();
        write_table(&mut writer, &["Package", "Version", "Source"], &rows, style)?;
    }
    for (path, label) in [
        ("/estimates/approxNewDiskBytes", "Estimated new disk use"),
        (
            "/estimates/minimumFreeDiskBytes",
            "Free disk needed to start",
        ),
    ] {
        let text = plan
            .pointer(path)
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".into(), format_bytes);
        style.text(&mut writer, &format!("{label}: "), &text)?;
    }
    style.text(
        &mut writer,
        "",
        "Build estimates are approximate. A build can need more space.",
    )?;
    writeln!(writer)
}

pub(super) fn write_next(
    mut writer: impl Write,
    command: &str,
    result: &CommandResult,
    view: View,
    preview: bool,
) -> io::Result<()> {
    if !view.style.is_terminal() {
        return Ok(());
    }
    let next = if preview {
        "Run the command without --dry-run to apply this plan."
    } else {
        match command {
            "search" if result.records().is_empty() => {
                "Try a shorter search, or run pkg update to refresh the catalog."
            }
            "search" => "pkg info <package> shows details. pkg install <package> installs it.",
            "info" => "pkg install <package> installs a package.",
            "list" if result.records().is_empty() => {
                "pkg search <query> finds packages. pkg install <package> installs one."
            }
            "outdated" if !result.records().is_empty() => {
                "pkg upgrade --all upgrades all unpinned packages."
            }
            "update" if result.fields().get("checkedOnly") == Some(&Value::Bool(true)) => {
                "pkg update refreshes the catalog."
            }
            "update" => "pkg outdated shows available package updates.",
            "install" | "upgrade" | "remove" | "rollback" => {
                "pkg list shows your packages. pkg history shows saved environments."
            }
            "pin" => {
                "pkg list --pinned shows pinned packages. pkg unpin <package> allows upgrades."
            }
            "unpin" => "pkg upgrade <package> installs the latest available version.",
            "history" => "pkg rollback <ID> restores a saved environment.",
            "repair" => "pkg doctor checks your system.",
            _ => return Ok(()),
        }
    };
    writeln!(writer)?;
    write!(writer, "{} ", view.style.paint("Next", Tone::Muted))?;
    let lines = crate::presentation::wrap(
        next,
        crate::presentation::terminal_width().saturating_sub(5),
    );
    writeln!(writer, "{}", lines.join("\n     "))
}
