//! Tests for the `tuf` module.

use super::*;
use crate::policy::{AcceptedChannel, ChannelError, validate_descriptor};
use pkg_core::{ChannelSequence, PolicyVersion};
use tempfile::TempDir;
use tough::Repository;

const ROOT: &[u8] = include_bytes!("../../../../fixtures/channel-v1/root.json");

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

/// Test-only frozen clock. `pkg-channel` sits below `pkg-testkit` in the
/// dependency order, so the double is defined locally instead of shared.
struct FrozenClock(std::sync::Mutex<Timestamp>);

impl FrozenClock {
    fn frozen_at(instant: Timestamp) -> Self {
        Self(std::sync::Mutex::new(instant))
    }

    fn freeze_at(&self, instant: Timestamp) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = instant;
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> Timestamp {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[tokio::test]
async fn host_index_is_returned_only_from_the_authenticated_repository_view() {
    let temp = TempDir::new().unwrap();
    let repository = fixture_repository(&temp.path().to_path_buf()).await;
    let descriptor = descriptor_bytes(&repository).await;
    let now = "2026-08-09T00:00:00Z".parse().unwrap();
    let RefreshOutcome::Updated(channel) =
        validate_descriptor(&descriptor, &repository, System::Aarch64Darwin, None, now).unwrap()
    else {
        panic!("first acceptance must update");
    };
    let bytes = read_required_index_target(&repository, channel.descriptor().index().target())
        .await
        .unwrap();
    assert!(bytes.len() > 32);
    assert!(!bytes.starts_with(b"[ fixture catalog index"));
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
        accepted: AcceptedChannelStore::new(datastore.path()),
        refresh_lease: tokio::sync::Mutex::new(()),
        clock: std::sync::Arc::new(FrozenClock::frozen_at(
            "2026-08-09T00:00:00Z".parse().unwrap(),
        )),
        _datastore_lease: lease,
    };
    client.accepted.initialize(None).unwrap();
    assert!(matches!(
        client
            .refresh_with_index_and_transport(
                System::Aarch64Darwin,
                "2026-08-09T00:00:00Z".parse().unwrap(),
                DefaultTransport::default(),
                true,
                |_, _| Err::<AuthenticatedIndexTarget, ()>(()),
            )
            .await,
        Err(ChannelError::IndexVerificationRefused)
    ));
    assert_eq!(client.accepted.load().unwrap(), None);
    let checked = client
        .refresh_with_index_and_transport(
            System::Aarch64Darwin,
            "2026-08-09T00:00:00Z".parse().unwrap(),
            DefaultTransport::default(),
            false,
            |_, target| Ok(target.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(checked.outcome(), RefreshOutcome::Updated(_)));
    assert_eq!(client.accepted.load().unwrap(), None);
    let refresh = client
        .refresh_with_index_and_transport(
            System::Aarch64Darwin,
            "2026-08-09T00:00:00Z".parse().unwrap(),
            DefaultTransport::default(),
            true,
            |_, target| Ok(target.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(refresh.outcome(), RefreshOutcome::Updated(_)));
    assert_eq!(refresh.index().system(), System::Aarch64Darwin);
    assert!(refresh.index().bytes().len() > 32);
    assert!(
        !refresh
            .index()
            .bytes()
            .starts_with(b"[ fixture catalog index")
    );
    assert_eq!(
        client.accepted.load().unwrap(),
        Some(match refresh.outcome() {
            RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => {
                channel.accepted_state()
            }
        })
    );
    client
        .accepted
        .persist(&AcceptedChannel::new(
            ChannelSequence::from_u64(43).unwrap(),
            PolicyVersion::from_u64(1).unwrap(),
            [0x43; 32],
        ))
        .unwrap();
    assert!(matches!(
        client
            .refresh_with_index_and_transport(
                System::Aarch64Darwin,
                "2026-08-09T00:00:00Z".parse().unwrap(),
                DefaultTransport::default(),
                true,
                |_, target| Ok(target.clone()),
            )
            .await,
        Err(ChannelError::SequenceRollback)
    ));
}

/// The fixture descriptor expires at `2036-04-01T00:00:00Z`.
#[tokio::test]
async fn descriptor_freshness_refuses_the_exact_expiry_instant() {
    let temp = TempDir::new().unwrap();
    let repository = fixture_repository(&temp.path().to_path_buf()).await;
    let bytes = descriptor_bytes(&repository).await;

    // One instant before expiry the descriptor is still fresh.
    let fresh = validate_descriptor(
        &bytes,
        &repository,
        System::Aarch64Darwin,
        None,
        "2036-03-31T23:59:59Z".parse().unwrap(),
    )
    .unwrap();
    assert!(matches!(fresh, RefreshOutcome::Updated(_)));

    // At the exact expiry instant the descriptor is expired, so a
    // freeze attack cannot replay a stale descriptor across the line.
    assert!(matches!(
        validate_descriptor(
            &bytes,
            &repository,
            System::Aarch64Darwin,
            None,
            "2036-04-01T00:00:00Z".parse().unwrap(),
        ),
        Err(ChannelError::ExpiredDescriptor)
    ));
}

#[tokio::test]
async fn refresh_reads_time_only_through_the_injected_clock() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/channel-v1")
        .canonicalize()
        .unwrap();
    let temp = TempDir::new().unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        temp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let clock = std::sync::Arc::new(FrozenClock::frozen_at(
        "2026-08-09T00:00:00Z".parse().unwrap(),
    ));
    // Built through the struct literal because the production constructor
    // admits only pinned HTTPS origins; the fixture repository is local.
    let lease = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(temp.path().join("pkg-channel.lock"))
        .unwrap();
    let client = ChannelClient {
        trusted_root: TrustedRoot::from_embedded(ROOT).unwrap(),
        metadata_url: Url::from_directory_path(fixture.join("metadata")).unwrap(),
        targets_url: Url::from_directory_path(fixture.join("targets")).unwrap(),
        datastore: temp.path().to_path_buf(),
        accepted: AcceptedChannelStore::new(temp.path()),
        refresh_lease: tokio::sync::Mutex::new(()),
        clock: clock.clone(),
        _datastore_lease: lease,
    };
    client.accepted.initialize(None).unwrap();

    let first = client.refresh(System::Aarch64Darwin).await.unwrap();
    assert!(matches!(first, RefreshOutcome::Updated(_)));
    let second = client.refresh(System::Aarch64Darwin).await.unwrap();
    assert!(matches!(second, RefreshOutcome::Unchanged(_)));

    // Frozen at the exact expiry instant, the same signed descriptor is
    // refused and the accepted identity survives the refused refresh.
    clock.freeze_at("2036-04-01T00:00:00Z".parse().unwrap());
    assert!(matches!(
        client.refresh(System::Aarch64Darwin).await,
        Err(ChannelError::ExpiredDescriptor)
    ));
    assert!(client.accepted.load().unwrap().is_some());
}

#[tokio::test]
async fn signed_fixture_loads_and_same_descriptor_is_unchanged() {
    let temp = TempDir::new().unwrap();
    let repository = fixture_repository(&temp.path().to_path_buf()).await;
    let bytes = descriptor_bytes(&repository).await;
    let now = "2026-08-09T00:00:00Z".parse().unwrap();
    let first = validate_descriptor(&bytes, &repository, System::Aarch64Darwin, None, now).unwrap();
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
    value["buildPolicy"]["nativeLocalBuilds"]["x86_64-linux"]["mode"] = serde_json::json!("deny");
    let emergency_deny = serde_json::to_vec(&value).unwrap();
    assert!(
        validate_descriptor(&emergency_deny, &repository, System::X8664Linux, None, now,).is_ok()
    );

    value["buildPolicy"]["nativeLocalBuilds"]["x86_64-linux"]["mode"] = serde_json::json!("prompt");
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
        ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), temp.path()).unwrap();
    assert!(matches!(
        ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), temp.path()),
        Err(ChannelError::DatastoreBusy)
    ));
    drop(first);
    assert!(ChannelClient::new(root, metadata, targets, temp.path()).is_ok());
}

#[test]
fn established_tuf_datastore_without_rollback_memory_is_not_first_run() {
    let temp = TempDir::new().unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        temp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(temp.path().join("root.json"), b"legacy tuf state").unwrap();
    std::fs::write(temp.path().join(LOCK_FILE), b"").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(
        temp.path().join(LOCK_FILE),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let root = TrustedRoot::from_embedded(ROOT).unwrap();
    let metadata = Url::parse("https://updates.example/metadata/").unwrap();
    let targets = Url::parse("https://updates.example/targets/").unwrap();
    assert!(matches!(
        ChannelClient::new(root.clone(), metadata.clone(), targets.clone(), temp.path()),
        Err(ChannelError::AcceptedStateUnavailable)
    ));
    let legacy = AcceptedChannel::new(
        ChannelSequence::from_u64(41).unwrap(),
        PolicyVersion::from_u64(1).unwrap(),
        [0x41; 32],
    );
    let migrated =
        ChannelClient::migrate_legacy(root, metadata, targets, temp.path(), &legacy).unwrap();
    assert_eq!(migrated.accepted.load().unwrap(), Some(legacy));
    #[cfg(unix)]
    assert_eq!(
        migrated
            ._datastore_lease
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
