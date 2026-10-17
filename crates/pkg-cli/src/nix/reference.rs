//! Classify package references and encode filesystem paths.
//!
//! Parse commit pins and `path:` references without running processes or reading state.

use std::path::{Path, PathBuf};

/// Flake reference schemes that pin a commit as the final path segment
/// (`owner/repo/<commit>`): the forge reference syntax shared by GitHub,
/// GitLab, and `SourceHut`.
const FINAL_SEGMENT_COMMIT_SCHEMES: [&str; 3] = ["github", "gitlab", "sourcehut"];

/// The reference scheme prefix of `text`, when it starts with one.
///
/// A scheme contains ASCII letters, digits, `+`, `-`, or `.` before the first `:`.
/// Examples include `github`, `git+file`, and `path`. Filesystem paths have no scheme.
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

/// Read the full commit from an exact `rev=` query parameter, if present.
///
/// Split parameters on `&`. Reject names such as `prev` and values with
/// extra characters after the commit.
fn query_commit(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|param| param.strip_prefix("rev="))
        .filter(|value| cask_catalog::valid_revision(value))
}

/// The exact full commit a reference or locked URL names, if any.
///
/// The scheme determines which commit pins are valid:
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
    let prefix = scheme(without_fragment)?;
    let forge_reference = FINAL_SEGMENT_COMMIT_SCHEMES.contains(&prefix);
    let names_revision = forge_reference || prefix == "git" || prefix.starts_with("git+");
    query_commit(query)
        .filter(|_| names_revision)
        .or_else(|| final_segment_commit(base).filter(|_| forge_reference))
}

/// The final path segment, when it is exactly one full commit id.
fn final_segment_commit(base: &str) -> Option<&str> {
    let last = base.rsplit('/').next()?;
    cask_catalog::valid_revision(last).then_some(last)
}

/// Detect explicit full-commit semantics in a reference.
///
/// Forge references can pin a full commit in the final path segment or a
/// `rev=` parameter. Git references require `rev=`. Tags, branches, and
/// commit-shaped filenames are moving references. Their locked revision
/// must come from flake metadata.
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
pub use cask_catalog::valid_revision as is_full_commit_id;

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
const PATH_ENCODING: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

/// Percent-encode one filesystem path for a `path:` flake reference.
///
/// The encoded text (without the `path:` scheme) is what Nix records as the
/// profile entry's original URL, so the same encoding must round-trip
/// through [`decode_path_reference`]. Spaces, `#`, `?`, `%`, quotes, and
/// non-ASCII bytes are `%XX`-encoded; `/` and unreserved ASCII stay literal.
#[must_use]
pub fn encode_path_reference(path: &Path) -> String {
    percent_encoding::percent_encode(path.as_os_str().as_encoded_bytes(), PATH_ENCODING).to_string()
}

/// Decode one percent-encoded `path:` reference body back to a path.
///
/// Return `None` if `%` is not followed by two hex digits. Preserve decoded
/// bytes, including non-UTF-8 Unix paths from [`encode_path_reference`].
#[must_use]
pub fn decode_path_reference(encoded: &str) -> Option<PathBuf> {
    if !encoded.split('%').skip(1).all(|suffix| {
        suffix
            .as_bytes()
            .get(..2)
            .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
    }) {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(encoded).collect::<Vec<_>>();
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(decoded))
    };
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
