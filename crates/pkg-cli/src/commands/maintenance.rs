//! Catalog preparation and scheduled refresh commands.

use crate::{
    catalog,
    cli::{BackgroundCommand, Cli},
};

use super::{CommandError, session};

pub(super) fn update(cli: &Cli, catalogs_only: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    let _lock =
        crate::profile_lock::ProfileLock::acquire(&session.paths.cache_dir.join("refresh"))?;
    let system = session.nix.system()?;
    catalog::record_search_system(&session.paths.cache_dir, &system)?;
    eprintln!("Preparing local package search. The first run can take several minutes.");
    let mut failed = false;
    eprintln!("Checking Homebrew Formulae…");
    match catalog::brew::prepare(
        &session.nix,
        &session.paths.cache_dir,
        &session.config.sources.formulae,
        &system,
    ) {
        Ok(count) => eprintln!("Ready: Homebrew Formulae search data ({count} formulae)."),
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => {
            failed = true;
            eprintln!(
                "Failed: Homebrew Formulae: {}. Previous complete data was kept.",
                super::concise_cause(&error.to_string())
            );
        }
    }
    for (kind, source) in super::configured_sources(&session.config).into_iter().rev() {
        eprintln!("Checking {}…", kind.label());
        let result = session
            .nix
            .refresh_source_identity(source)
            .map_err(catalog::CatalogError::from)
            .and_then(|identity| match kind {
                catalog::SourceKind::Nixpkgs => catalog::prepare_source(
                    &session.nix,
                    &session.paths.cache_dir,
                    source,
                    &system,
                    &identity,
                ),
                catalog::SourceKind::Cask => catalog::prepare_catalog(
                    &session.nix,
                    &session.paths.cache_dir,
                    source,
                    &system,
                    &identity,
                ),
            });
        match result {
            Ok(()) => eprintln!("Ready: {} search data.", kind.label()),
            Err(error) if error.signal().is_some() => return Err(error.into()),
            Err(error) => {
                failed = true;
                eprintln!(
                    "Failed: {}: {}. Previous complete data was kept.",
                    kind.label(),
                    super::concise_cause(&error.to_string())
                );
            }
        }
    }
    if !catalogs_only {
        let registry = crate::tap::Registry::load(&crate::tap::registry::registry_path(
            &session.paths.state_home,
        ))?;
        for source in registry.sources.keys() {
            match super::tap::update(cli, Some(source), None) {
                Ok(()) => {}
                Err(error)
                    if error.is_interrupted() || matches!(error, CommandError::Reported(_)) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    failed = true;
                    eprintln!("Failed: {source}: {error}");
                }
            }
        }
    }
    if failed {
        return Err(
            String::from("search preparation is incomplete. Run `pkg update` to retry.").into(),
        );
    }
    println!("Ready: local package search. Installed packages did not change.");
    Ok(())
}

pub(super) fn setup(cli: &Cli, no_background: bool) -> Result<(), CommandError> {
    let preparation = update(cli, true);
    if no_background
        || preparation.as_ref().is_err_and(|error| {
            error.is_interrupted() || matches!(error, CommandError::Reported(_))
        })
    {
        return preparation;
    }
    match background(&BackgroundCommand::Enable) {
        Ok(()) => preparation,
        Err(error) if error.is_interrupted() || matches!(error, CommandError::Reported(_)) => {
            Err(error)
        }
        Err(error) => {
            if preparation.is_ok() {
                Err(format!("search is ready, but background setup failed: {error}. Run `pkg background enable` to retry.").into())
            } else {
                eprintln!(
                    "Background refresh could not be enabled: {}",
                    super::concise_cause(&error.to_string())
                );
                preparation
            }
        }
    }
}

pub(super) fn background(command: &BackgroundCommand) -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    match command {
        BackgroundCommand::Enable => {
            let config = crate::config::Config::load(&paths.config_file)?;
            let nix = crate::nix::discover(config.runtime.nix.as_deref())?;
            crate::background::enable(&paths, nix.executable())?;
            println!("Enabled: daily catalog refresh. Control it with `pkg background disable`.");
        }
        BackgroundCommand::Disable => {
            crate::background::disable(&paths)?;
            println!("Disabled: daily catalog refresh.");
        }
        BackgroundCommand::Status => {
            println!("{}", crate::background::status(&paths)?);
        }
    }
    Ok(())
}
