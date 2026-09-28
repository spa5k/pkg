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

/// The cask support manifest schema this client decodes (design D6).
pub const CASK_SUPPORT_SCHEMA: &str = "pkg-cask-support/1";

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

/// One source record inside the cask support manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSourceRef {
    /// The source URL the record describes.
    pub url: String,
    /// The locked revision, when the source provides one.
    #[serde(default)]
    pub rev: Option<String>,
}

/// One supported cask record (schema `pkg-cask-support/1`).
///
/// Field types follow the actual generated manifest: `override` is a
/// boolean, and `version`/`homepage` may be null by flake interface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSupported {
    /// The cask token.
    pub token: String,
    /// The package kind, for example `app` or `app+cli`.
    pub kind: String,
    /// The upstream version, when the flake interface provides one.
    pub version: Option<String>,
    /// The upstream homepage, when the flake interface provides one.
    pub homepage: Option<String>,
    /// The artifact kinds the package provides.
    #[serde(rename = "artifactKinds")]
    pub artifact_kinds: Vec<String>,
    /// Whether an override is applied on top of upstream.
    #[serde(rename = "override", default)]
    pub override_applied: bool,
}

/// One excluded cask record with the exclusion reason.
///
/// `version` and `detail` may be null in the actual generated manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskExcluded {
    /// The cask token.
    pub token: String,
    /// The upstream version that was excluded, when known.
    pub version: Option<String>,
    /// A short machine-readable reason.
    pub reason: String,
    /// Human-readable detail for the exclusion, when present.
    #[serde(default)]
    pub detail: Option<String>,
}

/// The cask support manifest exported by the casks flake (design D6).
///
/// Decoded from `caskSupport.<system>` with schema `pkg-cask-support/1`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSupport {
    /// The manifest schema identifier; must equal [`CASK_SUPPORT_SCHEMA`].
    pub schema: String,
    /// The system triple the manifest describes.
    pub system: String,
    /// The provenance records for the sources the manifest was built from.
    pub sources: BTreeMap<String, CaskSourceRef>,
    /// The casks supported on this system.
    pub supported: Vec<CaskSupported>,
    /// The casks excluded on this system, with reasons.
    pub excluded: Vec<CaskExcluded>,
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

    /// Decodes the actual generated cask support manifest fixture
    /// (Determinate 3.22.1 / Nix 2.35.2 host output), not an invented shape.
    #[test]
    fn decodes_the_real_cask_support_manifest_fixture() {
        let manifest = include_str!("../../tests/fixtures/cask-support.json");
        let support: CaskSupport = serde_json::from_str(manifest).expect("decodes real fixture");
        assert_eq!(support.schema, CASK_SUPPORT_SCHEMA);
        assert_eq!(support.system, "aarch64-darwin");
        assert_eq!(support.supported.len(), 3);
        assert_eq!(support.excluded.len(), 5);

        let raycast = &support.excluded[4];
        assert_eq!(raycast.token, "raycast");
        assert_eq!(raycast.reason, "minimum-os");
        // A real on-device exclusion detail; it is a non-null string here.
        assert!(
            raycast
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("LSMinimumSystemVersion"))
        );
        assert_eq!(raycast.version.as_deref(), Some("2.0.5.0"));

        let iterm2 = &support.supported[0];
        assert_eq!(iterm2.token, "iterm2");
        assert_eq!(iterm2.kind, "app");
        assert!(!iterm2.override_applied, "override is a boolean");
        assert_eq!(iterm2.version.as_deref(), Some("3.6.11"));
        assert!(
            iterm2
                .homepage
                .as_deref()
                .is_some_and(|url| url.starts_with("https://"))
        );
        assert!(iterm2.artifact_kinds.contains(&String::from("app")));

        let cursor = &support.supported[2];
        assert_eq!(cursor.token, "cursor");
        assert!(cursor.override_applied, "cursor applies an override");
        // Version carries the appended source revision in the real manifest.
        assert!(cursor.version.as_deref().is_some_and(|v| v.contains(',')));

        let zoom = &support.excluded[0];
        assert_eq!(zoom.token, "zoom");
        assert_eq!(zoom.reason, "native-installer");
        assert_eq!(zoom.detail, None, "excluded detail can be null");
        assert_eq!(zoom.version.as_deref(), Some("7.1.5.84650"));

        let nightly = &support.excluded[3];
        assert_eq!(nightly.token, "1password@nightly");
        assert_eq!(nightly.version.as_deref(), Some("latest"));

        let brew_nix = &support.sources["brew-nix"];
        assert_eq!(
            brew_nix.rev.as_deref(),
            Some("16131ae4126c54b1502aa7eaf6573d7fbf16b656")
        );
        assert!(
            brew_nix
                .url
                .ends_with("16131ae4126c54b1502aa7eaf6573d7fbf16b656")
        );
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
