//! Source routing: which reference answers each catalog source.
//!
//! One place maps a catalog source identity to the reference pkg installs
//! from: the configured casks flake for `homebrew/cask`, and one stable
//! per-tap `current` path reference for every locally imported tap. Every
//! query, gate, and install derives its reference through [`Routing`], so
//! no command repeats its own routing glue.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Sources;
use crate::nix::OFFICIAL_CASK_SOURCE;
use crate::tap::{registry::Registry, store};

/// The references behind every catalog source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    /// The configured Nixpkgs flake reference.
    pub nixpkgs: String,
    /// The configured official casks flake reference.
    pub casks: String,
    /// One stable `current` path reference per imported tap, keyed by
    /// source identity.
    pub taps: BTreeMap<String, String>,
}

impl Routing {
    /// Assemble routing from explicit references.
    #[must_use]
    pub fn new(sources: &Sources, taps: BTreeMap<String, String>) -> Self {
        Self {
            nixpkgs: sources.nixpkgs.clone(),
            casks: sources.casks.clone(),
            taps,
        }
    }

    /// Assemble routing from the configuration and the loaded tap registry.
    ///
    /// Registry sources are strictly validated already; a source whose
    /// reference cannot be formed fails the whole routing instead of
    /// silently routing an invalid source.
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
    pub fn cask_reference(&self, source: &str) -> Result<String, String> {
        if source == OFFICIAL_CASK_SOURCE {
            Ok(self.casks.clone())
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
