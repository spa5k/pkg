//! Query commands: search, info, list.
//!
//! These commands read state and never mutate the profile. Each offers a
//! versioned JSON envelope and a short human rendering of the same data.

use std::process::ExitCode;

use crate::catalog::{self, ParsedId};
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::output::{self, ListRow};

use super::{CommandError, installed_entries, print_json, session};

pub(super) fn search(cli: &Cli, options: &crate::cli::SearchOptions) -> Result<(), CommandError> {
    let (text, source) = search_input(options)?;
    let query = catalog::SearchQuery::new(&text, options.regex, options.unavailable)
        .map_err(|error| {
            eprintln!("Invalid search pattern `{text}`: {error}");
            CommandError::Reported(ExitCode::from(super::USAGE))
        })?
        .broad(options.broad)
        .fuzzy(!options.no_fuzzy);
    let paths = crate::config::paths()?;
    let config = crate::config::Config::load(&paths.config_file)?;
    let limit = if options.all || cli.json && options.limit.is_none() {
        None
    } else {
        Some(options.limit.unwrap_or(20))
    };
    let (system, reports, rows) =
        local_search(&paths, &config, &query, source.as_deref(), limit, !cli.json)?;
    let failed = reports
        .iter()
        .any(|report| report.status == catalog::SourceStatus::Failed);
    if cli.json {
        rows.json(&system, &reports, limit)?;
    } else {
        if rows.count > 0 || !failed {
            rows.text(&text, limit)?;
        }
        eprint!("{}", output::render_source_reports(&reports, cli.verbose));
    }
    for report in reports
        .iter()
        .filter(|report| report.status == catalog::SourceStatus::Failed)
    {
        eprintln!(
            "Warning: {}: {}",
            report
                .display
                .clone()
                .unwrap_or_else(|| output::short_source(&report.source)),
            super::concise_cause(report.detail.as_deref().unwrap_or("unknown cause"))
        );
    }
    if failed {
        if rows.count == 0 {
            return Err(CommandError::Reported(ExitCode::from(super::FAILURE)));
        }
        eprintln!("Results are incomplete. Run `pkg setup` or repair the source shown above.");
    }
    Ok(())
}

fn search_input(
    options: &crate::cli::SearchOptions,
) -> Result<(String, Option<String>), CommandError> {
    let mut text = if options.regex {
        options.query.clone()
    } else {
        options.query.trim().to_string()
    };
    let mut implied = None;
    if !options.regex {
        if let Some(rest) = text.strip_prefix("nixpkgs:") {
            implied = Some(String::from("nixpkgs"));
            text = rest.to_string();
        } else if let Some(rest) = text.strip_prefix("brew:") {
            implied = Some(String::from("brew"));
            text = rest
                .strip_prefix("homebrew/core/")
                .unwrap_or(rest)
                .to_string();
        } else if let Some(rest) = text.strip_prefix("homebrew:") {
            implied = Some(String::from("homebrew"));
            text = rest.to_string();
        } else if let Some(rest) = text.strip_prefix("cask:") {
            let parts: Vec<_> = rest.splitn(3, '/').collect();
            if let [owner, tap, token] = parts.as_slice() {
                implied = Some(format!("{owner}/{tap}"));
                text = token.to_string();
            } else {
                implied = Some(String::from("cask"));
                text = rest.to_string();
            }
        }
    }
    let explicit = options.source.as_deref().map(|source| {
        if source == "homebrew/core" {
            "brew"
        } else {
            source
        }
    });
    if let (Some(implied), Some(source)) = (&implied, explicit)
        && implied != source
        && !(source == "cask" && implied.contains('/'))
        && !(implied == "cask" && source.contains('/'))
    {
        eprintln!("The query selects `{implied}`, but --source selects `{source}`.");
        return Err(CommandError::Reported(ExitCode::from(super::USAGE)));
    }
    let source = match (implied, explicit) {
        (Some(implied), Some(source)) if implied == "cask" => Some(source.to_string()),
        (Some(implied), _) => Some(implied),
        (None, source) => source.map(str::to_string),
    };
    Ok((text, source))
}

fn local_search(
    paths: &crate::config::Paths,
    config: &crate::config::Config,
    query: &catalog::SearchQuery,
    source: Option<&str>,
    limit: Option<u32>,
    grouped: bool,
) -> Result<(String, Vec<catalog::SourceReport>, SearchSpool), CommandError> {
    let system = catalog::search_system(&paths.cache_dir);
    let mut reports = Vec::new();
    let registry = if matches!(source, Some("nixpkgs" | "brew" | "homebrew/cask")) {
        crate::tap::Registry::empty()
    } else {
        match crate::tap::Registry::load(&crate::tap::registry::registry_path(&paths.state_home)) {
            Ok(registry) => registry,
            Err(error) => {
                if source
                    .is_some_and(|source| !matches!(source, "cask" | "homebrew" | "homebrew/cask"))
                {
                    return Err(error.into());
                }
                reports.push(catalog::missing_report("saved taps", &error));
                crate::tap::Registry::empty()
            }
        }
    };
    if let Some(source) = source
        && !matches!(
            source,
            "nixpkgs" | "brew" | "homebrew" | "cask" | "homebrew/cask"
        )
        && !registry.sources.contains_key(source)
    {
        return Err(format!(
            "unknown search source `{source}`. Use nixpkgs, brew, homebrew, cask, homebrew/core, homebrew/cask, or a source from `pkg tap list`."
        )
        .into());
    }
    let host = catalog::CaskHost::default();
    let mut rows = SearchSpool::new(limit, grouped)?;
    for (kind, moving) in super::configured_sources(config) {
        if source.is_some_and(|source| {
            source != kind.label()
                && !(source == "homebrew/cask" && kind == catalog::SourceKind::Cask)
                && !(source == "homebrew" && kind == catalog::SourceKind::Cask)
        }) {
            continue;
        }
        let result = match kind {
            catalog::SourceKind::Nixpkgs => catalog::search_local_native(
                &paths.cache_dir,
                moving,
                &system,
                query,
                |row, rank| rows.push(&row, rank),
            ),
            catalog::SourceKind::Cask => catalog::search_local_casks(
                &paths.cache_dir,
                moving,
                &system,
                query,
                &host,
                |row, rank| {
                    if source != Some("homebrew/cask") || row.id.starts_with("cask:homebrew/cask/")
                    {
                        rows.push(&row, rank)?;
                    }
                    Ok(())
                },
            ),
        };
        reports.push(
            result.unwrap_or_else(|error| catalog::missing_report(moving, &error.to_string())),
        );
    }
    if source.is_none() || matches!(source, Some("brew" | "homebrew")) {
        let result = catalog::brew::search(
            &paths.cache_dir,
            &config.sources.formulae,
            &system,
            query,
            |row, rank| rows.push(&row, rank),
        );
        reports.push(result.unwrap_or_else(|error| {
            catalog::missing_report(&config.sources.formulae, &error.to_string())
        }));
    }
    for tap_source in registry.sources.keys() {
        if source
            .is_some_and(|source| source != "cask" && source != "homebrew" && source != tap_source)
        {
            continue;
        }
        let result = crate::tap::SavedCatalogs::load_one(&registry, &paths.state_home, tap_source)
            .map_err(catalog::CatalogError::Source)
            .and_then(|saved| {
                catalog::search_local_saved(&saved, &system, query, &host, |row, rank| {
                    rows.push(&row, rank)
                })
            });
        reports.push(
            result.unwrap_or_else(|error| catalog::missing_report(tap_source, &error.to_string())),
        );
    }
    Ok((system, reports, rows))
}

pub(super) fn install_resolution_error(
    session: &super::Session,
    name: &str,
    source: Option<&str>,
    error: catalog::CatalogError,
) -> CommandError {
    if !matches!(
        error,
        catalog::CatalogError::NotFound(_)
            | catalog::CatalogError::Ambiguous { .. }
            | catalog::CatalogError::RequiresId { .. }
    ) {
        return error.into();
    }
    let suggestions = catalog::SearchQuery::new(name, false, false)
        .ok()
        .and_then(|query| {
            local_search(
                &session.paths,
                &session.config,
                &query,
                source,
                Some(5),
                false,
            )
            .ok()
        })
        .map(|(_, _, rows)| rows)
        .filter(|rows| rows.count > 0);
    if suggestions.is_some() {
        match &error {
            catalog::CatalogError::Ambiguous { choices, .. } => eprintln!(
                "pkg: `{name}` matches {} exact package IDs. Choose an ID.",
                choices.len()
            ),
            catalog::CatalogError::RequiresId { .. } => eprintln!(
                "pkg: `{name}` needs an exact package ID for its development package or variant."
            ),
            _ => eprintln!("pkg: {error}"),
        }
    } else {
        eprintln!("pkg: {error}");
    }
    if let Some(rows) = suggestions {
        eprintln!(
            "{}",
            if rows.fuzzy {
                "Similar package names:"
            } else {
                "Related packages:"
            }
        );
        let _ = rows.visit(Some(5), |body| {
            let row: catalog::SearchResult =
                serde_json::from_str(body).map_err(std::io::Error::other)?;
            eprint!(
                "  {}",
                output::render_search_row(&row, row.id.len(), row.version.len().max(1))
            );
            Ok(())
        });
        eprintln!("Choose a package ID, then run `pkg install ID`.");
    }
    if matches!(
        error,
        catalog::CatalogError::Ambiguous { .. } | catalog::CatalogError::RequiresId { .. }
    ) {
        eprintln!("Use `pkg search {name} --broad` to inspect other package IDs.");
    } else {
        eprintln!("Use `pkg search {name}` to find packages.");
    }
    CommandError::Reported(ExitCode::from(super::FAILURE))
}

struct SearchSpool {
    _file: tempfile::NamedTempFile,
    connection: rusqlite::Connection,
    count: usize,
    limit: Option<u32>,
    worst: std::collections::BTreeMap<String, (u32, String)>,
    sections: std::collections::BTreeMap<String, usize>,
    grouped: bool,
    fuzzy: bool,
}

impl SearchSpool {
    fn new(limit: Option<u32>, grouped: bool) -> Result<Self, CommandError> {
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

    fn push(
        &mut self,
        row: &catalog::SearchResult,
        rank: u32,
    ) -> Result<(), catalog::CatalogError> {
        let fuzzy = rank >= catalog::FUZZY_RANK;
        if !fuzzy && self.fuzzy {
            self.connection
                .execute("DELETE FROM results", [])
                .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
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
        let body = serde_json::to_string(row)
            .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
        self.connection
            .execute(
                "INSERT INTO results VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![rank, row.id, body, section],
            )
            .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
        if let Some(limit) = self.limit
            && retained_count >= limit as usize
        {
            let worst = if self.grouped {
                self.connection.execute("DELETE FROM results WHERE section=?2 AND rowid NOT IN (SELECT rowid FROM results WHERE section=?2 ORDER BY rank, id LIMIT ?1)", rusqlite::params![limit, section])
                    .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
                self.connection.query_row("SELECT rank, id FROM results WHERE section=?1 ORDER BY rank DESC, id DESC LIMIT 1", [&section], |row| Ok((row.get(0)?, row.get(1)?)))
            } else {
                self.connection.execute("DELETE FROM results WHERE rowid NOT IN (SELECT rowid FROM results ORDER BY rank, id LIMIT ?1)", [limit])
                    .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
                self.connection.query_row(
                        "SELECT rank, id FROM results ORDER BY rank DESC, id DESC LIMIT 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
            }.map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
            self.worst.insert(key, worst);
        }
        Ok(())
    }

    fn visit(
        &self,
        limit: Option<u32>,
        mut sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM results ORDER BY rank, id LIMIT ?1")
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query([limit.map(i64::from).unwrap_or(-1)])
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let body: String = row.get(0).map_err(|error| error.to_string())?;
            sink(&body).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn json(
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
        let mut statement = self
            .connection
            .prepare("SELECT body FROM results WHERE section=?1 ORDER BY rank, id LIMIT ?2")
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query(rusqlite::params![
                section,
                i64::try_from(limit).map_err(|error| error.to_string())?
            ])
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let body: String = row.get(0).map_err(|error| error.to_string())?;
            sink(serde_json::from_str(&body).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn text(&self, query: &str, limit: Option<u32>) -> Result<(), CommandError> {
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

/// Collected data for one info result.
#[derive(Debug)]
struct InfoData {
    /// The requested attribute or token.
    attribute: String,
    /// The actual full matched native attribute, when a derivation lookup
    /// ran.
    matched_attribute: Option<String>,
    /// Evaluated package metadata, when a derivation lookup ran.
    meta: Option<nix::SearchMeta>,
    /// Source provenance for the result.
    report: catalog::SourceReport,
    /// Cask classification, when the ID is a cask token.
    cask: Option<CaskDisplay>,
    availability: Option<catalog::Availability>,
    formula_kind: Option<String>,
}

pub(super) fn info(cli: &Cli, id: &str) -> Result<(), CommandError> {
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let parsed = catalog::parse_id(id)?;
    let session = session(cli)?;
    let system = session.nix.system()?;
    let mut catalog_handle = catalog::CatalogOnce::new(&session.nix, &session.config.sources.casks);
    let formula_view = if matches!(&parsed, ParsedId::Qualified(catalog::CatalogId::Formula(_))) {
        Some(catalog::brew::FormulaView::load(
            &session.nix,
            &session.config.sources.formulae,
        )?)
    } else {
        None
    };
    let tap_state = if formula_view.is_some() {
        super::TapState::official(&session)
    } else {
        super::TapState::load(&session)?
    };
    // Qualified IDs resolve without reading any saved tap; bare names
    // load every source so their resolution stays strict.
    let mut resolved = match parsed {
        ParsedId::Qualified(qualified) => qualified,
        ParsedId::Bare(name) => {
            let saved = tap_state.saved_all()?;
            catalog::resolve_bare(
                &session.nix,
                &session.paths.cache_dir,
                &tap_state.routing,
                &saved,
                &name,
                &system,
                &mut catalog_handle,
            )?
        }
        ParsedId::BareCask(token) => {
            let saved = tap_state.saved_all()?;
            catalog::resolve_bare_cask(&session.nix, &saved, &token, &system, &mut catalog_handle)?
        }
    };
    if let (catalog::CatalogId::Formula(name), Some(view)) = (&resolved, &formula_view) {
        resolved = catalog::CatalogId::Formula(view.resolve(&system, name)?);
    }
    let (reference, attribute) = resolved
        .source_and_attribute(&tap_state.routing, &system)
        .map_err(CommandError::Message)?;
    let installable = resolved
        .installable(&tap_state.routing, &system)
        .map_err(CommandError::Message)?;
    // Cask status and metadata come from the generated index before any
    // derivation lookup: excluded and unknown tokens report their recorded
    // state without forcing a package evaluation.
    let data = if let (catalog::CatalogId::Formula(name), Some(view)) = (&resolved, &formula_view) {
        let record = view.catalog.record(&system, "homebrew/core", name);
        let mut report = catalog::report_for(&reference, &view.catalog.identity);
        report.display = Some(String::from("Homebrew Formulae"));
        InfoData {
            attribute,
            matched_attribute: Some(catalog::native_package_attribute(
                &system,
                &format!("homebrew/core/{name}"),
            )),
            meta: record.map(|entry| catalog::catalog_meta(name, entry)),
            report,
            cask: None,
            availability: Some(view.availability(&catalog::CaskHost::default(), &system, name)),
            formula_kind: record.and_then(|entry| entry.kind.clone()),
        }
    } else if let catalog::CatalogId::Cask { source, token } = &resolved {
        cask_info(
            &mut catalog_handle,
            &tap_state,
            &system,
            &reference,
            source,
            token,
        )
        .map_err(CommandError::Message)?
    } else {
        match catalog::exact_lookup(&session.nix, &reference, &attribute) {
            Ok((exact, report)) => InfoData {
                attribute,
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: None,
                formula_kind: None,
                availability: None,
            },
            Err(catalog::CatalogError::NotFound(_)) => {
                return Err(CommandError::Message(format!(
                    "`{}` has no exact match in `{reference}`; \
                     pkg does not report package identities it cannot evaluate",
                    resolved.qualified()
                )));
            }
            Err(catalog::CatalogError::Ambiguous { name, mut choices }) => {
                // Each choice becomes a CLI ID that round-trips through
                // info and install for this request: `packages` and
                // `legacyPackages` entries that share a short name stay
                // full attribute paths, never two identical choices.
                for choice in &mut choices {
                    *choice = request_qualified(&resolved, choice);
                }
                return Err(CommandError::Message(format!(
                    "`{name}` is ambiguous in `{reference}`; \
                     choose one of: {}",
                    choices.join(", ")
                )));
            }
            Err(error) => return Err(error.into()),
        }
    };
    // Installed matching compares the actual full matched attribute and
    // the canonical source with its flake subdirectory, never a short
    // request alone. A cask identity is compared against the normalized
    // native form: `nix profile` records the attribute without quotes.
    let identity_attribute = match &resolved {
        catalog::CatalogId::Cask { source, token } => {
            catalog::native_package_attribute(&system, &nix::full_entry_id(source, token))
        }
        _ => data
            .matched_attribute
            .clone()
            .unwrap_or_else(|| data.attribute.clone()),
    };
    let installed_result = installed_entries(&session);
    let installed_error = installed_result.as_ref().err().map(ToString::to_string);
    let installed = installed_result.ok().and_then(|entries| {
        entries.into_iter().find(|(_, entry)| {
            entry.active
                && entry.attr_path == identity_attribute
                && catalog::canonical_source(&entry.original_url)
                    == catalog::canonical_source(&reference)
        })
    });
    if cli.json {
        let mut result = info_json(&resolved, &data, &reference, &system, &installed);
        result["installed_state"] = serde_json::json!(if installed_error.is_some() {
            "unknown"
        } else if installed.is_some() {
            "installed"
        } else {
            "absent"
        });
        result["installed_error"] = serde_json::json!(installed_error);
        print_json(&output::envelope("info", &result))?;
    } else {
        print!(
            "{}",
            info_text(
                &resolved,
                &data,
                &reference,
                &installed,
                &installable,
                cli.verbose,
                installed_error.as_deref(),
            )
        );
    }
    if let Some(error) = installed_error {
        return Err(format!("installed state is unknown: {error}").into());
    }
    Ok(())
}

/// Build the JSON result for one collected info outcome.
fn info_json(
    resolved: &catalog::CatalogId,
    data: &InfoData,
    reference: &str,
    system: &str,
    installed: &Option<(String, ProfileEntry)>,
) -> serde_json::Value {
    // The installed record carries the actual original and locked source
    // separately.
    let installed_json = installed.as_ref().map(|(entry_id, entry)| {
        serde_json::json!({
            "entry_id": entry_id,
            "original_url": entry.original_url,
            "locked_url": entry.locked_url,
        })
    });
    let name = data
        .meta
        .as_ref()
        .map(|meta| meta.pname.clone())
        .unwrap_or_else(|| match resolved {
            catalog::CatalogId::Cask { token, .. } => token.clone(),
            _ => data.attribute.clone(),
        });
    // Null and empty versions stay null; only a real value is claimed.
    let version = data
        .meta
        .as_ref()
        .map(|meta| meta.version.clone())
        .filter(|version| !version.is_empty());
    let description = data
        .meta
        .as_ref()
        .map(|meta| meta.description.clone())
        .unwrap_or_default();
    let cask_json = data.cask.as_ref().map(cask_json);
    serde_json::json!({
        "id": resolved.qualified(),
        "source": data.report.display.clone().unwrap_or_else(|| reference.to_string()),
        "reference": reference,
        "locked_reference": data.report.locked_reference,
        "revision": data.report.revision,
        "system": system,
        "attribute": data.attribute,
        "matched_attribute": data.matched_attribute,
        "name": name,
        "version": version,
        "description": description,
        "installed": installed_json,
        "cask": cask_json,
        "availability": data.availability,
        "formula_kind": data.formula_kind,
    })
}

/// Build the JSON value for one cask display state.
///
/// `status` mirrors the generated index vocabulary: `eligible` is a
/// metadata claim, never a verification promise.
fn cask_json(display: &CaskDisplay) -> serde_json::Value {
    match display {
        CaskDisplay::UnsupportedPlatform { system, targets } => serde_json::json!({
            "status": "unsupported-platform",
            "system": system,
            "targets": targets,
        }),
        CaskDisplay::Status {
            status: catalog::CaskStatus::Eligible,
            kind,
            homepage,
        } => serde_json::json!({
            "status": "eligible",
            "kind": kind,
            "homepage": homepage,
        }),
        CaskDisplay::Status {
            status: catalog::CaskStatus::Excluded { reason, detail },
            ..
        } => serde_json::json!({
            "status": "excluded",
            "reason": reason,
            "detail": detail,
        }),
        CaskDisplay::Status {
            status: catalog::CaskStatus::Unknown,
            ..
        } => serde_json::json!({
            "status": "unknown",
        }),
    }
}

/// Build the human rendering for one collected info outcome.
fn info_text(
    resolved: &catalog::CatalogId,
    data: &InfoData,
    reference: &str,
    installed: &Option<(String, ProfileEntry)>,
    installable: &str,
    verbose: bool,
    installed_error: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let name = data
        .meta
        .as_ref()
        .map(|meta| meta.pname.clone())
        .unwrap_or_else(|| match resolved {
            catalog::CatalogId::Cask { token, .. } => token.clone(),
            _ => data.attribute.clone(),
        });
    let version = data
        .meta
        .as_ref()
        .map(|meta| meta.version.clone())
        .filter(|version| !version.is_empty());
    let _ = writeln!(
        out,
        "{name}  {}",
        version.as_deref().unwrap_or("version unknown")
    );
    if let Some(meta) = &data.meta
        && !meta.description.is_empty()
    {
        let _ = writeln!(out, "{}", meta.description);
    }
    let _ = writeln!(out, "ID: {}", resolved.qualified());
    if let Some(kind) = &data.formula_kind {
        let _ = writeln!(
            out,
            "Package: {}",
            if kind == "nixpkgs-equivalent" {
                "Nixpkgs equivalent"
            } else {
                "Homebrew bottle through Nix"
            }
        );
    }
    let _ = writeln!(
        out,
        "Installed: {}",
        if installed_error.is_some() {
            "Unknown (profile could not be read)"
        } else if installed.is_some() {
            "Yes"
        } else {
            "No"
        }
    );
    if let Some(availability) = &data.availability {
        match availability {
            catalog::Availability::Available => {
                let _ = writeln!(out, "Status: Available on this system");
            }
            catalog::Availability::Unavailable { reason } => {
                let _ = writeln!(out, "Status: Cannot install on this system");
                let _ = writeln!(out, "Reason: {reason}");
            }
            catalog::Availability::Unknown { reason } => {
                let _ = writeln!(out, "Status: Unknown");
                let _ = writeln!(out, "Reason: {reason}");
            }
        }
    }
    let _ = writeln!(
        out,
        "Source: {}",
        crate::output::short_source(data.report.display.as_deref().unwrap_or(reference))
    );
    if verbose {
        let _ = writeln!(out, "Source reference: {reference}");
        let _ = writeln!(out, "Attribute: {}", data.attribute);
        let _ = writeln!(out, "Installable: {installable}");
        if let Some(revision) = &data.report.revision {
            let _ = writeln!(out, "Revision: {revision}");
        }
        if let Some(CaskDisplay::Status {
            homepage: Some(homepage),
            ..
        }) = &data.cask
        {
            let _ = writeln!(out, "URL: {homepage}");
        }
    }
    out
}

/// One ambiguity choice as a CLI ID that round-trips through `info` and
/// `install` for the request it came from.
///
/// The attribute part is the choice the catalog layer produced — a full
/// attribute path when namespaces collide — so the ID stays unambiguous.
fn request_qualified(resolved: &catalog::CatalogId, attribute: &str) -> String {
    match resolved {
        catalog::CatalogId::Nixpkgs(_) => format!("nixpkgs:{attribute}"),
        catalog::CatalogId::Formula(name) => format!("brew:homebrew/core/{name}"),
        catalog::CatalogId::Explicit { reference, .. } => {
            format!("{reference}#{attribute}")
        }
        catalog::CatalogId::Cask { source, token } => format!("cask:{source}/{token}"),
    }
}

/// Build info data for one cask identity from the official generated index
/// or the source's saved catalog capture.
///
/// The official source's index is evaluated once per command run; an
/// imported tap answers entirely from its saved capture. No package
/// derivation is evaluated for cask metadata. Eligible, excluded, and
/// unknown tokens all report their recorded state, and a system outside the
/// catalog targets reports the target list.
fn cask_info(
    catalog: &mut catalog::CatalogOnce<'_>,
    state: &super::TapState,
    system: &str,
    reference: &str,
    source: &str,
    token: &str,
) -> Result<InfoData, String> {
    let saved_view;
    let view = if source == nix::OFFICIAL_CASK_SOURCE {
        catalog.load().map_err(|error| error.to_string())?
    } else {
        // Info reads exactly the source it reports: an unrelated broken
        // tap cannot block a qualified cask lookup.
        let saved = state.saved_one(source)?;
        saved_view = catalog::CatalogView::from_saved(&saved);
        &saved_view
    };
    let attribute = catalog::package_attribute(system, &nix::full_entry_id(source, token));
    if !view.targeted(system) {
        return Ok(InfoData {
            attribute,
            matched_attribute: None,
            meta: None,
            report: catalog::SourceReport::skipped_platform(reference, view.targets()),
            availability: Some(
                catalog::CaskHost::default().availability(view, system, source, token),
            ),
            formula_kind: None,
            cask: Some(CaskDisplay::UnsupportedPlatform {
                system: system.to_string(),
                targets: view.targets().to_vec(),
            }),
        });
    }
    let record = view.record(system, source, token);
    let meta = record.map(|entry| catalog::catalog_meta(token, entry));
    Ok(InfoData {
        attribute,
        matched_attribute: None,
        meta,
        report: catalog::report_for(reference, &view.identity),
        availability: Some(catalog::CaskHost::default().availability(view, system, source, token)),
        formula_kind: None,
        cask: Some(CaskDisplay::Status {
            status: view.entry(system, source, token),
            kind: record.and_then(|entry| entry.kind.clone()),
            homepage: record.and_then(|entry| entry.homepage.clone()),
        }),
    })
}

/// Cask status display state for one token.
#[derive(Debug)]
enum CaskDisplay {
    /// The system is outside the catalog's target list.
    UnsupportedPlatform {
        /// The system pkg is running on.
        system: String,
        /// The catalog's declared target systems.
        targets: Vec<String>,
    },
    /// The generated index status, with the recorded kind and homepage.
    Status {
        /// Eligible, excluded, or unknown.
        status: catalog::CaskStatus,
        /// The package kind, when the record is eligible.
        kind: Option<String>,
        /// The effective homepage, when the record has one.
        homepage: Option<String>,
    },
}

pub(super) fn list(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    // A fresh install has no profile yet; that state is empty, not an error.
    let entries = installed_entries(&session)?;
    let rows = list_rows(&entries, &session.config, &session.paths.state_home);
    if cli.json {
        print_json(&output::envelope("list", &rows))?;
    } else {
        print!("{}", output::render_list(&rows));
    }
    Ok(())
}

fn list_rows(
    entries: &std::collections::BTreeMap<String, ProfileEntry>,
    config: &crate::config::Config,
    state_home: &std::path::Path,
) -> Vec<ListRow> {
    // Discovery and display scans only consider active entries.
    entries
        .iter()
        .filter(|(_, entry)| entry.active)
        .map(|(entry_id, entry)| ListRow {
            entry_id: entry_id.clone(),
            package_id: crate::installed::package_id(entry, config, state_home),
            name: entry.name(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}
