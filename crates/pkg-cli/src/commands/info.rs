//! Package metadata, availability, and installed identity.
use super::{CommandError, installed_entries, print_json, session};
use crate::catalog;
use crate::cli::Cli;
use crate::nix::{self, ProfileEntry};
use crate::output;

use super::resolution::{CaskDetails, PackageMetadata, Resolution, ResolvedPackage};

pub(super) fn run(cli: &Cli, id: &str) -> Result<(), CommandError> {
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let parsed = catalog::parse_id(id)?;
    let session = session(cli)?;
    let ResolvedPackage {
        resolved,
        data,
        reference,
        installable,
        system,
    } = Resolution::new(&session)?.inspect(parsed)?;
    // A cask identity is compared against the normalized
    // native form: `nix profile` records the attribute without quotes.
    let identity_attribute = match resolved.kind() {
        catalog::CatalogIdKind::Cask { source, token } => {
            catalog::native_package_attribute(&system, &nix::full_entry_id(source, token))
        }
        _ => data
            .matched_attribute
            .clone()
            .unwrap_or_else(|| data.attribute.clone()),
    };
    let installed_result = match installed_entries(&session) {
        Err(error) if error.signal().is_some() => return Err(error.into()),
        result => result,
    };
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
        let mut result = info_json(&resolved, &data, &reference, &system, installed.as_ref());
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
                installed.as_ref(),
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
    data: &PackageMetadata,
    reference: &str,
    system: &str,
    installed: Option<&(String, ProfileEntry)>,
) -> serde_json::Value {
    // The installed record carries the actual original and locked source
    // separately.
    let installed_json = installed.map(|(entry_id, entry)| {
        serde_json::json!({
            "entry_id": entry_id,
            "original_url": entry.original_url,
            "locked_url": entry.locked_url,
        })
    });
    let (name, version) = metadata_name(resolved, data);
    let description = data
        .meta
        .as_ref()
        .map(|meta| meta.description.clone())
        .unwrap_or_default();
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
        "cask": data.cask,
        "availability": data.availability,
        "formula_kind": data.formula_kind,
    })
}

/// Build the human rendering for one collected info outcome.
fn info_text(
    resolved: &catalog::CatalogId,
    data: &PackageMetadata,
    reference: &str,
    installed: Option<&(String, ProfileEntry)>,
    installable: &str,
    verbose: bool,
    installed_error: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let (name, version) = metadata_name(resolved, data);
    let _ = writeln!(out, "{name}  {}", version.unwrap_or("version unknown"));
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
        if let Some(
            CaskDetails::Eligible {
                homepage: Some(homepage),
                ..
            }
            | CaskDetails::Excluded {
                homepage: Some(homepage),
                ..
            },
        ) = &data.cask
        {
            let _ = writeln!(out, "URL: {homepage}");
        }
    }
    out
}

fn metadata_name<'a>(
    id: &'a catalog::CatalogId,
    data: &'a PackageMetadata,
) -> (&'a str, Option<&'a str>) {
    let fallback = match id.kind() {
        catalog::CatalogIdKind::Cask { token, .. } => token,
        _ => &data.attribute,
    };
    let name = data.meta.as_ref().map_or(fallback, |meta| &meta.pname);
    let version = data
        .meta
        .as_ref()
        .map(|meta| meta.version.as_str())
        .filter(|version| !version.is_empty());
    (name, version)
}
