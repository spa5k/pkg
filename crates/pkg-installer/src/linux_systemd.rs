//! Reversible activation of the fixed Linux systemd service set.

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    error::Error,
    fmt, fs,
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const SYSTEMCTL_PATHS: &[&str] = &["/usr/bin/systemctl", "/bin/systemctl"];
const TMPFILES_PATHS: &[&str] = &[
    "/usr/bin/systemd-tmpfiles",
    "/bin/systemd-tmpfiles",
    "/usr/lib/systemd/systemd-tmpfiles",
];
const TMPFILES_CONFIG: &str = "/usr/lib/tmpfiles.d/pkg.conf";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

const UNITS: [&str; 4] = [
    "pkg-root-helper.socket",
    "pkg-nix-broker.socket",
    "pkg-root-helper.service",
    "pkg-nix-broker.service",
];

/// Stable production systemd activation failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxSystemdErrorCode {
    /// A required root-controlled executable is absent or unsafe.
    ProgramUnavailable,
    /// Existing unit state could not be established exactly.
    StateQueryFailed,
    /// One fixed systemd or tmpfiles operation failed.
    CommandFailed,
    /// Exact prior service state could not be restored.
    RollbackFailed,
}

/// Redacted production systemd activation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxSystemdError {
    code: LinuxSystemdErrorCode,
}

impl LinuxSystemdError {
    const fn new(code: LinuxSystemdErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxSystemdErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxSystemdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux service activation failed")
    }
}

impl Error for LinuxSystemdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnitState {
    enabled: bool,
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnitJournal {
    name: &'static str,
    prior: UnitState,
    enable_intent: bool,
    start_intent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceActivation {
    Preserve,
    Start,
    Restart,
}

trait SystemdSystem: fmt::Debug {
    fn daemon_reload(&mut self) -> Result<(), LinuxSystemdError>;
    fn apply_tmpfiles(&mut self) -> Result<(), LinuxSystemdError>;
    fn unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError>;
    fn enable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn disable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn start(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn restart(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn stop(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
}

/// Production manager for the fixed pkg systemd units.
pub struct LinuxSystemdManager {
    system: Box<dyn SystemdSystem>,
    journal: Option<Vec<UnitJournal>>,
}

impl fmt::Debug for LinuxSystemdManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxSystemdManager")
            .field("activation_journaled", &self.journal.is_some())
            .finish_non_exhaustive()
    }
}

impl LinuxSystemdManager {
    /// Resolves the fixed root-controlled systemd executables.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when either executable is unavailable or unsafe.
    pub(crate) fn production() -> Result<Self, LinuxSystemdError> {
        Ok(Self {
            system: Box::new(ProductionSystemdSystem::new()?),
            journal: None,
        })
    }

    #[cfg(test)]
    fn with_system(system: Box<dyn SystemdSystem>) -> Self {
        Self {
            system,
            journal: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn inert_for_preflight_test() -> (Self, std::rc::Rc<std::cell::Cell<usize>>) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Self::with_system(Box::new(PreflightTestSystem {
                calls: calls.clone(),
            })),
            calls,
        )
    }

    /// Reloads systemd and applies, enables, and starts the fixed service set.
    ///
    /// The complete prior state is captured before the first enable/start
    /// mutation. Intent is recorded before each command so an uncertain command
    /// failure remains reversible.
    ///
    /// # Errors
    ///
    /// Returns a redacted query or command failure. Call [`Self::rollback`]
    /// after any error.
    pub(crate) fn activate(
        &mut self,
        activation: ServiceActivation,
    ) -> Result<bool, LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }

        self.system.daemon_reload()?;
        let mut journal = UNITS
            .iter()
            .copied()
            .map(|name| {
                self.system.unit_state(name).map(|prior| UnitJournal {
                    name,
                    prior,
                    enable_intent: false,
                    start_intent: false,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.journal = Some(journal.clone());
        self.system.apply_tmpfiles()?;

        for index in 0..journal.len() {
            if activation == ServiceActivation::Start && !journal[index].prior.enabled {
                journal[index].enable_intent = true;
                self.journal = Some(journal.clone());
                self.system.enable(journal[index].name)?;
            }
        }
        for index in 0..journal.len() {
            if activation == ServiceActivation::Restart && journal[index].prior.active {
                journal[index].start_intent = true;
                self.journal = Some(journal.clone());
                self.system.restart(journal[index].name)?;
            } else if activation == ServiceActivation::Start && !journal[index].prior.active {
                journal[index].start_intent = true;
                self.journal = Some(journal.clone());
                self.system.start(journal[index].name)?;
            }
        }

        let changed = journal
            .iter()
            .any(|entry| entry.enable_intent || entry.start_intent);
        if changed {
            self.journal = Some(journal);
        } else {
            self.journal = None;
        }
        Ok(changed)
    }

    /// Restores the exact enabled and active state captured by this attempt.
    ///
    /// # Errors
    ///
    /// Returns a redacted rollback failure after attempting every restoration.
    #[cfg(test)]
    fn rollback(&mut self) -> Result<(), LinuxSystemdError> {
        self.prepare_rollback()?;
        self.finish_rollback()
    }

    /// Stops candidate processes before old product files are restored.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the candidate set cannot be quiesced.
    pub(crate) fn prepare_rollback(&mut self) -> Result<(), LinuxSystemdError> {
        let Some(journal) = self.journal.as_ref() else {
            return Ok(());
        };
        let mut failed = false;
        for entry in journal.iter().rev() {
            if self.system.disable(entry.name).is_err() {
                failed = true;
            }
        }
        for entry in journal.iter().rev() {
            if self.system.stop(entry.name).is_err() {
                failed = true;
            }
        }
        if self.classify_activation() != Ok(false) {
            failed = true;
        }
        if failed {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
        }
        Ok(())
    }

    /// Reloads restored unit files and restores the prior active service set.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the prior set cannot be resumed.
    pub(crate) fn finish_rollback(&mut self) -> Result<(), LinuxSystemdError> {
        let Some(journal) = self.journal.as_ref() else {
            return Ok(());
        };
        let mut failed = self.system.daemon_reload().is_err();
        for entry in journal {
            if entry.prior.enabled && self.system.enable(entry.name).is_err() {
                failed = true;
            }
        }
        for entry in journal {
            if entry.prior.active && self.system.start(entry.name).is_err() {
                failed = true;
            }
        }
        if failed {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
        }
        self.journal = None;
        Ok(())
    }

    /// Forgets the in-memory rollback record after the new receipt is durable.
    pub(crate) fn commit_activation(&mut self) {
        self.journal = None;
    }

    /// Stops and disables the complete fixed service set for uninstall.
    ///
    /// Every unit state is read before the first mutation. The manager then
    /// attempts all required stops and disables in reverse activation order,
    /// reloads systemd, and verifies that no fixed unit remains active or
    /// enabled.
    ///
    /// # Errors
    ///
    /// Returns a redacted failure after attempting the complete fixed set.
    pub(crate) fn deactivate_for_uninstall(&mut self) -> Result<(), LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        self.system.daemon_reload()?;
        let states = UNITS
            .iter()
            .copied()
            .map(|unit| self.system.unit_state(unit).map(|state| (unit, state)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut failed = false;
        for (unit, state) in states.iter().rev() {
            if state.active && self.system.stop(unit).is_err() {
                failed = true;
            }
        }
        for (unit, state) in states.iter().rev() {
            if state.enabled && self.system.disable(unit).is_err() {
                failed = true;
            }
        }
        if self.system.daemon_reload().is_err() {
            failed = true;
        }
        for unit in UNITS {
            match self.system.unit_state(unit) {
                Ok(state) if !state.active && !state.enabled => {}
                Ok(_) | Err(_) => failed = true,
            }
        }
        if failed {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
        }
        Ok(())
    }

    /// Quiesces candidate processes before durable install recovery restores files.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the candidate set cannot be quiesced.
    pub(crate) fn prepare_recovery(&mut self) -> Result<(), LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        let states = UNITS
            .iter()
            .copied()
            .map(|unit| self.system.unit_state(unit).map(|state| (unit, state)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut failed = false;
        for (unit, state) in states.iter().rev() {
            if state.enabled && self.system.disable(unit).is_err() {
                failed = true;
            }
        }
        for (unit, state) in states.iter().rev() {
            if state.active && self.system.stop(unit).is_err() {
                failed = true;
            }
        }
        if self.classify_activation() != Ok(false) {
            failed = true;
        }
        if failed {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
        }
        Ok(())
    }

    /// Reloads restored files and resumes the exact prior active service set.
    ///
    /// # Errors
    ///
    /// Returns a command failure when the prior set cannot be resumed.
    pub(crate) fn finish_recovery(&mut self, prior_active: bool) -> Result<(), LinuxSystemdError> {
        self.system.daemon_reload()?;
        if prior_active {
            self.system.apply_tmpfiles()?;
            for unit in UNITS {
                self.system.enable(unit)?;
            }
            for unit in UNITS {
                self.system.start(unit)?;
            }
        }
        Ok(())
    }

    /// Verifies that all fixed sockets and services are active.
    ///
    /// # Errors
    ///
    /// Returns a redacted state error for any inactive or unreadable unit.
    pub(crate) fn verify_active(&mut self) -> Result<(), LinuxSystemdError> {
        for unit in UNITS {
            if !self.system.unit_state(unit)?.active {
                return Err(LinuxSystemdError::new(
                    LinuxSystemdErrorCode::StateQueryFailed,
                ));
            }
        }
        Ok(())
    }

    /// Classifies the complete fixed service set as active or inactive.
    ///
    /// Mixed enabled or active state is unsafe for install journaling and is
    /// refused. This method does not mutate systemd.
    ///
    /// # Errors
    ///
    /// Returns a redacted state error for mixed or unreadable unit state.
    pub(crate) fn classify_activation(&mut self) -> Result<bool, LinuxSystemdError> {
        let states = UNITS
            .iter()
            .copied()
            .map(|unit| self.system.unit_state(unit))
            .collect::<Result<Vec<_>, _>>()?;
        if states.iter().all(|state| state.active && state.enabled) {
            return Ok(true);
        }
        if states.iter().all(|state| !state.active && !state.enabled) {
            return Ok(false);
        }
        Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::StateQueryFailed,
        ))
    }

    /// Reloads unit definitions after an attempt-owned unit file is removed.
    ///
    /// # Errors
    ///
    /// Returns a redacted command failure when systemd cannot reload.
    pub(crate) fn reload_units(&mut self) -> Result<(), LinuxSystemdError> {
        self.system.daemon_reload()
    }
}

#[cfg(test)]
#[derive(Debug)]
struct PreflightTestSystem {
    calls: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(test)]
impl PreflightTestSystem {
    fn refuse<T>(&self) -> Result<T, LinuxSystemdError> {
        self.calls.set(self.calls.get().saturating_add(1));
        Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))
    }
}

#[cfg(test)]
impl SystemdSystem for PreflightTestSystem {
    fn daemon_reload(&mut self) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn apply_tmpfiles(&mut self) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn unit_state(&mut self, _unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        self.refuse()
    }

    fn enable(&mut self, _unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn disable(&mut self, _unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn start(&mut self, _unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn restart(&mut self, _unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }

    fn stop(&mut self, _unit: &'static str) -> Result<(), LinuxSystemdError> {
        self.refuse()
    }
}

#[derive(Debug)]
struct ProductionSystemdSystem {
    systemctl: PathBuf,
    tmpfiles: PathBuf,
}

impl ProductionSystemdSystem {
    fn new() -> Result<Self, LinuxSystemdError> {
        Ok(Self {
            systemctl: resolve_program(SYSTEMCTL_PATHS)?,
            tmpfiles: resolve_program(TMPFILES_PATHS)?,
        })
    }

    fn systemctl_status(
        &self,
        arguments: &[&str],
        false_codes: &[i32],
    ) -> Result<bool, LinuxSystemdError> {
        match run_status_code(&self.systemctl, arguments)? {
            0 => Ok(true),
            code if false_codes.contains(&code) => Ok(false),
            _ => Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            )),
        }
    }
}

impl SystemdSystem for ProductionSystemdSystem {
    fn daemon_reload(&mut self) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["daemon-reload"])
    }

    fn apply_tmpfiles(&mut self) -> Result<(), LinuxSystemdError> {
        run_success(&self.tmpfiles, &["--create", TMPFILES_CONFIG])
    }

    fn unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        Ok(UnitState {
            enabled: self.systemctl_status(&["is-enabled", "--quiet", unit], &[1, 4])?,
            active: self.systemctl_status(&["is-active", "--quiet", unit], &[3, 4])?,
        })
    }

    fn enable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["enable", unit])
    }

    fn disable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["disable", unit])
    }

    fn start(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["start", unit])
    }

    fn restart(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["restart", unit])
    }

    fn stop(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
        run_success(&self.systemctl, &["stop", unit])
    }
}

fn resolve_program(candidates: &[&str]) -> Result<PathBuf, LinuxSystemdError> {
    candidates
        .iter()
        .filter_map(|candidate| fs::canonicalize(candidate).ok())
        .find(|candidate| require_program(candidate).is_ok())
        .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::ProgramUnavailable))
}

fn require_program(program: &Path) -> Result<(), LinuxSystemdError> {
    let metadata = program
        .metadata()
        .map_err(|_| LinuxSystemdError::new(LinuxSystemdErrorCode::ProgramUnavailable))?;
    if !program.is_absolute()
        || !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::ProgramUnavailable,
        ));
    }
    Ok(())
}

fn run_success(program: &Path, arguments: &[&str]) -> Result<(), LinuxSystemdError> {
    if run_status_code(program, arguments)? == 0 {
        Ok(())
    } else {
        Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))
    }
}

fn run_status_code(program: &Path, arguments: &[&str]) -> Result<i32, LinuxSystemdError> {
    require_program(program)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))?;
    let status = wait_bounded(&mut child)?;
    status
        .code()
        .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))
}

fn wait_bounded(child: &mut Child) -> Result<ExitStatus, LinuxSystemdError> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                terminate(child);
                return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
            }
        }
    }
}

fn terminate(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    #[derive(Debug, Default)]
    struct FakeState {
        units: BTreeMap<&'static str, UnitState>,
        calls: Vec<String>,
        fail_call: Option<String>,
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
            self.state
                .borrow()
                .units
                .get(unit)
                .copied()
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

        fn restart(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError> {
            self.call(&format!("restart:{unit}"))?;
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

    #[test]
    fn absent_systemd_unit_status_is_false() -> Result<(), Box<dyn Error>> {
        let system = ProductionSystemdSystem {
            systemctl: PathBuf::from("/bin/sh"),
            tmpfiles: PathBuf::from("/bin/true"),
        };
        assert!(!system.systemctl_status(&["-c", "exit 4"], &[1, 4])?);
        assert!(system.systemctl_status(&["-c", "exit 5"], &[1, 4]).is_err());
        Ok(())
    }

    #[test]
    fn activation_orders_reload_tmpfiles_enable_and_start() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let state = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(manager.activate(ServiceActivation::Start)?);
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
    fn rollback_restores_only_state_changed_by_this_attempt() -> Result<(), Box<dyn Error>> {
        let mut states = all(UnitState {
            enabled: false,
            active: false,
        });
        states[0].1 = UnitState {
            enabled: true,
            active: true,
        };
        let fake = FakeSystem::new(states.clone());
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(manager.activate(ServiceActivation::Start)?);
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert_eq!(
            manager.activate(ServiceActivation::Start),
            Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))
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
    fn already_active_service_set_is_idempotent() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(!manager.activate(ServiceActivation::Preserve)?);
        assert_eq!(shared.borrow().calls, ["daemon-reload", "tmpfiles"]);
        manager.rollback()?;
        Ok(())
    }

    #[test]
    fn changed_active_service_set_is_restarted_and_rollback_resumes_prior_set()
    -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(manager.activate(ServiceActivation::Restart)?);
        assert_eq!(
            shared
                .borrow()
                .calls
                .iter()
                .filter(|call| call.starts_with("restart:"))
                .count(),
            UNITS.len()
        );
        manager.prepare_rollback()?;
        assert!(shared.borrow().units.values().all(|unit| !unit.active));
        manager.finish_rollback()?;
        assert!(shared.borrow().units.values().all(|unit| unit.active));
        Ok(())
    }

    #[test]
    fn changed_inactive_service_set_remains_inactive() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(!manager.activate(ServiceActivation::Preserve)?);
        assert!(shared.borrow().units.values().all(|unit| !unit.active));
        assert!(
            !shared
                .borrow()
                .calls
                .iter()
                .any(|call| call.starts_with("start:") || call.starts_with("restart:"))
        );
        Ok(())
    }

    #[test]
    fn recovery_requiesces_reboot_reactivation_before_resuming_prior_set()
    -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        manager.prepare_recovery()?;
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
        for state in shared.borrow_mut().units.values_mut() {
            state.enabled = true;
            state.active = true;
        }

        manager.prepare_recovery()?;
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
        manager.finish_recovery(true)?;
        assert!(
            shared
                .borrow()
                .units
                .values()
                .all(|state| state.enabled && state.active)
        );
        Ok(())
    }

    #[test]
    fn recovery_quiescence_does_not_command_an_absent_inactive_unit() -> Result<(), Box<dyn Error>>
    {
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        manager.prepare_recovery()?;

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
    fn failed_restart_rolls_back_to_the_prior_active_service_set() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        shared.borrow_mut().fail_call = Some("restart:pkg-root-helper.service".to_owned());
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert_eq!(
            manager.activate(ServiceActivation::Restart),
            Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))
        );
        shared.borrow_mut().fail_call = None;
        manager.prepare_rollback()?;
        manager.finish_rollback()?;
        assert!(shared.borrow().units.values().all(|unit| unit.active));
        Ok(())
    }

    #[test]
    fn activation_classification_accepts_only_complete_terminal_states()
    -> Result<(), Box<dyn Error>> {
        let mut active =
            LinuxSystemdManager::with_system(Box::new(FakeSystem::new(all(UnitState {
                enabled: true,
                active: true,
            }))));
        assert!(active.classify_activation()?);

        let mut inactive =
            LinuxSystemdManager::with_system(Box::new(FakeSystem::new(all(UnitState {
                enabled: false,
                active: false,
            }))));
        assert!(!inactive.classify_activation()?);
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(FakeSystem::new(states)));

        assert_eq!(
            manager
                .classify_activation()
                .map_err(super::LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
    }

    #[test]
    fn rollback_attempts_all_units_and_remains_retryable() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));
        assert!(manager.activate(ServiceActivation::Start)?);
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
    fn uninstall_deactivation_stops_disables_and_verifies_every_unit() -> Result<(), Box<dyn Error>>
    {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        manager.deactivate_for_uninstall()?;

        let state = shared.borrow();
        assert!(
            state
                .units
                .values()
                .all(|unit| !unit.enabled && !unit.active)
        );
        let reverse = UNITS.into_iter().rev().collect::<Vec<_>>();
        let stop_end = 1 + UNITS.len();
        let disable_end = stop_end + UNITS.len();
        assert_eq!(
            &state.calls[1..stop_end],
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert_eq!(
            manager.deactivate_for_uninstall(),
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
}
