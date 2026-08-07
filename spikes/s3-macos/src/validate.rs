// Spike S3 (PR-7 / DR-003) — pin-summary semantic validation.
//
// This module is the *semantic* gate for the pin-summary DTO defined in
// `manifest.rs`. `serde` already enforces structural shape (types, required
// keys, `deny_unknown_fields`); this module enforces that the pin is EXACTLY
// the canonical S3 pin:
//
//   * `schemaVersion` must be exactly [`SCHEMA_VERSION`] (1).
//   * `nix.version` must be exactly the pinned release ([`NIX_VERSION`]).
//   * `nixpkgs.owner`/`repo` must be exactly [`NIXPKGS_OWNER`]/[`NIXPKGS_REPO`].
//   * `nixpkgs.rev` must be exactly 40 lowercase-hex characters AND equal the
//     pinned [`NIXPKGS_REV`] (a structural check first yields a distinct
//     `RevMalformed` error for a mis-shaped rev, then an exact-match check
//     yields `RevMismatch` for a well-shaped but wrong rev).
//   * `nixpkgs.narHash` must start with `sha256-` (structural) AND equal the
//     pinned canonical [`NIXPKGS_NAR_HASH`] SRI string. No raw-archive hash
//     evidence is duplicated here (it is irrelevant to S3 — see `manifest.rs`).
//   * `systems` must be exactly the two pinned Darwin systems, in order.
//   * `attrs` must be exactly the three pinned attributes, in order.
//   * `cacheStoreUrl` must be exactly the single pinned v1 cache store URL.
//
// `PinError`'s `Display` is *bounded*: any caller-controlled snippet included in
// an error message is truncated via [`bound_snippet`] so a malicious or
// mis-authored pin cannot bloat logs or messages.

use std::fmt;

use crate::manifest::PinSummary;

/// The one-and-only pin-summary schema revision this slice understands.
pub const SCHEMA_VERSION: u32 = 1;

/// The single pinned Nix release this slice targets (same release S4 measures).
pub const NIX_VERSION: &str = "2.34.8";

/// Pinned Nixpkgs flake source owner.
pub const NIXPKGS_OWNER: &str = "NixOS";
/// Pinned Nixpkgs flake source repo.
pub const NIXPKGS_REPO: &str = "nixpkgs";
/// Pinned 40-char Nixpkgs revision.
pub const NIXPKGS_REV: &str = "a62e6edd6d5e1fa0329b8653c801147986f8d446";
/// Pinned flake NAR hash as a canonical `sha256-…` SRI string.
pub const NIXPKGS_NAR_HASH: &str = "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";

/// The exact, ordered Darwin systems S3 concerns itself with.
pub const DARWIN_SYSTEMS: [&str; 2] = ["x86_64-darwin", "aarch64-darwin"];

/// The exact, ordered fixture attribute set S3 covers.
pub const ATTRS: [&str; 3] = ["hello", "ripgrep", "git"];

/// The single v1 binary cache store URL.
pub const CACHE_STORE_URL: &str = "https://cache.nixos.org/";

/// Maximum characters of any caller-controlled string included in a
/// `PinError`'s `Display` output. Keeps messages bounded regardless of size.
pub const DISPLAY_SNIPPET_MAX: usize = 64;

/// Validate a parsed pin summary against the exact canonical pin above.
pub fn validate_pin(p: &PinSummary) -> Result<(), PinError> {
    if p.schema_version != SCHEMA_VERSION {
        return Err(PinError::SchemaVersion {
            got: p.schema_version,
        });
    }
    if p.nix.version != NIX_VERSION {
        return Err(PinError::NixVersion {
            got: p.nix.version.clone(),
        });
    }
    if p.nixpkgs.owner != NIXPKGS_OWNER {
        return Err(PinError::OwnerMismatch {
            got: p.nixpkgs.owner.clone(),
        });
    }
    if p.nixpkgs.repo != NIXPKGS_REPO {
        return Err(PinError::RepoMismatch {
            got: p.nixpkgs.repo.clone(),
        });
    }
    if !is_40_lowercase_hex(&p.nixpkgs.rev) {
        return Err(PinError::RevMalformed {
            got: p.nixpkgs.rev.clone(),
        });
    }
    if p.nixpkgs.rev != NIXPKGS_REV {
        return Err(PinError::RevMismatch {
            got: p.nixpkgs.rev.clone(),
        });
    }
    if !p.nixpkgs.nar_hash.starts_with("sha256-") {
        return Err(PinError::NarHashMalformed {
            got: p.nixpkgs.nar_hash.clone(),
        });
    }
    if p.nixpkgs.nar_hash != NIXPKGS_NAR_HASH {
        return Err(PinError::NarHashMismatch {
            got: p.nixpkgs.nar_hash.clone(),
        });
    }
    if !exact_seq(&p.systems, &DARWIN_SYSTEMS) {
        return Err(PinError::SystemsNotExact {
            got: p.systems.clone(),
        });
    }
    if !exact_seq(&p.attrs, &ATTRS) {
        return Err(PinError::AttrsNotExact {
            got: p.attrs.clone(),
        });
    }
    if p.cache_store_url != CACHE_STORE_URL {
        return Err(PinError::CacheStoreUrlMismatch {
            got: p.cache_store_url.clone(),
        });
    }
    Ok(())
}

/// A bounded, caller-snippet-safe pin-validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    /// `schemaVersion` did not equal [`SCHEMA_VERSION`].
    SchemaVersion { got: u32 },
    /// `nix.version` did not equal [`NIX_VERSION`].
    NixVersion { got: String },
    /// `nixpkgs.owner` did not equal [`NIXPKGS_OWNER`].
    OwnerMismatch { got: String },
    /// `nixpkgs.repo` did not equal [`NIXPKGS_REPO`].
    RepoMismatch { got: String },
    /// `nixpkgs.rev` was not 40 lowercase-hex characters.
    RevMalformed { got: String },
    /// `nixpkgs.rev` was well-shaped but did not equal [`NIXPKGS_REV`].
    RevMismatch { got: String },
    /// `nixpkgs.narHash` did not start with `sha256-`.
    NarHashMalformed { got: String },
    /// `nixpkgs.narHash` was shaped but did not equal [`NIXPKGS_NAR_HASH`].
    NarHashMismatch { got: String },
    /// `systems` was not exactly the two pinned Darwin systems, in order.
    SystemsNotExact { got: Vec<String> },
    /// `attrs` was not exactly the three pinned attributes, in order.
    AttrsNotExact { got: Vec<String> },
    /// `cacheStoreUrl` did not equal [`CACHE_STORE_URL`].
    CacheStoreUrlMismatch { got: String },
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinError::SchemaVersion { got } => {
                write!(f, "schemaVersion must be {SCHEMA_VERSION}, got {got}")
            }
            PinError::NixVersion { got } => write!(
                f,
                "nix.version must be exactly {NIX_VERSION:?}, got {:?}",
                bound_snippet(got)
            ),
            PinError::OwnerMismatch { got } => write!(
                f,
                "nixpkgs.owner must be {NIXPKGS_OWNER:?}, got {:?}",
                bound_snippet(got)
            ),
            PinError::RepoMismatch { got } => write!(
                f,
                "nixpkgs.repo must be {NIXPKGS_REPO:?}, got {:?}",
                bound_snippet(got)
            ),
            PinError::RevMalformed { got } => write!(
                f,
                "nixpkgs.rev must be 40 lowercase-hex chars, got {:?}",
                bound_snippet(got)
            ),
            PinError::RevMismatch { got } => write!(
                f,
                "nixpkgs.rev must be exactly {NIXPKGS_REV}, got {:?}",
                bound_snippet(got)
            ),
            PinError::NarHashMalformed { got } => write!(
                f,
                "nixpkgs.narHash must be a sha256-… SRI string, got {:?}",
                bound_snippet(got)
            ),
            PinError::NarHashMismatch { got } => write!(
                f,
                "nixpkgs.narHash must be exactly the pinned SRI, got {:?}",
                bound_snippet(got)
            ),
            PinError::SystemsNotExact { got } => write!(
                f,
                "systems must be exactly {:?} in order, got {:?}",
                DARWIN_SYSTEMS,
                bound_snippet(&got.join(","))
            ),
            PinError::AttrsNotExact { got } => write!(
                f,
                "attrs must be exactly {:?} in order, got {:?}",
                ATTRS,
                bound_snippet(&got.join(","))
            ),
            PinError::CacheStoreUrlMismatch { got } => write!(
                f,
                "cacheStoreUrl must be exactly {CACHE_STORE_URL:?}, got {:?}",
                bound_snippet(got)
            ),
        }
    }
}

impl std::error::Error for PinError {}

/// True iff `got` and `want` have equal length and pairwise-equal items.
fn exact_seq(got: &[String], want: &[&str]) -> bool {
    got.len() == want.len() && got.iter().zip(want.iter()).all(|(g, w)| g.as_str() == *w)
}

/// Truncate a caller-controlled string for safe inclusion in a bounded
/// `PinError` `Display` output. Slices on a UTF-8 boundary.
pub(crate) fn bound_snippet(s: &str) -> String {
    if s.len() <= DISPLAY_SNIPPET_MAX {
        return s.to_string();
    }
    let mut end = DISPLAY_SNIPPET_MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&s[..end]);
    out.push_str("...");
    out
}

/// Exactly 40 lowercase-hex characters (`[0-9a-f]`).
fn is_40_lowercase_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;

    fn base() -> PinSummary {
        manifest::pin_summary().clone()
    }

    #[test]
    fn embedded_pin_is_valid() {
        assert!(validate_pin(&base()).is_ok());
    }

    // ---- schema ------------------------------------------------------------
    #[test]
    fn rejects_wrong_schema_version() {
        for v in [0u32, 2, u32::MAX] {
            let mut p = base();
            p.schema_version = v;
            assert_eq!(validate_pin(&p), Err(PinError::SchemaVersion { got: v }));
        }
    }

    // ---- nix version -------------------------------------------------------
    #[test]
    fn rejects_wrong_nix_version() {
        for v in ["2.34.9", "2.34", "", "2.34.8 "] {
            let mut p = base();
            p.nix.version = v.to_string();
            assert!(
                matches!(validate_pin(&p), Err(PinError::NixVersion { .. })),
                "version {v:?}"
            );
        }
    }

    // ---- owner / repo ------------------------------------------------------
    #[test]
    fn rejects_wrong_owner() {
        let mut p = base();
        p.nixpkgs.owner = "evil".to_string();
        assert!(matches!(
            validate_pin(&p),
            Err(PinError::OwnerMismatch { .. })
        ));
    }

    #[test]
    fn rejects_wrong_repo() {
        let mut p = base();
        p.nixpkgs.repo = "hell".to_string();
        assert!(matches!(
            validate_pin(&p),
            Err(PinError::RepoMismatch { .. })
        ));
    }

    // ---- rev: malformed vs mismatch ---------------------------------------
    #[test]
    fn rejects_malformed_rev() {
        let good = NIXPKGS_REV;
        for bad in [
            good.to_uppercase(),
            good[..39].to_string(),
            format!("{good}0"),
            format!("{}g", &good[..39]),
            good.replace('a', "-"),
            "xyz".to_string(),
            String::new(),
        ] {
            let mut p = base();
            p.nixpkgs.rev = bad.clone();
            assert!(
                matches!(validate_pin(&p), Err(PinError::RevMalformed { .. })),
                "rev {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_well_shaped_but_wrong_rev() {
        // A valid 40-hex rev that is NOT the pin => RevMismatch (not Malformed).
        let mut p = base();
        p.nixpkgs.rev = "b62e6edd6d5e1fa0329b8653c801147986f8d446".to_string();
        assert_eq!(
            validate_pin(&p),
            Err(PinError::RevMismatch {
                got: "b62e6edd6d5e1fa0329b8653c801147986f8d446".to_string()
            })
        );
    }

    // ---- nar hash ----------------------------------------------------------
    #[test]
    fn rejects_malformed_nar_hash_prefix() {
        let mut p = base();
        p.nixpkgs.nar_hash = format!("sha512-{}", &p.nixpkgs.nar_hash["sha256-".len()..]);
        assert!(matches!(
            validate_pin(&p),
            Err(PinError::NarHashMalformed { .. })
        ));
    }

    #[test]
    fn rejects_well_shaped_but_wrong_nar_hash() {
        let mut p = base();
        // Same prefix, different base64 payload => Mismatch (not Malformed).
        p.nixpkgs.nar_hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string();
        assert!(matches!(
            validate_pin(&p),
            Err(PinError::NarHashMismatch { .. })
        ));
    }

    // ---- systems -----------------------------------------------------------
    #[test]
    fn rejects_wrong_systems() {
        {
            let mut p = base();
            p.systems = vec!["aarch64-darwin".into(), "x86_64-darwin".into()];
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::SystemsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.systems = vec!["x86_64-darwin".into()];
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::SystemsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.systems.push("x86_64-linux".into());
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::SystemsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.systems[0] = "x86_64-linux".into();
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::SystemsNotExact { .. })
            ));
        }
    }

    // ---- attrs -------------------------------------------------------------
    #[test]
    fn rejects_wrong_attrs() {
        {
            let mut p = base();
            p.attrs = vec!["git".into(), "ripgrep".into(), "hello".into()];
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::AttrsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.attrs = vec!["hello".into(), "ripgrep".into()];
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::AttrsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.attrs.push("curl".into());
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::AttrsNotExact { .. })
            ));
        }
        {
            let mut p = base();
            p.attrs[1] = "grep".into();
            assert!(matches!(
                validate_pin(&p),
                Err(PinError::AttrsNotExact { .. })
            ));
        }
    }

    // ---- cache store url ---------------------------------------------------
    #[test]
    fn rejects_wrong_cache_store_url() {
        for bad in [
            "https://cache.nixos.org", // missing trailing slash
            "http://cache.nixos.org/", // wrong scheme
            "https://cache.example.org/",
            "",
        ] {
            let mut p = base();
            p.cache_store_url = bad.to_string();
            assert!(
                matches!(
                    validate_pin(&p),
                    Err(PinError::CacheStoreUrlMismatch { .. })
                ),
                "cache {bad:?}"
            );
        }
    }

    // ---- bounded display ---------------------------------------------------
    #[test]
    fn display_is_bounded_for_huge_input() {
        let mut p = base();
        p.nix.version = "x".repeat(10_000);
        let s = validate_pin(&p).unwrap_err().to_string();
        assert!(
            s.len() < 256,
            "display must be bounded, was {}: {s:?}",
            s.len()
        );
    }

    #[test]
    fn bound_snippet_passthrough_and_truncation() {
        assert_eq!(bound_snippet("abc"), "abc");
        let s = bound_snippet(&"a".repeat(200));
        assert!(s.len() <= DISPLAY_SNIPPET_MAX + 3);
        assert!(s.ends_with("..."));
        // UTF-8 safety: truncate inside a multibyte char without panicking.
        let _ = bound_snippet(&"é".repeat(100));
    }

    #[test]
    fn is_40_lowercase_hex_rules() {
        assert!(is_40_lowercase_hex(NIXPKGS_REV));
        assert!(!is_40_lowercase_hex(&NIXPKGS_REV.to_uppercase()));
        assert!(!is_40_lowercase_hex(&NIXPKGS_REV[..39]));
        assert!(!is_40_lowercase_hex("xyz"));
    }

    #[test]
    fn exact_seq_rules() {
        assert!(exact_seq(
            &["x86_64-darwin".to_string(), "aarch64-darwin".to_string()],
            &DARWIN_SYSTEMS
        ));
        assert!(!exact_seq(&["x86_64-darwin".to_string()], &DARWIN_SYSTEMS));
        assert!(!exact_seq(&DARWIN_SYSTEMS.map(|s| s.to_string()), &ATTRS));
    }
}
