//! macOS installation, peer authentication, and release contracts.

mod assets;
mod launchd;
#[cfg(test)]
mod tests;

use crate::{
    AssetPresence, BrokerHelperDispatch, LinuxHelperSession,
    platform::linux::{LinuxRootSetStore, provision_product_root_if_absent},
};
pub use assets::*;
pub use launchd::*;

/// Progress callback for one macOS install journal update.
pub(super) type MacOsPersistProgress<'a> =
    dyn FnMut(&crate::MacOsInstallJournal) -> Result<(), MacOsError> + 'a;
#[cfg(target_os = "macos")]
use nix::unistd::getpeereid;
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedHelper, AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig,
    BrokerHelperRequest, BrokerHelperResponse, BuildReadiness, MaintenanceError,
};
use std::{error::Error, fmt, os::unix::net::UnixStream};

pub(super) const BUILD_USER_COUNT: usize = 32;

/// Stable macOS platform/installer failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsErrorCode {
    /// `getpeereid` was unavailable or failed.
    PeerCredentialsUnavailable,
    /// The peer was not the configured service identity.
    UnauthenticatedPeer,
    /// The requested target was not a native Darwin system.
    UnsupportedPlatform,
    /// Existing Nix state was unmanaged or ambiguous.
    UnmanagedNix,
    /// The fixed privileged backend operation failed.
    BackendFailure,
    /// Sandbox, build users, or Apple toolchain readiness failed closed.
    BuildReadinessFailed,
    /// Installed product code failed its Developer ID verification contract.
    CodeSignatureInvalid,
    /// Service activation or the daemon readiness check failed.
    ServiceUnhealthy,
    /// Receipt-last publication failed.
    ReceiptFailure,
    /// Exact reverse-order rollback did not fully succeed.
    RollbackIncomplete,
}

/// Redacted macOS platform failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsError {
    pub(super) code: MacOsErrorCode,
}

impl MacOsError {
    pub(super) const fn new(code: MacOsErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a closed backend failure for platform implementations.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(MacOsErrorCode::BackendFailure)
    }

    pub(crate) const fn rollback_incomplete() -> Self {
        Self::new(MacOsErrorCode::RollbackIncomplete)
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS managed-runtime operation failed")
    }
}

impl Error for MacOsError {}

/// Kernel-authenticated effective identity for a connected Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsPeerCredentials {
    pub(super) uid: u32,
    pub(super) gid: u32,
}

impl MacOsPeerCredentials {
    /// Returns the peer's effective uid.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the peer's effective gid.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

/// Reads the effective uid/gid before any product frame is consumed.
///
/// # Errors
///
/// Returns `PeerCredentialsUnavailable` if the kernel query fails or this is
/// not a macOS build.
#[cfg(target_os = "macos")]
pub fn peer_credentials(stream: &UnixStream) -> Result<MacOsPeerCredentials, MacOsError> {
    let (uid, gid) = getpeereid(stream)
        .map_err(|_| MacOsError::new(MacOsErrorCode::PeerCredentialsUnavailable))?;
    Ok(MacOsPeerCredentials {
        uid: uid.as_raw(),
        gid: gid.as_raw(),
    })
}

/// Reports the Darwin-only contract as unavailable on other build hosts.
///
/// # Errors
///
/// Always returns `PeerCredentialsUnavailable` outside macOS.
#[cfg(not(target_os = "macos"))]
pub const fn peer_credentials(_stream: &UnixStream) -> Result<MacOsPeerCredentials, MacOsError> {
    Err(MacOsError::new(MacOsErrorCode::PeerCredentialsUnavailable))
}

/// Requires the connected peer to be the singleton broker service uid.
///
/// # Errors
///
/// Returns a closed error when credentials are unavailable or the uid differs.
pub fn authenticate_broker_peer(
    stream: &UnixStream,
    broker_uid: u32,
) -> Result<MacOsPeerCredentials, MacOsError> {
    let peer = peer_credentials(stream)?;
    if peer.uid == broker_uid {
        Ok(peer)
    } else {
        Err(MacOsError::new(MacOsErrorCode::UnauthenticatedPeer))
    }
}

/// macOS wrapper over the shared crash-durable `/nix` GC-root implementation.
#[derive(Debug, Clone)]
pub struct MacOsRootSetStore {
    pub(super) inner: LinuxRootSetStore,
}

impl MacOsRootSetStore {
    /// Opens the product root tree and verifies root-owned safe ancestors.
    ///
    /// # Errors
    ///
    /// Returns a closed filesystem failure on unsafe or unavailable state.
    pub fn production() -> Result<Self, MacOsError> {
        provision_product_root_if_absent(std::path::Path::new("/nix/var/nix/gcroots"), 0)
            .map(|inner| Self { inner })
            .map_err(|_| MacOsError::new(MacOsErrorCode::BackendFailure))
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn new_at(path: std::path::PathBuf, owner_uid: u32) -> Result<Self, MacOsError> {
        LinuxRootSetStore::new_at(path, owner_uid)
            .map(|inner| Self { inner })
            .map_err(|_| MacOsError::new(MacOsErrorCode::BackendFailure))
    }
}

/// PR-39 helper state bound to the Darwin peer-authenticated transport.
pub struct MacOsHelperSession {
    pub(super) inner: LinuxHelperSession,
}

impl fmt::Debug for MacOsHelperSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MacOsHelperSession(<authenticated-private-state>)")
    }
}

impl MacOsHelperSession {
    /// Binds authenticated capability state to the durable Darwin root store.
    #[must_use]
    pub fn new(authenticated: AuthenticatedHelper, roots: MacOsRootSetStore) -> Self {
        Self {
            inner: LinuxHelperSession::new(authenticated, roots.inner),
        }
    }
}

impl BrokerHelperDispatch for MacOsHelperSession {
    fn dispatch(
        &self,
        request: BrokerHelperRequest,
    ) -> Result<BrokerHelperResponse, MaintenanceError> {
        self.inner.dispatch(request)
    }

    fn dispatch_build(
        &self,
        request: &pkg_nix::BuildRequest,
        deadline: std::time::Instant,
        cancelled: &std::sync::atomic::AtomicBool,
        progress: &mut dyn FnMut(
            pkg_nix::BuildProgressEstimate,
        ) -> Result<(), pkg_nix::NixAdapterError>,
    ) -> pkg_nix::RootNixResponse {
        self.inner
            .dispatch_build(request, deadline, cancelled, progress)
    }

    fn dispatch_root_nix(
        &self,
        request: pkg_nix::RootNixRequest,
        deadline: std::time::Instant,
    ) -> pkg_nix::RootNixResponse {
        self.inner.dispatch_root_nix(request, deadline)
    }
}

/// Closed privileged operations used by the macOS installer.
pub trait MacOsInstallBackend {
    /// Returns the durable product operation mode.
    fn install_mode(&self) -> crate::InstallMode {
        crate::InstallMode::FreshInstall
    }
    /// Rechecks the offline barrier before one product mutation.
    ///
    /// # Errors
    /// Returns a closed error when the product jobs are not offline.
    fn preflight_product_mutation(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    /// Binds exact authenticated product executable bytes in memory.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError>;
    /// Binds the exact authenticated managed-Nix configuration in memory.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError>;

    /// Binds the authenticated release identity in memory.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        release_identity_digest: Digest,
    ) -> Result<(), MacOsError>;

    /// Records that an authenticated install journal will be recovered.
    ///
    /// # Errors
    /// Returns a closed error when recovery state cannot be bound.
    fn begin_authenticated_recovery(&mut self, mode: crate::InstallMode) -> Result<(), MacOsError>;

    /// Verifies AuthorizationServices/sudo authority.
    ///
    /// # Errors
    /// Returns a closed error when privilege is unavailable.
    fn preflight_privilege(&mut self) -> Result<(), MacOsError>;
    /// Scans the production host, including `/nix`, profiles, Homebrew, and launchd.
    ///
    /// # Errors
    /// Returns a closed error for unmanaged, ambiguous, or unreadable evidence.
    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError>;
    /// Returns the fixed broker uid after exact account observation.
    ///
    /// # Errors
    /// Returns a closed error when the broker account is absent or changed.
    fn broker_uid(&mut self) -> Result<u32, MacOsError>;
    /// Classifies one fixed artifact without mutation.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_asset(&mut self, asset: MacOsInstallAsset) -> Result<AssetPresence, MacOsError>;
    /// Classifies the complete APFS/keychain/synthetic/record contract.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_store_volume(&mut self) -> Result<AssetPresence, MacOsError>;
    /// Classifies the authenticated managed runtime.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, MacOsError>;
    /// Classifies the four fixed launchd jobs.
    ///
    /// # Errors
    /// Returns a closed error for partial, unreadable, or ambiguous state.
    fn classify_services(&mut self) -> Result<AssetPresence, MacOsError>;
    /// Classifies the authenticated root ownership receipt.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or unbound state.
    fn classify_ownership_receipt(&mut self) -> Result<AssetPresence, MacOsError>;
    /// Removes one revalidated artifact during interrupted-install recovery.
    ///
    /// # Errors
    /// Returns a closed error unless the artifact is exact or safely absent.
    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError>;
    /// Removes the exact product-owned APFS state during recovery.
    ///
    /// # Errors
    /// Returns a closed error for foreign state or incomplete removal.
    fn recover_store_volume(&mut self) -> Result<(), MacOsError>;
    /// Removes the exact launchd activation state during recovery.
    ///
    /// # Errors
    /// Returns a closed error for partial state or incomplete deactivation.
    fn recover_services(&mut self) -> Result<(), MacOsError>;
    /// Removes the exact authenticated ownership receipt during recovery.
    ///
    /// # Errors
    /// Returns a closed error unless the receipt is exact or safely absent.
    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError>;
    /// Verifies authenticated release hashes before the first mutation.
    ///
    /// # Errors
    /// Returns a closed error when release authentication fails.
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError>;
    /// Creates/mounts the product-owned encrypted APFS store and journals the
    /// synthetic.conf/keychain/volume state before every mutation.
    ///
    /// # Errors
    /// Returns a closed error when the exact encrypted-volume contract fails.
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError>;
    /// Reverts only the APFS/keychain/synthetic state created by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact volume rollback is incomplete.
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError>;
    /// Creates or verifies one fixed artifact; journals before mutation.
    ///
    /// # Errors
    /// Returns a closed error when exact creation or verification fails.
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError>;
    /// Installs one exact compiled-in launchd plist; journals before mutation.
    ///
    /// # Errors
    /// Returns a closed error when exact installation fails.
    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError>;
    /// Installs the complete authenticated per-platform Nix configuration.
    ///
    /// # Errors
    /// Returns a closed error when rendering or atomic installation fails.
    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError>;
    /// Provisions the authenticated pinned Nix runtime.
    ///
    /// # Errors
    /// Returns a closed error when authenticated provisioning fails.
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError>;
    /// Rolls back runtime state created by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact rollback is incomplete.
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError>;
    /// Accepts Base Nix after the standard adapter proves readiness.
    ///
    /// # Errors
    /// Returns a closed error for an invalid or incomplete handoff.
    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError>;
    /// Verifies installed product executables' Developer ID requirement.
    ///
    /// # Errors
    /// Returns a closed error when any required signature is invalid.
    fn verify_installed_code(&mut self) -> Result<(), MacOsError>;
    /// Bootstraps fixed jobs and journals prior launchd state.
    ///
    /// # Errors
    /// Returns a closed error when a fixed launchd mutation fails.
    fn activate_services(&mut self) -> Result<bool, MacOsError>;
    /// Reverts only service state changed by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact launchd rollback is incomplete.
    fn rollback_services(&mut self) -> Result<(), MacOsError>;
    /// Performs the bounded managed-store health check.
    ///
    /// # Errors
    /// Returns a closed error when the daemon is not ready.
    fn check_managed_daemon(&mut self) -> Result<(), MacOsError>;
    /// Observes sandbox/config/build-user/toolchain readiness after activation.
    ///
    /// # Errors
    /// Returns a closed error when a required probe cannot be completed.
    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError>;
    /// Publishes the root-owned ownership receipt last.
    ///
    /// # Errors
    /// Returns a closed error when atomic receipt publication fails.
    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError>;
    /// Removes one exact artifact owned by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact rollback is incomplete.
    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError>;
    /// Returns the prior receipt digest for one owned file during upgrade.
    ///
    /// # Errors
    /// Returns a closed error when authenticated prior state is unavailable.
    fn prior_file_digest(
        &mut self,
        _asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        Ok(None)
    }
    /// Restores an authenticated prior product file.
    ///
    /// # Errors
    /// Returns a closed error when exact replacement recovery fails.
    fn recover_replaced_asset(
        &mut self,
        _asset: MacOsInstallAsset,
        _prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Keeps authenticated release bytes after interrupted explicit repair.
    ///
    /// # Errors
    /// Returns a closed error when exact replacement recovery fails.
    fn roll_forward_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Removes a replacement backup after receipt durability.
    ///
    /// # Errors
    /// Returns a closed error when exact backup removal fails.
    fn finalize_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Ok(())
    }
    /// Returns the digest of the authenticated prior product receipt.
    ///
    /// # Errors
    /// Returns a closed error when authenticated prior state is unavailable.
    fn prior_ownership_receipt_digest(&mut self) -> Result<Option<Digest>, MacOsError> {
        Ok(None)
    }
    /// Restores the authenticated prior product receipt.
    ///
    /// # Errors
    /// Returns a closed error when exact receipt recovery fails.
    fn recover_replaced_ownership_receipt(
        &mut self,
        _prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Keeps the authenticated candidate receipt after interrupted repair.
    ///
    /// # Errors
    /// Returns a closed error when exact receipt recovery fails.
    fn roll_forward_replaced_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
}

/// Sanitized idempotent installation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsInstallReport {
    pub(super) created_artifacts: usize,
    pub(super) existing_artifacts: usize,
}

impl MacOsInstallReport {
    /// Count created by this attempt.
    #[must_use]
    pub const fn created_artifacts(self) -> usize {
        self.created_artifacts
    }

    /// Count already exact before this attempt.
    #[must_use]
    pub const fn existing_artifacts(self) -> usize {
        self.existing_artifacts
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum InstallMutation {
    Asset(MacOsInstallAsset),
    Runtime,
    Services,
    OwnershipReceipt,
}

/// Executes authenticated, failure-atomic, receipt-last macOS installation.
///
/// # Errors
///
/// Returns a stable error for unsupported/unmanaged hosts, signature or
/// readiness failure, unhealthy services, receipt failure, or incomplete rollback.
#[allow(
    clippy::too_many_lines,
    reason = "one macOS install walks a closed vendor lifecycle sequence with per-phase rollback"
)]
pub fn install_macos(
    system: System,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<MacOsInstallReport, MacOsError> {
    preflight_macos(system, backend)?;
    let mut mutations = Vec::new();
    let mut created = 0_usize;
    let mut existing = 0_usize;
    let result = (|| {
        for asset in macos_product_install_assets()
            .filter(|asset| asset.kind != MacOsAssetKind::File && asset.id != "nix-root")
        {
            record_asset_result(backend, asset, &mut mutations, &mut created, &mut existing)?;
        }
        mutations.push(InstallMutation::Runtime);
        if !backend.provision_managed_runtime()? {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?;
        backend.accept_base_nix_handoff()?;
        let nix_root = macos_product_install_assets()
            .find(|asset| asset.id == "nix-root")
            .ok_or_else(MacOsError::backend_failure)?;
        record_asset_result(
            backend,
            nix_root,
            &mut mutations,
            &mut created,
            &mut existing,
        )?;
        for asset in macos_product_install_assets()
            .filter(|asset| asset.kind == MacOsAssetKind::File && asset.id != "uninstall-manifest")
        {
            mutations.push(InstallMutation::Asset(asset));
            let was_created = match asset.id {
                "helper-plist" => {
                    backend.install_launchd_plist(asset, MacOsLaunchdAssets::ROOT_HELPER)?
                }
                "broker-plist" => {
                    backend.install_launchd_plist(asset, MacOsLaunchdAssets::BROKER)?
                }
                _ => backend.ensure_asset(asset)?,
            };
            if was_created {
                created = created.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        backend
            .verify_installed_code()
            .map_err(|_| MacOsError::new(MacOsErrorCode::CodeSignatureInvalid))?;
        mutations.push(InstallMutation::Services);
        if !backend
            .activate_services()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?
        {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?;
        let receipt_presence = backend.classify_ownership_receipt()?;
        if receipt_presence == AssetPresence::Absent
            || backend.install_mode() == crate::InstallMode::OfflineUpgrade
        {
            mutations.push(InstallMutation::OwnershipReceipt);
        }
        let receipt_created = backend
            .publish_ownership_receipt()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ReceiptFailure))?;
        let expected_receipt_change = match backend.install_mode() {
            crate::InstallMode::FreshInstall => receipt_presence == AssetPresence::Absent,
            crate::InstallMode::OfflineUpgrade => true,
            crate::InstallMode::OfflineRepair => false,
        };
        if receipt_created != expected_receipt_change {
            return Err(MacOsError::backend_failure());
        }
        Ok(MacOsInstallReport {
            created_artifacts: created,
            existing_artifacts: existing,
        })
    })();

    if result.is_err() {
        let mut rollback_incomplete = false;
        for mutation in mutations.into_iter().rev() {
            let rollback = match mutation {
                InstallMutation::Asset(asset) => backend.rollback_asset(asset),
                InstallMutation::Runtime => backend.rollback_managed_runtime(),
                InstallMutation::Services => backend.rollback_services(),
                InstallMutation::OwnershipReceipt => backend.recover_ownership_receipt(),
            };
            if rollback.is_err() {
                rollback_incomplete = true;
            }
        }
        if rollback_incomplete {
            return Err(MacOsError::new(MacOsErrorCode::RollbackIncomplete));
        }
    }
    result
}

/// Reverts one interrupted authenticated macOS installation from durable state.
///
/// # Errors
///
/// Returns a redacted failure for changed, foreign, ambiguous, or incomplete state.
pub fn recover_macos_install(
    journal: &mut crate::MacOsInstallJournal,
    backend: &mut dyn MacOsInstallBackend,
    recover_runtime: &mut dyn FnMut() -> Result<(), MacOsError>,
    persist_progress: &mut MacOsPersistProgress<'_>,
) -> Result<(), MacOsError> {
    while let Some((mutation, disposition, prior_digest)) =
        journal
            .recovery_actions()
            .first()
            .map(|action| match action {
                crate::MacOsInstallRecoveryAction::RevalidateIntended(mutation) => {
                    ((*mutation).clone(), 0_u8, None)
                }
                crate::MacOsInstallRecoveryAction::RevertCreated(mutation) => {
                    ((*mutation).clone(), 1, None)
                }
                crate::MacOsInstallRecoveryAction::RestoreReplaced(mutation, digest) => {
                    ((*mutation).clone(), 2, Some(*digest))
                }
                crate::MacOsInstallRecoveryAction::RollForwardReplaced(mutation) => {
                    ((*mutation).clone(), 3, None)
                }
            })
    {
        match &mutation {
            crate::MacOsInstallMutation::Asset { id } => {
                let asset = macos_asset_by_id(id)?;
                match disposition {
                    0 if backend.classify_asset(asset)? == AssetPresence::ExactPresent => {
                        backend.recover_asset(asset)?;
                    }
                    0 => {}
                    1 => backend.recover_asset(asset)?,
                    2 => backend.recover_replaced_asset(
                        asset,
                        prior_digest.ok_or_else(MacOsError::backend_failure)?,
                    )?,
                    3 => backend.roll_forward_replaced_asset(asset)?,
                    _ => return Err(MacOsError::backend_failure()),
                }
            }
            crate::MacOsInstallMutation::StoreVolume => {
                if disposition == 1
                    || backend.classify_store_volume()? == AssetPresence::ExactPresent
                {
                    backend.recover_store_volume()?;
                }
            }
            crate::MacOsInstallMutation::ManagedRuntime => recover_runtime()?,
            crate::MacOsInstallMutation::Services => {
                if disposition == 1 || backend.classify_services()? == AssetPresence::ExactPresent {
                    backend.recover_services()?;
                }
            }
            crate::MacOsInstallMutation::OwnershipReceipt => match disposition {
                0 if backend.classify_ownership_receipt()? == AssetPresence::ExactPresent => {
                    backend.recover_ownership_receipt()?;
                }
                0 => {}
                1 => backend.recover_ownership_receipt()?,
                2 => backend.recover_replaced_ownership_receipt(
                    prior_digest.ok_or_else(MacOsError::backend_failure)?,
                )?,
                3 => backend.roll_forward_replaced_ownership_receipt()?,
                _ => return Err(MacOsError::backend_failure()),
            },
        }
        journal
            .complete_recovery_action(&mutation)
            .map_err(|_| MacOsError::backend_failure())?;
        persist_progress(journal)?;
    }
    Ok(())
}

pub(super) fn macos_asset_by_id(id: &str) -> Result<MacOsInstallAsset, MacOsError> {
    MACOS_ASSETS
        .iter()
        .copied()
        .find(|asset| asset.id == id)
        .ok_or_else(MacOsError::backend_failure)
}

pub(super) fn preflight_macos(
    system: System,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<(), MacOsError> {
    if system != System::Aarch64Darwin {
        return Err(MacOsError::new(MacOsErrorCode::UnsupportedPlatform));
    }
    backend.preflight_privilege()?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| MacOsError::new(MacOsErrorCode::UnmanagedNix))?;
    backend.verify_release_bundle()
}

pub(super) fn record_asset_result(
    backend: &mut dyn MacOsInstallBackend,
    asset: MacOsInstallAsset,
    mutations: &mut Vec<InstallMutation>,
    created: &mut usize,
    existing: &mut usize,
) -> Result<(), MacOsError> {
    mutations.push(InstallMutation::Asset(asset));
    if backend.ensure_asset(asset)? {
        *created = created.saturating_add(1);
    } else {
        let _ = mutations.pop();
        *existing = existing.saturating_add(1);
    }
    Ok(())
}
