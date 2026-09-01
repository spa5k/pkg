//! Privacy-minimized, preview-only support bundle for `pkg doctor --support`.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::commands::doctor::{CheckStatus, DoctorReport};
use crate::ux::PUBLIC_SCHEMA_VERSION;

const MAX_OPERATIONS: usize = 20;
const MAX_LOG_FILES: u8 = 10;
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const MAX_LOG_LINE_BYTES: usize = 16 * 1024;
const MAX_STATE_NODES: usize = 10_000;

/// Exact, stable preview emitted by `pkg doctor --support`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportBundle {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    cli_version: &'static str,
    platform: SupportPlatform,
    channel: SupportChannel,
    managed_runtime: SupportRuntime,
    index: SupportVerification,
    doctor: Vec<SupportCheck>,
    operations: Vec<SupportOperation>,
    state: StateHealth,
    privacy: PrivacyContract,
}

impl SupportBundle {
    /// Collect only allowlisted, typed local facts. Raw log fields are never retained.
    #[must_use]
    pub fn collect(report: &DoctorReport, state_root: &Path) -> Self {
        Self {
            schema_version: PUBLIC_SCHEMA_VERSION,
            kind: "support_bundle",
            cli_version: env!("CARGO_PKG_VERSION"),
            platform: SupportPlatform {
                os: friendly_os(),
                architecture: friendly_architecture(),
            },
            channel: SupportChannel {
                policy_version: None,
                sequence: None,
                expires_at: None,
                verification: status_for(report, "channel.signed"),
            },
            managed_runtime: SupportRuntime {
                version: None,
                verification: status_for(report, "runtime.managed"),
            },
            index: SupportVerification {
                verification: CheckStatus::Deferred,
            },
            doctor: report
                .checks()
                .iter()
                .map(|check| SupportCheck {
                    id: check.id(),
                    status: check.status(),
                })
                .collect(),
            operations: collect_operations(&state_root.join("logs")),
            state: inspect_state(state_root),
            privacy: PrivacyContract {
                preview_only: true,
                uploaded: false,
                package_names_included: false,
            },
        }
    }

    /// Serialize the exact bytes shown to the user. A future sender must reuse these bytes.
    pub fn preview_bytes(&self) -> io::Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Write the complete bundle preview. This function never uploads data.
    pub fn write_preview(&self, mut writer: impl Write) -> io::Result<()> {
        writer.write_all(&self.preview_bytes()?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportPlatform {
    os: &'static str,
    architecture: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportChannel {
    policy_version: Option<u64>,
    sequence: Option<u64>,
    expires_at: Option<&'static str>,
    verification: CheckStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportRuntime {
    version: Option<&'static str>,
    verification: CheckStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportVerification {
    verification: CheckStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportCheck {
    id: &'static str,
    status: CheckStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationOutcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportOperation {
    phase: &'static str,
    outcome: OperationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateHealth {
    exists: bool,
    private: bool,
    mode: Option<u32>,
    bytes: u64,
    scan_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyContract {
    preview_only: bool,
    uploaded: bool,
    package_names_included: bool,
}

#[derive(Debug, Deserialize)]
struct PrivateLogProjection {
    event: String,
    code: Option<u8>,
}

fn collect_operations(log_directory: &Path) -> Vec<SupportOperation> {
    let mut operations = Vec::new();
    let Ok(metadata) = fs::symlink_metadata(log_directory) else {
        return operations;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !is_private(&metadata) {
        return operations;
    }
    for index in 0..MAX_LOG_FILES {
        let path = if index == 0 {
            log_directory.join("pkg.log")
        } else {
            log_directory.join(format!("pkg.log.{index}"))
        };
        collect_log_file(&path, &mut operations);
        if operations.len() == MAX_OPERATIONS {
            break;
        }
    }
    operations
}

fn status_for(report: &DoctorReport, id: &str) -> CheckStatus {
    report
        .checks()
        .iter()
        .find(|check| check.id() == id)
        .map_or(
            CheckStatus::Deferred,
            super::commands::doctor::DoctorCheck::status,
        )
}

fn collect_log_file(path: &Path, operations: &mut Vec<SupportOperation>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_LOG_BYTES
        || !is_private(&metadata)
    {
        return;
    }
    let Ok(file) = File::open(path) else {
        return;
    };
    let mut lines = BufReader::new(file.take(MAX_LOG_BYTES))
        .lines()
        .map_while(Result::ok)
        .collect::<Vec<_>>();
    lines.reverse();
    for line in lines {
        if line.len() > MAX_LOG_LINE_BYTES {
            continue;
        }
        let Ok(record) = serde_json::from_str::<PrivateLogProjection>(&line) else {
            continue;
        };
        if record.event != "command_finished" {
            continue;
        }
        operations.push(SupportOperation {
            phase: "command",
            outcome: if record.code == Some(0) {
                OperationOutcome::Success
            } else {
                OperationOutcome::Failure
            },
        });
        if operations.len() == MAX_OPERATIONS {
            return;
        }
    }
}

fn inspect_state(root: &Path) -> StateHealth {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return StateHealth {
            exists: false,
            private: false,
            mode: None,
            bytes: 0,
            scan_complete: true,
        };
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return StateHealth {
            exists: true,
            private: false,
            mode: mode(&metadata),
            bytes: 0,
            scan_complete: false,
        };
    }

    let mut pending = vec![root.to_path_buf()];
    let mut bytes = 0_u64;
    let mut visited = 0_usize;
    let mut complete = true;
    while let Some(path) = pending.pop() {
        if visited == MAX_STATE_NODES {
            complete = false;
            break;
        }
        visited += 1;
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            complete = false;
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
            continue;
        }
        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                complete = false;
                continue;
            };
            for entry in entries {
                match entry {
                    Ok(entry) => pending.push(entry.path()),
                    Err(_) => complete = false,
                }
            }
        }
    }

    StateHealth {
        exists: true,
        private: is_private(&metadata),
        mode: mode(&metadata),
        bytes,
        scan_complete: complete,
    }
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
const fn mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn is_private(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o077 == 0
}

#[cfg(not(unix))]
const fn is_private(_metadata: &fs::Metadata) -> bool {
    true
}

fn friendly_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        _ => "unsupported",
    }
}

fn friendly_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86-64",
        "aarch64" => "arm64",
        _ => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::path::PathBuf;

    use crate::commands::doctor::{DoctorInputs, SubsystemObservation, UnmanagedNixObservation};
    use crate::path::{PathObservation, RawNixVisibility};
    use pkg_core::System;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pkg-support-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn report(root: &Path) -> DoctorReport {
        let mut inputs = DoctorInputs::local_development(
            root.to_path_buf(),
            PathObservation::inspect(
                root.join("current/bin").as_path(),
                [root.join("current/bin")],
            ),
        );
        inputs.system = Some(System::Aarch64Darwin);
        inputs.raw_nix_visibility = RawNixVisibility::Hidden;
        inputs.installed_nix = Some(crate::commands::doctor::InstalledNixState::Accepted);
        inputs.unmanaged_nix = UnmanagedNixObservation::Clean;
        inputs.managed_runtime = SubsystemObservation::Passed("managed runtime is healthy".into());
        inputs.channel = SubsystemObservation::Passed("signed channel is current".into());
        DoctorReport::evaluate(&inputs)
    }

    #[test]
    #[cfg(unix)]
    fn preview_is_stable_and_projects_away_private_log_fields() {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

        let root = temp("redaction");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root.join("logs"))
            .unwrap();
        let mut log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("logs/pkg.log"))
            .unwrap();
        writeln!(
            log,
            "{}",
            serde_json::json!({
                "event": "command_finished",
                "command": "install",
                "code": 69,
                "detail": "secret-package /nix/store/private token=hunter2",
                "argv": ["secret-package"],
                "environment": {"TOKEN": "hunter2"}
            })
        )
        .unwrap();
        let bundle = SupportBundle::collect(&report(&root), &root);
        let first = bundle.preview_bytes().unwrap();
        let second = bundle.preview_bytes().unwrap();
        assert_eq!(first, second);
        let text = String::from_utf8(first).unwrap();
        assert!(text.contains("\"phase\": \"command\""));
        assert!(text.contains("\"outcome\": \"failure\""));
        for forbidden in [
            "secret-package",
            "/nix/store",
            "hunter2",
            "argv",
            "environment",
            root.to_str().unwrap(),
        ] {
            assert!(!text.contains(forbidden), "leaked {forbidden}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_state_is_reported_without_exposing_its_path() {
        let root = temp("missing");
        let text = String::from_utf8(
            SupportBundle::collect(&report(&root), &root)
                .preview_bytes()
                .unwrap(),
        )
        .unwrap();
        assert!(text.contains("\"exists\": false"));
        assert!(!text.contains(root.to_str().unwrap()));
    }
}
