//! Cask catalog index access (design D6).
//!
//! Status decisions come from the generated `pkg-cask-catalog/4` index the
//! casks flake exposes as `catalogIndex`, evaluated from the same locked
//! source reference that search rows use, so status and rows always describe
//! one source revision. The client classifies nothing itself: eligibility,
//! exclusion reasons, and details are generated data. Malformed raw
//! Homebrew records are already isolated by the generator as excluded
//! entries; this module holds no second classifier.
//!
//! Platform capability also comes from the envelope: `targets` is read
//! before any per-system data, so a system outside the target list is
//! platform-skipped from the envelope alone and an evaluation failure is
//! never mistaken for a platform skip.

use std::collections::BTreeMap;

use crate::nix::{CatalogEntry, CatalogIndex, EntryStatus, Nix, SourceIdentity};

use super::CatalogError;

/// The flake attribute that carries the catalog index envelope.
pub const INDEX_ATTRIBUTE: &str = "catalogIndex";

/// The install attribute for one full cask entry id on one system.
///
/// `info` and `install` resolve cask identities to this attribute of the
/// casks source. The full entry id (`owner/tap/token`) is encoded by the
/// shared backend helper (`_` becomes `_u_` and `.` becomes `_d_`, one
/// pass) and quoted as one attribute segment, so the id never becomes a
/// nested attribute path. The client never duplicates the encoding
/// algorithm: `cask_catalog::package_attribute` is the one source of it.
#[must_use]
pub fn package_attribute(system: &str, full_id: &str) -> String {
    format!(
        "packages.{system}.\"{}\"",
        cask_catalog::package_attribute(full_id)
    )
}

/// The same attribute as native profile manifests spell it: unquoted.
///
/// `nix profile` records the attribute path without the quotes, for
/// example `packages.x86_64-linux.example/tap/tool@1_d_2`. Installed
/// matching compares against this normalized native form.
#[must_use]
pub fn native_package_attribute(system: &str, full_id: &str) -> String {
    format!(
        "packages.{system}.{}",
        cask_catalog::package_attribute(full_id)
    )
}

/// Status for one cask token on one system (design D6).
///
/// Derived from the generated index envelope: `eligible` is a metadata
/// claim of the generator, never a verification promise; excluded tokens
/// carry their recorded reason; a token absent from the index is unknown
/// and never becomes eligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaskStatus {
    /// The token is eligible on this system.
    Eligible,
    /// The token is excluded on this system, with the recorded reason.
    Excluded {
        /// A short machine-readable reason.
        reason: String,
        /// Human-readable detail for the exclusion, when present.
        detail: Option<String>,
    },
    /// The token is absent from the index for this system.
    Unknown,
}

impl CaskStatus {
    /// The status of one decoded entry, when present.
    #[must_use]
    pub fn for_entry(entry: Option<&CatalogEntry>) -> Self {
        match entry {
            None => Self::Unknown,
            Some(entry) => match entry.status {
                EntryStatus::Eligible => Self::Eligible,
                EntryStatus::Excluded => Self::Excluded {
                    reason: entry.reason.clone().unwrap_or_default(),
                    detail: entry.detail.clone(),
                },
            },
        }
    }
}

/// One loaded catalog index bound to the source identity it came from.
///
/// The identity is the same locked source any metadata lookup for the same
/// result must use; installs keep using the moving reference. Loading
/// evaluates only the generated index attribute; no package derivation is
/// forced.
#[derive(Debug, Clone)]
pub struct CatalogView {
    /// The resolved identity of the casks source.
    pub identity: SourceIdentity,
    index: CatalogIndex,
}

impl CatalogView {
    /// Assemble a view from a resolved identity and a decoded index.
    ///
    /// Saved tap catalogs load from validated local captures; their
    /// identity is synthetic (the stable reference and the recorded
    /// revision), because no flake metadata was evaluated for them.
    #[must_use]
    pub fn from_parts(identity: SourceIdentity, index: CatalogIndex) -> Self {
        Self { identity, index }
    }

    /// Assemble a view over one saved tap catalog.
    ///
    /// The identity is synthetic: the stable per-source reference, the
    /// recorded revision, and the source identity as the display name.
    /// Status and records come from the validated capture; no Nix call
    /// and no tap Ruby ever runs for this view.
    #[must_use]
    pub fn from_saved(saved: &crate::tap::SavedCatalog) -> Self {
        Self {
            identity: SourceIdentity {
                reference: saved.reference.clone(),
                locked_url: Some(saved.reference.clone()),
                revision: Some(saved.provenance.revision.clone()),
                display: saved.source.clone(),
            },
            index: saved.index.clone(),
        }
    }

    /// Load the index from `moving_source` through its locked reference.
    pub fn load(nix: &Nix, moving_source: &str) -> Result<Self, CatalogError> {
        let identity = nix
            .source_identity(moving_source)
            .map_err(CatalogError::from)?;
        let eval_ref = identity
            .locked_url
            .clone()
            .unwrap_or_else(|| moving_source.to_string());
        let index = nix.catalog_index(&eval_ref).map_err(CatalogError::from)?;
        Ok(Self { identity, index })
    }

    /// The decoded index envelope.
    #[must_use]
    pub fn index(&self) -> &CatalogIndex {
        &self.index
    }

    /// The target systems declared by the envelope.
    #[must_use]
    pub fn targets(&self) -> &[String] {
        &self.index.targets
    }

    /// Whether the envelope targets `system`.
    ///
    /// This is the platform rule; it is read from the envelope, not from a
    /// client constant.
    #[must_use]
    pub fn targeted(&self, system: &str) -> bool {
        self.index.targets.iter().any(|target| target == system)
    }

    /// The status entries for one system, when the system is targeted.
    #[must_use]
    pub fn entries(&self, system: &str) -> Option<&BTreeMap<String, CatalogEntry>> {
        self.index
            .systems
            .get(system)
            .map(|section| &section.entries)
    }

    /// The decoded record for one source and token on one system.
    ///
    /// Keys are full entry identities; the record is found by matching
    /// both its `source` and its bare `token`.
    #[must_use]
    pub fn record(&self, system: &str, source: &str, token: &str) -> Option<&CatalogEntry> {
        self.entries(system)?
            .values()
            .find(|entry| entry.source == source && entry.token == token)
    }

    /// Resolve the status of one source and token on one system.
    #[must_use]
    pub fn entry(&self, system: &str, source: &str, token: &str) -> CaskStatus {
        CaskStatus::for_entry(self.record(system, source, token))
    }

    /// Every source in this catalog that carries one bare token.
    ///
    /// A match counts whatever its status is: an excluded entry still
    /// makes a bare token ambiguous across sources.
    #[must_use]
    pub fn sources_with_token(&self, system: &str, token: &str) -> Vec<String> {
        sources_with_token(&self.index, system, token)
    }
}

/// The decoded record for one source and token in one index.
///
/// Keys are full entry identities; the record is found by matching both
/// its `source` and its bare `token`.
#[must_use]
pub fn record<'a>(
    index: &'a CatalogIndex,
    system: &str,
    source: &str,
    token: &str,
) -> Option<&'a CatalogEntry> {
    index
        .systems
        .get(system)?
        .entries
        .values()
        .find(|entry| entry.source == source && entry.token == token)
}

/// The status of one source and token in one index.
#[must_use]
pub fn status(index: &CatalogIndex, system: &str, source: &str, token: &str) -> CaskStatus {
    CaskStatus::for_entry(record(index, system, source, token))
}

/// Every source in one index that carries one bare token on one system.
///
/// A match counts whatever its status is: an excluded entry still makes a
/// bare token ambiguous across sources. The result is sorted and unique.
#[must_use]
pub fn sources_with_token(index: &CatalogIndex, system: &str, token: &str) -> Vec<String> {
    let mut sources: Vec<String> = index
        .systems
        .get(system)
        .map(|section| {
            section
                .entries
                .values()
                .filter(|entry| entry.token == token)
                .map(|entry| entry.source.clone())
                .collect()
        })
        .unwrap_or_default();
    sources.sort();
    sources.dedup();
    sources
}

/// A catalog index loaded at most once per command run.
///
/// Discovery, gating, and info share one evaluation of the index attribute
/// per query, even when one command touches several cask tokens.
pub struct CatalogOnce<'a> {
    nix: &'a Nix,
    moving: &'a str,
    view: Option<CatalogView>,
}

impl<'a> CatalogOnce<'a> {
    /// Start a lazily loading handle for one casks source.
    #[must_use]
    pub fn new(nix: &'a Nix, moving: &'a str) -> Self {
        Self {
            nix,
            moving,
            view: None,
        }
    }

    /// Load the view once and return it for every later call.
    pub fn load(&mut self) -> Result<&CatalogView, CatalogError> {
        if self.view.is_none() {
            let view = CatalogView::load(self.nix, self.moving)?;
            self.view = Some(view);
        }
        let Some(view) = self.view.as_ref() else {
            // Unreachable by construction: the branch above stored a view.
            return Err(CatalogError::Source(String::from(
                "the catalog index view was not loaded",
            )));
        };
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nix::OFFICIAL_CASK_SOURCE;

    /// Decode the fixture once for the tests below: an actual
    /// generated-catalog slice projected to the `catalogIndex` shape.
    fn fixture_view() -> CatalogView {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json"))
                .expect("valid json");
        CatalogView {
            identity: SourceIdentity {
                reference: String::from("github:spa5k/pkg/main?dir=nix/casks"),
                locked_url: Some(String::from(
                    "github:spa5k/pkg/2403040105060708090a0b0c0d0e0f1011121314?dir=nix/casks",
                )),
                revision: Some(String::from("2403040105060708090a0b0c0d0e0f1011121314")),
                display: String::from("github:spa5k/pkg"),
            },
            index: crate::nix::decode_catalog_index(&raw).expect("decodes fixture"),
        }
    }

    #[test]
    fn platform_capability_comes_from_the_envelope_targets() {
        let view = fixture_view();
        assert!(view.targeted("aarch64-darwin"));
        assert!(view.targeted("x86_64-linux"));
        // No client constant decides platforms anymore.
        assert!(!view.targeted("x86_64-darwin"));
        assert!(!view.targeted("aarch64-linux"));
        assert_eq!(view.targets(), ["aarch64-darwin", "x86_64-linux"]);
        assert_eq!(view.index().generator.name, "cask-catalog");
        assert_eq!(view.index().macos_baseline, "15.7.7");
    }

    #[test]
    fn entries_resolve_eligible_excluded_and_unknown_tokens() {
        let view = fixture_view();
        assert_eq!(
            view.entry("aarch64-darwin", OFFICIAL_CASK_SOURCE, "iterm2"),
            CaskStatus::Eligible
        );
        assert_eq!(
            view.entry("aarch64-darwin", OFFICIAL_CASK_SOURCE, "zoom"),
            CaskStatus::Excluded {
                reason: String::from("installer-script"),
                detail: Some(String::from("stanza: postflight_steps")),
            }
        );
        // The same token can be excluded on one target and eligible on
        // another; per-system data decides, not the token.
        assert_eq!(
            view.entry("x86_64-linux", OFFICIAL_CASK_SOURCE, "iterm2"),
            CaskStatus::Excluded {
                reason: String::from("unsupported-platform"),
                detail: None,
            }
        );
        assert_eq!(
            view.entry("x86_64-linux", OFFICIAL_CASK_SOURCE, "koreader"),
            CaskStatus::Eligible
        );
        assert_eq!(
            view.entry("aarch64-darwin", OFFICIAL_CASK_SOURCE, "missing-token"),
            CaskStatus::Unknown
        );
        // Records keep their metadata for info, including the
        // target-effective version merge the projection applies.
        let raycast_mac = view
            .record("aarch64-darwin", OFFICIAL_CASK_SOURCE, "raycast")
            .expect("record");
        assert_eq!(raycast_mac.version.as_deref(), Some("1.104.25"));
        let raycast_linux = view
            .record("x86_64-linux", OFFICIAL_CASK_SOURCE, "raycast")
            .expect("record");
        assert_eq!(raycast_linux.version.as_deref(), None);
    }

    #[test]
    fn bare_resolution_matches_the_token_only() {
        let view = fixture_view();
        // The official source carries the token; the id stays unique.
        assert_eq!(
            view.sources_with_token("aarch64-darwin", "cursor"),
            [String::from("homebrew/cask")]
        );
        // A display name is never a bare token match, in either case form.
        assert!(
            view.sources_with_token("aarch64-darwin", "Cursor")
                .is_empty()
        );
        assert!(
            view.sources_with_token("aarch64-darwin", "iTerm2")
                .is_empty()
        );
        assert!(
            view.sources_with_token("aarch64-darwin", "iterm2.app")
                .is_empty()
        );
        // Untargeted systems have no entries at all.
        assert!(
            view.sources_with_token("x86_64-darwin", "iterm2")
                .is_empty()
        );
    }

    #[test]
    fn full_ids_install_through_one_encoded_quoted_segment() {
        assert_eq!(
            package_attribute("aarch64-darwin", "homebrew/cask/iterm2"),
            "packages.aarch64-darwin.\"homebrew/cask/iterm2\""
        );
        // Dots in the tap or the token encode to `_d_`, underscores to
        // `_u_`, and `@` passes through: the full id stays exactly one
        // quoted segment.
        assert_eq!(
            package_attribute("x86_64-linux", "example/tap/tool@1.2"),
            "packages.x86_64-linux.\"example/tap/tool@1_d_2\""
        );
        assert_eq!(
            package_attribute("aarch64-darwin", "a_b.c/d.e/f_g.h"),
            "packages.aarch64-darwin.\"a_u_b_d_c/d_d_e/f_u_g_d_h\""
        );
        // Native manifests spell the same attribute without quotes.
        assert_eq!(
            native_package_attribute("x86_64-linux", "example/tap/tool@1.2"),
            "packages.x86_64-linux.example/tap/tool@1_d_2"
        );
    }

    #[test]
    fn encoded_segments_round_trip_collision_free() {
        // Dots, underscores, `@`, and a literal `_d_` in the id all round
        // trip through the shared encoder and its strict inverse; no two
        // distinct ids share one encoding.
        for id in [
            "homebrew/cask/iterm2",
            "example/tap/tool@1.2",
            "a_b.c/d.e/f_g.h",
            "some/tap/_d_",
            "some/tap/x_u_y",
        ] {
            let encoded = cask_catalog::package_attribute(id);
            assert_eq!(
                cask_catalog::decode_package_attribute(&encoded).ok(),
                Some(id.to_string())
            );
        }
        // The literal id `_d_` encodes to `_u_d_u_`, never to the encoding
        // of `.`, and an unknown escape never silently decodes.
        assert_eq!(cask_catalog::package_attribute("a/b/_d_"), "a/b/_u_d_u_");
        assert!(cask_catalog::decode_package_attribute("a/b/_x_").is_err());
    }
}
