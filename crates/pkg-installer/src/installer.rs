//! Idempotent Linux installation orchestration over a closed privileged API.

use crate::{
    LinuxAssetPresence,
    assets::{
        LinuxAssetKind, LinuxInstallAsset, LinuxSystemdAssets, linux_install_assets,
        linux_product_mutation_assets,
    },
    linux_install_journal::{
        LinuxInstallJournal, LinuxInstallMode, LinuxInstallMutation, LinuxInstallRecoveryAction,
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
    fn install_mode(&self) -> LinuxInstallMode;

    /// Revalidates privilege, handoff, and host state for the journal policy.
    ///
    /// This rejects a journal whose mode differs from the requested operation.
    ///
    /// # Errors
    ///
    /// Returns a backend error when durable recovery authority is absent or stale.
    fn preflight_recovery(
        &mut self,
        _mode: LinuxInstallMode,
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
    fn preflight_product_file_mutation(&mut self) -> Result<(), InstallError>;

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
    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError>;
    /// Classifies the authenticated ownership receipt without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the receipt is absent from the closed asset set or is unsafe.
    fn classify_ownership_receipt(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<LinuxAssetPresence, InstallError> {
        self.classify_asset(asset)
    }

    /// Classifies the authenticated managed runtime without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for partial, changed, or unreadable state.
    fn classify_managed_runtime(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        Err(InstallError::backend_failure())
    }

    /// Classifies the complete fixed service set without mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error for mixed or unreadable state.
    fn classify_services(&mut self) -> Result<LinuxAssetPresence, InstallError> {
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

    /// Reverts a managed runtime created by this exact attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted backend error when exact rollback is incomplete.
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError>;

    /// Validates the installed Base Nix runtime before accepting its durable handoff.
    ///
    /// # Errors
    ///
    /// Returns a backend error unless the installed managed Nix store responds correctly.
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
pub fn install_linux(
    system: System,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    require_linux(system)?;
    if backend.install_mode() == LinuxInstallMode::OfflineRepair {
        return Err(InstallError::backend_failure());
    }
    backend.preflight_privilege()?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| InstallError::new(InstallErrorCode::UnmanagedNix))?;
    install_linux_preflighted(system, backend)
}

pub fn install_linux_preflighted(
    system: System,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<LinuxInstallReport, InstallError> {
    require_linux(system)?;
    if backend.install_mode() == LinuxInstallMode::OfflineRepair {
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
        for asset in linux_product_mutation_assets()
            .filter(|asset| asset.kind() != LinuxAssetKind::File && asset.id() != "nix-gcroots")
        {
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
            asset.id() == "nix-gcroots"
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
    persist_progress: &mut dyn FnMut(&LinuxInstallJournal) -> Result<(), InstallError>,
) -> Result<(), InstallError> {
    let mode = journal.mode();
    let system = journal
        .system()
        .map_err(|_| InstallError::backend_failure())?;
    backend.preflight_recovery(mode, system)?;
    if mode == LinuxInstallMode::OfflineRepair {
        return backend.recover_repair_assets();
    }
    if mode == LinuxInstallMode::FreshInstall
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
            match &mutation {
                LinuxInstallMutation::Asset { id } => {
                    let asset = asset_by_id(id)?;
                    if !revalidate
                        || backend.classify_asset(asset)? == LinuxAssetPresence::ExactPresent
                    {
                        backend.recover_asset(asset)?;
                    }
                }
                LinuxInstallMutation::ManagedRuntime => recover_runtime()?,
                LinuxInstallMutation::Services => {
                    if mode != LinuxInstallMode::FreshInstall {
                        return Err(InstallError::backend_failure());
                    }
                }
            }
            journal
                .complete_recovery_action(&mutation)
                .map_err(|_| InstallError::backend_failure())?;
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
    for asset in linux_product_mutation_assets().filter(|asset| is_service_runtime_asset(*asset)) {
        if backend.classify_asset(asset)? != LinuxAssetPresence::ExactPresent {
            return Err(InstallError::backend_failure());
        }
    }
    Ok(())
}

fn is_service_runtime_asset(asset: LinuxInstallAsset) -> bool {
    static_asset_contents(asset).is_some()
        || matches!(asset.id(), "root-helper-binary" | "broker-binary")
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
mod tests {
    use super::*;
    use pkg_core::state::Digest;
    use std::{collections::BTreeSet, error::Error, io};

    struct FakeBackend {
        existing: BTreeSet<&'static str>,
        created: Vec<&'static str>,
        rolled_back: Vec<&'static str>,
        fail_after: Option<usize>,
        states: BTreeSet<&'static str>,
        rollback_events: Vec<&'static str>,
        rollback_failures: BTreeSet<&'static str>,
        fail_health_check: bool,
        fail_service_activation: bool,
        gcroots_present_at_runtime: bool,
        recovery_modes: Vec<LinuxInstallMode>,
    }

    impl FakeBackend {
        fn clean() -> Self {
            Self {
                existing: BTreeSet::new(),
                created: Vec::new(),
                rolled_back: Vec::new(),
                fail_after: None,
                states: BTreeSet::new(),
                rollback_events: Vec::new(),
                rollback_failures: BTreeSet::new(),
                fail_health_check: false,
                fail_service_activation: false,
                gcroots_present_at_runtime: false,
                recovery_modes: Vec::new(),
            }
        }

        fn ensure(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            if self.existing.contains(asset.id()) {
                Ok(false)
            } else {
                let fail_after_mutation = self.fail_after == Some(self.created.len());
                self.existing.insert(asset.id());
                self.created.push(asset.id());
                if fail_after_mutation {
                    Err(InstallError::new(InstallErrorCode::BackendFailure))
                } else {
                    Ok(true)
                }
            }
        }
    }

    fn journal_before_runtime(digest: u8) -> Result<LinuxInstallJournal, Box<dyn Error>> {
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([digest; 32]),
            Digest::from_bytes([digest.wrapping_add(1); 32]),
        )?;
        for asset in linux_product_mutation_assets()
            .filter(|asset| asset.kind() != LinuxAssetKind::File && asset.id() != "nix-gcroots")
        {
            journal.record_preexisting(LinuxInstallMutation::Asset {
                id: asset.id().to_owned(),
            })?;
        }
        journal.record_preexisting(LinuxInstallMutation::Asset {
            id: "nix-config".to_owned(),
        })?;
        Ok(journal)
    }

    fn journal_before_services(digest: u8) -> Result<LinuxInstallJournal, Box<dyn Error>> {
        let mut journal = journal_before_runtime(digest)?;
        journal.record_preexisting(LinuxInstallMutation::ManagedRuntime)?;
        journal.record_preexisting(LinuxInstallMutation::Asset {
            id: "nix-gcroots".to_owned(),
        })?;
        for asset in linux_product_mutation_assets().filter(|asset| {
            asset.kind() == LinuxAssetKind::File
                && !matches!(asset.id(), "nix-config" | "uninstall-manifest")
        }) {
            journal.record_preexisting(LinuxInstallMutation::Asset {
                id: asset.id().to_owned(),
            })?;
        }
        Ok(journal)
    }

    impl LinuxInstallBackend for FakeBackend {
        fn install_mode(&self) -> LinuxInstallMode {
            if self.states.contains("repair-mode") {
                LinuxInstallMode::OfflineRepair
            } else {
                LinuxInstallMode::FreshInstall
            }
        }

        fn preflight_product_file_mutation(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_recovery(
            &mut self,
            mode: LinuxInstallMode,
            _system: System,
        ) -> Result<(), InstallError> {
            self.recovery_modes.push(mode);
            if mode == LinuxInstallMode::OfflineRepair {
                self.rollback_events.push("preflight-recovery");
            }
            if self.states.contains("fail-recovery-preflight") {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn bind_authenticated_release_identity(
            &mut self,
            _system: System,
            _digest: Digest,
        ) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
            if self.states.contains("unmanaged") {
                Err(InstallError::new(InstallErrorCode::UnmanagedNix))
            } else {
                Ok(())
            }
        }

        fn classify_asset(
            &mut self,
            asset: LinuxInstallAsset,
        ) -> Result<LinuxAssetPresence, InstallError> {
            Ok(if self.existing.contains(asset.id()) {
                LinuxAssetPresence::ExactPresent
            } else {
                LinuxAssetPresence::Absent
            })
        }

        fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
            self.existing.remove(asset.id());
            self.rolled_back.push(asset.id());
            Ok(())
        }

        fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
            self.rollback_events.push("recover-repair-assets");
            if self.states.contains("fail-repair-recovery") {
                Err(InstallError::backend_failure())
            } else {
                Ok(())
            }
        }

        fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
            self.states.remove("services");
            self.rollback_events.push("recover-services");
            Ok(())
        }

        fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
            self.ensure(asset)
        }

        fn install_systemd_unit(
            &mut self,
            asset: LinuxInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, InstallError> {
            self.ensure(asset)
        }

        fn activate_services(&mut self) -> Result<bool, InstallError> {
            let changed = self.states.insert("services");
            if self.fail_service_activation {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(changed)
            }
        }

        fn rollback_services(&mut self) -> Result<(), InstallError> {
            self.states.remove("services");
            self.rollback_events.push("services");
            if self.rollback_failures.contains("services") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }

        fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
            self.gcroots_present_at_runtime = self.existing.contains("nix-gcroots");
            let changed = self.states.insert("runtime");
            if self.states.contains("fail-runtime") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(changed)
            }
        }

        fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
            self.states.remove("runtime");
            self.rollback_events.push("runtime");
            if self.rollback_failures.contains("runtime") {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }

        fn validate_base_nix(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
            if self.fail_health_check {
                Err(InstallError::new(InstallErrorCode::ServiceUnhealthy))
            } else {
                Ok(())
            }
        }

        fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
            Ok(self.states.insert("receipt"))
        }

        fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
            Ok(())
        }

        fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
            self.existing.remove(asset.id());
            self.rolled_back.push(asset.id());
            self.rollback_events.push(asset.id());
            if self.rollback_failures.contains(asset.id()) {
                Err(InstallError::new(InstallErrorCode::BackendFailure))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn runtime_registration_precedes_the_gcroots_asset() -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();

        install_linux(System::X8664Linux, &mut backend)?;

        assert!(!backend.gcroots_present_at_runtime);
        assert!(backend.existing.contains("nix-gcroots"));
        Ok(())
    }

    #[test]
    fn install_is_receipt_last_and_idempotent() -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();
        let report = install_linux(System::X8664Linux, &mut backend)?;
        assert_eq!(
            report.created_artifacts(),
            linux_product_mutation_assets().count()
        );
        assert!(backend.states.contains("receipt"));
        assert!(backend.states.contains("runtime"));
        assert!(backend.rolled_back.is_empty());
        let second = install_linux(System::X8664Linux, &mut backend)?;
        assert_eq!(second.created_artifacts(), 0);
        assert_eq!(
            second.existing_artifacts(),
            linux_product_mutation_assets().count()
        );
        Ok(())
    }

    #[test]
    fn fresh_recovery_removes_only_revalidated_created_assets() -> Result<(), Box<dyn Error>> {
        let asset = linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.kind() != LinuxAssetKind::File)
            .ok_or_else(|| io::Error::other("missing fixed asset"))?;
        let mutation = LinuxInstallMutation::Asset {
            id: asset.id().to_owned(),
        };
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x44; 32]),
            Digest::from_bytes([0x45; 32]),
        )?;
        journal.intend(mutation)?;
        journal.complete_created()?;
        let mut backend = FakeBackend::clean();
        backend.existing.insert(asset.id());

        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

        assert!(!backend.existing.contains(asset.id()));
        assert_eq!(backend.rolled_back, [asset.id()]);
        Ok(())
    }

    #[test]
    fn fresh_recovery_preserves_absent_intended_asset() -> Result<(), Box<dyn Error>> {
        let asset = linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.kind() != LinuxAssetKind::File)
            .ok_or_else(|| io::Error::other("missing fixed asset"))?;
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::FreshInstall,
            System::X8664Linux,
            Digest::from_bytes([0x55; 32]),
            Digest::from_bytes([0x56; 32]),
        )?;
        journal.intend(LinuxInstallMutation::Asset {
            id: asset.id().to_owned(),
        })?;
        let mut backend = FakeBackend::clean();

        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

        assert!(backend.rolled_back.is_empty());
        Ok(())
    }

    #[test]
    fn fresh_recovery_refuses_unconnected_runtime_cleanup() -> Result<(), Box<dyn Error>> {
        let mut journal = journal_before_runtime(0x66)?;
        journal.intend(LinuxInstallMutation::ManagedRuntime)?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("runtime");

        let error = match recover_linux_install(
            &mut journal,
            &mut backend,
            &mut || Err(InstallError::backend_failure()),
            &mut |_| Ok(()),
        ) {
            Ok(()) => {
                return Err(io::Error::other("runtime recovery unexpectedly succeeded").into());
            }
            Err(error) => error,
        };

        assert_eq!(error.code(), InstallErrorCode::BackendFailure);
        assert!(backend.states.contains("runtime"));
        Ok(())
    }

    #[test]
    fn fresh_recovery_deactivates_only_with_exact_service_assets() -> Result<(), Box<dyn Error>> {
        let mut journal = journal_before_services(0x77)?;
        journal.intend_services()?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("services");
        backend.existing.extend(
            linux_product_mutation_assets()
                .filter(|asset| is_service_runtime_asset(*asset))
                .map(LinuxInstallAsset::id),
        );

        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

        assert!(!backend.states.contains("services"));
        assert_eq!(backend.rollback_events, ["recover-services"]);
        Ok(())
    }

    #[test]
    fn fresh_recovery_refuses_service_cleanup_without_exact_assets() -> Result<(), Box<dyn Error>> {
        let mut journal = journal_before_services(0x88)?;
        journal.intend_services()?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("services");

        assert!(
            recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))
                .is_err()
        );
        assert!(backend.states.contains("services"));
        assert!(backend.rollback_events.is_empty());
        Ok(())
    }

    #[test]
    fn fresh_recovery_saves_progress_before_a_later_failure() -> Result<(), Box<dyn Error>> {
        let mut journal = journal_before_runtime(0x89)?;
        journal.intend(LinuxInstallMutation::ManagedRuntime)?;
        journal.complete_created()?;
        for asset in linux_product_mutation_assets().filter(|asset| {
            asset.id() == "nix-gcroots"
                || (asset.kind() == LinuxAssetKind::File
                    && !matches!(asset.id(), "nix-config" | "uninstall-manifest"))
        }) {
            journal.record_preexisting(LinuxInstallMutation::Asset {
                id: asset.id().to_owned(),
            })?;
        }
        journal.intend_services()?;
        journal.complete_created()?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("services");
        backend.existing.extend(
            linux_product_mutation_assets()
                .filter(|asset| is_service_runtime_asset(*asset))
                .map(LinuxInstallAsset::id),
        );
        let mut persisted = 0_usize;

        let first = recover_linux_install(
            &mut journal,
            &mut backend,
            &mut || Err(InstallError::backend_failure()),
            &mut |_| {
                persisted = persisted.saturating_add(1);
                Ok(())
            },
        );
        assert_eq!(
            first.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert!(persisted > 0);
        assert_eq!(
            journal.recovery_actions().first(),
            Some(&LinuxInstallRecoveryAction::RevertCreated(
                &LinuxInstallMutation::ManagedRuntime
            ))
        );
        let recovery_events = backend.rollback_events.len();

        let second = recover_linux_install(
            &mut journal,
            &mut backend,
            &mut || Err(InstallError::backend_failure()),
            &mut |_| Ok(()),
        );
        assert_eq!(
            second.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(backend.rollback_events.len(), recovery_events);
        assert_eq!(backend.rollback_events.last(), Some(&"recover-services"));
        Ok(())
    }

    #[test]
    fn repair_journal_drives_offline_roll_forward_without_service_recovery()
    -> Result<(), Box<dyn Error>> {
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xa1; 32]),
            Digest::from_bytes([0xa2; 32]),
        )?;
        let mut backend = FakeBackend::clean();

        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;
        assert_eq!(backend.recovery_modes, [LinuxInstallMode::OfflineRepair]);
        assert_eq!(
            backend.rollback_events,
            ["preflight-recovery", "recover-repair-assets"]
        );
        assert!(!backend.states.contains("services"));
        Ok(())
    }

    #[test]
    fn repair_recovery_preflight_fails_before_service_or_file_mutation()
    -> Result<(), Box<dyn Error>> {
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xa3; 32]),
            Digest::from_bytes([0xa4; 32]),
        )?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("fail-recovery-preflight");

        let result =
            recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()));

        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(backend.rollback_events, ["preflight-recovery"]);
        Ok(())
    }

    #[test]
    fn repair_recovery_converges_even_when_only_preexisting_entries_were_recorded()
    -> Result<(), Box<dyn Error>> {
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xa5; 32]),
            Digest::from_bytes([0xa6; 32]),
        )?;
        let first = linux_product_mutation_assets()
            .find(|asset| asset.kind() != LinuxAssetKind::File && asset.id() != "nix-gcroots")
            .ok_or_else(|| io::Error::other("missing first Linux mutation"))?;
        journal.record_preexisting(LinuxInstallMutation::Asset {
            id: first.id().to_owned(),
        })?;
        let mut backend = FakeBackend::clean();

        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

        assert_eq!(backend.recovery_modes, [LinuxInstallMode::OfflineRepair]);
        assert_eq!(
            backend.rollback_events,
            ["preflight-recovery", "recover-repair-assets"]
        );
        Ok(())
    }

    #[test]
    fn repair_retry_rechecks_offline_state_and_never_uses_service_recovery()
    -> Result<(), Box<dyn Error>> {
        let mut journal = LinuxInstallJournal::new(
            LinuxInstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xa7; 32]),
            Digest::from_bytes([0xa8; 32]),
        )?;
        let mut backend = FakeBackend::clean();
        backend.states.insert("fail-repair-recovery");

        assert_eq!(
            recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))
                .map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(
            backend.rollback_events,
            ["preflight-recovery", "recover-repair-assets"]
        );

        backend.states.remove("fail-repair-recovery");
        backend.states.insert("fail-recovery-preflight");
        assert!(
            recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))
                .is_err()
        );
        assert_eq!(
            backend.rollback_events,
            [
                "preflight-recovery",
                "recover-repair-assets",
                "preflight-recovery"
            ]
        );

        backend.states.remove("fail-recovery-preflight");
        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;
        assert_eq!(
            backend.rollback_events,
            [
                "preflight-recovery",
                "recover-repair-assets",
                "preflight-recovery",
                "preflight-recovery",
                "recover-repair-assets"
            ]
        );
        assert!(
            !backend
                .rollback_events
                .iter()
                .any(|event| matches!(*event, "recover-services" | "resume-services"))
        );
        Ok(())
    }

    #[test]
    fn direct_install_entry_points_refuse_repair_without_a_durable_journal() {
        let mut backend = FakeBackend::clean();
        backend.states.insert("repair-mode");

        assert_eq!(
            install_linux(System::X8664Linux, &mut backend).map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(
            install_linux_preflighted(System::X8664Linux, &mut backend).map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert!(backend.created.is_empty());
        assert!(backend.rollback_events.is_empty());
    }

    #[test]
    fn failure_rolls_back_only_this_attempt_in_reverse_order() {
        let mut backend = FakeBackend::clean();
        backend.fail_after = Some(3);
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        let expected = backend.created.iter().rev().copied().collect::<Vec<_>>();
        assert_eq!(backend.rolled_back, expected);
        assert!(!backend.states.contains("runtime"));
        assert!(!backend.states.contains("services"));
        assert!(!backend.states.contains("receipt"));
    }

    #[test]
    fn post_activation_failure_reverts_services_files_runtime_then_directories()
    -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();
        backend.fail_health_check = true;
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::ServiceUnhealthy)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        let runtime = backend
            .rollback_events
            .iter()
            .position(|event| *event == "runtime")
            .ok_or_else(|| io::Error::other("runtime rollback missing"))?;
        let post_runtime_asset_count = linux_product_mutation_assets()
            .filter(|asset| {
                asset.id() == "nix-gcroots"
                    || (asset.kind() == LinuxAssetKind::File
                        && !matches!(asset.id(), "nix-config" | "uninstall-manifest"))
            })
            .count();
        assert_eq!(runtime, post_runtime_asset_count.saturating_add(1));
        assert!(!backend.states.contains("runtime"));
        assert!(!backend.states.contains("services"));
        assert!(backend.existing.is_empty());
        assert!(!backend.states.contains("receipt"));
        Ok(())
    }

    #[test]
    fn partial_service_activation_is_rolled_back_before_dependencies() {
        let mut backend = FakeBackend::clean();
        backend.fail_service_activation = true;
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::ServiceUnhealthy)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        assert!(!backend.states.contains("services"));
        assert!(!backend.states.contains("runtime"));
        assert!(backend.existing.is_empty());
    }

    #[test]
    fn privileged_host_scan_refuses_before_any_mutation() {
        let mut backend = FakeBackend::clean();
        backend.states.insert("unmanaged");
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::UnmanagedNix)
        );
        assert!(backend.created.is_empty());
        assert!(backend.rollback_events.is_empty());
    }

    #[test]
    fn partial_runtime_provisioning_is_rolled_back_before_directories() {
        let mut backend = FakeBackend::clean();
        backend.states.insert("fail-runtime");
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::BackendFailure)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"runtime"));
        assert!(!backend.states.contains("runtime"));
        assert!(backend.existing.is_empty());
    }

    #[test]
    fn failed_service_quiescence_blocks_every_file_and_runtime_rollback() {
        let mut backend = FakeBackend::clean();
        backend.fail_health_check = true;
        backend.rollback_failures.extend(["services", "runtime"]);
        let result = install_linux(System::X8664Linux, &mut backend);
        assert_eq!(
            result.map_err(InstallError::code),
            Err(InstallErrorCode::RollbackIncomplete)
        );
        assert_eq!(backend.rollback_events.first(), Some(&"services"));
        assert!(!backend.rollback_events.contains(&"runtime"));
        assert!(!backend.existing.is_empty());
    }
}
