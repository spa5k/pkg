//! Tests for the `version` module.

use super::*;

fn v(s: &str) -> PackageVersion {
    PackageVersion::new(s)
}

fn assert_cmp(a: &str, b: &str, expected: Ordering, msg: &str) {
    assert_eq!(
        compare_nix_versions(a, b),
        expected,
        "{msg}: {a:?} vs {b:?}"
    );
    // Reverse symmetry.
    let rev = match expected {
        Ordering::Less => Ordering::Greater,
        Ordering::Greater => Ordering::Less,
        Ordering::Equal => Ordering::Equal,
    };
    assert_eq!(
        compare_nix_versions(b, a),
        rev,
        "reverse {msg}: {b:?} vs {a:?}"
    );
}

#[test]
fn empty_versions_are_equal_and_valid() {
    // Empty is valid; empty == empty; empty < numeric.
    assert_eq!(v("").as_str(), "");
    assert_cmp("", "", Ordering::Equal, "empty==empty");
    assert_cmp("", "1", Ordering::Less, "empty<num");
    assert_cmp("1", "", Ordering::Greater, "num>empty");
}

#[test]
fn separator_equivalence() {
    // '.' and '-' are equivalent separators.
    assert_cmp("1.0", "1-0", Ordering::Equal, "dot==dash");
    assert_cmp("1.2.3", "1-2-3", Ordering::Equal, "multi");
    // Leading/trailing/repeated separators are skipped.
    assert_cmp("1", "1.", Ordering::Equal, "trailing");
    assert_cmp("1", ".1", Ordering::Equal, "leading");
    assert_cmp("1..2", "1.2", Ordering::Equal, "repeated");
    assert_cmp("1.--2", "1.2", Ordering::Equal, "mixed-repeated");
}

#[test]
fn leading_zero_equivalence() {
    assert_cmp("1.007", "1.7", Ordering::Equal, "leading zeros");
    assert_cmp("007", "7", Ordering::Equal, "all zeros");
    assert_cmp("0", "000", Ordering::Equal, "zero variants");
}

#[test]
fn numeric_ordering() {
    assert_cmp("1", "2", Ordering::Less, "1<2");
    assert_cmp("2", "10", Ordering::Less, "2<10");
    assert_cmp("1.10", "1.9", Ordering::Greater, "1.10>1.9");
    assert_cmp("1.2.3", "1.2.4", Ordering::Less, "patch");
    assert_cmp("1.2", "1.2.0", Ordering::Less, "shorter<extra-num");
}

#[test]
fn numeric_equal_continues_to_next_component() {
    // Regression (P0): equal numeric values that differ lexically
    // (e.g. `01` vs `1`) must not decide the comparison; the loop
    // continues so later components still participate, matching upstream
    // Nix `compareVersions`.
    //
    // Forward cases.
    assert_eq!(compare_nix_versions("01a", "1b"), Ordering::Less);
    assert_eq!(compare_nix_versions("1.01a", "1.1b"), Ordering::Less);
    assert_eq!(compare_nix_versions("1.01", "1.1.2"), Ordering::Less);
    // Reverse cases.
    assert_eq!(compare_nix_versions("1b", "01a"), Ordering::Greater);
    assert_eq!(compare_nix_versions("1.1b", "1.01a"), Ordering::Greater);
    assert_eq!(compare_nix_versions("1.1.2", "1.01"), Ordering::Greater);
}

#[test]
fn pre_sorts_before() {
    // "pre" sorts before every component other than "pre".
    assert_cmp("1pre", "1", Ordering::Less, "pre<num");
    assert_cmp("1.0pre", "1.0", Ordering::Less, "pre<release");
    assert_cmp("pre", "1", Ordering::Less, "bare pre<num");
    assert_cmp("pre", "abc", Ordering::Less, "pre<word");
    assert_cmp("1pre", "1pre", Ordering::Equal, "pre==pre");
    // Empty component is still "after pre".
    assert_cmp("1", "1pre", Ordering::Greater, "release>pre");
}

#[test]
fn string_below_numeric() {
    assert_cmp("abc", "1", Ordering::Less, "word<num");
    assert_cmp("1.abc", "1.1", Ordering::Less, "word<num at pos");
    assert_cmp("1.z", "1.9", Ordering::Less, "z<9");
}

#[test]
fn lexical_for_words() {
    assert_cmp("abc", "abd", Ordering::Less, "lexical");
    assert_cmp("1.abc", "1.abd", Ordering::Less, "lexical at pos");
    assert_cmp("1.b", "1.a", Ordering::Greater, "lexical gt");
    // Empty word < non-empty word.
    assert_cmp("1.", "1.a", Ordering::Less, "empty-word<word");
}

#[test]
fn mixed_digit_and_string_runs() {
    // A run stops at the first digit: "a1b2" -> "a","1","b","2".
    assert_cmp("a1", "a2", Ordering::Less, "a1<a2");
    assert_cmp("a1", "a9", Ordering::Less, "a1<a9 numerically");
    assert_cmp("a1", "a10", Ordering::Less, "a1<a10");
    assert_cmp(
        "ripgrep-14.1.0",
        "ripgrep-14.1.1",
        Ordering::Less,
        "realistic",
    );
    assert_cmp("1a", "1b", Ordering::Less, "1a<1b");
    assert_cmp("1a2", "1a10", Ordering::Less, "embedded num");
}

#[test]
fn i32_overflow_behavior() {
    // i32::MAX = 2147483647 fits; 2147483648 overflows -> treated as a word.
    let max = "2147483647";
    let over = "2147483648";
    // Both are digit runs; the overflowing one parses as None -> word.
    // word < numeric, so the overflowing value sorts *below* the max.
    assert_cmp(over, max, Ordering::Less, "overflow<max");
    // Two overflowing values compare lexically (both None).
    let big1 = "99999999999"; // 11 nines, overflows
    let big2 = "99999999998";
    assert_cmp(big1, big2, Ordering::Greater, "two overflow lexical");
    // A small fitting number sorts above an overflowing "word".
    assert_cmp("9", big1, Ordering::Greater, "small-fit > overflow-word");
}

#[test]
fn unicode_words_compare_as_bytes() {
    // Non-ASCII bytes are >= 0x80, not digits/separators, so whole
    // multibyte chars form word components compared by UTF-8 byte order
    // (which matches Unicode code-point order). ASCII sorts before
    // non-ASCII because ASCII bytes are all < 0x80.
    assert_eq!(compare_nix_versions("ä", "ö"), Ordering::Less); // U+00E4 < U+00F6
    assert_eq!(compare_nix_versions("z", "ä"), Ordering::Less); // ASCII < non-ASCII
    assert_eq!(compare_nix_versions("cafö", "cafä"), Ordering::Greater);
}

#[test]
fn package_version_literal_eq_and_hash() {
    // Literal equality: differently-spelled Nix-equal strings are NOT Eq.
    assert_ne!(v("1.0"), v("1.00"));
    assert_eq!(v("1.0"), v("1.0"));
    // Hash agrees with Eq.
    fn h(x: &PackageVersion) -> u64 {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        x.hash(&mut s);
        s.finish()
    }
    assert_eq!(h(&v("1.0")), h(&v("1.0")));
    // cmp_nix, however, reports them equal.
    assert_eq!(v("1.0").cmp_nix(&v("1.00")), Ordering::Equal);
    assert_eq!(v("1.0").cmp_nix(&v("1.2")), Ordering::Less);
}

#[test]
fn exact_vs_nix_version_distinction() {
    // Exact uses literal Eq.
    let exact = VersionPreference::Exact(v("1.0"));
    assert!(exact.matches(&v("1.0")));
    assert!(!exact.matches(&v("1.00")));
    assert!(!exact.matches(&v("1.0.0")));
    // Minimum uses cmp_nix.
    let min = VersionPreference::Minimum(v("1.0"));
    assert!(min.matches(&v("1.0")));
    assert!(min.matches(&v("1.00"))); // Nix-equal
    assert!(min.matches(&v("2")));
    assert!(!min.matches(&v("0.9")));
    // Any matches everything.
    assert!(VersionPreference::Any.matches(&v("")));
    assert!(VersionPreference::Any.matches(&v("9.9")));
}

#[test]
fn version_range_validation_and_contains() {
    let lo = VersionBound::inclusive(v("1.0"));
    let hi = VersionBound::inclusive(v("2.0"));
    let r = VersionRange::new(Some(lo), Some(hi)).unwrap();
    assert!(!r.contains(&v("0.9")));
    assert!(r.contains(&v("1.0")));
    assert!(r.contains(&v("1.5")));
    assert!(r.contains(&v("2.0")));
    assert!(!r.contains(&v("2.1")));

    // Exclusive upper.
    let r2 = VersionRange::new(
        Some(VersionBound::inclusive(v("1.0"))),
        Some(VersionBound::exclusive(v("2.0"))),
    )
    .unwrap();
    assert!(r2.contains(&v("1.0")));
    assert!(!r2.contains(&v("2.0")));

    // Half-open.
    let r3 = VersionRange::new(Some(VersionBound::inclusive(v("1.0"))), None).unwrap();
    assert!(r3.contains(&v("99")));
    assert!(!r3.contains(&v("0.5")));

    // Nix-equal boundary still contained with inclusive bounds.
    let r4 = VersionRange::new(
        Some(VersionBound::inclusive(v("1.0"))),
        Some(VersionBound::inclusive(v("1.00"))),
    )
    .unwrap();
    assert!(r4.contains(&v("1.0")));
}

#[test]
fn version_range_rejects_invalid() {
    // Empty.
    assert_eq!(
        VersionRange::new(None, None).unwrap_err(),
        VersionError::EmptyRange
    );
    // Inverted.
    assert_eq!(
        VersionRange::new(
            Some(VersionBound::inclusive(v("2.0"))),
            Some(VersionBound::inclusive(v("1.0")))
        )
        .unwrap_err(),
        VersionError::InvertedRange
    );
    // Equal but not both inclusive.
    assert_eq!(
        VersionRange::new(
            Some(VersionBound::exclusive(v("1.0"))),
            Some(VersionBound::inclusive(v("1.0")))
        )
        .unwrap_err(),
        VersionError::PointRangeNotInclusive
    );
    assert_eq!(
        VersionRange::new(
            Some(VersionBound::exclusive(v("1.0"))),
            Some(VersionBound::exclusive(v("1.0")))
        )
        .unwrap_err(),
        VersionError::PointRangeNotInclusive
    );
    // Equal and both inclusive is OK (a point range).
    assert!(
        VersionRange::new(
            Some(VersionBound::inclusive(v("1.0"))),
            Some(VersionBound::inclusive(v("1.0")))
        )
        .is_ok()
    );
}
