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
    ActivationEvent, CurrentError, RecoveryAction, RecoveryEvidence, StateLayout,
    activate_generation, activate_rooted_generation, classify_recovery,
};
pub use gc::{
    GcError, GcPlan, GcPolicy, GcRunReport, PruneCandidate, PruneOutcome, execute_gc, plan_gc,
    prune_generation, recover_prunes,
};
pub use journal::{StateJournal, StateJournalError};
pub use leases::{LeaseError, LeaseIdentity, LeaseMode, StateLease};
pub use roots::{PreparedRootSet, RootCandidate, RootError, prepare_root_set, publish_root_set};
