// Spike S3 (PR-7 / DR-003) — PIN-SUMMARY slice: the pin-summary DTO.
//
// This module owns the strictly-validated, immutable pin summary embedded at
// build time via `include_str!` from `fixtures.json` at the spike root. The pin
// is the SAME canonical pin S4 measures (Nix 2.34.8, NixOS/nixpkgs rev
// a62e…d446), scoped to the two Darwin systems and three fixture attributes S3
// covers, plus the single v1 cache store URL. It does NOT duplicate S4's
// raw-archive hash evidence: that hash concerns GitHub tarballs and is
// irrelevant to S3's substitution/build/sign questions.
//
// The DTO mirrors `fixtures.json`. Every field is public so the validator and
// tests can read it; mutation is the caller's responsibility and MUST be
// followed by [`crate::validate::validate_pin`] before the value is trusted.
//
// All DTO structs use `#[serde(deny_unknown_fields)]` so a typo'd or extra key
// is rejected at parse time, and `rename_all = "camelCase"` to mirror the
// fixture's camelCase JSON keys.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::validate;

/// The DTO mirror of `fixtures.json`: the pinned inputs every S3 report targets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PinSummary {
    /// Pin schema revision. Must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Pinned Nix release.
    pub nix: NixSpec,
    /// Pinned Nixpkgs flake source (owner/repo/rev/narHash).
    pub nixpkgs: NixpkgsSpec,
    /// Exact, ordered Darwin systems S3 covers.
    pub systems: Vec<String>,
    /// Exact, ordered fixture attribute set S3 covers.
    pub attrs: Vec<String>,
    /// Single v1 binary cache store URL.
    pub cache_store_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NixSpec {
    /// Exact pinned Nix version, e.g. `2.34.8`.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NixpkgsSpec {
    /// Flake owner (e.g. `NixOS`).
    pub owner: String,
    /// Flake repo (e.g. `nixpkgs`).
    pub repo: String,
    /// Exact 40-character lowercase-hex git revision.
    pub rev: String,
    /// Flake NAR hash as a canonical `sha256-…` SRI string.
    pub nar_hash: String,
}

/// Parse a pin-summary JSON string. Rejects unknown fields. Does NOT validate
/// semantics; pair with [`crate::validate::validate_pin`] for that.
pub fn parse(json: &str) -> Result<PinSummary, serde_json::Error> {
    serde_json::from_str(json)
}

/// Combined parse + validate, returning a typed error for either stage.
pub fn parse_and_validate(json: &str) -> Result<PinSummary, ManifestError> {
    let p = parse(json).map_err(ManifestError::Parse)?;
    validate::validate_pin(&p)?;
    Ok(p)
}

/// Error from [`parse_and_validate`].
#[derive(Debug)]
pub enum ManifestError {
    /// Structural JSON / serde failure (includes unknown-field rejection).
    Parse(serde_json::Error),
    /// Semantic validation failure.
    Invalid(validate::PinError),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Parse(e) => write!(f, "pin parse error: {e}"),
            ManifestError::Invalid(e) => write!(f, "pin validation error: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<validate::PinError> for ManifestError {
    fn from(e: validate::PinError) -> Self {
        ManifestError::Invalid(e)
    }
}

/// Return the validated embedded pin-summary singleton.
///
/// The JSON is compiled into the binary via `include_str!("../fixtures.json")`
/// and validated exactly once (the result is cached in a `OnceLock`). Because
/// the bytes are build-time-constant trusted data, a validation failure here
/// panics with a clear, bounded message — it means the pinned `fixtures.json`
/// was tampered with or mis-authored, a release-blocker rather than a runtime
/// condition.
pub fn pin_summary() -> &'static PinSummary {
    static PIN: OnceLock<PinSummary> = OnceLock::new();
    PIN.get_or_init(|| {
        let json = include_str!("../fixtures.json");
        let p = parse(json).expect("embedded fixtures.json must be valid JSON");
        validate::validate_pin(&p).expect("embedded fixtures.json must pass strict validation");
        p
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate;
    use serde_json::{Value, json};

    /// The embedded fixtures.json as a `Value`, for structural comparisons.
    fn embedded_value() -> Value {
        serde_json::from_str(include_str!("../fixtures.json")).unwrap()
    }

    #[test]
    fn embedded_pin_is_valid() {
        assert!(validate::validate_pin(pin_summary()).is_ok());
    }

    #[test]
    fn embedded_pin_roundtrips_serde() {
        let p = pin_summary();
        let json = serde_json::to_string(p).unwrap();
        let p2: PinSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(p, &p2);
        assert_eq!(p.schema_version, validate::SCHEMA_VERSION);
    }

    #[test]
    fn reserialization_matches_embedded_json_value() {
        let p = pin_summary();
        let roundtrip: Value = serde_json::from_str(&serde_json::to_string(p).unwrap()).unwrap();
        assert_eq!(roundtrip, embedded_value());
    }

    #[test]
    fn embedded_pin_carries_exact_pin() {
        let p = pin_summary();
        assert_eq!(p.nix.version, validate::NIX_VERSION);
        assert_eq!(p.nixpkgs.owner, validate::NIXPKGS_OWNER);
        assert_eq!(p.nixpkgs.repo, validate::NIXPKGS_REPO);
        assert_eq!(p.nixpkgs.rev, validate::NIXPKGS_REV);
        assert_eq!(p.nixpkgs.nar_hash, validate::NIXPKGS_NAR_HASH);
        assert_eq!(p.systems.as_slice(), validate::DARWIN_SYSTEMS);
        assert_eq!(p.attrs.as_slice(), validate::ATTRS);
        assert_eq!(p.cache_store_url, validate::CACHE_STORE_URL);
    }

    #[test]
    fn nixpkgs_spec_round_trips_camel_case_keys() {
        let s = NixpkgsSpec {
            owner: "NixOS".to_string(),
            repo: "nixpkgs".to_string(),
            rev: "a62e6edd6d5e1fa0329b8653c801147986f8d446".to_string(),
            nar_hash: "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=".to_string(),
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({
                "owner": "NixOS",
                "repo": "nixpkgs",
                "rev": "a62e6edd6d5e1fa0329b8653c801147986f8d446",
                "narHash": "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw="
            })
        );
        let back: NixpkgsSpec = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn pin_summary_round_trips_camel_case_keys() {
        let p = pin_summary();
        let v: Value = serde_json::from_str(&serde_json::to_string(p).unwrap()).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("schemaVersion"));
        assert!(obj.contains_key("cacheStoreUrl"));
        assert!(
            obj.get("nixpkgs")
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("narHash")
        );
        let back: PinSummary = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, *p);
    }

    // ---- deny_unknown_fields ----------------------------------------------
    fn pin_value() -> Value {
        serde_json::from_str(&serde_json::to_string(pin_summary()).unwrap()).unwrap()
    }

    fn with_unknown_key(path: &[&str]) -> String {
        let mut v = pin_value();
        if path.is_empty() {
            v["bogusKey"] = json!(1);
        } else {
            let mut cur = &mut v;
            for seg in path {
                cur = &mut cur[*seg];
            }
            cur["bogusKey"] = json!(1);
        }
        v.to_string()
    }

    #[test]
    fn parse_rejects_unknown_top_level_key() {
        assert!(parse(&with_unknown_key(&[])).is_err());
    }

    #[test]
    fn parse_rejects_unknown_keys_in_every_container() {
        for path in [&[][..], &["nix"][..], &["nixpkgs"][..]] {
            assert!(
                parse(&with_unknown_key(path)).is_err(),
                "unknown key under {} must be rejected",
                path.join(".")
            );
        }
    }

    #[test]
    fn parse_rejects_missing_required_container() {
        let mut v = pin_value();
        v.as_object_mut().unwrap().remove("nixpkgs");
        assert!(parse(&v.to_string()).is_err());
    }

    #[test]
    fn parse_rejects_raw_archive_key() {
        // The S4 raw-archive evidence must NOT appear in the S3 pin.
        let mut v = pin_value();
        v["rawArchive"] = json!({ "sha256Sri": "sha256-dead" });
        assert!(parse(&v.to_string()).is_err());
    }

    // ---- parse_and_validate stages ----------------------------------------
    #[test]
    fn parse_and_validate_accepts_embedded_pin() {
        let json = serde_json::to_string(pin_summary()).unwrap();
        let p = parse_and_validate(&json).expect("embedded pin must validate");
        assert_eq!(p, *pin_summary());
    }

    #[test]
    fn parse_and_validate_reports_parse_errors() {
        let err = parse_and_validate("{ not json ").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
        let err2 = parse_and_validate(&with_unknown_key(&["nix"])).unwrap_err();
        assert!(matches!(err2, ManifestError::Parse(_)));
    }

    #[test]
    fn parse_and_validate_reports_validation_errors() {
        let mut v = pin_value();
        v["schemaVersion"] = json!(2);
        let err = parse_and_validate(&v.to_string()).unwrap_err();
        match err {
            ManifestError::Invalid(e) => {
                assert_eq!(e, validate::PinError::SchemaVersion { got: 2 });
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn manifest_error_display_is_bounded_and_named() {
        let err = parse_and_validate("{ not json ").unwrap_err();
        let s = err.to_string();
        assert!(s.starts_with("pin parse error:"), "got: {s}");
    }
}
