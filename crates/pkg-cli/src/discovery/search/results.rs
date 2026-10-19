//! Bounded search result storage and output.
use crate::commands::CommandError;
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
        let file = tempfile::NamedTempFile::new()?;
        let connection = rusqlite::Connection::open(file.path())?;
        let order_index = if grouped {
            "CREATE INDEX section_order ON results(section, rank, id);"
        } else {
            "CREATE INDEX result_order ON results(rank, id);"
        };
        let bucket_index = if limit.is_some() {
            "CREATE INDEX bucket_order ON results(bucket, rank, id);"
        } else {
            ""
        };
        connection.execute_batch(&format!(
            "PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; PRAGMA journal_mode=OFF;
            CREATE TABLE results(rank INTEGER, id TEXT, body TEXT, section TEXT, bucket TEXT);
            {order_index}
            {bucket_index}
            BEGIN;"
        ))?;
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
            self.connection.execute("DELETE FROM results", [])?;
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
            .prepare_cached("INSERT INTO results VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(rusqlite::params![rank, row.id, body, section, key])?;
        if let Some(limit) = self.limit
            && retained_count >= limit as usize
        {
            self.connection
                .execute(
                    "DELETE FROM results WHERE bucket=?2 AND rowid NOT IN (SELECT rowid FROM results WHERE bucket=?2 ORDER BY rank, id LIMIT ?1)",
                    rusqlite::params![limit, key],
                )?;
            let worst = self.connection.query_row(
                "SELECT rank, id FROM results WHERE bucket=?1 ORDER BY rank DESC, id DESC LIMIT 1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
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
            [limit.map_or(-1, i64::from)],
            sink,
        )
    }

    fn visit_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        mut sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(params)?;
        while let Some(row) = rows.next()? {
            let body: String = row.get(0)?;
            sink(&body)?;
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

    fn visit_section(
        &self,
        section: &str,
        limit: usize,
        mut sink: impl FnMut(catalog::SearchResult) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        self.visit_rows(
            "SELECT body FROM results WHERE section=?1 ORDER BY rank, id LIMIT ?2",
            rusqlite::params![section, i64::try_from(limit)?],
            |body| sink(serde_json::from_str(body).map_err(std::io::Error::other)?),
        )
    }

    pub(super) fn text(&self, query: &str, limit: Option<u32>) -> Result<(), CommandError> {
        use std::io::Write as _;
        if self.count == 0 {
            println!("No matches for \"{query}\".");
            println!("Next: Try fewer search words or use --broad.");
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
                writeln!(out)?;
            }
            if shown == *count {
                let noun = if **count == 1 { "match" } else { "matches" };
                writeln!(out, "{section} ({count} {noun})")?;
            } else {
                writeln!(out, "{section} ({shown} of {count} matches)")?;
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
            )?;
            self.visit_section(section, *shown, |row| {
                out.write_all(output::render_search_row(&row, width, version_width).as_bytes())
            })?;
            if let Some(command) = command {
                writeln!(out, "Install example: {}", command.join(" "))?;
            }
        }
        if let Some(limit) = limit
            && self.count > limit as usize
        {
            writeln!(
                out,
                "Showing {limit} of {} matches. Use --all or --limit N to show more.",
                self.count
            )?;
        }
        out.flush()?;
        Ok(())
    }
}
