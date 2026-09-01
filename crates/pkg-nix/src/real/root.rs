//! Root-helper Nix repair and garbage-collection executors.

use super::process::*;
use super::*;
pub(super) const MAX_REPAIR_CLOSURE: usize = 4096;
/// Root-helper-only executor for the fixed, capability-validated Nix repair
/// operation.
///
/// This type accepts no raw command, option, substituter, or path outside the
/// [`VerifiedRepairScope`]. Cache-only mode disables every build worker and
/// verifies each path after the repair attempt before reporting a cache miss.
pub struct RootNixRepairExecutor {
    pub(super) executor: Arc<dyn CommandExecutor>,
}

/// Root-installer-only executor for garbage-collecting the fixed managed local
/// Nix store after the managed daemon has stopped.
///
/// The operation accepts no raw command, store URL, path, or option. It first
/// validates Nix's bounded dead-path report and then invokes garbage collection
/// directly against the local store, without using the daemon socket.
pub struct RootNixGcExecutor {
    pub(super) executor: Arc<dyn CommandExecutor>,
}

impl std::fmt::Debug for RootNixRepairExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootNixRepairExecutor")
            .finish_non_exhaustive()
    }
}

impl RootNixRepairExecutor {
    /// Constructs the managed root-only repair executor used on macOS.
    pub fn new(nix_binary: &Path, private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(validated_process_executor(
                nix_binary,
                private_home,
                Some(Path::new(MANAGED_DAEMON_SOCKET)),
            )?),
        })
    }

    /// Constructs the Linux repair executor for the fixed standard Determinate profile.
    ///
    /// The vendor environment is preserved. No caller can select a binary,
    /// daemon socket, Nix configuration, state directory, or remote.
    pub fn new_standard_determinate(private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(standard_determinate_process_executor(private_home)?),
        })
    }

    #[cfg(test)]
    pub(super) fn scripted(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    pub(super) fn run(
        &self,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, MaintenanceError> {
        execute_checked(self.executor.as_ref(), NixProgram::Modern, args, timeout)
            .map_err(|_| MaintenanceError::backend_failure())
    }
}

impl std::fmt::Debug for RootNixGcExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootNixGcExecutor")
            .finish_non_exhaustive()
    }
}

impl RootNixGcExecutor {
    /// Constructs the root-only garbage collector from installer-authenticated
    /// absolute binary and private-home paths.
    ///
    /// # Errors
    ///
    /// Returns a redacted adapter error when either fixed binary is unavailable
    /// or the private execution directories fail validation.
    pub fn new(nix_binary: &Path, private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(validated_process_executor(
                nix_binary,
                private_home,
                Some(Path::new(MANAGED_DAEMON_SOCKET)),
            )?),
        })
    }

    #[cfg(test)]
    pub(super) fn scripted(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Collects unreachable objects from the fixed local managed store.
    ///
    /// This method does not contact the managed daemon and accepts no
    /// caller-selected command, store, path, or option.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error if the dead-path report is malformed,
    /// either fixed command fails, output exceeds its bound, or execution times
    /// out.
    pub fn collect(&self) -> Result<GcReport, NixAdapterError> {
        collect_garbage(self.executor.as_ref(), os_args(["--store", "local"]), None)
    }

    /// Resolves the exact local closure protected by product GC roots.
    ///
    /// The command is fixed to the local store and recursive JSON path-info.
    /// It accepts only validated store roots and returns a canonical bounded set.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error for excessive input, missing roots,
    /// malformed output, a non-local store, or command failure.
    pub fn closure_for_roots(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<StorePath>, NixAdapterError> {
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        if roots.len() > MAX_UNINSTALL_ROOTS
            || roots
                .iter()
                .try_fold(0_usize, |total, root| {
                    total.checked_add(root.as_str().len())
                })
                .is_none_or(|total| total > MAX_UNINSTALL_ROOT_BYTES)
        {
            return Err(NixAdapterError::OperationFailed);
        }
        let mut args = base_args();
        args.extend(os_args([
            "path-info",
            "--json",
            "--json-format",
            "2",
            "--recursive",
            "--store",
            "local",
        ]));
        args.extend(roots.iter().map(|root| root.as_str().into()));
        let outcome = execute_checked(
            self.executor.as_ref(),
            NixProgram::Modern,
            args,
            SHORT_TIMEOUT,
        )?;
        if outcome.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        let raw: RawPathInfoEnvelope = parse_json(&outcome.stdout)?;
        validate_path_info_envelope(&raw)?;
        for root in roots {
            root_path_info(&raw, root)?;
        }
        let mut closure = Vec::new();
        for (path, info) in raw.info {
            if info.is_none() {
                return Err(malformed());
            }
            closure.push(store_path(&path)?);
        }
        closure.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        closure.dedup_by(|left, right| left.as_str() == right.as_str());
        Ok(closure)
    }

    /// Lists every valid path registered in the fixed local managed store.
    ///
    /// The command accepts no installable or caller-selected store.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error for malformed, missing, or excessive
    /// local path information.
    pub fn registered_paths(&self) -> Result<Vec<StorePath>, NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args([
            "path-info",
            "--all",
            "--json",
            "--json-format",
            "2",
            "--store",
            "local",
        ]));
        let outcome = execute_checked(
            self.executor.as_ref(),
            NixProgram::Modern,
            args,
            SHORT_TIMEOUT,
        )?;
        if outcome.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        let raw: RawPathInfoEnvelope = parse_json(&outcome.stdout)?;
        validate_path_info_envelope(&raw)?;
        let mut paths = Vec::with_capacity(raw.info.len());
        for (path, info) in raw.info {
            if info.is_none() {
                return Err(malformed());
            }
            paths.push(store_path(&path)?);
        }
        paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        paths.dedup_by(|left, right| left.as_str() == right.as_str());
        Ok(paths)
    }
}

impl VerifiedRepairExecutor for RootNixRepairExecutor {
    fn execute(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<Vec<RepairOutcomeKind>, MaintenanceError> {
        let deadline = Instant::now()
            .checked_add(MAX_REPAIR_EXECUTION_DURATION)
            .ok_or_else(MaintenanceError::backend_failure)?;
        let mut outcomes = Vec::with_capacity(scope.paths().len());
        for path in scope.paths() {
            let mut repair = root_store_args();
            repair.extend(os_args([
                "--option",
                "max-jobs",
                match scope.mode() {
                    RepairMode::CacheOnly => "0",
                    RepairMode::Build => "1",
                },
                "--option",
                "builders",
                "",
                "store",
                "repair",
            ]));
            repair.push(OsString::from(path.as_str()));
            if self.run(repair, repair_time_remaining(deadline)?)?.code != Some(0) {
                return Err(MaintenanceError::backend_failure());
            }

            let mut verify = root_store_args();
            verify.extend(os_args(["store", "verify", "--no-trust"]));
            verify.push(OsString::from(path.as_str()));
            let verify = self.run(verify, repair_short_time_remaining(deadline)?)?;
            if verify.code == Some(0) {
                outcomes.push(RepairOutcomeKind::Restored);
                continue;
            }
            if scope.mode() != RepairMode::CacheOnly {
                return Err(MaintenanceError::backend_failure());
            }

            let mut info = root_store_args();
            info.extend(os_args(["store", "info"]));
            if self.run(info, repair_short_time_remaining(deadline)?)?.code != Some(0) {
                return Err(MaintenanceError::backend_failure());
            }
            outcomes.push(RepairOutcomeKind::CacheMiss);
        }
        Ok(outcomes)
    }
}

pub(super) fn repair_time_remaining(deadline: Instant) -> Result<Duration, MaintenanceError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(MaintenanceError::backend_failure)
}

pub(super) fn repair_short_time_remaining(deadline: Instant) -> Result<Duration, MaintenanceError> {
    Ok(repair_time_remaining(deadline)?.min(SHORT_TIMEOUT))
}

pub(super) fn collect_garbage(
    executor: &dyn CommandExecutor,
    fixed_prefix: Vec<OsString>,
    operation_deadline: Option<Instant>,
) -> Result<GcReport, NixAdapterError> {
    let mut preflight_args = fixed_prefix.clone();
    preflight_args.extend(os_args(["--gc", "--print-dead"]));
    let preflight = execute_checked(
        executor,
        NixProgram::LegacyStore,
        preflight_args,
        bounded_timeout(operation_deadline, GC_TIMEOUT)?,
    )?;
    if preflight.code != Some(0) {
        return Err(NixAdapterError::OperationFailed);
    }
    // Validate the bounded report shape before the destructive call. This
    // scales with dead paths rather than total store size.
    GcReport::new(
        GcStatus::Collected,
        parse_gc_candidates(&preflight.stdout)?,
        0,
    )?;

    let mut collect_args = fixed_prefix;
    collect_args.extend(os_args(["--gc"]));
    let outcome = execute_checked(
        executor,
        NixProgram::LegacyStore,
        collect_args,
        bounded_timeout(operation_deadline, GC_TIMEOUT)?,
    )?;
    if outcome.code != Some(0) {
        return Err(NixAdapterError::OperationFailed);
    }
    let collected = parse_gc_deletions(&outcome.stderr)?;
    GcReport::new(GcStatus::Collected, collected, 0)
}

pub(super) fn bounded_timeout(
    deadline: Option<Instant>,
    timeout: Duration,
) -> Result<Duration, NixAdapterError> {
    match deadline {
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| remaining.min(timeout))
            .ok_or(NixAdapterError::Timeout),
        None => Ok(timeout),
    }
}

pub(super) fn parse_gc_candidates(stdout: &[u8]) -> Result<Vec<StorePath>, NixAdapterError> {
    let text = std::str::from_utf8(stdout).map_err(|_| malformed())?;
    text.lines()
        .map(normalize_gc_store_entry)
        .filter_map(Result::transpose)
        .collect()
}

pub(super) fn parse_gc_deletions(stderr: &[u8]) -> Result<Vec<StorePath>, NixAdapterError> {
    let text = std::str::from_utf8(stderr).map_err(|_| malformed())?;
    text.lines()
        .filter_map(|line| {
            line.strip_prefix("deleting '")
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .map(normalize_gc_store_entry)
        .filter_map(Result::transpose)
        .collect()
}

pub(super) fn normalize_gc_store_entry(value: &str) -> Result<Option<StorePath>, NixAdapterError> {
    let relative = value.strip_prefix("/nix/store/").ok_or_else(malformed)?;
    if relative.is_empty() || relative.contains('/') {
        return Err(malformed());
    }
    match StorePath::new(value) {
        Ok(path) => Ok(Some(path)),
        // The pinned collector also reports invalid direct children (for
        // example its `trash` directory). They are housekeeping, not Nix
        // store objects and therefore cannot enter the product report.
        Err(_) => Ok(None),
    }
}
