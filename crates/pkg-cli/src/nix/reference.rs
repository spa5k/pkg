//! Package reference classification rules (design D4).
//!
//! Pure string rules over Nix flake references: which references are
//! provably fixed, which locked revision a locked URL names, and how one
//! filesystem path becomes the text of a `path:` flake reference. No
//! process runs here and no state is read.

use std::path::{Path, PathBuf};

/// A full commit id length in hex characters.
const FULL_COMMIT_LEN: usize = 40;

/// Whether `text` is exactly one full commit id.
fn is_full_commit(text: &str) -> bool {
    text.len() == FULL_COMMIT_LEN && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// The exact full commit named by one `rev=` query parameter, if one is.
///
/// Parameters are split on `&`, so only a parameter *named* `rev` counts —
/// `prev=` is not `rev=` — and only an exact full-commit value counts, so
/// `rev=<commit><suffix>` is not a fixed reference.
fn query_commit(query: &str) -> Option<&str> {
    query
        .split('&')
        .filter(|param| !param.is_empty())
        .find_map(|param| param.strip_prefix("rev="))
        .filter(|value| is_full_commit(value))
}

/// The exact full commit a reference or locked URL names, if any.
///
/// Exactly one rule serves both fixed classification and locked-revision
/// extraction: an exact `rev=` parameter wins, else the final path segment
/// must itself be a full commit. Tags, branch names, short hex-looking
/// branch names, and near-miss values never match.
fn full_commit_named(text: &str) -> Option<&str> {
    let without_fragment = text.split('#').next().unwrap_or(text);
    let (base, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    query_commit(query).or_else(|| final_segment_commit(base))
}

/// The final path segment, when it is exactly one full commit id.
fn final_segment_commit(base: &str) -> Option<&str> {
    let last = base.rsplit('/').next()?;
    is_full_commit(last).then_some(last)
}

/// Detect explicit full-commit semantics in a reference.
///
/// Only a full-length (40 hex character) commit id proves a fixed reference:
/// either an exact `rev=<commit>` parameter or a full commit as the final
/// path segment. Tags, branch names, and short hex-looking branch names are
/// moving references and never get fixed status here; the locked revision
/// comes from actual flake metadata instead (design D4).
#[must_use]
pub fn is_fixed_reference(reference: &str) -> bool {
    full_commit_named(reference).is_some()
}

/// Whether `text` is exactly one bare full commit id.
///
/// A revision pin a user supplies must be exactly this: 40 hexadecimal
/// characters and nothing else. A reference that merely contains a full
/// commit (for example `github:owner/repo?rev=<commit>`) does not count,
/// so a qualified URL passed as a revision pin is refused.
#[must_use]
pub fn is_full_commit_id(text: &str) -> bool {
    is_full_commit(text)
}

/// Extract a locked full commit from a locked URL, when present.
#[must_use]
pub fn locked_revision(url: &str) -> Option<&str> {
    full_commit_named(url)
}

/// The bytes kept literally in a `path:` flake reference.
///
/// Slash is the path structure and stays literal; every other byte that is
/// not unreserved ASCII is percent-encoded so a path with spaces, `#`, `?`,
/// `%`, or non-ASCII bytes stays exactly one reference text both ways.
fn kept_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// Percent-encode one filesystem path for a `path:` flake reference.
///
/// The encoded text (without the `path:` scheme) is what Nix records as the
/// profile entry's original URL, so the same encoding must round-trip
/// through [`decode_path_reference`]. Spaces, `#`, `?`, `%`, quotes, and
/// non-ASCII bytes are `%XX`-encoded; `/` and unreserved ASCII stay literal.
#[must_use]
pub fn encode_path_reference(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if kept_path_byte(byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Decode one percent-encoded `path:` reference body back to a path.
///
/// Returns `None` for a malformed escape (`%` not followed by two hex
/// digits). Decoded bytes are preserved as-is: on Unix an arbitrary
/// non-UTF-8 path that [`encode_path_reference`] encoded — a state home
/// the user named with raw OS path bytes — decodes back to the same path
/// instead of being refused. A reference pkg did not encode itself is
/// still never guessed at.
#[must_use]
pub fn decode_path_reference(encoded: &str) -> Option<PathBuf> {
    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(decoded))
    };
    #[cfg(not(unix))]
    let path = String::from_utf8(decoded).ok().map(PathBuf::from)?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";

    #[test]
    fn fixed_reference_detection_requires_explicit_full_commits() {
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs/{COMMIT}"
        )));
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs?rev={COMMIT}"
        )));
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs?rev={COMMIT}&dir=."
        )));
        assert!(is_fixed_reference(&format!(
            "git+file:///tmp/repo?ref=main&rev={COMMIT}"
        )));
        // Tags, branches, and hex-looking branch names are not provably fixed.
        assert!(!is_fixed_reference("github:NixOS/nixpkgs/refs/tags/25.05"));
        assert!(!is_fixed_reference("github:NixOS/nixpkgs/nixpkgs-unstable"));
        assert!(!is_fixed_reference("github:spa5k/pkg/main?dir=nix/casks"));
        assert!(!is_fixed_reference("github:owner/repo/0a56ceb"));
        let truncated = &COMMIT[..39];
        assert!(!is_fixed_reference(&format!(
            "github:owner/repo/{truncated}"
        )));
        // A hex prefix of full length plus a suffix is not a commit, and a
        // parameter merely ending in `rev=` is not `rev=` (one shared exact
        // extractor for both fixed classification and locked revision).
        assert!(!is_fixed_reference(&format!(
            "github:owner/repo?rev={COMMIT}not-a-commit"
        )));
        assert!(!is_fixed_reference(&format!(
            "github:owner/repo?prev={COMMIT}"
        )));
        assert_eq!(
            locked_revision(&format!("github:owner/repo?rev={COMMIT}not-a-commit")),
            None
        );
        assert_eq!(
            locked_revision(&format!("github:owner/repo?prev={COMMIT}")),
            None
        );
    }

    #[test]
    fn locked_revision_reads_full_commits_only() {
        assert_eq!(
            locked_revision(&format!("github:NixOS/nixpkgs/{COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision(&format!("github:NixOS/nixpkgs?rev={COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision(&format!("git+file:///tmp/repo?ref=main&rev={COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision("github:NixOS/nixpkgs/nixpkgs-unstable"),
            None
        );
        assert_eq!(locked_revision("path:/tmp/pkg-native-proof.UUZyyl"), None);
    }

    #[test]
    fn path_references_encode_every_special_byte_and_round_trip() {
        // Spaces, `#`, `?`, `%`, and non-ASCII bytes are encoded; slash and
        // unreserved ASCII stay literal.
        let path = Path::new("/tmp/pkg tap/src/owner#1/app?x/%directory/ünïcode");
        let encoded = encode_path_reference(path);
        assert!(
            encoded.starts_with("/tmp/pkg%20tap/src/owner%231/app%3Fx/%25directory/"),
            "{encoded}"
        );
        assert!(encoded.contains("%C3%BCn%C3%AFcode"), "{encoded}");
        assert!(!encoded.contains(' '));
        assert!(!encoded.contains('#'));
        assert!(!encoded.contains('?'));
        assert_eq!(decode_path_reference(&encoded).as_deref(), Some(path));
        // A plain reference with nothing to encode is unchanged.
        let plain = Path::new("/state/pkg/taps/src/somebody/apps/current");
        assert_eq!(
            encode_path_reference(plain),
            plain.as_os_str().to_string_lossy()
        );
        assert_eq!(
            decode_path_reference(plain.to_str().unwrap()).as_deref(),
            Some(plain)
        );
    }

    #[test]
    fn malformed_escapes_never_decode() {
        assert_eq!(decode_path_reference("/tmp/a%2"), None);
        assert_eq!(decode_path_reference("/tmp/a%zz"), None);
        assert_eq!(decode_path_reference("/tmp/a%"), None);
        // Malformed escapes are refused; malformed UTF-8 is not: on Unix
        // arbitrary OS path bytes round-trip and are preserved as-is.
        assert_eq!(
            decode_path_reference("/tmp/%20").as_deref(),
            Some(Path::new("/tmp/ "))
        );
    }

    #[test]
    #[cfg(unix)]
    fn arbitrary_non_utf8_path_bytes_round_trip() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        // A path with a byte sequence that is not valid UTF-8 must still
        // encode to exactly one reference text and decode back intact.
        let path = PathBuf::from(OsString::from_vec(
            b"/state/pkg\xff\xfe/lag-\x80tap".to_vec(),
        ));
        let encoded = encode_path_reference(&path);
        assert_eq!(encoded, "/state/pkg%FF%FE/lag-%80tap");
        assert_eq!(
            decode_path_reference(&encoded).as_deref(),
            Some(path.as_path())
        );
    }
}
