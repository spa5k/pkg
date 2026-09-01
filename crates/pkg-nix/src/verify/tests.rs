//! Tests for the `verify` module.

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
fn exact_report_classifies_corrupt_missing_and_untrusted() -> Result<(), Box<dyn std::error::Error>>
{
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
fn closure_duplicates_and_report_coverage_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
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
fn missing_integrity_round_trips_through_public_codec() -> Result<(), Box<dyn std::error::Error>> {
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
