//! Read approved sources and their saved catalogs without starting an importer.

use super::{self as tap, Registry, SavedCatalog, SavedCatalogs, registry, store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One registry snapshot with catalogs loaded on demand.
pub struct Catalogs {
    registry: Registry,
    state_home: PathBuf,
    saved: BTreeMap<String, SavedCatalog>,
}

impl Catalogs {
    pub fn load(state_home: &Path) -> Result<Self, String> {
        let registry = Registry::load(&registry::registry_path(state_home))?;
        Ok(Self::from_registry(registry, state_home))
    }

    pub fn from_registry(registry: Registry, state_home: &Path) -> Self {
        Self {
            registry,
            state_home: state_home.to_owned(),
            saved: BTreeMap::new(),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn withdrawn_source(&self, reference: &str) -> Option<String> {
        store::source_of_reference(&self.state_home, reference).filter(|source| {
            source != crate::nix::OFFICIAL_CASK_SOURCE && self.registry.origin(source).is_none()
        })
    }

    /// Load exactly one approved source. Unrelated broken sources do not fail it.
    pub fn saved_one(&mut self, source: &str) -> Result<&SavedCatalog, String> {
        use std::collections::btree_map::Entry;
        match self.saved.entry(source.to_owned()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                if !self.registry.sources.contains_key(source) {
                    return Err(format!(
                        "unknown cask source `{source}`; add it first with `pkg tap add {source}`"
                    ));
                }
                let catalog = SavedCatalogs::load_one(&self.registry, &self.state_home, source)?;
                Ok(entry.insert(catalog))
            }
        }
    }

    /// Bare names need all approved sources to avoid hiding an ambiguous match.
    pub fn saved_all(&mut self) -> Result<SavedCatalogs, String> {
        let sources: Vec<_> = self.registry.sources.keys().cloned().collect();
        for source in sources {
            self.saved_one(&source)?;
        }
        SavedCatalogs::from_catalogs(self.saved.values().cloned())
    }
}

#[cfg(test)]
mod tests;

/// One `pkg tap list` row.
#[derive(serde::Serialize)]
pub(super) struct TapRow {
    pub(super) source: String,
    pub(super) origin: String,
    pub(super) revision: Option<String>,
    pub(super) eligible: Option<usize>,
    pub(super) excluded: Option<usize>,
    pub(super) added_unix: u64,
    pub(super) error: Option<String>,
}

pub(super) fn rows(state_home: &Path) -> Result<Vec<TapRow>, String> {
    let registry_path = registry::registry_path(state_home);
    let registry = registry::Registry::load(&registry_path)?;
    let rows = registry
        .sources
        .iter()
        .map(|(source, entry)| {
            let state = tap::SavedCatalogs::load_one(&registry, state_home, source);
            let provenance = state.as_ref().ok().map(tap::SavedCatalog::provenance);
            TapRow {
                source: source.clone(),
                origin: entry.origin.clone(),
                revision: provenance.map(|value| value.revision.clone()),
                eligible: provenance.map(|value| value.eligible),
                excluded: provenance.map(|value| value.excluded),
                added_unix: entry.added_unix,
                error: state.err(),
            }
        })
        .collect();
    Ok(rows)
}

pub fn health(state_home: &Path) -> Vec<crate::output::DoctorRow> {
    let observed = |component: &str, status: &str, detail: String| crate::output::DoctorRow {
        component: component.to_owned(),
        status: status.to_owned(),
        detail: Some(detail),
    };
    let taps = match Catalogs::load(state_home) {
        Ok(taps) => taps,
        Err(error) => return vec![observed("tap registry", "unreadable", error)],
    };
    if taps.registry().sources.is_empty() {
        return vec![observed("taps", "none", String::from("no saved taps"))];
    }
    taps.registry()
        .sources
        .keys()
        .map(|source| {
            let component = format!("tap {source}");
            match SavedCatalogs::load_one(taps.registry(), state_home, source) {
                Ok(catalog) => observed(
                    &component,
                    "ok",
                    format!(
                        "{} entries saved at revision {}",
                        catalog.provenance().eligible + catalog.provenance().excluded,
                        catalog.provenance().revision,
                    ),
                ),
                Err(error) => observed(&component, "unhealthy", error),
            }
        })
        .collect()
}
