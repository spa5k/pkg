//! Product-only macOS uninstall binding for Determinate-owned Base Nix.

use std::path::Path;

use nix::unistd::{Gid, Uid};
use pkg_core::{System, state::Digest};
use pkg_nix::{AuthenticatedInstallerPayloads, ManagedGroupBindings};
use sha2::{Digest as _, Sha256};

use crate::{
    MacOsAssetKind, MacOsAssetPresence, MacOsInstallAsset, MacOsInstallJournal,
    MacOsInstallJournalStorage, RecordedAssetState, UninstallAction, UninstallAssetKind,
    UninstallBackend, UninstallError, UninstallManifest,
    determinate::DeterminateInstaller,
    determinate_handoff::{DeterminateHandoff, DeterminateHandoffState},
    linux_user_cleanup::LinuxUserCleanup,
    macos_launchd::MacOsLaunchdManager,
    macos_platform_assets::MacOsPlatformAssetManager,
    macos_product_install_assets,
};

const UNINSTALL_RECEIPT: &str = "/opt/pkg/uninstall/manifest.json";
const PRODUCT_PRIVATE_STATE: [&str; 2] = [
    "/private/var/db/pkg-install-auth",
    "/private/var/db/pkg-install-journal",
];

/// Product cleanup followed by terminal vendor uninstall.
#[allow(clippy::struct_excessive_bools)]
pub struct ProductionMacOsUninstallBackend {
    system: System,
    release_digest: Digest,
    groups: ManagedGroupBindings,
    assets: MacOsPlatformAssetManager,
    user_cleanup: LinuxUserCleanup,
    determinate: DeterminateInstaller,
    handoff: DeterminateHandoff,
    manifest: Option<UninstallManifest>,
    uninstall_journal: Option<MacOsInstallJournalStorage>,
    recovery_context: Option<Digest>,
    recovery_mode: bool,
    services_stopped: bool,
    roots_removed: bool,
    registered_state_removed: bool,
}

impl ProductionMacOsUninstallBackend {
    /// Binds the exact product release and vendor executable.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for unsupported identity or invalid payloads.
    pub fn new(
        system: System,
        release_digest: Digest,
        groups: ManagedGroupBindings,
        payloads: &AuthenticatedInstallerPayloads,
        determinate: DeterminateInstaller,
    ) -> Result<Self, UninstallError> {
        if system != System::Aarch64Darwin || payloads.system() != system {
            return Err(UninstallError::backend_failure());
        }
        let mut assets = MacOsPlatformAssetManager::new(groups)
            .map_err(|_| UninstallError::backend_failure())?;
        assets
            .bind_authenticated_installer_payloads(payloads)
            .and_then(|()| assets.bind_authenticated_ownership(system, release_digest))
            .map_err(|_| UninstallError::backend_failure())?;
        Ok(Self {
            system,
            release_digest,
            groups,
            assets,
            user_cleanup: LinuxUserCleanup::production(),
            determinate,
            handoff: DeterminateHandoff::production()
                .map_err(|_| UninstallError::backend_failure())?,
            manifest: None,
            uninstall_journal: None,
            recovery_context: None,
            recovery_mode: false,
            services_stopped: false,
            roots_removed: false,
            registered_state_removed: false,
        })
    }

    /// Loads the product receipt or its durable uninstall marker.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for changed, unsafe, or non-Accepted state.
    pub fn installed_manifest(&mut self) -> Result<Option<UninstallManifest>, UninstallError> {
        self.require_accepted_handoff()?;
        if !path_is_absent(Path::new(UNINSTALL_RECEIPT))? {
            return self
                .assets
                .installed_uninstall_manifest()
                .map_err(|_| UninstallError::backend_failure());
        }
        match crate::macos_accounts::broker_account_presence(self.groups)
            .map_err(|_| UninstallError::backend_failure())?
        {
            MacOsAssetPresence::ExactPresent => {}
            MacOsAssetPresence::Absent => self
                .assets
                .bind_filesystem_after_broker_removal()
                .map_err(|_| UninstallError::backend_failure())?,
        }
        let manifest = self
            .assets
            .expected_product_manifest(self.system, self.release_digest)
            .map_err(|_| UninstallError::backend_failure())?;
        let context = uninstall_recovery_context_digest(&manifest)?;
        let Some(storage) =
            MacOsInstallJournalStorage::open_existing(self.system, self.release_digest, context)
                .map_err(|_| UninstallError::backend_failure())?
        else {
            if verify_macos_install_absent().is_err() {
                return Ok(None);
            }
            self.recovery_mode = true;
            self.recovery_context = Some(context);
            return Ok(Some(manifest));
        };
        let marker = require_uninstall_marker(&storage)?;
        self.bind_user_snapshot(&marker)?;
        self.recovery_mode = true;
        self.recovery_context = Some(context);
        self.uninstall_journal = Some(storage);
        Ok(Some(manifest))
    }

    fn require_accepted_handoff(&self) -> Result<(), UninstallError> {
        if self
            .handoff
            .state()
            .map_err(|_| UninstallError::backend_failure())?
            != DeterminateHandoffState::Accepted
        {
            return Err(UninstallError::backend_failure());
        }
        Ok(())
    }

    fn bind_marker(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError> {
        let context = uninstall_recovery_context_digest(manifest)?;
        if self.uninstall_journal.is_none()
            && let Some(storage) =
                MacOsInstallJournalStorage::open_existing(self.system, self.release_digest, context)
                    .map_err(|_| UninstallError::backend_failure())?
        {
            let marker = require_uninstall_marker(&storage)?;
            self.bind_user_snapshot(&marker)?;
            self.recovery_mode = true;
            self.uninstall_journal = Some(storage);
        }
        self.recovery_context = Some(context);
        Ok(())
    }

    fn start_marker(&mut self) -> Result<(), UninstallError> {
        if self.uninstall_journal.is_some() {
            return Ok(());
        }
        let context = self
            .recovery_context
            .ok_or_else(UninstallError::backend_failure)?;
        let storage =
            MacOsInstallJournalStorage::prepare(self.system, self.release_digest, context)
                .map_err(|_| UninstallError::backend_failure())?;
        let marker = MacOsInstallJournal::new(self.system, self.release_digest, context)
            .map_err(|_| UninstallError::backend_failure())?;
        storage
            .create(&marker)
            .map_err(|_| UninstallError::backend_failure())?;
        self.uninstall_journal = Some(storage);
        Ok(())
    }

    fn bind_user_snapshot(&mut self, marker: &MacOsInstallJournal) -> Result<(), UninstallError> {
        if let Some(uids) = marker.uninstall_registered_uids() {
            self.user_cleanup
                .bind_registered_uids(uids)
                .map_err(|_| UninstallError::backend_failure())?;
        }
        Ok(())
    }

    fn persist_user_snapshot(&mut self) -> Result<(), UninstallError> {
        let storage = self
            .uninstall_journal
            .as_ref()
            .ok_or_else(UninstallError::backend_failure)?;
        persist_user_snapshot_before_root_removal(storage, &mut self.user_cleanup)
    }

    fn remove_asset(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        let UninstallAction::RemoveAsset { id, kind, target } = action else {
            return Err(UninstallError::backend_failure());
        };
        let asset = product_asset(id).ok_or_else(UninstallError::backend_failure)?;
        if uninstall_kind(asset.kind()) != kind || asset.path_or_name() != target {
            return Err(UninstallError::backend_failure());
        }
        if self.recovery_mode
            && matches!(
                asset.kind(),
                MacOsAssetKind::Directory | MacOsAssetKind::File
            )
            && path_is_absent(Path::new(asset.path_or_name()))?
        {
            return Ok(());
        }
        self.assets
            .remove_uninstall_asset(asset)
            .map_err(|_| UninstallError::backend_failure())
    }

    fn verify_product_absent(&mut self) -> Result<(), UninstallError> {
        MacOsLaunchdManager::require_offline().map_err(|_| UninstallError::backend_failure())?;
        self.user_cleanup
            .verify_absent()
            .map_err(|_| UninstallError::backend_failure())?;
        for asset in macos_product_install_assets() {
            if asset.id() == "nix-root" {
                if self
                    .assets
                    .classify_asset(asset)
                    .map_err(|_| UninstallError::backend_failure())?
                    != MacOsAssetPresence::ExactPresent
                {
                    return Err(UninstallError::backend_failure());
                }
                continue;
            }
            if self
                .assets
                .classify_for_removal(asset)
                .map_err(|_| UninstallError::backend_failure())?
                != MacOsAssetPresence::Absent
            {
                return Err(UninstallError::backend_failure());
            }
        }
        self.require_accepted_handoff()?;
        Ok(())
    }
}

impl UninstallBackend for ProductionMacOsUninstallBackend {
    fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
        if Uid::effective().is_root() && Gid::effective().as_raw() == 0 {
            Ok(())
        } else {
            Err(UninstallError::backend_failure())
        }
    }

    fn verify_ownership(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError> {
        if manifest.system() != self.system
            || manifest.ownership_manifest_digest() != self.release_digest
            || self
                .manifest
                .as_ref()
                .is_some_and(|bound| bound != manifest)
        {
            return Err(UninstallError::backend_failure());
        }
        self.require_accepted_handoff()?;
        self.bind_marker(manifest)?;
        self.assets
            .bind_uninstall_manifest(manifest)
            .map_err(|_| UninstallError::backend_failure())?;
        for record in manifest.assets() {
            let asset = product_asset(record.id()).ok_or_else(UninstallError::backend_failure)?;
            if record.state() == RecordedAssetState::PreExisting {
                if asset.id() != "nix-root"
                    || self
                        .assets
                        .classify_asset(asset)
                        .map_err(|_| UninstallError::backend_failure())?
                        != MacOsAssetPresence::ExactPresent
                {
                    return Err(UninstallError::backend_failure());
                }
            } else if !self.recovery_mode
                && self
                    .assets
                    .classify_asset(asset)
                    .map_err(|_| UninstallError::backend_failure())?
                    != MacOsAssetPresence::ExactPresent
            {
                return Err(UninstallError::backend_failure());
            } else if self.recovery_mode {
                self.assets
                    .classify_for_removal(asset)
                    .map_err(|_| UninstallError::backend_failure())?;
            }
        }
        self.manifest = Some(manifest.clone());
        Ok(())
    }

    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
        self.require_accepted_handoff()
    }

    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        if action != UninstallAction::StopServices && !self.services_stopped {
            return Err(UninstallError::backend_failure());
        }
        match action {
            UninstallAction::StopServices => {
                self.start_marker()?;
                MacOsLaunchdManager::deactivate_verified()
                    .map_err(|_| UninstallError::backend_failure())?;
                self.services_stopped = true;
                Ok(())
            }
            UninstallAction::RemoveUserRoots => {
                self.persist_user_snapshot()?;
                self.user_cleanup
                    .remove_user_roots()
                    .map_err(|_| UninstallError::backend_failure())?;
                self.roots_removed = true;
                Ok(())
            }
            UninstallAction::RemoveRegisteredUserState => {
                if !self.roots_removed {
                    return Err(UninstallError::backend_failure());
                }
                self.user_cleanup
                    .remove_registered_user_state()
                    .map_err(|_| UninstallError::backend_failure())?;
                self.registered_state_removed = true;
                Ok(())
            }
            UninstallAction::RemoveAsset { .. } => {
                if !self.registered_state_removed {
                    return Err(UninstallError::backend_failure());
                }
                self.remove_asset(action)
            }
            UninstallAction::VerifyNoPrivilegedResidue => self.verify_product_absent(),
            UninstallAction::ExecDeterminateUninstall => {
                self.uninstall_journal
                    .take()
                    .ok_or_else(UninstallError::backend_failure)?
                    .remove()
                    .map_err(|_| UninstallError::backend_failure())?;
                self.handoff
                    .run_terminal_uninstall(|| self.determinate.exec_uninstall())
                    .map_err(|_| UninstallError::backend_failure())
            }
            UninstallAction::CollectGarbage
            | UninstallAction::RemoveManagedStoreIfExclusive
            | UninstallAction::RemoveManagedRuntimePreservingStore => {
                Err(UninstallError::backend_failure())
            }
        }
    }
}

fn require_uninstall_marker(
    storage: &MacOsInstallJournalStorage,
) -> Result<MacOsInstallJournal, UninstallError> {
    let marker = storage
        .load()
        .map_err(|_| UninstallError::backend_failure())?
        .ok_or_else(UninstallError::backend_failure)?;
    if !marker.is_uninstall_marker() {
        return Err(UninstallError::backend_failure());
    }
    Ok(marker)
}

fn persist_user_snapshot_before_root_removal(
    storage: &MacOsInstallJournalStorage,
    cleanup: &mut LinuxUserCleanup,
) -> Result<(), UninstallError> {
    let mut marker = require_uninstall_marker(storage)?;
    if let Some(uids) = marker.uninstall_registered_uids() {
        cleanup
            .bind_registered_uids(uids)
            .map_err(|_| UninstallError::backend_failure())?;
        return Ok(());
    }
    cleanup
        .capture_user_roots()
        .map_err(|_| UninstallError::backend_failure())?;
    marker
        .record_uninstall_user_snapshot(
            cleanup
                .registered_uids()
                .ok_or_else(UninstallError::backend_failure)?,
        )
        .map_err(|_| UninstallError::backend_failure())?;
    storage
        .replace(&marker)
        .map_err(|_| UninstallError::backend_failure())
}

fn product_asset(id: &str) -> Option<MacOsInstallAsset> {
    macos_product_install_assets().find(|asset| asset.id() == id)
}

const fn uninstall_kind(kind: MacOsAssetKind) -> UninstallAssetKind {
    match kind {
        MacOsAssetKind::File => UninstallAssetKind::File,
        MacOsAssetKind::Directory => UninstallAssetKind::Directory,
        MacOsAssetKind::User => UninstallAssetKind::User,
        MacOsAssetKind::Group => UninstallAssetKind::Group,
    }
}

fn uninstall_recovery_context_digest(
    manifest: &UninstallManifest,
) -> Result<Digest, UninstallError> {
    let bytes = crate::encode_uninstall_manifest(manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(b"pkg-macos-uninstall-recovery-v2\0");
    hasher.update(bytes.len().to_be_bytes());
    hasher.update(bytes);
    Ok(Digest::from_bytes(hasher.finalize().into()))
}

fn path_is_absent(path: &Path) -> Result<bool, UninstallError> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Err(UninstallError::backend_failure()),
    }
}

/// Verifies that all product-owned macOS state is absent.
///
/// # Errors
///
/// Returns a redacted error when any product asset or private state remains.
pub fn verify_macos_install_absent() -> Result<(), UninstallError> {
    crate::macos_launchd::verify_macos_services_absent()
        .map_err(|_| UninstallError::backend_failure())?;
    let groups = ManagedGroupBindings::new(333, crate::macos_accounts::BUILD_GID)
        .map_err(|_| UninstallError::backend_failure())?;
    if crate::macos_accounts::broker_account_presence(groups)
        .map_err(|_| UninstallError::backend_failure())?
        != MacOsAssetPresence::Absent
    {
        return Err(UninstallError::backend_failure());
    }
    for asset in macos_product_install_assets() {
        if asset.id() == "nix-root" {
            continue;
        }
        if matches!(
            asset.kind(),
            MacOsAssetKind::Directory | MacOsAssetKind::File
        ) && !path_is_absent(Path::new(asset.path_or_name()))?
        {
            return Err(UninstallError::backend_failure());
        }
    }
    verify_private_state_absent(PRODUCT_PRIVATE_STATE.iter().map(Path::new))
}

fn verify_private_state_absent<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<(), UninstallError> {
    for path in paths {
        if !path_is_absent(path)? {
            return Err(UninstallError::backend_failure());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_store::{STATE_OWNERSHIP_MARKER_BYTES, STATE_OWNERSHIP_MARKER_NAME};
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };

    #[test]
    fn private_state_absence_rejects_files_directories_and_dangling_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        assert!(verify_private_state_absent([first.as_path(), second.as_path()]).is_ok());
        fs::write(&first, b"state")?;
        assert!(verify_private_state_absent([first.as_path(), second.as_path()]).is_err());
        fs::remove_file(&first)?;
        fs::create_dir(&first)?;
        assert!(verify_private_state_absent([first.as_path(), second.as_path()]).is_err());
        fs::remove_dir(&first)?;
        symlink(temporary.path().join("missing"), &second)?;
        assert!(verify_private_state_absent([first.as_path(), second.as_path()]).is_err());
        Ok(())
    }

    #[test]
    fn persisted_uid_snapshot_recovers_user_state_after_root_removal_crash()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let home = temporary.path().join("home");
        let state = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/pkg")
        } else {
            home.join(".local/share/pkg")
        };
        let roots = temporary.path().join("nix/var/nix/gcroots/pkg/users");
        fs::create_dir_all(&state)?;
        fs::create_dir_all(&roots)?;
        fs::write(
            state.join(STATE_OWNERSHIP_MARKER_NAME),
            STATE_OWNERSHIP_MARKER_BYTES,
        )?;
        fs::set_permissions(
            state.join(STATE_OWNERSHIP_MARKER_NAME),
            fs::Permissions::from_mode(0o600),
        )?;
        let uid = home.metadata()?.uid();
        if uid == 0 {
            return Err("test requires a non-root uid".into());
        }
        symlink(
            "/nix/store/22222222222222222222222222222222-example",
            roots.join(uid.to_string()),
        )?;

        let ownership = Digest::from_bytes([0x41; 32]);
        let recovery = Digest::from_bytes([0x42; 32]);
        let gid = temporary.path().metadata()?.gid();
        let storage = MacOsInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::Aarch64Darwin,
            ownership,
            recovery,
        )?;
        let marker = MacOsInstallJournal::new(System::Aarch64Darwin, ownership, recovery)?;
        storage.create(&marker)?;

        let mut first_process = LinuxUserCleanup::for_test(temporary.path(), &home)?;
        persist_user_snapshot_before_root_removal(&storage, &mut first_process)?;
        first_process.remove_user_roots()?;
        assert!(state.exists());
        drop(first_process);
        drop(storage);

        let storage = MacOsInstallJournalStorage::prepare_for_test(
            temporary.path(),
            uid,
            gid,
            System::Aarch64Darwin,
            ownership,
            recovery,
        )?;
        let mut second_process = LinuxUserCleanup::for_test(temporary.path(), &home)?;
        persist_user_snapshot_before_root_removal(&storage, &mut second_process)?;
        second_process.remove_user_roots()?;
        second_process.remove_registered_user_state()?;
        second_process.verify_absent()?;
        assert!(!state.exists());
        assert!(storage.load()?.is_some(), "terminal action is still gated");
        Ok(())
    }
}
