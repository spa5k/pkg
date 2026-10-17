//! Filtered live reporting for long native profile mutations.
//!
//! Show the current fetch, copy, or build in one status line. Filter transfer
//! and evaluation messages. Show warnings and interactive trust prompts as
//! they arrive. Capture raw stderr for error decoding and show it with
//! `--verbose`. Commands report failures after checking the profile.

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::time::Instant;

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

/// Native chatter and errors reported after the profile check.
const HIDDEN_PREFIXES: &[&str] = &[
    "From ",
    "remote: ",
    "Enumerating objects:",
    "Counting objects:",
    "Compressing objects:",
    "Receiving objects:",
    "Resolving deltas:",
    "Unpacking objects:",
    "* branch ",
    "error (ignored): ",
    "error:",
    "❌ ",
    "Reason:",
    "Output paths:",
    "> ",
    "For full logs, run:",
    "nix log ",
    "failed to allocate ",
    "warning: redirecting to ",
    "evaluating derivation ",
    "these ",
    "/nix/store/",
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
    if line.is_empty()
        || HIDDEN_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        || line.starts_with("removing '") && line.ends_with('\'')
        || line.starts_with("Last ") && line.contains(" log lines:")
        || line.starts_with("unpacking '") && line.ends_with(" into the Git cache...")
    {
        return Kind::Hide;
    }
    if let Some(rest) = line.strip_prefix("fetching Git repository '") {
        let url = rest.strip_suffix('\'').unwrap_or(rest);
        return Kind::Status(format!("fetching {}", short_name(url)));
    }
    if let Some(rest) = line.strip_prefix("copying path '") {
        let path = rest.split('\'').next().unwrap_or(rest);
        return Kind::Status(format!("downloading {}", short_name(path)));
    }
    if let Some(rest) = line.strip_prefix("building '") {
        let path = rest.strip_suffix('\'').unwrap_or(rest);
        return Kind::Status(format!("building {}", short_name(path)));
    }
    Kind::Show
}

/// Whether an incomplete line is a Nix trust prompt, which must be shown
/// before its newline exists so the user can answer it.
fn ends_with_prompt(partial: &[u8]) -> bool {
    partial.starts_with(b"do you want to ") && partial.ends_with(b"? ")
}

/// Live stderr reporter for one native mutation child.
pub struct MutationReporter {
    line: PartialLine,
    display: DisplayState,
    filtering: FilterState,
    profile: String,
    verbose: bool,
    started: Instant,
    work: String,
    frame: usize,
    last_render: Option<Instant>,
}

#[derive(Default)]
struct PartialLine {
    bytes: Vec<u8>,
    passthrough: bool,
}

#[derive(Default)]
struct DisplayState {
    tty: bool,
    status_active: bool,
}

#[derive(Default)]
struct FilterState {
    lock_diff: bool,
    file_conflict: bool,
}

impl MutationReporter {
    /// Build the reporter for one child run.
    pub fn new(profile: &str, verbose: bool) -> Self {
        let tty = std::io::stderr().is_terminal();
        Self {
            line: PartialLine::default(),
            display: DisplayState {
                tty,
                ..DisplayState::default()
            },
            filtering: FilterState::default(),
            started: Instant::now(),
            work: String::from("preparing"),
            frame: 0,
            last_render: None,
            profile: profile.to_string(),
            verbose,
        }
    }

    /// Print one line, clearing any active status line first.
    fn show(&mut self, line: &str) {
        self.clear_status();
        eprintln!("{line}");
    }

    /// Replace the compact status line.
    fn status(&mut self, text: &str) {
        self.work = text.chars().take(64).collect();
        self.tick();
    }

    /// Erase the compact status line.
    fn clear_status(&mut self) {
        if self.display.status_active {
            eprint!("\r\x1b[2K");
            self.display.status_active = false;
            self.last_render = None;
        }
    }

    /// Write bytes the user must see unfiltered.
    fn write_raw(bytes: &[u8]) {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(bytes);
        let _ = stderr.flush();
    }

    /// Report one complete line.
    fn line(&mut self, text: &str) {
        if self.verbose {
            self.show(text);
            return;
        }
        let plain = crate::output::strip_ansi(text);
        let line = plain.trim();
        if super::conflict::starts_conflict(line) {
            self.filtering.file_conflict = true;
            return;
        }
        if self.filtering.file_conflict {
            if line.starts_with("warning:") || line.starts_with("do you want to ") {
                self.show(text);
                return;
            }
            if !line.starts_with("error:") {
                return;
            }
            self.filtering.file_conflict = false;
        }
        if let Some(scoped) = scoped_hint(line, &self.profile) {
            self.show(&scoped);
            return;
        }
        if line.starts_with("warning: not writing modified lock file of flake ") {
            // Input overrides deliberately do not write the upstream lock.
            // This warning and its diff are expected setup detail; verbose
            // output retains them. Other warnings still pass through.
            self.filtering.lock_diff = true;
            return;
        }
        if self.filtering.lock_diff {
            if line.starts_with("• ") || line.starts_with('\'') || line.starts_with("→ '") {
                return;
            }
            self.filtering.lock_diff = false;
        }
        match classify(line) {
            Kind::Show => self.show(text),
            Kind::Hide => {}
            Kind::Status(text) => self.status(&text),
        }
    }
}

fn scoped_hint(line: &str, profile: &str) -> Option<String> {
    let plain = crate::output::strip_ansi(line);
    let line = plain.trim();
    let rest = line.strip_prefix("nix profile ")?;
    let (command, arguments) = rest.split_once(' ')?;
    if !matches!(command, "remove" | "add" | "install" | "upgrade") {
        return None;
    }
    let profile = format!("'{}'", profile.replace('\'', "'\\''"));
    if arguments.contains("--profile") {
        return Some(format!(
            "  nix profile list --profile {profile}\n  Use pkg remove with the conflicting entry ID."
        ));
    }
    Some(format!(
        "  nix profile {command} --profile {profile} {arguments}"
    ))
}

impl StderrSink for MutationReporter {
    fn chunk(&mut self, bytes: &[u8]) {
        if self.line.passthrough {
            if let Some(newline) = bytes.iter().position(|&b| b == b'\n') {
                Self::write_raw(&bytes[..=newline]);
                self.line.passthrough = false;
                if bytes.len() > newline + 1 {
                    self.chunk(&bytes[newline + 1..]);
                }
                return;
            }
            Self::write_raw(bytes);
            return;
        }
        self.line.bytes.extend_from_slice(bytes);
        while let Some(newline) = self.line.bytes.iter().position(|&b| b == b'\n') {
            let drained: Vec<u8> = self.line.bytes.drain(..=newline).collect();
            let text = String::from_utf8_lossy(&drained[..newline]).into_owned();
            self.line(text.trim_end_matches('\r'));
        }
        if ends_with_prompt(&self.line.bytes) {
            self.line.passthrough = true;
            let prompt = std::mem::take(&mut self.line.bytes);
            self.clear_status();
            Self::write_raw(&prompt);
        }
    }

    fn tick(&mut self) {
        if !self.display.tty || self.verbose || self.line.passthrough || !self.line.bytes.is_empty()
        {
            return;
        }
        let now = Instant::now();
        if self
            .last_render
            .is_some_and(|last| now.duration_since(last).as_millis() < 250)
        {
            return;
        }
        self.last_render = Some(now);
        let frame = ['|', '/', '-', '\\'][self.frame % 4];
        self.frame = self.frame.wrapping_add(1);
        eprint!(
            "\r\x1b[2K  {frame} {} ({}s)",
            self.work,
            self.started.elapsed().as_secs()
        );
        self.display.status_active = true;
    }

    fn finish(&mut self, success: bool) {
        let _ = success;
        if !self.line.passthrough && !self.line.bytes.is_empty() {
            let text = String::from_utf8_lossy(&self.line.bytes).into_owned();
            self.line(text.trim_end_matches('\r'));
            self.line.bytes.clear();
        }
        self.clear_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "removing 'github:NixOS/nixpkgs#legacyPackages.aarch64-darwin.fff'",
        ] {
            assert_eq!(classify(line), Kind::Hide, "{line}");
        }
    }

    #[test]
    fn warnings_and_prompts_stay_visible_but_errors_wait_for_summary() {
        for line in [
            "warning: ignoring untrusted substituter 'https://helix.cachix.org'",
            "do you want to permanently mark this value as trusted (y/N)?",
        ] {
            assert_eq!(classify(line), Kind::Show, "{line}");
        }
        assert_eq!(
            classify("error: Cannot build the requested derivation."),
            Kind::Hide
        );
        assert_eq!(
            classify("failed to allocate 1048576 bytes at 0x300100000"),
            Kind::Hide
        );
        assert_eq!(
            classify("❌ github:hraban/mac-app-util/039f33de#"),
            Kind::Hide
        );
    }

    #[test]
    fn store_work_becomes_compact_status() {
        assert_eq!(
            classify(
                "copying path '/nix/store/mwfmi86ii02c4zlb3z40s09jww8dxqq2-file-5.47-dev' from 'https://cache.nixos.org'..."
            ),
            Kind::Status(String::from("downloading file-5.47-dev"))
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
