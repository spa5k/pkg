//! macOS binding for the descriptor-relative production filesystem writer.

use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};
use rustix::fs::{Dir, FileType, Mode, OFlags, fstat, open, openat};

use crate::{
    LinuxAssetKind, LinuxAssetPrincipal, LinuxFilesystemManager, LinuxInstallAsset,
    LinuxReleasePayloads, MacOsAssetKind, MacOsAssetPrincipal, MacOsError, MacOsInstallAsset,
    UninstallManifest,
};

/// Reuses the production no-follow writer with the closed macOS asset set.
pub struct MacOsFilesystemManager {
    inner: LinuxFilesystemManager,
}

impl MacOsFilesystemManager {
    pub(crate) fn new(
        groups: ManagedGroupBindings,
        broker_uid: u32,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<Self, MacOsError> {
        let payloads = LinuxReleasePayloads::from_authenticated_bundle(payloads)
            .map_err(|_| MacOsError::backend_failure())?;
        let inner = LinuxFilesystemManager::new(groups, broker_uid, payloads)
            .map_err(|_| MacOsError::backend_failure())?;
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

    pub(crate) fn verify_empty_directory(
        &self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.inner
            .verify_empty_directory(map(asset)?)
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

pub fn verify_inert_nix_mountpoint() -> Result<(), MacOsError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = open("/", flags, Mode::empty()).map_err(|_| MacOsError::backend_failure())?;
    let root_metadata = fstat(&root).map_err(|_| MacOsError::backend_failure())?;
    let nix =
        openat(&root, "nix", flags, Mode::empty()).map_err(|_| MacOsError::backend_failure())?;
    let metadata = fstat(&nix).map_err(|_| MacOsError::backend_failure())?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
        || metadata.st_uid != 0
        || metadata.st_gid != 0
        || metadata.st_mode & 0o7777 != 0o755
        || metadata.st_dev != root_metadata.st_dev
    {
        return Err(MacOsError::backend_failure());
    }
    let mut entries = Dir::read_from(&nix).map_err(|_| MacOsError::backend_failure())?;
    for entry in &mut entries {
        let entry = entry.map_err(|_| MacOsError::backend_failure())?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            return Err(MacOsError::backend_failure());
        }
    }
    Ok(())
}

fn map(asset: MacOsInstallAsset) -> Result<LinuxInstallAsset, MacOsError> {
    let kind = match asset.kind() {
        MacOsAssetKind::Directory => LinuxAssetKind::Directory,
        MacOsAssetKind::File => LinuxAssetKind::File,
        MacOsAssetKind::User | MacOsAssetKind::Group => return Err(MacOsError::backend_failure()),
    };
    let owner = map_principal(asset.owner().ok_or_else(MacOsError::backend_failure)?)?;
    let group = map_principal(asset.group().ok_or_else(MacOsError::backend_failure)?)?;
    Ok(LinuxInstallAsset::platform_filesystem(
        asset.id(),
        kind,
        asset.path_or_name(),
        asset.mode().ok_or_else(MacOsError::backend_failure)?,
        owner,
        group,
        asset.install_owner(),
    ))
}

const fn map_principal(principal: MacOsAssetPrincipal) -> Result<LinuxAssetPrincipal, MacOsError> {
    match principal {
        MacOsAssetPrincipal::Root | MacOsAssetPrincipal::Wheel => Ok(LinuxAssetPrincipal::Root),
        MacOsAssetPrincipal::Broker => Ok(LinuxAssetPrincipal::Broker),
        MacOsAssetPrincipal::Build => Ok(LinuxAssetPrincipal::BuildUsers),
        MacOsAssetPrincipal::Admin => Err(MacOsError::backend_failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assets::InstallAssetOwner, macos_install_assets};

    #[test]
    fn map_preserves_base_nix_install_owner() -> Result<(), Box<dyn std::error::Error>> {
        let source = macos_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "nix-root")
            .ok_or_else(|| std::io::Error::other("missing Base Nix test asset"))?;

        assert_eq!(
            map(source)?,
            LinuxInstallAsset::platform_filesystem(
                source.id(),
                LinuxAssetKind::Directory,
                source.path_or_name(),
                source
                    .mode()
                    .ok_or_else(|| std::io::Error::other("missing Base Nix test mode"))?,
                LinuxAssetPrincipal::Root,
                LinuxAssetPrincipal::Root,
                InstallAssetOwner::BaseNix,
            )
        );
        Ok(())
    }
}
