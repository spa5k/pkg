//! JSON output and terminal text.
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

/// Doctor statuses that fail the check with a nonzero exit.
pub const UNHEALTHY_STATUSES: [&str; 4] = ["missing", "unreachable", "unreadable", "unhealthy"];

/// Doctor statuses that need the user's attention but do not fail the check.
pub const ATTENTION_STATUSES: [&str; 2] = ["off", "not on path"];

/// Render doctor rows for humans.
#[must_use]
pub fn render_doctor(rows: &[DoctorRow], verbose: bool) -> String {
    let mut out = String::new();
    if !verbose {
        let issues: Vec<&DoctorRow> = rows
            .iter()
            .filter(|row| {
                let status = row.status.as_str();
                UNHEALTHY_STATUSES.contains(&status) || ATTENTION_STATUSES.contains(&status)
            })
            .collect();
        if issues.is_empty() {
            let _ = writeln!(out, "Everything is working.");
        } else {
            let _ = writeln!(out, "Needs attention:");
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

/// Set when `--verbose` traces native commands, which a status line would hide.
static PLAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Print permanent lines instead of transient status lines.
pub fn plain(plain: bool) {
    PLAIN.store(plain, std::sync::atomic::Ordering::Relaxed);
}

/// Whether transient status lines can replace each other on stderr.
fn status_line_terminal() -> bool {
    use std::io::IsTerminal as _;
    !PLAIN.load(std::sync::atomic::Ordering::Relaxed)
        && std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
}

/// Show one transient status line on an interactive terminal.
///
/// The line replaces the previous status line. Logs and pipes receive
/// nothing, so a scheduled job does not write one line per update.
pub fn status(text: &str) {
    use std::io::Write as _;
    if status_line_terminal() {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[2K{text}");
        let _ = stderr.flush();
    }
}

/// Remove the transient status line, if one is shown.
pub fn clear_status() {
    status("");
}

/// Print one permanent stderr line below any transient status line.
pub fn note(text: &str) {
    clear_status();
    eprintln!("{text}");
}

/// Print one stderr line for logs and pipes only.
///
/// An interactive terminal shows the transient status line instead.
pub fn log(text: &str) {
    if !status_line_terminal() {
        eprintln!("{text}");
    }
}

/// Format a count with thousands separators, for example `45,231`.
#[must_use]
pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Whether `directory` is an entry of the inherited `PATH`.
#[must_use]
pub fn on_path(directory: &std::path::Path) -> bool {
    let wanted = std::fs::canonicalize(directory).ok();
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|entry| {
            entry == directory
                || wanted.as_ref().is_some_and(|wanted| {
                    std::fs::canonicalize(&entry).ok().as_ref() == Some(wanted)
                })
        })
    })
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
    let mut normalized = no_query.replace(':', "/");
    if let Some((end, _)) = normalized.match_indices('/').nth(1) {
        normalized.truncate(end);
    }
    normalized
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
        .map(|row| row.package_id.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let _ = writeln!(out, "{:<width$}  ENTRY ID", "PACKAGE");
    for row in rows {
        let _ = writeln!(out, "{:<width$}  {}", row.package_id, row.entry_id);
    }
    out
}

/// One installed entry row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ListRow {
    /// The exact native entry ID.
    pub entry_id: String,
    /// The package ID accepted by info, install, remove, and upgrade.
    pub package_id: String,
    /// The final attribute name.
    pub name: String,
    /// The original moving reference the entry was installed from.
    pub source: String,
    /// The locked source reference that Nix resolved.
    pub locked_source: String,
    /// The locked revision, when present.
    pub revision: Option<String>,
}

/// Render one search row at the shared column width.
#[must_use]
pub fn render_search_row(
    row: &crate::catalog::SearchResult,
    width: usize,
    version_width: usize,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let stale = if row.stale { " [cached]" } else { "" };
    let support = match &row.support {
        Some(crate::catalog::SupportBadge::Eligible) | None => "",
        Some(crate::catalog::SupportBadge::Unknown { reason }) => {
            &format!(" [availability unknown: {reason}]")
        }
        Some(crate::catalog::SupportBadge::Excluded { reason, .. }) => {
            &format!(" [excluded: {reason}]")
        }
    };
    let _ = writeln!(
        out,
        "{:<width$}  {:<version_width$}  {}{}{}",
        row.id,
        if row.version.is_empty() {
            "-"
        } else {
            &row.version
        },
        truncate(&row.description, 80),
        stale,
        support
    );
    out
}

/// Render per-source provenance headers for humans.
#[must_use]
pub fn render_source_reports(reports: &[crate::catalog::SourceReport], verbose: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for report in reports {
        if !verbose
            && !matches!(
                report.status,
                crate::catalog::SourceStatus::Stale | crate::catalog::SourceStatus::SkippedPlatform
            )
        {
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
            let _ = writeln!(
                out,
                "{}: {}: {}",
                if report.status == crate::catalog::SourceStatus::SkippedPlatform {
                    "Skipped"
                } else {
                    "Warning"
                },
                short_source(label),
                report
                    .detail
                    .as_deref()
                    .unwrap_or("catalog may be old; run `pkg update`")
            );
        }
        if !verbose {
            continue;
        }
        if let Some(detail) = &report.detail {
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

#[expect(
    clippy::expect_used,
    reason = "the fixed CSI pattern is valid and checked by the output tests"
)]
static CSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\x1b\[[^A-Za-z]*[A-Za-z]?").expect("valid fixed CSI pattern")
});

/// Remove ANSI escape sequences from native text.
///
/// Apply `--no-color` to Nix output, including profile history.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    CSI.replace_all(text, "").into_owned()
}
