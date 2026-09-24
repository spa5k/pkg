//! Small terminal presentation layer. Machine output never uses this module.

use std::io::{self, IsTerminal, Write};

/// Stream-specific presentation policy, kept explicit for render tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    terminal: bool,
    color: bool,
}

/// Semantic emphasis; meaning is also carried by text and symbols.
#[derive(Debug, Clone, Copy)]
pub enum Tone {
    /// Section or column title.
    Heading,
    /// Completed operation or passing check.
    Success,
    /// Non-blocking warning.
    Warning,
    /// Failed operation or check.
    Error,
    /// Secondary detail.
    Muted,
}

impl Style {
    /// Detect presentation for standard output.
    #[must_use]
    pub fn stdout(no_color: bool) -> Self {
        Self::detect(io::stdout().is_terminal(), no_color)
    }

    /// Detect presentation for standard error, independently of stdout.
    #[must_use]
    pub fn stderr(no_color: bool) -> Self {
        Self::detect(io::stderr().is_terminal(), no_color)
    }

    fn detect(terminal: bool, no_color: bool) -> Self {
        let terminal = terminal
            && std::env::var_os("CI").is_none()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        Self::new(
            terminal,
            !no_color && std::env::var_os("NO_COLOR").is_none(),
        )
    }

    /// Construct an explicit policy. Non-terminal output cannot contain ANSI.
    #[must_use]
    pub const fn new(terminal: bool, color: bool) -> Self {
        Self {
            terminal,
            color: terminal && color,
        }
    }

    /// Whether terminal-only symbols and layout are available.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.terminal
    }

    /// Whether redraws are allowed. `NO_COLOR` also gives a static transcript.
    #[must_use]
    pub const fn animated(self) -> bool {
        self.color
    }

    /// Style trusted, already sanitized public text.
    #[must_use]
    pub fn paint(self, text: &str, tone: Tone) -> String {
        if !self.color {
            return text.to_owned();
        }
        let code = match tone {
            Tone::Heading => "1;36",
            Tone::Success => "32",
            Tone::Warning => "33",
            Tone::Error => "31",
            Tone::Muted => "2",
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    /// Write one result with a terminal check mark and a plain fallback.
    ///
    /// # Errors
    /// Returns an error if the output stream cannot be written.
    pub fn success(self, mut writer: impl Write, text: &str) -> io::Result<()> {
        if self.terminal {
            writeln!(writer, "{} {text}", self.paint("✓", Tone::Success))
        } else {
            writeln!(writer, "{text}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirected_and_color_disabled_output_never_has_ansi() {
        for style in [Style::new(false, true), Style::new(true, false)] {
            assert_eq!(style.paint("Ready", Tone::Success), "Ready");
            assert!(!style.animated());
        }
        assert!(
            Style::new(true, true)
                .paint("Ready", Tone::Success)
                .contains("\x1b[32m")
        );
    }
}
