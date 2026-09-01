//! Tests for the `linux_systemd` module.

use super::*;
use std::{cell::RefCell, collections::BTreeMap, os::unix::fs::symlink, rc::Rc};

#[derive(Debug, Default)]
struct FakeState {
    units: BTreeMap<&'static str, UnitState>,
    definitions: BTreeMap<&'static str, UnitDefinition>,
    calls: Vec<String>,
    fail_call: Option<String>,
    fail_unit_state: Option<&'static str>,
}

#[derive(Debug, Clone)]
struct FakeSystem {
    state: Rc<RefCell<FakeState>>,
}

impl FakeSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, UnitState)>) -> Self {
        Self {
            state: Rc::new(RefCell::new(FakeState {
                units: states.into_iter().collect(),
                definitions: UNITS
                    .into_iter()
                    .map(|unit| {
                        (
                            unit,
                            UnitDefinition {
                                fragment_path: "/bin/sh".to_owned(),
                                drop_in_paths: String::new(),
                            },
                        )
                    })
                    .collect(),
                ..FakeState::default()
            })),
        }
    }

    fn call(&self, value: &str) -> Result<(), LinuxSystemdError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(value.to_owned());
        if state.fail_call.as_deref() == Some(value) {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        Ok(())
    }
}

impl SystemdSystem for FakeSystem {
    fn daemon_reload(&mut self) -> Result<(), LinuxSystemdError> {
        self.call("daemon-reload")
    }

    fn apply_tmpfiles(&mut self) -> Result<(), LinuxSystemdError> {
        self.call("tmpfiles")
    }

    fn unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        if self.state.borrow().fail_unit_state == Some(unit) {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            ));
        }
        self.state
            .borrow()
            .units
            .get(unit)
            .copied()
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))
    }

    fn required_unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        self.unit_state(unit)
    }

    fn required_unit_definition(
        &mut self,
        unit: &'static str,
    ) -> Result<UnitDefinition, LinuxSystemdError> {
        self.state
            .borrow()
            .definitions
            .get(unit)
            .cloned()
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))
    }

    fn enable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.call(&format!("enable:{unit}"))?;
        self.state
            .borrow_mut()
            .units
            .get_mut(unit)
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?
            .enabled = true;
        Ok(())
    }

    fn disable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.call(&format!("disable:{unit}"))?;
        self.state
            .borrow_mut()
            .units
            .get_mut(unit)
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?
            .enabled = false;
        Ok(())
    }

    fn start(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.call(&format!("start:{unit}"))?;
        self.state
            .borrow_mut()
            .units
            .get_mut(unit)
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?
            .active = true;
        Ok(())
    }

    fn stop(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.call(&format!("stop:{unit}"))?;
        self.state
            .borrow_mut()
            .units
            .get_mut(unit)
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?
            .active = false;
        Ok(())
    }
}

fn all(state: UnitState) -> Vec<(&'static str, UnitState)> {
    UNITS.into_iter().map(|unit| (unit, state)).collect()
}

fn test_manager(fake: FakeSystem) -> LinuxSystemdManager {
    LinuxSystemdManager::with_system(Box::new(fake), UNITS.map(|_| PathBuf::from("/bin/sh")))
}

#[test]
fn same_file_accepts_merged_usr_alias() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let expected = temp.path().join("usr/lib/systemd/system/pkg.service");
    fs::create_dir_all(expected.parent().ok_or("missing parent")?)?;
    fs::write(&expected, "unit")?;
    symlink("usr/lib", temp.path().join("lib"))?;

    assert!(same_file(
        &temp.path().join("lib/systemd/system/pkg.service"),
        &expected,
    ));
    Ok(())
}

#[test]
fn same_file_rejects_different_files() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let expected = temp.path().join("expected.service");
    let reported = temp.path().join("reported.service");
    fs::write(&expected, "unit")?;
    fs::write(&reported, "unit")?;

    assert!(!same_file(&reported, &expected));
    Ok(())
}

#[test]
fn same_file_rejects_exact_missing_path() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing.service");
    assert!(!same_file(&missing, &missing));
    Ok(())
}

#[test]
fn same_file_rejects_broken_alias() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let expected = temp.path().join("missing.service");
    let reported = temp.path().join("alias.service");
    symlink(&expected, &reported)?;

    assert!(!same_file(&reported, &expected));
    Ok(())
}

#[test]
fn same_file_rejects_relative_path() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let expected = temp.path().join("expected.service");
    fs::write(&expected, "unit")?;

    assert!(!same_file(Path::new("expected.service"), &expected));
    Ok(())
}

#[test]
fn absent_systemd_unit_status_is_false() -> Result<(), Box<dyn Error>> {
    let system = ProductionSystemdSystem {
        systemctl: PathBuf::from("/bin/sh"),
        tmpfiles: PathBuf::from("/bin/true"),
    };
    assert!(!system.systemctl_status(&["-c", "exit 4"], &[1, 4])?);
    assert!(system.systemctl_status(&["-c", "exit 4"], &[1]).is_err());
    assert!(system.systemctl_status(&["-c", "exit 5"], &[1, 4]).is_err());
    Ok(())
}

#[test]
fn required_unit_file_state_is_closed() {
    assert_eq!(parse_required_unit_file_state("enabled"), Ok(true));
    assert_eq!(parse_required_unit_file_state("enabled-runtime"), Ok(true));
    assert_eq!(parse_required_unit_file_state("disabled"), Ok(false));
    assert_eq!(parse_required_unit_file_state("static"), Ok(false));
    for refused in ["", "masked", "linked", "indirect", "not-found"] {
        assert_eq!(
            parse_required_unit_file_state(refused).map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
    }
}

#[test]
fn command_terminal_metadata_is_fixed_bounded_and_raw_free() -> Result<(), Box<dyn Error>> {
    let cases = [
        (LinuxSystemdTerminal::NotRun, "terminal=not-run"),
        (LinuxSystemdTerminal::SpawnFailed, "terminal=spawn-failed"),
        (LinuxSystemdTerminal::TimedOut, "terminal=timed-out"),
        (LinuxSystemdTerminal::WaitFailed, "terminal=wait-failed"),
        (
            LinuxSystemdTerminal::ExitedNonzero(23),
            "terminal=exited-nonzero exit-code=23",
        ),
        (
            LinuxSystemdTerminal::Signaled(15),
            "terminal=signaled signal=15",
        ),
        (LinuxSystemdTerminal::OutputFailed, "terminal=output-failed"),
    ];
    for (terminal, expected) in cases {
        let rendered = LinuxSystemdFailure::new(
            LinuxSystemdFailurePhase::Start,
            Some("pkg-nix-broker.service"),
            LinuxSystemdErrorCode::CommandFailed,
            terminal,
        )
        .to_string();
        assert!(rendered.contains("class=command-failed"));
        assert!(rendered.contains(expected));
        for forbidden in ["synthetic-raw", "secret-marker", "\x1b", "\r"] {
            assert!(!rendered.contains(forbidden));
        }
    }

    let nonzero = run_success(Path::new("/bin/sh"), &["-c", "exit 23"])
        .err()
        .ok_or("nonzero command unexpectedly succeeded")?;
    assert_eq!(nonzero.terminal, LinuxSystemdTerminal::ExitedNonzero(23));

    let signaled = run_status_code(Path::new("/bin/sh"), &["-c", "kill -TERM $$"])
        .err()
        .ok_or("signaled command unexpectedly succeeded")?;
    assert_eq!(signaled.terminal, LinuxSystemdTerminal::Signaled(15));

    let output = run_output(Path::new("/bin/sh"), &["-c", "printf '\\377'"])
        .err()
        .ok_or("invalid output unexpectedly succeeded")?;
    assert_eq!(output.terminal, LinuxSystemdTerminal::OutputFailed);
    Ok(())
}

#[test]
fn activation_orders_reload_tmpfiles_enable_and_start() -> Result<(), Box<dyn Error>> {
    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let state = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);

    assert!(manager.activate_fresh(|| true)?);
    manager.verify_active()?;

    let state = state.borrow();
    assert_eq!(state.calls[0], "daemon-reload");
    assert_eq!(state.calls[1], "tmpfiles");
    assert_eq!(
        &state.calls[2..2 + UNITS.len()],
        UNITS.map(|unit| format!("enable:{unit}"))
    );
    assert_eq!(
        &state.calls[2 + UNITS.len()..2 + UNITS.len() * 2],
        UNITS.map(|unit| format!("start:{unit}"))
    );
    Ok(())
}

#[test]
fn activation_accepts_merged_usr_unit_fragments() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let systemd_dir = temp.path().join("usr/lib/systemd/system");
    fs::create_dir_all(&systemd_dir)?;
    symlink("usr/lib", temp.path().join("lib"))?;
    let unit_fragments = UNITS.map(|unit| systemd_dir.join(unit));
    for fragment in &unit_fragments {
        fs::write(fragment, "unit")?;
    }
    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    for unit in UNITS {
        fake.state
            .borrow_mut()
            .definitions
            .get_mut(unit)
            .ok_or("missing unit definition")?
            .fragment_path = temp
            .path()
            .join("lib/systemd/system")
            .join(unit)
            .to_string_lossy()
            .into_owned();
    }

    assert!(
        LinuxSystemdManager::with_system(Box::new(fake), unit_fragments).activate_fresh(|| true)?
    );
    Ok(())
}

#[test]
fn activation_failures_retain_only_fixed_phase_and_unit_metadata() {
    let expected = |phase, unit, code| {
        LinuxSystemdFailure::new(phase, unit, code, LinuxSystemdTerminal::NotRun)
    };
    let run = |fake: FakeSystem, authenticate| {
        test_manager(fake)
            .activate_fresh(|| authenticate)
            .err()
            .and_then(LinuxSystemdError::failure)
    };

    for unit in UNITS {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        fake.state.borrow_mut().fail_unit_state = Some(unit);
        assert_eq!(
            run(fake, true),
            Some(expected(
                LinuxSystemdFailurePhase::StateQuery,
                Some(unit),
                LinuxSystemdErrorCode::StateQueryFailed,
            ))
        );
    }

    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    fake.state.borrow_mut().fail_call = Some("daemon-reload".to_owned());
    assert_eq!(
        run(fake, true),
        Some(expected(
            LinuxSystemdFailurePhase::DaemonReload,
            None,
            LinuxSystemdErrorCode::CommandFailed,
        ))
    );

    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    assert_eq!(
        run(fake, false),
        Some(expected(
            LinuxSystemdFailurePhase::AuthenticateUnit,
            None,
            LinuxSystemdErrorCode::StateQueryFailed,
        ))
    );

    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    fake.state.borrow_mut().fail_call = Some("tmpfiles".to_owned());
    assert_eq!(
        run(fake, true),
        Some(expected(
            LinuxSystemdFailurePhase::Tmpfiles,
            None,
            LinuxSystemdErrorCode::CommandFailed,
        ))
    );

    for (phase, action) in [
        (LinuxSystemdFailurePhase::Enable, "enable"),
        (LinuxSystemdFailurePhase::Start, "start"),
    ] {
        for unit in UNITS {
            let fake = FakeSystem::new(all(UnitState {
                enabled: false,
                active: false,
            }));
            fake.state.borrow_mut().fail_call = Some(format!("{action}:{unit}"));
            assert_eq!(
                run(fake, true),
                Some(expected(
                    phase,
                    Some(unit),
                    LinuxSystemdErrorCode::CommandFailed,
                ))
            );
        }
    }
}

#[test]
fn authentication_failure_does_not_retain_raw_unit_metadata() {
    for unit in UNITS {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        fake.state
            .borrow_mut()
            .definitions
            .entry(unit)
            .and_modify(|definition| {
                definition.drop_in_paths = "synthetic-raw\x1b[31m".to_owned();
            });
        let failure = test_manager(fake)
            .activate_fresh(|| true)
            .err()
            .and_then(LinuxSystemdError::failure);
        assert_eq!(
            failure,
            Some(LinuxSystemdFailure::new(
                LinuxSystemdFailurePhase::AuthenticateUnit,
                Some(unit),
                LinuxSystemdErrorCode::StateQueryFailed,
                LinuxSystemdTerminal::NotRun,
            ))
        );
        let rendered = failure
            .map(|failure| failure.to_string())
            .unwrap_or_default();
        assert!(!rendered.contains("synthetic-raw"));
        assert!(!rendered.contains('\x1b'));
    }
}

#[test]
fn verify_active_retains_the_exact_fixed_unit() {
    for failed_unit in UNITS {
        let fake = FakeSystem::new(UNITS.map(|unit| {
            (
                unit,
                UnitState {
                    enabled: true,
                    active: unit != failed_unit,
                },
            )
        }));
        let mut manager = test_manager(fake);
        assert_eq!(
            manager
                .verify_active()
                .err()
                .and_then(LinuxSystemdError::failure),
            Some(LinuxSystemdFailure::new(
                LinuxSystemdFailurePhase::VerifyActive,
                Some(failed_unit),
                LinuxSystemdErrorCode::StateQueryFailed,
                LinuxSystemdTerminal::NotRun,
            ))
        );
    }
}

#[test]
fn fresh_rollback_leaves_the_complete_service_set_offline() -> Result<(), Box<dyn Error>> {
    let states = all(UnitState {
        enabled: false,
        active: false,
    });
    let fake = FakeSystem::new(states.clone());
    let shared = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);

    assert!(manager.activate_fresh(|| true)?);
    manager.rollback()?;

    let state = shared.borrow();
    for (unit, expected) in states {
        assert_eq!(state.units[unit], expected);
    }
    assert!(
        !state
            .calls
            .iter()
            .any(|call| call == "stop:pkg-nix-daemon.socket")
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|call| call == "disable:pkg-nix-daemon.socket")
    );
    Ok(())
}

#[test]
fn failed_start_keeps_write_ahead_intent_for_rollback() {
    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let shared = Rc::clone(&fake.state);
    shared.borrow_mut().fail_call = Some("start:pkg-root-helper.service".to_owned());
    let mut manager = test_manager(fake);

    let error = manager.activate_fresh(|| true).err();
    assert_eq!(
        error.map(LinuxSystemdError::code),
        Some(LinuxSystemdErrorCode::CommandFailed)
    );
    assert_eq!(
        error.and_then(LinuxSystemdError::failure),
        Some(LinuxSystemdFailure::new(
            LinuxSystemdFailurePhase::Start,
            Some("pkg-root-helper.service"),
            LinuxSystemdErrorCode::CommandFailed,
            LinuxSystemdTerminal::NotRun,
        ))
    );
    assert!(manager.rollback().is_ok());
    let state = shared.borrow();
    assert!(
        state
            .calls
            .iter()
            .any(|call| call == "stop:pkg-root-helper.service")
    );
    assert!(
        state
            .units
            .values()
            .all(|unit| !unit.enabled && !unit.active)
    );
}

#[test]
fn fresh_activation_refuses_a_preexisting_active_service_set() {
    let fake = FakeSystem::new(all(UnitState {
        enabled: true,
        active: true,
    }));
    let shared = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);

    assert_eq!(
        manager
            .activate_fresh(|| true)
            .map_err(LinuxSystemdError::code),
        Err(LinuxSystemdErrorCode::StateQueryFailed)
    );
    assert!(shared.borrow().calls.is_empty());
}

#[test]
fn fresh_activation_refuses_foreign_effective_units_before_start_or_enable() {
    for drop_in_paths in [
        String::new(),
        "/etc/systemd/system/pkg-root-helper.service.d/local.conf".to_owned(),
    ] {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let shared = Rc::clone(&fake.state);
        shared.borrow_mut().definitions.insert(
            "pkg-root-helper.service",
            UnitDefinition {
                fragment_path: if drop_in_paths.is_empty() {
                    "/etc/systemd/system/pkg-root-helper.service".to_owned()
                } else {
                    "/usr/lib/systemd/system/pkg-root-helper.service".to_owned()
                },
                drop_in_paths,
            },
        );
        let mut manager = test_manager(fake);

        assert_eq!(
            manager
                .activate_fresh(|| true)
                .map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert_eq!(shared.borrow().calls, ["daemon-reload"]);
    }
}

#[test]
fn fresh_recovery_deactivates_only_the_authenticated_loaded_set() -> Result<(), Box<dyn Error>> {
    let fake = FakeSystem::new(all(UnitState {
        enabled: true,
        active: true,
    }));
    let shared = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);

    manager.deactivate_fresh_recovery(|| true)?;
    assert!(
        !shared
            .borrow()
            .calls
            .iter()
            .any(|call| call == "daemon-reload")
    );
    assert!(
        shared
            .borrow()
            .units
            .values()
            .all(|state| !state.enabled && !state.active)
    );
    Ok(())
}

#[test]
fn fresh_recovery_never_mutates_a_foreign_loaded_unit() {
    for drop_in_paths in [
        String::new(),
        "/etc/systemd/system/pkg-root-helper.service.d/local.conf".to_owned(),
    ] {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        shared.borrow_mut().definitions.insert(
            "pkg-root-helper.service",
            UnitDefinition {
                fragment_path: if drop_in_paths.is_empty() {
                    "/etc/systemd/system/pkg-root-helper.service".to_owned()
                } else {
                    "/usr/lib/systemd/system/pkg-root-helper.service".to_owned()
                },
                drop_in_paths,
            },
        );
        let prior = shared.borrow().units.clone();
        let mut manager = test_manager(fake);

        assert_eq!(
            manager
                .deactivate_fresh_recovery(|| true)
                .map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert!(shared.borrow().calls.is_empty());
        assert_eq!(shared.borrow().units, prior);
    }
}

#[test]
fn fresh_rollback_refuses_foreign_effective_units_before_stop_or_disable()
-> Result<(), Box<dyn Error>> {
    for drop_in_paths in [
        String::new(),
        "/etc/systemd/system/pkg-root-helper.service.d/local.conf".to_owned(),
    ] {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = test_manager(fake);
        assert!(manager.activate_fresh(|| true)?);
        shared.borrow_mut().calls.clear();
        shared.borrow_mut().definitions.insert(
            "pkg-root-helper.service",
            UnitDefinition {
                fragment_path: if drop_in_paths.is_empty() {
                    "/etc/systemd/system/pkg-root-helper.service".to_owned()
                } else {
                    "/usr/lib/systemd/system/pkg-root-helper.service".to_owned()
                },
                drop_in_paths,
            },
        );

        assert_eq!(
            manager
                .prepare_rollback(|| true)
                .map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::RollbackFailed)
        );
        assert!(shared.borrow().calls.is_empty());
    }
    Ok(())
}

#[test]
fn recovery_quiescence_does_not_command_an_absent_inactive_unit() -> Result<(), Box<dyn Error>> {
    let mut states = all(UnitState {
        enabled: true,
        active: true,
    });
    let absent = states[0].0;
    states[0].1 = UnitState {
        enabled: false,
        active: false,
    };
    let fake = FakeSystem::new(states);
    let shared = Rc::clone(&fake.state);
    shared.borrow_mut().fail_call = Some(format!("disable:{absent}"));
    let mut manager = test_manager(fake);

    manager.deactivate_fresh_recovery(|| true)?;

    let state = shared.borrow();
    assert!(
        !state
            .calls
            .iter()
            .any(|call| call == &format!("disable:{absent}"))
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|call| call == &format!("stop:{absent}"))
    );
    assert!(
        state
            .units
            .values()
            .all(|unit| !unit.enabled && !unit.active)
    );
    Ok(())
}

#[test]
fn activation_classification_accepts_only_complete_terminal_states() -> Result<(), Box<dyn Error>> {
    let mut active = test_manager(FakeSystem::new(all(UnitState {
        enabled: true,
        active: true,
    })));
    assert!(active.classify_exact_activation()?);

    let mut inactive = test_manager(FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    })));
    assert!(!inactive.classify_exact_activation()?);
    Ok(())
}

#[test]
fn activation_classification_refuses_mixed_state() {
    let mut states = all(UnitState {
        enabled: false,
        active: false,
    });
    states[0].1 = UnitState {
        enabled: true,
        active: false,
    };
    let mut manager = test_manager(FakeSystem::new(states));

    let error = manager.classify_activation().err();
    assert_eq!(
        error.map(super::LinuxSystemdError::code),
        Some(LinuxSystemdErrorCode::StateQueryFailed)
    );
    assert_eq!(
        error.and_then(LinuxSystemdError::failure),
        Some(LinuxSystemdFailure::new(
            LinuxSystemdFailurePhase::StateQuery,
            Some(UNITS[0]),
            LinuxSystemdErrorCode::StateQueryFailed,
            LinuxSystemdTerminal::NotRun,
        ))
    );

    for unit in UNITS {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        fake.state.borrow_mut().fail_unit_state = Some(unit);
        let mut manager = test_manager(fake);
        assert_eq!(
            manager
                .classify_activation()
                .err()
                .and_then(LinuxSystemdError::failure),
            Some(LinuxSystemdFailure::new(
                LinuxSystemdFailurePhase::StateQuery,
                Some(unit),
                LinuxSystemdErrorCode::StateQueryFailed,
                LinuxSystemdTerminal::NotRun,
            ))
        );
    }
}

#[test]
fn offline_preflight_is_query_only_and_refuses_every_non_offline_state() {
    let offline = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let offline_state = Rc::clone(&offline.state);
    let mut manager = test_manager(offline);
    assert_eq!(manager.require_offline(), Ok(()));
    assert!(offline_state.borrow().calls.is_empty());

    let mut refused = vec![
        all(UnitState {
            enabled: true,
            active: true,
        }),
        all(UnitState {
            enabled: true,
            active: false,
        }),
    ];
    let mut mixed = all(UnitState {
        enabled: false,
        active: false,
    });
    mixed[0].1.active = true;
    refused.push(mixed);
    let mut unreadable = all(UnitState {
        enabled: false,
        active: false,
    });
    let _ = unreadable.pop();
    refused.push(unreadable);

    for states in refused {
        let fake = FakeSystem::new(states);
        let shared = Rc::clone(&fake.state);
        let mut manager = test_manager(fake);
        assert_eq!(
            manager.require_offline().map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert!(shared.borrow().calls.is_empty());
    }

    let wrong_fragment = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let shared = Rc::clone(&wrong_fragment.state);
    shared.borrow_mut().definitions.insert(
        "pkg-root-helper.socket",
        UnitDefinition {
            fragment_path: "/etc/systemd/system/pkg-root-helper.socket".to_owned(),
            drop_in_paths: String::new(),
        },
    );
    let mut manager = test_manager(wrong_fragment);
    assert_eq!(
        manager.require_offline().map_err(LinuxSystemdError::code),
        Err(LinuxSystemdErrorCode::StateQueryFailed)
    );
    assert!(shared.borrow().calls.is_empty());

    let with_drop_in = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let shared = Rc::clone(&with_drop_in.state);
    shared.borrow_mut().definitions.insert(
        "pkg-nix-broker.service",
        UnitDefinition {
            fragment_path: "/usr/lib/systemd/system/pkg-nix-broker.service".to_owned(),
            drop_in_paths: "/etc/systemd/system/pkg-nix-broker.service.d/local.conf".to_owned(),
        },
    );
    let mut manager = test_manager(with_drop_in);
    assert_eq!(
        manager.require_offline().map_err(LinuxSystemdError::code),
        Err(LinuxSystemdErrorCode::StateQueryFailed)
    );
    assert!(shared.borrow().calls.is_empty());
}

#[test]
fn rollback_attempts_all_units_and_remains_retryable() -> Result<(), Box<dyn Error>> {
    let fake = FakeSystem::new(all(UnitState {
        enabled: false,
        active: false,
    }));
    let shared = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);
    assert!(manager.activate_fresh(|| true)?);
    shared.borrow_mut().fail_call = Some("stop:pkg-nix-broker.service".to_owned());

    assert_eq!(
        manager.rollback(),
        Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::RollbackFailed
        ))
    );
    assert!(
        shared
            .borrow()
            .calls
            .iter()
            .any(|call| call == "disable:pkg-root-helper.socket")
    );
    shared.borrow_mut().fail_call = None;
    manager.rollback()?;
    Ok(())
}

#[test]
fn uninstall_deactivation_stops_disables_and_verifies_every_unit() -> Result<(), Box<dyn Error>> {
    let fake = FakeSystem::new(all(UnitState {
        enabled: true,
        active: true,
    }));
    let shared = Rc::clone(&fake.state);
    let mut manager = test_manager(fake);

    manager.deactivate_for_uninstall(|| true)?;

    let state = shared.borrow();
    assert!(
        state
            .units
            .values()
            .all(|unit| !unit.enabled && !unit.active)
    );
    let reverse = UNITS.into_iter().rev().collect::<Vec<_>>();
    let stop_end = UNITS.len();
    let disable_end = stop_end + UNITS.len();
    assert_eq!(
        &state.calls[..stop_end],
        reverse
            .iter()
            .map(|unit| format!("stop:{unit}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        &state.calls[stop_end..disable_end],
        reverse
            .iter()
            .map(|unit| format!("disable:{unit}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(state.calls[disable_end], "daemon-reload");
    Ok(())
}

#[test]
fn uninstall_deactivation_attempts_all_units_after_failure() {
    let fake = FakeSystem::new(all(UnitState {
        enabled: true,
        active: true,
    }));
    let shared = Rc::clone(&fake.state);
    shared.borrow_mut().fail_call = Some("stop:pkg-nix-broker.service".to_owned());
    let mut manager = test_manager(fake);

    assert_eq!(
        manager.deactivate_for_uninstall(|| true),
        Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::RollbackFailed
        ))
    );
    assert!(
        shared
            .borrow()
            .calls
            .iter()
            .any(|call| call == "disable:pkg-root-helper.socket")
    );
}

#[test]
fn uninstall_refuses_foreign_effective_units_before_stop_or_disable() {
    for drop_in_paths in [
        String::new(),
        "/etc/systemd/system/pkg-root-helper.service.d/local.conf".to_owned(),
    ] {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        shared.borrow_mut().definitions.insert(
            "pkg-root-helper.service",
            UnitDefinition {
                fragment_path: if drop_in_paths.is_empty() {
                    "/etc/systemd/system/pkg-root-helper.service".to_owned()
                } else {
                    "/usr/lib/systemd/system/pkg-root-helper.service".to_owned()
                },
                drop_in_paths,
            },
        );
        let mut manager = test_manager(fake);

        assert_eq!(
            manager
                .deactivate_for_uninstall(|| true)
                .map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert!(shared.borrow().calls.is_empty());
    }
}
