//! Reversible activation of the fixed Linux systemd service set.

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    error::Error,
    fmt, fs,
    io::Read,
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

const UNIT_FRAGMENTS: [&str; 4] = [
    "/usr/lib/systemd/system/pkg-root-helper.socket",
    "/usr/lib/systemd/system/pkg-nix-broker.socket",
    "/usr/lib/systemd/system/pkg-root-helper.service",
    "/usr/lib/systemd/system/pkg-nix-broker.service",
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnitDefinition {
    fragment_path: String,
    drop_in_paths: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnitJournal {
    name: &'static str,
    enable_intent: bool,
    start_intent: bool,
}

trait SystemdSystem: fmt::Debug {
    fn daemon_reload(&mut self) -> Result<(), LinuxSystemdError>;
    fn apply_tmpfiles(&mut self) -> Result<(), LinuxSystemdError>;
    fn unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError>;
    fn required_unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError>;
    fn required_unit_definition(
        &mut self,
        unit: &'static str,
    ) -> Result<UnitDefinition, LinuxSystemdError>;
    fn enable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn disable(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
    fn start(&mut self, unit: &'static str) -> Result<(), LinuxSystemdError>;
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
    pub(crate) fn activate_fresh(&mut self) -> Result<bool, LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }

        let prior = UNITS
            .iter()
            .copied()
            .map(|name| self.system.unit_state(name).map(|state| (name, state)))
            .collect::<Result<Vec<_>, _>>()?;
        if prior.iter().any(|(_, state)| state.active || state.enabled) {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            ));
        }
        self.system.daemon_reload()?;
        let mut journal = prior
            .into_iter()
            .map(|(name, _)| UnitJournal {
                name,
                enable_intent: false,
                start_intent: false,
            })
            .collect::<Vec<_>>();
        self.journal = Some(journal.clone());
        self.system.apply_tmpfiles()?;

        for index in 0..journal.len() {
            journal[index].enable_intent = true;
            self.journal = Some(journal.clone());
            self.system.enable(journal[index].name)?;
        }
        for index in 0..journal.len() {
            journal[index].start_intent = true;
            self.journal = Some(journal.clone());
            self.system.start(journal[index].name)?;
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

    /// Stops the fresh attempt before its product files are removed.
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

    /// Reloads systemd after fresh-install attempt files are removed.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the prior set cannot be resumed.
    pub(crate) fn finish_rollback(&mut self) -> Result<(), LinuxSystemdError> {
        if self.journal.is_none() {
            return Ok(());
        }
        if self.system.daemon_reload().is_err() {
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

    /// Deactivates only an authenticated fresh-install service set before file rollback.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the candidate set cannot be quiesced.
    pub(crate) fn deactivate_fresh_recovery(&mut self) -> Result<(), LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        let states = UNITS
            .iter()
            .copied()
            .enumerate()
            .map(|(index, unit)| {
                self.system.unit_state(unit).and_then(|state| {
                    if state.active || state.enabled {
                        self.require_expected_definition(index, unit)?;
                    }
                    Ok((unit, state))
                })
            })
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
        if UNITS.iter().copied().any(|unit| {
            !matches!(
                self.system.unit_state(unit),
                Ok(UnitState {
                    enabled: false,
                    active: false
                })
            )
        }) {
            failed = true;
        }
        if failed {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
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

    /// Requires every fixed product unit to be inactive and disabled.
    ///
    /// This operation only reads unit state. It does not reload, enable,
    /// disable, start, stop, or restart any unit.
    ///
    /// # Errors
    ///
    /// Returns a redacted state error for active, enabled, mixed, or unreadable state.
    pub(crate) fn require_offline(&mut self) -> Result<(), LinuxSystemdError> {
        for (index, unit) in UNITS.into_iter().enumerate() {
            self.require_expected_definition(index, unit)?;
            let state = self.system.required_unit_state(unit)?;
            if state.active || state.enabled {
                return Err(LinuxSystemdError::new(
                    LinuxSystemdErrorCode::StateQueryFailed,
                ));
            }
        }
        Ok(())
    }

    fn require_expected_definition(
        &mut self,
        index: usize,
        unit: &'static str,
    ) -> Result<(), LinuxSystemdError> {
        let definition = self.system.required_unit_definition(unit)?;
        if definition.fragment_path != UNIT_FRAGMENTS[index] || !definition.drop_in_paths.is_empty()
        {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            ));
        }
        Ok(())
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

    fn required_unit_state(&mut self, _unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        self.refuse()
    }

    fn required_unit_definition(
        &mut self,
        _unit: &'static str,
    ) -> Result<UnitDefinition, LinuxSystemdError> {
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

    fn systemctl_value(&self, arguments: &[&str]) -> Result<String, LinuxSystemdError> {
        run_output(&self.systemctl, arguments)
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

    fn required_unit_state(&mut self, unit: &'static str) -> Result<UnitState, LinuxSystemdError> {
        Ok(UnitState {
            enabled: self.systemctl_status(&["is-enabled", "--quiet", unit], &[1])?,
            active: self.systemctl_status(&["is-active", "--quiet", unit], &[3])?,
        })
    }

    fn required_unit_definition(
        &mut self,
        unit: &'static str,
    ) -> Result<UnitDefinition, LinuxSystemdError> {
        Ok(UnitDefinition {
            fragment_path: self.systemctl_value(&[
                "show",
                "--property=FragmentPath",
                "--value",
                unit,
            ])?,
            drop_in_paths: self.systemctl_value(&[
                "show",
                "--property=DropInPaths",
                "--value",
                unit,
            ])?,
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

fn run_output(program: &Path, arguments: &[&str]) -> Result<String, LinuxSystemdError> {
    require_program(program)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed))?;
    let status = wait_bounded(&mut child)?;
    if !status.success() {
        return Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::StateQueryFailed,
        ));
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))?;
    if bytes.len() > 4096 {
        return Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::StateQueryFailed,
        ));
    }
    String::from_utf8(bytes)
        .map(|value| value.trim_end_matches('\n').to_owned())
        .map_err(|_| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))
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
        definitions: BTreeMap<&'static str, UnitDefinition>,
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
                    definitions: UNITS
                        .into_iter()
                        .zip(UNIT_FRAGMENTS)
                        .map(|(unit, fragment_path)| {
                            (
                                unit,
                                UnitDefinition {
                                    fragment_path: fragment_path.to_owned(),
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
            self.state
                .borrow()
                .units
                .get(unit)
                .copied()
                .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed))
        }

        fn required_unit_state(
            &mut self,
            unit: &'static str,
        ) -> Result<UnitState, LinuxSystemdError> {
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
    fn activation_orders_reload_tmpfiles_enable_and_start() -> Result<(), Box<dyn Error>> {
        let fake = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let state = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(manager.activate_fresh()?);
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
    fn fresh_rollback_leaves_the_complete_service_set_offline() -> Result<(), Box<dyn Error>> {
        let states = all(UnitState {
            enabled: false,
            active: false,
        });
        let fake = FakeSystem::new(states.clone());
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert!(manager.activate_fresh()?);
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
            manager.activate_fresh(),
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
    fn fresh_activation_refuses_a_preexisting_active_service_set() {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert_eq!(
            manager.activate_fresh().map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert!(shared.borrow().calls.is_empty());
    }

    #[test]
    fn fresh_recovery_deactivates_only_the_authenticated_loaded_set() -> Result<(), Box<dyn Error>>
    {
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        manager.deactivate_fresh_recovery()?;
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
        let fake = FakeSystem::new(all(UnitState {
            enabled: true,
            active: true,
        }));
        let shared = Rc::clone(&fake.state);
        shared.borrow_mut().definitions.insert(
            "pkg-root-helper.service",
            UnitDefinition {
                fragment_path: "/etc/systemd/system/pkg-root-helper.service".to_owned(),
                drop_in_paths: String::new(),
            },
        );
        let prior = shared.borrow().units.clone();
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));

        assert_eq!(
            manager
                .deactivate_fresh_recovery()
                .map_err(LinuxSystemdError::code),
            Err(LinuxSystemdErrorCode::StateQueryFailed)
        );
        assert!(shared.borrow().calls.is_empty());
        assert_eq!(shared.borrow().units, prior);
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

        manager.deactivate_fresh_recovery()?;

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
    fn offline_preflight_is_query_only_and_refuses_every_non_offline_state() {
        let offline = FakeSystem::new(all(UnitState {
            enabled: false,
            active: false,
        }));
        let offline_state = Rc::clone(&offline.state);
        let mut manager = LinuxSystemdManager::with_system(Box::new(offline));
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
            let mut manager = LinuxSystemdManager::with_system(Box::new(fake));
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(wrong_fragment));
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(with_drop_in));
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
        let mut manager = LinuxSystemdManager::with_system(Box::new(fake));
        assert!(manager.activate_fresh()?);
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
