//! Package reference classification rules (design D4).
//!
//! Pure string rules over Nix flake references: which references are
//! provably fixed, and which locked revision a locked URL names. No process
//! runs here and no state is read.

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

/// Extract a locked full commit from a locked URL, when present.
#[must_use]
pub fn locked_revision(url: &str) -> Option<&str> {
    full_commit_named(url)
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
}
