//! Explicit launcher commands and the reaction to a completed profile change.

use crate::cli::{AppsCommand, Cli};
use crate::commands::{CommandError, FAILURE, error, report_cause, session};
use std::process::ExitCode;

pub fn command(cli: &Cli, command: &AppsCommand) -> Result<(), CommandError> {
    if !cfg!(target_os = "macos") {
        return Err(String::from("app launchers work on macOS only.").into());
    }
    match command {
        AppsCommand::Sync => sync(cli),
        AppsCommand::Setup => setup(),
    }
}

fn sync(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    eprintln!("Checking installed apps…");
    super::sync(&session.nix, &session.paths).map_err(|detail| {
        eprintln!("Failed: app launchers could not be updated.");
        let detail_text = detail.to_string();
        report_cause(&detail_text);
        if detail.dock_effect_possible() {
            eprintln!("The package profile did not change. Dock state may have changed.");
        } else {
            eprintln!("The package profile and Dock state did not change.");
        }
        CommandError::Reported(
            detail
                .signal()
                .map_or_else(|| ExitCode::from(FAILURE), error::signal_exit),
        )
    })?;
    println!("App launchers synced.");
    Ok(())
}

fn setup() -> Result<(), CommandError> {
    super::setup_storage()?;
    println!("Shared app storage is ready.");
    Ok(())
}

/// Refresh derived macOS launchers after a successful profile mutation.
///
/// The package change has already completed in the native profile. When the
/// launcher sync fails, the failure is reported as partial completion with a
/// nonzero exit and `pkg apps sync` named as the retry (design D4). On other
/// systems app exposure does not apply and this is a no-op.
///
/// Returns one line for each launcher that the sync added or removed.
pub fn refresh_after_profile_change(
    profile: &crate::profile::LockedProfile<'_>,
    entries: &std::collections::BTreeMap<String, crate::nix::ProfileEntry>,
    action: &str,
    names: &[String],
) -> Result<String, CommandError> {
    use std::fmt::Write as _;
    if !cfg!(target_os = "macos") {
        return Ok(String::new());
    }
    match super::sync_locked(profile, entries) {
        Ok(changes) => {
            let mut text = String::new();
            for name in &changes.added {
                let _ = writeln!(text, "App: ~/Applications/pkg/{name}");
            }
            for name in &changes.removed {
                let _ = writeln!(text, "Removed app: {name}");
            }
            Ok(text)
        }
        Err(detail) => {
            if names.is_empty() {
                eprintln!(
                    "Partial: {action} left installed entries unchanged. App launcher setup failed."
                );
            } else {
                eprintln!(
                    "Partial: {action} changed {}. App launcher setup failed.",
                    names.join(", ")
                );
            }
            let detail_text = detail.to_string();
            report_cause(&detail_text);
            eprintln!("Next: Resolve the cause, then run `pkg apps sync`.");
            if detail.dock_effect_possible() {
                eprintln!("Dock state may have changed.");
            } else {
                eprintln!("Dock state did not change.");
            }
            Err(detail.signal().map_or_else(
                || CommandError::Reported(ExitCode::from(FAILURE)),
                |signal| CommandError::Reported(error::signal_exit(signal)),
            ))
        }
    }
}
