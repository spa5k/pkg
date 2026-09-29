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
/// The index is the generated `pkg-cask-catalog/3` envelope exposed by a
/// casks flake as `catalogIndex`. It carries one provenance object per
/// source (`inputs`), the target system list, and per-system status entries
/// keyed by the full entry identity `owner/tap/token` (the official source
/// is `homebrew/cask`). Schema 2 is gone: no compatibility layer exists.
pub const CATALOG_INDEX_SCHEMA: &str = "pkg-cask-catalog/3";

/// The source identity of the official generated cask catalog.
///
/// The official catalog is a schema-3 catalog like any tap catalog; its
/// source key is reserved and can never be added as a local tap.
pub const OFFICIAL_CASK_SOURCE: &str = "homebrew/cask";

/// The generator provenance recorded in a catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogGenerator {
    /// The generator name, for example `cask-catalog`.
    pub name: String,
    /// The generator version.
    pub version: String,
}

/// The raw-input provenance recorded for a locally imported tap source.
///
/// A local (tap) input's `url` is the canonical approved repository
/// origin, and this object carries what the importer actually fetched and
/// captured, including the backend envelope identity
/// (`pkg-tap-raw-export/1`). Unknown extra fields are accepted: the
/// backend envelope grows fields the client does not interpret. The
/// official catalog input has no `raw` object at all, so the field stays
/// optional and absent there. There is deliberately no `origin` inside
/// `raw`: the approved origin is the input's own `url`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInputProvenance {
    /// The archive URL the importer fetched the raw checkout from.
    pub archive_url: String,
    /// The SHA-256 of the fetched archive.
    pub archive_sha256: String,
    /// The SHA-256 of the captured metadata (the import's provenance hash).
    pub capture_sha256: String,
    /// The backend raw-export envelope identity, for example
    /// `pkg-tap-raw-export/1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_schema: Option<String>,
}

/// The pinned input provenance recorded for one catalog source.
///
/// Unknown extra fields are accepted permissively on decode (generated
/// protocol data grows); the required identity fields must be present and
/// nonempty. On encode only the decoded fields are written back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogInput {
    /// The pinned source URL the catalog was generated from.
    pub url: String,
    /// The exact pinned revision.
    pub revision: String,
    /// The SHA-256 of the pinned input.
    pub sha256: String,
    /// The upstream data license attribution.
    pub license: String,
    /// The source kind, for example `official` or `tap`, when recorded.
    pub kind: Option<String>,
    /// The raw-input provenance, present only for locally imported taps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawInputProvenance>,
}

/// Whether one index entry is installable on its system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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
/// Map keys are full entry identities `owner/tap/token`; the record must
/// repeat its `source` and bare `token` and both must match the key.
///
/// Null rules (validated in [`decode_catalog_index`]): `name`,
/// `description`, `version`, and `homepage` are string or null; an eligible
/// entry has a non-null `kind` and null `reason`/`detail`; an excluded entry
/// has a null `kind`, a non-null `reason`, and a string-or-null `detail`.
/// The full catalog projection lives in the backend flake; this client
/// decodes exactly one index envelope shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogEntry {
    /// The source identity (`owner/tap`, or `homebrew/cask`); must match
    /// the source segment of the map key and name an `inputs` entry.
    pub source: String,
    /// The bare cask token; must equal the final segment of the map key.
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogSystem {
    /// The status entries for this system, keyed by the full entry
    /// identity `owner/tap/token`.
    pub entries: BTreeMap<String, CatalogEntry>,
}

/// The decoded `catalogIndex` envelope of a casks flake.
///
/// The envelope serializes back to the exact shape it decodes from — the
/// `schema` identity and the `macosBaseline` field name included — so a
/// published generation's capture file is the same strict schema-3 index
/// the decoder consumes. No parallel encoding exists in this client.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogIndex {
    /// The schema identity written on encode. Decoding gates on the raw
    /// JSON `schema` value before this type is built, so the field is not
    /// read back from input.
    #[serde(skip_deserializing)]
    pub schema: String,
    /// The generator provenance.
    pub generator: CatalogGenerator,
    /// The pinned input provenance of every source in this catalog, keyed
    /// by source identity (`owner/tap`, or `homebrew/cask`).
    pub inputs: BTreeMap<String, CatalogInput>,
    /// The target systems the catalog was generated for.
    pub targets: Vec<String>,
    /// The declared macOS baseline, for example `15.7.7`.
    #[serde(rename = "macosBaseline")]
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
    /// The display name of this entry.
    ///
    /// A cask entry's final attribute segment is the encoded full human
    /// cask id (`owner/tap/token`, dots and underscores escaped); it is
    /// decoded back for display, but only when the segment is a valid
    /// cask segment — anything else, including an unknown escape, is
    /// shown exactly as recorded and never silently misdecoded. Ordinary
    /// Nixpkgs names contain no `/` and stay unchanged.
    #[must_use]
    pub fn name(&self) -> String {
        let segment = self.attr_path.rsplit('.').next().unwrap_or("?");
        if !segment.contains('/') {
            return segment.to_string();
        }
        cask_catalog::decode_package_attribute(segment)
            .ok()
            .filter(|id| split_entry_id(id).is_some())
            .unwrap_or_else(|| segment.to_string())
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

/// Decode one captured `catalogIndex` envelope (schema 3).
///
/// The schema gate is strict: an envelope whose `schema` is absent or is
/// not [`CATALOG_INDEX_SCHEMA`] fails naming the schema found and the
/// schema supported, so an old client meeting a new catalog (or the
/// reverse) is a clear error instead of a misread. Required provenance and
/// identity fields must be present: every `inputs` key is a valid source
/// slug, every `systems.<system>.entries` key is the full identity
/// `owner/tap/token`, each record repeats a `source` and bare `token` that
/// match its key, and every record source names an `inputs` entry. Unknown
/// extra provenance fields are accepted; broken identity or null rules fail
/// the decode as a whole with the entry named. A duplicate entry id cannot
/// decode twice: a JSON object holds one value per key, and a record whose
/// identity disagrees with its key fails instead of colliding.
///
/// Exactly one layout decodes here: the per-system index envelope with
/// `systems.<system>.entries` maps. The full generated catalog layout is
/// projected by the backend flake, never by this client, so no second
/// classifier or projection can silently skip malformed records.
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

    // One provenance object per source; unknown extra fields are fine, a
    // missing or empty required identity field is not.
    let Some(inputs_raw) = raw.get("inputs").and_then(serde_json::Value::as_object) else {
        return Err(gate(String::from(
            "the `inputs` provenance map is absent from the index",
        )));
    };
    if inputs_raw.is_empty() {
        return Err(gate(String::from(
            "the `inputs` provenance map is empty; a catalog names at least one source",
        )));
    }
    let mut inputs = BTreeMap::new();
    for (source, provenance) in inputs_raw {
        if !valid_catalog_source(source) {
            return Err(gate(format!(
                "inputs key {source:?} is not a valid owner/tap source identity"
            )));
        }
        let input: CatalogInput = serde_json::from_value(provenance.clone())
            .map_err(|error| gate(format!("inputs.{source} provenance is invalid: {error}")))?;
        for (field, value) in [
            ("url", &input.url),
            ("revision", &input.revision),
            ("sha256", &input.sha256),
            ("license", &input.license),
        ] {
            if value.trim().is_empty() {
                return Err(gate(format!(
                    "inputs.{source}.{field} must be a nonempty string"
                )));
            }
        }
        if let Some(raw) = &input.raw {
            for (field, value) in [
                ("raw.archiveUrl", &raw.archive_url),
                ("raw.archiveSha256", &raw.archive_sha256),
                ("raw.captureSha256", &raw.capture_sha256),
            ] {
                if value.trim().is_empty() {
                    return Err(gate(format!(
                        "inputs.{source}.{field} must be a nonempty string"
                    )));
                }
            }
        }
        inputs.insert(source.clone(), input);
    }

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

    // The per-system sections are the one decoded layout; an envelope
    // without them is not an index envelope.
    let Some(systems_raw) = raw
        .get("systems")
        .and_then(serde_json::Value::as_object)
        .cloned()
    else {
        return Err(gate(String::from(
            "the `systems` per-system sections are absent from the index",
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
        for (full_id, record) in entries_raw {
            let entry = validate_entry(full_id, record, &inputs)
                .map_err(|detail| gate(format!("{system}/{full_id}: {detail}")))?;
            entries.insert(full_id.clone(), entry);
        }
        systems.insert(system.clone(), CatalogSystem { entries });
    }
    Ok(CatalogIndex {
        schema: CATALOG_INDEX_SCHEMA.to_string(),
        generator,
        inputs,
        targets,
        macos_baseline: macos_baseline.to_string(),
        systems,
    })
}

/// Validate one index entry record against its full-id key and the inputs.
///
/// The key must be `owner/tap/token` with a valid source slug and a valid
/// bare token; the record must repeat exactly that `source` and `token`;
/// the source must carry provenance in `inputs`; and the status null rules
/// must hold. Anything else fails the whole decode with the entry named.
fn validate_entry(
    full_id: &str,
    record: &serde_json::Value,
    inputs: &BTreeMap<String, CatalogInput>,
) -> Result<CatalogEntry, String> {
    let Some((source, token)) = split_entry_id(full_id) else {
        return Err(String::from(
            "the key is not a full owner/tap/token entry identity",
        ));
    };
    let entry: CatalogEntry = serde_json::from_value(record.clone())
        .map_err(|error| format!("does not follow the schema: {error}"))?;
    if !valid_catalog_token(&token) {
        return Err(String::from(
            "the token is not a valid catalog token identifier",
        ));
    }
    if entry.source != source {
        return Err(format!(
            "record source {:?} does not match its key {source:?}",
            entry.source
        ));
    }
    if entry.token != token {
        return Err(format!(
            "record token {:?} does not match its key {token:?}",
            entry.token
        ));
    }
    if !inputs.contains_key(&source) {
        return Err(format!(
            "record source {source:?} has no `inputs` provenance"
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

/// The full entry identity of one source and token: `owner/tap/token`.
#[must_use]
pub fn full_entry_id(source: &str, token: &str) -> String {
    format!("{source}/{token}")
}

/// Split one full entry identity into its source and bare token.
///
/// Returns `None` unless the identity is exactly `owner/tap/token` with a
/// valid source slug and a valid bare token.
#[must_use]
pub fn split_entry_id(full_id: &str) -> Option<(String, String)> {
    let mut segments = full_id.split('/');
    let owner = segments.next()?;
    let tap = segments.next()?;
    let token = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let source = format!("{owner}/{tap}");
    if !valid_catalog_source(&source) || !valid_catalog_token(token) {
        return None;
    }
    Some((source, token.to_string()))
}

/// Whether `source` is a valid catalog source identity.
///
/// The rule is the tap slug rule: exactly `owner/tap`, each segment a
/// nonempty run of letters, digits, `_`, `-`, or `.` that is neither `.`
/// nor `..`. The official source `homebrew/cask` follows the same rule.
#[must_use]
pub fn valid_catalog_source(source: &str) -> bool {
    let Some((owner, tap)) = source.split_once('/') else {
        return false;
    };
    let segment = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    };
    segment(owner) && segment(tap)
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

    /// A cask entry's native attribute segment decodes to the full human
    /// cask id; unknown escapes and ordinary names are never misdecoded.
    #[test]
    fn cask_entry_names_decode_the_full_human_id() {
        let decode_name = |attr_path: &str| {
            let entry: ProfileEntry = serde_json::from_value(serde_json::json!({
                "active": true, "attrPath": attr_path,
                "originalUrl": "path:/state/pkg/taps/src/example/tap/current",
                "url": "path:/state/pkg/taps/src/example/tap/current",
                "storePaths": ["/nix/store/x"],
            }))
            .expect("decodes");
            entry.name()
        };
        // Dots and underscores in the id round trip back to the human form.
        assert_eq!(
            decode_name("packages.x86_64-linux.example/tap/tool@1_d_2"),
            "example/tap/tool@1.2"
        );
        assert_eq!(
            decode_name("packages.x86_64-linux.a_u_b_d_c/d_d_e/f_u_g_d_h"),
            "a_b.c/d.e/f_g.h"
        );
        // Ordinary Nixpkgs names stay unchanged.
        assert_eq!(decode_name("legacyPackages.x86_64-linux.hello"), "hello");
        // An unknown or truncated escape never silently misdecodes: the
        // recorded segment is shown exactly as the manifest spells it.
        assert_eq!(
            decode_name("packages.x86_64-linux.example/tap/to_x_"),
            "example/tap/to_x_"
        );
        assert_eq!(
            decode_name("packages.x86_64-linux.example/tap/trailing_"),
            "example/tap/trailing_"
        );
    }

    /// Real backend raw provenance carries `envelopeSchema` beside the
    /// three hashes; the decoder accepts it (and any other growth) while
    /// official inputs stay without `raw`.
    #[test]
    fn raw_provenance_accepts_the_backend_envelope_shape() {
        let revision = "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let raw = serde_json::json!({
            "schema": CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"somebody/apps": {
                "url": "https://github.com/somebody/homebrew-apps",
                "revision": revision,
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license",
                "raw": {
                    "archiveUrl": format!(
                        "https://api.github.com/repos/somebody/homebrew-apps/tarball/{revision}"),
                    "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                    "captureSha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                    "envelopeSchema": "pkg-tap-raw-export/1",
                }}},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "somebody/apps/tool": {
                    "source": "somebody/apps", "token": "tool", "name": null,
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary",
                    "reason": null, "detail": null}}}},
        });
        let index = decode_catalog_index(&raw).expect("decodes the backend shape");
        let input = &index.inputs["somebody/apps"];
        let raw = input.raw.as_ref().expect("raw preserved");
        assert_eq!(raw.envelope_schema.as_deref(), Some("pkg-tap-raw-export/1"));
        assert_eq!(
            raw.capture_sha256,
            "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
        );
        // The official catalog input carries no `raw` at all and decodes.
        let official = serde_json::json!({
            "schema": CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"homebrew/cask": {
                "url": "github:spa5k/pkg/2403?dir=nix/casks", "revision": revision,
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license"}},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "homebrew/cask/iterm2": {
                    "source": "homebrew/cask", "token": "iterm2", "name": null,
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary",
                    "reason": null, "detail": null}}}},
        });
        let index = decode_catalog_index(&official).expect("official raw stays absent");
        assert!(index.inputs["homebrew/cask"].raw.is_none());
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
    /// `245947c0`, schema `pkg-cask-catalog/3`) projected to the flake
    /// `catalogIndex` shape by the same rule `nix/casks/flake.nix` uses.
    /// Values are real generated data, not invented examples. The official
    /// catalog is one source among possibly many: its identity is
    /// `homebrew/cask` and every entry key repeats source and token.
    #[test]
    fn decodes_the_actual_catalog_index_slice() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        let index = decode_catalog_index(&raw).expect("decodes actual slice");
        assert_eq!(index.generator.name, "cask-catalog");
        assert_eq!(index.generator.version, "0.1.0");
        let official = &index.inputs[OFFICIAL_CASK_SOURCE];
        assert_eq!(
            official.revision,
            "245947c0b920cbe83f4003bb7c6be737352c8314"
        );
        assert_eq!(
            official.sha256,
            "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
        );
        assert_eq!(official.license, "BSD-2-Clause (Homebrew Cask data)");
        assert_eq!(index.targets, ["aarch64-darwin", "x86_64-linux"]);
        assert_eq!(index.macos_baseline, "15.7.7");

        let macos = &index.systems["aarch64-darwin"].entries;
        let iterm2 = &macos["homebrew/cask/iterm2"];
        assert_eq!(iterm2.source, OFFICIAL_CASK_SOURCE);
        assert_eq!(iterm2.token, "iterm2");
        assert_eq!(iterm2.status, EntryStatus::Eligible);
        assert_eq!(iterm2.kind.as_deref(), Some("app"));
        assert_eq!(iterm2.version.as_deref(), Some("3.6.11"));
        assert_eq!(iterm2.name.as_deref(), Some("iTerm2"));
        // Special token characters are real identifiers in the catalog.
        assert_eq!(
            macos["homebrew/cask/1password@7"].status,
            EntryStatus::Eligible
        );
        assert_eq!(
            macos["homebrew/cask/4k-video-downloader+"].status,
            EntryStatus::Eligible
        );
        // One token can be eligible on one target and excluded on another,
        // with a target-effective version that differs from the entry.
        let cursor_linux = &index.systems["x86_64-linux"].entries["homebrew/cask/cursor"];
        assert_eq!(cursor_linux.status, EntryStatus::Eligible);
        assert_eq!(cursor_linux.kind.as_deref(), Some("appimage"));
        let raycast_linux = &index.systems["x86_64-linux"].entries["homebrew/cask/raycast"];
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
        let zoom = &macos["homebrew/cask/zoom"];
        assert_eq!(zoom.status, EntryStatus::Excluded);
        assert_eq!(zoom.reason.as_deref(), Some("installer-script"));
        assert_eq!(zoom.kind, None);
        // The Linux lane that was probed for real is eligible here.
        let koreader = &index.systems["x86_64-linux"].entries["homebrew/cask/koreader"];
        assert_eq!(koreader.status, EntryStatus::Eligible);
        assert_eq!(koreader.version.as_deref(), Some("2026.07.1"));
    }

    /// Nullable metadata is part of the protocol: a synthetic minimal
    /// record proves null name/description/version/homepage decode without
    /// inventing values (the real slice carries non-null strings). The same
    /// envelope proves two sources may carry the same bare token: they
    /// coexist under different full identities.
    #[test]
    fn nullable_metadata_stays_null_and_tokens_coexist_per_source() {
        let input = |url: &str| {
            serde_json::json!({
                "url": url,
                "revision": "245947c0b920cbe83f4003bb7c6be737352c8314",
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license",
                "extra-unknown-field": {"any": ["shape"]}
            })
        };
        let record = |source: &str, status: &str| {
            let eligible = status == "eligible";
            serde_json::json!({
                "source": source, "token": "bare", "name": null,
                "description": null, "version": null, "homepage": null,
                "status": status,
                "kind": if eligible { serde_json::json!("binary") } else { serde_json::Value::Null },
                "reason": if eligible { serde_json::Value::Null } else { serde_json::json!("unsupported-platform") },
                "detail": null})
        };
        let raw: serde_json::Value = serde_json::json!({
            "schema": CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {
                "homebrew/cask": input("https://example.com/official.json"),
                "somebody/apps": input("https://github.com/somebody/homebrew-apps")
            },
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "homebrew/cask/bare": record(OFFICIAL_CASK_SOURCE, "eligible"),
                "somebody/apps/bare": record("somebody/apps", "excluded")}}}
        });
        let index = decode_catalog_index(&raw).expect("decodes nullable records");
        let entries = &index.systems["x86_64-linux"].entries;
        let bare = &entries["homebrew/cask/bare"];
        assert_eq!(bare.name, None);
        assert_eq!(bare.description, None);
        assert_eq!(bare.version, None);
        assert_eq!(bare.homepage, None);
        assert_eq!(bare.status, EntryStatus::Eligible);
        // The same bare token from a second source coexists; identity is
        // the full id, never the token alone.
        let tap_bare = &entries["somebody/apps/bare"];
        assert_eq!(tap_bare.status, EntryStatus::Excluded);
        assert_eq!(tap_bare.source, "somebody/apps");
        assert_eq!(index.inputs.len(), 2);
    }

    /// The envelope serializes back to the exact decode shape — published
    /// captures are written by the decoder's own types, and the full
    /// generated catalog layout (no `systems` sections) is refused here.
    #[test]
    fn envelope_round_trips_and_full_catalog_layout_is_refused() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        let index = decode_catalog_index(&raw).expect("decodes");
        let encoded = serde_json::to_value(&index).expect("encodes");
        // The written capture carries the schema identity and the exact
        // `macosBaseline` field name, so saved captures decode again.
        assert_eq!(encoded["schema"], CATALOG_INDEX_SCHEMA);
        assert_eq!(encoded["macosBaseline"], "15.7.7");
        assert_eq!(decode_catalog_index(&encoded).expect("round trips"), index);

        // A layout without per-system `systems` sections is backend data;
        // this client refuses it instead of projecting it.
        let mut full = raw.clone();
        let systems = full["systems"].clone();
        full.as_object_mut()
            .expect("object")
            .insert(String::from("entries"), systems);
        full.as_object_mut().expect("object").remove("systems");
        assert!(decode_catalog_index(&full).is_err());
    }

    /// The schema gate names the schema found and the schema supported, so a
    /// future catalog (or an old envelope) is a clear error, never a misread.
    #[test]
    fn catalog_index_schema_gate_names_both_schemas() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        raw["schema"] = serde_json::json!("pkg-cask-catalog/4");
        let error = decode_catalog_index(&raw).expect_err("future schema is refused");
        let text = error.to_string();
        assert!(text.contains("pkg-cask-catalog/4"), "{text}");
        assert!(text.contains(CATALOG_INDEX_SCHEMA), "{text}");
        // The removed schema 2 is refused by the same gate.
        raw["schema"] = serde_json::json!("pkg-cask-catalog/2");
        assert!(decode_catalog_index(&raw).is_err());
        raw.as_object_mut().unwrap().remove("schema");
        let error = decode_catalog_index(&raw).expect_err("missing schema is refused");
        assert!(error.to_string().contains("no schema"));
    }

    /// A record that breaks the identity rules, the null rules, or the
    /// provenance rules fails the whole decode with the entry named; a
    /// missing target section or an undeclared system section fails the
    /// envelope as inconsistent.
    #[test]
    fn broken_records_fail_the_whole_decode_named() {
        let manifest = include_str!("../../tests/fixtures/catalog-index.json");
        let mut raw: serde_json::Value = serde_json::from_str(manifest).expect("valid json");
        let pristine = raw.clone();
        // An eligible entry without a kind breaks the null rules.
        raw["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["kind"] =
            serde_json::Value::Null;
        let error = decode_catalog_index(&raw).expect_err("broken record fails the decode");
        let text = error.to_string();
        assert!(text.contains("koreader"), "{text}");
        assert!(text.contains("kind"), "{text}");

        // A record token that does not match its key fails the decode.
        let mut mismatched = raw.clone();
        mismatched["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["token"] =
            serde_json::json!("other-token");
        let error = decode_catalog_index(&mismatched).expect_err("token mismatch fails");
        assert!(error.to_string().contains("other-token"));

        // A record source that does not match its key fails the decode.
        let mut foreign = raw.clone();
        foreign["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"]["source"] =
            serde_json::json!("somebody/apps");
        let error = decode_catalog_index(&foreign).expect_err("source mismatch fails");
        assert!(error.to_string().contains("source"));

        // A record whose source carries no provenance fails the decode.
        let mut unprovenanced = pristine.clone();
        unprovenanced["systems"]["x86_64-linux"]["entries"]
            .as_object_mut()
            .unwrap()
            .insert(
                String::from("somebody/apps/koreader"),
                serde_json::json!({
                    "source": "somebody/apps", "token": "koreader",
                    "name": null, "description": null, "version": null,
                    "homepage": null, "status": "eligible", "kind": "binary",
                    "reason": null, "detail": null}),
            );
        let error = decode_catalog_index(&unprovenanced).expect_err("unprovenanced source fails");
        assert!(error.to_string().contains("somebody/apps"));

        // A key that is not a full owner/tap/token identity cannot be a
        // map key: a bare token fails, and so does an invalid token.
        let mut keyed = raw.clone();
        let record = keyed["systems"]["x86_64-linux"]["entries"]["homebrew/cask/koreader"].clone();
        keyed["systems"]["x86_64-linux"]["entries"]
            .as_object_mut()
            .unwrap()
            .insert(String::from("koreader"), record.clone());
        let error = decode_catalog_index(&keyed).expect_err("bare key fails");
        assert!(error.to_string().contains("koreader"));
        let mut upper = raw.clone();
        upper["systems"]["x86_64-linux"]["entries"]
            .as_object_mut()
            .unwrap()
            .insert(String::from("homebrew/cask/Iterm2"), record);
        let error = decode_catalog_index(&upper).expect_err("invalid token key fails");
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

        // An inputs provenance object without required data fails closed.
        let mut pinless = raw.clone();
        pinless["inputs"][OFFICIAL_CASK_SOURCE]["revision"] = serde_json::json!("");
        let error = decode_catalog_index(&pinless).expect_err("empty provenance fails");
        assert!(error.to_string().contains("revision"), "{}", error);
    }

    /// Full entry identities split and reassemble exactly, and the source
    /// slug rule accepts the official source and real tap slugs while
    /// refusing traversal and malformed input.
    #[test]
    fn full_entry_identities_split_and_reassemble() {
        assert_eq!(
            split_entry_id("somebody/apps/iterm2"),
            Some((String::from("somebody/apps"), String::from("iterm2")))
        );
        assert_eq!(
            split_entry_id(OFFICIAL_CASK_SOURCE),
            None,
            "an entry id needs a token segment"
        );
        assert_eq!(split_entry_id("somebody/apps"), None);
        assert_eq!(split_entry_id("somebody/apps/a/b"), None);
        assert_eq!(
            split_entry_id("somebody/apps/1password-cli@beta")
                .unwrap()
                .1,
            "1password-cli@beta"
        );
        assert_eq!(
            full_entry_id(OFFICIAL_CASK_SOURCE, "iterm2"),
            "homebrew/cask/iterm2"
        );

        assert!(valid_catalog_source(OFFICIAL_CASK_SOURCE));
        assert!(valid_catalog_source("somebody/apps"));
        assert!(valid_catalog_source("a-b.c/d_e"));
        for refused in [
            "",
            "somebody",
            "somebody/apps/extra",
            "/apps",
            "somebody/",
            "../apps",
            "somebody/..",
            "some body/apps",
            "somebody/ap ps",
            "somebody/apps?dir=x",
        ] {
            assert!(!valid_catalog_source(refused), "{refused} must be refused");
        }
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
