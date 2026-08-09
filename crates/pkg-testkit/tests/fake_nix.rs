//! Black-box integration tests for `pkg-testkit`'s `FakeNix` (`plans/09`
//! §4.4).
//!
//! These tests treat `pkg-testkit` purely as an external consumer: they
//! exercise only its public API (`FakeNix`, `TranscriptError`, and the
//! re-exported `pkg-nix` contract types) and the public `NixAdapter` trait.
//! They are hermetic and deterministic: no network, no Nix process, no timing,
//! no `#[ignore]`, no `should_panic`, and no `todo!`/`unimplemented!`.
//!
//! Coverage map (acceptance criterion #8):
//!
//! - All seven `NixAdapter` methods return their canned results.
//! - Exact first-in-first-out ordering.
//! - Exact request matching for every request-bearing method.
//! - Canned `Ok` and canned `Err` results.
//! - Wrong-method and same-method request mismatches return the redacted
//!   `UnexpectedCall` and consume the head.
//! - Extra calls / empty transcript.
//! - `assert_exhausted` reports the remaining count only.
//! - Safe bounded `Display`/`Debug` (no raw request or transcript data leaks).
//! - `Send + Sync`, `dyn NixAdapter` dispatch, and concurrent access.
//! - No reverse dependency (`pkg-nix` never names `pkg-testkit`).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use pkg_nix::{
    AcceptedFormats, AttributePath, BuildApprovalReceipt, BuildReport, BuildRequest, BuildStatus,
    DerivationPath, DerivationPlanReport, Digest, EvaluateDerivationRequest, EvaluatedDerivation,
    FormatVersion, GcReport, GcStatus, MalformedKind, MethodKind, NarHash, NarIntegrity,
    NixAdapter, NixAdapterError, NixAdapterErrorCode, NixVersion, NixpkgsRevision, OperationId,
    OutputName, OutputSelection, PackageVersion, PathInfoReport, PathVerifyResult, Signature,
    StorePath, SubstituteOutcome, SubstituteReceipt, SubstituteReport, System, TrustStatus,
    VerifyMode, VerifyReport, VerifyRequest, VersionInfo,
};
use pkg_testkit::{FakeNix, TranscriptError};

// Compile-time check: the contract types that appear in `FakeNix`'s public
// signatures are nameable directly from the `pkg_testkit` re-exports (no need
// to depend on `pkg-nix` to call the API).
const _: () = {
    let _: std::marker::PhantomData<pkg_testkit::VersionInfo> = std::marker::PhantomData;
    let _: std::marker::PhantomData<pkg_testkit::StorePath> = std::marker::PhantomData;
    let _: std::marker::PhantomData<Box<dyn pkg_testkit::NixAdapter>> = std::marker::PhantomData;
    let _: std::marker::PhantomData<pkg_testkit::MethodKind> = std::marker::PhantomData;
};

// ===========================================================================
// Fixed, valid fixtures (deterministic). Mirrors the pkg-nix contract tests.
// ===========================================================================

const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

fn store_path(name: &str) -> StorePath {
    StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
}

fn drv(name: &str) -> DerivationPath {
    DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-{name}.drv")).unwrap()
}

fn sig(name: &str) -> Signature {
    Signature::new(&format!("{name}:BBBBBBBB")).unwrap()
}

fn nar_hash() -> NarHash {
    NarHash::new(NAR).unwrap()
}

fn nixpkgs_revision() -> NixpkgsRevision {
    NixpkgsRevision::new(REV).unwrap()
}

fn version_info(major: u32) -> VersionInfo {
    VersionInfo::new(
        NixVersion::new(&format!("2.{major}.5")).unwrap(),
        AcceptedFormats::new(FormatVersion::new(1).unwrap()),
    )
}

fn realization_fixture() -> DerivationPlanReport {
    let root = drv("hello-1.0");
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), store_path("hello-1.0"));
    let evaluated = EvaluatedDerivation::new(
        root.clone(),
        "hello-1.0".into(),
        System::X8664Linux,
        outputs,
        Digest::from_bytes([1; 32]),
        false,
    )
    .unwrap();
    DerivationPlanReport::new(
        4,
        root,
        vec![OutputName::new("out").unwrap()],
        vec![evaluated],
        Digest::from_bytes([2; 32]),
        "hello".into(),
        PackageVersion::new("1.0"),
    )
    .unwrap()
}

fn path_info_fixture() -> PathInfoReport {
    PathInfoReport::new(
        store_path("hello-1.0"),
        nar_hash(),
        vec![sig("k"), sig("m")],
        vec![store_path("glibc-2.39")],
        Some(drv("hello-1.0")),
        1024,
        4096,
    )
    .unwrap()
}

fn substitute_fixture(outcome: SubstituteOutcome) -> SubstituteReport {
    if outcome == SubstituteOutcome::Fetched {
        SubstituteReport::fetched(
            store_path("hello-1.0"),
            SubstituteReceipt::new(
                "https://cache.nixos.org",
                nar_hash(),
                vec![sig("cache.nixos.org-1")],
            )
            .unwrap(),
        )
    } else {
        SubstituteReport::miss(store_path("hello-1.0"), outcome).unwrap()
    }
}

fn receipt_fixture() -> BuildApprovalReceipt {
    BuildApprovalReceipt::new(OperationId::new("op-0001").unwrap())
}

fn build_request_fixture() -> BuildRequest {
    BuildRequest::new(
        vec![drv("hello-1.0")],
        System::X8664Linux,
        receipt_fixture(),
    )
    .unwrap()
}

fn build_report_built() -> BuildReport {
    BuildReport::new(BuildStatus::Built, vec![store_path("hello-1.0")]).unwrap()
}

fn verify_request_fixture() -> VerifyRequest {
    VerifyRequest::new(vec![store_path("hello-1.0")], VerifyMode::Recursive).unwrap()
}

fn verify_report_fixture() -> VerifyReport {
    VerifyReport::new(vec![PathVerifyResult::new(
        store_path("hello-1.0"),
        NarIntegrity::Intact,
        TrustStatus::Trusted,
    )])
    .unwrap()
}

fn gc_report_collected() -> GcReport {
    GcReport::new(
        GcStatus::Collected,
        vec![store_path("unreachable-1")],
        12_345,
    )
    .unwrap()
}

fn eval_request_default_outputs() -> EvaluateDerivationRequest {
    EvaluateDerivationRequest::new(
        AttributePath::new("python311.pkgs.requests").unwrap(),
        System::X8664Linux,
        nixpkgs_revision(),
        nar_hash(),
        OutputSelection::default_selection(),
    )
    .unwrap()
}

// ===========================================================================
// Tests: all seven methods return canned results, in exact FIFO order.
// ===========================================================================

#[test]
fn all_seven_methods_return_canned_results_in_fifo_order() {
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)))
        .expect_evaluate_derivation(eval_request_default_outputs(), Ok(realization_fixture()))
        .expect_path_info(store_path("hello-1.0"), Ok(path_info_fixture()))
        .expect_substitute(
            store_path("hello-1.0"),
            Ok(substitute_fixture(SubstituteOutcome::Fetched)),
        )
        .expect_build(build_request_fixture(), Ok(build_report_built()))
        .expect_verify(verify_request_fixture(), Ok(verify_report_fixture()))
        .expect_gc(Ok(gc_report_collected()));

    // The calls must happen in exactly the scripted order (FIFO).
    assert_eq!(fake.version().unwrap().nix_version().as_str(), "2.33.5");
    let r = fake
        .evaluate_derivation(&eval_request_default_outputs())
        .unwrap();
    assert_eq!(r.pname(), "hello");
    let pi = fake.path_info(&store_path("hello-1.0")).unwrap();
    assert_eq!(pi.references(), &[store_path("glibc-2.39")]);
    assert_eq!(
        fake.substitute(&store_path("hello-1.0")).unwrap().outcome(),
        SubstituteOutcome::Fetched
    );
    assert_eq!(
        fake.build(&build_request_fixture()).unwrap().status(),
        BuildStatus::Built
    );
    assert_eq!(
        fake.verify(&verify_request_fixture())
            .unwrap()
            .results()
            .len(),
        1
    );
    assert_eq!(fake.gc().unwrap().status(), GcStatus::Collected);

    // Everything was consumed in order.
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn fifo_order_is_enforced_out_of_order_calls_mismatch() {
    // Script: version, then gc.
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)))
        .expect_gc(Ok(gc_report_collected()));

    // Calling gc() first pops the version head: wrong method. The version
    // head is consumed, and the error honestly names expected=version,
    // actual=gc.
    let err = fake.gc().unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.expected_method(), Some(MethodKind::Version));
    assert_eq!(err.actual_method(), Some(MethodKind::Gc));
    assert_eq!(err.mismatch_summary(), Some("method mismatch"));

    // Now the gc head is at the front; calling gc() matches.
    assert_eq!(fake.gc().unwrap().status(), GcStatus::Collected);
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: exact request matching for every request-bearing method.
// ===========================================================================

#[test]
fn path_info_matches_exact_store_path_only() {
    let fake = FakeNix::new();
    fake.expect_path_info(store_path("hello-1.0"), Ok(path_info_fixture()));

    // Different path -> request mismatch, head consumed.
    let err = fake.path_info(&store_path("other-1.0")).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.expected_method(), Some(MethodKind::PathInfo));
    assert_eq!(err.actual_method(), Some(MethodKind::PathInfo));
    assert_eq!(err.mismatch_summary(), Some("request mismatch"));

    // The transcript is now empty (the head was consumed by the mismatch).
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn substitute_matches_exact_store_path_only() {
    let fake = FakeNix::new();
    fake.expect_substitute(
        store_path("hello-1.0"),
        Ok(substitute_fixture(SubstituteOutcome::Fetched)),
    )
    .expect_substitute(
        store_path("hello-1.0"),
        Ok(substitute_fixture(SubstituteOutcome::Fetched)),
    );
    // Matching path returns the canned result.
    assert!(fake.substitute(&store_path("hello-1.0")).is_ok());
    // A different path is a request mismatch against the fresh head.
    assert_eq!(
        fake.substitute(&store_path("other-1.0"))
            .unwrap_err()
            .mismatch_summary(),
        Some("request mismatch")
    );
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn evaluate_derivation_matches_exact_request_only() {
    let req = eval_request_default_outputs();
    let fake = FakeNix::new();
    fake.expect_evaluate_derivation(req.clone(), Ok(realization_fixture()))
        .expect_evaluate_derivation(req.clone(), Ok(realization_fixture()));
    // Matching request returns the canned result.
    assert!(fake.evaluate_derivation(&req).is_ok());

    // A different-but-valid request mismatches the fresh head.
    let other = EvaluateDerivationRequest::new(
        AttributePath::new("ripgrep").unwrap(),
        System::Aarch64Darwin,
        nixpkgs_revision(),
        nar_hash(),
        OutputSelection::default_selection(),
    )
    .unwrap();
    let err = fake.evaluate_derivation(&other).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.mismatch_summary(), Some("request mismatch"));
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn build_matches_exact_request_only() {
    let req = build_request_fixture();
    let fake = FakeNix::new();
    fake.expect_build(req.clone(), Ok(build_report_built()))
        .expect_build(req.clone(), Ok(build_report_built()));
    assert_eq!(fake.build(&req).unwrap().status(), BuildStatus::Built);

    // A different build request (different target) mismatches the fresh head.
    let other = BuildRequest::new(
        vec![drv("other-1.0")],
        System::X8664Linux,
        receipt_fixture(),
    )
    .unwrap();
    let err = fake.build(&other).unwrap_err();
    assert_eq!(err.mismatch_summary(), Some("request mismatch"));
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn verify_matches_exact_request_only() {
    let req = verify_request_fixture();
    let fake = FakeNix::new();
    fake.expect_verify(req.clone(), Ok(verify_report_fixture()))
        .expect_verify(req.clone(), Ok(verify_report_fixture()));
    assert!(fake.verify(&req).is_ok());

    // Different mode -> mismatch against the fresh head.
    let other = VerifyRequest::new(vec![store_path("hello-1.0")], VerifyMode::Shallow).unwrap();
    assert_eq!(
        fake.verify(&other).unwrap_err().mismatch_summary(),
        Some("request mismatch")
    );
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: canned errors are returned unchanged; no request-bearing matcher is
// required for the error itself.
// ===========================================================================

#[test]
fn canned_errors_are_returned_unchanged() {
    let fake = FakeNix::new();
    fake.expect_version(Err(NixAdapterError::Unavailable))
        .expect_path_info(store_path("x"), Err(NixAdapterError::TrustFailure))
        .expect_substitute(store_path("x"), Err(NixAdapterError::IntegrityFailure))
        .expect_gc(Err(NixAdapterError::Timeout));

    assert_eq!(
        fake.version().unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    // A canned error is returned ONLY for a matching call; the request must
    // still match the head matcher exactly.
    assert_eq!(
        fake.path_info(&store_path("x")).unwrap_err().code(),
        NixAdapterErrorCode::TrustFailure
    );
    assert_eq!(
        fake.substitute(&store_path("x")).unwrap_err().code(),
        NixAdapterErrorCode::IntegrityFailure
    );
    assert_eq!(fake.gc().unwrap_err().code(), NixAdapterErrorCode::Timeout);
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn canned_malformed_payload_error_round_trips() {
    // A canned error carrying a redacted kind is returned with that kind
    // intact.
    let fake = FakeNix::new();
    fake.expect_gc(Err(NixAdapterError::MalformedPayload {
        kind: MalformedKind::ExcessiveNesting,
    }));
    let err = fake.gc().unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
    assert!(matches!(
        err,
        NixAdapterError::MalformedPayload {
            kind: MalformedKind::ExcessiveNesting
        }
    ));
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: wrong-method mismatch consumes the head and is honest about expected.
// ===========================================================================

#[test]
fn wrong_method_mismatch_consumes_head_and_names_real_expected() {
    for (head_is, call_is, expected, actual) in [
        (
            MethodKind::Version,
            MethodKind::Gc,
            MethodKind::Version,
            MethodKind::Gc,
        ),
        (
            MethodKind::Gc,
            MethodKind::Version,
            MethodKind::Gc,
            MethodKind::Version,
        ),
        (
            MethodKind::PathInfo,
            MethodKind::Substitute,
            MethodKind::PathInfo,
            MethodKind::Substitute,
        ),
        (
            MethodKind::Build,
            MethodKind::Verify,
            MethodKind::Build,
            MethodKind::Verify,
        ),
    ] {
        let fake = FakeNix::new();
        // Seed a head expectation of `head_is`.
        match head_is {
            MethodKind::Version => fake.expect_version(Ok(version_info(33))),
            MethodKind::Gc => fake.expect_gc(Ok(gc_report_collected())),
            MethodKind::PathInfo => {
                fake.expect_path_info(store_path("hello-1.0"), Ok(path_info_fixture()))
            }
            MethodKind::Substitute => fake.expect_substitute(
                store_path("hello-1.0"),
                Ok(substitute_fixture(SubstituteOutcome::Fetched)),
            ),
            MethodKind::Build => {
                fake.expect_build(build_request_fixture(), Ok(build_report_built()))
            }
            MethodKind::Verify => {
                fake.expect_verify(verify_request_fixture(), Ok(verify_report_fixture()))
            }
            _ => unreachable!("fixture covers a subset"),
        };
        // Make a call of `call_is` and assert the honest mismatch.
        let err = match call_is {
            MethodKind::Version => fake.version().unwrap_err(),
            MethodKind::Gc => fake.gc().unwrap_err(),
            MethodKind::PathInfo => fake.path_info(&store_path("hello-1.0")).unwrap_err(),
            MethodKind::Substitute => fake.substitute(&store_path("hello-1.0")).unwrap_err(),
            MethodKind::Build => fake.build(&build_request_fixture()).unwrap_err(),
            MethodKind::Verify => fake.verify(&verify_request_fixture()).unwrap_err(),
            _ => unreachable!("fixture covers a subset"),
        };
        assert_eq!(
            err.code(),
            NixAdapterErrorCode::UnexpectedCall,
            "case {head_is}->{call_is}"
        );
        assert_eq!(
            err.expected_method(),
            Some(expected),
            "case {head_is}->{call_is}"
        );
        assert_eq!(
            err.actual_method(),
            Some(actual),
            "case {head_is}->{call_is}"
        );
        assert_eq!(
            err.mismatch_summary(),
            Some("method mismatch"),
            "case {head_is}->{call_is}"
        );
        // The head was consumed.
        assert_eq!(fake.assert_exhausted(), Ok(()), "case {head_is}->{call_is}");
    }
}

// ===========================================================================
// Tests: extra calls and the empty/exhausted transcript — the no-head case.
// ===========================================================================

/// Extra call against an EMPTY transcript has no head, so there is no honest
/// `expected: MethodKind`. `FakeNix` returns the dedicated, redacted
/// `NixAdapterError::UnexpectedExtraCall` (code `UnexpectedCall`,
/// `expected_method() == None`, `actual_method() == Some(actual)`,
/// `mismatch_summary() == Some("extra call")`) and consumes nothing — never a
/// panic, never a fabricated expected value, and never a generic backend error.
#[test]
fn extra_call_against_empty_transcript_returns_unexpected_extra_call() {
    let fake = FakeNix::new();
    // Empty transcript: every method returns UnexpectedExtraCall (no head).
    let err = fake.version().unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.expected_method(), None);
    assert_eq!(err.actual_method(), Some(MethodKind::Version));
    assert_eq!(err.mismatch_summary(), Some("extra call"));

    let err = fake.gc().unwrap_err();
    assert_eq!(err.actual_method(), Some(MethodKind::Gc));
    assert_eq!(err.mismatch_summary(), Some("extra call"));

    let err = fake.path_info(&store_path("x")).unwrap_err();
    assert_eq!(err.actual_method(), Some(MethodKind::PathInfo));

    // Nothing was consumed; the transcript is still empty.
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn extra_call_after_exhausting_transcript_returns_unexpected_extra_call() {
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)));
    // Consume the one expectation.
    assert_eq!(fake.version().unwrap().nix_version().as_str(), "2.33.5");
    assert_eq!(fake.assert_exhausted(), Ok(()));
    // One more call: transcript is now exhausted (no head).
    let err = fake.version().unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.expected_method(), None);
    assert_eq!(err.actual_method(), Some(MethodKind::Version));
    assert_eq!(err.mismatch_summary(), Some("extra call"));
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn unexpected_extra_call_display_is_bounded_and_redacted() {
    // An extra call carries only the actual method kind + the static summary;
    // the request value never enters the error. Pass a request carrying
    // distinctive, secret-looking values and confirm none leak into Display or
    // Debug.
    let fake = FakeNix::new();
    let err = fake
        .evaluate_derivation(&eval_request_default_outputs())
        .unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.actual_method(), Some(MethodKind::EvaluateDerivation));
    assert_eq!(err.mismatch_summary(), Some("extra call"));
    let display = err.to_string();
    let debug = format!("{err:?}");
    for surface in [display.as_str(), debug.as_str()] {
        assert!(
            surface.contains("no expectation remained")
                || surface.contains("extra call")
                || surface.contains("evaluateDerivation")
                || surface.contains("EvaluateDerivation"),
            "expected a truthful token in {surface:?}"
        );
        // The request's attribute path, revision, NAR hash, and store hash
        // never appear in any formatting surface.
        assert!(!surface.contains("python311"), "leak in {surface:?}");
        assert!(!surface.contains("requests"), "leak in {surface:?}");
        assert!(!surface.contains(REV), "leak in {surface:?}");
        assert!(!surface.contains(NAR), "leak in {surface:?}");
        assert!(!surface.contains(STORE_HASH), "leak in {surface:?}");
    }
}

#[test]
fn ignored_extra_call_errors_are_not_observable_via_assert_exhausted() {
    // An extra call returns an error and consumes nothing; if the caller
    // ignores that error, assert_exhausted has no record of it (the transcript
    // stays empty). Extra calls are reported only via the returned Result,
    // never via assert_exhausted.
    let fake = FakeNix::new();
    let _ = fake.version(); // ignored extra-call error
    let _ = fake.gc(); // ignored extra-call error
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: assert_exhausted reports the remaining COUNT ONLY.
// ===========================================================================

#[test]
fn assert_exhausted_ok_when_empty() {
    assert_eq!(FakeNix::new().assert_exhausted(), Ok(()));
}

#[test]
fn assert_exhausted_reports_remaining_count_only() {
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)))
        .expect_gc(Ok(gc_report_collected()))
        .expect_path_info(store_path("hello-1.0"), Ok(path_info_fixture()));

    let err = fake.assert_exhausted().unwrap_err();
    assert_eq!(err, TranscriptError::UnmetExpectations { remaining: 3 });
    assert_eq!(err.remaining(), 3);

    // Display and Debug carry ONLY the count — never any matcher, canned
    // result, store path, or report value.
    let display = err.to_string();
    assert!(display.contains("3"));
    assert!(!display.contains("hello-1.0"));
    assert!(!display.contains("2.33.5"));
    assert!(!display.contains(STORE_HASH));
    let debug = format!("{err:?}");
    assert!(debug.contains("remaining: 3"));
    assert!(!debug.contains("hello-1.0"));
    assert!(!debug.contains(STORE_HASH));
}

// ===========================================================================
// Tests: safe bounded Display/Debug — no raw request or transcript data leaks
// into any error or formatting surface.
// ===========================================================================

#[test]
fn unexpected_call_display_carries_only_method_names_and_static_summary() {
    // Seed a head carrying a request with a distinctive, secret-looking path.
    let secret = store_path("SECRET-target-1.0");
    let fake = FakeNix::new();
    fake.expect_path_info(secret, Ok(path_info_fixture()));

    // A mismatched path also carrying a distinctive value.
    let caller_secret = store_path("CALLER-secret-2.0");
    let err = fake.path_info(&caller_secret).unwrap_err();

    let display = err.to_string();
    let debug = format!("{err:?}");
    for surface in [display.as_str(), debug.as_str()] {
        // Method names and the static summary appear.
        assert!(
            surface.contains("pathInfo")
                || surface.contains("path_info")
                || surface.contains("request mismatch")
        );
        // Neither the head's secret path nor the caller's secret path appears.
        assert!(
            !surface.contains("SECRET-target-1.0"),
            "leak in {surface:?}"
        );
        assert!(
            !surface.contains("CALLER-secret-2.0"),
            "leak in {surface:?}"
        );
        assert!(!surface.contains(STORE_HASH), "leak in {surface:?}");
    }
}

#[test]
fn fake_nix_debug_shows_only_remaining_count() {
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)))
        .expect_gc(Ok(gc_report_collected()))
        .expect_path_info(store_path("hello-1.0"), Ok(path_info_fixture()));

    let debug = format!("{fake:?}");
    assert!(debug.contains("remaining: 3"));
    // No transcript value leaks through Debug.
    assert!(!debug.contains("hello-1.0"));
    assert!(!debug.contains("2.33.5"));
    assert!(!debug.contains(STORE_HASH));
}

// ===========================================================================
// Tests: Send + Sync, dyn NixAdapter dispatch.
// ===========================================================================

#[test]
fn fake_nix_is_send_sync_and_dyn_compatible() {
    fn _assert_send_sync<T: Send + Sync + ?Sized>() {}
    _assert_send_sync::<FakeNix>();
    _assert_send_sync::<dyn NixAdapter>();

    let boxed: Box<dyn NixAdapter> = Box::new(FakeNix::new());
    let _: Arc<dyn NixAdapter> = Arc::new(FakeNix::new());
    // An empty fake behind a trait object returns UnexpectedExtraCall (no
    // panic): no head existed, so no expected method is fabricated.
    let err = boxed.version().unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::UnexpectedCall);
    assert_eq!(err.expected_method(), None);
    assert_eq!(err.actual_method(), Some(MethodKind::Version));
}

#[test]
fn dyn_dispatch_replays_transcript() {
    // Keep `Arc<FakeNix>` as the concrete handle so exhaustion is reachable;
    // coerce a clone to `Arc<dyn NixAdapter>` and drive the two calls through
    // the trait object.
    let fake: Arc<FakeNix> = Arc::new(FakeNix::new());
    fake.expect_version(Ok(version_info(33)))
        .expect_gc(Ok(gc_report_collected()));
    let adapter: Arc<dyn NixAdapter> = fake.clone();
    assert_eq!(adapter.version().unwrap().nix_version().as_str(), "2.33.5");
    assert_eq!(adapter.gc().unwrap().status(), GcStatus::Collected);
    // Both heads were consumed through the trait object; the shared concrete
    // handle observes an empty transcript.
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: concurrent access consumes each head exactly once (no data race,
// no double-consume, no skip, no panic).
// ===========================================================================

#[test]
fn concurrent_calls_consume_distinct_heads_exactly_once() {
    // Eight version expectations, each returning a DISTINCT version string.
    // Eight threads each call version() exactly once. Regardless of the
    // nondeterministic scheduling, each call consumes exactly one head, so the
    // multiset of returned versions equals the multiset of expectations.
    let fake = Arc::new(FakeNix::new());
    for i in 0..8u32 {
        fake.expect_version(Ok(version_info(i)));
    }

    let shared = fake.clone();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let shared = shared.clone();
            std::thread::spawn(move || {
                // Every thread reads the same method; ordering is
                // nondeterministic but each consumes one distinct head.
                shared
                    .version()
                    .expect("concurrent version call")
                    .nix_version()
                    .as_str()
                    .to_owned()
            })
        })
        .collect();

    let mut got: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("thread did not panic"))
        .collect();
    got.sort();
    let mut want: Vec<String> = (0..8u32).map(|i| format!("2.{i}.5")).collect();
    want.sort();
    assert_eq!(got, want, "each head consumed exactly once");

    // The transcript is fully consumed; no double-consume left leftovers and
    // no call starved.
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: construction validation & minimal public API surface.
// ===========================================================================

#[test]
fn default_is_empty() {
    let fake = FakeNix::default();
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn expect_methods_chain_and_preserve_ownership() {
    // The builder returns &Self for chaining but does not move the FakeNix.
    let fake = FakeNix::new();
    fake.expect_version(Ok(version_info(33)))
        .expect_gc(Ok(gc_report_collected()));
    // `fake` is still owned and usable.
    assert_eq!(fake.version().unwrap().nix_version().as_str(), "2.33.5");
    assert_eq!(fake.gc().unwrap().status(), GcStatus::Collected);
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

// ===========================================================================
// Tests: no reverse dependency. `pkg-nix` never names `pkg-testkit`; this is
// enforced structurally by the manifests and asserted here by confirming the
// public API is reachable purely through `pkg-testkit`'s re-exports (the
// `pkg-nix` types in FakeNix's signatures come back as the same types).
// ===========================================================================

#[test]
fn pkg_testkit_types_are_pkg_nix_types() {
    // The re-exported types ARE the pkg-nix types (re-export, not newtype), so
    // a value constructed via `pkg_nix::` is accepted by an API taking the
    // `pkg_testkit::` re-export. This is the runtime check that the dependency
    // is one-way and the re-exports are sound.
    fn takes_nix_path(_: pkg_testkit::StorePath) {}
    takes_nix_path(store_path("hello-1.0"));
}

#[test]
fn transcript_error_is_std_error() {
    fn is_error<E: std::error::Error>(_: &E) {}
    let err = TranscriptError::UnmetExpectations { remaining: 1 };
    is_error(&err);
}
