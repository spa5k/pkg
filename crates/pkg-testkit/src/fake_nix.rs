//! The deterministic, exact-FIFO [`FakeNix`] — a `NixAdapter` replay engine.
//!
//! PR-3 slice 2 implements the transcript replay engine defined in
//! `plans/09` §4.4. A [`FakeNix`] holds an ordered transcript of expectations
//! behind thread-safe interior mutability. Every
//! [`NixAdapter`] call **consumes exactly the head
//! expectation** (first-in-first-out): the call's method kind and request must
//! match the head exactly, and a matching call returns the head's owned canned
//! `Result`. A call that misses the head — wrong method, or same method but a
//! non-equal request — returns the closed, redacted
//! [`NixAdapterError::UnexpectedCall`] and still
//! consumes the head, so the transcript always advances by exactly one per
//! call. When the test is finished, [`FakeNix::assert_exhausted`] confirms no
//! expectations remain.
//!
//! [`FakeNix`] **never panics**: not on an unexpected call, not on an empty
//! transcript, not on drop. It uses `std` only, contains no `unsafe` code, and
//! performs no network, Nix-process, or timing operations (`plans/09` §3,
//! §4.4).
//!
//! # Extra call against an empty/exhausted transcript
//!
//! A call that arrives when the transcript holds **no head** (it was empty to
//! begin with, or every expectation has been consumed) has no expectation to
//! match, so there is no honest `expected: MethodKind`. Rather than fabricate
//! one or fall back to a generic backend error, [`FakeNix`] returns the
//! dedicated, redacted [`NixAdapterError::UnexpectedExtraCall`] (constructed
//! via [`NixAdapterError::unexpected_extra_call`]) and consumes nothing. It
//! reuses the single `NixAdapterErrorCode::UnexpectedCall` code, reports
//! `expected_method() == None`, `actual_method() == Some(actual)`, and
//! `mismatch_summary() == Some("extra call")`, and its `Display` truthfully
//! states that no expectation remained — the `pkg-nix` contract's honest
//! sibling for the no-head case (`plans/09` §4.4). An ignored extra-call
//! error is **not** later observable via [`FakeNix::assert_exhausted`]: the
//! transcript stays empty and consumes nothing, so exhaustion still reports
//! `Ok(())`.
//!
//! # Concurrency
//!
//! Exact FIFO result ordering is deterministic only for **serialized**
//! access. Under concurrent callers, mutex acquisition determines which caller
//! consumes which head: every call still consumes exactly one head, but the
//! caller-to-result assignment is nondeterministic, so concurrent tests should
//! assert a multiset of results rather than per-thread order.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

use pkg_nix::{
    AddRootRequest, BuildReport, BuildRequest, EvalRealizeRequest, GcReport, MethodKind,
    NixAdapter, NixAdapterError, PathInfoReport, RealizationReport, RootRef, StorePath,
    SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};

use crate::transcript::TranscriptError;

/// One expectation in a [`FakeNix`] exact-FIFO transcript (`plans/09` §4.4).
///
/// Each variant names a method, carries the **exact typed request matcher** the
/// head call must equal (for request-bearing methods), and the **owned canned
/// `Result`** returned for a matching call. The type is crate-private: nothing
/// about a stored matcher or canned result is exposed across the crate
/// boundary, and the public expectation API is the typed
/// [`FakeNix::expect_version`] / [`FakeNix::expect_eval_realize`] / … methods.
enum Expectation {
    /// `version()` — no request.
    Version {
        /// The owned canned result returned for a matching call.
        respond: Result<VersionInfo, NixAdapterError>,
    },
    /// `eval_realize()` — exact request matcher + canned result.
    EvalRealize {
        /// The exact request the head call must equal.
        expect: EvalRealizeRequest,
        /// The owned canned result returned for a matching call.
        respond: Result<RealizationReport, NixAdapterError>,
    },
    /// `path_info()` — exact store-path matcher + canned result.
    PathInfo {
        /// The exact store path the head call must equal.
        expect: StorePath,
        /// The owned canned result returned for a matching call.
        respond: Result<PathInfoReport, NixAdapterError>,
    },
    /// `substitute()` — exact store-path matcher + canned result.
    Substitute {
        /// The exact store path the head call must equal.
        expect: StorePath,
        /// The owned canned result returned for a matching call.
        respond: Result<SubstituteReport, NixAdapterError>,
    },
    /// `build()` — exact request matcher + canned result.
    Build {
        /// The exact request the head call must equal.
        expect: BuildRequest,
        /// The owned canned result returned for a matching call.
        respond: Result<BuildReport, NixAdapterError>,
    },
    /// `verify()` — exact request matcher + canned result.
    Verify {
        /// The exact request the head call must equal.
        expect: VerifyRequest,
        /// The owned canned result returned for a matching call.
        respond: Result<VerifyReport, NixAdapterError>,
    },
    /// `gc()` — no request.
    Gc {
        /// The owned canned result returned for a matching call.
        respond: Result<GcReport, NixAdapterError>,
    },
    /// `add_root()` — exact request matcher + canned result.
    AddRoot {
        /// The exact request the head call must equal.
        expect: AddRootRequest,
        /// The owned canned result returned for a matching call.
        respond: Result<RootRef, NixAdapterError>,
    },
}

impl Expectation {
    /// Returns the [`MethodKind`] of this expectation.
    fn kind(&self) -> MethodKind {
        match self {
            Expectation::Version { .. } => MethodKind::Version,
            Expectation::EvalRealize { .. } => MethodKind::EvalRealize,
            Expectation::PathInfo { .. } => MethodKind::PathInfo,
            Expectation::Substitute { .. } => MethodKind::Substitute,
            Expectation::Build { .. } => MethodKind::Build,
            Expectation::Verify { .. } => MethodKind::Verify,
            Expectation::Gc { .. } => MethodKind::Gc,
            Expectation::AddRoot { .. } => MethodKind::AddRoot,
        }
    }
}

/// A deterministic, exact-FIFO-transcript fake [`NixAdapter`] (`plans/09`
/// §4.4).
///
/// Build a transcript with the typed `expect_*` methods, drive it through the
/// [`NixAdapter`] trait (directly, behind `Box<dyn NixAdapter>` /
/// `Arc<dyn NixAdapter>`, or shared across threads), then call
/// [`FakeNix::assert_exhausted`] to confirm every expectation was consumed in
/// order. See the [module docs](crate::fake_nix) for the exact-FIFO replay
/// rules and the no-head extra-call behavior.
///
/// # Thread safety
///
/// [`FakeNix`] is `Send + Sync`: its transcript lives behind a `std::sync`
/// `Mutex`. Mutex **poisoning is handled without panicking**: a poisoned lock
/// is recovered (the inner guard is taken anyway), so a panic on one thread
/// never propagates as a lock-acquisition panic on another. In practice the
/// `FakeNix` methods themselves never panic.
///
/// Exact FIFO result ordering is deterministic only for **serialized**
/// access. Under concurrent callers, mutex acquisition determines which caller
/// consumes which head, so every call still consumes exactly one head but the
/// caller-to-result assignment is nondeterministic; concurrent tests should
/// assert a multiset of results rather than per-thread order.
pub struct FakeNix {
    transcript: Mutex<VecDeque<Expectation>>,
}

impl FakeNix {
    /// Creates an empty `FakeNix` (no expectations).
    #[must_use]
    pub fn new() -> Self {
        Self {
            transcript: Mutex::new(VecDeque::new()),
        }
    }

    /// Pushes an expectation onto the **back** of the transcript (FIFO).
    fn push(&self, expectation: Expectation) -> &Self {
        let mut guard = self.lock();
        guard.push_back(expectation);
        self
    }

    /// Acquires the transcript lock, recovering from poisoning without
    /// panicking (`plans/09` §4.4, "handle mutex poisoning without panicking").
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Expectation>> {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Pops and returns the **head** expectation, or returns the redacted
    /// [`NixAdapterError::UnexpectedExtraCall`] (via
    /// [`NixAdapterError::unexpected_extra_call`]) if the transcript holds no
    /// head (it was empty or fully consumed).
    ///
    /// Every non-empty trait call consumes exactly one expectation via this
    /// method, so the transcript always advances by exactly one per call —
    /// including mismatched calls (which then also return an error). An
    /// empty/exhausted transcript has no head expectation, so there is no
    /// honest `expected: MethodKind`; the dedicated `UnexpectedExtraCall`
    /// variant names that truthfully (see the [module docs](crate::fake_nix)).
    fn take_head(&self, actual: MethodKind) -> Result<Expectation, NixAdapterError> {
        self.lock()
            .pop_front()
            .ok_or_else(|| NixAdapterError::unexpected_extra_call(actual))
    }

    // ---------------------------------------------------------------------
    // Typed public expectation API (plans/09 §4.4).
    //
    // Each method pushes one expectation carrying the exact typed request
    // matcher (where the method has a request) and the owned canned Result
    // with the correct report type. All return &Self for chaining.
    // ---------------------------------------------------------------------

    /// Expects exactly one `version()` call and returns the owned canned
    /// result for it.
    pub fn expect_version(&self, respond: Result<VersionInfo, NixAdapterError>) -> &Self {
        self.push(Expectation::Version { respond })
    }

    /// Expects exactly one `eval_realize()` call whose request equals `expect`
    /// exactly, and returns the owned canned result for it.
    pub fn expect_eval_realize(
        &self,
        expect: EvalRealizeRequest,
        respond: Result<RealizationReport, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::EvalRealize { expect, respond })
    }

    /// Expects exactly one `path_info()` call whose store path equals `expect`
    /// exactly, and returns the owned canned result for it.
    pub fn expect_path_info(
        &self,
        expect: StorePath,
        respond: Result<PathInfoReport, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::PathInfo { expect, respond })
    }

    /// Expects exactly one `substitute()` call whose store path equals
    /// `expect` exactly, and returns the owned canned result for it.
    pub fn expect_substitute(
        &self,
        expect: StorePath,
        respond: Result<SubstituteReport, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::Substitute { expect, respond })
    }

    /// Expects exactly one `build()` call whose request equals `expect`
    /// exactly, and returns the owned canned result for it.
    pub fn expect_build(
        &self,
        expect: BuildRequest,
        respond: Result<BuildReport, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::Build { expect, respond })
    }

    /// Expects exactly one `verify()` call whose request equals `expect`
    /// exactly, and returns the owned canned result for it.
    pub fn expect_verify(
        &self,
        expect: VerifyRequest,
        respond: Result<VerifyReport, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::Verify { expect, respond })
    }

    /// Expects exactly one `gc()` call and returns the owned canned result for
    /// it.
    pub fn expect_gc(&self, respond: Result<GcReport, NixAdapterError>) -> &Self {
        self.push(Expectation::Gc { respond })
    }

    /// Expects exactly one `add_root()` call whose request equals `expect`
    /// exactly, and returns the owned canned result for it.
    pub fn expect_add_root(
        &self,
        expect: AddRootRequest,
        respond: Result<RootRef, NixAdapterError>,
    ) -> &Self {
        self.push(Expectation::AddRoot { expect, respond })
    }

    /// Returns `Ok(())` if every expectation was consumed in order, or
    /// `Err(TranscriptError::UnmetExpectations { remaining })` carrying only
    /// the remaining count (`plans/09` §4.4).
    ///
    /// This is **not** a [`NixAdapter`] method and never
    /// leaks leftover expectation contents — only a count. An extra call
    /// against an empty/exhausted transcript is reported only via the returned
    /// `Result` of the trait method that made it; if that error is ignored, it
    /// is **not** later observable here (the transcript stays empty and
    /// consumes nothing), so exhaustion still reports `Ok(())`.
    pub fn assert_exhausted(&self) -> Result<(), TranscriptError> {
        let remaining = self.lock().len();
        if remaining == 0 {
            Ok(())
        } else {
            Err(TranscriptError::UnmetExpectations { remaining })
        }
    }
}

impl Default for FakeNix {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FakeNix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Debug exposes ONLY the remaining count — never any matcher, canned
        // result, or transcript value (plans/09 §4.4).
        let remaining = self.lock().len();
        f.debug_struct("FakeNix")
            .field("remaining", &remaining)
            .finish()
    }
}

impl NixAdapter for FakeNix {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        let head = self.take_head(MethodKind::Version)?;
        match head {
            Expectation::Version { respond } => respond,
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::Version,
            )),
        }
    }

    fn eval_realize(&self, req: &EvalRealizeRequest) -> Result<RealizationReport, NixAdapterError> {
        let head = self.take_head(MethodKind::EvalRealize)?;
        match head {
            Expectation::EvalRealize { expect, respond } => {
                if req == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::EvalRealize,
                        MethodKind::EvalRealize,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::EvalRealize,
            )),
        }
    }

    fn path_info(&self, path: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        let head = self.take_head(MethodKind::PathInfo)?;
        match head {
            Expectation::PathInfo { expect, respond } => {
                if path == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::PathInfo,
                        MethodKind::PathInfo,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::PathInfo,
            )),
        }
    }

    fn substitute(&self, path: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        let head = self.take_head(MethodKind::Substitute)?;
        match head {
            Expectation::Substitute { expect, respond } => {
                if path == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::Substitute,
                        MethodKind::Substitute,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::Substitute,
            )),
        }
    }

    fn build(&self, req: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        let head = self.take_head(MethodKind::Build)?;
        match head {
            Expectation::Build { expect, respond } => {
                if req == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::Build,
                        MethodKind::Build,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::Build,
            )),
        }
    }

    fn verify(&self, req: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        let head = self.take_head(MethodKind::Verify)?;
        match head {
            Expectation::Verify { expect, respond } => {
                if req == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::Verify,
                        MethodKind::Verify,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::Verify,
            )),
        }
    }

    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        let head = self.take_head(MethodKind::Gc)?;
        match head {
            Expectation::Gc { respond } => respond,
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::Gc,
            )),
        }
    }

    fn add_root(&self, req: &AddRootRequest) -> Result<RootRef, NixAdapterError> {
        let head = self.take_head(MethodKind::AddRoot)?;
        match head {
            Expectation::AddRoot { expect, respond } => {
                if req == &expect {
                    respond
                } else {
                    Err(NixAdapterError::unexpected_call(
                        MethodKind::AddRoot,
                        MethodKind::AddRoot,
                    ))
                }
            }
            other => Err(NixAdapterError::unexpected_call(
                other.kind(),
                MethodKind::AddRoot,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    //! White-box unit test for mutex-poisoning recovery. The internal mutex is
    //! private, so only a same-module test can poison it directly to prove the
    //! `unwrap_or_else(|p| p.into_inner())` recovery path never panics.

    use super::*;
    use pkg_nix::{AcceptedFormats, FormatVersion, NixVersion};

    fn version_info() -> VersionInfo {
        VersionInfo::new(
            NixVersion::new("2.33.5").unwrap(),
            AcceptedFormats::new(FormatVersion::new(1).unwrap()),
        )
    }

    #[test]
    fn lock_recovers_from_poison_without_panicking() {
        let fake = std::sync::Arc::new(FakeNix::new());

        // Poison the internal mutex by panicking while holding it on another
        // thread. (Production code never panics while holding the lock; this
        // deliberately induces the poisoned state to exercise the recovery.)
        let poisoned = fake.clone();
        let join = std::thread::spawn(move || {
            let _guard = poisoned.transcript.lock().unwrap();
            panic!("intentional poison for test");
        });
        assert!(join.join().is_err(), "poisoning thread must panic");

        // Recovery: every subsequent operation acquires the lock via
        // `unwrap_or_else(|p| p.into_inner())` and must NOT panic.
        fake.expect_version(Ok(version_info()));
        assert_eq!(fake.version().unwrap().nix_version().as_str(), "2.33.5");
        assert_eq!(fake.assert_exhausted(), Ok(()));
    }
}
