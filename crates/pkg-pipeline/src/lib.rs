#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Ordered install-phase orchestration and crash-safe generation commits.

mod acquire;
mod activate;
mod activation_metadata;
mod build_authority;
mod build_authority_refresh;
mod build_intent;
mod build_plan;
mod build_preparation;
mod commit;
mod host_facts;
mod install_generation;
mod lifecycle;
mod preflight;
mod resolve;
mod rollback;
mod stage;
mod state_edit;
mod verify;

pub use acquire::{
    AcquireError, AcquiredInstall, AcquiredOutput, CacheEvidenceError, acquire_cache_only,
    acquire_cache_only_with_progress, assemble_cache_install_evidence,
};
pub use activate::{activate_prepared, finish_activated, prepare_activation};
pub use build_authority::{
    AuthenticatedBuildAuthority, BuildAuthorityError, BuildAuthorityErrorCode, BuildAuthorityUpdate,
};
pub use build_authority_refresh::{
    AuthenticatedBuildAuthorityService, BuildAuthorityRefreshError, BuildAuthorityRefreshErrorCode,
};
pub use build_intent::{
    AuthenticatedBuildIntent, AuthenticatedBuildReplanner, BuildHostFacts, BuildHostFactsError,
    BuildHostFactsProbe, BuildIntentError, BuildIntentErrorCode, BuildPlanningAdapter,
};
pub use build_plan::{
    AuthenticatedBuildPolicy, LocalBuildPlanError, LocalBuildPlanErrorCode,
    prepare_local_build_plan,
};
pub use build_preparation::{
    AuthenticatedBuildPreparation, BuildPreparationError, BuildPreparationErrorCode,
};
pub use commit::{
    ActivatedGeneration, CandidateGeneration, CommitError, PreparedGeneration, RecoveryResult,
    discard_unprepared_installs, discard_unprepared_state_edits, load_active_snapshot,
    load_retained_history, pending_install_discard_generation, pending_install_generation,
    pending_state_edit_generation, pending_state_transition_source, recover_generation,
    recover_transitioned_state_edit, resume_prepared_install, resume_prepared_state_edit,
};
pub use host_facts::ProductionBuildHostFactsProbe;
pub use install_generation::{
    InstallGenerationError, InstallGenerationMetadata, prepare_install_generation,
};
pub use lifecycle::{
    InstallStateError, assemble_install_evidence_state, assemble_install_state,
    assemble_upgrade_evidence_state,
};
pub use preflight::{PlannedOutput, PreflightError, PreflightInstall, preflight_cache_only};
pub use resolve::{ResolveBatchError, ResolvedInstall, resolve_install};
pub use rollback::{RollbackPrepareError, prepare_rollback};
pub use stage::{StagedInstall, stage_verified};
pub use state_edit::{StateEditKind, StateEditMetadata, StateEditPrepareError, prepare_state_edit};
pub use verify::{VerifiedInstall, verify_acquired};
