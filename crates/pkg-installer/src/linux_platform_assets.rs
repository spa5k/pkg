//! Production composition of Linux account and filesystem installation assets.

use std::collections::BTreeMap;

use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};

use crate::{
    InstallError, LinuxAccountManager, LinuxFilesystemManager, LinuxInstallAsset,
    LinuxReleasePayloads, RecordedAsset, RecordedAssetState, UninstallManifest,
    assets::{is_linux_product_asset, is_linux_service_runtime_asset},
    linux_filesystem::LinuxReceiptReader,
    linux_install_assets,
};

/// Whether one fixed asset is exact-present or absent before a write-ahead intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxAssetPresence {
    /// The exact fixed asset exists and matches the closed contract.
    ExactPresent,
    /// The fixed asset is absent.
    Absent,
}

/// Explicit product-file mutation authority for one installer invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxProductAssetIntent {
    /// Install on a clean host, repeat the same release, or authenticate an upgrade.
    InstallOrUpgrade,
    /// Restore owned Linux product files from the same authenticated release.
    Repair,
}

#[derive(Debug, Clone)]
enum InstalledManifest {
    Unloaded,
    Absent,
    Present(UninstallManifest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementAuthority {
    Upgrade { prior_digest: Digest },
    RepairExisting,
    RepairMissing,
}

impl ReplacementAuthority {
    const fn prior_digest(self) -> Option<Digest> {
        match self {
            Self::Upgrade { prior_digest } => Some(prior_digest),
            Self::RepairExisting | Self::RepairMissing => None,
        }
    }

    const fn is_repair(self) -> bool {
        matches!(self, Self::RepairExisting | Self::RepairMissing)
    }
}

/// Routes the closed Linux asset set through the production account and
/// descriptor-relative filesystem implementations.
pub struct LinuxPlatformAssetManager {
    groups: ManagedGroupBindings,
    accounts: LinuxAccountManager,
    filesystem: Option<LinuxFilesystemManager>,
    payloads: Option<LinuxReleasePayloads>,
    config: Option<AuthenticatedManagedNixConfig>,
    #[cfg(test)]
    authenticated_config_system_for_test: Option<System>,
    receipt_binding: Option<(System, Digest)>,
    intent: LinuxProductAssetIntent,
    installed_manifest: InstalledManifest,
    states: BTreeMap<&'static str, RecordedAssetState>,
    #[cfg(test)]
    pre_broker_receipt_reader: Option<LinuxReceiptReader>,
}

#[cfg(test)]
pub struct ExistingNonFilePreflightAssets {
    pub manager: LinuxPlatformAssetManager,
    pub temporary: tempfile::TempDir,
    pub account_mutation_calls: std::rc::Rc<std::cell::Cell<usize>>,
}

impl std::fmt::Debug for LinuxPlatformAssetManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxPlatformAssetManager")
            .field("groups", &self.groups)
            .field("filesystem_ready", &self.filesystem.is_some())
            .field("config_bound", &self.config.is_some())
            .field("receipt_binding", &self.receipt_binding)
            .field("intent", &self.intent)
            .field("recorded_assets", &self.states.len())
            .finish_non_exhaustive()
    }
}

impl LinuxPlatformAssetManager {
    /// Creates the production asset router from authenticated host group ids.
    #[must_use]
    pub fn new(groups: ManagedGroupBindings) -> Self {
        Self::with_intent(groups, LinuxProductAssetIntent::InstallOrUpgrade)
    }

    /// Creates the existing asset manager with one explicit mutation intent.
    #[must_use]
    pub(super) fn with_intent(
        groups: ManagedGroupBindings,
        intent: LinuxProductAssetIntent,
    ) -> Self {
        Self {
            groups,
            accounts: LinuxAccountManager::new(groups),
            filesystem: None,
            payloads: None,
            config: None,
            #[cfg(test)]
            authenticated_config_system_for_test: None,
            receipt_binding: None,
            intent,
            installed_manifest: InstalledManifest::Unloaded,
            states: BTreeMap::new(),
            #[cfg(test)]
            pre_broker_receipt_reader: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_existing_non_file_preflight_test(
        groups: ManagedGroupBindings,
        system: System,
        release: Digest,
        missing_id: &str,
    ) -> Result<ExistingNonFilePreflightAssets, Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        for path in [
            "opt",
            "nix",
            "nix/var",
            "nix/var/nix",
            "nix/var/nix/gcroots",
            "var",
            "var/lib",
            "run",
            "usr",
            "usr/lib",
            "usr/lib/systemd",
            "usr/lib/systemd/system",
            "usr/lib/tmpfiles.d",
            "usr/local",
            "usr/local/bin",
            "etc",
            "etc/profile.d",
        ] {
            std::fs::create_dir(temporary.path().join(path))?;
        }
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"root-helper", b"broker", b"pkg-cli")?;
        let (accounts, account_mutation_calls) =
            LinuxAccountManager::for_existing_preflight_test(groups, Some(missing_id));
        let mut filesystem = LinuxFilesystemManager::for_existing_preflight_test(
            temporary.path().to_path_buf(),
            payloads.clone(),
        );
        filesystem.bind_config_bytes_for_test(b"test-nix-config");
        for asset in crate::assets::linux_product_install_assets()
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::Directory)
            .filter(|asset| asset.id() != missing_id)
        {
            filesystem.ensure_asset(asset)?;
        }
        for asset in crate::assets::linux_product_install_assets()
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::File)
            .filter(|asset| asset.id() != "uninstall-manifest" && asset.id() != missing_id)
        {
            filesystem.ensure_asset(asset)?;
        }
        let records = crate::assets::linux_product_install_assets()
            .map(|asset| -> Result<_, Box<dyn std::error::Error>> {
                let record = RecordedAsset::new(asset.id(), RecordedAssetState::Created)?;
                Ok(
                    if asset.kind() == crate::LinuxAssetKind::File
                        && asset.id() != "uninstall-manifest"
                    {
                        record.with_content_digest(filesystem.expected_file_digest(asset)?)
                    } else {
                        record
                    },
                )
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let manifest = UninstallManifest::new(system, release, records)?;
        filesystem.bind_uninstall_manifest(&manifest)?;
        let receipt = uninstall_manifest_asset()?;
        if !filesystem.ensure_asset(receipt)? {
            return Err(std::io::Error::other("test receipt already existed").into());
        }
        Ok(ExistingNonFilePreflightAssets {
            manager: Self {
                groups,
                accounts,
                filesystem: Some(filesystem),
                payloads: Some(payloads),
                config: None,
                authenticated_config_system_for_test: Some(system),
                receipt_binding: Some((system, release)),
                intent: LinuxProductAssetIntent::InstallOrUpgrade,
                installed_manifest: InstalledManifest::Unloaded,
                states: BTreeMap::new(),
                pre_broker_receipt_reader: None,
            },
            temporary,
            account_mutation_calls,
        })
    }

    pub(crate) const fn set_intent(&mut self, intent: LinuxProductAssetIntent) {
        self.intent = intent;
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

    /// Returns whether the canonical installed receipt names a different
    /// release, or verifies every compiled product asset for this release.
    pub(crate) fn classify_exact_release(&mut self) -> Result<bool, InstallError> {
        let (system, release) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let Some(manifest) = self.load_installed_manifest()? else {
            return Ok(false);
        };
        if manifest.system() != system {
            return Err(InstallError::backend_failure());
        }
        self.ensure_filesystem()?
            .bind_uninstall_manifest(&manifest)
            .map_err(|_| InstallError::backend_failure())?;
        self.verify_asset_exact(uninstall_manifest_asset()?)?;
        if manifest.ownership_manifest_digest() != release {
            return Ok(false);
        }
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
        {
            self.verify_asset_exact(asset)?;
            if asset.kind() == crate::LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                let recorded = manifest
                    .assets()
                    .iter()
                    .find(|record| record.id() == asset.id())
                    .and_then(RecordedAsset::content_digest)
                    .ok_or_else(InstallError::backend_failure)?;
                let expected = self
                    .ensure_filesystem()?
                    .expected_file_digest(asset)
                    .map_err(|_| InstallError::backend_failure())?;
                if recorded != expected {
                    return Err(InstallError::backend_failure());
                }
            }
        }
        Ok(true)
    }

    /// Authenticates the complete non-file repair boundary before mutation.
    pub(crate) fn preflight_repair(&mut self) -> Result<(), InstallError> {
        if self.intent != LinuxProductAssetIntent::Repair {
            return Err(InstallError::backend_failure());
        }
        let (system, release) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let manifest = self
            .load_installed_manifest()?
            .ok_or_else(InstallError::backend_failure)?;
        if manifest.system() != system || manifest.ownership_manifest_digest() != release {
            return Err(InstallError::backend_failure());
        }
        let receipt = uninstall_manifest_asset()?;
        self.ensure_filesystem()?
            .bind_uninstall_manifest(&manifest)
            .map_err(|_| InstallError::backend_failure())?;
        self.ensure_filesystem()?
            .verify_asset(receipt)
            .map_err(|_| InstallError::backend_failure())?;

        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
        {
            let record = manifest
                .assets()
                .iter()
                .find(|record| record.id() == asset.id())
                .ok_or_else(InstallError::backend_failure)?;
            if asset.kind() != crate::LinuxAssetKind::File
                || record.state() == RecordedAssetState::PreExisting
            {
                self.verify_asset_exact(asset)?;
            } else if asset.id() != "uninstall-manifest" {
                self.ensure_filesystem()?
                    .verify_repair_target(asset)
                    .map_err(|_| InstallError::backend_failure())?;
            }
        }
        Ok(())
    }

    pub(crate) fn preflight_existing_non_files(&mut self) -> Result<(), InstallError> {
        let (system, _) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let manifest = self
            .load_installed_manifest()?
            .ok_or_else(InstallError::backend_failure)?;
        if manifest.system() != system {
            return Err(InstallError::backend_failure());
        }
        let receipt = uninstall_manifest_asset()?;
        self.ensure_filesystem()?
            .bind_uninstall_manifest(&manifest)
            .map_err(|_| InstallError::backend_failure())?;
        self.ensure_filesystem()?
            .verify_asset(receipt)
            .map_err(|_| InstallError::backend_failure())?;
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .filter(|asset| asset.kind() != crate::LinuxAssetKind::File)
        {
            if !manifest
                .assets()
                .iter()
                .any(|record| record.id() == asset.id())
            {
                return Err(InstallError::backend_failure());
            }
            self.verify_asset_exact(asset)?;
        }
        Ok(())
    }

    pub(crate) fn verify_service_runtime_assets(&mut self) -> Result<(), InstallError> {
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_service_runtime_asset(*asset))
        {
            self.verify_asset_exact(asset)?;
        }
        Ok(())
    }

    pub(crate) fn recover_repair_assets(
        &mut self,
        mut preflight_mutation: impl FnMut() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        self.preflight_repair()?;
        let manifest = self
            .load_installed_manifest()?
            .ok_or_else(InstallError::backend_failure)?;
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::File)
            .filter(|asset| asset.id() != "uninstall-manifest")
        {
            let record = manifest
                .assets()
                .iter()
                .find(|record| record.id() == asset.id())
                .ok_or_else(InstallError::backend_failure)?;
            if record.state() == RecordedAssetState::Created {
                preflight_mutation()?;
                self.ensure_filesystem()?
                    .roll_forward_owned_file(asset)
                    .map_err(|_| InstallError::backend_failure())?;
            }
        }
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
        {
            self.verify_asset_exact(asset)?;
        }
        Ok(())
    }

    /// Verifies or creates one closed account, directory, or release file.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure when the production component refuses.
    pub fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let created = if LinuxAccountManager::handles(asset) {
            self.require_non_file_mutation_authority(asset)?;
            self.accounts
                .ensure_asset(asset)
                .map_err(|_| InstallError::backend_failure())?
        } else {
            if asset.kind() != crate::LinuxAssetKind::File {
                self.require_non_file_mutation_authority(asset)?;
            }
            let replacement = self.file_replacement(asset)?;
            let filesystem = self.ensure_filesystem()?;
            match replacement {
                Some(ReplacementAuthority::RepairMissing) => filesystem
                    .ensure_asset(asset)
                    .map_err(|_| InstallError::backend_failure())?,
                Some(authority) => filesystem
                    .replace_owned_file(asset, authority.prior_digest(), authority.is_repair())
                    .map_err(|_| InstallError::backend_failure())?,
                None => filesystem
                    .ensure_asset(asset)
                    .map_err(|_| InstallError::backend_failure())?,
            }
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
        let replacement = self.file_replacement(asset)?;
        let filesystem = self.ensure_filesystem()?;
        let created = match replacement {
            Some(ReplacementAuthority::RepairMissing) => filesystem
                .install_static_asset(asset, contents)
                .map_err(|_| InstallError::backend_failure())?,
            Some(authority) => filesystem
                .replace_static_owned_file(
                    asset,
                    contents,
                    authority.prior_digest(),
                    authority.is_repair(),
                )
                .map_err(|_| InstallError::backend_failure())?,
            None => filesystem
                .install_static_asset(asset, contents)
                .map_err(|_| InstallError::backend_failure())?,
        };
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
        let config_system = self
            .config
            .as_ref()
            .map(AuthenticatedManagedNixConfig::system);
        #[cfg(test)]
        let config_system = config_system.or(self.authenticated_config_system_for_test);
        if config_system != Some(system) {
            return Err(InstallError::backend_failure());
        }

        if let Some(existing) = self.load_installed_manifest()? {
            if existing.system() != system {
                return Err(InstallError::backend_failure());
            }
            if existing.ownership_manifest_digest() == digest {
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
            if self.intent == LinuxProductAssetIntent::Repair {
                return Err(InstallError::backend_failure());
            }

            let records = self.current_records(Some(&existing))?;
            let manifest = UninstallManifest::new(system, digest, records)
                .map_err(|_| InstallError::backend_failure())?;
            let filesystem = self.ensure_filesystem()?;
            let asset = uninstall_manifest_asset()?;
            let created = filesystem
                .replace_uninstall_manifest(asset, &existing, &manifest)
                .map_err(|_| InstallError::backend_failure())?;
            if !created {
                return Err(InstallError::backend_failure());
            }
            self.states.insert(asset.id(), RecordedAssetState::Created);
            self.installed_manifest = InstalledManifest::Present(manifest);
            return Ok(true);
        }

        if self.intent == LinuxProductAssetIntent::Repair {
            return Err(InstallError::backend_failure());
        }
        let records = self.current_records(None)?;
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
        self.installed_manifest = InstalledManifest::Present(manifest);
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
        let has_backup = self
            .ensure_filesystem()?
            .replacement_backup_exists(uninstall_manifest_asset()?)
            .map_err(|_| InstallError::backend_failure())?;
        let Some(existing) = self.load_installed_manifest()? else {
            return Ok(if has_backup {
                LinuxAssetPresence::ExactPresent
            } else {
                LinuxAssetPresence::Absent
            });
        };
        if existing.system() != system {
            return Err(InstallError::backend_failure());
        }
        if has_backup {
            return Ok(LinuxAssetPresence::ExactPresent);
        }
        if existing.ownership_manifest_digest() != digest {
            return if self.intent == LinuxProductAssetIntent::InstallOrUpgrade {
                Ok(LinuxAssetPresence::Absent)
            } else {
                Err(InstallError::backend_failure())
            };
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
        if self.intent == LinuxProductAssetIntent::Repair
            && asset.kind() == crate::LinuxAssetKind::File
            && asset.id() != "uninstall-manifest"
        {
            let manifest = self
                .load_installed_manifest()?
                .ok_or_else(InstallError::backend_failure)?;
            self.replacement_authority(asset, &manifest)?;
            self.ensure_filesystem()?
                .roll_forward_owned_file(asset)
                .map_err(|_| InstallError::backend_failure())?;
            self.states.remove(asset.id());
            return Ok(());
        }
        if LinuxAccountManager::handles(asset) {
            self.accounts
                .rollback_asset(asset)
                .map_err(|_| InstallError::backend_failure())?;
        } else {
            let filesystem = self
                .filesystem
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?;
            if asset.id() == "uninstall-manifest"
                && filesystem
                    .replacement_backup_exists(asset)
                    .map_err(|_| InstallError::backend_failure())?
            {
                filesystem
                    .rollback_uninstall_manifest_replacement(asset)
                    .map_err(|_| InstallError::backend_failure())?;
                self.installed_manifest = InstalledManifest::Present(
                    filesystem
                        .existing_uninstall_manifest()
                        .map_err(|_| InstallError::backend_failure())?
                        .ok_or_else(InstallError::backend_failure)?,
                );
            } else {
                filesystem
                    .rollback_asset(asset)
                    .map_err(|_| InstallError::backend_failure())?;
            }
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
            if self.ensure_filesystem()?.verify_asset(asset).is_ok() {
                return Ok(LinuxAssetPresence::ExactPresent);
            }
            if asset.kind() == crate::LinuxAssetKind::File
                && self
                    .ensure_filesystem()?
                    .replacement_backup_exists(asset)
                    .map_err(|_| InstallError::backend_failure())?
            {
                return Ok(LinuxAssetPresence::ExactPresent);
            }
            let absent = self.ensure_filesystem()?.verify_asset_absent(asset).is_ok();
            let manifest = self.load_installed_manifest()?;
            let Some(manifest) = manifest else {
                return absent
                    .then_some(LinuxAssetPresence::Absent)
                    .ok_or_else(InstallError::backend_failure);
            };
            if asset.kind() != crate::LinuxAssetKind::File {
                return Err(InstallError::backend_failure());
            }
            let replacement = self.replacement_authority(asset, &manifest)?;
            if absent {
                return if replacement.is_repair() {
                    Ok(LinuxAssetPresence::Absent)
                } else {
                    Err(InstallError::backend_failure())
                };
            }
            if let Some(prior) = replacement.prior_digest() {
                self.ensure_filesystem()?
                    .verify_asset_digest(asset, prior)
                    .map_err(|_| InstallError::backend_failure())?;
            }
            Ok(LinuxAssetPresence::Absent)
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

    /// Restores one interrupted asset mutation from durable receipt state.
    pub(crate) fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if LinuxAccountManager::handles(asset) || asset.kind() != crate::LinuxAssetKind::File {
            if self.intent == LinuxProductAssetIntent::Repair {
                return Err(InstallError::backend_failure());
            }
            return self.remove_verified_asset(asset);
        }
        if asset.id() == "uninstall-manifest" {
            return self.recover_uninstall_manifest();
        }

        let Some(manifest) = self.load_installed_manifest()? else {
            return self
                .ensure_filesystem()?
                .recover_created_file(asset)
                .map_err(|_| InstallError::backend_failure());
        };
        let replacement = self.replacement_authority(asset, &manifest)?;
        if replacement.is_repair() {
            return self
                .ensure_filesystem()?
                .roll_forward_owned_file(asset)
                .map_err(|_| InstallError::backend_failure());
        }
        let prior_digest = replacement
            .prior_digest()
            .ok_or_else(InstallError::backend_failure)?;
        let has_backup = self
            .ensure_filesystem()?
            .replacement_backup_exists(asset)
            .map_err(|_| InstallError::backend_failure())?;
        if has_backup {
            return self
                .ensure_filesystem()?
                .recover_owned_file(asset, prior_digest)
                .map_err(|_| InstallError::backend_failure());
        }

        self.ensure_filesystem()?
            .recover_owned_file(asset, prior_digest)
            .map_err(|_| InstallError::backend_failure())
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
        let prior = match &self.installed_manifest {
            InstalledManifest::Present(manifest) => manifest
                .assets()
                .iter()
                .find(|record| record.id() == asset.id())
                .map(RecordedAsset::state),
            InstalledManifest::Unloaded | InstalledManifest::Absent => None,
        };
        self.states.entry(asset.id()).or_insert_with(|| {
            prior.unwrap_or(if created {
                RecordedAssetState::Created
            } else {
                RecordedAssetState::PreExisting
            })
        });
    }

    fn load_installed_manifest(&mut self) -> Result<Option<UninstallManifest>, InstallError> {
        if matches!(self.installed_manifest, InstalledManifest::Unloaded) {
            let manifest = if self.filesystem.is_none() {
                let payloads = self
                    .payloads
                    .clone()
                    .ok_or_else(InstallError::backend_failure)?;
                #[cfg(test)]
                let result = self.pre_broker_receipt_reader.as_ref().map_or_else(
                    || LinuxReceiptReader::new(self.groups, payloads).existing_uninstall_manifest(),
                    LinuxReceiptReader::existing_uninstall_manifest,
                );
                #[cfg(not(test))]
                let result =
                    LinuxReceiptReader::new(self.groups, payloads).existing_uninstall_manifest();
                result.map_err(|_| InstallError::backend_failure())?
            } else {
                self.ensure_filesystem()?
                    .existing_uninstall_manifest()
                    .map_err(|_| InstallError::backend_failure())?
            };
            self.installed_manifest =
                manifest.map_or(InstalledManifest::Absent, InstalledManifest::Present);
        }
        Ok(match &self.installed_manifest {
            InstalledManifest::Present(manifest) => Some(manifest.clone()),
            InstalledManifest::Unloaded | InstalledManifest::Absent => None,
        })
    }

    fn verify_asset_exact(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if LinuxAccountManager::handles(asset) {
            self.accounts
                .verify_asset(asset)
                .map_err(|_| InstallError::backend_failure())
        } else {
            self.ensure_filesystem()?
                .verify_asset(asset)
                .map_err(|_| InstallError::backend_failure())
        }
    }

    fn require_non_file_mutation_authority(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), InstallError> {
        if asset.kind() == crate::LinuxAssetKind::File {
            return Err(InstallError::backend_failure());
        }
        let Some(manifest) = self.load_installed_manifest()? else {
            return self.non_file_requires_exact(asset, None).map(|_| ());
        };
        if self.non_file_requires_exact(asset, Some(&manifest))? {
            self.verify_asset_exact(asset)?;
        }
        Ok(())
    }

    fn non_file_requires_exact(
        &self,
        asset: LinuxInstallAsset,
        manifest: Option<&UninstallManifest>,
    ) -> Result<bool, InstallError> {
        if asset.kind() == crate::LinuxAssetKind::File {
            return Err(InstallError::backend_failure());
        }
        let Some(manifest) = manifest else {
            return if self.intent == LinuxProductAssetIntent::Repair {
                Err(InstallError::backend_failure())
            } else {
                Ok(false)
            };
        };
        manifest
            .assets()
            .iter()
            .find(|record| record.id() == asset.id())
            .ok_or_else(InstallError::backend_failure)?;
        Ok(true)
    }

    fn recover_uninstall_manifest(&mut self) -> Result<(), InstallError> {
        let (system, current_release) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let current = self
            .ensure_filesystem()?
            .existing_uninstall_manifest()
            .map_err(|_| InstallError::backend_failure())?;

        if current.is_none() {
            return self
                .ensure_filesystem()?
                .recover_absent_uninstall_manifest_staging(system, current_release)
                .map_err(|_| InstallError::backend_failure());
        }

        if let Some(prior) = current
            .as_ref()
            .filter(|manifest| manifest.ownership_manifest_digest() != current_release)
        {
            if prior.system() != system || self.intent == LinuxProductAssetIntent::Repair {
                return Err(InstallError::backend_failure());
            }
            let candidate =
                UninstallManifest::new(system, current_release, self.current_records(Some(prior))?)
                    .map_err(|_| InstallError::backend_failure())?;
            self.ensure_filesystem()?
                .bind_uninstall_manifest(&candidate)
                .map_err(|_| InstallError::backend_failure())?;
            self.ensure_filesystem()?
                .recover_uninstall_manifest_replacement(uninstall_manifest_asset()?, prior)
                .map_err(|_| InstallError::backend_failure())?;
            self.installed_manifest = InstalledManifest::Present(prior.clone());
            return Ok(());
        }

        let prior = self
            .ensure_filesystem()?
            .replacement_uninstall_manifest()
            .map_err(|_| InstallError::backend_failure())?;
        if let Some(prior) = prior {
            let current = current.ok_or_else(InstallError::backend_failure)?;
            if current.system() != system
                || current.ownership_manifest_digest() != current_release
                || prior.system() != system
                || prior.ownership_manifest_digest() == current_release
            {
                return Err(InstallError::backend_failure());
            }
            self.ensure_filesystem()?
                .bind_uninstall_manifest(&current)
                .map_err(|_| InstallError::backend_failure())?;
            self.ensure_filesystem()?
                .recover_uninstall_manifest_replacement(uninstall_manifest_asset()?, &prior)
                .map_err(|_| InstallError::backend_failure())?;
            self.installed_manifest = InstalledManifest::Present(prior);
            return Ok(());
        }

        match current {
            Some(manifest) if manifest.ownership_manifest_digest() != current_release => {
                if manifest.system() != system {
                    return Err(InstallError::backend_failure());
                }
                self.installed_manifest = InstalledManifest::Present(manifest);
                Ok(())
            }
            Some(manifest) if manifest.system() == system => {
                if self.any_replacement_backup()? {
                    return Err(InstallError::backend_failure());
                }
                self.ensure_filesystem()?
                    .bind_uninstall_manifest(&manifest)
                    .map_err(|_| InstallError::backend_failure())?;
                self.remove_verified_asset(uninstall_manifest_asset()?)?;
                self.installed_manifest = InstalledManifest::Absent;
                Ok(())
            }
            Some(_) => Err(InstallError::backend_failure()),
            None => unreachable!("the absent receipt returned before replacement recovery"),
        }
    }

    fn any_replacement_backup(&mut self) -> Result<bool, InstallError> {
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::File)
        {
            if self
                .ensure_filesystem()?
                .replacement_backup_exists(asset)
                .map_err(|_| InstallError::backend_failure())?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn replacement_authority(
        &self,
        asset: LinuxInstallAsset,
        manifest: &UninstallManifest,
    ) -> Result<ReplacementAuthority, InstallError> {
        let (system, current) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        if manifest.system() != system {
            return Err(InstallError::backend_failure());
        }
        let record = manifest
            .assets()
            .iter()
            .find(|record| record.id() == asset.id())
            .filter(|record| record.state() == RecordedAssetState::Created)
            .ok_or_else(InstallError::backend_failure)?;
        match self.intent {
            LinuxProductAssetIntent::Repair => {
                if manifest.ownership_manifest_digest() != current {
                    return Err(InstallError::backend_failure());
                }
                Ok(ReplacementAuthority::RepairExisting)
            }
            LinuxProductAssetIntent::InstallOrUpgrade => {
                if manifest.ownership_manifest_digest() == current {
                    return Err(InstallError::backend_failure());
                }
                Ok(ReplacementAuthority::Upgrade {
                    prior_digest: record
                        .content_digest()
                        .ok_or_else(InstallError::backend_failure)?,
                })
            }
        }
    }

    fn file_replacement(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<Option<ReplacementAuthority>, InstallError> {
        if asset.kind() != crate::LinuxAssetKind::File {
            return Ok(None);
        }
        let Some(manifest) = self.load_installed_manifest()? else {
            return if self.intent == LinuxProductAssetIntent::Repair {
                Err(InstallError::backend_failure())
            } else {
                Ok(None)
            };
        };
        if self.ensure_filesystem()?.verify_asset(asset).is_ok() {
            return Ok(None);
        }
        if self.ensure_filesystem()?.verify_asset_absent(asset).is_ok() {
            return self
                .missing_file_replacement_authority(asset, &manifest)
                .map(Some);
        }
        self.replacement_authority(asset, &manifest).map(Some)
    }

    fn missing_file_replacement_authority(
        &self,
        asset: LinuxInstallAsset,
        manifest: &UninstallManifest,
    ) -> Result<ReplacementAuthority, InstallError> {
        match self.replacement_authority(asset, manifest)? {
            ReplacementAuthority::RepairExisting => Ok(ReplacementAuthority::RepairMissing),
            ReplacementAuthority::Upgrade { .. } | ReplacementAuthority::RepairMissing => {
                Err(InstallError::backend_failure())
            }
        }
    }

    fn current_records(
        &mut self,
        prior: Option<&UninstallManifest>,
    ) -> Result<Vec<RecordedAsset>, InstallError> {
        linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .map(|asset| {
                let prior_state = prior
                    .and_then(|manifest| {
                        manifest
                            .assets()
                            .iter()
                            .find(|record| record.id() == asset.id())
                    })
                    .map(RecordedAsset::state);
                let state = match prior_state {
                    Some(state) => state,
                    None if asset.id() == "nix-root" => RecordedAssetState::PreExisting,
                    None if asset.id() == "uninstall-manifest" => RecordedAssetState::Created,
                    None => *self
                        .states
                        .get(asset.id())
                        .ok_or_else(InstallError::backend_failure)?,
                };
                let mut record = RecordedAsset::new(asset.id(), state)
                    .map_err(|_| InstallError::backend_failure())?;
                if asset.kind() == crate::LinuxAssetKind::File && asset.id() != "uninstall-manifest"
                {
                    let digest = self
                        .ensure_filesystem()?
                        .expected_file_digest(asset)
                        .map_err(|_| InstallError::backend_failure())?;
                    record = record.with_content_digest(digest);
                }
                Ok(record)
            })
            .collect()
    }

    pub(crate) fn finalize_replacement_backups(
        &mut self,
        mut preflight_mutation: impl FnMut() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        let (system, release) = self
            .receipt_binding
            .ok_or_else(InstallError::backend_failure)?;
        let manifest = self
            .load_installed_manifest()?
            .ok_or_else(InstallError::backend_failure)?;
        if manifest.system() != system || manifest.ownership_manifest_digest() != release {
            return Err(InstallError::backend_failure());
        }
        self.ensure_filesystem()?
            .bind_uninstall_manifest(&manifest)
            .map_err(|_| InstallError::backend_failure())?;
        for asset in linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::File)
        {
            preflight_mutation()?;
            self.ensure_filesystem()?
                .finalize_owned_file(asset)
                .map_err(|_| InstallError::backend_failure())?;
        }
        Ok(())
    }
}

fn uninstall_manifest_asset() -> Result<LinuxInstallAsset, InstallError> {
    linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == "uninstall-manifest")
        .ok_or_else(InstallError::backend_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_core::state::body_digest;
    use std::os::unix::fs::PermissionsExt;

    fn manifest(
        system: System,
        release: Digest,
        target: LinuxInstallAsset,
        target_state: RecordedAssetState,
        target_digest: Digest,
    ) -> Result<UninstallManifest, Box<dyn std::error::Error>> {
        let records = crate::assets::linux_product_install_assets()
            .map(|asset| {
                let record = RecordedAsset::new(
                    asset.id(),
                    if asset == target {
                        target_state
                    } else {
                        RecordedAssetState::Created
                    },
                )?;
                Ok::<_, crate::UninstallError>(
                    if asset.kind() == crate::LinuxAssetKind::File
                        && asset.id() != "uninstall-manifest"
                    {
                        record.with_content_digest(if asset == target {
                            target_digest
                        } else {
                            body_digest(asset.id().as_bytes())
                        })
                    } else {
                        record
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UninstallManifest::new(system, release, records)?)
    }

    fn asset(id: &str) -> LinuxInstallAsset {
        linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == id)
            .unwrap_or_else(|| unreachable!("test asset is in the closed set"))
    }

    fn restart_fixture_filesystem(
        fixture: &mut ExistingNonFilePreflightAssets,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let payloads = fixture
            .manager
            .payloads
            .clone()
            .ok_or_else(|| std::io::Error::other("test payloads are absent"))?;
        let mut filesystem = LinuxFilesystemManager::for_existing_preflight_test(
            fixture.temporary.path().to_path_buf(),
            payloads,
        );
        filesystem.bind_config_bytes_for_test(b"test-nix-config");
        fixture.manager.filesystem = Some(filesystem);
        fixture.manager.installed_manifest = InstalledManifest::Unloaded;
        Ok(())
    }

    fn assert_exact_prior_receipt_recovered(
        fixture: &mut ExistingNonFilePreflightAssets,
        prior: &UninstallManifest,
        candidate: &UninstallManifest,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let receipt_asset = uninstall_manifest_asset()?;
        assert_eq!(
            fixture
                .manager
                .ensure_filesystem()?
                .existing_uninstall_manifest()?,
            Some(prior.clone())
        );
        assert_eq!(
            fixture.manager.load_installed_manifest()?,
            Some(prior.clone())
        );
        assert!(
            !fixture
                .manager
                .ensure_filesystem()?
                .replacement_backup_exists(receipt_asset)?
        );
        fixture
            .manager
            .ensure_filesystem()?
            .bind_uninstall_manifest(prior)?;
        assert!(
            fixture
                .manager
                .ensure_filesystem()?
                .bind_uninstall_manifest(candidate)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn exact_release_classification_accepts_only_same_undrifted_assets()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let release = Digest::from_bytes([0xa1; 32]);
        let groups = ManagedGroupBindings::new(30_000, 30_001)?;

        let mut exact = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            groups, system, release, "none",
        )?;
        assert!(exact.manager.classify_exact_release()?);

        let mut different = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            groups, system, release, "none",
        )?;
        different.manager.receipt_binding = Some((system, Digest::from_bytes([0xa2; 32])));
        assert!(!different.manager.classify_exact_release()?);

        let mut drifted = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            groups, system, release, "none",
        )?;
        std::fs::write(
            drifted.temporary.path().join("usr/local/bin/pkg"),
            b"changed",
        )?;
        assert!(drifted.manager.classify_exact_release().is_err());
        Ok(())
    }

    #[test]
    fn offline_upgrade_publishes_and_finalizes_the_candidate_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let prior = Digest::from_bytes([0xb1; 32]);
        let candidate = Digest::from_bytes([0xb2; 32]);
        let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            ManagedGroupBindings::new(30_000, 30_001)?,
            system,
            prior,
            "none",
        )?;
        fixture.manager.receipt_binding = Some((system, candidate));

        fixture.manager.preflight_existing_non_files()?;
        assert!(fixture.manager.publish_uninstall_manifest()?);
        let receipt_asset = uninstall_manifest_asset()?;
        fixture.manager.finalize_replacement_backups(|| Ok(()))?;

        let receipt = fixture
            .manager
            .ensure_filesystem()?
            .existing_uninstall_manifest()?
            .ok_or_else(|| std::io::Error::other("candidate receipt is absent"))?;
        assert_eq!(receipt.ownership_manifest_digest(), candidate);
        assert!(
            !fixture
                .manager
                .ensure_filesystem()?
                .replacement_backup_exists(receipt_asset)?
        );
        Ok(())
    }

    #[test]
    fn failed_offline_upgrade_rollback_restores_the_exact_prior_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let prior = Digest::from_bytes([0xc1; 32]);
        let candidate = Digest::from_bytes([0xc2; 32]);
        let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            ManagedGroupBindings::new(30_000, 30_001)?,
            system,
            prior,
            "none",
        )?;
        fixture.manager.receipt_binding = Some((system, candidate));

        fixture.manager.preflight_existing_non_files()?;
        let prior_receipt = fixture
            .manager
            .load_installed_manifest()?
            .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
        assert!(fixture.manager.publish_uninstall_manifest()?);
        let receipt_asset = uninstall_manifest_asset()?;
        fixture.manager.rollback_asset(receipt_asset)?;

        let receipt = fixture
            .manager
            .ensure_filesystem()?
            .existing_uninstall_manifest()?
            .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
        assert_eq!(receipt.ownership_manifest_digest(), prior);
        assert!(
            !fixture
                .manager
                .ensure_filesystem()?
                .replacement_backup_exists(receipt_asset)?
        );
        fixture
            .manager
            .ensure_filesystem()?
            .bind_uninstall_manifest(&prior_receipt)?;
        assert_eq!(
            fixture.manager.load_installed_manifest()?,
            Some(prior_receipt)
        );
        Ok(())
    }

    #[test]
    fn recovery_before_receipt_exchange_restores_the_prior_disk_cache_and_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let prior_release = Digest::from_bytes([0xd1; 32]);
        let candidate_release = Digest::from_bytes([0xd2; 32]);
        let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            ManagedGroupBindings::new(30_000, 30_001)?,
            system,
            prior_release,
            "none",
        )?;
        fixture.manager.receipt_binding = Some((system, candidate_release));
        fixture.manager.preflight_existing_non_files()?;
        let prior = fixture
            .manager
            .load_installed_manifest()?
            .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
        let candidate = UninstallManifest::new(
            system,
            candidate_release,
            fixture.manager.current_records(Some(&prior))?,
        )?;
        let staging = fixture
            .temporary
            .path()
            .join("opt/pkg/uninstall/.pkg-install-rollback-uninstall-manifest");
        std::fs::write(&staging, crate::encode_uninstall_manifest(&candidate)?)?;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))?;

        restart_fixture_filesystem(&mut fixture)?;
        fixture.manager.recover_asset(uninstall_manifest_asset()?)?;

        assert_exact_prior_receipt_recovered(&mut fixture, &prior, &candidate)
    }

    #[test]
    fn recovery_after_receipt_exchange_restores_the_prior_disk_cache_and_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let prior_release = Digest::from_bytes([0xe1; 32]);
        let candidate_release = Digest::from_bytes([0xe2; 32]);
        let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            ManagedGroupBindings::new(30_000, 30_001)?,
            system,
            prior_release,
            "none",
        )?;
        fixture.manager.receipt_binding = Some((system, candidate_release));
        fixture.manager.preflight_existing_non_files()?;
        let prior = fixture
            .manager
            .load_installed_manifest()?
            .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
        assert!(fixture.manager.publish_uninstall_manifest()?);
        let candidate = fixture
            .manager
            .load_installed_manifest()?
            .ok_or_else(|| std::io::Error::other("candidate receipt is absent"))?;

        restart_fixture_filesystem(&mut fixture)?;
        fixture.manager.recover_asset(uninstall_manifest_asset()?)?;

        assert_exact_prior_receipt_recovered(&mut fixture, &prior, &candidate)
    }

    fn manager_before_broker(
        groups: ManagedGroupBindings,
        payloads: LinuxReleasePayloads,
        reader: LinuxReceiptReader,
        fail_account_read: bool,
    ) -> (
        LinuxPlatformAssetManager,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let (accounts, mutation_calls) =
            LinuxAccountManager::for_fresh_preflight_test(groups, usize::from(fail_account_read));
        (
            LinuxPlatformAssetManager {
                groups,
                accounts,
                filesystem: None,
                payloads: Some(payloads),
                config: None,
                authenticated_config_system_for_test: None,
                receipt_binding: None,
                intent: LinuxProductAssetIntent::InstallOrUpgrade,
                installed_manifest: InstalledManifest::Unloaded,
                states: BTreeMap::new(),
                pre_broker_receipt_reader: Some(reader),
            },
            mutation_calls,
        )
    }

    #[test]
    fn fresh_receipt_preflight_does_not_require_the_broker_uid()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let groups = ManagedGroupBindings::new(30_000, 30_001)?;
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
        let reader =
            LinuxReceiptReader::for_test(temporary.path().to_path_buf(), groups, payloads.clone());
        let (mut manager, mutation_calls) = manager_before_broker(groups, payloads, reader, false);

        assert!(manager.broker_uid().is_err());
        assert!(manager.ensure_asset(asset("broker-group"))?);
        assert_eq!(mutation_calls.get(), 1);
        assert!(matches!(
            manager.installed_manifest,
            InstalledManifest::Absent
        ));
        Ok(())
    }

    #[test]
    fn account_read_error_is_not_consumed_by_receipt_discovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let groups = ManagedGroupBindings::new(30_000, 30_001)?;
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
        let reader =
            LinuxReceiptReader::for_test(temporary.path().to_path_buf(), groups, payloads.clone());
        let (mut manager, mutation_calls) = manager_before_broker(groups, payloads, reader, true);

        assert!(manager.ensure_asset(asset("broker-group")).is_err());
        assert_eq!(mutation_calls.get(), 0);
        Ok(())
    }

    #[test]
    fn unsafe_pre_broker_receipts_refuse_before_account_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let groups = ManagedGroupBindings::new(30_000, 30_001)?;
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
        let mut fixtures = Vec::new();

        let unsafe_mode = tempfile::tempdir()?;
        std::fs::set_permissions(unsafe_mode.path(), std::fs::Permissions::from_mode(0o777))?;
        fixtures.push((
            unsafe_mode,
            None,
            nix::unistd::Uid::effective().as_raw(),
            nix::unistd::Gid::effective().as_raw(),
        ));

        let symlinked = tempfile::tempdir()?;
        symlink("/", symlinked.path().join("opt"))?;
        fixtures.push((
            symlinked,
            None,
            nix::unistd::Uid::effective().as_raw(),
            nix::unistd::Gid::effective().as_raw(),
        ));

        let wrong_owner = tempfile::tempdir()?;
        fixtures.push((
            wrong_owner,
            None,
            nix::unistd::Uid::effective().as_raw().saturating_add(1),
            nix::unistd::Gid::effective().as_raw(),
        ));

        let noncanonical = tempfile::tempdir()?;
        std::fs::create_dir(noncanonical.path().join("opt"))?;
        let mut filesystem = LinuxFilesystemManager::for_existing_preflight_test(
            noncanonical.path().to_path_buf(),
            payloads.clone(),
        );
        for directory in crate::assets::linux_product_install_assets()
            .filter(|asset| asset.kind() == crate::LinuxAssetKind::Directory)
            .filter(|asset| asset.path_or_name().starts_with("/opt/pkg"))
        {
            filesystem.ensure_asset(directory)?;
        }
        std::fs::write(
            noncanonical.path().join("opt/pkg/uninstall/manifest.json"),
            b"not canonical json",
        )?;
        std::fs::set_permissions(
            noncanonical.path().join("opt/pkg/uninstall/manifest.json"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        fixtures.push((
            noncanonical,
            Some(filesystem),
            nix::unistd::Uid::effective().as_raw(),
            nix::unistd::Gid::effective().as_raw(),
        ));

        for (temporary, filesystem, root_uid, root_gid) in fixtures {
            drop(filesystem);
            let reader = LinuxReceiptReader::for_test_with_owner(
                temporary.path().to_path_buf(),
                (root_uid, root_gid),
                groups,
                payloads.clone(),
            );
            let (mut manager, mutation_calls) =
                manager_before_broker(groups, payloads.clone(), reader, false);
            assert!(manager.ensure_asset(asset("broker-group")).is_err());
            assert_eq!(mutation_calls.get(), 0);
        }
        Ok(())
    }

    #[test]
    fn ordinary_upgrade_requires_different_release_and_prior_content_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let prior_release = Digest::from_bytes([1; 32]);
        let candidate_release = Digest::from_bytes([2; 32]);
        let prior_content = Digest::from_bytes([3; 32]);
        let target = asset("broker-service-unit");
        let receipt = manifest(
            system,
            prior_release,
            target,
            RecordedAssetState::Created,
            prior_content,
        )?;
        let mut manager = LinuxPlatformAssetManager::new(ManagedGroupBindings::new(100, 101)?);
        manager.bind_authenticated_release_identity(system, candidate_release)?;

        assert_eq!(
            manager.replacement_authority(target, &receipt)?,
            ReplacementAuthority::Upgrade {
                prior_digest: prior_content,
            }
        );

        manager.bind_authenticated_release_identity(system, candidate_release)?;
        let same_release = manifest(
            system,
            candidate_release,
            target,
            RecordedAssetState::Created,
            prior_content,
        )?;
        assert!(
            manager
                .replacement_authority(target, &same_release)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn repair_requires_same_release_and_created_product_ownership()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let candidate_release = Digest::from_bytes([2; 32]);
        let target = asset("broker-service-unit");
        let mut manager = LinuxPlatformAssetManager::with_intent(
            ManagedGroupBindings::new(100, 101)?,
            LinuxProductAssetIntent::Repair,
        );
        manager.bind_authenticated_release_identity(system, candidate_release)?;
        let owned = manifest(
            system,
            candidate_release,
            target,
            RecordedAssetState::Created,
            Digest::from_bytes([3; 32]),
        )?;
        assert_eq!(
            manager.replacement_authority(target, &owned)?,
            ReplacementAuthority::RepairExisting
        );
        assert_eq!(
            manager.missing_file_replacement_authority(target, &owned)?,
            ReplacementAuthority::RepairMissing
        );

        let prior_release = manifest(
            system,
            Digest::from_bytes([1; 32]),
            target,
            RecordedAssetState::Created,
            Digest::from_bytes([3; 32]),
        )?;
        assert!(
            manager
                .replacement_authority(target, &prior_release)
                .is_err()
        );

        let preexisting = manifest(
            system,
            candidate_release,
            target,
            RecordedAssetState::PreExisting,
            Digest::from_bytes([3; 32]),
        )?;
        assert!(manager.replacement_authority(target, &preexisting).is_err());
        assert!(
            manager
                .missing_file_replacement_authority(target, &preexisting)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn repair_requires_a_receipt_and_non_files_never_gain_implicit_ownership()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let release = Digest::from_bytes([0x41; 32]);
        let account = asset("broker-user");
        let directory = asset("nix-root");
        let preexisting_account = manifest(
            system,
            release,
            account,
            RecordedAssetState::PreExisting,
            Digest::from_bytes([0x42; 32]),
        )?;
        let preexisting_directory = manifest(
            system,
            release,
            directory,
            RecordedAssetState::PreExisting,
            Digest::from_bytes([0x43; 32]),
        )?;
        let created_account = manifest(
            system,
            release,
            account,
            RecordedAssetState::Created,
            Digest::from_bytes([0x44; 32]),
        )?;
        let ordinary = LinuxPlatformAssetManager::new(ManagedGroupBindings::new(100, 101)?);
        assert!(ordinary.non_file_requires_exact(account, Some(&preexisting_account))?);
        assert!(ordinary.non_file_requires_exact(directory, Some(&preexisting_directory))?);

        let mut repair = LinuxPlatformAssetManager::with_intent(
            ManagedGroupBindings::new(100, 101)?,
            LinuxProductAssetIntent::Repair,
        );
        repair.installed_manifest = InstalledManifest::Absent;
        assert!(repair.non_file_requires_exact(account, Some(&created_account))?);
        assert!(repair.non_file_requires_exact(account, None).is_err());
        assert!(repair.file_replacement(asset("broker-binary")).is_err());
        Ok(())
    }
}
