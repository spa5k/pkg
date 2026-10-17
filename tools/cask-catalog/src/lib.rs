//! Catalog generation core: token validation, effective-record merge,
//! per-target eligibility, typed artifact plans, and deterministic output.
//!
//! `generate` is offline and deterministic over the pinned input; the
//! `fetch` and `import-tap` commands do bounded network access (see
//! main.rs and `tap`).

#![cfg(unix)]
// Tests may abort on broken fixtures or failed setup; production code
// denies explicit unwrap/expect/panic (these Clippy lints are denied by
// the workspace for every non-test target).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
    )
)]

pub mod classify;
pub mod effective;
pub mod emit;
mod github;
mod net;
pub mod plan;
pub mod tap;
pub mod token;
pub mod version;

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The catalog schema identifier this generator emits.
pub const CATALOG_SCHEMA: &str = "pkg-cask-catalog/4";
/// Generator identity recorded in every output.
pub const GENERATOR_NAME: &str = "cask-catalog";
/// Generator version recorded in every output (contract value).
pub const GENERATOR_VERSION: &str = "0.1.0";
/// The declared macOS baseline (contract value).
pub const MACOS_BASELINE: &str = "15.7.7";
/// The only target systems the catalog publishes.
pub const TARGETS: [&str; 2] = ["aarch64-darwin", "x86_64-linux"];
/// Default cache directory for pinned inputs (never inside the repo).
pub const CACHE_DIR: &str = "/tmp/cask-catalog";
/// Source identity of the official Homebrew Cask JSON snapshot.
pub const OFFICIAL_SOURCE: &str = "homebrew/cask";
/// The Homebrew `brew` revision whose loader the raw exporter pins.
pub const HOMEBREW_PIN: &str = "cc9ff034b1d98cdd4061f68cf715bb967c483a8f";
/// nixpkgs revision pinned for the raw Ruby export runtime and flake
/// outputs (same pin as `nix/casks/flake.lock`).
pub const NIXPKGS_PIN: &str = "8edc0c72e3a38faf5434e40d1f19431125fe4b30";

/// The committed input pin: exact snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// Raw snapshot URL at the exact revision.
    pub url: String,
    /// Exact revision the URL names.
    pub revision: String,
    /// SHA-256 of the snapshot bytes.
    pub sha256: String,
    /// Upstream data license attribution.
    pub license: String,
}

/// Whether every character is ASCII hexadecimal.
#[must_use = "the result states whether the text is hexadecimal"]
pub fn is_hex(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether a revision is an exact 40-hex commit id.
#[must_use = "the result states whether the revision is an exact commit id"]
pub fn valid_revision(revision: &str) -> bool {
    revision.len() == 40 && is_hex(revision)
}

pub use token::is_valid_source as valid_repo;

/// Encode a full catalog entry id (`owner/tap/token`) into the safe
/// Nix `packages` attribute name used by `nix profile` operations.
///
/// One pass, `_` becomes `_u_` and `.` becomes `_d_`; every other
/// character passes through unchanged. This mirrors
/// `builtins.replaceStrings ["_" "."] ["_u_" "_d_"]` in the
/// generated flakes exactly.
#[must_use = "the result is the Nix packages attribute for this id"]
pub fn package_attribute(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        match c {
            '_' => out.push_str("_u_"),
            '.' => out.push_str("_d_"),
            _ => out.push(c),
        }
    }
    out
}

/// Decode a `packages` attribute name back into the catalog entry id.
/// One pass over `_u_`/`_d_` escapes; any other underscore sequence
/// (including a trailing `_` or an unknown escape) is rejected.
///
/// # Errors
///
/// Returns an error when `attr` contains an unknown or truncated escape.
pub fn decode_package_attribute(attr: &str) -> Result<String, String> {
    let mut out = String::with_capacity(attr.len());
    let mut chars = attr.chars();
    while let Some(c) = chars.next() {
        if c != '_' {
            out.push(c);
            continue;
        }
        let kind = chars.next();
        let close = chars.next();
        match (kind, close) {
            (Some('u'), Some('_')) => out.push('_'),
            (Some('d'), Some('_')) => out.push('.'),
            _ => {
                return Err(format!(
                    "attribute {attr:?} contains an unknown or truncated underscore escape"
                ));
            }
        }
    }
    Ok(out)
}

/// Whether a hash is 64 hex characters.
#[must_use = "the result states whether the hash is a SHA-256"]
pub fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64 && is_hex(sha)
}

/// Whether a URL parses as HTTP or HTTPS and has a host.
#[must_use = "the result states whether the URL scheme is fetchable"]
pub fn valid_url(url: &str) -> bool {
    url::Url::parse(url)
        .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some())
}

/// Load and strictly decode the committed pin file.
///
/// A malformed pin is a hard error; there is no fallback pin. The pin
/// must name an exact 40-hex revision, an http/https URL, and a 64-hex
/// content hash.
///
/// # Errors
///
/// Returns an error if the file cannot be read or decoded, or if any pin
/// field fails validation.
pub fn load_pin(path: &Path) -> Result<Pin, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read pin {}: {e}", path.display()))?;
    let pin: Pin = serde_json::from_str(&text)
        .map_err(|e| format!("pin {} is malformed: {e}", path.display()))?;
    if !valid_revision(&pin.revision) {
        return Err(format!(
            "pin {} revision {:?} is not an exact 40-hex commit id",
            path.display(),
            pin.revision
        ));
    }
    if !valid_url(&pin.url) {
        return Err(format!(
            "pin {} url {:?} is not an http/https URL",
            path.display(),
            pin.url
        ));
    }
    let sha = pin.sha256.trim().to_ascii_lowercase();
    if !valid_sha256(&sha) {
        return Err(format!(
            "pin {} sha256 is not 64 hex characters",
            path.display()
        ));
    }
    Ok(Pin { sha256: sha, ..pin })
}

/// Verify raw bytes against an expected SHA-256, case-insensitively.
///
/// The error carries the actual hash, so each caller reports its own
/// context around the mismatch.
///
/// # Errors
///
/// Returns the actual digest when it does not match `expected`.
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(actual)
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Sha256::digest(bytes)
        .into_iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

/// Read the raw snapshot bytes and verify them against the pin.
///
/// # Errors
///
/// Returns an error if the file cannot be read or its digest does not match
/// the pin.
pub fn read_verified_input(path: &Path, pin: &Pin) -> Result<Vec<u8>, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read input {}: {e}", path.display()))?;
    verify_sha256(&bytes, &pin.sha256).map_err(|actual| {
        format!(
            "input {} hashes to {actual} but the pin {} expects {}; refusing to generate",
            path.display(),
            pin.url,
            pin.sha256
        )
    })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_verification_preserves_canonical_hex_and_case_insensitive_comparison() {
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(verify_sha256(b"abc", expected), Ok(()));
        assert_eq!(
            verify_sha256(b"abc", &expected.to_ascii_uppercase()),
            Ok(())
        );
        assert_eq!(verify_sha256(b"abc", &"0".repeat(64)), Err(expected.into()));
    }

    #[test]
    fn package_attribute_encoding_is_single_pass() {
        assert_eq!(
            package_attribute("homebrew/cask/visual-studio-code"),
            "homebrew/cask/visual-studio-code"
        );
        assert_eq!(package_attribute("a.b_c"), "a_d_b_u_c");
    }

    #[test]
    fn literal_d_underscore_do_not_collide_with_dot() {
        // The literal id "_d_" encodes to "_u_d_u_"; the id "." encodes
        // to "_d_". Decoding each returns the original; neither string
        // decodes to the other's original.
        assert_eq!(package_attribute("_d_"), "_u_d_u_");
        assert_eq!(package_attribute("."), "_d_");
        assert_eq!(decode_package_attribute("_u_d_u_").unwrap(), "_d_");
        assert_eq!(decode_package_attribute("_d_").unwrap(), ".");
        assert_ne!(decode_package_attribute("_u_d_u_").unwrap(), ".");
    }

    #[test]
    fn decode_rejects_unknown_underscore_escapes() {
        for bad in ["_u", "_", "a__u_", "trailing_", "_U_"] {
            assert!(
                decode_package_attribute(bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn package_attribute_roundtrips() {
        for id in [
            "homebrew/cask/foo",
            "example/tap/goreleaser-pro@2",
            "a_b.c/d_e.f/g_h.i",
            "_u_d_",
            "_x",
            "plain",
        ] {
            let attr = package_attribute(id);
            assert_eq!(
                decode_package_attribute(&attr).unwrap(),
                id,
                "{id} roundtrip"
            );
        }
    }

    #[test]
    fn valid_url_requires_http_scheme_and_host() {
        assert!(valid_url("https://example.com/cask.json"));
        assert!(valid_url("http://127.0.0.1/cask.json"));
        assert!(!valid_url("https://"));
        assert!(!valid_url("http://?query"));
        assert!(!valid_url("file:///tmp/cask.json"));
    }

    #[test]
    fn load_pin_rejects_http_url_without_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pin.json");
        std::fs::write(
            &path,
            r#"{"url":"https://","revision":"0123456789abcdef0123456789abcdef01234567","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","license":"MIT"}"#,
        )
        .unwrap();

        let error = load_pin(&path).unwrap_err();
        assert!(error.contains("not an http/https URL"));
    }
}
