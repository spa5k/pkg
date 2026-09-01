//! Reversible activation of the fixed Linux systemd service set.

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    error::Error,
    fmt, fs,
    io::Read,
    os::unix::{
        fs::MetadataExt,
        process::{CommandExt, ExitStatusExt},
    },
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
    terminal: LinuxSystemdTerminal,
    failure: Option<LinuxSystemdFailure>,
}

impl LinuxSystemdError {
    const fn new(code: LinuxSystemdErrorCode) -> Self {
        Self {
            code,
            terminal: LinuxSystemdTerminal::NotRun,
            failure: None,
        }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxSystemdErrorCode {
        self.code
    }

    const fn at(mut self, phase: LinuxSystemdFailurePhase, unit: Option<&'static str>) -> Self {
        self.failure = Some(LinuxSystemdFailure::new(
            phase,
            unit,
            self.code,
            self.terminal,
        ));
        self
    }

    const fn with_terminal(mut self, terminal: LinuxSystemdTerminal) -> Self {
        self.terminal = terminal;
        self
    }

    pub(crate) const fn failure(self) -> Option<LinuxSystemdFailure> {
        self.failure
    }
}

impl fmt::Display for LinuxSystemdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux service activation failed")
    }
}

impl Error for LinuxSystemdError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxSystemdFailurePhase {
    StateQuery,
    DaemonReload,
    AuthenticateUnit,
    Tmpfiles,
    Enable,
    Start,
    VerifyActive,
}

impl fmt::Display for LinuxSystemdErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ProgramUnavailable => "program-unavailable",
            Self::StateQueryFailed => "state-query-failed",
            Self::CommandFailed => "command-failed",
            Self::RollbackFailed => "rollback-failed",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxSystemdTerminal {
    NotRun,
    SpawnFailed,
    TimedOut,
    WaitFailed,
    ExitedNonzero(i32),
    Signaled(i32),
    OutputFailed,
}

impl fmt::Display for LinuxSystemdTerminal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRun => formatter.write_str("not-run"),
            Self::SpawnFailed => formatter.write_str("spawn-failed"),
            Self::TimedOut => formatter.write_str("timed-out"),
            Self::WaitFailed => formatter.write_str("wait-failed"),
            Self::ExitedNonzero(_) => formatter.write_str("exited-nonzero"),
            Self::Signaled(_) => formatter.write_str("signaled"),
            Self::OutputFailed => formatter.write_str("output-failed"),
        }
    }
}

impl fmt::Display for LinuxSystemdFailurePhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StateQuery => "state-query",
            Self::DaemonReload => "daemon-reload",
            Self::AuthenticateUnit => "authenticate-unit",
            Self::Tmpfiles => "tmpfiles",
            Self::Enable => "enable",
            Self::Start => "start",
            Self::VerifyActive => "verify-active",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxSystemdFailure {
    phase: LinuxSystemdFailurePhase,
    unit: Option<&'static str>,
    code: LinuxSystemdErrorCode,
    terminal: LinuxSystemdTerminal,
}

impl LinuxSystemdFailure {
    const fn new(
        phase: LinuxSystemdFailurePhase,
        unit: Option<&'static str>,
        code: LinuxSystemdErrorCode,
        terminal: LinuxSystemdTerminal,
    ) -> Self {
        Self {
            phase,
            unit,
            code,
            terminal,
        }
    }

    pub(crate) const fn not_run(
        phase: LinuxSystemdFailurePhase,
        unit: Option<&'static str>,
        code: LinuxSystemdErrorCode,
    ) -> Self {
        Self::new(phase, unit, code, LinuxSystemdTerminal::NotRun)
    }
}

impl fmt::Display for LinuxSystemdFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "phase={} class={} terminal={}",
            self.phase, self.code, self.terminal
        )?;
        match self.terminal {
            LinuxSystemdTerminal::ExitedNonzero(code) => {
                write!(formatter, " exit-code={code}")?;
            }
            LinuxSystemdTerminal::Signaled(signal) => {
                write!(formatter, " signal={signal}")?;
            }
            _ => {}
        }
        if let Some(unit) = self.unit {
            write!(formatter, " unit={unit}")?;
        }
        Ok(())
    }
}

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
    unit_fragments: [PathBuf; UNITS.len()],
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
            unit_fragments: UNIT_FRAGMENTS.map(PathBuf::from),
        })
    }

    #[cfg(test)]
    fn with_system(system: Box<dyn SystemdSystem>, unit_fragments: [PathBuf; UNITS.len()]) -> Self {
        Self {
            system,
            journal: None,
            unit_fragments,
        }
    }

    #[cfg(test)]
    pub(crate) fn inert_for_preflight_test() -> (Self, std::rc::Rc<std::cell::Cell<usize>>) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Self::with_system(
                Box::new(PreflightTestSystem {
                    calls: calls.clone(),
                }),
                UNIT_FRAGMENTS.map(PathBuf::from),
            ),
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
    pub(crate) fn activate_fresh(
        &mut self,
        authenticate_candidate: impl FnOnce() -> bool,
    ) -> Result<bool, LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
                .at(LinuxSystemdFailurePhase::StateQuery, None));
        }

        let prior = UNITS
            .iter()
            .copied()
            .map(|name| {
                self.system
                    .unit_state(name)
                    .map(|state| (name, state))
                    .map_err(|error| error.at(LinuxSystemdFailurePhase::StateQuery, Some(name)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some((unit, _)) = prior
            .iter()
            .find(|(_, state)| state.active || state.enabled)
        {
            return Err(
                LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                    .at(LinuxSystemdFailurePhase::StateQuery, Some(unit)),
            );
        }
        self.system
            .daemon_reload()
            .map_err(|error| error.at(LinuxSystemdFailurePhase::DaemonReload, None))?;
        if !authenticate_candidate() {
            return Err(
                LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                    .at(LinuxSystemdFailurePhase::AuthenticateUnit, None),
            );
        }
        self.authenticate_expected_definitions()?;
        let mut journal = prior
            .into_iter()
            .map(|(name, _)| UnitJournal {
                name,
                enable_intent: false,
                start_intent: false,
            })
            .collect::<Vec<_>>();
        self.journal = Some(journal.clone());
        self.system
            .apply_tmpfiles()
            .map_err(|error| error.at(LinuxSystemdFailurePhase::Tmpfiles, None))?;

        for index in 0..journal.len() {
            journal[index].enable_intent = true;
            self.journal = Some(journal.clone());
            let unit = journal[index].name;
            self.system
                .enable(unit)
                .map_err(|error| error.at(LinuxSystemdFailurePhase::Enable, Some(unit)))?;
        }
        for index in 0..journal.len() {
            journal[index].start_intent = true;
            self.journal = Some(journal.clone());
            let unit = journal[index].name;
            self.system
                .start(unit)
                .map_err(|error| error.at(LinuxSystemdFailurePhase::Start, Some(unit)))?;
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
        self.prepare_rollback(|| true)?;
        self.finish_rollback();
        Ok(())
    }

    /// Stops the fresh attempt before its product files are removed.
    ///
    /// # Errors
    ///
    /// Returns a rollback failure when the candidate set cannot be quiesced.
    pub(crate) fn prepare_rollback(
        &mut self,
        authenticate_candidate: impl FnOnce() -> bool,
    ) -> Result<(), LinuxSystemdError> {
        if self.journal.is_none() {
            return Ok(());
        }
        if !authenticate_candidate() || self.authenticate_expected_definitions().is_err() {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::RollbackFailed,
            ));
        }
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| LinuxSystemdError::new(LinuxSystemdErrorCode::RollbackFailed))?;
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

    /// Forgets the in-memory activation record after the service set is offline.
    ///
    pub(crate) fn finish_rollback(&mut self) {
        self.journal = None;
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
    pub(crate) fn deactivate_for_uninstall(
        &mut self,
        authenticate_owned_assets: impl FnOnce() -> bool,
    ) -> Result<(), LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        if !authenticate_owned_assets() {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            ));
        }
        let states = UNITS
            .iter()
            .copied()
            .enumerate()
            .map(|(index, unit)| {
                self.require_expected_definition(index, unit)?;
                self.system
                    .required_unit_state(unit)
                    .map(|state| (unit, state))
            })
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
    pub(crate) fn deactivate_fresh_recovery(
        &mut self,
        authenticate_candidate: impl FnOnce() -> bool,
    ) -> Result<(), LinuxSystemdError> {
        if self.journal.is_some() {
            return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed));
        }
        if !authenticate_candidate() || self.authenticate_expected_definitions().is_err() {
            return Err(LinuxSystemdError::new(
                LinuxSystemdErrorCode::StateQueryFailed,
            ));
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
            let state = self
                .system
                .unit_state(unit)
                .map_err(|error| error.at(LinuxSystemdFailurePhase::VerifyActive, Some(unit)))?;
            if !state.active {
                return Err(
                    LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                        .at(LinuxSystemdFailurePhase::VerifyActive, Some(unit)),
                );
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
            .map(|unit| {
                self.system
                    .unit_state(unit)
                    .map(|state| (unit, state))
                    .map_err(|error| error.at(LinuxSystemdFailurePhase::StateQuery, Some(unit)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if states
            .iter()
            .all(|(_, state)| state.active && state.enabled)
        {
            return Ok(true);
        }
        if states
            .iter()
            .all(|(_, state)| !state.active && !state.enabled)
        {
            return Ok(false);
        }
        let unit = states
            .iter()
            .find(|(_, state)| state.active != state.enabled)
            .or_else(|| {
                states
                    .windows(2)
                    .find(|pair| pair[0].1 != pair[1].1)
                    .map(|pair| &pair[1])
            })
            .map_or(UNITS[0], |(unit, _)| *unit);
        Err(
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                .at(LinuxSystemdFailurePhase::StateQuery, Some(unit)),
        )
    }

    /// Classifies the active state after authenticating all canonical unit
    /// fragments and refusing drop-ins.
    pub(crate) fn classify_exact_activation(&mut self) -> Result<bool, LinuxSystemdError> {
        self.authenticate_expected_definitions()?;
        self.classify_activation()
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

    pub(crate) fn require_fresh_recovery_offline(
        &mut self,
        recorded: impl Fn(&'static str) -> bool,
    ) -> Result<(), LinuxSystemdError> {
        for (index, unit) in UNITS.into_iter().enumerate() {
            let state = if recorded(unit) {
                self.require_expected_definition(index, unit)?;
                self.system.required_unit_state(unit)?
            } else {
                self.system.unit_state(unit)?
            };
            if state.active || state.enabled {
                return Err(LinuxSystemdError::new(
                    LinuxSystemdErrorCode::StateQueryFailed,
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn verify_offline_or_absent(&mut self) -> Result<(), LinuxSystemdError> {
        for unit in UNITS {
            let state = self.system.unit_state(unit)?;
            if state.active || state.enabled {
                return Err(LinuxSystemdError::new(
                    LinuxSystemdErrorCode::StateQueryFailed,
                ));
            }
        }
        Ok(())
    }

    fn authenticate_expected_definitions(&mut self) -> Result<(), LinuxSystemdError> {
        for (index, unit) in UNITS.into_iter().enumerate() {
            self.require_expected_definition(index, unit)
                .map_err(|error| {
                    error.at(LinuxSystemdFailurePhase::AuthenticateUnit, Some(unit))
                })?;
        }
        Ok(())
    }

    fn require_expected_definition(
        &mut self,
        index: usize,
        unit: &'static str,
    ) -> Result<(), LinuxSystemdError> {
        let definition = self.system.required_unit_definition(unit)?;
        if !same_file(
            Path::new(&definition.fragment_path),
            &self.unit_fragments[index],
        ) || !definition.drop_in_paths.is_empty()
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
            code => Err(
                LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                    .with_terminal(LinuxSystemdTerminal::ExitedNonzero(code)),
            ),
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
            enabled: parse_required_unit_file_state(&self.systemctl_value(&[
                "show",
                "--property=UnitFileState",
                "--value",
                unit,
            ])?)?,
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
    let code = run_status_code(program, arguments)?;
    if code == 0 {
        Ok(())
    } else {
        Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
            .with_terminal(LinuxSystemdTerminal::ExitedNonzero(code)))
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
    let mut child = command.spawn().map_err(|_| {
        LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
            .with_terminal(LinuxSystemdTerminal::SpawnFailed)
    })?;
    let status = wait_bounded(&mut child)?;
    status.code().ok_or_else(|| {
        LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed).with_terminal(
            status.signal().map_or(
                LinuxSystemdTerminal::WaitFailed,
                LinuxSystemdTerminal::Signaled,
            ),
        )
    })
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
    let mut child = command.spawn().map_err(|_| {
        LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
            .with_terminal(LinuxSystemdTerminal::SpawnFailed)
    })?;
    let status = wait_bounded(&mut child)?;
    if !status.success() {
        let terminal = status.code().map_or_else(
            || {
                status.signal().map_or(
                    LinuxSystemdTerminal::WaitFailed,
                    LinuxSystemdTerminal::Signaled,
                )
            },
            LinuxSystemdTerminal::ExitedNonzero,
        );
        return Err(
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed).with_terminal(terminal),
        );
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| {
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                .with_terminal(LinuxSystemdTerminal::OutputFailed)
        })?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                .with_terminal(LinuxSystemdTerminal::OutputFailed)
        })?;
    if bytes.len() > 4096 {
        return Err(
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                .with_terminal(LinuxSystemdTerminal::OutputFailed),
        );
    }
    String::from_utf8(bytes)
        .map(|value| value.trim_end_matches('\n').to_owned())
        .map_err(|_| {
            LinuxSystemdError::new(LinuxSystemdErrorCode::StateQueryFailed)
                .with_terminal(LinuxSystemdTerminal::OutputFailed)
        })
}

fn wait_bounded(child: &mut Child) -> Result<ExitStatus, LinuxSystemdError> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate(child);
                return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
                    .with_terminal(LinuxSystemdTerminal::TimedOut));
            }
            Err(_) => {
                terminate(child);
                return Err(LinuxSystemdError::new(LinuxSystemdErrorCode::CommandFailed)
                    .with_terminal(LinuxSystemdTerminal::WaitFailed));
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

fn same_file(left: &Path, right: &Path) -> bool {
    left.is_absolute()
        && right.is_absolute()
        && fs::canonicalize(left)
            .and_then(|left| fs::canonicalize(right).map(|right| left == right))
            .unwrap_or(false)
}

fn parse_required_unit_file_state(value: &str) -> Result<bool, LinuxSystemdError> {
    match value {
        "enabled" | "enabled-runtime" => Ok(true),
        "disabled" | "static" => Ok(false),
        _ => Err(LinuxSystemdError::new(
            LinuxSystemdErrorCode::StateQueryFailed,
        )),
    }
}

#[cfg(test)]
mod tests;
