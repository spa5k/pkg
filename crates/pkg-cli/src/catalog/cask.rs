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

use std::rc::Rc;

use crate::nix::{CatalogIndex, Nix, SourceIdentity};

use super::CatalogError;

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
    /// The validated index shared with saved and live query views.
    pub index: Rc<CatalogIndex>,
}

impl CatalogView {
    /// Assemble a view from a resolved identity and a decoded index.
    ///
    /// Saved tap catalogs load from validated local captures; their
    /// identity is synthetic (the stable reference and the recorded
    /// revision), because no flake metadata was evaluated for them.
    #[must_use]
    pub fn from_parts(identity: SourceIdentity, index: CatalogIndex) -> Self {
        Self {
            identity,
            index: Rc::new(index),
        }
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
                reference: saved.reference().to_string(),
                locked_url: Some(saved.reference().to_string()),
                revision: Some(saved.provenance().revision.clone()),
                display: saved.source().to_string(),
            },
            index: saved.index().clone(),
        }
    }

    /// Load the index from `moving_source` through its locked reference.
    /// # Errors
    ///
    /// Returns an error when source identity lookup, catalog evaluation, or catalog decoding fails.
    pub fn load(nix: &Nix, moving_source: &str) -> Result<Self, CatalogError> {
        let identity = nix
            .source_identity(moving_source)
            .map_err(CatalogError::from)?;
        let eval_ref = identity
            .locked_url
            .clone()
            .unwrap_or_else(|| moving_source.to_string());
        let index = nix.catalog_index(&eval_ref).map_err(CatalogError::from)?;
        Ok(Self::from_parts(identity, index))
    }
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
    /// # Errors
    ///
    /// Returns an error when catalog loading or decoding fails.
    pub fn load(&mut self) -> Result<&CatalogView, CatalogError> {
        match &mut self.view {
            Some(view) => Ok(view),
            empty @ None => Ok(empty.insert(CatalogView::load(self.nix, self.moving)?)),
        }
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
            index: Rc::new(crate::nix::decode_catalog_index(&raw).expect("decodes fixture")),
        }
    }

    #[test]
    fn platform_capability_comes_from_the_envelope_targets() {
        let view = fixture_view();
        assert!(view.index.targeted("aarch64-darwin"));
        assert!(view.index.targeted("x86_64-linux"));
        // No client constant decides platforms anymore.
        assert!(!view.index.targeted("x86_64-darwin"));
        assert!(!view.index.targeted("aarch64-linux"));
        assert_eq!(view.index.targets(), ["aarch64-darwin", "x86_64-linux"]);
        let encoded = serde_json::to_value(view.index.as_ref()).expect("serializes");
        assert_eq!(encoded["generator"]["name"], "cask-catalog");
        assert_eq!(encoded["macosBaseline"], "15.7.7");
    }

    #[test]
    fn entries_resolve_eligible_excluded_and_unknown_tokens() {
        let view = fixture_view();
        let entry = |system, token| {
            view.index
                .record(system, OFFICIAL_CASK_SOURCE, token)
                .unwrap()
        };
        assert_eq!(
            entry("aarch64-darwin", "iterm2").status,
            crate::nix::EntryStatus::Eligible
        );
        let zoom = entry("aarch64-darwin", "zoom");
        assert_eq!(zoom.status, crate::nix::EntryStatus::Excluded);
        assert_eq!(zoom.reason.as_deref(), Some("installer-script"));
        assert_eq!(zoom.detail.as_deref(), Some("stanza: postflight_steps"));
        let iterm = entry("x86_64-linux", "iterm2");
        assert_eq!(iterm.status, crate::nix::EntryStatus::Excluded);
        assert_eq!(iterm.reason.as_deref(), Some("unsupported-platform"));
        assert_eq!(iterm.detail, None);
        assert_eq!(
            entry("x86_64-linux", "koreader").status,
            crate::nix::EntryStatus::Eligible
        );
        assert!(
            view.index
                .record("aarch64-darwin", OFFICIAL_CASK_SOURCE, "missing-token")
                .is_none()
        );
        // Records keep their metadata for info, including the
        // target-effective version merge the projection applies.
        let raycast_mac = view
            .index
            .record("aarch64-darwin", OFFICIAL_CASK_SOURCE, "raycast")
            .expect("record");
        assert_eq!(raycast_mac.version.as_deref(), Some("1.104.25"));
        let raycast_linux = view
            .index
            .record("x86_64-linux", OFFICIAL_CASK_SOURCE, "raycast")
            .expect("record");
        assert_eq!(raycast_linux.version.as_deref(), None);
    }

    #[test]
    fn bare_resolution_matches_the_token_only() {
        let view = fixture_view();
        // The official source carries the token; the id stays unique.
        assert_eq!(
            view.index.sources_with_token("aarch64-darwin", "cursor"),
            [String::from("homebrew/cask")]
        );
        // A display name is never a bare token match, in either case form.
        assert_eq!(
            view.index.sources_with_token("aarch64-darwin", "Cursor"),
            [] as [String; 0]
        );
        assert_eq!(
            view.index.sources_with_token("aarch64-darwin", "iTerm2"),
            [] as [String; 0]
        );
        assert_eq!(
            view.index
                .sources_with_token("aarch64-darwin", "iterm2.app"),
            [] as [String; 0]
        );
        // Untargeted systems have no entries at all.
        assert_eq!(
            view.index.sources_with_token("x86_64-darwin", "iterm2"),
            [] as [String; 0]
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
