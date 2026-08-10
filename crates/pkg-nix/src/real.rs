//! Pinned Nix 2.34.8 subprocess adapter used by the managed broker.
//!
//! All argv is assembled from validated strong types plus fixed product policy.
//! The child environment is cleared, output is bounded, stderr is retained only
//! for internal parsing, and every upstream format is validated before normalization.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use pkg_core::state::body_digest;
#[cfg(unix)]
use rustix::{
    io::Errno,
    process::{Pid, Signal, kill_process_group},
};
use serde::{Deserialize, Serialize};

use crate::{
    AcceptedFormats, BuildOutput, BuildOutputProvenance, BuildReport, BuildRequest, BuildStatus,
    DerivationPath, DerivationPlanReport, DerivationSystem, EvaluateDerivationRequest,
    EvaluatedDerivation, FormatVersion, GcReport, GcStatus, MaintenanceError, MethodKind, NarHash,
    NarIntegrity, NixAdapter, NixAdapterError, NixVersion, OutputName, PathInfoReport,
    PathVerifyResult, RepairMode, RepairOutcomeKind, Signature, StorePath, SubstituteOutcome,
    SubstituteReceipt, SubstituteReport, TrustStatus, VerifiedRepairExecutor, VerifiedRepairScope,
    VerifyMode, VerifyReport, VerifyRequest, VersionInfo,
};

const PINNED_NIX_VERSION: &str = "2.34.8";
const PATH_INFO_FORMAT: u32 = 2;
const STORE_DIRECTORY: &str = "/nix/store";
const CACHE_URL: &str = "https://cache.nixos.org";
const CACHE_SIGNING_KEY_NAME: &str = "cache.nixos.org-1";
const MANAGED_NIX_CONFIG: &str = "include /opt/pkg/etc/pkg/nix.conf";
const MANAGED_NIX_STATE: &str = "/nix/var/nix";
const MANAGED_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";
const MANAGED_PATH: &str = "/usr/bin:/bin";
const MAX_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 128 * 1024 * 1024;
const SHORT_TIMEOUT: Duration = Duration::from_secs(60);
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const GC_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const REPAIR_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Real, version-pinned adapter around the product-managed Nix executable.
pub struct RealNixAdapter {
    executor: Arc<dyn CommandExecutor>,
}

/// Root-helper-only executor for the fixed, capability-validated Nix repair
/// operation.
///
/// This type accepts no raw command, option, substituter, or path outside the
/// [`VerifiedRepairScope`]. Cache-only mode disables every build worker and
/// verifies each path after the repair attempt before reporting a cache miss.
pub struct RootNixRepairExecutor {
    executor: Arc<dyn CommandExecutor>,
}

impl std::fmt::Debug for RootNixRepairExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootNixRepairExecutor")
            .finish_non_exhaustive()
    }
}

impl RootNixRepairExecutor {
    /// Constructs the root-only repair executor from installer-authenticated
    /// absolute binary and private-home paths.
    pub fn new(nix_binary: &Path, private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(validated_process_executor(nix_binary, private_home)?),
        })
    }

    #[cfg(test)]
    fn scripted(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    fn run(
        &self,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, MaintenanceError> {
        execute_checked(self.executor.as_ref(), NixProgram::Modern, args, timeout)
            .map_err(|_| MaintenanceError::backend_failure())
    }
}

impl VerifiedRepairExecutor for RootNixRepairExecutor {
    fn execute(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<Vec<RepairOutcomeKind>, MaintenanceError> {
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
            if self.run(repair, REPAIR_TIMEOUT)?.code != Some(0) {
                return Err(MaintenanceError::backend_failure());
            }

            let mut verify = root_store_args();
            verify.extend(os_args(["store", "verify", "--no-trust"]));
            verify.push(OsString::from(path.as_str()));
            let verify = self.run(verify, SHORT_TIMEOUT)?;
            if verify.code == Some(0) {
                outcomes.push(RepairOutcomeKind::Restored);
                continue;
            }
            if scope.mode() != RepairMode::CacheOnly {
                return Err(MaintenanceError::backend_failure());
            }

            let mut info = root_store_args();
            info.extend(os_args(["store", "info"]));
            if self.run(info, SHORT_TIMEOUT)?.code != Some(0) {
                return Err(MaintenanceError::backend_failure());
            }
            outcomes.push(RepairOutcomeKind::CacheMiss);
        }
        Ok(outcomes)
    }
}

impl std::fmt::Debug for RealNixAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealNixAdapter")
            .finish_non_exhaustive()
    }
}

impl RealNixAdapter {
    /// Construct an adapter for an installer-authenticated absolute Nix binary and private HOME.
    ///
    /// This constructor does not create or repair either path. The privileged installer owns
    /// their provenance and permissions.
    pub fn new(nix_binary: &Path, private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(validated_process_executor(nix_binary, private_home)?),
        })
    }

    #[cfg(test)]
    fn scripted(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    fn run(
        &self,
        method: MethodKind,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, NixAdapterError> {
        self.run_with_program(method, NixProgram::Modern, args, timeout)
    }

    fn run_with_program(
        &self,
        method: MethodKind,
        program: NixProgram,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, NixAdapterError> {
        let outcome = execute_checked(self.executor.as_ref(), program, args, timeout)?;
        let _ = method;
        Ok(outcome)
    }

    fn require_success(
        &self,
        method: MethodKind,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<Vec<u8>, NixAdapterError> {
        let outcome = self.run(method, args, timeout)?;
        if outcome.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        Ok(outcome.stdout)
    }

    fn raw_path_info(
        &self,
        path: &StorePath,
        recursive: bool,
        remote: bool,
    ) -> Result<RawPathInfoEnvelope, NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args(["path-info", "--json", "--json-format", "2"]));
        if recursive {
            args.push("--recursive".into());
        }
        if remote {
            args.extend(os_args(["--store", CACHE_URL]));
        }
        args.push(path.as_str().into());
        let bytes = self.require_success(MethodKind::PathInfo, args, SHORT_TIMEOUT)?;
        parse_json(&bytes)
    }
}

impl NixAdapter for RealNixAdapter {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        let bytes =
            self.require_success(MethodKind::Version, vec!["--version".into()], SHORT_TIMEOUT)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| malformed())?.trim();
        let version = text.strip_prefix("nix (Nix) ").ok_or_else(malformed)?;
        if version != PINNED_NIX_VERSION {
            return Err(NixAdapterError::UnsupportedUpstreamFormat {
                command: MethodKind::Version,
                observed: 0,
            });
        }
        let legacy = self.run_with_program(
            MethodKind::Version,
            NixProgram::LegacyStore,
            vec!["--version".into()],
            SHORT_TIMEOUT,
        )?;
        if legacy.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        let legacy_text = std::str::from_utf8(&legacy.stdout)
            .map_err(|_| malformed())?
            .trim();
        if legacy_text != format!("nix-store (Nix) {PINNED_NIX_VERSION}") {
            return Err(NixAdapterError::UnsupportedUpstreamFormat {
                command: MethodKind::Version,
                observed: 0,
            });
        }
        Ok(VersionInfo::new(
            NixVersion::new(version)?,
            AcceptedFormats::new(FormatVersion::new(PATH_INFO_FORMAT)?),
        ))
    }

    fn evaluate_derivation(
        &self,
        request: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError> {
        let installable = pinned_installable(request);
        let mut root_args = base_args();
        root_args.extend(os_args(["derivation", "show"]));
        root_args.push(installable.clone().into());
        let root_bytes =
            self.require_success(MethodKind::EvaluateDerivation, root_args, EVALUATE_TIMEOUT)?;
        let root_name = single_derivation_name(&root_bytes)?;
        let mut args = base_args();
        args.extend(os_args(["derivation", "show", "--recursive"]));
        args.push(installable.into());
        let bytes = self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)?;
        normalize_derivation(&bytes, request, &root_name)
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        normalize_path_info(self.raw_path_info(path, true, false)?, path)
    }

    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        let mut ping = base_args();
        ping.extend(os_args(["store", "ping", "--store", CACHE_URL]));
        self.require_success(MethodKind::Substitute, ping, SHORT_TIMEOUT)
            .map_err(|_| NixAdapterError::Unavailable)?;

        let remote = match self.raw_path_info(path, false, true) {
            Ok(remote) => remote,
            Err(NixAdapterError::OperationFailed) => {
                return SubstituteReport::miss(
                    path.clone(),
                    SubstituteOutcome::AbsentFromSubstituters,
                );
            }
            Err(error) => return Err(error),
        };
        let remote_entry = root_path_info(&remote, path)?;
        let signatures = signatures(&remote_entry.signatures)?;
        if !has_approved_cache_signature(&signatures) {
            return Err(NixAdapterError::TrustFailure);
        }
        let nar_hash =
            NarHash::new(&remote_entry.nar_hash).map_err(|_| NixAdapterError::IntegrityFailure)?;

        let mut copy = base_args();
        copy.extend(os_args(["copy", "--from", CACHE_URL]));
        copy.push(path.as_str().into());
        self.require_success(MethodKind::Substitute, copy, BUILD_TIMEOUT)
            .map_err(|_| NixAdapterError::TrustFailure)?;
        let local = self.raw_path_info(path, false, false)?;
        let local_entry = root_path_info(&local, path)?;
        if local_entry.nar_hash != remote_entry.nar_hash {
            return Err(NixAdapterError::IntegrityFailure);
        }
        let receipt = SubstituteReceipt::new(CACHE_URL, nar_hash, signatures)?;
        Ok(SubstituteReport::fetched(path.clone(), receipt))
    }

    fn build(&self, request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args(["build", "--no-link", "--json"]));
        for target in request.targets() {
            args.push(target.render_private().into());
        }
        let bytes = self.require_success(MethodKind::Build, args, BUILD_TIMEOUT)?;
        let raw: Vec<RawBuildResult> = parse_json(&bytes)?;
        let expected = expected_build_outputs(request);
        let mut seen_paths = BTreeSet::new();
        let mut seen_outputs = BTreeSet::new();
        let mut outputs = Vec::new();
        for result in raw {
            validate_build_metrics(&result)?;
            let derivation = derivation_path(&result.drv_path)?;
            for (name, raw_path) in result.outputs {
                let output_name =
                    OutputName::new(&name).map_err(|_| NixAdapterError::OperationFailed)?;
                let derivation_name = derivation.as_str().to_owned();
                if !expected.contains(&(derivation_name.clone(), Some(name.clone())))
                    && !expected.contains(&(derivation_name.clone(), None))
                {
                    return Err(NixAdapterError::OperationFailed);
                }
                let path = store_path(&raw_path)?;
                if !seen_paths.insert(path.as_str().to_owned()) {
                    return Err(NixAdapterError::OperationFailed);
                }
                seen_outputs.insert((derivation_name, output_name.as_str().to_owned()));
                let info = self.raw_path_info(&path, false, false)?;
                let entry = root_path_info(&info, &path)?;
                let output_signatures = signatures(&entry.signatures)?;
                let provenance =
                    classify_build_provenance(self, &path, entry.ultimate, &output_signatures)?;
                outputs.push(BuildOutput::new(path, provenance));
            }
        }
        if !expected.iter().all(|(derivation, output)| match output {
            Some(output) => seen_outputs.contains(&(derivation.clone(), output.clone())),
            None => seen_outputs
                .iter()
                .any(|(observed, _)| observed == derivation),
        }) {
            return Err(NixAdapterError::OperationFailed);
        }
        BuildReport::new(BuildStatus::Built, outputs)
    }

    fn verify(&self, request: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        let mut overall = base_args();
        overall.extend(os_args(["store", "verify"]));
        if request.mode() == VerifyMode::Recursive {
            overall.push("--recursive".into());
        }
        overall.extend(request.paths().iter().map(|path| path.as_str().into()));
        let overall = self.run(MethodKind::Verify, overall, BUILD_TIMEOUT)?;
        if !matches!(overall.code, Some(0..=3)) {
            return Err(NixAdapterError::OperationFailed);
        }

        let mut results = Vec::with_capacity(request.paths().len());
        for path in request.paths() {
            if fs::symlink_metadata(path.as_str()).is_err() {
                results.push(PathVerifyResult::new(
                    path.clone(),
                    NarIntegrity::Missing,
                    TrustStatus::Untrusted,
                ));
                continue;
            }
            let recursive = request.mode() == VerifyMode::Recursive;
            let integrity = verify_dimension(self, path, "--no-trust", 1, recursive)?;
            let trust = verify_dimension(self, path, "--no-contents", 2, recursive)?;
            results.push(PathVerifyResult::new(
                path.clone(),
                if integrity {
                    NarIntegrity::Intact
                } else {
                    NarIntegrity::Corrupt
                },
                if trust {
                    TrustStatus::Trusted
                } else {
                    TrustStatus::Untrusted
                },
            ));
        }
        VerifyReport::new(results)
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        let preflight = self.run_with_program(
            MethodKind::Gc,
            NixProgram::LegacyStore,
            os_args(["--gc", "--print-dead"]),
            GC_TIMEOUT,
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

        let outcome = self.run_with_program(
            MethodKind::Gc,
            NixProgram::LegacyStore,
            os_args(["--gc"]),
            GC_TIMEOUT,
        )?;
        if outcome.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        let collected = parse_gc_deletions(&outcome.stderr)?;
        GcReport::new(GcStatus::Collected, collected, 0)
    }
}

trait CommandExecutor: Send + Sync {
    fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError>;
}

fn validated_process_executor(
    nix_binary: &Path,
    private_home: &Path,
) -> Result<ProcessExecutor, NixAdapterError> {
    if !nix_binary.is_absolute() || !private_home.is_absolute() {
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
    })
}

fn execute_checked(
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

#[derive(Debug)]
struct ProcessExecutor {
    nix_binary: PathBuf,
    nix_store_binary: PathBuf,
    private_home: PathBuf,
}

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError> {
        let binary = match spec.program {
            NixProgram::Modern => &self.nix_binary,
            NixProgram::LegacyStore => &self.nix_store_binary,
        };
        let mut command = Command::new(binary);
        command
            .args(&spec.args)
            .env_clear()
            .env("HOME", &self.private_home)
            .env("TMPDIR", self.private_home.join("tmp"))
            .env("NIX_CONFIG", MANAGED_NIX_CONFIG)
            .env("NIX_DAEMON_SOCKET_PATH", MANAGED_DAEMON_SOCKET)
            .env("NIX_REMOTE", "daemon")
            .env("NIX_STATE_DIR", MANAGED_NIX_STATE)
            .env("NIX_USER_CONF_FILES", "")
            .env("PATH", MANAGED_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|_| NixAdapterError::Unavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(NixAdapterError::OperationFailed)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(NixAdapterError::OperationFailed)?;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, MAX_STDOUT_BYTES));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, MAX_STDERR_BYTES));
        let started = Instant::now();
        let mut observed_status = None;
        let (status, timed_out) = loop {
            if observed_status.is_none() {
                observed_status = child
                    .try_wait()
                    .map_err(|_| NixAdapterError::OperationFailed)?;
            }
            if let Some(status) = observed_status
                && stdout_reader.is_finished()
                && stderr_reader.is_finished()
            {
                break (status, false);
            }
            if started.elapsed() >= spec.timeout {
                let status = terminate_and_reap(&mut child, observed_status)?;
                break (status, true);
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
struct CommandSpec {
    program: NixProgram,
    args: Vec<OsString>,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NixProgram {
    Modern,
    LegacyStore,
}

#[derive(Debug, Clone)]
struct CommandOutcome {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_oversized: bool,
    stderr_oversized: bool,
    timed_out: bool,
}

#[cfg(unix)]
fn terminate_and_reap(
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
fn terminate_and_reap(
    child: &mut std::process::Child,
    observed_status: Option<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus, NixAdapterError> {
    if let Some(status) = observed_status {
        return Ok(status);
    }
    child.kill().map_err(|_| NixAdapterError::OperationFailed)?;
    child.wait().map_err(|_| NixAdapterError::OperationFailed)
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
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

fn base_args() -> Vec<OsString> {
    os_args([
        "--extra-experimental-features",
        "nix-command flakes",
        "--option",
        "allow-import-from-derivation",
        "false",
    ])
}

fn root_store_args() -> Vec<OsString> {
    let mut args = base_args();
    // Nix 2.34.8's daemon protocol rejects repairPath even for root. The
    // privileged helper therefore opens only the fixed managed local store;
    // no caller-selectable store URL crosses the capability boundary.
    args.extend(os_args(["--store", "local"]));
    args
}

fn os_args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

fn pinned_installable(request: &EvaluateDerivationRequest) -> String {
    format!(
        "github:NixOS/nixpkgs/{}?narHash={}#legacyPackages.{}.{}",
        request.nixpkgs_revision().as_str(),
        percent_encode(request.nixpkgs_nar_hash().as_str()),
        request.system().as_str(),
        request.attribute().as_str()
    )
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawDerivationEnvelope {
    version: u32,
    derivations: BTreeMap<String, RawDerivation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawDerivation {
    args: Vec<String>,
    builder: String,
    env: BTreeMap<String, String>,
    inputs: RawInputs,
    name: String,
    outputs: BTreeMap<String, RawDerivationOutput>,
    structured_attrs: Option<BTreeMap<String, serde_json::Value>>,
    system: String,
    version: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawInputs {
    drvs: BTreeMap<String, RawInputDerivation>,
    srcs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum RawInputDerivation {
    Outputs(Vec<String>),
    Dynamic(RawDynamicInputDerivation),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawDynamicInputDerivation {
    dynamic_outputs: BTreeMap<String, serde_json::Value>,
    outputs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawDerivationOutput {
    path: Option<String>,
    hash: Option<String>,
    method: Option<String>,
    hash_algo: Option<String>,
    impure: Option<bool>,
}

fn normalize_derivation(
    bytes: &[u8],
    request: &EvaluateDerivationRequest,
    root_name: &str,
) -> Result<DerivationPlanReport, NixAdapterError> {
    let raw: RawDerivationEnvelope = parse_json(bytes)?;
    validate_derivation_envelope(&raw)?;
    let root_raw = raw
        .derivations
        .get(root_name)
        .ok_or(NixAdapterError::OperationFailed)?;
    let root = derivation_path(root_name)?;
    let outputs_to_install = match request.outputs().explicit_outputs() {
        Some(outputs) => outputs.to_vec(),
        None => root_raw
            .env
            .get("outputsToInstall")
            .or_else(|| root_raw.env.get("outputs"))
            .ok_or(NixAdapterError::OperationFailed)?
            .split_whitespace()
            .map(|name| OutputName::new(name).map_err(|_| NixAdapterError::OperationFailed))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mut derivations = Vec::with_capacity(raw.derivations.len());
    for (raw_path, item) in &raw.derivations {
        let system = DerivationSystem::from_str(&item.system)?;
        let outputs = item
            .outputs
            .iter()
            .map(|(name, output)| {
                let fixed_output = validate_derivation_output(output)?;
                let path = output
                    .path
                    .as_deref()
                    .or_else(|| item.env.get(name).map(String::as_str))
                    .ok_or(NixAdapterError::OperationFailed)?;
                Ok((
                    OutputName::new(name).map_err(|_| NixAdapterError::OperationFailed)?,
                    store_path(path)?,
                    fixed_output,
                ))
            })
            .collect::<Result<Vec<_>, NixAdapterError>>()?;
        let fixed_output = outputs.iter().any(|(_, _, fixed)| *fixed);
        let outputs = outputs
            .into_iter()
            .map(|(name, path, _)| (name, path))
            .collect::<BTreeMap<_, _>>();
        let document = serde_json::to_vec(item).map_err(|_| malformed())?;
        derivations.push(EvaluatedDerivation::new(
            derivation_path(raw_path)?,
            item.name.clone(),
            system,
            outputs,
            body_digest(&document),
            fixed_output,
        )?);
    }
    let closure = serde_json::to_vec(&raw.derivations).map_err(|_| malformed())?;
    let pname = root_raw
        .env
        .get("pname")
        .cloned()
        .ok_or(NixAdapterError::OperationFailed)?;
    let version = root_raw
        .env
        .get("version")
        .cloned()
        .ok_or(NixAdapterError::OperationFailed)?;
    DerivationPlanReport::new(
        raw.version,
        root,
        outputs_to_install,
        derivations,
        body_digest(&closure),
        pname,
        pkg_core::PackageVersion::new(version),
    )
}

fn validate_derivation_output(output: &RawDerivationOutput) -> Result<bool, NixAdapterError> {
    match (
        output.path.is_some(),
        output.hash.as_deref(),
        output.method.as_deref(),
        output.hash_algo.as_deref(),
        output.impure,
    ) {
        (true, None, None, None, None) => Ok(false),
        (false, Some(hash), Some(method), None, None)
            if valid_hash(hash) && valid_ca_method(method) =>
        {
            Ok(true)
        }
        (false, None, Some(method), Some(algorithm), None)
            if valid_ca_method(method) && valid_hash_algorithm(algorithm) =>
        {
            Err(NixAdapterError::OperationFailed)
        }
        (false, None, Some(method), Some(algorithm), Some(true))
            if valid_ca_method(method) && valid_hash_algorithm(algorithm) =>
        {
            Err(NixAdapterError::PermissionDenied)
        }
        (false, None, None, None, None) => Err(NixAdapterError::OperationFailed),
        _ => Err(NixAdapterError::OperationFailed),
    }
}

fn valid_ca_method(value: &str) -> bool {
    matches!(value, "flat" | "nar" | "text" | "git")
}

fn valid_hash_algorithm(value: &str) -> bool {
    matches!(value, "blake3" | "md5" | "sha1" | "sha256" | "sha512")
}

fn valid_hash(value: &str) -> bool {
    value
        .split_once('-')
        .is_some_and(|(algorithm, digest)| valid_hash_algorithm(algorithm) && !digest.is_empty())
}

fn single_derivation_name(bytes: &[u8]) -> Result<String, NixAdapterError> {
    let raw: RawDerivationEnvelope = parse_json(bytes)?;
    validate_derivation_envelope(&raw)?;
    if raw.derivations.len() != 1 {
        return Err(NixAdapterError::OperationFailed);
    }
    raw.derivations
        .into_keys()
        .next()
        .ok_or(NixAdapterError::OperationFailed)
}

fn validate_derivation_envelope(raw: &RawDerivationEnvelope) -> Result<(), NixAdapterError> {
    if raw.version != 4 || raw.derivations.values().any(|item| item.version != 4) {
        return Err(NixAdapterError::UnsupportedUpstreamFormat {
            command: MethodKind::EvaluateDerivation,
            observed: raw.version,
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawPathInfoEnvelope {
    version: u32,
    store_dir: String,
    info: BTreeMap<String, RawPathInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawPathInfo {
    ca: Option<RawContentAddress>,
    compression: Option<String>,
    deriver: Option<String>,
    download_hash: Option<String>,
    download_size: Option<u64>,
    nar_hash: String,
    nar_size: u64,
    references: Vec<String>,
    #[serde(rename = "registrationTime")]
    _registration_time: Option<u64>,
    signatures: Vec<String>,
    store_dir: String,
    ultimate: bool,
    url: Option<String>,
    version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContentAddress {
    hash: String,
    method: String,
}

fn normalize_path_info(
    raw: RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<PathInfoReport, NixAdapterError> {
    validate_path_info_envelope(&raw)?;
    let root = root_path_info(&raw, requested)?;
    let closure_size = raw.info.values().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.nar_size)
            .ok_or(NixAdapterError::OperationFailed)
    })?;
    let references = root
        .references
        .iter()
        .map(|reference| store_path(reference))
        .filter(|result| result.as_ref() != Ok(requested))
        .collect::<Result<Vec<_>, _>>()?;
    let deriver = root.deriver.as_deref().map(derivation_path).transpose()?;
    PathInfoReport::new(
        requested.clone(),
        NarHash::new(&root.nar_hash).map_err(|_| NixAdapterError::OperationFailed)?,
        signatures(&root.signatures)?,
        references,
        deriver,
        root.nar_size,
        closure_size,
    )
}

fn validate_path_info_envelope(raw: &RawPathInfoEnvelope) -> Result<(), NixAdapterError> {
    if raw.version != PATH_INFO_FORMAT
        || raw.store_dir != STORE_DIRECTORY
        || raw.info.values().any(|item| {
            let remote_fields = [
                item.compression.is_some(),
                item.download_hash.is_some(),
                item.download_size.is_some(),
                item.url.is_some(),
            ];
            item.version != PATH_INFO_FORMAT
                || item.store_dir != STORE_DIRECTORY
                || item.ca.as_ref().is_some_and(|ca| {
                    NarHash::new(&ca.hash).is_err() || !valid_ca_method(&ca.method)
                })
                || remote_fields
                    .iter()
                    .any(|present| *present != remote_fields[0])
        })
    {
        return Err(NixAdapterError::UnsupportedUpstreamFormat {
            command: MethodKind::PathInfo,
            observed: raw.version,
        });
    }
    Ok(())
}

fn root_path_info<'a>(
    raw: &'a RawPathInfoEnvelope,
    requested: &StorePath,
) -> Result<&'a RawPathInfo, NixAdapterError> {
    validate_path_info_envelope(raw)?;
    let name = requested
        .as_str()
        .strip_prefix("/nix/store/")
        .ok_or(NixAdapterError::OperationFailed)?;
    raw.info.get(name).ok_or(NixAdapterError::OperationFailed)
}

fn signatures(values: &[String]) -> Result<Vec<Signature>, NixAdapterError> {
    values
        .iter()
        .map(|value| Signature::new(value).map_err(|_| NixAdapterError::TrustFailure))
        .collect()
}

fn has_approved_cache_signature(signatures: &[Signature]) -> bool {
    signatures
        .iter()
        .any(|signature| signature.key_name() == CACHE_SIGNING_KEY_NAME)
}

fn classify_build_provenance(
    adapter: &RealNixAdapter,
    path: &StorePath,
    ultimate: bool,
    signatures: &[Signature],
) -> Result<BuildOutputProvenance, NixAdapterError> {
    if ultimate {
        return Ok(BuildOutputProvenance::LocalBuild);
    }
    if has_approved_cache_signature(signatures)
        && verify_dimension(adapter, path, "--no-contents", 2, false)?
    {
        return Ok(BuildOutputProvenance::CacheSigned);
    }
    Err(NixAdapterError::TrustFailure)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawBuildResult {
    drv_path: String,
    outputs: BTreeMap<String, String>,
    #[serde(default)]
    start_time: Option<u64>,
    #[serde(default)]
    stop_time: Option<u64>,
    #[serde(default)]
    cpu_user: Option<f64>,
    #[serde(default)]
    cpu_system: Option<f64>,
}

fn validate_build_metrics(result: &RawBuildResult) -> Result<(), NixAdapterError> {
    let _ = (result.start_time, result.stop_time);
    if [result.cpu_user, result.cpu_system]
        .into_iter()
        .flatten()
        .any(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(NixAdapterError::OperationFailed);
    }
    Ok(())
}

fn expected_build_outputs(request: &BuildRequest) -> BTreeSet<(String, Option<String>)> {
    request
        .targets()
        .iter()
        .flat_map(|target| match target.outputs() {
            Some(outputs) => outputs
                .iter()
                .map(|output| {
                    (
                        target.derivation().as_str().to_owned(),
                        Some(output.as_str().to_owned()),
                    )
                })
                .collect::<Vec<_>>(),
            None => vec![(target.derivation().as_str().to_owned(), None)],
        })
        .collect()
}

fn verify_dimension(
    adapter: &RealNixAdapter,
    path: &StorePath,
    fixed_flag: &'static str,
    failure_code: i32,
    recursive: bool,
) -> Result<bool, NixAdapterError> {
    let mut args = base_args();
    args.extend(os_args(["store", "verify", fixed_flag]));
    if recursive {
        args.push("--recursive".into());
    }
    args.push(path.as_str().into());
    let outcome = adapter.run(MethodKind::Verify, args, BUILD_TIMEOUT)?;
    match outcome.code {
        Some(0) => Ok(true),
        Some(code) if code == failure_code => Ok(false),
        _ => Err(NixAdapterError::OperationFailed),
    }
}

fn parse_gc_candidates(stdout: &[u8]) -> Result<Vec<StorePath>, NixAdapterError> {
    let text = std::str::from_utf8(stdout).map_err(|_| malformed())?;
    text.lines()
        .map(normalize_gc_store_entry)
        .filter_map(Result::transpose)
        .collect()
}

fn parse_gc_deletions(stderr: &[u8]) -> Result<Vec<StorePath>, NixAdapterError> {
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

fn normalize_gc_store_entry(value: &str) -> Result<Option<StorePath>, NixAdapterError> {
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

fn store_path(value: &str) -> Result<StorePath, NixAdapterError> {
    let absolute = if value.starts_with('/') {
        value.to_owned()
    } else {
        format!("{STORE_DIRECTORY}/{value}")
    };
    StorePath::new(&absolute).map_err(|_| NixAdapterError::OperationFailed)
}

fn derivation_path(value: &str) -> Result<DerivationPath, NixAdapterError> {
    DerivationPath::from_str(store_path(value)?.as_str())
        .map_err(|_| NixAdapterError::OperationFailed)
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, NixAdapterError> {
    serde_json::from_slice(bytes).map_err(|_| malformed())
}

const fn malformed() -> NixAdapterError {
    NixAdapterError::MalformedPayload {
        kind: crate::MalformedKind::Json,
    }
}

#[cfg(unix)]
fn is_private(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o077 == 0
}

#[cfg(not(unix))]
const fn is_private(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pkg_core::{
        NixpkgsRevision, OutputSelection, PolicyVersion, System, identity::NarHash,
        selector::AttributePath,
    };

    use super::*;

    #[derive(Debug)]
    struct Scripted {
        calls: Arc<Mutex<Vec<Vec<OsString>>>>,
        outcomes: Mutex<Vec<CommandOutcome>>,
    }

    impl Scripted {
        fn new(outcomes: Vec<CommandOutcome>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outcomes: Mutex::new(outcomes.into_iter().rev().collect()),
            }
        }
    }

    impl CommandExecutor for Scripted {
        fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError> {
            self.calls
                .lock()
                .map_err(|_| NixAdapterError::OperationFailed)?
                .push(spec.args);
            self.outcomes
                .lock()
                .map_err(|_| NixAdapterError::OperationFailed)?
                .pop()
                .ok_or(NixAdapterError::UnexpectedExtraCall {
                    actual: MethodKind::Version,
                    summary: crate::error::BoundedSummary::new("extra call"),
                })
        }
    }

    fn success(stdout: impl Into<Vec<u8>>) -> CommandOutcome {
        CommandOutcome {
            code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
            stdout_oversized: false,
            stderr_oversized: false,
            timed_out: false,
        }
    }

    fn success_with_stderr(stderr: impl Into<Vec<u8>>) -> CommandOutcome {
        let mut outcome = success(Vec::new());
        outcome.stderr = stderr.into();
        outcome
    }

    fn failure(code: i32) -> CommandOutcome {
        let mut outcome = success(Vec::new());
        outcome.code = Some(code);
        outcome
    }

    fn repair_scope(mode: RepairMode) -> Result<VerifiedRepairScope, Box<dyn std::error::Error>> {
        Ok(VerifiedRepairScope::new(
            1001,
            crate::GenerationId::new("gen-0007")?,
            [StorePath::new(
                "/nix/store/22222222222222222222222222222222-demo",
            )?],
            (mode == RepairMode::Build).then(|| body_digest(b"approved repair plan")),
            PolicyVersion::from_u64(1).ok_or("policy version")?,
            mode,
        )?)
    }

    #[test]
    fn root_repair_cache_miss_requires_successful_repair_and_live_local_store()
    -> Result<(), Box<dyn std::error::Error>> {
        let scripted = Scripted::new(vec![success(Vec::new()), failure(1), success(Vec::new())]);
        let calls = Arc::clone(&scripted.calls);
        let executor = RootNixRepairExecutor::scripted(scripted);
        let outcomes = executor.execute(&repair_scope(RepairMode::CacheOnly)?)?;
        assert_eq!(outcomes, vec![RepairOutcomeKind::CacheMiss]);
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|call| {
            call.windows(2)
                .any(|arguments| arguments == [OsString::from("--store"), OsString::from("local")])
        }));
        assert!(calls[0].windows(3).any(|arguments| {
            arguments
                == [
                    OsString::from("--option"),
                    OsString::from("max-jobs"),
                    OsString::from("0"),
                ]
        }));
        assert!(calls[0].windows(3).any(|arguments| {
            arguments
                == [
                    OsString::from("--option"),
                    OsString::from("builders"),
                    OsString::new(),
                ]
        }));
        assert_eq!(
            &calls[0][calls[0].len() - 3..],
            [
                OsString::from("store"),
                OsString::from("repair"),
                OsString::from("/nix/store/22222222222222222222222222222222-demo"),
            ]
        );
        assert_eq!(
            &calls[1][calls[1].len() - 4..],
            [
                OsString::from("store"),
                OsString::from("verify"),
                OsString::from("--no-trust"),
                OsString::from("/nix/store/22222222222222222222222222222222-demo"),
            ]
        );
        assert_eq!(
            &calls[2][calls[2].len() - 2..],
            [OsString::from("store"), OsString::from("info")]
        );
        Ok(())
    }

    #[test]
    fn root_repair_build_is_bounded_and_must_verify_clean() -> Result<(), Box<dyn std::error::Error>>
    {
        let scripted = Scripted::new(vec![success(Vec::new()), success(Vec::new())]);
        let calls = Arc::clone(&scripted.calls);
        let executor = RootNixRepairExecutor::scripted(scripted);
        assert_eq!(
            executor.execute(&repair_scope(RepairMode::Build)?)?,
            vec![RepairOutcomeKind::Restored]
        );
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 2);
        assert!(calls[0].windows(3).any(|arguments| {
            arguments
                == [
                    OsString::from("--option"),
                    OsString::from("max-jobs"),
                    OsString::from("1"),
                ]
        }));

        let executor =
            RootNixRepairExecutor::scripted(Scripted::new(vec![success(Vec::new()), failure(1)]));
        assert_eq!(
            executor
                .execute(&repair_scope(RepairMode::Build)?)
                .unwrap_err()
                .code(),
            crate::MaintenanceErrorCode::BackendFailure
        );
        Ok(())
    }

    #[test]
    fn root_repair_command_failure_is_not_downgraded_to_cache_miss()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = RootNixRepairExecutor::scripted(Scripted::new(vec![failure(1)]));
        assert_eq!(
            executor
                .execute(&repair_scope(RepairMode::CacheOnly)?)
                .unwrap_err()
                .code(),
            crate::MaintenanceErrorCode::BackendFailure
        );
        Ok(())
    }

    #[test]
    fn version_is_exact_and_environment_runner_is_not_bypassed()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = RealNixAdapter::scripted(Scripted::new(vec![
            success("nix (Nix) 2.34.8\n"),
            success("nix-store (Nix) 2.34.8\n"),
        ]));
        let version = adapter.version()?;
        assert_eq!(version.nix_version().as_str(), PINNED_NIX_VERSION);
        assert_eq!(version.accepted_formats().path_info().get(), 2);
        Ok(())
    }

    #[test]
    fn derivation_v4_normalizes_relative_paths_and_closed_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = br#"{"version":4,"derivations":{"00000000000000000000000000000000-demo.drv":{"args":[],"builder":"/nix/store/11111111111111111111111111111111-bash","env":{"outputs":"out","out":"/nix/store/22222222222222222222222222222222-demo","pname":"demo","version":"1.0"},"inputs":{"drvs":{"33333333333333333333333333333333-dep.drv":["out"]},"srcs":[]},"name":"demo-1.0","outputs":{"out":{"path":"22222222222222222222222222222222-demo"}},"structuredAttrs":{"__structuredAttrs":true},"system":"aarch64-linux","version":4}}}"#;
        let executor = Scripted::new(vec![success(raw.as_slice()), success(raw.as_slice())]);
        let calls = Arc::clone(&executor.calls);
        let adapter = RealNixAdapter::scripted(executor);
        let request = EvaluateDerivationRequest::new(
            AttributePath::new("demo")?,
            System::Aarch64Linux,
            NixpkgsRevision::new("a62e6edd6d5e1fa0329b8653c801147986f8d446")?,
            NarHash::new("sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=")?,
            OutputSelection::default_selection(),
        )?;
        let report = adapter.evaluate_derivation(&request)?;
        assert_eq!(report.json_version(), 4);
        assert_eq!(report.pname(), "demo");
        assert_eq!(report.outputs_to_install()[0].as_str(), "out");
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 2);
        for call in calls.iter() {
            assert!(call.windows(3).any(|arguments| {
                arguments
                    == [
                        OsString::from("--option"),
                        OsString::from("allow-import-from-derivation"),
                        OsString::from("false"),
                    ]
            }));
        }
        Ok(())
    }

    #[test]
    fn path_info_v2_filters_upstream_self_reference_and_sums_closure()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = br#"{"info":{"22222222222222222222222222222222-demo":{"ca":null,"deriver":"00000000000000000000000000000000-demo.drv","narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["22222222222222222222222222222222-demo","33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"version":2},"33333333333333333333333333333333-dep":{"ca":{"hash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","method":"nar"},"deriver":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":["33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"version":2}},"storeDir":"/nix/store","version":2}"#;
        let adapter = RealNixAdapter::scripted(Scripted::new(vec![success(raw.as_slice())]));
        let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
        let report = adapter.path_info(&path)?;
        assert_eq!(report.nar_size(), 5);
        assert_eq!(report.closure_size(), 12);
        assert_eq!(report.references().len(), 1);
        Ok(())
    }

    #[test]
    fn fixed_args_never_accept_caller_trust_or_store_controls()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = Scripted::new(vec![
            success("nix (Nix) 2.34.8\n"),
            success("nix-store (Nix) 2.34.8\n"),
        ]);
        let calls = Arc::clone(&executor.calls);
        let adapter = RealNixAdapter::scripted(executor);
        let _ = adapter.version()?;
        let forbidden = [
            "--substituters",
            "--trusted-public-keys",
            "--builders",
            "--store",
        ];
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 2);
        for call in calls.iter() {
            for value in forbidden {
                assert!(!call.iter().any(|argument| argument == value));
            }
        }
        let _ = PolicyVersion::from_u64(1).ok_or("policy")?;
        Ok(())
    }

    #[test]
    fn gc_preflights_dead_paths_then_reports_only_actual_deletions()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = "/nix/store/22222222222222222222222222222222-first";
        let second = "/nix/store/33333333333333333333333333333333-second";
        let executor = Scripted::new(vec![
            success(format!("{first}\n/nix/store/trash\n{second}\n")),
            success_with_stderr(format!(
                "finding garbage collector roots...\ndeleting garbage...\ndeleting '/nix/store/trash'\ndeleting '{second}'\n"
            )),
        ]);
        let calls = Arc::clone(&executor.calls);
        let adapter = RealNixAdapter::scripted(executor);

        let report = adapter.gc()?;

        assert_eq!(report.collected(), &[StorePath::new(second)?]);
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls[0], os_args(["--gc", "--print-dead"]));
        assert_eq!(calls[1], os_args(["--gc"]));
        Ok(())
    }

    #[test]
    fn build_json_accepts_pinned_optional_timing_metrics() -> Result<(), NixAdapterError> {
        let raw = br#"[{"drvPath":"/nix/store/00000000000000000000000000000000-demo.drv","outputs":{"out":"/nix/store/22222222222222222222222222222222-demo"},"startTime":30,"stopTime":50,"cpuUser":1.25,"cpuSystem":0.5}]"#;
        let results: Vec<RawBuildResult> = parse_json(raw)?;

        assert_eq!(results.len(), 1);
        validate_build_metrics(&results[0])?;
        assert_eq!(results[0].start_time, Some(30));
        assert_eq!(results[0].stop_time, Some(50));
        assert_eq!(results[0].cpu_user, Some(1.25));
        assert_eq!(results[0].cpu_system, Some(0.5));
        Ok(())
    }

    #[test]
    fn build_provenance_requires_ultimate_or_cryptographic_trust()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
        let local = RealNixAdapter::scripted(Scripted::new(Vec::new()));
        assert_eq!(
            classify_build_provenance(&local, &path, true, &[])?,
            BuildOutputProvenance::LocalBuild
        );
        assert!(matches!(
            classify_build_provenance(&local, &path, false, &[]),
            Err(NixAdapterError::TrustFailure)
        ));

        let executor = Scripted::new(vec![success(Vec::new())]);
        let calls = Arc::clone(&executor.calls);
        let cached = RealNixAdapter::scripted(executor);
        let signature = Signature::new("cache.nixos.org-1:AAAA")?;
        assert_eq!(
            classify_build_provenance(&cached, &path, false, &[signature])?,
            BuildOutputProvenance::CacheSigned
        );
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].iter().any(|argument| argument == "--no-contents"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_descendants_before_joining_capture_threads()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        fs::create_dir(home.path().join("tmp"))?;
        let executor = ProcessExecutor {
            nix_binary: PathBuf::from("/bin/sh"),
            nix_store_binary: PathBuf::from("/bin/sh"),
            private_home: home.path().to_path_buf(),
        };
        for script in ["sleep 30 & wait", "sleep 30 &"] {
            let started = Instant::now();
            let outcome = executor.execute(CommandSpec {
                program: NixProgram::Modern,
                args: os_args(["-c", script]),
                timeout: Duration::from_millis(100),
            })?;

            assert!(outcome.timed_out);
            assert!(started.elapsed() < Duration::from_secs(5));
        }
        Ok(())
    }

    #[test]
    fn recursive_verify_dimension_cannot_drop_closure_semantics()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = Scripted::new(vec![success(Vec::new())]);
        let calls = Arc::clone(&executor.calls);
        let adapter = RealNixAdapter::scripted(executor);
        let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
        assert!(verify_dimension(&adapter, &path, "--no-trust", 1, true)?);
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].iter().any(|argument| argument == "--recursive"));
        Ok(())
    }
}
