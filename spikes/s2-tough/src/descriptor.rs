// The `pkg` channel descriptor schema (policy metadata), mirroring
// `plans/02-trust-and-update-model.md` §7 — the CANONICAL schema.
//
// IMPORTANT BOUNDARY: a `descriptor.json` is itself a TUF *target*. `tough`
// supplies the CRYPTOGRAPHIC / TUF guarantees for it (authentication, integrity,
// rollback, freeze, mix-and-match, threshold). The *semantic* policy fields
// below — `schemaVersion`, `policyVersion`, `sequence`, `expiresAt`,
// `supportedSystems`, `buildPolicy`, the `substituters`/`trustedPublicKeys`
// allowlists, and the cross-checks between the hashes recorded here and the TUF
// target hashes — are PRODUCT-semantic validation that PR-11 still must
// implement. This spike does not implement those checks; it only proves that
// tough can authenticate and deliver the bytes, and that the descriptor
// serializes to the EXACT canonical shape in plans/02 §7.

use serde::{Deserialize, Serialize};

/// The four pkg-supported Nix systems (`plans/00` D-14). Every system-specific
/// map in the canonical descriptor carries exactly these four keys.
pub const SUPPORTED_SYSTEMS: [&str; 4] = [
    "x86_64-linux",
    "aarch64-linux",
    "x86_64-darwin",
    "aarch64-darwin",
];

/// The well-known, single v1 binary-cache substituter and its public key
/// (`plans/02` §6.5, DR-006). These are PUBLIC values pinned in the descriptor.
pub const CACHE_NIXOS_ORG_URL: &str = "https://cache.nixos.org";
pub const CACHE_NIXOS_ORG_KEY: &str =
    "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

/// The bundled Nix runtime version this descriptor ships (`plans/02` §7
/// `nixRuntime.version`).
pub const NIX_RUNTIME_VERSION: &str = "2.24.10";

/// A per-system runtime entry carrying the values declared in policy
/// (`nixRuntime.perSystem[system]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemEntry {
    pub url: String,
    pub sha256: String,
}

/// A per-system index entry: the TUF target name plus its declared sha256
/// (`index.perSystem[system]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexEntry {
    pub target: String,
    pub sha256: String,
}

/// The managed-Nix runtime block (`nixRuntime`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NixRuntime {
    pub version: String,
    #[serde(rename = "perSystem")]
    pub per_system: std::collections::BTreeMap<String, SystemEntry>,
}

/// The pinned Nixpkgs source block (`nixpkgs`). `sourceTarget` is the TUF
/// target name of the source tarball; it MUST serialize camelCase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Nixpkgs {
    pub owner: String,
    pub repo: String,
    pub rev: String,
    #[serde(rename = "narHash")]
    pub nar_hash: String,
    #[serde(rename = "sourceTarget")]
    pub source_target: String,
}

/// The disposable-index block (`plans/03`, DR-010 — not a trust root).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Index {
    pub source: String,
    #[serde(rename = "perSystem")]
    pub per_system: std::collections::BTreeMap<String, IndexEntry>,
}

/// The pinned substituter block (D-10, DR-006). `trustedPublicKeys` MUST
/// serialize camelCase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Substituters {
    pub urls: Vec<String>,
    #[serde(rename = "trustedPublicKeys")]
    pub trusted_public_keys: Vec<String>,
}

/// A local-build mode for a host system (`buildPolicy.nativeLocalBuilds[system].mode`).
///
/// Implements D-11. The v1 mode for all four native systems is
/// `AllowWithGates` (`"allow-with-gates"`). A system with no entry is
/// implicitly `Deny`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuildMode {
    /// `"allow-with-gates"`: substitution first; on a cache miss a build is
    /// permitted only after a deterministic preview + explicit single-operation
    /// approval AND verified `sandbox=true`/`sandbox-fallback=false` + build-user
    /// readiness + resource limits. The v1 mode for all four native systems.
    AllowWithGates,
    /// `"prompt"`: preview + explicit single-operation approval required.
    Prompt,
    /// `"deny"`: substitution only; an unresolvable cache miss is
    /// `ACQUIRE_NO_BINARY`.
    Deny,
}

/// The per-system native-build entry (`buildPolicy.nativeLocalBuilds[system]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeBuildEntry {
    pub mode: BuildMode,
}

/// The build-policy block (`buildPolicy`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildPolicy {
    #[serde(rename = "nativeLocalBuilds")]
    pub native_local_builds: std::collections::BTreeMap<String, NativeBuildEntry>,
}

impl BuildPolicy {
    /// The canonical v1 policy: every one of the four supported systems is
    /// `allow-with-gates` (plans/02 §7, D-11).
    pub fn canonical_v1() -> Self {
        let mut native_local_builds = std::collections::BTreeMap::new();
        for sys in SUPPORTED_SYSTEMS {
            native_local_builds.insert(
                sys.to_string(),
                NativeBuildEntry {
                    mode: BuildMode::AllowWithGates,
                },
            );
        }
        Self {
            native_local_builds,
        }
    }
}

/// The canonical channel descriptor. See `plans/02` §7 for field semantics.
///
/// `policyVersion`/`sequence` MUST be monotonic (TRU-INV-03) and `expiresAt`
/// MUST be honored for new installs (TRU-INV-04) — these are PR-11 product
/// checks, NOT tough responsibilities.
///
/// Field declaration order matches the canonical JSON in plans/02 §7 so
/// `to_json_bytes` emits the documented shape byte-for-byte (modulo the `…`
/// hash placeholders).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelDescriptor {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub channel: String,
    #[serde(rename = "policyVersion")]
    pub policy_version: u64,
    pub sequence: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
    #[serde(rename = "supportedSystems")]
    pub supported_systems: Vec<String>,
    #[serde(rename = "buildPolicy")]
    pub build_policy: BuildPolicy,
    #[serde(rename = "nixRuntime")]
    pub nix_runtime: NixRuntime,
    pub nixpkgs: Nixpkgs,
    pub index: Index,
    pub substituters: Substituters,
}

impl ChannelDescriptor {
    /// Serialize to pretty JSON bytes (the exact bytes placed in the repo as a
    /// TUF target, so its recorded TUF hash matches).
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("serialize descriptor")
    }

    /// The canonical plans/02 §7 sample descriptor, with all four systems
    /// populated and dummy-but-well-formed sha256 values. Used by the strict
    /// shape test and as a starting point for fixtures.
    ///
    /// The sha256/narHash/url values here are NOT real artifacts; they exist
    /// only so the serialized shape is fully exercised. The fixture builder
    /// (`src/fixture.rs`) replaces them with hashes computed from real target
    /// bytes so the descriptor's declared hashes match the TUF-authenticated
    /// ones (defense in depth, plans/02 §11).
    pub fn sample() -> Self {
        let dummy_sha = "0".repeat(64);
        let seq = 42u64;
        let mut nix_per_system = std::collections::BTreeMap::new();
        let mut index_per_system = std::collections::BTreeMap::new();
        for sys in SUPPORTED_SYSTEMS {
            nix_per_system.insert(
                sys.to_string(),
                SystemEntry {
                    url: format!(
                        "https://releases.nixos.org/nix/nix-{ver}/nix-{ver}-{sys}.tar.xz",
                        ver = NIX_RUNTIME_VERSION,
                    ),
                    sha256: dummy_sha.clone(),
                },
            );
            index_per_system.insert(
                sys.to_string(),
                IndexEntry {
                    target: format!("index/{seq}/{sys}.json.br"),
                    sha256: dummy_sha.clone(),
                },
            );
        }
        Self {
            schema_version: 1,
            channel: "pkg-stable-1".to_string(),
            policy_version: 1,
            sequence: seq,
            expires_at: "2025-04-01T00:00:00Z".to_string(),
            supported_systems: SUPPORTED_SYSTEMS.iter().map(|s| (*s).to_string()).collect(),
            build_policy: BuildPolicy::canonical_v1(),
            nix_runtime: NixRuntime {
                version: NIX_RUNTIME_VERSION.to_string(),
                per_system: nix_per_system,
            },
            nixpkgs: Nixpkgs {
                owner: "NixOS".to_string(),
                repo: "nixpkgs".to_string(),
                rev: "abc123".to_string(),
                nar_hash: format!("sha256-{dummy_sha}"),
                source_target: "nixpkgs/abc123/src.tar.gz".to_string(),
            },
            index: Index {
                source: "self-built".to_string(),
                per_system: index_per_system,
            },
            substituters: Substituters {
                urls: vec![CACHE_NIXOS_ORG_URL.to_string()],
                trusted_public_keys: vec![CACHE_NIXOS_ORG_KEY.to_string()],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    //! STRICT serialization guards for the canonical plans/02 §7 descriptor.
    //!
    //! These exist so snake_case drift (a missing `#[serde(rename = ...)]`)
    //! can never silently change the on-the-wire schema. They assert BOTH the
    //! required camelCase key paths AND the absence of every snake_case spell.

    use super::*;
    use serde_json::Value;

    fn serialized() -> Value {
        let bytes = ChannelDescriptor::sample().to_json_bytes();
        serde_json::from_slice(&bytes).expect("descriptor serializes to valid JSON")
    }

    /// The exact canonical top-level keys, in the plans/02 §7 order.
    const CANONICAL_TOP_KEYS: &[&str] = &[
        "schemaVersion",
        "channel",
        "policyVersion",
        "sequence",
        "expiresAt",
        "supportedSystems",
        "buildPolicy",
        "nixRuntime",
        "nixpkgs",
        "index",
        "substituters",
    ];

    #[test]
    fn top_level_keys_are_exactly_canonical_and_ordered() {
        let bytes = ChannelDescriptor::sample().to_json_bytes();
        let s = String::from_utf8(bytes.clone()).unwrap();

        // Exact SET: `serde_json::Value` sorts object keys, so compare the
        // parsed (sorted) key set to the sorted canonical set. This catches a
        // missing or misspelled `#[serde(rename = ...)]`.
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let mut got: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        got.sort();
        let mut want: Vec<&str> = CANONICAL_TOP_KEYS.to_vec();
        want.sort_unstable();
        assert_eq!(
            got.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            want,
            "descriptor top-level key SET must be exactly the canonical plans/02 §7 set"
        );

        // ORDER: struct field order == canonical order. Verify in the RAW bytes
        // (Value loses order) by scanning left-to-right with an advancing cursor.
        let mut cursor = 0usize;
        let mut prev: usize = 0;
        for (i, key) in CANONICAL_TOP_KEYS.iter().enumerate() {
            let needle = format!("\"{key}\":");
            let pos = s[cursor..]
                .find(&needle)
                .map(|p| cursor + p)
                .unwrap_or_else(|| panic!("canonical key `{key}` missing from descriptor bytes"));
            if i > 0 {
                assert!(
                    prev < pos,
                    "descriptor key `{key}` is out of the canonical plans/02 §7 order"
                );
            }
            prev = pos;
            cursor = pos + needle.len();
        }
    }

    #[test]
    fn build_policy_native_local_builds_covers_all_four_systems() {
        let v = serialized();
        let modes = &v["buildPolicy"]["nativeLocalBuilds"];
        let modes_obj = modes.as_object().expect("nativeLocalBuilds is an object");
        let mut seen: Vec<&str> = modes_obj.keys().map(String::as_str).collect();
        seen.sort();
        assert_eq!(
            seen,
            &[
                "aarch64-darwin",
                "aarch64-linux",
                "x86_64-darwin",
                "x86_64-linux"
            ],
            "nativeLocalBuilds must cover exactly the four supported systems"
        );
        for sys in SUPPORTED_SYSTEMS {
            assert_eq!(
                modes[sys]["mode"], "allow-with-gates",
                "v1 mode for {sys} must be allow-with-gates"
            );
        }
    }

    #[test]
    fn camel_case_required_paths_exist() {
        let v = serialized();
        // buildPolicy.nativeLocalBuilds.<sys>.mode
        assert_eq!(
            v["buildPolicy"]["nativeLocalBuilds"]["x86_64-linux"]["mode"],
            "allow-with-gates"
        );
        // nixRuntime.perSystem.<sys>.{url,sha256}
        assert!(v["nixRuntime"]["perSystem"]["x86_64-linux"]["url"].is_string());
        assert!(v["nixRuntime"]["perSystem"]["x86_64-linux"]["sha256"].is_string());
        assert_eq!(v["nixRuntime"]["version"], NIX_RUNTIME_VERSION);
        // nixpkgs.sourceTarget (camelCase) + narHash
        assert!(v["nixpkgs"]["sourceTarget"].is_string());
        assert!(v["nixpkgs"]["narHash"].is_string());
        // index.perSystem.<sys>.{target,sha256}
        assert!(v["index"]["perSystem"]["aarch64-linux"]["target"].is_string());
        assert!(v["index"]["perSystem"]["aarch64-linux"]["sha256"].is_string());
        // substituters.{urls,trustedPublicKeys}
        assert_eq!(v["substituters"]["urls"][0], CACHE_NIXOS_ORG_URL);
        assert_eq!(
            v["substituters"]["trustedPublicKeys"][0],
            CACHE_NIXOS_ORG_KEY
        );
    }

    /// The regression guard: NONE of the snake_case field names may appear as a
    /// key in the serialized JSON. If any `#[serde(rename)]` is dropped, the
    /// snake_case Rust name would leak and this test fails.
    #[test]
    fn no_snake_case_drift_in_serialized_bytes() {
        let bytes = String::from_utf8(ChannelDescriptor::sample().to_json_bytes()).unwrap();
        let forbidden_keys = [
            "schema_version",
            "policy_version",
            "supported_systems",
            "build_policy",
            "native_local_builds",
            "nix_runtime",
            "per_system",
            "nar_hash",
            "source_target",
            "trusted_public_keys",
        ];
        for key in forbidden_keys {
            // Match the key as a JSON object key: `"key"`.
            let needle = format!("\"{key}\"");
            assert!(
                !bytes.contains(&needle),
                "snake_case drift: serialized descriptor contains forbidden key `{key}` \
                 (missing #[serde(rename = \"...\")])"
            );
        }
    }

    /// Round-trip: the canonical sample parses back to an equal descriptor.
    #[test]
    fn round_trip_sample_is_equal() {
        let original = ChannelDescriptor::sample();
        let bytes = original.to_json_bytes();
        let parsed: ChannelDescriptor =
            serde_json::from_slice(&bytes).expect("deserialize descriptor");
        assert_eq!(original, parsed);
    }
}
