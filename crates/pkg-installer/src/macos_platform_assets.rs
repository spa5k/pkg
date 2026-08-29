//! Production composition for the closed macOS account and filesystem assets.

use std::collections::BTreeMap;

use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};

use crate::{
    MacOsAssetPresence, MacOsError, MacOsInstallAsset, RecordedAsset, RecordedAssetState,
    UninstallManifest, macos_accounts::MacOsAccountManager,
    macos_filesystem::MacOsFilesystemManager, macos_install_assets, macos_product_install_assets,
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

    #[cfg(test)]
    fn with_filesystem_for_test(
        groups: ManagedGroupBindings,
        filesystem: MacOsFilesystemManager,
        system: System,
        digest: Digest,
    ) -> Result<Self, MacOsError> {
        Ok(Self {
            groups,
            accounts: MacOsAccountManager::new(groups)?,
            filesystem: Some(filesystem),
            payloads: None,
            config: None,
            receipt_binding: Some((system, digest)),
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

    pub(crate) fn verify_repair_target(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?.verify_repair_target(asset)
    }

    pub(crate) fn replace_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, MacOsError> {
        self.ensure_filesystem()?
            .replace_owned_file(asset, prior_digest, repair)
    }

    pub(crate) fn replace_static_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, MacOsError> {
        self.ensure_filesystem()?
            .replace_static_owned_file(asset, contents, prior_digest, repair)
    }

    pub(crate) fn recover_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?
            .recover_owned_file(asset, prior_digest)
    }

    pub(crate) fn roll_forward_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?.roll_forward_owned_file(asset)
    }

    pub(crate) fn finalize_owned_file(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?.finalize_owned_file(asset)
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
        // The receipt records the original installation, not the current run.
        // A repeat install observes every asset as preexisting, so a run-local
        // manifest would rewrite an exact, verified receipt. Compare against
        // the canonical installation manifest instead; an exact prior receipt
        // is verified and reused, not rewritten.
        let manifest = self.expected_product_manifest(system, digest)?;
        let asset = uninstall_manifest_asset()?;
        if let Some(prior) = self
            .ensure_filesystem()?
            .existing_uninstall_manifest(asset)?
        {
            if prior == manifest {
                self.ensure_filesystem()?
                    .bind_uninstall_manifest(&manifest)?;
                self.ensure_filesystem()?.verify_asset(asset)?;
                return Ok(false);
            }
            self.ensure_filesystem()?.bind_uninstall_manifest(&prior)?;
            let changed = self
                .ensure_filesystem()?
                .replace_uninstall_manifest(asset, &prior, &manifest)?;
            self.states.insert(asset.id(), RecordedAssetState::Created);
            return Ok(changed);
        }
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

    pub(crate) fn rollback_uninstall_manifest_replacement(&mut self) -> Result<(), MacOsError> {
        let asset = uninstall_manifest_asset()?;
        self.ensure_filesystem()?
            .rollback_uninstall_manifest_replacement(asset)
    }

    pub(crate) fn recover_uninstall_manifest_replacement(
        &mut self,
        prior: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        let asset = uninstall_manifest_asset()?;
        self.ensure_filesystem()?
            .recover_uninstall_manifest_replacement(asset, prior)
    }

    pub(crate) fn recover_uninstall_manifest_replacement_by_digest(
        &mut self,
        system: System,
        digest: Digest,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        use sha2::{Digest as _, Sha256};

        let candidate = self.expected_product_manifest(system, digest)?;
        let asset = uninstall_manifest_asset()?;
        let current = self
            .ensure_filesystem()?
            .existing_uninstall_manifest(asset)?;
        let current_is_prior = current.as_ref().is_some_and(|manifest| {
            crate::encode_uninstall_manifest(manifest).is_ok_and(|bytes| {
                manifest.system() == system
                    && Digest::from_bytes(Sha256::digest(bytes).into()) == prior_digest
            })
        });
        let prior = if current_is_prior {
            current.ok_or_else(MacOsError::backend_failure)?
        } else {
            self.ensure_filesystem()?
                .replacement_uninstall_manifest()?
                .ok_or_else(MacOsError::backend_failure)?
        };
        let bytes =
            crate::encode_uninstall_manifest(&prior).map_err(|_| MacOsError::backend_failure())?;
        if prior.system() != system
            || Digest::from_bytes(Sha256::digest(bytes).into()) != prior_digest
        {
            return Err(MacOsError::backend_failure());
        }
        self.ensure_filesystem()?
            .bind_uninstall_manifest(&candidate)?;
        self.ensure_filesystem()?
            .recover_uninstall_manifest_replacement(asset, &prior)
    }

    pub(crate) fn installed_uninstall_manifest(
        &mut self,
    ) -> Result<Option<UninstallManifest>, MacOsError> {
        let asset = uninstall_manifest_asset()?;
        self.ensure_filesystem()?.existing_uninstall_manifest(asset)
    }

    pub(crate) fn expected_product_manifest(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<UninstallManifest, MacOsError> {
        let mut records = Vec::new();
        for asset in macos_product_install_assets() {
            let state = if asset.id() == "nix-root" {
                RecordedAssetState::PreExisting
            } else {
                RecordedAssetState::Created
            };
            let mut record =
                RecordedAsset::new(asset.id(), state).map_err(|_| MacOsError::backend_failure())?;
            if asset.kind() == crate::MacOsAssetKind::File && asset.id() != "uninstall-manifest" {
                record = record
                    .with_content_digest(self.ensure_filesystem()?.expected_file_digest(asset)?);
            }
            records.push(record);
        }
        UninstallManifest::new(system, digest, records).map_err(|_| MacOsError::backend_failure())
    }

    pub(crate) fn bind_uninstall_manifest(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        self.ensure_filesystem()?.bind_uninstall_manifest(manifest)
    }

    pub(crate) fn bind_prior_asset_states(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), MacOsError> {
        for record in manifest.assets() {
            let asset = macos_product_install_assets()
                .find(|asset| asset.id() == record.id())
                .ok_or_else(MacOsError::backend_failure)?;
            // The clean-host preflight runs once in the recovery loader and
            // once in the installer, so rebinding the exact same prior state
            // is the normal repeat-install path. Only a conflicting prior
            // state is refused.
            match self.states.insert(asset.id(), record.state()) {
                Some(prior) if prior != record.state() => {
                    return Err(MacOsError::backend_failure());
                }
                _ => {}
            }
        }
        Ok(())
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
            "broker-socket-dir" | "helper-socket-dir" | "helper-log-dir" | "log-root" => {
                self.ensure_filesystem()?.remove_runtime_state(asset)
            }
            "broker-home" | "broker-log-dir" | "helper-home" => {
                self.ensure_filesystem()?.remove_private_tree(asset)
            }
            _ => self.ensure_filesystem()?.remove_verified_asset(asset),
        }
    }

    #[cfg(target_os = "macos")]
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

    pub(crate) fn verify_empty_store_mountpoint(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        if asset.id() != "nix-root" {
            return Err(MacOsError::backend_failure());
        }
        self.ensure_filesystem_with_broker_uid(self.groups.broker_gid())?
            .verify_empty_directory(asset)
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

    pub(crate) fn record_preexisting(&mut self, asset: MacOsInstallAsset) {
        self.states
            .insert(asset.id(), RecordedAssetState::PreExisting);
    }
}

fn uninstall_manifest_asset() -> Result<MacOsInstallAsset, MacOsError> {
    macos_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == "uninstall-manifest")
        .ok_or_else(MacOsError::backend_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LinuxFilesystemManager, LinuxReleasePayloads};
    use sha2::{Digest as _, Sha256};
    use std::{error::Error, fs, os::unix::fs::PermissionsExt as _, path::Path};

    fn manager(
        root: &Path,
        groups: ManagedGroupBindings,
        release: Digest,
    ) -> Result<MacOsPlatformAssetManager, Box<dyn Error>> {
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"root-helper", b"broker", b"pkg-cli")?;
        let inner =
            LinuxFilesystemManager::for_existing_preflight_test(root.to_path_buf(), payloads);
        MacOsPlatformAssetManager::with_filesystem_for_test(
            groups,
            MacOsFilesystemManager::from_linux_for_test(inner),
            System::Aarch64Darwin,
            release,
        )
        .map_err(Into::into)
    }

    fn prepare_receipt_parent(root: &Path) -> Result<(), Box<dyn Error>> {
        for (path, mode) in [
            ("opt", 0o755),
            ("opt/pkg", 0o755),
            ("opt/pkg/uninstall", 0o700),
        ] {
            let path = root.join(path);
            fs::create_dir(&path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    }

    #[test]
    fn repeat_install_receipt_records_the_original_installation() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        prepare_receipt_parent(temporary.path())?;
        let groups = ManagedGroupBindings::new(333, 350)?;
        let release = Digest::from_bytes([0x31; 32]);
        let mut installed = manager(temporary.path(), groups, release)?;
        let original = installed.expected_product_manifest(System::Aarch64Darwin, release)?;
        installed.bind_prior_asset_states(&original)?;
        let receipt = temporary.path().join("opt/pkg/uninstall/manifest.json");
        let bytes = crate::encode_uninstall_manifest(&original)?;
        fs::write(&receipt, &bytes)?;
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600))?;

        // A repeat install observes every asset as preexisting, but the exact
        // receipt from the original installation is verified and reused.
        let mut repeated = manager(temporary.path(), groups, release)?;
        repeated.bind_prior_asset_states(&original)?;
        assert!(!repeated.publish_uninstall_manifest()?);
        assert_eq!(fs::read(&receipt)?, bytes);
        Ok(())
    }

    #[test]
    fn rebinding_the_same_prior_states_is_idempotent() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        prepare_receipt_parent(temporary.path())?;
        let groups = ManagedGroupBindings::new(333, 350)?;
        let release = Digest::from_bytes([0x31; 32]);
        let mut manager = manager(temporary.path(), groups, release)?;
        let manifest = manager.expected_product_manifest(System::Aarch64Darwin, release)?;
        manager.bind_prior_asset_states(&manifest)?;
        // The clean-host preflight runs twice on a repeat install, so the
        // exact same prior states must rebind cleanly.
        manager.bind_prior_asset_states(&manifest)?;
        Ok(())
    }

    #[test]
    fn receipt_recovery_handles_intent_before_and_after_exchange() -> Result<(), Box<dyn Error>> {
        for exchanged in [false, true] {
            let temporary = tempfile::tempdir()?;
            prepare_receipt_parent(temporary.path())?;
            let groups = ManagedGroupBindings::new(333, 350)?;
            let current_release = Digest::from_bytes([0x31; 32]);
            let prior_release = Digest::from_bytes([0x21; 32]);
            let mut initial = manager(temporary.path(), groups, current_release)?;
            let candidate =
                initial.expected_product_manifest(System::Aarch64Darwin, current_release)?;
            let prior = UninstallManifest::new(
                System::Aarch64Darwin,
                prior_release,
                candidate.assets().to_vec(),
            )?;
            let prior_bytes = crate::encode_uninstall_manifest(&prior)?;
            let receipt = temporary.path().join("opt/pkg/uninstall/manifest.json");
            fs::write(&receipt, &prior_bytes)?;
            fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600))?;
            if exchanged {
                initial.bind_uninstall_manifest(&prior)?;
                initial.ensure_filesystem()?.replace_uninstall_manifest(
                    uninstall_manifest_asset()?,
                    &prior,
                    &candidate,
                )?;
            }
            drop(initial);

            let mut recovered = manager(temporary.path(), groups, current_release)?;
            recovered.recover_uninstall_manifest_replacement_by_digest(
                System::Aarch64Darwin,
                current_release,
                Digest::from_bytes(Sha256::digest(&prior_bytes).into()),
            )?;

            assert_eq!(recovered.installed_uninstall_manifest()?, Some(prior));
            assert!(
                recovered
                    .ensure_filesystem()?
                    .replacement_uninstall_manifest()?
                    .is_none()
            );
        }
        Ok(())
    }
}
