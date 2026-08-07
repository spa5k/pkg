//! Minimal redacted crash records without memory dumps or panic payloads.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::log::write_private_json;
use crate::ux::PUBLIC_SCHEMA_VERSION;

const MAX_OPERATION_ID_CHARS: usize = 64;

/// Coarse product phase permitted in a crash record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashPhase {
    /// CLI parsing or semantic validation.
    Cli,
    /// Read-only resolution.
    Resolve,
    /// Substitution or local build.
    Acquire,
    /// Activation transaction.
    Activate,
    /// Garbage collection.
    GarbageCollect,
    /// Repair workflow.
    Repair,
}

/// Allowlisted context captured by the panic hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashContext {
    phase: CrashPhase,
    channel_sequence: Option<u64>,
    operation_id: Option<String>,
}

impl CrashContext {
    /// Validate bounded, opaque crash context. It accepts no paths, args, or diagnostic text.
    pub fn new(
        phase: CrashPhase,
        channel_sequence: Option<u64>,
        operation_id: Option<&str>,
    ) -> io::Result<Self> {
        let operation_id = operation_id.map(str::to_owned);
        if operation_id.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.chars().count() > MAX_OPERATION_ID_CHARS
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid operation id",
            ));
        }
        Ok(Self {
            phase,
            channel_sequence,
            operation_id,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CrashRecord<'a> {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    cli_version: &'static str,
    phase: CrashPhase,
    channel_sequence: Option<u64>,
    operation_id: Option<&'a str>,
}

/// Private crash-record destination and fixed context.
#[derive(Debug, Clone)]
pub struct CrashReporter {
    path: PathBuf,
    context: CrashContext,
}

impl CrashReporter {
    /// Construct a reporter. Installing it is explicit and does not enable memory dumps.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, context: CrashContext) -> Self {
        Self {
            path: path.into(),
            context,
        }
    }

    /// Write the minimal record. Panic messages, backtraces, args, and environment are excluded.
    pub fn write_record(&self) -> io::Result<()> {
        write_record(&self.path, &self.context)
    }

    /// Install a panic hook that best-effort writes the minimal record.
    ///
    /// This replaces the process hook and intentionally does not print the panic payload.
    pub fn install(self) {
        std::panic::set_hook(Box::new(move |_| {
            let _ = self.write_record();
        }));
    }
}

fn write_record(path: &Path, context: &CrashContext) -> io::Result<()> {
    write_private_json(
        path,
        &CrashRecord {
            schema_version: PUBLIC_SCHEMA_VERSION,
            kind: "crash",
            cli_version: env!("CARGO_PKG_VERSION"),
            phase: context.phase,
            channel_sequence: context.channel_sequence,
            operation_id: context.operation_id.as_deref(),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn crash_record_is_allowlisted_and_excludes_sensitive_runtime_data() {
        let root = std::env::temp_dir().join(format!(
            "pkg-crash-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("crash.json");
        let context =
            CrashContext::new(CrashPhase::Activate, Some(7), Some("operation-42")).unwrap();
        CrashReporter::new(&path, context).write_record().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("operation-42"));
        for forbidden in ["message", "backtrace", "argv", "environment", "/nix/store"] {
            assert!(!text.contains(forbidden));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operation_id_rejects_path_like_or_unbounded_values() {
        assert!(CrashContext::new(CrashPhase::Cli, None, Some("../../secret")).is_err());
        assert!(CrashContext::new(CrashPhase::Cli, None, Some(&"x".repeat(65))).is_err());
    }
}
