//! Complete production binding for the closed Linux installer transaction.

use crate::{
    InstallError, LinuxAssetPresence, LinuxInstallAsset, LinuxInstallBackend,
    LinuxPlatformAssetManager, LinuxReleasePayloads, LinuxSystemdManager,
};
use nix::unistd::{Gid, Uid};
use pkg_core::System;
use pkg_nix::{
    AuthenticatedManagedNixConfig, DetectionDisposition, ManagedGroupBindings,
    OwnershipExpectation, RealNixAdapter, detect_unmanaged_nix,
    verify_authenticated_managed_install,
};
use std::{env, path::Path};

const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
const BROKER_HOME: &str = "/var/lib/pkg/broker-home";

/// Production implementation of the closed Linux installer backend.
#[derive(Debug)]
pub struct ProductionLinuxInstallBackend {
    system: System,
    assets: LinuxPlatformAssetManager,
    services: LinuxSystemdManager,
    ownership_expectation: Option<OwnershipExpectation>,
    existing_managed_install: bool,
}

impl ProductionLinuxInstallBackend {
    /// Creates a backend for one authenticated native Linux installation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Linux system or unavailable systemd tools.
    pub fn new(
        system: System,
        groups: ManagedGroupBindings,
        payloads: LinuxReleasePayloads,
    ) -> Result<Self, InstallError> {
        if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
            return Err(InstallError::backend_failure());
        }
        Ok(Self {
            system,
            assets: LinuxPlatformAssetManager::new(groups, payloads),
            services: LinuxSystemdManager::production()
                .map_err(|_| InstallError::backend_failure())?,
            ownership_expectation: None,
            existing_managed_install: false,
        })
    }

    fn is_systemd_unit(asset: LinuxInstallAsset) -> bool {
        matches!(
            asset.id(),
            "daemon-socket-unit"
                | "daemon-service-unit"
                | "helper-socket-unit"
                | "helper-service-unit"
                | "broker-socket-unit"
                | "broker-service-unit"
        )
    }
}

impl LinuxInstallBackend for ProductionLinuxInstallBackend {
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        if config.system() != self.system {
            return Err(InstallError::backend_failure());
        }
        self.assets.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_ownership_expectation(
        &mut self,
        expectation: &OwnershipExpectation,
    ) -> Result<(), InstallError> {
        if expectation.system() != self.system
            || self
                .ownership_expectation
                .as_ref()
                .is_some_and(|bound| bound != expectation)
        {
            return Err(InstallError::backend_failure());
        }
        self.assets.bind_authenticated_ownership_manifest(
            expectation.system(),
            expectation.asset_manifest_digest(),
        )?;
        self.ownership_expectation = Some(expectation.clone());
        Ok(())
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        if !Uid::effective().is_root() || Gid::effective().as_raw() != 0 {
            return Err(InstallError::backend_failure());
        }
        Ok(())
    }

    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError> {
        if system != self.system || !Uid::effective().is_root() || Gid::effective().as_raw() != 0 {
            return Err(InstallError::backend_failure());
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
        verify_authenticated_managed_install(
            Path::new("/"),
            self.ownership_expectation
                .as_ref()
                .ok_or_else(InstallError::backend_failure)?,
            &path_entries,
            &environment_keys,
        )
        .map_err(|_| InstallError::backend_failure())?;
        self.existing_managed_install = true;
        Ok(())
    }

    fn broker_uid(&mut self) -> Result<u32, InstallError> {
        self.assets.broker_uid()
    }

    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError> {
        self.assets.classify_asset(asset)
    }

    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.assets.remove_verified_asset(asset)?;
        if Self::is_systemd_unit(asset) {
            self.services
                .reload_units()
                .map_err(|_| InstallError::backend_failure())?;
        }
        Ok(())
    }

    fn recover_services(&mut self) -> Result<(), InstallError> {
        self.services
            .deactivate_for_uninstall()
            .map_err(|_| InstallError::backend_failure())
    }

    fn classify_managed_runtime(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        Ok(if self.existing_managed_install {
            LinuxAssetPresence::ExactPresent
        } else {
            LinuxAssetPresence::Absent
        })
    }

    fn classify_services(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.services
            .classify_activation()
            .map(|active| {
                if active {
                    LinuxAssetPresence::ExactPresent
                } else {
                    LinuxAssetPresence::Absent
                }
            })
            .map_err(|_| InstallError::backend_failure())
    }

    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        self.assets.ensure_asset(asset)
    }

    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        self.assets.install_static_asset(asset, contents)
    }

    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        // The authenticated bundle adapter owns this operation. A direct call
        // cannot supply authenticated runtime bytes and therefore fails closed.
        Err(InstallError::backend_failure())
    }

    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        // The authenticated bundle transaction owns runtime rollback.
        Ok(())
    }

    fn activate_services(&mut self) -> Result<bool, InstallError> {
        self.services
            .activate()
            .map_err(|_| InstallError::backend_failure())
    }

    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.services
            .rollback()
            .map_err(|_| InstallError::backend_failure())
    }

    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        self.services
            .verify_active()
            .map_err(|_| InstallError::backend_failure())?;
        RealNixAdapter::new(Path::new(MANAGED_NIX_BINARY), Path::new(BROKER_HOME))
            .and_then(|adapter| adapter.ping_managed_store())
            .map_err(|_| InstallError::backend_failure())
    }

    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        self.assets.publish_uninstall_manifest()
    }

    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.assets.rollback_asset(asset)?;
        if Self::is_systemd_unit(asset) {
            self.services
                .reload_units()
                .map_err(|_| InstallError::backend_failure())?;
        }
        Ok(())
    }
}
