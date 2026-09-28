//! Catalog generation core: token validation, effective-record merge,
//! per-target eligibility, typed artifact plans, and deterministic output.
//!
//! Everything here is offline and deterministic over the pinned input.
//! The only network step lives in the `fetch` command (main.rs).

pub mod classify;
pub mod effective;
pub mod emit;
pub mod plan;
pub mod token;

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The catalog schema identifier this generator emits.
pub const CATALOG_SCHEMA: &str = "pkg-cask-catalog/2";
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

/// Whether a repository is one `owner/name` pair of safe characters.
#[must_use = "the result states whether the repository is an owner/name pair"]
pub fn valid_repo(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// Whether a hash is 64 hex characters.
#[must_use = "the result states whether the hash is a SHA-256"]
pub fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64 && is_hex(sha)
}

/// Whether a URL is an http/https URL.
#[must_use = "the result states whether the URL scheme is fetchable"]
pub fn valid_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Load and strictly decode the committed pin file.
///
/// A malformed pin is a hard error; there is no fallback pin. The pin
/// must name an exact 40-hex revision, an http/https URL, and a 64-hex
/// content hash.
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
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(actual)
    }
}

/// Read the raw snapshot bytes and verify them against the pin.
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
