//! Local source queries and install resolution diagnostics.
mod results;
mod source;

use crate::catalog;
use crate::cli::Cli;
use crate::commands::{CommandError, FAILURE, USAGE, concise_cause};
use crate::output;
use results::SearchSpool;
use source::{SearchSource, search_input};
use std::process::ExitCode;

pub fn run(cli: &Cli, options: &crate::cli::SearchOptions) -> Result<(), CommandError> {
    let (text, source) = search_input(options)?;
    let query =
        catalog::SearchQuery::new(&text, options.matching.regex, options.results.unavailable)
            .map_err(|error| {
                eprintln!("Invalid search pattern `{text}`: {error}");
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
    let (mut system, mut reports, mut rows) =
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
        (system, reports, rows) = local_search(&paths, &config, &query, &source, limit, !cli.json)?;
        if reports.iter().any(catalog::SourceReport::is_not_ready) {
            output::note("Preparing search for first use. This takes a few minutes.");
            crate::discovery::refresh(&session)?;
            (system, reports, rows) =
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
            concise_cause(report.detail.as_deref().unwrap_or("unknown cause"))
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

fn local_search(
    paths: &crate::config::Paths,
    config: &crate::config::Config,
    query: &catalog::SearchQuery,
    source: &SearchSource,
    limit: Option<u32>,
    grouped: bool,
) -> Result<(String, Vec<catalog::SourceReport>, SearchSpool), CommandError> {
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
    let mut rows = SearchSpool::new(limit, grouped)?;
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
                |row, rank| rows.push(&row, rank),
            ),
            catalog::SourceKind::Cask => catalog::search_local_casks(
                &paths.cache_dir,
                moving,
                &system,
                query,
                &host,
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
    Ok((system, reports, rows))
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
    if !matches!(
        error,
        catalog::CatalogError::NotFound(_) | catalog::CatalogError::RequiresId { .. }
    ) {
        return error.into();
    }
    let suggestions = match catalog::SearchQuery::new(name, false, false) {
        Ok(query) => match local_search(
            &session.paths,
            &session.config,
            &query,
            &SearchSource::parse(source),
            Some(5),
            false,
        ) {
            Ok((_, _, rows)) => Some(rows),
            Err(error) if error.signal().is_some() => return error,
            Err(_) => None,
        },
        Err(_) => None,
    }
    .filter(|rows| rows.count > 0);
    eprintln!("pkg: {error}");
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
    if matches!(error, catalog::CatalogError::RequiresId { .. }) {
        eprintln!("Use `pkg search {name} --broad` to inspect other package IDs.");
    } else {
        eprintln!("Use `pkg search {name}` to find packages.");
    }
    CommandError::Reported(ExitCode::from(FAILURE))
}
