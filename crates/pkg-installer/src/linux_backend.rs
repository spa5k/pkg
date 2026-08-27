//! Complete production binding for the closed Linux installer transaction.

use crate::{
    DeterminateHandoffState, InstallError, LinuxAssetPresence, LinuxInstallAsset,
    LinuxInstallBackend, LinuxPlatformAssetManager, determinate_handoff::DeterminateHandoff,
    linux_platform_assets::LinuxProductAssetIntent, linux_systemd::LinuxSystemdManager,
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
    requested_product_asset_intent: LinuxProductAssetIntent,
    mode: crate::LinuxInstallMode,
    existing_managed_install: bool,
    recovered_fresh_install: bool,
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
            requested_product_asset_intent: product_asset_intent,
            mode: if product_asset_intent == LinuxProductAssetIntent::Repair {
                crate::LinuxInstallMode::OfflineRepair
            } else {
                crate::LinuxInstallMode::FreshInstall
            },
            existing_managed_install: false,
            recovered_fresh_install: false,
            #[cfg(test)]
            preflight_fixture: None,
        })
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
                requested_product_asset_intent: product_asset_intent,
                mode: if product_asset_intent == LinuxProductAssetIntent::Repair {
                    crate::LinuxInstallMode::OfflineRepair
                } else {
                    crate::LinuxInstallMode::FreshInstall
                },
                existing_managed_install: false,
                recovered_fresh_install: false,
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

const fn validate_recovery_mode(
    requested_intent: LinuxProductAssetIntent,
    journal_mode: crate::LinuxInstallMode,
    handoff_state: DeterminateHandoffState,
) -> Result<(), InstallError> {
    if matches!(handoff_state, DeterminateHandoffState::Started) {
        return Err(InstallError::backend_failure());
    }
    match (requested_intent, journal_mode, handoff_state) {
        (
            LinuxProductAssetIntent::Repair,
            crate::LinuxInstallMode::OfflineRepair,
            DeterminateHandoffState::Accepted,
        )
        | (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::LinuxInstallMode::OfflineUpgrade,
            DeterminateHandoffState::Accepted,
        )
        | (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::LinuxInstallMode::FreshInstall,
            DeterminateHandoffState::NotStarted | DeterminateHandoffState::Accepted,
        ) => Ok(()),
        _ => Err(InstallError::recovery_mode_mismatch()),
    }
}

impl LinuxInstallBackend for ProductionLinuxInstallBackend {
    fn install_mode(&self) -> crate::LinuxInstallMode {
        self.mode
    }

    fn preflight_recovery(
        &mut self,
        mode: crate::LinuxInstallMode,
        system: System,
    ) -> Result<(), InstallError> {
        self.preflight_privilege()?;
        #[cfg(test)]
        let handoff_state = Self::handoff_state(self.preflight_fixture.as_ref())?;
        #[cfg(not(test))]
        let handoff_state = Self::production_handoff_state()?;
        validate_recovery_mode(self.requested_product_asset_intent, mode, handoff_state)?;
        if system != self.system || !self.assets.authenticated_inputs_bound(system) {
            return Err(InstallError::backend_failure());
        }
        self.mode = mode;
        self.recovered_fresh_install = mode == crate::LinuxInstallMode::FreshInstall;
        self.assets
            .set_intent(if mode == crate::LinuxInstallMode::OfflineRepair {
                LinuxProductAssetIntent::Repair
            } else {
                LinuxProductAssetIntent::InstallOrUpgrade
            });
        if mode == crate::LinuxInstallMode::OfflineRepair {
            self.assets.preflight_repair()?;
        }
        if mode != crate::LinuxInstallMode::FreshInstall {
            self.preflight_product_file_mutation()?;
        }
        Ok(())
    }

    fn preflight_product_file_mutation(&mut self) -> Result<(), InstallError> {
        if self.mode == crate::LinuxInstallMode::FreshInstall {
            return Ok(());
        }
        self.services
            .require_offline()
            .map_err(|_| InstallError::offline_services_required())
    }

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
        if self.requested_product_asset_intent == LinuxProductAssetIntent::Repair {
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
        if self.requested_product_asset_intent == LinuxProductAssetIntent::Repair {
            validate_product_repair_handoff_preflight(state)?;
            self.assets.preflight_repair()?;
            self.preflight_product_file_mutation()?;
            self.existing_managed_install = true;
            return Ok(());
        }
        if validate_determinate_handoff_preflight(state)? {
            self.existing_managed_install = true;
            self.mode = if self.recovered_fresh_install {
                crate::LinuxInstallMode::FreshInstall
            } else {
                crate::LinuxInstallMode::OfflineUpgrade
            };
            self.recovered_fresh_install = false;
            self.assets
                .set_intent(LinuxProductAssetIntent::InstallOrUpgrade);
            if self.mode == crate::LinuxInstallMode::OfflineUpgrade {
                self.preflight_product_file_mutation()?;
            }
            return Ok(());
        }
        let report = detect_unmanaged_nix(Path::new("/"), system, &path_entries, &environment_keys);
        if report.disposition() != DetectionDisposition::Clean {
            return Err(InstallError::backend_failure());
        }
        self.existing_managed_install = false;
        self.mode = crate::LinuxInstallMode::FreshInstall;
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
        if self.mode == crate::LinuxInstallMode::OfflineRepair {
            return Err(InstallError::backend_failure());
        }
        self.preflight_product_file_mutation()?;
        self.assets.recover_asset(asset)?;
        Ok(())
    }

    fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
        let (assets, services) = (&mut self.assets, &mut self.services);
        assets.recover_repair_assets(|| {
            services
                .require_offline()
                .map_err(|_| InstallError::offline_services_required())
        })
    }

    fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            return Err(InstallError::backend_failure());
        }
        self.services
            .deactivate_fresh_recovery()
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
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            self.preflight_product_file_mutation()?;
            return Ok(LinuxAssetPresence::ExactPresent);
        }
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
        self.mode == crate::LinuxInstallMode::FreshInstall && !prior_active
    }

    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        if asset.kind() == crate::LinuxAssetKind::File {
            self.preflight_product_file_mutation()?;
        }
        self.assets.ensure_asset(asset)
    }

    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        self.preflight_product_file_mutation()?;
        self.assets.install_static_asset(asset, contents)
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

    fn validate_base_nix(&mut self) -> Result<(), InstallError> {
        RealNixAdapter::new_standard_determinate(Path::new(BROKER_HOME))
            .and_then(|adapter| adapter.ping_managed_store())
            .map_err(|_| InstallError::backend_failure())
    }

    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
        Ok(())
    }

    fn activate_services(&mut self) -> Result<bool, InstallError> {
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            self.preflight_product_file_mutation()?;
            return Ok(false);
        }
        self.services
            .activate_fresh()
            .map_err(|_| InstallError::backend_failure())
    }

    fn rollback_services(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            return Err(InstallError::backend_failure());
        }
        self.services
            .prepare_rollback()
            .map_err(|_| InstallError::backend_failure())
    }

    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            return Err(InstallError::rollback_incomplete());
        }
        self.services
            .finish_rollback()
            .map_err(|_| InstallError::backend_failure())
    }

    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::LinuxInstallMode::FreshInstall {
            return self.preflight_product_file_mutation();
        }
        self.services
            .verify_active()
            .map_err(|_| InstallError::backend_failure())?;
        crate::broker::probe_broker_readiness(Path::new(crate::service::LINUX_BROKER_SOCKET))
            .map_err(|_| InstallError::backend_failure())
    }

    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        self.preflight_product_file_mutation()?;
        self.assets.publish_uninstall_manifest()
    }

    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        let mode = self.mode;
        let (assets, services) = (&mut self.assets, &mut self.services);
        assets.finalize_replacement_backups(|| {
            if mode == crate::LinuxInstallMode::FreshInstall {
                Ok(())
            } else {
                services
                    .require_offline()
                    .map_err(|_| InstallError::offline_services_required())
            }
        })?;
        self.services.commit_activation();
        Ok(())
    }

    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if asset.kind() == crate::LinuxAssetKind::File {
            self.preflight_product_file_mutation()?;
        }
        self.assets.rollback_asset(asset)?;
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
    fn recovery_mode_is_derived_from_the_journal_and_handoff() {
        for (intent, mode, handoff) in [
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::FreshInstall,
                DeterminateHandoffState::NotStarted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::OfflineUpgrade,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::LinuxInstallMode::OfflineRepair,
                DeterminateHandoffState::Accepted,
            ),
        ] {
            assert_eq!(
                validate_recovery_mode(intent, mode, handoff).map_err(InstallError::code),
                Ok(())
            );
        }

        for (intent, mode, handoff) in [
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::OfflineUpgrade,
                DeterminateHandoffState::NotStarted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::OfflineRepair,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::LinuxInstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::LinuxInstallMode::OfflineRepair,
                DeterminateHandoffState::NotStarted,
            ),
        ] {
            assert_eq!(
                validate_recovery_mode(intent, mode, handoff).map_err(InstallError::code),
                Err(crate::InstallErrorCode::RecoveryModeMismatch)
            );
        }
        assert_eq!(
            validate_recovery_mode(
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::LinuxInstallMode::FreshInstall,
                DeterminateHandoffState::Started,
            )
            .map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure)
        );
    }

    #[test]
    fn privilege_preflight_does_not_classify_an_existing_install()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
            DeterminateHandoffState::Accepted,
        ]));
        let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
            System::X8664Linux,
            ManagedGroupBindings::new(100, 101)?,
            snapshots,
            LinuxProductAssetIntent::InstallOrUpgrade,
        );

        backend.preflight_privilege()?;

        assert_eq!(backend.mode, crate::LinuxInstallMode::FreshInstall);
        assert_eq!(service_calls.get(), 0);
        Ok(())
    }

    #[test]
    fn production_service_transition_is_fresh_install_only()
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
        assert!(!backend.services_need_mutation(true));
        backend.mode = crate::LinuxInstallMode::OfflineUpgrade;
        assert!(!backend.services_need_mutation(false));
        assert!(!backend.services_need_mutation(true));
        Ok(())
    }
}
