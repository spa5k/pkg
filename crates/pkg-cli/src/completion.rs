//! Static shell-completion generation from the canonical clap grammar.

use std::io::{self, Write};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::{Cli, CompletionShell};

/// Emit static completion source for one supported shell.
pub fn write_completion(shell: CompletionShell, mut writer: impl Write) -> io::Result<()> {
    let mut command = Cli::command();
    generate(completion_shell(shell), &mut command, "pkg", &mut writer);
    Ok(())
}

const fn completion_shell(value: CompletionShell) -> Shell {
    match value {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
        CompletionShell::Powershell => Shell::PowerShell,
    }
}

#[cfg(test)]
mod tests;
