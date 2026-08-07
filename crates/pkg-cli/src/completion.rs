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
mod tests {
    use super::*;

    #[test]
    fn every_supported_shell_receives_nonempty_static_completion_source() {
        for (shell, marker) in [
            (CompletionShell::Bash, "_pkg"),
            (CompletionShell::Zsh, "#compdef pkg"),
            (CompletionShell::Fish, "complete -c pkg"),
            (CompletionShell::Powershell, "Register-ArgumentCompleter"),
        ] {
            let mut output = Vec::new();
            write_completion(shell, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains(marker), "missing {marker:?} for {shell:?}");
            assert!(output.contains("install"));
            assert!(output.contains("repair"));
        }
    }
}
