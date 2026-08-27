//! Race-resistant installation of the closed Linux filesystem asset set.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use nix::unistd::{Gid, Uid};
use pkg_core::state::{Digest, body_digest};
use pkg_nix::{
    AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig, ManagedGroupBindings,
};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fchmod, fsync, mkdirat, open, openat,
    renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use sha2::{Digest as _, Sha256};

use crate::bootstrap::validate_linux_auth_datastore_file;
use crate::linux_user_cleanup::remove_owned_tree;
use crate::{
    LinuxAssetKind, LinuxAssetPrincipal, LinuxInstallAsset, LinuxSystemdAssets, MacOsLaunchdAssets,
    UninstallManifest, assets::is_linux_product_asset, encode_uninstall_manifest,
};

const MAX_RELEASE_BINARY_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_UNINSTALL_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PRIVATE_STATE_FILES: usize = 1_024;
const PROFILE_SNIPPET: &[u8] = b"# managed by pkg \xE2\x80\x94 do not edit\n\
__pkg_state=\"$HOME/.local/share/pkg\"\n\
case \":$PATH:\" in\n\
  *\":$__pkg_state/current/bin:\"*) ;;\n\
  *) PATH=\"$__pkg_state/current/bin:$PATH\" ;;\n\
esac\n\
export MANPATH=\"$__pkg_state/current/share/man:${MANPATH:-}\"\n\
unset __pkg_state\n";

/// Stable failure classes for the privileged filesystem boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxFilesystemErrorCode {
    /// The requested artifact is not a filesystem artifact owned by this component.
    UnsupportedAsset,
    /// Required authenticated or release-supplied bytes were not bound.
    MissingPayload,
    /// An existing path does not match the closed asset contract.
    Conflict,
    /// A path contains a symlink, unexpected type, or unsafe component.
    UnsafeFilesystemState,
    /// A bounded filesystem operation failed.
    IoFailure,
    /// Attempt-owned rollback could not prove the target identity.
    RollbackConflict,
}

/// Redacted Linux filesystem failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxFilesystemError {
    code: LinuxFilesystemErrorCode,
}

impl LinuxFilesystemError {
    const fn new(code: LinuxFilesystemErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable public failure class.
    #[must_use]
    pub const fn code(self) -> LinuxFilesystemErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxFilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux filesystem installation failed")
    }
}

impl std::error::Error for LinuxFilesystemError {}

/// Exact release binary bytes supplied by the authenticated release boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxReleasePayloads {
    root_helper: Arc<[u8]>,
    broker: Arc<[u8]>,
    product_cli: Arc<[u8]>,
}

impl fmt::Debug for LinuxReleasePayloads {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxReleasePayloads")
            .field("root_helper_bytes", &self.root_helper.len())
            .field("broker_bytes", &self.broker.len())
            .field("product_cli_bytes", &self.product_cli.len())
            .finish()
    }
}

impl LinuxReleasePayloads {
    pub(crate) fn from_authenticated_bundle(
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<Self, LinuxFilesystemError> {
        Self::from_authenticated_bytes(
            payloads.root_helper(),
            payloads.broker(),
            payloads.product_cli(),
        )
    }

    /// Binds exact bytes that an outer release verifier already authenticated.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when a payload is empty or exceeds the fixed cap.
    pub fn from_authenticated_bytes(
        root_helper: &[u8],
        broker: &[u8],
        product_cli: &[u8],
    ) -> Result<Self, LinuxFilesystemError> {
        for payload in [root_helper, broker, product_cli] {
            if payload.is_empty() || payload.len() > MAX_RELEASE_BINARY_BYTES {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::MissingPayload,
                ));
            }
        }
        Ok(Self {
            root_helper: Arc::from(root_helper),
            broker: Arc::from(broker),
            product_cli: Arc::from(product_cli),
        })
    }

    fn for_asset(&self, asset: LinuxInstallAsset) -> Option<Arc<[u8]>> {
        match asset.id() {
            "root-helper-binary" | "helper-binary" => Some(Arc::clone(&self.root_helper)),
            "broker-binary" => Some(Arc::clone(&self.broker)),
            "product-cli" => Some(Arc::clone(&self.product_cli)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PrincipalBindings {
    root_uid: u32,
    root_gid: u32,
    broker_uid: Option<u32>,
    broker_gid: u32,
    build_users_gid: u32,
}

impl PrincipalBindings {
    const fn production(
        groups: ManagedGroupBindings,
        broker_uid: u32,
    ) -> Result<Self, LinuxFilesystemError> {
        if broker_uid == 0 {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        Ok(Self {
            root_uid: 0,
            root_gid: 0,
            broker_uid: Some(broker_uid),
            broker_gid: groups.broker_gid(),
            build_users_gid: groups.build_users_gid(),
        })
    }

    fn owner(self, principal: LinuxAssetPrincipal) -> Result<u32, LinuxFilesystemError> {
        match principal {
            LinuxAssetPrincipal::Broker => self.broker_uid.ok_or_else(unsupported),
            LinuxAssetPrincipal::Root | LinuxAssetPrincipal::BuildUsers => Ok(self.root_uid),
        }
    }

    const fn group(self, principal: LinuxAssetPrincipal) -> u32 {
        match principal {
            LinuxAssetPrincipal::Root => self.root_gid,
            LinuxAssetPrincipal::Broker => self.broker_gid,
            LinuxAssetPrincipal::BuildUsers => self.build_users_gid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    kind: LinuxAssetKind,
}

#[derive(Debug, Clone)]
struct StagingIdentity {
    name: OsString,
    identity: FileIdentity,
}

#[derive(Debug, Clone)]
struct AttemptOwnership {
    target: Option<FileIdentity>,
    staging: Option<StagingIdentity>,
    previous: Option<StagingIdentity>,
    target_uncertain: bool,
}

impl AttemptOwnership {
    const fn pending() -> Self {
        Self {
            target: None,
            staging: None,
            previous: None,
            target_uncertain: false,
        }
    }
}

/// Installs fixed Linux directories and files without following path symlinks.
pub struct LinuxFilesystemManager {
    root: PathBuf,
    principals: PrincipalBindings,
    payloads: LinuxReleasePayloads,
    authenticated_config: Option<Arc<[u8]>>,
    uninstall_manifest: Option<Arc<[u8]>>,
    attempt_owned: BTreeMap<&'static str, AttemptOwnership>,
}

impl fmt::Debug for LinuxFilesystemManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxFilesystemManager")
            .field("root", &self.root)
            .field("payloads", &self.payloads)
            .field("config_bound", &self.authenticated_config.is_some())
            .field(
                "uninstall_manifest_bound",
                &self.uninstall_manifest.is_some(),
            )
            .field("attempt_owned", &self.attempt_owned.keys())
            .finish_non_exhaustive()
    }
}

/// Read-only root-owned receipt access before the broker account exists.
pub struct LinuxReceiptReader(LinuxFilesystemManager);

impl LinuxReceiptReader {
    pub(crate) fn new(groups: ManagedGroupBindings, payloads: LinuxReleasePayloads) -> Self {
        Self(Self::at(PathBuf::from("/"), (0, 0), groups, payloads))
    }

    const fn at(
        root: PathBuf,
        root_ids: (u32, u32),
        groups: ManagedGroupBindings,
        payloads: LinuxReleasePayloads,
    ) -> LinuxFilesystemManager {
        LinuxFilesystemManager {
            root,
            principals: PrincipalBindings {
                root_uid: root_ids.0,
                root_gid: root_ids.1,
                broker_uid: None,
                broker_gid: groups.broker_gid(),
                build_users_gid: groups.build_users_gid(),
            },
            payloads,
            authenticated_config: None,
            uninstall_manifest: None,
            attempt_owned: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        root: PathBuf,
        groups: ManagedGroupBindings,
        payloads: LinuxReleasePayloads,
    ) -> Self {
        Self::for_test_with_owner(
            root,
            (Uid::effective().as_raw(), Gid::effective().as_raw()),
            groups,
            payloads,
        )
    }

    #[cfg(test)]
    pub(crate) const fn for_test_with_owner(
        root: PathBuf,
        root_ids: (u32, u32),
        groups: ManagedGroupBindings,
        payloads: LinuxReleasePayloads,
    ) -> Self {
        Self(Self::at(root, root_ids, groups, payloads))
    }

    pub(crate) fn existing_uninstall_manifest(
        &self,
    ) -> Result<Option<UninstallManifest>, LinuxFilesystemError> {
        self.0.existing_uninstall_manifest()
    }
}

impl LinuxFilesystemManager {
    /// Creates the production writer fixed to the real host root.
    ///
    /// # Errors
    ///
    /// Returns a redacted conflict if the broker identity is privileged.
    pub fn new(
        groups: ManagedGroupBindings,
        broker_uid: u32,
        payloads: LinuxReleasePayloads,
    ) -> Result<Self, LinuxFilesystemError> {
        Ok(Self {
            root: PathBuf::from("/"),
            principals: PrincipalBindings::production(groups, broker_uid)?,
            payloads,
            authenticated_config: None,
            uninstall_manifest: None,
            attempt_owned: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    const fn with_root(
        root: PathBuf,
        principals: PrincipalBindings,
        payloads: LinuxReleasePayloads,
    ) -> Self {
        Self {
            root,
            principals,
            payloads,
            authenticated_config: None,
            uninstall_manifest: None,
            attempt_owned: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_existing_preflight_test(
        root: PathBuf,
        payloads: LinuxReleasePayloads,
    ) -> Self {
        let user_id = nix::unistd::Uid::effective().as_raw();
        let group_id = nix::unistd::Gid::effective().as_raw();
        Self::with_root(
            root,
            PrincipalBindings {
                root_uid: user_id,
                root_gid: group_id,
                broker_uid: Some(user_id),
                broker_gid: group_id,
                build_users_gid: group_id,
            },
            payloads,
        )
    }

    /// Returns true for every closed file or directory asset.
    #[must_use]
    pub const fn handles(asset: LinuxInstallAsset) -> bool {
        matches!(
            asset.kind(),
            LinuxAssetKind::Directory | LinuxAssetKind::File
        )
    }

    /// Binds the exact authenticated managed-Nix configuration in memory.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for a non-Linux config, an oversized config, or
    /// a conflicting second binding.
    pub fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), LinuxFilesystemError> {
        self.bind_config_bytes(config.system(), config.as_bytes())
    }

    fn bind_config_bytes(
        &mut self,
        system: pkg_core::System,
        contents: &[u8],
    ) -> Result<(), LinuxFilesystemError> {
        if !matches!(
            system,
            pkg_core::System::X8664Linux
                | pkg_core::System::Aarch64Linux
                | pkg_core::System::X8664Darwin
                | pkg_core::System::Aarch64Darwin
        ) || contents.is_empty()
            || contents.len() > MAX_CONFIG_BYTES
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::MissingPayload,
            ));
        }
        if self
            .authenticated_config
            .as_deref()
            .is_some_and(|bound| bound != contents)
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        self.authenticated_config = Some(Arc::from(contents));
        Ok(())
    }

    /// Verifies or creates one fixed non-systemd filesystem asset.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for missing bytes, unsafe paths, conflicts, or I/O.
    pub fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, LinuxFilesystemError> {
        match asset.kind() {
            LinuxAssetKind::Directory => self.ensure_directory(asset),
            LinuxAssetKind::File => {
                let payload = self.payload_for(asset)?;
                self.ensure_file(asset, &payload)
            }
            LinuxAssetKind::User | LinuxAssetKind::Group => Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsupportedAsset,
            )),
        }
    }

    /// Installs one exact compiled systemd or tmpfiles asset.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the id and bytes are not an exact compiled pair.
    pub fn install_static_asset(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, LinuxFilesystemError> {
        let expected = static_payload(asset)
            .ok_or_else(|| LinuxFilesystemError::new(LinuxFilesystemErrorCode::UnsupportedAsset))?;
        if expected != contents.as_bytes() {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        self.ensure_file(asset, expected)
    }

    /// Binds the validated receipt-last uninstall manifest in memory.
    ///
    /// # Errors
    ///
    /// Returns a redacted conflict for a different second binding or an invalid
    /// canonical encoding.
    pub fn bind_uninstall_manifest(
        &mut self,
        manifest: &UninstallManifest,
    ) -> Result<(), LinuxFilesystemError> {
        let bytes = encode_uninstall_manifest(manifest)
            .map_err(|_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::MissingPayload))?;
        if self
            .uninstall_manifest
            .as_deref()
            .is_some_and(|bound| bound != bytes)
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        self.uninstall_manifest = Some(Arc::from(bytes));
        Ok(())
    }

    /// Reads and validates an existing root-owned uninstall manifest, if present.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for unsafe metadata, oversized bytes, or a
    /// non-canonical manifest.
    pub fn existing_uninstall_manifest(
        &self,
    ) -> Result<Option<UninstallManifest>, LinuxFilesystemError> {
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(unsupported)?;
        self.existing_uninstall_manifest_for(asset)
    }

    pub(crate) fn existing_uninstall_manifest_for(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<Option<UninstallManifest>, LinuxFilesystemError> {
        if asset.id() != "uninstall-manifest" || asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(None);
        };
        let file = match open_child(&parent, &name, LinuxAssetKind::File) {
            Ok(file) => file,
            Err(error) if error == Errno::NOENT => return Ok(None),
            Err(error) => return Err(open_error(error)),
        };
        self.decode_uninstall_manifest_file(asset, file).map(Some)
    }

    /// Returns whether a fixed replacement backup exists.
    pub(crate) fn replacement_backup_exists(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<bool, LinuxFilesystemError> {
        let Some((parent, _)) = self.open_parent_optional(asset)? else {
            return Ok(false);
        };
        match open_child(&parent, &rollback_name(asset), LinuxAssetKind::File) {
            Ok(file) => {
                self.verify_replacement_backup(asset, &file)?;
                Ok(true)
            }
            Err(error) if error == Errno::NOENT => Ok(false),
            Err(error) => Err(open_error(error)),
        }
    }

    /// Reads the prior uninstall receipt retained for rollback, if present.
    pub(crate) fn replacement_uninstall_manifest(
        &self,
    ) -> Result<Option<UninstallManifest>, LinuxFilesystemError> {
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(unsupported)?;
        let (parent, _) = self.open_parent(asset)?;
        let file = match open_child(&parent, &rollback_name(asset), LinuxAssetKind::File) {
            Ok(file) => file,
            Err(error) if error == Errno::NOENT => return Ok(None),
            Err(error) => return Err(open_error(error)),
        };
        self.decode_uninstall_manifest_file(asset, file).map(Some)
    }

    /// Removes only an interrupted initial candidate receipt staging file.
    pub(crate) fn recover_absent_uninstall_manifest_staging(
        &self,
        system: pkg_core::System,
        release: Digest,
    ) -> Result<(), LinuxFilesystemError> {
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(unsupported)?;
        let (parent, _) = self.open_parent(asset)?;
        let name = rollback_name(asset);
        let file = match open_child(&parent, &name, LinuxAssetKind::File) {
            Ok(file) => file,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_replacement_backup(asset, &file)?;
        let staged_identity = identity(&file, LinuxAssetKind::File)?;
        match self.decode_uninstall_manifest_file(asset, file) {
            Ok(manifest)
                if manifest.system() == system
                    && manifest.ownership_manifest_digest() == release => {}
            Ok(_) => {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::RollbackConflict,
                ));
            }
            Err(_) => {}
        }
        Self::remove_if_owned(&parent, &name, staged_identity)
    }

    fn decode_uninstall_manifest_file(
        &self,
        asset: LinuxInstallAsset,
        mut file: File,
    ) -> Result<UninstallManifest, LinuxFilesystemError> {
        self.verify_metadata(asset, &file)?;
        require_single_link(&file)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_UNINSTALL_MANIFEST_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| io_failure())?;
        if u64::try_from(bytes.len()).map_err(|_| io_failure())? > MAX_UNINSTALL_MANIFEST_BYTES {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        crate::decode_uninstall_manifest(&bytes)
            .map_err(|_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::Conflict))
    }

    /// Verifies one fixed filesystem artifact without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the artifact is absent or its type,
    /// ancestry, metadata, or authenticated bytes do not match.
    pub fn verify_asset(&self, asset: LinuxInstallAsset) -> Result<(), LinuxFilesystemError> {
        let (parent, name) = self.open_parent(asset)?;
        let target = open_child(&parent, &name, asset.kind()).map_err(open_error)?;
        match asset.kind() {
            LinuxAssetKind::Directory => self.verify_metadata(asset, &target),
            LinuxAssetKind::File => {
                let payload = self.payload_for(asset)?;
                self.verify_file(asset, target, &payload)
            }
            LinuxAssetKind::User | LinuxAssetKind::Group => Err(unsupported()),
        }
    }

    /// Returns the authenticated candidate digest for one fixed file asset.
    pub(crate) fn expected_file_digest(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<Digest, LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        Ok(body_digest(&self.payload_for(asset)?))
    }

    /// Verifies one fixed file against a prior manifest content identity.
    pub(crate) fn verify_asset_digest(
        &self,
        asset: LinuxInstallAsset,
        expected: Digest,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        let (parent, name) = self.open_parent(asset)?;
        let file = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        self.verify_file_digest(asset, file, expected)
    }

    /// Accepts an exact, absent, or metadata-safe file as an explicit repair target.
    pub(crate) fn verify_repair_target(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if self.verify_asset(asset).is_ok() || self.verify_asset_absent(asset).is_ok() {
            return Ok(());
        }
        let (parent, name) = self.open_parent(asset)?;
        let file = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        self.verify_metadata(asset, &file)?;
        require_single_link(&file)
    }

    /// Replaces one owned file with exact candidate bytes.
    ///
    /// Ordinary upgrade requires the prior manifest digest. Explicit repair
    /// may replace changed bytes after its caller proves product ownership.
    pub(crate) fn replace_owned_file(
        &mut self,
        asset: LinuxInstallAsset,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File || (!repair && prior_digest.is_none()) {
            return Err(unsupported());
        }
        let payload = self.payload_for(asset)?;
        let (parent, name) = self.open_parent(asset)?;
        let current = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        if self
            .verify_file(
                asset,
                current.try_clone().map_err(|_| io_failure())?,
                &payload,
            )
            .is_ok()
        {
            return Ok(false);
        }
        self.verify_metadata(asset, &current)?;
        require_single_link(&current)?;
        let previous_identity = identity(&current, LinuxAssetKind::File)?;
        if let Some(expected) = prior_digest {
            self.verify_file_digest(
                asset,
                current.try_clone().map_err(|_| io_failure())?,
                expected,
            )?;
        }

        let backup_name = rollback_name(asset);
        match open_child(&parent, &backup_name, LinuxAssetKind::File) {
            Err(error) if error == Errno::NOENT => {}
            Ok(_) | Err(_) => {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::RollbackConflict,
                ));
            }
        }

        self.attempt_owned
            .insert(asset.id(), AttemptOwnership::pending());
        let mut staging = File::from(
            openat(
                &parent,
                &backup_name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| io_failure())?,
        );
        let candidate_identity = identity(&staging, LinuxAssetKind::File)?;
        if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
            ownership.staging = Some(StagingIdentity {
                name: backup_name.clone(),
                identity: candidate_identity,
            });
        }
        staging.write_all(&payload).map_err(|_| io_failure())?;
        self.apply_metadata(asset, &staging)?;
        staging.sync_all().map_err(|_| io_failure())?;
        self.verify_file(asset, staging, &payload)?;
        fsync(&parent).map_err(|_| io_failure())?;
        renameat_with(&parent, &backup_name, &parent, &name, RenameFlags::EXCHANGE)
            .map_err(|_| io_failure())?;
        if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
            ownership.target = Some(candidate_identity);
            ownership.staging = None;
            ownership.previous = Some(StagingIdentity {
                name: backup_name.clone(),
                identity: previous_identity,
            });
        }
        fsync(&parent).map_err(|_| io_failure())?;
        let installed = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        self.verify_file(asset, installed, &payload)?;
        let previous =
            open_child(&parent, &backup_name, LinuxAssetKind::File).map_err(open_error)?;
        if identity(&previous, LinuxAssetKind::File)? != previous_identity {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::RollbackConflict,
            ));
        }
        Ok(true)
    }

    pub(crate) fn replace_static_owned_file(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
        prior_digest: Option<Digest>,
        repair: bool,
    ) -> Result<bool, LinuxFilesystemError> {
        if static_payload(asset) != Some(contents.as_bytes()) {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        self.replace_owned_file(asset, prior_digest, repair)
    }

    /// Restores an authenticated prior file after an interrupted upgrade.
    pub(crate) fn recover_owned_file(
        &self,
        asset: LinuxInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        let payload = self.payload_for(asset)?;
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let backup_name = rollback_name(asset);
        let backup = match open_child(&parent, &backup_name, LinuxAssetKind::File) {
            Ok(file) => Some(file),
            Err(error) if error == Errno::NOENT => None,
            Err(error) => return Err(open_error(error)),
        };
        let current = match open_child(&parent, &name, LinuxAssetKind::File) {
            Ok(file) => Some(file),
            Err(error) if error == Errno::NOENT => None,
            Err(error) => return Err(open_error(error)),
        };

        let Some(backup) = backup else {
            let current = current.ok_or_else(io_failure)?;
            return self.verify_file_digest(asset, current, prior_digest);
        };
        let backup_identity = identity(&backup, LinuxAssetKind::File)?;
        let current = current.ok_or_else(io_failure)?;
        if self
            .verify_file(
                asset,
                current.try_clone().map_err(|_| io_failure())?,
                &payload,
            )
            .is_ok()
        {
            self.verify_metadata(asset, &backup)?;
            require_single_link(&backup)?;
            self.verify_file_digest(
                asset,
                backup.try_clone().map_err(|_| io_failure())?,
                prior_digest,
            )?;
            renameat_with(&parent, &backup_name, &parent, &name, RenameFlags::EXCHANGE).map_err(
                |_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::RollbackConflict),
            )?;
            let candidate =
                open_child(&parent, &backup_name, LinuxAssetKind::File).map_err(open_error)?;
            let candidate_identity = identity(&candidate, LinuxAssetKind::File)?;
            self.verify_file(asset, candidate, &payload)?;
            Self::remove_if_owned(&parent, &backup_name, candidate_identity)
        } else {
            self.verify_file_digest(asset, current, prior_digest)?;
            self.verify_replacement_backup(asset, &backup)?;
            Self::remove_if_owned(&parent, &backup_name, backup_identity)
        }
    }

    /// Removes an authenticated replacement backup after receipt durability.
    pub(crate) fn finalize_owned_file(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        let payload = self.payload_for(asset)?;
        let (parent, name) = self.open_parent(asset)?;
        let current = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        self.verify_file(asset, current, &payload)?;
        let backup_name = rollback_name(asset);
        let backup = match open_child(&parent, &backup_name, LinuxAssetKind::File) {
            Ok(file) => file,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_metadata(asset, &backup)?;
        require_single_link(&backup)?;
        let backup_identity = identity(&backup, LinuxAssetKind::File)?;
        drop(backup);
        Self::remove_if_owned(&parent, &backup_name, backup_identity)
    }

    /// Removes deterministic residue and keeps authenticated repair bytes live.
    pub(crate) fn roll_forward_owned_file(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        self.remove_fixed_staging(asset)?;
        if self.verify_asset(asset).is_ok() {
            self.remove_fixed_staging(asset)?;
            self.attempt_owned.remove(asset.id());
            return Ok(());
        }
        if self.verify_asset_absent(asset).is_ok() {
            let payload = self.payload_for(asset)?;
            self.ensure_file(asset, &payload)?;
            self.attempt_owned.remove(asset.id());
            return Ok(());
        }

        self.remove_fixed_staging(asset)?;
        self.replace_owned_file(asset, None, true)?;
        self.remove_fixed_staging(asset)?;
        self.attempt_owned.remove(asset.id());
        self.verify_asset(asset)
    }

    /// Removes one interrupted initial file write and its deterministic residue.
    pub(crate) fn recover_created_file(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File {
            return Err(unsupported());
        }
        self.remove_fixed_staging(asset)?;
        self.remove_verified_asset(asset)
    }

    pub(crate) fn remove_fixed_staging(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        let Some((parent, _)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let name = rollback_name(asset);
        let file = match open_child(&parent, &name, LinuxAssetKind::File) {
            Ok(file) => file,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_replacement_backup(asset, &file)?;
        let identity = identity(&file, LinuxAssetKind::File)?;
        drop(file);
        Self::remove_if_owned(&parent, &name, identity)
    }

    pub(crate) fn verify_empty_directory(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::Directory {
            return Err(unsupported());
        }
        let (parent, name) = self.open_parent(asset)?;
        let target = open_child(&parent, &name, LinuxAssetKind::Directory).map_err(open_error)?;
        self.verify_metadata(asset, &target)?;
        let mut entries = Dir::read_from(&target).map_err(|_| io_failure())?;
        for entry in &mut entries {
            let entry = entry.map_err(|_| io_failure())?;
            if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
        }
        Ok(())
    }

    /// Removes one artifact only when this exact attempt recorded its inode.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if identity changed or removal is incomplete.
    pub fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), LinuxFilesystemError> {
        let Some(ownership) = self.attempt_owned.get(asset.id()).cloned() else {
            return Ok(());
        };
        let (parent, name) = self.open_parent(asset)?;
        if ownership.target_uncertain {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::RollbackConflict,
            ));
        }
        if let (Some(target), Some(previous)) = (ownership.target, ownership.previous) {
            let current = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
            let backup =
                open_child(&parent, &previous.name, LinuxAssetKind::File).map_err(open_error)?;
            if identity(&current, LinuxAssetKind::File)? != target
                || identity(&backup, LinuxAssetKind::File)? != previous.identity
            {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::RollbackConflict,
                ));
            }
            renameat_with(
                &parent,
                &previous.name,
                &parent,
                &name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::RollbackConflict))?;
            Self::remove_if_owned(&parent, &previous.name, target)?;
            self.attempt_owned.remove(asset.id());
            return Ok(());
        }
        if let Some(target) = ownership.target {
            Self::remove_if_owned(&parent, &name, target)?;
        }
        if let Some(staging) = ownership.staging {
            Self::remove_if_owned(&parent, &staging.name, staging.identity)?;
        }
        self.attempt_owned.remove(asset.id());
        Ok(())
    }

    /// Removes one manifest-owned filesystem artifact after exact verification.
    ///
    /// This path is for a later uninstall process, so it does not depend on
    /// in-memory installation-attempt state. An already absent artifact is an
    /// idempotent success. Changed metadata, bytes, type, ancestry, or identity
    /// always refuses removal.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when verification fails or the verified inode
    /// cannot be removed without crossing the closed filesystem boundary.
    pub fn remove_verified_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let target = match open_child(&parent, &name, asset.kind()) {
            Ok(target) => target,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        let target_identity = identity(&target, asset.kind())?;
        match asset.kind() {
            LinuxAssetKind::Directory => self.verify_metadata(asset, &target)?,
            LinuxAssetKind::File => {
                let payload = self.payload_for(asset)?;
                self.verify_file(asset, target, &payload)?;
            }
            LinuxAssetKind::User | LinuxAssetKind::Group => return Err(unsupported()),
        }
        Self::remove_if_owned(&parent, &name, target_identity)
    }

    /// Removes the private, flat broker channel datastore and its verified files.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the directory or a child is unsafe or changed.
    pub fn remove_broker_channel_state(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if asset.id() != "broker-channel-state" {
            return Err(unsupported());
        }
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let target = match open_child(&parent, &name, LinuxAssetKind::Directory) {
            Ok(target) => target,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_metadata(asset, &target)?;
        let target_identity = identity(&target, LinuxAssetKind::Directory)?;
        let owner = self
            .principals
            .owner(asset.owner().ok_or_else(unsupported)?)?;
        let group = self
            .principals
            .group(asset.group().ok_or_else(unsupported)?);
        let mut names = Vec::new();
        let mut entries = Dir::read_from(&target).map_err(|_| io_failure())?;
        for entry in &mut entries {
            let entry = entry.map_err(|_| io_failure())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if bytes.is_empty() || bytes.contains(&b'/') || names.len() >= MAX_PRIVATE_STATE_FILES {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
        for child_name in names {
            let child = open_child(&target, &child_name, LinuxAssetKind::File).map_err(|_| {
                LinuxFilesystemError::new(LinuxFilesystemErrorCode::UnsafeFilesystemState)
            })?;
            let metadata = child.metadata().map_err(|_| io_failure())?;
            let common_identity = metadata.is_file()
                && metadata.gid() == group
                && metadata.nlink() == 1
                && metadata.dev() == target_identity.device;
            let private_broker_file =
                metadata.uid() == owner && metadata.mode().trailing_zeros() >= 6;
            let authenticated_root_metadata = metadata.uid() == self.principals.root_uid
                && validate_linux_auth_datastore_file(
                    &child_name,
                    &metadata,
                    self.principals.root_uid,
                    group,
                )
                .unwrap_or(false);
            if !common_identity || !(private_broker_file || authenticated_root_metadata) {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
            let child_identity = identity(&child, LinuxAssetKind::File)?;
            drop(child);
            Self::remove_if_owned(&target, &child_name, child_identity)?;
        }
        fsync(&target).map_err(|_| io_failure())?;
        Self::remove_if_owned(&parent, &name, target_identity)
    }

    /// Removes the verified private broker home without following links.
    ///
    /// # Errors
    ///
    /// Returns a closed filesystem error when the asset is not a supported
    /// private tree or its ownership, identity, mount, or tree state is unsafe.
    pub fn remove_private_tree(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if !matches!(asset.id(), "broker-home" | "broker-log-dir" | "helper-home") {
            return Err(unsupported());
        }
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let target = match open_child(&parent, &name, LinuxAssetKind::Directory) {
            Ok(target) => target,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_metadata(asset, &target)?;
        drop(target);
        let tree_owner_uid = if asset.id() == "helper-home" {
            self.principals.root_uid
        } else {
            self.principals.owner(LinuxAssetPrincipal::Broker)?
        };
        remove_owned_tree(
            &self.root,
            Path::new(asset.path_or_name()),
            self.principals.root_uid,
            tree_owner_uid,
        )
        .map_err(|_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::UnsafeFilesystemState))
    }

    #[cfg_attr(target_os = "linux", allow(clippy::useless_conversion))]
    pub(crate) fn remove_runtime_state(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        if !matches!(
            asset.id(),
            "broker-socket-dir" | "helper-socket-dir" | "helper-log-dir" | "log-root"
        ) {
            return Err(unsupported());
        }
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        let target = match open_child(&parent, &name, LinuxAssetKind::Directory) {
            Ok(target) => target,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(error) => return Err(open_error(error)),
        };
        self.verify_metadata(asset, &target)?;
        let target_identity = identity(&target, LinuxAssetKind::Directory)?;
        let mut names = Vec::new();
        let mut entries = Dir::read_from(&target).map_err(|_| io_failure())?;
        for entry in &mut entries {
            let entry = entry.map_err(|_| io_failure())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if bytes.is_empty() || bytes.contains(&b'/') || names.len() >= MAX_PRIVATE_STATE_FILES {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
        for child_name in names {
            let child = statat(&target, &child_name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| io_failure())?;
            let expected = match (asset.id(), child_name.as_bytes()) {
                ("broker-socket-dir", b"broker.sock") => (
                    FileType::Socket,
                    self.principals.owner(LinuxAssetPrincipal::Broker)?,
                    self.principals.broker_gid,
                    0o666,
                ),
                ("helper-socket-dir", b"root-helper.sock") => (
                    FileType::Socket,
                    self.principals.root_uid,
                    self.principals.broker_gid,
                    0o660,
                ),
                ("helper-log-dir", b"root-helper.log") => (
                    FileType::RegularFile,
                    self.principals.root_uid,
                    self.principals.root_gid,
                    0o600,
                ),
                ("log-root", b"nix-daemon.log" | b"store-volume.log") => (
                    FileType::RegularFile,
                    self.principals.root_uid,
                    self.principals.broker_gid,
                    0o600,
                ),
                _ => {
                    return Err(LinuxFilesystemError::new(
                        LinuxFilesystemErrorCode::UnsafeFilesystemState,
                    ));
                }
            };
            if FileType::from_raw_mode(child.st_mode) != expected.0
                || child.st_uid != expected.1
                || child.st_gid != expected.2
                || child.st_mode & 0o777 != expected.3
                || child.st_nlink != 1
                || u64::try_from(child.st_dev) != Ok(target_identity.device)
            {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
            let current = statat(&target, &child_name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| io_failure())?;
            if child.st_dev != current.st_dev
                || child.st_ino != current.st_ino
                || child.st_mode != current.st_mode
                || child.st_uid != current.st_uid
                || child.st_gid != current.st_gid
                || child.st_nlink != current.st_nlink
            {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::RollbackConflict,
                ));
            }
            unlinkat(&target, &child_name, AtFlags::empty()).map_err(|_| {
                LinuxFilesystemError::new(LinuxFilesystemErrorCode::RollbackConflict)
            })?;
        }
        fsync(&target).map_err(|_| io_failure())?;
        Self::remove_if_owned(&parent, &name, target_identity)
    }

    /// Verifies that one fixed filesystem asset is absent without following links.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the asset still exists or its parent path is unsafe.
    pub fn verify_asset_absent(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxFilesystemError> {
        let Some((parent, name)) = self.open_parent_optional(asset)? else {
            return Ok(());
        };
        match open_child(&parent, &name, asset.kind()) {
            Err(error) if error == Errno::NOENT => Ok(()),
            Ok(_) | Err(_) => Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            )),
        }
    }

    fn payload_for(&self, asset: LinuxInstallAsset) -> Result<Arc<[u8]>, LinuxFilesystemError> {
        if let Some(payload) = self.payloads.for_asset(asset) {
            return Ok(payload);
        }
        if let Some(payload) = static_payload(asset) {
            return Ok(Arc::from(payload));
        }
        match asset.id() {
            "nix-config" => self
                .authenticated_config
                .clone()
                .ok_or_else(|| LinuxFilesystemError::new(LinuxFilesystemErrorCode::MissingPayload)),
            "profile-snippet" => Ok(Arc::from(PROFILE_SNIPPET)),
            "uninstall-manifest" => self
                .uninstall_manifest
                .clone()
                .ok_or_else(|| LinuxFilesystemError::new(LinuxFilesystemErrorCode::MissingPayload)),
            _ => Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsupportedAsset,
            )),
        }
    }

    fn ensure_directory(&mut self, asset: LinuxInstallAsset) -> Result<bool, LinuxFilesystemError> {
        let (parent, name) = self.open_parent(asset)?;
        match open_child(&parent, &name, LinuxAssetKind::Directory) {
            Ok(directory) => {
                self.verify_metadata(asset, &directory)?;
                Ok(false)
            }
            Err(error) if error == Errno::NOENT => {
                self.attempt_owned
                    .insert(asset.id(), AttemptOwnership::pending());
                match mkdirat(&parent, &name, rustix_mode(asset)?) {
                    Ok(()) => {
                        if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
                            ownership.target_uncertain = true;
                        }
                    }
                    Err(error) if error == Errno::EXIST => {
                        let directory = open_child(&parent, &name, LinuxAssetKind::Directory)
                            .map_err(open_error)?;
                        self.verify_metadata(asset, &directory)?;
                        self.attempt_owned.remove(asset.id());
                        return Ok(false);
                    }
                    Err(_) => return Err(io_failure()),
                }
                let directory =
                    open_child(&parent, &name, LinuxAssetKind::Directory).map_err(open_error)?;
                let identity = identity(&directory, LinuxAssetKind::Directory)?;
                if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
                    ownership.target = Some(identity);
                    ownership.target_uncertain = false;
                }
                self.apply_metadata(asset, &directory)?;
                fsync(&directory).map_err(|_| io_failure())?;
                fsync(&parent).map_err(|_| io_failure())?;
                self.verify_metadata(asset, &directory)?;
                Ok(true)
            }
            Err(error) => Err(open_error(error)),
        }
    }

    fn ensure_file(
        &mut self,
        asset: LinuxInstallAsset,
        payload: &[u8],
    ) -> Result<bool, LinuxFilesystemError> {
        if asset.kind() != LinuxAssetKind::File || payload.is_empty() {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsupportedAsset,
            ));
        }
        let (parent, name) = self.open_parent(asset)?;
        match open_child(&parent, &name, LinuxAssetKind::File) {
            Ok(file) => {
                self.verify_file(asset, file, payload)?;
                return Ok(false);
            }
            Err(error) if error == Errno::NOENT => {}
            Err(error) => return Err(open_error(error)),
        }

        self.attempt_owned
            .insert(asset.id(), AttemptOwnership::pending());
        let (staging_name, mut staging) = create_staging(&parent, asset)?;
        let staging_identity = identity(&staging, LinuxAssetKind::File)?;
        if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
            ownership.staging = Some(StagingIdentity {
                name: staging_name.clone(),
                identity: staging_identity,
            });
        }
        staging.write_all(payload).map_err(|_| io_failure())?;
        self.apply_metadata(asset, &staging)?;
        staging.sync_all().map_err(|_| io_failure())?;
        self.verify_file(asset, staging, payload)?;

        match renameat_with(
            &parent,
            &staging_name,
            &parent,
            &name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                if let Some(ownership) = self.attempt_owned.get_mut(asset.id()) {
                    ownership.target = Some(staging_identity);
                    ownership.staging = None;
                }
            }
            Err(error) if error == Errno::EXIST => {
                let existing =
                    open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
                self.verify_file(asset, existing, payload)?;
                Self::remove_if_owned(&parent, &staging_name, staging_identity)?;
                self.attempt_owned.remove(asset.id());
                return Ok(false);
            }
            Err(_) => return Err(io_failure()),
        }
        fsync(&parent).map_err(|_| io_failure())?;
        let installed = open_child(&parent, &name, LinuxAssetKind::File).map_err(open_error)?;
        self.verify_file(asset, installed, payload)?;
        Ok(true)
    }

    fn apply_metadata(
        &self,
        asset: LinuxInstallAsset,
        file: &File,
    ) -> Result<(), LinuxFilesystemError> {
        let owner = asset.owner().ok_or_else(unsupported)?;
        let group = asset.group().ok_or_else(unsupported)?;
        nix::unistd::fchown(
            file,
            Some(Uid::from_raw(self.principals.owner(owner)?)),
            Some(Gid::from_raw(self.principals.group(group))),
        )
        .map_err(|_| io_failure())?;
        fchmod(file, rustix_mode(asset)?).map_err(|_| io_failure())?;
        Ok(())
    }

    fn verify_metadata(
        &self,
        asset: LinuxInstallAsset,
        file: &File,
    ) -> Result<(), LinuxFilesystemError> {
        let metadata = file.metadata().map_err(|_| io_failure())?;
        let expected_kind = asset.kind();
        if (expected_kind == LinuxAssetKind::Directory && !metadata.is_dir())
            || (expected_kind == LinuxAssetKind::File && !metadata.is_file())
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsafeFilesystemState,
            ));
        }
        let owner = asset.owner().ok_or_else(unsupported)?;
        let group = asset.group().ok_or_else(unsupported)?;
        if metadata.uid() != self.principals.owner(owner)?
            || metadata.gid() != self.principals.group(group)
            || metadata.mode() & 0o7777 != asset_mode(asset)?
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        Ok(())
    }

    fn verify_replacement_backup(
        &self,
        asset: LinuxInstallAsset,
        file: &File,
    ) -> Result<(), LinuxFilesystemError> {
        require_single_link(file)?;
        let metadata = file.metadata().map_err(|_| io_failure())?;
        if !metadata.is_file() {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsafeFilesystemState,
            ));
        }
        let staged = metadata.uid() == self.principals.root_uid
            && metadata.gid() == self.principals.root_gid
            && metadata.mode() & 0o7777 == 0o600;
        if staged || self.verify_metadata(asset, file).is_ok() {
            Ok(())
        } else {
            Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ))
        }
    }

    fn verify_file(
        &self,
        asset: LinuxInstallAsset,
        file: File,
        payload: &[u8],
    ) -> Result<(), LinuxFilesystemError> {
        self.verify_metadata(asset, &file)?;
        require_single_link(&file)?;
        Self::verify_file_contents(file, payload)
    }

    fn verify_file_contents(mut file: File, payload: &[u8]) -> Result<(), LinuxFilesystemError> {
        let metadata = file.metadata().map_err(|_| io_failure())?;
        if metadata.len() != u64::try_from(payload.len()).map_err(|_| io_failure())? {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        file.rewind().map_err(|_| io_failure())?;
        let mut observed = Vec::with_capacity(payload.len());
        file.read_to_end(&mut observed).map_err(|_| io_failure())?;
        if observed != payload {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        Ok(())
    }

    fn verify_file_digest(
        &self,
        asset: LinuxInstallAsset,
        mut file: File,
        expected: Digest,
    ) -> Result<(), LinuxFilesystemError> {
        self.verify_metadata(asset, &file)?;
        require_single_link(&file)?;
        file.rewind().map_err(|_| io_failure())?;
        let mut hasher = Sha256::new();
        let mut observed = 0_usize;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|_| io_failure())?;
            if read == 0 {
                break;
            }
            observed = observed.checked_add(read).ok_or_else(io_failure)?;
            if observed > MAX_RELEASE_BINARY_BYTES {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::Conflict,
                ));
            }
            hasher.update(&buffer[..read]);
        }
        if Digest::from_bytes(hasher.finalize().into()) != expected {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::Conflict,
            ));
        }
        Ok(())
    }

    fn open_parent(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<(File, OsString), LinuxFilesystemError> {
        self.open_parent_optional(asset)?.ok_or_else(io_failure)
    }

    fn open_parent_optional(
        &self,
        asset: LinuxInstallAsset,
    ) -> Result<Option<(File, OsString)>, LinuxFilesystemError> {
        if !Self::handles(asset) {
            return Err(unsupported());
        }
        let components = absolute_components(Path::new(asset.path_or_name()))?;
        let (name, parents) = components.split_last().ok_or_else(unsupported)?;
        let root = match open(
            &self.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(root) => File::from(root),
            Err(error) if error == Errno::NOENT => return Ok(None),
            Err(error) => return Err(open_error(error)),
        };
        let mut parent = root;
        self.verify_ancestor(Path::new("/"), &parent)?;
        let mut path = PathBuf::from("/");
        for component in parents {
            path.push(component);
            parent = match openat(
                &parent,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(child) => File::from(child),
                Err(error) if error == Errno::NOENT => return Ok(None),
                Err(error) => return Err(open_error(error)),
            };
            self.verify_ancestor(&path, &parent)?;
        }
        Ok(Some((parent, name.clone())))
    }

    fn verify_ancestor(&self, path: &Path, directory: &File) -> Result<(), LinuxFilesystemError> {
        if let Some(asset) = crate::linux_install_assets().iter().copied().find(|asset| {
            is_linux_product_asset(*asset)
                && asset.kind() == LinuxAssetKind::Directory
                && Path::new(asset.path_or_name()) == path
        }) {
            return self.verify_metadata(asset, directory);
        }
        let metadata = directory.metadata().map_err(|_| io_failure())?;
        if !metadata.is_dir()
            || metadata.uid() != self.principals.root_uid
            || metadata.gid() != self.principals.root_gid
            || metadata.mode() & 0o022 != 0
        {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::UnsafeFilesystemState,
            ));
        }
        Ok(())
    }

    fn remove_if_owned(
        parent: &File,
        name: &OsStr,
        expected: FileIdentity,
    ) -> Result<(), LinuxFilesystemError> {
        let current = match open_child(parent, name, expected.kind) {
            Ok(current) => current,
            Err(error) if error == Errno::NOENT => return Ok(()),
            Err(_) => {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::RollbackConflict,
                ));
            }
        };
        if identity(&current, expected.kind)? != expected {
            return Err(LinuxFilesystemError::new(
                LinuxFilesystemErrorCode::RollbackConflict,
            ));
        }
        drop(current);
        let flags = if expected.kind == LinuxAssetKind::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        unlinkat(parent, name, flags)
            .map_err(|_| LinuxFilesystemError::new(LinuxFilesystemErrorCode::RollbackConflict))?;
        fsync(parent).map_err(|_| io_failure())?;
        Ok(())
    }
}

fn static_payload(asset: LinuxInstallAsset) -> Option<&'static [u8]> {
    match asset.id() {
        "helper-socket-unit" => Some(LinuxSystemdAssets::HELPER_SOCKET.as_bytes()),
        "helper-service-unit" => Some(LinuxSystemdAssets::HELPER_SERVICE.as_bytes()),
        "broker-socket-unit" => Some(LinuxSystemdAssets::BROKER_SOCKET.as_bytes()),
        "broker-service-unit" => Some(LinuxSystemdAssets::BROKER_SERVICE.as_bytes()),
        "runtime-tmpfiles" => Some(LinuxSystemdAssets::TMPFILES.as_bytes()),
        "store-volume-plist" => Some(MacOsLaunchdAssets::STORE_VOLUME.as_bytes()),
        "daemon-plist" => Some(MacOsLaunchdAssets::NIX_DAEMON.as_bytes()),
        "helper-plist" => Some(MacOsLaunchdAssets::ROOT_HELPER.as_bytes()),
        "broker-plist" => Some(MacOsLaunchdAssets::BROKER.as_bytes()),
        "path-file" => Some(b"/opt/pkg/bin\n"),
        _ => None,
    }
}

fn rollback_name(asset: LinuxInstallAsset) -> OsString {
    OsString::from(format!(".pkg-install-rollback-{}", asset.id()))
}

fn require_single_link(file: &File) -> Result<(), LinuxFilesystemError> {
    if file.metadata().map_err(|_| io_failure())?.nlink() != 1 {
        return Err(LinuxFilesystemError::new(
            LinuxFilesystemErrorCode::Conflict,
        ));
    }
    Ok(())
}

fn absolute_components(path: &Path) -> Result<Vec<OsString>, LinuxFilesystemError> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(unsupported());
    }
    let mut result = Vec::new();
    for component in components {
        match component {
            Component::Normal(value) if !value.is_empty() => result.push(value.to_os_string()),
            Component::RootDir
            | Component::Prefix(_)
            | Component::CurDir
            | Component::ParentDir
            | Component::Normal(_) => {
                return Err(LinuxFilesystemError::new(
                    LinuxFilesystemErrorCode::UnsafeFilesystemState,
                ));
            }
        }
    }
    Ok(result)
}

fn open_child(parent: &File, name: &OsStr, kind: LinuxAssetKind) -> Result<File, Errno> {
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if kind == LinuxAssetKind::Directory {
        flags |= OFlags::DIRECTORY;
    }
    openat(parent, name, flags, Mode::empty()).map(File::from)
}

fn create_staging(
    parent: &File,
    asset: LinuxInstallAsset,
) -> Result<(OsString, File), LinuxFilesystemError> {
    let name = rollback_name(asset);
    let file = openat(
        parent,
        &name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| io_failure())?;
    Ok((name, File::from(file)))
}

fn identity(file: &File, kind: LinuxAssetKind) -> Result<FileIdentity, LinuxFilesystemError> {
    let metadata = file.metadata().map_err(|_| io_failure())?;
    if (kind == LinuxAssetKind::Directory && !metadata.is_dir())
        || (kind == LinuxAssetKind::File && !metadata.is_file())
    {
        return Err(LinuxFilesystemError::new(
            LinuxFilesystemErrorCode::UnsafeFilesystemState,
        ));
    }
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        kind,
    })
}

fn asset_mode(asset: LinuxInstallAsset) -> Result<u32, LinuxFilesystemError> {
    asset.mode().ok_or_else(unsupported)
}

fn rustix_mode(asset: LinuxInstallAsset) -> Result<Mode, LinuxFilesystemError> {
    let mode = u16::try_from(asset_mode(asset)?).map_err(|_| unsupported())?;
    #[cfg(target_os = "linux")]
    let mode = u32::from(mode);
    Ok(Mode::from_raw_mode(mode))
}

const fn unsupported() -> LinuxFilesystemError {
    LinuxFilesystemError::new(LinuxFilesystemErrorCode::UnsupportedAsset)
}

const fn io_failure() -> LinuxFilesystemError {
    LinuxFilesystemError::new(LinuxFilesystemErrorCode::IoFailure)
}

const fn open_error(error: Errno) -> LinuxFilesystemError {
    if matches!(error, Errno::LOOP | Errno::NOTDIR) {
        LinuxFilesystemError::new(LinuxFilesystemErrorCode::UnsafeFilesystemState)
    } else {
        io_failure()
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;

    use pkg_core::{System, state::Digest};
    use tempfile::TempDir;

    use super::*;
    use crate::linux_install_assets;

    struct Fixture {
        temporary: TempDir,
        manager: LinuxFilesystemManager,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            let temporary = tempfile::tempdir()?;
            for path in [
                "opt",
                "var",
                "var/lib",
                "run",
                "usr",
                "usr/lib",
                "usr/lib/systemd",
                "usr/lib/systemd/system",
                "usr/lib/tmpfiles.d",
                "usr/local",
                "usr/local/bin",
                "etc",
                "etc/profile.d",
            ] {
                fs::create_dir(temporary.path().join(path))?;
            }
            let uid = nix::unistd::Uid::current().as_raw();
            let gid = nix::unistd::Gid::current().as_raw();
            let principals = PrincipalBindings {
                root_uid: uid,
                root_gid: gid,
                broker_uid: Some(uid),
                broker_gid: gid,
                build_users_gid: gid,
            };
            let payloads = LinuxReleasePayloads::from_authenticated_bytes(
                b"root-helper",
                b"broker",
                b"pkg-cli",
            )?;
            Ok(Self {
                manager: LinuxFilesystemManager::with_root(
                    temporary.path().to_path_buf(),
                    principals,
                    payloads,
                ),
                temporary,
            })
        }

        fn asset(id: &str) -> LinuxInstallAsset {
            linux_install_assets()
                .iter()
                .copied()
                .find(|asset| asset.id() == id)
                .unwrap_or_else(|| unreachable!("test asset is in the closed list"))
        }
    }

    fn failure_code<T>(
        result: &Result<T, LinuxFilesystemError>,
    ) -> Result<LinuxFilesystemErrorCode, Box<dyn Error>> {
        match result {
            Ok(_) => Err(std::io::Error::other("expected filesystem failure").into()),
            Err(error) => Ok(error.code()),
        }
    }

    #[test]
    fn creates_verifies_and_rolls_back_nested_directories() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in [
            "nix-root",
            "nix-store",
            "nix-var",
            "nix-state",
            "nix-gcroots",
            "daemon-socket-dir",
        ] {
            assert!(fixture.manager.ensure_asset(Fixture::asset(id))?);
        }
        assert_eq!(
            fs::metadata(fixture.temporary.path().join("nix/store"))?
                .permissions()
                .mode()
                & 0o7777,
            0o1775
        );
        assert!(!fixture.manager.ensure_asset(Fixture::asset("nix-store"))?);
        for id in [
            "daemon-socket-dir",
            "nix-gcroots",
            "nix-state",
            "nix-var",
            "nix-store",
            "nix-root",
        ] {
            fixture.manager.rollback_asset(Fixture::asset(id))?;
        }
        assert!(!fixture.temporary.path().join("nix").exists());
        Ok(())
    }

    #[test]
    fn privileged_writes_refuse_unsafe_external_and_managed_ancestors() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        fs::set_permissions(
            fixture.temporary.path().join("usr/lib/systemd"),
            fs::Permissions::from_mode(0o777),
        )?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .ensure_asset(Fixture::asset("broker-service-unit"))
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );

        fs::set_permissions(
            fixture.temporary.path().join("usr/lib/systemd"),
            fs::Permissions::from_mode(0o755),
        )?;
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("product-root"))?
        );
        fs::set_permissions(
            fixture.temporary.path().join("opt/pkg"),
            fs::Permissions::from_mode(0o777),
        )?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .ensure_asset(Fixture::asset("service-bin-dir"))
            )?,
            LinuxFilesystemErrorCode::Conflict
        );

        let mut wrong_owner = Fixture::new()?;
        wrong_owner.manager.principals.root_uid = wrong_owner
            .manager
            .principals
            .root_uid
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("uid overflow"))?;
        assert_eq!(
            failure_code(
                &wrong_owner
                    .manager
                    .ensure_asset(Fixture::asset("product-root"))
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        Ok(())
    }

    #[test]
    fn installs_exact_release_static_and_authenticated_bytes() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in [
            "product-root",
            "product-config-root",
            "product-config-dir",
            "uninstall-root",
            "service-bin-dir",
        ] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        fixture
            .manager
            .bind_config_bytes(System::X8664Linux, b"sandbox = true\n")?;
        assert!(fixture.manager.ensure_asset(Fixture::asset("nix-config"))?);
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("root-helper-binary"))?
        );
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("broker-binary"))?
        );
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("product-cli"))?
        );
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("profile-snippet"))?
        );
        let profile = fs::read_to_string(fixture.temporary.path().join("etc/profile.d/pkg.sh"))?;
        assert!(profile.contains("__pkg_state=\"$HOME/.local/share/pkg\""));
        assert!(!profile.contains("XDG_DATA_HOME"));
        assert!(fixture.manager.install_static_asset(
            Fixture::asset("broker-service-unit"),
            LinuxSystemdAssets::BROKER_SERVICE,
        )?);
        fixture
            .manager
            .verify_asset(Fixture::asset("broker-service-unit"))?;
        let records = crate::assets::linux_product_install_assets()
            .map(|asset| {
                let record =
                    crate::RecordedAsset::new(asset.id(), crate::RecordedAssetState::Created)?;
                Ok::<_, crate::UninstallError>(
                    if asset.kind() == LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                        record.with_content_digest(body_digest(asset.id().as_bytes()))
                    } else {
                        record
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let manifest =
            UninstallManifest::new(System::X8664Linux, Digest::from_bytes([9; 32]), records)?;
        fixture.manager.bind_uninstall_manifest(&manifest)?;
        assert!(
            fixture
                .manager
                .ensure_asset(Fixture::asset("uninstall-manifest"))?
        );
        assert_eq!(
            crate::decode_uninstall_manifest(&fs::read(
                fixture
                    .temporary
                    .path()
                    .join("opt/pkg/uninstall/manifest.json"),
            )?)?,
            manifest
        );
        assert_eq!(
            fs::read(fixture.temporary.path().join("opt/pkg/etc/pkg/nix.conf"))?,
            b"sandbox = true\n"
        );
        assert_eq!(
            fs::read(fixture.temporary.path().join("opt/pkg/bin/pkg-root-helper"))?,
            b"root-helper"
        );
        assert!(!fixture.manager.ensure_asset(Fixture::asset("nix-config"))?);
        Ok(())
    }

    #[test]
    fn conflicts_and_symlinked_ancestors_fail_closed() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        fs::write(fixture.temporary.path().join("opt/pkg"), b"foreign")?;
        assert_eq!(
            failure_code(&fixture.manager.ensure_asset(Fixture::asset("product-root")),)?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        fs::remove_file(fixture.temporary.path().join("opt/pkg"))?;
        symlink("/tmp", fixture.temporary.path().join("opt/pkg"))?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .ensure_asset(Fixture::asset("product-config-root")),
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        Ok(())
    }

    #[test]
    fn rollback_refuses_replaced_attempt_owned_file() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "service-bin-dir"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let asset = Fixture::asset("product-cli");
        assert!(fixture.manager.ensure_asset(asset)?);
        let path = fixture.temporary.path().join("usr/local/bin/pkg");
        let replacement = fixture.temporary.path().join("usr/local/bin/pkg.foreign");
        fs::write(&replacement, b"foreign")?;
        fs::remove_file(&path)?;
        fs::rename(replacement, &path)?;
        assert_eq!(
            failure_code(&fixture.manager.rollback_asset(asset))?,
            LinuxFilesystemErrorCode::RollbackConflict
        );
        assert_eq!(fs::read(path)?, b"foreign");
        Ok(())
    }

    #[test]
    fn upgrade_replaces_only_exact_prior_owned_bytes_and_rolls_back() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let path = fixture
            .temporary
            .path()
            .join("usr/lib/systemd/system/pkg-nix-broker.service");
        let prior = b"prior authenticated unit\n";
        fs::write(&path, prior)?;
        let prior_digest = body_digest(prior);

        assert!(fixture.manager.replace_static_owned_file(
            asset,
            LinuxSystemdAssets::BROKER_SERVICE,
            Some(prior_digest),
            false,
        )?);
        assert_eq!(
            fs::read(&path)?,
            LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
        );

        fixture.manager.rollback_asset(asset)?;
        assert_eq!(fs::read(&path)?, prior);

        fs::write(&path, b"locally changed unit\n")?;
        assert_eq!(
            failure_code(&fixture.manager.replace_static_owned_file(
                asset,
                LinuxSystemdAssets::BROKER_SERVICE,
                Some(prior_digest),
                false,
            ))?,
            LinuxFilesystemErrorCode::Conflict
        );
        assert_eq!(fs::read(path)?, b"locally changed unit\n");
        Ok(())
    }

    #[test]
    fn explicit_repair_restores_candidate_bytes_and_rolls_back_on_later_failure()
    -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let path = fixture
            .temporary
            .path()
            .join("usr/lib/systemd/system/pkg-nix-broker.service");
        let modified = b"locally changed unit\n";
        fs::write(&path, modified)?;

        assert!(fixture.manager.replace_static_owned_file(
            asset,
            LinuxSystemdAssets::BROKER_SERVICE,
            None,
            true,
        )?);
        assert_eq!(
            fs::read(&path)?,
            LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
        );

        fixture.manager.rollback_asset(asset)?;
        assert_eq!(fs::read(path)?, modified);
        Ok(())
    }

    #[test]
    fn interrupted_upgrade_restores_prior_bytes_from_the_fixed_backup() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let path = fixture
            .temporary
            .path()
            .join("usr/lib/systemd/system/pkg-nix-broker.service");
        let prior = b"prior authenticated unit\n";
        fs::write(&path, prior)?;
        let prior_digest = body_digest(prior);
        fixture.manager.replace_static_owned_file(
            asset,
            LinuxSystemdAssets::BROKER_SERVICE,
            Some(prior_digest),
            false,
        )?;
        fixture.manager.attempt_owned.clear();

        fixture.manager.recover_owned_file(asset, prior_digest)?;

        assert_eq!(fs::read(&path)?, prior);
        assert!(!fixture.manager.replacement_backup_exists(asset)?);
        Ok(())
    }

    #[test]
    fn interrupted_upgrade_before_exchange_discards_only_the_candidate_backup()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let parent = fixture.temporary.path().join("usr/lib/systemd/system");
        let path = parent.join("pkg-nix-broker.service");
        let backup = parent.join(rollback_name(asset));
        let prior = b"prior authenticated unit\n";
        fs::write(&path, prior)?;
        fs::write(&backup, LinuxSystemdAssets::BROKER_SERVICE)?;

        fixture
            .manager
            .recover_owned_file(asset, body_digest(prior))?;

        assert_eq!(fs::read(&path)?, prior);
        assert!(!backup.exists());

        fs::write(&backup, b"partial candidate")?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
        fixture
            .manager
            .recover_owned_file(asset, body_digest(prior))?;
        assert_eq!(fs::read(path)?, prior);
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn partial_upgrade_staging_refuses_when_live_prior_identity_is_wrong()
    -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let parent = fixture.temporary.path().join("usr/lib/systemd/system");
        let path = parent.join("pkg-nix-broker.service");
        let staging = parent.join(rollback_name(asset));
        let authenticated_prior = b"authenticated prior unit\n";
        fs::write(&path, b"unknown live bytes\n")?;
        fs::write(&staging, b"partial candidate")?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;

        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .recover_owned_file(asset, body_digest(authenticated_prior)),
            )?,
            LinuxFilesystemErrorCode::Conflict
        );
        assert_eq!(fs::read(path)?, b"unknown live bytes\n");
        assert_eq!(fs::read(staging)?, b"partial candidate");
        Ok(())
    }

    #[test]
    fn deterministic_initial_file_staging_recovers_before_and_after_rename()
    -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("product-cli");
        let parent = fixture.temporary.path().join("usr/local/bin");
        let target = parent.join("pkg");
        let staging = parent.join(rollback_name(asset));

        fs::write(&staging, b"partial candidate")?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        assert!(fixture.manager.ensure_asset(asset).is_err());
        assert_eq!(fs::read(&staging)?, b"partial candidate");
        fixture.manager.recover_created_file(asset)?;
        assert!(!staging.exists());
        assert!(!target.exists());

        assert!(fixture.manager.ensure_asset(asset)?);
        fixture.manager.attempt_owned.clear();
        fixture.manager.recover_created_file(asset)?;
        assert!(!staging.exists());
        assert!(!target.exists());
        Ok(())
    }

    #[test]
    fn deterministic_initial_receipt_staging_recovers_without_residue() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "uninstall-root"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let asset = Fixture::asset("uninstall-manifest");
        let records = crate::assets::linux_product_install_assets()
            .map(|asset| {
                let record =
                    crate::RecordedAsset::new(asset.id(), crate::RecordedAssetState::Created)?;
                Ok::<_, crate::UninstallError>(
                    if asset.kind() == LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                        record.with_content_digest(body_digest(asset.id().as_bytes()))
                    } else {
                        record
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let manifest =
            UninstallManifest::new(System::X8664Linux, Digest::from_bytes([0x91; 32]), records)?;
        let release = manifest.ownership_manifest_digest();
        let prior = UninstallManifest::new(
            System::X8664Linux,
            Digest::from_bytes([0x92; 32]),
            manifest.assets().to_vec(),
        )?;
        fixture.manager.bind_uninstall_manifest(&manifest)?;
        let parent = fixture.temporary.path().join("opt/pkg/uninstall");
        let staging = parent.join(rollback_name(asset));
        fs::write(&staging, b"partial receipt")?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;

        fixture
            .manager
            .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)?;

        assert!(!staging.exists());
        assert!(!parent.join("manifest.json").exists());

        fs::write(&staging, crate::encode_uninstall_manifest(&manifest)?)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        fixture
            .manager
            .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)?;
        assert!(!staging.exists());

        fs::write(&staging, crate::encode_uninstall_manifest(&prior)?)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        assert!(
            fixture
                .manager
                .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)
                .is_err()
        );
        assert!(staging.exists());
        Ok(())
    }

    #[test]
    fn repair_recovery_keeps_candidate_and_discards_unknown_prior_bytes()
    -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let parent = fixture.temporary.path().join("usr/lib/systemd/system");
        let path = parent.join("pkg-nix-broker.service");
        let backup = parent.join(rollback_name(asset));
        fs::write(&path, LinuxSystemdAssets::BROKER_SERVICE)?;
        fs::write(&backup, b"unknown prior bytes")?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;

        fixture.manager.roll_forward_owned_file(asset)?;
        assert_eq!(
            fs::read(path)?,
            LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
        );
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn repair_roll_forward_replaces_unknown_binaries_and_changed_or_missing_units()
    -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "service-bin-dir"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        for (id, relative, mode) in [
            ("root-helper-binary", "opt/pkg/bin/pkg-root-helper", 0o750),
            ("broker-binary", "opt/pkg/bin/pkg-nix-broker", 0o750),
            (
                "broker-service-unit",
                "usr/lib/systemd/system/pkg-nix-broker.service",
                0o644,
            ),
        ] {
            let asset = Fixture::asset(id);
            let path = fixture.temporary.path().join(relative);
            fs::write(&path, b"unknown damaged bytes")?;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
            fixture.manager.roll_forward_owned_file(asset)?;
            fixture.manager.verify_asset(asset)?;
            assert!(!fixture.manager.replacement_backup_exists(asset)?);
        }

        let missing = Fixture::asset("helper-service-unit");
        fixture.manager.roll_forward_owned_file(missing)?;
        fixture.manager.verify_asset(missing)?;
        assert!(!fixture.manager.replacement_backup_exists(missing)?);
        Ok(())
    }

    #[test]
    fn committed_repair_cleanup_is_exact_and_resumable() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        let asset = Fixture::asset("broker-service-unit");
        let path = fixture
            .temporary
            .path()
            .join("usr/lib/systemd/system/pkg-nix-broker.service");
        fs::write(&path, b"locally changed unit\n")?;
        fixture.manager.replace_static_owned_file(
            asset,
            LinuxSystemdAssets::BROKER_SERVICE,
            None,
            true,
        )?;
        fixture.manager.attempt_owned.clear();

        fixture.manager.finalize_owned_file(asset)?;
        fixture.manager.finalize_owned_file(asset)?;

        assert_eq!(
            fs::read(path)?,
            LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
        );
        assert!(!fixture.manager.replacement_backup_exists(asset)?);
        Ok(())
    }

    #[test]
    fn verified_uninstall_removes_exact_files_and_is_retry_safe() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "service-bin-dir"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let asset = Fixture::asset("product-cli");
        assert!(fixture.manager.ensure_asset(asset)?);
        let path = fixture.temporary.path().join("usr/local/bin/pkg");

        fixture.manager.remove_verified_asset(asset)?;
        fixture
            .manager
            .remove_verified_asset(Fixture::asset("service-bin-dir"))?;
        fixture.manager.remove_verified_asset(asset)?;

        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn verified_uninstall_refuses_changed_files_and_nonempty_directories()
    -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "service-bin-dir"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let file = Fixture::asset("product-cli");
        assert!(fixture.manager.ensure_asset(file)?);
        let file_path = fixture.temporary.path().join("usr/local/bin/pkg");
        fs::write(&file_path, b"foreign")?;
        assert_eq!(
            failure_code(&fixture.manager.remove_verified_asset(file))?,
            LinuxFilesystemErrorCode::Conflict
        );
        assert_eq!(fs::read(file_path)?, b"foreign");

        let directory = Fixture::asset("service-bin-dir");
        fs::write(
            fixture.temporary.path().join("opt/pkg/bin/foreign"),
            b"foreign",
        )?;
        assert_eq!(
            failure_code(&fixture.manager.remove_verified_asset(directory))?,
            LinuxFilesystemErrorCode::RollbackConflict
        );
        assert!(fixture.temporary.path().join("opt/pkg/bin").is_dir());
        Ok(())
    }

    #[test]
    fn broker_channel_cleanup_removes_private_files_and_refuses_links() -> Result<(), Box<dyn Error>>
    {
        let mut fixture = Fixture::new()?;
        for id in [
            "service-root",
            "broker-home",
            "broker-channel-state",
            "log-root",
            "broker-log-dir",
        ] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let channel = fixture
            .temporary
            .path()
            .join("var/lib/pkg/broker-home/channel");
        let metadata = channel.join("root.json");
        fs::write(&metadata, b"authenticated")?;
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o644))?;

        fixture
            .manager
            .remove_broker_channel_state(Fixture::asset("broker-channel-state"))?;
        assert!(!channel.exists());
        let cache = fixture
            .temporary
            .path()
            .join("var/lib/pkg/broker-home/.cache/nix");
        fs::create_dir_all(&cache)?;
        fs::write(cache.join("cache.sqlite"), b"cache")?;
        fixture
            .manager
            .remove_private_tree(Fixture::asset("broker-home"))?;
        assert!(
            !fixture
                .temporary
                .path()
                .join("var/lib/pkg/broker-home")
                .exists()
        );
        let audit_directory = fixture.temporary.path().join("var/lib/pkg/log/broker");
        let audit = audit_directory.join("approvals.ndjson");
        fs::write(&audit, b"approved\n")?;
        fs::set_permissions(&audit, fs::Permissions::from_mode(0o600))?;
        fixture
            .manager
            .remove_private_tree(Fixture::asset("broker-log-dir"))?;
        assert!(!audit_directory.exists());

        let helper_home = fixture.temporary.path().join("var/lib/pkg/helper-home");
        fixture
            .manager
            .ensure_asset(Fixture::asset("helper-home"))?;
        let helper_cache = helper_home.join(".cache/nix");
        fs::create_dir_all(&helper_cache)?;
        fs::write(helper_cache.join("binary-cache-v7.sqlite"), b"cache")?;
        fixture
            .manager
            .remove_private_tree(Fixture::asset("helper-home"))?;
        assert!(!helper_home.exists());

        let mut fixture = Fixture::new()?;
        for id in ["service-root", "broker-home", "broker-channel-state"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let channel = fixture
            .temporary
            .path()
            .join("var/lib/pkg/broker-home/channel");
        symlink("/tmp", channel.join("root.json"))?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .remove_broker_channel_state(Fixture::asset("broker-channel-state")),
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        assert!(channel.join("root.json").is_symlink());

        let mut fixture = Fixture::new()?;
        for id in ["service-root", "broker-home", "broker-channel-state"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let channel = fixture
            .temporary
            .path()
            .join("var/lib/pkg/broker-home/channel");
        let foreign = channel.join("foreign.json");
        fs::write(&foreign, b"{}")?;
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o644))?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .remove_broker_channel_state(Fixture::asset("broker-channel-state")),
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        assert_eq!(fs::read(foreign)?, b"{}");
        Ok(())
    }

    #[test]
    fn runtime_cleanup_accepts_only_exact_socket_and_log_residue() -> Result<(), Box<dyn Error>> {
        let mut fixture = Fixture::new()?;
        for id in [
            "service-root",
            "helper-socket-dir",
            "broker-socket-dir",
            "log-root",
        ] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let helper_socket = fixture
            .temporary
            .path()
            .join("run/pkg-helper/root-helper.sock");
        let _helper = UnixListener::bind(&helper_socket)?;
        fs::set_permissions(&helper_socket, fs::Permissions::from_mode(0o660))?;
        let broker_socket = fixture.temporary.path().join("run/pkg/broker.sock");
        let _broker = UnixListener::bind(&broker_socket)?;
        fs::set_permissions(&broker_socket, fs::Permissions::from_mode(0o666))?;
        let log_root = fixture.temporary.path().join("var/lib/pkg/log");
        for name in ["nix-daemon.log", "store-volume.log"] {
            let path = log_root.join(name);
            fs::write(&path, b"runtime output")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .verify_empty_directory(Fixture::asset("log-root")),
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        for id in ["helper-socket-dir", "broker-socket-dir", "log-root"] {
            fixture.manager.remove_runtime_state(Fixture::asset(id))?;
        }
        assert!(!helper_socket.exists());
        assert!(!broker_socket.exists());
        assert!(!log_root.exists());

        let mut fixture = Fixture::new()?;
        for id in ["service-root", "log-root"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        let foreign = fixture.temporary.path().join("var/lib/pkg/log/foreign.log");
        fs::write(&foreign, b"preserve")?;
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600))?;
        assert_eq!(
            failure_code(
                &fixture
                    .manager
                    .remove_runtime_state(Fixture::asset("log-root")),
            )?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
        assert_eq!(fs::read(foreign)?, b"preserve");
        Ok(())
    }

    #[test]
    fn missing_or_conflicting_payloads_are_rejected() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            failure_code(&LinuxReleasePayloads::from_authenticated_bytes(
                b"", b"broker", b"pkg",
            ))?,
            LinuxFilesystemErrorCode::MissingPayload
        );
        let mut fixture = Fixture::new()?;
        for id in ["product-root", "product-config-root", "product-config-dir"] {
            fixture.manager.ensure_asset(Fixture::asset(id))?;
        }
        assert_eq!(
            failure_code(&fixture.manager.ensure_asset(Fixture::asset("nix-config")),)?,
            LinuxFilesystemErrorCode::MissingPayload
        );
        Ok(())
    }
}
