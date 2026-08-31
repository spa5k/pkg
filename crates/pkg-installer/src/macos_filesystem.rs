//! macOS binding for the descriptor-relative production filesystem writer.

use pkg_core::state::Digest;
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};
use rustix::fs::{Dir, FileType, Mode, OFlags, fstat, open, openat};

use crate::{
    LinuxFilesystemManager, LinuxInstallAsset, LinuxReleasePayloads, MacOsError, MacOsInstallAsset,
    UninstallManifest, linux_filesystem::map_macos_filesystem_asset,
};

/// Reuses the production no-follow writer with the closed macOS asset set.
pub struct MacOsFilesystemManager {
    inner: LinuxFilesystemManager,
}

impl MacOsFilesystemManager {
    #[cfg(test)]
    pub(crate) const fn from_linux_for_test(inner: LinuxFilesystemManager) -> Self {
        Self {
            inner: inner.for_macos(),
        }
    }

    pub(crate) fn new(
        groups: ManagedGroupBindings,
        broker_uid: u32,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<Self, MacOsError> {
        let payloads = LinuxReleasePayloads::from_authenticated_bundle(payloads)
            .map_err(|_| MacOsError::backend_failure())?;
        let inner = LinuxFilesystemManager::new(groups, broker_uid, payloads)
            .map_err(|_| MacOsError::backend_failure())?
            .for_macos();
        Ok(Self { inner })
    }

    pub(crate) fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        self.inner
            .bind_authenticated_nix_config(config)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn bind_uninstall_manifest(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        self.inner
            .bind_uninstall_manifest(manifest)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn replace_uninstall_manifest(
        &mut self,
        asset: MacOsInstallAsset,
        prior: &UninstallManifest,
        candidate: &UninstallManifest,
    ) -> Result<bool, MacOsError> {
        self.inner
            .replace_uninstall_manifest(map(asset)?, prior, candidate)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn rollback_uninstall_manifest_replacement(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .rollback_uninstall_manifest_replacement(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn recover_uninstall_manifest_replacement(
        &mut self,
        asset: MacOsInstallAsset,
        prior: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        self.inner
            .recover_uninstall_manifest_replacement(map(asset)?, prior)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn replacement_uninstall_manifest(
        &self,
    ) -> Result<Option<UninstallManifest>, MacOsError> {
        self.inner
            .replacement_uninstall_manifest()
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn existing_uninstall_manifest(
        &self,
        asset: MacOsInstallAsset,
    ) -> Result<Option<UninstallManifest>, MacOsError> {
        self.inner
            .existing_uninstall_manifest_for(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        self.inner
            .ensure_asset(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn install_static_asset(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        self.inner
            .install_static_asset(map(asset)?, contents)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn verify_asset(&self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .verify_asset(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn expected_file_digest(
        &self,
        asset: MacOsInstallAsset,
    ) -> Result<Digest, MacOsError> {
        self.inner
            .expected_file_digest(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn verify_repair_target(&self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .verify_repair_target(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn replace_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, MacOsError> {
        self.inner
            .replace_owned_file(map(asset)?, prior_digest, repair)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn replace_static_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, MacOsError> {
        self.inner
            .replace_static_owned_file(map(asset)?, contents, prior_digest, repair)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn recover_owned_file(
        &self,
        asset: MacOsInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner
            .recover_owned_file(map(asset)?, prior_digest)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn roll_forward_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .roll_forward_owned_file(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn finalize_owned_file(&self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .finalize_owned_file(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn verify_asset_absent(&self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .verify_asset_absent(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .rollback_asset(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn remove_verified_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .remove_verified_asset(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn remove_broker_channel_state(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .remove_broker_channel_state(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn remove_private_tree(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .remove_private_tree(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn remove_runtime_state(&self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner
            .remove_runtime_state(map(asset)?)
            .map_err(|_| MacOsError::backend_failure())
    }
}

fn map(asset: MacOsInstallAsset) -> Result<LinuxInstallAsset, MacOsError> {
    map_macos_filesystem_asset(asset).map_err(|_| MacOsError::backend_failure())
}
