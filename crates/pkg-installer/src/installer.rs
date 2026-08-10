//! Idempotent Linux installation orchestration over a closed privileged API.

use crate::assets::{LinuxAssetKind, LinuxInstallAsset, LinuxSystemdAssets, linux_install_assets};
use pkg_core::System;
use std::{error::Error, fmt};

/// Stable Linux installation failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallErrorCode {
    /// The requested platform is not Linux.
    UnsupportedPlatform,
    /// Unmanaged or ambiguous Nix evidence makes installation unsafe.
    UnmanagedNix,
    /// A closed backend operation failed.
    BackendFailure,
    /// Service activation or the final daemon check failed.
    ServiceUnhealthy,
    /// Receipt-last publication failed after artifact verification.
    ReceiptFailure,
    /// Rollback could not remove every artifact created by this attempt.
    RollbackIncomplete,
}

/// Redacted installer error carrying no host path or command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallError {
    code: InstallErrorCode,
}

impl InstallError {
    const fn new(code: InstallErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> InstallErrorCode {
        self.code
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux installation failed")
    }
}

impl Error for InstallError {}

/// Closed privileged operations used by the Linux installer.
///
/// Every artifact value originates from [`linux_install_assets`]; the trait
/// carries no arbitrary path, command, unit text, user, or group input.
pub trait LinuxInstallBackend {
    /// Verifies this process has the fixed privileged installer authority.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when root/polkit authority is absent.
    fn preflight_privilege(&mut self) -> Result<(), InstallError>;

    /// Scans the real production host as the privileged installer immediately
    /// before its first mutation. The backend accepts no caller-selected root.
    ///
    /// # Errors
    ///
    /// Returns `UnmanagedNix` for unmanaged, ambiguous, or unreadable evidence.
    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError>;

    /// Ensures one fixed artifact exists and returns whether this attempt created it.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the exact artifact cannot be verified or created.
    /// Before mutating, the backend must journal attempt ownership so
    /// [`Self::rollback_asset`] is safe after either `Ok` or `Err`.
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError>;

    /// Installs exact compiled-in unit bytes for one fixed unit artifact.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact unit installation fails.
    /// Partial attempt-owned writes must remain removable through
    /// [`Self::rollback_asset`].
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError>;

    /// Provisions the already authenticated managed-Nix runtime through PR-12.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when verified provisioning fails.
    /// Partial attempt-owned provisioning must remain removable through
    /// [`Self::rollback_managed_runtime`].
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError>;

    /// Reverts a managed runtime created by this exact attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback is incomplete.
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError>;

    /// Reloads systemd and enables/starts the fixed product units.
    ///
    /// Before each mutation the backend must journal enough prior state for
    /// [`Self::rollback_services`] to revert only this attempt. That rollback
    /// must remain safe and idempotent when this method returns an error before
    /// changing anything.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when fixed service activation fails.
    fn activate_services(&mut self) -> Result<bool, InstallError>;

    /// Reverts service enable/start changes journaled by this exact attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback is incomplete.
    fn rollback_services(&mut self) -> Result<(), InstallError>;

    /// Runs the fixed managed-daemon readiness check.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the daemon is not ready.
    fn check_managed_daemon(&mut self) -> Result<(), InstallError>;

    /// Publishes the authenticated ownership receipt after all verification.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when receipt-last publication fails.
    fn publish_ownership_receipt(&mut self) -> Result<(), InstallError>;

    /// Reverts one exact artifact created by this attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback fails.
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError>;
}

/// Sanitized result of one idempotent Linux installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxInstallReport {
    created_artifacts: usize,
    existing_artifacts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMutation {
    Asset(LinuxInstallAsset),
    ManagedRuntime,
    Services,
}

impl LinuxInstallReport {
    /// Returns how many allowlisted artifacts this attempt created.
    #[must_use]
    pub const fn created_artifacts(self) -> usize {
        self.created_artifacts
    }

    /// Returns how many allowlisted artifacts were already correct.
    #[must_use]
    pub const fn existing_artifacts(self) -> usize {
        self.existing_artifacts
    }
}

/// Executes the closed, receipt-last Linux installation sequence.
///
/// # Errors
///
/// Returns a stable install error for unsupported/non-clean hosts, backend or
/// service failure, receipt failure, or incomplete rollback.
pub fn install_linux(
    system: System,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
        return Err(InstallError::new(InstallErrorCode::UnsupportedPlatform));
    }
    backend.preflight_privilege()?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| InstallError::new(InstallErrorCode::UnmanagedNix))?;

    let mut mutations = Vec::new();
    let mut created_artifacts = 0_usize;
    let mut existing = 0_usize;
    let result = (|| {
        for asset in linux_install_assets()
            .iter()
            .filter(|asset| asset.kind() != LinuxAssetKind::File)
        {
            mutations.push(InstallMutation::Asset(*asset));
            let was_created = backend.ensure_asset(*asset)?;
            if was_created {
                created_artifacts = created_artifacts.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        mutations.push(InstallMutation::ManagedRuntime);
        if !backend.provision_managed_runtime()? {
            let _ = mutations.pop();
        }
        for asset in linux_install_assets()
            .iter()
            .filter(|asset| asset.kind() == LinuxAssetKind::File)
        {
            mutations.push(InstallMutation::Asset(*asset));
            let was_created = {
                if let Some(contents) = static_asset_contents(*asset) {
                    backend.install_systemd_unit(*asset, contents)?
                } else {
                    backend.ensure_asset(*asset)?
                }
            };
            if was_created {
                created_artifacts = created_artifacts.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        mutations.push(InstallMutation::Services);
        let services_changed = backend
            .activate_services()
            .map_err(|_| InstallError::new(InstallErrorCode::ServiceUnhealthy))?;
        if !services_changed {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| InstallError::new(InstallErrorCode::ServiceUnhealthy))?;
        backend
            .publish_ownership_receipt()
            .map_err(|_| InstallError::new(InstallErrorCode::ReceiptFailure))?;
        Ok(LinuxInstallReport {
            created_artifacts,
            existing_artifacts: existing,
        })
    })();

    if result.is_err() {
        let mut rollback_incomplete = false;
        for mutation in mutations.into_iter().rev() {
            let rollback = match mutation {
                InstallMutation::Asset(asset) => backend.rollback_asset(asset),
                InstallMutation::ManagedRuntime => backend.rollback_managed_runtime(),
                InstallMutation::Services => backend.rollback_services(),
            };
            if rollback.is_err() {
                rollback_incomplete = true;
            }
        }
        if rollback_incomplete {
            return Err(InstallError::new(InstallErrorCode::RollbackIncomplete));
        }
    }
    result
}

fn static_asset_contents(asset: LinuxInstallAsset) -> Option<&'static str> {
    match asset.id() {
        "daemon-socket-unit" => Some(LinuxSystemdAssets::DAEMON_SOCKET),
        "daemon-service-unit" => Some(LinuxSystemdAssets::DAEMON_SERVICE),
        "helper-socket-unit" => Some(LinuxSystemdAssets::HELPER_SOCKET),
        "helper-service-unit" => Some(LinuxSystemdAssets::HELPER_SERVICE),
        "broker-socket-unit" => Some(LinuxSystemdAssets::BROKER_SOCKET),
        "broker-service-unit" => Some(LinuxSystemdAssets::BROKER_SERVICE),
        "runtime-tmpfiles" => Some(LinuxSystemdAssets::TMPFILES),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, error::Error, io};

    struct FakeBackend {
        existing: BTreeSet<&'static str>,
        created: Vec<&'static str>,
        rolled_back: Vec<&'static str>,
        fail_after: Option<usize>,
        states: BTreeSet<&'static str>,
        rollback_events: Vec<&'static str>,
        rollback_failures: BTreeSet<&'static str>,
        fail_health_check: bool,
        fail_service_activation: bool,
    }

    impl FakeBackend {
        fn clean() -> Self {
            Self {
                existing: BTreeSet::new(),
                created: Vec::new(),
                rolled_back: Vec::new(),
                fail_after: None,
                states: BTreeSet::new(),
                rollback_events: Vec::new(),
                rollback_failures: BTreeSet::new(),
                fail_health_check: false,
                fail_service_activation: false,
            }
        }

        fn ensure(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            if self.existing.contains(asset.id()) {
                Ok(false)
            } else {
                let fail_after_mutation = self.fail_after == Some(self.created.len());
                self.existing.insert(asset.id());
                self.created.push(asset.id());
                if fail_after_mutation {
                    Err(InstallError::new(InstallErrorCode::BackendFailure))
                } else {
                    Ok(true)
                }
            }
        }
    }

    impl LinuxInstallBackend for FakeBackend {
        fn preflight_privilege(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
            if self.states.contains("unmanaged") {
                Err(InstallError::new(InstallErrorCode::UnmanagedNix))
            } else {
                Ok(())
            }
        }

        fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            self.ensure(asset)
        }

        fn install_systemd_unit(
            &mut self,
            asset: LinuxInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, InstallError> {
            self.ensure(asset)
        }

        fn activate_services(&mut self) -> Result<bool, InstallError> {
            let changed = self.states.insert("services");
            if self.fail_service_activation {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(changed)
            }
        }

        fn rollback_services(&mut self) -> Result<(), InstallError> {
            self.states.remove("services");
            self.rollback_events.push("services");
            if self.rollback_failures.contains("services") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }

        fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
            let changed = self.states.insert("runtime");
            if self.states.contains("fail-runtime") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(changed)
            }
        }

        fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
            self.states.remove("runtime");
            self.rollback_events.push("runtime");
            if self.rollback_failures.contains("runtime") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }

        fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
            if self.fail_health_check {
                Err(InstallError::new(InstallErrorCode::ServiceUnhealthy))
            } else {
                Ok(())
            }
        }

        fn publish_ownership_receipt(&mut self) -> Result<(), InstallError> {
            self.states.insert("receipt");
            Ok(())
        }

        fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
            self.existing.remove(asset.id());
            self.rolled_back.push(asset.id());
            self.rollback_events.push(asset.id());
            if self.rollback_failures.contains(asset.id()) {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn install_is_receipt_last_and_idempotent() -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();
        let report = install_linux(System::X8664Linux, &mut backend)?;
        assert_eq!(report.created_artifacts(), linux_install_assets().len());
        assert!(backend.states.contains("receipt"));
        assert!(backend.states.contains("runtime"));
        assert!(backend.rolled_back.is_empty());
        let second = install_linux(System::X8664Linux, &mut backend)?;
        assert_eq!(second.created_artifacts(), 0);
        assert_eq!(second.existing_artifacts(), linux_install_assets().len());
        Ok(())
    }

    #[test]
    fn failure_rolls_back_only_this_attempt_in_reverse_order() {
        let mut backend = FakeBackend::clean();
        backend.fail_after = Some(3);
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        let expected = backend.created.iter().rev().copied().collect::<Vec<_>>();
        assert_eq!(backend.rolled_back, expected);
        assert!(!backend.states.contains("runtime"));
        assert!(!backend.states.contains("services"));
        assert!(!backend.states.contains("receipt"));
    }

    #[test]
    fn post_activation_failure_reverts_services_files_runtime_then_directories()
    -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();
        backend.fail_health_check = true;
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::ServiceUnhealthy)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        let runtime = backend
            .rollback_events
            .iter()
            .position(|event| *event == "runtime")
            .ok_or_else(|| io::Error::other("runtime rollback missing"))?;
        let first_file = linux_install_assets()
            .iter()
            .position(|asset| asset.kind() == LinuxAssetKind::File)
            .ok_or_else(|| io::Error::other("file asset missing"))?;
        let file_count = linux_install_assets().len().saturating_sub(first_file);
        assert_eq!(runtime, file_count.saturating_add(1));
        assert!(!backend.states.contains("runtime"));
        assert!(!backend.states.contains("services"));
        assert!(backend.existing.is_empty());
        assert!(!backend.states.contains("receipt"));
        Ok(())
    }

    #[test]
    fn partial_service_activation_is_rolled_back_before_dependencies() {
        let mut backend = FakeBackend::clean();
        backend.fail_service_activation = true;
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::ServiceUnhealthy)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        assert!(!backend.states.contains("services"));
        assert!(!backend.states.contains("runtime"));
        assert!(backend.existing.is_empty());
    }

    #[test]
    fn privileged_host_scan_refuses_before_any_mutation() {
        let mut backend = FakeBackend::clean();
        backend.states.insert("unmanaged");
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::UnmanagedNix)
        );
        assert!(backend.created.is_empty());
        assert!(backend.rollback_events.is_empty());
    }

    #[test]
    fn partial_runtime_provisioning_is_rolled_back_before_directories() {
        let mut backend = FakeBackend::clean();
        backend.states.insert("fail-runtime");
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"runtime"));
        assert!(!backend.states.contains("runtime"));
        assert!(backend.existing.is_empty());
    }

    #[test]
    fn rollback_failure_does_not_skip_older_linux_mutations() {
        let mut backend = FakeBackend::clean();
        backend.fail_health_check = true;
        backend.rollback_failures.extend(["services", "runtime"]);
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::RollbackIncomplete)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        assert!(backend.rollback_events.contains(&"runtime"));
        assert!(backend.existing.is_empty());
    }
}
