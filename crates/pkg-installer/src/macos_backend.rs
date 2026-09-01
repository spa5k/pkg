//! Production macOS installer backend.

use std::{io::ErrorKind, path::Path};

use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
    RealNixAdapter, observe_build_accounts,
};
use sha2::{Digest as _, Sha256};

use crate::{
    AssetPresence, InstallMode, MacOsBuildReadiness, MacOsBuildUsersReadiness, MacOsError,
    MacOsInstallAsset, MacOsInstallBackend, MacOsSandboxReadiness, MacOsToolchainReadiness,
    UninstallManifest,
    determinate_handoff::{DeterminateHandoff, DeterminateHandoffState},
    macos_launchd::MacOsLaunchdManager,
    macos_platform_assets::MacOsPlatformAssetManager,
    macos_product_install_assets,
};

const BROKER_HOME: &str = "/Library/Application Support/pkg/broker-home";
const CODESIGN: &str = "/usr/bin/codesign";
const XCRUN: &str = "/usr/bin/xcrun";

fn select_install_mode(
    handoff: DeterminateHandoffState,
    installed: Option<(System, Digest)>,
    requested_repair: bool,
    system: System,
    release_identity: Digest,
) -> Result<InstallMode, MacOsError> {
    match (handoff, installed, requested_repair) {
        (DeterminateHandoffState::NotStarted, None, false) => Ok(InstallMode::FreshInstall),
        (DeterminateHandoffState::Accepted, Some((installed_system, installed_release)), false)
            if installed_system == system && installed_release == release_identity =>
        {
            Ok(InstallMode::FreshInstall)
        }
        (DeterminateHandoffState::Accepted, Some((installed_system, _)), false)
            if installed_system == system =>
        {
            Ok(InstallMode::OfflineUpgrade)
        }
        (DeterminateHandoffState::Accepted, Some((installed_system, installed_release)), true)
            if installed_system == system && installed_release == release_identity =>
        {
            Ok(InstallMode::OfflineRepair)
        }
        _ => Err(MacOsError::backend_failure()),
    }
}

/// The production macOS install backend: asset, service, and journal state
/// for one attempt, with every preflight classification as a named flag.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each boolean mirrors one documented macOS preflight classification"
)]
pub struct ProductionMacOsInstallBackend {
    system: System,
    assets: MacOsPlatformAssetManager,
    services: MacOsLaunchdManager,
    release_identity: Option<Digest>,
    config: Option<AuthenticatedManagedNixConfig>,
    existing_managed_install: bool,
    authenticated_recovery: bool,
    store_created: bool,
    requested_repair: bool,
    mode: InstallMode,
    prior_manifest: Option<UninstallManifest>,
}

impl ProductionMacOsInstallBackend {
    /// Creates the fixed preview backend for one native Darwin system.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Darwin system or invalid fixed group bindings.
    pub fn new(system: System, groups: ManagedGroupBindings) -> Result<Self, MacOsError> {
        if system != System::Aarch64Darwin {
            return Err(MacOsError::backend_failure());
        }
        Ok(Self {
            system,
            assets: MacOsPlatformAssetManager::new(groups)?,
            services: MacOsLaunchdManager::new(),
            release_identity: None,
            config: None,
            existing_managed_install: false,
            authenticated_recovery: false,
            store_created: false,
            requested_repair: false,
            mode: InstallMode::FreshInstall,
            prior_manifest: None,
        })
    }

    /// Creates the fixed backend for an explicit same-release repair.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for an unsupported system or invalid groups.
    pub fn new_product_repair(
        system: System,
        groups: ManagedGroupBindings,
    ) -> Result<Self, MacOsError> {
        let mut backend = Self::new(system, groups)?;
        backend.requested_repair = true;
        Ok(backend)
    }

    /// The classified install mode for this attempt.
    #[must_use]
    pub const fn install_mode(&self) -> InstallMode {
        self.mode
    }

    fn verify_service_assets(&mut self) -> Result<(), MacOsError> {
        for asset in macos_product_install_assets()
            .filter(|asset| matches!(asset.id(), "helper-plist" | "broker-plist"))
        {
            if self.assets.classify_asset(asset)? != AssetPresence::ExactPresent {
                return Err(MacOsError::backend_failure());
            }
        }
        Ok(())
    }

    fn installed_product_manifest(&mut self) -> Result<Option<UninstallManifest>, MacOsError> {
        let receipt = macos_product_install_assets()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(MacOsError::backend_failure)?;
        match std::fs::symlink_metadata(receipt.path_or_name()) {
            Ok(_) => self.assets.installed_uninstall_manifest(),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(MacOsError::backend_failure()),
        }
    }

    fn bind_prior_manifest(
        &mut self,
        manifest: Option<UninstallManifest>,
    ) -> Result<(), MacOsError> {
        if let Some(manifest) = manifest.as_ref() {
            self.assets.bind_uninstall_manifest(manifest)?;
            self.assets.bind_prior_asset_states(manifest)?;
        }
        self.prior_manifest = manifest;
        Ok(())
    }

    fn classify_preview_presence(
        &self,
        presence: AssetPresence,
    ) -> Result<AssetPresence, MacOsError> {
        if !self.existing_managed_install && presence == AssetPresence::ExactPresent {
            Err(MacOsError::backend_failure())
        } else {
            Ok(presence)
        }
    }

    fn classify_asset_presence(
        &self,
        asset: MacOsInstallAsset,
        presence: AssetPresence,
    ) -> Result<AssetPresence, MacOsError> {
        if self.store_created && asset.id() == "nix-root" {
            return (presence == AssetPresence::ExactPresent)
                .then_some(presence)
                .ok_or_else(MacOsError::backend_failure);
        }
        self.classify_preview_presence(presence)
    }
}

impl MacOsInstallBackend for ProductionMacOsInstallBackend {
    fn install_mode(&self) -> InstallMode {
        self.mode
    }

    fn preflight_product_mutation(&mut self) -> Result<(), MacOsError> {
        if self.mode == InstallMode::FreshInstall {
            Ok(())
        } else {
            MacOsLaunchdManager::require_offline()
        }
    }
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

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        release_identity_digest: Digest,
    ) -> Result<(), MacOsError> {
        if system != self.system
            || self
                .release_identity
                .is_some_and(|bound| bound != release_identity_digest)
        {
            return Err(MacOsError::backend_failure());
        }
        self.assets
            .bind_authenticated_ownership(system, release_identity_digest)?;
        self.release_identity = Some(release_identity_digest);
        Ok(())
    }

    fn begin_authenticated_recovery(&mut self, mode: InstallMode) -> Result<(), MacOsError> {
        if self.release_identity.is_none() || !self.assets.authenticated_inputs_bound(self.system) {
            return Err(MacOsError::backend_failure());
        }
        if (mode == InstallMode::OfflineRepair) != self.requested_repair {
            return Err(MacOsError::backend_failure());
        }
        self.mode = mode;
        if mode != InstallMode::FreshInstall {
            MacOsLaunchdManager::require_offline()?;
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
        let handoff = DeterminateHandoff::production()
            .and_then(|handoff| handoff.state())
            .map_err(|_| MacOsError::backend_failure())?;
        if handoff == DeterminateHandoffState::Started {
            return Err(MacOsError::backend_failure());
        }
        if self.authenticated_recovery {
            if self.mode != InstallMode::FreshInstall {
                if handoff != DeterminateHandoffState::Accepted {
                    return Err(MacOsError::backend_failure());
                }
                MacOsLaunchdManager::require_offline()?;
            }
            let installed = self.installed_product_manifest()?;
            if self.mode != InstallMode::FreshInstall && installed.is_none() {
                return Err(MacOsError::backend_failure());
            }
            self.bind_prior_manifest(installed)?;
            self.existing_managed_install = handoff == DeterminateHandoffState::Accepted;
            return Ok(());
        }
        let installed = self.installed_product_manifest()?;
        self.mode = select_install_mode(
            handoff,
            installed
                .as_ref()
                .map(|manifest| (manifest.system(), manifest.ownership_manifest_digest())),
            self.requested_repair,
            system,
            self.release_identity
                .ok_or_else(MacOsError::backend_failure)?,
        )?;
        if self.mode != InstallMode::FreshInstall {
            MacOsLaunchdManager::require_offline()?;
        }
        self.bind_prior_manifest(installed)?;
        self.existing_managed_install = handoff == DeterminateHandoffState::Accepted;
        Ok(())
    }

    fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.assets.broker_uid()
    }

    fn classify_asset(&mut self, asset: MacOsInstallAsset) -> Result<AssetPresence, MacOsError> {
        match self.assets.classify_asset(asset) {
            Ok(presence) => self.classify_asset_presence(asset, presence),
            Err(_)
                if self.mode != InstallMode::FreshInstall
                    && asset.kind() == crate::MacOsAssetKind::File =>
            {
                self.assets.verify_repair_target(asset)?;
                Ok(AssetPresence::ExactPresent)
            }
            Err(error) => Err(error),
        }
    }

    fn classify_store_volume(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::ExactPresent)
    }

    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, MacOsError> {
        match DeterminateHandoff::production()
            .and_then(|handoff| handoff.state())
            .map_err(|_| MacOsError::backend_failure())?
        {
            DeterminateHandoffState::Accepted => Ok(AssetPresence::ExactPresent),
            DeterminateHandoffState::NotStarted => Ok(AssetPresence::Absent),
            DeterminateHandoffState::Started => Err(MacOsError::backend_failure()),
        }
    }

    fn classify_services(&mut self) -> Result<AssetPresence, MacOsError> {
        if self.mode != InstallMode::FreshInstall {
            MacOsLaunchdManager::require_offline()?;
            return Ok(AssetPresence::ExactPresent);
        }
        let presence = MacOsLaunchdManager::classify_activation().map(|active| {
            if active {
                AssetPresence::ExactPresent
            } else {
                AssetPresence::Absent
            }
        })?;
        self.classify_preview_presence(presence)
    }

    fn classify_ownership_receipt(&mut self) -> Result<AssetPresence, MacOsError> {
        let presence = if self.mode == InstallMode::FreshInstall {
            self.assets.classify_uninstall_manifest()?
        } else if self.assets.installed_uninstall_manifest()?.is_some() {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
        };
        self.classify_preview_presence(presence)
    }

    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if store_volume_owns_rollback(asset) {
            return (self.assets.classify_asset(asset)? == AssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if asset.id() == "broker-channel-state" {
            return self.assets.remove_uninstall_asset(asset);
        }
        self.assets.remove_verified_asset(asset)
    }

    fn recover_services(&mut self) -> Result<(), MacOsError> {
        self.verify_service_assets()?;
        MacOsLaunchdManager::deactivate_verified()
    }

    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        if self.mode == InstallMode::OfflineUpgrade {
            self.assets.rollback_uninstall_manifest_replacement()
        } else {
            self.assets.recover_uninstall_manifest()
        }
    }

    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        if self.assets.authenticated_inputs_bound(self.system) {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        if asset.id() == "nix-root" {
            if self.assets.classify_asset(asset)? != AssetPresence::ExactPresent {
                return Err(MacOsError::backend_failure());
            }
            self.assets.record_preexisting(asset);
            return Ok(false);
        }
        let created = if self.mode != InstallMode::FreshInstall
            && asset.kind() == crate::MacOsAssetKind::File
        {
            if self.assets.classify_asset(asset) == Ok(AssetPresence::Absent) {
                self.assets.ensure_asset(asset)?
            } else {
                self.assets.replace_owned_file(
                    asset,
                    self.repair_replacement_digest(asset)?,
                    self.mode == InstallMode::OfflineRepair,
                )?
            }
        } else {
            self.assets.ensure_asset(asset)?
        };
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
        if self.mode == InstallMode::FreshInstall
            || self.assets.classify_asset(asset) == Ok(AssetPresence::Absent)
        {
            self.assets.install_static_asset(asset, contents)
        } else {
            let prior = self.repair_replacement_digest(asset)?;
            self.assets.replace_static_owned_file(
                asset,
                contents,
                prior,
                self.mode == InstallMode::OfflineRepair,
            )
        }
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

    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
        // A fresh install that just accepted the vendor Base Nix handoff now
        // manages an existing vendor-created /nix. Without this flag the
        // post-acceptance nix-root classification rejects the vendor-created
        // directory as a preexisting asset, because this run did not provision
        // a store volume itself.
        self.existing_managed_install = true;
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
        if self.mode == InstallMode::FreshInstall {
            self.services.activate()
        } else {
            MacOsLaunchdManager::require_offline()?;
            Ok(false)
        }
    }

    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        if self.mode == InstallMode::FreshInstall {
            self.services.rollback()
        } else {
            MacOsLaunchdManager::require_offline()
        }
    }

    fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
        let adapter = RealNixAdapter::new_standard_determinate(Path::new(BROKER_HOME))
            .map_err(|_| MacOsError::backend_failure())?;
        adapter
            .wait_for_managed_store()
            .map_err(|_| MacOsError::backend_failure())
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
        } else if self.mode == InstallMode::OfflineRepair
            && asset.kind() == crate::MacOsAssetKind::File
        {
            self.assets.roll_forward_owned_file(asset)
        } else {
            self.assets.rollback_asset(asset)
        }
    }

    fn prior_file_digest(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        self.prior_digest(asset)
    }

    fn recover_replaced_asset(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.assets.recover_owned_file(asset, prior_digest)
    }

    fn roll_forward_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.assets.roll_forward_owned_file(asset)
    }

    fn finalize_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if self.mode == InstallMode::FreshInstall {
            Ok(())
        } else {
            self.preflight_product_mutation()?;
            self.assets.finalize_owned_file(asset)
        }
    }

    fn prior_ownership_receipt_digest(&mut self) -> Result<Option<Digest>, MacOsError> {
        let Some(manifest) = self.prior_manifest.as_ref() else {
            return Ok(None);
        };
        let bytes = crate::encode_uninstall_manifest(manifest)
            .map_err(|_| MacOsError::backend_failure())?;
        Ok(Some(Digest::from_bytes(Sha256::digest(bytes).into())))
    }

    fn recover_replaced_ownership_receipt(
        &mut self,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        if self.prior_manifest.is_none() {
            return self
                .assets
                .recover_uninstall_manifest_replacement_by_digest(
                    self.system,
                    self.release_identity
                        .ok_or_else(MacOsError::backend_failure)?,
                    prior_digest,
                );
        }
        if self.prior_ownership_receipt_digest()? != Some(prior_digest) {
            return Err(MacOsError::backend_failure());
        }
        let prior = self
            .prior_manifest
            .clone()
            .ok_or_else(MacOsError::backend_failure)?;
        self.assets.recover_uninstall_manifest_replacement(&prior)
    }

    fn roll_forward_replaced_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        (self.assets.classify_uninstall_manifest()? == AssetPresence::ExactPresent)
            .then_some(())
            .ok_or_else(MacOsError::backend_failure)
    }

    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        Ok(false)
    }
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
}

impl ProductionMacOsInstallBackend {
    /// Linux `ReplacementAuthority::RepairExisting` parity: an explicit repair
    /// replaces a metadata-safe damaged file without requiring its current
    /// bytes to match any recorded digest. The caller still proves product
    /// ownership through the exact ownership receipt before this is reached.
    fn repair_replacement_digest(
        &self,
        asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        if self.mode == InstallMode::OfflineRepair {
            return Ok(None);
        }
        self.prior_digest(asset)
    }

    fn prior_digest(&self, asset: MacOsInstallAsset) -> Result<Option<Digest>, MacOsError> {
        let digest = self
            .prior_manifest
            .as_ref()
            .and_then(|manifest| {
                manifest
                    .assets()
                    .iter()
                    .find(|record| record.id() == asset.id())
            })
            .and_then(crate::RecordedAsset::content_digest);
        if self.mode == InstallMode::OfflineUpgrade && digest.is_none() {
            return Err(MacOsError::backend_failure());
        }
        Ok(digest)
    }
}

fn store_volume_owns_rollback(asset: MacOsInstallAsset) -> bool {
    asset.id() == "nix-root"
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
    fn install_mode_requires_an_exact_accepted_handoff_and_receipt() {
        let system = System::Aarch64Darwin;
        let current = Digest::from_bytes([1; 32]);
        let prior = Digest::from_bytes([2; 32]);
        assert_eq!(
            select_install_mode(
                DeterminateHandoffState::NotStarted,
                None,
                false,
                system,
                current,
            ),
            Ok(InstallMode::FreshInstall)
        );
        assert_eq!(
            select_install_mode(
                DeterminateHandoffState::Accepted,
                Some((system, current)),
                false,
                system,
                current,
            ),
            Ok(InstallMode::FreshInstall)
        );
        assert_eq!(
            select_install_mode(
                DeterminateHandoffState::Accepted,
                Some((system, prior)),
                false,
                system,
                current,
            ),
            Ok(InstallMode::OfflineUpgrade)
        );
        assert_eq!(
            select_install_mode(
                DeterminateHandoffState::Accepted,
                Some((system, current)),
                true,
                system,
                current,
            ),
            Ok(InstallMode::OfflineRepair)
        );
        for refused in [
            select_install_mode(
                DeterminateHandoffState::Started,
                None,
                false,
                system,
                current,
            ),
            select_install_mode(
                DeterminateHandoffState::NotStarted,
                None,
                true,
                system,
                current,
            ),
            select_install_mode(
                DeterminateHandoffState::Accepted,
                None,
                false,
                system,
                current,
            ),
            select_install_mode(
                DeterminateHandoffState::Accepted,
                Some((system, prior)),
                true,
                system,
                current,
            ),
            select_install_mode(
                DeterminateHandoffState::Accepted,
                Some((System::Aarch64Linux, current)),
                false,
                system,
                current,
            ),
        ] {
            assert!(refused.is_err());
        }
    }

    #[test]
    fn clean_preview_refuses_preexisting_assets() -> Result<(), Box<dyn std::error::Error>> {
        let groups = ManagedGroupBindings::new(333, 350)?;
        let mut backend = ProductionMacOsInstallBackend::new(System::Aarch64Darwin, groups)?;
        assert!(
            backend
                .classify_preview_presence(AssetPresence::ExactPresent)
                .is_err()
        );
        let nix_root = macos_product_install_assets()
            .find(|asset| asset.id() == "nix-root")
            .ok_or("missing nix-root asset")?;
        assert!(store_volume_owns_rollback(nix_root));
        backend.store_created = true;
        assert_eq!(
            backend.classify_asset_presence(nix_root, AssetPresence::ExactPresent),
            Ok(AssetPresence::ExactPresent)
        );
        assert!(
            backend
                .classify_asset_presence(nix_root, AssetPresence::Absent)
                .is_err()
        );
        backend.store_created = false;
        backend.existing_managed_install = true;
        assert_eq!(
            backend.classify_preview_presence(AssetPresence::ExactPresent),
            Ok(AssetPresence::ExactPresent)
        );
        Ok(())
    }

    #[test]
    fn accepted_handoff_allows_the_vendor_created_nix_root()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::MacOsInstallBackend;

        let groups = ManagedGroupBindings::new(333, 350)?;
        let mut backend = ProductionMacOsInstallBackend::new(System::Aarch64Darwin, groups)?;
        let nix_root = macos_product_install_assets()
            .find(|asset| asset.id() == "nix-root")
            .ok_or("missing nix-root asset")?;
        assert!(
            backend
                .classify_asset_presence(nix_root, AssetPresence::ExactPresent)
                .is_err()
        );
        backend.accept_base_nix_handoff()?;
        assert_eq!(
            backend.classify_asset_presence(nix_root, AssetPresence::ExactPresent),
            Ok(AssetPresence::ExactPresent)
        );
        Ok(())
    }
}
