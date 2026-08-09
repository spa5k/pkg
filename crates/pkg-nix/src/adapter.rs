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
    BuildReport, BuildRequest, EvalRealizeRequest, GcReport, PathInfoReport, RealizationReport,
    SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use crate::error::NixAdapterError;
use pkg_core::identity::StorePath;

/// The single object-safe boundary between `pkg` and any concrete Nix backend
/// (`plans/09` §4.1).
///
/// See the [module docs](self) for the object-safety, `Send + Sync`, and
/// no-per-call-knobs guarantees. There are exactly seven methods; the
/// [`MethodKind`](crate::MethodKind) enum enumerates them in the same order for
/// the `pkg-testkit` transcript replay engine (`plans/09` §4.4).
pub trait NixAdapter: Send + Sync {
    /// Pinned managed-Nix version and the upstream per-command JSON format
    /// versions this adapter accepts/rejects (`plans/01` §11). A read-only
    /// capability probe taking no input.
    fn version(&self) -> Result<VersionInfo, NixAdapterError>;

    /// Evaluate and realize a selector into a store path, deriver, and outputs.
    fn eval_realize(&self, req: &EvalRealizeRequest) -> Result<RealizationReport, NixAdapterError>;

    /// NAR hash, signatures, references, and NAR/closure sizes for one store
    /// path.
    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError>;

    /// Substitute (download) one path under the adapter's pinned trust set.
    /// Trust/signature failures are
    /// [`NixAdapterError`], never a report outcome.
    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError>;

    /// Approved, sandboxed local build. No per-call trust/flag toggles.
    fn build(&self, req: &BuildRequest) -> Result<BuildReport, NixAdapterError>;

    /// Read-only integrity/trust verification. Never mutates the store.
    fn verify(&self, req: &VerifyRequest) -> Result<VerifyReport, NixAdapterError>;

    /// Collect unreachable paths. Consults the on-disk gcroots tree — there is
    /// no roots argument (`plans/01` ARCH-INV-04, `plans/05` T-STATE-4).
    fn gc(&self) -> Result<GcReport, NixAdapterError>;
}
