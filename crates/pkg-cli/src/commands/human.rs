//! Human views of validated public command results.

use std::io::{self, Write};

use serde_json::{Map, Value};

use super::execute::CommandResult;

type Column = (&'static str, &'static str);

pub(super) fn write_result(
    mut writer: impl Write,
    command: &str,
    result: &CommandResult,
) -> io::Result<()> {
    let records = result.records();
    if command == "list" && result.fields().get("nameOnly") == Some(&Value::Bool(true)) {
        for record in records {
            writeln!(writer, "{}", cell(record, "name"))?;
        }
        return Ok(());
    }
    if command == "info" {
        write_info(&mut writer, records)?;
    } else if let Some(first) = records.first() {
        let columns = columns(command, first);
        if !columns.is_empty() {
            write_table(&mut writer, records, &columns)?;
            writeln!(writer)?;
        }
    }
    writeln!(writer, "{}", result.summary())
}

fn columns(command: &str, first: &Map<String, Value>) -> Vec<Column> {
    match command {
        "list" => {
            let mut columns = vec![
                ("Package", "selector"),
                ("Version", "version"),
                ("Pinned", "pinned"),
            ];
            for column in [
                ("Outputs", "outputsToInstall"),
                ("Size (bytes)", "closureBytes"),
                ("Outdated", "outdated"),
            ] {
                if first.contains_key(column.1) {
                    columns.push(column);
                }
            }
            columns
        }
        "outdated" => vec![
            ("Package", "package"),
            ("Installed", "current"),
            ("Available", "available"),
            ("Pinned", "pinned"),
            ("Change", "kind"),
        ],
        "history" if first.get("type").and_then(Value::as_str) == Some("generation_change") => {
            vec![
                ("Package", "selector"),
                ("Change", "kind"),
                ("Before", "beforeVersion"),
                ("After", "afterVersion"),
            ]
        }
        "history" => vec![
            ("Generation", "id"),
            ("Created", "createdAt"),
            ("Operation", "operation"),
            ("Active", "active"),
        ],
        _ => Vec::new(),
    }
}

fn write_table(
    mut writer: impl Write,
    records: &[Map<String, Value>],
    columns: &[Column],
) -> io::Result<()> {
    let rows = records
        .iter()
        .map(|record| {
            columns
                .iter()
                .map(|(_, key)| cell(record, key))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let widths = columns
        .iter()
        .enumerate()
        .map(|(index, (title, _))| {
            rows.iter()
                .map(|row| row[index].chars().count())
                .max()
                .unwrap_or(0)
                .max(title.len())
        })
        .collect::<Vec<_>>();
    write_row(
        &mut writer,
        columns.iter().map(|(title, _)| *title),
        &widths,
    )?;
    for row in &rows {
        write_row(&mut writer, row.iter().map(String::as_str), &widths)?;
    }
    Ok(())
}

fn write_row<'a>(
    mut writer: impl Write,
    cells: impl Iterator<Item = &'a str>,
    widths: &[usize],
) -> io::Result<()> {
    for (index, value) in cells.enumerate() {
        if index > 0 {
            write!(writer, "  ")?;
        }
        if index + 1 == widths.len() {
            write!(writer, "{value}")?;
        } else {
            write!(writer, "{value:<width$}", width = widths[index])?;
        }
    }
    writeln!(writer)
}

fn write_info(mut writer: impl Write, records: &[Map<String, Value>]) -> io::Result<()> {
    for record in records {
        writeln!(
            writer,
            "{} {}",
            cell(record, "package"),
            cell(record, "version")
        )?;
        for (title, key) in [
            ("Description", "description"),
            ("Homepage", "homepage"),
            ("License", "licenses"),
            ("Outputs", "outputs"),
            ("Available", "available"),
            ("Broken", "broken"),
        ] {
            writeln!(writer, "  {title}: {}", cell(record, key))?;
        }
        writeln!(writer)?;
    }
    Ok(())
}

fn cell(record: &Map<String, Value>, key: &str) -> String {
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
        write_result(&mut output, command, &result).unwrap();
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
            "Size (bytes)",
            "github:casey/just#default",
            "1.58.0",
            "yes",
            "out",
            "1234",
        ] {
            assert!(output.contains(expected), "missing {expected}: {output}");
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
            write_result(&mut output, "list", &result).unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
    }

    #[test]
    fn history_outdated_and_info_show_the_data_needed_for_the_next_command() {
        let history = render(
            "history",
            &json!([{"type":"generation", "id":"gen-0002", "createdAt":"2026-09-25T00:00:00Z", "operation":"install", "active":true}]),
        );
        assert!(
            history.contains("gen-0002") && history.contains("Active") && history.contains("yes")
        );
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
