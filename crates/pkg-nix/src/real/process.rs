//! Bounded subprocess execution for the real Nix adapter.

use super::*;
/// Callback that receives one stderr chunk during process execution.
pub(super) type StderrChunk<'a> = dyn FnMut(&[u8]) -> Result<(), NixAdapterError> + 'a;
pub(super) const MAX_STDOUT_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_STDERR_BYTES: usize = 128 * 1024 * 1024;
pub(super) const MAX_INTERNAL_JSON_LINE_BYTES: usize = 256 * 1024;
pub(super) const MAX_STDERR_CHUNKS_PER_TICK: usize = 64;
pub(super) const INTERNAL_JSON_PREFIX: &[u8] = b"@nix ";
pub(crate) trait CommandExecutor: Send + Sync {
    fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError>;

    fn execute_with_stderr(
        &self,
        spec: CommandSpec,
        cancelled: &dyn Fn() -> bool,
        stderr_chunk: &mut StderrChunk<'_>,
    ) -> Result<CommandOutcome, NixAdapterError> {
        if cancelled() {
            return Err(NixAdapterError::Unavailable);
        }
        let outcome = self.execute(spec)?;
        if cancelled() {
            return Err(NixAdapterError::Unavailable);
        }
        stderr_chunk(&outcome.stderr)?;
        Ok(outcome)
    }
}

pub(super) fn validated_process_executor(
    nix_binary: &Path,
    private_home: &Path,
    daemon_socket: Option<&Path>,
) -> Result<ProcessExecutor, NixAdapterError> {
    if !nix_binary.is_absolute()
        || !private_home.is_absolute()
        || daemon_socket.is_some_and(|path| !path.is_absolute())
    {
        return Err(NixAdapterError::ValidationFailure {
            summary: crate::error::BoundedSummary::new("adapter path is not absolute"),
        });
    }
    let home = fs::symlink_metadata(private_home).map_err(|_| NixAdapterError::Unavailable)?;
    if home.file_type().is_symlink() || !home.is_dir() || !is_private(&home) {
        return Err(NixAdapterError::PermissionDenied);
    }
    let temporary =
        fs::symlink_metadata(private_home.join("tmp")).map_err(|_| NixAdapterError::Unavailable)?;
    if temporary.file_type().is_symlink() || !temporary.is_dir() || !is_private(&temporary) {
        return Err(NixAdapterError::PermissionDenied);
    }
    let binary = fs::metadata(nix_binary).map_err(|_| NixAdapterError::Unavailable)?;
    if !binary.is_file() {
        return Err(NixAdapterError::Unavailable);
    }
    let nix_store_binary = nix_binary.with_file_name("nix-store");
    let legacy_binary =
        fs::metadata(&nix_store_binary).map_err(|_| NixAdapterError::Unavailable)?;
    if !legacy_binary.is_file() {
        return Err(NixAdapterError::Unavailable);
    }
    Ok(ProcessExecutor {
        nix_binary: nix_binary.to_path_buf(),
        nix_store_binary,
        private_home: private_home.to_path_buf(),
        daemon_socket: daemon_socket.map(Path::to_path_buf),
    })
}

pub(super) fn standard_determinate_process_executor(
    private_home: &Path,
) -> Result<ProcessExecutor, NixAdapterError> {
    validated_process_executor(
        Path::new(STANDARD_DETERMINATE_NIX_BINARY),
        private_home,
        None,
    )
}

pub(super) fn execute_checked(
    executor: &dyn CommandExecutor,
    program: NixProgram,
    args: Vec<OsString>,
    timeout: Duration,
) -> Result<CommandOutcome, NixAdapterError> {
    let outcome = executor.execute(CommandSpec {
        program,
        args,
        timeout,
    })?;
    if outcome.stdout_oversized || outcome.stderr_oversized {
        return Err(NixAdapterError::OversizedInput {
            limit_bytes: if outcome.stdout_oversized {
                MAX_STDOUT_BYTES
            } else {
                MAX_STDERR_BYTES
            },
        });
    }
    if outcome.timed_out {
        return Err(NixAdapterError::Timeout);
    }
    if outcome.code.is_none() {
        return Err(NixAdapterError::OperationFailed);
    }
    Ok(outcome)
}

pub(super) fn execute_checked_with_stderr(
    executor: &dyn CommandExecutor,
    program: NixProgram,
    args: Vec<OsString>,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
    stderr_chunk: &mut StderrChunk<'_>,
) -> Result<CommandOutcome, NixAdapterError> {
    let outcome = executor.execute_with_stderr(
        CommandSpec {
            program,
            args,
            timeout,
        },
        cancelled,
        stderr_chunk,
    )?;
    if outcome.stdout_oversized || outcome.stderr_oversized {
        return Err(NixAdapterError::OversizedInput {
            limit_bytes: if outcome.stdout_oversized {
                MAX_STDOUT_BYTES
            } else {
                MAX_STDERR_BYTES
            },
        });
    }
    if outcome.timed_out {
        return Err(NixAdapterError::Timeout);
    }
    if outcome.code.is_none() {
        return Err(NixAdapterError::OperationFailed);
    }
    Ok(outcome)
}

#[derive(Debug)]
pub(super) struct ProcessExecutor {
    pub(super) nix_binary: PathBuf,
    pub(super) nix_store_binary: PathBuf,
    pub(super) private_home: PathBuf,
    pub(super) daemon_socket: Option<PathBuf>,
}

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError> {
        self.execute_process(&spec, &|| false, &mut |_| Ok(()))
    }

    fn execute_with_stderr(
        &self,
        spec: CommandSpec,
        cancelled: &dyn Fn() -> bool,
        stderr_chunk: &mut StderrChunk<'_>,
    ) -> Result<CommandOutcome, NixAdapterError> {
        self.execute_process(&spec, cancelled, stderr_chunk)
    }
}

impl ProcessExecutor {
    pub(super) fn execute_process(
        &self,
        spec: &CommandSpec,
        cancelled: &dyn Fn() -> bool,
        stderr_chunk: &mut StderrChunk<'_>,
    ) -> Result<CommandOutcome, NixAdapterError> {
        let mut child = build_command(self, spec)
            .spawn()
            .map_err(|_| NixAdapterError::Unavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(NixAdapterError::OperationFailed)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(NixAdapterError::OperationFailed)?;
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(64);
        let stdout_reader = thread::spawn(move || read_bounded(stdout, MAX_STDOUT_BYTES));
        let stderr_reader =
            thread::spawn(move || read_bounded_forward(stderr, MAX_STDERR_BYTES, &stderr_tx));
        let started = Instant::now();
        let mut observed_status = None;
        let mut timed_out = false;
        let mut callback_error = None;
        let status = loop {
            drain_stderr_chunks(&stderr_rx, &mut callback_error, stderr_chunk);
            if callback_error.is_none() && cancelled() {
                callback_error = Some(NixAdapterError::Unavailable);
            }
            if observed_status.is_none() {
                observed_status = child
                    .try_wait()
                    .map_err(|_| NixAdapterError::OperationFailed)?;
            }
            if callback_error.is_some() && observed_status.is_none() {
                observed_status = Some(terminate_and_reap(&mut child, None)?);
            }
            if let Some(status) = observed_status
                && stdout_reader.is_finished()
                && stderr_reader.is_finished()
            {
                drain_stderr_chunks(&stderr_rx, &mut callback_error, stderr_chunk);
                break status;
            }
            if !timed_out && started.elapsed() >= spec.timeout {
                observed_status = Some(terminate_and_reap(&mut child, observed_status)?);
                timed_out = true;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let (stdout, stdout_oversized) = stdout_reader
            .join()
            .map_err(|_| NixAdapterError::OperationFailed)?
            .map_err(|_| NixAdapterError::OperationFailed)?;
        let (stderr, stderr_oversized) = stderr_reader
            .join()
            .map_err(|_| NixAdapterError::OperationFailed)?
            .map_err(|_| NixAdapterError::OperationFailed)?;
        if let Some(error) = callback_error {
            return Err(error);
        }
        Ok(CommandOutcome {
            code: status.code(),
            stdout,
            stderr,
            stdout_oversized,
            stderr_oversized,
            timed_out,
        })
    }
}

#[derive(Debug)]
pub(crate) struct CommandSpec {
    pub(super) program: NixProgram,
    pub(super) args: Vec<OsString>,
    pub(super) timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NixProgram {
    Modern,
    LegacyStore,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandOutcome {
    pub(super) code: Option<i32>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) stdout_oversized: bool,
    pub(super) stderr_oversized: bool,
    pub(super) timed_out: bool,
}

#[cfg(unix)]
pub(super) fn terminate_and_reap(
    child: &mut std::process::Child,
    observed_status: Option<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus, NixAdapterError> {
    let group = Pid::from_child(&*child);
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NixAdapterError::OperationFailed);
        }
    }
    match observed_status {
        Some(status) => Ok(status),
        None => child.wait().map_err(|_| NixAdapterError::OperationFailed),
    }
}

#[cfg(not(unix))]
pub(super) fn terminate_and_reap(
    child: &mut std::process::Child,
    observed_status: Option<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus, NixAdapterError> {
    if let Some(status) = observed_status {
        return Ok(status);
    }
    child.kill().map_err(|_| NixAdapterError::OperationFailed)?;
    child.wait().map_err(|_| NixAdapterError::OperationFailed)
}

pub(super) fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::new();
    let mut oversized = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(stored.len());
        stored.extend_from_slice(&chunk[..count.min(remaining)]);
        oversized |= count > remaining;
    }
    Ok((stored, oversized))
}

pub(super) fn read_bounded_forward(
    mut reader: impl Read,
    limit: usize,
    sender: &mpsc::SyncSender<Vec<u8>>,
) -> io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::new();
    let mut oversized = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(stored.len());
        stored.extend_from_slice(&chunk[..count.min(remaining)]);
        oversized |= count > remaining;
        if sender.send(chunk[..count].to_vec()).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stderr progress receiver closed",
            ));
        }
    }
    Ok((stored, oversized))
}

#[derive(Debug, Default)]
pub(super) struct InternalBuildProgressParser {
    pub(super) pending: Vec<u8>,
    pub(super) dropping_oversized_line: bool,
    pub(super) build_activity_ids: BTreeSet<u64>,
    pub(super) last_millionths: u32,
}

impl InternalBuildProgressParser {
    pub(super) fn push(
        &mut self,
        chunk: &[u8],
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<(), NixAdapterError> {
        let mut remaining = chunk;
        if self.dropping_oversized_line {
            let Some(end) = remaining.iter().position(|byte| *byte == b'\n') else {
                return Ok(());
            };
            self.dropping_oversized_line = false;
            remaining = &remaining[end + 1..];
        }
        self.pending.extend_from_slice(remaining);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() <= MAX_INTERNAL_JSON_LINE_BYTES {
                self.parse_line(&line, progress)?;
            }
        }
        if self.pending.len() > MAX_INTERNAL_JSON_LINE_BYTES {
            self.pending.clear();
            self.dropping_oversized_line = true;
        }
        Ok(())
    }

    pub(super) fn finish(
        &mut self,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<(), NixAdapterError> {
        if !self.dropping_oversized_line && !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.parse_line(&line, progress)?;
        }
        Ok(())
    }

    pub(super) fn parse_line(
        &mut self,
        line: &[u8],
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<(), NixAdapterError> {
        let Some(payload) = line.strip_prefix(INTERNAL_JSON_PREFIX) else {
            return Ok(());
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
            return Ok(());
        };
        let Some(action) = value.get("action").and_then(serde_json::Value::as_str) else {
            return Ok(());
        };
        let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) else {
            return Ok(());
        };
        match action {
            "start"
                if value.get("type").and_then(serde_json::Value::as_u64) == Some(ACT_BUILDS) =>
            {
                self.build_activity_ids.insert(id);
            }
            "stop" => {
                self.build_activity_ids.remove(&id);
            }
            "result"
                if self.build_activity_ids.contains(&id)
                    && value.get("type").and_then(serde_json::Value::as_u64)
                        == Some(RESULT_PROGRESS) =>
            {
                let Some(fields) = value.get("fields").and_then(serde_json::Value::as_array) else {
                    return Ok(());
                };
                if fields.len() != 4 {
                    return Ok(());
                }
                let Some(done) = fields[0].as_u64() else {
                    return Ok(());
                };
                let Some(expected) = fields[1].as_u64() else {
                    return Ok(());
                };
                if fields[2].as_u64().is_none() || fields[3].as_u64().is_none() {
                    return Ok(());
                }
                if expected == 0 || done == 0 || done > expected {
                    return Ok(());
                }
                let scaled = ((u128::from(done) * u128::from(BuildProgressEstimate::SCALE))
                    / u128::from(expected))
                .min(u128::from(BuildProgressEstimate::SCALE - 1))
                    as u32;
                if scaled > self.last_millionths {
                    let estimate = BuildProgressEstimate::new(scaled)?;
                    progress(estimate)?;
                    self.last_millionths = scaled;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn base_args() -> Vec<OsString> {
    os_args([
        "--extra-experimental-features",
        "nix-command flakes",
        "--option",
        "allow-import-from-derivation",
        "false",
    ])
}

pub(super) fn root_store_args() -> Vec<OsString> {
    let mut args = base_args();
    // Nix 2.34.8's daemon protocol rejects repairPath even for root. The
    // privileged helper therefore opens only the fixed managed local store;
    // no caller-selectable store URL crosses the capability boundary.
    args.extend(os_args(["--store", "local"]));
    args
}

pub(super) fn os_args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

/// Builds the fully configured command for one execution request.
pub(super) fn build_command(
    executor: &ProcessExecutor,
    spec: &CommandSpec,
) -> std::process::Command {
    let binary = match spec.program {
        NixProgram::Modern => &executor.nix_binary,
        NixProgram::LegacyStore => &executor.nix_store_binary,
    };
    let mut command = Command::new(binary);
    command
        .args(&spec.args)
        .env_clear()
        .env("HOME", &executor.private_home)
        .env("TMPDIR", executor.private_home.join("tmp"))
        .env("NIX_USER_CONF_FILES", "")
        .env("PATH", MANAGED_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(daemon_socket) = &executor.daemon_socket {
        command
            .env("NIX_CONFIG", MANAGED_NIX_CONFIG)
            .env("NIX_DAEMON_SOCKET_PATH", daemon_socket)
            .env("NIX_REMOTE", "daemon")
            .env("NIX_STATE_DIR", MANAGED_NIX_STATE);
    }
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// Forwards one batch of pending stderr chunks to the callback and records
/// the first callback failure.
pub(super) fn drain_stderr_chunks(
    stderr_rx: &mpsc::Receiver<Vec<u8>>,
    callback_error: &mut Option<NixAdapterError>,
    stderr_chunk: &mut StderrChunk<'_>,
) {
    for chunk in stderr_rx.try_iter().take(MAX_STDERR_CHUNKS_PER_TICK) {
        if callback_error.is_none()
            && let Err(error) = stderr_chunk(&chunk)
        {
            *callback_error = Some(error);
        }
    }
}
