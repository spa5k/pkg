// Spike S4 (PR-6 / DR-004) — FLAKE-REF slice: build the EXACT pinned, pure-flake
// installables every measurement runs, from a validated [`Manifest`].
//
// This module produces ONLY pure flake installable strings of the form
//
//     github:NixOS/nixpkgs/<rev>?narHash=<percent-encoded-SRI>#<attr-path>
//
// It NEVER emits generic user shell text: no `nix …` command, no `--impure`,
// no `NIX_PATH`, no mutable channel reference, no `nix-build`/`--build`, and no
// `--substituter`. The `narHash` query value is percent-encoded so that the
// `+`, `/`, `=` and `%` bytes that legitimately occur in an SRI string — plus
// every other non-unreserved byte — cannot be misread as query structure. The
// system triple embedded in an installable is checked against the manifest's
// allow-list via [`CheckedSystem`], so an unknown system can NEVER reach an
// installable string.
//
// The manifest passed to these builders MUST already be validated (e.g. obtained
// from [`crate::manifest::benchmark_manifest`]); this module does not re-run
// validation.

use crate::manifest::Manifest;

/// Percent-encode `value` for use as a flake-URL query-parameter value.
///
/// Only RFC 3986 *unreserved* bytes (`A-Za-z0-9-._~`) pass through untouched.
/// Every other byte — including `+`, `/`, `=`, `%`, spaces, and all non-ASCII —
/// is encoded as `%XX` with uppercase hex. This is the strictest safe encoding
/// for a query value and guarantees that the `/`, `=` and (hypothetical) `+`
/// bytes present in an SRI hash cannot be parsed as query delimiters.
#[must_use]
pub fn percent_encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    percent_encode_query_into(value, &mut out);
    out
}

fn percent_encode_query_into(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
}

/// Build the exact pinned flake reference:
/// `github:<owner>/<repo>/<rev>?narHash=<percent-encoded-SRI>`.
///
/// `owner`, `repo`, `rev` and the SRI `narHash` all come from
/// `manifest.nixpkgs`, which MUST already be validated. The `narHash` value is
/// encoded with [`percent_encode_query`].
#[must_use]
pub fn flake_ref(manifest: &Manifest) -> String {
    let mut out = String::with_capacity(96);
    flake_ref_into(manifest, &mut out);
    out
}

fn flake_ref_into(manifest: &Manifest, out: &mut String) {
    out.push_str("github:");
    out.push_str(&manifest.nixpkgs.owner);
    out.push('/');
    out.push_str(&manifest.nixpkgs.repo);
    out.push('/');
    out.push_str(&manifest.nixpkgs.rev);
    out.push_str("?narHash=");
    percent_encode_query_into(&manifest.nixpkgs.nar_hash, out);
}

/// A system triple verified against a [`Manifest`]'s allow-list.
///
/// The only constructor is [`check_system`], which borrows the matched string
/// directly from the manifest's `systems` vector. Because the installable
/// builders below take a [`CheckedSystem`], an unknown or typo'd system triple
/// can never be interpolated into a flake URL — the check is enforced by the
/// type system, not by a runtime assertion at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedSystem<'m> {
    system: &'m str,
}

impl<'m> CheckedSystem<'m> {
    /// The canonical Nix `system` string, borrowed from the validating manifest.
    #[must_use]
    pub fn as_str(self) -> &'m str {
        self.system
    }
}

/// Error returned by [`check_system`] when a system triple is absent from the
/// manifest's allow-list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemError {
    /// The input contained no characters.
    Empty,
    /// The input is not one of the manifest's supported systems.
    Unknown {
        /// The rejected input.
        input: String,
        /// The manifest's allow-list, surfaced for an actionable error message.
        allowed: Vec<String>,
    },
}

impl std::fmt::Display for SystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemError::Empty => f.write_str("empty system triple"),
            SystemError::Unknown { input, allowed } => write!(
                f,
                "unknown system triple {input:?}; expected one of {}",
                allowed.join(", ")
            ),
        }
    }
}

impl std::error::Error for SystemError {}

/// Verify that `system` is one of `manifest.systems`, borrowing the matched
/// canonical string back as a [`CheckedSystem`].
pub fn check_system<'m>(
    manifest: &'m Manifest,
    system: &str,
) -> Result<CheckedSystem<'m>, SystemError> {
    if system.is_empty() {
        return Err(SystemError::Empty);
    }
    for candidate in &manifest.systems {
        if candidate == system {
            return Ok(CheckedSystem {
                system: candidate.as_str(),
            });
        }
    }
    Err(SystemError::Unknown {
        input: system.to_owned(),
        allowed: manifest.systems.clone(),
    })
}

/// Build the exact pure single-attribute installable for the measured
/// attribute's derivation path:
///
/// ```text
/// github:<owner>/<repo>/<rev>?narHash=<encoded>#legacyPackages.<system>.<attr>.drvPath
/// ```
///
/// `<system>` is the checked system triple; `<attr>` is the manifest's single
/// measured attribute (`manifest.attr`, e.g. `ripgrep`). The result is a pure
/// flake installable ONLY — never a shell command, and never carrying any
/// impure / build / channel / `NIX_PATH` / substituter flag.
#[must_use]
pub fn single_attr_installable(manifest: &Manifest, system: &CheckedSystem<'_>) -> String {
    let mut out = String::with_capacity(160);
    flake_ref_into(manifest, &mut out);
    out.push_str("#legacyPackages.");
    out.push_str(system.as_str());
    out.push('.');
    out.push_str(&manifest.attr);
    out.push_str(".drvPath");
    out
}

/// Build the exact pure index installable:
///
/// ```text
/// github:<owner>/<repo>/<rev>?narHash=<encoded>#legacyPackages.<system>
/// ```
///
/// This is the `legacyPackages.<system>` attribute set passed as the single
/// argument to the index-meta projection (`nix/index-meta.nix`). As with
/// [`single_attr_installable`], the result is a pure installable string only.
#[must_use]
pub fn index_installable(manifest: &Manifest, system: &CheckedSystem<'_>) -> String {
    let mut out = String::with_capacity(128);
    flake_ref_into(manifest, &mut out);
    out.push_str("#legacyPackages.");
    out.push_str(system.as_str());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::benchmark_manifest;

    // Pinned exact outputs derived from `benchmark.json`. If the manifest's
    // owner/repo/rev/narHash ever changes, these literals break loudly — that is
    // the intent: they pin the harness's installable strings to the evidence.
    const EXPECTED_FLAKE_REF: &str = concat!(
        "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446",
        "?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D",
    );
    const EXPECTED_SINGLE_ATTR_X86: &str = concat!(
        "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446",
        "?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D",
        "#legacyPackages.x86_64-linux.ripgrep.drvPath",
    );
    const EXPECTED_INDEX_AARCH64_DARWIN: &str = concat!(
        "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446",
        "?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D",
        "#legacyPackages.aarch64-darwin",
    );

    #[test]
    fn percent_encode_known_vector() {
        // `+`, `/`, `=`, `%` are all encoded; alnum and `-` are preserved.
        assert_eq!(
            percent_encode_query("sha256-AB/CD+EF=GH%IJ"),
            "sha256-AB%2FCD%2BEF%3DGH%25IJ",
        );
    }

    #[test]
    fn percent_encode_preserves_unreserved_and_encodes_rest() {
        // RFC 3986 unreserved set: A-Za-z0-9 - . _ ~
        assert_eq!(percent_encode_query("AZaz09-._~"), "AZaz09-._~");
        // A space is non-unreserved.
        assert_eq!(percent_encode_query("a b"), "a%20b");
        // A non-ASCII byte (U+00E9 in UTF-8 = 0xC3 0xA9) is fully encoded.
        assert_eq!(percent_encode_query("é"), "%C3%A9");
    }

    #[test]
    fn flake_ref_matches_pinned_literal() {
        let manifest = benchmark_manifest();
        let got = flake_ref(manifest);
        assert_eq!(got, EXPECTED_FLAKE_REF);
        assert!(got.contains("?narHash=sha256-"));
        // The raw `/` and `=` from the SRI must NOT survive into the ref.
        assert!(!got.ends_with('='));
        assert!(!got.contains("/3Xuw="));
    }

    #[test]
    fn single_attr_installable_matches_pinned_literal() {
        let manifest = benchmark_manifest();
        let system = check_system(manifest, "x86_64-linux").unwrap();
        assert_eq!(
            single_attr_installable(manifest, &system),
            EXPECTED_SINGLE_ATTR_X86
        );
        assert!(single_attr_installable(manifest, &system).ends_with(".drvPath"));
    }

    #[test]
    fn index_installable_matches_pinned_literal() {
        let manifest = benchmark_manifest();
        let system = check_system(manifest, "aarch64-darwin").unwrap();
        assert_eq!(
            index_installable(manifest, &system),
            EXPECTED_INDEX_AARCH64_DARWIN
        );
    }

    #[test]
    fn check_system_accepts_every_manifest_system() {
        let manifest = benchmark_manifest();
        for system in &manifest.systems {
            let checked = check_system(manifest, system.as_str());
            assert!(checked.is_ok(), "manifest system {system:?} was rejected");
            assert_eq!(checked.unwrap().as_str(), system.as_str());
        }
    }

    #[test]
    fn check_system_rejects_unknown_and_empty() {
        let manifest = benchmark_manifest();
        let err = check_system(manifest, "i686-linux").unwrap_err();
        assert_eq!(
            err,
            SystemError::Unknown {
                input: "i686-linux".to_owned(),
                allowed: manifest.systems.clone(),
            },
        );
        assert_eq!(check_system(manifest, "").unwrap_err(), SystemError::Empty);
        // Rust *target* triples (vs Nix `system` triples) must be rejected.
        assert!(check_system(manifest, "x86_64-unknown-linux-gnu").is_err());
    }

    /// No produced string may carry any impure / build / channel / `NIX_PATH` /
    /// substituter token, nor any `http` URL (the raw-archive evidence URL is
    /// never part of a pure installable), nor any shell construct.
    #[test]
    fn produced_strings_forbid_impurity_tokens() {
        let manifest = benchmark_manifest();
        let mut produced: Vec<String> = vec![flake_ref(manifest)];
        for system in &manifest.systems {
            let checked = check_system(manifest, system.as_str()).unwrap();
            produced.push(single_attr_installable(manifest, &checked));
            produced.push(index_installable(manifest, &checked));
        }
        const FORBIDDEN: &[&str] = &[
            "impure",
            "build",
            "channel",
            "NIX_PATH",
            "substituter",
            "http",
            "--",
            "$(",
            "`",
        ];
        for string in &produced {
            for &token in FORBIDDEN {
                assert!(
                    !string.contains(token),
                    "produced installable {string:?} contains forbidden token {token:?}",
                );
            }
        }
    }
}
