//! Closed Broker-to-Root-Helper Nix operation contract.

use std::{str::FromStr, time::Duration};

use crate::{
    BuildCacheErrorCode, BuildPreview, BuildProgressEstimate, BuildReadiness, BuildReport,
    BuildRequest, CacheDownloadClosure, CachePathObservation, DerivationPlanReport, Digest,
    EvaluateDerivationRequest, GcReport, NixAdapterErrorCode, NixpkgsPin, NixpkgsSourceErrorCode,
    PathInfoReport, PolicyVersion, StorePath, SubstituteReport, System, VerifyReport,
    VerifyRequest, VersionInfo,
};

const CLIENT_GRACE: Duration = Duration::from_mins(1);

/// One fixed Root Helper Nix operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootNixOperation {
    /// Read the fixed Nix version.
    Version,
    /// Evaluate one typed derivation request.
    Evaluate,
    /// Inspect one typed store path.
    PathInfo,
    /// Substitute one typed store path.
    Substitute,
    /// Substitute a bounded typed path set.
    SubstituteMany,
    /// Execute one approved typed build.
    Build,
    /// Verify a bounded typed path set.
    Verify,
    /// Run product package garbage collection.
    Gc,
    /// Inspect cache availability for typed paths.
    CacheInspect,
    /// Inspect cache closures for typed roots.
    CacheInspectClosures,
    /// Materialize one authenticated Nixpkgs pin.
    NixpkgsMetadata,
    /// Resolve the closure of typed generation roots.
    ClosureForRoots,
    /// Produce a sanitized repair preview and digest.
    RepairPlan,
}

impl RootNixOperation {
    /// Returns the fixed helper-channel method id.
    #[must_use]
    pub const fn method_id(self) -> u8 {
        match self {
            Self::Version => 20,
            Self::Evaluate => 21,
            Self::PathInfo => 22,
            Self::Substitute => 23,
            Self::SubstituteMany => 24,
            Self::Build => 25,
            Self::Verify => 26,
            Self::Gc => 27,
            Self::CacheInspect => 28,
            Self::CacheInspectClosures => 29,
            Self::NixpkgsMetadata => 30,
            Self::ClosureForRoots => 31,
            Self::RepairPlan => 32,
        }
    }

    /// Returns the absolute server operation budget.
    #[must_use]
    pub const fn server_budget(self) -> Duration {
        match self {
            Self::Version => Duration::from_mins(2),
            Self::PathInfo => Duration::from_mins(1),
            Self::Evaluate => Duration::from_mins(30),
            Self::NixpkgsMetadata => Duration::from_mins(15),
            Self::Substitute | Self::SubstituteMany | Self::Build | Self::Verify => {
                Duration::from_hours(24)
            }
            Self::Gc
            | Self::CacheInspect
            | Self::CacheInspectClosures
            | Self::ClosureForRoots
            | Self::RepairPlan => Duration::from_hours(1),
        }
    }

    /// Returns the fixed client budget, one minute beyond the server budget.
    pub fn client_budget(self) -> Option<Duration> {
        self.server_budget().checked_add(CLIENT_GRACE)
    }

    /// Promotes one fixed helper-channel method id.
    #[must_use]
    pub const fn from_method_id(method: u8) -> Option<Self> {
        match method {
            20 => Some(Self::Version),
            21 => Some(Self::Evaluate),
            22 => Some(Self::PathInfo),
            23 => Some(Self::Substitute),
            24 => Some(Self::SubstituteMany),
            25 => Some(Self::Build),
            26 => Some(Self::Verify),
            27 => Some(Self::Gc),
            28 => Some(Self::CacheInspect),
            29 => Some(Self::CacheInspectClosures),
            30 => Some(Self::NixpkgsMetadata),
            31 => Some(Self::ClosureForRoots),
            32 => Some(Self::RepairPlan),
            _ => None,
        }
    }
}

/// Typed input for fixed repair-plan projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRepairPlanRequest {
    damaged: Vec<StorePath>,
    policy_version: PolicyVersion,
    system: System,
    readiness: BuildReadiness,
    host_cores: u32,
}

impl RootRepairPlanRequest {
    /// Constructs one bounded canonical request.
    pub fn new(
        mut damaged: Vec<StorePath>,
        policy_version: PolicyVersion,
        system: System,
        readiness: BuildReadiness,
        host_cores: u32,
    ) -> Option<Self> {
        damaged.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if damaged.is_empty()
            || damaged.len() > 4096
            || damaged.windows(2).any(|pair| pair[0] == pair[1])
            || host_cores == 0
        {
            return None;
        }
        Some(Self {
            damaged,
            policy_version,
            system,
            readiness,
            host_cores,
        })
    }

    /// Returns the canonical damaged paths.
    #[must_use]
    pub fn damaged(&self) -> &[StorePath] {
        &self.damaged
    }

    /// Returns the authenticated policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the host system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns whether a repair proof targets this request's exact system.
    #[must_use]
    pub fn accepts(&self, proof: &RootRepairPlanProof) -> bool {
        proof.preview.matches_system(self.system)
    }

    /// Returns the fixed build-readiness facts.
    #[must_use]
    pub const fn readiness(&self) -> &BuildReadiness {
        &self.readiness
    }

    /// Returns the observed host core count.
    #[must_use]
    pub const fn host_cores(&self) -> u32 {
        self.host_cores
    }
}

/// Sanitized repair approval proof. Private plan internals never cross the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRepairPlanProof {
    preview: BuildPreview,
    digest: Digest,
}

impl RootRepairPlanProof {
    /// Promotes one validated local-rebuild repair preview.
    pub fn new(preview: BuildPreview) -> Option<Self> {
        if !preview.is_repair_approval() {
            return None;
        }
        let hex = preview.build_plan_digest().strip_prefix("sha256:")?;
        let digest = Digest::from_str(&format!("sha256-{hex}")).ok()?;
        Some(Self { preview, digest })
    }

    /// Returns the sanitized preview containing the plan digest.
    #[must_use]
    pub const fn preview(&self) -> &BuildPreview {
        &self.preview
    }

    /// Returns the typed digest bound to the sanitized preview.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }
}

/// Closed typed request grammar for Root Helper Nix work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootNixRequest {
    /// Version request.
    Version,
    /// Derivation evaluation request.
    Evaluate(EvaluateDerivationRequest),
    /// Path-info request.
    PathInfo(StorePath),
    /// Single-path substitution request.
    Substitute(StorePath),
    /// Multi-path substitution request.
    SubstituteMany(Vec<StorePath>),
    /// Approved build request.
    Build(BuildRequest),
    /// Verification request.
    Verify(VerifyRequest),
    /// Package GC request.
    Gc,
    /// Cache inspection request.
    CacheInspect(Vec<StorePath>),
    /// Cache-closure inspection request.
    CacheInspectClosures(Vec<StorePath>),
    /// Nixpkgs metadata request.
    NixpkgsMetadata(NixpkgsPin),
    /// Generation-root closure request.
    ClosureForRoots(Vec<StorePath>),
    /// Repair approval proof request.
    RepairPlan(RootRepairPlanRequest),
}

impl RootNixRequest {
    /// Returns the exact operation kind.
    #[must_use]
    pub const fn operation(&self) -> RootNixOperation {
        match self {
            Self::Version => RootNixOperation::Version,
            Self::Evaluate(_) => RootNixOperation::Evaluate,
            Self::PathInfo(_) => RootNixOperation::PathInfo,
            Self::Substitute(_) => RootNixOperation::Substitute,
            Self::SubstituteMany(_) => RootNixOperation::SubstituteMany,
            Self::Build(_) => RootNixOperation::Build,
            Self::Verify(_) => RootNixOperation::Verify,
            Self::Gc => RootNixOperation::Gc,
            Self::CacheInspect(_) => RootNixOperation::CacheInspect,
            Self::CacheInspectClosures(_) => RootNixOperation::CacheInspectClosures,
            Self::NixpkgsMetadata(_) => RootNixOperation::NixpkgsMetadata,
            Self::ClosureForRoots(_) => RootNixOperation::ClosureForRoots,
            Self::RepairPlan(_) => RootNixOperation::RepairPlan,
        }
    }
}

/// Closed, redacted Root Helper failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootNixFailure {
    /// Adapter failure code.
    Adapter(NixAdapterErrorCode),
    /// Cache-probe failure code.
    Cache(BuildCacheErrorCode),
    /// Nixpkgs metadata failure code.
    Nixpkgs(NixpkgsSourceErrorCode),
    /// The fixed operation admission limit is full.
    Busy,
    /// The inactive DN09 production state rejected the request.
    Inactive,
}

/// Closed typed result grammar for Root Helper Nix work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootNixResponse {
    /// Version result.
    Version(VersionInfo),
    /// Derivation evaluation result.
    Evaluate(DerivationPlanReport),
    /// Path-info result.
    PathInfo(PathInfoReport),
    /// Single-path substitution result.
    Substitute(SubstituteReport),
    /// Multi-path substitution result.
    SubstituteMany(Vec<SubstituteReport>),
    /// One live build progress estimate.
    BuildProgress(BuildProgressEstimate),
    /// Terminal build result.
    Build(BuildReport),
    /// Verification result.
    Verify(VerifyReport),
    /// Package GC result.
    Gc(GcReport),
    /// Cache inspection result.
    CacheInspect(Vec<CachePathObservation>),
    /// Cache-closure inspection result.
    CacheInspectClosures(Vec<CacheDownloadClosure>),
    /// Raw bounded metadata JSON from the fixed command.
    NixpkgsMetadata(Vec<u8>),
    /// Canonical generation-root closure.
    ClosureForRoots(Vec<StorePath>),
    /// Sanitized repair preview proof.
    RepairPlan(RootRepairPlanProof),
    /// Closed failure correlated to one exact operation.
    Failed {
        /// The operation that failed.
        operation: RootNixOperation,
        /// The redacted failure.
        failure: RootNixFailure,
    },
}

impl RootNixResponse {
    /// Returns the exact operation kind.
    #[must_use]
    pub const fn operation(&self) -> RootNixOperation {
        match self {
            Self::Version(_) => RootNixOperation::Version,
            Self::Evaluate(_) => RootNixOperation::Evaluate,
            Self::PathInfo(_) => RootNixOperation::PathInfo,
            Self::Substitute(_) => RootNixOperation::Substitute,
            Self::SubstituteMany(_) => RootNixOperation::SubstituteMany,
            Self::BuildProgress(_) | Self::Build(_) => RootNixOperation::Build,
            Self::Verify(_) => RootNixOperation::Verify,
            Self::Gc(_) => RootNixOperation::Gc,
            Self::CacheInspect(_) => RootNixOperation::CacheInspect,
            Self::CacheInspectClosures(_) => RootNixOperation::CacheInspectClosures,
            Self::NixpkgsMetadata(_) => RootNixOperation::NixpkgsMetadata,
            Self::ClosureForRoots(_) => RootNixOperation::ClosureForRoots,
            Self::RepairPlan(_) => RootNixOperation::RepairPlan,
            Self::Failed { operation, .. } => *operation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_ids_and_client_grace_are_fixed_and_unique() {
        let operations = [
            RootNixOperation::Version,
            RootNixOperation::Evaluate,
            RootNixOperation::PathInfo,
            RootNixOperation::Substitute,
            RootNixOperation::SubstituteMany,
            RootNixOperation::Build,
            RootNixOperation::Verify,
            RootNixOperation::Gc,
            RootNixOperation::CacheInspect,
            RootNixOperation::CacheInspectClosures,
            RootNixOperation::NixpkgsMetadata,
            RootNixOperation::ClosureForRoots,
            RootNixOperation::RepairPlan,
        ];
        let mut ids = operations.map(RootNixOperation::method_id);
        ids.sort_unstable();
        assert_eq!(ids, [20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]);
        for operation in operations {
            assert_eq!(
                RootNixOperation::from_method_id(operation.method_id()),
                Some(operation)
            );
            assert_eq!(
                operation.client_budget(),
                operation
                    .server_budget()
                    .checked_add(Duration::from_mins(1))
            );
        }
        assert_eq!(RootNixOperation::from_method_id(19), None);
        assert_eq!(RootNixOperation::from_method_id(33), None);
    }
}
