//! Bounded search result storage and output.
use std::collections::{BTreeMap, BTreeSet};

use crate::commands::CommandError;
use crate::output::{self, Stream, Table, Tone};
use crate::{catalog, nix};

/// Stored search rows in relevance order.
///
/// Human output merges rows with the same name from several sources into
/// the row that a bare name selects: sources arrive in choice order, so
/// the first available row for a name wins. JSON output keeps every row.
pub(super) struct SearchSpool {
    _file: tempfile::NamedTempFile,
    connection: rusqlite::Connection,
    pub(super) count: usize,
    limit: Option<u32>,
    worst: Option<(u32, String)>,
    merged: bool,
    /// Merged names and whether their stored row can install here.
    names: BTreeMap<String, bool>,
    pub(super) fuzzy: bool,
}

/// Rank added in human output to a row that cannot install here, so the
/// exact names that can install come first.
const UNAVAILABLE_RANK: u32 = 1_000_000;

/// How rows render for humans: the name that installs a row, and which
/// rows are installed.
pub(super) struct Display<'a> {
    pub(super) names: Option<&'a catalog::Names>,
    pub(super) installed: &'a BTreeSet<String>,
}

impl SearchSpool {
    pub(super) fn new(limit: Option<u32>, merged: bool) -> Result<Self, CommandError> {
        let file = tempfile::NamedTempFile::new()?;
        let connection = rusqlite::Connection::open(file.path())?;
        connection.execute_batch(
            "PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF;
            CREATE TABLE results(rank INTEGER, id TEXT, body TEXT, name TEXT);
            CREATE INDEX result_order ON results(rank, id);
            CREATE INDEX result_names ON results(name);
            BEGIN;",
        )?;
        Ok(Self {
            _file: file,
            connection,
            count: 0,
            limit,
            worst: None,
            merged,
            names: BTreeMap::new(),
            fuzzy: false,
        })
    }

    pub(super) fn push(
        &mut self,
        row: &catalog::SearchResult,
        rank: u32,
    ) -> Result<(), catalog::CatalogError> {
        let fuzzy = rank >= catalog::FUZZY_RANK;
        if !fuzzy && self.fuzzy {
            self.connection.execute("DELETE FROM results", [])?;
            self.count = 0;
            self.worst = None;
            self.names.clear();
        } else if fuzzy && self.count > 0 && !self.fuzzy {
            return Ok(());
        }
        self.fuzzy = fuzzy;
        let name = row_name(row).to_lowercase();
        let available = !matches!(
            row.support,
            Some(catalog::SupportBadge::Excluded { .. } | catalog::SupportBadge::Unknown { .. })
        );
        let mut rank = rank;
        if self.merged {
            match self.names.get(&name) {
                // An earlier source already owns this name.
                Some(true) => return Ok(()),
                Some(false) if !available => return Ok(()),
                // An installable row replaces an unavailable one.
                Some(false) => {
                    let removed = self
                        .connection
                        .execute("DELETE FROM results WHERE name=?1", [&name])?;
                    if removed > 0 {
                        // The table now has room below the limit.
                        self.worst = None;
                    }
                    self.count -= 1;
                }
                None => {}
            }
            self.names.insert(name.clone(), available);
            if !available {
                rank = rank.saturating_add(UNAVAILABLE_RANK);
            }
        }
        self.count += 1;
        if self.worst.as_ref().is_some_and(|(worst_rank, worst_id)| {
            (rank, row.id.as_str()) > (*worst_rank, worst_id.as_str())
        }) {
            return Ok(());
        }
        let body = serde_json::to_string(row).map_err(catalog::CatalogError::cache_write)?;
        self.connection
            .prepare_cached("INSERT INTO results VALUES (?1, ?2, ?3, ?4)")?
            .execute(rusqlite::params![rank, row.id, body, name])?;
        if let Some(limit) = self.limit
            && self.count >= limit as usize
        {
            self.connection.execute(
                "DELETE FROM results WHERE rowid NOT IN (SELECT rowid FROM results ORDER BY rank, id LIMIT ?1)",
                [limit],
            )?;
            self.worst = Some(self.connection.query_row(
                "SELECT rank, id FROM results ORDER BY rank DESC, id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?);
        }
        Ok(())
    }

    pub(super) fn visit(
        &self,
        limit: Option<u32>,
        mut sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM results ORDER BY rank, id LIMIT ?1")?;
        let mut rows = statement.query([limit.map_or(-1, i64::from)])?;
        while let Some(row) = rows.next()? {
            let body: String = row.get(0)?;
            sink(&body)?;
        }
        Ok(())
    }

    fn rows(&self, limit: Option<u32>) -> Result<Vec<catalog::SearchResult>, CommandError> {
        let mut rows = Vec::new();
        self.visit(limit, |body| {
            rows.push(serde_json::from_str(body).map_err(std::io::Error::other)?);
            Ok(())
        })?;
        Ok(rows)
    }

    pub(super) fn json(
        &self,
        system: &str,
        reports: &[catalog::SourceReport],
        limit: Option<u32>,
    ) -> Result<(), CommandError> {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        write!(
            out,
            "{{\"schema\":1,\"command\":\"search\",\"result\":{{\"system\":{},\"sources\":{},\"total\":{},\"approximate\":{},\"results\":[",
            serde_json::to_string(system)?,
            serde_json::to_string(reports)?,
            self.count,
            self.fuzzy
        )?;
        let mut first = true;
        self.visit(limit, |body| {
            if !first {
                out.write_all(b",")?;
            }
            first = false;
            out.write_all(body.as_bytes())
        })?;
        out.write_all(b"]}}\n").and_then(|()| out.flush())?;
        Ok(())
    }

    /// Print the merged list for humans.
    pub(super) fn text(
        &self,
        query: &str,
        limit: Option<u32>,
        display: &Display<'_>,
    ) -> Result<(), CommandError> {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        if self.count == 0 {
            writeln!(out, "No matches for \"{query}\".")?;
            writeln!(out, "Try fewer words, or use --broad.")?;
            out.flush()?;
            return Ok(());
        }
        if self.fuzzy {
            writeln!(out, "No exact matches for \"{query}\". Similar names:")?;
        }
        let rows = self.rows(limit)?;
        let (table, first) = render(&rows, display, Stream::Out, true);
        out.write_all(table.as_bytes())?;
        if let Some(limit) = limit
            && self.count > limit as usize
        {
            writeln!(
                out,
                "{}",
                output::paint(
                    Stream::Out,
                    Tone::Muted,
                    &format!(
                        "Showing {limit} of {} matches. Use --all or --limit N to show more.",
                        self.count
                    )
                )
            )?;
        }
        if let Some(first) = first {
            writeln!(
                out,
                "Install: {}",
                output::paint(Stream::Out, Tone::Accent, &format!("pkg install {first}"))
            )?;
        }
        out.flush()?;
        Ok(())
    }

    /// Print up to `limit` rows on stderr after a failed name lookup.
    pub(super) fn suggestions(
        &self,
        limit: u32,
        display: &Display<'_>,
    ) -> Result<(), CommandError> {
        let rows = self.rows(Some(limit))?;
        let (table, _) = render(&rows, display, Stream::Err, false);
        for line in table.lines() {
            eprintln!("  {line}");
        }
        Ok(())
    }
}

/// Long versions such as `2024.08.24-unstable-2025-02-22` keep their start.
const VERSION_WIDTH: usize = 16;

/// Render rows as a table. Returns the table and the first installable
/// name, for the install hint.
fn render(
    rows: &[catalog::SearchResult],
    display: &Display<'_>,
    stream: Stream,
    headings: bool,
) -> (String, Option<String>) {
    let sources = rows.iter().any(|row| row.source != "nixpkgs");
    let mut table = if headings {
        Table::new(&[
            "",
            "NAME",
            "VERSION",
            if sources { "SOURCE" } else { "" },
            "DESCRIPTION",
        ])
    } else {
        Table::new(&[])
    };
    let mut first = None;
    for row in rows {
        let name = display_name(row, display.names);
        let installed = display.installed.contains(&row.attribute.replace('"', ""));
        let note = match &row.support {
            Some(catalog::SupportBadge::Excluded { reason, .. }) => Some(format!(
                "Not available here: {}",
                catalog::plain_reason(reason)
            )),
            Some(catalog::SupportBadge::Unknown { reason }) => {
                Some(format!("Availability unknown: {reason}"))
            }
            _ => None,
        };
        if first.is_none() && note.is_none() {
            first = Some(name.clone());
        }
        // Descriptions stay plain: the table removes color from a cell
        // that it cuts to fit the terminal.
        let description = note.map_or_else(
            || row.description.clone(),
            |note| output::paint(stream, Tone::Warning, &note),
        );
        table.row(vec![
            if installed {
                output::paint(stream, Tone::Success, "✓")
            } else {
                String::new()
            },
            output::paint(stream, Tone::Strong, &name),
            output::fit(&row.version, VERSION_WIDTH),
            source_label(row),
            description,
        ]);
    }
    (table.render(stream), first)
}

/// The short source label: empty for Nixpkgs.
fn source_label(row: &catalog::SearchResult) -> String {
    match row.source.as_str() {
        "nixpkgs" => String::new(),
        "brew" => String::from("brew"),
        _ => match cask_source(row) {
            Some(source) if source != nix::OFFICIAL_CASK_SOURCE => source.to_string(),
            _ => String::from("cask"),
        },
    }
}

fn cask_source(row: &catalog::SearchResult) -> Option<&str> {
    row.id
        .strip_prefix("cask:")
        .and_then(|id| id.rsplit_once('/'))
        .map(|(source, _)| source)
}

/// The name a bare request uses for this row: the exposed Nixpkgs
/// attribute or the token.
pub(super) fn row_name(row: &catalog::SearchResult) -> &str {
    let identity = row
        .id
        .split_once(':')
        .map_or(row.id.as_str(), |(_, name)| name);
    if row.source == "nixpkgs" {
        identity
    } else {
        identity.rsplit('/').next().unwrap_or(identity)
    }
}

/// The shortest ID that installs `row`: its bare name when the bare name
/// selects this row, else its short source-qualified ID.
fn display_name(row: &catalog::SearchResult, names: Option<&catalog::Names>) -> String {
    let Ok(catalog::ParsedId::Qualified(id)) = catalog::parse_id(&row.id) else {
        return row.id.clone();
    };
    let bare = row_name(row);
    if names.is_some_and(|names| names.local_choice(bare).as_ref() == Some(&id)) {
        bare.to_string()
    } else {
        catalog::short_id(&id)
    }
}
