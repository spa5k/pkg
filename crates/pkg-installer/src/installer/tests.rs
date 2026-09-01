//! Tests for the `installer` module.

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
    recovery_modes: Vec<InstallMode>,
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
        InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([digest; 32]),
        Digest::from_bytes([digest.wrapping_add(1); 32]),
    )?;
    for asset in linux_product_mutation_assets().filter(|asset| {
        asset.kind() != LinuxAssetKind::File && !is_linux_product_gcroots_asset(*asset)
    }) {
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
    for asset in
        linux_product_mutation_assets().filter(|asset| is_linux_product_gcroots_asset(*asset))
    {
        journal.record_preexisting(LinuxInstallMutation::Asset {
            id: asset.id().to_owned(),
        })?;
    }
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
    fn install_mode(&self) -> InstallMode {
        if self.states.contains("repair-mode") {
            InstallMode::OfflineRepair
        } else {
            InstallMode::FreshInstall
        }
    }

    fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
        Ok(())
    }

    fn preflight_fresh_recovery_mutation(
        &mut self,
        journal: &LinuxInstallJournal,
    ) -> Result<(), InstallError> {
        if journal.mode() != InstallMode::FreshInstall || !journal.fresh_services_deactivated() {
            return Err(InstallError::backend_failure());
        }
        self.rollback_events.push("preflight-fresh-recovery");
        if self.states.contains("fail-fresh-recovery-preflight") {
            Err(InstallError::backend_failure())
        } else {
            Ok(())
        }
    }

    fn preflight_recovery(
        &mut self,
        mode: InstallMode,
        _system: System,
    ) -> Result<(), InstallError> {
        self.recovery_modes.push(mode);
        if mode == InstallMode::OfflineRepair {
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

    fn classify_asset(&mut self, asset: LinuxInstallAsset) -> Result<AssetPresence, InstallError> {
        Ok(if self.existing.contains(asset.id()) {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
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
        self.gcroots_present_at_runtime = ["nix-gcroots", "nix-gcroots-users"]
            .into_iter()
            .any(|id| self.existing.contains(id));
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
fn runtime_registration_precedes_the_gcroots_assets() -> Result<(), Box<dyn Error>> {
    let mut backend = FakeBackend::clean();

    install_linux(System::X8664Linux, &mut backend)?;

    assert!(!backend.gcroots_present_at_runtime);
    assert!(backend.existing.contains("nix-gcroots"));
    assert!(backend.existing.contains("nix-gcroots-users"));
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
        InstallMode::FreshInstall,
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
        InstallMode::FreshInstall,
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
            .filter(|asset| is_linux_service_runtime_asset(*asset))
            .map(LinuxInstallAsset::id),
    );

    recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

    assert!(!backend.states.contains("services"));
    assert_eq!(
        backend.rollback_events,
        ["recover-services", "preflight-fresh-recovery"]
    );
    Ok(())
}

#[test]
fn fresh_recovery_refuses_service_cleanup_without_exact_assets() -> Result<(), Box<dyn Error>> {
    let mut journal = journal_before_services(0x88)?;
    journal.intend_services()?;
    let mut backend = FakeBackend::clean();
    backend.states.insert("services");

    assert!(
        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(())).is_err()
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
        is_linux_product_gcroots_asset(*asset)
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
            .filter(|asset| is_linux_service_runtime_asset(*asset))
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
    assert_eq!(backend.rollback_events.len(), recovery_events + 1);
    assert_eq!(
        backend.rollback_events.last(),
        Some(&"preflight-fresh-recovery")
    );
    Ok(())
}

#[test]
fn repair_journal_drives_offline_roll_forward_without_service_recovery()
-> Result<(), Box<dyn Error>> {
    let mut journal = LinuxInstallJournal::new(
        InstallMode::OfflineRepair,
        System::X8664Linux,
        Digest::from_bytes([0xa1; 32]),
        Digest::from_bytes([0xa2; 32]),
    )?;
    let mut backend = FakeBackend::clean();

    recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;
    assert_eq!(backend.recovery_modes, [InstallMode::OfflineRepair]);
    assert_eq!(
        backend.rollback_events,
        ["preflight-recovery", "recover-repair-assets"]
    );
    assert!(!backend.states.contains("services"));
    Ok(())
}

#[test]
fn repair_recovery_preflight_fails_before_service_or_file_mutation() -> Result<(), Box<dyn Error>> {
    let mut journal = LinuxInstallJournal::new(
        InstallMode::OfflineRepair,
        System::X8664Linux,
        Digest::from_bytes([0xa3; 32]),
        Digest::from_bytes([0xa4; 32]),
    )?;
    let mut backend = FakeBackend::clean();
    backend.states.insert("fail-recovery-preflight");

    let result = recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()));

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
        InstallMode::OfflineRepair,
        System::X8664Linux,
        Digest::from_bytes([0xa5; 32]),
        Digest::from_bytes([0xa6; 32]),
    )?;
    let first = linux_product_mutation_assets()
        .find(|asset| {
            asset.kind() != LinuxAssetKind::File && !is_linux_product_gcroots_asset(*asset)
        })
        .ok_or_else(|| io::Error::other("missing first Linux mutation"))?;
    journal.record_preexisting(LinuxInstallMutation::Asset {
        id: first.id().to_owned(),
    })?;
    let mut backend = FakeBackend::clean();

    recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(()))?;

    assert_eq!(backend.recovery_modes, [InstallMode::OfflineRepair]);
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
        InstallMode::OfflineRepair,
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
        recover_linux_install(&mut journal, &mut backend, &mut || Ok(()), &mut |_| Ok(())).is_err()
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
    let gcroots_users = backend
        .rollback_events
        .iter()
        .position(|event| *event == "nix-gcroots-users")
        .ok_or_else(|| io::Error::other("GC-root users rollback missing"))?;
    let gcroots = backend
        .rollback_events
        .iter()
        .position(|event| *event == "nix-gcroots")
        .ok_or_else(|| io::Error::other("GC-root rollback missing"))?;
    assert!(gcroots_users < gcroots && gcroots < runtime);
    let post_runtime_asset_count = linux_product_mutation_assets()
        .filter(|asset| {
            is_linux_product_gcroots_asset(*asset)
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
