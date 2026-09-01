//! Tests for the `linux_backend` module.

use super::*;

fn rendered_service_diagnostic(
    failure: Option<LinuxServiceFailure>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut output = Vec::new();
    write_service_diagnostic(&mut output, failure)?;
    Ok(String::from_utf8(output)?)
}

#[test]
fn service_diagnostics_are_one_fixed_line_and_success_is_silent()
-> Result<(), Box<dyn std::error::Error>> {
    assert!(rendered_service_diagnostic(None)?.is_empty());

    let systemd = rendered_service_diagnostic(Some(LinuxServiceFailure::Systemd(
        LinuxSystemdFailure::not_run(
            LinuxSystemdFailurePhase::Start,
            Some("pkg-root-helper.service"),
            crate::linux_systemd::LinuxSystemdErrorCode::CommandFailed,
        ),
    )))?;
    assert_eq!(
        systemd,
        "pkg-service-failure phase=start class=command-failed terminal=not-run unit=pkg-root-helper.service\n"
    );
    assert_eq!(systemd.lines().count(), 1);

    for (code, class) in [
        (
            BrokerTransportErrorCode::UnauthenticatedPeer,
            "unauthenticated-peer",
        ),
        (
            BrokerTransportErrorCode::TransportFailure,
            "transport-failure",
        ),
        (BrokerTransportErrorCode::InvalidFrame, "invalid-frame"),
        (BrokerTransportErrorCode::BrokerFailure, "broker-failure"),
    ] {
        let line = rendered_service_diagnostic(Some(LinuxServiceFailure::BrokerReadiness(code)))?;
        assert_eq!(
            line,
            format!("pkg-service-failure phase=broker-readiness class={class}\n")
        );
        assert_eq!(line.lines().count(), 1);
        for forbidden in ["synthetic-raw", "secret-marker", "\x1b", "\r"] {
            assert!(!line.contains(forbidden));
        }
    }
    Ok(())
}

#[test]
fn classify_services_failure_writes_exactly_one_line_before_mapping() {
    let (mut services, calls) = LinuxSystemdManager::inert_for_preflight_test();
    let mut output = Vec::new();

    assert_eq!(
        classify_service_state(&mut services, &mut output).map_err(InstallError::code),
        Err(crate::InstallErrorCode::BackendFailure)
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(
            output,
            b"pkg-service-failure phase=state-query class=command-failed terminal=not-run unit=pkg-root-helper.socket\n"
        );
}

#[test]
fn base_nix_readiness_waits_only_for_a_fresh_install() {
    let pings = std::cell::Cell::new(0);

    assert!(
        validate_base_nix_readiness(
            true,
            || {
                pings.set(pings.get() + 1);
                Ok(())
            },
            || Err(pkg_nix::NixAdapterError::OperationFailed),
        )
        .is_ok()
    );
    assert_eq!(pings.get(), 1);

    let waits = std::cell::Cell::new(0);
    assert!(
        validate_base_nix_readiness(
            false,
            || Err(pkg_nix::NixAdapterError::OperationFailed),
            || {
                waits.set(waits.get() + 1);
                Ok(())
            },
        )
        .is_ok()
    );
    assert_eq!(waits.get(), 1);
}

#[test]
fn started_handoff_is_the_only_refused_linux_preflight_state() {
    assert_eq!(
        validate_determinate_handoff_preflight(DeterminateHandoffState::NotStarted)
            .map_err(InstallError::code),
        Ok(false)
    );
    assert!(validate_determinate_handoff_preflight(DeterminateHandoffState::Started).is_err());
    assert_eq!(
        validate_determinate_handoff_preflight(DeterminateHandoffState::Accepted)
            .map_err(InstallError::code),
        Ok(true)
    );
}

#[test]
fn production_preflight_refuses_persisted_started_without_later_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
        DeterminateHandoffState::Started,
    ]));
    let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
        System::X8664Linux,
        ManagedGroupBindings::new(100, 101)?,
        snapshots.clone(),
        LinuxProductAssetIntent::InstallOrUpgrade,
    );

    assert_eq!(
        crate::installer::install_linux(System::X8664Linux, &mut backend)
            .map_err(InstallError::code),
        Err(crate::InstallErrorCode::BackendFailure)
    );
    assert_eq!(
        snapshots.borrow().as_slice(),
        &[DeterminateHandoffState::Started]
    );
    assert_eq!(service_calls.get(), 0);
    assert!(backend.release_identity.is_none());
    assert!(!backend.existing_managed_install);
    Ok(())
}

#[test]
fn product_repair_requires_an_accepted_determinate_handoff()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        validate_product_repair_handoff_preflight(DeterminateHandoffState::Accepted)
            .map_err(InstallError::code),
        Ok(())
    );
    for state in [
        DeterminateHandoffState::NotStarted,
        DeterminateHandoffState::Started,
    ] {
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![state]));
        let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
            System::X8664Linux,
            ManagedGroupBindings::new(100, 101)?,
            snapshots,
            LinuxProductAssetIntent::Repair,
        );

        assert_eq!(
            crate::installer::install_linux(System::X8664Linux, &mut backend)
                .map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure)
        );
        assert_eq!(service_calls.get(), 0);
        assert!(!backend.existing_managed_install);
    }
    Ok(())
}

#[test]
fn recovery_mode_is_derived_from_the_journal_and_handoff() {
    for (intent, mode, handoff) in [
        (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::NotStarted,
        ),
        (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Accepted,
        ),
        (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::OfflineUpgrade,
            DeterminateHandoffState::Accepted,
        ),
        (
            LinuxProductAssetIntent::Repair,
            crate::InstallMode::OfflineRepair,
            DeterminateHandoffState::Accepted,
        ),
    ] {
        assert_eq!(
            validate_recovery_mode(intent, mode, handoff).map_err(InstallError::code),
            Ok(())
        );
    }

    for (intent, mode, handoff) in [
        (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::OfflineUpgrade,
            DeterminateHandoffState::NotStarted,
        ),
        (
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::OfflineRepair,
            DeterminateHandoffState::Accepted,
        ),
        (
            LinuxProductAssetIntent::Repair,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Accepted,
        ),
        (
            LinuxProductAssetIntent::Repair,
            crate::InstallMode::OfflineRepair,
            DeterminateHandoffState::NotStarted,
        ),
    ] {
        assert_eq!(
            validate_recovery_mode(intent, mode, handoff).map_err(InstallError::code),
            Err(crate::InstallErrorCode::RecoveryModeMismatch)
        );
    }
    assert_eq!(
        validate_recovery_mode(
            LinuxProductAssetIntent::InstallOrUpgrade,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Started,
        )
        .map_err(InstallError::code),
        Err(crate::InstallErrorCode::BackendFailure)
    );
}

#[test]
fn privilege_preflight_does_not_classify_an_existing_install()
-> Result<(), Box<dyn std::error::Error>> {
    let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
        DeterminateHandoffState::Accepted,
    ]));
    let (mut backend, service_calls) = ProductionLinuxInstallBackend::for_preflight_test(
        System::X8664Linux,
        ManagedGroupBindings::new(100, 101)?,
        snapshots,
        LinuxProductAssetIntent::InstallOrUpgrade,
    );

    backend.preflight_privilege()?;

    assert_eq!(backend.mode, crate::InstallMode::FreshInstall);
    assert_eq!(service_calls.get(), 0);
    Ok(())
}

#[test]
fn production_service_transition_is_fresh_install_only() -> Result<(), Box<dyn std::error::Error>> {
    let snapshots = std::rc::Rc::new(std::cell::RefCell::new(vec![
        DeterminateHandoffState::Accepted,
    ]));
    let (mut backend, _) = ProductionLinuxInstallBackend::for_preflight_test(
        System::X8664Linux,
        ManagedGroupBindings::new(100, 101)?,
        snapshots,
        LinuxProductAssetIntent::InstallOrUpgrade,
    );
    assert!(backend.services_need_mutation(false));
    assert!(!backend.services_need_mutation(true));
    backend.mode = crate::InstallMode::OfflineUpgrade;
    assert!(!backend.services_need_mutation(false));
    assert!(!backend.services_need_mutation(true));
    Ok(())
}

#[test]
fn active_install_policy_is_fresh_normal_accepted_and_fully_bound() {
    let normal = LinuxProductAssetIntent::InstallOrUpgrade;
    assert_eq!(
        can_classify_active_install(
            normal,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Accepted,
            true,
            true,
        ),
        Ok(true)
    );
    for policy in [
        can_classify_active_install(
            LinuxProductAssetIntent::Repair,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Accepted,
            true,
            true,
        ),
        can_classify_active_install(
            normal,
            crate::InstallMode::OfflineUpgrade,
            DeterminateHandoffState::Accepted,
            true,
            true,
        ),
        can_classify_active_install(
            normal,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::NotStarted,
            true,
            true,
        ),
        can_classify_active_install(
            normal,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Accepted,
            false,
            true,
        ),
    ] {
        assert_eq!(policy, Ok(false));
    }
    assert!(
        can_classify_active_install(
            normal,
            crate::InstallMode::FreshInstall,
            DeterminateHandoffState::Started,
            true,
            true,
        )
        .is_err()
    );
}

#[test]
fn production_offline_upgrade_refuses_missing_receipt_owned_non_files_before_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let groups = ManagedGroupBindings::new(30_000, 30_001)?;
    let release = Digest::from_bytes([0xd1; 32]);
    for (missing_id, missing_path) in [
        ("broker-user", None),
        ("broker-log-dir", Some("var/lib/pkg/log/broker")),
    ] {
        let fixture = ProductionLinuxInstallBackend::for_existing_non_file_preflight_test(
            system, groups, release, missing_id,
        )?;
        let ExistingNonFilePreflightBackend {
            mut backend,
            temporary,
            account_mutation_calls,
            service_calls,
        } = fixture;
        let receipt_path = temporary.path().join("opt/pkg/uninstall/manifest.json");
        let receipt_before = std::fs::read(&receipt_path)?;
        let handoff_before = backend
            .preflight_fixture
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing preflight fixture"))?
            .handoff_snapshots
            .borrow()
            .clone();

        assert_eq!(
            backend
                .preflight_clean_host(system)
                .map_err(InstallError::code),
            Err(crate::InstallErrorCode::BackendFailure),
            "missing {missing_id} must fail closed"
        );

        assert_eq!(account_mutation_calls.get(), 0);
        assert_eq!(service_calls.get(), 0);
        assert_eq!(std::fs::read(&receipt_path)?, receipt_before);
        assert!(!temporary.path().join("pkg-install").exists());
        if let Some(path) = missing_path {
            assert!(!temporary.path().join(path).exists());
        }
        assert_eq!(
            backend
                .preflight_fixture
                .as_ref()
                .ok_or_else(|| std::io::Error::other("missing preflight fixture"))?
                .handoff_snapshots
                .borrow()
                .as_slice(),
            handoff_before.as_slice()
        );
    }
    Ok(())
}
