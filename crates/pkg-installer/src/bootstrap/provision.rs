//! Authenticated bundle provisioning for one closed install attempt.

#[allow(
    clippy::wildcard_imports,
    reason = "the split module shares the bootstrap parent namespace"
)]
use super::*;
use super::{
    backend::{determinate_succeeded, run_with_new_determinate_handoff},
    recovery::{prepare_private_directory_at, prepare_vendor_tmp_directory_at},
};
pub(super) enum BootstrapOutcome {
    DeterminatePending {
        bundle: Box<AuthenticatedInstallerBundle>,
        handoff: Box<DeterminateHandoff>,
    },
    DeterminateComplete,
    Existing,
    #[cfg(test)]
    Stub(std::rc::Rc<std::cell::Cell<bool>>),
    #[cfg(test)]
    DeterminateTestPending(Box<DeterminateHandoff>),
}

impl BootstrapOutcome {
    pub(super) fn has_accepted_base_nix(&self) -> bool {
        match self {
            Self::DeterminatePending { handoff, .. } => {
                handoff.state() == Ok(DeterminateHandoffState::Accepted)
            }
            Self::Existing | Self::DeterminateComplete => true,
            #[cfg(test)]
            Self::DeterminateTestPending(handoff) => {
                handoff.state() == Ok(DeterminateHandoffState::Accepted)
            }
            #[cfg(test)]
            Self::Stub(_) => false,
        }
    }

    pub(super) fn into_linux_bootstrap(self) -> Result<(), InstallError> {
        match self {
            Self::Existing | Self::DeterminateComplete => Ok(()),
            Self::DeterminatePending { .. } => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Err(InstallError::backend_failure()),
        }
    }

    pub(super) fn into_macos_bootstrap(self) -> Result<(), MacOsError> {
        match self {
            Self::Existing | Self::DeterminateComplete => Ok(()),
            Self::DeterminatePending { .. } => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Err(MacOsError::backend_failure()),
        }
    }

    pub(super) fn rollback_linux(self) -> Result<(), InstallError> {
        match self {
            // Accepted Base Nix is already past the vendor rollback boundary.
            // Roll back only product state and retain the Fresh journal for continuation.
            Self::DeterminatePending { .. } => Ok(()),
            Self::Existing => Ok(()),
            Self::DeterminateComplete => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::Stub(rolled_back) => {
                rolled_back.set(true);
                Ok(())
            }
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Ok(()),
        }
    }

    pub(super) fn rollback_macos(self) -> Result<(), MacOsError> {
        match self {
            Self::Existing | Self::DeterminatePending { .. } => Ok(()),
            Self::DeterminateComplete => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::Stub(rolled_back) => {
                rolled_back.set(true);
                Ok(())
            }
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Err(MacOsError::backend_failure()),
        }
    }
}

pub(super) trait BundleProvisioner {
    fn reuse_existing(&mut self) -> Result<bool, BundleProvisionError> {
        Ok(false)
    }

    #[expect(
        dead_code,
        reason = "trait default method; no fake overrides the channel-commit hook"
    )]
    fn commit_authenticated_channel(&mut self) -> Result<(), BundleProvisionError> {
        Ok(())
    }

    fn reauthenticate_linux(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
        _backend: &mut dyn LinuxInstallBackend,
    ) -> Result<(), BundleProvisionError> {
        Ok(())
    }

    fn reauthenticate_macos(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
        _backend: &mut dyn MacOsInstallBackend,
    ) -> Result<(), BundleProvisionError> {
        Ok(())
    }

    fn preflight_workspace(
        &self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<(), BundleProvisionError> {
        Ok(())
    }

    fn provision(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError>;
}

#[derive(Clone, Copy)]
pub(super) enum BundleProvisionError {
    Failed,
    RollbackIncomplete,
}

pub(super) struct AuthenticatedProvisioner {
    trusted_root: Option<TrustedRoot>,
    bundle: Option<AuthenticatedInstallerBundle>,
}

impl AuthenticatedProvisioner {
    pub(super) const fn with_reauthentication(
        trusted_root: TrustedRoot,
        bundle: AuthenticatedInstallerBundle,
    ) -> Self {
        Self {
            trusted_root: Some(trusted_root),
            bundle: Some(bundle),
        }
    }
}

impl BundleProvisioner for AuthenticatedProvisioner {
    fn reuse_existing(&mut self) -> Result<bool, BundleProvisionError> {
        self.trusted_root = None;
        Ok(self.bundle.is_some())
    }

    fn commit_authenticated_channel(&mut self) -> Result<(), BundleProvisionError> {
        self.bundle
            .as_mut()
            .ok_or(BundleProvisionError::Failed)?
            .commit_authenticated_channel()
            .map_err(|_| BundleProvisionError::Failed)
    }

    fn reauthenticate_linux(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        backend: &mut dyn LinuxInstallBackend,
    ) -> Result<(), BundleProvisionError> {
        let trusted_root = self
            .trusted_root
            .take()
            .ok_or(BundleProvisionError::Failed)?;
        let bundle = self.bundle.take().ok_or(BundleProvisionError::Failed)?;
        let broker_uid = backend
            .broker_uid()
            .map_err(|_| BundleProvisionError::Failed)?;
        self.bundle = Some(
            reauthenticate_installer_bundle_blocking(trusted_root, request, bundle, broker_uid)
                .map_err(|_| BundleProvisionError::Failed)?,
        );
        Ok(())
    }

    fn reauthenticate_macos(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        backend: &mut dyn MacOsInstallBackend,
    ) -> Result<(), BundleProvisionError> {
        let trusted_root = self
            .trusted_root
            .take()
            .ok_or(BundleProvisionError::Failed)?;
        let bundle = self.bundle.take().ok_or(BundleProvisionError::Failed)?;
        let broker_uid = backend
            .broker_uid()
            .map_err(|_| BundleProvisionError::Failed)?;
        self.bundle = Some(
            reauthenticate_installer_bundle_blocking(trusted_root, request, bundle, broker_uid)
                .map_err(|_| BundleProvisionError::Failed)?,
        );
        Ok(())
    }

    fn preflight_workspace(
        &self,
        request: &InstallerProvisionRequest<'_>,
    ) -> Result<(), BundleProvisionError> {
        verify_provision_workspace_absent(request.scratch_parent)
            .map_err(|_| BundleProvisionError::Failed)
    }

    fn provision(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError> {
        let mut bundle = self.bundle.take().ok_or(BundleProvisionError::Failed)?;
        if matches!(
            request.system,
            System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
        ) {
            let (state, temporary) = if request.system == System::Aarch64Darwin {
                (MACOS_DETERMINATE_STATE, MACOS_DETERMINATE_TMP)
            } else {
                (LINUX_DETERMINATE_STATE, LINUX_DETERMINATE_TMP)
            };
            if request.system == System::Aarch64Darwin {
                prepare_private_directory_at(Path::new(state), 0, 0)
                    .and_then(|()| prepare_vendor_tmp_directory_at(Path::new(temporary), 0, 0))
                    .map_err(|_| BundleProvisionError::Failed)?;
            } else {
                prepare_private_directory_at(Path::new(state), 0, 0)
                    .and_then(|()| prepare_private_directory_at(Path::new(temporary), 0, 0))
                    .map_err(|_| BundleProvisionError::Failed)?;
            }
            let handoff =
                DeterminateHandoff::production().map_err(|_| BundleProvisionError::Failed)?;
            match handoff.state().map_err(|_| BundleProvisionError::Failed)? {
                DeterminateHandoffState::Accepted => {
                    // Keep the authenticated bundle so a repeat install can
                    // finish its receipt phase without dropping the channel.
                    self.bundle = Some(bundle);
                    return Ok(BootstrapOutcome::Existing);
                }
                DeterminateHandoffState::Started => return Err(BundleProvisionError::Failed),
                DeterminateHandoffState::NotStarted => {}
            }
            let staged = bundle
                .stage_determinate_installer(Path::new(temporary))
                .map_err(|_| BundleProvisionError::Failed)?;
            let installer = DeterminateInstaller::new(staged.length(), staged.sha256());
            let outcome = run_with_new_determinate_handoff(&handoff, || {
                installer
                    .install(staged.path())
                    .map_err(|_| BundleProvisionError::Failed)
            })?;
            if !determinate_succeeded(outcome) {
                return Err(BundleProvisionError::RollbackIncomplete);
            }
            return Ok(BootstrapOutcome::DeterminatePending {
                bundle: Box::new(bundle),
                handoff: Box::new(handoff),
            });
        }
        // Only the three supported systems reach the Determinate path above.
        // Every other system is unsupported and must fail closed.
        let _ = bundle;
        Err(BundleProvisionError::Failed)
    }
}
