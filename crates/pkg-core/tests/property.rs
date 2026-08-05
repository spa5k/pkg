//! Property-based tests required by the PR-2 roadmap (`plans/09` §6.1, layer 1):
//!
//! - `compare_nix_versions` reflexivity, reverse symmetry, transitivity (via
//!   cmp-result equivalence, not literal `Eq`), and no panic on arbitrary
//!   strings;
//! - `RealizationIdentity` `Eq`/`Hash` laws, including proof that realizations
//!   sharing a store path but differing in display metadata are equal.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use proptest::prelude::*;

use pkg_core::version::compare_nix_versions;
use pkg_core::{
    DerivationPath, NarHash, NixpkgsRevision, OutputName, PackageVersion, Realization, StorePath,
    System,
};

/// Nix base32 alphabet for store-path hash generation.
const NIX_BASE32: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// A fixed, valid derivation path used as the deriver for fixture realizations.
const DRV: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-fixture.drv";
/// A fixed, valid sha256 SRI NAR hash.
const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
/// A fixed, valid 40-hex Nixpkgs revision.
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// Hashes any `Hash` value with a fixed hasher (for law checks).
fn hashed<T: Hash>(x: &T) -> u64 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    x.hash(&mut s);
    s.finish()
}

/// Strategy producing version-like ASCII strings drawn from an alphabet that
/// exercises digits, both separators (`.`/`-`), letters (including the runes
/// that spell `pre`), and one uppercase word char.
fn version_strat() -> impl Strategy<Value = String> {
    let alpha: Vec<char> = "0123456789.-preabcxyzQ".chars().collect();
    proptest::collection::vec(proptest::sample::select(alpha), 0..24)
        .prop_map(|cs| cs.into_iter().collect())
}

/// Strategy producing valid store-path strings (32-char nix-base32 hash, plus a
/// 1–8 char name).
fn store_path_strat() -> impl Strategy<Value = String> {
    let hash_alpha: Vec<u8> = NIX_BASE32.to_vec();
    let name_alpha: Vec<u8> = b"abcdefghijklmnopqrstuvwxyz0123456789".to_vec();
    (
        proptest::collection::vec(proptest::sample::select(hash_alpha), 32),
        proptest::collection::vec(proptest::sample::select(name_alpha), 1..8),
    )
        .prop_map(|(hash, name)| {
            let h: String = hash.into_iter().map(|b| b as char).collect();
            let n: String = name.into_iter().map(|b| b as char).collect();
            format!("/nix/store/{h}-{n}")
        })
}

/// Builds a valid [`Realization`] whose identity is `store_path_str`, with the
/// given display `pname`/`version`.
fn realization(store_path_str: &str, pname: &str, version: &str) -> Realization {
    let out = StorePath::new(store_path_str).expect("generated store path is valid");
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
        1234,
        pname.to_owned(),
        PackageVersion::new(version),
    )
    .expect("realization is valid")
}

proptest! {
    /// Reflexivity: every version compares equal to itself.
    #[test]
    fn version_reflexive(a in version_strat()) {
        prop_assert_eq!(compare_nix_versions(&a, &a), Ordering::Equal);
    }

    /// Reverse symmetry: `cmp(a,b)` is the reverse of `cmp(b,a)`.
    #[test]
    fn version_reverse_symmetric(a in version_strat(), b in version_strat()) {
        let ab = compare_nix_versions(&a, &b);
        let expected_ba = match ab {
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
            Ordering::Equal => Ordering::Equal,
        };
        prop_assert_eq!(compare_nix_versions(&b, &a), expected_ba);
    }

    /// Transitivity of the <= preorder: if `a <= b` and `b <= c` then `a <= c`,
    /// using cmp-result equivalence (NOT literal string equality, since Nix may
    /// report distinct strings as Equal).
    #[test]
    fn version_transitive_le(
        a in version_strat(),
        b in version_strat(),
        c in version_strat()
    ) {
        let ab = compare_nix_versions(&a, &b);
        let bc = compare_nix_versions(&b, &c);
        if ab != Ordering::Greater && bc != Ordering::Greater {
            let ac = compare_nix_versions(&a, &c);
            prop_assert!(
                ac != Ordering::Greater,
                "transitivity violated: a={a:?} b={b:?} c={c:?} ab={ab:?} bc={bc:?} ac={ac:?}"
            );
        }
    }

    /// Transitivity of the Nix-equivalence ~: `a~b` and `b~c` imply `a~c`.
    #[test]
    fn version_transitive_equiv(
        a in version_strat(),
        b in version_strat(),
        c in version_strat()
    ) {
        if compare_nix_versions(&a, &b) == Ordering::Equal
            && compare_nix_versions(&b, &c) == Ordering::Equal
        {
            prop_assert_eq!(compare_nix_versions(&a, &c), Ordering::Equal);
        }
    }

    /// Never panics on fully arbitrary strings (unicode, control chars, etc.).
    #[test]
    fn version_never_panics_on_arbitrary(a in any::<String>(), b in any::<String>()) {
        let _ = compare_nix_versions(&a, &b);
    }

    /// Realization Eq/Hash are determined solely by the store path.
    #[test]
    fn realization_identity_eq_hash_law(
        path_a in store_path_strat(),
        path_b in store_path_strat(),
        pname_a in version_strat(),
        pname_b in version_strat(),
        ver_a in version_strat(),
        ver_b in version_strat()
    ) {
        let a = realization(&path_a, &pname_a, &ver_a);
        let b = realization(&path_b, &pname_b, &ver_b);
        let same_path = path_a == path_b;
        prop_assert_eq!(a == b, same_path, "Eq must follow store path");
        // Hashing guarantees equal => same hash (never the converse), so assert
        // hash equality only when the paths (hence the realizations) are equal.
        if same_path {
            prop_assert_eq!(hashed(&a), hashed(&b), "equal realizations must hash equally");
        }
        prop_assert_eq!(a.identity() == b.identity(), same_path);
    }

    /// Two realizations with the SAME store path but DIFFERING display metadata
    /// are equal (identity is the store path, per D-13 / INV-06).
    #[test]
    fn realization_same_path_differing_metadata_equal(
        path in store_path_strat(),
        pname_a in version_strat(),
        pname_b in version_strat(),
        ver_a in version_strat(),
        ver_b in version_strat()
    ) {
        let a = realization(&path, &pname_a, &ver_a);
        let b = realization(&path, &pname_b, &ver_b);
        prop_assert!(a == b, "display metadata must not affect identity");
        prop_assert_eq!(hashed(&a), hashed(&b));
        prop_assert_eq!(a.identity(), b.identity());
    }
}
