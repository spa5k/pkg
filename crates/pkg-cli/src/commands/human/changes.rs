//! Package names, source selectors, and generation changes in human views.

use super::cell;
use crate::presentation::{Style, table::write_table};
use serde_json::{Map, Value};
use std::io::{self, Write};

pub(super) fn write_sources(
    mut writer: impl Write,
    packages: &[Map<String, Value>],
    style: Style,
) -> io::Result<()> {
    for package in packages {
        let name = cell(package, "name");
        let selector = cell(package, "selector");
        if name != selector && selector != "-" {
            style.text(&mut writer, &format!("{name}: "), &selector)?;
        }
    }
    Ok(())
}

pub(super) fn write_packages(
    mut writer: impl Write,
    packages: &[Value],
    style: Style,
) -> io::Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    let records = packages
        .iter()
        .filter_map(Value::as_object)
        .cloned()
        .collect::<Vec<_>>();
    let pinned = records
        .iter()
        .any(|record| record.get("pinned") == Some(&Value::Bool(true)));
    let mut headings = vec!["Package", "Version"];
    if pinned {
        headings.push("Pinned");
    }
    let rows = records
        .iter()
        .map(|record| {
            let mut row = vec![cell(record, "name"), cell(record, "version")];
            if pinned {
                row.push(cell(record, "pinned"));
            }
            row
        })
        .collect::<Vec<_>>();
    write_table(&mut writer, &headings, &rows, style)?;
    write_sources(&mut writer, &records, style)?;
    writeln!(writer)
}

pub(super) fn write_changes(
    mut writer: impl Write,
    changes: &[Value],
    style: Style,
) -> io::Result<()> {
    if changes.is_empty() {
        return style.text(writer, "", "No package changes.");
    }
    let rows = changes
        .iter()
        .filter_map(Value::as_object)
        .map(|change| vec![cell(change, "name"), change_detail(change)])
        .collect::<Vec<_>>();
    write_table(&mut writer, &["Package", "Change"], &rows, style)?;
    let records = changes
        .iter()
        .filter_map(Value::as_object)
        .cloned()
        .collect::<Vec<_>>();
    write_sources(&mut writer, &records, style)?;
    writeln!(writer)
}

pub(super) fn history_summary(record: &Map<String, Value>) -> String {
    if let Some(changes) = record.get("packageChanges").and_then(Value::as_array) {
        if changes.is_empty() {
            return "No package changes".into();
        }
        return changes
            .iter()
            .filter_map(Value::as_object)
            .map(|change| format!("{}: {}", cell(change, "name"), change_detail(change)))
            .collect::<Vec<_>>()
            .join("; ");
    }
    let packages = record
        .get("packages")
        .and_then(Value::as_array)
        .map(|packages| {
            packages
                .iter()
                .filter_map(Value::as_object)
                .map(|package| format!("{} {}", cell(package, "name"), cell(package, "version")))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let label = if record.get("parent").is_some_and(|parent| !parent.is_null()) {
        "Previous generation not retained. Saved"
    } else {
        "Initial environment"
    };
    format!(
        "{label}: {}",
        if packages.is_empty() {
            "empty"
        } else {
            &packages
        }
    )
}

fn change_detail(change: &Map<String, Value>) -> String {
    let before = cell(change, "beforeVersion");
    let after = cell(change, "afterVersion");
    match change.get("kind").and_then(Value::as_str) {
        Some("added") => return format!("Added {after}"),
        Some("removed") => return format!("Removed {before}"),
        _ => {}
    }
    let mut details = Vec::new();
    if before != after {
        details.push(format!("{before} → {after}"));
    }
    for (before, after, label) in [
        ("beforePinned", "afterPinned", "pinned"),
        ("beforeOutputs", "afterOutputs", "outputs"),
        ("beforeRevision", "afterRevision", "source commit"),
    ] {
        if change.get(before) != change.get(after) {
            details.push(format!(
                "{label}: {} → {}",
                cell(change, before),
                cell(change, after)
            ));
        }
    }
    if details.is_empty() {
        format!("Updated {after}")
    } else {
        details.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn change_labels_preserve_versions_and_nonversion_changes() {
        for (change, expected) in [
            (
                json!({"kind":"added","afterVersion":"1.58.0"}),
                "Added 1.58.0",
            ),
            (
                json!({"kind":"removed","beforeVersion":"1.58.0"}),
                "Removed 1.58.0",
            ),
            (
                json!({"kind":"changed","beforeVersion":"1.0","afterVersion":"2.0"}),
                "1.0 → 2.0",
            ),
            (
                json!({"kind":"changed","beforeVersion":"1.0","afterVersion":"1.0","beforePinned":false,"afterPinned":true}),
                "pinned: no → yes",
            ),
            (
                json!({"kind":"changed","beforeVersion":"1.0","afterVersion":"1.0","beforeOutputs":["out"],"afterOutputs":["out","man"]}),
                "outputs: out → out, man",
            ),
        ] {
            assert_eq!(change_detail(change.as_object().unwrap()), expected);
        }
        let missing_parent = json!({"parent":"gen-0001","packageChanges":null,"packages":[{"name":"just","version":"1.58.0"}]});
        let summary = history_summary(missing_parent.as_object().unwrap());
        assert!(summary.contains("Previous generation not retained"));
        assert!(summary.contains("just 1.58.0"));
        assert!(!summary.contains("Added"));
    }
}
