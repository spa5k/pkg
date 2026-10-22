//! Client update, search data refresh, and the scheduled user job.

use crate::cli::{BackgroundCommand, Cli};
use crate::client::Update;
use crate::output::{self, Stream, Tone};

use super::{CommandError, session};

/// Update the installed client, then official catalogs and, unless the
/// scheduled job runs, saved taps. A failed client check does not stop
/// the catalog refresh.
///
/// After the client changes, the new client refreshes the search data,
/// because a newer client can store it in a newer format.
pub(super) fn update(cli: &Cli, scheduled: bool, skip_client: bool) -> Result<(), CommandError> {
    let session = session(cli)?;
    let lock = crate::profile_lock::ProfileLock::acquire_waiting(
        &session.paths.cache_dir.join("refresh"),
        "Waiting for another pkg update to finish…",
    )?;
    let mut client_updates = true;
    if !skip_client {
        match crate::client::update() {
            Ok(Update::Updated { from, to }) => {
                println!(
                    "{} pkg {from} → {to}",
                    output::paint(Stream::Out, Tone::Success, "Updated")
                );
                drop(lock);
                return refresh_with_new_client(cli, scheduled);
            }
            Ok(Update::Current) => {}
            Ok(Update::NotInstalled) => client_updates = false,
            Err(error) if error.signal().is_some() => return Err(error.into()),
            Err(error) => eprintln!(
                "{} {}",
                output::paint(Stream::Err, Tone::Warning, "Could not update pkg:"),
                super::sentence(super::plain_detail(&error.to_string()))
            ),
        }
    }
    let mut failed = crate::discovery::refresh(&session)?;
    if !scheduled {
        failed |= crate::tap::refresh_registered(cli, &session.paths.state_home)?;
    }
    if failed {
        return Err(String::from("search data is incomplete. Run `pkg update` to retry.").into());
    }
    if client_updates {
        println!("pkg and its search data are up to date. Installed packages did not change.");
    } else {
        println!("Search data is up to date. Installed packages did not change.");
        println!("This pkg is not the installed client, so `pkg update` does not replace it.");
    }
    Ok(())
}

/// Run `pkg update` again with the new client, without a client check.
fn refresh_with_new_client(cli: &Cli, scheduled: bool) -> Result<(), CommandError> {
    let mut args = Vec::new();
    if cli.verbose {
        args.push("--verbose");
    }
    if cli.no_color {
        args.push("--no-color");
    }
    args.extend(["update", "--skip-client"]);
    if scheduled {
        args.push("--scheduled");
    }
    match crate::client::run_installed(&args)? {
        0 => Ok(()),
        code => Err(CommandError::Reported(std::process::ExitCode::from(code))),
    }
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
