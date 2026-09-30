//! Utility commands: shellenv, completion, apps sync.

use std::io::Write as _;

use clap::CommandFactory as _;
use clap_complete;

use crate::cli::{Cli, Shell};

use super::{CommandError, session};

/// Quote `value` as one single-quoted shell word (Bash and Zsh). Single
/// quotes keep spaces, double quotes, `$`, and backticks literal; an
/// embedded single quote is closed, escaped, and reopened.
fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn shellenv() -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    let bin = shell_word(&paths.profile.join("bin").display().to_string());
    println!("# pkg shellenv (Bash and Zsh; safe to repeat)");
    println!("case \":$PATH:\" in");
    println!("  *:{bin}:*) ;;");
    println!("  *) export PATH={bin}\"${{PATH:+:$PATH}}\" ;;");
    println!("esac");
    Ok(())
}

pub(super) fn completion(shell: Shell) -> Result<(), CommandError> {
    let mut command = Cli::command();
    let target = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    let mut generated = Vec::new();
    clap_complete::generate(target, &mut command, "pkg", &mut generated);
    std::io::stdout()
        .write_all(&generated)
        .map_err(|error| CommandError::Message(error.to_string()))?;
    Ok(())
}

pub(super) fn apps_sync(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    if !cfg!(target_os = "macos") {
        println!("App launchers are available on macOS only.");
        return Ok(());
    }
    eprintln!("Checking installed apps…");
    crate::apps::sync(&session.nix, &session.paths).map_err(|detail| {
        eprintln!("Failed: app launchers could not be updated.");
        super::report_cause(&detail);
        eprintln!("Installed packages did not change.");
        CommandError::Reported(std::process::ExitCode::from(super::FAILURE))
    })?;
    println!("App launchers synced.");
    Ok(())
}

pub(super) fn apps_setup() -> Result<(), CommandError> {
    crate::apps::setup_storage().map_err(CommandError::Message)?;
    println!("Shared app storage is ready.");
    Ok(())
}
