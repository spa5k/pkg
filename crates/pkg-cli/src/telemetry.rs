//! Explicitly enabled, local-only aggregate telemetry.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::Serialize;

use pkg_core::System;

use crate::log::write_private_json;
use crate::ux::PUBLIC_SCHEMA_VERSION;

/// Explicit local telemetry policy. Disabled is the safe default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TelemetryPolicy {
    /// Do not collect or write telemetry.
    #[default]
    Disabled,
    /// Collect only the typed aggregate metrics in this module.
    Enabled,
}

/// Fixed metric names; package names, paths, and argument-derived names are impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Resolution wall-clock time in milliseconds.
    ResolveMilliseconds,
    /// Successful cache substitutions.
    CacheHitCount,
    /// Cache misses.
    CacheMissCount,
    /// Successful installs.
    InstallSuccessCount,
    /// Failed installs.
    InstallFailureCount,
    /// Index build wall-clock time in milliseconds.
    IndexBuildMilliseconds,
    /// Bytes reclaimed by garbage collection.
    GcBytesReclaimed,
}

impl Metric {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ResolveMilliseconds => "resolve_milliseconds",
            Self::CacheHitCount => "cache_hit_count",
            Self::CacheMissCount => "cache_miss_count",
            Self::InstallSuccessCount => "install_success_count",
            Self::InstallFailureCount => "install_failure_count",
            Self::IndexBuildMilliseconds => "index_build_milliseconds",
            Self::GcBytesReclaimed => "gc_bytes_reclaimed",
        }
    }
}

/// Product metadata allowed in a local telemetry snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryMetadata {
    /// Monotonic signed-channel sequence when known.
    pub channel_sequence: Option<u64>,
    /// Supported host system.
    pub system: System,
}

/// In-memory aggregate telemetry recorder that never transmits data.
#[derive(Debug)]
pub struct TelemetryRecorder {
    policy: TelemetryPolicy,
    path: PathBuf,
    metadata: TelemetryMetadata,
    metrics: BTreeMap<&'static str, u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TelemetrySnapshot<'a> {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    product_version: &'static str,
    channel_sequence: Option<u64>,
    system: &'static str,
    metrics: &'a BTreeMap<&'static str, u64>,
}

impl TelemetryRecorder {
    /// Construct a recorder. No file is created while policy is disabled.
    #[must_use]
    pub fn new(
        policy: TelemetryPolicy,
        path: impl Into<PathBuf>,
        metadata: TelemetryMetadata,
    ) -> Self {
        Self {
            policy,
            path: path.into(),
            metadata,
            metrics: BTreeMap::new(),
        }
    }

    /// Add a saturating aggregate value. Disabled recorders remain empty.
    pub fn add(&mut self, metric: Metric, value: u64) {
        if self.policy == TelemetryPolicy::Enabled {
            let total = self.metrics.entry(metric.as_str()).or_default();
            *total = total.saturating_add(value);
        }
    }

    /// Persist one private local snapshot. Disabled recorders perform no filesystem operation.
    pub fn flush(&self) -> io::Result<()> {
        if self.policy == TelemetryPolicy::Disabled {
            return Ok(());
        }
        write_private_json(
            &self.path,
            &TelemetrySnapshot {
                schema_version: PUBLIC_SCHEMA_VERSION,
                kind: "telemetry",
                product_version: env!("CARGO_PKG_VERSION"),
                channel_sequence: self.metadata.channel_sequence,
                system: self.metadata.system.as_str(),
                metrics: &self.metrics,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn metadata() -> TelemetryMetadata {
        TelemetryMetadata {
            channel_sequence: Some(7),
            system: System::Aarch64Darwin,
        }
    }

    #[test]
    fn telemetry_is_disabled_by_default_and_writes_nothing() {
        let path = std::env::temp_dir().join(format!("pkg-disabled-{}.json", std::process::id()));
        let mut recorder = TelemetryRecorder::new(TelemetryPolicy::default(), &path, metadata());
        recorder.add(Metric::CacheHitCount, 1);
        recorder.flush().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn enabled_snapshot_contains_only_fixed_aggregate_dimensions() {
        let root = std::env::temp_dir().join(format!(
            "pkg-telemetry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("telemetry.json");
        let mut recorder = TelemetryRecorder::new(TelemetryPolicy::Enabled, &path, metadata());
        recorder.add(Metric::CacheHitCount, 2);
        recorder.add(Metric::CacheHitCount, 3);
        recorder.flush().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["metrics"]["cache_hit_count"], 5);
        let text = value.to_string();
        assert!(!text.contains("package"));
        assert!(!text.contains("path"));
        assert!(!text.contains("args"));
        fs::remove_dir_all(root).unwrap();
    }
}
