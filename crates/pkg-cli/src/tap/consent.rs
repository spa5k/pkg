//! Consent before any tap Ruby, fetch, or import runs.
//!
//! The prompt names the canonical repository and the approval scope:
//! updates of the requested source. Approval is remembered per origin in
//! the registry; automation must opt in explicitly with `--trust`. A
//! refusal changes nothing.

use std::io::{BufRead, Write};

/// What one consent check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The user approved this origin and scope.
    Approved,
    /// The user refused; nothing may run.
    Refused,
    /// No terminal can ask, and `--trust` is absent; nothing may run.
    NoTerminal,
}

/// Ask for consent on the terminal.
///
/// Read one line and accept only `y` or `yes`. Refuse all other input,
/// including end of input. Without a terminal, require `--trust`.
#[must_use]
pub fn ask(_source: &str, origin: &str, trusted: bool) -> Decision {
    if trusted {
        return Decision::Approved;
    }
    if !stdin_is_terminal() {
        return Decision::NoTerminal;
    }
    print!("pkg will run Ruby code from {origin} in a sandboxed Nix build; approve? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer);
    match read {
        Ok(_)
            if answer.trim().eq_ignore_ascii_case("y")
                || answer.trim().eq_ignore_ascii_case("yes") =>
        {
            Decision::Approved
        }
        _ => Decision::Refused,
    }
}

/// Whether standard input is a terminal.
pub(crate) fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_flag_approves_without_input() {
        assert_eq!(
            ask(
                "somebody/apps",
                "https://github.com/somebody/homebrew-apps",
                true
            ),
            Decision::Approved
        );
    }
}
