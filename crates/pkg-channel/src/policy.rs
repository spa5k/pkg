use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use jiff::Timestamp;
use pkg_core::{ChannelName, ChannelSequence, NarHash, NixpkgsRevision, PolicyVersion, System};
use sha2::{Digest as _, Sha256};
use tough::{Repository, TargetName};
use url::Url;

use crate::descriptor::{
    BuildMode, CachePolicy, CachePublicKey, ChannelDescriptor, IndexArtifact, NixRuntimeArtifact,
    NixpkgsPin, WireDescriptor,
};

pub(crate) const DESCRIPTOR_TARGET: &str = "descriptor.json";
pub(crate) const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;
pub(crate) const CACHE_URL: &str = "https://cache.nixos.org";

/// Persisted identity of the descriptor accepted by a previous refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedChannel {
    sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
}

impl AcceptedChannel {
    /// Reconstructs the accepted descriptor identity from authoritative state.
    #[must_use]
    pub const fn new(
        sequence: ChannelSequence,
        policy_version: PolicyVersion,
        descriptor_sha256: [u8; 32],
    ) -> Self {
        Self {
            sequence,
            policy_version,
            descriptor_sha256,
        }
    }

    /// Returns the last accepted channel sequence.
    #[must_use]
    pub const fn sequence(&self) -> ChannelSequence {
        self.sequence
    }

    /// Returns the last accepted policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the digest of the exact accepted descriptor bytes.
    #[must_use]
    pub const fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }
}

/// A channel descriptor after cryptographic and semantic verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChannel {
    sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
    descriptor: ChannelDescriptor,
}

impl VerifiedChannel {
    /// Returns the authenticated channel sequence.
    #[must_use]
    pub const fn sequence(&self) -> ChannelSequence {
        self.sequence
    }

    /// Returns the authenticated policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the SHA-256 digest of the exact authenticated descriptor bytes.
    #[must_use]
    pub const fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }

    /// Returns the semantically validated descriptor policy.
    #[must_use]
    pub const fn descriptor(&self) -> &ChannelDescriptor {
        &self.descriptor
    }

    /// Returns the compact identity to persist for the next refresh.
    #[must_use]
    pub const fn accepted_state(&self) -> AcceptedChannel {
        AcceptedChannel::new(self.sequence, self.policy_version, self.descriptor_sha256)
    }
}

/// Result of comparing a verified remote descriptor with persisted state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A strictly newer descriptor was authenticated and accepted.
    Updated(VerifiedChannel),
    /// The authenticated descriptor exactly matches persisted accepted state.
    Unchanged(VerifiedChannel),
}

/// Fail-closed channel loading and policy errors.
#[derive(Debug)]
#[non_exhaustive]
pub enum ChannelError {
    /// The compile-time root bytes are empty, oversized, or not JSON.
    InvalidTrustedRoot,
    /// A repository URL is not canonical HTTPS.
    InvalidRepositoryUrl,
    /// The persistent TUF datastore is absent, unsafe, or inaccessible.
    DatastoreUnavailable,
    /// Durable product channel identity is missing, unsafe, or corrupt.
    AcceptedStateUnavailable,
    /// Another process already holds the datastore writer lease.
    DatastoreBusy,
    /// `tough` rejected the authenticated metadata or target stream.
    TufVerification(String),
    /// The signed repository has no `descriptor.json` target.
    MissingDescriptor,
    /// The descriptor exceeds pkg's fixed byte limit.
    DescriptorTooLarge,
    /// The descriptor is malformed or contains unknown fields.
    InvalidDescriptorJson,
    /// The descriptor schema is not implemented by this client.
    UnsupportedSchema(u64),
    /// The descriptor policy is not implemented by this client.
    UnsupportedPolicy(u64),
    /// The descriptor sequence is zero.
    InvalidSequence,
    /// The descriptor sequence is older than accepted state.
    SequenceRollback,
    /// The same sequence identifies different descriptor bytes or policy.
    SequenceReuse,
    /// The descriptor policy version is older than accepted state.
    PolicyRollback,
    /// The product expiry is not valid RFC3339.
    InvalidExpiration,
    /// The product descriptor has expired.
    ExpiredDescriptor,
    /// The display channel name violates the domain contract.
    InvalidChannelName,
    /// The V1 supported-system set is incomplete, duplicated, or extended.
    InvalidSystems,
    /// The native host is absent from the channel.
    HostSystemMissing,
    /// The native build policy is incomplete or unsafe for V1.
    InvalidBuildPolicy,
    /// The managed Nix version is not canonical numeric dotted form.
    InvalidNixVersion,
    /// A managed-Nix URL or digest violates the V1 artifact contract.
    InvalidRuntimeArtifact,
    /// The Nixpkgs owner, repository, revision, or NAR hash is invalid.
    InvalidNixpkgsPin,
    /// An index target name, source, or digest is invalid.
    InvalidIndexArtifact,
    /// The authenticated host index target is absent.
    MissingIndexTarget,
    /// The authenticated host index target exceeds the product byte ceiling.
    IndexTargetTooLarge,
    /// The trusted consumer refused semantic promotion of the index target.
    IndexVerificationRefused,
    /// The substituter URL or signed cache key is outside V1 policy.
    InvalidSubstituters,
    /// A descriptor-referenced artifact is absent from authenticated TUF metadata.
    MissingTufTarget(String),
    /// A descriptor digest disagrees with authenticated TUF target metadata.
    TargetHashMismatch(String),
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTrustedRoot => f.write_str("embedded TUF root is invalid"),
            Self::InvalidRepositoryUrl => {
                f.write_str("repository URLs must be canonical HTTPS URLs")
            }
            Self::DatastoreUnavailable => f.write_str("persistent TUF datastore is unavailable"),
            Self::AcceptedStateUnavailable => {
                f.write_str("durable accepted channel state is unavailable")
            }
            Self::DatastoreBusy => f.write_str("another channel refresh owns the TUF datastore"),
            Self::TufVerification(message) => write!(f, "TUF verification failed: {message}"),
            Self::MissingDescriptor => f.write_str("authenticated descriptor.json is missing"),
            Self::DescriptorTooLarge => f.write_str("descriptor.json exceeds the product limit"),
            Self::InvalidDescriptorJson => f.write_str("descriptor.json has an invalid schema"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported descriptor schema {version}")
            }
            Self::UnsupportedPolicy(version) => write!(f, "unsupported channel policy {version}"),
            Self::InvalidSequence => f.write_str("channel sequence must be nonzero"),
            Self::SequenceRollback => f.write_str("channel sequence rollback refused"),
            Self::SequenceReuse => f.write_str("channel sequence was reused for different policy"),
            Self::PolicyRollback => f.write_str("channel policy downgrade refused"),
            Self::InvalidExpiration => f.write_str("descriptor expiration is not valid RFC3339"),
            Self::ExpiredDescriptor => f.write_str("descriptor has expired"),
            Self::InvalidChannelName => f.write_str("channel name is invalid"),
            Self::InvalidSystems => f.write_str("supported system set is invalid"),
            Self::HostSystemMissing => f.write_str("host system is absent from the channel"),
            Self::InvalidBuildPolicy => f.write_str("native build policy is unsafe or incomplete"),
            Self::InvalidNixVersion => f.write_str("managed Nix version is invalid"),
            Self::InvalidRuntimeArtifact => f.write_str("managed Nix artifact is invalid"),
            Self::InvalidNixpkgsPin => f.write_str("Nixpkgs source pin is invalid"),
            Self::InvalidIndexArtifact => f.write_str("package index artifact is invalid"),
            Self::MissingIndexTarget => f.write_str("authenticated package index is missing"),
            Self::IndexTargetTooLarge => {
                f.write_str("authenticated package index exceeds the product limit")
            }
            Self::IndexVerificationRefused => {
                f.write_str("authenticated package index promotion was refused")
            }
            Self::InvalidSubstituters => f.write_str("substituter policy is not the V1 allowlist"),
            Self::MissingTufTarget(name) => write!(f, "TUF target `{name}` is missing"),
            Self::TargetHashMismatch(name) => {
                write!(f, "descriptor hash does not match TUF target `{name}`")
            }
        }
    }
}

impl std::error::Error for ChannelError {}

pub(crate) fn validate_datastore(path: &Path) -> Result<(), ChannelError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| ChannelError::DatastoreUnavailable)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ChannelError::DatastoreUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(ChannelError::DatastoreUnavailable);
        }
    }
    Ok(())
}

pub(crate) fn validate_repository_url(url: &Url) -> Result<(), ChannelError> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ChannelError::InvalidRepositoryUrl);
    }
    Ok(())
}

pub(crate) fn validate_descriptor(
    bytes: &[u8],
    repository: &Repository,
    host: System,
    previous: Option<&AcceptedChannel>,
    now: Timestamp,
) -> Result<RefreshOutcome, ChannelError> {
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(ChannelError::DescriptorTooLarge);
    }
    let wire: WireDescriptor =
        serde_json::from_slice(bytes).map_err(|_| ChannelError::InvalidDescriptorJson)?;
    if wire.schema_version != 1 {
        return Err(ChannelError::UnsupportedSchema(wire.schema_version));
    }
    if wire.policy_version != 1 {
        return Err(ChannelError::UnsupportedPolicy(wire.policy_version));
    }
    let sequence = ChannelSequence::from_u64(wire.sequence).ok_or(ChannelError::InvalidSequence)?;
    let policy_version = PolicyVersion::from_u64(wire.policy_version)
        .ok_or(ChannelError::UnsupportedPolicy(wire.policy_version))?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let unchanged = compare_previous(previous, sequence, policy_version, digest)?;

    let expires_at = wire
        .expires_at
        .parse::<Timestamp>()
        .map_err(|_| ChannelError::InvalidExpiration)?;
    if expires_at <= now {
        return Err(ChannelError::ExpiredDescriptor);
    }
    let channel = ChannelName::new(&wire.channel).map_err(|_| ChannelError::InvalidChannelName)?;

    let expected_systems: BTreeSet<&str> = System::ALL.map(System::as_str).into_iter().collect();
    let actual_systems: BTreeSet<&str> =
        wire.supported_systems.iter().map(String::as_str).collect();
    if wire.supported_systems.len() != expected_systems.len() || actual_systems != expected_systems
    {
        return Err(ChannelError::InvalidSystems);
    }
    if !actual_systems.contains(host.as_str()) {
        return Err(ChannelError::HostSystemMissing);
    }
    validate_map_keys(&wire.build_policy.native_local_builds, &expected_systems)
        .map_err(|()| ChannelError::InvalidBuildPolicy)?;
    if wire
        .build_policy
        .native_local_builds
        .values()
        .any(|entry| entry.mode == BuildMode::Prompt)
    {
        return Err(ChannelError::InvalidBuildPolicy);
    }

    if !valid_version(&wire.nix_runtime.version) {
        return Err(ChannelError::InvalidNixVersion);
    }
    validate_map_keys(&wire.nix_runtime.per_system, &expected_systems)
        .map_err(|()| ChannelError::InvalidRuntimeArtifact)?;
    for system in System::ALL {
        let artifact = &wire.nix_runtime.per_system[system.as_str()];
        validate_runtime(
            artifact.url.as_str(),
            &artifact.sha256,
            &wire.nix_runtime.version,
            system,
        )?;
        let target = format!("nix/{}/{system}.tar.xz", wire.nix_runtime.version);
        verify_target_hash(repository, &target, &artifact.sha256)?;
        let manifest_target = format!("nix/{}/{system}.assets.json", wire.nix_runtime.version);
        if artifact.asset_manifest_target != manifest_target
            || !valid_sha256(&artifact.asset_manifest_sha256)
        {
            return Err(ChannelError::InvalidRuntimeArtifact);
        }
        verify_target_hash(
            repository,
            &manifest_target,
            &artifact.asset_manifest_sha256,
        )?;
    }
    let runtime = &wire.nix_runtime.per_system[host.as_str()];

    let (nixpkgs_revision, nixpkgs_nar_hash) = validate_nixpkgs(
        &wire.nixpkgs.owner,
        &wire.nixpkgs.repo,
        &wire.nixpkgs.rev,
        &wire.nixpkgs.nar_hash,
    )?;

    if !matches!(
        wire.index.source.as_str(),
        "self-built" | "upstream-packages-json-br"
    ) {
        return Err(ChannelError::InvalidIndexArtifact);
    }
    validate_map_keys(&wire.index.per_system, &expected_systems)
        .map_err(|()| ChannelError::InvalidIndexArtifact)?;
    for system in System::ALL {
        let artifact = &wire.index.per_system[system.as_str()];
        let expected_prefix = format!("index/{}/{system}.json", wire.sequence);
        if !(artifact.target == expected_prefix
            || artifact.target == format!("{expected_prefix}.br"))
            || !valid_sha256(&artifact.sha256)
        {
            return Err(ChannelError::InvalidIndexArtifact);
        }
        verify_target_hash(repository, &artifact.target, &artifact.sha256)?;
    }
    let index = &wire.index.per_system[host.as_str()];

    if wire.substituters.urls != [CACHE_URL]
        || wire.substituters.trusted_public_keys.len() != 1
        || !valid_cache_key(&wire.substituters.trusted_public_keys[0])
    {
        return Err(ChannelError::InvalidSubstituters);
    }

    let verified = VerifiedChannel {
        sequence,
        policy_version,
        descriptor_sha256: digest,
        descriptor: ChannelDescriptor {
            channel,
            expires_at,
            nix_version: wire.nix_runtime.version.clone(),
            runtime: NixRuntimeArtifact {
                url: runtime.url.clone(),
                sha256: runtime.sha256.clone(),
                target: format!("nix/{}/{}.tar.xz", wire.nix_runtime.version, host),
                asset_manifest_target: runtime.asset_manifest_target.clone(),
                asset_manifest_sha256: runtime.asset_manifest_sha256.clone(),
            },
            nixpkgs: NixpkgsPin {
                owner: wire.nixpkgs.owner,
                repo: wire.nixpkgs.repo,
                rev: nixpkgs_revision,
                nar_hash: nixpkgs_nar_hash,
            },
            index: IndexArtifact {
                target: index.target.clone(),
                sha256: index.sha256.clone(),
            },
            cache: CachePolicy {
                url: wire.substituters.urls[0].clone(),
                trusted_public_keys: wire
                    .substituters
                    .trusted_public_keys
                    .iter()
                    .map(|value| CachePublicKey::from_validated(value))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(ChannelError::InvalidSubstituters)?,
            },
            build_mode: wire.build_policy.native_local_builds[host.as_str()].mode,
        },
    };
    Ok(if unchanged {
        RefreshOutcome::Unchanged(verified)
    } else {
        RefreshOutcome::Updated(verified)
    })
}

fn compare_previous(
    previous: Option<&AcceptedChannel>,
    sequence: ChannelSequence,
    policy_version: PolicyVersion,
    digest: [u8; 32],
) -> Result<bool, ChannelError> {
    let Some(previous) = previous else {
        return Ok(false);
    };
    if sequence.get() < previous.sequence.get() {
        return Err(ChannelError::SequenceRollback);
    }
    if policy_version.get() < previous.policy_version.get() {
        return Err(ChannelError::PolicyRollback);
    }
    if sequence == previous.sequence {
        if policy_version != previous.policy_version || digest != previous.descriptor_sha256 {
            return Err(ChannelError::SequenceReuse);
        }
        return Ok(true);
    }
    Ok(false)
}

fn validate_map_keys<T>(map: &BTreeMap<String, T>, expected: &BTreeSet<&str>) -> Result<(), ()> {
    let actual: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    if map.len() == expected.len() && actual == *expected {
        Ok(())
    } else {
        Err(())
    }
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.split('.').count() >= 2
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_cache_key(value: &str) -> bool {
    let Some(encoded) = value.strip_prefix("cache.nixos.org-1:") else {
        return false;
    };
    encoded.len() == 44
        && encoded.ends_with('=')
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

fn validate_runtime(
    raw_url: &str,
    sha256: &str,
    version: &str,
    system: System,
) -> Result<(), ChannelError> {
    let url = Url::parse(raw_url).map_err(|_| ChannelError::InvalidRuntimeArtifact)?;
    let expected_path = format!("/nix/nix-{version}/nix-{version}-{system}.tar.xz");
    if url.scheme() != "https"
        || url.host_str() != Some("releases.nixos.org")
        || url.port().is_some()
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !valid_sha256(sha256)
    {
        return Err(ChannelError::InvalidRuntimeArtifact);
    }
    Ok(())
}

fn validate_nixpkgs(
    owner: &str,
    repo: &str,
    rev: &str,
    nar_hash: &str,
) -> Result<(NixpkgsRevision, NarHash), ChannelError> {
    if owner != "NixOS" || repo != "nixpkgs" {
        return Err(ChannelError::InvalidNixpkgsPin);
    }
    let revision = NixpkgsRevision::new(rev).map_err(|_| ChannelError::InvalidNixpkgsPin)?;
    let nar_hash = NarHash::new(nar_hash).map_err(|_| ChannelError::InvalidNixpkgsPin)?;
    Ok((revision, nar_hash))
}

fn verify_target_hash(
    repository: &Repository,
    name: &str,
    expected_sha256: &str,
) -> Result<(), ChannelError> {
    let target_name =
        TargetName::new(name).map_err(|_| ChannelError::MissingTufTarget(name.into()))?;
    let target = repository
        .all_targets()
        .find_map(|(candidate, target)| (candidate == &target_name).then_some(target))
        .ok_or_else(|| ChannelError::MissingTufTarget(name.into()))?;
    if hex::encode(target.hashes.sha256.as_ref()) != expected_sha256 {
        return Err(ChannelError::TargetHashMismatch(name.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_urls_require_canonical_https() {
        for raw in [
            "http://updates.example/metadata/",
            "https://user@updates.example/metadata/",
            "https://updates.example/metadata/?mirror=evil",
            "file:///tmp/repo/",
        ] {
            assert!(validate_repository_url(&Url::parse(raw).unwrap()).is_err());
        }
        assert!(
            validate_repository_url(&Url::parse("https://updates.example/metadata/").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn versions_and_hashes_are_canonical() {
        assert!(valid_version("2.34.8"));
        assert!(!valid_version("2.34.x"));
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(valid_cache_key(
            "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
        ));
        assert!(!valid_cache_key("evil.example-1:abcd"));
    }
}
