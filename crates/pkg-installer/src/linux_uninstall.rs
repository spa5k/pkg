//! Production Linux binding for the closed uninstall transaction.

use std::env;
use std::path::{Path, PathBuf};

use nix::unistd::{Gid, Uid};
use pkg_core::System;
use pkg_nix::{
    AuthenticatedManagedNixConfig, ManagedRuntimeRemoval, ManagedRuntimeRemovalOutcome,
    OwnershipExpectation, RootNixGcExecutor, prepare_managed_runtime_removal,
    verify_authenticated_managed_install,
};
use rustix::fs::{Mode, OFlags, open, openat};
use rustix::io::Errno;

use crate::linux_accounts::verify_linux_accounts_absent;
use crate::linux_user_cleanup::LinuxUserCleanup;
use crate::{
    LinuxAccountManager, LinuxAssetKind, LinuxFilesystemManager, LinuxInstallAsset,
    LinuxReleasePayloads, LinuxSystemdManager, RecordedAssetState, UninstallAction,
    UninstallAssetKind, UninstallBackend, UninstallError, UninstallManifest, linux_install_assets,
};

const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
const HELPER_HOME: &str = "/var/lib/pkg/helper-home";
const MANAGED_RUNTIME_ROOT: &str = "/opt/pkg/nix";

trait LinuxUninstallRuntime {
    fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError>;
    fn preflight_privilege(&mut self) -> Result<(), UninstallError>;
    fn verify_ownership(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError>;
    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError>;
    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError>;
}

/// Production Linux implementation of the closed [`UninstallBackend`] contract.
pub struct ProductionLinuxUninstallBackend {
    system: System,
    runtime: Box<dyn LinuxUninstallRuntime>,
    services_stopped: bool,
}

impl std::fmt::Debug for ProductionLinuxUninstallBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionLinuxUninstallBackend")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl ProductionLinuxUninstallBackend {
    /// Constructs a root-host uninstall backend from authenticated release data.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Linux system, mismatched authenticated
    /// inputs, unsafe account state, missing systemd tools, or an unavailable
    /// managed Nix executable.
    pub fn new(
        config: &AuthenticatedManagedNixConfig,
        expectation: &OwnershipExpectation,
        payloads: LinuxReleasePayloads,
    ) -> Result<Self, UninstallError> {
        let system = expectation.system();
        if !matches!(system, System::X8664Linux | System::Aarch64Linux) || config.system() != system
        {
            return Err(UninstallError::backend_failure());
        }
        let runtime = ProductionRuntime::new(config, expectation, payloads)?;
        Ok(Self {
            system,
            runtime: Box::new(runtime),
            services_stopped: false,
        })
    }

    /// Reads and verifies the installed uninstall manifest without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the fixed manifest path or metadata is unsafe.
    pub fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError> {
        self.runtime.installed_manifest()
    }

    #[cfg(test)]
    fn with_runtime(system: System, runtime: Box<dyn LinuxUninstallRuntime>) -> Self {
        Self {
            system,
            runtime,
            services_stopped: false,
        }
    }
}

impl UninstallBackend for ProductionLinuxUninstallBackend {
    fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
        self.runtime.preflight_privilege()
    }

    fn verify_ownership(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError> {
        if manifest.system() != self.system {
            return Err(UninstallError::backend_failure());
        }
        self.runtime.verify_ownership(manifest)
    }

    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
        self.runtime.preflight_unmanaged_nix()
    }

    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        let stopping_services = action == UninstallAction::StopServices;
        if !stopping_services && !self.services_stopped {
            return Err(UninstallError::backend_failure());
        }
        let result = self.runtime.execute(action);
        if stopping_services && result.is_ok() {
            self.services_stopped = true;
        }
        result
    }
}

struct ProductionRuntime {
    system: System,
    expectation: OwnershipExpectation,
    accounts: LinuxAccountManager,
    filesystem: LinuxFilesystemManager,
    services: LinuxSystemdManager,
    user_cleanup: LinuxUserCleanup,
    gc: RootNixGcExecutor,
    runtime_removal: Option<ManagedRuntimeRemoval>,
    manifest: Option<UninstallManifest>,
    preserve_nix: Option<bool>,
    user_roots_removed: bool,
    store_preserved: bool,
}

impl std::fmt::Debug for ProductionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionRuntime")
            .field("system", &self.system)
            .field("ownership_prepared", &self.runtime_removal.is_some())
            .field("manifest_bound", &self.manifest.is_some())
            .field("preserve_nix", &self.preserve_nix)
            .field("user_roots_removed", &self.user_roots_removed)
            .field("store_preserved", &self.store_preserved)
            .finish_non_exhaustive()
    }
}

impl ProductionRuntime {
    fn new(
        config: &AuthenticatedManagedNixConfig,
        expectation: &OwnershipExpectation,
        payloads: LinuxReleasePayloads,
    ) -> Result<Self, UninstallError> {
        let mut accounts = LinuxAccountManager::new(expectation.groups());
        let broker_uid = accounts
            .broker_uid()
            .map_err(|_| UninstallError::backend_failure())?;
        let mut filesystem =
            LinuxFilesystemManager::new(expectation.groups(), broker_uid, payloads)
                .map_err(|_| UninstallError::backend_failure())?;
        filesystem
            .bind_authenticated_nix_config(config)
            .map_err(|_| UninstallError::backend_failure())?;
        Ok(Self {
            system: expectation.system(),
            expectation: expectation.clone(),
            accounts,
            filesystem,
            services: LinuxSystemdManager::production()
                .map_err(|_| UninstallError::backend_failure())?,
            user_cleanup: LinuxUserCleanup::production(),
            gc: RootNixGcExecutor::new(Path::new(MANAGED_NIX_BINARY), Path::new(HELPER_HOME))
                .map_err(|_| UninstallError::backend_failure())?,
            runtime_removal: None,
            manifest: None,
            preserve_nix: None,
            user_roots_removed: false,
            store_preserved: false,
        })
    }

    fn environment_evidence() -> (Vec<PathBuf>, Vec<std::ffi::OsString>) {
        let path_entries = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect())
            .unwrap_or_default();
        let environment_keys = env::vars_os().map(|(key, _)| key).collect();
        (path_entries, environment_keys)
    }

    fn verify_managed_install(&self) -> Result<(), UninstallError> {
        let (path_entries, environment_keys) = Self::environment_evidence();
        verify_authenticated_managed_install(
            Path::new("/"),
            &self.expectation,
            &path_entries,
            &environment_keys,
        )
        .map_err(|_| UninstallError::backend_failure())?;
        Ok(())
    }

    fn restore_roots_and_preserve(&mut self) -> Result<(), UninstallError> {
        self.store_preserved = true;
        self.user_roots_removed = false;
        self.user_cleanup
            .restore_user_roots()
            .map_err(|_| UninstallError::backend_failure())
    }

    fn execute_remove_user_roots(&mut self) -> Result<(), UninstallError> {
        if self.preserve_nix == Some(false) {
            return Err(UninstallError::backend_failure());
        }
        if self.user_cleanup.remove_user_roots().is_ok() {
            self.user_roots_removed = true;
            return Ok(());
        }
        self.restore_roots_and_preserve()?;
        Err(UninstallError::backend_failure())
    }

    fn execute_remove_managed_store(&mut self) -> Result<(), UninstallError> {
        if self.preserve_nix != Some(false) || self.user_roots_removed {
            return Err(UninstallError::backend_failure());
        }
        self.user_cleanup
            .capture_user_roots()
            .map_err(|_| UninstallError::backend_failure())?;
        let removal = self
            .runtime_removal
            .take()
            .ok_or_else(UninstallError::backend_failure)?;
        let mut authority = removal
            .begin_exclusive_removal()
            .map_err(|_| UninstallError::backend_failure())?;
        let closure = self
            .gc
            .closure_for_roots(self.user_cleanup.store_roots())
            .map_err(|_| UninstallError::backend_failure())?;
        authority
            .capture_product_closure(&closure)
            .map_err(|_| UninstallError::backend_failure())?;
        if self.user_cleanup.remove_user_roots().is_err() {
            self.user_cleanup
                .restore_user_roots()
                .map_err(|_| UninstallError::backend_failure())?;
            return Err(UninstallError::backend_failure());
        }
        self.user_roots_removed = true;
        self.store_preserved = true;
        match authority.remove() {
            Ok(ManagedRuntimeRemovalOutcome::Removed) => {
                self.store_preserved = false;
                Ok(())
            }
            Ok(ManagedRuntimeRemovalOutcome::StorePreserved) | Err(_) => {
                self.user_cleanup
                    .restore_user_roots()
                    .map_err(|_| UninstallError::backend_failure())?;
                Err(UninstallError::backend_failure())
            }
        }
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
            let asset = linux_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            if LinuxAccountManager::handles(asset) {
                self.accounts
                    .verify_asset(asset)
                    .map_err(|_| UninstallError::backend_failure())?;
            } else {
                self.filesystem
                    .verify_asset(asset)
                    .map_err(|_| UninstallError::backend_failure())?;
            }
        }
        Ok(())
    }

    fn remove_asset(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        let UninstallAction::RemoveAsset { id, kind, target } = action else {
            return Err(UninstallError::backend_failure());
        };
        let asset = linux_asset(id).ok_or_else(UninstallError::backend_failure)?;
        if uninstall_kind(asset.kind()) != kind || asset.path_or_name() != target {
            return Err(UninstallError::backend_failure());
        }
        if self.preserve_nix == Some(false) && self.store_preserved {
            // A failed exclusivity or GC proof preserves the complete managed
            // installation. Removing any later asset could strand `/nix`.
            return Ok(());
        }
        if (self.preserve_nix == Some(true) || self.store_preserved) && is_nix_asset(asset) {
            // ManagedRuntimeRemoval deliberately preserves the complete Nix
            // tree when the store is shared. A later generic asset action must
            // not weaken that stronger preservation decision.
            return Ok(());
        }
        if LinuxAccountManager::handles(asset) {
            self.accounts
                .remove_verified_asset(asset)
                .map_err(|_| UninstallError::backend_failure())?;
        } else if asset.id() == "broker-channel-state" {
            self.filesystem
                .remove_broker_channel_state(asset)
                .map_err(|_| UninstallError::backend_failure())?;
        } else {
            self.filesystem
                .remove_verified_asset(asset)
                .map_err(|_| UninstallError::backend_failure())?;
            if is_systemd_unit(asset) {
                self.services
                    .reload_units()
                    .map_err(|_| UninstallError::backend_failure())?;
            }
        }
        Ok(())
    }

    fn verify_residue(&mut self) -> Result<(), UninstallError> {
        self.services
            .deactivate_for_uninstall()
            .map_err(|_| UninstallError::backend_failure())?;
        self.user_cleanup
            .verify_absent()
            .map_err(|_| UninstallError::backend_failure())?;
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(UninstallError::backend_failure)?;
        for record in manifest
            .assets()
            .iter()
            .filter(|record| record.state() == RecordedAssetState::Created)
        {
            let asset = linux_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            if (self.preserve_nix == Some(true) || self.store_preserved) && is_nix_asset(asset) {
                continue;
            }
            if LinuxAccountManager::handles(asset) {
                self.accounts
                    .verify_asset_absent(asset)
                    .map_err(|_| UninstallError::backend_failure())?;
            } else {
                self.filesystem
                    .verify_asset_absent(asset)
                    .map_err(|_| UninstallError::backend_failure())?;
            }
        }
        verify_fixed_path_absent(Path::new(MANAGED_RUNTIME_ROOT))
    }
}

impl LinuxUninstallRuntime for ProductionRuntime {
    fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError> {
        self.filesystem
            .existing_uninstall_manifest()
            .map_err(|_| UninstallError::backend_failure())
    }

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
            || self.runtime_removal.is_some()
        {
            return Err(UninstallError::backend_failure());
        }
        self.verify_managed_install()?;
        self.filesystem
            .bind_uninstall_manifest(manifest)
            .map_err(|_| UninstallError::backend_failure())?;
        if self
            .filesystem
            .existing_uninstall_manifest()
            .map_err(|_| UninstallError::backend_failure())?
            .as_ref()
            != Some(manifest)
        {
            return Err(UninstallError::backend_failure());
        }
        self.verify_created_assets(manifest)?;
        self.runtime_removal = Some(
            prepare_managed_runtime_removal(Path::new("/"), &self.expectation)
                .map_err(|_| UninstallError::backend_failure())?,
        );
        let preserve_nix = manifest_preserves_nix(manifest)?;
        self.preserve_nix = Some(preserve_nix);
        self.store_preserved = preserve_nix;
        self.manifest = Some(manifest.clone());
        Ok(())
    }

    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
        if self.runtime_removal.is_none() || self.manifest.is_none() {
            return Err(UninstallError::backend_failure());
        }
        self.verify_managed_install()
    }

    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        match action {
            UninstallAction::StopServices => self
                .services
                .deactivate_for_uninstall()
                .map_err(|_| UninstallError::backend_failure()),
            UninstallAction::RemoveUserRoots => self.execute_remove_user_roots(),
            UninstallAction::CollectGarbage => Err(UninstallError::backend_failure()),
            UninstallAction::RemoveManagedStoreIfExclusive => self.execute_remove_managed_store(),
            UninstallAction::RemoveManagedRuntimePreservingStore => {
                if self.preserve_nix != Some(true) {
                    return Err(UninstallError::backend_failure());
                }
                self.store_preserved = true;
                let removal = self
                    .runtime_removal
                    .take()
                    .ok_or_else(UninstallError::backend_failure)?;
                self.store_preserved = removal
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
        }
    }
}

fn linux_asset(id: &str) -> Option<LinuxInstallAsset> {
    linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == id)
}

const fn uninstall_kind(kind: LinuxAssetKind) -> UninstallAssetKind {
    match kind {
        LinuxAssetKind::File => UninstallAssetKind::File,
        LinuxAssetKind::Directory => UninstallAssetKind::Directory,
        LinuxAssetKind::User => UninstallAssetKind::User,
        LinuxAssetKind::Group => UninstallAssetKind::Group,
    }
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

fn is_nix_asset(asset: LinuxInstallAsset) -> bool {
    Path::new(asset.path_or_name()).starts_with("/nix")
}

fn manifest_preserves_nix(manifest: &UninstallManifest) -> Result<bool, UninstallError> {
    manifest
        .assets()
        .iter()
        .find(|record| record.id() == "nix-root")
        .map(|record| record.state() == RecordedAssetState::PreExisting)
        .ok_or_else(UninstallError::backend_failure)
}

pub fn verify_linux_install_absent() -> Result<(), UninstallError> {
    let mut services =
        LinuxSystemdManager::production().map_err(|_| UninstallError::backend_failure())?;
    if services
        .classify_activation()
        .map_err(|_| UninstallError::backend_failure())?
    {
        return Err(UninstallError::backend_failure());
    }
    verify_linux_accounts_absent().map_err(|_| UninstallError::backend_failure())?;
    for asset in linux_install_assets() {
        if !LinuxAccountManager::handles(*asset) {
            verify_fixed_path_absent(Path::new(asset.path_or_name()))?;
        }
    }
    verify_fixed_path_absent(Path::new(MANAGED_RUNTIME_ROOT))?;
    verify_fixed_path_absent(Path::new("/run/pkg-install-auth"))
}

fn verify_fixed_path_absent(path: &Path) -> Result<(), UninstallError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(UninstallError::backend_failure());
    }
    let root = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| UninstallError::backend_failure())?;
    let mut parent = root;
    let mut components = path.components().filter_map(|component| match component {
        std::path::Component::Normal(component) => Some(component),
        _ => None,
    });
    let Some(first) = components.next() else {
        return Err(UninstallError::backend_failure());
    };
    let mut current = first;
    for next in components {
        parent = match openat(
            &parent,
            current,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(Errno::NOENT) => return Ok(()),
            Err(_) => return Err(UninstallError::backend_failure()),
        };
        current = next;
    }
    match openat(
        &parent,
        current,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) | Err(_) => Err(UninstallError::backend_failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RecordedAsset, execute_uninstall, plan_uninstall};
    use pkg_core::state::Digest;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeState {
        calls: Vec<UninstallAction>,
        ownership_verified: bool,
        stop_fails: bool,
    }

    struct FakeRuntime(Rc<RefCell<FakeState>>);

    impl LinuxUninstallRuntime for FakeRuntime {
        fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError> {
            Ok(None)
        }

        fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
            Ok(())
        }

        fn verify_ownership(&mut self, _: &UninstallManifest) -> Result<(), UninstallError> {
            self.0.borrow_mut().ownership_verified = true;
            Ok(())
        }

        fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
            if self.0.borrow().ownership_verified {
                Ok(())
            } else {
                Err(UninstallError::backend_failure())
            }
        }

        fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
            let stopping_services = action == UninstallAction::StopServices;
            let mut state = self.0.borrow_mut();
            state.calls.push(action);
            if stopping_services && state.stop_fails {
                return Err(UninstallError::backend_failure());
            }
            Ok(())
        }
    }

    fn manifest() -> Result<UninstallManifest, UninstallError> {
        manifest_with_nix_root(RecordedAssetState::Created)
    }

    fn manifest_with_nix_root(
        nix_root_state: RecordedAssetState,
    ) -> Result<UninstallManifest, UninstallError> {
        let records = linux_install_assets()
            .iter()
            .map(|asset| {
                RecordedAsset::new(
                    asset.id(),
                    if asset.id() == "nix-root" {
                        nix_root_state
                    } else {
                        RecordedAssetState::Created
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([0xaa; 32]),
            records,
        )
    }

    #[test]
    fn production_router_dispatches_the_exact_validated_plan() -> Result<(), UninstallError> {
        let manifest = manifest()?;
        let plan = plan_uninstall(&manifest)?;
        let state = Rc::new(RefCell::new(FakeState::default()));
        let runtime = FakeRuntime(Rc::clone(&state));
        let mut backend =
            ProductionLinuxUninstallBackend::with_runtime(System::Aarch64Linux, Box::new(runtime));
        let report = execute_uninstall(&manifest, &plan, &mut backend)?;
        assert_eq!(report.completed_actions(), plan.actions().len());
        assert_eq!(state.borrow().calls, plan.actions());
        Ok(())
    }

    #[test]
    fn router_refuses_a_manifest_for_another_system() -> Result<(), UninstallError> {
        let manifest = manifest()?;
        let state = Rc::new(RefCell::new(FakeState::default()));
        let runtime = FakeRuntime(Rc::clone(&state));
        let mut backend =
            ProductionLinuxUninstallBackend::with_runtime(System::X8664Linux, Box::new(runtime));
        assert!(backend.verify_ownership(&manifest).is_err());
        assert!(!state.borrow().ownership_verified);
        Ok(())
    }

    #[test]
    fn failed_service_shutdown_blocks_every_destructive_action() {
        let state = Rc::new(RefCell::new(FakeState {
            stop_fails: true,
            ..FakeState::default()
        }));
        let runtime = FakeRuntime(Rc::clone(&state));
        let mut backend =
            ProductionLinuxUninstallBackend::with_runtime(System::Aarch64Linux, Box::new(runtime));

        assert!(backend.execute(UninstallAction::StopServices).is_err());
        assert!(backend.execute(UninstallAction::RemoveUserRoots).is_err());
        assert_eq!(state.borrow().calls, [UninstallAction::StopServices]);
    }

    #[test]
    fn fixed_path_absence_refuses_an_existing_or_symlinked_target() {
        assert!(verify_fixed_path_absent(Path::new("/definitely/not/a/pkg/path")).is_ok());
        assert!(verify_fixed_path_absent(Path::new("/tmp")).is_err());
    }

    #[test]
    fn shared_store_preservation_covers_only_the_nix_tree() -> Result<(), UninstallError> {
        let nix_state = linux_asset("nix-state").ok_or_else(UninstallError::backend_failure)?;
        let product_root =
            linux_asset("product-root").ok_or_else(UninstallError::backend_failure)?;
        assert!(is_nix_asset(nix_state));
        assert!(!is_nix_asset(product_root));
        Ok(())
    }

    #[test]
    fn preexisting_nix_root_selects_the_no_gc_preservation_policy() -> Result<(), UninstallError> {
        assert!(!manifest_preserves_nix(&manifest()?)?);
        assert!(manifest_preserves_nix(&manifest_with_nix_root(
            RecordedAssetState::PreExisting
        )?)?);
        Ok(())
    }
}
