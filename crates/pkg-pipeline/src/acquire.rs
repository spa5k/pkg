use std::fmt;

use pkg_channel::CachePolicy;
use pkg_nix::{NixAdapter, SubstituteResult, VerifiedSubstitute, acquire_substitute};

use crate::{PlannedOutput, PreflightInstall};

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
}
impl AcquiredInstall {
    /// Returns acquired outputs in preflight order.
    #[must_use]
    pub fn outputs(&self) -> &[AcquiredOutput] {
        &self.outputs
    }

    pub(crate) fn into_outputs(self) -> Vec<AcquiredOutput> {
        self.outputs
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

/// Substitutes every selected output; any miss stops before staging.
///
/// The caller must hold the broker-issued GC-inhibit permit for the complete
/// operation so successfully substituted outputs cannot be collected before
/// roots are durably published during activation.
pub fn acquire_cache_only(
    preflight: &PreflightInstall,
    cache_policy: &CachePolicy,
    adapter: &dyn NixAdapter,
) -> Result<AcquiredInstall, AcquireError> {
    let mut outputs = Vec::with_capacity(preflight.outputs().len());
    for planned in preflight.outputs() {
        match acquire_substitute(planned.store_path(), cache_policy, adapter)
            .map_err(|_| AcquireError::Refused)?
        {
            SubstituteResult::Fetched(substitute) => outputs.push(AcquiredOutput {
                planned: planned.clone(),
                substitute,
            }),
            SubstituteResult::Miss(_) => return Err(AcquireError::BuildRequired),
        }
    }
    Ok(AcquiredInstall { outputs })
}
