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

pub(super) fn search(cli: &Cli, query: &str) -> Result<(), CommandError> {
    // Reject a malformed pattern before any source query starts. Otherwise
    // each source would report the same input error as an outage.
    regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .build()
        .map_err(|error| {
            eprintln!("Invalid search pattern `{query}`: {error}");
            CommandError::Reported(std::process::ExitCode::from(super::USAGE))
        })?;
    let session = session(cli)?;
    let system = session.nix.system()?;
    let tap_state = super::TapState::load(&session)?;
    let mut reports = Vec::new();
    let mut rows = SearchSpool::new()?;
    for (kind, moving) in super::configured_sources(&session.config) {
        // The Nixpkgs lane filters a cached native snapshot locally.
        // The cask lane reads the generated catalog index once and filters
        // locally; a system outside the catalog targets is skipped from the
        // envelope alone inside that lane, and an index failure is a
        // failure, never a skip.
        let report = match kind {
            catalog::SourceKind::Nixpkgs => catalog::search_source_into(
                &session.nix,
                &session.paths.cache_dir,
                moving,
                query,
                &system,
                kind,
                |row| rows.push(&row),
            )?,
            catalog::SourceKind::Cask => {
                let (source_rows, report) = catalog::search_catalog(
                    &session.nix,
                    &session.paths.cache_dir,
                    moving,
                    query,
                    &system,
                )?;
                for row in source_rows {
                    rows.push(&row)?;
                }
                report
            }
        };
        reports.push(report);
    }
    // Every registered tap answers from its saved catalog capture: no tap
    // Ruby runs, no source is contacted, and a malformed capture fails the
    // search instead of being skipped. Search lists every source, so it
    // loads the complete strict set.
    let saved_all = tap_state.saved_all()?;
    for saved in saved_all.iter() {
        let (source_rows, report) = catalog::search_saved_catalog(saved, query, &system);
        reports.push(report);
        for row in source_rows {
            rows.push(&row)?;
        }
    }
    let failed: Vec<&catalog::SourceReport> = reports
        .iter()
        .filter(|report| report.status == catalog::SourceStatus::Failed)
        .collect();
    if cli.json {
        rows.json(&system, &reports)?;
    } else {
        if !rows.is_empty() || failed.is_empty() {
            rows.text(query)?;
        }
        print!("{}", output::render_source_reports(&reports, cli.verbose));
    }
    if !failed.is_empty() {
        for report in &failed {
            eprintln!(
                "Warning: source {} is unavailable: {}",
                output::short_source(&report.source),
                super::concise_cause(report.detail.as_deref().unwrap_or("unknown"))
            );
        }
        if rows.is_empty() {
            return Err(CommandError::Reported(ExitCode::from(super::FAILURE)));
        }
        // Only the failed sources are degraded: their rows are missing or
        // labeled stale. Rows from healthy sources above are fresh.
        eprintln!("Results may be incomplete. Cached rows may be old.");
    }
    Ok(())
}

struct SearchSpool {
    file: tempfile::NamedTempFile,
    count: usize,
    width: usize,
}

impl SearchSpool {
    fn new() -> Result<Self, CommandError> {
        Ok(Self {
            file: tempfile::NamedTempFile::new().map_err(|error| error.to_string())?,
            count: 0,
            width: 2,
        })
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn push(&mut self, row: &catalog::SearchResult) -> Result<(), catalog::CatalogError> {
        use std::io::Write as _;
        serde_json::to_writer(self.file.as_file_mut(), row)
            .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
        self.file
            .write_all(b"\n")
            .map_err(|error| catalog::CatalogError::CacheWrite(error.to_string()))?;
        self.count += 1;
        self.width = self.width.max(row.id.len());
        Ok(())
    }

    fn visit(
        &mut self,
        mut sink: impl FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), CommandError> {
        use std::io::{BufRead as _, Seek as _};
        self.file
            .as_file_mut()
            .rewind()
            .map_err(|error| error.to_string())?;
        let mut input = std::io::BufReader::new(self.file.as_file_mut());
        let mut line = String::new();
        while input
            .read_line(&mut line)
            .map_err(|error| error.to_string())?
            > 0
        {
            sink(line.trim_end()).map_err(|error| error.to_string())?;
            line.clear();
        }
        Ok(())
    }

    fn json(
        &mut self,
        system: &str,
        reports: &[catalog::SourceReport],
    ) -> Result<(), CommandError> {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        write!(out, "{{\"schema\":1,\"command\":\"search\",\"result\":{{\"system\":{},\"sources\":{},\"results\":[", serde_json::to_string(system).map_err(|error| error.to_string())?, serde_json::to_string(reports).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        let mut first = true;
        self.visit(|line| {
            if !first {
                out.write_all(b",")?;
            }
            first = false;
            out.write_all(line.as_bytes())
        })?;
        out.write_all(b"]}}\n")
            .and_then(|()| out.flush())
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn text(&mut self, query: &str) -> Result<(), CommandError> {
        use std::io::Write as _;
        if self.is_empty() {
            print!("{}", output::render_search(&[], query));
            return Ok(());
        }
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        let width = self.width;
        writeln!(out, "{:<width$}  VERSION  DESCRIPTION", "ID")
            .map_err(|error| error.to_string())?;
        self.visit(|line| {
            let row: catalog::SearchResult =
                serde_json::from_str(line).map_err(std::io::Error::other)?;
            out.write_all(output::render_search_row(&row, width).as_bytes())
        })?;
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
}

pub(super) fn info(cli: &Cli, id: &str) -> Result<(), CommandError> {
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let parsed = catalog::parse_id(id)?;
    let session = session(cli)?;
    let system = session.nix.system()?;
    let mut catalog_handle = catalog::CatalogOnce::new(&session.nix, &session.config.sources.casks);
    let tap_state = super::TapState::load(&session)?;
    // Qualified IDs resolve without reading any saved tap; bare names
    // load every source so their resolution stays strict.
    let resolved = match parsed {
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
    let (reference, attribute) = resolved
        .source_and_attribute(&tap_state.routing, &system)
        .map_err(CommandError::Message)?;
    let installable = resolved
        .installable(&tap_state.routing, &system)
        .map_err(CommandError::Message)?;
    // Cask status and metadata come from the generated index before any
    // derivation lookup: excluded and unknown tokens report their recorded
    // state without forcing a package evaluation.
    let data = if let catalog::CatalogId::Cask { source, token } = &resolved {
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
    let installed = installed_entries(&session).ok().and_then(|entries| {
        entries.into_iter().find(|(_, entry)| {
            entry.attr_path == identity_attribute
                && catalog::canonical_source(&entry.original_url)
                    == catalog::canonical_source(&reference)
        })
    });
    if cli.json {
        let result = info_json(&resolved, &data, &reference, &system, &installed);
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
            )
        );
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
        .unwrap_or_else(|| data.attribute.clone());
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
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let name = data
        .meta
        .as_ref()
        .map(|meta| meta.pname.clone())
        .unwrap_or_else(|| data.attribute.clone());
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
    let _ = writeln!(
        out,
        "Installed: {}",
        if installed.is_some() { "Yes" } else { "No" }
    );
    if let Some(display) = &data.cask {
        match display {
            CaskDisplay::Status {
                status: catalog::CaskStatus::Excluded { reason, detail },
                ..
            } => {
                let _ = writeln!(out, "Status: Cannot install on this system");
                let _ = writeln!(
                    out,
                    "Reason: {reason}{}",
                    detail
                        .as_deref()
                        .map(|text| format!(": {text}"))
                        .unwrap_or_default()
                );
            }
            CaskDisplay::UnsupportedPlatform { targets, .. } => {
                let _ = writeln!(
                    out,
                    "Status: Unsupported platform (targets: {})",
                    targets.join(", ")
                );
            }
            CaskDisplay::Status {
                status: catalog::CaskStatus::Eligible,
                ..
            } => {
                let _ = writeln!(out, "Status: Available on this system");
            }
            CaskDisplay::Status {
                status: catalog::CaskStatus::Unknown,
                ..
            } => {
                let _ = writeln!(out, "Status: Not in this catalog");
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
    let rows = list_rows(&entries);
    if cli.json {
        print_json(&output::envelope("list", &rows))?;
    } else {
        print!("{}", output::render_list(&rows));
    }
    Ok(())
}

fn list_rows(entries: &std::collections::BTreeMap<String, ProfileEntry>) -> Vec<ListRow> {
    // Discovery and display scans only consider active entries.
    entries
        .iter()
        .filter(|(_, entry)| entry.active)
        .map(|(entry_id, entry)| ListRow {
            entry_id: entry_id.clone(),
            name: entry.name(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}
