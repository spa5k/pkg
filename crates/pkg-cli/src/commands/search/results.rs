//! Bounded search result storage and output.
use super::super::CommandError;
use crate::{catalog, output};

pub(super) struct SearchSpool {
    _file: tempfile::NamedTempFile,
    connection: rusqlite::Connection,
    pub(super) count: usize,
    limit: Option<u32>,
    worst: std::collections::BTreeMap<String, (u32, String)>,
    sections: std::collections::BTreeMap<String, usize>,
    grouped: bool,
    pub(super) fuzzy: bool,
}

impl SearchSpool {
    pub(super) fn new(limit: Option<u32>, grouped: bool) -> Result<Self, CommandError> {
        let file = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        let connection =
            rusqlite::Connection::open(file.path()).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF;
            CREATE TABLE results(rank INTEGER, id TEXT, body TEXT, section TEXT);
            CREATE INDEX result_order ON results(rank, id);
            CREATE INDEX section_order ON results(section, rank, id);
            BEGIN;",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            _file: file,
            connection,
            count: 0,
            limit,
            worst: std::collections::BTreeMap::new(),
            sections: std::collections::BTreeMap::new(),
            grouped,
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
            self.connection
                .execute("DELETE FROM results", [])
                .map_err(catalog::CatalogError::cache_write)?;
            self.count = 0;
            self.worst.clear();
            self.sections.clear();
        } else if fuzzy && self.count > 0 && !self.fuzzy {
            return Ok(());
        }
        self.fuzzy = fuzzy;
        let section = match row.source.as_str() {
            "nixpkgs" => String::from("Nixpkgs"),
            "brew" => String::from("Homebrew Formulae"),
            _ => {
                let source = row
                    .id
                    .strip_prefix("cask:")
                    .unwrap_or(&row.id)
                    .rsplit_once('/')
                    .map_or(row.source.as_str(), |(source, _)| source);
                if source == "homebrew/cask" {
                    String::from("Homebrew Casks")
                } else {
                    format!("Cask tap: {source}")
                }
            }
        };
        self.count += 1;
        let section_count = self.sections.entry(section.clone()).or_default();
        *section_count += 1;
        let retained_count = if self.grouped {
            *section_count
        } else {
            self.count
        };
        let key = if self.grouped {
            section.clone()
        } else {
            String::new()
        };
        if self.worst.get(&key).is_some_and(|(worst_rank, worst_id)| {
            (rank, row.id.as_str()) > (*worst_rank, worst_id.as_str())
        }) {
            return Ok(());
        }
        let body = serde_json::to_string(row).map_err(catalog::CatalogError::cache_write)?;
        self.connection
            .execute(
                "INSERT INTO results VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![rank, row.id, body, section],
            )
            .map_err(catalog::CatalogError::cache_write)?;
        if let Some(limit) = self.limit
            && retained_count >= limit as usize
        {
            let (remove, select) = if self.grouped {
                (
                    "DELETE FROM results WHERE section=?2 AND rowid NOT IN (SELECT rowid FROM results WHERE section=?2 ORDER BY rank, id LIMIT ?1)",
                    "SELECT rank, id FROM results WHERE section=?2 ORDER BY rank DESC, id DESC LIMIT ?1",
                )
            } else {
                (
                    "DELETE FROM results WHERE rowid NOT IN (SELECT rowid FROM results ORDER BY rank, id LIMIT ?1)",
                    "SELECT rank, id FROM results ORDER BY rank DESC, id DESC LIMIT ?1",
                )
            };
            let bindings: &[&dyn rusqlite::ToSql] = if self.grouped {
                &[&limit, &section]
            } else {
                &[&limit]
            };
            self.connection
                .execute(remove, bindings)
                .map_err(catalog::CatalogError::cache_write)?;
            let one = 1;
            let bindings: &[&dyn rusqlite::ToSql] = if self.grouped {
                &[&one, &section]
            } else {
                &[&one]
            };
            let worst = self
                .connection
                .query_row(select, bindings, |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(catalog::CatalogError::cache_write)?;
            self.worst.insert(key, worst);
        }
        Ok(())
    }

    pub(super) fn visit(
        &self,
        limit: Option<u32>,
        sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        self.visit_rows(
            "SELECT body FROM results ORDER BY rank, id LIMIT ?1",
            [limit.map(i64::from).unwrap_or(-1)],
            sink,
        )
    }

    fn visit_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        mut sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query(params).map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let body: String = row.get(0).map_err(|error| error.to_string())?;
            sink(&body).map_err(|error| error.to_string())?;
        }
        Ok(())
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
        write!(out, "{{\"schema\":1,\"command\":\"search\",\"result\":{{\"system\":{},\"sources\":{},\"total\":{},\"approximate\":{},\"results\":[", serde_json::to_string(system).map_err(|error| error.to_string())?, serde_json::to_string(reports).map_err(|error| error.to_string())?, self.count, self.fuzzy).map_err(|error| error.to_string())?;
        let mut first = true;
        self.visit(limit, |body| {
            if !first {
                out.write_all(b",")?;
            }
            first = false;
            out.write_all(body.as_bytes())
        })?;
        out.write_all(b"]}}\n")
            .and_then(|()| out.flush())
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn visit_section(
        &self,
        section: &str,
        limit: usize,
        mut sink: impl FnMut(catalog::SearchResult) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        self.visit_rows(
            "SELECT body FROM results WHERE section=?1 ORDER BY rank, id LIMIT ?2",
            rusqlite::params![
                section,
                i64::try_from(limit).map_err(|error| error.to_string())?
            ],
            |body| sink(serde_json::from_str(body).map_err(std::io::Error::other)?),
        )
    }

    pub(super) fn text(&self, query: &str, limit: Option<u32>) -> Result<(), CommandError> {
        use std::io::Write as _;
        if self.count == 0 {
            print!("{}", output::render_search(&[], query));
            return Ok(());
        }
        if self.fuzzy {
            println!("No literal matches. Showing similar package names for `{query}`.");
        }
        let mut sections: Vec<_> = self.sections.iter().collect();
        sections.sort_by_key(|(section, _)| {
            (
                match section.as_str() {
                    "Nixpkgs" => 0,
                    "Homebrew Formulae" => 1,
                    "Homebrew Casks" => 2,
                    _ => 3,
                },
                section.as_str(),
            )
        });
        let mut shown = vec![0; sections.len()];
        let mut remaining = limit.map_or(self.count, |limit| (limit as usize).min(self.count));
        if limit.is_none() {
            for ((_, count), shown) in sections.iter().zip(&mut shown) {
                *shown = **count;
            }
        } else {
            while remaining > 0 {
                for ((_, count), shown) in sections.iter().zip(&mut shown) {
                    if remaining > 0 && *shown < **count {
                        *shown += 1;
                        remaining -= 1;
                    }
                }
            }
        }
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        for (index, ((section, count), shown)) in sections.iter().zip(&shown).enumerate() {
            if index > 0 {
                writeln!(out).map_err(|error| error.to_string())?;
            }
            if shown == *count {
                let noun = if **count == 1 { "match" } else { "matches" };
                writeln!(out, "{section} ({count} {noun})").map_err(|error| error.to_string())?;
            } else {
                writeln!(out, "{section} ({shown} of {count} matches)")
                    .map_err(|error| error.to_string())?;
            }
            if *shown == 0 {
                continue;
            }
            let mut width = 2;
            let mut version_width = 7;
            let mut command = None;
            self.visit_section(section, *shown, |row| {
                width = width.max(row.id.len());
                version_width = version_width.max(row.version.len());
                if command.is_none() {
                    command = row.install_command;
                }
                Ok(())
            })?;
            writeln!(
                out,
                "{:<width$}  {:<version_width$}  DESCRIPTION",
                "ID", "VERSION"
            )
            .map_err(|error| error.to_string())?;
            self.visit_section(section, *shown, |row| {
                out.write_all(output::render_search_row(&row, width, version_width).as_bytes())
            })?;
            if let Some(command) = command {
                writeln!(out, "Install example: {}", command.join(" "))
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(limit) = limit
            && self.count > limit as usize
        {
            writeln!(
                out,
                "Showing {limit} of {} matches. Use --all or --limit N to show more.",
                self.count
            )
            .map_err(|error| error.to_string())?;
        }
        out.flush().map_err(|error| error.to_string())?;
        Ok(())
    }
}
