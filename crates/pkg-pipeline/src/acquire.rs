use std::fmt;

use pkg_channel::VerifiedChannel;
use pkg_core::{ChannelSequence, PolicyVersion};
use pkg_nix::{
    Digest, InstallEvidence, NixAdapter, SubstituteResult, VerifiedSubstitute, acquire_substitute,
};

use crate::{PlannedOutput, PreflightInstall, ResolvedInstall, VerifiedInstall};

/// One selected output proven present and trusted after substitution.
#[derive(Debug)]
pub struct AcquiredOutput {
    planned: PlannedOutput,
    substitute: VerifiedSubstitute,
}

impl AcquiredOutput {
    /// Returns the desired-state/output binding.
    #[must_use]
    pub const fn planned(&self) -> &PlannedOutput {
        &self.planned
    }
    /// Returns the cryptographically verified cache result.
    #[must_use]
    pub const fn substitute(&self) -> &VerifiedSubstitute {
        &self.substitute
    }
}

/// Every exact output acquired for this operation.
#[derive(Debug)]
pub struct AcquiredInstall {
    outputs: Vec<AcquiredOutput>,
    authority: CacheAuthorityIdentity,
}
impl AcquiredInstall {
    /// Returns acquired outputs in preflight order.
    #[must_use]
    pub fn outputs(&self) -> &[AcquiredOutput] {
        &self.outputs
    }

    pub(crate) fn into_parts(self) -> (Vec<AcquiredOutput>, CacheAuthorityIdentity) {
        (self.outputs, self.authority)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CacheAuthorityIdentity {
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
}

impl CacheAuthorityIdentity {
    fn from_channel(channel: &VerifiedChannel) -> Self {
        Self {
            channel_sequence: channel.sequence(),
            policy_version: channel.policy_version(),
            descriptor_sha256: channel.descriptor_sha256(),
        }
    }

    pub(crate) fn matches_resolved(&self, resolved: &ResolvedInstall) -> bool {
        self.channel_sequence == resolved.channel_sequence()
            && self.policy_version == resolved.policy_version()
            && self.descriptor_sha256 == resolved.descriptor_sha256()
    }
}

/// Cache-only acquisition failure; a normal miss is explicitly build-required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// At least one output needs the PR-26 approved local-build path.
    BuildRequired,
    /// Substitution or verification failed closed.
    Refused,
}
impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "install acquisition refused: {self:?}")
    }
}
impl std::error::Error for AcquireError {}

/// Cache evidence could not be bound to the authenticated resolve result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheEvidenceError;

impl fmt::Display for CacheEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cache install evidence refused")
    }
}

impl std::error::Error for CacheEvidenceError {}

/// Substitutes every selected output; any miss stops before staging.
///
/// The caller must hold the broker-issued GC-inhibit permit for the complete
/// operation so successfully substituted outputs cannot be collected before
/// roots are durably published during activation.
pub fn acquire_cache_only(
    resolved: &ResolvedInstall,
    preflight: &PreflightInstall,
    channel: &VerifiedChannel,
    adapter: &dyn NixAdapter,
) -> Result<AcquiredInstall, AcquireError> {
    let authority = CacheAuthorityIdentity::from_channel(channel);
    let expected = crate::preflight_cache_only(resolved).map_err(|_| AcquireError::Refused)?;
    if !authority.matches_resolved(resolved) || expected.outputs() != preflight.outputs() {
        return Err(AcquireError::Refused);
    }
    let mut outputs = Vec::with_capacity(preflight.outputs().len());
    for planned in preflight.outputs() {
        match acquire_substitute(planned.store_path(), channel.descriptor().cache(), adapter)
            .map_err(|_| AcquireError::Refused)?
        {
            SubstituteResult::Fetched(substitute) => outputs.push(AcquiredOutput {
                planned: planned.clone(),
                substitute,
            }),
            SubstituteResult::Miss(_) => return Err(AcquireError::BuildRequired),
        }
    }
    Ok(AcquiredInstall { outputs, authority })
}

/// Binds verified cache outputs to the exact authenticated resolve result.
///
/// This is the cache-hit counterpart to post-build evidence creation. It does
/// not accept raw store paths or caller-supplied provenance.
///
/// # Errors
///
/// Refuses any target, source identity, output, or fresh metadata mismatch.
pub fn assemble_cache_install_evidence(
    resolved: &ResolvedInstall,
    verified: &VerifiedInstall,
    adapter: &dyn NixAdapter,
) -> Result<InstallEvidence, CacheEvidenceError> {
    if !verified.authority().matches_resolved(resolved) {
        return Err(CacheEvidenceError);
    }
    let targets = resolved
        .build_plan_targets()
        .map_err(|_| CacheEvidenceError)?;
    let substitutes = verified
        .outputs()
        .iter()
        .map(|output| output.substitute().clone())
        .collect();
    InstallEvidence::from_cache_substitutes(
        Digest::from_bytes(resolved.descriptor_sha256()),
        resolved.channel_sequence(),
        resolved.policy_version(),
        resolved.revision().clone(),
        resolved.nar_hash().clone(),
        resolved.system(),
        targets,
        substitutes,
        adapter,
    )
    .map_err(|_| CacheEvidenceError)
}
