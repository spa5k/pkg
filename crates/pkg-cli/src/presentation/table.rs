//! Width-aware tables and compact cards for narrow terminals.

use super::{Style, Tone, terminal_width, wrap};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

pub fn write_table(
    mut writer: impl Write,
    headings: &[&str],
    rows: &[Vec<String>],
    style: Style,
) -> io::Result<()> {
    write_at_width(&mut writer, headings, rows, style, terminal_width())
}

fn write_at_width(
    mut writer: impl Write,
    headings: &[&str],
    rows: &[Vec<String>],
    style: Style,
    available: usize,
) -> io::Result<()> {
    if headings.is_empty() {
        return Ok(());
    }
    let Some(widths) = table_widths(headings, rows, style, available) else {
        return write_cards(writer, headings, rows, style, available);
    };
    let gaps = 2 * (headings.len() - 1);
    let header = row_line(
        &headings.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        &widths,
    );
    writeln!(writer, "{}", style.paint(&header, Tone::Heading))?;
    if style.is_terminal() {
        writeln!(
            writer,
            "{}",
            style.paint(
                &"─".repeat(widths.iter().sum::<usize>() + gaps),
                Tone::Muted
            )
        )?;
    }
    for row in rows {
        let cells = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| wrap(cell, *width))
            .collect::<Vec<_>>();
        let height = cells.iter().map(Vec::len).max().unwrap_or(0);
        for index in 0..height {
            let cells = cells
                .iter()
                .map(|lines| lines.get(index).cloned().unwrap_or_default())
                .collect::<Vec<_>>();
            writeln!(writer, "{}", row_line(&cells, &widths))?;
        }
    }
    Ok(())
}

fn table_widths(
    headings: &[&str],
    rows: &[Vec<String>],
    style: Style,
    available: usize,
) -> Option<Vec<usize>> {
    let mut widths = headings
        .iter()
        .enumerate()
        .map(|(index, title)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|value| value.width())
                .max()
                .unwrap_or(0)
                .max(title.width())
        })
        .collect::<Vec<_>>();
    let minimum = headings
        .iter()
        .map(|title| title.width().max(12))
        .collect::<Vec<_>>();
    let gaps = 2 * (headings.len() - 1);
    if style.is_terminal() && minimum.iter().sum::<usize>() + gaps > available {
        return None;
    }
    if style.is_terminal() {
        while widths.iter().sum::<usize>() + gaps > available {
            let Some(index) = widths
                .iter()
                .enumerate()
                .filter(|(i, width)| **width > minimum[*i])
                .max_by_key(|(_, width)| **width)
                .map(|(i, _)| i)
            else {
                break;
            };
            widths[index] -= 1;
        }
    }
    Some(widths)
}

fn row_line(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (value, width))| {
            if index + 1 == widths.len() {
                value.clone()
            } else {
                format!(
                    "{value}{}  ",
                    " ".repeat(width.saturating_sub(value.width()))
                )
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn write_cards(
    mut writer: impl Write,
    headings: &[&str],
    rows: &[Vec<String>],
    style: Style,
    width: usize,
) -> io::Result<()> {
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            writeln!(writer)?;
        }
        for (column, (heading, value)) in headings.iter().zip(row).enumerate() {
            let text = if column == 0 {
                value.clone()
            } else {
                format!("{heading}: {value}")
            };
            for line in wrap(&text, width.saturating_sub(2)) {
                let line = if column == 0 {
                    style.paint(&line, Tone::Heading)
                } else {
                    line
                };
                writeln!(writer, "  {line}")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn narrow_and_wide_layouts_keep_all_data_and_fit_display_cells() {
        let rows = vec![vec![
            "github:casey/just#default".into(),
            "1.58.0".into(),
            "日本語 tools with a long description and e\u{301}".into(),
        ]];
        for width in [20, 40, 60, 80, 120] {
            let mut out = Vec::new();
            write_at_width(
                &mut out,
                &["Package", "Version", "Description"],
                &rows,
                Style::new(true, false),
                width,
            )
            .unwrap();
            let out = String::from_utf8(out).unwrap();
            assert!(
                out.lines().all(|line| line.width() <= width),
                "{width}: {out}"
            );
            assert!(out.contains("日本語"));
            assert!(out.contains("1.58.0"));
            assert!(out.contains("description"));
        }
    }
}
