// Spike S4 (PR-6 / DR-004) — manifest semantic validation.
//
// This module is the *semantic* gate for the benchmark manifest DTO defined in
// `manifest.rs`. `serde` already enforces structural shape (types, required
// keys, `deny_unknown_fields`); this module enforces that the values are the
// pinned, safe, evidence-correct ones the harness expects:
//
//   * `schemaVersion` must be exactly `SCHEMA_VERSION` (1).
//   * `nix.version` must be exactly the pinned release under measurement
//     (`NIX_VERSION` = "2.34.8").
//   * `nixpkgs.owner`/`repo` must be *safe identifiers*: a strict allowlist
//     that rejects traversal (`..`, `/`, leading/trailing dots), control bytes,
//     whitespace, and shell punctuation.
//   * `nixpkgs.attr` must be *exactly* the pinned attribute under measurement
//     (`ATTR` = "ripgrep") — not merely syntactically safe. This pins the
//     evidence to a single, known package so a manifest cannot silently
//     retarget the benchmark.
//   * `nixpkgs.rev` must be exactly 40 lowercase-hex characters.
//   * `nixpkgs.narHash` must be a *canonical* `sha256-…` SRI string: the base64
//     payload must decode to exactly 32 bytes AND re-encode to the identical
//     payload (the decode+encode equality check from `base64sri`). This rejects
//     non-canonical encodings (trailing-bit pollution) and any non-`sha256`
//     algorithm prefix.
//   * `systems` must be exactly the four pinned systems in order.
//   * `sampling.warmup` must be `0..=100`; `sampling.singleAttrSamples` and
//     `sampling.indexSamples` must each be `1..=100`. The sample errors name the
//     exact field so a mis-authored manifest is unambiguous.
//   * `caps.singleAttrStdoutBytes`, `caps.indexStdoutBytes`, and
//     `caps.stderrBytes` must each be within `1024..=536_870_912` bytes
//     (1 KiB .. 512 MiB). The cap errors name the exact field.
//   * `timeouts.singleAttrSeconds` and `timeouts.indexSeconds` must each be
//     `1..=3600`; `timeouts.overallSeconds` must be `1..=14400`; and
//     `overallSeconds` must be at least each per-command timeout. The timeout
//     errors name the exact field.
//   * `rawArchive.bytes` must equal the pinned constant `RAW_ARCHIVE_BYTES`
//     (an exact pin, not a range).
//   * `rawArchive.url` must be exactly the GitHub archive URL derived from the
//     validated owner/repo/rev (so the archive is *tied to the rev*).
//   * `rawArchive.sha256Hex` must be exactly 64 lowercase-hex characters.
//   * `rawArchive.sha256Sri` must be a canonical `sha256-…` SRI (same rules as
//     the flake `narHash`) AND decode to the *same* 32-byte digest as the hex.
//   * the flake `narHash` digest must *differ* from the raw-archive digest
//     (DR-004 finding: the two hash domains are distinct and must not be
//     conflated).
//
// `ValidationError`'s `Display` is *bounded*: any caller-controlled snippet
// included in an error message is truncated (`DISPLAY_SNIPPET_MAX`) so a
// malicious manifest cannot bloat logs, panic messages, or reports.

use std::fmt;

use crate::base64sri;
use crate::manifest::{Manifest, SCHEMA_VERSION};

/// The single pinned Nix release this harness measures.
pub const NIX_VERSION: &str = "2.34.8";

/// The exact, ordered set of supported systems (DR-004 environment).
pub const SYSTEMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "x86_64-darwin",
    "aarch64-darwin",
];

/// The single, pinned attribute path under measurement.
pub const ATTR: &str = "ripgrep";
/// Maximum accepted warmup iteration count.
pub const MAX_WARMUP: u32 = 100;
/// Minimum accepted measured-sample count for EACH sample field (must run at
/// least once). Applies to both `singleAttrSamples` and `indexSamples`.
pub const MIN_SAMPLES: u32 = 1;
/// Maximum accepted measured-sample count for EACH sample field.
pub const MAX_SAMPLES: u32 = 100;
/// Minimum accepted per-stream child-output cap, in bytes (1 KiB).
pub const MIN_CAP_BYTES: u64 = 1024;
/// Maximum accepted per-stream child-output cap, in bytes (512 MiB
/// = 536_870_912). Caps exist to fail closed on runaway output; this is a hard,
/// bounded ceiling well above any plausible real capture.
pub const MAX_CAP_BYTES: u64 = 536_870_912;
/// Minimum accepted per-command or overall timeout, in seconds.
pub const MIN_TIMEOUT_SECONDS: u64 = 1;
/// Maximum accepted PER-COMMAND timeout (`singleAttrSeconds`/`indexSeconds`),
/// in seconds (1 hour).
pub const MAX_PER_COMMAND_TIMEOUT_SECONDS: u64 = 3600;
/// Maximum accepted OVERALL timeout (`overallSeconds`), in seconds (4 hours).
pub const MAX_OVERALL_TIMEOUT_SECONDS: u64 = 14400;
/// Pinned raw-archive byte length. An EXACT pin, not a range.
pub const RAW_ARCHIVE_BYTES: u64 = 38_667_882;

/// Maximum characters of any caller-controlled string included in a
/// `ValidationError`'s `Display` output. Keeps error / log / panic messages
/// bounded regardless of input size.
pub const DISPLAY_SNIPPET_MAX: usize = 64;

/// Validate a parsed manifest against the pinned, safe, evidence-correct rules
/// above. Returns `Ok(())` if every field is acceptable, or the first
/// [`ValidationError`] otherwise.
pub fn validate(m: &Manifest) -> Result<(), ValidationError> {
    if m.schema_version != SCHEMA_VERSION {
        return Err(ValidationError::SchemaVersion {
            got: m.schema_version,
        });
    }
    if m.nix.version != NIX_VERSION {
        return Err(ValidationError::NixVersion {
            got: m.nix.version.clone(),
        });
    }
    if !is_safe_identifier(&m.nixpkgs.owner) {
        return Err(ValidationError::OwnerUnsafe {
            got: m.nixpkgs.owner.clone(),
        });
    }
    if !is_safe_identifier(&m.nixpkgs.repo) {
        return Err(ValidationError::RepoUnsafe {
            got: m.nixpkgs.repo.clone(),
        });
    }
    if !is_40_lowercase_hex(&m.nixpkgs.rev) {
        return Err(ValidationError::RevMalformed {
            got: m.nixpkgs.rev.clone(),
        });
    }
    let nar_bytes = canonical_sri_sha256(&m.nixpkgs.nar_hash).ok_or_else(|| {
        ValidationError::NarHashMalformed {
            got: m.nixpkgs.nar_hash.clone(),
        }
    })?;

    let systems_ok = m.systems.len() == SYSTEMS.len()
        && m.systems
            .iter()
            .zip(SYSTEMS.iter())
            .all(|(s, e)| s.as_str() == *e);
    if !systems_ok {
        return Err(ValidationError::SystemsNotExact {
            got: m.systems.clone(),
        });
    }

    if m.attr != ATTR {
        return Err(ValidationError::AttrMismatch {
            got: m.attr.clone(),
        });
    }

    if m.sampling.warmup > MAX_WARMUP {
        return Err(ValidationError::WarmupOutOfRange {
            got: m.sampling.warmup,
        });
    }
    if !(MIN_SAMPLES..=MAX_SAMPLES).contains(&m.sampling.single_attr_samples) {
        return Err(ValidationError::SamplesOutOfRange {
            field: "singleAttrSamples",
            got: m.sampling.single_attr_samples,
        });
    }
    if !(MIN_SAMPLES..=MAX_SAMPLES).contains(&m.sampling.index_samples) {
        return Err(ValidationError::SamplesOutOfRange {
            field: "indexSamples",
            got: m.sampling.index_samples,
        });
    }

    if !(MIN_CAP_BYTES..=MAX_CAP_BYTES).contains(&m.caps.single_attr_stdout_bytes) {
        return Err(ValidationError::CapOutOfRange {
            field: "singleAttrStdoutBytes",
            got: m.caps.single_attr_stdout_bytes,
        });
    }
    if !(MIN_CAP_BYTES..=MAX_CAP_BYTES).contains(&m.caps.index_stdout_bytes) {
        return Err(ValidationError::CapOutOfRange {
            field: "indexStdoutBytes",
            got: m.caps.index_stdout_bytes,
        });
    }
    if !(MIN_CAP_BYTES..=MAX_CAP_BYTES).contains(&m.caps.stderr_bytes) {
        return Err(ValidationError::CapOutOfRange {
            field: "stderrBytes",
            got: m.caps.stderr_bytes,
        });
    }

    if !(MIN_TIMEOUT_SECONDS..=MAX_PER_COMMAND_TIMEOUT_SECONDS)
        .contains(&m.timeouts.single_attr_seconds)
    {
        return Err(ValidationError::TimeoutOutOfRange {
            field: "singleAttrSeconds",
            got: m.timeouts.single_attr_seconds,
        });
    }
    if !(MIN_TIMEOUT_SECONDS..=MAX_PER_COMMAND_TIMEOUT_SECONDS).contains(&m.timeouts.index_seconds)
    {
        return Err(ValidationError::TimeoutOutOfRange {
            field: "indexSeconds",
            got: m.timeouts.index_seconds,
        });
    }
    if !(MIN_TIMEOUT_SECONDS..=MAX_OVERALL_TIMEOUT_SECONDS).contains(&m.timeouts.overall_seconds) {
        return Err(ValidationError::TimeoutOutOfRange {
            field: "overallSeconds",
            got: m.timeouts.overall_seconds,
        });
    }
    if m.timeouts.overall_seconds < m.timeouts.single_attr_seconds {
        return Err(ValidationError::OverallTimeoutBelowCommand {
            command_field: "singleAttrSeconds",
            overall_seconds: m.timeouts.overall_seconds,
            command_seconds: m.timeouts.single_attr_seconds,
        });
    }
    if m.timeouts.overall_seconds < m.timeouts.index_seconds {
        return Err(ValidationError::OverallTimeoutBelowCommand {
            command_field: "indexSeconds",
            overall_seconds: m.timeouts.overall_seconds,
            command_seconds: m.timeouts.index_seconds,
        });
    }

    let expected_url = format!(
        "https://github.com/{}/{}/archive/{}.tar.gz",
        m.nixpkgs.owner, m.nixpkgs.repo, m.nixpkgs.rev
    );
    if m.raw_archive.url != expected_url {
        return Err(ValidationError::ArchiveUrlMismatch {
            got: m.raw_archive.url.clone(),
        });
    }

    if m.raw_archive.bytes != RAW_ARCHIVE_BYTES {
        return Err(ValidationError::RawArchiveBytesMismatch {
            got: m.raw_archive.bytes,
        });
    }

    let hex_bytes =
        decode_hex32(&m.raw_archive.sha256_hex).ok_or_else(|| ValidationError::HexMalformed {
            got: m.raw_archive.sha256_hex.clone(),
        })?;
    let raw_sri_bytes = canonical_sri_sha256(&m.raw_archive.sha256_sri).ok_or_else(|| {
        ValidationError::RawSriMalformed {
            got: m.raw_archive.sha256_sri.clone(),
        }
    })?;

    if raw_sri_bytes != hex_bytes {
        return Err(ValidationError::RawDigestMismatch);
    }
    if nar_bytes == raw_sri_bytes {
        return Err(ValidationError::NarDigestDoesNotDiffer);
    }

    Ok(())
}

/// A bounded, caller-snippet-safe validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// `schemaVersion` did not equal `SCHEMA_VERSION`.
    SchemaVersion { got: u32 },
    /// `nix.version` did not equal `NIX_VERSION`.
    NixVersion { got: String },
    /// `nixpkgs.owner` failed the safe-identifier check.
    OwnerUnsafe { got: String },
    /// `nixpkgs.repo` failed the safe-identifier check.
    RepoUnsafe { got: String },
    /// `nixpkgs.rev` was not exactly 40 lowercase-hex characters.
    RevMalformed { got: String },
    /// `nixpkgs.narHash` was not a canonical `sha256-…` SRI of 32 bytes.
    NarHashMalformed { got: String },
    /// `systems` was not exactly the four pinned systems in order.
    SystemsNotExact { got: Vec<String> },
    /// `attr` did not equal the pinned attribute [`ATTR`] ("ripgrep").
    AttrMismatch { got: String },
    /// `sampling.warmup` exceeded `MAX_WARMUP`.
    WarmupOutOfRange { got: u32 },
    /// `sampling.<field>` was outside `[MIN_SAMPLES..=MAX_SAMPLES]`. `field` is
    /// the exact JSON key (`singleAttrSamples` or `indexSamples`).
    SamplesOutOfRange { field: &'static str, got: u32 },
    /// `caps.<field>` was outside `[MIN_CAP_BYTES..=MAX_CAP_BYTES]`. `field` is
    /// the exact JSON key.
    CapOutOfRange { field: &'static str, got: u64 },
    /// `timeouts.<field>` was outside its allowed range. `field` is the exact
    /// JSON key (`singleAttrSeconds`, `indexSeconds`, or `overallSeconds`); the
    /// per-command and overall fields have different ceilings.
    TimeoutOutOfRange { field: &'static str, got: u64 },
    /// `timeouts.overallSeconds` was below a per-command timeout. `command_field`
    /// names which per-command field it fell below.
    OverallTimeoutBelowCommand {
        command_field: &'static str,
        overall_seconds: u64,
        command_seconds: u64,
    },
    /// `rawArchive.bytes` did not equal the pinned `RAW_ARCHIVE_BYTES`.
    RawArchiveBytesMismatch { got: u64 },
    /// `rawArchive.url` was not the exact archive URL tied to the rev.
    ArchiveUrlMismatch { got: String },
    /// `rawArchive.sha256Hex` was not exactly 64 lowercase-hex characters.
    HexMalformed { got: String },
    /// `rawArchive.sha256Sri` was not a canonical `sha256-…` SRI of 32 bytes.
    RawSriMalformed { got: String },
    /// `rawArchive.sha256Sri` and `sha256Hex` did not encode the same digest.
    RawDigestMismatch,
    /// `nixpkgs.narHash` did not differ from `rawArchive.sha256Sri`.
    NarDigestDoesNotDiffer,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion { got } => {
                write!(f, "schemaVersion must be {SCHEMA_VERSION}, got {got}")
            }
            Self::NixVersion { got } => write!(
                f,
                "nix.version must be exactly {NIX_VERSION:?}, got {:?}",
                bound_snippet(got)
            ),
            Self::OwnerUnsafe { got } => write!(
                f,
                "nixpkgs.owner is not a safe identifier: {:?}",
                bound_snippet(got)
            ),
            Self::RepoUnsafe { got } => write!(
                f,
                "nixpkgs.repo is not a safe identifier: {:?}",
                bound_snippet(got)
            ),
            Self::RevMalformed { got } => write!(
                f,
                "nixpkgs.rev must be 40 lowercase-hex chars, got {:?}",
                bound_snippet(got)
            ),
            Self::NarHashMalformed { got } => write!(
                f,
                "nixpkgs.narHash must be a canonical sha256-\u{2026} SRI of 32 bytes, got {:?}",
                bound_snippet(got)
            ),
            Self::SystemsNotExact { got } => write!(
                f,
                "systems must be exactly {:?} in order, got {:?}",
                SYSTEMS,
                bound_snippet(&got.join(","))
            ),
            Self::AttrMismatch { got } => {
                write!(
                    f,
                    "attr must be exactly {ATTR:?}, got {:?}",
                    bound_snippet(got)
                )
            }
            Self::WarmupOutOfRange { got } => {
                write!(f, "sampling.warmup must be 0..={MAX_WARMUP}, got {got}")
            }
            Self::SamplesOutOfRange { field, got } => {
                write!(
                    f,
                    "sampling.{field} must be {MIN_SAMPLES}..={MAX_SAMPLES}, got {got}"
                )
            }
            Self::CapOutOfRange { field, got } => write!(
                f,
                "caps.{field} must be {MIN_CAP_BYTES}..={MAX_CAP_BYTES} bytes, got {got}"
            ),
            Self::TimeoutOutOfRange { field, got } => {
                let max = if *field == "overallSeconds" {
                    MAX_OVERALL_TIMEOUT_SECONDS
                } else {
                    MAX_PER_COMMAND_TIMEOUT_SECONDS
                };
                write!(
                    f,
                    "timeouts.{field} must be {MIN_TIMEOUT_SECONDS}..={max} seconds, got {got}"
                )
            }
            Self::OverallTimeoutBelowCommand {
                command_field,
                overall_seconds,
                command_seconds,
            } => write!(
                f,
                "timeouts.overallSeconds ({overall_seconds}) must be >= timeouts.{command_field} ({command_seconds})"
            ),
            Self::RawArchiveBytesMismatch { got } => write!(
                f,
                "rawArchive.bytes must be exactly {RAW_ARCHIVE_BYTES}, got {got}"
            ),
            Self::ArchiveUrlMismatch { got } => write!(
                f,
                "rawArchive.url must be the GitHub archive URL tied to the rev, got {:?}",
                bound_snippet(got)
            ),
            Self::HexMalformed { got } => write!(
                f,
                "rawArchive.sha256Hex must be 64 lowercase-hex chars, got {:?}",
                bound_snippet(got)
            ),
            Self::RawSriMalformed { got } => write!(
                f,
                "rawArchive.sha256Sri must be a canonical sha256-\u{2026} SRI of 32 bytes, got {:?}",
                bound_snippet(got)
            ),
            Self::RawDigestMismatch => {
                f.write_str("rawArchive.sha256Sri and sha256Hex must encode the same sha256 digest")
            }
            Self::NarDigestDoesNotDiffer => f.write_str(
                "nixpkgs.narHash must differ from rawArchive.sha256Sri (DR-004 hash domains)",
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Truncate a caller-controlled string for safe inclusion in a bounded
/// `ValidationError` `Display` output. Slices on a UTF-8 boundary.
fn bound_snippet(s: &str) -> String {
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

/// A "safe identifier": a non-empty, bounded string over a strict allowlist of
/// `[A-Za-z0-9._-]` with no traversal (`..`), no `/`, and no leading/trailing
/// dot. This rejects control bytes, whitespace, and shell punctuation for
/// `owner` and `repo` (dots allowed for compatibility with dotted paths).
/// `attr` is NOT checked here — it is pinned to an exact value in `validate`.
fn is_safe_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Exactly 40 lowercase-hex characters (`[0-9a-f]`).
fn is_40_lowercase_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Decode exactly 64 lowercase-hex characters to 32 bytes, rejecting uppercase
/// hex, non-hex, and any wrong length.
fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = nibble(b[2 * i])?;
        let lo = nibble(b[2 * i + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Validate a `sha256-<base64>` SRI string and return the decoded 32 bytes iff:
/// the prefix is exactly `sha256-`, the base64 payload decodes (via
/// [`base64sri::decode`]) to exactly 32 bytes, and the payload is *canonical*
/// ([`base64sri::encode`] of the decoded bytes reproduces the exact payload).
/// This rejects the wrong algorithm, malformed base64, non-32-byte hashes, and
/// non-canonical encodings.
fn canonical_sri_sha256(s: &str) -> Option<[u8; 32]> {
    const PREFIX: &str = "sha256-";
    let payload = s.strip_prefix(PREFIX)?;
    let decoded = base64sri::decode(payload).ok()?;
    if decoded.len() != 32 {
        return None;
    }
    if base64sri::encode(&decoded) != payload {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&decoded);
    Some(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;

    /// A clone of the validated embedded manifest, safe to mutate per-test.
    fn base() -> Manifest {
        manifest::benchmark_manifest().clone()
    }

    #[test]
    fn embedded_manifest_is_valid() {
        assert!(validate(&base()).is_ok());
    }

    // ---- schema -------------------------------------------------------------
    #[test]
    fn rejects_wrong_schema_version() {
        for v in [0u32, 2, u32::MAX] {
            let mut m = base();
            m.schema_version = v;
            assert_eq!(validate(&m), Err(ValidationError::SchemaVersion { got: v }));
        }
    }

    // ---- nix version --------------------------------------------------------
    #[test]
    fn rejects_wrong_nix_version() {
        for v in ["2.34.9", "2.34", "", "2.34.8 "] {
            let mut m = base();
            m.nix.version = v.to_string();
            assert!(
                matches!(validate(&m), Err(ValidationError::NixVersion { .. })),
                "version {v:?}"
            );
        }
    }

    // ---- owner / repo safety ------------------------------------------------
    #[test]
    fn rejects_unsafe_owner() {
        let bads: Vec<String> = [
            "",
            "..",
            ".x",
            "x.",
            "Nix/../OS",
            "a/b",
            "a b",
            "a;b",
            "a|b",
            "a&b",
            "$HOME",
            "`x`",
            "a\nb",
            "a\tb",
            "a\0b",
            "(x)",
            "x~y",
            "x!y",
            "x#y",
            "x*y",
            "x?y",
        ]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once("x".repeat(129)))
        .collect();
        for bad in bads {
            let mut m = base();
            m.nixpkgs.owner = bad.clone();
            assert!(
                matches!(validate(&m), Err(ValidationError::OwnerUnsafe { .. })),
                "owner {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_repo() {
        for bad in ["a b", "../nixpkgs", "nixpkgs;rm -rf /", "$REPO", "a\nb"] {
            let mut m = base();
            m.nixpkgs.repo = bad.to_string();
            assert!(
                matches!(validate(&m), Err(ValidationError::RepoUnsafe { .. })),
                "repo {bad:?}"
            );
        }
    }

    // ---- rev ----------------------------------------------------------------
    #[test]
    fn rejects_malformed_rev() {
        let good = "a62e6edd6d5e1fa0329b8653c801147986f8d446";
        for bad in [
            good.to_uppercase(),
            good[..39].to_string(),
            format!("{good}0"),
            format!("{}g", &good[..39]),
            good.replace('a', "-"),
            format!("{good}\n"),
            format!("{good} "),
        ] {
            let mut m = base();
            m.nixpkgs.rev = bad.clone();
            assert!(
                matches!(validate(&m), Err(ValidationError::RevMalformed { .. })),
                "rev {bad:?}"
            );
        }
    }

    // ---- nar hash -----------------------------------------------------------
    #[test]
    fn rejects_nar_hash_wrong_prefix() {
        let mut m = base();
        m.nixpkgs.nar_hash = format!("sha512-{}", &m.nixpkgs.nar_hash["sha256-".len()..]);
        assert!(matches!(
            validate(&m),
            Err(ValidationError::NarHashMalformed { .. })
        ));
    }

    #[test]
    fn rejects_nar_hash_non_canonical() {
        // Last data char 'w' -> 'x' keeps the same 32 bytes but pollutes the
        // padding bits, so decode+encode equality fails (non-canonical base64).
        let mut m = base();
        m.nixpkgs.nar_hash = m.nixpkgs.nar_hash.replace("3Xuw=", "3Xux=");
        assert!(matches!(
            validate(&m),
            Err(ValidationError::NarHashMalformed { .. })
        ));
    }

    #[test]
    fn rejects_nar_hash_wrong_length() {
        let mut m = base();
        // "sha256-aGVsbG8sIHdvcmxkIQ==" decodes to 16 bytes, not 32.
        m.nixpkgs.nar_hash = "sha256-aGVsbG8sIHdvcmxkIQ==".to_string();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::NarHashMalformed { .. })
        ));
    }

    // ---- systems ------------------------------------------------------------
    #[test]
    fn rejects_wrong_systems() {
        {
            let mut m = base();
            m.systems = vec![
                "aarch64-linux".into(),
                "x86_64-linux".into(),
                "x86_64-darwin".into(),
                "aarch64-darwin".into(),
            ];
            assert!(matches!(
                validate(&m),
                Err(ValidationError::SystemsNotExact { .. })
            ));
        }
        {
            let mut m = base();
            m.systems = vec![
                "x86_64-linux".into(),
                "x86_64-darwin".into(),
                "aarch64-darwin".into(),
            ];
            assert!(matches!(
                validate(&m),
                Err(ValidationError::SystemsNotExact { .. })
            ));
        }
        {
            let mut m = base();
            m.systems.push("wasm32-wasi".into());
            assert!(matches!(
                validate(&m),
                Err(ValidationError::SystemsNotExact { .. })
            ));
        }
        {
            let mut m = base();
            m.systems[0] = "x86_64-linuxx".into();
            assert!(matches!(
                validate(&m),
                Err(ValidationError::SystemsNotExact { .. })
            ));
        }
    }

    // ---- attr ---------------------------------------------------------------
    #[test]
    fn attr_is_pinned_to_ripgrep() {
        // The pinned value is accepted verbatim.
        let mut m = base();
        m.attr = ATTR.to_string();
        assert!(validate(&m).is_ok());

        // Anything else — unsafe OR merely syntactically safe — is rejected as
        // an exact-equality mismatch (no trimming, case-sensitive, no suffix).
        for bad in [
            "ripgrep; rm -rf /",
            "ripgrep `x`",
            "ripgrep\n",
            "ripgrep $(x)",
            "../ripgrep",
            "rip grep",
            "$x",
            "a|b",
            "",
            ".x",
            "x.",
            // Syntactically safe but NOT the pinned attribute:
            "foo.bar-baz_3",
            "ripgrep ",
            " ripgrep",
            "RIpgrep",
            "ripgrep\x00",
        ] {
            let mut m = base();
            m.attr = bad.to_string();
            assert_eq!(
                validate(&m),
                Err(ValidationError::AttrMismatch {
                    got: bad.to_string()
                }),
                "attr {bad:?}"
            );
        }
    }

    // ---- sampling: warmup + both sample fields ------------------------------
    #[test]
    fn accepts_warmup_zero_and_max() {
        let mut m = base();
        m.sampling.warmup = 0;
        assert!(validate(&m).is_ok());
        m.sampling.warmup = MAX_WARMUP;
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rejects_warmup_out_of_range() {
        let mut m = base();
        m.sampling.warmup = MAX_WARMUP + 1;
        assert_eq!(
            validate(&m),
            Err(ValidationError::WarmupOutOfRange {
                got: MAX_WARMUP + 1
            })
        );
    }

    #[test]
    fn accepts_both_sample_fields_at_bounds() {
        // singleAttrSamples = MIN, indexSamples = MIN.
        {
            let mut m = base();
            m.sampling.single_attr_samples = MIN_SAMPLES;
            m.sampling.index_samples = MIN_SAMPLES;
            assert!(validate(&m).is_ok());
        }
        // singleAttrSamples = MAX, indexSamples = MAX.
        {
            let mut m = base();
            m.sampling.single_attr_samples = MAX_SAMPLES;
            m.sampling.index_samples = MAX_SAMPLES;
            assert!(validate(&m).is_ok());
        }
    }

    #[test]
    fn rejects_single_attr_samples_out_of_range_and_names_field() {
        {
            let mut m = base();
            m.sampling.single_attr_samples = 0;
            assert_eq!(
                validate(&m),
                Err(ValidationError::SamplesOutOfRange {
                    field: "singleAttrSamples",
                    got: 0
                })
            );
        }
        {
            let mut m = base();
            m.sampling.single_attr_samples = MAX_SAMPLES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::SamplesOutOfRange {
                    field: "singleAttrSamples",
                    got: MAX_SAMPLES + 1
                })
            );
        }
    }

    #[test]
    fn rejects_index_samples_out_of_range_and_names_field() {
        {
            let mut m = base();
            m.sampling.index_samples = 0;
            assert_eq!(
                validate(&m),
                Err(ValidationError::SamplesOutOfRange {
                    field: "indexSamples",
                    got: 0
                })
            );
        }
        {
            let mut m = base();
            m.sampling.index_samples = MAX_SAMPLES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::SamplesOutOfRange {
                    field: "indexSamples",
                    got: MAX_SAMPLES + 1
                })
            );
        }
    }

    // ---- caps: all three fields ---------------------------------------------
    #[test]
    fn accepts_caps_bounds() {
        let mut m = base();
        m.caps.single_attr_stdout_bytes = MIN_CAP_BYTES;
        m.caps.index_stdout_bytes = MIN_CAP_BYTES;
        m.caps.stderr_bytes = MIN_CAP_BYTES;
        assert!(validate(&m).is_ok());
        m.caps.single_attr_stdout_bytes = MAX_CAP_BYTES;
        m.caps.index_stdout_bytes = MAX_CAP_BYTES;
        m.caps.stderr_bytes = MAX_CAP_BYTES;
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rejects_each_cap_out_of_range_and_names_field() {
        // singleAttrStdoutBytes, low then high.
        {
            let mut m = base();
            m.caps.single_attr_stdout_bytes = MIN_CAP_BYTES - 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "singleAttrStdoutBytes",
                    got: MIN_CAP_BYTES - 1
                })
            );
        }
        {
            let mut m = base();
            m.caps.single_attr_stdout_bytes = MAX_CAP_BYTES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "singleAttrStdoutBytes",
                    got: MAX_CAP_BYTES + 1
                })
            );
        }
        // indexStdoutBytes, low then high.
        {
            let mut m = base();
            m.caps.index_stdout_bytes = MIN_CAP_BYTES - 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "indexStdoutBytes",
                    got: MIN_CAP_BYTES - 1
                })
            );
        }
        {
            let mut m = base();
            m.caps.index_stdout_bytes = MAX_CAP_BYTES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "indexStdoutBytes",
                    got: MAX_CAP_BYTES + 1
                })
            );
        }
        // stderrBytes, low then high.
        {
            let mut m = base();
            m.caps.stderr_bytes = MIN_CAP_BYTES - 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "stderrBytes",
                    got: MIN_CAP_BYTES - 1
                })
            );
        }
        {
            let mut m = base();
            m.caps.stderr_bytes = MAX_CAP_BYTES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::CapOutOfRange {
                    field: "stderrBytes",
                    got: MAX_CAP_BYTES + 1
                })
            );
        }
    }

    #[test]
    fn rejects_cap_zero() {
        // 0 is below the 1 KiB floor for every cap.
        for set in [
            |c: &mut manifest::Caps| c.single_attr_stdout_bytes = 0,
            |c: &mut manifest::Caps| c.index_stdout_bytes = 0,
            |c: &mut manifest::Caps| c.stderr_bytes = 0,
        ] {
            let mut m = base();
            set(&mut m.caps);
            assert!(matches!(
                validate(&m),
                Err(ValidationError::CapOutOfRange { .. })
            ));
        }
    }

    // ---- timeouts: ranges + overall>=command -------------------------------
    #[test]
    fn accepts_timeouts_at_per_command_and_overall_bounds() {
        // Both per-command at MIN and overall at MIN (overall >= each command).
        {
            let mut m = base();
            m.timeouts.single_attr_seconds = MIN_TIMEOUT_SECONDS;
            m.timeouts.index_seconds = MIN_TIMEOUT_SECONDS;
            m.timeouts.overall_seconds = MIN_TIMEOUT_SECONDS;
            assert!(validate(&m).is_ok());
        }
        // Both per-command at MAX_PER_COMMAND and overall at MAX_OVERALL.
        {
            let mut m = base();
            m.timeouts.single_attr_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS;
            m.timeouts.index_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS;
            m.timeouts.overall_seconds = MAX_OVERALL_TIMEOUT_SECONDS;
            assert!(validate(&m).is_ok());
        }
        // overall EXACTLY equal to a per-command budget is accepted (>=).
        {
            let mut m = base();
            m.timeouts.single_attr_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS;
            m.timeouts.index_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS;
            m.timeouts.overall_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS;
            assert!(validate(&m).is_ok());
        }
    }

    #[test]
    fn rejects_per_command_timeouts_out_of_range_and_names_field() {
        {
            let mut m = base();
            m.timeouts.single_attr_seconds = 0;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "singleAttrSeconds",
                    got: 0
                })
            );
        }
        {
            let mut m = base();
            m.timeouts.single_attr_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "singleAttrSeconds",
                    got: MAX_PER_COMMAND_TIMEOUT_SECONDS + 1
                })
            );
        }
        {
            let mut m = base();
            m.timeouts.index_seconds = 0;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "indexSeconds",
                    got: 0
                })
            );
        }
        {
            let mut m = base();
            m.timeouts.index_seconds = MAX_PER_COMMAND_TIMEOUT_SECONDS + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "indexSeconds",
                    got: MAX_PER_COMMAND_TIMEOUT_SECONDS + 1
                })
            );
        }
    }

    #[test]
    fn rejects_overall_timeout_out_of_range_and_names_field() {
        // overall range is checked before the cross-check, so an out-of-range
        // overall surfaces as TimeoutOutOfRange, not OverallTimeoutBelowCommand.
        {
            let mut m = base();
            m.timeouts.overall_seconds = 0;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "overallSeconds",
                    got: 0
                })
            );
        }
        {
            let mut m = base();
            m.timeouts.overall_seconds = MAX_OVERALL_TIMEOUT_SECONDS + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::TimeoutOutOfRange {
                    field: "overallSeconds",
                    got: MAX_OVERALL_TIMEOUT_SECONDS + 1
                })
            );
        }
    }

    #[test]
    fn rejects_overall_below_single_attr_timeout_and_names_field() {
        // Per-command values are in range; overall is in range but BELOW
        // singleAttrSeconds -> cross-check fails, naming singleAttrSeconds.
        let mut m = base();
        m.timeouts.single_attr_seconds = 300;
        m.timeouts.index_seconds = 100;
        m.timeouts.overall_seconds = 299;
        assert_eq!(
            validate(&m),
            Err(ValidationError::OverallTimeoutBelowCommand {
                command_field: "singleAttrSeconds",
                overall_seconds: 299,
                command_seconds: 300
            })
        );
    }

    #[test]
    fn rejects_overall_below_index_timeout_and_names_field() {
        let mut m = base();
        m.timeouts.single_attr_seconds = 100;
        m.timeouts.index_seconds = 600;
        m.timeouts.overall_seconds = 599;
        assert_eq!(
            validate(&m),
            Err(ValidationError::OverallTimeoutBelowCommand {
                command_field: "indexSeconds",
                overall_seconds: 599,
                command_seconds: 600
            })
        );
    }

    // ---- raw archive bytes: exact pin --------------------------------------
    #[test]
    fn accepts_pinned_raw_archive_bytes() {
        let m = base();
        assert_eq!(m.raw_archive.bytes, RAW_ARCHIVE_BYTES);
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rejects_raw_archive_bytes_off_by_one_both_directions() {
        {
            let mut m = base();
            m.raw_archive.bytes = RAW_ARCHIVE_BYTES - 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::RawArchiveBytesMismatch {
                    got: RAW_ARCHIVE_BYTES - 1
                })
            );
        }
        {
            let mut m = base();
            m.raw_archive.bytes = RAW_ARCHIVE_BYTES + 1;
            assert_eq!(
                validate(&m),
                Err(ValidationError::RawArchiveBytesMismatch {
                    got: RAW_ARCHIVE_BYTES + 1
                })
            );
        }
    }

    // ---- archive url --------------------------------------------------------
    #[test]
    fn rejects_archive_url_mismatch() {
        {
            let mut m = base();
            m.raw_archive.url = m.raw_archive.url.replace("a62e", "dead");
            assert!(matches!(
                validate(&m),
                Err(ValidationError::ArchiveUrlMismatch { .. })
            ));
        }
        {
            let mut m = base();
            m.raw_archive.url = m.raw_archive.url.replace("github.com", "evil.example");
            assert!(matches!(
                validate(&m),
                Err(ValidationError::ArchiveUrlMismatch { .. })
            ));
        }
        {
            let mut m = base();
            m.raw_archive.url = m.raw_archive.url.trim_end_matches(".tar.gz").to_string();
            assert!(matches!(
                validate(&m),
                Err(ValidationError::ArchiveUrlMismatch { .. })
            ));
        }
    }

    // ---- sha256 hex ---------------------------------------------------------
    #[test]
    fn rejects_malformed_hex() {
        let good = "ad7546baaf1b25f07225b3abac1dc8fbed8c4ecbd90dc4e8ec2e941d70f99ae1";
        for bad in [
            good.to_uppercase(),
            good[..63].to_string(),
            format!("{good}0"),
            format!("{}g", &good[..63]),
        ] {
            let mut m = base();
            m.raw_archive.sha256_hex = bad.clone();
            assert!(
                matches!(validate(&m), Err(ValidationError::HexMalformed { .. })),
                "hex {bad:?}"
            );
        }
    }

    // ---- raw sri ------------------------------------------------------------
    #[test]
    fn rejects_raw_sri_wrong_prefix() {
        let mut m = base();
        m.raw_archive.sha256_sri =
            format!("sha512-{}", &m.raw_archive.sha256_sri["sha256-".len()..]);
        assert!(matches!(
            validate(&m),
            Err(ValidationError::RawSriMalformed { .. })
        ));
    }

    #[test]
    fn rejects_raw_sri_non_canonical() {
        // Last data char 'E' -> 'F' pollutes padding bits: same 32 bytes, but
        // the canonical re-encode differs (non-canonical base64).
        let mut m = base();
        m.raw_archive.sha256_sri = m.raw_archive.sha256_sri.replace("5muE=", "5muF=");
        assert!(matches!(
            validate(&m),
            Err(ValidationError::RawSriMalformed { .. })
        ));
    }

    // ---- digest cross-checks / mismatches -----------------------------------
    #[test]
    fn rejects_raw_digest_mismatch() {
        let mut m = base();
        // Flip every '1' to '0': still valid 64 lowercase hex, but no longer the
        // same digest as sha256_sri.
        m.raw_archive.sha256_hex = m.raw_archive.sha256_hex.replace('1', "0");
        assert!(matches!(
            validate(&m),
            Err(ValidationError::RawDigestMismatch)
        ));
    }

    #[test]
    fn rejects_nar_digest_not_differing() {
        let mut m = base();
        // Conflating the two hash domains: set narHash == raw SRI.
        m.nixpkgs.nar_hash = m.raw_archive.sha256_sri.clone();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::NarDigestDoesNotDiffer)
        ));
    }

    // ---- bounded display ----------------------------------------------------
    #[test]
    fn display_is_bounded_for_huge_input() {
        let mut m = base();
        m.nix.version = "x".repeat(10_000);
        let s = validate(&m).unwrap_err().to_string();
        assert!(
            s.len() < 256,
            "display must be bounded, was {} bytes: {s:?}",
            s.len()
        );
    }

    #[test]
    fn sample_and_timeout_errors_name_the_field() {
        // Display output must contain the exact JSON key for field clarity.
        let mut m = base();
        m.sampling.index_samples = 0;
        assert!(
            validate(&m)
                .unwrap_err()
                .to_string()
                .contains("sampling.indexSamples"),
            "must name indexSamples"
        );

        let mut m = base();
        m.timeouts.overall_seconds = 0;
        assert!(
            validate(&m)
                .unwrap_err()
                .to_string()
                .contains("timeouts.overallSeconds"),
            "must name overallSeconds"
        );

        let mut m = base();
        m.timeouts.overall_seconds = 599;
        m.timeouts.index_seconds = 600;
        let s = validate(&m).unwrap_err().to_string();
        assert!(s.contains("timeouts.overallSeconds"), "{s}");
        assert!(s.contains("timeouts.indexSeconds"), "{s}");
    }

    #[test]
    fn bound_snippet_short_passthrough() {
        assert_eq!(bound_snippet("abc"), "abc");
    }

    #[test]
    fn bound_snippet_truncates() {
        let s = bound_snippet(&"a".repeat(200));
        assert!(s.len() <= DISPLAY_SNIPPET_MAX + 3);
        assert!(s.ends_with("..."));
    }

    // ---- unit helpers -------------------------------------------------------
    #[test]
    fn safe_identifier_rules() {
        assert!(is_safe_identifier("NixOS"));
        assert!(is_safe_identifier("nixpkgs"));
        assert!(is_safe_identifier("ripgrep"));
        assert!(is_safe_identifier("foo.bar-baz_3"));
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier(".."));
        assert!(!is_safe_identifier(".x"));
        assert!(!is_safe_identifier("x."));
        assert!(!is_safe_identifier("a..b"));
        assert!(!is_safe_identifier("a/b"));
        assert!(!is_safe_identifier("a b"));
        assert!(!is_safe_identifier("a;b"));
        assert!(!is_safe_identifier(&"x".repeat(129)));
    }

    #[test]
    fn hex_helpers() {
        assert!(is_40_lowercase_hex(
            "a62e6edd6d5e1fa0329b8653c801147986f8d446"
        ));
        assert!(!is_40_lowercase_hex(
            "A62E6EDD6D5E1FA0329B8653C801147986F8D446"
        ));
        assert_eq!(
            decode_hex32("ad7546baaf1b25f07225b3abac1dc8fbed8c4ecbd90dc4e8ec2e941d70f99ae1")
                .unwrap()
                .len(),
            32
        );
        assert!(decode_hex32("ABC").is_none());
        assert!(decode_hex32("ad7546").is_none());
    }
}
