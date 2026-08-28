//! Private installer-repository authentication and runtime snapshots.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt as _;
use jiff::Timestamp;
use pkg_channel::{
    AcceptedChannel, ChannelError, RefreshOutcome, TrustedRoot, VerifiedChannel,
    validate_https_repository_url, validate_private_datastore, verify_authenticated_descriptor,
};
use pkg_core::{ChannelSequence, PolicyVersion, System, state::Digest};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tough::{
    DefaultTransport, ExpirationEnforcement, FilesystemTransport, HttpTransportBuilder, IntoVec,
    Limits, RepositoryLoader, TargetName,
};
use url::Url;

use super::provision::InstallerRepository;

const DESCRIPTOR_TARGET: &str = "descriptor.json";
const MAX_DESCRIPTOR_BYTES: u64 = 256 * 1024;
const MAX_INDEX_TARGET_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RUNTIME_TARGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RUNTIME_MANIFEST_TARGET_BYTES: u64 = 1024 * 1024;
const MAX_INSTALLER_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const DETERMINATE_VERSION: &str = "3.22.1";
const LIMITS: Limits = Limits {
    max_root_size: 64 * 1024,
    max_targets_size: 256 * 1024,
    max_timestamp_size: 32 * 1024,
    max_snapshot_size: 32 * 1024,
    max_root_updates: 256,
};

const STATE_FILE: &str = "accepted-channel.json";
const TEMP_FILE: &str = ".accepted-channel.json.tmp";
const INITIALIZING_FILE: &str = "accepted-channel.initializing";
const LOCK_FILE: &str = "pkg-channel.lock";
const MAX_STATE_BYTES: u64 = 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct DatastoreOwner {
    uid: u32,
    gid: u32,
}

impl DatastoreOwner {
    pub(super) fn current() -> Self {
        Self {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
        }
    }

    pub(super) const fn new(uid: u32, gid: u32) -> Option<Self> {
        if uid == 0 || gid == 0 {
            None
        } else {
            Some(Self { uid, gid })
        }
    }
}

/// One completely authenticated installer view retained in private files.
pub(super) struct VerifiedRuntimeBundle {
    channel: VerifiedChannel,
    system: System,
    index: Option<Vec<u8>>,
    base_nix: VerifiedBaseNix,
    installer_payloads: BTreeMap<String, File>,
    accepted_state: AcceptedChannel,
    accepted: AcceptedChannelStore,
    datastore_lease: Option<File>,
}

enum VerifiedBaseNix {
    Managed {
        archive: File,
        asset_manifest: File,
        runtime_target: String,
        asset_manifest_target: String,
    },
    Determinate {
        installer: Option<File>,
        length: u64,
        sha256: Digest,
    },
}

impl std::fmt::Debug for VerifiedRuntimeBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedRuntimeBundle")
            .field("channel_sequence", &self.channel.sequence())
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl VerifiedRuntimeBundle {
    pub(super) const fn channel(&self) -> &VerifiedChannel {
        &self.channel
    }

    pub(super) const fn system(&self) -> System {
        self.system
    }

    pub(super) const fn descriptor_sha256(&self) -> [u8; 32] {
        self.channel.descriptor_sha256()
    }

    pub(super) fn take_index(&mut self) -> Result<Vec<u8>, ChannelError> {
        self.index
            .take()
            .ok_or(ChannelError::InstallerBundleUnavailable)
    }

    pub(super) fn open_target(&self, target: &str) -> Result<File, ChannelError> {
        let source = match &self.base_nix {
            VerifiedBaseNix::Managed {
                archive,
                runtime_target,
                ..
            } if target == runtime_target => archive,
            VerifiedBaseNix::Managed {
                asset_manifest,
                asset_manifest_target,
                ..
            } if target == asset_manifest_target => asset_manifest,
            VerifiedBaseNix::Managed { .. } | VerifiedBaseNix::Determinate { .. } => self
                .installer_payloads
                .get(target)
                .ok_or(ChannelError::InstallerBundleUnavailable)?,
        };
        let mut file = source
            .try_clone()
            .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
        Ok(file)
    }

    pub(super) fn take_determinate_installer(
        &mut self,
    ) -> Result<(File, u64, Digest), ChannelError> {
        match &mut self.base_nix {
            VerifiedBaseNix::Determinate {
                installer,
                length,
                sha256,
            } => installer
                .take()
                .map(|file| (file, *length, *sha256))
                .ok_or(ChannelError::InstallerBundleUnavailable),
            VerifiedBaseNix::Managed { .. } => Err(ChannelError::InstallerBundleUnavailable),
        }
    }

    pub(super) fn determinate_installer_identity(&self) -> Option<(u64, Digest)> {
        match &self.base_nix {
            VerifiedBaseNix::Determinate { length, sha256, .. } => Some((*length, *sha256)),
            VerifiedBaseNix::Managed { .. } => None,
        }
    }

    pub(super) fn commit_accepted_channel(&mut self) -> Result<(), ChannelError> {
        if self.datastore_lease.is_none() {
            return Ok(());
        }
        self.persist_accepted_channel()?;
        drop(self.datastore_lease.take());
        Ok(())
    }

    pub(super) fn persist_accepted_channel(&self) -> Result<(), ChannelError> {
        self.accepted.persist(&self.accepted_state)
    }
}

/// Authenticates one fixed-layout offline installer repository without
/// publishing any target reader or rollback-state capability outside this
/// crate.
pub(super) async fn load_installer_bundle(
    trusted_root: TrustedRoot,
    source: InstallerRepository<'_>,
    datastore: &Path,
    host: System,
    datastore_owner: Option<DatastoreOwner>,
) -> Result<VerifiedRuntimeBundle, ChannelError> {
    let local_urls = match source {
        InstallerRepository::Bundle(bundle_root) => {
            let root = canonical_directory(bundle_root)?;
            let metadata = canonical_directory(&root.join("metadata"))?;
            let targets = canonical_directory(&root.join("targets"))?;
            if !metadata.starts_with(&root) || !targets.starts_with(&root) {
                return Err(ChannelError::InstallerBundleUnavailable);
            }
            Some((
                Url::from_directory_path(metadata)
                    .map_err(|()| ChannelError::InstallerBundleUnavailable)?,
                Url::from_directory_path(targets)
                    .map_err(|()| ChannelError::InstallerBundleUnavailable)?,
            ))
        }
        InstallerRepository::Remote {
            metadata_url,
            targets_url,
        } => {
            validate_https_repository_url(metadata_url)?;
            validate_https_repository_url(targets_url)?;
            None
        }
    };
    validate_private_datastore(datastore)?;
    if let Some(owner) = datastore_owner {
        let metadata =
            fs::symlink_metadata(datastore).map_err(|_| ChannelError::DatastoreUnavailable)?;
        validate_owner(&metadata, owner, 0o700)?;
    }
    validate_datastore_files(datastore, datastore_owner)?;
    let datastore_lease = open_datastore_lease(datastore, datastore_owner)?;
    let accepted = AcceptedChannelStore::new(datastore, datastore_owner);
    accepted.initialize()?;

    let repository = if let Some((metadata_url, targets_url)) = local_urls {
        RepositoryLoader::new(&trusted_root.as_bytes(), metadata_url, targets_url)
            .transport(FilesystemTransport)
            .limits(LIMITS)
            .expiration_enforcement(ExpirationEnforcement::Safe)
            .datastore(datastore)
            .load()
            .await
    } else if let InstallerRepository::Remote {
        metadata_url,
        targets_url,
    } = source
    {
        RepositoryLoader::new(
            &trusted_root.as_bytes(),
            metadata_url.clone(),
            targets_url.clone(),
        )
        .transport(DefaultTransport::new_with_http_settings(
            HttpTransportBuilder::new()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .tries(3),
        ))
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(datastore)
        .load()
        .await
    } else {
        return Err(ChannelError::InstallerBundleUnavailable);
    }
    .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    handoff_datastore_files(datastore, datastore_owner)?;
    load_verified_repository(
        repository,
        accepted,
        datastore_lease,
        host,
        Timestamp::now(),
    )
    .await
}

async fn load_verified_repository(
    repository: tough::Repository,
    accepted: AcceptedChannelStore,
    datastore_lease: File,
    host: System,
    now: Timestamp,
) -> Result<VerifiedRuntimeBundle, ChannelError> {
    let descriptor = read_descriptor_target(&repository)
        .await
        .map_err(redact_repository_error)?;
    let previous = accepted.load()?;
    let outcome =
        verify_authenticated_descriptor(&descriptor, &repository, host, previous.as_ref(), now)?;
    let channel = match outcome {
        RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
    };
    let index = read_required_index_target(&repository, channel.descriptor().index().target())
        .await
        .map_err(redact_repository_error)?;
    let base_nix = match host {
        System::X8664Linux | System::Aarch64Linux => {
            let (target, length, sha256) = determinate_installer_identity(host)?;
            let installer =
                snapshot_exact_target(&repository, target, length, Some(*sha256.as_bytes()))
                    .await?;
            if installer
                .metadata()
                .map_err(|_| ChannelError::InstallerBundleUnavailable)?
                .len()
                != length
            {
                return Err(ChannelError::InstallerBundleUnavailable);
            }
            VerifiedBaseNix::Determinate {
                installer: Some(installer),
                length,
                sha256,
            }
        }
        System::X8664Darwin | System::Aarch64Darwin => {
            let runtime = channel.descriptor().runtime();
            let archive = snapshot_exact_target(
                &repository,
                runtime.target(),
                MAX_RUNTIME_TARGET_BYTES,
                Some(parse_authenticated_sha256(runtime.sha256())?),
            )
            .await?;
            let asset_manifest = snapshot_exact_target(
                &repository,
                runtime.asset_manifest_target(),
                MAX_RUNTIME_MANIFEST_TARGET_BYTES,
                Some(parse_authenticated_sha256(runtime.asset_manifest_sha256())?),
            )
            .await?;
            VerifiedBaseNix::Managed {
                archive,
                asset_manifest,
                runtime_target: runtime.target().to_owned(),
                asset_manifest_target: runtime.asset_manifest_target().to_owned(),
            }
        }
    };
    let mut installer_payloads = BTreeMap::new();
    for name in ["pkg-root-helper", "pkg-nix-broker", "pkg"] {
        let target = format!("installer/{host}/{name}");
        let snapshot =
            snapshot_exact_target(&repository, &target, MAX_INSTALLER_BINARY_BYTES, None).await?;
        installer_payloads.insert(target, snapshot);
    }
    Ok(VerifiedRuntimeBundle {
        accepted_state: channel.accepted_state(),
        channel,
        system: host,
        index: Some(index),
        base_nix,
        installer_payloads,
        accepted,
        datastore_lease: Some(datastore_lease),
    })
}

fn determinate_installer_identity(
    host: System,
) -> Result<(&'static str, u64, Digest), ChannelError> {
    let (target, length, sha256) = match host {
        System::X8664Linux => (
            "determinate/3.22.1/nix-installer-x86_64-linux",
            74_918_096,
            "9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c",
        ),
        System::Aarch64Linux => (
            "determinate/3.22.1/nix-installer-aarch64-linux",
            69_625_424,
            "9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179",
        ),
        System::X8664Darwin | System::Aarch64Darwin => {
            return Err(ChannelError::InstallerBundleUnavailable);
        }
    };
    debug_assert!(target.starts_with(&format!("determinate/{DETERMINATE_VERSION}/")));
    let digest = format!("sha256-{sha256}")
        .parse()
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    Ok((target, length, digest))
}

async fn read_descriptor_target(repository: &tough::Repository) -> Result<Vec<u8>, ChannelError> {
    let name = TargetName::new(DESCRIPTOR_TARGET).map_err(|_| ChannelError::MissingDescriptor)?;
    let metadata = repository
        .all_targets()
        .find_map(|(candidate, metadata)| (candidate == &name).then_some(metadata))
        .ok_or(ChannelError::MissingDescriptor)?;
    if metadata.length > MAX_DESCRIPTOR_BYTES {
        return Err(ChannelError::DescriptorTooLarge);
    }
    let stream = repository
        .read_target(&name)
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))?
        .ok_or(ChannelError::MissingDescriptor)?;
    IntoVec::into_vec(stream)
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))
}

async fn read_required_index_target(
    repository: &tough::Repository,
    target: &str,
) -> Result<Vec<u8>, ChannelError> {
    let name = TargetName::new(target).map_err(|_| ChannelError::MissingIndexTarget)?;
    let metadata = repository
        .all_targets()
        .find_map(|(candidate, metadata)| (candidate == &name).then_some(metadata))
        .ok_or(ChannelError::MissingIndexTarget)?;
    if metadata.length > MAX_INDEX_TARGET_BYTES {
        return Err(ChannelError::IndexTargetTooLarge);
    }
    let stream = repository
        .read_target(&name)
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))?
        .ok_or(ChannelError::MissingIndexTarget)?;
    IntoVec::into_vec(stream)
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))
}

async fn snapshot_exact_target(
    repository: &tough::Repository,
    target: &str,
    max_bytes: u64,
    expected_sha256: Option<[u8; 32]>,
) -> Result<File, ChannelError> {
    let name =
        TargetName::new(target).map_err(|_| ChannelError::MissingTufTarget(target.into()))?;
    let metadata = repository
        .all_targets()
        .find_map(|(candidate, metadata)| (candidate == &name).then_some(metadata))
        .ok_or_else(|| ChannelError::MissingTufTarget(target.into()))?;
    if metadata.length > max_bytes {
        return Err(ChannelError::InstallerBundleUnavailable);
    }
    let mut stream = repository
        .read_target(&name)
        .await
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?
        .ok_or_else(|| ChannelError::MissingTufTarget(target.into()))?;
    let mut snapshot =
        tempfile::tempfile().map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ChannelError::InstallerBundleUnavailable)?;
        copied = copied
            .checked_add(chunk.len() as u64)
            .ok_or(ChannelError::InstallerBundleUnavailable)?;
        if copied > max_bytes {
            return Err(ChannelError::InstallerBundleUnavailable);
        }
        digest.update(chunk.as_ref());
        snapshot
            .write_all(chunk.as_ref())
            .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    }
    if copied != metadata.length
        || expected_sha256.is_some_and(|expected| <[u8; 32]>::from(digest.finalize()) != expected)
    {
        return Err(ChannelError::InstallerBundleUnavailable);
    }
    snapshot
        .sync_all()
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    snapshot
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    Ok(snapshot)
}

fn parse_authenticated_sha256(value: &str) -> Result<[u8; 32], ChannelError> {
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest)
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    Ok(digest)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ChannelError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|_| ChannelError::InstallerBundleUnavailable)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ChannelError::InstallerBundleUnavailable);
    }
    path.canonicalize()
        .map_err(|_| ChannelError::InstallerBundleUnavailable)
}

fn redact_repository_error(error: ChannelError) -> ChannelError {
    match error {
        ChannelError::TufVerification(_) => ChannelError::InstallerBundleUnavailable,
        error => error,
    }
}

fn open_datastore_lease(
    datastore: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<File, ChannelError> {
    let lock_path = datastore.join(LOCK_FILE);
    if matches!(
        fs::symlink_metadata(&lock_path),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file()
    ) {
        return Err(ChannelError::DatastoreUnavailable);
    }
    let lease = if owner.is_some() {
        let mut create = File::options();
        create.read(true).write(true).create_new(true);
        #[cfg(unix)]
        create.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        match create.open(&lock_path) {
            Ok(file) => {
                apply_owner(&file, owner, &lock_path)?;
                file
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_existing_lease(&lock_path)?
            }
            Err(_) => return Err(ChannelError::DatastoreUnavailable),
        }
    } else {
        let mut options = File::options();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        options
            .open(&lock_path)
            .map_err(|_| ChannelError::DatastoreUnavailable)?
    };
    #[cfg(unix)]
    {
        let metadata = lease
            .metadata()
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        if let Some(owner) = owner {
            validate_owner(&metadata, owner, 0o600)?;
        }
        if metadata.permissions().mode() & 0o177 != 0 {
            lease
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| ChannelError::DatastoreUnavailable)?;
            lease
                .sync_all()
                .map_err(|_| ChannelError::DatastoreUnavailable)?;
        }
    }
    lease.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => ChannelError::DatastoreBusy,
        fs::TryLockError::Error(_) => ChannelError::DatastoreUnavailable,
    })?;
    Ok(lease)
}

fn open_existing_lease(path: &Path) -> Result<File, ChannelError> {
    let mut options = File::options();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options
        .open(path)
        .map_err(|_| ChannelError::DatastoreUnavailable)
}

#[derive(Debug, Clone)]
struct AcceptedChannelStore {
    directory: PathBuf,
    owner: Option<DatastoreOwner>,
}

impl AcceptedChannelStore {
    fn new(directory: &Path, owner: Option<DatastoreOwner>) -> Self {
        Self {
            directory: directory.to_path_buf(),
            owner,
        }
    }

    fn load(&self) -> Result<Option<AcceptedChannel>, ChannelError> {
        let path = self.directory.join(STATE_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ChannelError::AcceptedStateUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ChannelError::AcceptedStateUnavailable),
        }
        let file = open_read_nofollow(&path)?;
        let metadata = file
            .metadata()
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        #[cfg(unix)]
        {
            if metadata.permissions().mode() & 0o177 != 0 {
                return Err(ChannelError::AcceptedStateUnavailable);
            }
            if let Some(owner) = self.owner {
                validate_owner(&metadata, owner, 0o600)?;
            }
        }
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let wire: AcceptedChannelWire =
            serde_json::from_slice(&bytes).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        wire.promote().map(Some)
    }

    fn initialize(&self) -> Result<(), ChannelError> {
        if self.load()?.is_some() {
            return Ok(());
        }
        let marker = self.directory.join(INITIALIZING_FILE);
        match fs::symlink_metadata(&marker) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() == 0 =>
            {
                let file = open_read_nofollow(&marker)?;
                let metadata = file
                    .metadata()
                    .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
                if !metadata.is_file() || metadata.len() != 0 {
                    return Err(ChannelError::AcceptedStateUnavailable);
                }
                #[cfg(unix)]
                {
                    if metadata.permissions().mode() & 0o177 != 0 {
                        return Err(ChannelError::AcceptedStateUnavailable);
                    }
                    if let Some(owner) = self.owner {
                        validate_owner(&metadata, owner, 0o600)?;
                    }
                }
                return Ok(());
            }
            Ok(_) => return Err(ChannelError::AcceptedStateUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ChannelError::AcceptedStateUnavailable),
        }
        let entries =
            fs::read_dir(&self.directory).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        for entry in entries {
            let entry = entry.map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            if entry.file_name() != LOCK_FILE {
                return Err(ChannelError::AcceptedStateUnavailable);
            }
        }
        let marker_file = open_create_new_nofollow(&marker, self.owner)?;
        marker_file
            .sync_all()
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        sync_directory(&self.directory)
    }

    fn persist(&self, state: &AcceptedChannel) -> Result<(), ChannelError> {
        let wire = AcceptedChannelWire::from_state(state);
        let mut bytes =
            serde_json::to_vec(&wire).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let temporary = self.directory.join(TEMP_FILE);
        remove_stale_regular_temp(&temporary, self.owner)?;
        let mut file = open_create_new_nofollow(&temporary, self.owner)?;
        let result = (|| {
            file.write_all(&bytes)
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            file.sync_all()
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            fs::rename(&temporary, self.directory.join(STATE_FILE))
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            sync_directory(&self.directory)?;
            remove_initializing_marker(&self.directory.join(INITIALIZING_FILE), self.owner)?;
            sync_directory(&self.directory)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptedChannelWire {
    schema_version: u64,
    sequence: u64,
    policy_version: u64,
    descriptor_sha256: String,
}

impl AcceptedChannelWire {
    fn from_state(state: &AcceptedChannel) -> Self {
        Self {
            schema_version: 1,
            sequence: state.sequence().get().get(),
            policy_version: state.policy_version().get().get(),
            descriptor_sha256: hex::encode(state.descriptor_sha256()),
        }
    }

    fn promote(self) -> Result<AcceptedChannel, ChannelError> {
        if self.schema_version != 1
            || self.descriptor_sha256.len() != 64
            || !self
                .descriptor_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let sequence = ChannelSequence::from_u64(self.sequence)
            .ok_or(ChannelError::AcceptedStateUnavailable)?;
        let policy_version = PolicyVersion::from_u64(self.policy_version)
            .ok_or(ChannelError::AcceptedStateUnavailable)?;
        let digest = hex::decode(self.descriptor_sha256)
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        let descriptor_sha256 = digest
            .try_into()
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        Ok(AcceptedChannel::new(
            sequence,
            policy_version,
            descriptor_sha256,
        ))
    }
}

fn remove_initializing_marker(
    path: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<(), ChannelError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && owner.is_none_or(|owner| validate_owner(&metadata, owner, 0o600).is_ok()) =>
        {
            fs::remove_file(path).map_err(|_| ChannelError::AcceptedStateUnavailable)
        }
        Ok(_) => Err(ChannelError::AcceptedStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ChannelError::AcceptedStateUnavailable),
    }
}

fn sync_directory(path: &Path) -> Result<(), ChannelError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ChannelError::AcceptedStateUnavailable)
}

fn remove_stale_regular_temp(
    path: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<(), ChannelError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && owner.is_none_or(|owner| validate_owner(&metadata, owner, 0o600).is_ok()) =>
        {
            fs::remove_file(path).map_err(|_| ChannelError::AcceptedStateUnavailable)
        }
        Ok(_) => Err(ChannelError::AcceptedStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ChannelError::AcceptedStateUnavailable),
    }
}

fn open_read_nofollow(path: &Path) -> Result<File, ChannelError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options
        .open(path)
        .map_err(|_| ChannelError::AcceptedStateUnavailable)
}

fn open_create_new_nofollow(
    path: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<File, ChannelError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
    apply_owner(&file, owner, path)?;
    Ok(file)
}

fn apply_owner(
    file: &File,
    owner: Option<DatastoreOwner>,
    path: &Path,
) -> Result<(), ChannelError> {
    let Some(owner) = owner else {
        return Ok(());
    };
    if rustix::fs::fchown(
        file,
        Some(rustix::fs::Uid::from_raw(owner.uid)),
        Some(rustix::fs::Gid::from_raw(owner.gid)),
    )
    .is_err()
    {
        let _ = fs::remove_file(path);
        return Err(ChannelError::DatastoreUnavailable);
    }
    file.sync_all()
        .map_err(|_| ChannelError::DatastoreUnavailable)
}

fn validate_datastore_files(
    datastore: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<(), ChannelError> {
    for entry in fs::read_dir(datastore).map_err(|_| ChannelError::DatastoreUnavailable)? {
        let entry = entry.map_err(|_| ChannelError::DatastoreUnavailable)?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|_| ChannelError::DatastoreUnavailable)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1 {
            return Err(ChannelError::DatastoreUnavailable);
        }
        if let Some(owner) = owner {
            validate_owner(&metadata, owner, 0o600)?;
        }
    }
    Ok(())
}

fn handoff_datastore_files(
    datastore: &Path,
    owner: Option<DatastoreOwner>,
) -> Result<(), ChannelError> {
    let Some(owner) = owner else {
        return Ok(());
    };
    for entry in fs::read_dir(datastore).map_err(|_| ChannelError::DatastoreUnavailable)? {
        let path = entry
            .map_err(|_| ChannelError::DatastoreUnavailable)?
            .path();
        let mut options = File::options();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW);
        let file = options
            .open(&path)
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(ChannelError::DatastoreUnavailable);
        }
        rustix::fs::fchown(
            &file,
            Some(rustix::fs::Uid::from_raw(owner.uid)),
            Some(rustix::fs::Gid::from_raw(owner.gid)),
        )
        .map_err(|_| ChannelError::DatastoreUnavailable)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        file.sync_all()
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
    }
    sync_directory(datastore)
}

fn validate_owner(
    metadata: &fs::Metadata,
    owner: DatastoreOwner,
    mode: u32,
) -> Result<(), ChannelError> {
    if metadata.uid() != owner.uid
        || metadata.gid() != owner.gid
        || metadata.permissions().mode() & 0o7777 != mode
    {
        return Err(ChannelError::DatastoreUnavailable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_channel::ChannelClient;
    use tempfile::TempDir;

    const ROOT: &[u8] = include_bytes!("../../../../fixtures/channel-v1/root.json");

    #[test]
    fn platform_route_selects_only_the_two_linux_determinate_targets() {
        let (x86_target, x86_length, x86_digest) =
            determinate_installer_identity(System::X8664Linux).unwrap();
        assert_eq!(x86_target, "determinate/3.22.1/nix-installer-x86_64-linux");
        assert_eq!(x86_length, 74_918_096);
        assert_eq!(
            x86_digest.to_string(),
            "sha256-9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c"
        );

        let (arm_target, arm_length, arm_digest) =
            determinate_installer_identity(System::Aarch64Linux).unwrap();
        assert_eq!(arm_target, "determinate/3.22.1/nix-installer-aarch64-linux");
        assert_eq!(arm_length, 69_625_424);
        assert_eq!(
            arm_digest.to_string(),
            "sha256-9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179"
        );

        assert!(determinate_installer_identity(System::X8664Darwin).is_err());
        assert!(determinate_installer_identity(System::Aarch64Darwin).is_err());
    }

    #[tokio::test]
    async fn remote_repository_refuses_unsafe_urls_before_state_creation() {
        let temporary = TempDir::new().unwrap();
        let datastore = temporary.path().join("datastore");
        let metadata_url = Url::parse("http://release.invalid/metadata/").unwrap();
        let targets_url = Url::parse("https://release.invalid/targets/").unwrap();

        let result = load_installer_bundle(
            TrustedRoot::from_embedded(ROOT).unwrap(),
            InstallerRepository::Remote {
                metadata_url: &metadata_url,
                targets_url: &targets_url,
            },
            &datastore,
            System::Aarch64Linux,
            None,
        )
        .await;

        assert!(matches!(result, Err(ChannelError::InvalidRepositoryUrl)));
        assert!(!datastore.exists());
    }

    #[tokio::test]
    async fn bundle_targets_are_private_and_floor_is_explicit() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/channel-v1")
            .canonicalize()
            .unwrap();
        let temporary = TempDir::new().unwrap();
        let datastore = temporary.path().join("datastore");
        fs::create_dir(&datastore).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&datastore, fs::Permissions::from_mode(0o700)).unwrap();

        let mut bundle = load_installer_bundle(
            TrustedRoot::from_embedded(ROOT).unwrap(),
            InstallerRepository::Bundle(&fixture),
            &datastore,
            System::Aarch64Darwin,
            None,
        )
        .await
        .unwrap();
        assert_eq!(bundle.channel().sequence().get().get(), 42);
        assert!(!bundle.take_index().unwrap().is_empty());
        assert!(!datastore.join(STATE_FILE).exists());
        let runtime_target = bundle.channel().descriptor().runtime().target();
        assert!(
            bundle
                .open_target(runtime_target)
                .unwrap()
                .metadata()
                .unwrap()
                .is_file()
        );
        for name in ["pkg-root-helper", "pkg-nix-broker", "pkg"] {
            let target = format!("installer/aarch64-darwin/{name}");
            let mut bytes = Vec::new();
            bundle
                .open_target(&target)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(
                bytes,
                format!("installer payload {name} aarch64-darwin\n").as_bytes()
            );
        }
        bundle.commit_accepted_channel().unwrap();
        assert!(datastore.join(STATE_FILE).is_file());
    }

    #[tokio::test]
    async fn accepted_floor_commit_releases_the_datastore_lease() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/channel-v1")
            .canonicalize()
            .unwrap();
        let temporary = TempDir::new().unwrap();
        let datastore = temporary.path().join("datastore");
        fs::create_dir(&datastore).unwrap();
        fs::set_permissions(&datastore, fs::Permissions::from_mode(0o700)).unwrap();
        let root = TrustedRoot::from_embedded(ROOT).unwrap();
        let metadata = Url::parse("https://updates.example/metadata/").unwrap();
        let targets = Url::parse("https://updates.example/targets/").unwrap();
        let mut bundle = load_installer_bundle(
            root.clone(),
            InstallerRepository::Bundle(&fixture),
            &datastore,
            System::Aarch64Darwin,
            None,
        )
        .await
        .unwrap();

        assert!(matches!(
            ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), &datastore),
            Err(ChannelError::DatastoreBusy)
        ));
        bundle.accepted.directory = temporary.path().join("missing");
        assert!(bundle.commit_accepted_channel().is_err());
        assert!(matches!(
            ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), &datastore),
            Err(ChannelError::DatastoreBusy)
        ));

        bundle.accepted.directory = datastore.clone();
        bundle.commit_accepted_channel().unwrap();
        let _client = ChannelClient::new(root, metadata, targets, &datastore).unwrap();
        bundle.accepted.directory = temporary.path().join("missing");
        bundle.commit_accepted_channel().unwrap();
    }

    #[test]
    fn owner_bound_state_is_private_before_publication() {
        let temporary = TempDir::new().unwrap();
        let datastore = temporary.path().join("datastore");
        fs::create_dir(&datastore).unwrap();
        fs::set_permissions(&datastore, fs::Permissions::from_mode(0o700)).unwrap();
        let owner = DatastoreOwner {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
        };
        let lease = open_datastore_lease(&datastore, Some(owner)).unwrap();
        let store = AcceptedChannelStore::new(&datastore, Some(owner));

        store.initialize().unwrap();
        for name in [LOCK_FILE, INITIALIZING_FILE] {
            validate_owner(
                &fs::symlink_metadata(datastore.join(name)).unwrap(),
                owner,
                0o600,
            )
            .unwrap();
        }
        store
            .persist(&AcceptedChannel::new(
                ChannelSequence::from_u64(7).unwrap(),
                PolicyVersion::from_u64(3).unwrap(),
                [5; 32],
            ))
            .unwrap();
        validate_owner(
            &fs::symlink_metadata(datastore.join(STATE_FILE)).unwrap(),
            owner,
            0o600,
        )
        .unwrap();
        assert!(!datastore.join(INITIALIZING_FILE).exists());

        fs::set_permissions(
            datastore.join(STATE_FILE),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(store.load().is_err());

        drop(lease);
        fs::set_permissions(datastore.join(LOCK_FILE), fs::Permissions::from_mode(0o640)).unwrap();
        assert!(open_datastore_lease(&datastore, Some(owner)).is_err());
    }

    #[test]
    fn datastore_handoff_is_private_and_rejects_links() {
        let temporary = TempDir::new().unwrap();
        let datastore = temporary.path().join("datastore");
        fs::create_dir(&datastore).unwrap();
        fs::set_permissions(&datastore, fs::Permissions::from_mode(0o700)).unwrap();
        let metadata_path = datastore.join("root.json");
        fs::write(&metadata_path, b"{}\n").unwrap();
        fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o644)).unwrap();
        let owner = DatastoreOwner {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
        };

        handoff_datastore_files(&datastore, Some(owner)).unwrap();
        validate_owner(&fs::symlink_metadata(&metadata_path).unwrap(), owner, 0o600).unwrap();

        let link = datastore.join("timestamp.json");
        std::os::unix::fs::symlink(&metadata_path, &link).unwrap();
        assert!(validate_datastore_files(&datastore, Some(owner)).is_err());
        fs::remove_file(link).unwrap();

        let link = datastore.join("snapshot.json");
        fs::hard_link(&metadata_path, &link).unwrap();
        assert!(validate_datastore_files(&datastore, Some(owner)).is_err());
    }
}
