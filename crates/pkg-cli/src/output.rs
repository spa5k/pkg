//! Output envelopes and human presentation (design D4).
//!
//! Query commands (search, info, list, doctor) offer a small versioned JSON
//! result. Mutating commands show native progress and a short final result.

use std::fmt::Write as _;

/// The JSON result schema version for query commands.
pub const JSON_SCHEMA: u32 = 1;

/// A versioned JSON envelope for one query command.
#[derive(Debug, serde::Serialize)]
pub struct JsonEnvelope<T> {
    /// The envelope schema version.
    pub schema: u32,
    /// The command that produced this result.
    pub command: &'static str,
    /// The command result.
    pub result: T,
}

/// Build a JSON envelope for a query command.
#[must_use]
pub fn envelope<T>(command: &'static str, result: T) -> JsonEnvelope<T> {
    JsonEnvelope {
        schema: JSON_SCHEMA,
        command,
        result,
    }
}

/// One doctor observation row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DoctorRow {
    /// The component name.
    pub component: String,
    /// The observed status.
    pub status: String,
    /// Optional detail line.
    pub detail: Option<String>,
}

/// Render doctor rows for humans.
#[must_use]
pub fn render_doctor(rows: &[DoctorRow], verbose: bool) -> String {
    let mut out = String::new();
    if !verbose {
        let issues: Vec<&DoctorRow> = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.status.as_str(),
                    "missing" | "unreachable" | "unreadable" | "unhealthy"
                )
            })
            .collect();
        if issues.is_empty() {
            let _ = writeln!(out, "Healthy: no problems found.");
        } else {
            let _ = writeln!(out, "Needs attention: {} issue(s).", issues.len());
            for row in issues {
                let _ = writeln!(
                    out,
                    "  {}: {}",
                    row.component,
                    concise_detail(row.detail.as_deref().unwrap_or(&row.status))
                );
            }
        }
        return out;
    }
    for row in rows {
        let _ = writeln!(out, "{}: {}", row.component, row.status);
        if let Some(detail) = &row.detail {
            let _ = writeln!(out, "  {detail}");
        }
    }
    out
}

/// Keep one useful line from a native diagnostic in compact output.
fn concise_detail(detail: &str) -> &str {
    detail
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("error:") && !line.starts_with("error (ignored):"))
        .or_else(|| detail.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("unknown cause")
}

/// The short source label: nixpkgs, or owner/repo from a flake reference.
#[must_use]
pub fn short_source(source: &str) -> String {
    if source.contains("nixpkgs") {
        return String::from("nixpkgs");
    }
    let stripped = source.strip_prefix("github:").unwrap_or(source);
    let no_query = stripped.split('?').next().unwrap_or(stripped);
    let normalized = no_query.replace(":", "/");
    match normalized.find('/') {
        Some(first) => match normalized[first + 1..].find('/') {
            Some(second) => normalized[..first + 1 + second].to_string(),
            None => normalized,
        },
        None => normalized,
    }
}
/// Render list rows for humans.
#[must_use]
pub fn render_list(rows: &[ListRow]) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        let _ = writeln!(out, "No packages installed.");
        let _ = writeln!(out, "Next: pkg search QUERY");
        return out;
    }
    let width = rows
        .iter()
        .map(|row| row.entry_id.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let _ = writeln!(out, "{:<width$}  PACKAGE  SOURCE", "ENTRY ID");
    for row in rows {
        let is_cask = row.name.contains('/');
        let name = row.name.rsplit('/').next().unwrap_or(&row.name);
        let source = if is_cask {
            String::from("cask")
        } else {
            short_source(&row.source)
        };
        let _ = writeln!(out, "{:<width$}  {}  {}", row.entry_id, name, source);
    }
    out
}

/// One installed entry row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ListRow {
    /// The exact native entry ID.
    pub entry_id: String,
    /// The final attribute name.
    pub name: String,
    /// The original moving reference the entry was installed from.
    pub source: String,
    /// The locked source Nix resolved, reported separately (manager review 4).
    pub locked_source: String,
    /// The locked revision, when present.
    pub revision: Option<String>,
}

/// Render search rows for humans.
///
/// Rows carry their own provenance.
#[must_use]
pub fn render_search(rows: &[crate::catalog::SearchResult], query: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if rows.is_empty() {
        let _ = writeln!(out, "No matches for \"{query}\".");
        let _ = writeln!(out, "Next: Try a shorter search term.");
        return out;
    }
    let width = rows
        .iter()
        .map(|row| row.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let _ = writeln!(out, "{:<width$}  VERSION  DESCRIPTION", "ID");
    for row in rows {
        let stale = if row.stale { " [cached]" } else { "" };
        let support = match &row.support {
            Some(crate::catalog::SupportBadge::Eligible) => "",
            Some(crate::catalog::SupportBadge::Excluded { reason, .. }) => {
                &format!(" [excluded: {reason}]")
            }
            None => "",
        };
        let _ = writeln!(
            out,
            "{:<width$}  {}  {}{}{}",
            row.id,
            row.version,
            truncate(&row.description, 80),
            stale,
            support
        );
    }
    out
}

/// Render per-source provenance headers for humans.
#[must_use]
pub fn render_source_reports(reports: &[crate::catalog::SourceReport], verbose: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for report in reports {
        if !verbose && report.status.label() != "stale" {
            continue;
        }
        let label = report.display.as_deref().unwrap_or(report.source.as_str());
        if verbose {
            let revision = report.revision.as_deref().unwrap_or("moving, no revision");
            let _ = writeln!(
                out,
                "source: {label} ({revision}) [{}]",
                report.status.label()
            );
        } else {
            let _ = writeln!(out, "Warning: {label} uses cached results.");
        }
        if !verbose {
            continue;
        }
        if let Some(detail) = &report.detail {
            let _ = writeln!(out, "  {detail}");
        }
        if let Some(detail) = &report.support_detail {
            let _ = writeln!(out, "  {detail}");
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Remove ANSI escape sequences from native text.
///
/// `--no-color` must hold for text pkg passes through from Nix (native
/// history) even when the runtime colorizes anyway (manager review 4).
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                // A CSI sequence ends at its final alphabetic byte.
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_is_versioned() {
        let json = serde_json::to_value(envelope(
            "list",
            vec![ListRow {
                entry_id: String::from("ripgrep"),
                name: String::from("ripgrep"),
                source: String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
                locked_source: String::from(
                    "github:NixOS/nixpkgs/0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a",
                ),
                revision: Some(String::from("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a")),
            }],
        ))
        .expect("serializes");
        assert_eq!(json["schema"], JSON_SCHEMA);
        assert_eq!(json["command"], "list");
        assert_eq!(json["result"][0]["entry_id"], "ripgrep");
        assert_eq!(
            json["result"][0]["locked_source"],
            "github:NixOS/nixpkgs/0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"
        );
    }

    #[test]
    fn ansi_sequences_are_stripped_from_native_text() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m plain"), "red plain");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
        assert_eq!(strip_ansi("\u{1b}[1;32mgen 3\u{1b}[m"), "gen 3");
        // An unterminated sequence consumes nothing beyond its end.
        assert_eq!(strip_ansi("\u{1b}["), "");
        assert_eq!(strip_ansi("plain\u{1b}b"), "plain\u{1b}b");
    }

    #[test]
    fn human_renderers_stay_small_and_stable() {
        let rows = vec![DoctorRow {
            component: String::from("nix"),
            status: String::from("ok"),
            detail: Some(String::from("2.35.2")),
        }];
        assert_eq!(render_doctor(&rows, false), "Healthy: no problems found.\n");
        assert_eq!(render_doctor(&rows, true), "nix: ok\n  2.35.2\n");
        assert_eq!(
            render_list(&[]),
            "No packages installed.\nNext: pkg search QUERY\n"
        );
    }
}
