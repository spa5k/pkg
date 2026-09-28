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
    let session = session(cli)?;
    let system = session.nix.system()?;
    let mut reports = Vec::new();
    let mut rows = Vec::new();
    for (kind, moving) in super::configured_sources(&session.config) {
        // The Nixpkgs lane passes the pattern to native search unchanged.
        // The cask lane reads the generated catalog index once and filters
        // locally; a system outside the catalog targets is skipped from the
        // envelope alone inside that lane, and an index failure is a
        // failure, never a skip.
        let (source_rows, report) = match kind {
            catalog::SourceKind::Nixpkgs => catalog::search_source(
                &session.nix,
                &session.paths.cache_dir,
                moving,
                query,
                &system,
                kind,
            ),
            catalog::SourceKind::Cask => catalog::search_catalog(
                &session.nix,
                &session.paths.cache_dir,
                moving,
                query,
                &system,
            ),
        };
        reports.push(report);
        rows.extend(source_rows);
    }
    let failed: Vec<&catalog::SourceReport> = reports
        .iter()
        .filter(|report| report.status == catalog::SourceStatus::Failed)
        .collect();
    if cli.json {
        let result = serde_json::json!({
            "system": system,
            "sources": reports,
            "results": rows,
        });
        print_json(&output::envelope("search", &result))?;
    } else {
        print!("{}", output::render_source_reports(&reports));
        print!("{}", output::render_search(&rows));
    }
    if !failed.is_empty() {
        for report in &failed {
            eprintln!(
                "pkg: source failure for {}: {}",
                report.source,
                report.detail.as_deref().unwrap_or("unknown")
            );
        }
        if rows.is_empty() {
            return Err(CommandError::Reported(ExitCode::from(super::FAILURE)));
        }
        // Only the failed sources are degraded: their rows are missing or
        // labeled stale. Rows from healthy sources above are fresh.
        eprintln!(
            "pkg: results above are incomplete. \
             Rows from failed sources are missing or labeled [stale cache]."
        );
    }
    Ok(())
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
    let resolved = match parsed {
        ParsedId::Qualified(qualified) => qualified,
        ParsedId::Bare(name) => catalog::resolve_bare(
            &session.nix,
            &session.config.sources,
            &name,
            &system,
            &mut catalog_handle,
        )?,
    };
    let (reference, attribute) = resolved.source_and_attribute(&session.config.sources, &system);
    let installable = resolved.installable(&session.config.sources, &system);
    // Cask status and metadata come from the generated index before any
    // derivation lookup: excluded and unknown tokens report their recorded
    // state without forcing a package evaluation.
    let data = if let catalog::CatalogId::Cask(token) = &resolved {
        cask_info(&mut catalog_handle, &system, &reference, token).map_err(CommandError::Message)?
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
            Err(error) => return Err(error.into()),
        }
    };
    // Installed matching compares the actual full matched attribute and the
    // canonical source with its flake subdirectory, never a short request
    // alone.
    let identity_attribute = data
        .matched_attribute
        .clone()
        .unwrap_or_else(|| data.attribute.clone());
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
            info_text(&resolved, &data, &reference, &installed, &installable)
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
        "{}  {}  {}",
        resolved.qualified(),
        name,
        version.as_deref().unwrap_or("unknown version")
    );
    let _ = writeln!(
        out,
        "source: {} ({})",
        data.report.display.as_deref().unwrap_or(reference),
        reference
    );
    if let Some(revision) = &data.report.revision {
        let _ = writeln!(out, "revision: {revision}");
    }
    let _ = writeln!(
        out,
        "attribute: {}",
        data.matched_attribute.as_deref().unwrap_or(&data.attribute)
    );
    if let Some(meta) = &data.meta
        && !meta.description.is_empty()
    {
        let _ = writeln!(out, "{}", meta.description);
    }
    if let Some(display) = &data.cask {
        out.push_str(&render_cask_display(display));
    }
    match installed {
        Some((entry_id, _)) => {
            let _ = writeln!(out, "installed as entry {entry_id}");
        }
        None => {
            let _ = writeln!(out, "not installed");
        }
    }
    let _ = writeln!(out, "installable: {installable}");
    out
}

/// Build info data for one cask token from the generated catalog index.
///
/// The index is evaluated once per command run and answers entirely from
/// recorded data: no package derivation is evaluated for cask metadata.
/// Eligible, excluded, and unknown tokens all report their recorded state,
/// and a system outside the catalog targets reports the target list.
fn cask_info(
    catalog: &mut catalog::CatalogOnce<'_>,
    system: &str,
    reference: &str,
    token: &str,
) -> Result<InfoData, String> {
    let view = catalog.load().map_err(|error| error.to_string())?;
    let attribute = catalog::package_attribute(system, token);
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
    let record = view.record(system, token);
    let meta = record.map(|entry| nix::SearchMeta {
        pname: entry.name.clone().unwrap_or_else(|| token.to_string()),
        version: entry.version.clone().unwrap_or_default(),
        description: entry.description.clone().unwrap_or_default(),
    });
    Ok(InfoData {
        attribute,
        matched_attribute: None,
        meta,
        report: catalog::report_for(reference, &view.identity),
        cask: Some(CaskDisplay::Status {
            status: view.entry(system, token),
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

fn render_cask_display(display: &CaskDisplay) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    match display {
        CaskDisplay::UnsupportedPlatform { system, targets } => {
            let _ = writeln!(
                out,
                "cask status: unsupported platform ({system}; catalog targets {})",
                targets.join(", ")
            );
        }
        CaskDisplay::Status {
            status: catalog::CaskStatus::Eligible,
            kind,
            homepage,
        } => {
            let kind = kind.as_deref().unwrap_or("unknown kind");
            let _ = writeln!(out, "cask status: eligible ({kind})");
            if let Some(homepage) = homepage {
                let _ = writeln!(out, "cask homepage: {homepage}");
            }
            let _ = writeln!(
                out,
                "cask eligibility is a metadata claim; the build proves the payload"
            );
        }
        CaskDisplay::Status {
            status: catalog::CaskStatus::Excluded { reason, detail },
            ..
        } => {
            let detail = detail.as_deref().unwrap_or("no detail");
            let _ = writeln!(out, "cask status: excluded ({reason}: {detail})");
        }
        CaskDisplay::Status {
            status: catalog::CaskStatus::Unknown,
            ..
        } => {
            let _ = writeln!(
                out,
                "cask status: not in the generated catalog for this system"
            );
        }
    }
    out
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
            name: entry.name().to_string(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}
