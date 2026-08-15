//! The object-safe [`NixAdapter`] trait: the **single boundary** between
//! `pkg`'s install/state pipeline and any concrete Nix backend (a hermetic
//! `FakeNix` or a real bundled-Nix adapter).
//!
//! `plans/01` §11 defines the canonical set of Nix invocations; `plans/09`
//! §4.1 defines this trait. Only **validated, `pkg`-owned request/report
//! types** cross this boundary — never raw Nix CLI JSON, never
//! `serde_json::Value` (`plans/09` §4.2, T-DAEMON-2) — and the only error type
//! is the closed, redacted [`NixAdapterError`].
//!
//! # Object safety and `Send + Sync`
//!
//! [`NixAdapter`] is **object-safe**: every method takes `&self`, none are
//! generic, and none return `Self`. It is also `Send + Sync`, so it can live
//! behind an `Arc<dyn NixAdapter>` shared across the journal/worker threads.
//! Each method takes its request **by reference** (or no input at all) and
//! returns the corresponding validated report or a
//! [`NixAdapterError`].
//!
//! # No per-call trust/flag knobs
//!
//! There are **no** per-call trust, substituter, key, sandbox, builder,
//! build-user, `max-jobs`, expression-string, environment, or policy knobs on
//! any method or request type (`plans/01` §11.1; T-DAEMON-1/T-CACHE-1/
//! T-BUILD-1). All trust and build enforcement is fixed once, at adapter
//! construction / managed-runtime config time, sourced from the signed channel
//! descriptor and the channel-locked `/opt/pkg/etc/pkg/nix.conf` (INV-03), and
//! is **immutable for the life of the adapter**. The trait therefore accepts
//! only selector/store-path/realization inputs plus already-pinned identifiers
//! — never `--substituters`, `--trusted-public-keys`, `--sandbox`,
//! `--builders`, or an expression string.
//!
//! [`BuildApprovalReceipt`](crate::BuildApprovalReceipt) is a **bounded opaque
//! operation-id carrier**, not proof of authorization: PR-3 defines only this
//! stable carrier and its validation; PR-26 owns its production issuance,
//! journal binding, single-use verification, and rejection (`plans/09` §4.1).

use crate::contract::{
    BuildReport, BuildRequest, DerivationPlanReport, EvaluateDerivationRequest, GcReport,
    PathInfoReport, SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use crate::error::NixAdapterError;
use pkg_core::identity::StorePath;

/// Sanitized best-effort completion estimate for one approved local build.
///
/// The value uses millionths so it crosses broker boundaries without
/// floating-point ambiguity. Live estimates stay below completion;
/// successful report validation is the only source of terminal 100%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BuildProgressEstimate(u32);

impl BuildProgressEstimate {
    /// Number of units in one complete build.
    pub const SCALE: u32 = 1_000_000;

    /// Creates one non-terminal live estimate.
    ///
    /// # Errors
    ///
    /// Refuses zero and terminal-or-larger values.
    pub fn new(millionths: u32) -> Result<Self, NixAdapterError> {
        if millionths == 0 || millionths >= Self::SCALE {
            return Err(NixAdapterError::OperationFailed);
        }
        Ok(Self(millionths))
    }

    /// Returns the fixed-point millionths value.
    #[must_use]
    pub const fn millionths(self) -> u32 {
        self.0
    }
}

/// The single object-safe boundary between `pkg` and any concrete Nix backend
/// (`plans/09` §4.1).
///
/// See the [module docs](self) for the object-safety, `Send + Sync`, and
/// no-per-call-knobs guarantees. There are exactly seven methods; the
/// [`MethodKind`](crate::MethodKind) enum enumerates them in the same order for
/// the `pkg-testkit` transcript replay engine (`plans/09` §4.4). The progress
/// method is an observation form of `build`, not another upstream operation.
pub trait NixAdapter: Send + Sync {
    /// Pinned managed-Nix version and the upstream per-command JSON format
    /// versions this adapter accepts/rejects (`plans/01` §11). A read-only
    /// capability probe taking no input.
    fn version(&self) -> Result<VersionInfo, NixAdapterError>;

    /// Evaluate a selector into a deterministic derivation plan without
    /// substituting, building, realizing, or mutating the store.
    fn evaluate_derivation(
        &self,
        req: &EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, NixAdapterError>;

    /// NAR hash, signatures, references, and NAR/closure sizes for one store
    /// path.
    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError>;

    /// Substitute (download) one path under the adapter's pinned trust set.
    /// Trust/signature failures are
    /// [`NixAdapterError`], never a report outcome.
    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError>;

    /// Substitute a bounded set under the same fixed trust policy.
    ///
    /// The default preserves adapters that expose only the seven upstream
    /// operations. Concrete adapters may batch the fixed operation.
    fn substitute_many(
        &self,
        paths: &[StorePath],
    ) -> Result<Vec<SubstituteReport>, NixAdapterError> {
        paths.iter().map(|path| self.substitute(path)).collect()
    }

    /// Approved, sandboxed local build. No per-call trust/flag toggles.
    fn build(&self, req: &BuildRequest) -> Result<BuildReport, NixAdapterError>;

    /// Approved local build with sanitized best-effort live estimates.
    ///
    /// Adapters without a trusted live source keep the stable build contract
    /// and emit no estimates.
    fn build_with_progress(
        &self,
        req: &BuildRequest,
        _progress: &mut dyn FnMut(BuildProgressEstimate) -> Result<(), NixAdapterError>,
    ) -> Result<BuildReport, NixAdapterError> {
        self.build(req)
    }

    /// Read-only integrity/trust verification. Never mutates the store.
    fn verify(&self, req: &VerifyRequest) -> Result<VerifyReport, NixAdapterError>;

    /// Collect unreachable paths. Consults the on-disk gcroots tree — there is
    /// no roots argument (`plans/01` ARCH-INV-04, `plans/05` T-STATE-4).
    fn gc(&self) -> Result<GcReport, NixAdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_build_estimates_are_strictly_non_terminal() {
        assert!(BuildProgressEstimate::new(0).is_err());
        assert_eq!(BuildProgressEstimate::new(1).unwrap().millionths(), 1);
        assert_eq!(
            BuildProgressEstimate::new(BuildProgressEstimate::SCALE - 1)
                .unwrap()
                .millionths(),
            999_999
        );
        assert!(BuildProgressEstimate::new(BuildProgressEstimate::SCALE).is_err());
    }
}
