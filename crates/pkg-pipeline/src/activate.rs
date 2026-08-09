use pkg_nix::MaintenanceAdapter;
use pkg_store::{ActivationPlan, StateLayout, StateLease};

use crate::{ActivatedGeneration, CandidateGeneration, CommitError, PreparedGeneration};

/// Durably writes candidate state before any root/current mutation.
pub fn prepare_activation(
    layout: StateLayout,
    candidate: CandidateGeneration,
    plan: ActivationPlan,
    lease: StateLease,
) -> Result<PreparedGeneration, CommitError> {
    PreparedGeneration::prepare(layout, candidate, plan, lease)
}

/// Roots every output before retaining and atomically exposing the forest.
pub fn activate_prepared(
    prepared: PreparedGeneration,
    helper: &dyn MaintenanceAdapter,
    nonce: &str,
) -> Result<ActivatedGeneration, CommitError> {
    prepared.activate(helper, nonce)
}

/// Restores current views and appends the final committed row.
pub fn finish_activated(activated: ActivatedGeneration) -> Result<(), CommitError> {
    activated.finish()
}
