//! Canonical artifact identity: store paths, derivations, output names, NAR
//! hashes, and the [`RealizationIdentity`] that uniquely names a realized
//! artifact.
//!
//! Per `plans/05` §6 and `plans/00` INV-06, the canonical identity of a
//! realized artifact is its **store path** alone — `pname@version` is
//! display-only. [`RealizationIdentity`] is a strong wrapper around
//! [`StorePath`] with no competing composite identity API.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

/// The fixed prefix of every Nix store path.
const STORE_PREFIX: &str = "/nix/store/";

/// The Nix "base32" alphabet used for store-path hashes (32 symbols; note the
/// absence of `e`, `o`, `t`, `u`).
const NIX_BASE32: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// The required length of a store-path digest, in Nix32 characters.
///
/// A rendered store path encodes a **20-byte (160-bit)** digest as 32 Nix32
/// characters (5 bits each). Upstream Nix derives this digest from a SHA-256
/// *fingerprint* compressed to 160 bits. It is **opaque** and is **not** the
/// raw NAR SHA-256 (see [`NarHash`]).
const STORE_HASH_LEN: usize = 32;

/// The maximum length, in bytes, of a store-path name (upstream Nix
/// `StorePath::MaxPathLen`). The same cap applies to [`OutputName`] values,
/// since an output name may be any valid store-path name.
const MAX_NAME_LEN: usize = 211;

/// Error returned when an identity value cannot be parsed or validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// A store path failed validation.
    InvalidStorePath {
        /// The rejected input.
        input: String,
    },
    /// A store path did not name a derivation (`.drv`).
    NotADerivation {
        /// The rejected input.
        input: String,
    },
    /// An output name failed validation.
    InvalidOutputName {
        /// The rejected input.
        input: String,
    },
    /// A NAR hash failed validation.
    InvalidNarHash {
        /// The rejected input.
        input: String,
    },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::InvalidStorePath { input } => write!(
                f,
                "invalid store path {input:?}: expected `/nix/store/<32-char-hash>-<name>`"
            ),
            IdentityError::NotADerivation { input } => {
                write!(
                    f,
                    "not a derivation path {input:?}: name must end in `.drv`"
                )
            }
            IdentityError::InvalidOutputName { input } => write!(
                f,
                "invalid output name {input:?}: must be a nonempty Nix output token"
            ),
            IdentityError::InvalidNarHash { input } => write!(
                f,
                "invalid nar hash {input:?}: expected `sha256-<44-char-base64>`"
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

/// Returns `true` if `b` is a valid store-path *name* byte: ASCII alphanumeric
/// or one of `+ - . _ ? =`. No slash, no control characters.
const fn is_store_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
}

/// Validates `name` as a Nix store-path name (and hence as a valid output
/// name), matching current upstream Nix `src/libstore/path.cc` `checkName`
/// exactly:
///
/// - nonempty;
/// - at most [`MAX_NAME_LEN`] (211) bytes;
/// - not exactly `.` or `..`;
/// - the first dash-separated component is not `.` or `..` (rejects names
///   beginning `.-` or `..-`, while still allowing `.foo`, `...`,
///   `...-foo`, …);
/// - every byte is ASCII alphanumeric or one of `+ - . _ ? =` (upstream
///   explicitly includes `=`).
///
/// With no `-`, the whole name is the first component, so bare `.`/`..` are
/// rejected here too (they are also checked explicitly above to match upstream
/// `checkName` line for line). This is the single shared validator used by both
/// [`StorePath`] and [`OutputName`].
fn is_valid_store_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    if name == b"." || name == b".." {
        return false;
    }
    if !name.iter().all(|&b| is_store_name_byte(b)) {
        return false;
    }
    let first_component = name.split(|&b| b == b'-').next().unwrap_or(&[]);
    first_component != b"." && first_component != b".."
}

/// Returns `true` if `bytes` is a valid Nix store-path hash: exactly
/// [`STORE_HASH_LEN`] characters, each in the Nix base32 alphabet.
fn is_store_hash(bytes: &[u8]) -> bool {
    bytes.len() == STORE_HASH_LEN && bytes.iter().all(|b| NIX_BASE32.contains(b))
}

/// A validated Nix store path: `/nix/store/<digest>-<name>`.
///
/// The 32-character `digest` uses Nix's base32 alphabet
/// (`0123456789abcdfghijklmnpqrsvwxyz`) and encodes a **20-byte (160-bit)**
/// opaque store-path digest — upstream Nix derives it from a SHA-256
/// fingerprint compressed to 160 bits, and it is **not** the raw NAR SHA-256
/// (that is [`NarHash`]). The `name` follows upstream Nix `checkName`:
/// nonempty, at most 211 bytes, not `.`/`..`, no leading `.`/`..`
/// dash-component, and only ASCII alphanumeric bytes or `+ - . _ ? =`.
#[derive(Debug, Clone)]
pub struct StorePath {
    raw: String,
    // Byte offset where the name begins (after the `-` following the hash).
    name_offset: usize,
}

impl StorePath {
    /// Validates and constructs a store path.
    pub fn new(path: &str) -> Result<Self, IdentityError> {
        let (hash, name, name_offset) =
            split_store_path(path).ok_or_else(|| IdentityError::InvalidStorePath {
                input: path.to_owned(),
            })?;
        if !is_store_hash(hash) || !is_valid_store_name(name) {
            return Err(IdentityError::InvalidStorePath {
                input: path.to_owned(),
            });
        }
        Ok(Self {
            raw: path.to_owned(),
            name_offset,
        })
    }

    /// Returns the full store path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns the 32-character Nix-base32 store-path digest portion of the
    /// path.
    ///
    /// This encodes a **20-byte (160-bit)** digest. Upstream Nix derives it from
    /// a SHA-256 *fingerprint* compressed to 160 bits; it is **opaque** and is
    /// **not** the raw NAR SHA-256 (which lives in [`NarHash`]). Treat it as an
    /// opaque path identifier.
    ///
    /// Named `digest` (rather than `hash`) so it does not shadow the [`Hash`]
    /// trait method on this type.
    #[must_use]
    pub fn digest(&self) -> &str {
        // The digest occupies bytes [STORE_PREFIX.len() .. name_offset - 1).
        &self.raw[STORE_PREFIX.len()..self.name_offset - 1]
    }

    /// Returns the name portion (everything after the hash and its `-`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.raw[self.name_offset..]
    }
}

impl PartialEq for StorePath {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for StorePath {}

impl Hash for StorePath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for StorePath {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Splits a candidate store path into `(hash, name, name_offset)`.
///
/// Returns `None` if the prefix is wrong or no `-` separates the hash from a
/// nonempty name.
fn split_store_path(path: &str) -> Option<(&[u8], &[u8], usize)> {
    let bytes = path.as_bytes();
    let prefix = STORE_PREFIX.as_bytes();
    if bytes.len() < prefix.len() + STORE_HASH_LEN + 2 || !bytes.starts_with(prefix) {
        return None;
    }
    let hash = &bytes[prefix.len()..prefix.len() + STORE_HASH_LEN];
    let sep = bytes[prefix.len() + STORE_HASH_LEN];
    if sep != b'-' {
        return None;
    }
    let name_offset = prefix.len() + STORE_HASH_LEN + 1;
    let name = &bytes[name_offset..];
    Some((hash, name, name_offset))
}

/// A validated derivation store path: a [`StorePath`] whose name ends in
/// `.drv`.
#[derive(Debug, Clone)]
pub struct DerivationPath(StorePath);

impl DerivationPath {
    /// Wraps a [`StorePath`], requiring its name to end in `.drv`.
    pub fn new(path: StorePath) -> Result<Self, IdentityError> {
        if path.name().ends_with(".drv") {
            Ok(Self(path))
        } else {
            Err(IdentityError::NotADerivation {
                input: path.as_str().to_owned(),
            })
        }
    }

    /// Returns the underlying store path.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.0
    }

    /// Returns the full derivation path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for DerivationPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for DerivationPath {}

impl Hash for DerivationPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Display for DerivationPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for DerivationPath {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = StorePath::new(s)?;
        Self::new(path)
    }
}

/// A validated Nix output name (e.g. `out`, `man`, `lib`).
///
/// Official Nix permits any valid store-path name as an output name, so output
/// names are validated with the exact same rules as [`StorePath`] names:
/// nonempty, at most 211 bytes, not `.`/`..`, no leading `.`/`..`
/// dash-component, and only ASCII alphanumeric bytes or `+ - . _ ? =`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputName(String);

impl OutputName {
    /// Validates and constructs an output name.
    pub fn new(name: &str) -> Result<Self, IdentityError> {
        if is_valid_store_name(name.as_bytes()) {
            Ok(Self(name.to_owned()))
        } else {
            Err(IdentityError::InvalidOutputName {
                input: name.to_owned(),
            })
        }
    }

    /// Returns the output name string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OutputName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for OutputName {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// A validated `sha256` SRI NAR hash: `sha256-` followed by exactly 43 base64
/// characters and a single `=` padding character (encoding 32 bytes).
///
/// Kept distinct from the store-path digest ([`StorePath::digest`]). Validated
/// with simple byte checks — no regex or base64 dependency.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NarHash(String);

/// The required prefix of an SRI `sha256` NAR hash.
const SRI_PREFIX: &str = "sha256-";

impl NarHash {
    /// Validates and constructs a NAR hash.
    pub fn new(value: &str) -> Result<Self, IdentityError> {
        if is_nar_hash(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(IdentityError::InvalidNarHash {
                input: value.to_owned(),
            })
        }
    }

    /// Returns the SRI string (`sha256-<base64>`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NarHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NarHash {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Returns the 6-bit value of a standard base64 symbol, or `None` if `b` is
/// not a valid base64 alphabet byte (`A-Z`, `a-z`, `0-9`, `+`, `/`).
const fn base64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Returns `true` if `s` is a canonical `sha256` SRI NAR hash: the literal
/// prefix, exactly 43 base64 characters, then a single `=` (encoding exactly
/// 32 bytes), with the two unused low bits of the last non-padding symbol set
/// to zero (canonical base64 padding).
fn is_nar_hash(s: &str) -> bool {
    let bytes = s.as_bytes();
    let prefix = SRI_PREFIX.as_bytes();
    // `sha256-` (7) + 43 base64 symbols + 1 `=` padding == 51 bytes, encoding
    // exactly 32 bytes.
    if bytes.len() != prefix.len() + 44 || !bytes.starts_with(prefix) {
        return false;
    }
    let payload = &bytes[prefix.len()..];
    // Exactly one `=` padding following 43 standard-alphabet symbols.
    if payload[43] != b'=' || !payload[..43].iter().all(|&b| base64_value(b).is_some()) {
        return false;
    }
    // Canonical padding: a 32-byte digest leaves exactly two unused low bits in
    // the last non-padding symbol (index 42); those bits must be zero, so the
    // symbol's 6-bit value must be divisible by 4.
    base64_value(payload[42]).is_some_and(|v| v % 4 == 0)
}

/// The canonical identity of a realized artifact: the store path alone
/// (`plans/05` §6, `plans/00` INV-06).
///
/// `pname@version` is **not** an identity. This is a strong wrapper over
/// [`StorePath`] with [`Eq`]/[`Hash`]/[`Ord`] delegating to the store path, so
/// it can be used directly as a map key.
#[derive(Debug, Clone)]
pub struct RealizationIdentity(StorePath);

impl RealizationIdentity {
    /// Constructs an identity from a store path.
    #[must_use]
    pub fn new(store_path: StorePath) -> Self {
        Self(store_path)
    }

    /// Returns the store path that is this identity.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.0
    }

    /// Returns the full store path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for RealizationIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for RealizationIdentity {}

impl Hash for RealizationIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialOrd for RealizationIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RealizationIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_str().cmp(other.0.as_str())
    }
}

impl fmt::Display for RealizationIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz"; // 32 valid chars
    const GOOD_PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-14.1.0";
    const GOOD_DRV: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-ripgrep-14.1.0.drv";
    const GOOD_NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn store_path_valid() {
        let p = StorePath::new(GOOD_PATH).unwrap();
        assert_eq!(p.as_str(), GOOD_PATH);
        assert_eq!(p.digest(), GOOD_HASH);
        assert_eq!(p.name(), "ripgrep-14.1.0");
        assert_eq!(p.to_string(), GOOD_PATH);
        assert_eq!(StorePath::from_str(GOOD_PATH).unwrap(), p);
        // Name may contain + . _ ? =
        let p2 = StorePath::new("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-pkg+1.2_3?x=y.bin")
            .unwrap();
        assert_eq!(p2.name(), "pkg+1.2_3?x=y.bin");
    }

    #[test]
    fn store_path_invalid() {
        let bad = [
            "",
            "/nix/store/x-foo",                                     // short hash
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyzz-foo",     // 33 hash chars
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyzfoo",       // missing dash
            "/nix/store/0123456789abcdfghijklmnopqrstuvwxy-foo",    // 't'/'u' not in alphabet
            "/nix/store/0123456789ABCDFGHIJKLMNOPQRSVWXYZ-foo",     // uppercase
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-",         // empty name
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-bad/name", // slash in name
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-bad name", // space in name
            "/other/store/0123456789abcdfghijklmnpqrsvwxyz-foo",    // wrong prefix
            "/nix/store",                                           // too short
        ];
        for s in bad {
            assert!(StorePath::new(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn derivation_path_requires_drv() {
        let drv = DerivationPath::from_str(GOOD_DRV).unwrap();
        assert_eq!(drv.as_str(), GOOD_DRV);
        assert_eq!(drv.store_path().name(), "ripgrep-14.1.0.drv");
        // Wrapping a non-.drv store path fails.
        let sp = StorePath::new(GOOD_PATH).unwrap();
        assert!(matches!(
            DerivationPath::new(sp),
            Err(IdentityError::NotADerivation { .. })
        ));
    }

    #[test]
    fn output_name_valid_and_invalid() {
        for ok in ["out", "man", "lib", "dev", "a1", "x.y", "bin+"] {
            let n = OutputName::new(ok).unwrap();
            assert_eq!(n.as_str(), ok);
            assert_eq!(n.to_string(), ok);
        }
        let bad = ["", "out bin", "out/bin", "out#bin", "café", "out\n"];
        for s in bad {
            assert!(OutputName::new(s).is_err(), "should reject {s:?}");
        }
        // Ordering usable for BTreeMap keys.
        assert!(OutputName::new("a").unwrap() < OutputName::new("b").unwrap());
    }

    #[test]
    fn nar_hash_valid_and_invalid() {
        let h = NarHash::new(GOOD_NAR).unwrap();
        assert_eq!(h.as_str(), GOOD_NAR);
        assert_eq!(h.to_string(), GOOD_NAR);
        // A realistic sha256 SRI.
        let real = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        assert!(NarHash::new(real).is_ok());
        let bad = [
            "",
            "sha256-AAAA",                                         // too short
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", // 44 no padding
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA#", // bad char
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", // wrong prefix
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",  // missing '='
        ];
        for s in bad {
            assert!(NarHash::new(s).is_err(), "should reject {s:?}");
        }
        // Noncanonical base64 padding: exactly 43 symbols + '=', but the last
        // non-padding symbol ('B', value 1) carries nonzero padding bits
        // (1 % 4 != 0) and must be rejected.
        let mut noncanonical = String::from("sha256-");
        noncanonical.push_str(&"A".repeat(42));
        noncanonical.push('B');
        noncanonical.push('=');
        assert_eq!(noncanonical.len(), "sha256-".len() + 44);
        assert!(
            NarHash::new(&noncanonical).is_err(),
            "should reject noncanonical padding bits"
        );
    }

    #[test]
    fn realization_identity_eq_hash_ord_on_store_path() {
        let id_a = RealizationIdentity::new(StorePath::new(GOOD_PATH).unwrap());
        let id_a2 = RealizationIdentity::new(StorePath::new(GOOD_PATH).unwrap());
        let id_b = RealizationIdentity::new(
            StorePath::new("/nix/store/11111111111111111111111111111111-other-1.0").unwrap(),
        );

        assert_eq!(id_a, id_a2);
        assert_ne!(id_a, id_b);
        assert_eq!(id_a.as_str(), GOOD_PATH);
        assert_eq!(id_a.store_path().name(), "ripgrep-14.1.0");

        fn hash<T: Hash>(x: &T) -> u64 {
            let mut s = std::collections::hash_map::DefaultHasher::new();
            x.hash(&mut s);
            s.finish()
        }
        assert_eq!(hash(&id_a), hash(&id_a2));

        // Ord by store path bytes.
        let mut sorted = [id_b.clone(), id_a.clone()];
        sorted.sort();
        assert_eq!(sorted[0], id_a); // "0..." < "1..."
        assert_eq!(id_a.cmp(&id_b), std::cmp::Ordering::Less);
    }

    #[test]
    fn store_name_checkname_rules() {
        const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz"; // 32 valid Nix32 chars

        // Boundary on the 211-byte name cap (applies to the NAME, not the
        // whole path): exactly 211 accepted, 212 rejected.
        let name_211 = "a".repeat(211);
        let name_212 = "a".repeat(212);
        assert!(StorePath::new(&format!("/nix/store/{HASH}-{name_211}")).is_ok());
        assert!(StorePath::new(&format!("/nix/store/{HASH}-{name_212}")).is_err());
        assert!(OutputName::new(&name_211).is_ok());
        assert!(OutputName::new(&name_212).is_err());

        // Rejected by checkName: ".", "..", and names whose first
        // dash-component is "." or ".." (e.g. ".-x", "..-x").
        for bad_name in [".", "..", ".-x", "..-x"] {
            assert!(
                StorePath::new(&format!("/nix/store/{HASH}-{bad_name}")).is_err(),
                "store path name {bad_name:?} should be rejected"
            );
            assert!(
                OutputName::new(bad_name).is_err(),
                "output name {bad_name:?} should be rejected"
            );
        }

        // Allowed leading-dot forms (first dash-component is not "." / "..").
        for ok_name in [".foo", "...", "...-foo"] {
            assert!(
                StorePath::new(&format!("/nix/store/{HASH}-{ok_name}")).is_ok(),
                "store path name {ok_name:?} should be accepted"
            );
            assert!(
                OutputName::new(ok_name).is_ok(),
                "output name {ok_name:?} should be accepted"
            );
        }
    }

    #[test]
    fn store_name_accepts_equals_sign() {
        // Upstream Nix explicitly allows '=' in store-path/output names; pin
        // that acceptance so a future cleanup cannot silently drop it.
        const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
        assert!(StorePath::new(&format!("/nix/store/{HASH}-name=k=v")).is_ok());
        assert!(OutputName::new("out=1").is_ok());
        assert!(OutputName::new("a=b.c").is_ok());
    }

    #[test]
    fn nar_hash_rejects_url_safe_base64() {
        // Standard SRI/base64 uses '+'/'/'. URL-safe '-'/'_' must be rejected.
        // 42 standard symbols + one URL-safe symbol + '=' padding == 44 payload
        // bytes (otherwise well-formed), rejected solely on the alphabet.
        let dash = format!("sha256-{}-=", "A".repeat(42));
        let under = format!("sha256-{}_=", "A".repeat(42));
        assert_eq!(dash.len(), "sha256-".len() + 44);
        assert_eq!(under.len(), "sha256-".len() + 44);
        assert!(
            NarHash::new(&dash).is_err(),
            "'-' (URL-safe) must be rejected"
        );
        assert!(
            NarHash::new(&under).is_err(),
            "'_' (URL-safe) must be rejected"
        );
    }
}
