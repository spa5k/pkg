#![forbid(unsafe_code)]
//! Rust-owned activation forests and generation GC-root orchestration.
//!
//! Activation never invokes Nix. Store outputs are merged into a deterministic
//! symlink forest, rooted through the authenticated maintenance boundary, and
//! only then exposed by an atomic relative `current` symlink.

mod activate;
mod current;
mod gc;
mod journal;
mod leases;
mod roots;

pub use activate::{
    ActivationError, ActivationInput, ActivationPlan, Collision, ForestEntry,
    inspect_staged_activation, stage_activation, verify_activation, verify_recorded_activation,
};
pub use current::{
    ActivationEvent, CurrentError, RecoveryAction, RecoveryEvidence, STATE_OWNERSHIP_MARKER_BYTES,
    STATE_OWNERSHIP_MARKER_NAME, StateLayout, activate_generation, activate_published_generation,
    activate_rooted_generation, activate_transitioned_generation, classify_recovery,
};
pub use gc::{
    GcError, GcPlan, GcPolicy, GcRunReport, PruneCandidate, PruneOutcome,
    authorize_generation_root_removal, execute_gc, plan_gc, plan_generation_prune,
    prune_generation, recover_prunes,
};
pub use journal::{StateJournal, StateJournalError};
pub use leases::{LeaseError, LeaseIdentity, LeaseMode, StateLease};
pub use roots::{PreparedRootSet, RootCandidate, RootError, prepare_root_set, publish_root_set};
