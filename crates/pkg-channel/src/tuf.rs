use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use pkg_core::System;
use tough::{
    DefaultTransport, ExpirationEnforcement, HttpTransportBuilder, IntoVec, Limits,
    RepositoryLoader, TargetName,
};
use url::Url;

use crate::keys::TrustedRoot;
use crate::policy::{
    AcceptedChannel, ChannelError, DESCRIPTOR_TARGET, MAX_DESCRIPTOR_BYTES, RefreshOutcome,
    VerifiedChannel, validate_datastore, validate_descriptor, validate_repository_url,
};
use crate::state::{AcceptedChannelStore, LOCK_FILE};

const LIMITS: Limits = Limits {
    max_root_size: 64 * 1024,
    max_targets_size: 256 * 1024,
    max_timestamp_size: 32 * 1024,
    max_snapshot_size: 32 * 1024,
    max_root_updates: 256,
};
const MAX_INDEX_TARGET_BYTES: u64 = 128 * 1024 * 1024;

/// Exact host-index bytes returned only after TUF target verification completes.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedIndexTarget {
    system: System,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for AuthenticatedIndexTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedIndexTarget")
            .field("system", &self.system)
            .field("byte_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl AuthenticatedIndexTarget {
    /// Returns the native system selected before authenticated target lookup.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Borrows the exact verified compressed target bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the capability into the exact verified compressed target bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// One descriptor refresh and its index from the same authenticated repository view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRefresh<T> {
    outcome: RefreshOutcome,
    index: T,
}

fn open_datastore_lease(datastore: &Path) -> Result<File, ChannelError> {
    let lock_path = datastore.join(LOCK_FILE);
    if matches!(
        std::fs::symlink_metadata(&lock_path),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file()
    ) {
        return Err(ChannelError::DatastoreUnavailable);
    }
    let mut lock_options = File::options();
    lock_options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false);
    #[cfg(unix)]
    lock_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let datastore_lease = lock_options
        .open(&lock_path)
        .map_err(|_| ChannelError::DatastoreUnavailable)?;
    #[cfg(unix)]
    {
        let metadata = datastore_lease
            .metadata()
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        if metadata.permissions().mode() & 0o177 != 0 {
            datastore_lease
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ChannelError::DatastoreUnavailable)?;
            datastore_lease
                .sync_all()
                .map_err(|_| ChannelError::DatastoreUnavailable)?;
        }
    }
    datastore_lease.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => ChannelError::DatastoreBusy,
        std::fs::TryLockError::Error(_) => ChannelError::DatastoreUnavailable,
    })?;
    Ok(datastore_lease)
}

impl<T> ChannelRefresh<T> {
    /// Borrows the verified descriptor refresh outcome.
    #[must_use]
    pub const fn outcome(&self) -> &RefreshOutcome {
        &self.outcome
    }

    /// Borrows the authenticated host-index target.
    #[must_use]
    pub const fn index(&self) -> &T {
        &self.index
    }

    /// Consumes the refresh into its two authenticated capabilities.
    #[must_use]
    pub fn into_parts(self) -> (RefreshOutcome, T) {
        (self.outcome, self.index)
    }
}

/// Fixed-policy production TUF client.
#[derive(Debug)]
pub struct ChannelClient {
    trusted_root: TrustedRoot,
    metadata_url: Url,
    targets_url: Url,
    datastore: PathBuf,
    accepted: AcceptedChannelStore,
    refresh_lease: tokio::sync::Mutex<()>,
    _datastore_lease: File,
}

impl ChannelClient {
    /// Creates a client for pinned HTTPS origins and an existing private,
    /// persistent datastore directory. The client holds the datastore's sole
    /// cross-process writer lease until it is dropped.
    pub fn new(
        trusted_root: TrustedRoot,
        metadata_url: Url,
        targets_url: Url,
        datastore: impl Into<PathBuf>,
    ) -> Result<Self, ChannelError> {
        Self::new_inner(
            trusted_root,
            metadata_url,
            targets_url,
            datastore.into(),
            None,
        )
    }

    /// Opens an established pre-durable-state datastore using its formerly
    /// caller-owned accepted identity as a one-time migration seed.
    ///
    /// This refuses fresh datastores, interrupted new-format initialization,
    /// and any seed that conflicts with an already durable identity.
    pub fn migrate_legacy(
        trusted_root: TrustedRoot,
        metadata_url: Url,
        targets_url: Url,
        datastore: impl Into<PathBuf>,
        legacy: &AcceptedChannel,
    ) -> Result<Self, ChannelError> {
        Self::new_inner(
            trusted_root,
            metadata_url,
            targets_url,
            datastore.into(),
            Some(legacy),
        )
    }

    fn new_inner(
        trusted_root: TrustedRoot,
        metadata_url: Url,
        targets_url: Url,
        datastore: PathBuf,
        legacy: Option<&AcceptedChannel>,
    ) -> Result<Self, ChannelError> {
        validate_repository_url(&metadata_url)?;
        validate_repository_url(&targets_url)?;
        validate_datastore(&datastore)?;
        let datastore_lease = open_datastore_lease(&datastore)?;
        let accepted = AcceptedChannelStore::new(&datastore);
        accepted.initialize(legacy)?;
        Ok(Self {
            trusted_root,
            metadata_url,
            targets_url,
            accepted,
            datastore,
            refresh_lease: tokio::sync::Mutex::new(()),
            _datastore_lease: datastore_lease,
        })
    }

    /// Refreshes TUF metadata and returns only fully verified V1 policy.
    pub async fn refresh(&self, host: System) -> Result<RefreshOutcome, ChannelError> {
        self.refresh_with_transport(
            host,
            Timestamp::now(),
            DefaultTransport::new_with_http_settings(
                HttpTransportBuilder::new()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .tries(3),
            ),
        )
        .await
    }

    /// Refreshes policy and fetches the referenced host index from the same
    /// authenticated TUF repository view.
    ///
    /// Target length is bounded from signed metadata before allocation. Bytes
    /// are returned only after `tough` reaches end-of-stream and validates the
    /// complete target checksum.
    pub async fn refresh_with_index<T>(
        &self,
        host: System,
        verifier: impl FnOnce(&VerifiedChannel, &AuthenticatedIndexTarget) -> Result<T, ()>,
    ) -> Result<ChannelRefresh<T>, ChannelError> {
        self.refresh_with_index_and_transport(
            host,
            Timestamp::now(),
            DefaultTransport::new_with_http_settings(
                HttpTransportBuilder::new()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .tries(3),
            ),
            true,
            verifier,
        )
        .await
    }

    /// Authenticates the newest channel and index without accepting the
    /// descriptor identity into durable product state.
    ///
    /// The TUF client may update its disposable transport metadata cache. The
    /// accepted channel floor and all live broker authority remain unchanged.
    pub async fn check_with_index<T>(
        &self,
        host: System,
        verifier: impl FnOnce(&VerifiedChannel, &AuthenticatedIndexTarget) -> Result<T, ()>,
    ) -> Result<ChannelRefresh<T>, ChannelError> {
        self.refresh_with_index_and_transport(
            host,
            Timestamp::now(),
            DefaultTransport::new_with_http_settings(
                HttpTransportBuilder::new()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .tries(3),
            ),
            false,
            verifier,
        )
        .await
    }

    async fn refresh_with_transport(
        &self,
        host: System,
        now: Timestamp,
        transport: DefaultTransport,
    ) -> Result<RefreshOutcome, ChannelError> {
        let _refresh = self.refresh_lease.lock().await;
        let repository = RepositoryLoader::new(
            &self.trusted_root.as_bytes(),
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .transport(transport)
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(&self.datastore)
        .load()
        .await
        .map_err(map_tough_error)?;

        let bytes = read_descriptor_target(&repository).await?;
        let previous = self.accepted.load()?;
        let outcome = validate_descriptor(&bytes, &repository, host, previous.as_ref(), now)?;
        self.persist_outcome(&outcome)?;
        Ok(outcome)
    }

    async fn refresh_with_index_and_transport<T>(
        &self,
        host: System,
        now: Timestamp,
        transport: DefaultTransport,
        persist: bool,
        verifier: impl FnOnce(&VerifiedChannel, &AuthenticatedIndexTarget) -> Result<T, ()>,
    ) -> Result<ChannelRefresh<T>, ChannelError> {
        let _refresh = self.refresh_lease.lock().await;
        let repository = RepositoryLoader::new(
            &self.trusted_root.as_bytes(),
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .transport(transport)
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(&self.datastore)
        .load()
        .await
        .map_err(map_tough_error)?;

        let bytes = read_descriptor_target(&repository).await?;
        let previous = self.accepted.load()?;
        let outcome = validate_descriptor(&bytes, &repository, host, previous.as_ref(), now)?;
        let channel = match &outcome {
            RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
        };
        let target = channel.descriptor().index().target();
        let bytes = read_required_index_target(&repository, target).await?;
        let target = AuthenticatedIndexTarget {
            system: host,
            bytes,
        };
        let index =
            verifier(channel, &target).map_err(|()| ChannelError::IndexVerificationRefused)?;
        if persist {
            self.persist_outcome(&outcome)?;
        }
        Ok(ChannelRefresh { outcome, index })
    }

    fn persist_outcome(&self, outcome: &RefreshOutcome) -> Result<(), ChannelError> {
        let channel = match outcome {
            RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
        };
        self.accepted.persist(&channel.accepted_state())
    }
}

async fn read_descriptor_target(repository: &tough::Repository) -> Result<Vec<u8>, ChannelError> {
    let name = TargetName::new(DESCRIPTOR_TARGET).map_err(|_| ChannelError::MissingDescriptor)?;
    let metadata = repository
        .all_targets()
        .find_map(|(candidate, metadata)| (candidate == &name).then_some(metadata))
        .ok_or(ChannelError::MissingDescriptor)?;
    if metadata.length > MAX_DESCRIPTOR_BYTES as u64 {
        return Err(ChannelError::DescriptorTooLarge);
    }
    let stream = repository
        .read_target(&name)
        .await
        .map_err(map_tough_error)?
        .ok_or(ChannelError::MissingDescriptor)?;
    IntoVec::into_vec(stream).await.map_err(map_tough_error)
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
        .map_err(map_tough_error)?
        .ok_or(ChannelError::MissingIndexTarget)?;
    IntoVec::into_vec(stream).await.map_err(map_tough_error)
}

fn map_tough_error(error: tough::error::Error) -> ChannelError {
    use tough::error::Error;

    match error {
        Error::Transport { .. } => ChannelError::TransportUnavailable,
        Error::AbsolutePath { .. }
        | Error::DatastoreInit { .. }
        | Error::DatastoreCreate { .. }
        | Error::DatastoreOpen { .. }
        | Error::DatastoreRemove { .. }
        | Error::DatastoreSerialize { .. }
        | Error::DirCreate { .. }
        | Error::FileMetadata { .. }
        | Error::FileOpen { .. }
        | Error::FileRead { .. }
        | Error::FileParseJson { .. }
        | Error::FileWrite { .. }
        | Error::NamedTempFileCreate { .. }
        | Error::NamedTempFilePersist { .. }
        | Error::CacheFileRead { .. }
        | Error::CacheFileWrite { .. }
        | Error::CacheDirectoryCreate { .. }
        | Error::CacheTargetWrite { .. }
        | Error::WalkDir { .. }
        | Error::JoinSpawnBlockingTask { .. } => ChannelError::DatastoreUnavailable,
        other => ChannelError::TufVerification(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
