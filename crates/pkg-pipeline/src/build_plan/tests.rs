//! Tests for the `build_plan` module.

use std::{collections::BTreeMap, str::FromStr};

use pkg_core::{
    AttributePath, DerivationPath, OutputName, OutputSelection, PackageVersion, SelectorId,
    SelectorInput, SourceRevision, StorePath, VersionPreference,
};
use pkg_nix::{
    AcceptedFormats, BuildPlanTarget, CacheClassification, DerivationPlanReport,
    EvaluatedDerivation, FormatVersion,
};

use super::*;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

fn policy() -> AuthenticatedBuildPolicy {
    AuthenticatedBuildPolicy {
        channel_sequence: ChannelSequence::from_u64(42).unwrap(),
        policy_version: PolicyVersion::from_u64(7).unwrap(),
        descriptor_sha256: [3; 32],
        nix_runtime_version: NixVersion::new("2.34.8").unwrap(),
        revision: NixpkgsRevision::new(REVISION).unwrap(),
        nar_hash: NarHash::new(NAR_HASH).unwrap(),
        build_mode: BuildMode::AllowWithGates,
    }
}

#[test]
fn authenticated_identity_match_binds_every_source_field() {
    let policy = policy();
    let sequence = ChannelSequence::from_u64(42).unwrap();
    let version = PolicyVersion::from_u64(7).unwrap();
    let revision = NixpkgsRevision::new(REVISION).unwrap();
    let nar_hash = NarHash::new(NAR_HASH).unwrap();
    assert!(policy.matches_source_identity(sequence, version, [3; 32], &revision, &nar_hash));

    assert!(!policy.matches_source_identity(
        ChannelSequence::from_u64(43).unwrap(),
        version,
        [3; 32],
        &revision,
        &nar_hash
    ));
    assert!(!policy.matches_source_identity(
        sequence,
        PolicyVersion::from_u64(8).unwrap(),
        [3; 32],
        &revision,
        &nar_hash
    ));
    assert!(!policy.matches_source_identity(sequence, version, [4; 32], &revision, &nar_hash));
    assert!(!policy.matches_source_identity(
        sequence,
        version,
        [3; 32],
        &NixpkgsRevision::new("1123456789abcdef0123456789abcdef01234567").unwrap(),
        &nar_hash
    ));
    assert!(!policy.matches_source_identity(
        sequence,
        version,
        [3; 32],
        &revision,
        &NarHash::new("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap()
    ));
}

fn runtime(version: &str) -> VersionInfo {
    VersionInfo::new(
        NixVersion::new(version).unwrap(),
        AcceptedFormats::new(FormatVersion::new(2).unwrap()),
    )
}

fn complete_plan(version: &NixVersion, system: System) -> BuildPlan {
    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    let derivation =
        DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello-1.0.drv")).unwrap();
    let output = StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap();
    let report = DerivationPlanReport::new(
        4,
        derivation.clone(),
        vec![OutputName::new("out").unwrap()],
        vec![
            EvaluatedDerivation::new(
                derivation.clone(),
                "hello-1.0".to_owned(),
                system,
                BTreeMap::from([(OutputName::new("out").unwrap(), output)]),
                Digest::from_bytes([1; 32]),
                false,
            )
            .unwrap(),
        ],
        Digest::from_bytes([2; 32]),
        "hello".to_owned(),
        PackageVersion::new("1.0"),
    )
    .unwrap();
    let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
    BuildPlan::new(
        version,
        Digest::from_bytes([3; 32]),
        PolicyVersion::from_u64(7).unwrap(),
        ChannelSequence::from_u64(42).unwrap(),
        &NixpkgsRevision::new(REVISION).unwrap(),
        &NarHash::new(NAR_HASH).unwrap(),
        system,
        system,
        BuildMode::AllowWithGates,
        vec![BuildPlanTarget::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            AttributePath::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
            report,
        )],
        vec![derivation],
        CacheClassification::new(Digest::from_bytes([4; 32]), 0, 1, 0, 0).unwrap(),
        BuildReadiness::new(true, false, true, linux, linux),
        8,
    )
    .unwrap()
}

#[test]
fn complete_plan_identity_binds_the_platform_runtime_contract() {
    let policy = policy();
    let determinate = runtime(STANDARD_DETERMINATE_NIX_VERSION);
    for system in [System::X8664Linux, System::Aarch64Linux] {
        assert_eq!(
            plan_runtime_version(&policy, &determinate, system)
                .unwrap()
                .as_str(),
            STANDARD_DETERMINATE_NIX_VERSION
        );
        assert_eq!(
            plan_runtime_version(&policy, &runtime("2.35.1"), system)
                .unwrap_err()
                .code(),
            LocalBuildPlanErrorCode::RuntimeMismatch
        );
    }
    let linux = complete_plan(
        plan_runtime_version(&policy, &determinate, System::X8664Linux).unwrap(),
        System::X8664Linux,
    );
    assert_eq!(
        linux.digest().unwrap(),
        complete_plan(
            &NixVersion::new(STANDARD_DETERMINATE_NIX_VERSION).unwrap(),
            System::X8664Linux,
        )
        .digest()
        .unwrap()
    );
    assert_ne!(
        linux.digest().unwrap(),
        complete_plan(&policy.nix_runtime_version, System::X8664Linux)
            .digest()
            .unwrap()
    );

    let legacy = runtime("2.34.8");
    for system in [System::X8664Darwin, System::Aarch64Darwin] {
        assert_eq!(
            plan_runtime_version(&policy, &legacy, system)
                .unwrap()
                .as_str(),
            "2.34.8"
        );
        assert_eq!(
            plan_runtime_version(&policy, &determinate, system)
                .unwrap_err()
                .code(),
            LocalBuildPlanErrorCode::RuntimeMismatch
        );
    }
    let macos = complete_plan(
        plan_runtime_version(&policy, &legacy, System::Aarch64Darwin).unwrap(),
        System::Aarch64Darwin,
    );
    assert_eq!(
        macos.digest().unwrap(),
        complete_plan(&policy.nix_runtime_version, System::Aarch64Darwin)
            .digest()
            .unwrap()
    );
    assert_ne!(
        macos.digest().unwrap(),
        complete_plan(
            &NixVersion::new(STANDARD_DETERMINATE_NIX_VERSION).unwrap(),
            System::Aarch64Darwin,
        )
        .digest()
        .unwrap()
    );
}
