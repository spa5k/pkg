//! Read-only health-report framework for `pkg doctor`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pkg_core::System;
use pkg_nix::BrokerOperationKind;
use serde::Serialize;

use crate::broker::BrokerLifecycleClient;
use crate::exit::ExitCode;
use crate::path::PathObservation;
use crate::ux::{PUBLIC_SCHEMA_VERSION, sanitize_public_text, write_json_line};

/// Status of one independently actionable doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Check passed.
    Pass,
    /// Check found a non-blocking condition worth showing.
    Warning,
    /// Check found a blocking configuration or integrity problem.
    Fail,
    /// Owning subsystem has not supplied the observation yet.
    Deferred,
}

/// Overall doctor result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorOverall {
    /// Every required check passed; warnings may remain.
    Healthy,
    /// A fixable configuration problem blocks a healthy result.
    NeedsAttention,
    /// An unmanaged Nix installation requires manual remediation.
    UnmanagedNix,
    /// Nix artifacts exist but a possible pkg ownership claim is unauthenticated.
    NixOwnershipUnknown,
}

/// One bounded, product-owned doctor result row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    id: &'static str,
    status: CheckStatus,
    detail: String,
    hint: Option<String>,
}

impl DoctorCheck {
    fn new(
        id: &'static str,
        status: CheckStatus,
        detail: impl Into<String>,
        hint: Option<impl Into<String>>,
    ) -> Self {
        Self {
            id,
            status,
            detail: sanitize_public_text(&detail.into()),
            hint: hint
                .map(Into::into)
                .map(|value| sanitize_public_text(&value)),
        }
    }

    /// Stable check identifier.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Check status.
    #[must_use]
    pub const fn status(&self) -> CheckStatus {
        self.status
    }
}

/// Observation supplied by another roadmap-owned subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsystemObservation {
    /// The subsystem check passed.
    Passed(String),
    /// A fixable configuration problem was found.
    Failed {
        /// Product-facing summary.
        detail: String,
        /// Product-facing remediation.
        hint: String,
    },
    /// The check is not wired yet and must not be represented as healthy.
    Deferred {
        /// Owning milestone or missing observation.
        detail: String,
        /// Safe next action.
        hint: String,
    },
}

/// Unmanaged-Nix scan result owned by PR-9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnmanagedNixObservation {
    /// PR-9 found no unmanaged installation signals.
    Clean,
    /// PR-9 refused because one or more bounded signals were found.
    Refused {
        /// Stable signal IDs; never raw paths or environment values.
        signals: Vec<String>,
        /// True when foreign artifacts or an unauthenticated marker were found;
        /// false when the scan found ambiguity only.
        definite: bool,
    },
    /// PR-9 has not supplied an observation; doctor must not claim health.
    Deferred,
}

/// Inputs consumed by the read-only report assembler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorInputs {
    /// Detected supported host system, or `None` for unsupported hosts.
    pub system: Option<System>,
    /// Invoking user's activation-bin PATH observation.
    pub path: PathObservation,
    /// Invoking user's state root.
    pub state_root: PathBuf,
    /// Expected state owner uid when the caller can establish it.
    pub expected_state_uid: Option<u32>,
    /// Unmanaged-Nix result from the PR-9 detector.
    pub unmanaged_nix: UnmanagedNixObservation,
    /// Managed runtime/daemon observation from provisioning/platform code.
    pub managed_runtime: SubsystemObservation,
    /// Signed channel observation from the channel client.
    pub channel: SubsystemObservation,
}

impl DoctorInputs {
    /// Assemble honest local inputs before gated subsystems have landed.
    #[must_use]
    pub fn local_development(state_root: PathBuf, path: PathObservation) -> Self {
        Self {
            system: detect_system(),
            path,
            state_root,
            expected_state_uid: None,
            unmanaged_nix: UnmanagedNixObservation::Deferred,
            managed_runtime: SubsystemObservation::Deferred {
                detail: "managed runtime and daemon check is not wired".into(),
                hint: "complete managed runtime provisioning before relying on doctor".into(),
            },
            channel: SubsystemObservation::Deferred {
                detail: "signed channel check is not wired".into(),
                hint: "complete the signed channel client before relying on doctor".into(),
            },
        }
    }
}

/// Checks the fixed production broker, managed runtime, and broker-owned
/// authenticated channel without exposing a raw Nix or trust-control surface.
///
/// The production broker authenticates its signed channel and native index
/// before it accepts client work. A complete version operation therefore
/// proves that the broker is reachable, its startup trust bootstrap succeeded,
/// and the pinned managed Nix adapter answered through the private boundary.
///
/// # Errors
///
/// Failures are returned as closed subsystem observations. Transport, frame,
/// adapter, and host details never cross into the public doctor report.
#[must_use]
pub fn observe_production_subsystems() -> (SubsystemObservation, SubsystemObservation, bool) {
    let health = probe_production_broker();
    let ownership = health.as_ref().is_some_and(|(_, ownership)| *ownership);
    let (runtime, channel) = production_subsystems_from_health(health.map(|(version, _)| version));
    (runtime, channel, ownership)
}

fn probe_production_broker() -> Option<(String, bool)> {
    let mut broker = BrokerLifecycleClient::connect_default().ok()?;
    let handle = broker.begin(BrokerOperationKind::Doctor).ok()?;
    let version = broker.version(handle.clone());
    match version {
        Ok(version) => match broker.verify_managed_ownership(handle.clone()) {
            Ok(ownership) => {
                if broker.complete(handle).is_err() {
                    return None;
                }
                Some((version.nix_version().as_str().to_owned(), ownership))
            }
            Err(_) => {
                let _ = broker.cancel(handle);
                None
            }
        },
        Err(_) => {
            let _ = broker.cancel(handle);
            None
        }
    }
}

fn production_subsystems_from_health(
    nix_version: Option<String>,
) -> (SubsystemObservation, SubsystemObservation) {
    match nix_version {
        Some(version) => (
            SubsystemObservation::Passed(format!(
                "managed Nix {version} answered through the private broker"
            )),
            SubsystemObservation::Passed(
                "the private broker authenticated its signed channel and native index at startup"
                    .into(),
            ),
        ),
        None => {
            let detail = "the private broker health check did not complete".to_owned();
            let hint =
                "run `pkg doctor` again; restart the managed runtime if the failure persists"
                    .to_owned();
            (
                SubsystemObservation::Failed {
                    detail: detail.clone(),
                    hint: hint.clone(),
                },
                SubsystemObservation::Failed { detail, hint },
            )
        }
    }
}

/// Stable `pkg doctor --json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    schema_version: u64,
    checks: Vec<DoctorCheck>,
    overall: DoctorOverall,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheckRecord<'a> {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    check: &'a DoctorCheck,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorResultRecord {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    ok: bool,
    command: &'static str,
    overall: DoctorOverall,
    code: u8,
}

impl DoctorReport {
    /// Evaluate all supplied observations without mutating the host.
    #[must_use]
    pub fn evaluate(inputs: &DoctorInputs) -> Self {
        let checks = vec![
            match inputs.system {
                Some(system) => DoctorCheck::new(
                    "host.system",
                    CheckStatus::Pass,
                    friendly_system(system),
                    None::<String>,
                ),
                None => DoctorCheck::new(
                    "host.system",
                    CheckStatus::Fail,
                    "this operating-system and architecture pair is unsupported",
                    Some("use Linux or macOS on x86-64 or arm64"),
                ),
            },
            path_check(&inputs.path),
            state_permissions_check(&inputs.state_root, inputs.expected_state_uid),
            unmanaged_check(&inputs.unmanaged_nix),
            subsystem_check("runtime.managed", &inputs.managed_runtime),
            subsystem_check("channel.signed", &inputs.channel),
        ];

        let overall = match &inputs.unmanaged_nix {
            UnmanagedNixObservation::Refused { definite: true, .. } => DoctorOverall::UnmanagedNix,
            UnmanagedNixObservation::Refused {
                definite: false, ..
            } => DoctorOverall::NixOwnershipUnknown,
            _ if checks
                .iter()
                .any(|check| matches!(check.status, CheckStatus::Fail | CheckStatus::Deferred)) =>
            {
                DoctorOverall::NeedsAttention
            }
            _ => DoctorOverall::Healthy,
        };
        Self {
            schema_version: PUBLIC_SCHEMA_VERSION,
            checks,
            overall,
        }
    }

    /// Ordered check rows.
    #[must_use]
    pub fn checks(&self) -> &[DoctorCheck] {
        &self.checks
    }

    /// Overall health state.
    #[must_use]
    pub const fn overall(&self) -> DoctorOverall {
        self.overall
    }

    /// Process status mandated by the CLI contract.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self.overall {
            DoctorOverall::Healthy => ExitCode::Ok,
            DoctorOverall::NeedsAttention => ExitCode::Config,
            DoctorOverall::UnmanagedNix | DoctorOverall::NixOwnershipUnknown => {
                ExitCode::UnmanagedNix
            }
        }
    }

    /// Write the stable single-document JSON report.
    pub fn write_json(&self, writer: impl Write) -> io::Result<()> {
        write_json_line(writer, self)
    }

    /// Write independently versioned check rows and one terminal result record.
    pub fn write_jsonl(&self, mut writer: impl Write) -> io::Result<()> {
        for check in &self.checks {
            write_json_line(
                &mut writer,
                &DoctorCheckRecord {
                    schema_version: PUBLIC_SCHEMA_VERSION,
                    kind: "check",
                    check,
                },
            )?;
        }
        write_json_line(
            writer,
            &DoctorResultRecord {
                schema_version: PUBLIC_SCHEMA_VERSION,
                kind: "result",
                ok: self.exit_code() == ExitCode::Ok,
                command: "doctor",
                overall: self.overall,
                code: self.exit_code().as_u8(),
            },
        )
    }

    /// Write the accessible human checklist without relying on color.
    pub fn write_human(&self, mut writer: impl Write) -> io::Result<()> {
        for check in &self.checks {
            let marker = match check.status {
                CheckStatus::Pass => "[ok]",
                CheckStatus::Warning => "[!]",
                CheckStatus::Fail => "[x]",
                CheckStatus::Deferred => "[-]",
            };
            writeln!(writer, "{marker} {}: {}", check.id, check.detail)?;
            if let Some(hint) = &check.hint {
                writeln!(writer, "    hint: {hint}")?;
            }
        }
        writeln!(
            writer,
            "overall: {}",
            match self.overall {
                DoctorOverall::Healthy => "healthy",
                DoctorOverall::NeedsAttention => "needs attention",
                DoctorOverall::UnmanagedNix => "unmanaged Nix detected",
                DoctorOverall::NixOwnershipUnknown => "Nix ownership unknown",
            }
        )
    }
}

fn detect_system() -> Option<System> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Some(System::X8664Linux),
        ("aarch64", "linux") => Some(System::Aarch64Linux),
        ("x86_64", "macos") => Some(System::X8664Darwin),
        ("aarch64", "macos") => Some(System::Aarch64Darwin),
        _ => None,
    }
}

const fn friendly_system(system: System) -> &'static str {
    match system {
        System::X8664Linux => "Linux x86-64",
        System::Aarch64Linux => "Linux arm64",
        System::X8664Darwin => "macOS x86-64",
        System::Aarch64Darwin => "macOS arm64",
    }
}

fn path_check(observation: &PathObservation) -> DoctorCheck {
    match (
        observation.first_index(),
        observation.duplicate_count(),
        observation.shadowed_count(),
        observation.shadow_scan_complete(),
    ) {
        (None, _, _, _) => DoctorCheck::new(
            "shell.path",
            CheckStatus::Fail,
            "the active generation bin directory is not on PATH",
            Some("source the installer-managed pkg shell snippet"),
        ),
        (Some(_), _, _, false) => DoctorCheck::new(
            "shell.path",
            CheckStatus::Warning,
            "command shadowing could not be checked completely",
            Some("check that the active generation and earlier PATH directories are readable"),
        ),
        (Some(_), _, count @ 1.., true) => DoctorCheck::new(
            "shell.path",
            CheckStatus::Warning,
            format!("{count} managed command(s) are shadowed by an earlier PATH entry"),
            Some("move the pkg shell snippet before conflicting PATH entries"),
        ),
        (Some(_), 1, 0, true) => DoctorCheck::new(
            "shell.path",
            CheckStatus::Pass,
            "the active generation bin directory is on PATH",
            None::<String>,
        ),
        (Some(_), _, 0, true) => DoctorCheck::new(
            "shell.path",
            CheckStatus::Warning,
            "the active generation bin directory appears more than once on PATH",
            Some("remove duplicate pkg shell snippets"),
        ),
    }
}

fn state_permissions_check(path: &Path, expected_uid: Option<u32>) -> DoctorCheck {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return DoctorCheck::new(
                "state.permissions",
                CheckStatus::Pass,
                "the per-user state root has not been created yet",
                None::<String>,
            );
        }
        Err(_) => {
            return DoctorCheck::new(
                "state.permissions",
                CheckStatus::Fail,
                "the per-user state root cannot be inspected",
                Some("check the state root and its parent permissions"),
            );
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return DoctorCheck::new(
            "state.permissions",
            CheckStatus::Fail,
            "the per-user state root is not a real directory",
            Some("replace it with a user-owned directory; symlinks are refused"),
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return DoctorCheck::new(
                "state.permissions",
                CheckStatus::Fail,
                "the per-user state root is accessible to group or other users",
                Some("set the state root mode to 0700"),
            );
        }
        if expected_uid.is_some_and(|uid| metadata.uid() != uid) {
            return DoctorCheck::new(
                "state.permissions",
                CheckStatus::Fail,
                "the per-user state root has the wrong owner",
                Some("restore ownership to the invoking user"),
            );
        }
    }

    DoctorCheck::new(
        "state.permissions",
        CheckStatus::Pass,
        "the per-user state root is a private directory",
        None::<String>,
    )
}

fn unmanaged_check(observation: &UnmanagedNixObservation) -> DoctorCheck {
    match observation {
        UnmanagedNixObservation::Clean => DoctorCheck::new(
            "nix.unmanaged",
            CheckStatus::Pass,
            "no unmanaged Nix signals were detected",
            None::<String>,
        ),
        UnmanagedNixObservation::Refused { signals, definite } => {
            if *definite {
                DoctorCheck::new(
                    "nix.unmanaged",
                    CheckStatus::Fail,
                    format!(
                        "an existing unmanaged Nix installation was detected ({} signals)",
                        signals.len()
                    ),
                    Some(
                        "remove it with its own uninstaller, then rerun pkg doctor; pkg never removes it",
                    ),
                )
            } else {
                DoctorCheck::new(
                    "nix.unmanaged",
                    CheckStatus::Fail,
                    format!(
                        "the host could not be proven free of unmanaged Nix ({} signals)",
                        signals.len()
                    ),
                    Some(
                        "do not remove anything; rerun the full read-only scan with the privileged installer/helper",
                    ),
                )
            }
        }
        UnmanagedNixObservation::Deferred => DoctorCheck::new(
            "nix.unmanaged",
            CheckStatus::Deferred,
            "the unmanaged-Nix detector has not supplied an observation",
            Some("do not run mutating commands until managed ownership is verified"),
        ),
    }
}

fn subsystem_check(id: &'static str, observation: &SubsystemObservation) -> DoctorCheck {
    match observation {
        SubsystemObservation::Passed(detail) => {
            DoctorCheck::new(id, CheckStatus::Pass, detail, None::<String>)
        }
        SubsystemObservation::Failed { detail, hint } => {
            DoctorCheck::new(id, CheckStatus::Fail, detail, Some(hint))
        }
        SubsystemObservation::Deferred { detail, hint } => {
            DoctorCheck::new(id, CheckStatus::Deferred, detail, Some(hint))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn healthy_inputs(state_root: PathBuf) -> DoctorInputs {
        DoctorInputs {
            system: Some(System::Aarch64Darwin),
            path: PathObservation::inspect(
                Path::new("/user/pkg/current/bin"),
                [Path::new("/user/pkg/current/bin")],
            ),
            state_root,
            expected_state_uid: None,
            unmanaged_nix: UnmanagedNixObservation::Clean,
            managed_runtime: SubsystemObservation::Passed("managed runtime is healthy".into()),
            channel: SubsystemObservation::Passed("signed channel is current".into()),
        }
    }

    #[test]
    fn healthy_report_is_versioned_and_exits_zero() {
        let root = std::env::temp_dir().join(format!("pkg-doctor-missing-{}", std::process::id()));
        let report = DoctorReport::evaluate(&healthy_inputs(root));
        assert_eq!(report.overall(), DoctorOverall::Healthy);
        assert_eq!(report.exit_code(), ExitCode::Ok);
        let mut json = Vec::new();
        report.write_json(&mut json).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["schemaVersion"], PUBLIC_SCHEMA_VERSION);
        assert!(value["checks"].is_array());
    }

    #[test]
    fn unmanaged_signal_has_priority_and_never_lists_raw_paths() {
        let mut inputs = healthy_inputs(PathBuf::from("/missing"));
        inputs.unmanaged_nix = UnmanagedNixObservation::Refused {
            signals: vec!["NIX_STORE_POPULATED".into(), "SYSTEMD_UNIT".into()],
            definite: true,
        };
        let report = DoctorReport::evaluate(&inputs);
        assert_eq!(report.exit_code(), ExitCode::UnmanagedNix);
        let mut json = Vec::new();
        report.write_json(&mut json).unwrap();
        let text = String::from_utf8(json).unwrap();
        assert!(!text.contains("/nix/store"));
        assert!(text.contains("2 signals"));
    }

    #[test]
    fn unauthenticated_ownership_claim_never_advises_removal() {
        let mut inputs = healthy_inputs(PathBuf::from("/missing"));
        inputs.unmanaged_nix = UnmanagedNixObservation::Refused {
            signals: vec!["NIX_ROOT".into(), "PKG_OWNERSHIP_MARKER".into()],
            definite: false,
        };
        let report = DoctorReport::evaluate(&inputs);
        assert_eq!(report.overall(), DoctorOverall::NixOwnershipUnknown);
        let mut human = Vec::new();
        report.write_human(&mut human).unwrap();
        let text = String::from_utf8(human).unwrap();
        assert!(text.contains("do not remove anything"));
        assert!(!text.contains("own uninstaller"));
    }

    #[test]
    fn subsystem_details_cross_the_same_public_redaction_boundary() {
        let mut inputs = healthy_inputs(PathBuf::from("/missing"));
        inputs.managed_runtime = SubsystemObservation::Failed {
            detail: "bad /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-secret".into(),
            hint: "inspect github:NixOS/nixpkgs/private".into(),
        };
        let report = DoctorReport::evaluate(&inputs);
        let mut json = Vec::new();
        report.write_json(&mut json).unwrap();
        let text = String::from_utf8(json).unwrap();
        assert!(!text.contains("/nix/store"));
        assert!(!text.contains("github:NixOS"));
        assert!(text.contains("private-detail-omitted"));
    }

    #[test]
    #[cfg(unix)]
    fn permissive_state_directory_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("pkg-doctor-perms-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let report = DoctorReport::evaluate(&healthy_inputs(root.clone()));
        assert_eq!(report.exit_code(), ExitCode::Config);
        fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn deferred_required_checks_cannot_claim_health() {
        let inputs = DoctorInputs::local_development(
            PathBuf::from("/missing"),
            PathObservation::inspect(Path::new("/expected"), [Path::new("/expected")]),
        );
        let report = DoctorReport::evaluate(&inputs);
        assert_eq!(report.overall(), DoctorOverall::NeedsAttention);
        assert_eq!(report.exit_code(), ExitCode::Config);
    }

    #[test]
    fn completed_broker_probe_proves_runtime_and_startup_trust() {
        let (runtime, channel) = production_subsystems_from_health(Some("2.34.8".to_owned()));
        assert_eq!(
            runtime,
            SubsystemObservation::Passed(
                "managed Nix 2.34.8 answered through the private broker".into()
            )
        );
        assert_eq!(
            channel,
            SubsystemObservation::Passed(
                "the private broker authenticated its signed channel and native index at startup"
                    .into()
            )
        );
    }

    #[test]
    fn incomplete_broker_probe_fails_both_dependent_checks_without_detail() {
        let (runtime, channel) = production_subsystems_from_health(None);
        for observation in [runtime, channel] {
            let SubsystemObservation::Failed { detail, hint } = observation else {
                panic!("an incomplete production probe must fail closed")
            };
            assert_eq!(detail, "the private broker health check did not complete");
            assert!(hint.contains("restart the managed runtime"));
            assert!(!detail.contains("socket"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn shadowed_commands_are_reported_as_a_bounded_warning() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "pkg-doctor-shadow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let earlier = root.join("earlier");
        let expected = root.join("current/bin");
        fs::create_dir_all(&earlier).unwrap();
        fs::create_dir_all(&expected).unwrap();
        for path in [earlier.join("rg"), expected.join("rg")] {
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut inputs = healthy_inputs(PathBuf::from("/missing"));
        inputs.path = PathObservation::inspect(&expected, [&earlier, &expected]);

        let report = DoctorReport::evaluate(&inputs);
        let path_check = report
            .checks()
            .iter()
            .find(|check| check.id() == "shell.path")
            .unwrap();
        assert_eq!(path_check.status(), CheckStatus::Warning);
        assert_eq!(report.exit_code(), ExitCode::Ok);
        let mut json = Vec::new();
        report.write_json(&mut json).unwrap();
        let text = String::from_utf8(json).unwrap();
        assert!(text.contains("1 managed command(s)"));
        assert!(!text.contains("rg"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn jsonl_has_versioned_rows_and_one_terminal_result() {
        let report = DoctorReport::evaluate(&healthy_inputs(PathBuf::from("/missing")));
        let mut output = Vec::new();
        report.write_jsonl(&mut output).unwrap();
        let values = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(values.iter().all(|value| value["schemaVersion"] == 1));
        assert_eq!(values.last().unwrap()["type"], "result");
        assert_eq!(
            values
                .iter()
                .filter(|value| value["type"] == "result")
                .count(),
            1
        );
    }
}
