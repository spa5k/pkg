//! Tests for the `src` module.

use super::*;
use pkg_core::state::Digest;
use pkg_core::{
    OutputName, OutputSelection, PackageVersion, RealizationIdentity, SelectorId, SelectorInput,
    VersionPreference,
};
use pkg_index::{BuildMetadata, build_index_from_json};
use pkg_nix::{EvaluatedDerivation, StorePath};
use pkg_testkit::FakeNix;
use std::collections::BTreeMap;
use std::str::FromStr;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

fn sequence() -> ChannelSequence {
    ChannelSequence::from_u64(7).unwrap()
}

fn revision() -> NixpkgsRevision {
    NixpkgsRevision::new(REVISION).unwrap()
}

fn nar_hash() -> NarHash {
    NarHash::new(NAR_HASH).unwrap()
}

fn selector(input: &str, preference: VersionPreference) -> PackageSelector {
    PackageSelector::new(
        SelectorId::new("sel_test").unwrap(),
        SelectorInput::new(input).unwrap(),
        preference,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    )
}

fn request(attribute: &str) -> EvaluateDerivationRequest {
    EvaluateDerivationRequest::new(
        AttributePath::new(attribute).unwrap(),
        System::X8664Linux,
        revision(),
        nar_hash(),
        OutputSelection::default_selection(),
    )
    .unwrap()
}

fn plan(version: &str) -> DerivationPlanReport {
    let derivation = pkg_core::DerivationPath::from_str(&format!(
        "/nix/store/{STORE_HASH}-ripgrep-{version}.drv"
    ))
    .unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(
        OutputName::new("out").unwrap(),
        StorePath::new(&format!("/nix/store/{STORE_HASH}-ripgrep-{version}")).unwrap(),
    );
    let evaluated = EvaluatedDerivation::new(
        derivation.clone(),
        format!("ripgrep-{version}"),
        System::X8664Linux,
        outputs,
        Digest::from_bytes([1; 32]),
        false,
    )
    .unwrap();
    DerivationPlanReport::new(
        4,
        derivation,
        vec![OutputName::new("out").unwrap()],
        vec![evaluated],
        Digest::from_bytes([2; 32]),
        "ripgrep".into(),
        PackageVersion::new(version),
    )
    .unwrap()
}

fn resolve_for_test(
    selector: &PackageSelector,
    index: Option<&IndexDocument>,
    fake: &FakeNix,
) -> Result<ResolvedPackagePlan, ResolveError> {
    resolve_with_source(
        selector,
        sequence(),
        &revision(),
        &nar_hash(),
        System::X8664Linux,
        index,
        fake,
    )
}

#[test]
fn missing_index_direct_attribute_evaluates_once_without_realizing() {
    let fake = FakeNix::new();
    fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
    let resolved =
        resolve_for_test(&selector("ripgrep", VersionPreference::Any), None, &fake).unwrap();
    assert_eq!(resolved.selector().attribute().unwrap().as_str(), "ripgrep");
    assert_eq!(resolved.plan().version().as_str(), "14.1.0");
    assert!(resolved.build_plan_target().is_ok());
    assert_eq!(resolved.build_cache_subjects().unwrap().len(), 1);
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn index_alias_selects_attribute_but_evaluation_owns_version() {
    let metadata = BuildMetadata::new(
        sequence(),
        System::X8664Linux,
        revision(),
        "2026-08-09T00:00:00Z",
    )
    .unwrap();
    let projection = br#"[{"attrPath":"ripgrep","pname":"ripgrep","version":"old-display-value","aliases":["rg"]}]"#;
    let built = build_index_from_json(metadata, projection).unwrap();
    let fake = FakeNix::new();
    fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
    let resolved = resolve_for_test(
        &selector(
            "rg",
            VersionPreference::Exact(PackageVersion::new("14.1.0")),
        ),
        Some(built.document()),
        &fake,
    )
    .unwrap();
    assert_eq!(resolved.selector().attribute().unwrap().as_str(), "ripgrep");
    assert_eq!(resolved.plan().version().as_str(), "14.1.0");
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn ambiguity_and_version_mismatch_fail_closed() {
    let metadata = BuildMetadata::new(
        sequence(),
        System::X8664Linux,
        revision(),
        "2026-08-09T00:00:00Z",
    )
    .unwrap();
    let projection = br#"[
            {"attrPath":"one","pname":"one","aliases":["tool"]},
            {"attrPath":"two","pname":"two","aliases":["tool"]}
        ]"#;
    let built = build_index_from_json(metadata, projection).unwrap();
    let fake = FakeNix::new();
    let error = resolve_for_test(
        &selector("tool", VersionPreference::Any),
        Some(built.document()),
        &fake,
    )
    .unwrap_err();
    assert_eq!(error.code(), ResolveErrorCode::AmbiguousSelector);
    assert_eq!(error.candidate_count(), Some(2));

    let fake = FakeNix::new();
    fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
    let error = resolve_for_test(
        &selector(
            "ripgrep",
            VersionPreference::Exact(PackageVersion::new("13.0.0")),
        ),
        None,
        &fake,
    )
    .unwrap_err();
    assert_eq!(error.code(), ResolveErrorCode::VersionMismatch);
    assert_eq!(fake.assert_exhausted(), Ok(()));
}

#[test]
fn source_mismatch_and_adapter_failure_are_redacted() {
    let selector = PackageSelector::new(
        SelectorId::new("sel_test").unwrap(),
        SelectorInput::new("ripgrep").unwrap(),
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::ExactRevision(
            NixpkgsRevision::new("1123456789abcdef0123456789abcdef01234567").unwrap(),
        ),
    );
    let fake = FakeNix::new();
    assert_eq!(
        resolve_for_test(&selector, None, &fake).unwrap_err().code(),
        ResolveErrorCode::SourceMismatch
    );

    let fake = FakeNix::new();
    let error = resolve_for_test(
        &super::tests::selector("ripgrep", VersionPreference::Any),
        None,
        &fake,
    )
    .unwrap_err();
    assert_eq!(error.code(), ResolveErrorCode::EvaluationFailed);
    assert!(!error.to_string().contains("/nix/store"));

    let pinned = super::tests::selector("ripgrep", VersionPreference::Any)
        .with_attribute(AttributePath::new("ripgrep").unwrap())
        .unwrap()
        .pinned_to(RealizationIdentity::new(
            StorePath::new(&format!("/nix/store/{STORE_HASH}-ripgrep-14.1.0")).unwrap(),
        ))
        .unwrap();
    assert_eq!(
        resolve_for_test(&pinned, None, &FakeNix::new())
            .unwrap_err()
            .code(),
        ResolveErrorCode::AlreadyRealized
    );
}
