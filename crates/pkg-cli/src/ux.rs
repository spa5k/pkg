//! Output-mode, terminal, and stable error-envelope behavior.

use std::io::{self, IsTerminal, Write};

use serde::Serialize;

use crate::exit::ExitCode;

/// Version carried by every public machine-readable record.
pub const PUBLIC_SCHEMA_VERSION: u64 = 1;
const MAX_PUBLIC_TEXT_CHARS: usize = 1024;

/// Mutually exclusive presentation mode selected by global flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Human-readable output.
    #[default]
    Human,
    /// Exactly one final JSON document.
    Json,
    /// Public progress records followed by one terminal NDJSON result.
    JsonLines,
}

impl OutputMode {
    /// Map already-validated global flags to an output mode.
    #[must_use]
    pub const fn from_flags(json: bool, jsonl: bool) -> Self {
        match (json, jsonl) {
            (true, false) => Self::Json,
            (false, true) => Self::JsonLines,
            _ => Self::Human,
        }
    }
}

/// Terminal capabilities sampled by the inline renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    stdin: bool,
    stdout: bool,
    stderr: bool,
    color: bool,
}

impl Terminal {
    /// Sample the current process standard streams and color policy.
    #[must_use]
    pub fn detect(no_color: bool) -> Self {
        let stdin = io::stdin().is_terminal();
        let stdout = io::stdout().is_terminal();
        let stderr = io::stderr().is_terminal();
        let color = stdout && !no_color && std::env::var_os("NO_COLOR").is_none();
        Self {
            stdin,
            stdout,
            stderr,
            color,
        }
    }

    /// Construct explicit capabilities for deterministic tests and injected frontends.
    #[must_use]
    pub const fn new(stdin: bool, stdout: bool, stderr: bool, color: bool) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            color: color && stdout,
        }
    }

    /// Whether interactive approval prompts are possible.
    #[must_use]
    pub const fn can_prompt(self) -> bool {
        self.stdin && self.stderr
    }

    /// Whether in-place progress rendering is appropriate.
    #[must_use]
    pub const fn inline_progress(self) -> bool {
        self.stdout
    }

    /// Whether ANSI styling may be used.
    #[must_use]
    pub const fn color(self) -> bool {
        self.color
    }
}

/// Sanitized command failure with a stable code and remediation hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    exit_code: ExitCode,
    message: String,
    hint: String,
}

impl CommandError {
    /// Build a bounded, single-line public error.
    #[must_use]
    pub fn new(exit_code: ExitCode, message: impl AsRef<str>, hint: impl AsRef<str>) -> Self {
        Self {
            exit_code,
            message: sanitize_public_text(message.as_ref()),
            hint: sanitize_public_text(hint.as_ref()),
        }
    }

    /// Stable exit code.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        self.exit_code
    }

    /// Bounded public message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Bounded remediation hint.
    #[must_use]
    pub fn hint(&self) -> &str {
        &self.hint
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorDetail<'a> {
    symbol: &'static str,
    code: u8,
    message: &'a str,
    hint: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FinalError<'a> {
    schema_version: u64,
    ok: bool,
    command: &'a str,
    error: ErrorDetail<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalError<'a> {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    ok: bool,
    command: &'a str,
    error: ErrorDetail<'a>,
}

/// Render one error according to the selected public output contract.
pub fn write_error(
    mut stdout: impl Write,
    mut stderr: impl Write,
    mode: OutputMode,
    command: &str,
    error: &CommandError,
) -> io::Result<()> {
    let detail = ErrorDetail {
        symbol: error.exit_code.symbol(),
        code: error.exit_code.as_u8(),
        message: error.message(),
        hint: error.hint(),
    };
    match mode {
        OutputMode::Human => {
            writeln!(stderr, "error[{}]: {}", detail.symbol, detail.message)?;
            writeln!(stderr, "hint: {}", detail.hint)
        }
        OutputMode::Json => write_json_line(
            &mut stdout,
            &FinalError {
                schema_version: PUBLIC_SCHEMA_VERSION,
                ok: false,
                command,
                error: detail,
            },
        ),
        OutputMode::JsonLines => write_json_line(
            &mut stdout,
            &TerminalError {
                schema_version: PUBLIC_SCHEMA_VERSION,
                kind: "result",
                ok: false,
                command,
                error: detail,
            },
        ),
    }
}

/// Write one compact JSON value followed by exactly one newline.
pub fn write_json_line(mut writer: impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

pub(crate) fn sanitize_public_text(value: &str) -> String {
    let bounded: String = value
        .chars()
        .map(|character| {
            if matches!(character, '\n' | '\r' | '\t') {
                ' '
            } else {
                character
            }
        })
        .take(MAX_PUBLIC_TEXT_CHARS)
        .collect();
    bounded
        .split_whitespace()
        .map(|word| {
            if word.contains("/nix/store/")
                || word.ends_with(".drv")
                || word.starts_with("github:")
                || word.starts_with("flake:")
            {
                "[private-detail-omitted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_terminal_never_uses_color_or_prompts() {
        let terminal = Terminal::new(false, false, true, true);
        assert!(!terminal.can_prompt());
        assert!(!terminal.inline_progress());
        assert!(!terminal.color());
    }

    #[test]
    fn json_is_one_versioned_document_and_jsonl_is_one_terminal_record() {
        let error = CommandError::new(ExitCode::BuildFailed, "failed\nline", "retry\tlater");
        for (mode, expected_type) in [
            (OutputMode::Json, None),
            (OutputMode::JsonLines, Some("result")),
        ] {
            let mut stdout = Vec::new();
            write_error(&mut stdout, Vec::new(), mode, "install", &error).unwrap();
            assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
            let value: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(value["schemaVersion"], PUBLIC_SCHEMA_VERSION);
            assert_eq!(value.get("type").and_then(|v| v.as_str()), expected_type);
            assert_eq!(value["error"]["code"], 69);
            assert_eq!(value["error"]["message"], "failed line");
        }
    }

    #[test]
    fn human_error_goes_only_to_stderr() {
        let error = CommandError::new(ExitCode::Config, "bad config", "run pkg doctor");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        write_error(
            &mut stdout,
            &mut stderr,
            OutputMode::Human,
            "doctor",
            &error,
        )
        .unwrap();
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "error[CONFIG]: bad config\nhint: run pkg doctor\n"
        );
    }

    #[test]
    fn raw_managed_runtime_identities_are_redacted_at_the_public_error_boundary() {
        let error = CommandError::new(
            ExitCode::VerifyFail,
            "failed /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-secret",
            "inspect github:NixOS/nixpkgs/secret",
        );
        assert_eq!(error.message(), "failed [private-detail-omitted]");
        assert_eq!(error.hint(), "inspect [private-detail-omitted]");
    }
}
