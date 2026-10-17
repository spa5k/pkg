//! Map catalog sources to install references.
//!
//! [`Routing`] selects the configured casks flake for `homebrew/cask` and
//! a stable `current` path for each imported tap. Queries, eligibility checks,
//! and installation use these references.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Sources;
use crate::nix::OFFICIAL_CASK_SOURCE;
use crate::tap::{registry::Registry, store};

/// The references behind every catalog source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    /// The configured source references.
    pub sources: Sources,
    /// One stable `current` path reference per imported tap, keyed by
    /// source identity.
    pub taps: BTreeMap<String, String>,
}

impl Routing {
    /// Assemble routing from explicit references.
    #[must_use]
    pub fn new(sources: &Sources, taps: BTreeMap<String, String>) -> Self {
        Self {
            sources: sources.clone(),
            taps,
        }
    }

    /// Assemble routing from the configuration and the loaded tap registry.
    ///
    /// Registry sources are strictly validated already; a source whose
    /// reference cannot be formed fails the whole routing instead of
    /// silently routing an invalid source.
    /// # Errors
    ///
    /// Returns an error when a registry source cannot be converted to its stable local reference.
    pub fn from_registry(
        sources: &Sources,
        registry: &Registry,
        state_home: &Path,
    ) -> Result<Self, String> {
        let mut taps = BTreeMap::new();
        for source in registry.sources.keys() {
            let reference = store::current_ref(state_home, source)?;
            taps.insert(source.clone(), reference);
        }
        Ok(Self::new(sources, taps))
    }

    /// The moving reference one cask source installs from.
    ///
    /// An unknown tap source is refused with the `pkg tap add` instruction;
    /// pkg never reinterprets an unknown source as Nix code.
    /// # Errors
    ///
    /// Returns an error when `source` is not the official cask source or a registered tap.
    pub fn cask_reference(&self, source: &str) -> Result<String, String> {
        if source == OFFICIAL_CASK_SOURCE {
            Ok(self.sources.casks.clone())
        } else if let Some(reference) = self.taps.get(source) {
            Ok(reference.clone())
        } else {
            Err(format!(
                "unknown cask source `{source}`; \
                 add it first with `pkg tap add {source}`"
            ))
        }
    }
}
