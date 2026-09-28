//! Cask catalog index access (design D6).
//!
//! Status decisions come from the generated `pkg-cask-catalog/2` index the
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

/// The ordinary package attribute one token installs through.
///
/// `info` and `install` resolve cask tokens to this attribute of the casks
/// source; the client never installs through a token-only attribute. A
/// token is one attribute segment: characters outside plain Nix identifier
/// characters (`@`, `+`, `.`) are quoted so the token never becomes a
/// nested attribute path.
#[must_use]
pub fn package_attribute(system: &str, token: &str) -> String {
    let plain = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let segment = if plain {
        token.to_string()
    } else {
        format!("\"{token}\"")
    };
    format!("packages.{system}.{segment}")
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

    /// The decoded record for one token on one system, when present.
    #[must_use]
    pub fn record(&self, system: &str, token: &str) -> Option<&CatalogEntry> {
        self.entries(system).and_then(|entries| entries.get(token))
    }

    /// Resolve the status of one token on one system.
    #[must_use]
    pub fn entry(&self, system: &str, token: &str) -> CaskStatus {
        CaskStatus::for_entry(self.record(system, token))
    }

    /// Resolve one bare name as an exact token match on one system.
    ///
    /// Bare cask resolution is fully anchored: the name must equal the token
    /// exactly. A display `name` match is deliberately not a resolution, so
    /// an unusable or ambiguous name can never be selected implicitly.
    #[must_use]
    pub fn exact_token(&self, system: &str, name: &str) -> Option<&CatalogEntry> {
        self.entries(system)?.get(name)
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
        assert_eq!(view.entry("aarch64-darwin", "iterm2"), CaskStatus::Eligible);
        assert_eq!(
            view.entry("aarch64-darwin", "zoom"),
            CaskStatus::Excluded {
                reason: String::from("installer-script"),
                detail: Some(String::from("stanza: postflight_steps")),
            }
        );
        // The same token can be excluded on one target and eligible on
        // another; per-system data decides, not the token.
        assert_eq!(
            view.entry("x86_64-linux", "iterm2"),
            CaskStatus::Excluded {
                reason: String::from("unsupported-platform"),
                detail: None,
            }
        );
        assert_eq!(view.entry("x86_64-linux", "koreader"), CaskStatus::Eligible);
        assert_eq!(
            view.entry("aarch64-darwin", "missing-token"),
            CaskStatus::Unknown
        );
        // Records keep their metadata for info, including the
        // target-effective version merge the projection applies.
        let raycast_mac = view.record("aarch64-darwin", "raycast").expect("record");
        assert_eq!(raycast_mac.version.as_deref(), Some("1.104.25"));
        let raycast_linux = view.record("x86_64-linux", "raycast").expect("record");
        assert_eq!(raycast_linux.version.as_deref(), None);
    }

    #[test]
    fn bare_resolution_matches_the_token_only() {
        let view = fixture_view();
        // Exact token match resolves.
        assert_eq!(
            view.exact_token("aarch64-darwin", "cursor")
                .map(|e| e.kind.clone()),
            Some(Some(String::from("app+cli")))
        );
        // A display name is never a bare resolution, in either case form.
        assert!(view.exact_token("aarch64-darwin", "Cursor").is_none());
        assert!(view.exact_token("aarch64-darwin", "iTerm2").is_none());
        assert!(view.exact_token("aarch64-darwin", "iterm2.app").is_none());
        // Untargeted systems have no entries at all.
        assert!(view.exact_token("x86_64-darwin", "iterm2").is_none());
    }

    #[test]
    fn tokens_install_through_one_quoted_attribute_segment() {
        assert_eq!(
            package_attribute("aarch64-darwin", "iterm2"),
            "packages.aarch64-darwin.iterm2"
        );
        // `@`, `+`, and `.` stay one segment; they never become a nested
        // attribute path.
        assert_eq!(
            package_attribute("x86_64-linux", "1password-cli@beta"),
            "packages.x86_64-linux.\"1password-cli@beta\""
        );
        assert_eq!(
            package_attribute("x86_64-linux", "xournal++"),
            "packages.x86_64-linux.\"xournal++\""
        );
        assert_eq!(
            package_attribute("aarch64-darwin", "firefox@beta"),
            "packages.aarch64-darwin.\"firefox@beta\""
        );
    }
}
