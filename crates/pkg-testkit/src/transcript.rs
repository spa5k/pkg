//! The `pkg-testkit` error for a non-empty transcript at assertion time.
//!
//! This is deliberately a **separate** error type from
//! [`NixAdapterError`](pkg_nix::NixAdapterError): a trait call that misses the
//! head expectation is a `pkg-nix` contract failure surfaced through the
//! trait's `Result`; leftover expectations are a `pkg-testkit`-side concern
//! surfaced by [`FakeNix::assert_exhausted`](crate::FakeNix::assert_exhausted)
//! (`plans/09` §4.4). The two failure domains never share a type.

use std::fmt;

/// The error returned by
/// [`FakeNix::assert_exhausted`](crate::FakeNix::assert_exhausted) when the
/// transcript still holds unconsumed expectations.
///
/// Carries **only a remaining count** — never the leftover expectation values,
/// matchers, or canned results (`plans/09` §4.4). It is therefore safe to
/// print, log, or assert on without leaking any transcript contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptError {
    /// One or more expectations were never consumed by a trait call.
    UnmetExpectations {
        /// The number of expectations remaining in the transcript. This is a
        /// count only; no expectation contents are retained.
        remaining: usize,
    },
}

impl TranscriptError {
    /// Returns the number of unconsumed expectations, regardless of variant.
    ///
    /// Today there is exactly one variant ([`TranscriptError::UnmetExpectations`]);
    /// this accessor is stable as the type grows.
    #[must_use]
    pub const fn remaining(self) -> usize {
        match self {
            TranscriptError::UnmetExpectations { remaining } => remaining,
        }
    }
}

impl fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranscriptError::UnmetExpectations { remaining } => {
                write!(
                    f,
                    "unmet expectations: {remaining} expectation(s) were never consumed"
                )
            }
        }
    }
}

impl std::error::Error for TranscriptError {}
