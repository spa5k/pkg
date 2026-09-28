//! Decoding of captured native output shapes.
//!
//! Every type here is a pure transcription of a verified native JSON shape
//! (Determinate 3.22.1, Nix 2.35.2, aarch64-darwin). Required identity
//! fields must be present or decoding fails clearly (design D3); unknown
//! optional fields are ignored.

use std::collections::BTreeMap;

use super::TESTED_BASELINE;
use super::reference::locked_revision;

/// The `nix profile list --json` manifest version this client decodes.
///
/// Verified value on Nix 2.35.2 is `3`; entries changed to `elements` with
/// string `attrPath` and locked `url` fields in that version.
pub const PROFILE_MANIFEST_VERSION: u64 = 3;

/// The cask catalog index schema this client decodes (design D6).
///
/// The index is the generated `pkg-cask-catalog/2` envelope exposed by the
/// casks flake as `catalogIndex`. It carries provenance, the target system
/// list, and per-system status entries without build plans.
pub const CATALOG_INDEX_SCHEMA: &str = "pkg-cask-catalog/2";

/// The generator provenance recorded in a catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CatalogGenerator {
    /// The generator name, for example `cask-catalog`.
    pub name: String,
    /// The generator version.
    pub version: String,
}

/// The pinned input provenance recorded in a catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CatalogInput {
    /// The pinned source URL the catalog was generated from.
    pub url: String,
    /// The exact pinned revision.
    pub revision: String,
    /// The SHA-256 of the pinned input.
    pub sha256: String,
    /// The upstream data license attribution.
    pub license: String,
}

/// Whether one index entry is installable on its system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    /// The entry has a generated plan; a metadata claim, not a verified
    /// build.
    Eligible,
    /// The entry is excluded with a recorded reason.
    Excluded,
}

/// One per-system status entry of the catalog index.
///
/// Null rules (validated in [`decode_catalog_index`]): `name`,
/// `description`, `version`, and `homepage` are string or null; an eligible
/// entry has a non-null `kind` and null `reason`/`detail`; an excluded entry
/// has a null `kind`, a non-null `reason`, and a string-or-null `detail`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CatalogEntry {
    /// The cask token; must equal the map key it sits under.
    pub token: String,
    /// The upstream name, when the metadata provides one.
    pub name: Option<String>,
    /// The upstream description, when the metadata provides one.
    pub description: Option<String>,
    /// The effective version for this system, when known.
    pub version: Option<String>,
    /// The effective homepage for this system, when known.
    pub homepage: Option<String>,
    /// Whether the token is eligible or excluded on this system.
    pub status: EntryStatus,
    /// The package kind, for example `app+cli`; present when eligible.
    pub kind: Option<String>,
    /// The machine-readable exclusion reason; present when excluded.
    pub reason: Option<String>,
    /// Human-readable exclusion detail, when present.
    #[serde(default)]
    pub detail: Option<String>,
}

/// One system section of the catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CatalogSystem {
    /// The status entries for this system, keyed by token.
    pub entries: BTreeMap<String, CatalogEntry>,
}

/// The decoded `catalogIndex` envelope of the casks flake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIndex {
    /// The generator provenance.
    pub generator: CatalogGenerator,
    /// The pinned input provenance.
    pub input: CatalogInput,
    /// The target systems the catalog was generated for.
    pub targets: Vec<String>,
    /// The declared macOS baseline, for example `15.7.7`.
    pub macos_baseline: String,
    /// The per-system status sections.
    pub systems: BTreeMap<String, CatalogSystem>,
}

/// One installed profile element decoded from `nix profile list --json`.
///
/// Captured shape on Nix 2.35.2: the manifest is version `3`, the top level
/// object holds `elements`, and each element carries `active`, string
/// `attrPath`, `originalUrl`, `outputs` (nullable), `priority`, `storePaths`,
/// and the locked `url`. Element keys are opaque native entry IDs and differ
/// by platform: Linux uses plain source keys while Determinate macOS uses
/// full `originalUrl#attrPath` strings such as
/// `path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default`.
/// Keys must be passed back to remove and upgrade verbatim.
///
/// Unknown optional JSON fields are ignored; required identity fields must be
/// present or decoding fails clearly (design D3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ProfileEntry {
    /// Whether the element is active in the profile.
    ///
    /// Discovery and app-launcher scans must only consider active entries.
    pub active: bool,
    /// The attribute path of the installed output (a dot-separated string).
    #[serde(rename = "attrPath")]
    pub attr_path: String,
    /// The original (moving) source reference.
    #[serde(rename = "originalUrl")]
    pub original_url: String,
    /// The locked source reference Nix resolved.
    #[serde(rename = "url")]
    pub locked_url: String,
    /// The realized store output paths.
    #[serde(rename = "storePaths")]
    pub store_paths: Vec<String>,
}

impl ProfileEntry {
    /// The final attribute path segment, used for display matching.
    #[must_use]
    pub fn name(&self) -> &str {
        self.attr_path.rsplit('.').next().unwrap_or("?")
    }

    /// The locked revision embedded in the locked URL, if any.
    #[must_use]
    pub fn locked_revision(&self) -> Option<&str> {
        locked_revision(&self.locked_url)
    }
}

/// One search result from `nix search --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchMeta {
    /// The package name.
    pub pname: String,
    /// The package version.
    pub version: String,
    /// The package description.
    #[serde(default)]
    pub description: String,
}

/// Resolved source identity from `nix flake metadata --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// The moving reference as configured.
    pub reference: String,
    /// The locked reference Nix resolved, when metadata provides one.
    ///
    /// This is the metadata's top-level `url`, which keeps the transport
    /// and parameters, not the nested `locked.url`, which drops them.
    /// Discovery queries this reference so a result set can never mix
    /// revisions; installation keeps using the moving reference.
    pub locked_url: Option<String>,
    /// The locked revision, when the source provides one.
    pub revision: Option<String>,
    /// A short human description such as `github:NixOS/nixpkgs`.
    pub display: String,
}

/// Decode one captured `nix profile list --json` document.
///
/// The manifest `version` field is required and must equal
/// [`PROFILE_MANIFEST_VERSION`]: the supported native runtime always
/// reports it, so an absent or different version is an unsupported shape,
/// not an invented version floor.
pub(super) fn decode_profile_manifest(
    raw: &serde_json::Value,
) -> Result<BTreeMap<String, ProfileEntry>, super::NixError> {
    let Some(version) = raw.get("version").and_then(serde_json::Value::as_u64) else {
        return Err(super::NixError::Decode {
            what: "profile identity",
            detail: format!(
                "`nix profile list --json` reports no manifest version; \
                 version {PROFILE_MANIFEST_VERSION} is required \
                 (tested on Nix {TESTED_BASELINE})"
            ),
        });
    };
    if version != PROFILE_MANIFEST_VERSION {
        return Err(super::NixError::Decode {
            what: "profile identity",
            detail: format!(
                "profile manifest version {version} is not the decoded version \
                 {PROFILE_MANIFEST_VERSION} (tested on Nix {TESTED_BASELINE})"
            ),
        });
    }
    let Some(elements) = raw.get("elements") else {
        return Err(super::NixError::Decode {
            what: "profile identity",
            detail: String::from(
                "`elements` is absent from `nix profile list --json` \
                 (expected manifest version 3)",
            ),
        });
    };
    serde_json::from_value(elements.clone()).map_err(|error| super::NixError::Decode {
        what: "profile identity",
        detail: error.to_string(),
    })
}

/// Decode one captured `nix flake metadata --json` value.
///
/// The canonical locked URL is the top-level `url` Nix reports for the
/// reference: for a moving `git+file` reference it keeps the transport and
/// the `ref`/`rev` parameters, while the nested `locked.url` drops them
/// and is not a usable query reference. The nested value is only a
/// fallback for shapes without a top-level `url`.
pub(super) fn decode_source_identity(reference: &str, value: &serde_json::Value) -> SourceIdentity {
    let locked = value
        .get("locked")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let revision = locked
        .get("rev")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let locked_url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .or_else(|| locked.get("url").and_then(serde_json::Value::as_str))
        .map(ToString::to_string);
    let display = [
        locked.get("type").and_then(serde_json::Value::as_str),
        locked.get("owner").and_then(serde_json::Value::as_str),
        locked.get("repo").and_then(serde_json::Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(":");
    SourceIdentity {
        reference: reference.to_string(),
        locked_url,
        revision,
        display: if display.is_empty() {
            reference.to_string()
        } else {
            display
        },
    }
}

/// Decode one captured `catalogIndex` envelope.
///
/// The schema gate is strict: an envelope whose `schema` is absent or is
/// not [`CATALOG_INDEX_SCHEMA`] fails naming the schema found and the
/// schema supported, so an old client meeting a new catalog (or the
/// reverse) is a clear error instead of a misread. Required provenance
/// fields and the `systems` map must be present, and every target system
/// must have exactly one system section.
///
/// The envelope is generated protocol data, so a record that breaks the
/// entry null rules or the token identifier rule fails the decode as a
/// whole with the token named. Malformed raw Homebrew records are already
/// isolated by the generator as excluded entries; the client keeps no
/// second classification or salvage layer.
pub fn decode_catalog_index(raw: &serde_json::Value) -> Result<CatalogIndex, super::NixError> {
    let gate = |detail: String| super::NixError::Decode {
        what: "cask catalog index",
        detail,
    };
    let found = raw.get("schema").and_then(serde_json::Value::as_str);
    let Some(schema) = found else {
        return Err(gate(format!(
            "the index reports no schema; schema {CATALOG_INDEX_SCHEMA:?} is required"
        )));
    };
    if schema != CATALOG_INDEX_SCHEMA {
        return Err(gate(format!(
            "index schema {schema:?} is not the decoded schema {CATALOG_INDEX_SCHEMA:?}"
        )));
    }
    let generator: CatalogGenerator = serde_json::from_value(
        raw.get("generator")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|error| gate(format!("generator provenance is invalid: {error}")))?;
    let input: CatalogInput =
        serde_json::from_value(raw.get("input").cloned().unwrap_or(serde_json::Value::Null))
            .map_err(|error| gate(format!("input provenance is invalid: {error}")))?;
    let Some(targets) = raw.get("targets").and_then(serde_json::Value::as_array) else {
        return Err(gate(String::from(
            "the `targets` system list is absent; it must be read before any system data",
        )));
    };
    let targets: Vec<String> = targets
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| gate(String::from("`targets` must be a list of system strings")))
        })
        .collect::<Result<_, _>>()?;
    let Some(macos_baseline) = raw.get("macosBaseline").and_then(serde_json::Value::as_str) else {
        return Err(gate(String::from(
            "the declared `macosBaseline` string is absent",
        )));
    };
    let Some(systems_raw) = raw.get("systems").and_then(serde_json::Value::as_object) else {
        return Err(gate(String::from(
            "the `systems` map is absent from the index",
        )));
    };
    for target in &targets {
        if !systems_raw.contains_key(target) {
            return Err(gate(format!(
                "target system {target:?} has no `systems` section; \
             the envelope is inconsistent"
            )));
        }
    }
    for system in systems_raw.keys() {
        if !targets.contains(system) {
            return Err(gate(format!(
                "`systems` carries {system:?} which is not a declared target"
            )));
        }
    }

    let mut systems = BTreeMap::new();
    for (system, section) in systems_raw {
        let entries_raw = section
            .get("entries")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| gate(format!("system {system:?} has no `entries` map")))?;
        let mut entries = BTreeMap::new();
        for (token, record) in entries_raw {
            let entry = validate_entry(token, record)
                .map_err(|detail| gate(format!("{system}/{token}: {detail}")))?;
            entries.insert(token.clone(), entry);
        }
        systems.insert(system.clone(), CatalogSystem { entries });
    }
    Ok(CatalogIndex {
        generator,
        input,
        targets,
        macos_baseline: macos_baseline.to_string(),
        systems,
    })
}

/// Validate one index entry record against the token and null rules.
fn validate_entry(token: &str, record: &serde_json::Value) -> Result<CatalogEntry, String> {
    let entry: CatalogEntry = serde_json::from_value(record.clone())
        .map_err(|error| format!("does not follow the schema: {error}"))?;
    if !valid_catalog_token(token) {
        return Err(String::from(
            "the token is not a valid catalog token identifier",
        ));
    }
    if entry.token != token {
        return Err(format!(
            "record token {:?} does not match its key {token:?}",
            entry.token
        ));
    }
    match entry.status {
        EntryStatus::Eligible => {
            if entry.kind.is_none() {
                return Err(String::from(
                    "an eligible entry must carry a non-null `kind`",
                ));
            }
            if entry.reason.is_some() {
                return Err(String::from("an eligible entry must carry a null `reason`"));
            }
            if entry.detail.is_some() {
                return Err(String::from("an eligible entry must carry a null `detail`"));
            }
        }
        EntryStatus::Excluded => {
            if entry.kind.is_some() {
                return Err(String::from("an excluded entry must carry a null `kind`"));
            }
            if entry.reason.is_none() {
                return Err(String::from(
                    "an excluded entry must carry a non-null `reason`",
                ));
            }
        }
    }
    Ok(entry)
}

/// Whether `token` is a valid catalog token identifier.
///
/// Same rule as the generator: `^[a-z0-9][a-z0-9+._@-]*$` without `..`
/// path components. `@` and `+` are real Homebrew token characters
/// (versioned tokens such as `1password-cli@beta`, `xournal++`), so they
/// must parse and resolve, and a token can never shape the attribute path
/// it is installed through.
#[must_use]
pub fn valid_catalog_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut previous_dot = false;
    for (index, c) in token.chars().enumerate() {
        let allowed = c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '+' | '.' | '_' | '-' | '@');
        if index == 0 && !(c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return false;
        }
        if !allowed || (previous_dot && c == '.') {
            return false;
        }
        previous_dot = c == '.';
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::reference::is_fixed_reference;
    use super::*;

    /// A full commit id used by the manifest shape tests below.
    const COMMIT: &str = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";

    /// Captured manifest shape from Determinate 3.22.1 / Nix 2.35.2: version 3,
    /// top-level `elements`, string `attrPath`, locked `url`, and map keys that
    /// are full `originalUrl#attrPath` strings.
    #[test]
    fn decodes_captured_profile_manifest_and_rejects_other_shapes() {
        // Linux shape uses plain source keys; both are decoded opaquely.
        let linux_captured = r#"{
          "version": 3,
          "elements": {
            "github:NixOS/nixpkgs/nixpkgs-unstable": {
              "active": true,
              "attrPath": "legacyPackages.x86_64-linux.hello",
              "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
              "outputs": null,
              "priority": 5,
              "storePaths": ["/nix/store/Hash-hello-2.12.1"],
              "url": "github:NixOS/nixpkgs/0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"
            }
          }
        }"#;
        let linux: serde_json::Value = serde_json::from_str(linux_captured).expect("valid json");
        let linux_entries: BTreeMap<String, ProfileEntry> =
            serde_json::from_value(linux["elements"].clone()).expect("decodes linux keys");
        let linux_entry = &linux_entries["github:NixOS/nixpkgs/nixpkgs-unstable"];
        assert_eq!(linux_entry.name(), "hello");
        assert_eq!(linux_entry.locked_revision(), Some(COMMIT));

        let captured = format!(
            r#"{{
              "version": 3,
              "elements": {{
                "path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default": {{
                  "active": true,
                  "attrPath": "packages.aarch64-darwin.default",
                  "originalUrl": "path:/tmp/pkg-native-proof.UUZyyl",
                  "outputs": null,
                  "priority": 5,
                  "storePaths": ["/nix/store/Hash-default-1.0"],
                  "url": "path:/tmp/pkg-native-proof.UUZyyl"
                }},
                "github:NixOS/nixpkgs/nixpkgs-unstable#legacyPackages.aarch64-darwin.ripgrep": {{
                  "active": true,
                  "attrPath": "legacyPackages.aarch64-darwin.ripgrep",
                  "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
                  "outputs": null,
                  "priority": 5,
                  "storePaths": ["/nix/store/Hash-ripgrep-14.1.0"],
                  "url": "github:NixOS/nixpkgs/{COMMIT}"
                }}
              }}
            }}"#
        );
        let raw: serde_json::Value = serde_json::from_str(&captured).expect("valid json");
        let elements = raw.get("elements").expect("elements present");
        let entries: BTreeMap<String, ProfileEntry> =
            serde_json::from_value(elements.clone()).expect("decodes captured shape");
        let path_entry =
            &entries["path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default"];
        assert_eq!(path_entry.name(), "default");
        assert_eq!(path_entry.locked_revision(), None);
        assert!(!is_fixed_reference(path_entry.original_url.as_str()));
        let ripgrep =
            &entries["github:NixOS/nixpkgs/nixpkgs-unstable#legacyPackages.aarch64-darwin.ripgrep"];
        assert_eq!(ripgrep.name(), "ripgrep");
        assert_eq!(ripgrep.locked_revision(), Some(COMMIT));

        // Missing required identity fields must fail, not default silently.
        let missing = r#"{"version": 3, "elements": {"ripgrep": {"attrPath": "ripgrep"}}}"#;
        let value: serde_json::Value = serde_json::from_str(missing).expect("valid json");
        assert!(
            serde_json::from_value::<BTreeMap<String, ProfileEntry>>(value["elements"].clone())
                .is_err()
        );
    }

    /// The pre-mutation guard depends on a strict manifest decode: the
    /// `version` field is required, only version 3 decodes, and entry
    /// fields stay mandatory.
    #[test]
    fn profile_manifest_decode_requires_version_three() {
        let empty = serde_json::json!({"version": 3, "elements": {}});
        assert!(decode_profile_manifest(&empty).expect("decodes").is_empty());

        let missing = serde_json::json!({"elements": {}});
        let error = decode_profile_manifest(&missing).expect_err("version is required");
        let text = error.to_string();
        assert!(text.contains("no manifest version"), "{text}");

        let wrong = serde_json::json!({"version": 2, "elements": {}});
        let error = decode_profile_manifest(&wrong).expect_err("other versions are refused");
        let text = error.to_string();
        assert!(text.contains("version 2"), "{text}");

        let element_without_fields = serde_json::json!({
            "version": 3,
            "elements": {"ripgrep": {"attrPath": "ripgrep"}}
        });
        assert!(decode_profile_manifest(&element_without_fields).is_err());
    }

    /// Decodes the fixture: an actual generated-catalog slice (revision
    /// `245947c0`, schema `pkg-cask-catalog/2`) projected to the flake
    /// `catalogIndex` shape by the same rule `nix/casks/flake.nix` uses.
    /// Values are real generated data, not invented examples.
    #[test]
    fn decodes_the_actual_catalog_index_slice() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        let index = decode_catalog_index(&raw).expect("decodes actual slice");
        assert_eq!(index.generator.name, "cask-catalog");
        assert_eq!(index.generator.version, "0.1.0");
        assert_eq!(
            index.input.revision,
            "245947c0b920cbe83f4003bb7c6be737352c8314"
        );
        assert_eq!(
            index.input.sha256,
            "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
        );
        assert_eq!(index.input.license, "BSD-2-Clause (Homebrew Cask data)");
        assert_eq!(index.targets, ["aarch64-darwin", "x86_64-linux"]);
        assert_eq!(index.macos_baseline, "15.7.7");

        let macos = &index.systems["aarch64-darwin"].entries;
        let iterm2 = &macos["iterm2"];
        assert_eq!(iterm2.status, EntryStatus::Eligible);
        assert_eq!(iterm2.kind.as_deref(), Some("app"));
        assert_eq!(iterm2.version.as_deref(), Some("3.6.11"));
        assert_eq!(iterm2.name.as_deref(), Some("iTerm2"));
        // Special token characters are real identifiers in the catalog.
        assert_eq!(macos["1password@7"].status, EntryStatus::Eligible);
        assert_eq!(macos["4k-video-downloader+"].status, EntryStatus::Eligible);
        // One token can be eligible on one target and excluded on another,
        // with a target-effective version that differs from the entry.
        let cursor_linux = &index.systems["x86_64-linux"].entries["cursor"];
        assert_eq!(cursor_linux.status, EntryStatus::Eligible);
        assert_eq!(cursor_linux.kind.as_deref(), Some("appimage"));
        let raycast_mac = &macos["raycast"];
        assert_eq!(raycast_mac.status, EntryStatus::Eligible);
        assert_eq!(raycast_mac.version.as_deref(), Some("1.104.25"));
        let raycast_linux = &index.systems["x86_64-linux"].entries["raycast"];
        assert_eq!(raycast_linux.status, EntryStatus::Excluded);
        assert_eq!(
            raycast_linux.reason.as_deref(),
            Some("unsupported-platform")
        );
        // The Linux variation sets `version` to null; Nix `or` does not
        // coalesce a present null to the base value, so the real index
        // carries null here — the fixture must match that exactly.
        assert_eq!(raycast_linux.version, None);
        // Excluded records carry their generated reason; detail stays
        // string-or-null.
        let zoom = &macos["zoom"];
        assert_eq!(zoom.status, EntryStatus::Excluded);
        assert_eq!(zoom.reason.as_deref(), Some("installer-script"));
        assert_eq!(zoom.kind, None);
        let firefox = &macos["firefox"];
        assert_eq!(firefox.reason.as_deref(), Some("unsupported-artifact"));
        // The Linux lane that was probed for real is eligible here.
        let koreader = &index.systems["x86_64-linux"].entries["koreader"];
        assert_eq!(koreader.status, EntryStatus::Eligible);
        assert_eq!(koreader.version.as_deref(), Some("2026.07.1"));
    }

    /// Nullable metadata is part of the protocol: a synthetic minimal
    /// record proves null name/description/version/homepage decode without
    /// inventing values (the real slice carries non-null strings).
    #[test]
    fn nullable_metadata_stays_null_in_decode() {
        let raw: serde_json::Value = serde_json::json!({
            "schema": CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "input": {"url": "u", "revision": "245947c0b920cbe83f4003bb7c6be737352c8314",
                       "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                       "license": "BSD-2-Clause (Homebrew Cask data)"},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "bare": {"token": "bare", "name": null, "description": null,
                          "version": null, "homepage": null,
                          "status": "eligible", "kind": "binary",
                          "reason": null, "detail": null}}}}
        });
        let index = decode_catalog_index(&raw).expect("decodes nullable record");
        let bare = &index.systems["x86_64-linux"].entries["bare"];
        assert_eq!(bare.name, None);
        assert_eq!(bare.description, None);
        assert_eq!(bare.version, None);
        assert_eq!(bare.homepage, None);
        assert_eq!(bare.status, EntryStatus::Eligible);
    }

    /// The schema gate names the schema found and the schema supported, so a
    /// future catalog (or an old envelope) is a clear error, never a misread.
    #[test]
    fn catalog_index_schema_gate_names_both_schemas() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        raw["schema"] = serde_json::json!("pkg-cask-catalog/3");
        let error = decode_catalog_index(&raw).expect_err("future schema is refused");
        let text = error.to_string();
        assert!(text.contains("pkg-cask-catalog/3"), "{text}");
        assert!(text.contains(CATALOG_INDEX_SCHEMA), "{text}");
        raw.as_object_mut().unwrap().remove("schema");
        let error = decode_catalog_index(&raw).expect_err("missing schema is refused");
        assert!(error.to_string().contains("no schema"));
    }

    /// A record that breaks the null rules or the token rule fails the
    /// whole decode with the token named; a missing target section or an
    /// undeclared system section fails the envelope as inconsistent.
    #[test]
    fn broken_records_fail_the_whole_decode_named() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        // An eligible entry without a kind breaks the null rules.
        raw["systems"]["x86_64-linux"]["entries"]["koreader"]["kind"] = serde_json::Value::Null;
        let error = decode_catalog_index(&raw).expect_err("broken record fails the decode");
        let text = error.to_string();
        assert!(text.contains("koreader"), "{text}");
        assert!(text.contains("kind"), "{text}");

        // A record token that does not match its key fails the decode.
        let mut mismatched = raw.clone();
        mismatched["systems"]["x86_64-linux"]["entries"]["koreader"]["token"] =
            serde_json::json!("other-token");
        let error = decode_catalog_index(&mismatched).expect_err("token mismatch fails");
        assert!(error.to_string().contains("other-token"));

        // A token that is not a valid identifier cannot be a map key.
        let mut keyed = raw.clone();
        keyed["systems"]["x86_64-linux"]["entries"]["Iterm2"] =
            keyed["systems"]["x86_64-linux"]["entries"]["koreader"].clone();
        let error = decode_catalog_index(&keyed).expect_err("invalid token key fails");
        assert!(error.to_string().contains("Iterm2"));

        // A declared target without a system section is an inconsistent
        // envelope and fails as a whole.
        let mut broken = raw.clone();
        broken["systems"]
            .as_object_mut()
            .unwrap()
            .remove("aarch64-darwin");
        let error = decode_catalog_index(&broken).expect_err("missing target section");
        assert!(error.to_string().contains("aarch64-darwin"));
        // An undeclared system section fails too.
        let mut extra = raw.clone();
        extra["systems"]["x86_64-darwin"] = extra["systems"]["x86_64-linux"].clone();
        let error = decode_catalog_index(&extra).expect_err("undeclared system section");
        assert!(error.to_string().contains("x86_64-darwin"));
    }

    /// The token rule matches the generator's identifier rule, including
    /// the real `@` versioned tokens and `+` tokens of the snapshot.
    #[test]
    fn catalog_tokens_follow_the_generated_identifier_rule() {
        for token in [
            "a",
            "iterm2",
            "1password-cli",
            "1password-cli@beta",
            "1password@nightly",
            "xournal++",
            "4k-video-downloader+",
            "telegram+bot",
            "v2.0",
        ] {
            assert!(valid_catalog_token(token), "{token}");
        }
        for token in [
            "",
            "Iterm2",
            "-lead",
            ".lead",
            "a..b",
            "a/b",
            "to ken",
            "1password cli",
        ] {
            assert!(!valid_catalog_token(token), "{token}");
        }
    }

    /// Captured `nix flake metadata --json` for a moving `git+file`
    /// reference: the top-level `url` is the canonical locked URL and keeps
    /// the transport and parameters; the nested `locked.url` drops them and
    /// must not be used as the query reference.
    #[test]
    fn source_identity_uses_the_canonical_top_level_locked_url() {
        let captured = r#"{
          "originalUrl": "git+file:///tmp/pkg-cask-moving?ref=main",
          "url": "git+file:///tmp/pkg-cask-moving?ref=main&rev=c073a489725bb05621c798827025b2faf7482f6b",
          "locked": {
            "type": "git",
            "url": "file:///tmp/pkg-cask-moving",
            "ref": "main",
            "rev": "c073a489725bb05621c798827025b2faf7482f6b"
          }
        }"#;
        let value: serde_json::Value = serde_json::from_str(captured).expect("valid json");
        let identity = decode_source_identity("git+file:///tmp/pkg-cask-moving?ref=main", &value);
        assert_eq!(
            identity.locked_url.as_deref(),
            Some(
                "git+file:///tmp/pkg-cask-moving?ref=main&rev=c073a489725bb05621c798827025b2faf7482f6b"
            )
        );
        assert_eq!(
            identity.revision.as_deref(),
            Some("c073a489725bb05621c798827025b2faf7482f6b")
        );
        assert_eq!(identity.display, "git");
        // Without a top-level url, the nested locked url stays as fallback.
        let mut bare = value.clone();
        bare.as_object_mut().expect("object").remove("url");
        let fallback = decode_source_identity("git+file:///tmp/pkg-cask-moving?ref=main", &bare);
        assert_eq!(
            fallback.locked_url.as_deref(),
            Some("file:///tmp/pkg-cask-moving")
        );
    }
}
