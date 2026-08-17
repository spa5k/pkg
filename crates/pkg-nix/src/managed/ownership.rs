//! Authentication and corroboration of a managed-Nix ownership receipt.
//!
//! A receipt is never self-authenticating. The caller must construct an
//! [`OwnershipExpectation`] from a separately authenticated release/channel
//! manifest. Verification then requires a root-protected receipt to match that
//! expectation exactly and corroborates every declared artifact on disk.
//!
//! The expectation covers the complete **static privileged installation asset
//! set**, not every dynamically realized store path. Exclusive origin for the
//! mutable store is established when the privileged provisioner performs a
//! clean fail-closed preflight immediately before installation, and is then
//! preserved by the product-private daemon/broker boundary. Freezing a store
//! inventory here would make every later package installation invalidate the
//! ownership receipt.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use pkg_core::System;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{Digest, NixVersion};

const SCHEMA_VERSION: u32 = 1;
const PRODUCT: &str = "pkg";
const MAX_RECEIPT_BYTES: u64 = 1_048_576;
// A pinned Nix 2.34.8 runtime closure contains about two thousand filesystem
// entries on the supported Linux systems. Keep this limit aligned with the
// independently bounded archive-entry ceiling so a complete authenticated
// runtime can be represented without making either parser unbounded.
const MAX_ARTIFACTS: usize = 4096;
const MAX_PATH_BYTES: usize = 512;

/// The filesystem kind of one managed artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedArtifactKind {
    /// A regular file whose exact size and SHA-256 digest are verified.
    File,
    /// A directory whose ownership and mode are verified.
    Directory,
    /// A symbolic link whose owner and exact target are verified.
    Symlink,
}

/// Stable privileged group role recorded in the signed asset manifest.
///
/// Numeric gids are deliberately excluded from release metadata because the
/// installer may have to allocate product service groups differently on each
/// host. The authenticated role is resolved through [`ManagedGroupBindings`]
/// immediately before installation and the resulting gids are bound into the
/// root-owned ownership receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedGroup {
    /// The platform root/administrator group (gid 0).
    Root,
    /// The dedicated unprivileged `pkg-nix-broker` service group.
    Broker,
    /// The dedicated Nix build-users group.
    BuildUsers,
}

/// Host-local numeric gids bound to stable signed group roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedGroupBindings {
    broker_gid: u32,
    build_users_gid: u32,
}

impl ManagedGroupBindings {
    /// Validates host-local gids for the two non-root managed groups.
    pub fn new(broker_gid: u32, build_users_gid: u32) -> Result<Self, OwnershipError> {
        if broker_gid == 0 || build_users_gid == 0 || broker_gid == build_users_gid {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
        Ok(Self {
            broker_gid,
            build_users_gid,
        })
    }

    /// Returns the gid allocated to the broker group.
    #[must_use]
    pub const fn broker_gid(self) -> u32 {
        self.broker_gid
    }

    /// Returns the gid allocated to the build-users group.
    #[must_use]
    pub const fn build_users_gid(self) -> u32 {
        self.build_users_gid
    }

    /// Resolves a stable group role to its host-local gid.
    #[must_use]
    pub const fn gid_for(self, group: ManagedGroup) -> u32 {
        match group {
            ManagedGroup::Root => 0,
            ManagedGroup::Broker => self.broker_gid,
            ManagedGroup::BuildUsers => self.build_users_gid,
        }
    }

    #[cfg(test)]
    pub(super) const fn same_gid_for_test(gid: u32) -> Self {
        Self {
            broker_gid: gid,
            build_users_gid: gid,
        }
    }
}

/// One expected product-managed filesystem artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedArtifact {
    path: String,
    kind: ManagedArtifactKind,
    owner_uid: u32,
    group: ManagedGroup,
    mode: Option<u32>,
    size: Option<u64>,
    sha256: Option<Digest>,
    target: Option<String>,
}

impl ManagedArtifact {
    /// Constructs a root-owned regular-file expectation.
    pub fn file(
        path: impl Into<String>,
        group: ManagedGroup,
        mode: u32,
        size: u64,
        sha256: Digest,
    ) -> Result<Self, OwnershipError> {
        Self::new(
            path.into(),
            ManagedArtifactKind::File,
            group,
            Some(mode),
            Some(size),
            Some(sha256),
            None,
        )
    }

    /// Constructs a root-owned directory expectation.
    pub fn directory(
        path: impl Into<String>,
        group: ManagedGroup,
        mode: u32,
    ) -> Result<Self, OwnershipError> {
        Self::new(
            path.into(),
            ManagedArtifactKind::Directory,
            group,
            Some(mode),
            None,
            None,
            None,
        )
    }

    /// Constructs a root-owned symbolic-link expectation.
    pub fn symlink(
        path: impl Into<String>,
        group: ManagedGroup,
        target: impl Into<String>,
    ) -> Result<Self, OwnershipError> {
        let path = path.into();
        let target = target.into();
        if target.is_empty()
            || target.len() > MAX_PATH_BYTES
            || target.contains('\0')
            || !safe_symlink_target(Path::new(&path), Path::new(&target))
        {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
        Self::new(
            path,
            ManagedArtifactKind::Symlink,
            group,
            None,
            None,
            None,
            Some(target),
        )
    }

    fn new(
        path: String,
        kind: ManagedArtifactKind,
        group: ManagedGroup,
        mode: Option<u32>,
        size: Option<u64>,
        sha256: Option<Digest>,
        target: Option<String>,
    ) -> Result<Self, OwnershipError> {
        validate_absolute_path(&path)?;
        let is_multi_user_store =
            kind == ManagedArtifactKind::Directory && path == "/nix/store" && mode == Some(0o1775);
        if kind == ManagedArtifactKind::Directory && path == "/nix/store" && !is_multi_user_store {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
        if !is_multi_user_store && mode.is_some_and(|value| value > 0o777 || value & 0o022 != 0) {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
        Ok(Self {
            path,
            kind,
            owner_uid: 0,
            group,
            mode,
            size,
            sha256,
            target,
        })
    }

    /// Returns the canonical absolute artifact path.
    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    /// Returns the expected filesystem kind.
    #[must_use]
    pub const fn kind(&self) -> ManagedArtifactKind {
        self.kind
    }

    /// Returns the authenticated privileged group role.
    #[must_use]
    pub const fn group(&self) -> ManagedGroup {
        self.group
    }

    /// Returns the expected Unix mode for files and directories.
    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the expected byte size for a regular file.
    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
    }

    /// Returns the expected content digest for a regular file.
    #[must_use]
    pub const fn sha256(&self) -> Option<Digest> {
        self.sha256
    }

    /// Returns the exact target for a symbolic link.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }
}

/// Trusted inputs against which an untrusted local receipt is checked.
///
/// Instances must be populated from authenticated channel/release metadata,
/// never by reading the local receipt that this module verifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipExpectation {
    system: System,
    nix_version: NixVersion,
    asset_manifest_digest: Digest,
    groups: ManagedGroupBindings,
    artifacts: Vec<ManagedArtifact>,
}

impl OwnershipExpectation {
    /// Validates and constructs one authenticated ownership expectation.
    pub fn new(
        system: System,
        nix_version: NixVersion,
        asset_manifest_digest: Digest,
        groups: ManagedGroupBindings,
        mut artifacts: Vec<ManagedArtifact>,
    ) -> Result<Self, OwnershipError> {
        validate_artifacts(system, &mut artifacts)?;
        let encoded_manifest = encode_validated_asset_manifest(system, &nix_version, &artifacts)?;
        if digest_bytes(&encoded_manifest) != asset_manifest_digest {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
        Ok(Self {
            system,
            nix_version,
            asset_manifest_digest,
            groups,
            artifacts,
        })
    }

    /// Returns the target Nix system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the exact managed Nix version.
    #[must_use]
    pub const fn nix_version(&self) -> &NixVersion {
        &self.nix_version
    }

    /// Returns the authenticated asset-manifest digest.
    #[must_use]
    pub const fn asset_manifest_digest(&self) -> Digest {
        self.asset_manifest_digest
    }

    /// Returns the host-local group bindings captured during provisioning.
    #[must_use]
    pub const fn groups(&self) -> ManagedGroupBindings {
        self.groups
    }

    /// Returns the canonical, path-sorted artifact expectations.
    #[must_use]
    pub fn artifacts(&self) -> &[ManagedArtifact] {
        &self.artifacts
    }
}

fn validate_artifacts(
    system: System,
    artifacts: &mut [ManagedArtifact],
) -> Result<(), OwnershipError> {
    if artifacts.is_empty() || artifacts.len() > MAX_ARTIFACTS {
        return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
    }
    for artifact in artifacts.iter() {
        if !path_allowed_for_system(artifact.path(), system) {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
    }
    if !artifacts.iter().any(|artifact| {
        artifact.path == "/nix/store"
            && artifact.kind == ManagedArtifactKind::Directory
            && artifact.mode == Some(0o1775)
    }) {
        return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    if artifacts
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path)
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
    }
    for symlink in artifacts
        .iter()
        .filter(|artifact| artifact.kind == ManagedArtifactKind::Symlink)
    {
        if artifacts.iter().any(|artifact| {
            artifact.path != symlink.path && artifact.path().starts_with(symlink.path())
        }) {
            return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
        }
    }
    Ok(())
}

fn safe_symlink_target(link_path: &Path, target: &Path) -> bool {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        let Some(parent) = link_path.parent() else {
            return false;
        };
        let mut resolved = parent.to_path_buf();
        for component in target.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(value) => resolved.push(value),
                Component::ParentDir => {
                    if !resolved.pop() {
                        return false;
                    }
                }
                Component::RootDir | Component::Prefix(_) => return false,
            }
        }
        resolved
    };
    [
        Path::new("/nix"),
        Path::new("/opt/pkg"),
        Path::new("/Library/Application Support/pkg"),
        Path::new("/var/lib/pkg"),
    ]
    .iter()
    .any(|prefix| resolved == *prefix || resolved.starts_with(prefix))
}

/// Stable closed failure categories for ownership verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipErrorCode {
    /// The trusted expectation itself violates the contract.
    ExpectationInvalid,
    /// The authenticated asset manifest exceeds the fixed input limit.
    ManifestTooLarge,
    /// The authenticated asset manifest is malformed or non-canonical.
    ManifestMalformed,
    /// Manifest bytes or promoted identity differ from authenticated inputs.
    ManifestMismatch,
    /// No receipt exists at the platform-owned location.
    ReceiptMissing,
    /// Receipt path metadata is not root-only and symlink-free.
    ReceiptUnsafe,
    /// Receipt exceeds the fixed input limit.
    ReceiptTooLarge,
    /// Receipt JSON or promoted fields are invalid.
    ReceiptMalformed,
    /// Receipt differs from independently trusted inputs.
    ReceiptMismatch,
    /// A declared artifact is absent.
    ArtifactMissing,
    /// An artifact cannot be inspected without escaping the verification root.
    ArtifactUnsafe,
    /// The observed filesystem kind differs from the manifest.
    ArtifactTypeMismatch,
    /// Artifact ownership differs from the manifest.
    ArtifactOwnerMismatch,
    /// Artifact mode differs from the manifest.
    ArtifactModeMismatch,
    /// Artifact byte length differs from the manifest.
    ArtifactSizeMismatch,
    /// Artifact content digest differs from the manifest.
    ArtifactDigestMismatch,
    /// Symbolic-link target differs from the manifest.
    ArtifactTargetMismatch,
    /// A bounded local I/O operation failed.
    IoFailure,
}

/// Redacted ownership-verification error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipError {
    code: OwnershipErrorCode,
    artifact_index: Option<usize>,
}

impl OwnershipError {
    const fn new(code: OwnershipErrorCode) -> Self {
        Self {
            code,
            artifact_index: None,
        }
    }

    const fn artifact(code: OwnershipErrorCode, artifact_index: usize) -> Self {
        Self {
            code,
            artifact_index: Some(artifact_index),
        }
    }

    /// Returns the stable closed failure code.
    #[must_use]
    pub const fn code(&self) -> OwnershipErrorCode {
        self.code
    }

    /// Returns the canonical artifact index, without disclosing a host path.
    #[must_use]
    pub const fn artifact_index(&self) -> Option<usize> {
        self.artifact_index
    }
}

impl fmt::Display for OwnershipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "managed-Nix ownership verification failed: {:?}",
            self.code
        )
    }
}

impl std::error::Error for OwnershipError {}

/// Successful, fully corroborated proof that `pkg` owns the managed Nix tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOwnership {
    system: System,
    nix_version: NixVersion,
    asset_manifest_digest: Digest,
    artifact_count: usize,
}

impl VerifiedOwnership {
    /// Returns the verified target system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the verified exact Nix version.
    #[must_use]
    pub const fn nix_version(&self) -> &NixVersion {
        &self.nix_version
    }

    /// Returns the verified authenticated asset-manifest digest.
    #[must_use]
    pub const fn asset_manifest_digest(&self) -> Digest {
        self.asset_manifest_digest
    }

    /// Returns the number of corroborated filesystem artifacts.
    #[must_use]
    pub const fn artifact_count(&self) -> usize {
        self.artifact_count
    }
}

/// Returns the fixed machine-global ownership-receipt location.
#[must_use]
pub fn ownership_receipt_path(system: System) -> &'static Path {
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        Path::new("/var/lib/pkg/managed-nix/ownership-v1.json")
    } else {
        Path::new("/Library/Application Support/pkg/managed-nix/ownership-v1.json")
    }
}

/// Encodes the deterministic receipt body that the privileged provisioner
/// will install atomically with root-only permissions.
pub fn encode_ownership_receipt(
    expectation: &OwnershipExpectation,
) -> Result<Vec<u8>, OwnershipError> {
    serde_json::to_vec_pretty(&WireReceipt::from(expectation))
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::IoFailure))
}

/// Encodes the canonical static privileged-asset manifest whose exact bytes
/// are hashed and authenticated by release/channel metadata.
///
/// PR-12 must use these exact bytes as its signed target body. Verification
/// recomputes their SHA-256 before accepting an [`OwnershipExpectation`], so a
/// caller cannot pair an authenticated digest with a truncated artifact list.
pub fn encode_ownership_asset_manifest(
    system: System,
    nix_version: &NixVersion,
    artifacts: &[ManagedArtifact],
) -> Result<Vec<u8>, OwnershipError> {
    let mut artifacts = artifacts.to_vec();
    validate_artifacts(system, &mut artifacts)?;
    encode_validated_asset_manifest(system, nix_version, &artifacts)
}

/// Decodes a separately authenticated canonical asset manifest and binds its
/// stable group roles to host-local gids.
///
/// The digest must come from verified release/channel metadata. The local
/// manifest is never allowed to choose its own expected digest or group ids.
pub fn decode_ownership_asset_manifest(
    bytes: &[u8],
    expected_system: System,
    expected_nix_version: &NixVersion,
    expected_digest: Digest,
    groups: ManagedGroupBindings,
) -> Result<OwnershipExpectation, OwnershipError> {
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(OwnershipError::new(OwnershipErrorCode::ManifestTooLarge));
    }
    if digest_bytes(bytes) != expected_digest {
        return Err(OwnershipError::new(OwnershipErrorCode::ManifestMismatch));
    }
    let wire: WireAssetManifest = serde_json::from_slice(bytes)
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::ManifestMalformed))?;
    if wire.schema_version != SCHEMA_VERSION
        || wire.product != PRODUCT
        || wire.system != expected_system.as_str()
        || wire.nix_version != expected_nix_version.as_str()
        || wire.artifacts.is_empty()
        || wire.artifacts.len() > MAX_ARTIFACTS
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ManifestMismatch));
    }
    let artifacts = wire
        .artifacts
        .into_iter()
        .map(WireArtifact::into_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    OwnershipExpectation::new(
        expected_system,
        expected_nix_version.clone(),
        expected_digest,
        groups,
        artifacts,
    )
}

fn encode_validated_asset_manifest(
    system: System,
    nix_version: &NixVersion,
    artifacts: &[ManagedArtifact],
) -> Result<Vec<u8>, OwnershipError> {
    serde_json::to_vec(&WireAssetManifest {
        schema_version: SCHEMA_VERSION,
        product: PRODUCT.to_owned(),
        system: system.as_str().to_owned(),
        nix_version: nix_version.as_str().to_owned(),
        artifacts: artifacts.iter().map(WireArtifact::from).collect(),
    })
    .map_err(|_| OwnershipError::new(OwnershipErrorCode::IoFailure))
}

fn digest_bytes(bytes: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Digest::from_bytes(hasher.finalize().into())
}

/// Verifies the production root-owned receipt and every trusted artifact.
pub fn verify_ownership_receipt(
    root: &Path,
    expectation: &OwnershipExpectation,
) -> Result<VerifiedOwnership, OwnershipError> {
    verify_with_owner_uid(root, expectation, 0)
}

pub(super) fn verify_with_owner_uid(
    root: &Path,
    expectation: &OwnershipExpectation,
    required_owner_uid: u32,
) -> Result<VerifiedOwnership, OwnershipError> {
    let root = root
        .canonicalize()
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::IoFailure))?;
    let receipt_path = rooted(&root, ownership_receipt_path(expectation.system));
    let first_receipt = read_safe_receipt(&root, &receipt_path, required_owner_uid)?;
    let wire: WireReceipt = serde_json::from_slice(&first_receipt)
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::ReceiptMalformed))?;
    if wire.schema_version != SCHEMA_VERSION
        || wire.product != PRODUCT
        || wire.artifacts.len() > MAX_ARTIFACTS
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptMalformed));
    }
    if wire != WireReceipt::from(expectation) {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptMismatch));
    }

    let verified = verify_artifacts(&root, expectation, required_owner_uid)?;

    let second_receipt = read_safe_receipt(&root, &receipt_path, required_owner_uid)?;
    if first_receipt != second_receipt {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptMismatch));
    }
    Ok(verified)
}

/// Corroborates every authenticated artifact on disk without a receipt.
///
/// Recovery uses this when no root-owned receipt is available or trusted. It
/// accepts only an authenticated [`OwnershipExpectation`], a fixed host root,
/// and the required owner uid. It reuses the same per-artifact verification as
/// `verify_with_owner_uid` but never reads, trusts, or publishes a receipt,
/// and it inspects only paths rooted in the authenticated artifact set.
///
/// # Errors
///
/// Returns the same artifact failures as receipt-bound verification.
pub fn verify_ownership_expectation(
    root: &Path,
    expectation: &OwnershipExpectation,
    required_owner_uid: u32,
) -> Result<VerifiedOwnership, OwnershipError> {
    let root = root
        .canonicalize()
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::IoFailure))?;
    verify_artifacts(&root, expectation, required_owner_uid)
}

pub(super) fn verify_artifacts(
    root: &Path,
    expectation: &OwnershipExpectation,
    required_owner_uid: u32,
) -> Result<VerifiedOwnership, OwnershipError> {
    let store_gid = expectation
        .artifacts
        .iter()
        .find(|artifact| artifact.path == "/nix/store")
        .map(|artifact| expectation.groups.gid_for(artifact.group))
        .ok_or_else(|| OwnershipError::new(OwnershipErrorCode::ExpectationInvalid))?;

    // Dynamic store objects are authenticated by Nix and tracked by the Nix DB
    // plus pkg state; this corroborates all static privileged assets.
    for (index, artifact) in expectation.artifacts.iter().enumerate() {
        verify_artifact(
            root,
            artifact,
            expectation.groups.gid_for(artifact.group),
            required_owner_uid,
            store_gid,
            index,
        )?;
    }

    Ok(VerifiedOwnership {
        system: expectation.system,
        nix_version: expectation.nix_version.clone(),
        asset_manifest_digest: expectation.asset_manifest_digest,
        artifact_count: expectation.artifacts.len(),
    })
}

/// Verifies that every authenticated static artifact beneath `root` is absent or
/// matches the signed expectation exactly, without the ownership receipt.
///
/// This backs authenticated recovery of a partially installed managed runtime
/// whose install was interrupted, typically before the ownership receipt was
/// published. An absent path is accepted, because the outer journal plus
/// clean-host preflight established absence before mutation, so any present
/// allowlisted exact object is attempt-owned. A present path must match the
/// authenticated type, owner, group, mode, content, and symlink target exactly.
/// Ownership drift, mode drift, content mismatch, or unsafe access is refused.
pub(super) fn verify_artifacts_absent_or_exact(
    root: &Path,
    expectation: &OwnershipExpectation,
    required_owner_uid: u32,
) -> Result<(), OwnershipError> {
    let store_gid = expectation
        .artifacts
        .iter()
        .find(|artifact| artifact.path == "/nix/store")
        .map(|artifact| expectation.groups.gid_for(artifact.group))
        .ok_or_else(|| OwnershipError::new(OwnershipErrorCode::ExpectationInvalid))?;
    for (index, artifact) in expectation.artifacts.iter().enumerate() {
        let path = rooted(root, artifact.path());
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(OwnershipError::artifact(
                    OwnershipErrorCode::ArtifactUnsafe,
                    index,
                ));
            }
            Ok(_) => {}
        }
        verify_artifact(
            root,
            artifact,
            expectation.groups.gid_for(artifact.group),
            required_owner_uid,
            store_gid,
            index,
        )?;
    }
    Ok(())
}

fn read_safe_receipt(
    root: &Path,
    path: &Path,
    required_owner_uid: u32,
) -> Result<Vec<u8>, OwnershipError> {
    let parent = path
        .parent()
        .ok_or_else(|| OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe))?;
    verify_receipt_ancestors(root, parent, required_owner_uid)?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OwnershipError::new(OwnershipErrorCode::ReceiptMissing)
        } else {
            OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe)
        }
    })?;
    if !parent_metadata.file_type().is_dir()
        || parent_metadata.uid() != required_owner_uid
        || parent_metadata.mode() & 0o7777 != 0o700
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe));
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                OwnershipError::new(OwnershipErrorCode::ReceiptMissing)
            } else {
                OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe)
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != required_owner_uid
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe));
    }
    if metadata.len() > MAX_RECEIPT_BYTES {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptTooLarge));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::IoFailure))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(OwnershipError::new(OwnershipErrorCode::ReceiptTooLarge));
    }
    Ok(bytes)
}

fn verify_artifact(
    root: &Path,
    artifact: &ManagedArtifact,
    expected_group_gid: u32,
    required_owner_uid: u32,
    store_gid: u32,
    index: usize,
) -> Result<(), OwnershipError> {
    let path = rooted(root, artifact.path());
    let parent = path
        .parent()
        .ok_or_else(|| OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index))?;
    verify_artifact_parent(root, parent, required_owner_uid, store_gid, index)?;

    match artifact.kind {
        ManagedArtifactKind::File => verify_file(
            &path,
            artifact,
            expected_group_gid,
            required_owner_uid,
            index,
        ),
        ManagedArtifactKind::Directory => {
            let metadata = artifact_metadata(&path, index)?;
            if !metadata.file_type().is_dir() {
                return Err(OwnershipError::artifact(
                    OwnershipErrorCode::ArtifactTypeMismatch,
                    index,
                ));
            }
            verify_owner_and_mode(
                &metadata,
                artifact,
                expected_group_gid,
                required_owner_uid,
                index,
            )
        }
        ManagedArtifactKind::Symlink => {
            let metadata = artifact_metadata(&path, index)?;
            if !metadata.file_type().is_symlink() {
                return Err(OwnershipError::artifact(
                    OwnershipErrorCode::ArtifactTypeMismatch,
                    index,
                ));
            }
            verify_owner(
                &metadata,
                artifact,
                expected_group_gid,
                required_owner_uid,
                index,
            )?;
            let target = fs::read_link(&path)
                .map_err(|_| OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index))?;
            if target.as_os_str() != artifact.target.as_deref().unwrap_or_default() {
                return Err(OwnershipError::artifact(
                    OwnershipErrorCode::ArtifactTargetMismatch,
                    index,
                ));
            }
            Ok(())
        }
    }
}

fn verify_file(
    path: &Path,
    artifact: &ManagedArtifact,
    expected_group_gid: u32,
    required_owner_uid: u32,
    index: usize,
) -> Result<(), OwnershipError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                OwnershipError::artifact(OwnershipErrorCode::ArtifactMissing, index)
            } else {
                OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index)
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index))?;
    if !metadata.file_type().is_file() {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactTypeMismatch,
            index,
        ));
    }
    verify_owner_and_mode(
        &metadata,
        artifact,
        expected_group_gid,
        required_owner_uid,
        index,
    )?;
    if Some(metadata.len()) != artifact.size {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactSizeMismatch,
            index,
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| OwnershipError::artifact(OwnershipErrorCode::IoFailure, index))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = Digest::from_bytes(hasher.finalize().into());
    if Some(actual) != artifact.sha256 {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactDigestMismatch,
            index,
        ));
    }
    Ok(())
}

fn artifact_metadata(path: &Path, index: usize) -> Result<fs::Metadata, OwnershipError> {
    fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OwnershipError::artifact(OwnershipErrorCode::ArtifactMissing, index)
        } else {
            OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index)
        }
    })
}

fn verify_owner_and_mode(
    metadata: &fs::Metadata,
    artifact: &ManagedArtifact,
    expected_group_gid: u32,
    required_owner_uid: u32,
    index: usize,
) -> Result<(), OwnershipError> {
    verify_owner(
        metadata,
        artifact,
        expected_group_gid,
        required_owner_uid,
        index,
    )?;
    if Some(metadata.mode() & 0o7777) != artifact.mode {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactModeMismatch,
            index,
        ));
    }
    Ok(())
}

fn verify_owner(
    metadata: &fs::Metadata,
    artifact: &ManagedArtifact,
    expected_group_gid: u32,
    required_owner_uid: u32,
    index: usize,
) -> Result<(), OwnershipError> {
    if artifact.owner_uid != 0
        || metadata.uid() != required_owner_uid
        || metadata.gid() != expected_group_gid
    {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactOwnerMismatch,
            index,
        ));
    }
    Ok(())
}

fn rooted(root: &Path, absolute: &Path) -> PathBuf {
    let relative: PathBuf = absolute
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect();
    root.join(relative)
}

pub(super) fn verify_receipt_ancestors(
    root: &Path,
    parent: &Path,
    required_owner_uid: u32,
) -> Result<(), OwnershipError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe))?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != required_owner_uid
            || metadata.mode() & 0o022 != 0
        {
            return Err(OwnershipError::new(OwnershipErrorCode::ReceiptUnsafe));
        }
    }
    Ok(())
}

fn verify_artifact_parent(
    root: &Path,
    parent: &Path,
    required_owner_uid: u32,
    store_gid: u32,
    index: usize,
) -> Result<(), OwnershipError> {
    let canonical_parent = parent.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OwnershipError::artifact(OwnershipErrorCode::ArtifactMissing, index)
        } else {
            OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index)
        }
    })?;
    if !canonical_parent.starts_with(root) {
        return Err(OwnershipError::artifact(
            OwnershipErrorCode::ArtifactUnsafe,
            index,
        ));
    }
    let relative = canonical_parent
        .strip_prefix(root)
        .map_err(|_| OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| OwnershipError::artifact(OwnershipErrorCode::ArtifactUnsafe, index))?;
        let is_multi_user_store = current == root.join("nix/store")
            && metadata.file_type().is_dir()
            && metadata.uid() == required_owner_uid
            && metadata.gid() == store_gid
            && metadata.mode() & 0o7777 == 0o1775;
        if !is_multi_user_store
            && (!metadata.file_type().is_dir()
                || metadata.uid() != required_owner_uid
                || metadata.mode() & 0o022 != 0)
        {
            return Err(OwnershipError::artifact(
                OwnershipErrorCode::ArtifactUnsafe,
                index,
            ));
        }
    }
    Ok(())
}

fn validate_absolute_path(value: &str) -> Result<(), OwnershipError> {
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.contains('\0') || value == "/" {
        return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        || value.contains("//")
        || value.ends_with('/')
    {
        return Err(OwnershipError::new(OwnershipErrorCode::ExpectationInvalid));
    }
    Ok(())
}

fn path_allowed_for_system(path: &Path, system: System) -> bool {
    let common = [
        Path::new("/nix"),
        Path::new("/etc/nix"),
        Path::new("/opt/pkg"),
    ];
    if common
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(prefix))
    {
        return true;
    }
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        [
            Path::new("/etc/systemd/system"),
            Path::new("/etc/tmpfiles.d"),
            Path::new("/var/lib/pkg"),
        ]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(prefix))
    } else {
        [
            Path::new("/Library/LaunchDaemons"),
            Path::new("/Library/Application Support/pkg"),
        ]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(prefix))
            || matches!(path.to_str(), Some("/etc/synthetic.conf" | "/etc/fstab"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireReceipt {
    schema_version: u32,
    product: String,
    system: String,
    nix_version: String,
    asset_manifest_digest: String,
    groups: WireGroupBindings,
    artifacts: Vec<WireArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireGroupBindings {
    broker_gid: u32,
    build_users_gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireAssetManifest {
    schema_version: u32,
    product: String,
    system: String,
    nix_version: String,
    artifacts: Vec<WireArtifact>,
}

impl From<&OwnershipExpectation> for WireReceipt {
    fn from(expectation: &OwnershipExpectation) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            system: expectation.system.as_str().to_owned(),
            nix_version: expectation.nix_version.as_str().to_owned(),
            asset_manifest_digest: expectation.asset_manifest_digest.to_string(),
            groups: WireGroupBindings {
                broker_gid: expectation.groups.broker_gid,
                build_users_gid: expectation.groups.build_users_gid,
            },
            artifacts: expectation
                .artifacts
                .iter()
                .map(WireArtifact::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireArtifact {
    path: String,
    kind: ManagedArtifactKind,
    owner_uid: u32,
    group: ManagedGroup,
    mode: Option<u32>,
    size: Option<u64>,
    sha256: Option<String>,
    target: Option<String>,
}

impl From<&ManagedArtifact> for WireArtifact {
    fn from(artifact: &ManagedArtifact) -> Self {
        Self {
            path: artifact.path.clone(),
            kind: artifact.kind,
            owner_uid: artifact.owner_uid,
            group: artifact.group,
            mode: artifact.mode,
            size: artifact.size,
            sha256: artifact.sha256.map(|digest| digest.to_string()),
            target: artifact.target.clone(),
        }
    }
}

impl WireArtifact {
    fn into_artifact(self) -> Result<ManagedArtifact, OwnershipError> {
        if self.owner_uid != 0 {
            return Err(OwnershipError::new(OwnershipErrorCode::ManifestMalformed));
        }
        match self.kind {
            ManagedArtifactKind::File => {
                let (Some(mode), Some(size), Some(sha256), None) =
                    (self.mode, self.size, self.sha256, self.target)
                else {
                    return Err(OwnershipError::new(OwnershipErrorCode::ManifestMalformed));
                };
                let digest = sha256
                    .parse::<Digest>()
                    .map_err(|_| OwnershipError::new(OwnershipErrorCode::ManifestMalformed))?;
                ManagedArtifact::file(self.path, self.group, mode, size, digest)
            }
            ManagedArtifactKind::Directory => {
                let (Some(mode), None, None, None) =
                    (self.mode, self.size, self.sha256, self.target)
                else {
                    return Err(OwnershipError::new(OwnershipErrorCode::ManifestMalformed));
                };
                ManagedArtifact::directory(self.path, self.group, mode)
            }
            ManagedArtifactKind::Symlink => {
                let (None, None, None, Some(target)) =
                    (self.mode, self.size, self.sha256, self.target)
                else {
                    return Err(OwnershipError::new(OwnershipErrorCode::ManifestMalformed));
                };
                ManagedArtifact::symlink(self.path, self.group, target)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use pkg_core::state::body_digest;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn manifest_digest(
        system: System,
        nix_version: &NixVersion,
        artifacts: &[ManagedArtifact],
    ) -> Digest {
        body_digest(&encode_ownership_asset_manifest(system, nix_version, artifacts).unwrap())
    }

    struct Fixture {
        root: PathBuf,
        owner_uid: u32,
        group_gid: u32,
        expectation: OwnershipExpectation,
    }

    impl Fixture {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("pkg-ownership-{}-{serial}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let metadata = fs::metadata(&root).unwrap();
            let owner_uid = metadata.uid();
            let group_gid = metadata.gid();

            let file_path = root.join("opt/pkg/bin/nix");
            fs::create_dir_all(file_path.parent().unwrap()).unwrap();
            fs::write(&file_path, b"managed nix\n").unwrap();
            fs::set_permissions(&file_path, fs::Permissions::from_mode(0o555)).unwrap();
            let directory_path = root.join("nix/store");
            fs::create_dir_all(&directory_path).unwrap();
            fs::set_permissions(&directory_path, fs::Permissions::from_mode(0o1775)).unwrap();
            let store_object = directory_path.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix");
            fs::create_dir(&store_object).unwrap();
            let store_file = store_object.join("nix");
            fs::write(&store_file, b"store nix\n").unwrap();
            fs::set_permissions(&store_file, fs::Permissions::from_mode(0o444)).unwrap();
            fs::set_permissions(&store_object, fs::Permissions::from_mode(0o555)).unwrap();
            let link_path = root.join("opt/pkg/bin/nix-current");
            symlink("nix", &link_path).unwrap();

            let artifacts = vec![
                ManagedArtifact::file(
                    "/opt/pkg/bin/nix",
                    ManagedGroup::Broker,
                    0o555,
                    12,
                    body_digest(b"managed nix\n"),
                )
                .unwrap(),
                ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap(),
                ManagedArtifact::directory(
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix",
                    ManagedGroup::BuildUsers,
                    0o555,
                )
                .unwrap(),
                ManagedArtifact::file(
                    "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix/nix",
                    ManagedGroup::BuildUsers,
                    0o444,
                    10,
                    body_digest(b"store nix\n"),
                )
                .unwrap(),
                ManagedArtifact::symlink("/opt/pkg/bin/nix-current", ManagedGroup::Broker, "nix")
                    .unwrap(),
            ];
            let nix_version = NixVersion::new("2.34.8").unwrap();
            let asset_manifest_digest =
                manifest_digest(System::Aarch64Darwin, &nix_version, &artifacts);
            let expectation = OwnershipExpectation::new(
                System::Aarch64Darwin,
                nix_version,
                asset_manifest_digest,
                ManagedGroupBindings {
                    broker_gid: group_gid,
                    build_users_gid: group_gid,
                },
                artifacts,
            )
            .unwrap();

            let receipt = rooted(&root, ownership_receipt_path(expectation.system));
            fs::create_dir_all(receipt.parent().unwrap()).unwrap();
            fs::set_permissions(receipt.parent().unwrap(), fs::Permissions::from_mode(0o700))
                .unwrap();
            fs::write(&receipt, encode_ownership_receipt(&expectation).unwrap()).unwrap();
            fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();

            Self {
                root,
                owner_uid,
                group_gid,
                expectation,
            }
        }

        fn receipt(&self) -> PathBuf {
            rooted(&self.root, ownership_receipt_path(self.expectation.system))
        }

        fn verify(&self) -> Result<VerifiedOwnership, OwnershipError> {
            verify_with_owner_uid(&self.root, &self.expectation, self.owner_uid)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn valid_receipt_and_complete_artifact_set_verify() {
        let fixture = Fixture::new();
        let verified = fixture.verify().unwrap();
        assert_eq!(verified.system(), System::Aarch64Darwin);
        assert_eq!(verified.nix_version().as_str(), "2.34.8");
        assert_eq!(verified.artifact_count(), 5);
    }

    #[test]
    fn signed_manifest_binds_roles_without_baking_in_host_gids() {
        let fixture = Fixture::new();
        let bytes = encode_ownership_asset_manifest(
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            fixture.expectation.artifacts(),
        )
        .unwrap();
        let first = decode_ownership_asset_manifest(
            &bytes,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            body_digest(&bytes),
            ManagedGroupBindings::new(1001, 1002).unwrap(),
        )
        .unwrap();
        let second = decode_ownership_asset_manifest(
            &bytes,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            body_digest(&bytes),
            ManagedGroupBindings::new(2001, 2002).unwrap(),
        )
        .unwrap();
        assert_eq!(first.artifacts(), second.artifacts());
        assert_ne!(first.groups(), second.groups());
        assert_eq!(
            first.asset_manifest_digest(),
            second.asset_manifest_digest()
        );
    }

    #[test]
    fn asset_manifest_unknown_fields_and_digest_mismatch_fail_closed() {
        let fixture = Fixture::new();
        let bytes = encode_ownership_asset_manifest(
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            fixture.expectation.artifacts(),
        )
        .unwrap();
        assert_eq!(
            decode_ownership_asset_manifest(
                &bytes,
                fixture.expectation.system(),
                fixture.expectation.nix_version(),
                body_digest(b"other"),
                fixture.expectation.groups(),
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ManifestMismatch
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["unexpected"] = serde_json::json!(true);
        let malformed = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            decode_ownership_asset_manifest(
                &malformed,
                fixture.expectation.system(),
                fixture.expectation.nix_version(),
                body_digest(&malformed),
                fixture.expectation.groups(),
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ManifestMalformed
        );
    }

    #[test]
    fn authenticated_manifest_digest_rejects_a_truncated_artifact_set() {
        let fixture = Fixture::new();
        let mut truncated = fixture.expectation.artifacts().to_vec();
        truncated.pop();
        let error = OwnershipExpectation::new(
            fixture.expectation.system(),
            fixture.expectation.nix_version().clone(),
            fixture.expectation.asset_manifest_digest(),
            fixture.expectation.groups(),
            truncated,
        )
        .unwrap_err();
        assert_eq!(error.code(), OwnershipErrorCode::ExpectationInvalid);
    }

    #[test]
    fn receipt_is_not_self_authenticating() {
        let fixture = Fixture::new();
        let nix_version = NixVersion::new("2.34.9").unwrap();
        let artifacts = fixture.expectation.artifacts().to_vec();
        let asset_manifest_digest =
            manifest_digest(System::Aarch64Darwin, &nix_version, &artifacts);
        let other = OwnershipExpectation::new(
            System::Aarch64Darwin,
            nix_version,
            asset_manifest_digest,
            fixture.expectation.groups(),
            artifacts,
        )
        .unwrap();
        let error = verify_with_owner_uid(&fixture.root, &other, fixture.owner_uid).unwrap_err();
        assert_eq!(error.code(), OwnershipErrorCode::ReceiptMismatch);
    }

    #[test]
    fn unsafe_receipt_mode_is_rejected() {
        let fixture = Fixture::new();
        fs::set_permissions(fixture.receipt(), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptUnsafe
        );
    }

    #[test]
    fn oversized_or_symlinked_receipt_is_rejected() {
        let fixture = Fixture::new();
        let receipt = fixture.receipt();
        OpenOptions::new()
            .write(true)
            .open(&receipt)
            .unwrap()
            .set_len(MAX_RECEIPT_BYTES + 1)
            .unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptTooLarge
        );

        fs::remove_file(&receipt).unwrap();
        let target = fixture.root.join("receipt-target");
        fs::write(
            &target,
            encode_ownership_receipt(&fixture.expectation).unwrap(),
        )
        .unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(target, receipt).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptUnsafe
        );
    }

    #[test]
    fn malformed_or_extended_receipt_is_rejected() {
        let fixture = Fixture::new();
        let path = fixture.receipt();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptMalformed
        );
    }

    #[test]
    fn changed_file_bytes_are_rejected() {
        let fixture = Fixture::new();
        let path = fixture.root.join("opt/pkg/bin/nix");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&path, b"changed nix\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactDigestMismatch
        );
    }

    #[test]
    fn changed_file_size_or_type_is_rejected() {
        let fixture = Fixture::new();
        let path = fixture.root.join("opt/pkg/bin/nix");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&path, b"different length\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactSizeMismatch
        );

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactTypeMismatch
        );
    }

    #[test]
    fn changed_artifact_group_is_rejected_after_receipt_match() {
        let mut fixture = Fixture::new();
        fixture.expectation.groups.broker_gid = fixture.group_gid.wrapping_add(1);
        fs::write(
            fixture.receipt(),
            encode_ownership_receipt(&fixture.expectation).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactOwnerMismatch
        );
    }

    #[test]
    fn store_parent_uses_the_store_expectations_group() {
        let mut fixture = Fixture::new();
        fixture.expectation.groups.build_users_gid = fixture.group_gid.wrapping_add(1);
        fs::write(
            fixture.receipt(),
            encode_ownership_receipt(&fixture.expectation).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactOwnerMismatch
        );
    }

    #[test]
    fn changed_artifact_mode_is_rejected() {
        let fixture = Fixture::new();
        fs::set_permissions(
            fixture.root.join("opt/pkg/bin/nix"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactModeMismatch
        );
    }

    #[test]
    fn changed_symlink_target_is_rejected() {
        let fixture = Fixture::new();
        let path = fixture.root.join("opt/pkg/bin/nix-current");
        fs::remove_file(&path).unwrap();
        symlink("elsewhere", path).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactTargetMismatch
        );
    }

    #[test]
    fn missing_artifact_is_rejected() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.root.join("opt/pkg/bin/nix")).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ArtifactMissing
        );
    }

    #[test]
    fn duplicate_and_out_of_scope_paths_are_rejected() {
        assert_eq!(
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o775)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
        assert_eq!(
            ManagedArtifact::directory("/opt/pkg/open", ManagedGroup::Root, 0o777)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
        assert_eq!(
            ManagedArtifact::directory("/opt/pkg/../foreign", ManagedGroup::Root, 0o755)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
        let artifact = ManagedArtifact::directory("/tmp/pkg", ManagedGroup::Root, 0o755).unwrap();
        assert_eq!(
            OwnershipExpectation::new(
                System::Aarch64Darwin,
                NixVersion::new("2.34.8").unwrap(),
                body_digest(b"manifest"),
                ManagedGroupBindings::new(20, 21).unwrap(),
                vec![artifact],
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ExpectationInvalid
        );

        let artifact =
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap();
        assert_eq!(
            OwnershipExpectation::new(
                System::Aarch64Darwin,
                NixVersion::new("2.34.8").unwrap(),
                body_digest(b"manifest"),
                ManagedGroupBindings::new(20, 21).unwrap(),
                vec![artifact.clone(), artifact],
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
    }

    #[test]
    fn symlink_targets_cannot_escape_managed_prefixes_or_parent_other_assets() {
        assert_eq!(
            ManagedArtifact::symlink(
                "/opt/pkg/bin/escape",
                ManagedGroup::Broker,
                "../../../etc/passwd",
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
        assert_eq!(
            ManagedArtifact::symlink("/opt/pkg/bin/escape", ManagedGroup::Broker, "/etc/passwd",)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ExpectationInvalid
        );

        let store =
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap();
        let link =
            ManagedArtifact::symlink("/opt/pkg/runtime", ManagedGroup::Broker, "nix-2.24.10")
                .unwrap();
        let nested = ManagedArtifact::file(
            "/opt/pkg/runtime/bin/nix",
            ManagedGroup::Broker,
            0o550,
            0,
            body_digest(&[]),
        )
        .unwrap();
        let version = NixVersion::new("2.24.10").unwrap();
        let artifacts = vec![store, link, nested];
        assert_eq!(
            OwnershipExpectation::new(
                System::X8664Linux,
                version,
                body_digest(b"invalid manifest"),
                ManagedGroupBindings::new(1001, 1002).unwrap(),
                artifacts,
            )
            .unwrap_err()
            .code(),
            OwnershipErrorCode::ExpectationInvalid
        );
    }

    #[test]
    fn parent_symlink_escape_is_rejected() {
        let fixture = Fixture::new();
        let outside = fixture.root.with_extension("outside");
        fs::create_dir(&outside).unwrap();
        let escaped = fixture.root.join("opt/pkg/bin");
        fs::remove_file(escaped.join("nix-current")).unwrap();
        fs::remove_file(escaped.join("nix")).unwrap();
        fs::remove_dir(&escaped).unwrap();
        symlink(&outside, &escaped).unwrap();
        let error = fixture.verify().unwrap_err();
        assert_eq!(error.code(), OwnershipErrorCode::ArtifactUnsafe);
        fs::remove_file(&escaped).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn artifact_groups_are_bound_by_the_expectation() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.group_gid,
            fs::metadata(&fixture.root).unwrap().gid()
        );
        assert!(fixture.verify().is_ok());
    }

    #[test]
    fn receipt_free_verifier_passes_without_a_receipt() {
        let fixture = Fixture::new();
        // Receipt-bound verification requires the receipt; remove it to prove
        // the new verifier needs none.
        fs::remove_file(fixture.receipt()).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptMissing
        );
        let verified =
            verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
                .unwrap();
        assert_eq!(verified.system(), fixture.expectation.system());
        assert_eq!(
            verified.artifact_count(),
            fixture.expectation.artifacts().len()
        );
    }

    #[test]
    fn receipt_free_verifier_refuses_missing_and_tampered_artifacts() {
        let fixture = Fixture::new();

        // Missing artifact.
        let file = fixture.root.join("opt/pkg/bin/nix");
        fs::remove_file(&file).unwrap();
        assert_eq!(
            verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ArtifactMissing
        );

        // Tampered bytes restore the file but change its content at the
        // exact recorded size, exercising the digest check.
        fs::write(&file, b"tampered ni\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
                .unwrap_err()
                .code(),
            OwnershipErrorCode::ArtifactDigestMismatch
        );

        // Exact bytes pass again. Loosen the recorded mode for the rewrite,
        // then restore 0o555 before verification.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&file, b"managed nix\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(
            verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
                .is_ok()
        );
    }

    #[test]
    fn forged_receipt_is_irrelevant_to_the_receipt_free_verifier() {
        let fixture = Fixture::new();
        // Forge a structurally valid receipt bound to a different digest.
        let mut value: serde_json::Value =
            serde_json::from_slice(&encode_ownership_receipt(&fixture.expectation).unwrap())
                .unwrap();
        value["assetManifestDigest"] = serde_json::json!(
            "sha256-ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        );
        fs::write(fixture.receipt(), serde_json::to_vec(&value).unwrap()).unwrap();

        // Receipt-bound verification must reject the forgery.
        assert_eq!(
            fixture.verify().unwrap_err().code(),
            OwnershipErrorCode::ReceiptMismatch
        );
        // The receipt-free verifier never inspects the receipt.
        assert!(
            verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
                .is_ok()
        );
    }
}
