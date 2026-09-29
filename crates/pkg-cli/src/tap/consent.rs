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
}

/// Ask for consent on the terminal.
///
/// Interactive input reads one line and requires an explicit `y` or
/// `yes`; everything else — including end of input — is a refusal. When
/// stdin is not a terminal the prompt is skipped and consent is refused:
/// automation must pass `--trust`.
#[must_use]
pub fn ask(source: &str, origin: &str, trusted: bool) -> Decision {
    if trusted {
        return Decision::Approved;
    }
    if !stdin_is_terminal() {
        eprintln!(
            "pkg: tap add needs approval for {origin}; \
             pass --trust in noninteractive runs"
        );
        return Decision::Refused;
    }
    println!("pkg will run this tap's Ruby code under our sandboxed Nix build:");
    println!("  source: {source}");
    println!("  repository: {origin}");
    println!("  approval covers imports and updates of this source only");
    print!("approve? [y/N] ");
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
        _ => {
            println!("not approved; nothing was fetched or changed");
            Decision::Refused
        }
    }
}

/// Whether standard input is a terminal.
fn stdin_is_terminal() -> bool {
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
