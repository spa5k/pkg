//! Production macOS uninstall binding.

use std::{env, path::Path};

use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedRuntimeRemoval,
    ManagedRuntimeRemovalOutcome, OwnershipExpectation, RootNixGcExecutor,
    prepare_managed_runtime_removal, prepare_managed_runtime_removal_without_receipt,
    verify_authenticated_managed_install,
};
use sha2::{Digest as _, Sha256};

use crate::{
    MacOsAssetKind, MacOsAssetPresence, MacOsInstallAsset, MacOsInstallJournal,
    MacOsInstallJournalStorage, RecordedAsset, RecordedAssetState, UninstallAction,
    UninstallAssetKind, UninstallBackend, UninstallError, UninstallManifest,
    linux_user_cleanup::LinuxUserCleanup, macos_install_assets, macos_launchd::MacOsLaunchdManager,
    macos_platform_assets::MacOsPlatformAssetManager,
};

const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
const HELPER_HOME: &str = "/Library/Application Support/pkg/helper-home";
const MANAGED_RUNTIME_ROOT: &str = "/opt/pkg/nix";
const UNINSTALL_RECEIPT: &str = "/opt/pkg/uninstall/manifest.json";

#[allow(clippy::struct_excessive_bools)]
pub struct ProductionMacOsUninstallBackend {
    system: System,
    expectation: OwnershipExpectation,
    assets: MacOsPlatformAssetManager,
    user_cleanup: LinuxUserCleanup,
    gc: Option<RootNixGcExecutor>,
    runtime_removal: Option<ManagedRuntimeRemoval>,
    manifest: Option<UninstallManifest>,
    preserve_nix: Option<bool>,
    user_roots_removed: bool,
    store_preserved: bool,
    services_stopped: bool,
    recovery_mode: bool,
    recovery_context_digest: Option<Digest>,
    uninstall_journal: Option<MacOsInstallJournalStorage>,
}

impl ProductionMacOsUninstallBackend {
    /// Binds authenticated release inputs to the fixed macOS uninstall backend.
    ///
    /// # Errors
    ///
    /// Returns a stable refusal for mismatched systems or invalid fixed bindings.
    pub fn new(
        config: &AuthenticatedManagedNixConfig,
        expectation: &OwnershipExpectation,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<Self, UninstallError> {
        let system = expectation.system();
        if !matches!(system, System::X8664Darwin | System::Aarch64Darwin)
            || config.system() != system
            || payloads.system() != system
        {
            return Err(UninstallError::backend_failure());
        }
        let mut assets = MacOsPlatformAssetManager::new(expectation.groups())
            .map_err(|_| UninstallError::backend_failure())?;
        assets
            .bind_authenticated_installer_payloads(payloads)
            .and_then(|()| assets.bind_authenticated_nix_config(config))
            .and_then(|()| {
                assets.bind_authenticated_ownership(
                    expectation.system(),
                    expectation.asset_manifest_digest(),
                )
            })
            .map_err(|_| UninstallError::backend_failure())?;
        Ok(Self {
            system,
            expectation: expectation.clone(),
            assets,
            user_cleanup: LinuxUserCleanup::production(),
            gc: None,
            runtime_removal: None,
            manifest: None,
            preserve_nix: None,
            user_roots_removed: false,
            store_preserved: false,
            services_stopped: false,
            recovery_mode: false,
            recovery_context_digest: None,
            uninstall_journal: None,
        })
    }

    /// Loads the exact receipt or its authenticated empty recovery marker.
    ///
    /// # Errors
    ///
    /// Returns a stable refusal for unsafe, changed, ambiguous, or unbound state.
    pub fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError> {
        let broker = crate::macos_accounts::broker_account_presence(self.expectation.groups())
            .map_err(|_| UninstallError::backend_failure())?;
        if !path_is_absent(Path::new(UNINSTALL_RECEIPT))? {
            if broker != MacOsAssetPresence::ExactPresent {
                return Err(UninstallError::backend_failure());
            }
            return self
                .assets
                .installed_uninstall_manifest()
                .map_err(|_| UninstallError::backend_failure());
        }
        let manifest = expected_preview_manifest(&self.expectation)?;
        let context = uninstall_recovery_context_digest(&manifest)?;
        let Some(storage) = MacOsInstallJournalStorage::open_existing(
            self.system,
            self.expectation.asset_manifest_digest(),
            context,
        )
        .map_err(|_| UninstallError::backend_failure())?
        else {
            return Ok(None);
        };
        let marker = storage
            .load()
            .map_err(|_| UninstallError::backend_failure())?
            .ok_or_else(UninstallError::backend_failure)?;
        if !marker.is_empty_uncommitted() {
            return Err(UninstallError::backend_failure());
        }
        Ok(Some(manifest))
    }

    fn verify_managed_install(&self) -> Result<(), UninstallError> {
        let path_entries = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        let environment_keys = env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
        verify_authenticated_managed_install(
            Path::new("/"),
            &self.expectation,
            &path_entries,
            &environment_keys,
        )
        .map_err(|_| UninstallError::backend_failure())
    }

    fn verify_created_assets(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), UninstallError> {
        for record in manifest
            .assets()
            .iter()
            .filter(|record| record.state() == RecordedAssetState::Created)
        {
            let asset = macos_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            if self
                .assets
                .classify_asset(asset)
                .map_err(|_| UninstallError::backend_failure())?
                != MacOsAssetPresence::ExactPresent
            {
                return Err(UninstallError::backend_failure());
            }
        }
        Ok(())
    }

    fn verify_interrupted_assets(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), UninstallError> {
        for record in manifest
            .assets()
            .iter()
            .filter(|record| record.state() == RecordedAssetState::Created)
        {
            let asset = macos_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            self.assets
                .classify_for_removal(asset)
                .map_err(|_| UninstallError::backend_failure())?;
        }
        Ok(())
    }

    fn verify_broker_absent_recovery(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), UninstallError> {
        crate::macos_launchd::verify_macos_services_absent()
            .map_err(|_| UninstallError::backend_failure())?;
        crate::macos_accounts::verify_macos_accounts_after_broker_removal(
            self.expectation.groups(),
        )
        .map_err(|_| UninstallError::backend_failure())?;
        self.assets
            .bind_filesystem_after_broker_removal()
            .map_err(|_| UninstallError::backend_failure())?;
        self.verify_interrupted_assets(manifest)?;
        #[cfg(target_os = "macos")]
        crate::verify_macos_store_removal_state_production()
            .map_err(|_| UninstallError::backend_failure())?;
        #[cfg(not(target_os = "macos"))]
        return Err(UninstallError::backend_failure());
        Ok(())
    }

    fn bind_recovery_marker(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<bool, UninstallError> {
        let context = uninstall_recovery_context_digest(manifest)?;
        let storage = MacOsInstallJournalStorage::open_existing(
            self.system,
            self.expectation.asset_manifest_digest(),
            context,
        )
        .map_err(|_| UninstallError::backend_failure())?;
        let recovery = if let Some(storage) = storage {
            let marker = storage
                .load()
                .map_err(|_| UninstallError::backend_failure())?
                .ok_or_else(UninstallError::backend_failure)?;
            if !marker.is_empty_uncommitted() {
                return Err(UninstallError::backend_failure());
            }
            self.uninstall_journal = Some(storage);
            true
        } else {
            false
        };
        self.recovery_context_digest = Some(context);
        Ok(recovery)
    }

    fn start_uninstall_marker(&mut self) -> Result<(), UninstallError> {
        if self.recovery_mode {
            return self
                .uninstall_journal
                .as_ref()
                .map(|_| ())
                .ok_or_else(UninstallError::backend_failure);
        }
        let context = self
            .recovery_context_digest
            .ok_or_else(UninstallError::backend_failure)?;
        self.verify_managed_install()?;
        let manifest = self
            .manifest
            .clone()
            .ok_or_else(UninstallError::backend_failure)?;
        self.verify_created_assets(&manifest)?;
        let storage = MacOsInstallJournalStorage::prepare(
            self.system,
            self.expectation.asset_manifest_digest(),
            context,
        )
        .map_err(|_| UninstallError::backend_failure())?;
        if storage
            .load()
            .map_err(|_| UninstallError::backend_failure())?
            .is_some()
        {
            return Err(UninstallError::backend_failure());
        }
        let marker = MacOsInstallJournal::new(
            self.system,
            self.expectation.asset_manifest_digest(),
            context,
        )
        .map_err(|_| UninstallError::backend_failure())?;
        storage
            .create(&marker)
            .map_err(|_| UninstallError::backend_failure())?;
        self.uninstall_journal = Some(storage);
        Ok(())
    }

    fn remove_user_roots(&mut self) -> Result<(), UninstallError> {
        self.user_cleanup
            .capture_user_roots()
            .map_err(|_| UninstallError::backend_failure())?;
        self.remove_captured_user_state_and_roots()
    }

    fn remove_captured_user_state_and_roots(&mut self) -> Result<(), UninstallError> {
        self.user_cleanup
            .remove_registered_user_state()
            .map_err(|_| UninstallError::backend_failure())?;
        self.user_cleanup
            .remove_user_roots()
            .map_err(|_| UninstallError::backend_failure())?;
        self.user_roots_removed = true;
        Ok(())
    }

    fn remove_managed_store(&mut self) -> Result<(), UninstallError> {
        if self.preserve_nix != Some(false) || self.user_roots_removed {
            return Err(UninstallError::backend_failure());
        }
        if self.recovery_mode {
            if self.uninstall_journal.is_none() {
                return Err(UninstallError::backend_failure());
            }
            if let Some(removal) = self.runtime_removal.take() {
                removal
                    .remove_preserving_store()
                    .map_err(|_| UninstallError::backend_failure())?;
            }
            self.remove_user_roots()?;
            #[cfg(target_os = "macos")]
            crate::remove_macos_store_volume_production()
                .map_err(|_| UninstallError::backend_failure())?;
            #[cfg(not(target_os = "macos"))]
            return Err(UninstallError::backend_failure());
            self.store_preserved = false;
            return Ok(());
        }
        self.user_cleanup
            .capture_user_roots()
            .map_err(|_| UninstallError::backend_failure())?;
        let closure = self
            .gc
            .as_ref()
            .ok_or_else(UninstallError::backend_failure)?
            .closure_for_roots(self.user_cleanup.store_roots())
            .map_err(|_| UninstallError::backend_failure())?;
        let registered = self
            .gc
            .as_ref()
            .ok_or_else(UninstallError::backend_failure)?
            .registered_paths()
            .map_err(|_| UninstallError::backend_failure())?;
        let removal = self
            .runtime_removal
            .take()
            .ok_or_else(UninstallError::backend_failure)?;
        let mut authority = removal
            .begin_exclusive_removal()
            .map_err(|_| UninstallError::backend_failure())?;
        authority
            .capture_product_closure(&closure, &registered)
            .map_err(|_| UninstallError::backend_failure())?;
        self.remove_captured_user_state_and_roots()?;
        self.store_preserved = true;
        if authority
            .remove()
            .map_err(|_| UninstallError::backend_failure())?
            != ManagedRuntimeRemovalOutcome::Removed
        {
            return Err(UninstallError::backend_failure());
        }
        #[cfg(target_os = "macos")]
        crate::remove_macos_store_volume_production()
            .map_err(|_| UninstallError::backend_failure())?;
        #[cfg(not(target_os = "macos"))]
        return Err(UninstallError::backend_failure());
        self.store_preserved = false;
        Ok(())
    }

    fn remove_asset(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        let UninstallAction::RemoveAsset { id, kind, target } = action else {
            return Err(UninstallError::backend_failure());
        };
        let asset = macos_asset(id).ok_or_else(UninstallError::backend_failure)?;
        if uninstall_kind(asset.kind()) != kind || asset.path_or_name() != target {
            return Err(UninstallError::backend_failure());
        }
        if self.recovery_mode
            && matches!(
                asset.kind(),
                MacOsAssetKind::Directory | MacOsAssetKind::File
            )
            && path_is_absent(Path::new(asset.path_or_name()))?
        {
            return Ok(());
        }
        if self.preserve_nix == Some(false) && self.store_preserved {
            return Ok(());
        }
        if (self.preserve_nix == Some(true) || self.store_preserved) && is_nix_asset(asset) {
            return Ok(());
        }
        if asset.id() == "nix-root" {
            return self.verify_removed_store_mountpoint(asset);
        }
        self.assets
            .remove_uninstall_asset(asset)
            .map_err(|_| UninstallError::backend_failure())
    }

    fn verify_removed_store_mountpoint(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), UninstallError> {
        if !path_is_absent(Path::new(asset.path_or_name()))? {
            self.assets
                .verify_empty_store_mountpoint(asset)
                .map_err(|_| UninstallError::backend_failure())?;
        }
        #[cfg(target_os = "macos")]
        return crate::verify_macos_store_volume_absent_production()
            .map_err(|_| UninstallError::backend_failure());
        #[cfg(not(target_os = "macos"))]
        Err(UninstallError::backend_failure())
    }

    fn verify_residue(&mut self) -> Result<(), UninstallError> {
        MacOsLaunchdManager::deactivate_verified()
            .map_err(|_| UninstallError::backend_failure())?;
        self.user_cleanup
            .verify_absent()
            .map_err(|_| UninstallError::backend_failure())?;
        crate::macos_accounts::verify_macos_accounts_absent()
            .map_err(|_| UninstallError::backend_failure())?;
        if self.preserve_nix != Some(true) && !self.store_preserved {
            let nix_root = macos_asset("nix-root").ok_or_else(UninstallError::backend_failure)?;
            self.verify_removed_store_mountpoint(nix_root)?;
        }
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(UninstallError::backend_failure)?;
        for record in manifest
            .assets()
            .iter()
            .filter(|record| record.state() == RecordedAssetState::Created)
        {
            let asset = macos_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            if (self.preserve_nix == Some(true) || self.store_preserved) && is_nix_asset(asset) {
                continue;
            }
            if asset.id() == "nix-root" {
                continue;
            }
            if matches!(
                asset.kind(),
                MacOsAssetKind::Directory | MacOsAssetKind::File
            ) && !path_is_absent(Path::new(asset.path_or_name()))?
            {
                return Err(UninstallError::backend_failure());
            }
        }
        if Path::new(MANAGED_RUNTIME_ROOT).exists() {
            return Err(UninstallError::backend_failure());
        }
        #[cfg(target_os = "macos")]
        crate::verify_macos_store_volume_absent_production()
            .map_err(|_| UninstallError::backend_failure())?;
        remove_fixed_state_file(
            Path::new("/private/var/db/pkg-install-accounts.lock"),
            0o600,
        )?;
        remove_fixed_empty_directory(Path::new("/private/var/db/pkg-install-auth"), 0o700)?;
        self.uninstall_journal
            .take()
            .ok_or_else(UninstallError::backend_failure)?
            .remove()
            .map_err(|_| UninstallError::backend_failure())?;
        Ok(())
    }
}

impl UninstallBackend for ProductionMacOsUninstallBackend {
    fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
        if Uid::effective().is_root() && Gid::effective().as_raw() == 0 {
            Ok(())
        } else {
            Err(UninstallError::backend_failure())
        }
    }

    fn verify_ownership(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError> {
        if manifest.system() != self.system
            || manifest.ownership_manifest_digest() != self.expectation.asset_manifest_digest()
            || self
                .manifest
                .as_ref()
                .is_some_and(|bound| bound != manifest)
        {
            return Err(UninstallError::backend_failure());
        }
        if manifest
            .assets()
            .iter()
            .any(|record| record.state() != RecordedAssetState::Created)
            || self.installed_manifest()?.as_ref() != Some(manifest)
        {
            return Err(UninstallError::backend_failure());
        }
        let recovery = self.bind_recovery_marker(manifest)?;
        match (
            recovery,
            crate::macos_accounts::broker_account_presence(self.expectation.groups())
                .map_err(|_| UninstallError::backend_failure())?,
        ) {
            (false, MacOsAssetPresence::ExactPresent) => {
                self.assets
                    .bind_uninstall_manifest(manifest)
                    .map_err(|_| UninstallError::backend_failure())?;
                self.verify_managed_install()?;
                self.verify_created_assets(manifest)?;
                self.runtime_removal = Some(
                    prepare_managed_runtime_removal(Path::new("/"), &self.expectation)
                        .map_err(|_| UninstallError::backend_failure())?,
                );
                self.gc = Some(
                    RootNixGcExecutor::new(Path::new(MANAGED_NIX_BINARY), Path::new(HELPER_HOME))
                        .map_err(|_| UninstallError::backend_failure())?,
                );
            }
            (true, MacOsAssetPresence::ExactPresent) => {
                self.assets
                    .bind_uninstall_manifest(manifest)
                    .map_err(|_| UninstallError::backend_failure())?;
                self.verify_interrupted_assets(manifest)?;
                #[cfg(target_os = "macos")]
                crate::verify_macos_store_removal_state_production()
                    .map_err(|_| UninstallError::backend_failure())?;
                #[cfg(not(target_os = "macos"))]
                return Err(UninstallError::backend_failure());
                self.runtime_removal = prepare_managed_runtime_removal_without_receipt(
                    Path::new("/"),
                    &self.expectation,
                )
                .map_err(|_| UninstallError::backend_failure())?;
                self.recovery_mode = true;
            }
            (true, MacOsAssetPresence::Absent) => {
                self.verify_broker_absent_recovery(manifest)?;
                self.recovery_mode = true;
            }
            (false, MacOsAssetPresence::Absent) => {
                return Err(UninstallError::backend_failure());
            }
        }
        self.preserve_nix = Some(false);
        self.store_preserved = false;
        self.manifest = Some(manifest.clone());
        Ok(())
    }

    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
        if self.manifest.is_none() || !self.recovery_mode && self.runtime_removal.is_none() {
            return Err(UninstallError::backend_failure());
        }
        if self.recovery_mode {
            #[cfg(target_os = "macos")]
            return crate::verify_macos_store_removal_state_production()
                .map_err(|_| UninstallError::backend_failure());
            #[cfg(not(target_os = "macos"))]
            return Err(UninstallError::backend_failure());
        }
        self.verify_managed_install()
    }

    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        let stopping = action == UninstallAction::StopServices;
        if !stopping && !self.services_stopped {
            return Err(UninstallError::backend_failure());
        }
        let result = match action {
            UninstallAction::StopServices => self.start_uninstall_marker().and_then(|()| {
                MacOsLaunchdManager::deactivate_verified()
                    .map_err(|_| UninstallError::backend_failure())
            }),
            UninstallAction::RemoveUserRoots => self.remove_user_roots(),
            UninstallAction::CollectGarbage => Err(UninstallError::backend_failure()),
            UninstallAction::RemoveManagedStoreIfExclusive => self.remove_managed_store(),
            UninstallAction::RemoveManagedRuntimePreservingStore => {
                if self.preserve_nix != Some(true) {
                    return Err(UninstallError::backend_failure());
                }
                self.store_preserved = self
                    .runtime_removal
                    .take()
                    .ok_or_else(UninstallError::backend_failure)?
                    .remove_preserving_store()
                    .map_err(|_| UninstallError::backend_failure())?
                    == ManagedRuntimeRemovalOutcome::StorePreserved;
                Ok(())
            }
            UninstallAction::RemoveRegisteredUserState => {
                if !self.user_roots_removed
                    || self.preserve_nix == Some(false) && self.store_preserved
                {
                    return Err(UninstallError::backend_failure());
                }
                self.user_cleanup
                    .remove_registered_user_state()
                    .map_err(|_| UninstallError::backend_failure())
            }
            UninstallAction::RemoveAsset { .. } => self.remove_asset(action),
            UninstallAction::VerifyNoPrivilegedResidue => self.verify_residue(),
        };
        if stopping && result.is_ok() {
            self.services_stopped = true;
        }
        result
    }
}

fn macos_asset(id: &str) -> Option<MacOsInstallAsset> {
    macos_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == id)
}

const fn uninstall_kind(kind: MacOsAssetKind) -> UninstallAssetKind {
    match kind {
        MacOsAssetKind::File => UninstallAssetKind::File,
        MacOsAssetKind::Directory => UninstallAssetKind::Directory,
        MacOsAssetKind::User => UninstallAssetKind::User,
        MacOsAssetKind::Group => UninstallAssetKind::Group,
    }
}

fn is_nix_asset(asset: MacOsInstallAsset) -> bool {
    Path::new(asset.path_or_name()).starts_with("/nix")
}

fn expected_preview_manifest(
    expectation: &OwnershipExpectation,
) -> Result<UninstallManifest, UninstallError> {
    let records = macos_install_assets()
        .iter()
        .map(|asset| RecordedAsset::new(asset.id(), RecordedAssetState::Created))
        .collect::<Result<Vec<_>, _>>()?;
    UninstallManifest::new(
        expectation.system(),
        expectation.asset_manifest_digest(),
        records,
    )
}

fn uninstall_recovery_context_digest(
    manifest: &UninstallManifest,
) -> Result<Digest, UninstallError> {
    let bytes = crate::encode_uninstall_manifest(manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(b"pkg-macos-uninstall-recovery-v1\0");
    hasher.update(bytes.len().to_be_bytes());
    hasher.update(bytes);
    Ok(Digest::from_bytes(hasher.finalize().into()))
}

fn path_is_absent(path: &Path) -> Result<bool, UninstallError> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Err(UninstallError::backend_failure()),
    }
}

/// Verifies that all fixed product-owned macOS state is absent.
///
/// # Errors
///
/// Returns a stable error for unreadable, foreign, ambiguous, or remaining state.
pub fn verify_macos_install_absent() -> Result<(), UninstallError> {
    crate::macos_launchd::verify_macos_services_absent()
        .map_err(|_| UninstallError::backend_failure())?;
    crate::macos_accounts::verify_macos_accounts_absent()
        .map_err(|_| UninstallError::backend_failure())?;
    for asset in macos_install_assets().iter().filter(|asset| {
        matches!(
            asset.kind(),
            MacOsAssetKind::Directory | MacOsAssetKind::File
        )
    }) {
        if asset.id() == "nix-root" {
            if !path_is_absent(Path::new(asset.path_or_name()))? {
                crate::macos_filesystem::verify_inert_nix_mountpoint()
                    .map_err(|_| UninstallError::backend_failure())?;
            }
            continue;
        }
        if !path_is_absent(Path::new(asset.path_or_name()))? {
            return Err(UninstallError::backend_failure());
        }
    }
    for path in [
        "/private/var/db/pkg-install-accounts.lock",
        "/private/var/db/pkg-install-auth",
        "/private/var/db/pkg-install",
    ] {
        if !path_is_absent(Path::new(path))? {
            return Err(UninstallError::backend_failure());
        }
    }
    #[cfg(target_os = "macos")]
    return crate::verify_macos_store_volume_absent_production()
        .map_err(|_| UninstallError::backend_failure());
    #[cfg(not(target_os = "macos"))]
    Err(UninstallError::backend_failure())
}

fn remove_fixed_state_file(path: &Path, mode: u32) -> Result<(), UninstallError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(UninstallError::backend_failure()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != mode
    {
        return Err(UninstallError::backend_failure());
    }
    std::fs::remove_file(path).map_err(|_| UninstallError::backend_failure())
}

fn remove_fixed_empty_directory(path: &Path, mode: u32) -> Result<(), UninstallError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(UninstallError::backend_failure()),
    };
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != mode
    {
        return Err(UninstallError::backend_failure());
    }
    std::fs::remove_dir(path).map_err(|_| UninstallError::backend_failure())
}
