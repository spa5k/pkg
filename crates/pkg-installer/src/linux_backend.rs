//! Complete production binding for the closed Linux installer transaction.

use crate::{
    AssetPresence, BrokerTransportErrorCode, DeterminateHandoffState, InstallError,
    LinuxInstallAsset, LinuxInstallBackend, LinuxPlatformAssetManager,
    determinate_handoff::DeterminateHandoff,
    linux_systemd::{LinuxSystemdFailure, LinuxSystemdFailurePhase, LinuxSystemdManager},
    platform::linux::LinuxProductAssetIntent,
};
use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, DetectionDisposition,
    ManagedGroupBindings, RealNixAdapter, detect_unmanaged_nix,
};
use std::{env, fmt, io, io::Write, path::Path};

const BROKER_HOME: &str = "/var/lib/pkg/broker-home";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxServiceFailure {
    Systemd(LinuxSystemdFailure),
    BrokerReadiness(BrokerTransportErrorCode),
}

impl fmt::Display for LinuxServiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Systemd(failure) => failure.fmt(formatter),
            Self::BrokerReadiness(code) => write!(
                formatter,
                "phase=broker-readiness class={}",
                match code {
                    BrokerTransportErrorCode::UnauthenticatedPeer => "unauthenticated-peer",
                    BrokerTransportErrorCode::TransportFailure => "transport-failure",
                    BrokerTransportErrorCode::InvalidFrame => "invalid-frame",
                    BrokerTransportErrorCode::BrokerFailure => "broker-failure",
                }
            ),
        }
    }
}

fn write_service_diagnostic(
    mut writer: impl Write,
    failure: Option<LinuxServiceFailure>,
) -> io::Result<()> {
    if let Some(failure) = failure {
        writeln!(writer, "pkg-service-failure {failure}")?;
    }
    Ok(())
}

fn report_service_failure(failure: LinuxServiceFailure) {
    let stderr = io::stderr();
    let _ = write_service_diagnostic(stderr.lock(), Some(failure));
}

fn systemd_service_failure(
    error: crate::linux_systemd::LinuxSystemdError,
    fallback_phase: LinuxSystemdFailurePhase,
) -> LinuxServiceFailure {
    LinuxServiceFailure::Systemd(
        error
            .failure()
            .unwrap_or_else(|| LinuxSystemdFailure::not_run(fallback_phase, None, error.code())),
    )
}

fn classify_service_state(
    services: &mut LinuxSystemdManager,
    writer: impl Write,
) -> Result<AssetPresence, InstallError> {
    services
        .classify_activation()
        .map(|active| {
            if active {
                AssetPresence::ExactPresent
            } else {
                AssetPresence::Absent
            }
        })
        .map_err(|error| {
            let _ = write_service_diagnostic(
                writer,
                Some(systemd_service_failure(
                    error,
                    LinuxSystemdFailurePhase::StateQuery,
                )),
            );
            InstallError::backend_failure()
        })
}

/// Production implementation of the closed Linux installer backend.
#[derive(Debug)]
pub struct ProductionLinuxInstallBackend {
    system: System,
    assets: LinuxPlatformAssetManager,
    services: LinuxSystemdManager,
    release_identity: Option<Digest>,
    requested_product_asset_intent: LinuxProductAssetIntent,
    mode: crate::InstallMode,
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

#[cfg(test)]
struct ExistingNonFilePreflightBackend {
    backend: ProductionLinuxInstallBackend,
    temporary: tempfile::TempDir,
    account_mutation_calls: std::rc::Rc<std::cell::Cell<usize>>,
    service_calls: std::rc::Rc<std::cell::Cell<usize>>,
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
                crate::InstallMode::OfflineRepair
            } else {
                crate::InstallMode::FreshInstall
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
                    crate::InstallMode::OfflineRepair
                } else {
                    crate::InstallMode::FreshInstall
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

    #[cfg(test)]
    fn for_existing_non_file_preflight_test(
        system: System,
        groups: ManagedGroupBindings,
        release: Digest,
        missing_id: &str,
    ) -> Result<ExistingNonFilePreflightBackend, Box<dyn std::error::Error>> {
        let assets = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
            groups, system, release, missing_id,
        )?;
        let (services, service_calls) = LinuxSystemdManager::inert_for_preflight_test();
        Ok(ExistingNonFilePreflightBackend {
            backend: Self {
                system,
                assets: assets.manager,
                services,
                release_identity: Some(release),
                requested_product_asset_intent: LinuxProductAssetIntent::InstallOrUpgrade,
                mode: crate::InstallMode::FreshInstall,
                existing_managed_install: false,
                recovered_fresh_install: false,
                preflight_fixture: Some(ProductionPreflightFixture {
                    effective_ids: (0, 0),
                    handoff_snapshots: std::rc::Rc::new(std::cell::RefCell::new(vec![
                        DeterminateHandoffState::Accepted,
                    ])),
                }),
            },
            temporary: assets.temporary,
            account_mutation_calls: assets.account_mutation_calls,
            service_calls,
        })
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
    journal_mode: crate::InstallMode,
    handoff_state: DeterminateHandoffState,
) -> Result<(), InstallError> {
    if matches!(handoff_state, DeterminateHandoffState::Started) {
        return Err(InstallError::backend_failure());
    }
    match (requested_intent, journal_mode, handoff_state) {
        (
            LinuxProductAssetIntent::Repair,
            crate::InstallMode::OfflineRepair,
            DeterminateHandoffState::Accepted,
        )
        | (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::OfflineUpgrade,
            DeterminateHandoffState::Accepted,
        )
        | (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::NotStarted | DeterminateHandoffState::Accepted,
        ) => Ok(()),
        _ => Err(InstallError::recovery_mode_mismatch()),
    }
}

fn can_classify_active_install(
    intent: LinuxProductAssetIntent,
    mode: crate::InstallMode,
    handoff: DeterminateHandoffState,
    authenticated_inputs_bound: bool,
    release_identity_bound: bool,
) -> Result<bool, InstallError> {
    if intent == LinuxProductAssetIntent::Repair {
        return Ok(false);
    }
    match handoff {
        DeterminateHandoffState::NotStarted => Ok(false),
        DeterminateHandoffState::Started => Err(InstallError::backend_failure()),
        DeterminateHandoffState::Accepted => Ok(mode == crate::InstallMode::FreshInstall
            && authenticated_inputs_bound
            && release_identity_bound),
    }
}

impl LinuxInstallBackend for ProductionLinuxInstallBackend {
    fn install_mode(&self) -> crate::InstallMode {
        self.mode
    }

    fn classify_active_install(&mut self) -> Result<bool, InstallError> {
        #[cfg(test)]
        let handoff_state = Self::handoff_state(self.preflight_fixture.as_ref())?;
        #[cfg(not(test))]
        let handoff_state = Self::production_handoff_state()?;
        if !can_classify_active_install(
            self.requested_product_asset_intent,
            self.mode,
            handoff_state,
            self.assets.authenticated_inputs_bound(self.system),
            self.release_identity.is_some(),
        )? || !self.assets.classify_exact_release()?
            || !self
                .services
                .classify_exact_activation()
                .map_err(|_| InstallError::backend_failure())?
        {
            return Ok(false);
        }
        RealNixAdapter::new_standard_determinate(Path::new(BROKER_HOME))
            .and_then(|adapter| adapter.ping_managed_store())
            .map_err(|_| InstallError::backend_failure())?;
        crate::broker::probe_broker_readiness(Path::new(crate::service::LINUX_BROKER_SOCKET))
            .map_err(|_| InstallError::backend_failure())?;
        Ok(true)
    }

    fn preflight_recovery(
        &mut self,
        mode: crate::InstallMode,
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
        self.recovered_fresh_install = mode == crate::InstallMode::FreshInstall;
        self.assets
            .set_intent(if mode == crate::InstallMode::OfflineRepair {
                LinuxProductAssetIntent::Repair
            } else {
                LinuxProductAssetIntent::InstallOrUpgrade
            });
        if mode == crate::InstallMode::OfflineRepair {
            self.assets.preflight_repair()?;
        }
        if mode != crate::InstallMode::FreshInstall {
            self.preflight_product_mutation()?;
        }
        Ok(())
    }

    fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
        if self.mode == crate::InstallMode::FreshInstall {
            return Ok(());
        }
        self.services
            .require_offline()
            .map_err(|_| InstallError::offline_services_required())
    }

    fn preflight_fresh_recovery_mutation(
        &mut self,
        journal: &crate::LinuxInstallJournal,
    ) -> Result<(), InstallError> {
        if self.mode != crate::InstallMode::FreshInstall || !journal.fresh_services_deactivated() {
            return Err(InstallError::backend_failure());
        }
        self.services
            .require_fresh_recovery_offline(|unit| {
                journal.records_asset(match unit {
                    "pkg-root-helper.socket" => "helper-socket-unit",
                    "pkg-nix-broker.socket" => "broker-socket-unit",
                    "pkg-root-helper.service" => "helper-service-unit",
                    "pkg-nix-broker.service" => "broker-service-unit",
                    _ => return false,
                })
            })
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
        #[cfg(test)]
        let effective_ids = Self::effective_ids(self.preflight_fixture.as_ref());
        #[cfg(not(test))]
        let effective_ids = Self::production_effective_ids();
        #[cfg(test)]
        let authenticated_inputs_bound =
            self.preflight_fixture.is_some() || self.assets.authenticated_inputs_bound(system);
        #[cfg(not(test))]
        let authenticated_inputs_bound = self.assets.authenticated_inputs_bound(system);
        if system != self.system || !authenticated_inputs_bound || effective_ids != (0, 0) {
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
            self.preflight_product_mutation()?;
            self.existing_managed_install = true;
            return Ok(());
        }
        if validate_determinate_handoff_preflight(state)? {
            self.existing_managed_install = true;
            self.mode = if self.recovered_fresh_install {
                crate::InstallMode::FreshInstall
            } else {
                crate::InstallMode::OfflineUpgrade
            };
            self.recovered_fresh_install = false;
            self.assets
                .set_intent(LinuxProductAssetIntent::InstallOrUpgrade);
            if self.mode == crate::InstallMode::OfflineUpgrade {
                self.assets.preflight_existing_non_files()?;
                self.preflight_product_mutation()?;
            }
            return Ok(());
        }
        let report = detect_unmanaged_nix(Path::new("/"), system, &path_entries, &environment_keys);
        if report.disposition() != DetectionDisposition::Clean {
            return Err(InstallError::backend_failure());
        }
        self.existing_managed_install = false;
        self.mode = crate::InstallMode::FreshInstall;
        Ok(())
    }

    fn broker_uid(&mut self) -> Result<u32, InstallError> {
        self.assets.broker_uid()
    }

    fn classify_asset(&mut self, asset: LinuxInstallAsset) -> Result<AssetPresence, InstallError> {
        self.assets.classify_asset(asset)
    }

    fn classify_ownership_receipt(
        &mut self,
        _asset: LinuxInstallAsset,
    ) -> Result<AssetPresence, InstallError> {
        self.assets.classify_uninstall_manifest()
    }

    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        if self.mode == crate::InstallMode::OfflineRepair {
            return Err(InstallError::backend_failure());
        }
        self.preflight_product_mutation()?;
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
        if self.mode != crate::InstallMode::FreshInstall {
            return Err(InstallError::backend_failure());
        }
        let (assets, services) = (&mut self.assets, &mut self.services);
        services
            .deactivate_fresh_recovery(|| assets.verify_service_runtime_assets().is_ok())
            .map_err(|_| InstallError::backend_failure())
    }

    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, InstallError> {
        Ok(if self.existing_managed_install {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
        })
    }

    fn classify_services(&mut self) -> Result<AssetPresence, InstallError> {
        if self.mode != crate::InstallMode::FreshInstall {
            self.preflight_product_mutation()?;
            return Ok(AssetPresence::ExactPresent);
        }
        let stderr = io::stderr();
        classify_service_state(&mut self.services, stderr.lock())
    }

    fn services_need_mutation(&self, prior_active: bool) -> bool {
        self.mode == crate::InstallMode::FreshInstall && !prior_active
    }

    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        self.preflight_product_mutation()?;
        self.assets.ensure_asset(asset)
    }

    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        self.preflight_product_mutation()?;
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
        let adapter = RealNixAdapter::new_standard_determinate(Path::new(BROKER_HOME))
            .map_err(|_| InstallError::backend_failure())?;
        validate_base_nix_readiness(
            self.existing_managed_install,
            || adapter.ping_managed_store(),
            || adapter.wait_for_managed_store(),
        )
    }

    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
        Ok(())
    }

    fn activate_services(&mut self) -> Result<bool, InstallError> {
        if self.mode != crate::InstallMode::FreshInstall {
            self.preflight_product_mutation()?;
            return Ok(false);
        }
        let (assets, services) = (&mut self.assets, &mut self.services);
        services
            .activate_fresh(|| assets.verify_service_runtime_assets().is_ok())
            .map_err(|error| {
                report_service_failure(systemd_service_failure(
                    error,
                    LinuxSystemdFailurePhase::StateQuery,
                ));
                InstallError::backend_failure()
            })
    }

    fn rollback_services(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::InstallMode::FreshInstall {
            return Err(InstallError::backend_failure());
        }
        let (assets, services) = (&mut self.assets, &mut self.services);
        services
            .prepare_rollback(|| assets.verify_service_runtime_assets().is_ok())
            .map_err(|_| InstallError::backend_failure())
    }

    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::InstallMode::FreshInstall {
            return Err(InstallError::rollback_incomplete());
        }
        self.services.finish_rollback();
        Ok(())
    }

    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        if self.mode != crate::InstallMode::FreshInstall {
            return self.preflight_product_mutation();
        }
        self.services.verify_active().map_err(|error| {
            report_service_failure(systemd_service_failure(
                error,
                LinuxSystemdFailurePhase::VerifyActive,
            ));
            InstallError::backend_failure()
        })?;
        crate::broker::probe_broker_readiness(Path::new(crate::service::LINUX_BROKER_SOCKET))
            .map_err(|error| {
                report_service_failure(LinuxServiceFailure::BrokerReadiness(error.code()));
                InstallError::backend_failure()
            })
    }

    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        self.preflight_product_mutation()?;
        self.assets.publish_uninstall_manifest()
    }

    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        let mode = self.mode;
        let (assets, services) = (&mut self.assets, &mut self.services);
        assets.finalize_replacement_backups(|| {
            if mode == crate::InstallMode::FreshInstall {
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
        self.preflight_product_mutation()?;
        self.assets.rollback_asset(asset)?;
        Ok(())
    }
}

fn validate_base_nix_readiness(
    existing_managed_install: bool,
    ping: impl FnOnce() -> Result<(), pkg_nix::NixAdapterError>,
    wait: impl FnOnce() -> Result<(), pkg_nix::NixAdapterError>,
) -> Result<(), InstallError> {
    if existing_managed_install {
        ping()
    } else {
        wait()
    }
    .map_err(|_| InstallError::backend_failure())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_service_diagnostic(
        failure: Option<LinuxServiceFailure>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        write_service_diagnostic(&mut output, failure)?;
        Ok(String::from_utf8(output)?)
    }

    #[test]
    fn service_diagnostics_are_one_fixed_line_and_success_is_silent()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(rendered_service_diagnostic(None)?.is_empty());

        let systemd = rendered_service_diagnostic(Some(LinuxServiceFailure::Systemd(
            LinuxSystemdFailure::not_run(
                LinuxSystemdFailurePhase::Start,
                Some("pkg-root-helper.service"),
                crate::linux_systemd::LinuxSystemdErrorCode::CommandFailed,
            ),
        )))?;
        assert_eq!(
            systemd,
            "pkg-service-failure phase=start class=command-failed terminal=not-run unit=pkg-root-helper.service\n"
        );
        assert_eq!(systemd.lines().count(), 1);

        for (code, class) in [
            (
                BrokerTransportErrorCode::UnauthenticatedPeer,
                "unauthenticated-peer",
            ),
            (
                BrokerTransportErrorCode::TransportFailure,
                "transport-failure",
            ),
            (BrokerTransportErrorCode::InvalidFrame, "invalid-frame"),
            (BrokerTransportErrorCode::BrokerFailure, "broker-failure"),
        ] {
            let line =
                rendered_service_diagnostic(Some(LinuxServiceFailure::BrokerReadiness(code)))?;
            assert_eq!(
                line,
                format!("pkg-service-failure phase=broker-readiness class={class}\n")
            );
            assert_eq!(line.lines().count(), 1);
            for forbidden in ["synthetic-raw", "secret-marker", "\x1b", "\r"] {
                assert!(!line.contains(forbidden));
            }
        }
        Ok(())
    }

    #[test]
    fn classify_services_failure_writes_exactly_one_line_before_mapping() {
        let (mut services, calls) = LinuxSystemdManager::inert_for_preflight_test();
        let mut output = Vec::new();

        assert_eq!(
            classify_service_state(&mut services, &mut output).map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure)
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            output,
            b"pkg-service-failure phase=state-query class=command-failed terminal=not-run unit=pkg-root-helper.socket\n"
        );
    }

    #[test]
    fn base_nix_readiness_waits_only_for_a_fresh_install() {
        let pings = std::cell::Cell::new(0);

        assert!(
            validate_base_nix_readiness(
                true,
                || {
                    pings.set(pings.get() + 1);
                    Ok(())
                },
                || Err(pkg_nix::NixAdapterError::OperationFailed),
            )
            .is_ok()
        );
        assert_eq!(pings.get(), 1);

        let waits = std::cell::Cell::new(0);
        assert!(
            validate_base_nix_readiness(
                false,
                || Err(pkg_nix::NixAdapterError::OperationFailed),
                || {
                    waits.set(waits.get() + 1);
                    Ok(())
                },
            )
            .is_ok()
        );
        assert_eq!(waits.get(), 1);
    }

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
            crate::installer::install_linux(System::X8664Linux, &mut backend)
                .map_err(InstallError::code),
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
                crate::installer::install_linux(System::X8664Linux, &mut backend)
                    .map_err(InstallError::code),
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
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::NotStarted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::InstallMode::OfflineUpgrade,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::InstallMode::OfflineRepair,
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
                crate::InstallMode::OfflineUpgrade,
                DeterminateHandoffState::NotStarted,
            ),
            (
                LinuxProductAssetIntent::InstallOrUpgrade,
                crate::InstallMode::OfflineRepair,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
            ),
            (
                LinuxProductAssetIntent::Repair,
                crate::InstallMode::OfflineRepair,
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
                crate::InstallMode::FreshInstall,
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

        assert_eq!(backend.mode, crate::InstallMode::FreshInstall);
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
        backend.mode = crate::InstallMode::OfflineUpgrade;
        assert!(!backend.services_need_mutation(false));
        assert!(!backend.services_need_mutation(true));
        Ok(())
    }

    #[test]
    fn active_install_policy_is_fresh_normal_accepted_and_fully_bound() {
        let normal = LinuxProductAssetIntent::InstallOrUpgrade;
        assert_eq!(
            can_classify_active_install(
                normal,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
                true,
                true,
            ),
            Ok(true)
        );
        for policy in [
            can_classify_active_install(
                LinuxProductAssetIntent::Repair,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
                true,
                true,
            ),
            can_classify_active_install(
                normal,
                crate::InstallMode::OfflineUpgrade,
                DeterminateHandoffState::Accepted,
                true,
                true,
            ),
            can_classify_active_install(
                normal,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::NotStarted,
                true,
                true,
            ),
            can_classify_active_install(
                normal,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Accepted,
                false,
                true,
            ),
        ] {
            assert_eq!(policy, Ok(false));
        }
        assert!(
            can_classify_active_install(
                normal,
                crate::InstallMode::FreshInstall,
                DeterminateHandoffState::Started,
                true,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn production_offline_upgrade_refuses_missing_receipt_owned_non_files_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let system = System::X8664Linux;
        let groups = ManagedGroupBindings::new(30_000, 30_001)?;
        let release = Digest::from_bytes([0xd1; 32]);
        for (missing_id, missing_path) in [
            ("broker-user", None),
            ("broker-log-dir", Some("var/lib/pkg/log/broker")),
        ] {
            let fixture = ProductionLinuxInstallBackend::for_existing_non_file_preflight_test(
                system, groups, release, missing_id,
            )?;
            let ExistingNonFilePreflightBackend {
                mut backend,
                temporary,
                account_mutation_calls,
                service_calls,
            } = fixture;
            let receipt_path = temporary.path().join("opt/pkg/uninstall/manifest.json");
            let receipt_before = std::fs::read(&receipt_path)?;
            let handoff_before = backend
                .preflight_fixture
                .as_ref()
                .ok_or_else(|| std::io::Error::other("missing preflight fixture"))?
                .handoff_snapshots
                .borrow()
                .clone();

            assert_eq!(
                backend
                    .preflight_clean_host(system)
                    .map_err(InstallError::code),
                Err(crate::InstallErrorCode::BackendFailure),
                "missing {missing_id} must fail closed"
            );

            assert_eq!(account_mutation_calls.get(), 0);
            assert_eq!(service_calls.get(), 0);
            assert_eq!(std::fs::read(&receipt_path)?, receipt_before);
            assert!(!temporary.path().join("pkg-install").exists());
            if let Some(path) = missing_path {
                assert!(!temporary.path().join(path).exists());
            }
            assert_eq!(
                backend
                    .preflight_fixture
                    .as_ref()
                    .ok_or_else(|| std::io::Error::other("missing preflight fixture"))?
                    .handoff_snapshots
                    .borrow()
                    .as_slice(),
                handoff_before.as_slice()
            );
        }
        Ok(())
    }
}
