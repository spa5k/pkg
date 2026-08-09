//! `pkg-testkit` — the hermetic, deterministic `FakeNix` for `pkg`'s test
//! layers 1–4 (Fake) (`plans/09` §4).
//!
//! PR-3 slice 2 ships the **deterministic exact-FIFO transcript replay** engine
//! defined in `plans/09` §4.4: a [`FakeNix`] that implements all seven
//! [`NixAdapter`] methods and replays a scripted,
//! first-in-first-out transcript of expectations with byte-stable, hermetic
//! outputs — **no Nix process, no network, no timing** (`plans/09` §3, §4.4).
//! Richer simulation (keyed responses, latency, partial writes, fake cache/CDN,
//! chaos) is explicitly deferred to later checkpoints (`plans/09` §4.5).
//! [`FakeNixpkgsRunner`] separately replays the closed pinned-source metadata
//! seam introduced by PR-13; it cannot widen the seven-method adapter.
//!
//! # Dependencies (one way)
//!
//! `pkg-testkit` depends on `pkg-nix` one way and **never** the reverse
//! (`plans/09` §4.4). `pkg-nix` owns the [`NixAdapter`]
//! trait, the request/report contract types, and the closed/redacted
//! [`NixAdapterError`]; `pkg-testkit` consumes them.
//! There is no `pkg-nix` → `pkg-testkit` edge.
//!
//! # Transcript model
//!
//! A [`FakeNix`] holds an ordered transcript of expectations. Every trait call
//! consumes exactly the **head** expectation (first-in-first-out): the call's
//! method kind and request must match the head exactly, and a matching call
//! returns the head's owned canned `Result`. A wrong-method call, or a
//! same-method call whose request does not equal the head matcher, returns the
//! closed, redacted [`NixAdapterError`] and still consumes
//! the head, so the transcript always advances by exactly one per call. When
//! the test is finished, [`FakeNix::assert_exhausted`] returns
//! `Result<(), TranscriptError>` and a non-empty transcript yields
//! [`TranscriptError::UnmetExpectations`] carrying **only a remaining count**
//! — never the leftover expectation values (`plans/09` §4.4).
//!
//! [`FakeNix`] **never panics**: not on an unexpected call, not on an empty
//! transcript, not on drop. It uses `std` only, contains no `unsafe` code, and
//! performs no network, Nix-process, or timing operations.
//!
//! # Extra call against an empty/exhausted transcript
//!
//! A call that arrives when the transcript holds no head (initially empty, or
//! every expectation consumed) has no expectation to match, so there is no
//! honest `expected: MethodKind`. [`FakeNix`] returns the dedicated, redacted
//! [`NixAdapterError::UnexpectedExtraCall`] (via
//! [`NixAdapterError::unexpected_extra_call`]) for that case and consumes
//! nothing — never a synthetic expected value and never a generic backend
//! error. See the [`fake_nix`] module docs for the full replay rules.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod fake_nix;
pub mod fake_nixpkgs;
pub mod transcript;

pub use fake_nix::FakeNix;
pub use fake_nixpkgs::{FakeNixpkgsError, FakeNixpkgsRunner};
pub use transcript::TranscriptError;

// Focused re-export of the `pkg-nix` contract types that appear in `FakeNix`'s
// public signatures, so a consumer need depend only on `pkg-testkit` to name
// them. Construction helpers (NixVersion, OutputName, …) remain available by
// depending on `pkg-nix` directly.
pub use pkg_nix::{
    BuildReport, BuildRequest, DerivationPlanReport, EvaluateDerivationRequest, GcReport,
    MethodKind, NixAdapter, NixAdapterError, NixpkgsMetadataCommand, NixpkgsMetadataRunner,
    NixpkgsSourceError, PathInfoReport, StorePath, SubstituteReport, VerifyReport, VerifyRequest,
    VersionInfo,
};
