//! Complete production binding for the closed Linux installer transaction.

use crate::{
    InstallError, LinuxInstallAsset, LinuxInstallBackend, LinuxPlatformAssetManager,
    LinuxReleasePayloads, LinuxSystemdManager,
};
use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedManagedNixConfig, DetectionDisposition, ManagedGroupBindings, RealNixAdapter,
    detect_unmanaged_nix,
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

    fn bind_authenticated_ownership_manifest(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError> {
        if system != self.system {
            return Err(InstallError::backend_failure());
        }
        self.assets
            .bind_authenticated_ownership_manifest(system, digest)
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
        if report.disposition() != DetectionDisposition::Clean {
            return Err(InstallError::backend_failure());
        }
        Ok(())
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
