//! Product installer entry points for authenticated managed-Nix bundles.

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
    InstallError, LinuxAssetPresence, LinuxInstallAsset, LinuxInstallBackend, LinuxInstallJournal,
    LinuxInstallJournalFileError, LinuxInstallJournalFileErrorCode, LinuxInstallJournalStorage,
    LinuxInstallMutation, LinuxInstallReport, LinuxReleasePayloads, MacOsAssetPresence,
    MacOsBuildReadiness, MacOsError, MacOsInstallAsset, MacOsInstallBackend, MacOsInstallJournal,
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
use pkg_channel::TrustedRoot;
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedInstallerBundle, AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig,
    InstallerProvisionRequest, InstallerRepository, ManagedDaemon,
    load_authenticated_installer_bundle_blocking, reauthenticate_installer_bundle_blocking,
    recover_interrupted_provision_workspace, verify_provision_workspace_absent,
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
    daemon: &'a dyn ManagedDaemon,
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
        daemon,
        backend,
        &mut provisioner,
        release_digest,
        &LinuxJournalLocation::Production,
    )
}

fn continue_linux_bundle_install<'a, P: BundleProvisioner>(
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
    release_digest: Digest,
    journal_location: &LinuxJournalLocation,
) -> Result<LinuxBundleInstallReport, InstallError> {
    let recovery_context_digest = linux_recovery_context_digest(release_digest, request);
    let recovery = recover_linux_bundle_install(
        request.system,
        release_digest,
        recovery_context_digest,
        request,
        backend,
        journal_location,
    )?;
    if matches!(&recovery, LinuxBundleRecovery::Committed) {
        return Ok(LinuxBundleInstallReport {
            platform: LinuxInstallReport::recovered_existing(),
        });
    }
    if matches!(&recovery, LinuxBundleRecovery::None) && backend.classify_active_install()? {
        return Ok(LinuxBundleInstallReport {
            platform: LinuxInstallReport::recovered_existing(),
        });
    }
    backend.preflight_clean_host(request.system)?;
    // Journal creation is the durable proof that the fixed workspace was
    // absent before this attempt could create it.
    verify_provision_workspace_absent(request.scratch_parent)
        .map_err(|_| InstallError::backend_failure())?;
    let (storage, journal) = match recovery {
        LinuxBundleRecovery::Fresh { storage, journal } => (storage, *journal),
        LinuxBundleRecovery::None => {
            let storage = journal_location
                .prepare(request.system, release_digest, recovery_context_digest)
                .map_err(|_| InstallError::backend_failure())?;
            let journal = LinuxInstallJournal::new(
                backend.install_mode(),
                request.system,
                release_digest,
                recovery_context_digest,
            )
            .map_err(|_| InstallError::backend_failure())?;
            storage
                .create(&journal)
                .map_err(|_| InstallError::backend_failure())?;
            (storage, journal)
        }
        LinuxBundleRecovery::Committed => return Err(InstallError::backend_failure()),
    };
    // Keep the original request so final state is broker-owned and durable,
    // instead of root-owned and temporary under /run.
    let installation = install_linux_with_provisioner_journaled(
        request.system,
        request,
        daemon,
        backend,
        provisioner,
        &storage,
        journal,
    );
    let (platform, outcome) = installation?;
    storage
        .remove()
        .map_err(|_| InstallError::backend_failure())?;
    outcome.into_linux_bootstrap()?;
    Ok(LinuxBundleInstallReport { platform })
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

/// Loads the authenticated Linux release without a clean-host authorization scan.
///
/// Journal recovery must authenticate the signed release identity before it
/// can reconcile an interrupted transaction, and an interrupted attempt may
/// have already created fixed platform prerequisites such as build users that
/// a strict pre-mutation scan must refuse to treat as clean. The strict
/// privileged clean-host scan still runs in the caller after recovery and
/// before any new mutation, matching the reviewed macOS flow.
fn load_linux_bundle_for_recovery(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, InstallError> {
    let auth_datastore = prepare_linux_auth_datastore()?;
    let auth_request = InstallerProvisionRequest {
        repository: request.repository,
        datastore: &auth_datastore,
        installation_root: request.installation_root,
        scratch_parent: request.scratch_parent,
        system: request.system,
        groups: request.groups,
    };
    let result = load_authenticated_installer_bundle_blocking(trusted_root, &auth_request)
        .map_err(|_| InstallError::backend_failure());
    remove_linux_auth_datastore(&auth_datastore)?;
    result
}

fn prepare_linux_auth_datastore() -> Result<PathBuf, InstallError> {
    let uid = nix::unistd::Uid::effective().as_raw();
    let gid = nix::unistd::Gid::effective().as_raw();
    if uid != 0 || gid != 0 {
        return Err(InstallError::backend_failure());
    }
    let root = PathBuf::from(LINUX_AUTH_DATASTORE);
    prepare_private_directory_at(&root, uid, gid)?;
    remove_legacy_linux_auth_datastore_files(&root, uid, gid)?;
    remove_stale_linux_auth_datastores(&root, uid, gid)?;
    let path = root.join(std::process::id().to_string());
    prepare_linux_auth_datastore_at(&path, uid, gid)?;
    Ok(path)
}

fn prepare_private_directory_at(
    path: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(path)
                .map_err(|_| InstallError::backend_failure())?;
        }
        Err(_) => return Err(InstallError::backend_failure()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| InstallError::backend_failure())?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_user
        || metadata.gid() != expected_group
        || metadata.permissions().mode() & 0o7777 != 0o700
    {
        return Err(InstallError::backend_failure());
    }
    Ok(())
}

/// Prepares the vendor temp directory used as `TMPDIR` for the Determinate
/// installer on macOS. Unlike the private install-state directory, the vendor
/// temp directory must let the vendor's unprivileged Nix build users traverse
/// and stat it while they set up build environments, so only group and other
/// write bits are forbidden. The directory stays root-owned, is never a
/// symlink, and no unprivileged user can write into it.
fn prepare_vendor_tmp_directory_at(
    path: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o755);
            builder
                .create(path)
                .map_err(|_| InstallError::backend_failure())?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .map_err(|_| InstallError::backend_failure())?;
        }
        Err(_) => return Err(InstallError::backend_failure()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| InstallError::backend_failure())?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_user
        || metadata.gid() != expected_group
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(InstallError::backend_failure());
    }
    Ok(())
}

fn remove_stale_linux_auth_datastores(
    root: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    let own_pid = std::process::id();
    for entry in fs::read_dir(root).map_err(|_| InstallError::backend_failure())? {
        let entry = entry.map_err(|_| InstallError::backend_failure())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| InstallError::backend_failure())?;
        let pid = name
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(InstallError::backend_failure)?;
        if pid != own_pid && process_is_alive(pid)? {
            continue;
        }
        remove_linux_auth_datastore_at(&entry.path(), expected_user, expected_group)?;
    }
    Ok(())
}

fn remove_legacy_linux_auth_datastore_files(
    root: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| InstallError::backend_failure())? {
        let entry = entry.map_err(|_| InstallError::backend_failure())?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|_| InstallError::backend_failure())?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }
        validate_linux_auth_datastore_file(
            &entry.file_name(),
            &metadata,
            expected_user,
            expected_group,
        )?;
        files.push(entry.path());
    }
    for path in &files {
        fs::remove_file(path).map_err(|_| InstallError::backend_failure())?;
    }
    if !files.is_empty() {
        fs::File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| InstallError::backend_failure())?;
    }
    Ok(())
}

fn process_is_alive(pid: u32) -> Result<bool, InstallError> {
    let pid = i32::try_from(pid).map_err(|_| InstallError::backend_failure())?;
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(_) => Err(InstallError::backend_failure()),
    }
}

fn prepare_linux_auth_datastore_at(
    path: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    prepare_private_directory_at(path, expected_user, expected_group)?;
    let mut removed_metadata = false;
    for entry in fs::read_dir(path).map_err(|_| InstallError::backend_failure())? {
        let entry = entry.map_err(|_| InstallError::backend_failure())?;
        let name = entry.file_name();
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|_| InstallError::backend_failure())?;
        if validate_linux_auth_datastore_file(&name, &metadata, expected_user, expected_group)? {
            fs::remove_file(entry.path()).map_err(|_| InstallError::backend_failure())?;
            removed_metadata = true;
        }
    }
    if removed_metadata {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| InstallError::backend_failure())?;
    }
    Ok(())
}

pub fn validate_linux_auth_datastore_file(
    name: &OsStr,
    metadata: &fs::Metadata,
    expected_user: u32,
    expected_group: u32,
) -> Result<bool, InstallError> {
    let exact_restart_file = name == "pkg-channel.lock" || name == "accepted-channel.initializing";
    let metadata_limit = match name.to_str() {
        Some("root.json") => Some(64 * 1024),
        Some("timestamp.json" | "snapshot.json") => Some(32 * 1024),
        Some("targets.json") => Some(256 * 1024),
        Some("latest_known_time.json") => Some(1024),
        _ => None,
    };
    let mode = metadata.permissions().mode() & 0o7777;
    let invalid_metadata = metadata_limit.is_some_and(|limit| {
        mode & !0o644 != 0 || mode & 0o600 != 0o600 || metadata.len() == 0 || metadata.len() > limit
    });
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != expected_user
        || metadata.gid() != expected_group
        || (exact_restart_file && (mode != 0o600 || metadata.len() != 0))
        || invalid_metadata
        || (!exact_restart_file && metadata_limit.is_none())
    {
        return Err(InstallError::backend_failure());
    }
    Ok(metadata_limit.is_some())
}

fn remove_linux_auth_datastore(path: &Path) -> Result<(), InstallError> {
    remove_linux_auth_datastore_at(path, 0, 0)?;
    let Some(root) = path.parent() else {
        return Err(InstallError::backend_failure());
    };
    match fs::remove_dir(root) {
        Ok(()) => {
            let parent = root.parent().ok_or_else(InstallError::backend_failure)?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| InstallError::backend_failure())
        }
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(_) => Err(InstallError::backend_failure()),
    }
}

fn remove_linux_auth_datastore_at(
    path: &Path,
    expected_user: u32,
    expected_group: u32,
) -> Result<(), InstallError> {
    prepare_linux_auth_datastore_at(path, expected_user, expected_group)?;
    for entry in fs::read_dir(path).map_err(|_| InstallError::backend_failure())? {
        let entry = entry.map_err(|_| InstallError::backend_failure())?;
        fs::remove_file(entry.path()).map_err(|_| InstallError::backend_failure())?;
    }
    fs::remove_dir(path).map_err(|_| InstallError::backend_failure())?;
    let parent = path.parent().ok_or_else(InstallError::backend_failure)?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| InstallError::backend_failure())
}

enum LinuxBundleRecovery {
    None,
    Committed,
    Fresh {
        storage: LinuxInstallJournalStorage,
        journal: Box<LinuxInstallJournal>,
    },
}

enum LinuxJournalLocation {
    Production,
    #[cfg(test)]
    At {
        base: PathBuf,
        user_id: u32,
        group_id: u32,
    },
}

impl LinuxJournalLocation {
    fn open_existing(
        &self,
        system: System,
        digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<Option<LinuxInstallJournalStorage>, LinuxInstallJournalFileError> {
        match self {
            Self::Production => {
                LinuxInstallJournalStorage::open_existing(system, digest, recovery_context_digest)
            }
            #[cfg(test)]
            Self::At {
                base,
                user_id,
                group_id,
            } => LinuxInstallJournalStorage::open_existing_for_test(
                base,
                *user_id,
                *group_id,
                system,
                digest,
                recovery_context_digest,
            ),
        }
    }

    fn prepare(
        &self,
        system: System,
        digest: Digest,
        recovery_context_digest: Digest,
    ) -> Result<LinuxInstallJournalStorage, LinuxInstallJournalFileError> {
        match self {
            Self::Production => {
                LinuxInstallJournalStorage::prepare(system, digest, recovery_context_digest)
            }
            #[cfg(test)]
            Self::At {
                base,
                user_id,
                group_id,
            } => LinuxInstallJournalStorage::prepare_for_test(
                base,
                *user_id,
                *group_id,
                system,
                digest,
                recovery_context_digest,
            ),
        }
    }
}

fn recover_linux_bundle_install(
    system: System,
    digest: pkg_core::state::Digest,
    recovery_context_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
    backend: &mut dyn LinuxInstallBackend,
    journal_location: &LinuxJournalLocation,
) -> Result<LinuxBundleRecovery, InstallError> {
    let Some(storage) = journal_location
        .open_existing(system, digest, recovery_context_digest)
        .map_err(|_| InstallError::backend_failure())?
    else {
        return Ok(LinuxBundleRecovery::None);
    };
    if let Some(mut journal) = storage.load().map_err(linux_journal_file_error)? {
        let journal_system = journal
            .system()
            .map_err(|_| InstallError::backend_failure())?;
        backend.preflight_recovery(journal.mode(), journal_system)?;
        let committed = journal.is_committed();
        if committed {
            finalize_committed_linux_install(&journal, backend)?;
        } else {
            // This journal was created only after the fixed workspace was absent.
            // Cleanup is independent of runtime presence, including reinstalls.
            recover_interrupted_provision_workspace(request.scratch_parent)
                .map_err(|_| InstallError::backend_failure())?;
            recover_linux_install(&mut journal, backend, &mut || Ok(()), &mut |journal| {
                storage
                    .replace(journal)
                    .map_err(|_| InstallError::backend_failure())
            })?;
        }
        if committed {
            storage
                .remove()
                .map_err(|_| InstallError::backend_failure())?;
            return Ok(LinuxBundleRecovery::Committed);
        }
        if journal.mode() == crate::LinuxInstallMode::FreshInstall {
            return Ok(LinuxBundleRecovery::Fresh {
                storage,
                journal: Box::new(journal),
            });
        }
        storage
            .remove()
            .map_err(|_| InstallError::backend_failure())?;
        return Ok(LinuxBundleRecovery::None);
    }
    storage
        .remove()
        .map_err(|_| InstallError::backend_failure())?;
    Ok(LinuxBundleRecovery::None)
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

const fn linux_journal_file_error(error: LinuxInstallJournalFileError) -> InstallError {
    if matches!(
        error.code(),
        LinuxInstallJournalFileErrorCode::UnsupportedSchema
    ) {
        InstallError::unsupported_recovery_schema()
    } else {
        InstallError::backend_failure()
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
    daemon: &'a dyn ManagedDaemon,
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
        daemon,
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

fn load_macos_bundle_for_recovery(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, MacOsError> {
    let auth_datastore = prepare_macos_auth_datastore()?;
    let auth_request = InstallerProvisionRequest {
        repository: request.repository,
        datastore: &auth_datastore,
        installation_root: request.installation_root,
        scratch_parent: request.scratch_parent,
        system: request.system,
        groups: request.groups,
    };
    let result = load_authenticated_installer_bundle_blocking(trusted_root, &auth_request)
        .map_err(|_| MacOsError::backend_failure());
    remove_linux_auth_datastore(&auth_datastore).map_err(|_| MacOsError::backend_failure())?;
    result
}

fn prepare_macos_auth_datastore() -> Result<PathBuf, MacOsError> {
    let uid = nix::unistd::Uid::effective().as_raw();
    let gid = nix::unistd::Gid::effective().as_raw();
    if uid != 0 || gid != 0 {
        return Err(MacOsError::backend_failure());
    }
    let root = PathBuf::from(MACOS_AUTH_DATASTORE);
    prepare_private_directory_at(&root, uid, gid).map_err(|_| MacOsError::backend_failure())?;
    remove_legacy_linux_auth_datastore_files(&root, uid, gid)
        .map_err(|_| MacOsError::backend_failure())?;
    remove_stale_linux_auth_datastores(&root, uid, gid)
        .map_err(|_| MacOsError::backend_failure())?;
    let path = root.join(std::process::id().to_string());
    prepare_linux_auth_datastore_at(&path, uid, gid).map_err(|_| MacOsError::backend_failure())?;
    Ok(path)
}

fn recover_macos_bundle_install(
    system: System,
    digest: Digest,
    recovery_context_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<Option<(MacOsInstallJournalStorage, MacOsInstallJournal)>, MacOsError> {
    let Some(storage) =
        MacOsInstallJournalStorage::open_existing(system, digest, recovery_context_digest)
            .map_err(|_| MacOsError::backend_failure())?
    else {
        return Ok(None);
    };
    recover_macos_bundle_install_from_storage(storage, request, backend)
}

fn recover_macos_bundle_install_from_storage(
    storage: MacOsInstallJournalStorage,
    request: &InstallerProvisionRequest<'_>,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<Option<(MacOsInstallJournalStorage, MacOsInstallJournal)>, MacOsError> {
    let Some(mut journal) = storage.load().map_err(|_| MacOsError::backend_failure())? else {
        storage
            .remove()
            .map_err(|_| MacOsError::backend_failure())?;
        return Ok(None);
    };
    backend.begin_authenticated_recovery(journal.mode())?;
    recover_interrupted_provision_workspace(request.scratch_parent)
        .map_err(|_| MacOsError::backend_failure())?;
    if journal.is_committed() {
        for asset in crate::macos_product_install_assets()
            .filter(|asset| asset.kind() == crate::MacOsAssetKind::File)
        {
            backend.finalize_replaced_asset(asset)?;
        }
        storage
            .remove()
            .map_err(|_| MacOsError::backend_failure())?;
        return Ok(None);
    }
    crate::recover_macos_install(&mut journal, backend, &mut || Ok(()), &mut |journal| {
        storage
            .replace(journal)
            .map_err(|_| MacOsError::backend_failure())
    })?;
    Ok(Some((storage, journal)))
}

fn macos_recovery_context_digest(
    ownership_manifest_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"pkg-macos-install-recovery-v1\0");
    hasher.update(ownership_manifest_digest.as_bytes());
    for path in [request.installation_root, request.scratch_parent] {
        let bytes = path.as_os_str().as_bytes();
        hasher.update(bytes.len().to_be_bytes());
        hasher.update(bytes);
    }
    Digest::from_bytes(hasher.finalize().into())
}

enum BootstrapOutcome {
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
    fn has_accepted_base_nix(&self) -> bool {
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

    fn into_linux_bootstrap(self) -> Result<(), InstallError> {
        match self {
            Self::Existing | Self::DeterminateComplete => Ok(()),
            Self::DeterminatePending { .. } => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(InstallError::backend_failure()),
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Err(InstallError::backend_failure()),
        }
    }

    fn into_macos_bootstrap(self) -> Result<(), MacOsError> {
        match self {
            Self::Existing | Self::DeterminateComplete => Ok(()),
            Self::DeterminatePending { .. } => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::Stub(_) => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            Self::DeterminateTestPending(_) => Err(MacOsError::backend_failure()),
        }
    }

    fn rollback_linux(self) -> Result<(), InstallError> {
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

    fn rollback_macos(self) -> Result<(), MacOsError> {
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

trait BundleProvisioner {
    fn reuse_existing(&mut self) -> Result<bool, BundleProvisionError> {
        Ok(false)
    }

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

    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
    ) -> Result<BootstrapOutcome, BundleProvisionError>;
}

#[derive(Clone, Copy)]
enum BundleProvisionError {
    Failed,
    RollbackIncomplete,
}

struct AuthenticatedProvisioner {
    trusted_root: Option<TrustedRoot>,
    bundle: Option<AuthenticatedInstallerBundle>,
}

impl AuthenticatedProvisioner {
    const fn with_reauthentication(
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

    fn provision<'a>(
        &mut self,
        request: &InstallerProvisionRequest<'_>,
        daemon: &'a dyn ManagedDaemon,
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
        let _ = (bundle, daemon);
        Err(BundleProvisionError::Failed)
    }
}

struct LinuxBundleBackend<'a, 'j, P> {
    inner: &'a mut dyn LinuxInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome>,
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

    fn begin_services(&mut self) -> Result<(), InstallError> {
        self.journal
            .intend_services()
            .map_err(|_| InstallError::backend_failure())?;
        self.persist()
    }

    fn record_preexisting(&mut self, mutation: LinuxInstallMutation) -> Result<(), InstallError> {
        self.journal
            .record_preexisting(mutation)
            .map_err(|_| InstallError::backend_failure())?;
        self.persist()
    }

    fn complete_rollback(&mut self, mutation: &LinuxInstallMutation) -> Result<(), InstallError> {
        self.journal
            .complete_recovery_action(mutation)
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.persist()
            .map_err(|_| InstallError::rollback_incomplete())
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
    fn install_mode(&self) -> crate::LinuxInstallMode {
        self.inner.install_mode()
    }

    fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
        if let Some(transaction) = self.journal.as_ref()
            && transaction.journal.fresh_services_deactivated()
        {
            return self
                .inner
                .preflight_fresh_recovery_mutation(&transaction.journal);
        }
        self.inner.preflight_product_mutation()
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError> {
        self.inner
            .bind_authenticated_release_identity(system, digest)
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
    fn services_need_mutation(&self, prior_active: bool) -> bool {
        self.inner.services_need_mutation(prior_active)
    }
    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.inner.recover_asset(asset)
    }
    fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
        self.inner.recover_repair_assets()
    }
    fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
        self.inner.recover_fresh_services()
    }
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.inner.preflight_product_mutation()?;
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
        self.inner.preflight_product_mutation()?;
        let changed = self.inner.install_systemd_unit(asset, contents)?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        let mutation = LinuxInstallMutation::ManagedRuntime;
        let presence = self.inner.classify_managed_runtime()?;
        if presence == LinuxAssetPresence::ExactPresent
            && self
                .provisioner
                .reuse_existing()
                .map_err(linux_provision_error)?
        {
            self.outcome = Some(BootstrapOutcome::Existing);
            begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
            complete_linux_mutation(&mut self.journal, mutation, presence, false)?;
            return Ok(false);
        }
        self.provisioner
            .reauthenticate_linux(self.request, self.inner)
            .map_err(linux_provision_error)?;
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
        Ok(true)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        self.preflight_product_mutation()
            .map_err(|_| InstallError::rollback_incomplete())?;
        if !self
            .outcome
            .as_ref()
            .is_some_and(BootstrapOutcome::has_accepted_base_nix)
        {
            let Some(outcome) = self.outcome.take() else {
                return Err(InstallError::rollback_incomplete());
            };
            outcome.rollback_linux()?;
        }
        self.journal.as_mut().map_or(Ok(()), |journal| {
            if journal.journal.recovery_actions().iter().any(|action| {
                matches!(
                    action,
                    crate::LinuxInstallRecoveryAction::RevalidateIntended(
                        LinuxInstallMutation::ManagedRuntime
                    ) | crate::LinuxInstallRecoveryAction::RevertCreated(
                        LinuxInstallMutation::ManagedRuntime
                    )
                )
            }) {
                journal.complete_rollback(&LinuxInstallMutation::ManagedRuntime)
            } else {
                Ok(())
            }
        })
    }
    fn validate_base_nix(&mut self) -> Result<(), InstallError> {
        self.inner.validate_base_nix()
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
        match self.outcome.as_ref() {
            Some(BootstrapOutcome::DeterminatePending { handoff, .. }) => handoff
                .accept_after_installed_state_proof()
                .map_err(|_| InstallError::backend_failure()),
            #[cfg(test)]
            Some(BootstrapOutcome::DeterminateTestPending(handoff)) => handoff
                .accept_after_installed_state_proof()
                .map_err(|_| InstallError::backend_failure()),
            Some(_) => Ok(()),
            None => Err(InstallError::backend_failure()),
        }
    }
    fn activate_services(&mut self) -> Result<bool, InstallError> {
        if let Some(BootstrapOutcome::DeterminatePending { bundle, .. }) = self.outcome.as_mut() {
            bundle
                .commit_authenticated_channel()
                .map_err(|_| InstallError::backend_failure())?;
        }
        let mutation = LinuxInstallMutation::Services;
        let presence = self.inner.classify_services()?;
        let prior_active = presence == LinuxAssetPresence::ExactPresent;
        let needed = self.inner.services_need_mutation(prior_active);
        if needed {
            self.journal
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?
                .begin_services()?;
        }
        let changed = self.inner.activate_services()?;
        if changed != needed {
            return Err(InstallError::backend_failure());
        }
        if needed {
            let transaction = self
                .journal
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?;
            transaction
                .journal
                .complete_created()
                .map_err(|_| InstallError::backend_failure())?;
            transaction.persist()?;
        } else if let Some(transaction) = self.journal.as_mut() {
            transaction.record_preexisting(mutation)?;
        }
        Ok(changed)
    }
    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.inner.rollback_services()?;
        if let Some(transaction) = self.journal.as_mut() {
            transaction
                .journal
                .mark_fresh_services_deactivated()
                .map_err(|_| InstallError::rollback_incomplete())?;
            transaction.persist()?;
            transaction.complete_rollback(&LinuxInstallMutation::Services)?;
        }
        Ok(())
    }
    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
        self.inner.finish_fresh_services_rollback()
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
        let presence = self.inner.classify_ownership_receipt(asset)?;
        // An exact receipt is verified and reused by the production backend.
        // It is not rewritten on reinstall.
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.inner.preflight_product_mutation()?;
        let outcome = self
            .outcome
            .take()
            .ok_or_else(InstallError::backend_failure)?;
        match outcome {
            BootstrapOutcome::DeterminatePending { bundle, handoff } => {
                let changed = match publish_determinate_receipt(
                    self.inner,
                    &mut self.journal,
                    mutation,
                    presence,
                ) {
                    Ok(changed) => changed,
                    Err(error) => {
                        self.outcome =
                            Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                        return Err(error);
                    }
                };
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(changed)
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                let changed = result?;
                complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
                Ok(changed)
            }
            #[cfg(test)]
            BootstrapOutcome::DeterminateTestPending(handoff) => {
                let result =
                    publish_determinate_receipt(self.inner, &mut self.journal, mutation, presence);
                self.outcome = Some(if result.is_ok() {
                    BootstrapOutcome::DeterminateComplete
                } else {
                    BootstrapOutcome::DeterminateTestPending(handoff)
                });
                result
            }
            BootstrapOutcome::DeterminateComplete => Err(InstallError::backend_failure()),
            BootstrapOutcome::Existing => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Existing);
                let changed = result?;
                complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
                Ok(changed)
            }
        }
    }
    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        self.inner.finalize_ownership_receipt()
    }
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.preflight_product_mutation()
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.inner.rollback_asset(asset)?;
        let mutation = asset_mutation(asset);
        self.journal
            .as_mut()
            .map_or(Ok(()), |journal| journal.complete_rollback(&mutation))
    }
}

fn determinate_succeeded(outcome: DeterminateProcessOutcome) -> bool {
    outcome.terminal == DeterminateTerminal::Exited(0)
}

fn publish_determinate_receipt(
    backend: &mut dyn LinuxInstallBackend,
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
) -> Result<bool, InstallError> {
    let changed = backend.publish_ownership_receipt()?;
    complete_linux_mutation(journal, mutation, presence, changed)?;
    Ok(changed)
}

fn run_with_new_determinate_handoff<T>(
    handoff: &DeterminateHandoff,
    start: impl FnOnce() -> Result<T, BundleProvisionError>,
) -> Result<T, BundleProvisionError> {
    match handoff.state().map_err(|_| BundleProvisionError::Failed)? {
        DeterminateHandoffState::NotStarted => {
            handoff
                .record_started()
                .map_err(|_| BundleProvisionError::Failed)?;
            start()
        }
        DeterminateHandoffState::Started | DeterminateHandoffState::Accepted => {
            Err(BundleProvisionError::Failed)
        }
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
) -> Result<(LinuxInstallReport, BootstrapOutcome), InstallError> {
    if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
        return Err(InstallError::backend_failure());
    }
    let mode = journal.mode();
    if backend.install_mode() != mode {
        return Err(InstallError::recovery_mode_mismatch());
    }
    if mode == crate::LinuxInstallMode::OfflineRepair {
        backend.preflight_clean_host(system)?;
    }
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
        journal: Some(LinuxJournalTransaction { storage, journal }),
    };
    let report = match crate::installer::install_linux_journaled_preflighted(&mut adapter) {
        Ok(report) => report,
        Err(_) if mode == crate::LinuxInstallMode::OfflineRepair => {
            return Err(InstallError::rollback_incomplete());
        }
        Err(_)
            if mode == crate::LinuxInstallMode::FreshInstall
                && adapter
                    .outcome
                    .as_ref()
                    .is_some_and(BootstrapOutcome::has_accepted_base_nix) =>
        {
            return Err(InstallError::fresh_recovery_retained());
        }
        Err(error) => return Err(error),
    };
    adapter
        .journal
        .as_mut()
        .ok_or_else(InstallError::rollback_incomplete)?
        .commit()?;
    let committed = adapter
        .journal
        .as_ref()
        .ok_or_else(InstallError::rollback_incomplete)?;
    finalize_committed_linux_install(&committed.journal, adapter.inner)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

fn finalize_committed_linux_install(
    journal: &LinuxInstallJournal,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<(), InstallError> {
    if !journal.is_committed() {
        return Err(InstallError::rollback_incomplete());
    }
    let mode = journal.mode();
    let system = journal
        .system()
        .map_err(|_| InstallError::rollback_incomplete())?;
    backend
        .preflight_recovery(mode, system)
        .and_then(|()| backend.finalize_ownership_receipt())
        .map_err(|_| InstallError::rollback_incomplete())
}

#[cfg(test)]
fn install_linux_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
) -> Result<(LinuxInstallReport, BootstrapOutcome), InstallError> {
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
        journal: None,
    };
    let report = crate::installer::install_linux(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

struct MacOsBundleBackend<'a, 'j, P> {
    inner: &'a mut dyn MacOsInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome>,
    journal: Option<MacOsJournalTransaction<'j>>,
    store_created: bool,
}

struct MacOsJournalTransaction<'a> {
    storage: &'a dyn MacOsJournalPersistence,
    journal: MacOsInstallJournal,
}

trait MacOsJournalPersistence {
    fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError>;
}

impl MacOsJournalPersistence for MacOsInstallJournalStorage {
    fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError> {
        Self::replace(self, journal).map_err(|_| MacOsError::backend_failure())
    }
}

impl MacOsJournalTransaction<'_> {
    fn begin(
        &mut self,
        mutation: MacOsInstallMutation,
        presence: MacOsAssetPresence,
    ) -> Result<(), MacOsError> {
        if self
            .journal
            .mutation_state(&mutation)
            .map_err(|_| MacOsError::backend_failure())?
            == Some(crate::MacOsInstallMutationState::PreExisting)
        {
            return (presence == MacOsAssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if presence == MacOsAssetPresence::Absent {
            self.journal
                .intend(mutation)
                .map_err(|_| MacOsError::backend_failure())?;
            self.persist()?;
        }
        Ok(())
    }

    fn complete(
        &mut self,
        mutation: MacOsInstallMutation,
        presence: MacOsAssetPresence,
        changed: bool,
    ) -> Result<(), MacOsError> {
        if self
            .journal
            .mutation_state(&mutation)
            .map_err(|_| MacOsError::backend_failure())?
            == Some(crate::MacOsInstallMutationState::PreExisting)
        {
            return (!changed && presence == MacOsAssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if changed != (presence == MacOsAssetPresence::Absent) {
            return Err(MacOsError::backend_failure());
        }
        if changed {
            self.journal
                .complete_created()
                .map_err(|_| MacOsError::backend_failure())?;
        } else {
            self.journal
                .record_preexisting(mutation)
                .map_err(|_| MacOsError::backend_failure())?;
        }
        self.persist()
    }

    fn begin_replacement(
        &mut self,
        mutation: MacOsInstallMutation,
        prior_digest: Option<Digest>,
    ) -> Result<(), MacOsError> {
        self.journal
            .intend_replacement(mutation, prior_digest)
            .map_err(|_| MacOsError::backend_failure())?;
        self.persist()
    }

    fn complete_replacement(&mut self, changed: bool) -> Result<(), MacOsError> {
        if changed {
            self.journal
                .complete_replaced()
                .map_err(|_| MacOsError::backend_failure())?;
        } else {
            self.journal
                .complete_unchanged_replacement()
                .map_err(|_| MacOsError::backend_failure())?;
        }
        self.persist()
    }

    fn complete_rollback(&mut self, mutation: &MacOsInstallMutation) -> Result<(), MacOsError> {
        if self
            .journal
            .recovery_actions()
            .iter()
            .any(|action| match action {
                crate::MacOsInstallRecoveryAction::RevalidateIntended(current)
                | crate::MacOsInstallRecoveryAction::RevertCreated(current)
                | crate::MacOsInstallRecoveryAction::RollForwardReplaced(current)
                | crate::MacOsInstallRecoveryAction::RestoreReplaced(current, _) => {
                    *current == mutation
                }
            })
        {
            self.journal
                .complete_recovery_action(mutation)
                .map_err(|_| MacOsError::rollback_incomplete())?;
            self.persist()?;
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), MacOsError> {
        self.journal
            .commit()
            .map_err(|_| MacOsError::rollback_incomplete())?;
        self.storage
            .replace(&self.journal)
            .map_err(|_| MacOsError::rollback_incomplete())
    }

    fn persist(&self) -> Result<(), MacOsError> {
        self.storage.replace(&self.journal)
    }
}

impl<P: BundleProvisioner> MacOsInstallBackend for MacOsBundleBackend<'_, '_, P> {
    fn install_mode(&self) -> crate::MacOsInstallMode {
        self.inner.install_mode()
    }

    fn preflight_product_mutation(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()
    }
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError> {
        self.inner.bind_authenticated_installer_payloads(payloads)
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        release_identity_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner
            .bind_authenticated_release_identity(system, release_identity_digest)
    }

    fn begin_authenticated_recovery(
        &mut self,
        mode: crate::MacOsInstallMode,
    ) -> Result<(), MacOsError> {
        self.inner.begin_authenticated_recovery(mode)
    }

    fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError> {
        self.inner.preflight_clean_host(system)
    }
    fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.inner.broker_uid()
    }
    fn classify_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_asset(asset)
    }
    fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_managed_runtime()
    }
    fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_services()
    }
    fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_ownership_receipt()
    }
    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_asset(asset)
    }
    fn recover_services(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_services()
    }
    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_ownership_receipt()?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::OwnershipReceipt)?;
        }
        Ok(())
    }
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_release_bundle()
    }
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let observed = self.inner.classify_asset(asset)?;
        let presence = if self.store_created && asset.id() == "nix-root" {
            if observed != MacOsAssetPresence::ExactPresent {
                return Err(MacOsError::backend_failure());
            }
            MacOsAssetPresence::Absent
        } else {
            observed
        };
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && asset.kind() == crate::MacOsAssetKind::File
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_file_digest(asset)?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let changed = self.inner.ensure_asset(asset)?;
        if replacing {
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .complete_replacement(changed)?;
        } else {
            complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        }
        Ok(changed)
    }
    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_file_digest(asset)?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let changed = self.inner.install_launchd_plist(asset, contents)?;
        if replacing {
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .complete_replacement(changed)?;
        } else {
            complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        }
        Ok(changed)
    }
    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.install_nix_config(asset)?;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::ManagedRuntime;
        let presence = self.inner.classify_managed_runtime()?;
        if presence == MacOsAssetPresence::ExactPresent
            && self
                .provisioner
                .reuse_existing()
                .map_err(macos_provision_error)?
        {
            self.outcome = Some(BootstrapOutcome::Existing);
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
            complete_macos_mutation(&mut self.journal, mutation, presence, false)?;
            return Ok(false);
        }
        self.provisioner
            .reauthenticate_macos(self.request, self.inner)
            .map_err(macos_provision_error)?;
        self.provisioner
            .preflight_workspace(self.request)
            .map_err(macos_provision_error)?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.outcome = Some(
            self.provisioner
                .provision(self.request, self.daemon)
                .map_err(macos_provision_error)?,
        );
        let changed = presence == MacOsAssetPresence::Absent;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
        if self
            .outcome
            .as_ref()
            .is_some_and(BootstrapOutcome::has_accepted_base_nix)
        {
            if let Some(journal) = self.journal.as_mut() {
                journal.complete_rollback(&MacOsInstallMutation::ManagedRuntime)?;
            }
            return Ok(());
        }
        self.outcome
            .take()
            .map_or(Ok(()), BootstrapOutcome::rollback_macos)?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::ManagedRuntime)?;
        }
        Ok(())
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
        match self.outcome.as_ref() {
            Some(BootstrapOutcome::DeterminatePending { handoff, .. }) => {
                handoff
                    .accept_after_installed_state_proof()
                    .map_err(|_| MacOsError::backend_failure())?;
            }
            #[cfg(test)]
            Some(BootstrapOutcome::DeterminateTestPending(handoff)) => {
                handoff
                    .accept_after_installed_state_proof()
                    .map_err(|_| MacOsError::backend_failure())?;
            }
            Some(BootstrapOutcome::Existing) => {}
            #[cfg(test)]
            Some(BootstrapOutcome::Stub(_)) => {}
            Some(_) | None => return Err(MacOsError::backend_failure()),
        }
        // The inner backend owns the preexisting-asset policy. A handoff
        // accepted during this run must let the vendor-created /nix pass the
        // post-acceptance nix-root classification.
        self.inner.accept_base_nix_handoff()
    }
    fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_installed_code()
    }
    fn activate_services(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::Services;
        let presence = self.inner.classify_services()?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.activate_services()?;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_services()?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::Services)?;
        }
        Ok(())
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
    #[allow(clippy::too_many_lines)]
    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::OwnershipReceipt;
        let presence = self.inner.classify_ownership_receipt()?;
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_ownership_receipt_digest()?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let outcome = self
            .outcome
            .take()
            .ok_or_else(MacOsError::backend_failure)?;
        match outcome {
            BootstrapOutcome::DeterminatePending {
                mut bundle,
                handoff,
            } => {
                let receipt_created = match self.inner.publish_ownership_receipt() {
                    Ok(created) => created,
                    Err(error) => {
                        self.outcome =
                            Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                        return Err(error);
                    }
                };
                if bundle.commit_authenticated_channel().is_err() {
                    self.outcome = Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                    return Err(MacOsError::backend_failure());
                }
                if let Err(error) = complete_macos_receipt(
                    &mut self.journal,
                    mutation,
                    presence,
                    receipt_created,
                    replacing,
                ) {
                    self.outcome = Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                    return Err(error);
                }
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(receipt_created)
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                let changed = result?;
                complete_macos_receipt(&mut self.journal, mutation, presence, changed, replacing)?;
                Ok(changed)
            }
            BootstrapOutcome::DeterminateComplete => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            BootstrapOutcome::DeterminateTestPending(handoff) => {
                let receipt_created = match self.inner.publish_ownership_receipt() {
                    Ok(created) => created,
                    Err(error) => {
                        self.outcome = Some(BootstrapOutcome::DeterminateTestPending(handoff));
                        return Err(error);
                    }
                };
                if let Err(error) = complete_macos_receipt(
                    &mut self.journal,
                    mutation,
                    presence,
                    receipt_created,
                    replacing,
                ) {
                    self.outcome = Some(BootstrapOutcome::DeterminateTestPending(handoff));
                    return Err(error);
                }
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(receipt_created)
            }
            BootstrapOutcome::Existing => {
                // Match the Linux Existing arm: the accepted channel state is
                // already committed, so a repeat install only verifies and
                // reuses the exact ownership receipt.
                let changed = match self.inner.publish_ownership_receipt() {
                    Ok(changed) => changed,
                    Err(error) => {
                        self.outcome = Some(BootstrapOutcome::Existing);
                        return Err(error);
                    }
                };
                self.outcome = Some(BootstrapOutcome::Existing);
                complete_macos_receipt(&mut self.journal, mutation, presence, changed, replacing)?;
                Ok(changed)
            }
        }
    }
    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.rollback_asset(asset)?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&macos_asset_mutation(asset))?;
        }
        Ok(())
    }

    fn prior_file_digest(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        self.inner.prior_file_digest(asset)
    }

    fn recover_replaced_asset(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_replaced_asset(asset, prior_digest)
    }

    fn roll_forward_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.roll_forward_replaced_asset(asset)
    }

    fn finalize_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.finalize_replaced_asset(asset)
    }

    fn prior_ownership_receipt_digest(&mut self) -> Result<Option<Digest>, MacOsError> {
        self.inner.prior_ownership_receipt_digest()
    }

    fn recover_replaced_ownership_receipt(
        &mut self,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_replaced_ownership_receipt(prior_digest)
    }

    fn roll_forward_replaced_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.roll_forward_replaced_ownership_receipt()
    }

    fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_store_volume()
    }
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        self.inner.provision_store_volume()
    }
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_store_volume()
    }
    fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
        self.inner.recover_store_volume()
    }
}

fn complete_macos_receipt(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
    changed: bool,
    replacing: bool,
) -> Result<(), MacOsError> {
    if replacing {
        journal
            .as_mut()
            .ok_or_else(MacOsError::backend_failure)?
            .complete_replacement(changed)
    } else {
        complete_macos_mutation(journal, mutation, presence, changed)
    }
}

fn macos_asset_mutation(asset: MacOsInstallAsset) -> MacOsInstallMutation {
    MacOsInstallMutation::Asset {
        id: asset.id().to_owned(),
    }
}

fn begin_macos_mutation(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
) -> Result<(), MacOsError> {
    journal
        .as_mut()
        .map_or(Ok(()), |journal| journal.begin(mutation, presence))
}

fn complete_macos_mutation(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
    changed: bool,
) -> Result<(), MacOsError> {
    journal.as_mut().map_or(Ok(()), |journal| {
        journal.complete(mutation, presence, changed)
    })
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

#[cfg(test)]
fn install_macos_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn MacOsInstallBackend,
    provisioner: &'a mut P,
) -> Result<(MacOsInstallReport, BootstrapOutcome), MacOsError> {
    let mut adapter = MacOsBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
        journal: None,
        store_created: false,
    };
    let report = install_macos(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(MacOsError::backend_failure)?;
    Ok((report, outcome))
}

fn install_macos_with_provisioner_journaled<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    daemon: &'a dyn ManagedDaemon,
    backend: &'a mut dyn MacOsInstallBackend,
    provisioner: &'a mut P,
    storage: &dyn MacOsJournalPersistence,
    journal: MacOsInstallJournal,
) -> Result<(MacOsInstallReport, BootstrapOutcome), MacOsError> {
    let mut adapter = MacOsBundleBackend {
        inner: backend,
        request,
        daemon,
        provisioner,
        outcome: None,
        journal: Some(MacOsJournalTransaction { storage, journal }),
        store_created: false,
    };
    let mode = adapter
        .journal
        .as_ref()
        .ok_or_else(MacOsError::rollback_incomplete)?
        .journal
        .mode();
    let report = match install_macos(system, &mut adapter) {
        Ok(report) => report,
        Err(_) if mode == crate::MacOsInstallMode::OfflineRepair => {
            return Err(MacOsError::rollback_incomplete());
        }
        Err(_)
            if mode == crate::MacOsInstallMode::FreshInstall
                && adapter
                    .outcome
                    .as_ref()
                    .is_some_and(BootstrapOutcome::has_accepted_base_nix) =>
        {
            return Err(MacOsError::rollback_incomplete());
        }
        Err(error) => return Err(error),
    };
    adapter
        .journal
        .as_mut()
        .ok_or_else(MacOsError::rollback_incomplete)?
        .commit()?;
    for asset in crate::macos_product_install_assets()
        .filter(|asset| asset.kind() == crate::MacOsAssetKind::File)
    {
        adapter
            .inner
            .finalize_replaced_asset(asset)
            .map_err(|_| MacOsError::rollback_incomplete())?;
    }
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
    use pkg_testkit::{ChaosCheckpoint, ChaosCommand, FsyncMode, publish_checkpoint};
    use std::{
        cell::RefCell,
        fs,
        os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
        os::unix::process::ExitStatusExt as _,
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    const SUPERVISOR_LOSS_CHILD_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_CHILD";
    const SUPERVISOR_LOSS_ROOT_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_ROOT";
    const SUPERVISOR_LOSS_EXECUTABLE_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_EXECUTABLE";

    #[test]
    fn linux_recovery_context_binds_installation_and_scratch_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let digest = Digest::from_bytes([0x90; 32]);
        let groups = ManagedGroupBindings::new(100, 101)?;
        let context = |installation_root: &Path, scratch_parent: &Path| {
            linux_recovery_context_digest(
                digest,
                &InstallerProvisionRequest {
                    repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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

    #[test]
    fn linux_auth_datastore_accepts_only_exact_private_restart_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let uid = nix::unistd::Uid::effective().as_raw();
        let gid = nix::unistd::Gid::effective().as_raw();

        let exact = root.path().join("exact");
        prepare_linux_auth_datastore_at(&exact, uid, gid)?;
        prepare_linux_auth_datastore_at(&exact, uid, gid)?;
        for name in ["pkg-channel.lock", "accepted-channel.initializing"] {
            fs::File::options()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(exact.join(name))?;
        }
        for name in [
            "root.json",
            "timestamp.json",
            "snapshot.json",
            "targets.json",
            "latest_known_time.json",
        ] {
            let path = exact.join(name);
            fs::write(&path, b"{}")?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        }
        prepare_linux_auth_datastore_at(&exact, uid, gid)?;
        assert_eq!(fs::read_dir(&exact)?.count(), 2);
        remove_linux_auth_datastore_at(&exact, uid, gid)?;
        assert!(!exact.exists());

        let legacy_pool = root.path().join("legacy-pool");
        prepare_private_directory_at(&legacy_pool, uid, gid)?;
        fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(legacy_pool.join("pkg-channel.lock"))?;
        let legacy_metadata = legacy_pool.join("root.json");
        fs::write(&legacy_metadata, b"{}")?;
        fs::set_permissions(&legacy_metadata, fs::Permissions::from_mode(0o644))?;
        remove_legacy_linux_auth_datastore_files(&legacy_pool, uid, gid)?;
        assert_eq!(fs::read_dir(&legacy_pool)?.count(), 0);

        let legacy_foreign = root.path().join("legacy-foreign");
        prepare_private_directory_at(&legacy_foreign, uid, gid)?;
        fs::write(legacy_foreign.join("foreign"), [])?;
        assert!(remove_legacy_linux_auth_datastore_files(&legacy_foreign, uid, gid).is_err());
        assert!(legacy_foreign.join("foreign").exists());

        let unknown = root.path().join("unknown");
        prepare_linux_auth_datastore_at(&unknown, uid, gid)?;
        fs::write(unknown.join("foreign"), [])?;
        assert!(prepare_linux_auth_datastore_at(&unknown, uid, gid).is_err());

        let permissive = root.path().join("permissive");
        fs::DirBuilder::new().mode(0o755).create(&permissive)?;
        fs::set_permissions(&permissive, fs::Permissions::from_mode(0o755))?;
        assert!(prepare_linux_auth_datastore_at(&permissive, uid, gid).is_err());

        let linked = root.path().join("linked");
        symlink(root.path().join("missing"), &linked)?;
        assert!(prepare_linux_auth_datastore_at(&linked, uid, gid).is_err());

        // The macOS vendor temp directory is created traversable because the
        // vendor's unprivileged Nix build users must stat `TMPDIR`, while every
        // unprivileged write bit stays forbidden.
        let vendor_tmp = root.path().join("vendor-tmp");
        prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid)?;
        assert_eq!(
            fs::metadata(&vendor_tmp)?.permissions().mode() & 0o7777,
            0o755
        );
        prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid)?;
        fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o700))?;
        assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_ok());
        fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o770))?;
        assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_err());
        fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o757))?;
        assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_err());
        let vendor_linked = root.path().join("vendor-linked");
        symlink(root.path().join("missing"), &vendor_linked)?;
        assert!(prepare_vendor_tmp_directory_at(&vendor_linked, uid, gid).is_err());
        let vendor_file = root.path().join("vendor-file");
        fs::write(&vendor_file, b"not a directory")?;
        assert!(prepare_vendor_tmp_directory_at(&vendor_file, uid, gid).is_err());

        let pool = root.path().join("pool");
        prepare_private_directory_at(&pool, uid, gid)?;
        let stale = pool.join(std::process::id().to_string());
        prepare_linux_auth_datastore_at(&stale, uid, gid)?;
        fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(stale.join("pkg-channel.lock"))?;
        remove_stale_linux_auth_datastores(&pool, uid, gid)?;
        assert!(!stale.exists());
        assert!(process_is_alive(std::process::id())?);

        let foreign = pool.join("foreign");
        prepare_private_directory_at(&foreign, uid, gid)?;
        assert!(remove_stale_linux_auth_datastores(&pool, uid, gid).is_err());
        Ok(())
    }

    #[derive(Default)]
    struct MemoryJournalPersistence {
        snapshots: RefCell<Vec<LinuxInstallJournal>>,
        committed: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl LinuxJournalPersistence for MemoryJournalPersistence {
        fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
            self.snapshots.borrow_mut().push(journal.clone());
            if journal.is_committed() {
                self.committed.set(true);
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct MacMemoryJournalPersistence {
        snapshots: RefCell<Vec<MacOsInstallJournal>>,
    }

    impl MacOsJournalPersistence for MacMemoryJournalPersistence {
        fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError> {
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
        ) -> Result<BootstrapOutcome, BundleProvisionError> {
            self.calls = self.calls.saturating_add(1);
            Ok(BootstrapOutcome::Stub(self.rolled_back.clone()))
        }
    }

    struct ReauthProvisioner {
        calls: usize,
        reauthenticated: bool,
        reuse_existing: bool,
    }

    impl BundleProvisioner for ReauthProvisioner {
        fn reuse_existing(&mut self) -> Result<bool, BundleProvisionError> {
            Ok(self.reuse_existing)
        }

        fn reauthenticate_linux(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _backend: &mut dyn LinuxInstallBackend,
        ) -> Result<(), BundleProvisionError> {
            self.reauthenticated = true;
            Ok(())
        }

        fn provision<'a>(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _daemon: &'a dyn ManagedDaemon,
        ) -> Result<BootstrapOutcome, BundleProvisionError> {
            if !self.reauthenticated {
                return Err(BundleProvisionError::Failed);
            }
            self.calls = self.calls.saturating_add(1);
            Ok(BootstrapOutcome::Stub(std::rc::Rc::new(
                std::cell::Cell::new(false),
            )))
        }
    }

    struct RollbackFailedProvisioner;

    impl BundleProvisioner for RollbackFailedProvisioner {
        fn provision<'a>(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _daemon: &'a dyn ManagedDaemon,
        ) -> Result<BootstrapOutcome, BundleProvisionError> {
            Err(BundleProvisionError::RollbackIncomplete)
        }
    }

    const TEST_INSTALLED_INSTALLER: &[u8] = b"test installed Determinate helper";

    struct RealDeterminateFixture {
        temporary: tempfile::TempDir,
    }

    impl RealDeterminateFixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
            let installer = temporary.path().join("nix-installer");
            fs::write(&installer, TEST_INSTALLED_INSTALLER)?;
            fs::set_permissions(&installer, fs::Permissions::from_mode(0o755))?;
            Ok(Self { temporary })
        }

        fn handoff(&self) -> Result<DeterminateHandoff, Box<dyn std::error::Error>> {
            Ok(DeterminateHandoff::for_test_bytes(
                self.temporary.path(),
                0o600,
                TEST_INSTALLED_INSTALLER,
            )?)
        }

        fn receipt(&self) -> std::path::PathBuf {
            self.temporary.path().join("receipt.json")
        }

        fn marker(&self, name: &str) -> std::path::PathBuf {
            self.temporary.path().join(name)
        }

        fn write_receipt(&self) -> Result<(), Box<dyn std::error::Error>> {
            fs::write(self.receipt(), b"opaque test receipt")?;
            fs::set_permissions(self.receipt(), fs::Permissions::from_mode(0o600))?;
            Ok(())
        }
    }

    fn vendor_exit_zero(
        handoff: &DeterminateHandoff,
        receipt: &Path,
        marker: &Path,
    ) -> Result<DeterminateProcessOutcome, BundleProvisionError> {
        if handoff.state() != Ok(DeterminateHandoffState::Started) {
            return Err(BundleProvisionError::Failed);
        }
        let status = std::process::Command::new("/bin/sh")
            .args([
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(
                    "umask 077; printf 'opaque test receipt' > \"$1\"; printf '%s\\n' $$ >> \"$2\"",
                ),
                std::ffi::OsStr::new("determinate-test"),
            ])
            .arg(receipt)
            .arg(marker)
            .status()
            .map_err(|_| BundleProvisionError::Failed)?;
        let terminal = status.code().map_or_else(
            || {
                std::os::unix::process::ExitStatusExt::signal(&status)
                    .map(DeterminateTerminal::Signaled)
                    .ok_or(BundleProvisionError::Failed)
            },
            |code| Ok(DeterminateTerminal::Exited(code)),
        )?;
        Ok(DeterminateProcessOutcome {
            terminal,
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }

    struct RealDeterminateProvisioner {
        handoff: Option<DeterminateHandoff>,
        receipt: std::path::PathBuf,
        marker: std::path::PathBuf,
    }

    impl BundleProvisioner for RealDeterminateProvisioner {
        fn provision<'a>(
            &mut self,
            _request: &InstallerProvisionRequest<'_>,
            _daemon: &'a dyn ManagedDaemon,
        ) -> Result<BootstrapOutcome, BundleProvisionError> {
            let handoff = self.handoff.take().ok_or(BundleProvisionError::Failed)?;
            let outcome = run_with_new_determinate_handoff(&handoff, || {
                vendor_exit_zero(&handoff, &self.receipt, &self.marker)
            })?;
            if !determinate_succeeded(outcome) {
                return Err(BundleProvisionError::RollbackIncomplete);
            }
            Ok(BootstrapOutcome::DeterminateTestPending(Box::new(handoff)))
        }
    }

    fn write_vendor_script(
        fixture: &RealDeterminateFixture,
        name: &str,
        body: &str,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let directory = fixture.temporary.path().join("bin");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let executable = directory.join(name);
        fs::write(&executable, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;
        Ok(executable)
    }

    fn staged_installer_identity(
        path: &Path,
    ) -> Result<DeterminateInstaller, Box<dyn std::error::Error>> {
        let bytes = fs::read(path)?;
        Ok(DeterminateInstaller::new(
            u64::try_from(bytes.len())?,
            Digest::from_bytes(Sha256::digest(bytes).into()),
        ))
    }

    fn restart_refuses_vendor_start(handoff: &DeterminateHandoff, marker: &Path) -> bool {
        let retry = run_with_new_determinate_handoff(handoff, || {
            fs::write(marker, b"second start").map_err(|_| BundleProvisionError::Failed)?;
            Ok(())
        });
        matches!(retry, Err(BundleProvisionError::Failed)) && !marker.exists()
    }

    fn assert_restart_refuses_vendor_start(handoff: &DeterminateHandoff, marker: &Path) {
        assert!(restart_refuses_vendor_start(handoff, marker));
    }

    fn assert_terminal_failure_preserves_started_and_refuses_retry(
        name: &str,
        body: &str,
        expected: DeterminateTerminal,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let handoff = fixture.handoff()?;
        let executable = write_vendor_script(&fixture, name, body)?;
        let outcome = run_with_new_determinate_handoff(&handoff, || {
            crate::determinate::run_test_install_with_process(
                &executable,
                &staged_installer_identity(&executable)
                    .map_err(|_| BundleProvisionError::Failed)?,
                fixture.temporary.path(),
                std::process::Command::spawn,
                std::process::Child::wait,
            )
            .map_err(|_| BundleProvisionError::Failed)
        })
        .map_err(|_| std::io::Error::other("vendor process failed"))?;
        assert_eq!(outcome.terminal, expected);
        assert!(!determinate_succeeded(outcome));
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Started
        );
        assert_restart_refuses_vendor_start(&fixture.handoff()?, &fixture.marker("retry"));
        Ok(())
    }

    #[test]
    fn existing_handoff_refuses_before_vendor_spawn() -> Result<(), Box<dyn std::error::Error>> {
        for accepted in [false, true] {
            let fixture = RealDeterminateFixture::new()?;
            let handoff = fixture.handoff()?;
            handoff.record_started()?;
            if accepted {
                fixture.write_receipt()?;
                handoff.accept_after_installed_state_proof()?;
            }
            assert_restart_refuses_vendor_start(&handoff, &fixture.marker("unexpected-start"));
        }
        Ok(())
    }

    #[test]
    fn spawn_and_wait_uncertainty_preserves_started_and_refuses_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let spawn_fixture = RealDeterminateFixture::new()?;
        let spawn_handoff = spawn_fixture.handoff()?;
        let spawn_executable = write_vendor_script(
            &spawn_fixture,
            "spawn-installer",
            "printf ran > \"$TMPDIR/spawn-ran\"",
        )?;
        let spawn_result = run_with_new_determinate_handoff(&spawn_handoff, || {
            crate::determinate::run_test_install_with_process(
                &spawn_executable,
                &staged_installer_identity(&spawn_executable)
                    .map_err(|_| BundleProvisionError::Failed)?,
                spawn_fixture.temporary.path(),
                |_| Err(std::io::Error::other("simulated spawn failure")),
                std::process::Child::wait,
            )
            .map_err(|_| BundleProvisionError::Failed)
        });
        assert!(matches!(spawn_result, Err(BundleProvisionError::Failed)));
        assert_eq!(spawn_handoff.state()?, DeterminateHandoffState::Started);
        assert!(!spawn_fixture.marker("spawn-ran").exists());
        assert_restart_refuses_vendor_start(
            &spawn_fixture.handoff()?,
            &spawn_fixture.marker("spawn-retry"),
        );

        let wait_fixture = RealDeterminateFixture::new()?;
        let wait_handoff = wait_fixture.handoff()?;
        let wait_executable = write_vendor_script(
            &wait_fixture,
            "wait-installer",
            "printf '%s' $$ > \"$TMPDIR/wait.pid\"; sleep 0.05; exit 0",
        )?;
        let wait_result = run_with_new_determinate_handoff(&wait_handoff, || {
            crate::determinate::run_test_install_with_process(
                &wait_executable,
                &staged_installer_identity(&wait_executable)
                    .map_err(|_| BundleProvisionError::Failed)?,
                wait_fixture.temporary.path(),
                std::process::Command::spawn,
                |_| Err(std::io::Error::other("simulated wait failure")),
            )
            .map_err(|_| BundleProvisionError::Failed)
        });
        assert!(matches!(wait_result, Err(BundleProvisionError::Failed)));
        assert_eq!(wait_handoff.state()?, DeterminateHandoffState::Started);
        let pid = fs::read_to_string(wait_fixture.marker("wait.pid"))?.parse::<i32>()?;
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
        assert_restart_refuses_vendor_start(
            &wait_fixture.handoff()?,
            &wait_fixture.marker("wait-retry"),
        );
        Ok(())
    }

    #[test]
    fn crash_before_vendor_start_preserves_started_and_refuses_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let handoff = fixture.handoff()?;
        let first = run_with_new_determinate_handoff(&handoff, || {
            Err::<(), _>(BundleProvisionError::Failed)
        });
        assert!(matches!(first, Err(BundleProvisionError::Failed)));
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Started
        );
        assert_restart_refuses_vendor_start(&fixture.handoff()?, &fixture.marker("vendor-ran"));
        Ok(())
    }

    #[test]
    fn nonzero_exit_preserves_started_and_refuses_retry() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_terminal_failure_preserves_started_and_refuses_retry(
            "nonzero-installer",
            "exit 23",
            DeterminateTerminal::Exited(23),
        )
    }

    #[test]
    fn signal_preserves_started_and_refuses_retry() -> Result<(), Box<dyn std::error::Error>> {
        assert_terminal_failure_preserves_started_and_refuses_retry(
            "signaled-installer",
            "kill -TERM $$",
            DeterminateTerminal::Signaled(15),
        )
    }

    #[test]
    fn real_supervisor_loss_preserves_started_and_refuses_second_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let checkpoint = ChaosCheckpoint::new("install-supervisor-lost")?;
        if std::env::var_os(SUPERVISOR_LOSS_CHILD_ENV).is_some() {
            let root = std::path::PathBuf::from(
                std::env::var_os(SUPERVISOR_LOSS_ROOT_ENV).ok_or("missing fixture root")?,
            );
            let executable = std::path::PathBuf::from(
                std::env::var_os(SUPERVISOR_LOSS_EXECUTABLE_ENV)
                    .ok_or("missing vendor executable")?,
            );
            let pid_path = root.join("supervisor-loss.pid");
            let handoff =
                DeterminateHandoff::for_test_bytes(&root, 0o600, TEST_INSTALLED_INSTALLER)?;
            let _ = run_with_new_determinate_handoff(&handoff, || {
                crate::determinate::run_test_install_with_process(
                    &executable,
                    &staged_installer_identity(&executable)
                        .map_err(|_| BundleProvisionError::Failed)?,
                    &root,
                    |command| {
                        let mut child = command.spawn()?;
                        let deadline = Instant::now() + Duration::from_secs(10);
                        while !pid_path.try_exists()? {
                            if Instant::now() >= deadline {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "vendor pid was not published",
                                ));
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        let _ = publish_checkpoint(&checkpoint).map_err(std::io::Error::other)?;
                        Ok(child)
                    },
                    std::process::Child::wait,
                )
                .map_err(|_| BundleProvisionError::Failed)
            });
            return Err("supervisor was not terminated at its checkpoint".into());
        }

        let fixture = RealDeterminateFixture::new()?;
        let pid_path = fixture.marker("supervisor-loss.pid");
        let executable = write_vendor_script(
            &fixture,
            "supervisor-loss-installer",
            "printf '%s' $$ > \"$TMPDIR/supervisor-loss.pid\"; \
             attempts=0; \
             while [ ! -e \"$TMPDIR/vendor-release\" ] && [ \"$attempts\" -lt 1000 ]; do \
                 attempts=$((attempts + 1)); sleep 0.01; \
             done; \
             test -e \"$TMPDIR/vendor-release\"",
        )?;
        let mut command = ChaosCommand::new(
            std::env::current_exe()?,
            checkpoint,
            fixture.marker("install-supervisor-lost"),
            FsyncMode::Enabled,
        )?;
        command
            .arg("--exact")
            .arg(
                "bootstrap::tests::real_supervisor_loss_preserves_started_and_refuses_second_start",
            )
            .arg("--nocapture")
            .env(SUPERVISOR_LOSS_CHILD_ENV, "1")
            .env(SUPERVISOR_LOSS_ROOT_ENV, fixture.temporary.path())
            .env(SUPERVISOR_LOSS_EXECUTABLE_ENV, &executable);
        let mut supervisor = command.spawn()?;
        let status = supervisor.kill_at_checkpoint(Duration::from_secs(10))?;
        assert_eq!(status.signal(), Some(9));
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Started
        );
        let pid = fs::read_to_string(pid_path)?.parse::<i32>()?;
        assert_eq!(kill(Pid::from_raw(pid), None), Ok(()));
        fs::write(fixture.marker("vendor-release"), b"release")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while kill(Pid::from_raw(pid), None).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
        assert_restart_refuses_vendor_start(
            &fixture.handoff()?,
            &fixture.marker("second-supervisor-start"),
        );
        Ok(())
    }

    #[test]
    fn only_exit_zero_is_vendor_success() {
        let outcome = |terminal| DeterminateProcessOutcome {
            terminal,
            stdout_truncated: false,
            stderr_truncated: false,
        };

        assert!(determinate_succeeded(outcome(DeterminateTerminal::Exited(
            0
        ))));
        assert!(!determinate_succeeded(outcome(
            DeterminateTerminal::Exited(1)
        )));
        assert!(!determinate_succeeded(outcome(
            DeterminateTerminal::Signaled(15)
        )));
    }

    #[derive(Default, PartialEq, Eq)]
    enum LinuxBackendFailure {
        #[default]
        None,
        Asset,
        Unit,
        BaseNix,
        Activation,
        Health,
        Receipt,
        Finalize,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    enum TestServiceState {
        #[default]
        Stable,
        MutationNeeded,
        Offline,
        EnabledInactive,
        Mixed,
        Unqueryable,
    }

    #[derive(Default)]
    struct LinuxBackend {
        raw_provision_calls: usize,
        create: bool,
        replace_files: bool,
        mode: Option<crate::LinuxInstallMode>,
        active_install: bool,
        active_install_checks: usize,
        clean_host_checks: usize,
        service_state: TestServiceState,
        failure: LinuxBackendFailure,
        preflight_handoff: Option<DeterminateHandoffState>,
        managed_runtime_present: Option<bool>,
        mutation_calls: usize,
        file_mutation_calls: usize,
        offline_preflight_calls: usize,
        change_service_state_after_preflight: Option<usize>,
        rollback_calls: usize,
        finalize_calls: usize,
        finalize_requires_commit: Option<std::rc::Rc<std::cell::Cell<bool>>>,
        events: Vec<&'static str>,
    }

    impl LinuxInstallBackend for LinuxBackend {
        fn install_mode(&self) -> crate::LinuxInstallMode {
            self.mode.unwrap_or(crate::LinuxInstallMode::FreshInstall)
        }

        fn classify_active_install(&mut self) -> Result<bool, InstallError> {
            self.active_install_checks = self.active_install_checks.saturating_add(1);
            Ok(self.active_install)
        }

        fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
            if self.change_service_state_after_preflight == Some(self.offline_preflight_calls) {
                self.service_state = TestServiceState::EnabledInactive;
            }
            self.offline_preflight_calls = self.offline_preflight_calls.saturating_add(1);
            if self.install_mode() != crate::LinuxInstallMode::FreshInstall
                && self.service_state != TestServiceState::Offline
            {
                return Err(InstallError::offline_services_required());
            }
            Ok(())
        }

        fn preflight_fresh_recovery_mutation(
            &mut self,
            journal: &LinuxInstallJournal,
        ) -> Result<(), InstallError> {
            self.offline_preflight_calls = self.offline_preflight_calls.saturating_add(1);
            if journal.mode() != crate::LinuxInstallMode::FreshInstall
                || !journal.fresh_services_deactivated()
                || self.service_state != TestServiceState::Offline
            {
                return Err(InstallError::offline_services_required());
            }
            Ok(())
        }

        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn bind_authenticated_release_identity(
            &mut self,
            _system: System,
            _digest: Digest,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
            self.clean_host_checks = self.clean_host_checks.saturating_add(1);
            if self.mode == Some(crate::LinuxInstallMode::OfflineRepair) {
                if self.preflight_handoff != Some(DeterminateHandoffState::Accepted)
                    || self.service_state != TestServiceState::Offline
                {
                    return Err(InstallError::backend_failure());
                }
                return Ok(());
            }
            self.preflight_handoff.map_or(Ok(()), |state| {
                crate::linux_backend::validate_determinate_handoff_preflight(state).map(|_| ())
            })
        }
        fn preflight_recovery(
            &mut self,
            mode: crate::LinuxInstallMode,
            _system: System,
        ) -> Result<(), InstallError> {
            if self.install_mode() != mode {
                return Err(InstallError::recovery_mode_mismatch());
            }
            if mode != crate::LinuxInstallMode::FreshInstall
                && self.service_state != TestServiceState::Offline
            {
                return Err(InstallError::backend_failure());
            }
            Ok(())
        }
        fn classify_asset(
            &mut self,
            asset: LinuxInstallAsset,
        ) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(
                if self.create
                    || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File)
                {
                    crate::LinuxAssetPresence::Absent
                } else {
                    crate::LinuxAssetPresence::ExactPresent
                },
            )
        }
        fn classify_ownership_receipt(
            &mut self,
            asset: LinuxInstallAsset,
        ) -> Result<crate::LinuxAssetPresence, InstallError> {
            if self.mode == Some(crate::LinuxInstallMode::OfflineRepair) {
                Ok(crate::LinuxAssetPresence::ExactPresent)
            } else {
                self.classify_asset(asset)
            }
        }
        fn classify_managed_runtime(&mut self) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(if self.managed_runtime_present.unwrap_or(!self.create) {
                crate::LinuxAssetPresence::ExactPresent
            } else {
                crate::LinuxAssetPresence::Absent
            })
        }
        fn classify_services(&mut self) -> Result<crate::LinuxAssetPresence, InstallError> {
            Ok(
                if self.create || self.service_state == TestServiceState::Offline {
                    crate::LinuxAssetPresence::Absent
                } else {
                    crate::LinuxAssetPresence::ExactPresent
                },
            )
        }
        fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
            self.events.push("quiesce-services");
            self.service_state = TestServiceState::Offline;
            Ok(())
        }
        fn services_need_mutation(&self, _prior_active: bool) -> bool {
            self.install_mode() == crate::LinuxInstallMode::FreshInstall && self.create
        }
        fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            if asset.kind() == crate::LinuxAssetKind::File {
                self.file_mutation_calls = self.file_mutation_calls.saturating_add(1);
            }
            self.events.push("ensure-asset");
            if self.failure == LinuxBackendFailure::Asset {
                Err(InstallError::backend_failure())
            } else {
                Ok(self.create
                    || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File))
            }
        }
        fn install_systemd_unit(
            &mut self,
            asset: LinuxInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.file_mutation_calls = self.file_mutation_calls.saturating_add(1);
            if self.failure == LinuxBackendFailure::Unit {
                Err(InstallError::backend_failure())
            } else {
                Ok(self.create
                    || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File))
            }
        }
        fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
            Err(InstallError::backend_failure())
        }
        fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.rollback_calls = self.rollback_calls.saturating_add(1);
            Ok(())
        }
        fn validate_base_nix(&mut self) -> Result<(), InstallError> {
            self.events.push("validate-base-nix");
            if self.failure == LinuxBackendFailure::BaseNix {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
            self.events.push("accept-handoff");
            Ok(())
        }
        fn activate_services(&mut self) -> Result<bool, InstallError> {
            if self.install_mode() != crate::LinuxInstallMode::FreshInstall {
                return (self.service_state == TestServiceState::Offline)
                    .then_some(false)
                    .ok_or_else(InstallError::backend_failure);
            }
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.events.push("activate-services");
            let changed = self.create || self.service_state == TestServiceState::MutationNeeded;
            self.service_state = TestServiceState::Stable;
            if self.failure == LinuxBackendFailure::Activation {
                Err(InstallError::backend_failure())
            } else {
                Ok(changed)
            }
        }
        fn rollback_services(&mut self) -> Result<(), InstallError> {
            if self.install_mode() != crate::LinuxInstallMode::FreshInstall {
                return Err(InstallError::backend_failure());
            }
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.rollback_calls = self.rollback_calls.saturating_add(1);
            self.events.push("quiesce-services");
            self.service_state = TestServiceState::Offline;
            Ok(())
        }
        fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
            self.events.push("resume-services");
            Ok(())
        }
        fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
            if self.install_mode() != crate::LinuxInstallMode::FreshInstall {
                return (self.service_state == TestServiceState::Offline)
                    .then_some(())
                    .ok_or_else(InstallError::backend_failure);
            }
            self.events.push("validate-services");
            if self.failure == LinuxBackendFailure::Health {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.events.push("publish-receipt");
            if self.failure == LinuxBackendFailure::Receipt {
                Err(InstallError::backend_failure())
            } else {
                Ok(self.mode != Some(crate::LinuxInstallMode::OfflineRepair)
                    && (self.create || self.replace_files))
            }
        }
        fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
            self.finalize_calls = self.finalize_calls.saturating_add(1);
            if self
                .finalize_requires_commit
                .as_ref()
                .is_some_and(|committed| !committed.get())
                || self.failure == LinuxBackendFailure::Finalize
            {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn rollback_asset(&mut self, _asset: LinuxInstallAsset) -> Result<(), InstallError> {
            self.mutation_calls = self.mutation_calls.saturating_add(1);
            self.rollback_calls = self.rollback_calls.saturating_add(1);
            self.events.push("rollback-asset");
            Ok(())
        }
    }

    struct RealDeterminateInstallObservation {
        fixture: RealDeterminateFixture,
        result: Result<(), InstallError>,
        rollback_calls: usize,
        accepted_before_journal_completion: bool,
    }

    struct DeterminateJournalPersistence {
        handoff: DeterminateHandoff,
        accepted_before_completion: std::cell::Cell<bool>,
    }

    impl LinuxJournalPersistence for DeterminateJournalPersistence {
        fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
            if !journal.is_committed()
                && self.handoff.state() == Ok(DeterminateHandoffState::Accepted)
            {
                self.accepted_before_completion.set(true);
            }
            Ok(())
        }
    }

    fn run_real_determinate_install(
        failure: LinuxBackendFailure,
    ) -> Result<RealDeterminateInstallObservation, Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut provisioner = RealDeterminateProvisioner {
            handoff: Some(fixture.handoff()?),
            receipt: fixture.receipt(),
            marker: fixture.marker("vendor-starts"),
        };
        let persistence = DeterminateJournalPersistence {
            handoff: fixture.handoff()?,
            accepted_before_completion: std::cell::Cell::new(false),
        };
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            request.system,
            Digest::from_bytes([0xb1; 32]),
            Digest::from_bytes([0xb2; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            failure,
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
        Ok(RealDeterminateInstallObservation {
            fixture,
            result,
            rollback_calls: backend.rollback_calls,
            accepted_before_journal_completion: persistence.accepted_before_completion.get(),
        })
    }

    #[test]
    fn started_handoff_preflight_prevents_product_mutation_and_vendor_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
            create: true,
            preflight_handoff: Some(DeterminateHandoffState::Started),
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
            Err(crate::InstallErrorCode::UnmanagedNix)
        );
        assert_eq!(backend.mutation_calls, 0);
        assert_eq!(provisioner.calls, 0);
        assert!(!rolled_back.get());
        Ok(())
    }

    #[test]
    fn crash_after_exit_zero_before_acceptance_preserves_started()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let handoff = fixture.handoff()?;
        let outcome = run_with_new_determinate_handoff(&handoff, || {
            vendor_exit_zero(
                &handoff,
                &fixture.receipt(),
                &fixture.marker("vendor-starts"),
            )
        })
        .map_err(|_| "vendor run failed")?;
        assert!(determinate_succeeded(outcome));
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Started
        );
        assert_eq!(
            fs::read_to_string(fixture.marker("vendor-starts"))?
                .lines()
                .count(),
            1
        );
        assert_restart_refuses_vendor_start(
            &fixture.handoff()?,
            &fixture.marker("post-exit-retry"),
        );
        Ok(())
    }

    #[test]
    fn failed_installed_state_validation_preserves_started()
    -> Result<(), Box<dyn std::error::Error>> {
        let observation = run_real_determinate_install(LinuxBackendFailure::BaseNix)?;
        assert!(observation.result.is_err());
        assert_eq!(
            observation.fixture.handoff()?.state()?,
            DeterminateHandoffState::Started
        );
        assert_eq!(
            fs::read_to_string(observation.fixture.marker("vendor-starts"))?
                .lines()
                .count(),
            1
        );
        assert_restart_refuses_vendor_start(
            &observation.fixture.handoff()?,
            &observation.fixture.marker("health-retry"),
        );
        assert!(observation.rollback_calls > 0);
        Ok(())
    }

    #[test]
    fn failed_product_receipt_publication_keeps_accepted_handoff()
    -> Result<(), Box<dyn std::error::Error>> {
        let observation = run_real_determinate_install(LinuxBackendFailure::Receipt)?;
        assert!(observation.result.is_err());
        assert_eq!(
            observation.fixture.handoff()?.state()?,
            DeterminateHandoffState::Accepted
        );
        assert_eq!(
            fs::read_to_string(observation.fixture.marker("vendor-starts"))?
                .lines()
                .count(),
            1
        );
        assert_restart_refuses_vendor_start(
            &observation.fixture.handoff()?,
            &observation.fixture.marker("receipt-retry"),
        );
        assert!(observation.rollback_calls > 0);
        Ok(())
    }

    #[test]
    fn accepted_fresh_install_continues_with_the_same_journal_on_the_next_invocation()
    -> Result<(), Box<dyn std::error::Error>> {
        // The public entry authenticates before calling this core. The small signed
        // fixture has no production-pinned Determinate target, so a positive load
        // remains part of the native real-release-bundle proof.
        for failure in [
            LinuxBackendFailure::Activation,
            LinuxBackendFailure::Health,
            LinuxBackendFailure::Receipt,
        ] {
            assert_accepted_fresh_continuation(failure)?;
        }
        Ok(())
    }

    #[test]
    fn exact_active_install_returns_without_journal_or_provisioning()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let scratch_parent = temporary.path().join("scratch");
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: temporary.path(),
            installation_root: Path::new("/"),
            scratch_parent: &scratch_parent,
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let location = LinuxJournalLocation::At {
            base: temporary.path().to_path_buf(),
            user_id: nix::unistd::Uid::effective().as_raw(),
            group_id: nix::unistd::Gid::effective().as_raw(),
        };
        let release = Digest::from_bytes([0xe1; 32]);
        let context = linux_recovery_context_digest(release, &request);
        let mut backend = LinuxBackend {
            active_install: true,
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let report = continue_linux_bundle_install(
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            release,
            &location,
        )?;

        assert_eq!(report.platform().created_artifacts(), 0);
        assert_eq!(
            report.platform().existing_artifacts(),
            crate::assets::linux_product_mutation_assets().count()
        );
        assert_eq!(backend.active_install_checks, 1);
        assert_eq!(backend.clean_host_checks, 0);
        assert_eq!(backend.mutation_calls, 0);
        assert_eq!(backend.raw_provision_calls, 0);
        assert_eq!(provisioner.calls, 0);
        assert!(
            location
                .open_existing(request.system, release, context)?
                .is_none()
        );
        Ok(())
    }

    fn assert_accepted_fresh_continuation(
        first_failure: LinuxBackendFailure,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let journal_root = tempfile::tempdir()?;
        let user_id = nix::unistd::Uid::effective().as_raw();
        let group_id = nix::unistd::Gid::effective().as_raw();
        let journal_location = LinuxJournalLocation::At {
            base: journal_root.path().to_path_buf(),
            user_id,
            group_id,
        };
        let scratch_parent = journal_root.path().join("scratch");
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: journal_root.path(),
            installation_root: Path::new("/"),
            scratch_parent: &scratch_parent,
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let release_digest = Digest::from_bytes([0xc1; 32]);
        let recovery_context_digest = linux_recovery_context_digest(release_digest, &request);
        let mut backend = LinuxBackend {
            create: true,
            failure: first_failure,
            managed_runtime_present: Some(false),
            ..LinuxBackend::default()
        };
        let mut first_provisioner = RealDeterminateProvisioner {
            handoff: Some(fixture.handoff()?),
            receipt: fixture.receipt(),
            marker: fixture.marker("vendor-starts"),
        };

        assert_eq!(
            continue_linux_bundle_install(
                &request,
                &StubDaemon,
                &mut backend,
                &mut first_provisioner,
                release_digest,
                &journal_location,
            )
            .map(|_| ())
            .map_err(InstallError::code),
            Err(crate::InstallErrorCode::FreshRecoveryRetained)
        );
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Accepted
        );
        assert_eq!(vendor_start_count(&fixture)?, 1);
        assert_eq!(backend.service_state, TestServiceState::Offline);
        let retained_storage = journal_location
            .open_existing(request.system, release_digest, recovery_context_digest)?
            .ok_or_else(|| std::io::Error::other("missing retained Fresh journal storage"))?;
        let retained = retained_storage
            .load()?
            .ok_or_else(|| std::io::Error::other("missing retained Fresh journal"))?;
        assert_eq!(retained.mode(), crate::LinuxInstallMode::FreshInstall);
        assert!(!retained.is_committed());
        assert!(retained.fresh_services_deactivated());
        drop(retained_storage);

        backend.failure = LinuxBackendFailure::None;
        backend.managed_runtime_present = Some(true);
        backend.preflight_handoff = Some(DeterminateHandoffState::Accepted);
        backend.active_install = true;
        let active_install_checks = backend.active_install_checks;

        let mut second_provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };
        let report = continue_linux_bundle_install(
            &request,
            &StubDaemon,
            &mut backend,
            &mut second_provisioner,
            release_digest,
            &journal_location,
        )?;

        assert_eq!(backend.active_install_checks, active_install_checks);
        assert_eq!(second_provisioner.calls, 0);
        assert_eq!(backend.raw_provision_calls, 0);
        assert_eq!(vendor_start_count(&fixture)?, 1);
        assert_eq!(backend.service_state, TestServiceState::Stable);
        assert_eq!(backend.events.last(), Some(&"publish-receipt"));
        assert!(
            journal_location
                .open_existing(request.system, release_digest, recovery_context_digest)?
                .is_none()
        );
        Ok(())
    }

    fn vendor_start_count(fixture: &RealDeterminateFixture) -> std::io::Result<usize> {
        fs::read_to_string(fixture.marker("vendor-starts")).map(|bytes| bytes.lines().count())
    }

    #[test]
    fn exit_zero_plus_installed_state_validation_accepts_handoff_exactly_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let observation = run_real_determinate_install(LinuxBackendFailure::None)?;
        assert!(observation.result.is_ok());
        assert_eq!(
            observation.fixture.handoff()?.state()?,
            DeterminateHandoffState::Accepted
        );
        assert_eq!(
            fs::read_to_string(observation.fixture.marker("vendor-starts"))?
                .lines()
                .count(),
            1
        );
        let handoff_bytes =
            fs::read_to_string(observation.fixture.marker("determinate-handoff-v1.json"))?;
        assert_eq!(handoff_bytes.matches("\"accepted\"").count(), 1);
        assert_eq!(
            observation
                .fixture
                .handoff()?
                .accept_after_installed_state_proof(),
            Err(crate::determinate_handoff::DeterminateHandoffError::InvalidTransition)
        );
        assert_eq!(observation.rollback_calls, 0);
        assert!(observation.accepted_before_journal_completion);
        Ok(())
    }

    #[test]
    fn journaled_linux_install_persists_each_intent_completion_and_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x91; 32]),
            Digest::from_bytes([0xa1; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            finalize_requires_commit: Some(persistence.committed.clone()),
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
        drop(outcome);
        assert_eq!(
            report.created_artifacts(),
            crate::assets::linux_product_mutation_assets().count()
        );
        let snapshots = persistence.snapshots.borrow();
        assert!(snapshots.len() > crate::assets::linux_product_mutation_assets().count());
        assert!(
            snapshots
                .last()
                .is_some_and(LinuxInstallJournal::is_committed)
        );
        assert_eq!(backend.finalize_calls, 1);
        Ok(())
    }

    #[test]
    fn post_commit_cleanup_failure_keeps_a_resumable_committed_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0xc1; 32]),
            Digest::from_bytes([0xc2; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            failure: LinuxBackendFailure::Finalize,
            finalize_requires_commit: Some(persistence.committed.clone()),
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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

        let error = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )
        .err()
        .ok_or_else(|| std::io::Error::other("expected cleanup failure"))?;
        assert_eq!(error.code(), crate::InstallErrorCode::RollbackIncomplete);
        assert!(persistence.committed.get());
        assert_eq!(backend.finalize_calls, 1);
        assert_eq!(backend.rollback_calls, 0);
        let committed = persistence
            .snapshots
            .borrow()
            .last()
            .cloned()
            .ok_or_else(|| std::io::Error::other("missing committed snapshot"))?;
        assert!(committed.is_committed());

        backend.failure = LinuxBackendFailure::None;
        finalize_committed_linux_install(&committed, &mut backend)?;
        assert_eq!(backend.finalize_calls, 2);
        Ok(())
    }

    #[test]
    fn journaled_linux_install_keeps_uncertain_intent_on_mutation_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x92; 32]),
            Digest::from_bytes([0xa2; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            failure: LinuxBackendFailure::Asset,
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
        let snapshot = persistence
            .snapshots
            .borrow()
            .last()
            .cloned()
            .ok_or_else(|| std::io::Error::other("missing retained recovery snapshot"))?;
        assert!(!snapshot.is_committed());
        assert!(snapshot.recovery_actions().is_empty());
        assert_eq!(snapshot.mutation_state(&mutation)?, None);
        Ok(())
    }

    #[test]
    fn journaled_linux_install_preserves_provision_rollback_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x95; 32]),
            Digest::from_bytes([0xa5; 32]),
        )?;
        let mut backend = LinuxBackend {
            create: true,
            ..LinuxBackend::default()
        };
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
            crate::LinuxInstallMode::OfflineUpgrade,
            System::X8664Linux,
            Digest::from_bytes([0x93; 32]),
            Digest::from_bytes([0xa3; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            mode: Some(crate::LinuxInstallMode::OfflineUpgrade),
            service_state: TestServiceState::Offline,
            ..LinuxBackend::default()
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
        assert_eq!(report.created_artifacts(), 0);
        assert_eq!(
            report.existing_artifacts(),
            crate::assets::linux_product_mutation_assets().count()
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
    fn journaled_existing_product_update_stays_offline_and_never_starts_determinate()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::OfflineUpgrade,
            System::X8664Linux,
            Digest::from_bytes([0xd1; 32]),
            Digest::from_bytes([0xd2; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::LinuxInstallMode::OfflineUpgrade),
            service_state: TestServiceState::Offline,
            preflight_handoff: Some(DeterminateHandoffState::Accepted),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let (_, outcome) = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )?;

        assert!(matches!(outcome, BootstrapOutcome::Existing));
        drop(outcome);
        assert_eq!(provisioner.calls, 0);
        assert_eq!(backend.raw_provision_calls, 0);
        let receipt = backend
            .events
            .iter()
            .position(|event| *event == "publish-receipt")
            .ok_or_else(|| std::io::Error::other("missing receipt publication"))?;
        let base_nix = backend
            .events
            .iter()
            .position(|event| *event == "validate-base-nix")
            .ok_or_else(|| std::io::Error::other("missing Base Nix validation"))?;
        assert!(base_nix < receipt);
        assert!(!backend.events.iter().any(|event| matches!(
            *event,
            "activate-services" | "quiesce-services" | "resume-services"
        )));
        let committed = persistence
            .snapshots
            .borrow()
            .last()
            .cloned()
            .ok_or_else(|| std::io::Error::other("missing committed journal"))?;
        assert!(committed.is_committed());
        assert_eq!(
            committed.mutation_state(&LinuxInstallMutation::Services)?,
            Some(crate::LinuxInstallMutationState::PreExisting)
        );
        Ok(())
    }

    #[test]
    fn offline_state_change_blocks_the_next_file_mutation_and_rollback()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::OfflineUpgrade,
            System::X8664Linux,
            Digest::from_bytes([0xd3; 32]),
            Digest::from_bytes([0xd4; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::LinuxInstallMode::OfflineUpgrade),
            service_state: TestServiceState::Offline,
            change_service_state_after_preflight: Some(1),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        assert_eq!(
            install_linux_with_provisioner_journaled(
                request.system,
                &request,
                &StubDaemon,
                &mut backend,
                &mut provisioner,
                &persistence,
                journal,
            )
            .map(|_| ())
            .map_err(InstallError::code),
            Err(crate::InstallErrorCode::RollbackIncomplete)
        );
        assert_eq!(backend.mutation_calls, 1);
        assert_eq!(backend.file_mutation_calls, 0);
        assert!(backend.offline_preflight_calls >= 3);
        assert_eq!(backend.service_state, TestServiceState::EnabledInactive);
        Ok(())
    }

    #[test]
    fn journaled_offline_repair_changes_product_files_without_service_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        // This is the closest injectable seam below `install_linux_from_bundle`.
        // The public entry owns real signed TUF loading and fixed root-only `/run`
        // journal storage, so the native clean-host proof covers that final boundary.
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xd3; 32]),
            Digest::from_bytes([0xd4; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::LinuxInstallMode::OfflineRepair),
            service_state: TestServiceState::Offline,
            preflight_handoff: Some(DeterminateHandoffState::Accepted),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )?;

        assert_eq!(provisioner.calls, 0);
        assert_eq!(backend.raw_provision_calls, 0);
        assert!(backend.events.contains(&"ensure-asset"));
        assert!(!backend.events.iter().any(|event| matches!(
            *event,
            "activate-services" | "quiesce-services" | "resume-services" | "validate-services"
        )));
        let committed = persistence
            .snapshots
            .borrow()
            .last()
            .cloned()
            .ok_or_else(|| std::io::Error::other("missing committed repair journal"))?;
        assert!(committed.is_committed());
        assert_eq!(
            committed.mutation_state(&LinuxInstallMutation::Services)?,
            Some(crate::LinuxInstallMutationState::PreExisting)
        );
        Ok(())
    }

    #[test]
    fn journaled_repair_refuses_non_offline_service_state_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        for state in [
            TestServiceState::Stable,
            TestServiceState::MutationNeeded,
            TestServiceState::EnabledInactive,
            TestServiceState::Mixed,
            TestServiceState::Unqueryable,
        ] {
            let persistence = MemoryJournalPersistence::default();
            let journal = LinuxInstallJournal::new(
                crate::LinuxInstallMode::OfflineRepair,
                System::X8664Linux,
                Digest::from_bytes([0xd5; 32]),
                Digest::from_bytes([0xd6; 32]),
            )?;
            let mut backend = LinuxBackend {
                replace_files: true,
                mode: Some(crate::LinuxInstallMode::OfflineRepair),
                service_state: state,
                preflight_handoff: Some(DeterminateHandoffState::Accepted),
                ..LinuxBackend::default()
            };
            let mut provisioner = ReauthProvisioner {
                calls: 0,
                reauthenticated: false,
                reuse_existing: true,
            };

            let result = install_linux_with_provisioner_journaled(
                request.system,
                &request,
                &StubDaemon,
                &mut backend,
                &mut provisioner,
                &persistence,
                journal,
            );
            assert_eq!(
                result.err().map(InstallError::code),
                Some(crate::InstallErrorCode::BackendFailure)
            );
            assert_eq!(backend.mutation_calls, 0);
            assert!(backend.events.is_empty());
            assert_eq!(provisioner.calls, 0);
            assert!(persistence.snapshots.borrow().is_empty());
        }
        Ok(())
    }

    #[test]
    fn recovery_never_switches_between_upgrade_and_repair_modes()
    -> Result<(), Box<dyn std::error::Error>> {
        for (journal_mode, requested_mode) in [
            (
                crate::LinuxInstallMode::OfflineRepair,
                crate::LinuxInstallMode::OfflineUpgrade,
            ),
            (
                crate::LinuxInstallMode::OfflineUpgrade,
                crate::LinuxInstallMode::OfflineRepair,
            ),
        ] {
            let mut journal = LinuxInstallJournal::new(
                journal_mode,
                System::X8664Linux,
                Digest::from_bytes([0xf1; 32]),
                Digest::from_bytes([0xf2; 32]),
            )?;
            let mut backend = LinuxBackend {
                mode: Some(requested_mode),
                service_state: TestServiceState::Offline,
                ..LinuxBackend::default()
            };
            assert_eq!(
                crate::installer::recover_linux_install(
                    &mut journal,
                    &mut backend,
                    &mut || Ok(()),
                    &mut |_| Ok(()),
                )
                .map_err(InstallError::code),
                Err(crate::InstallErrorCode::RecoveryModeMismatch)
            );
            assert_eq!(backend.mutation_calls, 0);
        }
        Ok(())
    }

    #[test]
    fn failed_offline_repair_rolls_forward_files_without_service_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xd7; 32]),
            Digest::from_bytes([0xd8; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::LinuxInstallMode::OfflineRepair),
            service_state: TestServiceState::Offline,
            failure: LinuxBackendFailure::Unit,
            preflight_handoff: Some(DeterminateHandoffState::Accepted),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        );

        assert_eq!(
            result.err().map(InstallError::code),
            Some(crate::InstallErrorCode::RollbackIncomplete)
        );
        assert!(backend.events.contains(&"rollback-asset"));
        assert!(!backend.events.iter().any(|event| matches!(
            *event,
            "activate-services" | "quiesce-services" | "resume-services" | "validate-services"
        )));
        assert_eq!(provisioner.calls, 0);
        assert!(
            persistence
                .snapshots
                .borrow()
                .last()
                .is_some_and(|journal| !journal.is_committed())
        );
        Ok(())
    }

    #[test]
    fn failed_existing_product_update_restores_files_and_stays_offline()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::OfflineUpgrade,
            System::X8664Linux,
            Digest::from_bytes([0xe1; 32]),
            Digest::from_bytes([0xe2; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::LinuxInstallMode::OfflineUpgrade),
            service_state: TestServiceState::Offline,
            failure: LinuxBackendFailure::Receipt,
            preflight_handoff: Some(DeterminateHandoffState::Accepted),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        );

        assert_eq!(
            result.map(|_| ()).map_err(InstallError::code),
            Err(crate::InstallErrorCode::ReceiptFailure)
        );
        assert_eq!(provisioner.calls, 0);
        assert_eq!(backend.raw_provision_calls, 0);
        assert!(backend.events.contains(&"rollback-asset"));
        assert!(backend.events.contains(&"publish-receipt"));
        assert!(!backend.events.iter().any(|event| matches!(
            *event,
            "activate-services" | "quiesce-services" | "resume-services"
        )));
        Ok(())
    }

    #[test]
    fn journaled_linux_reinstall_rolls_back_its_temporary_daemon()
    -> Result<(), Box<dyn std::error::Error>> {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x94; 32]),
            Digest::from_bytes([0xa4; 32]),
        )?;
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
            failure: LinuxBackendFailure::Health,
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

    #[allow(clippy::struct_excessive_bools)]
    #[derive(Default)]
    struct MacBackend {
        raw_provision_calls: usize,
        fail_health: bool,
        fail_finalize: bool,
        fail_receipt: bool,
        create_store: bool,
        runtime_present: bool,
    }

    impl MacOsInstallBackend for MacBackend {
        fn bind_authenticated_installer_payloads(
            &mut self,
            _payloads: &AuthenticatedInstallerPayloads,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn bind_authenticated_release_identity(
            &mut self,
            _system: System,
            _release_identity_digest: Digest,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn begin_authenticated_recovery(
            &mut self,
            _mode: crate::MacOsInstallMode,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), MacOsError> {
            Ok(())
        }
        fn broker_uid(&mut self) -> Result<u32, MacOsError> {
            Ok(333)
        }
        fn classify_asset(
            &mut self,
            _asset: MacOsInstallAsset,
        ) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(MacOsAssetPresence::ExactPresent)
        }
        fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(if self.runtime_present {
                MacOsAssetPresence::ExactPresent
            } else {
                MacOsAssetPresence::Absent
            })
        }
        fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(MacOsAssetPresence::ExactPresent)
        }
        fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(MacOsAssetPresence::Absent)
        }
        fn recover_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            Ok(())
        }
        fn recover_services(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            Ok(self.create_store && asset.id() == "nix-root")
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
        fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
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
        fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
            if self.fail_receipt {
                Err(MacOsError::backend_failure())
            } else {
                Ok(true)
            }
        }
        fn rollback_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            Ok(())
        }
        fn finalize_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            if self.fail_finalize {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(if self.create_store {
                MacOsAssetPresence::Absent
            } else {
                MacOsAssetPresence::ExactPresent
            })
        }
        fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
            Ok(self.create_store)
        }
        fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
    }

    #[test]
    fn linux_adapter_routes_runtime_only_through_authenticated_provisioner()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend::default();
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: false,
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
        assert!(provisioner.reauthenticated);
        assert_eq!(backend.raw_provision_calls, 0);
        Ok(())
    }

    #[test]
    fn exact_linux_runtime_does_not_reacquire_the_broker_channel_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::X8664Linux,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = LinuxBackend::default();
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let (report, outcome) = install_linux_with_provisioner(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
        )?;

        assert!(matches!(&outcome, BootstrapOutcome::Existing));
        drop(outcome);
        assert_eq!(report.created_artifacts(), 0);
        assert_eq!(provisioner.calls, 0);
        assert!(!provisioner.reauthenticated);
        Ok(())
    }

    #[test]
    fn linux_adapter_rolls_back_through_the_authenticated_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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
            failure: LinuxBackendFailure::Health,
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
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
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

    #[test]
    fn journaled_macos_install_persists_receipt_last_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::Aarch64Darwin,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let persistence = MacMemoryJournalPersistence::default();
        let journal = MacOsInstallJournal::new(
            request.system,
            Digest::from_bytes([0x95; 32]),
            Digest::from_bytes([0xa5; 32]),
        )?;
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        let mut backend = MacBackend {
            create_store: false,
            ..MacBackend::default()
        };

        let (report, outcome) = install_macos_with_provisioner_journaled(
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
        let snapshots = persistence.snapshots.borrow();
        let committed = snapshots
            .last()
            .ok_or_else(|| std::io::Error::other("missing committed snapshot"))?;
        assert!(committed.is_committed());
        assert_eq!(
            committed.mutation_state(&MacOsInstallMutation::OwnershipReceipt)?,
            Some(crate::MacOsInstallMutationState::Created)
        );
        assert_eq!(
            committed.mutation_state(&MacOsInstallMutation::Asset {
                id: "nix-root".to_owned(),
            })?,
            Some(crate::MacOsInstallMutationState::PreExisting)
        );
        Ok(())
    }

    #[test]
    fn committed_macos_cleanup_failure_retains_a_resumable_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: Path::new("/state"),
            installation_root: Path::new("/"),
            scratch_parent: Path::new("/scratch"),
            system: System::Aarch64Darwin,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let persistence = MacMemoryJournalPersistence::default();
        let journal = MacOsInstallJournal::new(
            request.system,
            Digest::from_bytes([0x96; 32]),
            Digest::from_bytes([0xa6; 32]),
        )?;
        let mut provisioner = StubProvisioner {
            calls: 0,
            rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        let mut backend = MacBackend {
            fail_finalize: true,
            ..MacBackend::default()
        };

        let result = install_macos_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        );

        assert_eq!(
            result.map(|_| ()).map_err(MacOsError::code),
            Err(crate::MacOsErrorCode::RollbackIncomplete)
        );
        assert!(
            persistence
                .snapshots
                .borrow()
                .last()
                .is_some_and(MacOsInstallJournal::is_committed)
        );
        Ok(())
    }

    #[test]
    fn accepted_macos_fresh_install_continues_without_a_second_vendor_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = RealDeterminateFixture::new()?;
        let scratch = fixture.temporary.path().join("scratch");
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: fixture.temporary.path(),
            installation_root: Path::new("/"),
            scratch_parent: &scratch,
            system: System::Aarch64Darwin,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let persistence = MacMemoryJournalPersistence::default();
        let journal = MacOsInstallJournal::new(
            request.system,
            Digest::from_bytes([0x97; 32]),
            Digest::from_bytes([0xa7; 32]),
        )?;
        let marker = fixture.marker("macos-vendor-starts");
        let mut first_provisioner = RealDeterminateProvisioner {
            handoff: Some(fixture.handoff()?),
            receipt: fixture.receipt(),
            marker: marker.clone(),
        };
        let mut first_backend = MacBackend {
            fail_receipt: true,
            ..MacBackend::default()
        };

        let first = install_macos_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut first_backend,
            &mut first_provisioner,
            &persistence,
            journal,
        );
        assert_eq!(
            first.map(|_| ()).map_err(MacOsError::code),
            Err(crate::MacOsErrorCode::RollbackIncomplete)
        );
        assert_eq!(
            fixture.handoff()?.state()?,
            DeterminateHandoffState::Accepted
        );
        let retained = persistence
            .snapshots
            .borrow()
            .last()
            .cloned()
            .ok_or_else(|| std::io::Error::other("missing retained macOS journal"))?;
        assert!(!retained.is_committed());

        let mut second_backend = MacBackend {
            runtime_present: true,
            ..MacBackend::default()
        };
        let mut second_provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };
        let (_, outcome) = install_macos_with_provisioner_journaled(
            request.system,
            &request,
            &StubDaemon,
            &mut second_backend,
            &mut second_provisioner,
            &persistence,
            retained,
        )?;

        assert!(matches!(outcome, BootstrapOutcome::Existing));
        drop(outcome);
        assert_eq!(second_provisioner.calls, 0);
        assert_eq!(fs::read_to_string(marker)?.lines().count(), 1);
        assert!(
            persistence
                .snapshots
                .borrow()
                .last()
                .is_some_and(MacOsInstallJournal::is_committed)
        );
        Ok(())
    }

    #[test]
    fn macos_recovery_returns_uncommitted_fresh_storage_instead_of_deleting_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        let ownership = Digest::from_bytes([0x98; 32]);
        let context = Digest::from_bytes([0xa8; 32]);
        let uid = nix::unistd::Uid::current().as_raw();
        let gid = nix::unistd::Gid::current().as_raw();
        let storage = MacOsInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::Aarch64Darwin,
            ownership,
            context,
        )?;
        let journal = MacOsInstallJournal::new(System::Aarch64Darwin, ownership, context)?;
        storage.create(&journal)?;
        let scratch = temporary.path().join("scratch");
        let request = InstallerProvisionRequest {
            repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
            datastore: temporary.path(),
            installation_root: Path::new("/"),
            scratch_parent: &scratch,
            system: System::Aarch64Darwin,
            groups: ManagedGroupBindings::new(100, 101)?,
        };
        let mut backend = MacBackend::default();

        let (storage, recovered) =
            recover_macos_bundle_install_from_storage(storage, &request, &mut backend)?
                .ok_or_else(|| std::io::Error::other("fresh journal was not retained"))?;

        assert_eq!(recovered, journal);
        assert_eq!(storage.load()?, Some(journal));
        storage.remove()?;
        Ok(())
    }
}
