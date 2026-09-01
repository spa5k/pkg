use std::fmt;

use pkg_channel::{BuildMode, VerifiedChannel};
use pkg_core::state::Digest;
use pkg_core::{ChannelSequence, NarHash, NixpkgsRevision, PolicyVersion, System};
use pkg_nix::{
    BuildCacheEvidence, BuildEngineError, BuildEngineErrorCode, BuildPlan, BuildReadiness,
    NixVersion, STANDARD_DETERMINATE_NIX_VERSION, VersionInfo,
};

use crate::ResolvedInstall;

/// Authenticated deterministic policy needed to construct a private local-build plan.
///
/// Instances can only be derived from a fully verified channel. The type is not
/// serializable and is never part of the CLI/broker wire grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedBuildPolicy {
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
    nix_runtime_version: NixVersion,
    revision: NixpkgsRevision,
    nar_hash: NarHash,
    build_mode: BuildMode,
}

impl AuthenticatedBuildPolicy {
    /// Extracts build authority only from an authenticated, semantically valid channel.
    pub fn from_verified_channel(channel: &VerifiedChannel) -> Result<Self, LocalBuildPlanError> {
        let descriptor = channel.descriptor();
        Ok(Self {
            channel_sequence: channel.sequence(),
            policy_version: channel.policy_version(),
            descriptor_sha256: channel.descriptor_sha256(),
            nix_runtime_version: NixVersion::new(descriptor.nix_version())
                .map_err(|_| LocalBuildPlanError::new(LocalBuildPlanErrorCode::InvalidPolicy))?,
            revision: NixpkgsRevision::new(descriptor.nixpkgs().revision())
                .map_err(|_| LocalBuildPlanError::new(LocalBuildPlanErrorCode::InvalidPolicy))?,
            nar_hash: NarHash::new(descriptor.nixpkgs().nar_hash())
                .map_err(|_| LocalBuildPlanError::new(LocalBuildPlanErrorCode::InvalidPolicy))?,
            build_mode: descriptor.build_mode(),
        })
    }

    fn matches_resolved(&self, resolved: &ResolvedInstall) -> bool {
        self.matches_source_identity(
            resolved.channel_sequence(),
            resolved.policy_version(),
            resolved.descriptor_sha256(),
            resolved.revision(),
            resolved.nar_hash(),
        )
    }

    fn matches_source_identity(
        &self,
        sequence: ChannelSequence,
        policy_version: PolicyVersion,
        descriptor_sha256: [u8; 32],
        revision: &NixpkgsRevision,
        nar_hash: &NarHash,
    ) -> bool {
        self.channel_sequence == sequence
            && self.policy_version == policy_version
            && self.descriptor_sha256 == descriptor_sha256
            && &self.revision == revision
            && &self.nar_hash == nar_hash
    }
}

/// Stable fail-closed private-plan construction categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBuildPlanErrorCode {
    /// Authenticated channel policy could not be promoted into strong types.
    InvalidPolicy,
    /// Resolve results do not belong to the same authenticated channel identity.
    SourceIdentityMismatch,
    /// The running Nix version differs from the platform runtime contract.
    RuntimeMismatch,
    /// Resolver-owned targets could not be promoted without rebinding.
    InvalidResolvedTarget,
    /// Cache facts were classified for a different resolved derivation/output set.
    CacheEvidenceMismatch,
    /// Native policy, readiness, cache facts, or plan invariants refused the build.
    BuildRejected,
}

/// Redacted private-plan construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalBuildPlanError {
    code: LocalBuildPlanErrorCode,
    engine_code: Option<BuildEngineErrorCode>,
}

impl LocalBuildPlanError {
    const fn new(code: LocalBuildPlanErrorCode) -> Self {
        Self {
            code,
            engine_code: None,
        }
    }

    const fn from_engine(error: BuildEngineError) -> Self {
        Self {
            code: LocalBuildPlanErrorCode::BuildRejected,
            engine_code: Some(error.code()),
        }
    }

    /// Returns the stable orchestration failure class.
    #[must_use]
    pub const fn code(self) -> LocalBuildPlanErrorCode {
        self.code
    }

    /// Returns the underlying closed build-engine category when plan validation ran.
    #[must_use]
    pub const fn engine_code(self) -> Option<BuildEngineErrorCode> {
        self.engine_code
    }
}

impl fmt::Display for LocalBuildPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private local-build planning refused")
    }
}

impl std::error::Error for LocalBuildPlanError {}

/// Constructs one private deterministic plan from authenticated policy and
/// resolver-owned operation state.
///
/// Cache evidence and readiness observations are broker-private planning facts.
/// The opaque cache evidence keeps the classification identity paired with the
/// exact missing derivations. This function does not serialize the resulting
/// plan and accepts no installable string, flake reference, Nix option, or
/// command argv.
pub fn prepare_local_build_plan(
    policy: &AuthenticatedBuildPolicy,
    resolved: &ResolvedInstall,
    runtime: &VersionInfo,
    host_system: System,
    cache_evidence: BuildCacheEvidence,
    readiness: BuildReadiness,
    host_cores: u32,
) -> Result<BuildPlan, LocalBuildPlanError> {
    if !policy.matches_resolved(resolved) {
        return Err(LocalBuildPlanError::new(
            LocalBuildPlanErrorCode::SourceIdentityMismatch,
        ));
    }
    let plan_runtime_version = plan_runtime_version(policy, runtime, host_system)?;
    let targets = resolved
        .build_plan_targets()
        .map_err(|_| LocalBuildPlanError::new(LocalBuildPlanErrorCode::InvalidResolvedTarget))?;
    let cache_subjects = resolved
        .build_cache_subjects()
        .map_err(|_| LocalBuildPlanError::new(LocalBuildPlanErrorCode::InvalidResolvedTarget))?;
    if !cache_evidence.matches_subjects(&cache_subjects) {
        return Err(LocalBuildPlanError::new(
            LocalBuildPlanErrorCode::CacheEvidenceMismatch,
        ));
    }
    let (cache_classification, missing_derivations) = cache_evidence.into_parts();
    BuildPlan::new(
        plan_runtime_version,
        Digest::from_bytes(policy.descriptor_sha256),
        policy.policy_version,
        policy.channel_sequence,
        &policy.revision,
        &policy.nar_hash,
        resolved.system(),
        host_system,
        policy.build_mode,
        targets,
        missing_derivations,
        cache_classification,
        readiness,
        host_cores,
    )
    .map_err(LocalBuildPlanError::from_engine)
}

fn plan_runtime_version<'a>(
    policy: &'a AuthenticatedBuildPolicy,
    runtime: &'a VersionInfo,
    host_system: System,
) -> Result<&'a NixVersion, LocalBuildPlanError> {
    if matches!(host_system, System::X8664Linux | System::Aarch64Linux) {
        if runtime.nix_version().as_str() != STANDARD_DETERMINATE_NIX_VERSION {
            return Err(LocalBuildPlanError::new(
                LocalBuildPlanErrorCode::RuntimeMismatch,
            ));
        }
        Ok(runtime.nix_version())
    } else if runtime.nix_version() == &policy.nix_runtime_version {
        Ok(&policy.nix_runtime_version)
    } else {
        Err(LocalBuildPlanError::new(
            LocalBuildPlanErrorCode::RuntimeMismatch,
        ))
    }
}

#[cfg(test)]
mod tests;
