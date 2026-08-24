//! Verified acquisition of the signed-channel-pinned Nixpkgs source.
//!
//! Nix performs the normalized flake fetch. This module owns the closed
//! command grammar and independently promotes the pinned Nix 2.34.8 metadata
//! JSON into a source identity that downstream index/evaluation code may use.

use std::fmt;

use pkg_channel::VerifiedChannel;
use pkg_core::{ChannelSequence, NarHash, NixpkgsRevision, PolicyVersion, StorePath};
use serde::Deserialize;

const MAX_METADATA_BYTES: usize = 1024 * 1024;
const GITHUB_TYPE: &str = "github";
const NIXPKGS_OWNER: &str = "NixOS";
const NIXPKGS_REPO: &str = "nixpkgs";

/// Stable Nixpkgs acquisition failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NixpkgsSourceErrorCode {
    /// The verified channel did not contain the fixed V1 Nixpkgs pin.
    InvalidVerifiedPin,
    /// The closed metadata runner could not execute the fixed request.
    RunnerFailure,
    /// Nix returned more metadata than the product-owned limit.
    MetadataTooLarge,
    /// Nix metadata was malformed or omitted a required top-level field.
    MalformedMetadata,
    /// Returned source identity differed from the authenticated descriptor.
    IdentityMismatch,
    /// The returned source path was not a normal typed Nix store path.
    InvalidSourcePath,
}

/// Closed, redacted Nixpkgs source-acquisition error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NixpkgsSourceError {
    code: NixpkgsSourceErrorCode,
}

impl NixpkgsSourceError {
    const fn new(code: NixpkgsSourceErrorCode) -> Self {
        Self { code }
    }

    /// Constructs the only error an execution adapter may originate.
    #[must_use]
    pub const fn runner_failure() -> Self {
        Self::new(NixpkgsSourceErrorCode::RunnerFailure)
    }

    /// Returns the stable public failure category.
    #[must_use]
    pub const fn code(self) -> NixpkgsSourceErrorCode {
        self.code
    }
}

impl fmt::Display for NixpkgsSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "pinned Nixpkgs source acquisition refused: {:?}",
            self.code
        )
    }
}

impl std::error::Error for NixpkgsSourceError {}

/// Narrow immutable input promoted from an authenticated channel descriptor.
#[derive(Clone, PartialEq, Eq)]
pub struct NixpkgsFetchSpec {
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
    pin: NixpkgsPin,
}

impl fmt::Debug for NixpkgsFetchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NixpkgsFetchSpec")
            .field("channel_sequence", &self.channel_sequence)
            .field("policy_version", &self.policy_version)
            .field("source_identity", &"<authenticated-pin>")
            .finish()
    }
}

impl NixpkgsFetchSpec {
    /// Promotes the exact authenticated channel pin into the acquisition API.
    pub fn from_verified_channel(channel: &VerifiedChannel) -> Result<Self, NixpkgsSourceError> {
        let pin = channel.descriptor().nixpkgs();
        Self::from_parts(
            channel.sequence(),
            channel.policy_version(),
            channel.descriptor_sha256(),
            pin.owner(),
            pin.repo(),
            pin.revision(),
            pin.nar_hash(),
        )
    }

    fn from_parts(
        channel_sequence: ChannelSequence,
        policy_version: PolicyVersion,
        descriptor_sha256: [u8; 32],
        owner: &str,
        repo: &str,
        revision: &str,
        nar_hash: &str,
    ) -> Result<Self, NixpkgsSourceError> {
        if owner != NIXPKGS_OWNER || repo != NIXPKGS_REPO {
            return Err(NixpkgsSourceError::new(
                NixpkgsSourceErrorCode::InvalidVerifiedPin,
            ));
        }
        Ok(Self {
            channel_sequence,
            policy_version,
            descriptor_sha256,
            pin: NixpkgsPin::new(revision, nar_hash)?,
        })
    }

    /// Returns the authenticated channel sequence owning this pin.
    #[must_use]
    pub const fn channel_sequence(&self) -> ChannelSequence {
        self.channel_sequence
    }

    /// Returns the authenticated policy version owning this pin.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the exact authenticated descriptor digest.
    #[must_use]
    pub const fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }

    /// Returns the canonical pinned revision.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        self.pin.revision()
    }

    /// Returns the pinned normalized NAR identity.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        self.pin.nar_hash()
    }

    /// Returns the exact authenticated source pin.
    #[must_use]
    pub const fn pin(&self) -> &NixpkgsPin {
        &self.pin
    }
}

/// Exact Nixpkgs source identity selected by the release authority.
#[derive(Clone, PartialEq, Eq)]
pub struct NixpkgsPin {
    revision: NixpkgsRevision,
    nar_hash: NarHash,
}

impl fmt::Debug for NixpkgsPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NixpkgsPin(<exact-source-identity>)")
    }
}

impl NixpkgsPin {
    /// Validates an exact release-side Nixpkgs source identity.
    pub fn new(revision: &str, nar_hash: &str) -> Result<Self, NixpkgsSourceError> {
        Ok(Self {
            revision: NixpkgsRevision::new(revision)
                .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::InvalidVerifiedPin))?,
            nar_hash: NarHash::new(nar_hash)
                .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::InvalidVerifiedPin))?,
        })
    }

    /// Returns the exact revision.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        &self.revision
    }

    /// Returns the normalized NAR identity.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }
}

/// Closed execution seam implemented by FakeNix today and the contained
/// bundled-Nix subprocess adapter in the Real-Nix lane.
pub trait NixpkgsMetadataRunner: Send + Sync {
    /// Executes the one fixed metadata command reconstructed from this pin.
    fn run_metadata(&self, pin: &NixpkgsPin) -> Result<Vec<u8>, NixpkgsSourceError>;
}

/// A Nix-materialized source whose identity matched an exact release pin.
#[derive(Clone, PartialEq, Eq)]
pub struct PinnedNixpkgsSource {
    revision: NixpkgsRevision,
    nar_hash: NarHash,
    store_path: StorePath,
}

impl fmt::Debug for PinnedNixpkgsSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PinnedNixpkgsSource(<verified-private-source>)")
    }
}

impl PinnedNixpkgsSource {
    /// Returns the exact pinned revision.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        &self.revision
    }

    /// Returns the normalized source NAR identity.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }

    /// Returns the private materialized source path to fixed internal adapters.
    pub(crate) const fn private_store_path(&self) -> &StorePath {
        &self.store_path
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        revision: &str,
        nar_hash: &str,
        store_path: &str,
    ) -> Result<Self, NixpkgsSourceError> {
        let pin = NixpkgsPin::new(revision, nar_hash)?;
        Ok(Self {
            revision: pin.revision,
            nar_hash: pin.nar_hash,
            store_path: StorePath::new(store_path)
                .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::InvalidSourcePath))?,
        })
    }
}

/// A Nix-materialized source whose identity matched the authenticated pin.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedNixpkgsSource {
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
    source: PinnedNixpkgsSource,
}

impl fmt::Debug for VerifiedNixpkgsSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedNixpkgsSource")
            .field("channel_sequence", &self.channel_sequence)
            .field("policy_version", &self.policy_version)
            .field("source_identity", &"<verified>")
            .field("store_path", &"<private>")
            .finish()
    }
}

impl VerifiedNixpkgsSource {
    /// Returns the authenticated channel sequence that selected the source.
    #[must_use]
    pub const fn channel_sequence(&self) -> ChannelSequence {
        self.channel_sequence
    }

    /// Returns the authenticated policy version that selected the source.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the digest of the exact authenticated descriptor bytes.
    #[must_use]
    pub const fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }

    /// Returns the exact pinned Nixpkgs revision.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        self.source.revision()
    }

    /// Returns the normalized source NAR identity.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        self.source.nar_hash()
    }

    /// Returns the private Nix-materialized source path for trusted internal
    /// index/evaluation adapters. Public output and logs must never render it.
    #[must_use]
    pub const fn private_store_path(&self) -> &StorePath {
        self.source.private_store_path()
    }

    /// Returns the non-sensitive revision key used by the machine-global
    /// `/var/lib/pkg/broker-home/nixpkgs/<rev>/` marker directory.
    #[must_use]
    pub fn marker_key(&self) -> &str {
        self.revision().as_str()
    }
}

/// Materializes and independently verifies an exact release-side source pin.
pub fn fetch_pinned_nixpkgs(
    pin: &NixpkgsPin,
    runner: &dyn NixpkgsMetadataRunner,
) -> Result<PinnedNixpkgsSource, NixpkgsSourceError> {
    let metadata = runner.run_metadata(pin)?;
    verify_metadata(pin, &metadata)
}

/// Materializes and independently verifies the authenticated pinned source.
pub fn fetch_verified_nixpkgs(
    spec: &NixpkgsFetchSpec,
    runner: &dyn NixpkgsMetadataRunner,
) -> Result<VerifiedNixpkgsSource, NixpkgsSourceError> {
    let source = fetch_pinned_nixpkgs(&spec.pin, runner)?;
    Ok(VerifiedNixpkgsSource {
        channel_sequence: spec.channel_sequence,
        policy_version: spec.policy_version,
        descriptor_sha256: spec.descriptor_sha256,
        source,
    })
}

fn verify_metadata(
    pin: &NixpkgsPin,
    metadata: &[u8],
) -> Result<PinnedNixpkgsSource, NixpkgsSourceError> {
    if metadata.is_empty() {
        return Err(NixpkgsSourceError::new(
            NixpkgsSourceErrorCode::MalformedMetadata,
        ));
    }
    if metadata.len() > MAX_METADATA_BYTES {
        return Err(NixpkgsSourceError::new(
            NixpkgsSourceErrorCode::MetadataTooLarge,
        ));
    }
    let wire: MetadataWire = serde_json::from_slice(metadata)
        .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::MalformedMetadata))?;
    let locked_revision = NixpkgsRevision::new(wire.locked.revision())
        .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::MalformedMetadata))?;
    let locked_nar_hash = NarHash::new(&wire.locked.nar_hash)
        .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::MalformedMetadata))?;

    if wire.locked.kind != GITHUB_TYPE
        || wire.locked.owner != NIXPKGS_OWNER
        || wire.locked.repo != NIXPKGS_REPO
        || &locked_revision != pin.revision()
        || &locked_nar_hash != pin.nar_hash()
    {
        return Err(NixpkgsSourceError::new(
            NixpkgsSourceErrorCode::IdentityMismatch,
        ));
    }
    if let Some(revision) = wire.revision {
        let revision = NixpkgsRevision::new(&revision)
            .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::MalformedMetadata))?;
        if revision != locked_revision {
            return Err(NixpkgsSourceError::new(
                NixpkgsSourceErrorCode::IdentityMismatch,
            ));
        }
    }
    let store_path = StorePath::new(&wire.path)
        .map_err(|_| NixpkgsSourceError::new(NixpkgsSourceErrorCode::InvalidSourcePath))?;
    if store_path.as_str().ends_with(".drv") {
        return Err(NixpkgsSourceError::new(
            NixpkgsSourceErrorCode::InvalidSourcePath,
        ));
    }

    Ok(PinnedNixpkgsSource {
        revision: locked_revision,
        nar_hash: locked_nar_hash,
        store_path,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataWire {
    locked: LockedWire,
    path: String,
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockedWire {
    #[serde(rename = "type")]
    kind: String,
    owner: String,
    repo: String,
    rev: String,
    nar_hash: String,
}

impl LockedWire {
    fn revision(&self) -> &str {
        &self.rev
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Mutex;

    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const STORE_PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";

    fn spec() -> NixpkgsFetchSpec {
        NixpkgsFetchSpec::from_parts(
            ChannelSequence::new(NonZeroU64::new(7).unwrap()),
            PolicyVersion::new(NonZeroU64::new(3).unwrap()),
            [0x42; 32],
            NIXPKGS_OWNER,
            NIXPKGS_REPO,
            REVISION,
            NAR_HASH,
        )
        .unwrap()
    }

    fn metadata(revision: &str, nar_hash: &str) -> Vec<u8> {
        format!(
            r#"{{"locked":{{"type":"github","owner":"NixOS","repo":"nixpkgs","rev":"{revision}","narHash":"{nar_hash}","lastModified":1}},"path":"{STORE_PATH}","revision":"{revision}","locks":{{"version":7,"root":"root","nodes":{{"root":{{"locked":{{"rev":"ignored"}}}}}}}}}}"#
        )
        .into_bytes()
    }

    fn replace_ascii(input: Vec<u8>, from: &str, to: &str) -> Vec<u8> {
        String::from_utf8(input)
            .unwrap()
            .replace(from, to)
            .into_bytes()
    }

    struct ExactRunner {
        expected: NixpkgsPin,
        response: Mutex<Option<Result<Vec<u8>, NixpkgsSourceError>>>,
    }

    impl ExactRunner {
        fn new(expected: NixpkgsPin, response: Result<Vec<u8>, NixpkgsSourceError>) -> Self {
            Self {
                expected,
                response: Mutex::new(Some(response)),
            }
        }
    }

    impl NixpkgsMetadataRunner for ExactRunner {
        fn run_metadata(&self, pin: &NixpkgsPin) -> Result<Vec<u8>, NixpkgsSourceError> {
            if pin != &self.expected {
                return Err(NixpkgsSourceError::runner_failure());
            }
            self.response
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .unwrap_or_else(|| Err(NixpkgsSourceError::runner_failure()))
        }
    }

    #[test]
    fn fetch_spec_exposes_only_the_authenticated_pin() {
        let spec = spec();
        assert_eq!(spec.pin().revision().as_str(), REVISION);
        assert_eq!(spec.pin().nar_hash().as_str(), NAR_HASH);
    }

    #[test]
    fn top_level_locked_identity_promotes_a_private_source() {
        let spec = spec();
        let runner = ExactRunner::new(spec.pin().clone(), Ok(metadata(REVISION, NAR_HASH)));
        let source = fetch_verified_nixpkgs(&spec, &runner).unwrap();
        assert_eq!(source.revision().as_str(), REVISION);
        assert_eq!(source.nar_hash().as_str(), NAR_HASH);
        assert_eq!(source.private_store_path().as_str(), STORE_PATH);
        assert_eq!(source.marker_key(), REVISION);
        let debug = format!("{source:?}");
        assert!(!debug.contains(STORE_PATH));
        assert!(!debug.contains(NAR_HASH));
    }

    #[test]
    fn release_pin_promotes_the_same_verified_private_source() {
        let pin = NixpkgsPin::new(REVISION, NAR_HASH).unwrap();
        let runner = ExactRunner::new(pin.clone(), Ok(metadata(REVISION, NAR_HASH)));

        let source = fetch_pinned_nixpkgs(&pin, &runner).unwrap();

        assert_eq!(source.revision().as_str(), REVISION);
        assert_eq!(source.nar_hash().as_str(), NAR_HASH);
        let debug = format!("{source:?}");
        assert!(!debug.contains(STORE_PATH));
        assert!(!debug.contains(NAR_HASH));
    }

    #[test]
    fn revision_nar_hash_and_top_level_revision_mismatches_fail_closed() {
        let spec = spec();
        let other_revision = "1123456789abcdef0123456789abcdef01234567";
        let top_level_only = replace_ascii(
            metadata(REVISION, NAR_HASH),
            &format!(r#""revision":"{REVISION}""#),
            &format!(r#""revision":"{other_revision}""#),
        );
        for response in [
            metadata(other_revision, NAR_HASH),
            metadata(
                REVISION,
                "sha256-BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ),
            top_level_only,
        ] {
            let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
            assert_eq!(
                fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
                NixpkgsSourceErrorCode::IdentityMismatch
            );
        }
    }

    #[test]
    fn source_kind_owner_and_repo_mismatches_fail_closed() {
        let spec = spec();
        for response in [
            replace_ascii(metadata(REVISION, NAR_HASH), "github", "gitlab"),
            replace_ascii(metadata(REVISION, NAR_HASH), "NixOS", "attacker"),
            replace_ascii(metadata(REVISION, NAR_HASH), "nixpkgs", "other"),
        ] {
            let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
            assert_eq!(
                fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
                NixpkgsSourceErrorCode::IdentityMismatch
            );
        }
    }

    #[test]
    fn optional_top_level_revision_may_be_absent() {
        let spec = spec();
        let without_revision = replace_ascii(
            metadata(REVISION, NAR_HASH),
            &format!(r#","revision":"{REVISION}""#),
            "",
        );
        let runner = ExactRunner::new(spec.pin().clone(), Ok(without_revision));
        assert!(fetch_verified_nixpkgs(&spec, &runner).is_ok());
    }

    #[test]
    fn malformed_oversized_duplicate_and_non_store_outputs_are_refused() {
        let spec = spec();
        let cases = [
            (Vec::new(), NixpkgsSourceErrorCode::MalformedMetadata),
            (
                vec![b' '; MAX_METADATA_BYTES + 1],
                NixpkgsSourceErrorCode::MetadataTooLarge,
            ),
            (
                br#"{"locked":{},"locked":{},"path":"x"}"#.to_vec(),
                NixpkgsSourceErrorCode::MalformedMetadata,
            ),
            (
                replace_ascii(
                    metadata(REVISION, NAR_HASH),
                    STORE_PATH,
                    "/tmp/attacker-source",
                ),
                NixpkgsSourceErrorCode::InvalidSourcePath,
            ),
        ];
        for (response, expected) in cases {
            let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
            assert_eq!(
                fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
                expected
            );
        }
    }

    #[test]
    fn runner_failure_stays_closed_and_redacted() {
        let spec = spec();
        let runner = ExactRunner::new(
            spec.pin().clone(),
            Err(NixpkgsSourceError::runner_failure()),
        );
        let error = fetch_verified_nixpkgs(&spec, &runner).unwrap_err();
        assert_eq!(error.code(), NixpkgsSourceErrorCode::RunnerFailure);
        assert!(!error.to_string().contains(REVISION));
        assert!(!format!("{:?}", spec.pin()).contains(REVISION));
    }
}
