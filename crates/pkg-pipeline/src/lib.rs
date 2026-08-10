#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Ordered install-phase orchestration and crash-safe generation commits.

mod acquire;
mod activate;
mod build_authority;
mod build_intent;
mod build_plan;
mod build_preparation;
mod commit;
mod host_facts;
mod lifecycle;
mod preflight;
mod resolve;
mod rollback;
mod stage;
mod verify;

pub use acquire::{AcquireError, AcquiredInstall, AcquiredOutput, acquire_cache_only};
pub use activate::{activate_prepared, finish_activated, prepare_activation};
pub use build_authority::{
    AuthenticatedBuildAuthority, BuildAuthorityError, BuildAuthorityErrorCode, BuildAuthorityUpdate,
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
    recover_generation,
};
pub use host_facts::ProductionBuildHostFactsProbe;
pub use lifecycle::{InstallStateError, assemble_install_state};
pub use preflight::{PlannedOutput, PreflightError, PreflightInstall, preflight_cache_only};
pub use resolve::{ResolveBatchError, ResolvedInstall, resolve_install};
pub use rollback::{RollbackPrepareError, prepare_rollback};
pub use stage::{StagedInstall, stage_verified};
pub use verify::{VerifiedInstall, verify_acquired};

use std::fmt;

/// The only legal install phase order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPhase {
    /// User intent is mapped to an evaluated derivation plan.
    Resolve,
    /// Policy, cache/build need, resources, and approval are checked.
    Preflight,
    /// Selected outputs are substituted or explicitly built.
    Acquire,
    /// Every acquired output is independently accepted.
    Verify,
    /// Rust materializes the deterministic activation forest.
    Stage,
    /// Snapshots/record/roots precede the atomic current switch.
    Activate,
    /// Mutable views and the final journal row are made durable.
    Commit,
}

/// Stable pipeline failure with the phase that refused progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineError {
    phase: InstallPhase,
    activated: bool,
}

impl PipelineError {
    /// Constructs a redacted phase failure.
    #[must_use]
    pub const fn new(phase: InstallPhase, activated: bool) -> Self {
        Self { phase, activated }
    }
    /// Returns the phase that refused progress.
    #[must_use]
    pub const fn phase(self) -> InstallPhase {
        self.phase
    }
    /// Returns whether recovery must finish the already-live generation.
    #[must_use]
    pub const fn activated(self) -> bool {
        self.activated
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "install pipeline failed during {:?}", self.phase)
    }
}
impl std::error::Error for PipelineError {}

/// Backend contract consumed by the phase-ordering state machine.
///
/// Concrete phase modules supply production building blocks; this trait is the
/// deterministic seam used for fault injection and future command-specific
/// desired-state editors.
pub trait InstallBackend {
    /// Resolve output carried into preflight.
    type Resolved;
    /// Preflight output carried into acquire.
    type Preflighted;
    /// Acquire output carried into verification.
    type Acquired;
    /// Verified output carried into staging.
    type Verified;
    /// Staged output carried into activation.
    type Staged;
    /// Activated output carried into commit.
    type Activated;
    /// Final successful output.
    type Committed;

    /// Resolves desired intent without realization.
    fn resolve(&mut self) -> Result<Self::Resolved, PipelineError>;
    /// Produces and enforces the mutation-free preview.
    fn preflight(&mut self, value: Self::Resolved) -> Result<Self::Preflighted, PipelineError>;
    /// Acquires exact outputs under the broker-held GC inhibit permit.
    fn acquire(&mut self, value: Self::Preflighted) -> Result<Self::Acquired, PipelineError>;
    /// Verifies every acquired output before it may enter activation.
    fn verify(&mut self, value: Self::Acquired) -> Result<Self::Verified, PipelineError>;
    /// Builds the Rust-only staging forest while current remains unchanged.
    fn stage(&mut self, value: Self::Verified) -> Result<Self::Staged, PipelineError>;
    /// Prepares state, roots outputs, and atomically activates.
    fn activate(&mut self, value: Self::Staged) -> Result<Self::Activated, PipelineError>;
    /// Finalizes current views and the committed journal row.
    fn commit(&mut self, value: Self::Activated) -> Result<Self::Committed, PipelineError>;
    /// Cleans unreachable pre-swap work after a normal failure.
    fn rollback_unactivated(&mut self, failed_phase: InstallPhase);
}

/// Runs exactly one install through all seven phases.
///
/// Any failure before the activation linearization point invokes cleanup and
/// leaves the previous generation active. A failure after activation is
/// returned with `activated=true`; startup recovery must finish it forward.
pub fn run_install<B: InstallBackend>(backend: &mut B) -> Result<B::Committed, PipelineError> {
    let result = (|| {
        let resolved = backend.resolve()?;
        let preflighted = backend.preflight(resolved)?;
        let acquired = backend.acquire(preflighted)?;
        let verified = backend.verify(acquired)?;
        let staged = backend.stage(verified)?;
        let activated = backend.activate(staged)?;
        backend.commit(activated)
    })();
    if let Err(error) = result
        && !error.activated()
    {
        backend.rollback_unactivated(error.phase());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<InstallPhase>,
        fail_at: Option<InstallPhase>,
        rollback: Option<InstallPhase>,
    }

    impl FakeBackend {
        fn phase(&mut self, phase: InstallPhase) -> Result<(), PipelineError> {
            self.calls.push(phase);
            if self.fail_at == Some(phase) {
                Err(PipelineError::new(phase, phase == InstallPhase::Commit))
            } else {
                Ok(())
            }
        }
    }

    impl InstallBackend for FakeBackend {
        type Resolved = ();
        type Preflighted = ();
        type Acquired = ();
        type Verified = ();
        type Staged = ();
        type Activated = ();
        type Committed = ();

        fn resolve(&mut self) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Resolve)
        }
        fn preflight(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Preflight)
        }
        fn acquire(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Acquire)
        }
        fn verify(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Verify)
        }
        fn stage(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Stage)
        }
        fn activate(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Activate)
        }
        fn commit(&mut self, (): ()) -> Result<(), PipelineError> {
            self.phase(InstallPhase::Commit)
        }
        fn rollback_unactivated(&mut self, failed_phase: InstallPhase) {
            self.rollback = Some(failed_phase);
        }
    }

    #[test]
    fn executes_exact_phase_order() {
        let mut backend = FakeBackend::default();
        run_install(&mut backend).unwrap();
        assert_eq!(
            backend.calls,
            [
                InstallPhase::Resolve,
                InstallPhase::Preflight,
                InstallPhase::Acquire,
                InstallPhase::Verify,
                InstallPhase::Stage,
                InstallPhase::Activate,
                InstallPhase::Commit,
            ]
        );
        assert_eq!(backend.rollback, None);
    }

    #[test]
    fn every_precommit_failure_rolls_back_but_post_activation_commit_finishes_forward() {
        for phase in [
            InstallPhase::Resolve,
            InstallPhase::Preflight,
            InstallPhase::Acquire,
            InstallPhase::Verify,
            InstallPhase::Stage,
            InstallPhase::Activate,
        ] {
            let mut backend = FakeBackend {
                fail_at: Some(phase),
                ..FakeBackend::default()
            };
            let error = run_install(&mut backend).unwrap_err();
            assert!(!error.activated());
            assert_eq!(backend.rollback, Some(phase));
        }
        let mut backend = FakeBackend {
            fail_at: Some(InstallPhase::Commit),
            ..FakeBackend::default()
        };
        let error = run_install(&mut backend).unwrap_err();
        assert!(error.activated());
        assert_eq!(backend.rollback, None);
    }
}
