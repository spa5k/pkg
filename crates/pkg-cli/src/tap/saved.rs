//! Saved catalog data of every registered tap.
//!
//! Query, gate, and bare-resolution paths read the locally saved catalog
//! captures; they never execute tap Ruby. Loading is strict per source:
//! when a source's saved catalog is missing or malformed, loading that
//! source fails and no query silently proceeds without it. Who loads
//! which source is the isolation rule (see the namespaced-cask-catalog
//! spec): a qualified request reads only the source it names, through
//! [`SavedCatalogs::load_one`]; bare-name resolution loads every source
//! through the Package Resolution cache, because ignoring one failed
//! source could select the wrong package.

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use crate::nix::{CatalogIndex, EntryStatus};

use super::registry::Registry;
use super::store::{self, Provenance};

/// One source's saved catalog, bound to its stable reference.
#[derive(Debug, Clone)]
pub struct SavedCatalog {
    /// The canonical `owner/tap` source identity.
    source: String,
    /// The stable moving reference installs use for this source.
    reference: String,
    /// The provenance recorded inside the active generation.
    provenance: Provenance,
    /// The immutable index shared by saved catalogs and query views.
    index: Rc<CatalogIndex>,
}

impl SavedCatalog {
    /// The canonical source identity for this saved catalog.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The stable moving reference for this source.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Provenance checked against the captured index.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// The immutable catalog index.
    #[must_use]
    pub const fn index(&self) -> &Rc<CatalogIndex> {
        &self.index
    }
}

/// Every registered source's saved catalog, loaded strictly.
#[derive(Debug, Clone, Default)]
pub struct SavedCatalogs {
    catalogs: BTreeMap<String, SavedCatalog>,
}

impl SavedCatalogs {
    /// Load one registered source's saved catalog under a shared
    /// read-only lock.
    ///
    /// The shared lock spans the provenance and index reads, so no
    /// publication exchange can land between them. Taking the lock
    /// creates no file: a source without published state fails closed
    /// instead of being skipped.
    /// # Errors
    ///
    /// Returns an error if `source` is invalid or unregistered, its generation cannot be locked or read, or its provenance and index do not match the registry.
    pub fn load_one(
        registry: &Registry,
        state_home: &Path,
        source: &str,
    ) -> Result<SavedCatalog, String> {
        let reader = store::SourceStore::open_readonly(state_home, source)?;
        if !reader.is_published() {
            return Err(format!(
                "tap source `{source}` is registered but has no published \
                 generation; run `pkg tap update {source}` to publish one"
            ));
        }
        let corrupt = |context: &str, detail: String| {
            format!(
                "tap source `{source}` {context} ({detail}); \
                 run `pkg tap update {source}` to repair it, or `pkg tap remove \
                 {source}` to withdraw it"
            )
        };
        let provenance = reader
            .read_provenance()
            .map_err(|detail| corrupt("has a corrupt saved capture", detail))?;
        if provenance.source != source {
            return Err(format!(
                "tap source `{source}` holds a generation provenanced \
                 `{}`; refusing to use it",
                provenance.source
            ));
        }
        if registry.origin(source) != Some(provenance.origin.as_str()) {
            return Err(format!(
                "tap source `{source}` was approved for origin {} but its \
                 published generation claims {}; remove and re-add the tap",
                registry.origin(source).unwrap_or("(none)"),
                provenance.origin
            ));
        }
        let index = reader
            .read_index()
            .map_err(|detail| corrupt("has a corrupt saved capture", detail))?;
        // The saved capture must still be bound to exactly the generation
        // that claims it: source, origin, revision, capture hash, one
        // target, and the entry counts. A corrupt capture fails closed
        // here, before any query or bare resolution can use it.
        let inconsistent = |detail| corrupt("holds an inconsistent saved catalog", detail);
        super::validate_binding(&index, &provenance).map_err(inconsistent)?;
        Ok(SavedCatalog {
            source: source.to_string(),
            reference: store::current_ref(state_home, source).map_err(inconsistent)?,
            provenance,
            index: Rc::new(index),
        })
    }

    /// Assemble saved catalogs that were loaded individually.
    ///
    /// Qualified request paths load one source at a time through
    /// [`SavedCatalogs::load_one`]; this constructor builds the same
    /// strict set from those loads, so a caller that needs every source
    /// (bare-name resolution) reuses the per-source errors unchanged.
    /// # Errors
    /// Returns an error when two catalogs name the same source.
    pub fn from_catalogs(catalogs: impl IntoIterator<Item = SavedCatalog>) -> Result<Self, String> {
        let mut keyed = BTreeMap::new();
        for catalog in catalogs {
            if keyed.insert(catalog.source.clone(), catalog).is_some() {
                return Err(String::from("saved catalogs contain a duplicate source"));
            }
        }
        Ok(Self { catalogs: keyed })
    }

    /// Iterate every saved catalog in source order.
    pub fn iter(&self) -> impl Iterator<Item = &SavedCatalog> {
        self.catalogs.values()
    }

    /// Every nonofficial source that carries one bare token on `system`.
    ///
    /// A match counts whatever its status is: an excluded entry still
    /// makes a bare token ambiguous, because the bare form cannot say
    /// which source the user meant.
    #[must_use]
    pub fn sources_with_token(&self, system: &str, token: &str) -> Vec<String> {
        self.iter()
            .filter(|catalog| {
                catalog
                    .index
                    .record(system, &catalog.source, token)
                    .is_some()
            })
            .map(|catalog| catalog.source.clone())
            .collect()
    }
}

/// Validate that one index envelope is bound to exactly one provenance.
///
/// This is the one binding check shared by the staged import and the
/// saved-capture load. The envelope must carry provenance for exactly the
/// provenance's source (and nothing else), pin exactly its origin and
/// revision, record the generation's capture hash in its raw input
/// provenance, target exactly one system, and hold exactly the eligible
/// and excluded entry counts the provenance claims. Staged import callers
/// additionally require the one target to be the native system.
/// # Errors
///
/// Returns an error when the source, origin, revision, capture hash, sole target or target section, or eligible and excluded entry counts disagree.
pub fn validate_binding(index: &CatalogIndex, provenance: &Provenance) -> Result<(), String> {
    let source = provenance.source.as_str();
    if index.inputs().len() != 1 || !index.inputs().contains_key(source) {
        return Err(format!(
            "the catalog must describe exactly the source {source}; it names [{}]",
            index
                .inputs()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let input = &index.inputs()[source];
    if input.url != provenance.origin {
        return Err(format!(
            "the catalog claims origin {} but the generation recorded {}",
            input.url, provenance.origin
        ));
    }
    if input.revision != provenance.revision {
        return Err(format!(
            "the catalog pins revision {} but the generation recorded {}",
            input.revision, provenance.revision
        ));
    }
    let Some(raw) = input.raw.as_ref() else {
        return Err(format!(
            "the catalog carries no raw input provenance for {source}; \
             a tap capture must record its archive and capture hashes"
        ));
    };
    if raw.capture_sha256 != provenance.metadata_sha256 {
        return Err(format!(
            "the catalog records capture hash {} but the generation recorded {}",
            raw.capture_sha256, provenance.metadata_sha256
        ));
    }
    if index.targets().len() != 1 {
        return Err(format!(
            "the catalog must target exactly one system; it targets [{}]",
            index.targets().join(", ")
        ));
    }
    let Some(section) = index.systems().get(&index.targets()[0]) else {
        return Err(format!(
            "the catalog has no section for its target {}",
            index.targets()[0]
        ));
    };
    let eligible = section
        .entries
        .values()
        .filter(|entry| entry.status == EntryStatus::Eligible)
        .count();
    let excluded = section
        .entries
        .values()
        .filter(|entry| entry.status == EntryStatus::Excluded)
        .count();
    if eligible != provenance.eligible || excluded != provenance.excluded {
        return Err(format!(
            "the catalog counts {eligible} eligible and {excluded} entries, but the \
             generation recorded {} and {}",
            provenance.eligible, provenance.excluded
        ));
    }
    Ok(())
}
