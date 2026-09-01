//! Tests for the `completion` module.

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
