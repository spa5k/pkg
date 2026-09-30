//! Package reference classification rules (design D4).
//!
//! Pure string rules over Nix flake references: which references are
//! provably fixed, which locked revision a locked URL names, and how one
//! filesystem path becomes the text of a `path:` flake reference. No
//! process runs here and no state is read.

use std::path::{Path, PathBuf};

/// A full commit id length in hex characters.
const FULL_COMMIT_LEN: usize = 40;

/// Flake reference schemes that pin a commit as the final path segment
/// (`owner/repo/<commit>`): the forge reference syntax shared by GitHub,
/// GitLab, and SourceHut.
const FINAL_SEGMENT_COMMIT_SCHEMES: [&str; 3] = ["github", "gitlab", "sourcehut"];

/// Whether `text` is exactly one full commit id.
fn is_full_commit(text: &str) -> bool {
    text.len() == FULL_COMMIT_LEN && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// The reference scheme prefix of `text`, when it starts with one.
///
/// A scheme is the run of ASCII letters, digits, `+`, `-`, and `.` before
/// the first `:` — `github`, `gitlab`, `git+file`, `path`, `https`. A plain
/// filesystem path (`/tmp/...`, `./...`) has no scheme.
fn scheme(text: &str) -> Option<&str> {
    let (prefix, _) = text.split_once(':')?;
    (!prefix.is_empty()
        && prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(prefix)
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
/// Classification is scheme-aware, because a commit pin means what the
/// transport makes it mean:
///
/// * Forge references (`github:`, `gitlab:`, `sourcehut:`) pin a commit
///   either as the final `owner/repo/<commit>` segment or through an exact
///   `rev=<commit>` parameter.
/// * `git:` and `git+` schemes pin a commit only through an exact
///   `rev=<commit>` parameter; Nix reads a git URL's path as a repository
///   location, never as a revision. `git:` is the bare ssh form of the
///   same family the Nix manual documents as
///   `git(+http|+https|+ssh|+git|+file):`.
/// * Everything else names a location, not a commit: a `path:` reference,
///   a `file://` or tarball URL, and a plain filesystem path stay movable
///   even when the final segment happens to be 40 hex characters, because
///   a directory or file basename does not make content immutable.
fn full_commit_named(text: &str) -> Option<&str> {
    let without_fragment = text.split('#').next().unwrap_or(text);
    let (base, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    let forge_reference = scheme(without_fragment)
        .is_some_and(|prefix| FINAL_SEGMENT_COMMIT_SCHEMES.contains(&prefix));
    let names_revision = forge_reference
        || scheme(without_fragment)
            .is_some_and(|prefix| prefix == "git" || prefix.starts_with("git+"));
    query_commit(query)
        .filter(|_| names_revision)
        .or_else(|| final_segment_commit(base).filter(|_| forge_reference))
}

/// The final path segment, when it is exactly one full commit id.
fn final_segment_commit(base: &str) -> Option<&str> {
    let last = base.rsplit('/').next()?;
    is_full_commit(last).then_some(last)
}

/// Detect explicit full-commit semantics in a reference.
///
/// Only a commit pin the reference scheme actually has — a forge
/// `owner/repo/<commit>` segment, or an exact `rev=<commit>` parameter on
/// a forge or `git+` reference — proves a fixed reference. Tags, branch
/// names, short hex-looking branch names, and 40-hex directory or file
/// basenames on `path:`, tarball, `file://`, or plain filesystem
/// references are moving locations and never get fixed status here; the
/// locked revision comes from actual flake metadata instead (design D4).
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
        assert!(is_fixed_reference(&format!("gitlab:owner/repo/{COMMIT}")));
        assert!(is_fixed_reference(&format!(
            "sourcehut:~owner/repo/{COMMIT}"
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

    /// The audited misclassification: a mutable source whose final path
    /// segment is 40 hex characters is a location, not a commit pin. Only
    /// the forge schemes read that segment as a revision; `path:` and
    /// tarball/`file://`/plain references stay movable, and `git+` URLs pin
    /// only through `rev=`.
    #[test]
    fn hex_basenames_pin_only_where_the_scheme_names_a_commit() {
        // Forge references keep final-segment commit semantics.
        assert!(is_fixed_reference(&format!("github:owner/repo/{COMMIT}")));
        // A local path flake, plain path, and file URL never become fixed
        // from a 40-hex basename, with or without a `rev=` parameter a
        // path cannot carry.
        assert!(!is_fixed_reference(&format!("path:/state/src/{COMMIT}")));
        assert!(!is_fixed_reference(&format!("path:{COMMIT}")));
        assert!(!is_fixed_reference(&format!("/tmp/{COMMIT}")));
        assert!(!is_fixed_reference(&format!("./{COMMIT}")));
        assert!(!is_fixed_reference(&format!("file:///tmp/{COMMIT}")));
        assert!(!is_fixed_reference(&format!(
            "path:/state/src/repo?rev={COMMIT}"
        )));
        // Tarball URLs name downloadable content, not revisions.
        assert!(!is_fixed_reference(&format!(
            "https://example.com/archive/{COMMIT}.tar.gz"
        )));
        assert!(!is_fixed_reference(&format!(
            "https://example.com/src/{COMMIT}"
        )));
        assert!(!is_fixed_reference(&format!(
            "tarball+https://example.com/{COMMIT}"
        )));
        // Git URLs read their path as a repository location; only an exact
        // `rev=` pins.
        assert!(!is_fixed_reference(&format!("git+file:///tmp/{COMMIT}")));
        assert!(!is_fixed_reference(&format!(
            "git+https://example.com/repo/{COMMIT}"
        )));
        assert!(is_fixed_reference(&format!(
            "git+https://example.com/repo?rev={COMMIT}"
        )));
        // The bare ssh `git:` scheme is the same family per the Nix
        // manual (`git(+http|+https|+ssh|+git|+file):`) and pins only
        // through `rev=`.
        assert!(is_fixed_reference(&format!(
            "git:example.com/repo?rev={COMMIT}"
        )));
        assert!(!is_fixed_reference(&format!(
            "git:example.com/repo/{COMMIT}"
        )));
        // Locked-revision identity follows the same scheme-aware rule.
        assert_eq!(
            locked_revision(&format!("github:owner/repo/{COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision(&format!("git+file:///tmp/repo?rev={COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(locked_revision(&format!("path:/state/src/{COMMIT}")), None);
        assert_eq!(locked_revision(&format!("/tmp/{COMMIT}")), None);
        assert_eq!(
            locked_revision(&format!("https://example.com/src/{COMMIT}")),
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
