//! Bounded, fail-closed uninstall planning and execution.
//!
//! The recorded manifest carries stable asset ids, creation state, and exact
//! content identity for product files.
//! Paths, account names, and removal ordering always come from the compiled
//! platform allowlists, so a corrupted receipt cannot choose a deletion target.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::{
    LinuxAssetKind,
    assets::is_linux_product_asset,
    linux_install_assets,
    platform::macos::{MacOsAssetKind, macos_product_install_assets},
};

const MAX_RECORDED_ASSETS: usize = 256;
const MAX_ASSET_ID_BYTES: usize = 96;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MANIFEST_SCHEMA_VERSION: u8 = 2;
const PRODUCT: &str = "pkg";

/// Stable uninstall failure categories suitable for public reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallErrorCode {
    /// The recorded manifest is incomplete, duplicated, or otherwise invalid.
    InvalidManifest,
    /// The caller lacks the required administrative authority.
    PrivilegeRequired,
    /// The ownership receipt or authenticated asset set could not be verified.
    OwnershipRefused,
    /// Foreign or ambiguous Nix state was observed during the privileged scan.
    UnmanagedNix,
    /// Product services could not be stopped before destructive cleanup.
    ServiceStopFailed,
    /// One or more bounded cleanup operations failed.
    CleanupIncomplete,
    /// Privileged product residue remained after cleanup.
    ResidueRemaining,
}

/// Redacted uninstall error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UninstallError {
    code: UninstallErrorCode,
}

impl UninstallError {
    pub(super) const fn new(code: UninstallErrorCode) -> Self {
        Self { code }
    }

    /// Constructs one redacted backend failure for test and adapter boundaries.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(UninstallErrorCode::CleanupIncomplete)
    }

    /// Returns the stable public error category.
    #[must_use]
    pub const fn code(self) -> UninstallErrorCode {
        self.code
    }
}

impl fmt::Display for UninstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.code {
            UninstallErrorCode::InvalidManifest => "the uninstall manifest is invalid",
            UninstallErrorCode::PrivilegeRequired => "administrative authority is required",
            UninstallErrorCode::OwnershipRefused => "managed ownership could not be verified",
            UninstallErrorCode::UnmanagedNix => "foreign or ambiguous Nix state was detected",
            UninstallErrorCode::ServiceStopFailed => "managed services could not be stopped",
            UninstallErrorCode::CleanupIncomplete => "uninstall cleanup is incomplete",
            UninstallErrorCode::ResidueRemaining => "privileged product residue remains",
        })
    }
}

impl std::error::Error for UninstallError {}

/// Whether the installer created an asset or merely corroborated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedAssetState {
    /// The asset was created by pkg and is eligible for exact removal.
    Created,
    /// The asset existed beforehand and must be preserved.
    PreExisting,
}

/// One bounded manifest record. It deliberately contains no path or account name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAsset {
    id: String,
    state: RecordedAssetState,
    content_digest: Option<Digest>,
}

impl RecordedAsset {
    /// Creates an id-only asset record.
    ///
    /// # Errors
    ///
    /// Returns [`UninstallErrorCode::InvalidManifest`] when the id is empty,
    /// oversized, or outside the closed lowercase id grammar.
    pub fn new(id: impl Into<String>, state: RecordedAssetState) -> Result<Self, UninstallError> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_ASSET_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }
        Ok(Self {
            id,
            state,
            content_digest: None,
        })
    }

    /// Binds an exact installed-file identity to this record.
    #[must_use]
    pub(crate) const fn with_content_digest(mut self, digest: Digest) -> Self {
        self.content_digest = Some(digest);
        self
    }

    /// Returns the stable compiled-allowlist id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns whether pkg created the asset.
    #[must_use]
    pub const fn state(&self) -> RecordedAssetState {
        self.state
    }

    /// Returns the exact installed-file content identity, when the asset is a file.
    #[must_use]
    pub(crate) const fn content_digest(&self) -> Option<Digest> {
        self.content_digest
    }
}

/// Validated receipt input used to construct an uninstall plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallManifest {
    system: System,
    ownership_manifest_digest: Digest,
    assets: Vec<RecordedAsset>,
}

impl UninstallManifest {
    /// Validates exact, complete coverage of the compiled platform asset set.
    ///
    /// # Errors
    ///
    /// Returns [`UninstallErrorCode::InvalidManifest`] for missing, duplicate,
    /// unknown, or excessive records.
    pub fn new(
        system: System,
        ownership_manifest_digest: Digest,
        assets: Vec<RecordedAsset>,
    ) -> Result<Self, UninstallError> {
        let expected = platform_assets(system);
        if assets.len() > MAX_RECORDED_ASSETS || assets.len() != expected.len() {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }

        let mut records = BTreeMap::new();
        for asset in assets {
            if records
                .insert(asset.id, (asset.state, asset.content_digest))
                .is_some()
            {
                return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
            }
        }
        if records
            .keys()
            .map(String::as_str)
            .ne(expected.iter().map(|asset| asset.id))
        {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }
        for asset in &expected {
            let has_digest = records
                .get(asset.id)
                .is_some_and(|(_, digest)| digest.is_some());
            let needs_digest = matches!(
                system,
                System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
            ) && asset.kind == UninstallAssetKind::File
                && asset.id != "uninstall-manifest";
            if has_digest != needs_digest {
                return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
            }
        }
        if matches!(
            system,
            System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
        ) && records
            .get("uninstall-manifest")
            .is_none_or(|(state, _)| *state != RecordedAssetState::Created)
        {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }
        if system == System::Aarch64Darwin
            && records
                .get("nix-root")
                .is_none_or(|(state, _)| *state != RecordedAssetState::PreExisting)
        {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }

        let assets = records
            .into_iter()
            .map(|(id, (state, content_digest))| RecordedAsset {
                id,
                state,
                content_digest,
            })
            .collect();
        Ok(Self {
            system,
            ownership_manifest_digest,
            assets,
        })
    }

    /// Returns the target system bound into the root-owned receipt.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the authenticated release ownership identity.
    #[must_use]
    pub const fn ownership_manifest_digest(&self) -> Digest {
        self.ownership_manifest_digest
    }

    /// Returns the canonical id-sorted asset records.
    #[must_use]
    pub fn assets(&self) -> &[RecordedAsset] {
        &self.assets
    }
}

/// Encodes the validated uninstall manifest into its canonical V2 disk form.
///
/// # Errors
///
/// Returns [`UninstallErrorCode::InvalidManifest`] if serialization exceeds the
/// fixed receipt bound.
pub fn encode_uninstall_manifest(manifest: &UninstallManifest) -> Result<Vec<u8>, UninstallError> {
    let wire = WireManifest::from_manifest(manifest);
    let mut bytes = serde_json::to_vec(&wire)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    }
    Ok(bytes)
}

/// Decodes only the exact canonical V2 uninstall-manifest representation.
///
/// # Errors
///
/// Returns [`UninstallErrorCode::InvalidManifest`] for malformed, extended,
/// non-canonical, oversized, incomplete, duplicate, or unknown records.
pub fn decode_uninstall_manifest(bytes: &[u8]) -> Result<UninstallManifest, UninstallError> {
    if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    }
    let wire: WireManifest = serde_json::from_slice(bytes)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let manifest = wire.promote()?;
    if encode_uninstall_manifest(&manifest)? != bytes {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    }
    Ok(manifest)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireManifest {
    schema_version: u8,
    product: String,
    system: String,
    ownership_manifest_digest: String,
    assets: Vec<WireRecordedAsset>,
}

impl WireManifest {
    fn from_manifest(manifest: &UninstallManifest) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            product: PRODUCT.to_owned(),
            system: manifest.system().as_str().to_owned(),
            ownership_manifest_digest: manifest.ownership_manifest_digest().to_string(),
            assets: manifest
                .assets()
                .iter()
                .map(WireRecordedAsset::from_record)
                .collect(),
        }
    }

    fn promote(self) -> Result<UninstallManifest, UninstallError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION || self.product != PRODUCT {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }
        let system = System::from_str(&self.system)
            .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        let digest = Digest::from_str(&self.ownership_manifest_digest)
            .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        let assets = self
            .assets
            .into_iter()
            .map(WireRecordedAsset::promote)
            .collect::<Result<Vec<_>, _>>()?;
        UninstallManifest::new(system, digest, assets)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRecordedAsset {
    id: String,
    state: WireRecordedAssetState,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_digest: Option<String>,
}

impl WireRecordedAsset {
    fn from_record(record: &RecordedAsset) -> Self {
        Self {
            id: record.id().to_owned(),
            state: match record.state() {
                RecordedAssetState::Created => WireRecordedAssetState::Created,
                RecordedAssetState::PreExisting => WireRecordedAssetState::PreExisting,
            },
            content_digest: record.content_digest().map(|digest| digest.to_string()),
        }
    }

    fn promote(self) -> Result<RecordedAsset, UninstallError> {
        let state = match self.state {
            WireRecordedAssetState::Created => RecordedAssetState::Created,
            WireRecordedAssetState::PreExisting => RecordedAssetState::PreExisting,
        };
        let record = RecordedAsset::new(self.id, state)?;
        match self.content_digest {
            Some(digest) => Digest::from_str(&digest)
                .map(|digest| record.with_content_digest(digest))
                .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest)),
            None => Ok(record),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum WireRecordedAssetState {
    Created,
    PreExisting,
}

/// Closed target kind used in dry-run output and privileged dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UninstallAssetKind {
    /// A regular file or service definition.
    File,
    /// A directory, removed only after its recorded children.
    Directory,
    /// A fixed non-login system account.
    User,
    /// A fixed system group.
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformAsset {
    id: &'static str,
    kind: UninstallAssetKind,
    target: &'static str,
}

/// One closed uninstall operation. Every target is resolved from compiled data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallAction {
    /// Stop and disable every product-owned service before removing binaries.
    StopServices,
    /// Remove only product-owned per-user GC roots through the helper contract.
    RemoveUserRoots,
    /// Run managed Nix garbage collection once after roots are removed.
    CollectGarbage,
    /// Remove a single exact compiled asset.
    RemoveAsset {
        /// Stable manifest id.
        id: &'static str,
        /// Closed asset kind.
        kind: UninstallAssetKind,
        /// Exact compiled path or account name.
        target: &'static str,
    },
    /// Remove state belonging to users registered by pkg, never arbitrary homes.
    RemoveRegisteredUserState,
    /// Remove `/nix` (or the Darwin volume) only after proving exclusive ownership.
    RemoveManagedStoreIfExclusive,
    /// Remove only the product runtime when the manifest records a pre-existing store.
    RemoveManagedRuntimePreservingStore,
    /// Replace the Linux process with the authenticated Determinate full uninstall.
    ExecDeterminateUninstall,
    /// Verify that no product service, helper, account, or privileged path remains.
    VerifyNoPrivilegedResidue,
}

/// Immutable dry-run preview and executable uninstall authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallPlan {
    system: System,
    ownership_manifest_digest: Digest,
    actions: Vec<UninstallAction>,
}

impl UninstallPlan {
    /// Returns the target platform.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the release-authenticated ownership-manifest binding.
    #[must_use]
    pub const fn ownership_manifest_digest(&self) -> Digest {
        self.ownership_manifest_digest
    }

    /// Returns the exact ordered operations for `--dry-run` display.
    #[must_use]
    pub fn actions(&self) -> &[UninstallAction] {
        &self.actions
    }
}

/// Produces a deterministic, non-mutating uninstall preview.
///
/// # Errors
///
/// Returns [`UninstallErrorCode::InvalidManifest`] if the validated manifest
/// cannot be resolved to the exact compiled platform allowlist.
pub fn plan_uninstall(manifest: &UninstallManifest) -> Result<UninstallPlan, UninstallError> {
    let platform = platform_assets(manifest.system);
    let determinate_system = matches!(
        manifest.system,
        System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
    );
    let receipt_last = determinate_system;
    let states = manifest
        .assets
        .iter()
        .map(|asset| (asset.id.as_str(), asset.state))
        .collect::<BTreeMap<_, _>>();

    let mut removable = platform
        .iter()
        .copied()
        .filter(|asset| states.get(asset.id) == Some(&RecordedAssetState::Created))
        .filter(|asset| !receipt_last || asset.id != "uninstall-manifest")
        .collect::<Vec<_>>();
    removable.sort_by(|left, right| removal_key(right).cmp(&removal_key(left)));
    let manifest_asset = platform
        .into_iter()
        .find(|asset| asset.id == "uninstall-manifest");
    let mut manifest_parents = Vec::new();
    if receipt_last && states.get("uninstall-manifest") == Some(&RecordedAssetState::Created) {
        let manifest_asset = manifest_asset
            .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        removable.retain(|asset| {
            let defer = asset.kind == UninstallAssetKind::Directory
                && Path::new(manifest_asset.target).starts_with(asset.target);
            if defer {
                manifest_parents.push(*asset);
            }
            !defer
        });
    }

    let nix_root_state = states
        .get("nix-root")
        .copied()
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let mut actions = vec![UninstallAction::StopServices];
    let mut terminal_vendor = false;
    match nix_root_state {
        RecordedAssetState::Created => {
            actions.push(UninstallAction::RemoveManagedStoreIfExclusive);
        }
        RecordedAssetState::PreExisting => {
            actions.push(UninstallAction::RemoveUserRoots);
            if determinate_system {
                terminal_vendor = true;
            } else {
                actions.push(UninstallAction::RemoveManagedRuntimePreservingStore);
            }
        }
    }
    actions.push(UninstallAction::RemoveRegisteredUserState);
    actions.extend(
        removable
            .into_iter()
            .map(|asset| UninstallAction::RemoveAsset {
                id: asset.id,
                kind: asset.kind,
                target: asset.target,
            }),
    );
    if receipt_last && states.get("uninstall-manifest") == Some(&RecordedAssetState::Created) {
        let manifest_asset = manifest_asset
            .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        actions.push(UninstallAction::RemoveAsset {
            id: manifest_asset.id,
            kind: manifest_asset.kind,
            target: manifest_asset.target,
        });
        actions.extend(
            manifest_parents
                .into_iter()
                .map(|asset| UninstallAction::RemoveAsset {
                    id: asset.id,
                    kind: asset.kind,
                    target: asset.target,
                }),
        );
    }
    actions.push(UninstallAction::VerifyNoPrivilegedResidue);
    if terminal_vendor {
        actions.push(UninstallAction::ExecDeterminateUninstall);
    }

    Ok(UninstallPlan {
        system: manifest.system,
        ownership_manifest_digest: manifest.ownership_manifest_digest,
        actions,
    })
}

/// Trusted platform boundary for privileged uninstall work.
pub trait UninstallBackend {
    /// Confirms administrative authority without mutation.
    ///
    /// # Errors
    /// Returns a redacted error when administrative authority is unavailable.
    fn preflight_privilege(&mut self) -> Result<(), UninstallError>;
    /// Revalidates the root-owned receipt and authenticated asset set.
    ///
    /// # Errors
    /// Returns a redacted error when ownership cannot be corroborated.
    fn verify_ownership(&mut self, manifest: &UninstallManifest) -> Result<(), UninstallError>;
    /// Refuses foreign or ambiguous Nix state immediately before mutation.
    ///
    /// # Errors
    /// Returns a redacted error when foreign or ambiguous state is present.
    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError>;
    /// Executes one closed action without accepting caller-supplied paths.
    ///
    /// # Errors
    /// Returns a redacted error when the exact operation cannot complete.
    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError>;
}

/// Successful uninstall summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UninstallReport {
    completed_actions: usize,
}

pub fn preflight_uninstall(
    manifest: &UninstallManifest,
    plan: &UninstallPlan,
    backend: &mut dyn UninstallBackend,
) -> Result<(), UninstallError> {
    if plan.system != manifest.system
        || plan.ownership_manifest_digest != manifest.ownership_manifest_digest
        || plan.actions != plan_uninstall(manifest)?.actions
    {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    }

    backend
        .preflight_privilege()
        .map_err(|_| UninstallError::new(UninstallErrorCode::PrivilegeRequired))?;
    backend
        .verify_ownership(manifest)
        .map_err(|_| UninstallError::new(UninstallErrorCode::OwnershipRefused))?;
    backend
        .preflight_unmanaged_nix()
        .map_err(|_| UninstallError::new(UninstallErrorCode::UnmanagedNix))
}

impl UninstallReport {
    /// Returns the number of completed closed operations, including residue verification.
    #[must_use]
    pub const fn completed_actions(self) -> usize {
        self.completed_actions
    }
}

/// Executes an already-previewed plan with a last-moment fail-closed preflight.
///
/// # Errors
///
/// Returns a closed error if the plan binding differs, any preflight refuses,
/// services cannot stop, cleanup is incomplete, or privileged residue remains.
pub fn execute_uninstall(
    manifest: &UninstallManifest,
    plan: &UninstallPlan,
    backend: &mut dyn UninstallBackend,
) -> Result<UninstallReport, UninstallError> {
    preflight_uninstall(manifest, plan, backend)?;

    let mut completed = 0;
    let Some((first, rest)) = plan.actions.split_first() else {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    };
    if *first != UninstallAction::StopServices {
        return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
    }
    backend
        .execute(*first)
        .map_err(|_| UninstallError::new(UninstallErrorCode::ServiceStopFailed))?;
    completed += 1;

    let mut cleanup_failed = false;
    let mut residue_failed = false;
    let terminal_vendor = matches!(
        plan.system,
        System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
    ) && plan.actions.last()
        == Some(&UninstallAction::ExecDeterminateUninstall);
    for action in rest {
        if cleanup_failed
            && matches!(
                action,
                UninstallAction::RemoveAsset {
                    id: "uninstall-manifest",
                    ..
                }
            )
        {
            continue;
        }
        if terminal_vendor
            && *action == UninstallAction::ExecDeterminateUninstall
            && (cleanup_failed || residue_failed)
        {
            continue;
        }
        match backend.execute(*action) {
            Ok(()) => completed += 1,
            Err(_) if *action == UninstallAction::VerifyNoPrivilegedResidue => {
                residue_failed = true;
            }
            Err(_) => cleanup_failed = true,
        }
    }
    if residue_failed {
        return Err(UninstallError::new(UninstallErrorCode::ResidueRemaining));
    }
    if cleanup_failed {
        return Err(UninstallError::new(UninstallErrorCode::CleanupIncomplete));
    }
    Ok(UninstallReport {
        completed_actions: completed,
    })
}

fn removal_key(asset: &PlatformAsset) -> (u8, usize, &'static str) {
    let phase = match asset.kind {
        UninstallAssetKind::File => 4,
        UninstallAssetKind::Directory => 3,
        UninstallAssetKind::User => 2,
        UninstallAssetKind::Group => 0,
    };
    let phase = if asset.kind == UninstallAssetKind::User && asset.target == "pkg-nix-broker" {
        1
    } else {
        phase
    };
    (phase, asset.target.matches('/').count(), asset.target)
}

fn platform_assets(system: System) -> Vec<PlatformAsset> {
    let mut assets: Vec<PlatformAsset> = match system {
        System::X8664Linux | System::Aarch64Linux => linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| is_linux_product_asset(*asset))
            .map(|asset| PlatformAsset {
                id: asset.id(),
                kind: match asset.kind() {
                    LinuxAssetKind::File => UninstallAssetKind::File,
                    LinuxAssetKind::Directory => UninstallAssetKind::Directory,
                    LinuxAssetKind::User => UninstallAssetKind::User,
                    LinuxAssetKind::Group => UninstallAssetKind::Group,
                },
                target: asset.path_or_name(),
            })
            .collect(),
        System::X8664Darwin => Vec::new(),
        System::Aarch64Darwin => macos_product_install_assets()
            .map(|asset| PlatformAsset {
                id: asset.id(),
                kind: match asset.kind() {
                    MacOsAssetKind::File => UninstallAssetKind::File,
                    MacOsAssetKind::Directory => UninstallAssetKind::Directory,
                    MacOsAssetKind::User => UninstallAssetKind::User,
                    MacOsAssetKind::Group => UninstallAssetKind::Group,
                },
                target: asset.path_or_name(),
            })
            .collect(),
    };
    assets.sort_by_key(|asset| asset.id);
    assets
}

#[cfg(test)]
mod tests;
