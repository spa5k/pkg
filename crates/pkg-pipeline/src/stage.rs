use std::path::Path;

use pkg_core::state::CollisionPolicy;
use pkg_store::{ActivationError, ActivationInput, ActivationPlan, stage_activation};

use crate::VerifiedInstall;

/// Checks installed output collisions using the same bindings as generation staging.
///
/// # Errors
/// Returns an error for unreadable outputs or a conflict refused by the policy.
pub fn plan_state_activation(
    state: &pkg_core::lifecycle::LifecycleState,
    collision_policy: CollisionPolicy,
) -> Result<ActivationPlan, ActivationError> {
    pkg_store::plan_activation(
        &crate::activation_metadata::activation_inputs(state),
        collision_policy,
    )
}

/// A verified install paired with its durable Rust-only staging plan.
#[derive(Debug)]
pub struct StagedInstall {
    verified: VerifiedInstall,
    plan: ActivationPlan,
}
impl StagedInstall {
    /// Returns verified selected outputs.
    #[must_use]
    pub const fn verified(&self) -> &VerifiedInstall {
        &self.verified
    }
    /// Returns the staged forest binding.
    #[must_use]
    pub const fn plan(&self) -> &ActivationPlan {
        &self.plan
    }
    /// Consumes this stage into its parts.
    #[must_use]
    pub fn into_parts(self) -> (VerifiedInstall, ActivationPlan) {
        (self.verified, self.plan)
    }
}

/// Builds the activation forest from already-verified outputs with zero Nix calls.
pub fn stage_verified(
    verified: VerifiedInstall,
    staging: &Path,
    collision_policy: CollisionPolicy,
) -> Result<StagedInstall, ActivationError> {
    let inputs = verified
        .outputs()
        .iter()
        .map(|output| ActivationInput::new(output.substitute().store_path().clone()))
        .collect::<Vec<_>>();
    let plan = stage_activation(staging, &inputs, collision_policy)?;
    Ok(StagedInstall { verified, plan })
}
