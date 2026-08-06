// Spike S4 (PR-6 / DR-004) — DATA MODEL slice: the benchmark manifest DTO.
//
// This module owns the strictly-validated, immutable benchmark manifest: the
// pinned Nix version, the pinned Nixpkgs source (`owner`/`repo`/`rev`/`narHash`),
// the four supported systems, the single attribute under measurement (`ripgrep`),
// the warmup / per-phase sample defaults, the per-phase child-output caps, the
// per-phase and overall wall-clock timeouts, and the raw GitHub-archive evidence
// fields (URL + sha256 in both SRI and hex form + pinned byte length).
//
// The manifest text is embedded at compile time via `include_str!` from
// `benchmark.json` at the spike root, parsed with `serde_json`, and validated by
// `crate::validate`. `benchmark_manifest()` returns the validated singleton; the
// embedded bytes are trusted-by-build constant data, so a validation failure
// there is a hard error (the pinned evidence was tampered with or mis-authored).
//
// All DTO structs use `#[serde(deny_unknown_fields)]` so a typo'd or extra key
// is rejected at parse time rather than silently ignored. Field names are
// snake_case in Rust and mapped to the manifest's camelCase JSON keys with
// `rename_all = "camelCase"` (the per-field `rename` attributes repeat the wire
// name explicitly so the data contract is self-documenting).
//
// schemaVersion is still 1 (the contract slice described here is the
// pre-runner data-contract form of revision 1, still uncommitted).

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The one-and-only benchmark schema revision this harness understands.
pub const SCHEMA_VERSION: u32 = 1;

/// The DTO mirror of `benchmark.json`. Every field is public so the validator
/// (and tests) can read it; mutation is the caller's responsibility and MUST be
/// followed by `validate::validate` before the value is trusted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    /// Manifest schema revision. Must equal [`SCHEMA_VERSION`].
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Pinned Nix release under measurement.
    pub nix: NixSpec,
    /// Pinned Nixpkgs flake source (rev + flake `narHash`).
    pub nixpkgs: NixpkgsSpec,
    /// Exact, ordered set of supported systems.
    pub systems: Vec<String>,
    /// The single attribute path under measurement (e.g. `ripgrep`).
    pub attr: String,
    /// Warmup / per-phase measured-sample defaults.
    pub sampling: Sampling,
    /// Conservative per-phase child stdout/stderr capture caps in bytes.
    pub caps: Caps,
    /// Per-phase and overall wall-clock timeouts in seconds.
    pub timeouts: Timeouts,
    /// Raw GitHub-archive evidence (URL + sha256 SRI + sha256 hex + byte
    /// length). These are *evidence* fields, not the trust input: the flake
    /// `narHash` above is the verified value; the raw-archive hash differs
    /// (DR-004 finding) and is recorded here so the two domains cannot be
    /// conflated.
    #[serde(rename = "rawArchive")]
    pub raw_archive: RawArchive,
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
    pub owner: String,
    pub repo: String,
    /// Exact 40-character lowercase-hex git revision.
    pub rev: String,
    /// Flake NAR hash as a canonical `sha256-…` SRI string.
    #[serde(rename = "narHash")]
    pub nar_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sampling {
    /// Number of warmup iterations (may be 0 = no warmup).
    pub warmup: u32,
    /// Number of measured single-attribute iterations (must be >= 1).
    #[serde(rename = "singleAttrSamples")]
    pub single_attr_samples: u32,
    /// Number of measured index-meta iterations (must be >= 1).
    #[serde(rename = "indexSamples")]
    pub index_samples: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Caps {
    /// Maximum single-attribute stdout bytes captured before a bounded-overflow
    /// failure.
    #[serde(rename = "singleAttrStdoutBytes")]
    pub single_attr_stdout_bytes: u64,
    /// Maximum index-meta stdout bytes captured before a bounded-overflow
    /// failure.
    #[serde(rename = "indexStdoutBytes")]
    pub index_stdout_bytes: u64,
    /// Maximum child stderr bytes captured (shared across phases) before a
    /// bounded-overflow failure.
    #[serde(rename = "stderrBytes")]
    pub stderr_bytes: u64,
}

/// Per-phase and overall wall-clock timeouts, in whole seconds. The overall
/// budget must be at least each per-command budget; `validate` enforces that.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Timeouts {
    /// Per-command budget for the single-attribute measurement, in seconds.
    #[serde(rename = "singleAttrSeconds")]
    pub single_attr_seconds: u64,
    /// Per-command budget for the index-meta measurement, in seconds.
    #[serde(rename = "indexSeconds")]
    pub index_seconds: u64,
    /// Overall budget spanning both phases, in seconds.
    #[serde(rename = "overallSeconds")]
    pub overall_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RawArchive {
    /// `https://github.com/<owner>/<repo>/archive/<rev>.tar.gz` — must reference
    /// the exact same `rev` as [`NixpkgsSpec::rev`].
    pub url: String,
    /// Raw-archive sha256 as a canonical `sha256-…` SRI string.
    #[serde(rename = "sha256Sri")]
    pub sha256_sri: String,
    /// Raw-archive sha256 as 64 lowercase-hex characters. Must be the hex form
    /// of the same digest as [`RawArchive::sha256_sri`].
    #[serde(rename = "sha256Hex")]
    pub sha256_hex: String,
    /// Pinned byte length of the raw GitHub archive. An EXACT pin (must equal
    /// [`crate::validate::RAW_ARCHIVE_BYTES`]), not a range: the evidence byte
    /// length is part of the tamper-evident record.
    pub bytes: u64,
}

/// Parse a manifest JSON string. Rejects unknown fields (see `deny_unknown_fields`).
/// Does NOT validate semantics; pair with [`crate::validate::validate`] for that.
pub fn parse(json: &str) -> Result<Manifest, serde_json::Error> {
    serde_json::from_str(json)
}

/// Combined parse + validate, returning a typed error for either stage.
pub fn parse_and_validate(json: &str) -> Result<Manifest, ManifestError> {
    let m = parse(json).map_err(ManifestError::Parse)?;
    crate::validate::validate(&m)?;
    Ok(m)
}

/// Error from [`parse_and_validate`].
#[derive(Debug)]
pub enum ManifestError {
    Parse(serde_json::Error),
    Invalid(crate::validate::ValidationError),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Parse(e) => write!(f, "manifest parse error: {e}"),
            ManifestError::Invalid(e) => write!(f, "manifest validation error: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<crate::validate::ValidationError> for ManifestError {
    fn from(e: crate::validate::ValidationError) -> Self {
        ManifestError::Invalid(e)
    }
}

/// Return the validated embedded benchmark manifest singleton.
///
/// The JSON is compiled into the binary via `include_str!("../benchmark.json")`
/// and validated exactly once (the result is cached in a `OnceLock`). Because
/// the bytes are build-time-constant trusted evidence, a validation failure
/// here panics with a clear, bounded message — it means the pinned evidence in
/// `benchmark.json` was tampered with or mis-authored, which is a release-blocker
/// rather than a runtime condition.
pub fn benchmark_manifest() -> &'static Manifest {
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(|| {
        let json = include_str!("../benchmark.json");
        let m = parse(json).expect("embedded benchmark.json must be valid JSON");
        crate::validate::validate(&m).expect("embedded benchmark.json must pass strict validation");
        m
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate;
    use serde_json::{Value, json};

    /// The embedded benchmark.json as a `Value`, for structural comparisons.
    fn embedded_value() -> Value {
        serde_json::from_str(include_str!("../benchmark.json")).unwrap()
    }

    // ---- embedded manifest: validity + exact pins --------------------------
    #[test]
    fn embedded_manifest_is_valid() {
        assert!(validate::validate(benchmark_manifest()).is_ok());
    }

    #[test]
    fn embedded_manifest_roundtrips_serde() {
        let m = benchmark_manifest();
        // Serialize then re-parse; field-equality must hold (deny_unknown_fields
        // means re-serialized form has no stray keys).
        let json = serde_json::to_string(m).unwrap();
        let m2: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, &m2);
        assert_eq!(m.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn reserialization_matches_embedded_json_value() {
        // Serializing the parsed DTO must reproduce the SAME JSON value (same
        // keys, same values) as the embedded file — i.e. the DTO is a faithful,
        // lossless mirror of the wire contract (no dropped/added keys).
        let m = benchmark_manifest();
        let roundtrip: Value = serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap();
        assert_eq!(roundtrip, embedded_value());
    }

    #[test]
    fn embedded_manifest_pins_new_contract_fields() {
        let m = benchmark_manifest();
        assert_eq!(m.sampling.warmup, 1);
        assert_eq!(m.sampling.single_attr_samples, 5);
        assert_eq!(m.sampling.index_samples, 3);
        assert_eq!(m.caps.single_attr_stdout_bytes, 1_048_576);
        assert_eq!(m.caps.index_stdout_bytes, 268_435_456);
        assert_eq!(m.caps.stderr_bytes, 8_388_608);
        assert_eq!(m.timeouts.single_attr_seconds, 300);
        assert_eq!(m.timeouts.index_seconds, 600);
        assert_eq!(m.timeouts.overall_seconds, 3600);
        assert_eq!(m.raw_archive.bytes, validate::RAW_ARCHIVE_BYTES);
        assert_eq!(validate::RAW_ARCHIVE_BYTES, 38_667_882);
    }

    // ---- sub-struct serde round trips (wire names) -------------------------
    #[test]
    fn sampling_round_trips_camel_case_keys() {
        let s = Sampling {
            warmup: 2,
            single_attr_samples: 7,
            index_samples: 9,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({ "warmup": 2, "singleAttrSamples": 7, "indexSamples": 9 })
        );
        let back: Sampling = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn caps_round_trips_camel_case_keys() {
        let c = Caps {
            single_attr_stdout_bytes: 1_048_576,
            index_stdout_bytes: 268_435_456,
            stderr_bytes: 8_388_608,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({
                "singleAttrStdoutBytes": 1_048_576,
                "indexStdoutBytes": 268_435_456,
                "stderrBytes": 8_388_608
            })
        );
        let back: Caps = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn timeouts_round_trips_camel_case_keys() {
        let t = Timeouts {
            single_attr_seconds: 300,
            index_seconds: 600,
            overall_seconds: 3600,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(
            v,
            json!({
                "singleAttrSeconds": 300,
                "indexSeconds": 600,
                "overallSeconds": 3600
            })
        );
        let back: Timeouts = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn raw_archive_round_trips_with_bytes() {
        let r = RawArchive {
            url: "https://example.invalid/x/archive/deadbeef.tar.gz".to_string(),
            sha256_sri: "sha256-rXVGuq8bJfByJbOrrB3I++2MTsvZDcTo7C6UHXD5muE=".to_string(),
            sha256_hex: "ad7546baaf1b25f07225b3abac1dc8fbed8c4ecbd90dc4e8ec2e941d70f99ae1"
                .to_string(),
            bytes: 38_667_882,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["bytes"], json!(38_667_882));
        assert!(v["sha256Sri"].is_string());
        let back: RawArchive = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, r);
    }

    // ---- deny_unknown_fields: unknown keys are rejected everywhere ---------
    /// Clone the embedded manifest as a mutable `Value`.
    fn manifest_value() -> Value {
        serde_json::from_str(&serde_json::to_string(benchmark_manifest()).unwrap()).unwrap()
    }

    /// Inject `{"bogusKey": 1}` at the given dotted path into the manifest and
    /// return the resulting JSON string.
    fn with_unknown_key(path: &[&str]) -> String {
        let mut v = manifest_value();
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
        for path in [
            &["nix"][..],
            &["nixpkgs"][..],
            &["sampling"][..],
            &["caps"][..],
            &["timeouts"][..],
            &["rawArchive"][..],
        ] {
            assert!(
                parse(&with_unknown_key(path)).is_err(),
                "unknown key under {} must be rejected",
                path.join(".")
            );
        }
    }

    #[test]
    fn parse_rejects_missing_required_container() {
        // Drop the entire `timeouts` block: a required key is now absent.
        let mut v = manifest_value();
        let obj = v.as_object_mut().unwrap();
        obj.remove("timeouts");
        assert!(parse(&v.to_string()).is_err());
    }

    // ---- parse_and_validate: parse vs validation stage ---------------------
    #[test]
    fn parse_and_validate_accepts_embedded_manifest() {
        let json = serde_json::to_string(benchmark_manifest()).unwrap();
        let m = parse_and_validate(&json).expect("embedded manifest must validate");
        assert_eq!(m, *benchmark_manifest());
    }

    #[test]
    fn parse_and_validate_reports_parse_errors() {
        // Malformed JSON surfaces as the Parse stage.
        let err = parse_and_validate("{ not json ").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
        // An unknown key is also a structural parse failure (deny_unknown_fields).
        let bad = with_unknown_key(&["sampling"]);
        let err2 = parse_and_validate(&bad).unwrap_err();
        assert!(matches!(err2, ManifestError::Parse(_)));
    }

    #[test]
    fn parse_and_validate_reports_validation_errors() {
        // Well-formed JSON that fails SEMANTIC validation: schemaVersion wrong.
        let mut v = manifest_value();
        v["schemaVersion"] = json!(2);
        let err = parse_and_validate(&v.to_string()).unwrap_err();
        match err {
            ManifestError::Invalid(e) => {
                assert_eq!(e, validate::ValidationError::SchemaVersion { got: 2 });
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn manifest_error_display_is_bounded_and_named() {
        let err = parse_and_validate("{ not json ").unwrap_err();
        let s = err.to_string();
        assert!(s.starts_with("manifest parse error:"), "got: {s}");
    }
}
