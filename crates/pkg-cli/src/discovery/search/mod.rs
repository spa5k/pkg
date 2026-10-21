//! Local source queries and install resolution diagnostics.
mod results;
mod source;

use crate::catalog;
use crate::cli::Cli;
use crate::commands::{CommandError, FAILURE, USAGE};
use crate::output;
use results::{Display, SearchSpool};
use source::{SearchSource, search_input};
use std::process::ExitCode;

pub fn run(cli: &Cli, options: &crate::cli::SearchOptions) -> Result<(), CommandError> {
    let (text, source) = search_input(options)?;
    let query =
        catalog::SearchQuery::new(&text, options.matching.regex, options.results.unavailable)
            .map_err(|error| {
                if text.trim().is_empty() {
                    eprintln!("pkg: {error}.");
                } else {
                    // A regex error has a caret drawing; keep its last line.
                    let error = error.to_string();
                    let reason = error
                        .lines()
                        .rev()
                        .find_map(|line| line.strip_prefix("error: "))
                        .unwrap_or(&error);
                    eprintln!("pkg: invalid search pattern `{text}`: {reason}.");
                }
                CommandError::Reported(ExitCode::from(USAGE))
            })?
            .broad(options.matching.broad)
            .fuzzy(!options.matching.no_fuzzy);
    let paths = crate::config::paths()?;
    let config = crate::config::Config::load(&paths.config_file)?;
    let limit = if options.results.all || cli.json && options.limit.is_none() {
        None
    } else {
        Some(options.limit.unwrap_or(20))
    };
    let mut refreshed = false;
    let (mut system, mut reports, mut rows, mut kept) =
        local_search(&paths, &config, &query, &source, limit, !cli.json)?;
    if reports.iter().any(catalog::SourceReport::is_not_ready) {
        // First use: prepare the missing official data, then search again.
        // The background job can hold the lock while it prepares the same
        // data, so search again before preparing anything here.
        let session = crate::commands::session(cli)?;
        let _lock = crate::profile_lock::ProfileLock::acquire_waiting(
            &session.paths.cache_dir.join("refresh"),
            "Waiting for search data that pkg prepares in the background…",
        )?;
        (system, reports, rows, kept) =
            local_search(&paths, &config, &query, &source, limit, !cli.json)?;
        if reports.iter().any(catalog::SourceReport::is_not_ready) {
            let nixpkgs = &config.sources.nixpkgs;
            if catalog::published_channel(nixpkgs).is_none()
                && reports
                    .iter()
                    .any(|report| report.is_not_ready() && report.source == *nixpkgs)
            {
                exact_hint(&session, nixpkgs, &text)?;
                output::note(
                    "Preparing search data on this computer. This takes a few minutes one time.\n\
                     To stop, press Ctrl-C. The next search continues the work.",
                );
            } else {
                output::note("Downloading search data. This takes less than a minute.");
            }
            // The refresh prints each failure, so the warnings below skip
            // the sources that are still not ready.
            refreshed = true;
            crate::discovery::refresh(&session)?;
            (system, reports, rows, kept) =
                local_search(&paths, &config, &query, &source, limit, !cli.json)?;
        }
    }
    let failed = reports
        .iter()
        .any(|report| report.status == catalog::SourceStatus::Failed);
    if cli.json {
        rows.json(&system, &reports, limit)?;
    } else {
        if rows.count > 0 || !failed {
            let names = (rows.count > 0).then(|| local_names(&paths, &config, &system, kept));
            let installed = installed_attributes(&paths.profile);
            rows.text(
                &text,
                limit,
                &Display {
                    names: names.as_ref(),
                    installed: &installed,
                },
            )?;
        }
        eprint!("{}", output::render_source_reports(&reports, cli.verbose));
    }
    for report in reports.iter().filter(|report| {
        report.status == catalog::SourceStatus::Failed && !(refreshed && report.is_not_ready())
    }) {
        eprintln!(
            "Warning: {}: {}",
            output::source_title(&report.source).map_or_else(
                || report
                    .display
                    .clone()
                    .unwrap_or_else(|| output::short_source(&report.source)),
                String::from
            ),
            crate::commands::plain_detail(report.detail.as_deref().unwrap_or("unknown cause"))
        );
    }
    if failed {
        if rows.count == 0 {
            return Err(CommandError::Reported(ExitCode::from(FAILURE)));
        }
        eprintln!("Results are incomplete. Run `pkg update` to retry.");
    }
    Ok(())
}

/// Before a local preparation starts, show the Nixpkgs package whose name
/// is the query. One Nix check finds it.
fn exact_hint(
    session: &crate::commands::Session,
    source: &str,
    text: &str,
) -> Result<(), CommandError> {
    let name = text.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.+".contains(&byte))
    {
        return Ok(());
    }
    let system = session.nix.system()?;
    let mut path = vec![String::from("legacyPackages"), system];
    path.extend(name.split('.').map(str::to_string));
    match session.nix.exact_metadata(source, &[path]) {
        Ok(nodes) => {
            if let [
                crate::nix::MetadataNode {
                    value: crate::nix::MetadataValue::Package { meta, .. },
                    ..
                },
            ] = nodes.as_slice()
            {
                output::note(&format!(
                    "Found by exact name: nixpkgs:{name} {}. To install it now, run `pkg install nixpkgs:{name}`.",
                    meta.version
                ));
            }
            Ok(())
        }
        Err(error) if error.signal().is_some() => Err(error.into()),
        // The hint is optional. The full search follows.
        Err(_) => Ok(()),
    }
}

/// Local name data for display names. It reuses the data that search read.
/// Unreadable taps count as none.
fn local_names(
    paths: &crate::config::Paths,
    config: &crate::config::Config,
    system: &str,
    kept: catalog::Kept,
) -> catalog::Names {
    let saved = crate::tap::Catalogs::load(&paths.state_home)
        .ok()
        .and_then(|mut taps| taps.saved_all().ok())
        .unwrap_or_default();
    catalog::Names::local(&paths.cache_dir, &config.sources, system, saved, kept)
}

/// The native attributes of active entries in the profile manifest.
///
/// Search reads the manifest file directly. It runs no Nix command, and an
/// unreadable profile marks nothing as installed.
fn installed_attributes(profile: &std::path::Path) -> std::collections::BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(profile.join("manifest.json")) else {
        return std::collections::BTreeSet::new();
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
        return std::collections::BTreeSet::new();
    };
    let elements: Vec<&serde_json::Value> = match manifest.get("elements") {
        Some(serde_json::Value::Object(elements)) => elements.values().collect(),
        Some(serde_json::Value::Array(elements)) => elements.iter().collect(),
        _ => Vec::new(),
    };
    elements
        .into_iter()
        .filter(|element| element.get("active").and_then(serde_json::Value::as_bool) != Some(false))
        .filter_map(|element| element.get("attrPath").and_then(serde_json::Value::as_str))
        .map(|attribute| attribute.replace('"', ""))
        .collect()
}

fn local_search(
    paths: &crate::config::Paths,
    config: &crate::config::Config,
    query: &catalog::SearchQuery,
    source: &SearchSource,
    limit: Option<u32>,
    merged: bool,
) -> Result<
    (
        String,
        Vec<catalog::SourceReport>,
        SearchSpool,
        catalog::Kept,
    ),
    CommandError,
> {
    let mut kept = catalog::Kept::default();
    let system = catalog::search_system(&paths.cache_dir);
    let mut reports = Vec::new();
    let mut taps = if source.needs_registry() {
        match crate::tap::Catalogs::load(&paths.state_home) {
            Ok(taps) => taps,
            Err(error) => {
                if source.tap().is_some() {
                    return Err(error.into());
                }
                reports.push(catalog::missing_report("saved taps", &error));
                crate::tap::Catalogs::from_registry(
                    crate::tap::Registry::empty(),
                    &paths.state_home,
                )
            }
        }
    } else {
        crate::tap::Catalogs::from_registry(crate::tap::Registry::empty(), &paths.state_home)
    };
    if let Some(source) = source.tap()
        && !taps.registry().sources.contains_key(source)
    {
        return Err(format!("unknown search source `{source}`. Use nixpkgs, brew, homebrew, cask, homebrew/core, homebrew/cask, or a source from `pkg tap list`.").into());
    }
    let host = catalog::CaskHost::default();
    let mut rows = SearchSpool::new(limit, merged)?;
    for (kind, moving) in super::configured_sources(config) {
        let identity = match kind {
            catalog::SourceKind::Nixpkgs => "nixpkgs",
            catalog::SourceKind::Cask => "homebrew/cask",
        };
        if !source.includes(identity) {
            continue;
        }
        let result = match kind {
            catalog::SourceKind::Nixpkgs => catalog::search_local_native(
                &paths.cache_dir,
                moving,
                &system,
                query,
                &mut kept,
                |row, rank| rows.push(&row, rank),
            ),
            catalog::SourceKind::Cask => catalog::search_local_casks(
                &paths.cache_dir,
                moving,
                &system,
                query,
                &host,
                &mut kept,
                |row, rank| {
                    if !source.official_cask_only() || row.id.starts_with("cask:homebrew/cask/") {
                        rows.push(&row, rank)?;
                    }
                    Ok(())
                },
            ),
        };
        reports.push(source_report(moving, result)?);
    }
    if source.includes("brew") {
        let result = catalog::brew::search(
            &paths.cache_dir,
            &config.sources.formulae,
            &system,
            query,
            &mut kept,
            |row, rank| rows.push(&row, rank),
        );
        reports.push(source_report(&config.sources.formulae, result)?);
    }
    let sources: Vec<_> = taps.registry().sources.keys().cloned().collect();
    for tap_source in &sources {
        if !source.includes(tap_source) {
            continue;
        }
        let result = taps
            .saved_one(tap_source)
            .map_err(catalog::CatalogError::Source)
            .and_then(|saved| {
                catalog::search_local_saved(saved, &system, query, &host, |row, rank| {
                    rows.push(&row, rank)
                })
            });
        reports.push(source_report(tap_source, result)?);
    }
    Ok((system, reports, rows, kept))
}

fn source_report(
    source: &str,
    result: Result<catalog::SourceReport, catalog::CatalogError>,
) -> Result<catalog::SourceReport, CommandError> {
    match result {
        Ok(report) => Ok(report),
        Err(error) if error.signal().is_some() => Err(error.into()),
        Err(error) => Ok(catalog::missing_report(source, &error.to_string())),
    }
}

pub(super) fn install_resolution_error(
    session: &crate::commands::Session,
    name: &str,
    source: Option<&str>,
    error: catalog::CatalogError,
) -> CommandError {
    if !matches!(error, catalog::CatalogError::NotFound(_)) {
        return error.into();
    }
    let suggestions = match catalog::SearchQuery::new(name, false, false) {
        Ok(query) => match local_search(
            &session.paths,
            &session.config,
            &query,
            &SearchSource::parse(source),
            Some(5),
            true,
        ) {
            Ok((_, _, rows, kept)) => Some((rows, kept)),
            Err(error) if error.signal().is_some() => return error,
            Err(_) => None,
        },
        Err(_) => None,
    }
    .filter(|(rows, _)| rows.count > 0);
    if let Some((rows, kept)) = suggestions {
        eprintln!("pkg: no package named `{name}`. Nothing was installed.");
        eprintln!(
            "{}",
            if rows.fuzzy {
                "Similar names:"
            } else {
                "Related packages:"
            }
        );
        let system = catalog::search_system(&session.paths.cache_dir);
        let names = local_names(&session.paths, &session.config, &system, kept);
        let installed = std::collections::BTreeSet::new();
        let _ = rows.suggestions(
            5,
            &Display {
                names: Some(&names),
                installed: &installed,
            },
        );
        eprintln!("Install one with `pkg install NAME`.");
    } else {
        eprintln!(
            "pkg: no package named `{name}`. Nothing was installed. Search with `pkg search {name}`."
        );
    }
    CommandError::Reported(ExitCode::from(FAILURE))
}
