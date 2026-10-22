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
        let problems: Vec<&DoctorRow> = rows
            .iter()
            .filter(|row| UNHEALTHY_STATUSES.contains(&row.status.as_str()))
            .collect();
        let suggestions: Vec<&DoctorRow> = rows
            .iter()
            .filter(|row| ATTENTION_STATUSES.contains(&row.status.as_str()))
            .collect();
        if problems.is_empty() && suggestions.is_empty() {
            let _ = writeln!(
                out,
                "{}",
                paint(Stream::Out, Tone::Success, "Everything is working.")
            );
        }
        // Problems fail the check. Suggestions do not.
        for (heading, tone, issues) in [
            ("Problems:", Tone::Error, problems),
            ("Suggestions:", Tone::Warning, suggestions),
        ] {
            if issues.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{}", paint(Stream::Out, Tone::Strong, heading));
            for row in issues {
                let _ = writeln!(
                    out,
                    "  {}: {}",
                    paint(Stream::Out, tone, &row.component),
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

/// Whether `--verbose` is on, so errors keep the full native output.
pub fn verbose() -> bool {
    PLAIN.load(std::sync::atomic::Ordering::Relaxed)
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

/// One output stream of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output: results.
    Out,
    /// Standard error: progress, notes, and diagnostics.
    Err,
}

impl Stream {
    fn is_terminal(self) -> bool {
        use std::io::IsTerminal as _;
        match self {
            Self::Out => std::io::stdout().is_terminal(),
            Self::Err => std::io::stderr().is_terminal(),
        }
    }

    const fn fd(self) -> i32 {
        match self {
            Self::Out => 1,
            Self::Err => 2,
        }
    }
}

/// The meaning of a piece of text. Terminals show it as a color or weight;
/// pipes, logs, and `--no-color` get the plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Names and headings.
    Strong,
    /// A completed change.
    Success,
    /// A warning or partial result.
    Warning,
    /// A failure.
    Error,
    /// Secondary detail, such as descriptions and column headings.
    Muted,
    /// A command for the user to run.
    Accent,
}

impl Tone {
    const fn code(self) -> &'static str {
        match self {
            Self::Strong => "1",
            Self::Success => "32",
            Self::Warning => "33",
            Self::Error => "1;31",
            Self::Muted => "2",
            Self::Accent => "36",
        }
    }
}

/// Set by `--no-color` or `display.color = "never"`.
static COLOR_DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn off color for this process.
pub fn disable_color(disabled: bool) {
    COLOR_DISABLED.store(disabled, std::sync::atomic::Ordering::Relaxed);
}

/// Whether text written to `stream` can use color.
///
/// Color needs a terminal. `--no-color`, the configuration, a nonempty
/// `NO_COLOR`, and `TERM=dumb` turn it off.
#[must_use]
pub fn color(stream: Stream) -> bool {
    !COLOR_DISABLED.load(std::sync::atomic::Ordering::Relaxed)
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
        && stream.is_terminal()
}

/// Show `text` in `tone` when `stream` can use color.
#[must_use]
pub fn paint(stream: Stream, tone: Tone, text: &str) -> String {
    if text.is_empty() || !color(stream) {
        return text.to_string();
    }
    format!("\x1b[{}m{text}\x1b[0m", tone.code())
}

/// The column count of the terminal behind `stream`.
///
/// A numeric `COLUMNS` value wins, also for pipes. Without it, pipes and
/// logs have no width, so their lines are not cut.
#[must_use]
pub fn width(stream: Stream) -> Option<usize> {
    if let Some(columns) = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|columns| *columns > 0)
    {
        return Some(columns);
    }
    if !stream.is_terminal() {
        return None;
    }
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` into the valid pointer and
    // reads nothing else; the descriptor is a standard stream.
    let status = unsafe { libc::ioctl(stream.fd(), libc::TIOCGWINSZ, &raw mut size) };
    (status == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// The visible width of `text`, without ANSI escape sequences.
#[must_use]
pub fn visible_width(text: &str) -> usize {
    strip_ansi(text).chars().count()
}

/// Rows of text columns for a terminal.
///
/// Columns are padded to the widest visible cell. On a terminal, the last
/// column is cut to fit the line width. Pipes and logs get full lines.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    optional_last: bool,
}

/// The smallest room that an optional last column keeps.
const OPTIONAL_LAST_WIDTH: usize = 12;

impl Table {
    /// Start a table with these column headings. Empty headings print no
    /// heading line.
    #[must_use]
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(ToString::to_string).collect(),
            rows: Vec::new(),
            optional_last: false,
        }
    }

    /// Drop the last column on a terminal that is too narrow to show it,
    /// instead of cutting every cell to `…`.
    #[must_use]
    pub fn optional_last(mut self) -> Self {
        self.optional_last = true;
        self
    }

    /// Add one row. Cells may hold ANSI color.
    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    /// Whether the table has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Render the table for `stream`.
    #[must_use]
    pub fn render(&self, stream: Stream) -> String {
        let columns = self
            .rows
            .iter()
            .map(Vec::len)
            .chain([self.headers.len()])
            .max()
            .unwrap_or(0);
        let mut widths = vec![0; columns];
        let show_headers = self.headers.iter().any(|header| !header.is_empty());
        let header_cells = show_headers.then_some(&self.headers);
        for cells in header_cells.into_iter().chain(&self.rows) {
            for (index, cell) in cells.iter().enumerate() {
                widths[index] = widths[index].max(visible_width(cell));
            }
        }
        // Drop columns that are empty in every row and heading.
        let mut keep: Vec<bool> = widths.iter().map(|width| *width > 0).collect();
        let limit = width(stream);
        if self.optional_last
            && let Some(limit) = limit
            && let Some(last) = keep.iter().rposition(|kept| *kept)
        {
            let used: usize = (0..last)
                .filter(|index| keep[*index])
                .map(|index| widths[index] + 2)
                .sum();
            if limit.saturating_sub(used) < OPTIONAL_LAST_WIDTH.min(widths[last]) {
                keep[last] = false;
            }
        }
        let mut out = String::new();
        let mut line = |cells: &[String], muted: bool| {
            let mut text = String::new();
            let mut used = 0;
            let last = (0..cells.len()).rev().find(|index| keep[*index]);
            for (index, cell) in cells.iter().enumerate() {
                if !keep[index] {
                    continue;
                }
                if !text.is_empty() {
                    text.push_str("  ");
                    used += 2;
                }
                if Some(index) == last {
                    let room = limit.map(|limit| limit.saturating_sub(used));
                    let cell = match room {
                        Some(room) if visible_width(cell) > room => {
                            fit(&strip_ansi(cell), room.max(1))
                        }
                        _ => cell.clone(),
                    };
                    text.push_str(&cell);
                } else {
                    text.push_str(cell);
                    let pad = widths[index].saturating_sub(visible_width(cell));
                    text.push_str(&" ".repeat(pad));
                    used += widths[index];
                }
            }
            let text = text.trim_end();
            if muted {
                let _ = writeln!(out, "{}", paint(stream, Tone::Muted, text));
            } else {
                let _ = writeln!(out, "{text}");
            }
        };
        if let Some(headers) = header_cells {
            line(headers, true);
        }
        for cells in &self.rows {
            line(cells, false);
        }
        out
    }
}

/// Cut `text` to at most `max` characters, ending with `…` when cut.
#[must_use]
pub fn fit(text: &str, max: usize) -> String {
    truncate(text, max)
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

/// The name of an official search source, such as `Homebrew Casks`.
#[must_use]
pub fn source_title(source: &str) -> Option<&'static str> {
    if source.contains("dir=nix/casks") {
        Some("Homebrew Casks")
    } else if source.contains("dir=nix/formulae") {
        Some("Homebrew Formulae")
    } else if source.contains("nixpkgs") {
        Some("Nixpkgs")
    } else {
        None
    }
}

/// A short form of a source reference, such as `nixpkgs` or `owner/repo`.
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
/// The short name that the user types for a package ID.
///
/// `nixpkgs:ripgrep` gives `ripgrep`. `cask:homebrew/cask/stats` and
/// `brew:homebrew/core/jq` give the token. Other IDs stay complete.
#[must_use]
pub fn short_package_name(id: &str) -> String {
    if let Some(attribute) = id.strip_prefix("nixpkgs:") {
        return attribute.to_string();
    }
    if let Some(full) = id
        .strip_prefix("cask:")
        .or_else(|| id.strip_prefix("brew:"))
    {
        return full.rsplit('/').next().unwrap_or(full).to_string();
    }
    id.to_string()
}

/// The source label of a package ID: empty for Nixpkgs, `cask` for
/// official Casks, `brew` for Formulae, `owner/tap` for a saved tap, and
/// `source` for other IDs.
fn source_label(id: &str, source: &str) -> String {
    if id.starts_with("nixpkgs:") {
        return String::new();
    }
    if id.starts_with("brew:") {
        return String::from("brew");
    }
    if let Some(full) = id.strip_prefix("cask:") {
        return match full.rsplit_once('/') {
            Some(("homebrew/cask", _)) | None => String::from("cask"),
            Some((tap, _)) => tap.to_string(),
        };
    }
    source.to_string()
}

/// The name column of one list row. A flake entry shows its entry name,
/// because the source column names the flake.
fn list_name(row: &ListRow) -> String {
    if row.package_id.contains('#') {
        row.name.clone()
    } else {
        short_package_name(&row.package_id)
    }
}

/// Render list rows for humans, sorted by name.
///
/// The entry ID column shows only for duplicate packages or with
/// `--verbose`.
#[must_use]
pub fn render_list(rows: &[ListRow], verbose: bool) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        let _ = writeln!(out, "No packages installed.");
        let _ = writeln!(
            out,
            "Next: {}",
            paint(Stream::Out, Tone::Accent, "pkg search QUERY")
        );
        return out;
    }
    let mut ids: Vec<&str> = rows.iter().map(|row| row.package_id.as_str()).collect();
    ids.sort_unstable();
    let duplicates = ids.windows(2).any(|pair| pair[0] == pair[1]);
    let entries = verbose || duplicates;
    let mut sorted: Vec<&ListRow> = rows.iter().collect();
    sorted.sort_by_cached_key(|row| (list_name(row), row.entry_id.clone()));
    let mut table = Table::new(if entries {
        &["NAME", "VERSION", "SOURCE", "ENTRY ID"]
    } else {
        &["NAME", "VERSION", "SOURCE"]
    });
    for row in sorted {
        let mut cells = vec![
            paint(Stream::Out, Tone::Strong, &list_name(row)),
            row.version.clone().unwrap_or_default(),
            source_label(&row.package_id, &row.source),
        ];
        if entries {
            cells.push(row.entry_id.clone());
        }
        table.row(cells);
    }
    out.push_str(&table.render(Stream::Out));
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
    /// The version from the store path name, when known.
    pub version: Option<String>,
    /// The original moving reference the entry was installed from.
    pub source: String,
    /// The locked source reference that Nix resolved.
    pub locked_source: String,
    /// The locked revision, when present.
    pub revision: Option<String>,
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

/// Remove control characters and direction overrides from third-party text.
///
/// Catalog and flake text then cannot move the cursor, change the terminal,
/// or reorder what the user reads. Line breaks and tabs become spaces.
#[must_use]
pub fn printable(text: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() || direction_control(c);
    if !text.chars().any(unsafe_char) {
        return text.into();
    }
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if unsafe_char(c) => None,
            c => Some(c),
        })
        .collect::<String>()
        .into()
}

/// Whether `c` reorders text that a terminal shows.
pub(crate) const fn direction_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Clean text from Nix or a flake for one terminal write.
///
/// Keep line breaks, tabs, and color codes. Remove other escape sequences,
/// other control characters, and direction overrides, so the text cannot
/// move the cursor, set the window title, write the clipboard, or hide
/// part of a line.
#[must_use]
pub fn terminal_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !text
        .chars()
        .any(|c| (c.is_control() && !matches!(c, '\n' | '\t')) || direction_control(c))
    {
        return text.into();
    }
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => clean.push(c),
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut parameters = String::new();
                    while let Some(next) = chars.next_if(|c| ('\x20'..='\x3f').contains(c)) {
                        parameters.push(next);
                    }
                    if chars.next_if(|c| ('\x40'..='\x7e').contains(c)) == Some('m')
                        && parameters
                            .bytes()
                            .all(|b| b.is_ascii_digit() || b == b';' || b == b':')
                    {
                        clean.push_str("\x1b[");
                        clean.push_str(&parameters);
                        clean.push('m');
                    }
                }
                // Operating system commands and device strings run to BEL or ST.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(next) = chars.next() {
                        if next == '\x07' || (next == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() || direction_control(c) => {}
            c => clean.push(c),
        }
    }
    clean.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_pad_columns_and_skip_empty_ones() {
        let mut table = Table::new(&["NAME", "", "VERSION"]);
        table.row(vec![
            String::from("ripgrep"),
            String::new(),
            String::from("15.2.0"),
        ]);
        table.row(vec![
            String::from("jq"),
            String::new(),
            String::from("1.8.2"),
        ]);
        // Tests write to a pipe, so no width applies and no color is added.
        assert_eq!(
            table.render(Stream::Out),
            "NAME     VERSION\nripgrep  15.2.0\njq       1.8.2\n"
        );
    }

    #[test]
    fn tables_without_headings_print_rows_only() {
        let mut table = Table::new(&[]);
        table.row(vec![String::from("a"), String::from("b")]);
        assert_eq!(table.render(Stream::Out), "a  b\n");
    }

    #[test]
    fn printable_removes_terminal_controls() {
        assert_eq!(printable("ripgrep"), "ripgrep");
        assert_eq!(
            printable("a\x1b]8;;https://x\x07link\x1b]8;;\x07\rfake\u{9b}2K\u{202e}txt\tend"),
            "a]8;;https://xlink]8;; fake2Ktxt end"
        );
    }

    #[test]
    fn terminal_text_keeps_lines_and_colors_only() {
        assert_eq!(terminal_text("plain\n\ttext"), "plain\n\ttext");
        assert_eq!(
            terminal_text("\x1b[1;31merror:\x1b[0m x\x1b]0;title\x07y\x1b]52;c;aGk=\x1b\\z"),
            "\x1b[1;31merror:\x1b[0m xyz"
        );
        assert_eq!(
            terminal_text("ok\rfake\x1b[2K\x1b[3Aup\x1bcreset\u{9b}31m\u{202e}txt\x1b"),
            "okfakeupreset31mtxt"
        );
        assert_eq!(terminal_text("\x1b]8;;https://x unterminated"), "");
    }

    #[test]
    fn visible_width_ignores_color() {
        assert_eq!(visible_width("\x1b[1;31mfailed\x1b[0m"), 6);
        assert_eq!(fit("abcdef", 4), "abc…");
    }
}
