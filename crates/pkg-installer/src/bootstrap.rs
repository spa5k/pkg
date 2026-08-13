//! Product installer entry points for authenticated managed-Nix bundles.

use std::os::unix::ffi::OsStrExt;

use crate::{
    InstallError, LinuxAssetPresence, LinuxInstallAsset, LinuxInstallBackend, LinuxInstallJournal,
    LinuxInstallJournalStorage, LinuxInstallMutation, LinuxInstallReport, MacOsBuildReadiness,
    MacOsError, MacOsInstallAsset, MacOsInstallBackend, MacOsInstallReport, install_macos,
    installer::{install_linux_preflighted, recover_linux_install},
};
use pkg_channel::TrustedRoot;
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerBundle, AuthenticatedManagedNixConfig, InstallerProvisionRequest,
    ManagedDaemon, ManagedRuntimeRemovalOutcome, OwnershipExpectation, ProvisionErrorCode,
    ProvisionedBootstrap, ProvisionedBootstrapTransaction, authenticate_installer_bundle_blocking,
    prepare_managed_runtime_removal_without_receipt,
    provision_authenticated_installer_bundle_transaction, recover_interrupted_provision_workspace,
    verify_provision_workspace_absent,
};
use sha2::{Digest as _, Sha256};

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
    backend.bind_authenticated_ownership_expectation(bundle.ownership_expectation())?;
    backend.preflight_privilege()?;
    let recovery_context_digest =
        linux_recovery_context_digest(bundle.asset_manifest_digest(), request);
    recover_linux_bundle_install(
        system,
        bundle.asset_manifest_digest(),
        recovery_context_digest,
        request,
        daemon,
        bundle.ownership_expectation(),
        backend,
    )?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| InstallError::backend_failure())?;
    // Journal creation is the durable proof that the fixed workspace was
    // absent before this attempt could create it.
    verify_provision_workspace_absent(request.scratch_parent)
        .map_err(|_| InstallError::backend_failure())?;
    let storage = LinuxInstallJournalStorage::prepare(
        system,
        bundle.asset_manifest_digest(),
        recovery_context_digest,
    )
    .map_err(|_| InstallError::backend_failure())?;
    let journal = LinuxInstallJournal::new(
        system,
        bundle.asset_manifest_digest(),
        recovery_context_digest,
    )
    .map_err(|_| InstallError::backend_failure())?;
    storage
        .create(&journal)
        .map_err(|_| InstallError::backend_failure())?;
    let mut provisioner = AuthenticatedProvisioner::new(bundle);
    let installation = install_linux_with_provisioner_journaled(
        system,
        request,
        daemon,
        backend,
        &mut provisioner,
        &storage,
        journal,
    );
    let (platform, outcome) = match installation {
        Ok(success) => success,
        Err(error) => {
            if error.code() != crate::InstallErrorCode::RollbackIncomplete {
                storage
                    .remove()
                    .map_err(|_| InstallError::backend_failure())?;
            }
            return Err(error);
        }
    };
    storage
        .remove()
        .map_err(|_| InstallError::backend_failure())?;
    let bootstrap = outcome.into_linux_bootstrap()?;
    Ok(LinuxBundleInstallReport {
        platform,
        bootstrap,
    })
}

fn recover_linux_bundle_install(
    system: System,
    digest: pkg_core::state::Digest,
    recovery_context_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
    daemon: &dyn ManagedDaemon,
    expectation: &OwnershipExpectation,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<(), InstallError> {
    let Some(storage) =
        LinuxInstallJournalStorage::open_existing(system, digest, recovery_context_digest)
            .map_err(|_| InstallError::backend_failure())?
    else {
        return Ok(());
    };
    if let Some(mut journal) = storage
        .load()
        .map_err(|_| InstallError::backend_failure())?
        && !journal.is_committed()
    {
        // This journal was created only after the fixed workspace was absent.
        // Cleanup is independent of runtime presence, including reinstalls.
        recover_interrupted_provision_workspace(request.scratch_parent)
            .map_err(|_| InstallError::backend_failure())?;
        recover_linux_install(
            &mut journal,
            backend,
            &mut || {
                if let Some(removal) = prepare_managed_runtime_removal_without_receipt(
                    request.installation_root,
                    expectation,
                )
                .map_err(|_| InstallError::backend_failure())?
                {
                    daemon
                        .rollback_runtime_registration()
                        .map_err(|_| InstallError::backend_failure())?;
                    if removal
                        .remove()
                        .map_err(|_| InstallError::backend_failure())?
                        != ManagedRuntimeRemovalOutcome::Removed
                    {
                        return Err(InstallError::backend_failure());
                    }
                }
                // Empty outer /nix directories are fixed platform assets. The
                // later reverse journal actions remove them after this step.
                Ok(())
            },
            &mut |journal| {
                storage
                    .replace(journal)
                    .map_err(|_| InstallError::backend_failure())
            },
        )?;
    }
    storage
        .remove()
        .map_err(|_| InstallError::backend_failure())
}

fn linux_recovery_context_digest(
    ownership_manifest_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"pkg-linux-install-recovery-v1\0");
    hasher.update(ownership_manifest_digest.as_bytes());
    for path in [request.installation_root, request.scratch_parent] {
        let bytes = path.as_os_str().as_bytes();
        hasher.update(bytes.len().to_be_bytes());
        hasher.update(bytes);
    }
    Digest::from_bytes(hasher.finalize().into())
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
    backend.bind_authenticated_ownership_expectation(bundle.ownership_expectation())?;
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
    fn preflight_workspace(
        &self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<(), BundleProvisionError> {
        Ok(())
    }

    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
    ) -> Result<BootstrapOutcome<'a>, BundleProvisionError>;
}

#[derive(Clone, Copy)]
enum BundleProvisionError {
    Failed,
    RollbackIncomplete,
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
    fn preflight_workspace(
        &self,
        request: &InstallerProvisionRequest<'_>,
    ) -> Result<(), BundleProvisionError> {
        verify_provision_workspace_absent(request.scratch_parent)
            .map_err(|_| BundleProvisionError::Failed)
    }

    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
    ) -> Result<BootstrapOutcome<'a>, BundleProvisionError> {
        let bundle = self.bundle.take().ok_or(BundleProvisionError::Failed)?;
        provision_authenticated_installer_bundle_transaction(bundle, request, daemon)
            .map(Box::new)
            .map(BootstrapOutcome::Pending)
            .map_err(|error| {
                if error.code() == ProvisionErrorCode::RollbackFailed {
                    BundleProvisionError::RollbackIncomplete
                } else {
                    BundleProvisionError::Failed
                }
            })
    }
}

struct LinuxBundleBackend<'a, 'j, P> {
    inner: &'a mut dyn LinuxInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome<'a>>,
    journal: Option<LinuxJournalTransaction<'j>>,
}

struct LinuxJournalTransaction<'a> {
    storage: &'a dyn LinuxJournalPersistence,
    journal: LinuxInstallJournal,
}

trait LinuxJournalPersistence {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError>;
}

impl LinuxJournalPersistence for LinuxInstallJournalStorage {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
        Self::replace(self, journal).map_err(|_| InstallError::backend_failure())
    }
}

impl LinuxJournalTransaction<'_> {
    fn begin(
        &mut self,
        mutation: LinuxInstallMutation,
        presence: LinuxAssetPresence,
    ) -> Result<(), InstallError> {
        if presence == LinuxAssetPresence::Absent {
            self.journal
                .intend(mutation)
                .map_err(|_| InstallError::backend_failure())?;
            self.persist()?;
        }
        Ok(())
    }

    fn complete(
        &mut self,
        mutation: LinuxInstallMutation,
        presence: LinuxAssetPresence,
        changed: bool,
    ) -> Result<(), InstallError> {
        if changed != (presence == LinuxAssetPresence::Absent) {
            return Err(InstallError::backend_failure());
        }
        if changed {
            self.journal
                .complete_created()
                .map_err(|_| InstallError::backend_failure())?;
        } else {
            self.journal
                .record_preexisting(mutation)
                .map_err(|_| InstallError::backend_failure())?;
        }
        self.persist()
    }

    fn commit(&mut self) -> Result<(), InstallError> {
        self.journal
            .commit()
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.storage
            .replace(&self.journal)
            .map_err(|_| InstallError::rollback_incomplete())
    }

    fn persist(&self) -> Result<(), InstallError> {
        self.storage.replace(&self.journal)
    }
}

impl<P: BundleProvisioner> LinuxInstallBackend for LinuxBundleBackend<'_, '_, P> {
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_ownership_expectation(
        &mut self,
        expectation: &OwnershipExpectation,
    ) -> Result<(), InstallError> {
        self.inner
            .bind_authenticated_ownership_expectation(expectation)
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError> {
        self.inner.preflight_clean_host(system)
    }
    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<crate::LinuxAssetPresence, InstallError> {
        self.inner.classify_asset(asset)
    }
    fn classify_managed_runtime(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.inner.classify_managed_runtime()
    }
    fn classify_services(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.inner.classify_services()
    }
    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.inner.recover_asset(asset)
    }
    fn recover_services(&mut self) -> Result<(), InstallError> {
        self.inner.recover_services()
    }
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.ensure_asset(asset)?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.install_systemd_unit(asset, contents)?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        let mutation = LinuxInstallMutation::ManagedRuntime;
        let presence = self.inner.classify_managed_runtime()?;
        self.provisioner
            .preflight_workspace(self.request)
            .map_err(linux_provision_error)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.outcome = Some(
            self.provisioner
                .provision(self.request, self.daemon)
                .map_err(linux_provision_error)?,
        );
        let changed = presence == LinuxAssetPresence::Absent;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        // Reused runtimes still start a temporary daemon. On Linux it has a
        // parent-death signal. A later start removes only its stale root socket.
        Ok(true)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        self.outcome
            .take()
            .map_or(Ok(()), BootstrapOutcome::rollback_linux)
    }
    fn activate_services(&mut self) -> Result<bool, InstallError> {
        let mutation = LinuxInstallMutation::Services;
        let presence = self.inner.classify_services()?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.activate_services()?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.inner.rollback_services()
    }
    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        self.inner.check_managed_daemon()
    }
    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(InstallError::backend_failure)?;
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        // An exact receipt is verified and reused by the production backend.
        // It is not rewritten on reinstall.
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
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
                let receipt_created = match self.inner.publish_ownership_receipt() {
                    Ok(created) => created,
                    Err(error) => {
                        self.outcome = Some(BootstrapOutcome::Pending(transaction));
                        return Err(error);
                    }
                };
                if let Err(error) =
                    complete_linux_mutation(&mut self.journal, mutation, presence, receipt_created)
                {
                    self.outcome = Some(BootstrapOutcome::Pending(transaction));
                    return Err(error);
                }
                let bootstrap = transaction
                    .finalize()
                    .map_err(|_| InstallError::backend_failure())?;
                self.outcome = Some(BootstrapOutcome::Complete(bootstrap));
                Ok(receipt_created)
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                let changed = result?;
                complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
                Ok(changed)
            }
            BootstrapOutcome::Complete(_) => Err(InstallError::backend_failure()),
        }
    }
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.inner.rollback_asset(asset)
    }
}

fn asset_mutation(asset: LinuxInstallAsset) -> LinuxInstallMutation {
    LinuxInstallMutation::Asset {
        id: asset.id().to_owned(),
    }
}

fn begin_linux_mutation(
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
) -> Result<(), InstallError> {
    journal
        .as_mut()
        .map_or(Ok(()), |journal| journal.begin(mutation, presence))
}

fn complete_linux_mutation(
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
    changed: bool,
) -> Result<(), InstallError> {
    journal.as_mut().map_or(Ok(()), |journal| {
        journal.complete(mutation, presence, changed)
    })
}

fn install_linux_with_provisioner_journaled<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
    storage: &dyn LinuxJournalPersistence,
    journal: LinuxInstallJournal,
) -> Result<(LinuxInstallReport, BootstrapOutcome<'a>), InstallError> {
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
        journal: Some(LinuxJournalTransaction { storage, journal }),
    };
    let report = install_linux_preflighted(system, &mut adapter)?;
    adapter
        .journal
        .as_mut()
        .ok_or_else(InstallError::rollback_incomplete)?
        .commit()?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

#[cfg(test)]
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
        journal: None,
    };
    let report = crate::install_linux(system, &mut adapter)?;
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

    fn bind_authenticated_ownership_expectation(
        &mut self,
        expectation: &OwnershipExpectation,
    ) -> Result<(), MacOsError> {
        self.inner
            .bind_authenticated_ownership_expectation(expectation)
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
                .map_err(macos_provision_error)?,
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

const fn linux_provision_error(error: BundleProvisionError) -> InstallError {
    match error {
        BundleProvisionError::Failed => InstallError::backend_failure(),
        BundleProvisionError::RollbackIncomplete => InstallError::rollback_incomplete(),
    }
}

const fn macos_provision_error(error: BundleProvisionError) -> MacOsError {
    match error {
        BundleProvisionError::Failed => MacOsError::backend_failure(),
        BundleProvisionError::RollbackIncomplete => MacOsError::rollback_incomplete(),
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
    use pkg_core::state::Digest;
    use pkg_nix::{DaemonError, ManagedGroupBindings, NixVersion};
    use std::{cell::RefCell, path::Path};

    #[test]
    fn linux_recovery_context_binds_installation_and_scratch_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let digest = Digest::from_bytes([0x90; 32]);
        let groups = ManagedGroupBindings::new(100, 101)?;
        let context = |installation_root: &Path, scratch_parent: &Path| {
            linux_recovery_context_digest(
                digest,
                &InstallerProvisionRequest {
                    bundle_root: Path::new("/bundle"),
                    datastore: Path::new("/state"),
                    installation_root,
                    scratch_parent,
                    system: System::X8664Linux,
                    groups,
                },
            )
        };

        let expected = context(Path::new("/"), Path::new("/scratch"));
        assert_ne!(
            expected,
            context(Path::new("/target"), Path::new("/scratch"))
        );
        assert_ne!(
            expected,
            context(Path::new("/"), Path::new("/other-scratch"))
        );
        Ok(())
    }

    #[derive(Default)]
    struct MemoryJournalPersistence {
        snapshots: RefCell<Vec<LinuxInstallJournal>>,
    }

    impl LinuxJournalPersistence for MemoryJournalPersistence {
        fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
            self.snapshots.borrow_mut().push(journal.clone());
            Ok(())
        }
    }

    struct StubDaemon;

    impl ManagedDaemon for StubDaemon {
        fn register_runtime(
            &self,
            _root: &Path,
            _system: System,
            _version: &NixVersion,
            _registration: &Path,
        ) -> Result<(), DaemonError> {
            Ok(())
        }
        fn commit_runtime_registration(&self) -> Result<(), DaemonError> {
            Ok(())
        }
        fn rollback_runtime_registration(&self) -> Result<(), DaemonError> {
            Ok(())
        }
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
        ) -> Result<BootstrapOutcome<'a>, BundleProvisionError> {
            self.calls = self.calls.saturating_add(1);
            Ok(BootstrapOutcome::Stub(self.rolled_back.clone()))
        }
    }

    struct RollbackFailedProvisioner;

    impl BundleProvisioner for RollbackFailedProvisioner {
        fn provision<'a>(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _daemon: &'a dyn ManagedDaemon,
        ) -> Result<BootstrapOutcome<'a>, BundleProvisionError> {
            Err(BundleProvisionError::RollbackIncomplete)
        }
    }

    #[derive(Default)]
    struct LinuxBackend {
        raw_provision_calls: usize,
        fail_health: bool,
        create: bool,
        fail_asset: bool,
    }

    impl LinuxInstallBackend for LinuxBackend {
        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn bind_authenticated_ownership_expectation(
            &mut self,
            _expectation: &OwnershipExpectation,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
            Ok(())
        }
        fn classify_asset(
            &mut self,
            _asset: LinuxInstallAsset,
        ) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(if self.create {
                crate::LinuxAssetPresence::Absent
            } else {
                crate::LinuxAssetPresence::ExactPresent
            })
        }
        fn classify_managed_runtime(&mut self) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(if self.create {
                crate::LinuxAssetPresence::Absent
            } else {
                crate::LinuxAssetPresence::ExactPresent
            })
        }
        fn classify_services(&mut self) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(if self.create {
                crate::LinuxAssetPresence::Absent
            } else {
                crate::LinuxAssetPresence::ExactPresent
            })
        }
        fn ensure_asset(&mut self, _asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            if self.fail_asset {
                Err(InstallError::backend_failure())
            } else {
                Ok(self.create)
            }
        }
        fn install_systemd_unit(
            &mut self,
            _asset: LinuxInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, InstallError> {
            Ok(self.create)
        }
        fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
            self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
            Err(InstallError::backend_failure())
        }
        fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn activate_services(&mut self) -> Result<bool, InstallError> {
            Ok(self.create)
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
        fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
            Ok(self.create)
        }
        fn rollback_asset(&mut self, _asset: LinuxInstallAsset) -> Result<(), InstallError> {
            Ok(())
        }
    }

    #[test]
    fn journaled_linux_install_persists_each_intent_completion_and_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x91; 32]),
            Digest::from_bytes([0xa1; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        let (report, outcome) = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )?;

        assert!(matches!(outcome, BootstrapOutcome::Stub(_)));
        assert_eq!(
            report.created_artifacts(),
            crate::linux_install_assets().len()
        );
        let snapshots = persistence.snapshots.borrow();
        assert!(snapshots.len() > crate::linux_install_assets().len());
        assert!(
            snapshots
                .last()
                .is_some_and(LinuxInstallJournal::is_committed)
        );
        Ok(())
    }

    #[test]
    fn journaled_linux_install_keeps_uncertain_intent_on_mutation_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x92; 32]),
            Digest::from_bytes([0xa2; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            fail_asset: true,
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.kind() != crate::LinuxAssetKind::File)
            .ok_or_else(|| std::io::Error::other("missing fixed Linux asset"))?;
        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )
        .map(|_| ());

        assert_eq!(
            result.map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure)
        );
        let mutation = asset_mutation(asset);
        assert_eq!(
            persistence
                .snapshots
                .borrow()
                .last()
                .ok_or_else(|| std::io::Error::other("missing intent snapshot"))?
                .mutation_state(&mutation)?,
            Some(crate::LinuxInstallMutationState::Intended)
        );
        Ok(())
    }

    #[test]
    fn journaled_linux_install_preserves_provision_rollback_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x95; 32]),
            Digest::from_bytes([0xa5; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut RollbackFailedProvisioner,
            &persistence,
            journal,
        )
        .map(|_| ());

        assert_eq!(
            result.map_err(InstallError::code),
            Err(crate::InstallErrorCode::RollbackIncomplete)
        );
        assert_eq!(
            persistence
                .snapshots
                .borrow()
                .last()
                .ok_or_else(|| std::io::Error::other("missing runtime intent snapshot"))?
                .mutation_state(&LinuxInstallMutation::ManagedRuntime)?,
            Some(crate::LinuxInstallMutationState::Intended)
        );
        Ok(())
    }

    #[test]
    fn journaled_linux_reinstall_records_exact_state_without_created_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x93; 32]),
            Digest::from_bytes([0xa3; 32]),
        )?;
        let request = InstallerProvisionRequest {
            bundle_root: Path::new("/bundle"),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend::default();
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };

        let (report, outcome) = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )?;

        assert!(matches!(outcome, BootstrapOutcome::Stub(_)));
        assert_eq!(report.created_artifacts(), 0);
        assert_eq!(
            report.existing_artifacts(),
            crate::linux_install_assets().len()
        );
        assert!(
            persistence
                .snapshots
                .borrow()
                .last()
                .is_some_and(LinuxInstallJournal::is_committed)
        );
        Ok(())
    }

    #[test]
    fn journaled_linux_reinstall_rolls_back_its_temporary_daemon()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            System::X8664Linux,
            Digest::from_bytes([0x94; 32]),
            Digest::from_bytes([0xa4; 32]),
        )?;
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

        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )
        .map(|_| ());

        assert_eq!(
            result.map_err(InstallError::code),
            Err(crate::InstallErrorCode::ServiceUnhealthy)
        );
        assert!(rolled_back.get());
        Ok(())
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

        fn bind_authenticated_ownership_expectation(
            &mut self,
            _expectation: &OwnershipExpectation,
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
