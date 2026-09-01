//! Tests for the `realization` module.

use super::*;
use std::str::FromStr;

const STORE_OUT: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-14.1.0";
const STORE_MAN: &str = "/nix/store/11111111111111111111111111111111-ripgrep-14.1.0-man";
const DRV: &str = "/nix/store/22222222222222222222222222222222-ripgrep-14.1.0.drv";
const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
// Distinct-but-valid fixtures used to vary nonidentity fields in identity
// tests without touching the primary store path.
const DRV2: &str = "/nix/store/44444444444444444444444444444444-other-9.9.drv";
const NAR2: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const REV2: &str = "fedcba9876543210fedcba9876543210fedcba98";

fn make_realization(store_path: &str, pname: &str, version: &str) -> Realization {
    let out = StorePath::new(store_path).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![OutputName::new("out").unwrap()],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        4821034,
        pname.to_owned(),
        PackageVersion::new(version),
    )
    .unwrap()
}

fn make_realization_multi() -> Realization {
    let out = StorePath::new(STORE_OUT).unwrap();
    let man = StorePath::new(STORE_MAN).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    outputs.insert(OutputName::new("man").unwrap(), man);
    Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![
            OutputName::new("out").unwrap(),
            OutputName::new("man").unwrap(),
        ],
        System::Aarch64Darwin,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        9_999_999,
        "ripgrep".to_owned(),
        PackageVersion::new("14.1.0"),
    )
    .unwrap()
}

#[test]
fn happy_path_accessors_and_identity() {
    let r = make_realization(STORE_OUT, "ripgrep", "14.1.0");
    assert_eq!(r.identity().as_str(), STORE_OUT);
    assert_eq!(r.store_path().as_str(), STORE_OUT);
    assert_eq!(r.deriver().as_str(), DRV);
    assert_eq!(r.outputs().len(), 1);
    assert_eq!(r.outputs_to_install().len(), 1);
    assert_eq!(r.system(), System::X8664Linux);
    assert_eq!(r.nixpkgs_revision().as_str(), REV);
    assert_eq!(r.nar_hash().as_str(), NAR);
    assert_eq!(r.closure_nar_size(), 4821034);
    assert_eq!(r.pname(), "ripgrep");
    assert_eq!(r.version().as_str(), "14.1.0");
}

#[test]
fn multi_output_accessors() {
    let r = make_realization_multi();
    assert_eq!(r.outputs().len(), 2);
    assert_eq!(r.outputs_to_install().len(), 2);
    assert!(r.outputs().contains_key(&OutputName::new("man").unwrap()));
}

#[test]
fn rejects_empty_outputs() {
    let out = StorePath::new(STORE_OUT).unwrap();
    let err = Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        BTreeMap::new(),
        vec![OutputName::new("out").unwrap()],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        1,
        "x".to_owned(),
        PackageVersion::new("1"),
    )
    .unwrap_err();
    assert_eq!(err, RealizationError::EmptyOutputs);
}

#[test]
fn rejects_empty_outputs_to_install() {
    let out = StorePath::new(STORE_OUT).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    let err = Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        1,
        "x".to_owned(),
        PackageVersion::new("1"),
    )
    .unwrap_err();
    assert_eq!(err, RealizationError::EmptyOutputsToInstall);
}

#[test]
fn rejects_duplicate_outputs_to_install() {
    let out = StorePath::new(STORE_OUT).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    let err = Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![
            OutputName::new("out").unwrap(),
            OutputName::new("out").unwrap(),
        ],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        1,
        "x".to_owned(),
        PackageVersion::new("1"),
    )
    .unwrap_err();
    assert_eq!(err, RealizationError::DuplicateOutputToInstall);
}

#[test]
fn rejects_selected_output_not_present() {
    let out = StorePath::new(STORE_OUT).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    let err = Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![OutputName::new("man").unwrap()], // man not in outputs
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        1,
        "x".to_owned(),
        PackageVersion::new("1"),
    )
    .unwrap_err();
    assert_eq!(err, RealizationError::SelectedOutputNotPresent);
}

#[test]
fn rejects_primary_store_path_not_an_output() {
    let out = StorePath::new(STORE_OUT).unwrap();
    let other = StorePath::new(STORE_MAN).unwrap();
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("man").unwrap(), other);
    // primary store_path = out, but outputs only contain `man`'s path.
    let err = Realization::new(
        out,
        DerivationPath::from_str(DRV).unwrap(),
        outputs,
        vec![OutputName::new("man").unwrap()],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        1,
        "x".to_owned(),
        PackageVersion::new("1"),
    )
    .unwrap_err();
    assert_eq!(err, RealizationError::PrimaryStorePathNotAnOutput);
}

#[test]
fn identity_is_store_path_only() {
    // Same store path, differing display metadata -> equal realizations.
    let a = make_realization(STORE_OUT, "ripgrep", "14.1.0");
    let b = make_realization(STORE_OUT, "different-name", "99.0");
    assert_eq!(a, b, "display metadata must not affect identity");
    assert_eq!(a.identity(), b.identity());

    fn hash<T: Hash>(x: &T) -> u64 {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        x.hash(&mut s);
        s.finish()
    }
    assert_eq!(hash(&a), hash(&b), "hash must match for equal identity");

    // Different store path -> not equal. (We do NOT assert hash
    // inequality: hashing only guarantees equal => same hash, never the
    // converse.)
    let c = make_realization(
        "/nix/store/33333333333333333333333333333333-other-1.0",
        "ripgrep",
        "14.1.0",
    );
    assert_ne!(a, c);
}

#[test]
fn identity_ignores_nonidentity_fields() {
    // Two realizations with the SAME primary store path but differing in
    // system, Nixpkgs revision, deriver, output map/metadata, closure size,
    // pname/version, and NAR hash. Eq/Hash must still report them equal,
    // because identity is the store path only (D-13 / INV-06). This proves
    // the deliberate identity semantics survive when many nonidentity
    // fields move; the schema itself is unchanged.
    let out = StorePath::new(STORE_OUT).unwrap();
    let man = StorePath::new(STORE_MAN).unwrap();

    let mut outputs_a = BTreeMap::new();
    outputs_a.insert(OutputName::new("out").unwrap(), out.clone());

    let mut outputs_b = BTreeMap::new();
    outputs_b.insert(OutputName::new("out").unwrap(), out.clone());
    outputs_b.insert(OutputName::new("man").unwrap(), man);

    let a = Realization::new(
        out.clone(),
        DerivationPath::from_str(DRV).unwrap(),
        outputs_a,
        vec![OutputName::new("out").unwrap()],
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        NarHash::new(NAR).unwrap(),
        4821034,
        "ripgrep".to_owned(),
        PackageVersion::new("14.1.0"),
    )
    .unwrap();

    let b = Realization::new(
        out,
        DerivationPath::from_str(DRV2).unwrap(),
        outputs_b,
        vec![
            OutputName::new("out").unwrap(),
            OutputName::new("man").unwrap(),
        ],
        System::Aarch64Darwin,
        NixpkgsRevision::new(REV2).unwrap(),
        NarHash::new(NAR2).unwrap(),
        9_999_999,
        "completely-different".to_owned(),
        PackageVersion::new("99.0"),
    )
    .unwrap();

    assert_eq!(a, b, "nonidentity fields must not affect identity");
    assert_eq!(a.identity(), b.identity());

    fn hash<T: Hash>(x: &T) -> u64 {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        x.hash(&mut s);
        s.finish()
    }
    assert_eq!(hash(&a), hash(&b), "equal identity must hash equally");
}
