//! Deterministic, explicitly approved local-build planning and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use pkg_channel::{BuildMode, CachePolicy};
use pkg_core::state::{Digest, canonical_digest};
use pkg_core::{
    AttributePath, ChannelSequence, DerivationPath, NarHash, NixpkgsRevision, OutputName,
    OutputSelection, PackageVersion, PolicyVersion, SelectorId, SelectorInput, SourceRevision,
    StorePath, System, VersionBound, VersionPreference, VersionRange,
};
use serde::{Deserialize, Serialize};

use crate::{
    BuildApprovalReceipt, BuildOutputProvenance, BuildProgressEstimate, BuildReport, BuildRequest,
    BuildStatus, DerivationPlanReport, DerivedOutputTarget, NarIntegrity, NixAdapter,
    NixAdapterError, NixVersion, OperationId, PathInfoReport, Signature, TrustStatus, VerifyMode,
    VerifyRequest,
};

const MAX_TEXT: usize = 256;
const MAX_PREVIEW_ITEMS: usize = 4096;
const RESOURCE_NOTICE: &str = "Builds run sandboxed. The managed runtime applies no hard per-build memory/CPU/IO cap; daemon time/log ceilings and one machine-global build admission bound the operation.";
const BUILD_USERS_GROUP: &str = "nixbld";
const MAX_JOBS: u32 = 1;
const CORES_HINT: u32 = 0;
const MAX_SILENT_SECONDS: u64 = 3_600;
const TIMEOUT_SECONDS: u64 = 86_400;
const MAX_LOG_BYTES: u64 = 268_435_456;
const DISK_HEADROOM_PERCENT: u64 = 120;
const BOOTSTRAP_MISS_ALLOWANCE_BYTES: u64 = 1_073_741_824;
const ADMISSION_WAIT_POLL: Duration = Duration::from_millis(25);

/// Stable local-build refusal category exposed to the broker/CLI mapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildEngineErrorCode {
    /// Authenticated policy denies local building.
    BuildDenied,
    /// The deterministic private plan was internally inconsistent.
    InvalidPlan,
    /// Sandbox, build-user, or platform readiness was not proven.
    ReadinessFailed,
    /// No explicit single-operation approval exists.
    ApprovalRequired,
    /// The operation's approved plan or policy changed before execution.
    ApprovalInvalidated,
    /// The approval was already consumed or its operation id is active.
    ApprovalUnavailable,
    /// Disk or load preflight failed twice under admission.
    ResourcePreflightFailed,
    /// The waiter cancelled before acquiring build admission.
    Cancelled,
    /// Approval journaling failed closed.
    JournalFailed,
    /// The managed adapter failed or returned inconsistent output identity.
    BuildFailed,
    /// Nix reported that no permitted binary or local build was available.
    AcquireNoBinary,
}

/// Redacted local-build engine error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildEngineError {
    code: BuildEngineErrorCode,
}

impl BuildEngineError {
    const fn new(code: BuildEngineErrorCode) -> Self {
        Self { code }
    }

    pub(crate) const fn approval_invalidated() -> Self {
        Self::new(BuildEngineErrorCode::ApprovalInvalidated)
    }

    /// Returns the stable public mapping category.
    #[must_use]
    pub const fn code(self) -> BuildEngineErrorCode {
        self.code
    }
}

impl fmt::Display for BuildEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "local build refused: {:?}", self.code)
    }
}

impl std::error::Error for BuildEngineError {}

/// One product-owned target included in a private build plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlanTarget {
    selector_id: SelectorId,
    selector: SelectorInput,
    attribute: AttributePath,
    version_preference: VersionPreference,
    output_selection: OutputSelection,
    source_revision: SourceRevision,
    plan: DerivationPlanReport,
}

impl BuildPlanTarget {
    /// Binds resolved selector intent to one evaluated derivation plan.
    #[must_use]
    pub const fn new(
        selector_id: SelectorId,
        selector: SelectorInput,
        attribute: AttributePath,
        version_preference: VersionPreference,
        output_selection: OutputSelection,
        source_revision: SourceRevision,
        plan: DerivationPlanReport,
    ) -> Self {
        Self {
            selector_id,
            selector,
            attribute,
            version_preference,
            output_selection,
            source_revision,
            plan,
        }
    }
}

/// Deterministic cache classification of the union derivation closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheClassification {
    classification_digest: String,
    hits: u64,
    misses: u64,
    known_cache_bytes: KnownCacheBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnownCacheBytes {
    download_bytes: u64,
    nar_bytes: u64,
}

impl CacheClassification {
    /// Constructs exact cache-presence identity and known hit-only byte totals.
    pub fn new(
        classification_digest: Digest,
        hits: u64,
        misses: u64,
        download_bytes: u64,
        nar_bytes: u64,
    ) -> Result<Self, BuildEngineError> {
        if misses == 0 || hits.checked_add(misses).is_none() {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Ok(Self {
            classification_digest: digest_string(classification_digest),
            hits,
            misses,
            known_cache_bytes: KnownCacheBytes {
                download_bytes,
                nar_bytes,
            },
        })
    }
}

/// Stable cross-platform local-build readiness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReadiness {
    sandbox: SandboxReadiness,
    build_users_group: String,
    build_users_ready: bool,
    use_cgroups_enabled: bool,
    cgroup_v2_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SandboxReadiness {
    enabled: bool,
    fallback: bool,
}

impl BuildReadiness {
    /// Constructs explicit readiness; platform invariants are checked by the plan.
    #[must_use]
    pub fn new(
        sandbox_enabled: bool,
        sandbox_fallback: bool,
        build_users_ready: bool,
        use_cgroups_enabled: bool,
        cgroup_v2_ready: bool,
    ) -> Self {
        Self {
            sandbox: SandboxReadiness {
                enabled: sandbox_enabled,
                fallback: sandbox_fallback,
            },
            build_users_group: BUILD_USERS_GROUP.to_owned(),
            build_users_ready,
            use_cgroups_enabled,
            cgroup_v2_ready,
        }
    }

    fn validate(&self, system: System) -> Result<(), BuildEngineError> {
        if !self.sandbox.enabled
            || self.sandbox.fallback
            || !self.build_users_ready
            || self.build_users_group != BUILD_USERS_GROUP
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::ReadinessFailed));
        }
        let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
        if linux != self.use_cgroups_enabled || linux != self.cgroup_v2_ready {
            return Err(BuildEngineError::new(BuildEngineErrorCode::ReadinessFailed));
        }
        Ok(())
    }
}

/// Fixed, unit-bearing V1 daemon resource settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResources {
    max_jobs_per_connection: u32,
    machine_global_max_concurrent_build_operations: u32,
    cores_hint: u32,
    max_silent_time_seconds: u64,
    timeout_seconds_per_derivation: u64,
    max_build_log_size_bytes: u64,
}

impl Default for BuildResources {
    fn default() -> Self {
        Self {
            max_jobs_per_connection: MAX_JOBS,
            machine_global_max_concurrent_build_operations: 1,
            cores_hint: CORES_HINT,
            max_silent_time_seconds: MAX_SILENT_SECONDS,
            timeout_seconds_per_derivation: TIMEOUT_SECONDS,
            max_build_log_size_bytes: MAX_LOG_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildAdmissionPolicy {
    disk_headroom_percent: u64,
    max_loadavg_ceiling: u64,
}

impl BuildAdmissionPolicy {
    fn for_host_cores(host_cores: u32) -> Result<Self, BuildEngineError> {
        let host_cores = u64::from(host_cores.max(1));
        let max_loadavg_ceiling = host_cores
            .checked_mul(2)
            .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        Ok(Self {
            disk_headroom_percent: DISK_HEADROOM_PERCENT,
            max_loadavg_ceiling,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NixpkgsIdentity {
    rev: String,
    nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalTarget {
    selector_id: String,
    selector: String,
    attribute: String,
    package_name: String,
    package_version: String,
    version_preference: CanonicalVersionPreference,
    requested_outputs: Option<Vec<String>>,
    source_revision: String,
    outputs_to_install: Vec<String>,
    root_derivation: String,
    local_build_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum CanonicalVersionPreference {
    Any,
    Exact {
        version: String,
    },
    Minimum {
        version: String,
    },
    Range {
        lower: Option<CanonicalVersionBound>,
        upper: Option<CanonicalVersionBound>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalVersionBound {
    version: String,
    inclusive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DerivationClosureIdentity {
    json_version: u32,
    closure_digest: String,
    derivation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalBuild {
    derivation_digest: String,
    name: String,
    system: String,
    fixed_output: bool,
    network_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosureDocumentIdentity {
    derivation: String,
    document_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildExecution {
    targets: Vec<DerivedOutputTarget>,
    expected_outputs: BTreeSet<String>,
}

/// Private canonical approval subject retained only inside the managed engine.
///
/// This type intentionally does not implement `Serialize`; only its private
/// digest subject can cross the canonicalization boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct BuildPlan {
    schema_version: u32,
    nix_runtime_version: String,
    descriptor_hash: String,
    policy_version: u64,
    channel_seq: u64,
    nixpkgs: NixpkgsIdentity,
    system: String,
    targets: Vec<CanonicalTarget>,
    derivation_closure: DerivationClosureIdentity,
    builds: Vec<CanonicalBuild>,
    cache_classification: CacheClassification,
    readiness: BuildReadiness,
    resources: BuildResources,
    admission: BuildAdmissionPolicy,
    system_identity: System,
    policy_identity: PolicyVersion,
    execution: BuildExecution,
    install_targets: Vec<BuildPlanTarget>,
}

/// One broker-derived derivation included in a repair-build approval subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlanDerivation {
    derivation: DerivationPath,
    name: String,
    system: System,
    outputs: BTreeMap<OutputName, StorePath>,
    document_digest: Digest,
    fixed_output: bool,
}

impl RepairPlanDerivation {
    /// Constructs one validated repair derivation from trusted Nix metadata.
    pub fn new(
        derivation: DerivationPath,
        name: String,
        system: System,
        outputs: BTreeMap<OutputName, StorePath>,
        document_digest: Digest,
        fixed_output: bool,
    ) -> Result<Self, BuildEngineError> {
        if outputs.is_empty() || outputs.len() > MAX_PREVIEW_ITEMS {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        checked_text(&name)?;
        Ok(Self {
            derivation,
            name,
            system,
            outputs,
            document_digest,
            fixed_output,
        })
    }
}

/// One damaged path and the valid derivation that may replace it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlanTarget {
    path: StorePath,
    derivation: RepairPlanDerivation,
}

impl RepairPlanTarget {
    /// Binds one exact damaged path to trusted local derivation metadata.
    #[must_use]
    pub const fn new(path: StorePath, derivation: RepairPlanDerivation) -> Self {
        Self { path, derivation }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalRepairTarget {
    path: String,
    deriver: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalRepairOutput {
    name: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalRepairDerivation {
    derivation: String,
    document_digest: String,
    name: String,
    system: String,
    outputs: Vec<CanonicalRepairOutput>,
    fixed_output: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairBuildPlanDigestSubject<'a> {
    schema_version: u32,
    nix_runtime_version: &'a str,
    policy_version: u64,
    system: &'a str,
    targets: &'a [CanonicalRepairTarget],
    derivations: &'a [CanonicalRepairDerivation],
    readiness: &'a BuildReadiness,
    resources: &'a BuildResources,
    admission: &'a BuildAdmissionPolicy,
}

/// Private full-output approval subject for a local repair rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairBuildPlan {
    schema_version: u32,
    nix_runtime_version: String,
    policy_version: u64,
    system: String,
    targets: Vec<CanonicalRepairTarget>,
    derivations: Vec<CanonicalRepairDerivation>,
    readiness: BuildReadiness,
    resources: BuildResources,
    admission: BuildAdmissionPolicy,
    policy_identity: PolicyVersion,
    system_identity: System,
}

impl RepairBuildPlan {
    /// Constructs the deterministic full-output repair approval subject.
    pub fn new(
        nix_runtime_version: &NixVersion,
        policy_version: PolicyVersion,
        system: System,
        readiness: BuildReadiness,
        host_cores: u32,
        mut inputs: Vec<RepairPlanTarget>,
    ) -> Result<Self, BuildEngineError> {
        readiness.validate(system)?;
        if inputs.is_empty() || inputs.len() > MAX_PREVIEW_ITEMS {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        inputs.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        if inputs.windows(2).any(|pair| pair[0].path == pair[1].path)
            || inputs.iter().any(|input| input.derivation.system != system)
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }

        let targets = inputs
            .iter()
            .map(|input| CanonicalRepairTarget {
                path: input.path.as_str().to_owned(),
                deriver: input.derivation.derivation.as_str().to_owned(),
            })
            .collect::<Vec<_>>();
        let mut by_derivation = BTreeMap::new();
        for input in inputs {
            let key = input.derivation.derivation.as_str().to_owned();
            if by_derivation
                .insert(key, input.derivation.clone())
                .is_some_and(|previous| previous != input.derivation)
            {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
        }
        let derivations = by_derivation
            .into_values()
            .map(|derivation| CanonicalRepairDerivation {
                derivation: derivation.derivation.as_str().to_owned(),
                document_digest: digest_string(derivation.document_digest),
                name: derivation.name,
                system: derivation.system.as_str().to_owned(),
                outputs: derivation
                    .outputs
                    .into_iter()
                    .map(|(name, path)| CanonicalRepairOutput {
                        name: name.as_str().to_owned(),
                        path: path.as_str().to_owned(),
                    })
                    .collect(),
                fixed_output: derivation.fixed_output,
            })
            .collect::<Vec<_>>();
        if derivations.is_empty() || derivations.len() > MAX_PREVIEW_ITEMS {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Ok(Self {
            schema_version: 1,
            nix_runtime_version: checked_text(nix_runtime_version.as_str())?,
            policy_version: policy_version.get().get(),
            system: system.as_str().to_owned(),
            targets,
            derivations,
            readiness,
            resources: BuildResources::default(),
            admission: BuildAdmissionPolicy::for_host_cores(host_cores)?,
            policy_identity: policy_version,
            system_identity: system,
        })
    }

    /// Computes the RFC 8785/JCS repair-plan identity.
    pub fn digest(&self) -> Result<Digest, BuildEngineError> {
        canonical_digest(&RepairBuildPlanDigestSubject {
            schema_version: self.schema_version,
            nix_runtime_version: &self.nix_runtime_version,
            policy_version: self.policy_version,
            system: &self.system,
            targets: &self.targets,
            derivations: &self.derivations,
            readiness: &self.readiness,
            resources: &self.resources,
            admission: &self.admission,
        })
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }

    /// Returns the policy version bound into this plan.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_identity
    }

    /// Produces the ordinary sanitized build-preview shape for repair approval.
    pub fn preview(&self) -> Result<BuildPreview, BuildEngineError> {
        let digest = self.digest()?;
        let (os, arch) = product_platform(self.system_identity);
        let targets = self
            .derivations
            .iter()
            .enumerate()
            .map(|(index, derivation)| PreviewTarget {
                selector: format!("repair-{}", index + 1),
                package_name: derivation.name.clone(),
                version: "installed".to_owned(),
                outputs_to_install: derivation
                    .outputs
                    .iter()
                    .map(|output| output.name.clone())
                    .collect(),
                local_build_required: true,
            })
            .collect::<Vec<_>>();
        let unknown_local_outputs = self.derivations.iter().try_fold(0_u64, |total, item| {
            total
                .checked_add(
                    u64::try_from(item.outputs.len())
                        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
                )
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
        })?;
        let preview = BuildPreview {
            schema_version: 1,
            platform: PreviewPlatform {
                os: os.to_owned(),
                arch: arch.to_owned(),
            },
            policy_version: self.policy_version,
            build_plan_digest: digest_string(digest),
            targets,
            build: PreviewBuild {
                count: self.derivations.len(),
                names: self
                    .derivations
                    .iter()
                    .map(|derivation| derivation.name.clone())
                    .collect(),
                has_fixed_output: self
                    .derivations
                    .iter()
                    .any(|derivation| derivation.fixed_output),
            },
            cache: PreviewCache {
                known_download_bytes: 0,
                known_content_bytes: 0,
            },
            unknown_local_outputs,
            estimates: BuildPreviewEstimates::unavailable(),
            readiness: PreviewReadiness {
                sandboxed: self.readiness.sandbox.enabled,
                build_isolation_ready: self.readiness.build_users_ready,
                native_build: true,
                resource_boundary: PreviewResourceBoundary {
                    isolation: "sandbox".to_owned(),
                    per_build_resource_cap: false,
                    notice: RESOURCE_NOTICE.to_owned(),
                },
            },
            approval_required: true,
        };
        preview.validate()?;
        Ok(preview)
    }
}

impl fmt::Debug for BuildPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuildPlan")
            .field("policy_version", &self.policy_version)
            .field("channel_seq", &self.channel_seq)
            .field("target_count", &self.targets.len())
            .field(
                "derivation_count",
                &self.derivation_closure.derivation_count,
            )
            .field("build_count", &self.builds.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildPlanDigestSubject<'a> {
    schema_version: u32,
    nix_runtime_version: &'a str,
    descriptor_hash: &'a str,
    policy_version: u64,
    channel_seq: u64,
    nixpkgs: &'a NixpkgsIdentity,
    system: &'a str,
    targets: &'a [CanonicalTarget],
    derivation_closure: &'a DerivationClosureIdentity,
    builds: &'a [CanonicalBuild],
    cache_classification: &'a CacheClassification,
    readiness: &'a BuildReadiness,
    resources: &'a BuildResources,
    admission: &'a BuildAdmissionPolicy,
}

impl BuildPlan {
    /// Constructs the deterministic operation-wide approval subject.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nix_runtime_version: &NixVersion,
        descriptor_hash: Digest,
        policy_version: PolicyVersion,
        channel_seq: ChannelSequence,
        revision: &NixpkgsRevision,
        nar_hash: &NarHash,
        system: System,
        host_system: System,
        build_mode: BuildMode,
        mut targets: Vec<BuildPlanTarget>,
        mut missing_derivations: Vec<crate::DerivationPath>,
        cache_classification: CacheClassification,
        readiness: BuildReadiness,
        host_cores: u32,
    ) -> Result<Self, BuildEngineError> {
        if system != host_system || build_mode != BuildMode::AllowWithGates {
            return Err(BuildEngineError::new(BuildEngineErrorCode::BuildDenied));
        }
        readiness.validate(system)?;
        if targets.is_empty() || missing_derivations.is_empty() {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        missing_derivations.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if missing_derivations
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        targets.sort_by(|left, right| left.selector_id.as_str().cmp(right.selector_id.as_str()));
        if targets
            .windows(2)
            .any(|pair| pair[0].selector_id == pair[1].selector_id)
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }

        let missing_derivation_names = missing_derivations
            .iter()
            .map(|path| path.as_str())
            .collect::<BTreeSet<_>>();
        let mut closure = BTreeMap::new();
        let mut canonical_targets = Vec::with_capacity(targets.len());
        let mut execution_targets = Vec::with_capacity(targets.len());
        let mut expected_outputs = BTreeSet::new();
        for target in &targets {
            let source_matches = match &target.source_revision {
                SourceRevision::CurrentChannel => true,
                SourceRevision::PinnedChannel(sequence) => *sequence == channel_seq,
                SourceRevision::ExactRevision(target_revision) => target_revision == revision,
            };
            if !source_matches
                || target
                    .output_selection
                    .explicit_outputs()
                    .is_some_and(|requested| {
                        !same_output_selection(requested, target.plan.outputs_to_install())
                    })
            {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
            for derivation in target.plan.derivations() {
                if !derivation.system().is_compatible_with(system) {
                    return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
                }
                let identity = ClosureDocumentIdentity {
                    derivation: derivation.derivation().as_str().to_owned(),
                    document_digest: digest_string(derivation.document_digest()),
                };
                if closure
                    .insert(
                        derivation.derivation().as_str().to_owned(),
                        identity.clone(),
                    )
                    .is_some_and(|previous| previous != identity)
                {
                    return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
                }
            }
            let root = target
                .plan
                .derivations()
                .iter()
                .find(|derivation| derivation.derivation() == target.plan.root())
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            for output in target.plan.outputs_to_install() {
                let path = root
                    .outputs()
                    .get(output)
                    .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
                expected_outputs.insert(path.as_str().to_owned());
            }
            execution_targets.push(
                DerivedOutputTarget::new(
                    target.plan.root().clone(),
                    target.plan.outputs_to_install().to_vec(),
                )
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            );
            canonical_targets.push(CanonicalTarget {
                selector_id: target.selector_id.as_str().to_owned(),
                selector: target.selector.as_str().to_owned(),
                attribute: target.attribute.as_str().to_owned(),
                package_name: checked_text(target.plan.pname())?,
                package_version: checked_text(target.plan.version().as_str())?,
                version_preference: canonical_version_preference(&target.version_preference),
                requested_outputs: target.output_selection.explicit_outputs().map(|outputs| {
                    outputs
                        .iter()
                        .map(OutputName::as_str)
                        .map(str::to_owned)
                        .collect()
                }),
                source_revision: target.source_revision.to_canonical_string(),
                outputs_to_install: target
                    .plan
                    .outputs_to_install()
                    .iter()
                    .map(OutputName::as_str)
                    .map(str::to_owned)
                    .collect(),
                root_derivation: target.plan.root().as_str().to_owned(),
                local_build_required: target.plan.derivations().iter().any(|derivation| {
                    missing_derivation_names.contains(derivation.derivation().as_str())
                }),
            });
        }

        let closure_documents = closure.into_values().collect::<Vec<_>>();
        let closure_digest = canonical_digest(&closure_documents)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        let mut builds = Vec::with_capacity(missing_derivations.len());
        for missing in &missing_derivations {
            let derivation = targets
                .iter()
                .flat_map(|target| target.plan.derivations())
                .find(|derivation| derivation.derivation() == missing)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            builds.push(CanonicalBuild {
                derivation_digest: digest_string(derivation.document_digest()),
                name: checked_text(derivation.name())?,
                system: system.as_str().to_owned(),
                fixed_output: derivation.fixed_output(),
                network_enabled: derivation.fixed_output(),
            });
        }
        builds.sort_by(|left, right| {
            left.derivation_digest
                .cmp(&right.derivation_digest)
                .then_with(|| left.name.cmp(&right.name))
        });

        Ok(Self {
            schema_version: 1,
            nix_runtime_version: checked_text(nix_runtime_version.as_str())?,
            descriptor_hash: digest_string(descriptor_hash),
            policy_version: policy_version.get().get(),
            channel_seq: channel_seq.get().get(),
            nixpkgs: NixpkgsIdentity {
                rev: revision.as_str().to_owned(),
                nar_hash: nar_hash.as_str().to_owned(),
            },
            system: system.as_str().to_owned(),
            targets: canonical_targets,
            derivation_closure: DerivationClosureIdentity {
                json_version: 4,
                closure_digest: digest_string(closure_digest),
                derivation_count: closure_documents.len(),
            },
            builds,
            cache_classification,
            readiness,
            resources: BuildResources::default(),
            admission: BuildAdmissionPolicy::for_host_cores(host_cores)?,
            system_identity: system,
            policy_identity: policy_version,
            execution: BuildExecution {
                targets: execution_targets,
                expected_outputs,
            },
            install_targets: targets,
        })
    }

    /// Computes the RFC 8785/JCS identity bound by approval.
    pub fn digest(&self) -> Result<Digest, BuildEngineError> {
        canonical_digest(&BuildPlanDigestSubject {
            schema_version: self.schema_version,
            nix_runtime_version: &self.nix_runtime_version,
            descriptor_hash: &self.descriptor_hash,
            policy_version: self.policy_version,
            channel_seq: self.channel_seq,
            nixpkgs: &self.nixpkgs,
            system: &self.system,
            targets: &self.targets,
            derivation_closure: &self.derivation_closure,
            builds: &self.builds,
            cache_classification: &self.cache_classification,
            readiness: &self.readiness,
            resources: &self.resources,
            admission: &self.admission,
        })
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }

    /// Produces the only public, sanitized view of this private plan.
    pub fn preview(&self) -> Result<BuildPreview, BuildEngineError> {
        self.preview_with_estimates(BuildPreviewEstimates::unavailable())
    }

    /// Produces the fixed V1 bootstrap estimate used for disk admission.
    ///
    /// Nix cannot report the realized size of an uncached output before it is
    /// built. Until authenticated historical observations exist, V1 therefore
    /// reserves one GiB for every cache-miss path and adds the exact NarInfo
    /// content bytes for cache-present paths. The result is explicitly a
    /// heuristic: build time and total realized closure size remain unknown.
    pub fn bootstrap_estimates(&self) -> Result<BuildPreviewEstimates, BuildEngineError> {
        let miss_allowance = self
            .cache_classification
            .misses
            .checked_mul(BOOTSTRAP_MISS_ALLOWANCE_BYTES)
            .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        let approx_new_disk_bytes = self
            .cache_classification
            .known_cache_bytes
            .nar_bytes
            .checked_add(miss_allowance)
            .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        BuildPreviewEstimates::new(None, Some(approx_new_disk_bytes), None)
    }

    /// Produces a sanitized preview with volatile, non-digest-bound estimates.
    pub fn preview_with_estimates(
        &self,
        estimates: BuildPreviewEstimates,
    ) -> Result<BuildPreview, BuildEngineError> {
        estimates.validate()?;
        let digest = self.digest()?;
        let (os, arch) = product_platform(self.system_identity);
        let mut names = self
            .builds
            .iter()
            .map(|build| build.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        Ok(BuildPreview {
            schema_version: 1,
            platform: PreviewPlatform {
                os: os.to_owned(),
                arch: arch.to_owned(),
            },
            policy_version: self.policy_version,
            build_plan_digest: digest_string(digest),
            targets: self
                .targets
                .iter()
                .map(|target| PreviewTarget {
                    selector: target.selector.clone(),
                    package_name: target.package_name.clone(),
                    version: target.package_version.clone(),
                    outputs_to_install: target.outputs_to_install.clone(),
                    local_build_required: target.local_build_required,
                })
                .collect(),
            build: PreviewBuild {
                count: self.builds.len(),
                names,
                has_fixed_output: self.builds.iter().any(|build| build.fixed_output),
            },
            cache: PreviewCache {
                known_download_bytes: self.cache_classification.known_cache_bytes.download_bytes,
                known_content_bytes: self.cache_classification.known_cache_bytes.nar_bytes,
            },
            unknown_local_outputs: self.cache_classification.misses,
            estimates,
            readiness: PreviewReadiness {
                sandboxed: self.readiness.sandbox.enabled,
                build_isolation_ready: self.readiness.build_users_ready,
                native_build: true,
                resource_boundary: PreviewResourceBoundary {
                    isolation: "sandbox".to_owned(),
                    per_build_resource_cap: false,
                    notice: RESOURCE_NOTICE.to_owned(),
                },
            },
            approval_required: true,
        })
    }
}

/// Broker-produced, post-execution evidence used to assemble install state.
///
/// The value is derived only from the admission-time revalidated private plan,
/// the actual build report, and fresh integrity/path metadata from the managed
/// adapter. It is intentionally private-data-bearing and may cross only the
/// authenticated CLI↔broker channel; public rendering must continue to reject
/// store and derivation identities.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallEvidence {
    descriptor_hash: Digest,
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    revision: NixpkgsRevision,
    source_nar_hash: NarHash,
    system: System,
    targets: Vec<InstallTargetEvidence>,
}

impl fmt::Debug for InstallEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallEvidence")
            .field("channel_sequence", &self.channel_sequence)
            .field("policy_version", &self.policy_version)
            .field("target_count", &self.targets.len())
            .finish_non_exhaustive()
    }
}

impl InstallEvidence {
    /// Returns the authenticated channel descriptor digest.
    #[must_use]
    pub const fn descriptor_hash(&self) -> Digest {
        self.descriptor_hash
    }

    /// Returns the authenticated channel sequence.
    #[must_use]
    pub const fn channel_sequence(&self) -> ChannelSequence {
        self.channel_sequence
    }

    /// Returns the authenticated policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the exact Nixpkgs revision used for evaluation.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        &self.revision
    }

    /// Returns the authenticated normalized Nixpkgs source identity.
    #[must_use]
    pub const fn source_nar_hash(&self) -> &NarHash {
        &self.source_nar_hash
    }

    /// Returns the native target system used for evaluation and execution.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns targets in stable selector-id order.
    #[must_use]
    pub fn targets(&self) -> &[InstallTargetEvidence] {
        &self.targets
    }

    /// Encodes this private evidence using the closed schema consumed by the
    /// authenticated product frame.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, BuildEngineError> {
        serde_json::to_vec(&InstallEvidenceWire::from(self))
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }

    /// Strictly decodes and revalidates broker-produced install evidence.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, BuildEngineError> {
        if bytes.len() > crate::JsonCodec::PRODUCTION_LIMIT {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        let wire: InstallEvidenceWire = serde_json::from_slice(bytes)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        Self::try_from(wire)
    }

    /// Builds install evidence from outputs accepted by the cache-only
    /// substitution boundary.
    ///
    /// Each substitute is an opaque capability produced only after signature,
    /// integrity, trust, and metadata verification. Fresh path metadata must
    /// still match that capability exactly. The selected output set must also
    /// equal the resolver-owned target set, with no missing or extra path.
    ///
    /// # Errors
    ///
    /// Refuses inconsistent source identity, targets, substitute metadata, or
    /// adapter results.
    #[allow(clippy::too_many_arguments)]
    pub fn from_cache_substitutes(
        descriptor_hash: Digest,
        channel_sequence: ChannelSequence,
        policy_version: PolicyVersion,
        revision: NixpkgsRevision,
        source_nar_hash: NarHash,
        system: System,
        targets: Vec<BuildPlanTarget>,
        substitutes: Vec<crate::VerifiedSubstitute>,
        adapter: &dyn NixAdapter,
    ) -> Result<Self, BuildEngineError> {
        let mut metadata = BTreeMap::new();
        for substitute in substitutes {
            let path = substitute.store_path().clone();
            let info = adapter
                .path_info(&path)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
            if info.store_path() != &path
                || info.nar_hash() != substitute.nar_hash()
                || info.signatures() != substitute.signatures()
                || info.references() != substitute.references()
                || info.nar_size() != substitute.nar_size()
                || info.closure_size() != substitute.closure_size()
                || info.signatures().is_empty()
            {
                return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
            }
            let value = (info, BuildOutputProvenance::CacheSigned);
            if let Some(existing) = metadata.get(path.as_str()) {
                if existing != &value {
                    return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
                }
            } else {
                metadata.insert(path.as_str().to_owned(), value);
            }
        }
        Self::from_output_metadata(
            descriptor_hash,
            channel_sequence,
            policy_version,
            revision,
            source_nar_hash,
            system,
            &targets,
            metadata,
        )
    }

    fn from_executed_plan(
        plan: &BuildPlan,
        report: &BuildReport,
        adapter: &dyn NixAdapter,
    ) -> Result<Self, BuildEngineError> {
        let report_outputs = report
            .outputs()
            .iter()
            .map(|output| {
                (
                    output.store_path().as_str().to_owned(),
                    (output.store_path().clone(), output.provenance()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if report_outputs.len() != report.outputs().len() {
            return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
        }
        let paths = report_outputs
            .values()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let verify_request = VerifyRequest::new(paths.clone(), VerifyMode::Recursive)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
        let verification = adapter
            .verify(&verify_request)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
        if paths.iter().any(|path| {
            !verification
                .results()
                .iter()
                .any(|result| result.path() == path)
        }) || verification.results().iter().any(|result| {
            result.nar_integrity() != NarIntegrity::Intact || result.trust() != TrustStatus::Trusted
        }) {
            return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
        }

        let mut metadata = BTreeMap::new();
        for path in paths {
            let info = adapter
                .path_info(&path)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
            let provenance = report_outputs
                .get(path.as_str())
                .map(|(_, provenance)| *provenance)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
            if info.store_path() != &path {
                return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
            }
            metadata.insert(path.as_str().to_owned(), (info, provenance));
        }

        Self::from_output_metadata(
            parse_canonical_digest(&plan.descriptor_hash)?,
            ChannelSequence::from_u64(plan.channel_seq)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            plan.policy_identity,
            NixpkgsRevision::new(&plan.nixpkgs.rev)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            NarHash::new(&plan.nixpkgs.nar_hash)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            plan.system_identity,
            &plan.install_targets,
            metadata,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_output_metadata(
        descriptor_hash: Digest,
        channel_sequence: ChannelSequence,
        policy_version: PolicyVersion,
        revision: NixpkgsRevision,
        source_nar_hash: NarHash,
        system: System,
        plan_targets: &[BuildPlanTarget],
        metadata: BTreeMap<String, (PathInfoReport, BuildOutputProvenance)>,
    ) -> Result<Self, BuildEngineError> {
        let mut targets = Vec::with_capacity(plan_targets.len());
        let mut used_paths = BTreeSet::new();
        for target in plan_targets {
            let root = target
                .plan
                .derivations()
                .iter()
                .find(|item| item.derivation() == target.plan.root())
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            let mut acquired = Vec::with_capacity(target.plan.outputs_to_install().len());
            for output_name in target.plan.outputs_to_install() {
                let store_path = root
                    .outputs()
                    .get(output_name)
                    .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
                let (info, provenance) = metadata
                    .get(store_path.as_str())
                    .cloned()
                    .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
                used_paths.insert(store_path.as_str().to_owned());
                if provenance == BuildOutputProvenance::CacheSigned && info.signatures().is_empty()
                {
                    return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
                }
                acquired.push(InstallOutputEvidence {
                    output_name: output_name.clone(),
                    info,
                    provenance,
                });
            }
            targets.push(InstallTargetEvidence {
                selector_id: target.selector_id.clone(),
                selector: target.selector.clone(),
                attribute: target.attribute.clone(),
                version_preference: target.version_preference.clone(),
                output_selection: target.output_selection.clone(),
                source_revision: target.source_revision.clone(),
                root_derivation: target.plan.root().clone(),
                root_outputs: root.outputs().clone(),
                outputs_to_install: target.plan.outputs_to_install().to_vec(),
                package_name: target.plan.pname().to_owned(),
                package_version: target.plan.version().clone(),
                acquired,
            });
        }
        if used_paths.len() != metadata.len()
            || metadata.keys().any(|path| !used_paths.contains(path))
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
        }
        Self::new(
            descriptor_hash,
            channel_sequence,
            policy_version,
            revision,
            source_nar_hash,
            system,
            targets,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        descriptor_hash: Digest,
        channel_sequence: ChannelSequence,
        policy_version: PolicyVersion,
        revision: NixpkgsRevision,
        source_nar_hash: NarHash,
        system: System,
        mut targets: Vec<InstallTargetEvidence>,
    ) -> Result<Self, BuildEngineError> {
        if targets.is_empty() || targets.len() > MAX_PREVIEW_ITEMS {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        targets.sort_by(|left, right| left.selector_id.as_str().cmp(right.selector_id.as_str()));
        if targets
            .windows(2)
            .any(|pair| pair[0].selector_id == pair[1].selector_id)
            || targets.iter().any(|target| {
                target.validate().is_err()
                    || match target.source_revision() {
                        SourceRevision::CurrentChannel => false,
                        SourceRevision::PinnedChannel(sequence) => *sequence != channel_sequence,
                        SourceRevision::ExactRevision(target_revision) => {
                            target_revision != &revision
                        }
                    }
            })
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Ok(Self {
            descriptor_hash,
            channel_sequence,
            policy_version,
            revision,
            source_nar_hash,
            system,
            targets,
        })
    }
}

/// One selector and its fully verified realized outputs.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallTargetEvidence {
    selector_id: SelectorId,
    selector: SelectorInput,
    attribute: AttributePath,
    version_preference: VersionPreference,
    output_selection: OutputSelection,
    source_revision: SourceRevision,
    root_derivation: DerivationPath,
    root_outputs: BTreeMap<OutputName, StorePath>,
    outputs_to_install: Vec<OutputName>,
    package_name: String,
    package_version: PackageVersion,
    acquired: Vec<InstallOutputEvidence>,
}

impl InstallTargetEvidence {
    /// Returns the stable desired-state selector id.
    #[must_use]
    pub const fn selector_id(&self) -> &SelectorId {
        &self.selector_id
    }

    /// Returns the original validated selector spelling.
    #[must_use]
    pub const fn selector(&self) -> &SelectorInput {
        &self.selector
    }

    /// Returns the resolver-owned Nixpkgs attribute.
    #[must_use]
    pub const fn attribute(&self) -> &AttributePath {
        &self.attribute
    }

    /// Returns the desired-state version preference.
    #[must_use]
    pub const fn version_preference(&self) -> &VersionPreference {
        &self.version_preference
    }

    /// Returns the user's default or explicit output selection.
    #[must_use]
    pub const fn output_selection(&self) -> &OutputSelection {
        &self.output_selection
    }

    /// Returns the desired-state source selection.
    #[must_use]
    pub const fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }

    /// Returns the evaluated root derivation identity.
    #[must_use]
    pub const fn root_derivation(&self) -> &DerivationPath {
        &self.root_derivation
    }

    /// Returns every named output of the root derivation.
    #[must_use]
    pub const fn root_outputs(&self) -> &BTreeMap<OutputName, StorePath> {
        &self.root_outputs
    }

    /// Returns the selected outputs in canonical order.
    #[must_use]
    pub fn outputs_to_install(&self) -> &[OutputName] {
        &self.outputs_to_install
    }

    /// Returns Nix's authoritative package name.
    #[must_use]
    pub fn package_name(&self) -> &str {
        &self.package_name
    }

    /// Returns Nix's authoritative package version.
    #[must_use]
    pub const fn package_version(&self) -> &PackageVersion {
        &self.package_version
    }

    /// Returns fresh post-execution metadata for every selected output.
    #[must_use]
    pub fn acquired(&self) -> &[InstallOutputEvidence] {
        &self.acquired
    }

    fn validate(&self) -> Result<(), BuildEngineError> {
        if self.package_name.is_empty()
            || self.package_name.len() > MAX_TEXT
            || self.package_name.chars().any(char::is_control)
            || self.root_outputs.is_empty()
            || self.root_outputs.len() > 1024
            || self.outputs_to_install.is_empty()
            || self.outputs_to_install.len() > 1024
            || self.acquired.len() != self.outputs_to_install.len()
            || self
                .outputs_to_install
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        for (index, output_name) in self.outputs_to_install.iter().enumerate() {
            let expected = self
                .root_outputs
                .get(output_name)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            let acquired = &self.acquired[index];
            if acquired.output_name != *output_name || acquired.info.store_path() != expected {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
            if acquired.provenance == BuildOutputProvenance::CacheSigned
                && acquired.info.signatures().is_empty()
            {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
        }
        if let Some(requested) = self.output_selection.explicit_outputs()
            && !same_output_selection(requested, &self.outputs_to_install)
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Ok(())
    }
}

/// Fresh integrity and provenance evidence for one selected output.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallOutputEvidence {
    output_name: OutputName,
    info: PathInfoReport,
    provenance: BuildOutputProvenance,
}

impl InstallOutputEvidence {
    /// Returns the selected output name.
    #[must_use]
    pub const fn output_name(&self) -> &OutputName {
        &self.output_name
    }

    /// Returns freshly queried, validated store metadata.
    #[must_use]
    pub const fn path_info(&self) -> &PathInfoReport {
        &self.info
    }

    /// Returns whether Nix substituted or built this output.
    #[must_use]
    pub const fn provenance(&self) -> BuildOutputProvenance {
        self.provenance
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallEvidenceWire {
    schema_version: u32,
    descriptor_hash: String,
    channel_sequence: u64,
    policy_version: u64,
    revision: String,
    source_nar_hash: String,
    system: String,
    targets: Vec<InstallTargetEvidenceWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallTargetEvidenceWire {
    selector_id: String,
    selector: String,
    attribute: String,
    version_preference: CanonicalVersionPreference,
    requested_outputs: Option<Vec<String>>,
    source_revision: String,
    root_derivation: String,
    root_outputs: Vec<InstallRootOutputWire>,
    outputs_to_install: Vec<String>,
    package_name: String,
    package_version: String,
    acquired: Vec<InstallOutputEvidenceWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallRootOutputWire {
    name: String,
    store_path: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallOutputEvidenceWire {
    output_name: String,
    store_path: String,
    nar_hash: String,
    signatures: Vec<String>,
    references: Vec<String>,
    deriver: Option<String>,
    nar_size: u64,
    closure_size: u64,
    provenance: InstallOutputProvenanceWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum InstallOutputProvenanceWire {
    CacheSigned,
    LocalBuild,
}

impl From<&InstallEvidence> for InstallEvidenceWire {
    fn from(value: &InstallEvidence) -> Self {
        Self {
            schema_version: 1,
            descriptor_hash: value.descriptor_hash.to_string(),
            channel_sequence: value.channel_sequence.get().get(),
            policy_version: value.policy_version.get().get(),
            revision: value.revision.as_str().to_owned(),
            source_nar_hash: value.source_nar_hash.as_str().to_owned(),
            system: value.system.as_str().to_owned(),
            targets: value
                .targets
                .iter()
                .map(InstallTargetEvidenceWire::from)
                .collect(),
        }
    }
}

impl From<&InstallTargetEvidence> for InstallTargetEvidenceWire {
    fn from(value: &InstallTargetEvidence) -> Self {
        Self {
            selector_id: value.selector_id.as_str().to_owned(),
            selector: value.selector.as_str().to_owned(),
            attribute: value.attribute.as_str().to_owned(),
            version_preference: canonical_version_preference(&value.version_preference),
            requested_outputs: value.output_selection.explicit_outputs().map(|outputs| {
                outputs
                    .iter()
                    .map(OutputName::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
            source_revision: value.source_revision.to_canonical_string(),
            root_derivation: value.root_derivation.as_str().to_owned(),
            root_outputs: value
                .root_outputs
                .iter()
                .map(|(name, store_path)| InstallRootOutputWire {
                    name: name.as_str().to_owned(),
                    store_path: store_path.as_str().to_owned(),
                })
                .collect(),
            outputs_to_install: value
                .outputs_to_install
                .iter()
                .map(OutputName::as_str)
                .map(str::to_owned)
                .collect(),
            package_name: value.package_name.clone(),
            package_version: value.package_version.as_str().to_owned(),
            acquired: value
                .acquired
                .iter()
                .map(InstallOutputEvidenceWire::from)
                .collect(),
        }
    }
}

impl From<&InstallOutputEvidence> for InstallOutputEvidenceWire {
    fn from(value: &InstallOutputEvidence) -> Self {
        Self {
            output_name: value.output_name.as_str().to_owned(),
            store_path: value.info.store_path().as_str().to_owned(),
            nar_hash: value.info.nar_hash().as_str().to_owned(),
            signatures: value
                .info
                .signatures()
                .iter()
                .map(Signature::as_str)
                .map(str::to_owned)
                .collect(),
            references: value
                .info
                .references()
                .iter()
                .map(StorePath::as_str)
                .map(str::to_owned)
                .collect(),
            deriver: value
                .info
                .deriver()
                .map(DerivationPath::as_str)
                .map(str::to_owned),
            nar_size: value.info.nar_size(),
            closure_size: value.info.closure_size(),
            provenance: match value.provenance {
                BuildOutputProvenance::CacheSigned => InstallOutputProvenanceWire::CacheSigned,
                BuildOutputProvenance::LocalBuild => InstallOutputProvenanceWire::LocalBuild,
            },
        }
    }
}

impl TryFrom<InstallEvidenceWire> for InstallEvidence {
    type Error = BuildEngineError;

    fn try_from(value: InstallEvidenceWire) -> Result<Self, Self::Error> {
        if value.schema_version != 1 {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Self::new(
            Digest::from_str(&value.descriptor_hash)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            ChannelSequence::from_u64(value.channel_sequence)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            PolicyVersion::from_u64(value.policy_version)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            NixpkgsRevision::new(&value.revision)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            NarHash::new(&value.source_nar_hash)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            System::from_str(&value.system)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            value
                .targets
                .into_iter()
                .map(InstallTargetEvidence::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl TryFrom<InstallTargetEvidenceWire> for InstallTargetEvidence {
    type Error = BuildEngineError;

    fn try_from(value: InstallTargetEvidenceWire) -> Result<Self, Self::Error> {
        let mut root_outputs = BTreeMap::new();
        for output in value.root_outputs {
            let name = OutputName::new(&output.name)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            let path = StorePath::new(&output.store_path)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
            if root_outputs.insert(name, path).is_some() {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
        }
        let target = Self {
            selector_id: SelectorId::new(&value.selector_id)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            selector: SelectorInput::new(&value.selector)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            attribute: AttributePath::new(&value.attribute)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            version_preference: version_preference_from_canonical(value.version_preference)?,
            output_selection: match value.requested_outputs {
                None => OutputSelection::default_selection(),
                Some(outputs) => OutputSelection::explicit(
                    outputs
                        .into_iter()
                        .map(|name| {
                            OutputName::new(&name).map_err(|_| {
                                BuildEngineError::new(BuildEngineErrorCode::InvalidPlan)
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            },
            source_revision: SourceRevision::from_str(&value.source_revision)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            root_derivation: DerivationPath::from_str(&value.root_derivation)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            root_outputs,
            outputs_to_install: value
                .outputs_to_install
                .into_iter()
                .map(|name| {
                    OutputName::new(&name)
                        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
                })
                .collect::<Result<Vec<_>, _>>()?,
            package_name: value.package_name,
            package_version: PackageVersion::new(value.package_version),
            acquired: value
                .acquired
                .into_iter()
                .map(InstallOutputEvidence::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        };
        target.validate()?;
        Ok(target)
    }
}

impl TryFrom<InstallOutputEvidenceWire> for InstallOutputEvidence {
    type Error = BuildEngineError;

    fn try_from(value: InstallOutputEvidenceWire) -> Result<Self, Self::Error> {
        let info = PathInfoReport::new(
            StorePath::new(&value.store_path)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            NarHash::new(&value.nar_hash)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            value
                .signatures
                .into_iter()
                .map(|signature| {
                    Signature::new(&signature)
                        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
                })
                .collect::<Result<Vec<_>, _>>()?,
            value
                .references
                .into_iter()
                .map(|reference| {
                    StorePath::new(&reference)
                        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
                })
                .collect::<Result<Vec<_>, _>>()?,
            value
                .deriver
                .map(|deriver| {
                    DerivationPath::from_str(&deriver)
                        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
                })
                .transpose()?,
            value.nar_size,
            value.closure_size,
        )
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        Ok(Self {
            output_name: OutputName::new(&value.output_name)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?,
            info,
            provenance: match value.provenance {
                InstallOutputProvenanceWire::CacheSigned => BuildOutputProvenance::CacheSigned,
                InstallOutputProvenanceWire::LocalBuild => BuildOutputProvenance::LocalBuild,
            },
        })
    }
}

fn parse_canonical_digest(value: &str) -> Result<Digest, BuildEngineError> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
    Digest::from_str(&format!("sha256-{hex}"))
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
}

fn version_preference_from_canonical(
    value: CanonicalVersionPreference,
) -> Result<VersionPreference, BuildEngineError> {
    let bound = |value: CanonicalVersionBound| {
        let version = PackageVersion::new(value.version);
        if value.inclusive {
            VersionBound::inclusive(version)
        } else {
            VersionBound::exclusive(version)
        }
    };
    match value {
        CanonicalVersionPreference::Any => Ok(VersionPreference::Any),
        CanonicalVersionPreference::Exact { version } => {
            Ok(VersionPreference::Exact(PackageVersion::new(version)))
        }
        CanonicalVersionPreference::Minimum { version } => {
            Ok(VersionPreference::Minimum(PackageVersion::new(version)))
        }
        CanonicalVersionPreference::Range { lower, upper } => {
            VersionRange::new(lower.map(&bound), upper.map(bound))
                .map(VersionPreference::Range)
                .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
        }
    }
}

fn checked_text(value: &str) -> Result<String, BuildEngineError> {
    if value.is_empty() || value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    } else {
        Ok(value.to_owned())
    }
}

fn same_output_selection(left: &[OutputName], right: &[OutputName]) -> bool {
    left.len() == right.len() && left.iter().all(|name| right.contains(name))
}

fn digest_string(digest: Digest) -> String {
    let encoded = digest.to_string();
    format!("sha256:{}", encoded.trim_start_matches("sha256-"))
}

fn canonical_version_preference(value: &VersionPreference) -> CanonicalVersionPreference {
    match value {
        VersionPreference::Any => CanonicalVersionPreference::Any,
        VersionPreference::Exact(version) => CanonicalVersionPreference::Exact {
            version: version.as_str().to_owned(),
        },
        VersionPreference::Minimum(version) => CanonicalVersionPreference::Minimum {
            version: version.as_str().to_owned(),
        },
        VersionPreference::Range(range) => CanonicalVersionPreference::Range {
            lower: range.lower().map(|bound| CanonicalVersionBound {
                version: bound.version().as_str().to_owned(),
                inclusive: bound.is_inclusive(),
            }),
            upper: range.upper().map(|bound| CanonicalVersionBound {
                version: bound.version().as_str().to_owned(),
                inclusive: bound.is_inclusive(),
            }),
        },
    }
}

fn product_platform(system: System) -> (&'static str, &'static str) {
    match system {
        System::X8664Linux => ("linux", "x86_64"),
        System::Aarch64Linux => ("linux", "arm64"),
        System::X8664Darwin => ("macos", "x86_64"),
        System::Aarch64Darwin => ("macos", "arm64"),
    }
}

/// Public sanitized local-build preview; no store or derivation identity exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildPreview {
    schema_version: u32,
    platform: PreviewPlatform,
    policy_version: u64,
    build_plan_digest: String,
    targets: Vec<PreviewTarget>,
    build: PreviewBuild,
    cache: PreviewCache,
    unknown_local_outputs: u64,
    estimates: BuildPreviewEstimates,
    readiness: PreviewReadiness,
    approval_required: bool,
}

/// Heuristic public estimates measured outside the approval-bound plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildPreviewEstimates {
    approx_build_minutes: Option<String>,
    approx_new_disk_bytes: Option<u64>,
    approx_total_closure_bytes: Option<u64>,
}

impl BuildPreviewEstimates {
    /// Returns an honest unknown estimate set when no history is available.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            approx_build_minutes: None,
            approx_new_disk_bytes: None,
            approx_total_closure_bytes: None,
        }
    }

    /// Validates heuristic values supplied by the broker's estimator.
    pub fn new(
        approx_build_minutes: Option<&str>,
        approx_new_disk_bytes: Option<u64>,
        approx_total_closure_bytes: Option<u64>,
    ) -> Result<Self, BuildEngineError> {
        let estimates = Self {
            approx_build_minutes: approx_build_minutes.map(checked_text).transpose()?,
            approx_new_disk_bytes,
            approx_total_closure_bytes,
        };
        estimates.validate()?;
        Ok(estimates)
    }

    pub(crate) const fn execution_disk_estimate(&self) -> Option<VolatileBuildEstimate> {
        match self.approx_new_disk_bytes {
            Some(estimated_new_bytes) => Some(VolatileBuildEstimate {
                estimated_new_bytes,
            }),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewTarget {
    selector: String,
    package_name: String,
    version: String,
    outputs_to_install: Vec<String>,
    local_build_required: bool,
}

impl BuildPreview {
    /// Returns the private plan digest pointer displayed to the user.
    #[must_use]
    pub fn build_plan_digest(&self) -> &str {
        &self.build_plan_digest
    }

    /// Returns product-owned identities for targets that need local building.
    ///
    /// The iterator cannot expose derivation paths, store paths, or Nix
    /// implementation details. Its values are part of the approval-bound,
    /// sanitized preview.
    pub fn local_build_targets(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.targets
            .iter()
            .filter(|target| target.local_build_required)
            .map(|target| {
                (
                    target.selector.as_str(),
                    target.package_name.as_str(),
                    target.version.as_str(),
                )
            })
    }

    /// Serializes this allowlisted public object for CLI/RPC rendering.
    pub fn to_json_value(&self) -> Result<serde_json::Value, BuildEngineError> {
        serde_json::to_value(self)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }

    /// Serializes the strict sanitized preview for the private product frame.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, BuildEngineError> {
        serde_json::to_vec(self)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }

    /// Decodes and revalidates one strict sanitized preview from the broker.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, BuildEngineError> {
        let preview: Self = serde_json::from_slice(bytes)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        preview.validate()?;
        Ok(preview)
    }

    pub(crate) fn validate(&self) -> Result<(), BuildEngineError> {
        if self.schema_version != 1
            || self.policy_version == 0
            || !is_public_digest(&self.build_plan_digest)
            || self.targets.is_empty()
            || self.targets.len() > MAX_PREVIEW_ITEMS
            || self.build.count == 0
            || self.build.count != self.build.names.len()
            || self.build.count > MAX_PREVIEW_ITEMS
            || self.unknown_local_outputs == 0
            || !self
                .targets
                .iter()
                .any(|target| target.local_build_required)
            || !self.approval_required
            || !matches!(
                (self.platform.os.as_str(), self.platform.arch.as_str()),
                ("linux" | "macos", "x86_64" | "arm64")
            )
            || !self.readiness.sandboxed
            || !self.readiness.build_isolation_ready
            || !self.readiness.native_build
            || self.readiness.resource_boundary.isolation != "sandbox"
            || self.readiness.resource_boundary.per_build_resource_cap
            || self.readiness.resource_boundary.notice != RESOURCE_NOTICE
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        self.estimates.validate()?;
        for target in &self.targets {
            checked_text(&target.selector)?;
            checked_text(&target.package_name)?;
            checked_text(&target.version)?;
            if target.outputs_to_install.is_empty()
                || target.outputs_to_install.len() > MAX_PREVIEW_ITEMS
                || target
                    .outputs_to_install
                    .iter()
                    .any(|output| OutputName::new(output).is_err())
                || target
                    .outputs_to_install
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != target.outputs_to_install.len()
            {
                return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
            }
        }
        if self
            .build
            .names
            .iter()
            .any(|name| checked_text(name).is_err())
        {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        Ok(())
    }
}

impl BuildPreviewEstimates {
    fn validate(&self) -> Result<(), BuildEngineError> {
        if self.approx_new_disk_bytes == Some(0) {
            return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
        }
        self.approx_build_minutes
            .as_deref()
            .map(checked_text)
            .transpose()?;
        Ok(())
    }
}

fn is_public_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewPlatform {
    os: String,
    arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewBuild {
    count: usize,
    names: Vec<String>,
    has_fixed_output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewCache {
    known_download_bytes: u64,
    known_content_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewReadiness {
    sandboxed: bool,
    build_isolation_ready: bool,
    native_build: bool,
    resource_boundary: PreviewResourceBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewResourceBoundary {
    isolation: String,
    per_build_resource_cap: bool,
    notice: String,
}

/// One-operation approval source recorded durably before issuance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalSource {
    /// User answered the interactive prompt.
    Interactive,
    /// Global `--yes` pre-approved this operation only.
    AssumeYes,
}

impl ApprovalSource {
    /// Returns the stable allowlisted journal value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::AssumeYes => "yes",
        }
    }
}

/// Allowlisted approval row passed to the authoritative operation journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalJournalRecord {
    operation_id: OperationId,
    build_plan_digest: Digest,
    policy_version: PolicyVersion,
    source: ApprovalSource,
    timestamp: String,
}

impl ApprovalJournalRecord {
    /// Returns the opaque operation id.
    #[must_use]
    pub const fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    /// Returns the exact approved plan digest.
    #[must_use]
    pub const fn build_plan_digest(&self) -> Digest {
        self.build_plan_digest
    }

    /// Returns the authenticated policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the explicit approval source.
    #[must_use]
    pub const fn source(&self) -> ApprovalSource {
        self.source
    }

    /// Returns the caller-supplied validated audit timestamp.
    #[must_use]
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }
}

/// Closed persistence seam for the authoritative append-only journal.
pub trait ApprovalJournal: Send + Sync {
    /// Durably records approval before a receipt may be issued.
    fn record(&self, record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError>;
}

/// Redacted persistence failure returned by approval journal implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalJournalError;

impl ApprovalJournalError {
    /// Constructs the only public journal failure value.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for ApprovalJournalError {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ApprovalJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("approval journal failed")
    }
}

impl std::error::Error for ApprovalJournalError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalGrant {
    digest: Digest,
    policy_version: PolicyVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApprovalState {
    Recording {
        grant: ApprovalGrant,
        reservation: u64,
    },
    Approved(ApprovalGrant),
}

#[derive(Debug, Default)]
struct AdmissionState {
    next_ticket: u64,
    serving_ticket: u64,
    held: bool,
    cancelled: BTreeSet<u64>,
}

#[derive(Debug, Default)]
struct BuildAdmission {
    state: Mutex<AdmissionState>,
    changed: Condvar,
}

impl BuildAdmission {
    fn acquire(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<BuildPermit<'_>, BuildEngineError> {
        let ticket = {
            let mut state = lock_recover(&self.state);
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::ApprovalUnavailable))?;
            ticket
        };
        let mut state = lock_recover(&self.state);
        loop {
            advance_cancelled(&mut state);
            if cancellation.is_cancelled() {
                state.cancelled.insert(ticket);
                advance_cancelled(&mut state);
                self.changed.notify_all();
                return Err(BuildEngineError::new(BuildEngineErrorCode::Cancelled));
            }
            if !state.held && state.serving_ticket == ticket {
                state.held = true;
                return Ok(BuildPermit { admission: self });
            }
            state = self
                .changed
                .wait_timeout(state, ADMISSION_WAIT_POLL)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

fn advance_cancelled(state: &mut AdmissionState) {
    while state.cancelled.remove(&state.serving_ticket) {
        state.serving_ticket = state.serving_ticket.saturating_add(1);
    }
}

#[derive(Debug)]
struct BuildPermit<'a> {
    admission: &'a BuildAdmission,
}

impl Drop for BuildPermit<'_> {
    fn drop(&mut self) {
        let mut state = lock_recover(&self.admission.state);
        state.held = false;
        state.serving_ticket = state.serving_ticket.saturating_add(1);
        advance_cancelled(&mut state);
        self.admission.changed.notify_all();
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Cooperative cancellation checked while waiting for admission.
#[derive(Debug, Default)]
pub struct CancellationToken {
    cancelled: AtomicBool,
}

impl CancellationToken {
    /// Requests cancellation; a queued operation exits without building.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Volatile resource measurements taken only after build admission.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResourceSnapshot {
    /// Free bytes available on the managed store filesystem.
    pub free_bytes: u64,
    /// Current system load average.
    pub load_average: f64,
}

/// Measurement seam; production probes `/nix` and host load, tests stay hermetic.
pub trait ResourceProbe: Send + Sync {
    /// Returns one fresh volatile measurement.
    fn measure(&self) -> Result<ResourceSnapshot, BuildEngineError>;
}

/// Production resource probe for the fixed managed `/nix` filesystem.
///
/// Dynamic measurements are deliberately excluded from the approval digest
/// and are sampled only under broker build admission.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostResourceProbe;

impl HostResourceProbe {
    /// Constructs the fixed-path host probe.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ResourceProbe for HostResourceProbe {
    fn measure(&self) -> Result<ResourceSnapshot, BuildEngineError> {
        Ok(ResourceSnapshot {
            free_bytes: available_bytes(Path::new("/nix"))?,
            load_average: host_load_average()?,
        })
    }
}

fn available_bytes(path: &Path) -> Result<u64, BuildEngineError> {
    let statistics = rustix::fs::statvfs(path)
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))?;
    statistics
        .f_bavail
        .checked_mul(statistics.f_frsize)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))
}

#[cfg(target_os = "linux")]
fn host_load_average() -> Result<f64, BuildEngineError> {
    let text = std::fs::read_to_string("/proc/loadavg")
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))?;
    parse_load_average(&text)
}

#[cfg(target_os = "macos")]
fn host_load_average() -> Result<f64, BuildEngineError> {
    let output = Command::new("/usr/sbin/sysctl")
        .args(["-n", "vm.loadavg"])
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))?;
    if !output.status.success() || output.stdout.len() > 1024 {
        return Err(BuildEngineError::new(
            BuildEngineErrorCode::ResourcePreflightFailed,
        ));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))?;
    parse_load_average(text)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn host_load_average() -> Result<f64, BuildEngineError> {
    Err(BuildEngineError::new(
        BuildEngineErrorCode::ResourcePreflightFailed,
    ))
}

fn parse_load_average(text: &str) -> Result<f64, BuildEngineError> {
    let value = text
        .trim()
        .trim_start_matches('{')
        .split_ascii_whitespace()
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))?;
    Ok(value)
}

/// Heuristic size input deliberately excluded from the approval digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolatileBuildEstimate {
    estimated_new_bytes: u64,
}

impl VolatileBuildEstimate {
    /// Constructs an explicitly heuristic local-output estimate.
    #[must_use]
    pub const fn new(estimated_new_bytes: u64) -> Self {
        Self {
            estimated_new_bytes,
        }
    }
}

/// Shared V1 build engine: plan approval, fair admission, revalidation, build.
#[derive(Debug, Default)]
pub struct LocalBuildEngine {
    approvals: Mutex<BTreeMap<OperationId, ApprovalState>>,
    next_approval_reservation: AtomicU64,
    admission: BuildAdmission,
}

pub(crate) struct BuildExecutionRuntime<'a> {
    pub(crate) resources: &'a dyn ResourceProbe,
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) adapter: &'a dyn NixAdapter,
    pub(crate) progress: &'a mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
}

impl LocalBuildEngine {
    /// Creates an empty engine; approvals never survive process restart.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Journals and issues one receipt bound to exactly this operation and plan.
    pub fn approve(
        &self,
        operation_id: OperationId,
        plan: &BuildPlan,
        source: ApprovalSource,
        timestamp: &str,
        journal: &dyn ApprovalJournal,
    ) -> Result<BuildApprovalReceipt, BuildEngineError> {
        let digest = plan.digest()?;
        self.approve_subject(
            operation_id,
            digest,
            plan.policy_identity,
            source,
            timestamp,
            journal,
        )
    }

    pub(crate) fn approve_subject(
        &self,
        operation_id: OperationId,
        digest: Digest,
        policy_version: PolicyVersion,
        source: ApprovalSource,
        timestamp: &str,
        journal: &dyn ApprovalJournal,
    ) -> Result<BuildApprovalReceipt, BuildEngineError> {
        let timestamp = checked_text(timestamp)?;
        let record = ApprovalJournalRecord {
            operation_id: operation_id.clone(),
            build_plan_digest: digest,
            policy_version,
            source,
            timestamp,
        };
        let grant = ApprovalGrant {
            digest,
            policy_version,
        };
        let reservation = self
            .next_approval_reservation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::ApprovalUnavailable))?;
        let recording = ApprovalState::Recording {
            grant: grant.clone(),
            reservation,
        };
        {
            let mut approvals = lock_recover(&self.approvals);
            if approvals.contains_key(&operation_id) {
                return Err(BuildEngineError::new(
                    BuildEngineErrorCode::ApprovalUnavailable,
                ));
            }
            approvals.insert(operation_id.clone(), recording.clone());
        }
        if journal.record(&record).is_err() {
            let mut approvals = lock_recover(&self.approvals);
            if approvals.get(&operation_id) == Some(&recording) {
                approvals.remove(&operation_id);
            }
            return Err(BuildEngineError::new(BuildEngineErrorCode::JournalFailed));
        }
        let mut approvals = lock_recover(&self.approvals);
        if approvals.get(&operation_id) != Some(&recording) {
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalUnavailable,
            ));
        }
        approvals.insert(operation_id.clone(), ApprovalState::Approved(grant));
        drop(approvals);
        Ok(BuildApprovalReceipt::new(
            operation_id,
            digest,
            policy_version,
        ))
    }

    pub(crate) fn consume_subject(
        &self,
        receipt: &BuildApprovalReceipt,
        digest: Digest,
        policy_version: PolicyVersion,
    ) -> Result<(), BuildEngineError> {
        if receipt.build_plan_digest() != digest || receipt.policy_version() != policy_version {
            self.revoke(receipt.operation_id());
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalInvalidated,
            ));
        }
        let expected = ApprovalState::Approved(ApprovalGrant {
            digest,
            policy_version,
        });
        if lock_recover(&self.approvals).remove(receipt.operation_id()) != Some(expected) {
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalRequired,
            ));
        }
        Ok(())
    }

    /// Revokes an unconsumed approval when its broker operation ends.
    ///
    /// Disconnect, explicit cancellation, and operation expiry must call this
    /// so an abandoned receipt cannot remain live inside the singleton broker.
    pub fn cancel_approval(&self, operation_id: &OperationId) -> bool {
        lock_recover(&self.approvals).remove(operation_id).is_some()
    }

    #[cfg(test)]
    pub(crate) fn approval_count(&self) -> usize {
        lock_recover(&self.approvals).len()
    }

    /// Executes only after fair admission, exact replan, and two-shot resources.
    ///
    /// The broker must hold its GC-inhibit permit from before this call until
    /// every returned output has been committed to an authoritative root. This
    /// engine deliberately owns build admission, not root-transaction lifetime.
    pub fn execute(
        &self,
        receipt: BuildApprovalReceipt,
        replan: impl FnOnce() -> Result<BuildPlan, BuildEngineError>,
        estimate: VolatileBuildEstimate,
        resources: &dyn ResourceProbe,
        cancellation: &CancellationToken,
        adapter: &dyn NixAdapter,
    ) -> Result<BuildReport, BuildEngineError> {
        let mut no_progress = |_| Ok(());
        self.execute_revalidated(
            receipt,
            replan,
            estimate,
            BuildExecutionRuntime {
                resources,
                cancellation,
                adapter,
                progress: &mut no_progress,
            },
        )
        .map(|(report, _)| report)
    }

    pub(crate) fn execute_with_evidence_and_progress(
        &self,
        receipt: BuildApprovalReceipt,
        replan: impl FnOnce() -> Result<BuildPlan, BuildEngineError>,
        estimate: VolatileBuildEstimate,
        runtime: BuildExecutionRuntime<'_>,
    ) -> Result<(BuildReport, InstallEvidence), BuildEngineError> {
        let adapter = runtime.adapter;
        let (report, plan) = self.execute_revalidated(receipt, replan, estimate, runtime)?;
        let evidence = InstallEvidence::from_executed_plan(&plan, &report, adapter)?;
        Ok((report, evidence))
    }

    fn execute_revalidated(
        &self,
        receipt: BuildApprovalReceipt,
        replan: impl FnOnce() -> Result<BuildPlan, BuildEngineError>,
        estimate: VolatileBuildEstimate,
        runtime: BuildExecutionRuntime<'_>,
    ) -> Result<(BuildReport, BuildPlan), BuildEngineError> {
        let _permit = match self.admission.acquire(runtime.cancellation) {
            Ok(permit) => permit,
            Err(error) => {
                self.revoke(receipt.operation_id());
                return Err(error);
            }
        };
        let current = match replan() {
            Ok(plan) => plan,
            Err(error) => {
                self.revoke(receipt.operation_id());
                return Err(error);
            }
        };
        let current_digest = current.digest()?;
        let expected_grant = ApprovalGrant {
            digest: receipt.build_plan_digest(),
            policy_version: receipt.policy_version(),
        };
        if current_digest != receipt.build_plan_digest()
            || current.policy_identity != receipt.policy_version()
        {
            self.revoke(receipt.operation_id());
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalInvalidated,
            ));
        }
        {
            let approvals = lock_recover(&self.approvals);
            if approvals.get(receipt.operation_id())
                != Some(&ApprovalState::Approved(expected_grant.clone()))
            {
                return Err(BuildEngineError::new(
                    BuildEngineErrorCode::ApprovalRequired,
                ));
            }
        }
        let first_measurement = match runtime.resources.measure() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.revoke(receipt.operation_id());
                return Err(error);
            }
        };
        let resources_ready = if resource_ok(&current, estimate, first_measurement) {
            true
        } else {
            match runtime.resources.measure() {
                Ok(snapshot) => resource_ok(&current, estimate, snapshot),
                Err(error) => {
                    self.revoke(receipt.operation_id());
                    return Err(error);
                }
            }
        };
        if !resources_ready {
            self.revoke(receipt.operation_id());
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ResourcePreflightFailed,
            ));
        }
        let removed = lock_recover(&self.approvals).remove(receipt.operation_id());
        if removed != Some(ApprovalState::Approved(expected_grant)) {
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalUnavailable,
            ));
        }
        let request = BuildRequest::new(
            current.execution.targets.clone(),
            current.system_identity,
            receipt,
        )
        .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))?;
        let report = runtime
            .adapter
            .build_with_progress(&request, runtime.progress)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::BuildFailed))?;
        if report.status() == BuildStatus::AcquireNoBinary {
            return Err(BuildEngineError::new(BuildEngineErrorCode::AcquireNoBinary));
        }
        let actual = report
            .outputs()
            .iter()
            .map(|output| output.store_path().as_str().to_owned())
            .collect::<BTreeSet<_>>();
        if actual != current.execution.expected_outputs {
            return Err(BuildEngineError::new(BuildEngineErrorCode::BuildFailed));
        }
        Ok((report, current))
    }

    fn revoke(&self, operation_id: &OperationId) {
        lock_recover(&self.approvals).remove(operation_id);
    }
}

fn resource_ok(
    plan: &BuildPlan,
    estimate: VolatileBuildEstimate,
    snapshot: ResourceSnapshot,
) -> bool {
    if !snapshot.load_average.is_finite() || snapshot.load_average < 0.0 {
        return false;
    }
    let required = estimate
        .estimated_new_bytes
        .checked_mul(plan.admission.disk_headroom_percent)
        .and_then(|value| value.checked_add(99))
        .map(|value| value / 100);
    required.is_some_and(|required| snapshot.free_bytes >= required)
        && snapshot.load_average <= plan.admission.max_loadavg_ceiling as f64
}

/// Renders exact immutable V1 Nix settings for the managed daemon.
pub fn render_managed_build_nix_conf(
    system: System,
    cache: &CachePolicy,
) -> Result<String, BuildEngineError> {
    if cache.trusted_public_keys().is_empty() {
        return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
    }
    render_managed_build_nix_conf_from_parts(
        system,
        cache.url(),
        cache.trusted_public_keys().iter().map(|key| key.as_str()),
    )
}

fn render_managed_build_nix_conf_from_parts<'a>(
    system: System,
    cache_url: &str,
    trusted_keys: impl Iterator<Item = &'a str>,
) -> Result<String, BuildEngineError> {
    let keys = trusted_keys.collect::<Vec<_>>();
    if cache_url != "https://cache.nixos.org" || keys.is_empty() {
        return Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan));
    }
    let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
    let experimental = if linux {
        "nix-command flakes cgroups"
    } else {
        "nix-command flakes"
    };
    let keys = keys.join(" ");
    let mut lines = vec![
        format!("build-users-group = {BUILD_USERS_GROUP}"),
        "trusted-users = root".to_owned(),
        "allowed-users = pkg-nix-broker".to_owned(),
        format!("experimental-features = {experimental}"),
        "sandbox = true".to_owned(),
        "sandbox-fallback = false".to_owned(),
        "allow-import-from-derivation = false".to_owned(),
        "require-sigs = true".to_owned(),
        "builders =".to_owned(),
        format!("substituters = {cache_url}"),
        format!("trusted-public-keys = {keys}"),
        "connect-timeout = 10".to_owned(),
        "max-substitution-jobs = 4".to_owned(),
        format!("max-jobs = {MAX_JOBS}"),
        format!("cores = {CORES_HINT}"),
        format!("max-silent-time = {MAX_SILENT_SECONDS}"),
        format!("timeout = {TIMEOUT_SECONDS}"),
        format!("max-build-log-size = {MAX_LOG_BYTES}"),
    ];
    if linux {
        lines.insert(7, "use-cgroups = true".to_owned());
    }
    Ok(format!("{}\n", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use pkg_core::{PackageVersion, state::body_digest};

    use crate::{
        BuildOutput, BuildOutputProvenance, BuildReport, BuildRequest, BuildStatus, DerivationPath,
        EvaluateDerivationRequest, EvaluatedDerivation, GcReport, NixAdapterError, PathInfoReport,
        StorePath, SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
    };

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    fn derivation() -> DerivationPath {
        DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello-1.0.drv")).unwrap()
    }

    fn output() -> StorePath {
        StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap()
    }

    fn try_plan_with_mode(
        document_byte: u8,
        system: System,
        readiness: BuildReadiness,
        build_mode: BuildMode,
    ) -> Result<BuildPlan, BuildEngineError> {
        let derivation = derivation();
        let mut outputs = BTreeMap::new();
        outputs.insert(OutputName::new("out").unwrap(), output());
        let evaluated = EvaluatedDerivation::new(
            derivation.clone(),
            "hello-1.0".to_owned(),
            system,
            outputs,
            Digest::from_bytes([document_byte; 32]),
            false,
        )
        .unwrap();
        let report = DerivationPlanReport::new(
            4,
            derivation.clone(),
            vec![OutputName::new("out").unwrap()],
            vec![evaluated],
            Digest::from_bytes([document_byte.wrapping_add(1); 32]),
            "hello".to_owned(),
            PackageVersion::new("1.0"),
        )
        .unwrap();
        BuildPlan::new(
            &NixVersion::new("2.34.8").unwrap(),
            Digest::from_bytes([3; 32]),
            PolicyVersion::from_u64(7).unwrap(),
            ChannelSequence::from_u64(42).unwrap(),
            &NixpkgsRevision::new(REVISION).unwrap(),
            &NarHash::new(NAR_HASH).unwrap(),
            system,
            system,
            build_mode,
            vec![BuildPlanTarget::new(
                SelectorId::new("sel_hello").unwrap(),
                SelectorInput::new("hello").unwrap(),
                AttributePath::new("hello").unwrap(),
                VersionPreference::Any,
                OutputSelection::default_selection(),
                SourceRevision::CurrentChannel,
                report,
            )],
            vec![derivation],
            CacheClassification::new(Digest::from_bytes([4; 32]), 2, 1, 100, 200).unwrap(),
            readiness,
            4,
        )
    }

    fn try_plan(
        document_byte: u8,
        system: System,
        readiness: BuildReadiness,
    ) -> Result<BuildPlan, BuildEngineError> {
        try_plan_with_mode(document_byte, system, readiness, BuildMode::AllowWithGates)
    }

    fn plan(document_byte: u8, system: System, readiness: BuildReadiness) -> BuildPlan {
        try_plan(document_byte, system, readiness).unwrap()
    }

    fn linux_readiness() -> BuildReadiness {
        BuildReadiness::new(true, false, true, true, true)
    }

    #[derive(Default)]
    struct Journal {
        rows: Mutex<Vec<ApprovalJournalRecord>>,
    }

    impl ApprovalJournal for Journal {
        fn record(&self, record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError> {
            lock_recover(&self.rows).push(record.clone());
            Ok(())
        }
    }

    struct BlockingJournal {
        rows: Mutex<Vec<ApprovalJournalRecord>>,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl ApprovalJournal for BlockingJournal {
        fn record(&self, record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError> {
            lock_recover(&self.rows).push(record.clone());
            self.entered
                .send(())
                .map_err(|_| ApprovalJournalError::new())?;
            lock_recover(&self.release)
                .recv()
                .map_err(|_| ApprovalJournalError::new())
        }
    }

    struct Probe {
        values: Mutex<VecDeque<ResourceSnapshot>>,
        calls: AtomicUsize,
    }

    impl Probe {
        fn new(values: impl IntoIterator<Item = ResourceSnapshot>) -> Self {
            Self {
                values: Mutex::new(values.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl ResourceProbe for Probe {
        fn measure(&self) -> Result<ResourceSnapshot, BuildEngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            lock_recover(&self.values)
                .pop_front()
                .ok_or_else(|| BuildEngineError::new(BuildEngineErrorCode::ResourcePreflightFailed))
        }
    }

    struct BuildAdapter {
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        requests: Mutex<Vec<BuildRequest>>,
    }

    impl BuildAdapter {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl NixAdapter for BuildAdapter {
        fn version(&self) -> Result<VersionInfo, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
        fn evaluate_derivation(
            &self,
            _: &EvaluateDerivationRequest,
        ) -> Result<DerivationPlanReport, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
        fn path_info(&self, _: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
        fn substitute(&self, _: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
        fn build(&self, request: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            lock_recover(&self.requests).push(request.clone());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            self.active.fetch_sub(1, Ordering::SeqCst);
            BuildReport::new(
                BuildStatus::Built,
                vec![BuildOutput::new(
                    output(),
                    BuildOutputProvenance::LocalBuild,
                )],
            )
        }
        fn verify(&self, _: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
        fn gc(&self) -> Result<GcReport, NixAdapterError> {
            Err(NixAdapterError::Unavailable)
        }
    }

    fn good_snapshot() -> ResourceSnapshot {
        ResourceSnapshot {
            free_bytes: 10_000,
            load_average: 1.0,
        }
    }

    #[test]
    fn host_resource_probe_uses_safe_fixed_inputs_and_strict_load_parsing() {
        assert!(available_bytes(Path::new("/")).unwrap() > 0);
        assert_eq!(parse_load_average("1.25 0.50 0.25 1/2 3").unwrap(), 1.25);
        assert_eq!(parse_load_average("{ 2.5 1.0 0.5 }").unwrap(), 2.5);
        for invalid in ["", "{ }", "nan 1 1", "inf 1 1", "-1 1 1", "nope"] {
            assert_eq!(
                parse_load_average(invalid).unwrap_err().code(),
                BuildEngineErrorCode::ResourcePreflightFailed
            );
        }
    }

    #[test]
    fn plan_is_deterministic_mutation_sensitive_and_preview_is_public() {
        let first = plan(1, System::X8664Linux, linux_readiness());
        let same = plan(1, System::X8664Linux, linux_readiness());
        let changed = plan(2, System::X8664Linux, linux_readiness());
        assert_eq!(first.digest().unwrap(), same.digest().unwrap());
        assert_ne!(first.digest().unwrap(), changed.digest().unwrap());

        let preview_object = first.preview().unwrap();
        let preview_bytes = preview_object.to_json_bytes().unwrap();
        assert_eq!(
            BuildPreview::from_json_bytes(&preview_bytes).unwrap(),
            preview_object
        );
        let preview = String::from_utf8(preview_bytes).unwrap();
        assert!(preview.contains("\"approvalRequired\":true"));
        assert!(preview.contains("\"buildPlanDigest\":\"sha256:"));
        assert!(preview.contains("\"selector\":\"hello\""));
        assert!(preview.contains("\"localBuildRequired\":true"));
        assert!(preview.contains("\"perBuildResourceCap\":false"));
        assert!(preview.contains("\"approxBuildMinutes\":null"));
        assert_eq!(
            preview_object.local_build_targets().collect::<Vec<_>>(),
            vec![("hello", "hello", "1.0")]
        );
        for private in ["/nix/", ".drv", "x86_64-linux", "nixbld", "cgroup"] {
            assert!(
                !preview.contains(private),
                "preview leaked {private}: {preview}"
            );
        }

        let estimated = first
            .preview_with_estimates(
                BuildPreviewEstimates::new(Some("8-14"), Some(332_000_000), None).unwrap(),
            )
            .unwrap()
            .to_json_value()
            .unwrap()
            .to_string();
        assert!(estimated.contains("\"approxBuildMinutes\":\"8-14\""));
        assert!(estimated.contains("\"approxNewDiskBytes\":332000000"));
        assert_eq!(first.digest().unwrap(), same.digest().unwrap());
        assert!(BuildPreviewEstimates::new(Some("bad\nvalue"), None, None).is_err());
        assert!(BuildPreviewEstimates::new(None, Some(0), None).is_err());

        let mut zero_estimate = serde_json::to_value(&preview_object).unwrap();
        zero_estimate["estimates"]["approxNewDiskBytes"] = serde_json::json!(0);
        assert!(
            BuildPreview::from_json_bytes(&serde_json::to_vec(&zero_estimate).unwrap()).is_err()
        );

        let mut no_local_target = serde_json::to_value(&preview_object).unwrap();
        no_local_target["targets"][0]["localBuildRequired"] = serde_json::json!(false);
        assert!(
            BuildPreview::from_json_bytes(&serde_json::to_vec(&no_local_target).unwrap()).is_err()
        );

        let mut extended = serde_json::to_value(&preview_object).unwrap();
        extended
            .as_object_mut()
            .unwrap()
            .insert("privatePlan".to_owned(), serde_json::json!("forbidden"));
        assert!(BuildPreview::from_json_bytes(&serde_json::to_vec(&extended).unwrap()).is_err());
    }

    #[test]
    fn repair_plan_binds_every_deriver_output_and_preview_hides_store_identity() {
        let derivation =
            DerivationPath::from_str("/nix/store/00000000000000000000000000000000-demo.drv")
                .unwrap();
        let outputs = BTreeMap::from([
            (
                OutputName::new("man").unwrap(),
                StorePath::new("/nix/store/11111111111111111111111111111111-demo-man").unwrap(),
            ),
            (
                OutputName::new("out").unwrap(),
                StorePath::new("/nix/store/22222222222222222222222222222222-demo").unwrap(),
            ),
        ]);
        let input = RepairPlanTarget::new(
            StorePath::new("/nix/store/22222222222222222222222222222222-demo").unwrap(),
            RepairPlanDerivation::new(
                derivation,
                "demo-1.0".to_owned(),
                System::X8664Linux,
                outputs,
                body_digest(b"derivation document"),
                false,
            )
            .unwrap(),
        );
        let make = || {
            RepairBuildPlan::new(
                &NixVersion::new("2.34.8").unwrap(),
                PolicyVersion::from_u64(7).unwrap(),
                System::X8664Linux,
                linux_readiness(),
                8,
                vec![input.clone()],
            )
            .unwrap()
        };

        let first = make();
        assert_eq!(first.digest().unwrap(), make().digest().unwrap());
        let preview = first.preview().unwrap();
        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("\"man\""));
        assert!(json.contains("\"out\""));
        assert!(!json.contains("/nix/store"));
        assert!(!json.contains(".drv"));
        assert_eq!(preview.local_build_targets().count(), 1);
    }

    #[test]
    fn bootstrap_estimate_is_fixed_and_keeps_unknowns_honest() {
        let mut plan = plan(1, System::X8664Linux, linux_readiness());
        let estimate = plan.bootstrap_estimates().unwrap();
        assert_eq!(
            estimate.execution_disk_estimate(),
            Some(VolatileBuildEstimate::new(
                BOOTSTRAP_MISS_ALLOWANCE_BYTES + 200
            ))
        );
        let preview = plan
            .preview_with_estimates(estimate)
            .unwrap()
            .to_json_value()
            .unwrap();
        assert_eq!(
            preview["estimates"]["approxNewDiskBytes"],
            BOOTSTRAP_MISS_ALLOWANCE_BYTES + 200
        );
        assert!(preview["estimates"]["approxBuildMinutes"].is_null());
        assert!(preview["estimates"]["approxTotalClosureBytes"].is_null());
        assert_eq!(preview["unknownLocalOutputs"], 1);

        plan.cache_classification.misses = u64::MAX;
        assert_eq!(
            plan.bootstrap_estimates().unwrap_err().code(),
            BuildEngineErrorCode::InvalidPlan
        );
    }

    #[test]
    fn policy_and_readiness_refuse_before_approval() {
        let bad = BuildReadiness::new(true, false, true, false, false);
        assert_eq!(
            try_plan(1, System::X8664Linux, bad).unwrap_err().code(),
            BuildEngineErrorCode::ReadinessFailed
        );
        assert_eq!(
            try_plan_with_mode(1, System::X8664Linux, linux_readiness(), BuildMode::Deny,)
                .unwrap_err()
                .code(),
            BuildEngineErrorCode::BuildDenied
        );

        let valid = plan(
            1,
            System::Aarch64Darwin,
            BuildReadiness::new(true, false, true, false, false),
        );
        assert!(valid.digest().is_ok());
    }

    #[test]
    fn config_is_exact_cross_platform_and_never_claims_darwin_cgroups() {
        let key = "cache.nixos.org-1:AAAAAAAA";
        let linux = render_managed_build_nix_conf_from_parts(
            System::X8664Linux,
            "https://cache.nixos.org",
            [key].into_iter(),
        )
        .unwrap();
        let darwin = render_managed_build_nix_conf_from_parts(
            System::Aarch64Darwin,
            "https://cache.nixos.org",
            [key].into_iter(),
        )
        .unwrap();
        assert_eq!(
            linux,
            format!(
                "build-users-group = nixbld\ntrusted-users = root\nallowed-users = pkg-nix-broker\nexperimental-features = nix-command flakes cgroups\nsandbox = true\nsandbox-fallback = false\nallow-import-from-derivation = false\nuse-cgroups = true\nrequire-sigs = true\nbuilders =\nsubstituters = https://cache.nixos.org\ntrusted-public-keys = {key}\nconnect-timeout = 10\nmax-substitution-jobs = 4\nmax-jobs = 1\ncores = 0\nmax-silent-time = 3600\ntimeout = 86400\nmax-build-log-size = 268435456\n"
            )
        );
        assert_eq!(
            darwin,
            format!(
                "build-users-group = nixbld\ntrusted-users = root\nallowed-users = pkg-nix-broker\nexperimental-features = nix-command flakes\nsandbox = true\nsandbox-fallback = false\nallow-import-from-derivation = false\nrequire-sigs = true\nbuilders =\nsubstituters = https://cache.nixos.org\ntrusted-public-keys = {key}\nconnect-timeout = 10\nmax-substitution-jobs = 4\nmax-jobs = 1\ncores = 0\nmax-silent-time = 3600\ntimeout = 86400\nmax-build-log-size = 268435456\n"
            )
        );
        for required in [
            "sandbox = true",
            "sandbox-fallback = false",
            "max-jobs = 1",
            "cores = 0",
            "max-silent-time = 3600",
            "timeout = 86400",
            "max-build-log-size = 268435456",
            "allow-import-from-derivation = false",
            "builders =",
        ] {
            assert!(linux.contains(required));
            assert!(darwin.contains(required));
        }
        assert!(linux.contains("use-cgroups = true"));
        assert!(linux.contains("nix-command flakes cgroups"));
        assert!(!darwin.contains("use-cgroups"));
        assert!(!darwin.contains("cgroups"));
    }

    #[test]
    fn approval_is_journaled_once_consumed_once_and_binds_exact_request() {
        let engine = LocalBuildEngine::new();
        let journal = Journal::default();
        let plan = plan(1, System::X8664Linux, linux_readiness());
        let receipt = engine
            .approve(
                OperationId::new("op-one").unwrap(),
                &plan,
                ApprovalSource::AssumeYes,
                "2026-08-10T00:00:00Z",
                &journal,
            )
            .unwrap();
        let adapter = BuildAdapter::new();
        let report = engine
            .execute(
                receipt.clone(),
                || Ok(plan.clone()),
                VolatileBuildEstimate::new(100),
                &Probe::new([good_snapshot()]),
                &CancellationToken::default(),
                &adapter,
            )
            .unwrap();
        assert_eq!(
            report.outputs()[0].provenance(),
            BuildOutputProvenance::LocalBuild
        );
        let rows = lock_recover(&journal.rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source().as_str(), "yes");
        assert_eq!(rows[0].build_plan_digest(), plan.digest().unwrap());
        drop(rows);
        let requests = lock_recover(&adapter.requests);
        assert_eq!(
            requests[0].targets()[0].render_private(),
            format!("{}^out", derivation().as_str())
        );
        assert_eq!(requests[0].receipt(), &receipt);
        drop(requests);

        let error = engine
            .execute(
                receipt,
                || Ok(plan),
                VolatileBuildEstimate::new(100),
                &Probe::new([good_snapshot()]),
                &CancellationToken::default(),
                &adapter,
            )
            .unwrap_err();
        assert_eq!(error.code(), BuildEngineErrorCode::ApprovalRequired);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn approval_recording_does_not_block_lifecycle_cancellation() {
        let engine = Arc::new(LocalBuildEngine::new());
        let plan = plan(1, System::X8664Linux, linux_readiness());
        let retry_plan = plan.clone();
        let operation_id = OperationId::new("op-recording").unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let journal = Arc::new(BlockingJournal {
            rows: Mutex::new(Vec::new()),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });

        let approving_engine = Arc::clone(&engine);
        let approving_journal = Arc::clone(&journal);
        let approving_id = operation_id.clone();
        let approval = thread::spawn(move || {
            approving_engine.approve(
                approving_id,
                &plan,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:00Z",
                approving_journal.as_ref(),
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let cancelling_engine = Arc::clone(&engine);
        let cancelling_id = operation_id.clone();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let cancellation = thread::spawn(move || {
            let cancelled = cancelling_engine.cancel_approval(&cancelling_id);
            let _ = cancelled_tx.send(cancelled);
        });
        let cancelled = cancelled_rx.recv_timeout(Duration::from_secs(1));
        assert!(cancelled.unwrap());
        cancellation.join().unwrap();

        let retry_engine = Arc::clone(&engine);
        let retry_journal = Arc::clone(&journal);
        let retry_id = operation_id.clone();
        let retry = thread::spawn(move || {
            retry_engine.approve(
                retry_id,
                &retry_plan,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:01Z",
                retry_journal.as_ref(),
            )
        });

        release_tx.send(()).unwrap();

        assert_eq!(
            approval.join().unwrap().unwrap_err().code(),
            BuildEngineErrorCode::ApprovalUnavailable
        );
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        assert!(retry.join().unwrap().is_ok());
        assert_eq!(lock_recover(&journal.rows).len(), 2);
        assert!(engine.cancel_approval(&operation_id));
    }

    #[test]
    fn replan_mutation_invalidates_and_resource_failure_rechecks_exactly_once() {
        let engine = LocalBuildEngine::new();
        let journal = Journal::default();
        let approved = plan(1, System::X8664Linux, linux_readiness());
        let receipt = engine
            .approve(
                OperationId::new("op-change").unwrap(),
                &approved,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:00Z",
                &journal,
            )
            .unwrap();
        let adapter = BuildAdapter::new();
        let error = engine
            .execute(
                receipt,
                || Ok(plan(2, System::X8664Linux, linux_readiness())),
                VolatileBuildEstimate::new(100),
                &Probe::new([good_snapshot()]),
                &CancellationToken::default(),
                &adapter,
            )
            .unwrap_err();
        assert_eq!(error.code(), BuildEngineErrorCode::ApprovalInvalidated);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);

        let receipt = engine
            .approve(
                OperationId::new("op-resource").unwrap(),
                &approved,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:01Z",
                &journal,
            )
            .unwrap();
        let probe = Probe::new([
            ResourceSnapshot {
                free_bytes: 0,
                load_average: 100.0,
            },
            good_snapshot(),
        ]);
        engine
            .execute(
                receipt,
                || Ok(approved),
                VolatileBuildEstimate::new(100),
                &probe,
                &CancellationToken::default(),
                &adapter,
            )
            .unwrap();
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);

        let refused_plan = plan(1, System::X8664Linux, linux_readiness());
        let receipt = engine
            .approve(
                OperationId::new("op-resource-refused").unwrap(),
                &refused_plan,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:02Z",
                &journal,
            )
            .unwrap();
        let failed_probe = Probe::new([
            ResourceSnapshot {
                free_bytes: 0,
                load_average: 100.0,
            },
            ResourceSnapshot {
                free_bytes: 0,
                load_average: 100.0,
            },
        ]);
        let error = engine
            .execute(
                receipt,
                || Ok(refused_plan),
                VolatileBuildEstimate::new(100),
                &failed_probe,
                &CancellationToken::default(),
                &adapter,
            )
            .unwrap_err();
        assert_eq!(error.code(), BuildEngineErrorCode::ResourcePreflightFailed);
        assert_eq!(failed_probe.calls.load(Ordering::SeqCst), 2);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn admission_serializes_operations_and_cancelled_ticket_does_not_stall_queue() {
        let admission = BuildAdmission::default();
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert_eq!(
            admission.acquire(&cancelled).unwrap_err().code(),
            BuildEngineErrorCode::Cancelled
        );
        drop(admission.acquire(&CancellationToken::default()).unwrap());

        let engine = Arc::new(LocalBuildEngine::new());
        let journal = Arc::new(Journal::default());
        let adapter = Arc::new(BuildAdapter::new());
        let approved = plan(1, System::X8664Linux, linux_readiness());
        let cancelled_receipt = engine
            .approve(
                OperationId::new("op-cancelled").unwrap(),
                &approved,
                ApprovalSource::Interactive,
                "2026-08-10T00:00:00Z",
                journal.as_ref(),
            )
            .unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        assert_eq!(
            engine
                .execute(
                    cancelled_receipt.clone(),
                    || Ok(approved.clone()),
                    VolatileBuildEstimate::new(100),
                    &Probe::new([good_snapshot()]),
                    &cancellation,
                    adapter.as_ref(),
                )
                .unwrap_err()
                .code(),
            BuildEngineErrorCode::Cancelled
        );
        assert_eq!(
            engine
                .execute(
                    cancelled_receipt,
                    || Ok(approved.clone()),
                    VolatileBuildEstimate::new(100),
                    &Probe::new([good_snapshot()]),
                    &CancellationToken::default(),
                    adapter.as_ref(),
                )
                .unwrap_err()
                .code(),
            BuildEngineErrorCode::ApprovalRequired
        );
        let mut threads = Vec::new();
        for index in 0..2 {
            let operation = OperationId::new(&format!("op-{index}")).unwrap();
            let receipt = engine
                .approve(
                    operation,
                    &approved,
                    ApprovalSource::AssumeYes,
                    "2026-08-10T00:00:00Z",
                    journal.as_ref(),
                )
                .unwrap();
            let engine = Arc::clone(&engine);
            let adapter = Arc::clone(&adapter);
            let plan = approved.clone();
            threads.push(std::thread::spawn(move || {
                engine.execute(
                    receipt,
                    || Ok(plan),
                    VolatileBuildEstimate::new(100),
                    &Probe::new([good_snapshot()]),
                    &CancellationToken::default(),
                    adapter.as_ref(),
                )
            }));
        }
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(adapter.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
    }
}
