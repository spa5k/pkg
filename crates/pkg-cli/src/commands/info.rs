//! Package metadata, availability, and installed identity.
use super::{CommandError, installed_entries, print_json, session};
use crate::catalog::{self, ParsedId};
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::output;

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

pub(super) fn run(cli: &Cli, id: &str) -> Result<(), CommandError> {
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
    let mut tap_state = if formula_view.is_some() {
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
            &mut tap_state,
            &system,
            &reference,
            source,
            token,
        )?
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
    state: &mut super::TapState,
    system: &str,
    reference: &str,
    source: &str,
    token: &str,
) -> Result<InfoData, CommandError> {
    let saved_view;
    let view = if source == nix::OFFICIAL_CASK_SOURCE {
        catalog.load()?
    } else {
        // Info reads exactly the source it reports: an unrelated broken
        // tap cannot block a qualified cask lookup.
        let saved = state.saved_one(source)?;
        saved_view = catalog::CatalogView::from_saved(saved);
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
