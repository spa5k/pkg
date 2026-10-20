//! Client update, search data refresh, and the scheduled user job.

use crate::cli::{BackgroundCommand, Cli};
use crate::client::Update;

use super::{CommandError, concise_cause, session};

/// Update the installed client, then official catalogs and, unless the
/// scheduled job runs, saved taps. A failed client check does not stop
/// the catalog refresh.
pub(super) fn update(cli: &Cli, scheduled: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    let _lock = crate::profile_lock::ProfileLock::acquire_waiting(
        &session.paths.cache_dir.join("refresh"),
        "Waiting for another pkg update to finish…",
    )?;
    match crate::client::update() {
        Ok(Update::Updated { from, to }) => println!("Updated: pkg {from} to {to}."),
        Ok(Update::Current | Update::NotInstalled) => {}
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => eprintln!(
            "Could not update pkg: {}",
            concise_cause(&error.to_string())
        ),
    }
    let mut failed = crate::discovery::refresh(&session)?;
    if !scheduled {
        failed |= crate::tap::refresh_registered(cli, &session.paths.state_home)?;
    }
    if failed {
        return Err(String::from("search data is incomplete. Run `pkg update` to retry.").into());
    }
    println!("Search data is up to date. Installed packages did not change.");
    Ok(())
}

pub(super) fn background(command: &BackgroundCommand) -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    match command {
        BackgroundCommand::Enable => {
            let config = crate::config::Config::load(&paths.config_file)?;
            let nix = crate::nix::discover(config.runtime.nix.as_deref())?;
            crate::background::enable(&paths, nix.executable())?;
            println!("On: pkg and its search data update daily.");
        }
        BackgroundCommand::Disable => {
            crate::background::disable(&paths)?;
            println!("Off: pkg and its search data do not update.");
        }
        BackgroundCommand::Status => {
            println!("{}", crate::background::status(&paths)?);
        }
    }
    Ok(())
}
