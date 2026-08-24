//! Closed release-input schema and filesystem validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tough::TargetName;

const SYSTEMS: [&str; 2] = ["aarch64-darwin", "x86_64-linux"];
const DETERMINATE_VERSION: &str = "3.22.1";
const DETERMINATE_REVISION: &str = "4132ad07a15ee7d88c096ac7172b7afb2672866b";
const DETERMINATE_LICENSE: &str = "LGPL-2.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DeterminateArtifactKind {
    Installer,
    Source,
    License,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeterminateArtifact {
    kind: DeterminateArtifactKind,
    system: Option<String>,
    target: String,
    source: String,
    upstream_url: String,
    sha256: String,
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeterminateInventory {
    version: String,
    revision: String,
    license: String,
    artifacts: Vec<DeterminateArtifact>,
}

#[derive(Clone, Copy)]
struct ExpectedDeterminateArtifact {
    kind: DeterminateArtifactKind,
    system: Option<&'static str>,
    target: &'static str,
    upstream_url: &'static str,
    sha256: &'static str,
    length: u64,
}

const DETERMINATE_CATALOG: [ExpectedDeterminateArtifact; 5] = [
    ExpectedDeterminateArtifact {
        kind: DeterminateArtifactKind::Installer,
        system: Some("aarch64-darwin"),
        target: "determinate/3.22.1/nix-installer-aarch64-darwin",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-darwin",
        sha256: "90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b",
        length: 58_427_232,
    },
    ExpectedDeterminateArtifact {
        kind: DeterminateArtifactKind::Installer,
        system: Some("aarch64-linux"),
        target: "determinate/3.22.1/nix-installer-aarch64-linux",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux",
        sha256: "9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179",
        length: 69_625_424,
    },
    ExpectedDeterminateArtifact {
        kind: DeterminateArtifactKind::Installer,
        system: Some("x86_64-linux"),
        target: "determinate/3.22.1/nix-installer-x86_64-linux",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux",
        sha256: "9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c",
        length: 74_918_096,
    },
    ExpectedDeterminateArtifact {
        kind: DeterminateArtifactKind::Source,
        system: None,
        target: "determinate/3.22.1/nix-installer-v3.22.1.tar.gz",
        upstream_url: "https://codeload.github.com/DeterminateSystems/nix-installer/tar.gz/refs/tags/v3.22.1",
        sha256: "e946ce0920e1ac0a76281d1d0d24b5ddb0fa1807f5317d1545130fe8a04ff084",
        length: 214_322,
    },
    ExpectedDeterminateArtifact {
        kind: DeterminateArtifactKind::License,
        system: None,
        target: "determinate/3.22.1/LICENSE",
        upstream_url: "https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE",
        sha256: "36b6d3fa47916943fd5fec313c584784946047ec1337a78b440e5992cb595f89",
        length: 26_434,
    },
];

#[cfg(test)]
const TEST_DETERMINATE_CATALOG: [ExpectedDeterminateArtifact; 5] = [
    ExpectedDeterminateArtifact {
        sha256: "fc2e4d9312ed0006f7aaa1a7cb0e5922d369f29561ee35a263d8fff423041fdd",
        length: 45,
        ..DETERMINATE_CATALOG[0]
    },
    ExpectedDeterminateArtifact {
        sha256: "93e7cd435abee9defe0d33f78d5d1607ffe7229382d9fb9bc2d911ed6a9c463d",
        length: 44,
        ..DETERMINATE_CATALOG[1]
    },
    ExpectedDeterminateArtifact {
        sha256: "58e510c5de2326bbc6fed552a7129c4c48557d2d73c9a9cd7747f931e9a2c38c",
        length: 43,
        ..DETERMINATE_CATALOG[2]
    },
    ExpectedDeterminateArtifact {
        sha256: "db19720a0456184e6d4ecba1ce050f93df1ac52eab99f904d2129bd5fbbfe32a",
        length: 27,
        ..DETERMINATE_CATALOG[3]
    },
    ExpectedDeterminateArtifact {
        sha256: "e80f015aa127d20f9056c8f99afda13ee57123d6d695eb184b01ed3027832594",
        length: 28,
        ..DETERMINATE_CATALOG[4]
    },
];

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

/// One public binary category authenticated outside TUF with Sigstore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CliArtifactKind {
    /// The public package-manager CLI.
    Pkg,
    /// The Linux bootstrap installer.
    PkgInstall,
}

const CLI_ARTIFACTS: [(CliArtifactKind, &str); 3] = [
    (CliArtifactKind::Pkg, "aarch64-darwin"),
    (CliArtifactKind::Pkg, "x86_64-linux"),
    (CliArtifactKind::PkgInstall, "x86_64-linux"),
];

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
    kind: CliArtifactKind,
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
    determinate: DeterminateInventory,
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

    fn determinate_targets(&self) -> impl Iterator<Item = (&str, PathBuf, &str, u64)> {
        self.manifest.determinate.artifacts.iter().map(|artifact| {
            (
                artifact.target.as_str(),
                self.artifact_root.join(&artifact.source),
                artifact.sha256.as_str(),
                artifact.length,
            )
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
        self.manifest
            .artifacts
            .iter()
            .map(|artifact| {
                (
                    artifact.target.as_str(),
                    self.artifact_root.join(&artifact.source),
                    artifact.sha256.as_str(),
                    artifact.length,
                )
            })
            .chain(self.determinate_targets())
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
        for artifact in &self.manifest.determinate.artifacts {
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
        Self::from_json_with_catalog(bytes, artifact_root, authority, &DETERMINATE_CATALOG)
    }

    #[cfg(test)]
    pub(crate) fn from_json_with_determinate_fixture(
        bytes: &[u8],
        artifact_root: &Path,
        authority: &dyn ReleaseAuthority,
    ) -> Result<ValidatedRelease, ValidationError> {
        Self::from_json_with_catalog(bytes, artifact_root, authority, &TEST_DETERMINATE_CATALOG)
    }

    fn from_json_with_catalog(
        bytes: &[u8],
        artifact_root: &Path,
        authority: &dyn ReleaseAuthority,
        determinate_catalog: &[ExpectedDeterminateArtifact],
    ) -> Result<ValidatedRelease, ValidationError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| ValidationError::InvalidManifest)?;
        manifest.validate(artifact_root, authority, determinate_catalog)
    }

    fn validate(
        self,
        artifact_root: &Path,
        authority: &dyn ReleaseAuthority,
        determinate_catalog: &[ExpectedDeterminateArtifact],
    ) -> Result<ValidatedRelease, ValidationError> {
        if self.schema_version != 2
            || self.channel_sequence == 0
            || self.timestamp_version == 0
            || self.policy_version == 0
            || !valid_atom(&self.release_id)
            || !valid_digest(&self.trusted_root_sha256)
        {
            return Err(ValidationError::InvalidPolicy);
        }
        validate_approvals(&self.approvals)?;
        validate_sets(
            self.channel_sequence,
            &self.determinate,
            determinate_catalog,
            &self.artifacts,
            &self.cli_artifacts,
        )?;

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
        for artifact in &self.determinate.artifacts {
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
    determinate: &DeterminateInventory,
    determinate_catalog: &[ExpectedDeterminateArtifact],
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
    if determinate.version != DETERMINATE_VERSION
        || determinate.revision != DETERMINATE_REVISION
        || determinate.license != DETERMINATE_LICENSE
        || determinate.artifacts.len() != determinate_catalog.len()
    {
        return Err(ValidationError::InvalidArtifactSet);
    }
    let expected_determinate = determinate_catalog
        .iter()
        .map(|artifact| ((artifact.kind, artifact.system), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut actual_determinate = BTreeMap::new();
    for artifact in &determinate.artifacts {
        if !targets.insert(&artifact.target)
            || !sources.insert(&artifact.source)
            || artifact.source != artifact.target
            || actual_determinate
                .insert((artifact.kind, artifact.system.as_deref()), artifact)
                .is_some()
        {
            return Err(ValidationError::InvalidArtifactSet);
        }
    }
    if actual_determinate.len() != expected_determinate.len()
        || expected_determinate.iter().any(|(key, expected)| {
            actual_determinate.get(key).is_none_or(|actual| {
                actual.target != expected.target
                    || actual.upstream_url != expected.upstream_url
                    || actual.sha256 != expected.sha256
                    || actual.length != expected.length
            })
        })
    {
        return Err(ValidationError::InvalidArtifactSet);
    }
    if counts.get(&(ArtifactKind::Descriptor, None)) != Some(&1)
        || artifacts.len() != 1 + SYSTEMS.len() * 6
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
    let cli_artifacts: BTreeSet<_> = cli
        .iter()
        .map(|item| (item.kind, item.system.as_str()))
        .collect();
    if cli.len() != CLI_ARTIFACTS.len()
        || cli_artifacts != CLI_ARTIFACTS.into_iter().collect()
        || cli.iter().any(|item| {
            let expected_source = match item.kind {
                CliArtifactKind::Pkg => format!("cli/pkg-{}", item.system),
                CliArtifactKind::PkgInstall => format!("cli/pkg-installer-{}", item.system),
            };
            item.source != expected_source
                || item.sigstore_bundle != format!("{expected_source}.sigstore.json")
                || !sources.insert(&item.source)
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
