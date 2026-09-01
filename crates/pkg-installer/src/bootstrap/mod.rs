//! Product installer entry points for authenticated managed-Nix bundles.

// Re-exported submodule paths keep the module surface unchanged.
mod backend;
mod provision;
mod recovery;
#[cfg(test)]
mod tests;
use backend::install_macos_with_provisioner_journaled;
use provision::AuthenticatedProvisioner;
pub(crate) use recovery::validate_linux_auth_datastore_file;
use recovery::{
    LinuxJournalLocation, continue_linux_bundle_install, load_linux_bundle_for_recovery,
    load_macos_bundle_for_recovery, macos_recovery_context_digest, recover_macos_bundle_install,
};
use std::{
    ffi::OsStr,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use crate::{
    AssetPresence, InstallError, LinuxInstallAsset, LinuxInstallBackend, LinuxInstallJournal,
    LinuxInstallJournalFileError, LinuxInstallJournalFileErrorCode, LinuxInstallJournalStorage,
    LinuxInstallMutation, LinuxInstallReport, LinuxReleasePayloads, MacOsBuildReadiness,
    MacOsError, MacOsInstallAsset, MacOsInstallBackend, MacOsInstallJournal,
    MacOsInstallJournalStorage, MacOsInstallMutation, MacOsInstallReport,
    ProductionLinuxUninstallBackend, ProductionMacOsUninstallBackend, UninstallError,
    UninstallErrorCode,
    determinate::{DeterminateInstaller, DeterminateProcessOutcome, DeterminateTerminal},
    determinate_handoff::{DeterminateHandoff, DeterminateHandoffState},
    execute_uninstall, install_macos,
    installer::recover_linux_install,
    linux_uninstall::verify_linux_install_absent,
    macos_uninstall::verify_macos_install_absent,
    service::production_release_inputs,
    uninstall::preflight_uninstall,
};
use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

/// The macOS install journal storage and its opened journal.
type MacOsJournalPair = (MacOsInstallJournalStorage, MacOsInstallJournal);
use pkg_channel::TrustedRoot;
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerBundle, AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig,
    InstallerProvisionRequest, InstallerRepository, load_authenticated_installer_bundle_blocking,
    reauthenticate_installer_bundle_blocking, recover_interrupted_provision_workspace,
    verify_provision_workspace_absent,
};
use sha2::{Digest as _, Sha256};

const LINUX_AUTH_DATASTORE: &str = "/run/pkg-install-auth";
const LINUX_CHANNEL_DATASTORE: &str = "/var/lib/pkg/broker-home/channel";
const LINUX_SCRATCH_PARENT: &str = "/var/lib/pkg/helper-home/tmp";
const LINUX_DETERMINATE_STATE: &str = "/var/lib/pkg-install";
const LINUX_DETERMINATE_TMP: &str = "/var/lib/pkg-install/tmp";
const MACOS_DETERMINATE_STATE: &str = "/private/var/db/pkg-install";
const MACOS_DETERMINATE_TMP: &str = "/private/var/db/pkg-install-tmp";
const MACOS_AUTH_DATASTORE: &str = "/private/var/db/pkg-install-auth";
const MACOS_CHANNEL_DATASTORE: &str = "/Library/Application Support/pkg/broker-home/channel";
const MACOS_SCRATCH_PARENT: &str = "/Library/Application Support/pkg/helper-home/tmp";

/// Successful Linux installation and its authenticated runtime/index result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBundleInstallReport {
    platform: LinuxInstallReport,
}

impl LinuxBundleInstallReport {
    /// Returns the platform installation report.
    #[must_use]
    pub const fn platform(&self) -> LinuxInstallReport {
        self.platform
    }
}

/// Successful macOS installation and its authenticated runtime/index result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsBundleInstallReport {
    platform: MacOsInstallReport,
}

impl MacOsBundleInstallReport {
    /// Returns the platform installation report.
    #[must_use]
    pub const fn platform(&self) -> MacOsInstallReport {
        self.platform
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
    backend: &'a mut dyn LinuxInstallBackend,
) -> Result<LinuxBundleInstallReport, InstallError> {
    if request.system != system {
        return Err(InstallError::backend_failure());
    }
    backend.preflight_privilege()?;
    let bundle = load_linux_bundle_for_recovery(trusted_root.clone(), request)?;
    backend.bind_authenticated_installer_payloads(bundle.installer_payloads())?;
    backend.bind_authenticated_nix_config(bundle.managed_nix_config())?;
    let release_digest = bundle.release_identity_digest();
    backend.bind_authenticated_release_identity(bundle.system(), release_digest)?;
    let mut provisioner = AuthenticatedProvisioner::with_reauthentication(trusted_root, bundle);
    continue_linux_bundle_install(
        request,
        backend,
        &mut provisioner,
        release_digest,
        &LinuxJournalLocation::Production,
    )
}

/// Authenticates the fixed production release and uninstalls its Linux assets.
///
/// A dry run performs all read-only ownership and foreign-state checks but no
/// service, account, store, or installed-file mutation.
///
/// # Errors
///
/// Returns a stable redacted uninstall failure.
pub fn uninstall_linux_production(dry_run: bool) -> Result<usize, UninstallError> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(UninstallError::new(UninstallErrorCode::PrivilegeRequired));
    }
    let system = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => System::X8664Linux,
        ("aarch64", "linux") => System::Aarch64Linux,
        (_, _) => return Err(UninstallError::backend_failure()),
    };
    if verify_linux_install_absent().is_ok() {
        return Ok(0);
    }

    let groups = crate::plan_linux_group_bindings()
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    // These URLs are compiled into this installed CLI and name its immutable
    // release publication. They do not follow the latest product channel.
    let (trusted_root, metadata_url, targets_url) = production_release_inputs()
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let request = InstallerProvisionRequest {
        repository: InstallerRepository::Remote {
            metadata_url: &metadata_url,
            targets_url: &targets_url,
        },
        datastore: Path::new(LINUX_CHANNEL_DATASTORE),
        installation_root: Path::new("/"),
        scratch_parent: Path::new(LINUX_SCRATCH_PARENT),
        system,
        groups,
    };
    let bundle = load_linux_bundle_for_recovery(trusted_root, &request)
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let (determinate_length, determinate_sha256) = bundle
        .determinate_installer_identity()
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let payloads = LinuxReleasePayloads::from_authenticated_bundle(bundle.installer_payloads())
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let mut backend = ProductionLinuxUninstallBackend::new(
        bundle.system(),
        bundle.release_identity_digest(),
        request.groups,
        bundle.managed_nix_config(),
        payloads,
        DeterminateInstaller::new(determinate_length, determinate_sha256),
    )?;
    let manifest = backend
        .installed_manifest()?
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let plan = crate::plan_uninstall(&manifest)?;
    if dry_run {
        preflight_uninstall(&manifest, &plan, &mut backend)?;
        Ok(plan.actions().len())
    } else {
        execute_uninstall(&manifest, &plan, &mut backend)
            .map(crate::UninstallReport::completed_actions)
    }
}

/// Authenticates the fixed production release and uninstalls its macOS assets.
///
/// # Errors
///
/// Returns a stable refusal for insufficient privilege, invalid release data,
/// changed ownership state, incomplete cleanup, or unsupported hosts.
pub fn uninstall_macos_production(dry_run: bool) -> Result<usize, UninstallError> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(UninstallError::new(UninstallErrorCode::PrivilegeRequired));
    }
    let system = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => System::Aarch64Darwin,
        (_, _) => return Err(UninstallError::backend_failure()),
    };
    if verify_macos_install_absent().is_ok() {
        let handoff = DeterminateHandoff::production()
            .and_then(|handoff| handoff.state())
            .map_err(|_| UninstallError::backend_failure())?;
        match handoff {
            DeterminateHandoffState::Accepted => {}
            DeterminateHandoffState::NotStarted if matches!(Path::new("/nix").symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(0);
            }
            DeterminateHandoffState::NotStarted | DeterminateHandoffState::Started => {
                return Err(UninstallError::backend_failure());
            }
        }
    }
    let groups = pkg_nix::ManagedGroupBindings::new(333, crate::macos_accounts::BUILD_GID)
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let (trusted_root, metadata_url, targets_url) = production_release_inputs()
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let request = InstallerProvisionRequest {
        repository: InstallerRepository::Remote {
            metadata_url: &metadata_url,
            targets_url: &targets_url,
        },
        datastore: Path::new(MACOS_CHANNEL_DATASTORE),
        installation_root: Path::new("/"),
        scratch_parent: Path::new(MACOS_SCRATCH_PARENT),
        system,
        groups,
    };
    let bundle = load_macos_bundle_for_recovery(trusted_root, &request)
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let (determinate_length, determinate_sha256) = bundle
        .determinate_installer_identity()
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let mut backend = ProductionMacOsUninstallBackend::new(
        system,
        bundle.release_identity_digest(),
        groups,
        bundle.installer_payloads(),
        DeterminateInstaller::new(determinate_length, determinate_sha256),
    )?;
    let manifest = backend
        .installed_manifest()?
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    let plan = crate::plan_uninstall(&manifest)?;
    if dry_run {
        preflight_uninstall(&manifest, &plan, &mut backend)?;
        Ok(plan.actions().len())
    } else {
        execute_uninstall(&manifest, &plan, &mut backend)
            .map(crate::UninstallReport::completed_actions)
    }
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
    backend: &'a mut dyn MacOsInstallBackend,
) -> Result<MacOsBundleInstallReport, MacOsError> {
    if request.system != system || system != System::Aarch64Darwin {
        return Err(MacOsError::backend_failure());
    }
    backend.preflight_privilege()?;
    let bundle = load_macos_bundle_for_recovery(trusted_root.clone(), request)?;
    backend.bind_authenticated_installer_payloads(bundle.installer_payloads())?;
    backend.bind_authenticated_nix_config(bundle.managed_nix_config())?;
    let release_identity_digest = bundle.release_identity_digest();
    backend.bind_authenticated_release_identity(system, release_identity_digest)?;
    let recovery_context_digest = macos_recovery_context_digest(release_identity_digest, request);
    let recovery = recover_macos_bundle_install(
        system,
        release_identity_digest,
        recovery_context_digest,
        request,
        backend,
    )?;
    backend.preflight_clean_host(system)?;
    verify_provision_workspace_absent(request.scratch_parent)
        .map_err(|_| MacOsError::backend_failure())?;
    let (storage, journal) = if let Some(recovered) = recovery {
        recovered
    } else {
        let storage = MacOsInstallJournalStorage::prepare(
            system,
            release_identity_digest,
            recovery_context_digest,
        )
        .map_err(|_| MacOsError::backend_failure())?;
        let journal = MacOsInstallJournal::new_with_mode(
            system,
            release_identity_digest,
            recovery_context_digest,
            backend.install_mode(),
        )
        .map_err(|_| MacOsError::backend_failure())?;
        storage
            .create(&journal)
            .map_err(|_| MacOsError::backend_failure())?;
        (storage, journal)
    };
    let mut provisioner = AuthenticatedProvisioner::with_reauthentication(trusted_root, bundle);
    let installation = install_macos_with_provisioner_journaled(
        system,
        request,
        backend,
        &mut provisioner,
        &storage,
        journal,
    );
    let (platform, outcome) = match installation {
        Ok(success) => success,
        Err(error) => {
            if error.code() != crate::MacOsErrorCode::RollbackIncomplete {
                storage
                    .remove()
                    .map_err(|_| MacOsError::backend_failure())?;
            }
            return Err(error);
        }
    };
    storage
        .remove()
        .map_err(|_| MacOsError::backend_failure())?;
    outcome.into_macos_bootstrap()?;
    Ok(MacOsBundleInstallReport { platform })
}
