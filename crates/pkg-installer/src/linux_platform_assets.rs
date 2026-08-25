//! Production composition of Linux account and filesystem installation assets.

use std::collections::BTreeMap;

use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};

use crate::{
    InstallError, LinuxAccountManager, LinuxFilesystemManager, LinuxInstallAsset,
    LinuxReleasePayloads, RecordedAsset, RecordedAssetState, UninstallManifest,
    assets::is_linux_product_asset, linux_install_assets,
};

/// Whether one fixed asset is exact-present or absent before a write-ahead intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxAssetPresence {
    /// The exact fixed asset exists and matches the closed contract.
    ExactPresent,
    /// The fixed asset is absent.
    Absent,
}

/// Routes the closed Linux asset set through the production account and
/// descriptor-relative filesystem implementations.
pub struct LinuxPlatformAssetManager {
    groups: ManagedGroupBindings,
    accounts: LinuxAccountManager,
    filesystem: Option<LinuxFilesystemManager>,
    payloads: Option<LinuxReleasePayloads>,
    config: Option<AuthenticatedManagedNixConfig>,
    receipt_binding: Option<(System, Digest)>,
    states: BTreeMap<&'static str, RecordedAssetState>,
}

impl std::fmt::Debug for LinuxPlatformAssetManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxPlatformAssetManager")
            .field("groups", &self.groups)
            .field("filesystem_ready", &self.filesystem.is_some())
            .field("config_bound", &self.config.is_some())
            .field("receipt_binding", &self.receipt_binding)
            .field("recorded_assets", &self.states.len())
            .finish_non_exhaustive()
    }
}

impl LinuxPlatformAssetManager {
    /// Creates the production asset router from authenticated host group ids.
    #[must_use]
    pub fn new(groups: ManagedGroupBindings) -> Self {
        Self {
            groups,
            accounts: LinuxAccountManager::new(groups),
            filesystem: None,
            payloads: None,
            config: None,
            receipt_binding: None,
            states: BTreeMap::new(),
        }
    }

    /// Binds product binaries that the authenticated bundle supplied.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a conflicting binding or a filesystem that is already active.
    pub fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), InstallError> {
        if self.filesystem.is_some() {
            return Err(InstallError::backend_failure());
        }
        let payloads = LinuxReleasePayloads::from_authenticated_bundle(payloads)
            .map_err(|_| InstallError::backend_failure())?;
        if self
            .payloads
            .as_ref()
            .is_some_and(|bound| bound != &payloads)
        {
            return Err(InstallError::backend_failure());
        }
        self.payloads = Some(payloads);
        Ok(())
    }

    /// Binds exact authenticated managed-Nix configuration bytes without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure for a conflicting binding.
    pub fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        if self.config.as_ref().is_some_and(|bound| bound != config) {
            return Err(InstallError::backend_failure());
        }
        if let Some(filesystem) = self.filesystem.as_mut() {
            filesystem
                .bind_authenticated_nix_config(config)
                .map_err(|_| InstallError::backend_failure())?;
        }
        self.config = Some(config.clone());
        Ok(())
    }

    /// Binds the authenticated descriptor identity without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure for a non-Linux or conflicting binding.
    pub fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError> {
        if !matches!(system, System::X8664Linux | System::Aarch64Linux)
            || self
                .receipt_binding
                .is_some_and(|binding| binding != (system, digest))
        {
            return Err(InstallError::backend_failure());
        }
        self.receipt_binding = Some((system, digest));
        Ok(())
    }

    pub(crate) fn authenticated_inputs_bound(&self, system: System) -> bool {
        self.payloads.is_some()
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.system() == system)
            && self
                .receipt_binding
                .is_some_and(|(bound_system, _)| bound_system == system)
    }

    /// Verifies or creates one closed account, directory, or release file.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when the production component refuses.
    pub fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let created = if LinuxAccountManager::handles(asset) {
            self.accounts
                .ensure_asset(asset)
                .map_err(|_| InstallError::backend_failure())?
        } else {
            self.ensure_filesystem()?
                .ensure_asset(asset)
                .map_err(|_| InstallError::backend_failure())?
        };
        self.record(asset, created);
        Ok(created)
    }

    /// Returns the revalidated non-root broker uid after account creation.
    ///
    /// # Errors
    ///
    /// Returns a redacted failure if the broker account cannot be revalidated.
    pub fn broker_uid(&mut self) -> Result<u32, InstallError> {
        self.accounts
            .broker_uid()
            .map_err(|_| InstallError::backend_failure())
    }

    /// Installs one exact compiled systemd or tmpfiles file.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when the filesystem component refuses.
    pub fn install_static_asset(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        let created = self
            .ensure_filesystem()?
            .install_static_asset(asset, contents)
            .map_err(|_| InstallError::backend_failure())?;
        self.record(asset, created);
        Ok(created)
    }

    /// Publishes or verifies the receipt-last uninstall manifest.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure for missing authenticated bindings,
    /// incomplete state, an unsafe existing receipt, or publication failure.
    pub fn publish_uninstall_manifest(&mut self) -> Result<bool, InstallError> {
        let (system, digest) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        if self
            .config
            .as_ref()
            .is_none_or(|config| config.system() != system)
        {
            return Err(InstallError::backend_failure());
        }

        if let Some(existing) = self
            .ensure_filesystem()?
            .existing_uninstall_manifest()
            .map_err(|_| InstallError::backend_failure())?
        {
            if existing.system() != system || existing.ownership_manifest_digest() != digest {
                return Err(InstallError::backend_failure());
            }
            let filesystem = self.ensure_filesystem()?;
            filesystem
                .bind_uninstall_manifest(&existing)
                .map_err(|_| InstallError::backend_failure())?;
            let asset = uninstall_manifest_asset()?;
            let created = filesystem
                .ensure_asset(asset)
                .map_err(|_| InstallError::backend_failure())?;
            if created {
                return Err(InstallError::backend_failure());
            }
            self.states
                .entry(asset.id())
                .or_insert(RecordedAssetState::Created);
            return Ok(false);
        }

        let records = linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .map(|asset| {
                let state = if asset.id() == "nix-root" {
                    RecordedAssetState::PreExisting
                } else if asset.id() == "uninstall-manifest" {
                    RecordedAssetState::Created
                } else {
                    *self
                        .states
                        .get(asset.id())
                        .ok_or_else(InstallError::backend_failure)?
                };
                RecordedAsset::new(asset.id(), state).map_err(|_| InstallError::backend_failure())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let manifest = UninstallManifest::new(system, digest, records)
            .map_err(|_| InstallError::backend_failure())?;
        let filesystem = self.ensure_filesystem()?;
        filesystem
            .bind_uninstall_manifest(&manifest)
            .map_err(|_| InstallError::backend_failure())?;
        let asset = uninstall_manifest_asset()?;
        if !filesystem
            .ensure_asset(asset)
            .map_err(|_| InstallError::backend_failure())?
        {
            return Err(InstallError::backend_failure());
        }
        self.states.insert(asset.id(), RecordedAssetState::Created);
        Ok(true)
    }

    /// Classifies the authenticated uninstall receipt without changing it.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the receipt binding is absent, unsafe, or changed.
    pub fn classify_uninstall_manifest(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        let (system, digest) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let Some(existing) = self
            .ensure_filesystem()?
            .existing_uninstall_manifest()
            .map_err(|_| InstallError::backend_failure())?
        else {
            return Ok(LinuxAssetPresence::Absent);
        };
        if existing.system() != system || existing.ownership_manifest_digest() != digest {
            return Err(InstallError::backend_failure());
        }
        let filesystem = self.ensure_filesystem()?;
        filesystem
            .bind_uninstall_manifest(&existing)
            .map_err(|_| InstallError::backend_failure())?;
        filesystem
            .verify_asset(uninstall_manifest_asset()?)
            .map_err(|_| InstallError::backend_failure())?;
        Ok(LinuxAssetPresence::ExactPresent)
    }

    /// Removes one exact attempt-owned account or filesystem artifact.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when identity-bound rollback is incomplete.
    pub fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if LinuxAccountManager::handles(asset) {
            self.accounts
                .rollback_asset(asset)
                .map_err(|_| InstallError::backend_failure())?;
        } else {
            self.filesystem
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?
                .rollback_asset(asset)
                .map_err(|_| InstallError::backend_failure())?;
        }
        self.states.remove(asset.id());
        Ok(())
    }

    /// Classifies one fixed asset as exact-present or absent without mutation.
    ///
    /// This runs before a write-ahead intent. A conflicting, unreadable, unsafe,
    /// or wrong asset is neither exact-present nor cleanly absent and returns the
    /// existing redacted `InstallError`.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when the production component refuses.
    pub fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError> {
        if LinuxAccountManager::handles(asset) {
            if self.accounts.verify_asset(asset).is_ok() {
                return Ok(LinuxAssetPresence::ExactPresent);
            }
            self.accounts
                .verify_asset_absent(asset)
                .map(|()| LinuxAssetPresence::Absent)
                .map_err(|_| InstallError::backend_failure())
        } else {
            let filesystem = self.ensure_filesystem()?;
            if filesystem.verify_asset(asset).is_ok() {
                return Ok(LinuxAssetPresence::ExactPresent);
            }
            filesystem
                .verify_asset_absent(asset)
                .map(|()| LinuxAssetPresence::Absent)
                .map_err(|_| InstallError::backend_failure())
        }
    }

    /// Reopens, verifies, and removes one exact fixed asset after a process restart.
    ///
    /// Absence is safe. This method does not depend on in-memory attempt state.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when the current object is unsafe,
    /// changed, or cannot be removed.
    pub fn remove_verified_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if LinuxAccountManager::handles(asset) {
            self.accounts
                .remove_verified_asset(asset)
                .map_err(|_| InstallError::backend_failure())
        } else {
            self.ensure_filesystem()?
                .remove_verified_asset(asset)
                .map_err(|_| InstallError::backend_failure())
        }
    }

    fn ensure_filesystem(&mut self) -> Result<&mut LinuxFilesystemManager, InstallError> {
        if self.filesystem.is_none() {
            let broker_uid = self
                .accounts
                .broker_uid()
                .map_err(|_| InstallError::backend_failure())?;
            let payloads = self
                .payloads
                .clone()
                .ok_or_else(InstallError::backend_failure)?;
            let mut filesystem = LinuxFilesystemManager::new(self.groups, broker_uid, payloads)
                .map_err(|_| InstallError::backend_failure())?;
            if let Some(config) = self.config.as_ref() {
                filesystem
                    .bind_authenticated_nix_config(config)
                    .map_err(|_| InstallError::backend_failure())?;
            }
            self.filesystem = Some(filesystem);
        }
        self.filesystem
            .as_mut()
            .ok_or_else(InstallError::backend_failure)
    }

    fn record(&mut self, asset: LinuxInstallAsset, created: bool) {
        self.states.entry(asset.id()).or_insert(if created {
            RecordedAssetState::Created
        } else {
            RecordedAssetState::PreExisting
        });
    }
}

fn uninstall_manifest_asset() -> Result<LinuxInstallAsset, InstallError> {
    linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == "uninstall-manifest")
        .ok_or_else(InstallError::backend_failure)
}
