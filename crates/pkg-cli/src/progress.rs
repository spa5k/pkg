//! Sanitized public progress-event schema used by JSONL and user-owned logs.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use serde::Serialize;

use crate::ux::{PUBLIC_SCHEMA_VERSION, json_line_bytes};

const MAX_PUBLIC_FIELD_CHARS: usize = 256;

/// Validation failure while constructing a public progress event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressError {
    /// A required product-owned text field is empty, oversized, or contains controls.
    InvalidPublicField(&'static str),
    /// A field resembles a private managed-runtime identity.
    PrivateIdentity(&'static str),
    /// Download byte counters are contradictory.
    InvalidByteProgress,
    /// Best-effort progress is not a finite value in the inclusive range 0–1.
    InvalidPercentage,
    /// Collision providers are empty or not distinct.
    InvalidSelectors,
    /// A collision file is not a normalized relative path.
    InvalidRelativePath,
}

impl fmt::Display for ProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPublicField(field) => write!(formatter, "invalid public field {field}"),
            Self::PrivateIdentity(field) => {
                write!(formatter, "private runtime identity refused in {field}")
            }
            Self::InvalidByteProgress => formatter.write_str("download progress exceeds total"),
            Self::InvalidPercentage => {
                formatter.write_str("progress must be finite and within 0..=1")
            }
            Self::InvalidSelectors => formatter.write_str("collision selectors must be distinct"),
            Self::InvalidRelativePath => {
                formatter.write_str("collision file must be a normalized relative path")
            }
        }
    }
}

impl std::error::Error for ProgressError {}

/// Product-owned public progress event with a fixed schema version.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PublicEvent(EventKind);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventKind {
    Phase(PhaseEvent),
    DownloadStarted(DownloadStartedEvent),
    DownloadProgress(DownloadProgressEvent),
    BuildStarted(BuildStartedEvent),
    BuildProgress(BuildProgressEvent),
    Collision(CollisionEvent),
    Committed(CommittedEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PhaseEvent {
    schema_version: u64,
    op_id: String,
    phase: String,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadStartedEvent {
    schema_version: u64,
    op_id: String,
    selector: String,
    bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgressEvent {
    schema_version: u64,
    op_id: String,
    selector: String,
    done: u64,
    total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildStartedEvent {
    schema_version: u64,
    op_id: String,
    selector: String,
    package_name: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildProgressEvent {
    schema_version: u64,
    op_id: String,
    selector: String,
    pct: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollisionEvent {
    schema_version: u64,
    op_id: String,
    file: String,
    selectors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommittedEvent {
    schema_version: u64,
    op_id: String,
    generation_id: String,
}

impl PublicEvent {
    /// Construct a phase event.
    pub fn phase(
        op_id: impl AsRef<str>,
        phase: impl AsRef<str>,
        status: impl AsRef<str>,
    ) -> Result<Self, ProgressError> {
        Ok(Self(EventKind::Phase(PhaseEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            phase: product_text("phase", phase.as_ref())?,
            status: product_text("status", status.as_ref())?,
        })))
    }

    /// Construct a download-start event.
    pub fn download_started(
        op_id: impl AsRef<str>,
        selector: impl AsRef<str>,
        bytes: u64,
    ) -> Result<Self, ProgressError> {
        Ok(Self(EventKind::DownloadStarted(DownloadStartedEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            selector: product_text("selector", selector.as_ref())?,
            bytes,
        })))
    }

    /// Construct a download-progress event.
    pub fn download_progress(
        op_id: impl AsRef<str>,
        selector: impl AsRef<str>,
        done: u64,
        total: u64,
    ) -> Result<Self, ProgressError> {
        if done > total {
            return Err(ProgressError::InvalidByteProgress);
        }
        Ok(Self(EventKind::DownloadProgress(DownloadProgressEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            selector: product_text("selector", selector.as_ref())?,
            done,
            total,
        })))
    }

    /// Construct a build-start event.
    pub fn build_started(
        op_id: impl AsRef<str>,
        selector: impl AsRef<str>,
        package_name: impl AsRef<str>,
        version: impl AsRef<str>,
    ) -> Result<Self, ProgressError> {
        Ok(Self(EventKind::BuildStarted(BuildStartedEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            selector: product_text("selector", selector.as_ref())?,
            package_name: product_text("packageName", package_name.as_ref())?,
            version: product_text("version", version.as_ref())?,
        })))
    }

    /// Construct a best-effort build-progress event.
    pub fn build_progress(
        op_id: impl AsRef<str>,
        selector: impl AsRef<str>,
        pct: f64,
    ) -> Result<Self, ProgressError> {
        if !pct.is_finite() || !(0.0..=1.0).contains(&pct) {
            return Err(ProgressError::InvalidPercentage);
        }
        Ok(Self(EventKind::BuildProgress(BuildProgressEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            selector: product_text("selector", selector.as_ref())?,
            pct,
        })))
    }

    /// Construct a deterministic activation-collision event.
    pub fn collision(
        op_id: impl AsRef<str>,
        file: impl AsRef<str>,
        selectors: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, ProgressError> {
        let file = normalized_relative_path(file.as_ref())?;
        let selectors = selectors
            .into_iter()
            .map(|selector| product_text("selectors", selector.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if selectors.len() < 2 {
            return Err(ProgressError::InvalidSelectors);
        }
        Ok(Self(EventKind::Collision(CollisionEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            file,
            selectors: selectors.into_iter().collect(),
        })))
    }

    /// Construct a generation-commit event.
    pub fn committed(
        op_id: impl AsRef<str>,
        generation_id: impl AsRef<str>,
    ) -> Result<Self, ProgressError> {
        Ok(Self(EventKind::Committed(CommittedEvent {
            schema_version: PUBLIC_SCHEMA_VERSION,
            op_id: product_text("opId", op_id.as_ref())?,
            generation_id: product_text("generationId", generation_id.as_ref())?,
        })))
    }

    /// Serialize one independently self-describing NDJSON record.
    pub fn write_ndjson(&self, writer: impl Write) -> io::Result<()> {
        let mut writer = writer;
        writer.write_all(&self.to_ndjson_line()?)
    }

    /// Serialize one complete record for both live output and durable mirroring.
    pub fn to_ndjson_line(&self) -> io::Result<Vec<u8>> {
        json_line_bytes(self)
    }

    /// Return the validated product operation identity carried by this event.
    #[must_use]
    pub fn op_id(&self) -> &str {
        match &self.0 {
            EventKind::Phase(event) => &event.op_id,
            EventKind::DownloadStarted(event) => &event.op_id,
            EventKind::DownloadProgress(event) => &event.op_id,
            EventKind::BuildStarted(event) => &event.op_id,
            EventKind::BuildProgress(event) => &event.op_id,
            EventKind::Collision(event) => &event.op_id,
            EventKind::Committed(event) => &event.op_id,
        }
    }

    /// Render one stable line for the human progress stream.
    pub fn write_human(&self, mut writer: impl Write) -> io::Result<()> {
        match &self.0 {
            EventKind::Phase(event) => writeln!(writer, "{}: {}", event.phase, event.status),
            EventKind::DownloadStarted(event) => writeln!(
                writer,
                "Downloading {} ({} bytes)",
                event.selector, event.bytes
            ),
            EventKind::DownloadProgress(event) => writeln!(
                writer,
                "Downloading {}: {}/{} bytes",
                event.selector, event.done, event.total
            ),
            EventKind::BuildStarted(event) => writeln!(
                writer,
                "Building {} {} ({})",
                event.package_name, event.version, event.selector
            ),
            EventKind::BuildProgress(event) => {
                writeln!(
                    writer,
                    "Building {}: {:.0}%",
                    event.selector,
                    event.pct * 100.0
                )
            }
            EventKind::Collision(event) => writeln!(
                writer,
                "Collision at {}: {}",
                event.file,
                event.selectors.join(", ")
            ),
            EventKind::Committed(event) => {
                writeln!(writer, "Committed {}", event.generation_id)
            }
        }
    }
}

fn product_text(field: &'static str, value: &str) -> Result<String, ProgressError> {
    if value.is_empty()
        || value.chars().count() > MAX_PUBLIC_FIELD_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(ProgressError::InvalidPublicField(field));
    }
    if value.contains("/nix/store/")
        || value.ends_with(".drv")
        || value.starts_with("github:")
        || value.starts_with("flake:")
    {
        return Err(ProgressError::PrivateIdentity(field));
    }
    Ok(value.to_owned())
}

fn normalized_relative_path(value: &str) -> Result<String, ProgressError> {
    if value.is_empty()
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        Err(ProgressError::InvalidRelativePath)
    } else {
        product_text("file", value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_versioned_newline_terminated_and_product_owned() {
        let events = [
            PublicEvent::phase("op_1", "acquire", "started").unwrap(),
            PublicEvent::download_started("op_1", "ripgrep", 42).unwrap(),
            PublicEvent::committed("op_1", "gen-42").unwrap(),
        ];
        for event in events {
            let mut bytes = Vec::new();
            event.write_ndjson(&mut bytes).unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["schemaVersion"], PUBLIC_SCHEMA_VERSION);
            let encoded = String::from_utf8(bytes).unwrap();
            for forbidden in [
                "storePath",
                "deriver",
                "attribute",
                "flakeRef",
                "argv",
                "substituter",
            ] {
                assert!(!encoded.contains(forbidden));
            }
        }
    }

    #[test]
    fn invalid_progress_and_private_identities_fail_closed() {
        assert_eq!(
            PublicEvent::build_progress("op_1", "x", f64::NAN),
            Err(ProgressError::InvalidPercentage)
        );
        assert_eq!(
            PublicEvent::download_progress("op_1", "x", 2, 1),
            Err(ProgressError::InvalidByteProgress)
        );
        assert!(matches!(
            PublicEvent::build_started(
                "op_1",
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x",
                "x",
                "1"
            ),
            Err(ProgressError::PrivateIdentity("selector"))
        ));
        assert_eq!(
            PublicEvent::collision("op_1", "../bin/x", ["a", "b"]),
            Err(ProgressError::InvalidRelativePath)
        );
        assert_eq!(
            PublicEvent::collision("op_1", "bin/x", ["a", "a"]),
            Err(ProgressError::InvalidSelectors)
        );
    }
}
