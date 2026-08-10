use std::fs::File;
use std::path::PathBuf;
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
    AcceptedChannel, ChannelError, DESCRIPTOR_TARGET, RefreshOutcome, validate_datastore,
    validate_descriptor, validate_repository_url,
};

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
pub struct ChannelRefresh {
    outcome: RefreshOutcome,
    index: AuthenticatedIndexTarget,
}

impl ChannelRefresh {
    /// Borrows the verified descriptor refresh outcome.
    #[must_use]
    pub const fn outcome(&self) -> &RefreshOutcome {
        &self.outcome
    }

    /// Borrows the authenticated host-index target.
    #[must_use]
    pub const fn index(&self) -> &AuthenticatedIndexTarget {
        &self.index
    }

    /// Consumes the refresh into its two authenticated capabilities.
    #[must_use]
    pub fn into_parts(self) -> (RefreshOutcome, AuthenticatedIndexTarget) {
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
        validate_repository_url(&metadata_url)?;
        validate_repository_url(&targets_url)?;
        let datastore = datastore.into();
        validate_datastore(&datastore)?;
        let datastore_lease = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(datastore.join("pkg-channel.lock"))
            .map_err(|_| ChannelError::DatastoreUnavailable)?;
        datastore_lease.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => ChannelError::DatastoreBusy,
            std::fs::TryLockError::Error(_) => ChannelError::DatastoreUnavailable,
        })?;
        Ok(Self {
            trusted_root,
            metadata_url,
            targets_url,
            datastore,
            _datastore_lease: datastore_lease,
        })
    }

    /// Refreshes TUF metadata and returns only fully verified V1 policy.
    pub async fn refresh(
        &self,
        host: System,
        previous: Option<&AcceptedChannel>,
    ) -> Result<RefreshOutcome, ChannelError> {
        self.refresh_with_transport(
            host,
            previous,
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
    pub async fn refresh_with_index(
        &self,
        host: System,
        previous: Option<&AcceptedChannel>,
    ) -> Result<ChannelRefresh, ChannelError> {
        self.refresh_with_index_and_transport(
            host,
            previous,
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

    async fn refresh_with_transport(
        &self,
        host: System,
        previous: Option<&AcceptedChannel>,
        now: Timestamp,
        transport: DefaultTransport,
    ) -> Result<RefreshOutcome, ChannelError> {
        let repository = RepositoryLoader::new(
            &self.trusted_root.bytes(),
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .transport(transport)
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(&self.datastore)
        .load()
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))?;

        let name =
            TargetName::new(DESCRIPTOR_TARGET).map_err(|_| ChannelError::MissingDescriptor)?;
        let stream = repository
            .read_target(&name)
            .await
            .map_err(|error| ChannelError::TufVerification(error.to_string()))?
            .ok_or(ChannelError::MissingDescriptor)?;
        let bytes = IntoVec::into_vec(stream)
            .await
            .map_err(|error| ChannelError::TufVerification(error.to_string()))?;
        validate_descriptor(&bytes, &repository, host, previous, now)
    }

    async fn refresh_with_index_and_transport(
        &self,
        host: System,
        previous: Option<&AcceptedChannel>,
        now: Timestamp,
        transport: DefaultTransport,
    ) -> Result<ChannelRefresh, ChannelError> {
        let repository = RepositoryLoader::new(
            &self.trusted_root.bytes(),
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .transport(transport)
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(&self.datastore)
        .load()
        .await
        .map_err(|error| ChannelError::TufVerification(error.to_string()))?;

        let descriptor_name =
            TargetName::new(DESCRIPTOR_TARGET).map_err(|_| ChannelError::MissingDescriptor)?;
        let descriptor_stream = repository
            .read_target(&descriptor_name)
            .await
            .map_err(|error| ChannelError::TufVerification(error.to_string()))?
            .ok_or(ChannelError::MissingDescriptor)?;
        let bytes = IntoVec::into_vec(descriptor_stream)
            .await
            .map_err(|error| ChannelError::TufVerification(error.to_string()))?;
        let outcome = validate_descriptor(&bytes, &repository, host, previous, now)?;
        let channel = match &outcome {
            RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
        };
        let target = channel.descriptor().index().target();
        let bytes = read_required_index_target(&repository, target).await?;
        Ok(ChannelRefresh {
            outcome,
            index: AuthenticatedIndexTarget {
                system: host,
                bytes,
            },
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ChannelError, validate_descriptor};
    use pkg_core::{ChannelSequence, PolicyVersion};
    use tempfile::TempDir;
    use tough::Repository;

    const ROOT: &[u8] = include_bytes!("../../../fixtures/channel-v1/root.json");

    async fn fixture_repository(datastore: &PathBuf) -> Repository {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/channel-v1")
            .canonicalize()
            .unwrap();
        RepositoryLoader::new(
            &ROOT,
            Url::from_directory_path(fixture.join("metadata")).unwrap(),
            Url::from_directory_path(fixture.join("targets")).unwrap(),
        )
        .transport(DefaultTransport::default())
        .limits(LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(datastore)
        .load()
        .await
        .unwrap()
    }

    async fn descriptor_bytes(repository: &Repository) -> Vec<u8> {
        let name = TargetName::new(DESCRIPTOR_TARGET).unwrap();
        let stream = repository.read_target(&name).await.unwrap().unwrap();
        IntoVec::into_vec(stream).await.unwrap()
    }

    #[tokio::test]
    async fn host_index_is_returned_only_from_the_authenticated_repository_view() {
        let temp = TempDir::new().unwrap();
        let repository = fixture_repository(&temp.path().to_path_buf()).await;
        let descriptor = descriptor_bytes(&repository).await;
        let now = "2026-08-09T00:00:00Z".parse().unwrap();
        let RefreshOutcome::Updated(channel) =
            validate_descriptor(&descriptor, &repository, System::Aarch64Darwin, None, now)
                .unwrap()
        else {
            panic!("first acceptance must update");
        };
        let bytes = read_required_index_target(&repository, channel.descriptor().index().target())
            .await
            .unwrap();
        assert_eq!(bytes, b"[ fixture catalog index for aarch64-darwin ]\n");
        assert!(matches!(
            read_required_index_target(&repository, "index/42/missing.json.br").await,
            Err(ChannelError::MissingIndexTarget)
        ));
    }

    #[tokio::test]
    async fn refresh_bundle_keeps_descriptor_and_host_index_in_one_tuf_view() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/channel-v1")
            .canonicalize()
            .unwrap();
        let datastore = TempDir::new().unwrap();
        let lease = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(datastore.path().join("pkg-channel.lock"))
            .unwrap();
        let client = ChannelClient {
            trusted_root: TrustedRoot::from_embedded(ROOT).unwrap(),
            metadata_url: Url::from_directory_path(fixture.join("metadata")).unwrap(),
            targets_url: Url::from_directory_path(fixture.join("targets")).unwrap(),
            datastore: datastore.path().to_path_buf(),
            _datastore_lease: lease,
        };
        let refresh = client
            .refresh_with_index_and_transport(
                System::Aarch64Darwin,
                None,
                "2026-08-09T00:00:00Z".parse().unwrap(),
                DefaultTransport::default(),
            )
            .await
            .unwrap();
        assert!(matches!(refresh.outcome(), RefreshOutcome::Updated(_)));
        assert_eq!(refresh.index().system(), System::Aarch64Darwin);
        assert_eq!(
            refresh.index().bytes(),
            b"[ fixture catalog index for aarch64-darwin ]\n"
        );
    }

    #[tokio::test]
    async fn signed_fixture_loads_and_same_descriptor_is_unchanged() {
        let temp = TempDir::new().unwrap();
        let repository = fixture_repository(&temp.path().to_path_buf()).await;
        let bytes = descriptor_bytes(&repository).await;
        let now = "2026-08-09T00:00:00Z".parse().unwrap();
        let first =
            validate_descriptor(&bytes, &repository, System::Aarch64Darwin, None, now).unwrap();
        let RefreshOutcome::Updated(channel) = first else {
            panic!("first acceptance must be an update");
        };
        assert_eq!(channel.sequence().get().get(), 42);
        assert_eq!(channel.descriptor().nix_version(), "2.24.10");
        assert_eq!(
            channel.descriptor().cache().url(),
            "https://cache.nixos.org"
        );
        assert_eq!(channel.descriptor().cache().trusted_public_keys().len(), 1);
        assert_eq!(
            channel.descriptor().cache().trusted_public_keys()[0].name(),
            "cache.nixos.org-1"
        );
        assert!(
            channel
                .descriptor()
                .cache()
                .admits_signature_name("cache.nixos.org-1")
        );

        let state = channel.accepted_state();
        let second = validate_descriptor(
            &bytes,
            &repository,
            System::Aarch64Darwin,
            Some(&state),
            now,
        )
        .unwrap();
        assert!(matches!(second, RefreshOutcome::Unchanged(_)));

        let later = AcceptedChannel::new(
            ChannelSequence::from_u64(43).unwrap(),
            PolicyVersion::from_u64(1).unwrap(),
            [0; 32],
        );
        assert!(matches!(
            validate_descriptor(
                &bytes,
                &repository,
                System::Aarch64Darwin,
                Some(&later),
                now,
            ),
            Err(ChannelError::SequenceRollback)
        ));

        let reused = AcceptedChannel::new(
            ChannelSequence::from_u64(42).unwrap(),
            PolicyVersion::from_u64(1).unwrap(),
            [0; 32],
        );
        assert!(matches!(
            validate_descriptor(
                &bytes,
                &repository,
                System::Aarch64Darwin,
                Some(&reused),
                now,
            ),
            Err(ChannelError::SequenceReuse)
        ));
    }

    #[tokio::test]
    async fn semantic_tampering_fails_closed_after_tuf_authentication() {
        let temp = TempDir::new().unwrap();
        let repository = fixture_repository(&temp.path().to_path_buf()).await;
        let bytes = descriptor_bytes(&repository).await;
        let now = "2026-08-09T00:00:00Z".parse().unwrap();

        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["substituters"]["urls"] = serde_json::json!(["https://evil.invalid"]);
        let tampered = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(&tampered, &repository, System::X8664Linux, None, now),
            Err(ChannelError::InvalidSubstituters)
        ));

        value = serde_json::from_slice(&bytes).unwrap();
        value["unexpectedPolicy"] = serde_json::json!(true);
        let unknown = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(&unknown, &repository, System::X8664Linux, None, now),
            Err(ChannelError::InvalidDescriptorJson)
        ));

        value = serde_json::from_slice(&bytes).unwrap();
        value["index"]["perSystem"]["aarch64-darwin"]["sha256"] = serde_json::json!("0".repeat(64));
        let mismatched = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(&mismatched, &repository, System::X8664Linux, None, now),
            Err(ChannelError::TargetHashMismatch(_))
        ));

        value = serde_json::from_slice(&bytes).unwrap();
        value["nixRuntime"]["perSystem"]["x86_64-linux"]["assetManifestSha256"] =
            serde_json::json!("0".repeat(64));
        let mismatched_manifest = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(
                &mismatched_manifest,
                &repository,
                System::X8664Linux,
                None,
                now,
            ),
            Err(ChannelError::TargetHashMismatch(_))
        ));

        value = serde_json::from_slice(&bytes).unwrap();
        value["buildPolicy"]["nativeLocalBuilds"]["x86_64-linux"]["mode"] =
            serde_json::json!("deny");
        let emergency_deny = serde_json::to_vec(&value).unwrap();
        assert!(
            validate_descriptor(&emergency_deny, &repository, System::X8664Linux, None, now,)
                .is_ok()
        );

        value["buildPolicy"]["nativeLocalBuilds"]["x86_64-linux"]["mode"] =
            serde_json::json!("prompt");
        let unsafe_prompt = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(&unsafe_prompt, &repository, System::X8664Linux, None, now,),
            Err(ChannelError::InvalidBuildPolicy)
        ));

        value = serde_json::from_slice(&bytes).unwrap();
        value["expiresAt"] = serde_json::json!("2020-01-01T00:00:00Z");
        let expired = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            validate_descriptor(&expired, &repository, System::X8664Linux, None, now),
            Err(ChannelError::ExpiredDescriptor)
        ));
    }

    #[test]
    fn datastore_has_exactly_one_cross_process_writer() {
        let temp = TempDir::new().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            temp.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let root = TrustedRoot::from_embedded(ROOT).unwrap();
        let metadata = Url::parse("https://updates.example/metadata/").unwrap();
        let targets = Url::parse("https://updates.example/targets/").unwrap();
        let first =
            ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), temp.path())
                .unwrap();
        assert!(matches!(
            ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), temp.path()),
            Err(ChannelError::DatastoreBusy)
        ));
        drop(first);
        assert!(ChannelClient::new(root, metadata, targets, temp.path()).is_ok());
    }
}
