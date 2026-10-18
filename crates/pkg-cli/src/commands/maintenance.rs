//! Catalog preparation and scheduled refresh commands.

use crate::cli::{BackgroundCommand, Cli};

use super::{CommandError, session};

pub(super) fn update(cli: &Cli, catalogs_only: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    let _lock =
        crate::profile_lock::ProfileLock::acquire(&session.paths.cache_dir.join("refresh"))?;
    let mut failed = crate::discovery::refresh(&session)?;
    if !catalogs_only {
        failed |= crate::tap::refresh_registered(cli, &session.paths.state_home)?;
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
