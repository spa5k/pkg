//! Filtered live reporting for long native profile mutations.
//!
//! "nix profile add" and "nix profile upgrade" stream fetch and build
//! progress on stderr. Raw passthrough turns one install into dozens of
//! git transfer lines. The reporter keeps the stream readable: transfer
//! and evaluation chatter is dropped, one compact status line shows the
//! current fetch, copy, or build, and warnings, errors, and Nix's
//! interactive trust prompts always reach the user immediately. Nothing
//! is lost on failure: the raw stderr stays captured for the error
//! report.

use std::io::IsTerminal as _;
use std::io::Write as _;

use super::process::StderrSink;

/// How one complete native stderr line is reported.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    /// Print the line.
    Show,
    /// Drop the line.
    Hide,
    /// Replace the status line with this shorter progress text.
    Status(String),
}

/// Git transfer lines the fetch helper prints while Nix downloads one
/// flake input; none of them changes what the user should do next.
const GIT_TRANSFER: [&str; 9] = [
    "From ",
    "remote: ",
    "Enumerating objects:",
    "Counting objects:",
    "Compressing objects:",
    "Receiving objects:",
    "Resolving deltas:",
    "Unpacking objects:",
    "* branch ",
];

/// The compact name of one repository URL or store path: its final path
/// segment, without a ".git" suffix.
fn short_name(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("/nix/store/") {
        // Store basenames are "<32-char base32 hash>-<name>"; keep <name>.
        if let Some((hash, name)) = rest.split_once('-')
            && hash.len() == 32
            && hash
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return name.strip_suffix(".git").unwrap_or(name).to_string();
        }
    }
    let base = value.rsplit('/').next().unwrap_or(value);
    base.strip_suffix(".git").unwrap_or(base).to_string()
}

/// Classify one complete native stderr line.
fn classify(line: &str) -> Kind {
    let line = line.trim();
    if line.is_empty() {
        return Kind::Hide;
    }
    if line.starts_with("error (ignored): ") {
        // Nix marks ignored non-fatal errors with this prefix; the raw
        // text still reaches the failure diagnostic when a run fails.
        return Kind::Hide;
    }
    if let Some(rest) = line.strip_prefix("fetching Git repository '") {
        let url = rest.strip_suffix('\'').unwrap_or(rest);
        return Kind::Status(format!("fetching {}", short_name(url)));
    }
    if GIT_TRANSFER.iter().any(|prefix| line.starts_with(prefix)) {
        return Kind::Hide;
    }
    if line.starts_with("unpacking '") && line.ends_with(" into the Git cache...") {
        // A flake input being unpacked into the local git cache.
        return Kind::Hide;
    }
    if line.starts_with("warning: redirecting to ") {
        // The fetch helper's HTTP redirect notice; the retry follows.
        return Kind::Hide;
    }
    if line.starts_with("evaluating derivation ") {
        return Kind::Hide;
    }
    if line.starts_with("these ") {
        // Build and fetch plan introductions plus their store lists.
        return Kind::Hide;
    }
    if line.starts_with("/nix/store/") {
        return Kind::Hide;
    }
    if let Some(rest) = line.strip_prefix("copying path '") {
        let path = rest.split('\'').next().unwrap_or(rest);
        return Kind::Status(format!("copying {}", short_name(path)));
    }
    if let Some(rest) = line.strip_prefix("building '") {
        let path = rest.strip_suffix('\'').unwrap_or(rest);
        return Kind::Status(format!("building {}", short_name(path)));
    }
    Kind::Show
}

/// The bounded, signal-only tail of one failed run's captured stderr.
///
/// Filtered runs keep the raw stream for diagnostics, but echoing all of
/// it would replay exactly the chatter the filter removed. The tail keeps
/// only lines the classifier would show live -- warnings, errors, prompts
/// -- bounded to the last few of them; the complete text stays in the
/// store log the native nix-log pointer names.
pub fn diagnostic_tail(stderr: &str, keep: usize) -> String {
    let signal: Vec<&str> = stderr
        .lines()
        .filter(|line| matches!(classify(line), Kind::Show))
        .collect();
    if signal.is_empty() {
        return String::new();
    }
    if signal.len() <= keep {
        let mut tail = signal.join("\n");
        tail.push('\n');
        return tail;
    }
    let mut tail = String::from(
        "... earlier native output trimmed; run the reported nix log command for everything ...\n",
    );
    tail.push_str(&signal[signal.len() - keep..].join("\n"));
    tail.push('\n');
    tail
}

/// Whether an incomplete line is a Nix trust prompt, which must be shown
/// before its newline exists so the user can answer it.
fn ends_with_prompt(partial: &[u8]) -> bool {
    partial.starts_with(b"do you want to ") && partial.ends_with(b"? ")
}

/// Live stderr reporter for one native mutation child.
pub struct MutationReporter {
    /// Whether stderr is an interactive terminal.
    tty: bool,
    /// Bytes of the not yet complete line.
    partial: Vec<u8>,
    /// A trust prompt was printed; this line passes through raw.
    passthrough: bool,
    /// Whether the compact status line is on screen.
    status_active: bool,
}

impl MutationReporter {
    /// Build the reporter for one child run.
    pub fn new(tty: bool) -> Self {
        Self {
            tty,
            partial: Vec::new(),
            passthrough: false,
            status_active: false,
        }
    }

    /// Print one line, clearing any active status line first.
    fn show(&mut self, line: &str) {
        self.clear_status();
        eprintln!("{line}");
    }

    /// Replace the compact status line.
    fn status(&mut self, text: &str) {
        if !self.tty {
            return;
        }
        let short: String = text.chars().take(64).collect();
        eprint!("\r\x1b[2K  nix: {short}");
        self.status_active = true;
    }

    /// Erase the compact status line.
    fn clear_status(&mut self) {
        if self.status_active {
            eprint!("\r\x1b[2K");
            self.status_active = false;
        }
    }

    /// Write bytes the user must see unfiltered.
    fn write_raw(&mut self, bytes: &[u8]) {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(bytes);
        let _ = stderr.flush();
    }

    /// Report one complete line.
    fn line(&mut self, text: &str) {
        match classify(text) {
            Kind::Show => self.show(text),
            Kind::Hide => {}
            Kind::Status(text) => self.status(&text),
        }
    }
}

impl StderrSink for MutationReporter {
    fn chunk(&mut self, bytes: &[u8]) {
        if self.passthrough {
            if let Some(newline) = bytes.iter().position(|&b| b == b'\n') {
                self.write_raw(&bytes[..=newline]);
                self.passthrough = false;
                if bytes.len() > newline + 1 {
                    self.chunk(&bytes[newline + 1..]);
                }
                return;
            }
            self.write_raw(bytes);
            return;
        }
        self.partial.extend_from_slice(bytes);
        while let Some(newline) = self.partial.iter().position(|&b| b == b'\n') {
            let drained: Vec<u8> = self.partial.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&drained[..newline]).into_owned();
            self.line(text.trim_end_matches('\r'));
        }
        if ends_with_prompt(&self.partial) {
            self.passthrough = true;
            let prompt = std::mem::take(&mut self.partial);
            self.write_raw(&prompt);
        }
    }

    fn finish(&mut self, success: bool) {
        let _ = success;
        if !self.passthrough && !self.partial.is_empty() {
            let text = String::from_utf8_lossy(&self.partial).into_owned();
            self.line(text.trim_end_matches('\r'));
            self.partial.clear();
        }
        self.clear_status();
    }
}

/// The reporter wired into native profile mutations.
pub fn mutation_reporter() -> MutationReporter {
    MutationReporter::new(std::io::stderr().is_terminal())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_diagnostics_keep_signal_lines_only() {
        let noisy: String = (0..60)
            .map(|i| format!("fetching Git repository 'https://example.com/r{}'\n", i))
            .collect();
        let stderr = format!(
            "{noisy}error: Cannot build '/nix/store/aaa-tool.drv'.\nFor full logs, run:\n  nix log /nix/store/aaa-tool.drv\n"
        );
        let tail = diagnostic_tail(&stderr, 40);
        assert!(tail.contains("error: Cannot build"), "{tail}");
        assert!(tail.contains("nix log /nix/store/aaa-tool.drv"), "{tail}");
        assert!(!tail.contains("fetching Git repository"), "{tail}");
        assert_eq!(diagnostic_tail("one\nline\n", 40), "one\nline\n");
        assert_eq!(diagnostic_tail("fetching Git repository 'x'\n", 40), "");
    }

    #[test]
    fn fetch_progress_is_shortened_and_transfer_chatter_is_hidden() {
        assert_eq!(
            classify("fetching Git repository 'https://gitlab.com/gabmus/tree-sitter-blueprint'"),
            Kind::Status(String::from("fetching tree-sitter-blueprint"))
        );
        for line in [
            "remote: Enumerating objects: 32, done.",
            "Receiving objects: 100% (32/32), 42.45 KiB | 1.70 MiB/s, done.",
            "Unpacking objects: 100% (28/28), done.",
            "From https://gitlab.com/gabmus/tree-sitter-blueprint",
            "* branch 355ef84e -> FETCH_HEAD",
            "warning: redirecting to https://gitlab.com/gabmus/tree-sitter-blueprint.git/",
            "evaluating derivation 'github:helix-editor/helix#helix'...",
            "these 3 derivations will be built:",
            "these 14 paths will be fetched (73.0 MiB download, 465.5 MiB unpacked):",
            "  /nix/store/nsh0cwzk8kxsq5s8znsp9qpq1dx2jl6y-cask-rectangle.drv",
            "unpacking 'github:numtide/flake-utils/11707dc2' into the Git cache...",
            "error (ignored): opening file \"/etc/nix/sentry-endpoint\": Permission denied",
        ] {
            assert_eq!(classify(line), Kind::Hide, "{line}");
        }
    }

    #[test]
    fn warnings_errors_and_prompts_stay_visible() {
        for line in [
            "warning: ignoring untrusted substituter 'https://helix.cachix.org'",
            "error: Cannot build the requested derivation.",
            "do you want to permanently mark this value as trusted (y/N)?",
        ] {
            assert_eq!(classify(line), Kind::Show, "{line}");
        }
    }

    #[test]
    fn store_work_becomes_compact_status() {
        assert_eq!(
            classify(
                "copying path '/nix/store/mwfmi86ii02c4zlb3z40s09jww8dxqq2-file-5.47-dev' from 'https://cache.nixos.org'..."
            ),
            Kind::Status(String::from("copying file-5.47-dev"))
        );
        assert_eq!(
            classify(
                "building '/nix/store/nsh0cwzk8kxsq5s8znsp9qpq1dx2jl6y-cask-rectangle-0.99.drv'"
            ),
            Kind::Status(String::from("building cask-rectangle-0.99.drv"))
        );
    }

    #[test]
    fn trust_prompts_are_detected_without_a_newline() {
        assert!(ends_with_prompt(
            b"do you want to allow configuration setting 'extra-substituters' (y/N)? "
        ));
        assert!(!ends_with_prompt(
            b"fetching Git repository 'https://example.com/owner/repo'"
        ));
    }
}
