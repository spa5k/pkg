//! Journal recovery and interrupted-install reconciliation.

use super::*;
use super::{backend::*, provision::*};
pub(super) fn continue_linux_bundle_install<'a, P: BundleProvisioner>(
    request: &'a InstallerProvisionRequest<'a>,
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

/// Loads the authenticated Linux release without a clean-host authorization scan.
///
/// Journal recovery must authenticate the signed release identity before it
/// can reconcile an interrupted transaction, and an interrupted attempt may
/// have already created fixed platform prerequisites such as build users that
/// a strict pre-mutation scan must refuse to treat as clean. The strict
/// privileged clean-host scan still runs in the caller after recovery and
/// before any new mutation, matching the reviewed macOS flow.
pub(super) fn load_linux_bundle_for_recovery(
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

pub(super) fn prepare_linux_auth_datastore() -> Result<PathBuf, InstallError> {
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

pub(super) fn prepare_private_directory_at(
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
pub(super) fn prepare_vendor_tmp_directory_at(
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

pub(super) fn remove_stale_linux_auth_datastores(
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

pub(super) fn remove_legacy_linux_auth_datastore_files(
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

pub(super) fn process_is_alive(pid: u32) -> Result<bool, InstallError> {
    let pid = i32::try_from(pid).map_err(|_| InstallError::backend_failure())?;
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(_) => Err(InstallError::backend_failure()),
    }
}

pub(super) fn prepare_linux_auth_datastore_at(
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

pub(super) fn remove_linux_auth_datastore(path: &Path) -> Result<(), InstallError> {
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

pub(super) fn remove_linux_auth_datastore_at(
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

pub(super) enum LinuxBundleRecovery {
    None,
    Committed,
    Fresh {
        storage: LinuxInstallJournalStorage,
        journal: Box<LinuxInstallJournal>,
    },
}

pub(super) enum LinuxJournalLocation {
    Production,
    #[cfg(test)]
    At {
        base: PathBuf,
        user_id: u32,
        group_id: u32,
    },
}

impl LinuxJournalLocation {
    pub(super) fn open_existing(
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

    pub(super) fn prepare(
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

pub(super) fn recover_linux_bundle_install(
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
        if journal.mode() == crate::InstallMode::FreshInstall {
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

pub(super) fn linux_recovery_context_digest(
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

pub(super) const fn linux_journal_file_error(error: LinuxInstallJournalFileError) -> InstallError {
    if matches!(
        error.code(),
        LinuxInstallJournalFileErrorCode::UnsupportedSchema
    ) {
        InstallError::unsupported_recovery_schema()
    } else {
        InstallError::backend_failure()
    }
}

pub(super) fn load_macos_bundle_for_recovery(
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

pub(super) fn prepare_macos_auth_datastore() -> Result<PathBuf, MacOsError> {
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

pub(super) fn recover_macos_bundle_install(
    system: System,
    digest: Digest,
    recovery_context_digest: Digest,
    request: &InstallerProvisionRequest<'_>,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<Option<MacOsJournalPair>, MacOsError> {
    let Some(storage) =
        MacOsInstallJournalStorage::open_existing(system, digest, recovery_context_digest)
            .map_err(|_| MacOsError::backend_failure())?
    else {
        return Ok(None);
    };
    recover_macos_bundle_install_from_storage(storage, request, backend)
}

pub(super) fn recover_macos_bundle_install_from_storage(
    storage: MacOsInstallJournalStorage,
    request: &InstallerProvisionRequest<'_>,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<Option<MacOsJournalPair>, MacOsError> {
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

pub(super) fn macos_recovery_context_digest(
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
