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

use super::index::split_entry_id;

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

#[cfg(test)]
mod tests {
    use super::super::reference::is_fixed_reference;
    use super::*;

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
