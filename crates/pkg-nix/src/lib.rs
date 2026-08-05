//! `pkg-nix` — the validated Nix-adapter contract for `pkg`.
//!
//! This crate owns the **single object-safe boundary** between `pkg`'s
//! install/state pipeline and any concrete Nix backend (a hermetic `FakeNix`
//! or a real bundled-Nix adapter): the [`NixAdapter`] trait
//! (`plans/01` §11, `plans/09` §4.1). Only **validated, `pkg`-owned
//! request/report types** cross this boundary — never raw Nix CLI JSON, never
//! `serde_json::Value` (`plans/09` §4.2, T-DAEMON-2) — and the only error type
//! is the closed, redacted [`NixAdapterError`].
//!
//! # What this crate is *not*
//!
//! - It does **not** model raw, version-specific Nix CLI JSON shapes. Those are
//!   parsed by crate-private wire DTOs inside a *real* Nix adapter later
//!   (`plans/01` §11 footnote), which **requests an explicit upstream JSON
//!   format version per command** and **rejects any response whose format
//!   version it does not expect**, then normalizes into the `schemaVersion`-ed
//!   `pkg`-owned reports defined here. This crate owns the *normalized*
//!   contract, not the raw wire.
//! - `pkg-core` stays **serde-free**. Every serialized type in this crate is
//!   (de)serialized through **crate-private wire DTOs** with explicit,
//!   fallible conversion to the validated public types.
//!
//! # Design rules enforced here
//!
//! - **Object-safe, `Send + Sync` trait** with nine methods and **borrowed**
//!   inputs (`plans/09` §4.1).
//! - **No per-call trust/flag knobs.** None of `--substituters`,
//!   `--trusted-public-keys`, `--sandbox`, `--builders`, `--max-jobs`, an
//!   expression string, environment, or trust-policy fields appear on any
//!   request or report (`plans/01` §11.1; T-DAEMON-1/T-CACHE-1/T-BUILD-1).
//! - **Strict, deterministic, fail-closed decoding** through the public
//!   [`JsonCodec`]: every `pkg`-owned serialized report carries an explicit
//!   `schemaVersion` (currently [`SchemaVersion::CURRENT`] = 1), stable
//!   camelCase names, strict unknown-field rejection, default serde_json
//!   recursion protection, and rejection of malformed JSON, trailing data,
//!   oversized input, unsupported schemas, and invalid promoted values.
//! - **Bounded, redacted errors.** [`NixAdapterError`] never carries raw JSON,
//!   stdout/stderr, credentials, or unbounded paths — only stable closed codes
//!   and bounded redacted summaries.
//!
//! The intentional public surface is re-exported at the crate root below.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod adapter;
pub mod contract;
pub mod error;

pub use adapter::NixAdapter;
pub use contract::{
    AcceptedFormats, AddRootRequest, BuildApprovalReceipt, BuildReport, BuildRequest, BuildStatus,
    EvalRealizeRequest, FormatVersion, GcReport, GcStatus, JsonCodec, MethodKind, NarIntegrity,
    NixVersion, OperationId, PathInfoReport, PathRepairResult, PathVerifyResult, RealizationReport,
    RepairOutcome, RepairReport, RootName, RootRef, SchemaVersion, Signature, SubstituteOutcome,
    SubstituteReport, TrustStatus, VerifyMode, VerifyReport, VerifyRequest, VersionInfo,
};
pub use error::{MalformedKind, NixAdapterError, NixAdapterErrorCode};

// Focused re-exports of the `pkg-core` strong types that appear in this crate's
// public signatures, so consumers need only depend on `pkg-nix` to name them.
pub use pkg_core::channel::NixpkgsRevision;
pub use pkg_core::identity::{DerivationPath, NarHash, OutputName, StorePath};
pub use pkg_core::selector::{AttributePath, OutputSelection};
pub use pkg_core::system::System;
