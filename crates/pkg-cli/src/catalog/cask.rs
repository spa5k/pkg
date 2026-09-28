//! Cask support classification (design D6).
//!
//! Decisions here come from the `pkg-cask-support/1` manifest the casks
//! flake exports, evaluated from the same locked source native search
//! uses, so support decisions and search rows always describe one source
//! revision. The cask source is macOS-only; that platform rule also lives
//! here.

use crate::nix::{CaskSupport, Nix, SourceIdentity};

use super::CatalogError;

/// The only system the cask source supports (design D6).
pub const CASK_SYSTEM: &str = "aarch64-darwin";

/// Whether the cask source can be queried on `system`.
#[must_use]
pub fn casks_supported_on(system: &str) -> bool {
    system == CASK_SYSTEM
}

/// Support status for one cask token on one system (design D6).
///
/// Derived from the `pkg-cask-support/1` manifest the casks flake exports.
/// The manifest is evaluated from the locked reference of the same moving
/// source that native search uses, so support decisions and search results
/// always describe one source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaskStatus {
    /// The token is supported on this system.
    Supported {
        /// The package kind, for example `app` or `app+cli`.
        kind: String,
        /// The upstream version, when the flake interface provides one.
        version: Option<String>,
        /// The upstream homepage, when the flake interface provides one.
        homepage: Option<String>,
        /// The artifact kinds the package provides.
        artifact_kinds: Vec<String>,
        /// Whether an override is applied on top of upstream.
        override_applied: bool,
        /// The locked revision of the casks source this decision came from.
        source_revision: Option<String>,
    },
    /// The token is excluded on this system, with the recorded reason.
    Excluded {
        /// The upstream version that was excluded, when known.
        version: Option<String>,
        /// A short machine-readable reason.
        reason: String,
        /// Human-readable detail for the exclusion, when present.
        detail: Option<String>,
        /// The locked revision of the casks source this decision came from.
        source_revision: Option<String>,
    },
    /// The token is absent from the manifest for this system.
    Unlisted {
        /// The cask token.
        token: String,
    },
}

/// Classify one token against a decoded cask support manifest.
///
/// Exclusions win over support so a token listed in both is refused.
/// `source_revision` is attached to the decision as given: search rows
/// classify without one, live cask status resolves attach the locked
/// revision of the evaluated manifest.
#[must_use]
pub fn classify_cask(
    support: &CaskSupport,
    token: &str,
    source_revision: Option<&str>,
) -> CaskStatus {
    let source_revision = source_revision.map(ToString::to_string);
    if let Some(excluded) = support.excluded.iter().find(|e| e.token == token) {
        return CaskStatus::Excluded {
            version: excluded.version.clone(),
            reason: excluded.reason.clone(),
            detail: excluded.detail.clone(),
            source_revision,
        };
    }
    if let Some(supported) = support.supported.iter().find(|s| s.token == token) {
        return CaskStatus::Supported {
            kind: supported.kind.clone(),
            version: supported.version.clone(),
            homepage: supported.homepage.clone(),
            artifact_kinds: supported.artifact_kinds.clone(),
            override_applied: supported.override_applied,
            source_revision,
        };
    }
    CaskStatus::Unlisted {
        token: token.to_string(),
    }
}

/// One cask support decision with the source identity it came from.
///
/// The identity is the same locked source any metadata lookup for the same
/// info result must use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaskLookup {
    /// The resolved identity of the casks source.
    pub identity: SourceIdentity,
    /// The classification for the token.
    pub status: CaskStatus,
}

/// Resolve cask support status for `token` on a cask-capable `system`.
///
/// `source` must be the configured casks source reference; the manifest is
/// evaluated from its locked reference and the locked revision recorded by
/// `nix flake metadata` is attached to the decision.
pub fn cask_status(
    nix: &Nix,
    source: &str,
    system: &str,
    token: &str,
) -> Result<CaskLookup, CatalogError> {
    let identity = nix.source_identity(source).map_err(CatalogError::from)?;
    let eval_ref = identity
        .locked_url
        .clone()
        .unwrap_or_else(|| source.to_string());
    let support = nix
        .cask_support(&eval_ref, system)
        .map_err(CatalogError::from)?;
    let status = classify_cask(&support, token, identity.revision.as_deref());
    Ok(CaskLookup { identity, status })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classification runs against the actual generated manifest fixture,
    /// not an invented shape.
    #[test]
    fn classifies_cask_support_from_the_real_fixture() {
        let manifest = include_str!("../../tests/fixtures/cask-support.json");
        let support: crate::nix::CaskSupport =
            serde_json::from_str(manifest).expect("decodes pkg-cask-support/1");
        match classify_cask(&support, "iterm2", None) {
            CaskStatus::Supported {
                kind,
                override_applied,
                ..
            } => {
                assert_eq!(kind, "app");
                assert!(!override_applied);
            }
            other => panic!("iterm2 must be supported: {other:?}"),
        }
        match classify_cask(&support, "cursor", None) {
            CaskStatus::Supported {
                override_applied, ..
            } => assert!(override_applied),
            other => panic!("cursor must be supported: {other:?}"),
        }
        match classify_cask(&support, "zoom", None) {
            CaskStatus::Excluded {
                reason,
                detail,
                version,
                ..
            } => {
                assert_eq!(reason, "native-installer");
                assert_eq!(detail, None);
                assert_eq!(version.as_deref(), Some("7.1.5.84650"));
            }
            other => panic!("zoom must be excluded: {other:?}"),
        }
        // The real current manifest excludes raycast on-device with a
        // recorded reason and detail (manager verification).
        match classify_cask(&support, "raycast", None) {
            CaskStatus::Excluded {
                reason,
                detail,
                version,
                ..
            } => {
                assert_eq!(reason, "minimum-os");
                assert!(detail.as_deref().is_some_and(|d| !d.is_empty()));
                assert_eq!(version.as_deref(), Some("2.0.5.0"));
            }
            other => panic!("raycast must be excluded: {other:?}"),
        }
        assert_eq!(
            classify_cask(&support, "missing-token", None),
            CaskStatus::Unlisted {
                token: String::from("missing-token")
            }
        );
        // A resolved decision carries the locked revision of the manifest
        // source; a badge classification does not.
        let revision = "16131ae4126c54b1502aa7eaf6573d7fbf16b656";
        match classify_cask(&support, "iterm2", Some(revision)) {
            CaskStatus::Supported {
                source_revision, ..
            } => assert_eq!(source_revision.as_deref(), Some(revision)),
            other => panic!("iterm2 must be supported: {other:?}"),
        }
        // Unlisted tokens never get a badge in search rows; excluded tokens
        // carry their reason and (nullable) detail. Keys arrive in native
        // full-attribute form; the exposed ID is normalized.
        use super::super::search::{SourceKind, SupportBadge, rows_from};
        let mut results = std::collections::BTreeMap::new();
        results.insert(
            String::from("packages.aarch64-darwin.zoom"),
            crate::nix::SearchMeta {
                pname: String::from("zoom"),
                version: String::from("7.1.5.84650"),
                description: String::new(),
            },
        );
        results.insert(
            String::from("packages.aarch64-darwin.missing-token"),
            crate::nix::SearchMeta {
                pname: String::from("missing-token"),
                version: String::from("1"),
                description: String::new(),
            },
        );
        let rows = rows_from(
            &results,
            SourceKind::Cask,
            "github:spa5k/pkg/rev",
            &Some(String::from("rev")),
            CASK_SYSTEM,
            false,
            &Some(support),
        );
        let zoom_row = rows
            .iter()
            .find(|row| row.id == "cask:zoom")
            .expect("zoom row with normalized ID");
        assert_eq!(zoom_row.attribute, "packages.aarch64-darwin.zoom");
        assert_eq!(
            zoom_row.support,
            Some(SupportBadge::Excluded {
                reason: String::from("native-installer"),
                detail: None,
            })
        );
        let missing_row = rows
            .iter()
            .find(|row| row.id == "cask:missing-token")
            .expect("missing row");
        assert_eq!(missing_row.support, None);
    }

    #[test]
    fn cask_platform_capability_is_explicit() {
        assert!(casks_supported_on("aarch64-darwin"));
        assert!(!casks_supported_on("x86_64-darwin"));
        assert!(!casks_supported_on("x86_64-linux"));
    }
}
