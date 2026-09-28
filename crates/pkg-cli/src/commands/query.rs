//! Query commands: search, info, list.
//!
//! These commands read state and never mutate the profile. Each offers a
//! versioned JSON envelope and a short human rendering of the same data.

use std::process::ExitCode;

use crate::catalog::{self, CaskStatus, ParsedId};
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::output::{self, ListRow};

use super::{CommandError, Session, installed_entries, print_json, session};

pub(super) fn search(cli: &Cli, query: &str) -> Result<(), CommandError> {
    let session = session(cli)?;
    let system = session.nix.system()?;
    let mut reports = Vec::new();
    let mut rows = Vec::new();
    for (kind, moving) in super::configured_sources(&session.config) {
        // The cask source is macOS-only; on other systems it is skipped with
        // an honest status instead of a fatal requirement.
        if kind == catalog::SourceKind::Cask && !catalog::casks_supported_on(&system) {
            reports.push(catalog::SourceReport::skipped_platform(moving));
            continue;
        }
        let (source_rows, report) = catalog::search_source(
            &session.nix,
            &session.paths.cache_dir,
            moving,
            query,
            &system,
            kind,
        );
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
    let resolved = match parsed {
        ParsedId::Qualified(qualified) => qualified,
        ParsedId::Bare(name) => {
            catalog::resolve_bare(&session.nix, &session.config.sources, &name, &system)?
        }
    };
    let (reference, attribute) = resolved.source_and_attribute(&session.config.sources);
    let installable = resolved.installable(&session.config.sources);
    // Cask platform and support are checked before any derivation lookup, so
    // excluded and unlisted tokens report their recorded reason instead of
    // exiting on a nonexistent derivation.
    let data = if let catalog::CatalogId::Cask(token) = &resolved {
        cask_info(&session, &system, &reference, token).map_err(CommandError::Message)?
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
    let cask_version = match &data.cask {
        Some(CaskDisplay::Status(CaskStatus::Supported { version, .. })) => version.clone(),
        Some(CaskDisplay::Status(CaskStatus::Excluded { version, .. })) => version.clone(),
        _ => None,
    };
    let name = data
        .meta
        .as_ref()
        .map(|meta| meta.pname.clone())
        .unwrap_or_else(|| data.attribute.clone());
    let version = data
        .meta
        .as_ref()
        .map(|meta| meta.version.clone())
        .or(cask_version);
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
fn cask_json(display: &CaskDisplay) -> serde_json::Value {
    match display {
        CaskDisplay::UnsupportedPlatform { system } => serde_json::json!({
            "status": "unsupported-platform",
            "system": system,
            "required": catalog::CASK_SYSTEM,
        }),
        CaskDisplay::Unavailable(detail) => serde_json::json!({
            "status": "unavailable",
            "detail": detail,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Supported {
            kind,
            version,
            homepage,
            artifact_kinds,
            override_applied,
            source_revision,
        }) => serde_json::json!({
            "status": "supported",
            "kind": kind,
            "version": version,
            "homepage": homepage,
            "artifactKinds": artifact_kinds,
            "override": override_applied,
            "sourceRevision": source_revision,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Excluded {
            version,
            reason,
            detail,
            source_revision,
        }) => serde_json::json!({
            "status": "excluded",
            "version": version,
            "reason": reason,
            "detail": detail,
            "sourceRevision": source_revision,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Unlisted { .. }) => serde_json::json!({
            "status": "unlisted",
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
        .or_else(|| match &data.cask {
            Some(CaskDisplay::Status(status)) => cask_display_version(status),
            _ => None,
        });
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

/// The version a cask classification carries, when it has one.
fn cask_display_version(status: &CaskStatus) -> Option<String> {
    match status {
        CaskStatus::Supported { version, .. } => version.clone(),
        CaskStatus::Excluded { version, .. } => version.clone(),
        CaskStatus::Unlisted { .. } => None,
    }
}

/// Build info data for one cask token.
///
/// Platform and the support manifest are consulted before any derivation
/// lookup: excluded and unlisted tokens return their recorded metadata from
/// the same locked source without searching for a derivation that does not
/// exist.
fn cask_info(
    session: &Session,
    system: &str,
    reference: &str,
    token: &str,
) -> Result<InfoData, String> {
    if !catalog::casks_supported_on(system) {
        return Ok(InfoData {
            attribute: token.to_string(),
            matched_attribute: None,
            meta: None,
            report: catalog::SourceReport::skipped_platform(reference),
            cask: Some(CaskDisplay::UnsupportedPlatform {
                system: system.to_string(),
            }),
        });
    }
    let lookup = match catalog::cask_status(&session.nix, reference, system, token) {
        Ok(lookup) => lookup,
        Err(error) => {
            // The manifest is unavailable. A derivation lookup may still
            // work; the unavailability stays visible in the result.
            let detail = error.to_string();
            let (exact, report) =
                catalog::exact_lookup(&session.nix, reference, token).map_err(|lookup_error| {
                    format!(
                        "cask support manifest unavailable ({detail}); \
                             and no exact derivation match either: {lookup_error}"
                    )
                })?;
            return Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: Some(CaskDisplay::Unavailable(detail)),
            });
        }
    };
    match lookup.status {
        catalog::CaskStatus::Supported { .. } => {
            // Metadata comes from the same locked source as the support
            // decision.
            let (exact, report) =
                catalog::exact_lookup_in(&session.nix, reference, &lookup.identity, token)
                    .map_err(|error| error.to_string())?;
            Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: Some(CaskDisplay::Status(lookup.status)),
            })
        }
        status @ (catalog::CaskStatus::Excluded { .. } | catalog::CaskStatus::Unlisted { .. }) => {
            Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: None,
                meta: None,
                report: catalog::report_for(reference, &lookup.identity),
                cask: Some(CaskDisplay::Status(status)),
            })
        }
    }
}

/// Cask support display state for one token.
#[derive(Debug)]
enum CaskDisplay {
    /// The system cannot run casks at all.
    UnsupportedPlatform {
        /// The system pkg is running on.
        system: String,
    },
    /// The support manifest could not be evaluated.
    Unavailable(String),
    /// The manifest classify result.
    Status(catalog::CaskStatus),
}

fn render_cask_display(display: &CaskDisplay) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    match display {
        CaskDisplay::UnsupportedPlatform { system } => {
            let _ = writeln!(
                out,
                "cask support: unsupported platform ({system}; casks need {})",
                catalog::CASK_SYSTEM
            );
        }
        CaskDisplay::Unavailable(detail) => {
            let _ = writeln!(out, "cask support: unavailable ({detail})");
        }
        CaskDisplay::Status(catalog::CaskStatus::Supported {
            kind,
            source_revision,
            ..
        }) => {
            let _ = writeln!(out, "cask support: supported ({kind})");
            if let Some(revision) = source_revision {
                let _ = writeln!(out, "cask source revision: {revision}");
            }
        }
        CaskDisplay::Status(catalog::CaskStatus::Excluded {
            version,
            reason,
            detail,
            ..
        }) => {
            let version = version.as_deref().unwrap_or("unknown version");
            let detail = detail.as_deref().unwrap_or("no detail");
            let _ = writeln!(
                out,
                "cask support: excluded at {version} ({reason}: {detail})"
            );
        }
        CaskDisplay::Status(catalog::CaskStatus::Unlisted { .. }) => {
            let _ = writeln!(out, "cask support: not listed for this system");
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
