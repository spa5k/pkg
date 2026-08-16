//! Production composition for the closed macOS account and filesystem assets.

use std::collections::BTreeMap;

use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};

use crate::{
    MacOsAssetPresence, MacOsError, MacOsInstallAsset, RecordedAsset, RecordedAssetState,
    UninstallManifest, macos_accounts::MacOsAccountManager,
    macos_filesystem::MacOsFilesystemManager, macos_install_assets,
};

pub struct MacOsPlatformAssetManager {
    groups: ManagedGroupBindings,
    accounts: MacOsAccountManager,
    filesystem: Option<MacOsFilesystemManager>,
    payloads: Option<AuthenticatedInstallerPayloads>,
    config: Option<AuthenticatedManagedNixConfig>,
    receipt_binding: Option<(System, Digest)>,
    states: BTreeMap<&'static str, RecordedAssetState>,
}

impl MacOsPlatformAssetManager {
    pub(crate) fn new(groups: ManagedGroupBindings) -> Result<Self, MacOsError> {
        Ok(Self {
            groups,
            accounts: MacOsAccountManager::new(groups)?,
            filesystem: None,
            payloads: None,
            config: None,
            receipt_binding: None,
            states: BTreeMap::new(),
        })
    }

    pub(crate) fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError> {
        if self.filesystem.is_some()
            || self
                .payloads
                .as_ref()
                .is_some_and(|bound| bound != payloads)
        {
            return Err(MacOsError::backend_failure());
        }
        self.payloads = Some(payloads.clone());
        Ok(())
    }

    pub(crate) fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        if self.config.as_ref().is_some_and(|bound| bound != config) {
            return Err(MacOsError::backend_failure());
        }
        if let Some(filesystem) = self.filesystem.as_mut() {
            filesystem.bind_authenticated_nix_config(config)?;
        }
        self.config = Some(config.clone());
        Ok(())
    }

    pub(crate) fn bind_authenticated_ownership(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), MacOsError> {
        if !matches!(system, System::X8664Darwin | System::Aarch64Darwin)
            || self
                .receipt_binding
                .is_some_and(|binding| binding != (system, digest))
        {
            return Err(MacOsError::backend_failure());
        }
        self.receipt_binding = Some((system, digest));
        Ok(())
    }

    pub(crate) fn authenticated_inputs_bound(&self, system: System) -> bool {
        self.payloads
            .as_ref()
            .is_some_and(|payloads| payloads.system() == system)
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.system() == system)
            && self
                .receipt_binding
                .is_some_and(|(bound, _)| bound == system)
    }

    pub(crate) fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.accounts.broker_uid()
    }

    pub(crate) fn classify_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        if MacOsAccountManager::handles(asset) {
            if self.accounts.verify_asset(asset).is_ok() {
                return Ok(MacOsAssetPresence::ExactPresent);
            }
            self.accounts
                .verify_asset_absent(asset)
                .map(|()| MacOsAssetPresence::Absent)
        } else {
            if self.ensure_filesystem()?.verify_asset(asset).is_ok() {
                return Ok(MacOsAssetPresence::ExactPresent);
            }
            self.ensure_filesystem()?
                .verify_asset_absent(asset)
                .map(|()| MacOsAssetPresence::Absent)
        }
    }

    pub(crate) fn classify_for_removal(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        if MacOsAccountManager::handles(asset) {
            self.accounts.classify_for_removal(asset)
        } else {
            self.classify_asset(asset)
        }
    }

    pub(crate) fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let created = if MacOsAccountManager::handles(asset) {
            self.accounts.ensure_asset(asset)?
        } else {
            self.ensure_filesystem()?.ensure_asset(asset)?
        };
        self.record(asset, created);
        Ok(created)
    }

    pub(crate) fn install_static_asset(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        let created = self
            .ensure_filesystem()?
            .install_static_asset(asset, contents)?;
        self.record(asset, created);
        Ok(created)
    }

    pub(crate) fn remove_verified_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        if MacOsAccountManager::handles(asset) {
            self.accounts.remove_verified_asset(asset)
        } else {
            self.ensure_filesystem()?.remove_verified_asset(asset)
        }
    }

    pub(crate) fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if MacOsAccountManager::handles(asset) {
            self.accounts.rollback_asset(asset)?;
        } else {
            self.ensure_filesystem()?.rollback_asset(asset)?;
        }
        self.states.remove(asset.id());
        Ok(())
    }

    pub(crate) fn classify_uninstall_manifest(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        let (system, digest) = self
            .receipt_binding
            .ok_or_else(MacOsError::backend_failure)?;
        let asset = uninstall_manifest_asset()?;
        let Some(existing) = self
            .ensure_filesystem()?
            .existing_uninstall_manifest(asset)?
        else {
            return Ok(MacOsAssetPresence::Absent);
        };
        if existing.system() != system || existing.ownership_manifest_digest() != digest {
            return Err(MacOsError::backend_failure());
        }
        let filesystem = self.ensure_filesystem()?;
        filesystem.bind_uninstall_manifest(&existing)?;
        filesystem.verify_asset(asset)?;
        Ok(MacOsAssetPresence::ExactPresent)
    }

    pub(crate) fn publish_uninstall_manifest(&mut self) -> Result<bool, MacOsError> {
        let (system, digest) = self
            .receipt_binding
            .ok_or_else(MacOsError::backend_failure)?;
        if self.classify_uninstall_manifest()? == MacOsAssetPresence::ExactPresent {
            return Ok(false);
        }
        if self
            .states
            .values()
            .any(|state| *state != RecordedAssetState::Created)
        {
            return Err(MacOsError::backend_failure());
        }
        let records = macos_install_assets()
            .iter()
            .map(|asset| {
                let state = if asset.id() == "uninstall-manifest" {
                    RecordedAssetState::Created
                } else {
                    *self
                        .states
                        .get(asset.id())
                        .ok_or_else(MacOsError::backend_failure)?
                };
                RecordedAsset::new(asset.id(), state).map_err(|_| MacOsError::backend_failure())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let manifest = UninstallManifest::new(system, digest, records)
            .map_err(|_| MacOsError::backend_failure())?;
        let asset = uninstall_manifest_asset()?;
        let filesystem = self.ensure_filesystem()?;
        filesystem.bind_uninstall_manifest(&manifest)?;
        if !filesystem.ensure_asset(asset)? {
            return Err(MacOsError::backend_failure());
        }
        self.states.insert(asset.id(), RecordedAssetState::Created);
        Ok(true)
    }

    pub(crate) fn recover_uninstall_manifest(&mut self) -> Result<(), MacOsError> {
        let asset = uninstall_manifest_asset()?;
        self.ensure_filesystem()?.remove_verified_asset(asset)
    }

    pub(crate) fn installed_uninstall_manifest(
        &mut self,
    ) -> Result<Option<UninstallManifest>, MacOsError> {
        let asset = uninstall_manifest_asset()?;
        self.ensure_filesystem()?.existing_uninstall_manifest(asset)
    }

    pub(crate) fn bind_uninstall_manifest(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?.bind_uninstall_manifest(manifest)
    }

    pub(crate) fn remove_uninstall_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        if MacOsAccountManager::handles(asset) {
            return self.accounts.remove_verified_asset(asset);
        }
        match asset.id() {
            "broker-channel-state" => self.ensure_filesystem()?.remove_broker_channel_state(asset),
            "broker-home" | "broker-log-dir" | "helper-home" => {
                self.ensure_filesystem()?.remove_private_tree(asset)
            }
            _ => self.ensure_filesystem()?.remove_verified_asset(asset),
        }
    }

    pub(crate) fn classify_store_mountpoint(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        if asset.id() != "nix-root" {
            return Err(MacOsError::backend_failure());
        }
        self.ensure_filesystem_with_broker_uid(self.groups.broker_gid())?
            .verify_asset(asset)
            .map(|()| MacOsAssetPresence::ExactPresent)
    }

    pub(crate) fn bind_filesystem_after_broker_removal(&mut self) -> Result<(), MacOsError> {
        self.ensure_filesystem_with_broker_uid(self.groups.broker_gid())?;
        Ok(())
    }

    fn ensure_filesystem(&mut self) -> Result<&mut MacOsFilesystemManager, MacOsError> {
        if self.filesystem.is_none() {
            let broker_uid = self.accounts.broker_uid()?;
            return self.ensure_filesystem_with_broker_uid(broker_uid);
        }
        self.filesystem
            .as_mut()
            .ok_or_else(MacOsError::backend_failure)
    }

    fn ensure_filesystem_with_broker_uid(
        &mut self,
        broker_uid: u32,
    ) -> Result<&mut MacOsFilesystemManager, MacOsError> {
        if self.filesystem.is_none() {
            let payloads = self
                .payloads
                .as_ref()
                .ok_or_else(MacOsError::backend_failure)?;
            let mut filesystem = MacOsFilesystemManager::new(self.groups, broker_uid, payloads)?;
            if let Some(config) = self.config.as_ref() {
                filesystem.bind_authenticated_nix_config(config)?;
            }
            self.filesystem = Some(filesystem);
        }
        self.filesystem
            .as_mut()
            .ok_or_else(MacOsError::backend_failure)
    }

    fn record(&mut self, asset: MacOsInstallAsset, created: bool) {
        self.states.entry(asset.id()).or_insert(if created {
            RecordedAssetState::Created
        } else {
            RecordedAssetState::PreExisting
        });
    }

    pub(crate) fn record_created(&mut self, asset: MacOsInstallAsset) {
        self.states.insert(asset.id(), RecordedAssetState::Created);
    }
}

fn uninstall_manifest_asset() -> Result<MacOsInstallAsset, MacOsError> {
    macos_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == "uninstall-manifest")
        .ok_or_else(MacOsError::backend_failure)
}
