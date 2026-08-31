//! Package versions and Nix-native version comparison.
//!
//! A [`PackageVersion`] is the **raw** version string Nix emits for a
//! derivation (`env.version`). It is preserved byte-for-byte and the empty
//! string is **valid**, because Nix legitimately emits empty versions.
//!
//! `pname@version` is display metadata only (`plans/00` D-13 / `plans/05` §6):
//! it is never a unique identity. Accordingly [`PackageVersion`] implements
//! [`Eq`]/[`Hash`] as *literal string* equality, and deliberately does **not**
//! implement [`Ord`]/[`PartialOrd`], because Nix's comparison can report two
//! differently-spelled strings as equal (e.g. `1.0` == `1.00`).
//!
//! Use [`compare_nix_versions`] / [`PackageVersion::cmp_nix`] for ordering.
//! That function mirrors current upstream Nix `src/libstore/names.cc`
//! `compareVersions` exactly (see the module-level invariant notes in
//! [`compare_nix_versions`]). Do **not** invent SemVer, epoch handling,
//! arbitrary-precision numbers, split-on-dot-only behavior, or prerelease
//! rules.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Error returned when a [`VersionRange`] is constructed inconsistently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// A range must have at least one bound.
    EmptyRange,
    /// The lower bound sorts after the upper bound.
    InvertedRange,
    /// The bounds are equal but at least one is exclusive (empty interval).
    PointRangeNotInclusive,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionError::EmptyRange => f.write_str("version range must have at least one bound"),
            VersionError::InvertedRange => {
                f.write_str("version range lower bound is above the upper bound")
            }
            VersionError::PointRangeNotInclusive => {
                f.write_str("version range bounds are equal but not both inclusive")
            }
        }
    }
}

impl std::error::Error for VersionError {}

/// A raw Nix package version string.
///
/// Preserved exactly as emitted by Nix. Empty is valid. Equality and hashing
/// are **literal string** equality (D-13): `1.0` != `1.00`. For ordering use
/// [`PackageVersion::cmp_nix`].
#[derive(Debug, Clone)]
pub struct PackageVersion(String);

impl PackageVersion {
    /// Constructs a version from the raw Nix string, preserving it exactly.
    ///
    /// No validation is performed — any string (including the empty string,
    /// which Nix legitimately emits) is accepted.
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// Returns the raw version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compares this version against `other` using Nix's comparison rules
    /// (see [`compare_nix_versions`]).
    #[must_use]
    pub fn cmp_nix(&self, other: &PackageVersion) -> Ordering {
        compare_nix_versions(&self.0, &other.0)
    }
}

impl PartialEq for PackageVersion {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for PackageVersion {}

impl Hash for PackageVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Compares two version strings using Nix's `compareVersions` rules.
///
/// This mirrors current upstream Nix `src/libstore/names.cc` exactly:
///
/// - separators `.` and `-` are skipped;
/// - a component is the longest ASCII-digit run, otherwise the longest run
///   up to the next ASCII digit / `.` / `-`;
/// - numeric components are parsed as a signed 32-bit integer (matching
///   upstream `string2Int<int>`); if both sides parse, they compare
///   numerically — but equal numeric values (e.g. `01` vs `1`) do **not**
///   decide the comparison: the loop continues to the next component
///   (a value that overflows `i32` fails to parse and is treated as a word);
/// - the exact string component `pre` sorts before every component other than
///   `pre` itself;
/// - non-numeric components (including the empty component produced when one
///   side is exhausted) sort below numeric components;
/// - otherwise the components compare as bytes (UTF-8 order);
/// - comparison continues component-by-component until both inputs are
///   exhausted.
///
/// This defines a total *preorder*: two strings that differ in spelling but
/// are Nix-equal (e.g. `1.0` and `1.00`) compare [`Ordering::Equal`].
#[must_use]
pub fn compare_nix_versions(a: &str, b: &str) -> Ordering {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut i = 0usize;
    let mut j = 0usize;

    while i < a.len() || j < b.len() {
        // Skip separators in both.
        i = skip_version_separators(a, i);
        j = skip_version_separators(b, j);

        // Extract the next component: a maximal digit run, else a maximal run
        // of non-(digit/separator) bytes.
        let (a_comp, a_next) = next_version_component(a, i);
        let (b_comp, b_next) = next_version_component(b, j);
        i = a_next;
        j = b_next;

        // Equal components (byte-for-byte) contribute nothing.
        if a_comp == b_comp {
            continue;
        }
        // "pre" sorts before every component other than "pre" itself.
        if a_comp == b"pre" {
            return Ordering::Less;
        }
        if b_comp == b"pre" {
            return Ordering::Greater;
        }
        match (parse_i32(a_comp), parse_i32(b_comp)) {
            // Both numeric: only a *non-equal* numeric ordering decides the
            // result. Equal numeric values (e.g. `01` vs `1`) must CONTINUE
            // the outer loop so later components are still compared, matching
            // upstream Nix `compareVersions`.
            (Some(an), Some(bn)) => match an.cmp(&bn) {
                Ordering::Equal => continue,
                ord => return ord,
            },
            (Some(_), None) => return Ordering::Greater, // numeric > word/empty
            (None, Some(_)) => return Ordering::Less,
            (None, None) => {} // both words/empty -> lexical bytes
        }
        // Lexical byte comparison of the two word components.
        return a_comp.cmp(b_comp);
    }
    Ordering::Equal
}

/// Skips version separator bytes (`.` and `-`) from `index` onward.
fn skip_version_separators(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && (bytes[index] == b'.' || bytes[index] == b'-') {
        index += 1;
    }
    index
}

/// Returns the next version component and the index just past it.
///
/// A component is a maximal digit run, or a maximal run of bytes that are
/// neither digits nor separators. An exhausted input yields the empty slice.
fn next_version_component(bytes: &[u8], start: usize) -> (&[u8], usize) {
    if start >= bytes.len() {
        return (&[], start);
    }
    let mut end = start;
    if bytes[start].is_ascii_digit() {
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    } else {
        while end < bytes.len()
            && !bytes[end].is_ascii_digit()
            && bytes[end] != b'.'
            && bytes[end] != b'-'
        {
            end += 1;
        }
    }
    (&bytes[start..end], end)
}

/// Parses a component slice as a signed 32-bit integer, mirroring upstream
/// `string2Int<int>`: succeeds only for a non-empty all-ASCII-digit run that
/// fits in `i32` (leading zeros allowed). Returns `None` on overflow or for
/// non-numeric/empty input.
fn parse_i32(bytes: &[u8]) -> Option<i32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // Safe: digit runs are valid ASCII (hence valid UTF-8).
    std::str::from_utf8(bytes).ok()?.parse::<i32>().ok()
}

/// A user's version preference for a selector (`plans/04` §4.1 `versionPref`;
/// `plans/05` §5.1).
///
/// [`VersionPreference::Exact`] uses *literal* string equality (matching D-13:
/// `1.0` is not the same exact version as `1.00`).
/// [`VersionPreference::Minimum`] and [`VersionPreference::Range`] use Nix
/// comparison ([`compare_nix_versions`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VersionPreference {
    /// Any version (the default).
    Any,
    /// Exactly this version string, compared literally.
    Exact(PackageVersion),
    /// At least this version, by Nix comparison.
    Minimum(PackageVersion),
    /// Within an inclusive/exclusive range, by Nix comparison.
    Range(VersionRange),
}

impl VersionPreference {
    /// Returns `true` if `candidate` satisfies this preference.
    #[must_use]
    pub fn matches(&self, candidate: &PackageVersion) -> bool {
        match self {
            VersionPreference::Any => true,
            VersionPreference::Exact(v) => v == candidate,
            VersionPreference::Minimum(v) => {
                compare_nix_versions(candidate.as_str(), v.as_str()) != Ordering::Less
            }
            VersionPreference::Range(r) => r.contains(candidate),
        }
    }
}

/// One bound of a [`VersionRange`], either inclusive (`[v]`) or exclusive
/// (`(v)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionBound {
    version: PackageVersion,
    inclusive: bool,
}

impl VersionBound {
    /// Constructs an inclusive bound `[version]`.
    #[must_use]
    pub const fn inclusive(version: PackageVersion) -> Self {
        Self {
            version,
            inclusive: true,
        }
    }

    /// Constructs an exclusive bound `(version)`.
    #[must_use]
    pub const fn exclusive(version: PackageVersion) -> Self {
        Self {
            version,
            inclusive: false,
        }
    }

    /// Returns the bound's version.
    #[must_use]
    pub const fn version(&self) -> &PackageVersion {
        &self.version
    }

    /// Returns `true` if this bound is inclusive.
    #[must_use]
    pub const fn is_inclusive(&self) -> bool {
        self.inclusive
    }
}

/// A validated Nix-comparison version range with optional inclusive/exclusive
/// lower and upper bounds.
///
/// At least one bound must be present and the range must be non-empty. No
/// user-facing textual DSL is provided yet — construct via [`VersionRange::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionRange {
    lower: Option<VersionBound>,
    upper: Option<VersionBound>,
}

impl VersionRange {
    /// Constructs and validates a range.
    ///
    /// At least one of `lower`/`upper` must be [`Some`]; if both are present
    /// the lower bound must not sort above the upper bound, and equal bounds
    /// must both be inclusive.
    pub fn new(
        lower: Option<VersionBound>,
        upper: Option<VersionBound>,
    ) -> Result<Self, VersionError> {
        if lower.is_none() && upper.is_none() {
            return Err(VersionError::EmptyRange);
        }
        if let (Some(lo), Some(hi)) = (&lower, &upper) {
            let order = compare_nix_versions(lo.version.as_str(), hi.version.as_str());
            if order == Ordering::Greater {
                return Err(VersionError::InvertedRange);
            }
            if order == Ordering::Equal && !(lo.inclusive && hi.inclusive) {
                return Err(VersionError::PointRangeNotInclusive);
            }
        }
        Ok(Self { lower, upper })
    }

    /// Returns the lower bound, if any.
    #[must_use]
    pub const fn lower(&self) -> Option<&VersionBound> {
        self.lower.as_ref()
    }

    /// Returns the upper bound, if any.
    #[must_use]
    pub const fn upper(&self) -> Option<&VersionBound> {
        self.upper.as_ref()
    }

    /// Returns `true` if `candidate` lies within this range (Nix comparison).
    #[must_use]
    pub fn contains(&self, candidate: &PackageVersion) -> bool {
        if let Some(lo) = &self.lower {
            let order = compare_nix_versions(candidate.as_str(), lo.version.as_str());
            let ok = if lo.inclusive {
                order != Ordering::Less
            } else {
                order == Ordering::Greater
            };
            if !ok {
                return false;
            }
        }
        if let Some(hi) = &self.upper {
            let order = compare_nix_versions(candidate.as_str(), hi.version.as_str());
            let ok = if hi.inclusive {
                order != Ordering::Greater
            } else {
                order == Ordering::Less
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
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
}
