//! Deterministic, explicitly approved local-build planning and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use pkg_channel::{BuildMode, CachePolicy};
use pkg_core::state::{Digest, canonical_digest};
use pkg_core::{
    AttributePath, ChannelSequence, NarHash, NixpkgsRevision, OutputName, PolicyVersion,
    SelectorId, SelectorInput, System, VersionPreference,
};
use serde::Serialize;

use crate::{
    BuildApprovalReceipt, BuildReport, BuildRequest, BuildStatus, DerivationPlanReport,
    DerivedOutputTarget, NixAdapter, NixVersion, OperationId,
};

const MAX_TEXT: usize = 256;
const BUILD_USERS_GROUP: &str = "nixbld";
const MAX_JOBS: u32 = 1;
const CORES_HINT: u32 = 0;
const MAX_SILENT_SECONDS: u64 = 3_600;
const TIMEOUT_SECONDS: u64 = 86_400;
const MAX_LOG_BYTES: u64 = 268_435_456;
const DISK_HEADROOM_PERCENT: u64 = 120;
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
        plan: DerivationPlanReport,
    ) -> Self {
        Self {
            selector_id,
            selector,
            attribute,
            version_preference,
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
    outputs_to_install: Vec<String>,
    root_derivation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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

        let mut closure = BTreeMap::new();
        let mut canonical_targets = Vec::with_capacity(targets.len());
        let mut execution_targets = Vec::with_capacity(targets.len());
        let mut expected_outputs = BTreeSet::new();
        for target in &targets {
            for derivation in target.plan.derivations() {
                if derivation.system() != system {
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
                outputs_to_install: target
                    .plan
                    .outputs_to_install()
                    .iter()
                    .map(OutputName::as_str)
                    .map(str::to_owned)
                    .collect(),
                root_derivation: target.plan.root().as_str().to_owned(),
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

    /// Produces a sanitized preview with volatile, non-digest-bound estimates.
    pub fn preview_with_estimates(
        &self,
        estimates: BuildPreviewEstimates,
    ) -> Result<BuildPreview, BuildEngineError> {
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
            platform: PreviewPlatform { os, arch },
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
                    isolation: "sandbox",
                    per_build_resource_cap: false,
                    notice: "Builds run sandboxed. The managed runtime applies no hard per-build memory/CPU/IO cap; daemon time/log ceilings and one machine-global build admission bound the operation.",
                },
            },
            approval_required: true,
        })
    }
}

fn checked_text(value: &str) -> Result<String, BuildEngineError> {
    if value.is_empty() || value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        Err(BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    } else {
        Ok(value.to_owned())
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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
        Ok(Self {
            approx_build_minutes: approx_build_minutes.map(checked_text).transpose()?,
            approx_new_disk_bytes,
            approx_total_closure_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTarget {
    selector: String,
    package_name: String,
    version: String,
    outputs_to_install: Vec<String>,
}

impl BuildPreview {
    /// Returns the private plan digest pointer displayed to the user.
    #[must_use]
    pub fn build_plan_digest(&self) -> &str {
        &self.build_plan_digest
    }

    /// Serializes this allowlisted public object for CLI/RPC rendering.
    pub fn to_json_value(&self) -> Result<serde_json::Value, BuildEngineError> {
        serde_json::to_value(self)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::InvalidPlan))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PreviewPlatform {
    os: &'static str,
    arch: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewBuild {
    count: usize,
    names: Vec<String>,
    has_fixed_output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewCache {
    known_download_bytes: u64,
    known_content_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewReadiness {
    sandboxed: bool,
    build_isolation_ready: bool,
    native_build: bool,
    resource_boundary: PreviewResourceBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewResourceBoundary {
    isolation: &'static str,
    per_build_resource_cap: bool,
    notice: &'static str,
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
    fn record(&self, record: &ApprovalJournalRecord) -> Result<(), BuildEngineError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalGrant {
    digest: Digest,
    policy_version: PolicyVersion,
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
    approvals: Mutex<BTreeMap<OperationId, ApprovalGrant>>,
    admission: BuildAdmission,
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
        let timestamp = checked_text(timestamp)?;
        let digest = plan.digest()?;
        let record = ApprovalJournalRecord {
            operation_id: operation_id.clone(),
            build_plan_digest: digest,
            policy_version: plan.policy_identity,
            source,
            timestamp,
        };
        let mut approvals = lock_recover(&self.approvals);
        if approvals.contains_key(&operation_id) {
            return Err(BuildEngineError::new(
                BuildEngineErrorCode::ApprovalUnavailable,
            ));
        }
        journal
            .record(&record)
            .map_err(|_| BuildEngineError::new(BuildEngineErrorCode::JournalFailed))?;
        approvals.insert(
            operation_id.clone(),
            ApprovalGrant {
                digest,
                policy_version: plan.policy_identity,
            },
        );
        Ok(BuildApprovalReceipt::new(
            operation_id,
            digest,
            plan.policy_identity,
        ))
    }

    /// Revokes an unconsumed approval when its broker operation ends.
    ///
    /// Disconnect, explicit cancellation, and operation expiry must call this
    /// so an abandoned receipt cannot remain live inside the singleton broker.
    pub fn cancel_approval(&self, operation_id: &OperationId) -> bool {
        lock_recover(&self.approvals).remove(operation_id).is_some()
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
        let _permit = match self.admission.acquire(cancellation) {
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
            if approvals.get(receipt.operation_id()) != Some(&expected_grant) {
                return Err(BuildEngineError::new(
                    BuildEngineErrorCode::ApprovalRequired,
                ));
            }
        }
        let first_measurement = match resources.measure() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.revoke(receipt.operation_id());
                return Err(error);
            }
        };
        let resources_ready = if resource_ok(&current, estimate, first_measurement) {
            true
        } else {
            match resources.measure() {
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
        if removed != Some(expected_grant) {
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
        let report = adapter
            .build(&request)
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
        Ok(report)
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pkg_core::PackageVersion;

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
        fn record(&self, record: &ApprovalJournalRecord) -> Result<(), BuildEngineError> {
            lock_recover(&self.rows).push(record.clone());
            Ok(())
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
    fn plan_is_deterministic_mutation_sensitive_and_preview_is_public() {
        let first = plan(1, System::X8664Linux, linux_readiness());
        let same = plan(1, System::X8664Linux, linux_readiness());
        let changed = plan(2, System::X8664Linux, linux_readiness());
        assert_eq!(first.digest().unwrap(), same.digest().unwrap());
        assert_ne!(first.digest().unwrap(), changed.digest().unwrap());

        let preview = first
            .preview()
            .unwrap()
            .to_json_value()
            .unwrap()
            .to_string();
        assert!(preview.contains("\"approvalRequired\":true"));
        assert!(preview.contains("\"buildPlanDigest\":\"sha256:"));
        assert!(preview.contains("\"selector\":\"hello\""));
        assert!(preview.contains("\"perBuildResourceCap\":false"));
        assert!(preview.contains("\"approxBuildMinutes\":null"));
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
