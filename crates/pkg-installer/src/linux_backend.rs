//! Complete production binding for the closed Linux installer transaction.

use crate::{
    DeterminateHandoffState, InstallError, LinuxAssetPresence, LinuxInstallAsset,
    LinuxInstallBackend, LinuxPlatformAssetManager, LinuxSystemdManager,
    determinate_handoff::DeterminateHandoff, linux_platform_assets::LinuxProductAssetIntent,
};
use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, DetectionDisposition,
    ManagedGroupBindings, RealNixAdapter, detect_unmanaged_nix,
};
use std::{env, path::Path};

const BROKER_HOME: &str = "/var/lib/pkg/broker-home";

/// Production implementation of the closed Linux installer backend.
#[derive(Debug)]
pub struct ProductionLinuxInstallBackend {
    system: System,
    assets: LinuxPlatformAssetManager,
    services: LinuxSystemdManager,
    release_identity: Option<Digest>,
    product_asset_intent: LinuxProductAssetIntent,
    existing_managed_install: bool,
    product_files_changed: bool,
    prior_services_active: bool,
    #[cfg(test)]
    preflight_fixture: Option<ProductionPreflightFixture>,
}

#[cfg(test)]
#[derive(Debug)]
struct ProductionPreflightFixture {
    effective_ids: (u32, u32),
    handoff_snapshots: std::rc::Rc<std::cell::RefCell<Vec<DeterminateHandoffState>>>,
}

impl ProductionLinuxInstallBackend {
    /// Creates a backend for one authenticated native Linux installation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Linux system or unavailable systemd tools.
    pub fn new(system: System, groups: ManagedGroupBindings) -> Result<Self, InstallError> {
        Self::with_product_asset_intent(system, groups, LinuxProductAssetIntent::InstallOrUpgrade)
    }

    /// Creates a backend for explicit same-release Linux product-file repair.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Linux system or unavailable systemd tools.
    pub fn new_product_repair(
        system: System,
        groups: ManagedGroupBindings,
    ) -> Result<Self, InstallError> {
        Self::with_product_asset_intent(system, groups, LinuxProductAssetIntent::Repair)
    }

    fn with_product_asset_intent(
        system: System,
        groups: ManagedGroupBindings,
        product_asset_intent: LinuxProductAssetIntent,
    ) -> Result<Self, InstallError> {
        if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
            return Err(InstallError::backend_failure());
        }
        Ok(Self {
            system,
            assets: LinuxPlatformAssetManager::with_intent(groups, product_asset_intent),
            services: LinuxSystemdManager::production()
                .map_err(|_| InstallError::backend_failure())?,
            release_identity: None,
            product_asset_intent,
            existing_managed_install: false,
            product_files_changed: false,
            prior_services_active: false,
            #[cfg(test)]
            preflight_fixture: None,
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

    #[cfg(test)]
    fn handoff_state(
        fixture: Option<&ProductionPreflightFixture>,
    ) -> Result<DeterminateHandoffState, InstallError> {
        if let Some(fixture) = fixture {
            return fixture
                .handoff_snapshots
                .borrow()
                .last()
                .copied()
                .ok_or_else(InstallError::backend_failure);
        }
        Self::production_handoff_state()
    }

    fn production_handoff_state() -> Result<DeterminateHandoffState, InstallError> {
        DeterminateHandoff::production()
            .and_then(|handoff| handoff.state())
            .map_err(|_| InstallError::backend_failure())
    }

    #[cfg(test)]
    fn effective_ids(fixture: Option<&ProductionPreflightFixture>) -> (u32, u32) {
        if let Some(fixture) = fixture {
            return fixture.effective_ids;
        }
        Self::production_effective_ids()
    }

    fn production_effective_ids() -> (u32, u32) {
        (Uid::effective().as_raw(), Gid::effective().as_raw())
    }

    #[cfg(test)]
    fn for_preflight_test(
        system: System,
        groups: ManagedGroupBindings,
        handoff_snapshots: std::rc::Rc<std::cell::RefCell<Vec<DeterminateHandoffState>>>,
        product_asset_intent: LinuxProductAssetIntent,
    ) -> (Self, std::rc::Rc<std::cell::Cell<usize>>) {
        let (services, service_calls) = LinuxSystemdManager::inert_for_preflight_test();
        (
            Self {
                system,
                assets: LinuxPlatformAssetManager::with_intent(groups, product_asset_intent),
                services,
                release_identity: None,
                product_asset_intent,
                existing_managed_install: false,
                product_files_changed: false,
                prior_services_active: false,
                preflight_fixture: Some(ProductionPreflightFixture {
                    effective_ids: (0, 0),
                    handoff_snapshots,
                }),
            },
            service_calls,
        )
    }
}

pub const fn validate_determinate_handoff_preflight(
    state: DeterminateHandoffState,
) -> Result<bool, InstallError> {
    match state {
        DeterminateHandoffState::NotStarted => Ok(false),
        DeterminateHandoffState::Started => Err(InstallError::backend_failure()),
        DeterminateHandoffState::Accepted => Ok(true),
    }
}

const fn validate_product_repair_handoff_preflight(
    state: DeterminateHandoffState,
) -> Result<(), InstallError> {
    if matches!(state, DeterminateHandoffState::Accepted) {
        Ok(())
    } else {
        Err(InstallError::backend_failure())
    }
}

impl LinuxInstallBackend for ProductionLinuxInstallBackend {
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), InstallError> {
        if payloads.system() != self.system {
            return Err(InstallError::backend_failure());
        }
        self.assets.bind_authenticated_installer_payloads(payloads)
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        if config.system() != self.system {
            return Err(InstallError::backend_failure());
        }
        self.assets.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError> {
        if system != self.system || self.release_identity.is_some_and(|bound| bound != digest) {
            return Err(InstallError::backend_failure());
        }
        self.assets
            .bind_authenticated_release_identity(system, digest)?;
        self.release_identity = Some(digest);
        Ok(())
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        #[cfg(test)]
        let (uid, gid) = Self::effective_ids(self.preflight_fixture.as_ref());
        #[cfg(not(test))]
        let (uid, gid) = Self::production_effective_ids();
        if uid != 0 || gid != 0 {
            return Err(InstallError::backend_failure());
        }
        #[cfg(test)]
        let state = Self::handoff_state(self.preflight_fixture.as_ref())?;
        #[cfg(not(test))]
        let state = Self::production_handoff_state()?;
        if self.product_asset_intent == LinuxProductAssetIntent::Repair {
            validate_product_repair_handoff_preflight(state)?;
        } else {
            validate_determinate_handoff_preflight(state)?;
        }
        Ok(())
    }

    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError> {
        if system != self.system
            || !self.assets.authenticated_inputs_bound(system)
            || !Uid::effective().is_root()
            || Gid::effective().as_raw() != 0
        {
            return Err(InstallError::backend_failure());
        }
        let path_entries = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        let environment_keys = env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
        #[cfg(test)]
        let state = Self::handoff_state(self.preflight_fixture.as_ref())?;
        #[cfg(not(test))]
        let state = Self::production_handoff_state()?;
        if self.product_asset_intent == LinuxProductAssetIntent::Repair {
            validate_product_repair_handoff_preflight(state)?;
            self.existing_managed_install = true;
            return Ok(());
        }
        if validate_determinate_handoff_preflight(state)? {
            self.existing_managed_install = true;
            return Ok(());
        }
        let report = detect_unmanaged_nix(Path::new("/"), system, &path_entries, &environment_keys);
        if report.disposition() != DetectionDisposition::Clean {
            return Err(InstallError::backend_failure());
        }
        self.existing_managed_install = false;
        Ok(())
    }

    fn broker_uid(&mut self) -> Result<u32, InstallError> {
        self.assets.broker_uid()
    }

    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError> {
        self.assets.classify_asset(asset)
    }

    fn classify_ownership_receipt(
        &mut self,
        _asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError> {
        self.assets.classify_uninstall_manifest()
    }

    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.assets.recover_asset(asset)?;
        if Self::is_systemd_unit(asset) {
            self.services
                .reload_units()
                .map_err(|_| InstallError::backend_failure())?;
        }
        Ok(())
    }

    fn recover_services(&mut self) -> Result<(), InstallError> {
        self.services
            .deactivate_for_uninstall()
            .map_err(|_| InstallError::backend_failure())
    }

    fn prepare_service_recovery(&mut self, prior_active: bool) -> Result<(), InstallError> {
        self.services
            .prepare_recovery(prior_active)
            .map_err(|_| InstallError::backend_failure())
    }

    fn finish_service_recovery(&mut self, prior_active: bool) -> Result<(), InstallError> {
        self.services
            .finish_recovery(prior_active)
            .map_err(|_| InstallError::backend_failure())
    }

    fn classify_managed_runtime(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        Ok(if self.existing_managed_install {
            LinuxAssetPresence::ExactPresent
        } else {
            LinuxAssetPresence::Absent
        })
    }

    fn classify_services(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.services
            .classify_activation()
            .map(|active| {
                if active {
                    LinuxAssetPresence::ExactPresent
                } else {
                    LinuxAssetPresence::Absent
                }
            })
            .map_err(|_| InstallError::backend_failure())
    }

    fn services_need_mutation(&self, prior_active: bool) -> bool {
        (!self.existing_managed_install && !prior_active)
            || (self.product_files_changed && prior_active)
    }

    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let changed = self.assets.ensure_asset(asset)?;
        self.product_files_changed |= changed && asset.kind() == crate::LinuxAssetKind::File;
        Ok(changed)
    }

    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        let changed = self.assets.install_static_asset(asset, contents)?;
        self.product_files_changed |= changed;
        Ok(changed)
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
        self.prior_services_active = self
            .services
            .classify_activation()
            .map_err(|_| InstallError::backend_failure())?;
        self.services
            .activate(
                !self.existing_managed_install,
                self.product_files_changed && self.prior_services_active,
            )
            .map_err(|_| InstallError::backend_failure())
    }

    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.services
            .prepare_rollback()
            .map_err(|_| InstallError::backend_failure())
    }

    fn finish_services_rollback(&mut self) -> Result<(), InstallError> {
        self.services
            .finish_rollback()
            .map_err(|_| InstallError::backend_failure())
    }

    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        if self.existing_managed_install && !self.prior_services_active {
            return match self.services.classify_activation() {
                Ok(false) => Ok(()),
                Ok(true) | Err(_) => Err(InstallError::backend_failure()),
            };
        }
        self.services
            .verify_active()
            .map_err(|_| InstallError::backend_failure())?;
        RealNixAdapter::new_standard_determinate(Path::new(BROKER_HOME))
            .and_then(|adapter| adapter.ping_managed_store())
            .map_err(|_| InstallError::backend_failure())?;
        crate::broker::probe_broker_readiness(Path::new(crate::service::LINUX_BROKER_SOCKET))
            .map_err(|_| InstallError::backend_failure())
    }

    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        self.assets.publish_uninstall_manifest()
    }

    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        self.assets.finalize_replacement_backups()?;
        self.services.commit_activation();
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_handoff_is_the_only_refused_linux_preflight_state() {
        assert_eq!(
            validate_determinate_handoff_preflight(DeterminateHandoffState::NotStarted)
                .map_err(InstallError::code),
            Ok(false)
        );
        assert!(validate_determinate_handoff_preflight(DeterminateHandoffState::Started).is_err());
        assert_eq!(
            validate_determinate_handoff_preflight(DeterminateHandoffState::Accepted)
                .map_err(InstallError::code),
            Ok(true)
        );
    }

    #[test]
    fn production_preflight_refuses_persisted_started_without_later_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
            DeterminateHandoffState::Started,
        ]));
        let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
            System::X8664Linux,
            ManagedGroupBindings::new(100, 101)?,
            snapshots.clone(),
            LinuxProductAssetIntent::InstallOrUpgrade,
        );

        assert_eq!(
            crate::install_linux(System::X8664Linux, &mut backend).map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure)
        );
        assert_eq!(
            snapshots.borrow().as_slice(),
            &[DeterminateHandoffState::Started]
        );
        assert_eq!(service_calls.get(), 0);
        assert!(backend.release_identity.is_none());
        assert!(!backend.existing_managed_install);
        Ok(())
    }

    #[test]
    fn product_repair_requires_an_accepted_determinate_handoff()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            validate_product_repair_handoff_preflight(DeterminateHandoffState::Accepted)
                .map_err(InstallError::code),
            Ok(())
        );
        for state in [
            DeterminateHandoffState::NotStarted,
            DeterminateHandoffState::Started,
        ] {
            let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![state]));
            let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
                System::X8664Linux,
                ManagedGroupBindings::new(100, 101)?,
                snapshots,
                LinuxProductAssetIntent::Repair,
            );

            assert_eq!(
                crate::install_linux(System::X8664Linux, &mut backend).map_err(InstallError::code),
                Err(crate::InstallErrorCode::BackendFailure)
            );
            assert_eq!(service_calls.get(), 0);
            assert!(!backend.existing_managed_install);
        }
        Ok(())
    }

    #[test]
    fn production_service_transition_is_only_for_clean_start_or_changed_active_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
            DeterminateHandoffState::Accepted,
        ]));
        let (mut backend, _) = ProductionLinuxInstallBackend::for_preflight_test(
            System::X8664Linux,
            ManagedGroupBindings::new(100, 101)?,
            snapshots,
            LinuxProductAssetIntent::InstallOrUpgrade,
        );
        assert!(backend.services_need_mutation(false));
        backend.existing_managed_install = true;
        assert!(!backend.services_need_mutation(false));
        assert!(!backend.services_need_mutation(true));
        backend.product_files_changed = true;
        assert!(!backend.services_need_mutation(false));
        assert!(backend.services_need_mutation(true));
        Ok(())
    }
}
