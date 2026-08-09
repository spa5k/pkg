//! Exact-FIFO fake for the closed Nixpkgs metadata execution seam.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

use pkg_nix::{NixpkgsMetadataCommand, NixpkgsMetadataRunner, NixpkgsSourceError};

struct Expectation {
    command: NixpkgsMetadataCommand,
    response: Result<Vec<u8>, NixpkgsSourceError>,
}

/// Hermetic one-shot transcript fake for pinned-source metadata calls.
pub struct FakeNixpkgsRunner {
    transcript: Mutex<VecDeque<Expectation>>,
}

impl FakeNixpkgsRunner {
    /// Creates an empty transcript.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transcript: Mutex::new(VecDeque::new()),
        }
    }

    /// Appends one exact command and canned owned result.
    pub fn expect_metadata(
        &self,
        command: NixpkgsMetadataCommand,
        response: Result<Vec<u8>, NixpkgsSourceError>,
    ) -> &Self {
        self.lock().push_back(Expectation { command, response });
        self
    }

    /// Confirms every expected metadata call was consumed.
    pub fn assert_exhausted(&self) -> Result<(), FakeNixpkgsError> {
        let remaining = self.lock().len();
        if remaining == 0 {
            Ok(())
        } else {
            Err(FakeNixpkgsError { remaining })
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Expectation>> {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for FakeNixpkgsRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FakeNixpkgsRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeNixpkgsRunner")
            .field("remaining", &self.lock().len())
            .finish()
    }
}

impl NixpkgsMetadataRunner for FakeNixpkgsRunner {
    fn run_metadata(
        &self,
        command: &NixpkgsMetadataCommand,
    ) -> Result<Vec<u8>, NixpkgsSourceError> {
        let Some(expectation) = self.lock().pop_front() else {
            return Err(NixpkgsSourceError::runner_failure());
        };
        if expectation.command != *command {
            return Err(NixpkgsSourceError::runner_failure());
        }
        expectation.response
    }
}

/// Redacted non-exhaustion diagnostic for source transcripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeNixpkgsError {
    remaining: usize,
}

impl FakeNixpkgsError {
    /// Returns the count of unconsumed expectations.
    #[must_use]
    pub const fn remaining(self) -> usize {
        self.remaining
    }
}

impl fmt::Display for FakeNixpkgsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Nixpkgs metadata transcript has {} remaining call(s)",
            self.remaining
        )
    }
}

impl std::error::Error for FakeNixpkgsError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_runner<T: NixpkgsMetadataRunner>() {}

    #[test]
    fn fake_is_send_sync_runner_and_empty_transcript_is_exhausted() {
        assert_runner::<FakeNixpkgsRunner>();
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FakeNixpkgsRunner>();

        let fake = FakeNixpkgsRunner::new();
        assert!(fake.assert_exhausted().is_ok());
        assert_eq!(format!("{fake:?}"), "FakeNixpkgsRunner { remaining: 0 }");
    }
}
