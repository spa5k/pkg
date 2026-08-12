//! Bounded, fail-closed uninstall planning and execution.
//!
//! The recorded manifest carries only stable asset ids and creation state.
//! Paths, account names, and removal ordering always come from the compiled
//! platform allowlists, so a corrupted receipt cannot choose a deletion target.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use pkg_core::{System, state::Digest};
use serde::{Deserialize, Serialize};

use crate::{
    LinuxAssetKind, linux_install_assets,
    platform::macos::{MacOsAssetKind, macos_install_assets},
};

const MAX_RECORDED_ASSETS: usize = 256;
const MAX_ASSET_ID_BYTES: usize = 96;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MANIFEST_SCHEMA_VERSION: u8 = 1;
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
    const fn new(code: UninstallErrorCode) -> Self {
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
        Ok(Self { id, state })
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

        let mut states = BTreeMap::new();
        for asset in assets {
            if states.insert(asset.id, asset.state).is_some() {
                return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
            }
        }
        if states
            .keys()
            .map(String::as_str)
            .ne(expected.iter().map(|asset| asset.id))
        {
            return Err(UninstallError::new(UninstallErrorCode::InvalidManifest));
        }

        let assets = states
            .into_iter()
            .map(|(id, state)| RecordedAsset { id, state })
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

    /// Returns the authenticated managed-runtime asset-manifest digest.
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

/// Encodes the validated uninstall manifest into its canonical V1 disk form.
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

/// Decodes only the exact canonical V1 uninstall-manifest representation.
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
}

impl WireRecordedAsset {
    fn from_record(record: &RecordedAsset) -> Self {
        Self {
            id: record.id().to_owned(),
            state: match record.state() {
                RecordedAssetState::Created => WireRecordedAssetState::Created,
                RecordedAssetState::PreExisting => WireRecordedAssetState::PreExisting,
            },
        }
    }

    fn promote(self) -> Result<RecordedAsset, UninstallError> {
        let state = match self.state {
            WireRecordedAssetState::Created => RecordedAssetState::Created,
            WireRecordedAssetState::PreExisting => RecordedAssetState::PreExisting,
        };
        RecordedAsset::new(self.id, state)
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
    let states = manifest
        .assets
        .iter()
        .map(|asset| (asset.id.as_str(), asset.state))
        .collect::<BTreeMap<_, _>>();

    let mut removable = platform
        .into_iter()
        .filter(|asset| states.get(asset.id) == Some(&RecordedAssetState::Created))
        .filter(|asset| asset.id != "nix-root")
        .collect::<Vec<_>>();
    removable.sort_by(|left, right| removal_key(right).cmp(&removal_key(left)));

    let mut actions = vec![
        UninstallAction::StopServices,
        UninstallAction::RemoveUserRoots,
        UninstallAction::CollectGarbage,
        UninstallAction::RemoveRegisteredUserState,
    ];
    actions.extend(
        removable
            .into_iter()
            .map(|asset| UninstallAction::RemoveAsset {
                id: asset.id,
                kind: asset.kind,
                target: asset.target,
            }),
    );
    if states.get("nix-root") == Some(&RecordedAssetState::Created) {
        actions.push(UninstallAction::RemoveManagedStoreIfExclusive);
    }
    actions.push(UninstallAction::VerifyNoPrivilegedResidue);

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
        .map_err(|_| UninstallError::new(UninstallErrorCode::UnmanagedNix))?;

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
    for action in rest {
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
        UninstallAssetKind::Group => 1,
    };
    (phase, asset.target.matches('/').count(), asset.target)
}

fn platform_assets(system: System) -> Vec<PlatformAsset> {
    let mut assets: Vec<PlatformAsset> = match system {
        System::X8664Linux | System::Aarch64Linux => linux_install_assets()
            .iter()
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
        System::X8664Darwin | System::Aarch64Darwin => macos_install_assets()
            .iter()
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
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<String>,
        fail: Option<UninstallAction>,
        fail_preflight: Option<&'static str>,
    }

    impl UninstallBackend for FakeBackend {
        fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
            self.calls.push("privilege".into());
            if self.fail_preflight == Some("privilege") {
                Err(UninstallError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn verify_ownership(
            &mut self,
            _manifest: &UninstallManifest,
        ) -> Result<(), UninstallError> {
            self.calls.push("ownership".into());
            if self.fail_preflight == Some("ownership") {
                Err(UninstallError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
            self.calls.push("foreign-scan".into());
            if self.fail_preflight == Some("foreign") {
                Err(UninstallError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
            self.calls.push(format!("{action:?}"));
            if self.fail == Some(action) {
                Err(UninstallError::backend_failure())
            } else {
                Ok(())
            }
        }
    }

    fn manifest(
        system: System,
        state: RecordedAssetState,
    ) -> Result<UninstallManifest, UninstallError> {
        let assets = platform_assets(system)
            .into_iter()
            .map(|asset| RecordedAsset::new(asset.id, state))
            .collect::<Result<Vec<_>, _>>()?;
        UninstallManifest::new(system, Digest::from_bytes([7; 32]), assets)
    }

    fn error_code<T>(result: Result<T, UninstallError>) -> Option<UninstallErrorCode> {
        result.err().map(UninstallError::code)
    }

    #[test]
    fn manifest_requires_exact_complete_compiled_ids() -> Result<(), UninstallError> {
        let valid = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
        assert_eq!(valid.assets().len(), linux_install_assets().len());

        let mut missing = valid.assets().to_vec();
        missing.pop();
        assert_eq!(
            error_code(UninstallManifest::new(
                System::Aarch64Linux,
                Digest::from_bytes([7; 32]),
                missing
            )),
            Some(UninstallErrorCode::InvalidManifest)
        );

        let mut duplicate = valid.assets().to_vec();
        duplicate[0] = duplicate[1].clone();
        assert_eq!(
            error_code(UninstallManifest::new(
                System::Aarch64Linux,
                Digest::from_bytes([7; 32]),
                duplicate
            )),
            Some(UninstallErrorCode::InvalidManifest)
        );
        assert!(RecordedAsset::new("../../etc/passwd", RecordedAssetState::Created).is_err());
        Ok(())
    }

    #[test]
    fn uninstall_manifest_disk_form_is_strict_canonical_and_complete() -> Result<(), UninstallError>
    {
        let manifest = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
        let encoded = encode_uninstall_manifest(&manifest)?;
        assert_eq!(decode_uninstall_manifest(&encoded)?, manifest);
        assert!(encoded.ends_with(b"\n"));
        assert!(encoded.starts_with(b"{\"schemaVersion\":1,\"product\":\"pkg\","));

        let mut extended: serde_json::Value = serde_json::from_slice(&encoded)
            .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        extended["extension"] = serde_json::json!(true);
        let mut extended = serde_json::to_vec(&extended)
            .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
        extended.push(b'\n');
        assert_eq!(
            error_code(decode_uninstall_manifest(&extended)),
            Some(UninstallErrorCode::InvalidManifest)
        );
        assert_eq!(
            error_code(decode_uninstall_manifest(encoded.trim_ascii_end())),
            Some(UninstallErrorCode::InvalidManifest)
        );
        Ok(())
    }

    #[test]
    fn dry_run_is_deterministic_closed_and_non_mutating() -> Result<(), UninstallError> {
        for system in System::ALL {
            let manifest = manifest(system, RecordedAssetState::Created)?;
            let first = plan_uninstall(&manifest)?;
            let second = plan_uninstall(&manifest)?;
            assert_eq!(first, second);
            assert_eq!(
                first.actions().first(),
                Some(&UninstallAction::StopServices)
            );
            assert_eq!(
                first.actions().last(),
                Some(&UninstallAction::VerifyNoPrivilegedResidue)
            );
            assert!(
                first
                    .actions()
                    .contains(&UninstallAction::RemoveManagedStoreIfExclusive)
            );
            assert!(!first.actions().iter().any(|action| matches!(
                action,
                UninstallAction::RemoveAsset { target: "/nix", .. }
            )));
        }
        Ok(())
    }

    #[test]
    fn preexisting_assets_are_never_removal_targets() -> Result<(), UninstallError> {
        let manifest = manifest(System::Aarch64Darwin, RecordedAssetState::PreExisting)?;
        let plan = plan_uninstall(&manifest)?;
        assert!(!plan.actions().iter().any(|action| matches!(
            action,
            UninstallAction::RemoveAsset { .. } | UninstallAction::RemoveManagedStoreIfExclusive
        )));
        Ok(())
    }

    #[test]
    fn every_preflight_refusal_happens_before_mutation() -> Result<(), UninstallError> {
        let manifest = manifest(System::X8664Linux, RecordedAssetState::Created)?;
        let plan = plan_uninstall(&manifest)?;
        for (stage, code) in [
            ("privilege", UninstallErrorCode::PrivilegeRequired),
            ("ownership", UninstallErrorCode::OwnershipRefused),
            ("foreign", UninstallErrorCode::UnmanagedNix),
        ] {
            let mut backend = FakeBackend {
                fail_preflight: Some(stage),
                ..FakeBackend::default()
            };
            assert_eq!(
                error_code(execute_uninstall(&manifest, &plan, &mut backend)),
                Some(code)
            );
            assert!(backend.calls.iter().all(|call| {
                matches!(call.as_str(), "privilege" | "ownership" | "foreign-scan")
            }));
        }
        Ok(())
    }

    #[test]
    fn service_stop_is_a_cleanup_barrier() -> Result<(), UninstallError> {
        let manifest = manifest(System::Aarch64Darwin, RecordedAssetState::Created)?;
        let plan = plan_uninstall(&manifest)?;
        let mut backend = FakeBackend {
            fail: Some(UninstallAction::StopServices),
            ..FakeBackend::default()
        };
        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut backend)),
            Some(UninstallErrorCode::ServiceStopFailed)
        );
        assert_eq!(backend.calls.len(), 4);
        Ok(())
    }

    #[test]
    fn cleanup_failures_do_not_skip_residue_verification() -> Result<(), UninstallError> {
        let manifest = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
        let plan = plan_uninstall(&manifest)?;
        let failed = plan
            .actions()
            .iter()
            .copied()
            .find(|action| matches!(action, UninstallAction::RemoveAsset { .. }))
            .ok_or_else(UninstallError::backend_failure)?;
        let mut backend = FakeBackend {
            fail: Some(failed),
            ..FakeBackend::default()
        };
        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut backend)),
            Some(UninstallErrorCode::CleanupIncomplete)
        );
        assert_eq!(
            backend.calls.last().map(String::as_str),
            Some("VerifyNoPrivilegedResidue")
        );
        Ok(())
    }

    #[test]
    fn residue_failure_has_priority_and_success_is_total() -> Result<(), UninstallError> {
        let manifest = manifest(System::X8664Darwin, RecordedAssetState::Created)?;
        let plan = plan_uninstall(&manifest)?;
        let mut residue = FakeBackend {
            fail: Some(UninstallAction::VerifyNoPrivilegedResidue),
            ..FakeBackend::default()
        };
        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut residue)),
            Some(UninstallErrorCode::ResidueRemaining)
        );

        let mut success = FakeBackend::default();
        let report = execute_uninstall(&manifest, &plan, &mut success)?;
        assert_eq!(report.completed_actions(), plan.actions().len());
        Ok(())
    }
}
