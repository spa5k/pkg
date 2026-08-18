//! Production macOS installer backend.

use std::{env, path::Path};

use nix::unistd::{Gid, Uid};
use pkg_core::System;
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, DetectionDisposition,
    ManagedGroupBindings, OwnershipExpectation, RealNixAdapter, detect_unmanaged_nix,
    observe_build_accounts, verify_authenticated_managed_install,
};

#[cfg(target_os = "macos")]
use crate::MacOsStoreProvisionOutcome;
#[cfg(any(target_os = "macos", test))]
use pkg_nix::{DetectionReport, FindingKind};

use crate::{
    MacOsAssetPresence, MacOsBuildReadiness, MacOsBuildUsersReadiness, MacOsError,
    MacOsInstallAsset, MacOsInstallBackend, MacOsSandboxReadiness, MacOsToolchainReadiness,
    macos_install_assets, macos_launchd::MacOsLaunchdManager,
    macos_platform_assets::MacOsPlatformAssetManager,
};

const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
const BROKER_HOME: &str = "/Library/Application Support/pkg/broker-home";
const CODESIGN: &str = "/usr/bin/codesign";
const XCRUN: &str = "/usr/bin/xcrun";

pub struct ProductionMacOsInstallBackend {
    system: System,
    assets: MacOsPlatformAssetManager,
    services: MacOsLaunchdManager,
    ownership_expectation: Option<OwnershipExpectation>,
    config: Option<AuthenticatedManagedNixConfig>,
    existing_managed_install: bool,
    authenticated_recovery: bool,
    store_created: bool,
}

impl ProductionMacOsInstallBackend {
    /// Creates the fixed preview backend for one native Darwin system.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Darwin system or invalid fixed group bindings.
    pub fn new(system: System, groups: ManagedGroupBindings) -> Result<Self, MacOsError> {
        if !matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
            return Err(MacOsError::backend_failure());
        }
        Ok(Self {
            system,
            assets: MacOsPlatformAssetManager::new(groups)?,
            services: MacOsLaunchdManager::new(),
            ownership_expectation: None,
            config: None,
            existing_managed_install: false,
            authenticated_recovery: false,
            store_created: false,
        })
    }

    fn verify_service_assets(&mut self) -> Result<(), MacOsError> {
        for asset in macos_install_assets().iter().copied().filter(|asset| {
            matches!(
                asset.id(),
                "store-volume-plist" | "daemon-plist" | "helper-plist" | "broker-plist"
            )
        }) {
            if self.assets.classify_asset(asset)? != MacOsAssetPresence::ExactPresent {
                return Err(MacOsError::backend_failure());
            }
        }
        Ok(())
    }

    fn classify_preview_presence(
        &self,
        presence: MacOsAssetPresence,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        if !self.existing_managed_install && presence == MacOsAssetPresence::ExactPresent {
            Err(MacOsError::backend_failure())
        } else {
            Ok(presence)
        }
    }

    fn classify_asset_presence(
        &self,
        asset: MacOsInstallAsset,
        presence: MacOsAssetPresence,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        if self.store_created && asset.id() == "nix-root" {
            return (presence == MacOsAssetPresence::ExactPresent)
                .then_some(presence)
                .ok_or_else(MacOsError::backend_failure);
        }
        self.classify_preview_presence(presence)
    }
}

impl MacOsInstallBackend for ProductionMacOsInstallBackend {
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError> {
        if payloads.system() != self.system {
            return Err(MacOsError::backend_failure());
        }
        self.assets.bind_authenticated_installer_payloads(payloads)
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        if config.system() != self.system
            || self.config.as_ref().is_some_and(|bound| bound != config)
        {
            return Err(MacOsError::backend_failure());
        }
        self.assets.bind_authenticated_nix_config(config)?;
        self.config = Some(config.clone());
        Ok(())
    }

    fn bind_authenticated_ownership_expectation(
        &mut self,
        expectation: &OwnershipExpectation,
    ) -> Result<(), MacOsError> {
        if expectation.system() != self.system
            || self
                .ownership_expectation
                .as_ref()
                .is_some_and(|bound| bound != expectation)
        {
            return Err(MacOsError::backend_failure());
        }
        self.assets.bind_authenticated_ownership(
            expectation.system(),
            expectation.asset_manifest_digest(),
        )?;
        self.ownership_expectation = Some(expectation.clone());
        Ok(())
    }

    fn begin_authenticated_recovery(&mut self) -> Result<(), MacOsError> {
        if self.ownership_expectation.is_none()
            || !self.assets.authenticated_inputs_bound(self.system)
        {
            return Err(MacOsError::backend_failure());
        }
        self.authenticated_recovery = true;
        Ok(())
    }

    fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
        if Uid::effective().is_root() && Gid::effective().as_raw() == 0 {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError> {
        if system != self.system
            || !self.assets.authenticated_inputs_bound(system)
            || !Uid::effective().is_root()
            || Gid::effective().as_raw() != 0
        {
            return Err(MacOsError::backend_failure());
        }
        let path_entries = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        let environment_keys = env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
        let report = detect_unmanaged_nix(Path::new("/"), system, &path_entries, &environment_keys);
        if report.disposition() == DetectionDisposition::Clean {
            self.existing_managed_install = false;
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if self.authenticated_recovery && report_is_recovered_mountpoint_only(&report) {
            let nix_root = macos_install_assets()
                .iter()
                .copied()
                .find(|asset| store_volume_owns_rollback(*asset))
                .ok_or_else(MacOsError::backend_failure)?;
            if self.assets.classify_store_mountpoint(nix_root)? == MacOsAssetPresence::ExactPresent
                && !crate::classify_macos_store_volume_production()
                    .map_err(|_| MacOsError::backend_failure())?
            {
                self.existing_managed_install = false;
                return Ok(());
            }
        }
        verify_authenticated_managed_install(
            Path::new("/"),
            self.ownership_expectation
                .as_ref()
                .ok_or_else(MacOsError::backend_failure)?,
            &path_entries,
            &environment_keys,
        )
        .map_err(|_| MacOsError::backend_failure())?;
        self.existing_managed_install = true;
        Ok(())
    }

    fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.assets.broker_uid()
    }

    fn classify_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        let presence = self.assets.classify_asset(asset)?;
        self.classify_asset_presence(asset, presence)
    }

    fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        #[cfg(target_os = "macos")]
        {
            crate::classify_macos_store_volume_production()
                .map(|present| {
                    if present {
                        MacOsAssetPresence::ExactPresent
                    } else {
                        MacOsAssetPresence::Absent
                    }
                })
                .map_err(|_| MacOsError::backend_failure())
        }
        #[cfg(not(target_os = "macos"))]
        Err(MacOsError::backend_failure())
    }

    fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        Ok(if self.existing_managed_install {
            MacOsAssetPresence::ExactPresent
        } else {
            MacOsAssetPresence::Absent
        })
    }

    fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        let presence = MacOsLaunchdManager::classify_activation().map(|active| {
            if active {
                MacOsAssetPresence::ExactPresent
            } else {
                MacOsAssetPresence::Absent
            }
        })?;
        self.classify_preview_presence(presence)
    }

    fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        let presence = self.assets.classify_uninstall_manifest()?;
        self.classify_preview_presence(presence)
    }

    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if store_volume_owns_rollback(asset) {
            return (self.assets.classify_asset(asset)? == MacOsAssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if asset.id() == "broker-channel-state" {
            return self.assets.remove_uninstall_asset(asset);
        }
        self.assets.remove_verified_asset(asset)
    }

    fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
        #[cfg(target_os = "macos")]
        {
            crate::remove_macos_store_volume_production().map_err(|_| MacOsError::backend_failure())
        }
        #[cfg(not(target_os = "macos"))]
        Err(MacOsError::backend_failure())
    }

    fn recover_services(&mut self) -> Result<(), MacOsError> {
        self.verify_service_assets()?;
        MacOsLaunchdManager::deactivate_verified()
    }

    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        self.assets.recover_uninstall_manifest()
    }

    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        if self.assets.authenticated_inputs_bound(self.system) {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        #[cfg(target_os = "macos")]
        {
            let created = crate::provision_macos_store_volume_production()
                .map(|outcome| outcome == MacOsStoreProvisionOutcome::Provisioned)
                .map_err(|_| MacOsError::backend_failure())?;
            self.store_created = created;
            Ok(created)
        }
        #[cfg(not(target_os = "macos"))]
        Err(MacOsError::backend_failure())
    }

    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        if !self.store_created {
            return Ok(());
        }
        self.recover_store_volume()?;
        self.store_created = false;
        Ok(())
    }

    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let created = self.assets.ensure_asset(asset)?;
        if self.store_created && asset.id() == "nix-root" {
            self.assets.record_created(asset);
            Ok(true)
        } else {
            Ok(created)
        }
    }

    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        self.assets.install_static_asset(asset, contents)
    }

    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        self.assets.ensure_asset(asset)
    }

    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
        Err(MacOsError::backend_failure())
    }

    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }

    fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
        for path in [
            "/opt/pkg/bin/pkg-nix-broker",
            "/opt/pkg/bin/pkg-root-helper",
            "/usr/local/bin/pkg",
        ] {
            crate::linux_accounts::run_status(CODESIGN, &["--verify", "--strict", path])
                .map_err(|_| MacOsError::backend_failure())?;
        }
        Ok(())
    }

    fn activate_services(&mut self) -> Result<bool, MacOsError> {
        self.services.activate()
    }

    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        self.services.rollback()
    }

    fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
        MacOsLaunchdManager::verify_active()?;
        let adapter = RealNixAdapter::new(Path::new(MANAGED_NIX_BINARY), Path::new(BROKER_HOME))
            .map_err(|_| MacOsError::backend_failure())?;
        for attempt in 0..20 {
            if adapter.ping_managed_store().is_ok() {
                return Ok(());
            }
            if attempt < 19 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        Err(MacOsError::backend_failure())
    }

    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(MacOsError::backend_failure)?;
        let text =
            std::str::from_utf8(config.as_bytes()).map_err(|_| MacOsError::backend_failure())?;
        let sandbox = if text.lines().any(|line| line.trim() == "sandbox = true")
            && text
                .lines()
                .any(|line| line.trim() == "sandbox-fallback = false")
        {
            MacOsSandboxReadiness::Enforced
        } else {
            MacOsSandboxReadiness::Disabled
        };
        let directory =
            observe_build_accounts(system).map_err(|_| MacOsError::backend_failure())?;
        let expected = (1..=32)
            .map(|number| format!("_nixbld{number}"))
            .collect::<std::collections::BTreeSet<_>>();
        let accounts = directory.accounts();
        let accounts_exact = directory.group_gid() == crate::macos_accounts::BUILD_GID
            && directory.explicit_members() == &expected
            && accounts
                .iter()
                .filter(|account| account.primary_gid() == crate::macos_accounts::BUILD_GID)
                .count()
                == 32
            && (1..=32).all(|number| {
                let name = format!("_nixbld{number}");
                accounts
                    .iter()
                    .find(|account| account.name() == name)
                    .is_some_and(|account| {
                        account.uid() == crate::macos_accounts::BUILD_GID.saturating_add(number)
                            && account.primary_gid() == crate::macos_accounts::BUILD_GID
                            && account.home() == "/var/empty"
                            && account.shell() == "/usr/bin/false"
                            && accounts
                                .iter()
                                .filter(|candidate| candidate.uid() == account.uid())
                                .count()
                                == 1
                    })
            });
        let build_users = if accounts_exact {
            MacOsBuildUsersReadiness::Ready
        } else {
            MacOsBuildUsersReadiness::UserSetMismatch
        };
        let toolchain = if crate::linux_accounts::run_capture(XCRUN, &["--find", "clang"])
            .is_ok_and(|bytes| valid_tool_path(&bytes))
        {
            MacOsToolchainReadiness::Ready
        } else {
            MacOsToolchainReadiness::Missing
        };
        Ok(MacOsBuildReadiness::observed(
            system,
            sandbox,
            build_users,
            toolchain,
        ))
    }

    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
        self.assets.publish_uninstall_manifest()
    }

    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if store_volume_owns_rollback(asset) {
            self.recover_asset(asset)
        } else {
            self.assets.rollback_asset(asset)
        }
    }
}

fn store_volume_owns_rollback(asset: MacOsInstallAsset) -> bool {
    asset.id() == "nix-root"
}

#[cfg(any(target_os = "macos", test))]
fn report_is_recovered_mountpoint_only(report: &DetectionReport) -> bool {
    matches!(report.findings(), [finding] if finding.id() == "NIX_ROOT" && finding.kind() == FindingKind::Unmanaged)
}

fn valid_tool_path(bytes: &[u8]) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let path = Path::new(text.trim());
    let Ok(metadata) = path.symlink_metadata() else {
        return false;
    };
    path.is_absolute()
        && metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == 0
        && metadata.mode() & 0o022 == 0
        && metadata.mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_backend_refuses_non_darwin_systems() -> Result<(), Box<dyn std::error::Error>> {
        let groups = ManagedGroupBindings::new(333, 350)?;
        assert!(ProductionMacOsInstallBackend::new(System::Aarch64Linux, groups).is_err());
        Ok(())
    }

    #[test]
    fn clean_preview_refuses_preexisting_assets() -> Result<(), Box<dyn std::error::Error>> {
        let groups = ManagedGroupBindings::new(333, 350)?;
        let mut backend = ProductionMacOsInstallBackend::new(System::Aarch64Darwin, groups)?;
        assert!(
            backend
                .classify_preview_presence(MacOsAssetPresence::ExactPresent)
                .is_err()
        );
        let nix_root = macos_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "nix-root")
            .ok_or("missing nix-root asset")?;
        assert!(store_volume_owns_rollback(nix_root));
        backend.store_created = true;
        assert_eq!(
            backend.classify_asset_presence(nix_root, MacOsAssetPresence::ExactPresent),
            Ok(MacOsAssetPresence::ExactPresent)
        );
        assert!(
            backend
                .classify_asset_presence(nix_root, MacOsAssetPresence::Absent)
                .is_err()
        );
        backend.store_created = false;
        backend.existing_managed_install = true;
        assert_eq!(
            backend.classify_preview_presence(MacOsAssetPresence::ExactPresent),
            Ok(MacOsAssetPresence::ExactPresent)
        );
        Ok(())
    }

    #[test]
    fn authenticated_recovery_allows_only_an_empty_nix_root_finding()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("nix"))?;
        let report = detect_unmanaged_nix(root.path(), System::Aarch64Darwin, &[], &[]);
        assert!(report_is_recovered_mountpoint_only(&report));

        std::fs::create_dir(root.path().join("nix/store"))?;
        let report = detect_unmanaged_nix(root.path(), System::Aarch64Darwin, &[], &[]);
        assert!(!report_is_recovered_mountpoint_only(&report));
        Ok(())
    }
}
