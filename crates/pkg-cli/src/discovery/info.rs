//! Package metadata, availability, and installed identity.
use crate::catalog;
use crate::cli::Cli;
use crate::commands::{CommandError, print_json, session};
use crate::nix::{self, ProfileEntry};
use crate::output;
use crate::profile::installed_entries;

use super::resolution::{CaskDetails, PackageMetadata, Resolution, ResolvedPackage};

pub fn run(cli: &Cli, id: &str) -> Result<(), CommandError> {
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let parsed = catalog::parse_id(id)?;
    let session = session(cli)?;
    let package = Resolution::local(&session)?.inspect(parsed)?;
    let ResolvedPackage {
        resolved,
        data,
        reference,
        system,
        alternatives,
        ..
    } = &package;
    // A cask identity is compared against the normalized
    // native form: `nix profile` records the attribute without quotes.
    let identity_attribute = match resolved.kind() {
        catalog::CatalogIdKind::Cask { source, token } => {
            catalog::native_package_attribute(system, &nix::full_entry_id(source, token))
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
    let entries = installed_result.unwrap_or_default();
    let installed = entries
        .iter()
        .find(|(_, entry)| {
            entry.active
                && entry.attr_path == identity_attribute
                && catalog::canonical_source(&entry.original_url)
                    == catalog::canonical_source(reference)
        })
        .map(|(entry_id, entry)| (entry_id.clone(), entry.clone()));
    // Another source of the same name, such as `brew:figlet`, can be the
    // installed one.
    let installed_alternative = entries
        .values()
        .filter(|entry| entry.active)
        .filter_map(|entry| {
            let id = crate::profile::package_id(entry, &session.config, &session.paths.state_home);
            match catalog::parse_id(&id) {
                Ok(catalog::ParsedId::Qualified(id)) => Some(catalog::short_id(&id)),
                _ => None,
            }
        })
        .find(|id| alternatives.contains(id));
    if cli.json {
        let mut result = info_json(resolved, data, reference, system, installed.as_ref());
        result["installed_state"] = serde_json::json!(if installed_error.is_some() {
            "unknown"
        } else if installed.is_some() {
            "installed"
        } else {
            "absent"
        });
        result["installed_error"] = serde_json::json!(installed_error);
        result["commands"] = serde_json::json!(data.commands);
        result["alternatives"] = serde_json::json!(alternatives);
        result["installed_alternative"] = serde_json::json!(installed_alternative);
        print_json(&output::envelope("info", &result))?;
    } else {
        print!(
            "{}",
            info_text(
                &package,
                installed.as_ref(),
                installed_alternative.as_deref(),
                cli.verbose,
                installed_error.as_deref(),
            )
        );
    }
    if let Some(error) = installed_error {
        if cli.json {
            // The JSON result already reports the unknown state.
            eprintln!("pkg: installed state is unknown: {error}");
            return Err(CommandError::Reported(std::process::ExitCode::from(
                crate::commands::FAILURE,
            )));
        }
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
    package: &ResolvedPackage,
    installed: Option<&(String, ProfileEntry)>,
    installed_alternative: Option<&str>,
    verbose: bool,
    installed_error: Option<&str>,
) -> String {
    use crate::output::{Stream, Tone, paint};
    use std::fmt::Write as _;
    let ResolvedPackage {
        resolved,
        data,
        reference,
        installable,
        alternatives,
        ..
    } = package;
    let mut out = String::new();
    let (name, version) = metadata_name(resolved, data);
    let _ = writeln!(
        out,
        "{}  {}",
        paint(Stream::Out, Tone::Strong, name),
        version.unwrap_or("version unknown")
    );
    if let Some(meta) = &data.meta
        && !meta.description.is_empty()
    {
        let _ = writeln!(out, "{}", meta.description);
    }
    let _ = writeln!(out, "ID: {}", resolved.qualified());
    if !data.commands.is_empty() {
        let _ = writeln!(out, "Commands: {}", data.commands.join(", "));
    }
    if let Some(kind) = &data.formula_kind {
        let _ = writeln!(
            out,
            "Built from: {}",
            if kind == "nixpkgs-equivalent" {
                "the Nixpkgs package"
            } else {
                "the Homebrew bottle"
            }
        );
    }
    let state = match (installed_error, installed, installed_alternative) {
        (Some(_), ..) => String::from("Unknown (profile could not be read)"),
        (None, Some(_), _) => String::from("Yes"),
        (None, None, Some(other)) => format!("No ({other} is installed)"),
        (None, None, None) => String::from("No"),
    };
    let _ = writeln!(out, "Installed: {state}");
    if let Some(availability) = &data.availability
        && !(installed.is_some() && matches!(availability, catalog::Availability::Available))
    {
        write_availability(&mut out, availability, verbose);
    }
    let source = match resolved.kind() {
        catalog::CatalogIdKind::Cask { source, .. }
            if source == crate::nix::OFFICIAL_CASK_SOURCE =>
        {
            String::from("Homebrew Casks")
        }
        catalog::CatalogIdKind::Cask { source, .. } => source.clone(),
        catalog::CatalogIdKind::Formula(_) => String::from("Homebrew formulae"),
        _ => crate::output::short_source(data.report.display.as_deref().unwrap_or(reference)),
    };
    let _ = writeln!(out, "Source: {source}");
    if !alternatives.is_empty() {
        let _ = writeln!(out, "Also available: {}", alternatives.join(", "));
    }
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

/// Write whether the package can install on this computer, and why not.
fn write_availability(out: &mut String, availability: &catalog::Availability, verbose: bool) {
    use std::fmt::Write as _;
    match availability {
        catalog::Availability::Available => {
            let _ = writeln!(out, "Status: Can install on this computer");
        }
        catalog::Availability::Unavailable { reason } => {
            let status = if reason == catalog::OTHER_SYSTEMS {
                "Probably cannot install on this computer"
            } else {
                "Cannot install on this computer"
            };
            let _ = writeln!(out, "Status: {status}");
            let _ = writeln!(
                out,
                "Reason: {}",
                if verbose {
                    reason.as_str()
                } else {
                    catalog::plain_reason(reason)
                }
            );
        }
        catalog::Availability::Unknown { reason } => {
            let _ = writeln!(out, "Status: Unknown");
            let _ = writeln!(out, "Reason: {reason}");
        }
    }
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
