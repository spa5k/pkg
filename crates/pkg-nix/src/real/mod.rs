//! Real, version-pinned adapter around the product-managed Nix executable.

mod process;
mod root;
mod substitute;
#[cfg(test)]
mod tests;
use process::*;
use root::*;
pub use root::{RootNixGcExecutor, RootNixRepairExecutor};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use substitute::*;

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
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
    AcceptedFormats, BuildCacheError, BuildCacheErrorCode, BuildCacheProbe, BuildOutput,
    BuildOutputProvenance, BuildProgressEstimate, BuildReadiness, BuildReport, BuildRequest,
    BuildStatus, CacheDownloadClosure, CachePathObservation, DerivationPath, DerivationPlanReport,
    DerivationSystem, EvaluateDerivationRequest, EvaluatedDerivation, FormatVersion, GcReport,
    GcStatus, MaintenanceError, MethodKind, NarHash, NarIntegrity, NixAdapter, NixAdapterError,
    NixVersion, NixpkgsMetadataRunner, NixpkgsPin, NixpkgsSourceError, OutputName, PathInfoReport,
    PathVerifyResult, PinnedNixpkgsSource, RepairBuildPlan, RepairMode, RepairOutcomeKind,
    RepairPlanDerivation, RepairPlanTarget, RootNixOperation, RootRepairPlanProof,
    RootRepairPlanRequest, Signature, StorePath, SubstituteOutcome, SubstituteReceipt,
    SubstituteReport, System, TrustStatus, VerifiedRepairExecutor, VerifiedRepairScope, VerifyMode,
    VerifyReport, VerifyRequest, VersionInfo,
};

/// Exact managed Nix version embedded in the V1 runtime contract.
pub const PINNED_NIX_VERSION: &str = "2.34.8";
pub(super) const STORE_DIRECTORY: &str = "/nix/store";
pub(super) const MANAGED_NIX_CONFIG: &str = "include /opt/pkg/etc/pkg/nix.conf";
pub(super) const MANAGED_NIX_STATE: &str = "/nix/var/nix";
pub(super) const MANAGED_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";
pub(super) const MANAGED_PATH: &str = "/usr/bin:/bin";
pub(super) const STANDARD_DETERMINATE_NIX_BINARY: &str = "/nix/var/nix/profiles/default/bin/nix";
pub(super) const STANDARD_DETERMINATE_NIX_BRAND: &str = "Determinate Nix 3.22.1";
/// Exact Nix version supplied by the supported standard Determinate install.
pub const STANDARD_DETERMINATE_NIX_VERSION: &str = "2.35.2";
pub(super) const MAX_UNINSTALL_ROOTS: usize = 4_096;
pub(super) const MAX_UNINSTALL_ROOT_BYTES: usize = 1024 * 1024;
pub(super) const INDEX_META_EXPR: &str = include_str!("../../../pkg-index/nix/index-meta.nix");
pub(super) const ACT_BUILDS: u64 = 104;
pub(super) const RESULT_PROGRESS: u64 = 105;
pub(super) const SHORT_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const EVALUATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(super) const BUILD_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
pub(super) const GC_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Maximum wall time for one complete privileged repair request.
///
/// The broker helper client waits slightly longer than this bound. This keeps
/// the broker's admission and GC-inhibit leases live until the fixed root
/// executor has either completed or killed its child process group.
pub const MAX_REPAIR_EXECUTION_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

/// Real, version-pinned adapter around the product-managed Nix executable.
#[derive(Clone)]
pub struct RealNixAdapter {
    pub(super) executor: Arc<dyn CommandExecutor>,
    pub(super) expected_nix_brand: &'static str,
    pub(super) expected_nix_version: &'static str,
    pub(super) eager_source_metadata: bool,
    pub(super) operation_deadline: Option<Instant>,
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
            executor: Arc::new(validated_process_executor(
                nix_binary,
                private_home,
                Some(Path::new(MANAGED_DAEMON_SOCKET)),
            )?),
            expected_nix_brand: "Nix",
            expected_nix_version: PINNED_NIX_VERSION,
            eager_source_metadata: false,
            operation_deadline: None,
        })
    }

    /// Constructs an adapter for the fixed standard Determinate Nix profile.
    ///
    /// This mode uses the vendor configuration unchanged. It accepts no
    /// caller-selected executable, Nix configuration, daemon socket, state
    /// directory, or remote.
    pub fn new_standard_determinate(private_home: &Path) -> Result<Self, NixAdapterError> {
        Ok(Self {
            executor: Arc::new(standard_determinate_process_executor(private_home)?),
            expected_nix_brand: STANDARD_DETERMINATE_NIX_BRAND,
            expected_nix_version: STANDARD_DETERMINATE_NIX_VERSION,
            eager_source_metadata: true,
            operation_deadline: None,
        })
    }

    /// Returns a clone constrained by one absolute root-helper operation budget.
    pub fn for_root_operation(
        &self,
        operation: RootNixOperation,
        operation_deadline: Instant,
    ) -> Result<Self, NixAdapterError> {
        let maximum_deadline = Instant::now()
            .checked_add(operation.server_budget())
            .ok_or(NixAdapterError::Timeout)?;
        if operation_deadline > maximum_deadline || operation_deadline <= Instant::now() {
            return Err(NixAdapterError::Timeout);
        }
        Ok(Self {
            executor: Arc::clone(&self.executor),
            expected_nix_brand: self.expected_nix_brand,
            eager_source_metadata: self.eager_source_metadata,
            expected_nix_version: self.expected_nix_version,
            operation_deadline: Some(operation_deadline),
        })
    }

    /// Runs one root-helper build and stops its child process group when the
    /// authenticated client disconnects.
    pub fn build_with_progress_cancelled(
        &self,
        request: &BuildRequest,
        cancelled: &AtomicBool,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, NixAdapterError> {
        self.run_build_with_progress(request, &|| cancelled.load(Ordering::Acquire), progress)
    }

    /// Performs the fixed bounded managed-daemon store readiness check.
    ///
    /// This accepts no caller-selected store, command, option, or environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted adapter error when the managed daemon does not answer.
    pub fn ping_managed_store(&self) -> Result<(), NixAdapterError> {
        self.ping_managed_store_with_timeout(MANAGED_STORE_PING_TIMEOUT)
    }

    pub(super) fn ping_managed_store_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<(), NixAdapterError> {
        self.require_success(
            MethodKind::Version,
            vec![
                OsString::from("store"),
                OsString::from("ping"),
                OsString::from("--store"),
                OsString::from("daemon"),
            ],
            timeout,
        )
        .map(|_| ())
    }

    /// Waits for the fixed managed-daemon store to become ready.
    ///
    /// Only transient daemon-start errors are retried. All other errors fail
    /// closed without delay.
    ///
    /// # Errors
    ///
    /// Returns the first terminal adapter error, or a timeout when the fixed
    /// readiness window expires.
    pub fn wait_for_managed_store(&self) -> Result<(), NixAdapterError> {
        wait_for_managed_store_with(
            |timeout| self.ping_managed_store_with_timeout(timeout),
            Instant::now,
            thread::sleep,
            MANAGED_STORE_READY_WINDOW,
            MANAGED_STORE_RETRY_INTERVAL,
            MANAGED_STORE_PING_TIMEOUT,
        )
    }

    /// Projects the fixed native package index from an exact verified source.
    ///
    /// This accepts no caller-selected expression, installable, option, store,
    /// registry, network mode, or environment.
    pub fn project_nixpkgs_index(
        &self,
        source: &PinnedNixpkgsSource,
        system: System,
    ) -> Result<Vec<u8>, NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args(["--offline", "eval", "--json", "--apply"]));
        args.push(INDEX_META_EXPR.into());
        args.push(
            format!(
                "{}#legacyPackages.{}",
                source.private_store_path().as_str(),
                system.as_str()
            )
            .into(),
        );
        self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)
    }

    /// Resolves the exact recursive closure of authenticated generation roots.
    ///
    /// This is a fixed read-only managed-daemon query. It accepts only typed
    /// store paths and exposes no store, command, option, or trust control.
    ///
    /// # Errors
    ///
    /// Returns a closed adapter error for an empty or excessive scope, a
    /// missing root, malformed output, or a managed-daemon failure.
    pub fn closure_for_roots(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<StorePath>, NixAdapterError> {
        if roots.is_empty() || roots.len() > MAX_REPAIR_CLOSURE {
            return Err(NixAdapterError::OperationFailed);
        }
        let mut closure = BTreeMap::new();
        for root in roots {
            let raw = self.raw_path_info(root, true, false)?;
            validate_path_info_envelope(&raw)?;
            root_path_info(&raw, root)?;
            for (path, info) in raw.info {
                if info.is_none() {
                    return Err(malformed());
                }
                let path = store_path(&path)?;
                closure.insert(path.as_str().to_owned(), path);
                if closure.len() > MAX_REPAIR_CLOSURE {
                    return Err(NixAdapterError::OperationFailed);
                }
            }
        }
        if closure.is_empty() {
            return Err(NixAdapterError::OperationFailed);
        }
        Ok(closure.into_values().collect())
    }

    /// Builds a private full-output local-repair approval subject.
    ///
    /// Every input is a broker-derived damaged store path. The method accepts
    /// no installable, expression, option, store selector, or output selection.
    /// It requires a valid local deriver and includes every declared output of
    /// that deriver in the canonical plan.
    pub fn repair_build_plan(
        &self,
        damaged: &[StorePath],
        policy_version: pkg_core::PolicyVersion,
        system: pkg_core::System,
        readiness: BuildReadiness,
        host_cores: u32,
    ) -> Result<RepairBuildPlan, NixAdapterError> {
        if damaged.is_empty() || damaged.len() > 4096 {
            return Err(NixAdapterError::OperationFailed);
        }
        let mut ordered = damaged.to_vec();
        ordered.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if ordered.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(NixAdapterError::OperationFailed);
        }
        let mut targets = Vec::with_capacity(ordered.len());
        for path in ordered {
            targets.push(self.repair_plan_target(path, system)?);
        }
        let version = <Self as NixAdapter>::version(self)?.nix_version().clone();
        RepairBuildPlan::new(
            &version,
            policy_version,
            system,
            readiness,
            host_cores,
            targets,
        )
        .map_err(|_| NixAdapterError::OperationFailed)
    }

    /// Produces the only repair-planning data allowed to cross the helper wire.
    pub fn repair_plan_proof(
        &self,
        request: &RootRepairPlanRequest,
    ) -> Result<RootRepairPlanProof, NixAdapterError> {
        let plan = self.repair_build_plan(
            request.damaged(),
            request.policy_version(),
            request.system(),
            request.readiness().clone(),
            request.host_cores(),
        )?;
        let digest = plan
            .digest()
            .map_err(|_| NixAdapterError::OperationFailed)?;
        let preview = plan
            .preview()
            .map_err(|_| NixAdapterError::OperationFailed)?;
        let proof = RootRepairPlanProof::new(preview).ok_or(NixAdapterError::OperationFailed)?;
        if proof.digest() != digest || !request.accepts(&proof) {
            return Err(NixAdapterError::OperationFailed);
        }
        Ok(proof)
    }

    pub(super) fn repair_plan_target(
        &self,
        path: StorePath,
        system: pkg_core::System,
    ) -> Result<RepairPlanTarget, NixAdapterError> {
        let info = self.raw_path_info(&path, false, false)?;
        validate_path_info_envelope(&info)?;
        let deriver = root_path_info(&info, &path)?
            .deriver
            .as_deref()
            .map(derivation_path)
            .transpose()?
            .ok_or(NixAdapterError::OperationFailed)?;
        let deriver_store = store_path(deriver.as_str())?;
        let deriver_info = self.raw_path_info(&deriver_store, false, false)?;
        validate_path_info_envelope(&deriver_info)?;
        root_path_info(&deriver_info, &deriver_store)?;

        let mut args = base_args();
        args.extend(os_args(["derivation", "show", "--recursive"]));
        args.push(deriver.as_str().into());
        let bytes = self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)?;
        let raw: RawDerivationEnvelope = parse_json(&bytes)?;
        validate_derivation_envelope(&raw)?;
        let item = raw
            .derivations
            .get(deriver.as_str())
            .ok_or(NixAdapterError::OperationFailed)?;
        let observed_system = DerivationSystem::from_str(&item.system)?;
        if !observed_system.is_compatible_with(system) {
            return Err(NixAdapterError::OperationFailed);
        }
        let outputs = item
            .outputs
            .iter()
            .map(|(name, output)| {
                let output_path = output
                    .path
                    .as_deref()
                    .or_else(|| item.env.get(name).map(String::as_str))
                    .ok_or(NixAdapterError::OperationFailed)?;
                Ok((
                    OutputName::new(name).map_err(|_| NixAdapterError::OperationFailed)?,
                    store_path(output_path)?,
                    validate_derivation_output(output)?,
                ))
            })
            .collect::<Result<Vec<_>, NixAdapterError>>()?;
        let fixed_output = outputs.iter().any(|(_, _, fixed)| *fixed);
        let outputs = outputs
            .into_iter()
            .map(|(name, output_path, _)| (name, output_path))
            .collect();
        let document = serde_json::to_vec(item).map_err(|_| malformed())?;
        let derivation = RepairPlanDerivation::new(
            deriver,
            item.name.clone(),
            system,
            outputs,
            body_digest(&document),
            fixed_output,
        )
        .map_err(|_| NixAdapterError::OperationFailed)?;
        Ok(RepairPlanTarget::new(path, derivation))
    }

    #[cfg(test)]
    pub(super) fn scripted(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
            expected_nix_brand: "Nix",
            expected_nix_version: PINNED_NIX_VERSION,
            eager_source_metadata: false,
            operation_deadline: None,
        }
    }

    #[cfg(test)]
    pub(super) fn scripted_standard_determinate(executor: impl CommandExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
            expected_nix_brand: STANDARD_DETERMINATE_NIX_BRAND,
            expected_nix_version: STANDARD_DETERMINATE_NIX_VERSION,
            eager_source_metadata: true,
            operation_deadline: None,
        }
    }

    pub(super) fn run(
        &self,
        method: MethodKind,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, NixAdapterError> {
        self.run_with_program(method, NixProgram::Modern, args, timeout)
    }

    pub(super) fn run_with_program(
        &self,
        method: MethodKind,
        program: NixProgram,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> Result<CommandOutcome, NixAdapterError> {
        let timeout = bounded_timeout(self.operation_deadline, timeout)?;
        let outcome = execute_checked(self.executor.as_ref(), program, args, timeout)?;
        let _ = method;
        Ok(outcome)
    }

    pub(super) fn require_success(
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

    pub(super) fn copy_cache_signatures(
        &self,
        paths: &[&StorePath],
    ) -> Result<(), NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args([
            "store",
            "copy-sigs",
            "--substituter",
            CACHE_URL,
            "--recursive",
        ]));
        args.extend(paths.iter().map(|path| OsString::from(path.as_str())));
        self.require_success(MethodKind::Substitute, args, BUILD_TIMEOUT)
            .map(|_| ())
            .map_err(|_| NixAdapterError::TrustFailure)
    }

    pub(super) fn raw_path_info(
        &self,
        path: &StorePath,
        recursive: bool,
        remote: bool,
    ) -> Result<RawPathInfoEnvelope, NixAdapterError> {
        self.raw_path_infos(&[path], recursive, remote)
    }

    pub(super) fn raw_path_infos(
        &self,
        paths: &[&StorePath],
        recursive: bool,
        remote: bool,
    ) -> Result<RawPathInfoEnvelope, NixAdapterError> {
        if paths.is_empty() {
            return Err(NixAdapterError::OperationFailed);
        }
        let mut args = base_args();
        args.extend(os_args(["path-info", "--json", "--json-format", "2"]));
        if recursive {
            args.push("--recursive".into());
        }
        if remote {
            args.extend(os_args(["--store", CACHE_URL]));
        }
        args.extend(paths.iter().map(|path| OsString::from(path.as_str())));
        let bytes = self.require_success(MethodKind::PathInfo, args, SHORT_TIMEOUT)?;
        parse_json(&bytes)
    }

    pub(super) fn raw_remote_path_info_with_retry(
        &self,
        path: &StorePath,
    ) -> Result<RawPathInfoEnvelope, NixAdapterError> {
        match self.raw_path_info(path, false, true) {
            Ok(exact) => Ok(exact),
            Err(NixAdapterError::OperationFailed) => self.raw_path_info(path, false, true),
            Err(error) => Err(error),
        }
    }

    pub(super) fn verify_remote_cache_trust_batch(
        &self,
        paths: &[&StorePath],
    ) -> Result<(), BuildCacheError> {
        if paths.is_empty() {
            return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
        }
        let mut args = base_args();
        args.extend(os_args([
            "store",
            "verify",
            "--store",
            CACHE_URL,
            "--no-contents",
            "--sigs-needed",
            "1",
        ]));
        args.extend(paths.iter().map(|path| OsString::from(path.as_str())));
        self.require_success(MethodKind::Verify, args, SHORT_TIMEOUT)
            .map(|_| ())
            .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))
    }

    pub(super) fn run_build_with_progress(
        &self,
        request: &BuildRequest,
        cancelled: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, NixAdapterError> {
        let mut args = base_args();
        args.extend(os_args([
            "--log-format",
            "internal-json",
            "build",
            "--no-link",
            "--json",
        ]));
        for target in request.targets() {
            args.push(target.render_private().into());
        }
        let mut parser = InternalBuildProgressParser::default();
        let timeout = bounded_timeout(self.operation_deadline, BUILD_TIMEOUT)?;
        let outcome = execute_checked_with_stderr(
            self.executor.as_ref(),
            NixProgram::Modern,
            args,
            timeout,
            cancelled,
            &mut |chunk| parser.push(chunk, progress),
        )?;
        parser.finish(progress)?;
        if outcome.code != Some(0) {
            return Err(NixAdapterError::OperationFailed);
        }
        self.normalize_build_report(request, &outcome.stdout)
    }

    pub(super) fn normalize_build_report(
        &self,
        request: &BuildRequest,
        bytes: &[u8],
    ) -> Result<BuildReport, NixAdapterError> {
        let raw: Vec<RawBuildResult> = parse_json(bytes)?;
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
}

impl RealNixAdapter {
    /// Resolves one missing path against the batch probe, then the exact
    /// remote probe, and returns the validated `(download, nar)` sizes.
    /// `Ok(None)` means the path is absent (a miss).
    pub(super) fn remote_cache_sizes(
        &self,
        remote: &Option<RawPathInfoEnvelope>,
        path: &StorePath,
    ) -> Result<Option<(u64, u64)>, BuildCacheError> {
        let batch_entry = match remote {
            Some(remote) => batch_path_info_optional(remote, path)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?,
            None => None,
        };
        let Some(entry) = batch_entry else {
            let exact_remote = match self.raw_remote_path_info_with_retry(path) {
                Ok(exact) => exact,
                Err(NixAdapterError::OperationFailed) => return Ok(None),
                Err(_) => return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed)),
            };
            let Some(entry) = root_path_info_optional(&exact_remote, path)
                .map_err(|_| BuildCacheError::new(BuildCacheErrorCode::ProbeFailed))?
            else {
                return Ok(None);
            };
            return raw_path_sizes(entry).map(Some);
        };
        raw_path_sizes(entry).map(Some)
    }
}

impl NixpkgsMetadataRunner for RealNixAdapter {
    fn run_metadata(&self, pin: &NixpkgsPin) -> Result<Vec<u8>, NixpkgsSourceError> {
        let mut args = base_args();
        if self.eager_source_metadata {
            // Determinate Nix defaults to lazy trees, which omit the
            // materialized source store path from flake metadata. The pinned
            // source identity requires that exact path.
            args.extend(os_args(["--option", "lazy-trees", "false"]));
        }
        args.extend(os_args(["flake", "metadata", "--no-use-registries"]));
        args.push(
            format!(
                "github:NixOS/nixpkgs/{}?narHash={}",
                pin.revision().as_str(),
                pin.nar_hash().as_str()
            )
            .into(),
        );
        args.push("--json".into());
        self.require_success(MethodKind::EvaluateDerivation, args, EVALUATE_TIMEOUT)
            .map_err(|_| NixpkgsSourceError::runner_failure())
    }
}

impl NixAdapter for RealNixAdapter {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        let bytes =
            self.require_success(MethodKind::Version, vec!["--version".into()], SHORT_TIMEOUT)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| malformed())?.trim();
        if text
            != format!(
                "nix ({}) {}",
                self.expected_nix_brand, self.expected_nix_version
            )
        {
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
        if legacy_text
            != format!(
                "nix-store ({}) {}",
                self.expected_nix_brand, self.expected_nix_version
            )
        {
            return Err(NixAdapterError::UnsupportedUpstreamFormat {
                command: MethodKind::Version,
                observed: 0,
            });
        }
        Ok(VersionInfo::new(
            NixVersion::new(self.expected_nix_version)?,
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
        normalize_path_info(&self.raw_path_info(path, true, false)?, path)
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
        self.copy_cache_signatures(&[path])?;
        let local = self.raw_path_info(path, false, false)?;
        let local_entry = root_path_info(&local, path)?;
        if local_entry.nar_hash != remote_entry.nar_hash {
            return Err(NixAdapterError::IntegrityFailure);
        }
        let receipt = SubstituteReceipt::new(CACHE_URL, nar_hash, signatures)?;
        Ok(SubstituteReport::fetched(path.clone(), receipt))
    }

    fn substitute_many(
        &self,
        paths: &[StorePath],
    ) -> Result<Vec<SubstituteReport>, NixAdapterError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut ping = base_args();
        ping.extend(os_args(["store", "ping", "--store", CACHE_URL]));
        self.require_success(MethodKind::Substitute, ping, SHORT_TIMEOUT)
            .map_err(|_| NixAdapterError::Unavailable)?;

        let mut reports = Vec::with_capacity(paths.len());
        for chunk in paths.chunks(PATH_INFO_BATCH_SIZE) {
            let path_refs = chunk.iter().collect::<Vec<_>>();
            let remote = match self.raw_path_infos(&path_refs, false, true) {
                Ok(remote) => Some(remote),
                Err(NixAdapterError::OperationFailed) => None,
                Err(error) => return Err(error),
            };
            let mut chunk_reports = vec![None; chunk.len()];
            let mut authenticated = Vec::new();
            for (index, path) in chunk.iter().enumerate() {
                let exact_remote;
                let entry = match &remote {
                    Some(remote) => match batch_path_info_optional(remote, path)? {
                        Some(entry) => Some(entry),
                        None => {
                            exact_remote = self.raw_remote_path_info_with_retry(path)?;
                            root_path_info_optional(&exact_remote, path)?
                        }
                    },
                    None => {
                        exact_remote = self.raw_remote_path_info_with_retry(path)?;
                        root_path_info_optional(&exact_remote, path)?
                    }
                };
                let Some(entry) = entry else {
                    chunk_reports[index] = Some(SubstituteReport::miss(
                        path.clone(),
                        SubstituteOutcome::AbsentFromSubstituters,
                    )?);
                    continue;
                };
                let signatures = signatures(&entry.signatures)?;
                if !has_approved_cache_signature(&signatures) {
                    return Err(NixAdapterError::TrustFailure);
                }
                let nar_hash =
                    NarHash::new(&entry.nar_hash).map_err(|_| NixAdapterError::IntegrityFailure)?;
                authenticated.push((index, path, entry.nar_hash.clone(), nar_hash, signatures));
            }

            if !authenticated.is_empty() {
                let authenticated_paths = authenticated
                    .iter()
                    .map(|(_, path, _, _, _)| *path)
                    .collect::<Vec<_>>();
                let mut copy = base_args();
                copy.extend(os_args(["copy", "--from", CACHE_URL]));
                copy.extend(
                    authenticated_paths
                        .iter()
                        .map(|path| OsString::from(path.as_str())),
                );
                self.require_success(MethodKind::Substitute, copy, BUILD_TIMEOUT)
                    .map_err(|_| NixAdapterError::TrustFailure)?;
                self.copy_cache_signatures(&authenticated_paths)?;
                let local = self.raw_path_infos(&authenticated_paths, false, false)?;
                for (index, path, remote_hash, nar_hash, signatures) in authenticated {
                    let local_entry = root_path_info(&local, path)?;
                    if local_entry.nar_hash != remote_hash {
                        return Err(NixAdapterError::IntegrityFailure);
                    }
                    chunk_reports[index] = Some(SubstituteReport::fetched(
                        path.clone(),
                        SubstituteReceipt::new(CACHE_URL, nar_hash, signatures)?,
                    ));
                }
            }
            reports.extend(
                chunk_reports
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or(NixAdapterError::OperationFailed)?,
            );
        }
        Ok(reports)
    }

    fn build(&self, request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        self.run_build_with_progress(request, &|| false, &mut |_| Ok(()))
    }

    fn build_with_progress(
        &self,
        request: &BuildRequest,
        progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, NixAdapterError> {
        self.run_build_with_progress(request, &|| false, progress)
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
        collect_garbage(self.executor.as_ref(), Vec::new(), self.operation_deadline)
    }
}

pub(super) fn pinned_installable(request: &EvaluateDerivationRequest) -> String {
    format!(
        "github:NixOS/nixpkgs/{}?narHash={}#legacyPackages.{}.{}",
        request.nixpkgs_revision().as_str(),
        percent_encode(request.nixpkgs_nar_hash().as_str()),
        request.system().as_str(),
        request.attribute().as_str()
    )
}

pub(super) fn percent_encode(value: &str) -> String {
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

#[cfg(unix)]
pub(super) fn is_private(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o077 == 0
}

#[cfg(not(unix))]
pub(super) const fn is_private(_metadata: &fs::Metadata) -> bool {
    true
}
