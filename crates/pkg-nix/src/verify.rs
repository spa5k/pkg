//! Broker-side Phase-0 closure verification for two-phase repair.
//!
//! This module calls only [`NixAdapter::verify`]. It contains no repair method,
//! command construction, approval bypass, or privileged helper access.

use std::collections::BTreeMap;
use std::fmt;

use crate::{
    NarIntegrity, NixAdapter, StorePath, TrustStatus, VerifyMode, VerifyReport, VerifyRequest,
};

/// Stable Phase-0 failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPhaseErrorCode {
    /// The broker-derived closure was empty or contained duplicates.
    InvalidClosure,
    /// The read-only adapter call failed.
    AdapterFailure,
    /// The adapter report omitted, duplicated, or added a closure path.
    CoverageMismatch,
}

/// Redacted Phase-0 verification error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyPhaseError {
    code: VerifyPhaseErrorCode,
}

impl VerifyPhaseError {
    const fn new(code: VerifyPhaseErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable public error category.
    #[must_use]
    pub const fn code(self) -> VerifyPhaseErrorCode {
        self.code
    }
}

impl fmt::Display for VerifyPhaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.code {
            VerifyPhaseErrorCode::InvalidClosure => "the verified closure scope is invalid",
            VerifyPhaseErrorCode::AdapterFailure => "read-only store verification failed",
            VerifyPhaseErrorCode::CoverageMismatch => {
                "store verification did not cover the exact closure"
            }
        })
    }
}

impl std::error::Error for VerifyPhaseError {}

/// Exact Phase-0 result derived from the full rooted generation closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DamageSet {
    closure: Vec<StorePath>,
    damaged: Vec<StorePath>,
}

impl DamageSet {
    /// Returns every broker-derived closure path in canonical order.
    #[must_use]
    pub fn closure(&self) -> &[StorePath] {
        &self.closure
    }

    /// Returns corrupt, missing, or untrusted paths in canonical order.
    #[must_use]
    pub fn damaged(&self) -> &[StorePath] {
        &self.damaged
    }

    /// Whether a fresh full-closure verification found no damage.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.damaged.is_empty()
    }
}

/// Runs read-only recursive verification and requires exact result coverage.
///
/// # Errors
///
/// Returns a closed error for an invalid closure, adapter failure, or any
/// report whose path set is not exactly the requested full closure.
pub fn verify_closure(
    adapter: &dyn NixAdapter,
    closure: impl IntoIterator<Item = StorePath>,
) -> Result<DamageSet, VerifyPhaseError> {
    let closure = canonical_closure(closure)?;
    let request = VerifyRequest::new(closure.clone(), VerifyMode::Recursive)
        .map_err(|_| VerifyPhaseError::new(VerifyPhaseErrorCode::InvalidClosure))?;
    let report = adapter
        .verify(&request)
        .map_err(|_| VerifyPhaseError::new(VerifyPhaseErrorCode::AdapterFailure))?;
    classify_report(closure, &report)
}

fn canonical_closure(
    closure: impl IntoIterator<Item = StorePath>,
) -> Result<Vec<StorePath>, VerifyPhaseError> {
    let mut closure = closure.into_iter().collect::<Vec<_>>();
    closure.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let original_len = closure.len();
    closure.dedup_by(|left, right| left.as_str() == right.as_str());
    if closure.is_empty() || closure.len() != original_len {
        return Err(VerifyPhaseError::new(VerifyPhaseErrorCode::InvalidClosure));
    }
    Ok(closure)
}

fn classify_report(
    closure: Vec<StorePath>,
    report: &VerifyReport,
) -> Result<DamageSet, VerifyPhaseError> {
    let expected = closure
        .iter()
        .map(pkg_core::StorePath::as_str)
        .collect::<Vec<_>>();
    let mut results = BTreeMap::new();
    for result in report.results() {
        if results.insert(result.path().as_str(), result).is_some() {
            return Err(VerifyPhaseError::new(
                VerifyPhaseErrorCode::CoverageMismatch,
            ));
        }
    }
    if results.keys().copied().ne(expected.iter().copied()) {
        return Err(VerifyPhaseError::new(
            VerifyPhaseErrorCode::CoverageMismatch,
        ));
    }
    let damaged = closure
        .iter()
        .filter(|path| {
            let result = results.get(path.as_str());
            result.is_none_or(|result| {
                result.nar_integrity() != NarIntegrity::Intact
                    || result.trust() != TrustStatus::Trusted
            })
        })
        .cloned()
        .collect();
    Ok(DamageSet { closure, damaged })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PathVerifyResult;

    fn path(name: &str) -> Result<StorePath, pkg_core::IdentityError> {
        StorePath::new(&format!(
            "/nix/store/00000000000000000000000000000000-{name}"
        ))
    }

    fn code<T>(result: Result<T, VerifyPhaseError>) -> Option<VerifyPhaseErrorCode> {
        result.err().map(VerifyPhaseError::code)
    }

    #[test]
    fn exact_report_classifies_corrupt_missing_and_untrusted()
    -> Result<(), Box<dyn std::error::Error>> {
        let intact = path("intact")?;
        let corrupt = path("corrupt")?;
        let missing = path("missing")?;
        let untrusted = path("untrusted")?;
        let closure = canonical_closure([
            untrusted.clone(),
            intact.clone(),
            missing.clone(),
            corrupt.clone(),
        ])?;
        let report = VerifyReport::new(vec![
            PathVerifyResult::new(corrupt.clone(), NarIntegrity::Corrupt, TrustStatus::Trusted),
            PathVerifyResult::new(intact, NarIntegrity::Intact, TrustStatus::Trusted),
            PathVerifyResult::new(missing.clone(), NarIntegrity::Missing, TrustStatus::Trusted),
            PathVerifyResult::new(
                untrusted.clone(),
                NarIntegrity::Intact,
                TrustStatus::Untrusted,
            ),
        ])?;
        let damage = classify_report(closure, &report)?;
        assert_eq!(damage.damaged(), &[corrupt, missing, untrusted]);
        assert!(!damage.is_clean());
        Ok(())
    }

    #[test]
    fn closure_duplicates_and_report_coverage_fail_closed() -> Result<(), Box<dyn std::error::Error>>
    {
        let one = path("one")?;
        assert_eq!(
            code(canonical_closure([one.clone(), one.clone()])),
            Some(VerifyPhaseErrorCode::InvalidClosure)
        );
        let two = path("two")?;
        let report = VerifyReport::new(vec![PathVerifyResult::new(
            one.clone(),
            NarIntegrity::Intact,
            TrustStatus::Trusted,
        )])?;
        assert_eq!(
            code(classify_report(vec![one, two], &report)),
            Some(VerifyPhaseErrorCode::CoverageMismatch)
        );
        Ok(())
    }

    #[test]
    fn missing_integrity_round_trips_through_public_codec() -> Result<(), Box<dyn std::error::Error>>
    {
        let missing = path("missing")?;
        let report = VerifyReport::new(vec![PathVerifyResult::new(
            missing,
            NarIntegrity::Missing,
            TrustStatus::Trusted,
        )])?;
        let bytes = report.encode()?;
        assert_eq!(
            VerifyReport::decode(&crate::JsonCodec::default(), &bytes)?,
            report
        );
        Ok(())
    }
}
