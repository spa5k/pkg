//! Channel vocabulary: sequence numbers, policy versions, channel names,
//! Nixpkgs revisions, and source-revision selectors.
//!
//! This implements the channel/manifest fields referenced by:
//! - `plans/00-overview-and-decisions.md` INV-04 (mixed, exact-pinned
//!   revisions; per-selector `sourceRev` ∈ `channel:current` |
//!   `channel:pinned:<id>` | `rev:<gitsha>`),
//! - `plans/02-trust-and-update-model.md` §7 (channel descriptor:
//!   `sequence`, `policyVersion`, `channel`),
//! - `plans/05-state-locks-generations-gc.md` §5.1 / §7
//!   (`sourceRev`, selective upgrades, mixed revisions).
//!
//! These are *value* types only — no TUF, persistence, expiry, or timestamps.

use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

/// Error returned when a channel-related value cannot be parsed or validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// A channel sequence or policy version was not a positive decimal number.
    InvalidNumber {
        /// The rejected input.
        input: String,
    },
    /// A channel name failed validation.
    InvalidChannelName {
        /// The rejected input.
        input: String,
    },
    /// A Nixpkgs git revision failed validation.
    InvalidNixpkgsRevision {
        /// The rejected input.
        input: String,
    },
    /// A source-revision selector failed validation.
    InvalidSourceRevision {
        /// The rejected input.
        input: String,
    },
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::InvalidNumber { input } => write!(
                f,
                "invalid channel sequence/policy version {input:?}: expected a positive decimal integer"
            ),
            ChannelError::InvalidChannelName { input } => write!(
                f,
                "invalid channel name {input:?}: must be a nonempty [A-Za-z0-9._-] string"
            ),
            ChannelError::InvalidNixpkgsRevision { input } => write!(
                f,
                "invalid nixpkgs revision {input:?}: must be exactly 40 lowercase hex characters"
            ),
            ChannelError::InvalidSourceRevision { input } => write!(
                f,
                "invalid source revision {input:?}: expected `channel:current`, \
                 `channel:pinned:<n>`, or `rev:<40-hex-sha>`"
            ),
        }
    }
}

impl std::error::Error for ChannelError {}

/// A monotonically-checked channel sequence number (`plans/02` §7 `sequence`;
/// `plans/01` §10 `channelSeq`).
///
/// A strong newtype over [`NonZeroU64`]. Sequence numbers are strictly
/// positive (zero is rejected) and increment helpers **never wrap**: use
/// [`ChannelSequence::successor`] to advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelSequence(NonZeroU64);

impl ChannelSequence {
    /// Constructs a sequence number from a nonzero value.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Constructs a sequence number from a raw `u64`, returning `None` for
    /// zero.
    #[must_use]
    pub const fn from_u64(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }

    /// Returns the underlying value.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }

    /// Returns `true` if `self` is a strictly later sequence than `other`.
    #[must_use]
    pub const fn is_strictly_after(self, other: Self) -> bool {
        self.0.get() > other.0.get()
    }

    /// Returns the next sequence number, or `None` if it would overflow `u64`.
    ///
    /// This never wraps.
    #[must_use]
    pub const fn successor(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }
}

impl fmt::Display for ChannelSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for ChannelSequence {
    type Err = ChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Reject empty and anything that is not a clean base-10 integer so we
        // never accidentally accept "+1", "0x1", " 1 ", etc.
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ChannelError::InvalidNumber {
                input: s.to_owned(),
            });
        }
        let value = s.parse::<u64>().map_err(|_| ChannelError::InvalidNumber {
            input: s.to_owned(),
        })?;
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(ChannelError::InvalidNumber {
                input: s.to_owned(),
            })
    }
}

/// A channel policy version (`plans/02` §7 `policyVersion`).
///
/// A distinct strong type from [`ChannelSequence`] so the two can never be
/// confused. Monotonic and never wrapping, like [`ChannelSequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PolicyVersion(NonZeroU64);

impl PolicyVersion {
    /// Constructs a policy version from a nonzero value.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Constructs a policy version from a raw `u64`, returning `None` for
    /// zero.
    #[must_use]
    pub const fn from_u64(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }

    /// Returns the underlying value.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }

    /// Returns `true` if `self` is a strictly later policy version than
    /// `other`.
    #[must_use]
    pub const fn is_strictly_after(self, other: Self) -> bool {
        self.0.get() > other.0.get()
    }
}

impl fmt::Display for PolicyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for PolicyVersion {
    type Err = ChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ChannelError::InvalidNumber {
                input: s.to_owned(),
            });
        }
        let value = s.parse::<u64>().map_err(|_| ChannelError::InvalidNumber {
            input: s.to_owned(),
        })?;
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(ChannelError::InvalidNumber {
                input: s.to_owned(),
            })
    }
}

/// A human channel label (`plans/02` §7 `channel`).
///
/// Validated as a nonempty run of `[A-Za-z0-9._-]`. This is display metadata
/// only — never a trust or identity input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelName(String);

impl ChannelName {
    /// Validates and constructs a channel name.
    pub fn new(value: &str) -> Result<Self, ChannelError> {
        if is_channel_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ChannelError::InvalidChannelName {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the channel name string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChannelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ChannelName {
    type Err = ChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `s` is a valid [`ChannelName`] (nonempty
/// `[A-Za-z0-9._-]+`).
fn is_channel_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A pinned Nixpkgs git revision: exactly 40 lowercase hex characters.
///
/// This is the SHA-1 commit hash form used in `plans/01` §10.2 /
/// `plans/05` §5.2 (`nixpkgsRev`). It is **not** a NAR hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NixpkgsRevision(String);

impl NixpkgsRevision {
    /// Validates and constructs a Nixpkgs revision.
    pub fn new(value: &str) -> Result<Self, ChannelError> {
        if is_nixpkgs_revision(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ChannelError::InvalidNixpkgsRevision {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the 40-character lowercase-hex revision.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NixpkgsRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NixpkgsRevision {
    type Err = ChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns `true` if `s` is exactly 40 lowercase hex characters.
fn is_nixpkgs_revision(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Which Nixpkgs revision a selector resolves against (INV-04).
///
/// Every variant names an **exact, pinned** revision — never a floating
/// channel. Canonical display strings are:
/// - [`SourceRevision::CurrentChannel`] → `channel:current`
/// - [`SourceRevision::PinnedChannel`] → `channel:pinned:<n>`
/// - [`SourceRevision::ExactRevision`] → `rev:<40-hex-sha>`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceRevision {
    /// The currently-accepted channel descriptor's pinned revision.
    CurrentChannel,
    /// An older channel sequence (still within the managed window).
    PinnedChannel(ChannelSequence),
    /// An exact git revision.
    ExactRevision(NixpkgsRevision),
}

impl SourceRevision {
    /// Returns the canonical display string for this source revision.
    #[must_use]
    pub fn to_canonical_string(&self) -> String {
        match self {
            SourceRevision::CurrentChannel => "channel:current".to_owned(),
            SourceRevision::PinnedChannel(seq) => format!("channel:pinned:{seq}"),
            SourceRevision::ExactRevision(rev) => format!("rev:{rev}"),
        }
    }
}

impl fmt::Display for SourceRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_canonical_string())
    }
}

impl FromStr for SourceRevision {
    type Err = ChannelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "channel:current" {
            return Ok(SourceRevision::CurrentChannel);
        }
        if let Some(rest) = s.strip_prefix("channel:pinned:") {
            let seq = ChannelSequence::from_str(rest).map_err(|_| {
                ChannelError::InvalidSourceRevision {
                    input: s.to_owned(),
                }
            })?;
            return Ok(SourceRevision::PinnedChannel(seq));
        }
        if let Some(rest) = s.strip_prefix("rev:") {
            let rev =
                NixpkgsRevision::new(rest).map_err(|_| ChannelError::InvalidSourceRevision {
                    input: s.to_owned(),
                })?;
            return Ok(SourceRevision::ExactRevision(rev));
        }
        Err(ChannelError::InvalidSourceRevision {
            input: s.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_sequence_round_trip_and_accessors() {
        let s = ChannelSequence::from_str("42").unwrap();
        assert_eq!(s.get().get(), 42);
        assert_eq!(s.to_string(), "42");
        assert_eq!(ChannelSequence::from_str(&s.to_string()).unwrap(), s);
        assert_eq!(ChannelSequence::from_u64(0), None);
        assert_eq!(
            ChannelSequence::from_u64(7).unwrap(),
            ChannelSequence::from_str("7").unwrap()
        );
    }

    #[test]
    fn channel_sequence_rejects_bad() {
        for bad in ["", "0", "-1", "abc", "0x1", " 1", "1 ", "+1", "1.0"] {
            assert!(
                ChannelSequence::from_str(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn channel_sequence_monotonic_no_wrap() {
        let one = ChannelSequence::from_u64(1).unwrap();
        assert_eq!(one.successor(), Some(ChannelSequence::from_u64(2).unwrap()));
        assert!(one.successor().unwrap().is_strictly_after(one));
        let max = ChannelSequence::new(NonZeroU64::MAX);
        assert_eq!(max.successor(), None);
        assert!(!max.is_strictly_after(max));
    }

    #[test]
    fn policy_version_is_distinct_type() {
        let p = PolicyVersion::from_str("3").unwrap();
        assert_eq!(p.get().get(), 3);
        assert_eq!(p.to_string(), "3");
        // Compile-time distinctness: the two are different types; just exercise
        // that the same parser text yields equal values within each type.
        assert_eq!(
            PolicyVersion::from_str("3").unwrap(),
            PolicyVersion::from_u64(3).unwrap()
        );
        assert!(PolicyVersion::from_str("0").is_err());
    }

    #[test]
    fn channel_name_valid_and_invalid() {
        let ok = ["pkg-stable-1", "edge", "a.b.c", "nixpkgs_24_05", "X9"];
        for s in ok {
            let n = ChannelName::new(s).unwrap();
            assert_eq!(n.as_str(), s);
            assert_eq!(n.to_string(), s);
            assert_eq!(ChannelName::from_str(s).unwrap(), n);
        }
        let bad = ["", "bad name", "café", "a/b", "a#b", "a b", "中"];
        for s in bad {
            assert!(ChannelName::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn nixpkgs_revision_valid_and_invalid() {
        let good = "0123456789abcdef0123456789abcdef01234567";
        let r = NixpkgsRevision::new(good).unwrap();
        assert_eq!(r.as_str(), good);
        assert_eq!(r.to_string(), good);
        let bad = [
            "",
            "short",
            "0123456789abcdef0123456789abcdef0123456789", // 41
            "0123456789abcdef0123456789abcdef0123456",    // 39
            "0123456789ABCDEF0123456789abcdef01234567",   // uppercase
            "g123456789abcdef0123456789abcdef01234567",   // non-hex
        ];
        for s in bad {
            assert!(NixpkgsRevision::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn source_revision_display_round_trip() {
        let rev = NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap();
        let cases = [
            (SourceRevision::CurrentChannel, "channel:current"),
            (
                SourceRevision::PinnedChannel(ChannelSequence::from_u64(7).unwrap()),
                "channel:pinned:7",
            ),
            (
                SourceRevision::ExactRevision(rev),
                "rev:0123456789abcdef0123456789abcdef01234567",
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(value.to_string(), expected);
            let parsed = SourceRevision::from_str(expected).unwrap();
            assert_eq!(parsed, value);
            assert_eq!(parsed.to_string(), expected);
        }
    }

    #[test]
    fn source_revision_rejects_bad() {
        let bad = [
            "",
            "channel",
            "channel:future",
            "channel:pinned:",
            "channel:pinned:0",
            "channel:pinned:-1",
            "channel:pinned:abc",
            "rev:short",
            "rev:0123",
            "github:NixOS/nixpkgs",
        ];
        for s in bad {
            assert!(SourceRevision::from_str(s).is_err(), "should reject {s:?}");
        }
    }
}
