//! Idempotent Linux installation orchestration over a closed privileged API.

/// Progress callback for one Linux install journal update.
type LinuxPersistProgress<'a> = dyn FnMut(&LinuxInstallJournal) -> Result<(), InstallError> + 'a;

use crate::{
    AssetPresence, InstallMode,
    assets::{
        LinuxAssetKind, LinuxInstallAsset, LinuxSystemdAssets, is_linux_product_gcroots_asset,
        is_linux_service_runtime_asset, linux_install_assets, linux_product_mutation_assets,
    },
    linux_install_journal::{
        LinuxInstallJournal, LinuxInstallMutation, LinuxInstallRecoveryAction,
    },
};
use pkg_core::{System, state::Digest};
use pkg_nix::{AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig};
use std::{error::Error, fmt};

/// Stable Linux installation failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallErrorCode {
    /// The requested platform is not Linux.
    UnsupportedPlatform,
    /// Unmanaged or ambiguous Nix evidence makes installation unsafe.
    UnmanagedNix,
    /// A closed backend operation failed.
    BackendFailure,
    /// Service activation or the final daemon check failed.
    ServiceUnhealthy,
    /// Receipt-last publication failed after artifact verification.
    ReceiptFailure,
    /// Rollback could not remove every artifact created by this attempt.
    RollbackIncomplete,
    /// Existing product services are not safely offline for existing-install work.
    OfflineServicesRequired,
    /// Durable recovery mode does not match the requested operation.
    RecoveryModeMismatch,
    /// Durable recovery state uses a schema this installer cannot change safely.
    UnsupportedRecoverySchema,
    /// Accepted Base Nix is safe; product installation must continue from its retained journal.
    FreshRecoveryRetained,
}

/// Redacted installer error carrying no host path or command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallError {
    code: InstallErrorCode,
}

impl InstallError {
    const fn new(code: InstallErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a closed backend failure for platform implementations.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(InstallErrorCode::BackendFailure)
    }

    pub(crate) const fn rollback_incomplete() -> Self {
        Self::new(InstallErrorCode::RollbackIncomplete)
    }

    pub(crate) const fn offline_services_required() -> Self {
        Self::new(InstallErrorCode::OfflineServicesRequired)
    }

    pub(crate) const fn recovery_mode_mismatch() -> Self {
        Self::new(InstallErrorCode::RecoveryModeMismatch)
    }

    pub(crate) const fn unsupported_recovery_schema() -> Self {
        Self::new(InstallErrorCode::UnsupportedRecoverySchema)
    }

    pub(crate) const fn fresh_recovery_retained() -> Self {
        Self::new(InstallErrorCode::FreshRecoveryRetained)
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> InstallErrorCode {
        self.code
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.code {
            InstallErrorCode::OfflineServicesRequired => {
                "product services must be inactive and disabled with exact unit fragments and no drop-ins"
            }
            InstallErrorCode::RecoveryModeMismatch => {
                "install recovery mode does not match the requested operation"
            }
            InstallErrorCode::UnsupportedRecoverySchema => {
                "install recovery state uses an unsupported schema; keep it unchanged and use the matching installer"
            }
            InstallErrorCode::FreshRecoveryRetained => {
                "Base Nix is ready, but product installation is incomplete; run pkg-install again"
            }
            InstallErrorCode::UnsupportedPlatform
            | InstallErrorCode::UnmanagedNix
            | InstallErrorCode::BackendFailure
            | InstallErrorCode::ServiceUnhealthy
            | InstallErrorCode::ReceiptFailure
            | InstallErrorCode::RollbackIncomplete => "linux installation failed",
        })
    }
}

impl Error for InstallError {}

/// Closed privileged operations used by the Linux installer.
///
/// Every artifact value originates from [`linux_install_assets`]; the trait
/// carries no arbitrary path, command, unit text, user, or group input.
pub trait LinuxInstallBackend {
    /// Returns the policy for new work started by this invocation.
    fn install_mode(&self) -> InstallMode;

    /// Returns whether this is an exact, healthy, active installation of the
    /// authenticated release. This query must not mutate the host.
    ///
    /// # Errors
    ///
    /// Returns a backend error for partial, changed, mixed, or unhealthy state.
    fn classify_active_install(&mut self) -> Result<bool, InstallError> {
        Ok(false)
    }

    /// Revalidates privilege, handoff, and host state for the journal policy.
    ///
    /// This rejects a journal whose mode differs from the requested operation.
    ///
    /// # Errors
    ///
    /// Returns a backend error when durable recovery authority is absent or stale.
    fn preflight_recovery(
        &mut self,
        _mode: InstallMode,
        _system: System,
    ) -> Result<(), InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Rechecks the existing-install offline contract immediately before a file mutation.
    /// Fresh installation is the only mode where this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a backend error unless every fixed product unit is safely offline.
    fn preflight_product_mutation(&mut self) -> Result<(), InstallError>;

    /// Rechecks the query-only service boundary during fresh recovery.
    ///
    /// # Errors
    ///
    /// Returns a backend error when a recorded unit is missing or foreign, or any unit is online.
    fn preflight_fresh_recovery_mutation(
        &mut self,
        _journal: &LinuxInstallJournal,
    ) -> Result<(), InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Binds the exact product binaries authenticated by the release bundle.
    ///
    /// This must not mutate the host. The default refuses the operation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for a wrong-platform or conflicting binding.
    fn bind_authenticated_installer_payloads(
        &mut self,
        _payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Binds the exact authenticated managed-Nix configuration in memory.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for a wrong-platform or conflicting binding.
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError>;

    /// Binds the authenticated release identity used by Linux receipts and uninstall.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for a wrong-platform or conflicting binding.
    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError>;

    /// Verifies this process has the fixed privileged installer authority.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when root/polkit authority is absent.
    fn preflight_privilege(&mut self) -> Result<(), InstallError>;

    /// Scans the real production host as the privileged installer immediately
    /// before its first mutation. The backend accepts no caller-selected root.
    ///
    /// # Errors
    ///
    /// Returns `UnmanagedNix` for unmanaged, ambiguous, or unreadable evidence.
    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError>;

    /// Returns the revalidated non-root broker uid after account creation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend failure if the account cannot be revalidated.
    fn broker_uid(&mut self) -> Result<u32, InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Classifies one fixed asset without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the asset is conflicting, unsafe,
    /// or unreadable.
    fn classify_asset(&mut self, asset: LinuxInstallAsset) -> Result<AssetPresence, InstallError>;
    /// Classifies the authenticated ownership receipt without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the receipt is absent from the closed asset set or is unsafe.
    fn classify_ownership_receipt(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<AssetPresence, InstallError> {
        self.classify_asset(asset)
    }

    /// Classifies the authenticated Base Nix installation without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for partial, changed, or unreadable state.
    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Classifies the complete fixed service set without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for mixed or unreadable state.
    fn classify_services(&mut self) -> Result<AssetPresence, InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Returns whether this invocation must change the fixed service set.
    fn services_need_mutation(&self, prior_active: bool) -> bool {
        !prior_active
    }

    /// Recovers one exact fixed asset from durable journal authority.
    ///
    /// This operation must not use in-memory attempt ownership. It must reopen
    /// and verify the current object before mutation. Absence is safe.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the object is unsafe, changed, or
    /// cannot be removed.
    fn recover_asset(&mut self, _asset: LinuxInstallAsset) -> Result<(), InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Converges and verifies the complete same-release repair asset set.
    ///
    /// # Errors
    ///
    /// Returns a backend error unless every product asset becomes exact.
    fn recover_repair_assets(&mut self) -> Result<(), InstallError>;

    /// Deactivates a partially activated fresh-install service set before file rollback.
    /// The backend must authenticate the complete candidate service asset set first.
    ///
    /// # Errors
    ///
    /// Returns a backend error when exact service authentication or deactivation fails.
    fn recover_fresh_services(&mut self) -> Result<(), InstallError>;

    /// Ensures one fixed artifact exists and returns whether this attempt created it.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the exact artifact cannot be verified or created.
    /// Before mutating, the backend must journal attempt ownership so
    /// [`Self::rollback_asset`] is safe after either `Ok` or `Err`.
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError>;

    /// Installs exact compiled-in unit bytes for one fixed unit artifact.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact unit installation fails.
    /// Partial attempt-owned writes must remain removable through
    /// [`Self::rollback_asset`].
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError>;

    /// Provisions the already authenticated managed-Nix runtime through PR-12.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when verified provisioning fails.
    /// Partial attempt-owned provisioning must remain removable through
    /// [`Self::rollback_managed_runtime`].
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError>;

    /// Reverts Base Nix created by this exact attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback is incomplete.
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError>;

    /// Validates the installed Base Nix runtime before accepting its durable handoff.
    ///
    /// # Errors
    ///
    /// Returns a backend error unless the installed Base Nix store responds correctly.
    fn validate_base_nix(&mut self) -> Result<(), InstallError>;

    /// Accepts the Base Nix handoff after installed-state proof and before product activation.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the durable handoff cannot move to Accepted.
    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError>;

    /// Enables and starts the fixed product units for `FreshInstall` only.
    /// Both offline modes only revalidate offline state and return `false`.
    ///
    /// Before each mutation the backend must journal enough prior state for
    /// [`Self::rollback_services`] to revert only this attempt. That rollback
    /// must remain safe and idempotent when this method returns an error before
    /// changing anything.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when fixed service activation fails.
    fn activate_services(&mut self) -> Result<bool, InstallError>;

    /// Reverts service enable/start changes journaled by this exact fresh attempt.
    /// Both offline modes reject this operation without service mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback is incomplete.
    fn rollback_services(&mut self) -> Result<(), InstallError>;

    /// Completes fresh-install service rollback after attempt-owned files are removed.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when systemd cannot forget the removed attempt units.
    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError>;

    /// Runs fixed product-service readiness for `FreshInstall`.
    /// Both offline modes only revalidate offline service state.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when the daemon is not ready.
    fn check_managed_daemon(&mut self) -> Result<(), InstallError>;

    /// Publishes the authenticated ownership receipt after all verification.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when receipt-last publication fails.
    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError>;

    /// Removes superseded product-file backups after the new receipt is durable.
    ///
    /// This operation must be idempotent. A caller must invoke it only after
    /// the install journal records the committed receipt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact post-commit cleanup is incomplete.
    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError>;

    /// Reverts one exact artifact created by this attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback fails.
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError>;
}

/// Sanitized result of one idempotent Linux installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxInstallReport {
    created_artifacts: usize,
    existing_artifacts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMutation {
    Asset(LinuxInstallAsset),
    ManagedRuntime,
    Services,
}

impl LinuxInstallReport {
    pub(crate) fn recovered_existing() -> Self {
        Self {
            created_artifacts: 0,
            existing_artifacts: linux_product_mutation_assets().count(),
        }
    }

    /// Returns how many allowlisted artifacts this attempt created.
    #[must_use]
    pub const fn created_artifacts(self) -> usize {
        self.created_artifacts
    }

    /// Returns how many allowlisted artifacts were already correct.
    #[must_use]
    pub const fn existing_artifacts(self) -> usize {
        self.existing_artifacts
    }
}

/// Executes the closed, receipt-last Linux installation sequence.
///
/// # Errors
///
/// Returns a stable install error for unsupported/non-clean hosts, backend or
/// service failure, receipt failure, or incomplete rollback.
#[cfg(test)]
pub fn install_linux(
    system: System,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    require_linux(system)?;
    if backend.install_mode() == InstallMode::OfflineRepair {
        return Err(InstallError::backend_failure());
    }
    backend.preflight_privilege()?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| InstallError::new(InstallErrorCode::UnmanagedNix))?;
    install_linux_preflighted(system, backend)
}

#[cfg(test)]
fn install_linux_preflighted(
    system: System,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    require_linux(system)?;
    if backend.install_mode() == InstallMode::OfflineRepair {
        return Err(InstallError::backend_failure());
    }
    install_linux_journaled_preflighted(backend)
}

pub fn install_linux_journaled_preflighted(
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    let mut mutations = Vec::new();
    let mut created_artifacts = 0_usize;
    let mut existing = 0_usize;
    let result = (|| {
        for asset in linux_product_mutation_assets().filter(|asset| {
            asset.kind() != LinuxAssetKind::File && !is_linux_product_gcroots_asset(*asset)
        }) {
            mutations.push(InstallMutation::Asset(asset));
            let was_created = backend.ensure_asset(asset)?;
            if was_created {
                created_artifacts = created_artifacts.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        for asset in linux_product_mutation_assets().filter(|asset| asset.id() == "nix-config") {
            mutations.push(InstallMutation::Asset(asset));
            if backend.ensure_asset(asset)? {
                created_artifacts = created_artifacts.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        mutations.push(InstallMutation::ManagedRuntime);
        if !backend.provision_managed_runtime()? {
            let _ = mutations.pop();
        }
        for asset in linux_product_mutation_assets().filter(|asset| {
            is_linux_product_gcroots_asset(*asset)
                || (asset.kind() == LinuxAssetKind::File
                    && !matches!(asset.id(), "nix-config" | "uninstall-manifest"))
        }) {
            mutations.push(InstallMutation::Asset(asset));
            let was_created = {
                if let Some(contents) = static_asset_contents(asset) {
                    backend.install_systemd_unit(asset, contents)?
                } else {
                    backend.ensure_asset(asset)?
                }
            };
            if was_created {
                created_artifacts = created_artifacts.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        backend.validate_base_nix()?;
        backend.accept_base_nix_handoff()?;
        mutations.push(InstallMutation::Services);
        let services_changed = backend
            .activate_services()
            .map_err(|_| InstallError::new(InstallErrorCode::ServiceUnhealthy))?;
        if !services_changed {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| InstallError::new(InstallErrorCode::ServiceUnhealthy))?;
        publish_linux_receipt(
            backend,
            &mut mutations,
            &mut created_artifacts,
            &mut existing,
        )?;
        Ok(LinuxInstallReport {
            created_artifacts,
            existing_artifacts: existing,
        })
    })();

    if result.is_err() {
        let mut rollback_incomplete = false;
        let mut services_changed = false;
        for mutation in mutations.into_iter().rev() {
            let rollback = match mutation {
                InstallMutation::Asset(asset) => backend.rollback_asset(asset),
                InstallMutation::ManagedRuntime => backend.rollback_managed_runtime(),
                InstallMutation::Services => {
                    services_changed = true;
                    if backend.rollback_services().is_err() {
                        return Err(InstallError::new(InstallErrorCode::RollbackIncomplete));
                    }
                    Ok(())
                }
            };
            if rollback.is_err() {
                rollback_incomplete = true;
            }
        }
        if services_changed && backend.finish_fresh_services_rollback().is_err() {
            rollback_incomplete = true;
        }
        if rollback_incomplete {
            return Err(InstallError::new(InstallErrorCode::RollbackIncomplete));
        }
    }
    result
}

#[cfg(test)]
const fn require_linux(system: System) -> Result<(), InstallError> {
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        Ok(())
    } else {
        Err(InstallError::new(InstallErrorCode::UnsupportedPlatform))
    }
}

/// Reverts one interrupted authenticated Linux installation from durable state.
///
/// This uses stateless, verified recovery operations. It does not use the
/// in-memory rollback records from the process that created the journal.
///
/// # Errors
///
/// Returns a redacted backend failure when any current object is unsafe,
/// changed, or cannot be restored to absence.
pub fn recover_linux_install(
    journal: &mut LinuxInstallJournal,
    backend: &mut dyn LinuxInstallBackend,
    recover_runtime: &mut dyn FnMut() -> Result<(), InstallError>,
    persist_progress: &mut LinuxPersistProgress<'_>,
) -> Result<(), InstallError> {
    let mode = journal.mode();
    let system = journal
        .system()
        .map_err(|_| InstallError::backend_failure())?;
    backend.preflight_recovery(mode, system)?;
    if mode == InstallMode::OfflineRepair {
        return backend.recover_repair_assets();
    }
    if mode == InstallMode::FreshInstall
        && !journal.fresh_services_deactivated()
        && journal.recovery_actions().iter().any(|action| {
            matches!(
                action,
                LinuxInstallRecoveryAction::RevalidateIntended(LinuxInstallMutation::Services)
                    | LinuxInstallRecoveryAction::RevertCreated(LinuxInstallMutation::Services)
            )
        })
    {
        require_exact_service_assets(backend)?;
        backend.recover_fresh_services()?;
        journal
            .mark_fresh_services_deactivated()
            .map_err(|_| InstallError::backend_failure())?;
        persist_progress(journal)?;
    }
    {
        while let Some((mutation, revalidate)) =
            journal
                .recovery_actions()
                .first()
                .map(|action| match action {
                    LinuxInstallRecoveryAction::RevalidateIntended(mutation) => {
                        ((*mutation).clone(), true)
                    }
                    LinuxInstallRecoveryAction::RevertCreated(mutation) => {
                        ((*mutation).clone(), false)
                    }
                })
        {
            if journal.fresh_services_deactivated() {
                backend.preflight_fresh_recovery_mutation(journal)?;
            }
            match &mutation {
                LinuxInstallMutation::Asset { id } => {
                    let asset = asset_by_id(id)?;
                    if !revalidate || backend.classify_asset(asset)? == AssetPresence::ExactPresent
                    {
                        backend.recover_asset(asset)?;
                    }
                }
                LinuxInstallMutation::ManagedRuntime => recover_runtime()?,
                LinuxInstallMutation::Services => {
                    if mode != InstallMode::FreshInstall {
                        return Err(InstallError::backend_failure());
                    }
                }
            }
            journal
                .complete_recovery_action(&mutation)
                .map_err(|_| InstallError::backend_failure())?;
            persist_progress(journal)?;
        }
        if journal
            .finish_recovery()
            .map_err(|_| InstallError::backend_failure())?
        {
            persist_progress(journal)?;
        }
        Ok(())
    }
}

fn asset_by_id(id: &str) -> Result<LinuxInstallAsset, InstallError> {
    linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == id)
        .ok_or_else(InstallError::backend_failure)
}

fn require_exact_service_assets(backend: &mut dyn LinuxInstallBackend) -> Result<(), InstallError> {
    for asset in
        linux_product_mutation_assets().filter(|asset| is_linux_service_runtime_asset(*asset))
    {
        if backend.classify_asset(asset)? != AssetPresence::ExactPresent {
            return Err(InstallError::backend_failure());
        }
    }
    Ok(())
}

fn publish_linux_receipt(
    backend: &mut dyn LinuxInstallBackend,
    mutations: &mut Vec<InstallMutation>,
    created_artifacts: &mut usize,
    existing_artifacts: &mut usize,
) -> Result<(), InstallError> {
    let asset = linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == "uninstall-manifest")
        .ok_or_else(InstallError::backend_failure)?;
    mutations.push(InstallMutation::Asset(asset));
    if backend
        .publish_ownership_receipt()
        .map_err(|_| InstallError::new(InstallErrorCode::ReceiptFailure))?
    {
        *created_artifacts = created_artifacts.saturating_add(1);
    } else {
        let _ = mutations.pop();
        *existing_artifacts = existing_artifacts.saturating_add(1);
    }
    Ok(())
}

fn static_asset_contents(asset: LinuxInstallAsset) -> Option<&'static str> {
    match asset.id() {
        "helper-socket-unit" => Some(LinuxSystemdAssets::HELPER_SOCKET),
        "helper-service-unit" => Some(LinuxSystemdAssets::HELPER_SERVICE),
        "broker-socket-unit" => Some(LinuxSystemdAssets::BROKER_SOCKET),
        "broker-service-unit" => Some(LinuxSystemdAssets::BROKER_SERVICE),
        "runtime-tmpfiles" => Some(LinuxSystemdAssets::TMPFILES),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
