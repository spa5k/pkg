//! Product installer entry points for authenticated managed-Nix bundles.

use crate::{
    InstallError, LinuxInstallAsset, LinuxInstallBackend, LinuxInstallReport, MacOsBuildReadiness,
    MacOsError, MacOsInstallAsset, MacOsInstallBackend, MacOsInstallReport, install_linux,
    install_macos,
};
use pkg_channel::TrustedRoot;
use pkg_core::System;
use pkg_nix::{
    AuthenticatedInstallerBundle, AuthenticatedManagedNixConfig, InstallerProvisionRequest,
    ManagedDaemon, ProvisionedBootstrap, ProvisionedBootstrapTransaction,
    authenticate_installer_bundle_blocking, provision_authenticated_installer_bundle_transaction,
};

/// Successful Linux installation and its authenticated runtime/index result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBundleInstallReport {
    platform: LinuxInstallReport,
    bootstrap: ProvisionedBootstrap,
}

impl LinuxBundleInstallReport {
    /// Returns the platform installation report.
    #[must_use]
    pub const fn platform(&self) -> LinuxInstallReport {
        self.platform
    }

    /// Returns the authenticated runtime and host-index result.
    #[must_use]
    pub const fn bootstrap(&self) -> &ProvisionedBootstrap {
        &self.bootstrap
    }

    /// Consumes the report into its platform and bundle parts.
    #[must_use]
    pub fn into_parts(self) -> (LinuxInstallReport, ProvisionedBootstrap) {
        (self.platform, self.bootstrap)
    }
}

/// Successful macOS installation and its authenticated runtime/index result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsBundleInstallReport {
    platform: MacOsInstallReport,
    bootstrap: ProvisionedBootstrap,
}

impl MacOsBundleInstallReport {
    /// Returns the platform installation report.
    #[must_use]
    pub const fn platform(&self) -> MacOsInstallReport {
        self.platform
    }

    /// Returns the authenticated runtime and host-index result.
    #[must_use]
    pub const fn bootstrap(&self) -> &ProvisionedBootstrap {
        &self.bootstrap
    }

    /// Consumes the report into its platform and bundle parts.
    #[must_use]
    pub fn into_parts(self) -> (MacOsInstallReport, ProvisionedBootstrap) {
        (self.platform, self.bootstrap)
    }
}

/// Authenticates the bundle before the first Linux platform mutation, then installs it.
///
/// # Errors
///
/// Returns a redacted platform failure for invalid authentication, preparation,
/// installation, readiness, receipt publication, or rollback.
pub fn install_linux_from_bundle<'a>(
    system: System,
    trusted_root: TrustedRoot,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn LinuxInstallBackend,
) -> Result<LinuxBundleInstallReport, InstallError> {
    if request.system != system {
        return Err(InstallError::backend_failure());
    }
    let bundle = authenticate_installer_bundle_blocking(trusted_root, request)
        .map_err(|_| InstallError::backend_failure())?;
    backend.bind_authenticated_nix_config(bundle.managed_nix_config())?;
    let mut provisioner = AuthenticatedProvisioner::new(bundle);
    let (platform, outcome) =
        install_linux_with_provisioner(system, request, daemon, backend, &mut provisioner)?;
    let bootstrap = outcome.into_linux_bootstrap()?;
    Ok(LinuxBundleInstallReport {
        platform,
        bootstrap,
    })
}

/// Authenticates the bundle before the first macOS platform mutation, then installs it.
///
/// # Errors
///
/// Returns a redacted platform failure for invalid authentication, preparation,
/// installation, readiness, receipt publication, or rollback.
pub fn install_macos_from_bundle<'a>(
    system: System,
    trusted_root: TrustedRoot,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn MacOsInstallBackend,
) -> Result<MacOsBundleInstallReport, MacOsError> {
    if request.system != system {
        return Err(MacOsError::backend_failure());
    }
    let bundle = authenticate_installer_bundle_blocking(trusted_root, request)
        .map_err(|_| MacOsError::backend_failure())?;
    backend.bind_authenticated_nix_config(bundle.managed_nix_config())?;
    let mut provisioner = AuthenticatedProvisioner::new(bundle);
    let (platform, outcome) =
        install_macos_with_provisioner(system, request, daemon, backend, &mut provisioner)?;
    let bootstrap = outcome.into_macos_bootstrap()?;
    Ok(MacOsBundleInstallReport {
        platform,
        bootstrap,
    })
}

enum BootstrapOutcome<'a> {
    Pending(Box<ProvisionedBootstrapTransaction<'a>>),
    Complete(ProvisionedBootstrap),
    #[cfg(test)]
    Stub(std::rc::Rc<std::cell::Cell<bool>>),
}

impl BootstrapOutcome<'_> {
    fn into_linux_bootstrap(self) -> Result<ProvisionedBootstrap, InstallError> {
        match self {
            Self::Complete(bootstrap) => Ok(bootstrap),
            Self::Pending(_) => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(InstallError::backend_failure()),
        }
    }

    fn into_macos_bootstrap(self) -> Result<ProvisionedBootstrap, MacOsError> {
        match self {
            Self::Complete(bootstrap) => Ok(bootstrap),
            Self::Pending(_) => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(MacOsError::backend_failure()),
        }
    }

    fn rollback_linux(self) -> Result<(), InstallError> {
        match self {
            Self::Pending(transaction) => transaction
                .rollback()
                .map_err(|_| InstallError::backend_failure()),
            Self::Complete(_) => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::Stub(rolled_back) => {
                rolled_back.set(true);
                Ok(())
            }
        }
    }

    fn rollback_macos(self) -> Result<(), MacOsError> {
        match self {
            Self::Pending(transaction) => transaction
                .rollback()
                .map_err(|_| MacOsError::backend_failure()),
            Self::Complete(_) => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::Stub(rolled_back) => {
                rolled_back.set(true);
                Ok(())
            }
        }
    }
}

trait BundleProvisioner {
    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
    ) -> Result<BootstrapOutcome<'a>, ()>;
}

struct AuthenticatedProvisioner {
    bundle: Option<AuthenticatedInstallerBundle>,
}

impl AuthenticatedProvisioner {
    const fn new(bundle: AuthenticatedInstallerBundle) -> Self {
        Self {
            bundle: Some(bundle),
        }
    }
}

impl BundleProvisioner for AuthenticatedProvisioner {
    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
    ) -> Result<BootstrapOutcome<'a>, ()> {
        let bundle = self.bundle.take().ok_or(())?;
        provision_authenticated_installer_bundle_transaction(bundle, request, daemon)
            .map(Box::new)
            .map(BootstrapOutcome::Pending)
            .map_err(|_| ())
    }
}

struct LinuxBundleBackend<'a, P> {
    inner: &'a mut dyn LinuxInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome<'a>>,
}

impl<P: BundleProvisioner> LinuxInstallBackend for LinuxBundleBackend<'_, P> {
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError> {
        self.inner.preflight_clean_host(system)
    }
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        self.inner.ensure_asset(asset)
    }
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        self.inner.install_systemd_unit(asset, contents)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        self.outcome = Some(
            self.provisioner
                .provision(self.request, self.daemon)
                .map_err(|()| InstallError::backend_failure())?,
        );
        Ok(true)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        self.outcome
            .take()
            .map_or(Ok(()), BootstrapOutcome::rollback_linux)
    }
    fn activate_services(&mut self) -> Result<bool, InstallError> {
        self.inner.activate_services()
    }
    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.inner.rollback_services()
    }
    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        self.inner.check_managed_daemon()
    }
    fn publish_ownership_receipt(&mut self) -> Result<(), InstallError> {
        let outcome = self
            .outcome
            .take()
            .ok_or_else(InstallError::backend_failure)?;
        match outcome {
            BootstrapOutcome::Pending(mut transaction) => {
                if transaction.commit_channel().is_err() {
                    self.outcome = Some(BootstrapOutcome::Pending(transaction));
                    return Err(InstallError::backend_failure());
                }
                if let Err(error) = self.inner.publish_ownership_receipt() {
                    self.outcome = Some(BootstrapOutcome::Pending(transaction));
                    return Err(error);
                }
                let bootstrap = transaction
                    .finalize()
                    .map_err(|_| InstallError::backend_failure())?;
                self.outcome = Some(BootstrapOutcome::Complete(bootstrap));
                Ok(())
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                result
            }
            BootstrapOutcome::Complete(_) => Err(InstallError::backend_failure()),
        }
    }
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.inner.rollback_asset(asset)
    }
}

fn install_linux_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
) -> Result<(LinuxInstallReport, BootstrapOutcome<'a>), InstallError> {
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
    };
    let report = install_linux(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

struct MacOsBundleBackend<'a, P> {
    inner: &'a mut dyn MacOsInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome<'a>>,
}

impl<P: BundleProvisioner> MacOsInstallBackend for MacOsBundleBackend<'_, P> {
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError> {
        self.inner.preflight_clean_host(system)
    }
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_release_bundle()
    }
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        self.inner.provision_store_volume()
    }
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_store_volume()
    }
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        self.inner.ensure_asset(asset)
    }
    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        self.inner.install_launchd_plist(asset, contents)
    }
    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        self.inner.install_nix_config(asset)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
        self.outcome = Some(
            self.provisioner
                .provision(self.request, self.daemon)
                .map_err(|()| MacOsError::backend_failure())?,
        );
        Ok(true)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
        self.outcome
            .take()
            .map_or(Ok(()), BootstrapOutcome::rollback_macos)
    }
    fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_installed_code()
    }
    fn activate_services(&mut self) -> Result<bool, MacOsError> {
        self.inner.activate_services()
    }
    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_services()
    }
    fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
        self.inner.check_managed_daemon()
    }
    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError> {
        self.inner.observe_build_readiness(system)
    }
    fn publish_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        let outcome = self
            .outcome
            .take()
            .ok_or_else(MacOsError::backend_failure)?;
        match outcome {
            BootstrapOutcome::Pending(mut transaction) => {
                if transaction.commit_channel().is_err() {
                    self.outcome = Some(BootstrapOutcome::Pending(transaction));
                    return Err(MacOsError::backend_failure());
                }
                if let Err(error) = self.inner.publish_ownership_receipt() {
                    self.outcome = Some(BootstrapOutcome::Pending(transaction));
                    return Err(error);
                }
                let bootstrap = transaction
                    .finalize()
                    .map_err(|_| MacOsError::backend_failure())?;
                self.outcome = Some(BootstrapOutcome::Complete(bootstrap));
                Ok(())
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                result
            }
            BootstrapOutcome::Complete(_) => Err(MacOsError::backend_failure()),
        }
    }
    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.rollback_asset(asset)
    }
}

fn install_macos_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn MacOsInstallBackend,
    provisioner: &'a mut P,
) -> Result<(MacOsInstallReport, BootstrapOutcome<'a>), MacOsError> {
    let mut adapter = MacOsBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
    };
    let report = install_macos(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(MacOsError::backend_failure)?;
    Ok((report, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_nix::{DaemonError, ManagedGroupBindings, NixVersion};
    use std::path::Path;

    struct StubDaemon;

    impl ManagedDaemon for StubDaemon {
        fn start(
            &self,
            _root: &Path,
            _system: System,
            _version: &NixVersion,
        ) -> Result<(), DaemonError> {
            Ok(())
        }
        fn ping_store(&self) -> Result<(), DaemonError> {
            Ok(())
        }
        fn stop(&self) -> Result<(), DaemonError> {
            Ok(())
        }
    }

    struct StubProvisioner {
        calls: usize,
        rolled_back: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl BundleProvisioner for StubProvisioner {
        fn provision<'a>(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _daemon: &'a dyn ManagedDaemon,
        ) -> Result<BootstrapOutcome<'a>, ()> {
            self.calls = self.calls.saturating_add(1);
            Ok(BootstrapOutcome::Stub(self.rolled_back.clone()))
        }
    }

    #[derive(Default)]
    struct LinuxBackend {
        raw_provision_calls: usize,
        fail_health: bool,
    }

    impl LinuxInstallBackend for LinuxBackend {
        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
            Ok(())
        }
        fn ensure_asset(&mut self, _asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            Ok(false)
        }
        fn install_systemd_unit(
            &mut self,
            _asset: LinuxInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, InstallError> {
            Ok(false)
        }
        fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
            self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
            Err(InstallError::backend_failure())
        }
        fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn activate_services(&mut self) -> Result<bool, InstallError> {
            Ok(false)
        }
        fn rollback_services(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
            if self.fail_health {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn publish_ownership_receipt(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn rollback_asset(&mut self, _asset: LinuxInstallAsset) -> Result<(), InstallError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MacBackend {
        raw_provision_calls: usize,
        fail_health: bool,
    }

    impl MacOsInstallBackend for MacBackend {
        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), MacOsError> {
            Ok(())
        }
        fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
            Ok(false)
        }
        fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn ensure_asset(&mut self, _asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            Ok(false)
        }
        fn install_launchd_plist(
            &mut self,
            _asset: MacOsInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, MacOsError> {
            Ok(false)
        }
        fn install_nix_config(&mut self, _asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            Ok(false)
        }
        fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
            self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
            Err(MacOsError::backend_failure())
        }
        fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn activate_services(&mut self) -> Result<bool, MacOsError> {
            Ok(false)
        }
        fn rollback_services(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
            if self.fail_health {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn observe_build_readiness(
            &mut self,
            system: System,
        ) -> Result<MacOsBuildReadiness, MacOsError> {
            Ok(MacOsBuildReadiness::observed(
                system,
                crate::MacOsSandboxReadiness::Enforced,
                crate::MacOsBuildUsersReadiness::Ready,
                crate::MacOsToolchainReadiness::Ready,
            ))
        }
        fn publish_ownership_receipt(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn rollback_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            Ok(())
        }
    }

    #[test]
    fn linux_adapter_routes_runtime_only_through_authenticated_provisioner()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend::default();
        let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back,
        };
        let (report, outcome) = install_linux_with_provisioner(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
        )?;
        assert!(matches!(&outcome, BootstrapOutcome::Stub(_)));
        assert_eq!(report.created_artifacts(), 0);
        drop(outcome);
        assert_eq!(provisioner.calls, 1);
        assert_eq!(backend.raw_provision_calls, 0);
        Ok(())
    }

    #[test]
    fn linux_adapter_rolls_back_through_the_authenticated_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: rolled_back.clone(),
        };
        let mut backend = LinuxBackend {
            fail_health: true,
            ..LinuxBackend::default()
        };

        let result = install_linux_with_provisioner(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
        )
        .map(|_| ());

        assert_eq!(
            result.map_err(InstallError::code),
            Err(crate::InstallErrorCode::ServiceUnhealthy)
        );
        assert!(rolled_back.get());
        assert_eq!(backend.raw_provision_calls, 0);
        Ok(())
    }

    #[test]
    fn macos_adapter_rolls_back_through_the_authenticated_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::Aarch64Darwin,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: rolled_back.clone(),
        };
        let mut backend = MacBackend {
            fail_health: true,
            ..MacBackend::default()
        };

        let result = install_macos_with_provisioner(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
        )
        .map(|_| ());

        assert_eq!(
            result.map_err(MacOsError::code),
            Err(crate::MacOsErrorCode::ServiceUnhealthy)
        );
        assert!(rolled_back.get());
        assert_eq!(backend.raw_provision_calls, 0);
        Ok(())
    }
}
