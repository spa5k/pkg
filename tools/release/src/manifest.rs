//! Closed release-input schema and filesystem validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tough::TargetName;

const SYSTEMS: [&str; 4] = [
    "aarch64-darwin",
    "aarch64-linux",
    "x86_64-darwin",
    "x86_64-linux",
];
const CLI_SYSTEMS: [&str; 3] = ["aarch64-darwin", "aarch64-linux", "x86_64-linux"];

/// One TUF-authenticated release-artifact category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    /// The sole channel descriptor.
    Descriptor,
    /// One managed-Nix runtime archive.
    ManagedNix,
    /// One static privileged-asset manifest associated with a runtime.
    ManagedNixAssets,
    /// One disposable per-system index.
    Index,
    /// One product binary consumed only through the signed installer bundle.
    InstallerPayload,
}

/// One artifact that must become a TUF target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifact {
    kind: ArtifactKind,
    system: Option<String>,
    target: String,
    source: String,
    sha256: String,
    length: u64,
}

/// One CLI artifact published beside, but never inside, the TUF repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliArtifact {
    system: String,
    source: String,
    sha256: String,
    length: u64,
    sigstore_bundle: String,
    sigstore_bundle_sha256: String,
    sigstore_bundle_length: u64,
}

/// Human approval role required before publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalRole {
    /// Release custodian approval.
    Release,
    /// Independent security-owner approval.
    Security,
}

/// One approval recorded on the reviewed release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Approval {
    actor: String,
    role: ApprovalRole,
    evidence: String,
}

impl Approval {
    /// Returns the authenticated human identity claimed by the evidence.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }

    /// Returns the independent approval role.
    #[must_use]
    pub const fn role(&self) -> ApprovalRole {
        self.role
    }

    /// Returns the opaque attestation identifier verified by the authority.
    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

/// Durable authorization reserved by the external identity/sequence authority.
pub trait ReleaseAuthorization: Send {
    /// Returns the durable opaque lease identity used for crash recovery.
    fn lease_id(&self) -> &str;

    /// Returns the authenticated workload identity allowed to invoke online keys.
    fn signing_actor(&self) -> &str;

    /// Durably binds the lease to the exact persisted transaction digest.
    fn bind_transaction(&mut self, transaction_digest: &str) -> Result<(), ValidationError>;

    /// Atomically commits the reserved sequence after publication succeeds.
    ///
    /// Implementations must make this idempotent so a crash after remote commit
    /// can be reconciled safely. An uncommitted lease must remain reacquirable
    /// by id until committed or separately cancelled by authorized operations.
    fn commit(&mut self) -> Result<(), ValidationError>;
}

/// Authenticates approval evidence and reserves the authoritative next sequence.
pub trait ReleaseAuthority: Send + Sync {
    /// Verifies both attestations and exclusively reserves `sequence`.
    fn authorize(
        &self,
        release_digest: &str,
        sequence: u64,
        timestamp_version: u64,
        approvals: &[Approval],
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError>;

    /// Reacquires an existing lease while resuming a persisted transaction.
    fn resume(
        &self,
        release_digest: &str,
        transaction_digest: &str,
        lease_id: &str,
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError>;
}

/// Closed schema consumed by the release signer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    schema_version: u64,
    release_id: String,
    channel_sequence: u64,
    timestamp_version: u64,
    trusted_root_sha256: String,
    policy_version: u64,
    artifacts: Vec<ReleaseArtifact>,
    cli_artifacts: Vec<CliArtifact>,
    approvals: Vec<Approval>,
}

/// A release whose schema, approvals, target set, paths, lengths, and hashes agree.
pub struct ValidatedRelease {
    manifest: ReleaseManifest,
    artifact_root: PathBuf,
    release_digest: String,
    canonical_manifest: Vec<u8>,
    authorization: Option<Box<dyn ReleaseAuthorization>>,
}

impl ValidatedRelease {
    /// Returns the validated manifest.
    #[must_use]
    pub const fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }

    /// Returns the canonical digest of the reviewed manifest bytes.
    #[must_use]
    pub fn release_digest(&self) -> &str {
        &self.release_digest
    }

    /// Returns the monotonic product channel sequence.
    #[must_use]
    pub const fn channel_sequence(&self) -> u64 {
        self.manifest.channel_sequence
    }

    /// Returns the independently monotonic timestamp metadata version.
    #[must_use]
    pub const fn timestamp_version(&self) -> u64 {
        self.manifest.timestamp_version
    }

    /// Returns the reviewed release identifier.
    #[must_use]
    pub fn release_id(&self) -> &str {
        &self.manifest.release_id
    }

    /// Returns the independently approved trusted-root digest.
    #[must_use]
    pub fn trusted_root_sha256(&self) -> &str {
        &self.manifest.trusted_root_sha256
    }

    pub(crate) fn canonical_manifest(&self) -> &[u8] {
        &self.canonical_manifest
    }

    pub(crate) fn cli_files(&self) -> impl Iterator<Item = (&str, PathBuf, &str, u64)> {
        self.manifest.cli_artifacts.iter().flat_map(|artifact| {
            [
                (
                    artifact.source.as_str(),
                    self.artifact_root.join(&artifact.source),
                    artifact.sha256.as_str(),
                    artifact.length,
                ),
                (
                    artifact.sigstore_bundle.as_str(),
                    self.artifact_root.join(&artifact.sigstore_bundle),
                    artifact.sigstore_bundle_sha256.as_str(),
                    artifact.sigstore_bundle_length,
                ),
            ]
        })
    }

    pub(crate) fn signing_actor(&self) -> Result<&str, ValidationError> {
        let actor = self
            .authorization
            .as_ref()
            .ok_or(ValidationError::InvalidPolicy)?
            .signing_actor();
        if !valid_actor(actor) {
            return Err(ValidationError::InvalidPolicy);
        }
        Ok(actor)
    }

    pub(crate) fn into_authorization(
        mut self,
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
        self.authorization
            .take()
            .ok_or(ValidationError::InvalidPolicy)
    }

    /// Iterates over validated TUF target names and source paths.
    pub fn tuf_targets(&self) -> impl Iterator<Item = (&str, PathBuf, &str, u64)> {
        self.manifest.artifacts.iter().map(|artifact| {
            (
                artifact.target.as_str(),
                self.artifact_root.join(&artifact.source),
                artifact.sha256.as_str(),
                artifact.length,
            )
        })
    }

    pub(crate) fn revalidate_all(&self) -> Result<(), ValidationError> {
        for artifact in &self.manifest.artifacts {
            validate_file(
                &self.artifact_root,
                &artifact.source,
                &artifact.sha256,
                artifact.length,
            )?;
        }
        for artifact in &self.manifest.cli_artifacts {
            validate_file(
                &self.artifact_root,
                &artifact.source,
                &artifact.sha256,
                artifact.length,
            )?;
            validate_file(
                &self.artifact_root,
                &artifact.sigstore_bundle,
                &artifact.sigstore_bundle_sha256,
                artifact.sigstore_bundle_length,
            )?;
        }
        Ok(())
    }
}

/// Release-input refusal reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// JSON is malformed or violates the closed schema.
    InvalidManifest,
    /// A version, release id, or approval is invalid.
    InvalidPolicy,
    /// The exact V1 artifact set is incomplete, duplicated, or extended.
    InvalidArtifactSet,
    /// A source path or target name is unsafe.
    InvalidPath,
    /// A source is not a regular, non-symlinked file.
    InvalidSource,
    /// A source's committed length or digest disagrees with its bytes.
    ArtifactMismatch,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidManifest => "release manifest is invalid",
            Self::InvalidPolicy => "release policy or approvals are invalid",
            Self::InvalidArtifactSet => "release artifact set is invalid",
            Self::InvalidPath => "release path is unsafe",
            Self::InvalidSource => "release source is not a regular file",
            Self::ArtifactMismatch => "release artifact bytes do not match the manifest",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ValidationError {}

impl ReleaseManifest {
    /// Parses and fully validates a manifest against an artifact directory.
    pub fn from_json(
        bytes: &[u8],
        artifact_root: &Path,
        authority: &dyn ReleaseAuthority,
    ) -> Result<ValidatedRelease, ValidationError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| ValidationError::InvalidManifest)?;
        manifest.validate(artifact_root, authority)
    }

    fn validate(
        self,
        artifact_root: &Path,
        authority: &dyn ReleaseAuthority,
    ) -> Result<ValidatedRelease, ValidationError> {
        if self.schema_version != 1
            || self.channel_sequence == 0
            || self.timestamp_version == 0
            || self.policy_version == 0
            || !valid_atom(&self.release_id)
            || !valid_digest(&self.trusted_root_sha256)
        {
            return Err(ValidationError::InvalidPolicy);
        }
        validate_approvals(&self.approvals)?;
        validate_sets(self.channel_sequence, &self.artifacts, &self.cli_artifacts)?;

        let root_meta =
            fs::symlink_metadata(artifact_root).map_err(|_| ValidationError::InvalidSource)?;
        if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
            return Err(ValidationError::InvalidSource);
        }
        for artifact in &self.artifacts {
            validate_file(
                artifact_root,
                &artifact.source,
                &artifact.sha256,
                artifact.length,
            )?;
            TargetName::new(&artifact.target).map_err(|_| ValidationError::InvalidPath)?;
        }
        for artifact in &self.cli_artifacts {
            validate_file(
                artifact_root,
                &artifact.source,
                &artifact.sha256,
                artifact.length,
            )?;
            validate_file(
                artifact_root,
                &artifact.sigstore_bundle,
                &artifact.sigstore_bundle_sha256,
                artifact.sigstore_bundle_length,
            )?;
        }
        let canonical = serde_json::to_vec(&self).map_err(|_| ValidationError::InvalidManifest)?;
        let release_digest = hex::encode(Sha256::digest(&canonical));
        let authorization = authority.authorize(
            &release_digest,
            self.channel_sequence,
            self.timestamp_version,
            &self.approvals,
        )?;
        Ok(ValidatedRelease {
            manifest: self,
            artifact_root: artifact_root.to_path_buf(),
            release_digest,
            canonical_manifest: canonical,
            authorization: Some(authorization),
        })
    }
}

fn validate_approvals(approvals: &[Approval]) -> Result<(), ValidationError> {
    if approvals.len() != 2
        || approvals[0].actor == approvals[1].actor
        || !approvals
            .iter()
            .all(|approval| valid_actor(&approval.actor) && valid_evidence(&approval.evidence))
        || !approvals
            .iter()
            .any(|approval| approval.role == ApprovalRole::Release)
        || !approvals
            .iter()
            .any(|approval| approval.role == ApprovalRole::Security)
    {
        return Err(ValidationError::InvalidPolicy);
    }
    Ok(())
}

fn validate_sets(
    sequence: u64,
    artifacts: &[ReleaseArtifact],
    cli: &[CliArtifact],
) -> Result<(), ValidationError> {
    let mut counts = BTreeMap::new();
    let mut targets = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut runtime_versions = BTreeSet::new();
    for artifact in artifacts {
        *counts
            .entry((artifact.kind, artifact.system.clone()))
            .or_insert(0usize) += 1;
        if !targets.insert(&artifact.target) || !sources.insert(&artifact.source) {
            return Err(ValidationError::InvalidArtifactSet);
        }
        match artifact.kind {
            ArtifactKind::Descriptor
                if artifact.system.is_none() && artifact.target == "descriptor.json" => {}
            ArtifactKind::Descriptor => return Err(ValidationError::InvalidArtifactSet),
            ArtifactKind::ManagedNix
                if artifact.system.as_deref().is_some_and(|system| {
                    SYSTEMS.contains(&system)
                        && runtime_version(&artifact.target, system, ".tar.xz").is_some_and(
                            |version| {
                                runtime_versions.insert(version.to_owned());
                                true
                            },
                        )
                }) => {}
            ArtifactKind::ManagedNixAssets
                if artifact.system.as_deref().is_some_and(|system| {
                    SYSTEMS.contains(&system)
                        && runtime_version(&artifact.target, system, ".assets.json").is_some_and(
                            |version| {
                                runtime_versions.insert(version.to_owned());
                                true
                            },
                        )
                }) => {}
            ArtifactKind::Index
                if artifact.system.as_deref().is_some_and(|system| {
                    SYSTEMS.contains(&system)
                        && artifact.target == format!("index/{sequence}/{system}.json.br")
                }) => {}
            ArtifactKind::InstallerPayload
                if artifact.system.as_deref().is_some_and(|system| {
                    SYSTEMS.contains(&system)
                        && ["pkg-root-helper", "pkg-nix-broker", "pkg"]
                            .iter()
                            .any(|name| artifact.target == format!("installer/{system}/{name}"))
                }) => {}
            _ => return Err(ValidationError::InvalidArtifactSet),
        }
    }
    if counts.get(&(ArtifactKind::Descriptor, None)) != Some(&1)
        || artifacts.len() != 25
        || runtime_versions.len() != 1
        || [
            ArtifactKind::ManagedNix,
            ArtifactKind::ManagedNixAssets,
            ArtifactKind::Index,
        ]
        .iter()
        .any(|kind| {
            SYSTEMS
                .iter()
                .any(|system| counts.get(&(*kind, Some((*system).to_owned()))) != Some(&1))
        })
        || SYSTEMS.iter().any(|system| {
            counts.get(&(ArtifactKind::InstallerPayload, Some((*system).to_owned()))) != Some(&3)
        })
    {
        return Err(ValidationError::InvalidArtifactSet);
    }
    let cli_systems: BTreeSet<_> = cli.iter().map(|item| item.system.as_str()).collect();
    if cli.len() != CLI_SYSTEMS.len()
        || cli_systems != CLI_SYSTEMS.into_iter().collect()
        || cli.iter().any(|item| {
            !sources.insert(&item.source)
                || !sources.insert(&item.sigstore_bundle)
                || item.source == item.sigstore_bundle
        })
    {
        return Err(ValidationError::InvalidArtifactSet);
    }
    Ok(())
}

fn validate_file(
    root: &Path,
    relative: &str,
    digest: &str,
    length: u64,
) -> Result<(), ValidationError> {
    let path = safe_join(root, relative)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| ValidationError::InvalidSource)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ValidationError::InvalidSource);
    }
    let bytes = fs::read(path).map_err(|_| ValidationError::InvalidSource)?;
    if bytes.len() as u64 != length
        || !valid_digest(digest)
        || hex::encode(Sha256::digest(bytes)) != digest
    {
        return Err(ValidationError::ArtifactMismatch);
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, ValidationError> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ValidationError::InvalidPath);
    }
    let mut current = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ValidationError::InvalidPath);
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && metadata.file_type().is_symlink()
        {
            return Err(ValidationError::InvalidPath);
        }
    }
    Ok(current)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_atom(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_actor(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_evidence(value: &str) -> bool {
    (1..=512).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'\\' | b'\'' | b'"'))
}

fn runtime_version<'a>(target: &'a str, system: &str, suffix: &str) -> Option<&'a str> {
    let tail = format!("/{system}{suffix}");
    let version = target.strip_prefix("nix/")?.strip_suffix(&tail)?;
    if version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    Some(version)
}
