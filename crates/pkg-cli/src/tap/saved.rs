//! Saved catalog data of every registered tap.
//!
//! Query, gate, and bare-resolution paths read the locally saved catalog
//! captures; they never execute tap Ruby. Loading is strict per source:
//! when a source's saved catalog is missing or malformed, loading that
//! source fails and no query silently proceeds without it. Who loads
//! which source is the isolation rule (see the namespaced-cask-catalog
//! spec): a qualified request reads only the source it names, through
//! [`SavedCatalogs::load_one`]; bare-name resolution loads every source
//! through [`SavedCatalogs::load`], because ignoring one failed source
//! could select the wrong package.

use std::collections::BTreeMap;
use std::path::Path;

use crate::nix::{CatalogIndex, EntryStatus, OFFICIAL_CASK_SOURCE};

use super::registry::{self, Registry};
use super::store::{self, Provenance};

/// One source's saved catalog, bound to its stable reference.
#[derive(Debug, Clone)]
pub struct SavedCatalog {
    /// The canonical `owner/tap` source identity.
    pub source: String,
    /// The stable moving reference installs use for this source.
    pub reference: String,
    /// The provenance recorded inside the active generation.
    pub provenance: Provenance,
    /// The strictly decoded index envelope.
    pub index: CatalogIndex,
}

/// Every registered source's saved catalog, loaded strictly.
#[derive(Debug, Clone, Default)]
pub struct SavedCatalogs {
    catalogs: BTreeMap<String, SavedCatalog>,
}

impl SavedCatalogs {
    /// Load the saved catalog of every registered source.
    ///
    /// One unreadable or malformed capture fails the whole load: a query
    /// must never skip a failed source and answer from the rest.
    pub fn load(registry: &Registry, state_home: &Path) -> Result<Self, String> {
        let mut catalogs = BTreeMap::new();
        for source in registry.sources.keys() {
            let catalog = Self::load_one(registry, state_home, source)?;
            catalogs.insert(source.clone(), catalog);
        }
        Ok(Self { catalogs })
    }

    /// Load one registered source's saved catalog under a shared
    /// read-only lock.
    ///
    /// The shared lock spans the provenance and index reads, so no
    /// publication exchange can land between them. Taking the lock
    /// creates no file: a source without published state fails closed
    /// instead of being skipped.
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
        super::validate_binding(&index, &provenance)
            .map_err(|detail| corrupt("holds an inconsistent saved catalog", detail))?;
        let reference = store::current_ref(state_home, source)?;
        Ok(SavedCatalog {
            source: source.to_string(),
            reference,
            provenance,
            index,
        })
    }

    /// Assemble saved catalogs that were loaded individually.
    ///
    /// Qualified request paths load one source at a time through
    /// [`SavedCatalogs::load_one`]; this constructor builds the same
    /// strict set from those loads, so a caller that needs every source
    /// (bare-name resolution) reuses the per-source errors unchanged.
    #[must_use]
    pub fn from_catalogs(catalogs: BTreeMap<String, SavedCatalog>) -> Self {
        Self { catalogs }
    }

    /// Iterate every saved catalog in source order.
    pub fn iter(&self) -> impl Iterator<Item = &SavedCatalog> {
        self.catalogs.values()
    }

    /// One source's saved catalog, when it is registered.
    #[must_use]
    pub fn get(&self, source: &str) -> Option<&SavedCatalog> {
        self.catalogs.get(source)
    }

    /// Every nonofficial source that carries one bare token on `system`.
    ///
    /// A match counts whatever its status is: an excluded entry still
    /// makes a bare token ambiguous, because the bare form cannot say
    /// which source the user meant.
    #[must_use]
    pub fn sources_with_token(&self, system: &str, token: &str) -> Vec<String> {
        self.iter()
            .filter(|catalog| catalog.index.targets.iter().any(|target| target == system))
            .filter(|catalog| {
                catalog.index.systems.get(system).is_some_and(|section| {
                    section.entries.values().any(|entry| entry.token == token)
                })
            })
            .map(|catalog| catalog.source.clone())
            .collect()
    }
}

/// Load the registry and every saved catalog in one strict step.
pub fn load_all(state_home: &Path) -> Result<(Registry, SavedCatalogs), String> {
    let registry = Registry::load(&registry::registry_path(state_home))?;
    let saved = SavedCatalogs::load(&registry, state_home)?;
    Ok((registry, saved))
}

/// Explain a bare token that is ambiguous across cask sources.
///
/// The official source and every tap count, including excluded entries;
/// `cask:owner/tap/token` is the unambiguous form.
#[must_use]
pub fn bare_ambiguity(name: &str, mut sources: Vec<String>) -> String {
    sources.sort();
    format!(
        "`{name}` matches more than one cask source ({}); \
         use the full ID `cask:owner/tap/{name}`",
        sources
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Guard used by queries: the official source never registers as a tap.
#[must_use]
pub fn is_official(source: &str) -> bool {
    source == OFFICIAL_CASK_SOURCE
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
pub fn validate_binding(index: &CatalogIndex, provenance: &Provenance) -> Result<(), String> {
    let source = provenance.source.as_str();
    if index.inputs.len() != 1 || !index.inputs.contains_key(source) {
        return Err(format!(
            "the catalog must describe exactly the source {source}; it names [{}]",
            index.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    let input = &index.inputs[source];
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
    if index.targets.len() != 1 {
        return Err(format!(
            "the catalog must target exactly one system; it targets [{}]",
            index.targets.join(", ")
        ));
    }
    let Some(section) = index.systems.get(&index.targets[0]) else {
        return Err(format!(
            "the catalog has no section for its target {}",
            index.targets[0]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::store::SourceStore;

    fn index_for(revision: &str) -> crate::nix::CatalogIndex {
        let raw = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"somebody/apps": {
                "url": "https://github.com/somebody/homebrew-apps",
                "revision": revision,
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license",
                "raw": {
                    "archiveUrl": format!(
                        "https://api.github.com/repos/somebody/homebrew-apps/tarball/{revision}"),
                    "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                    "captureSha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                    "envelopeSchema": "pkg-tap-raw-export/1"}}},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "somebody/apps/tool": {
                    "source": "somebody/apps", "token": "tool", "name": null,
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary", "reason": null, "detail": null}}}}
        });
        crate::nix::decode_catalog_index(&raw).expect("decodes")
    }

    #[test]
    fn load_fails_closed_on_missing_or_malformed_saved_catalogs() {
        let state = tempfile::tempdir().expect("tempdir");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        registry.approve("other/cli", "https://github.com/other/homebrew-cli", 1);

        // Registered but never published: loading fails, naming the source.
        let error = SavedCatalogs::load(&registry, state.path())
            .expect_err("an unpublished source fails the load");
        assert!(error.contains("no published generation"), "{error}");

        // Publish one source; the other still fails the whole load. The
        // exclusive publication handle is dropped before loading: loaders
        // take a shared lock and must not deadlock against it.
        {
            let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
            let provenance = Provenance {
                source: String::from("somebody/apps"),
                origin: String::from("https://github.com/somebody/homebrew-apps"),
                revision: String::from("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
                metadata_sha256: String::from(
                    "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                ),
                eligible: 1,
                excluded: 0,
                published_unix: 1,
            };
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            store
                .publish(
                    &flake,
                    &provenance,
                    &index_for("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
                )
                .expect("publishes");
            guard.keep();
        }
        let error = SavedCatalogs::load(&registry, state.path())
            .expect_err("one broken source still fails the whole load");
        assert!(error.contains("other/cli"), "{error}");

        // A corrupted capture of the healthy source fails closed too.
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        let current = super::store::source_dir(state.path(), "somebody/apps")
            .expect("valid source")
            .join("current");
        let capture = current.join("index.json");
        std::fs::write(capture, "{ not json").expect("corrupt");
        let error = SavedCatalogs::load(&registry, state.path())
            .expect_err("a malformed capture fails the load");
        assert!(error.contains("somebody/apps"), "{error}");
        assert!(
            error.contains("pkg tap update somebody/apps"),
            "the failure must name the repair command: {error}"
        );

        // A registry origin that disagrees with the generation is refused.
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-moved",
            1,
        );
        let error = SavedCatalogs::load(&registry, state.path())
            .expect_err("an origin mismatch fails the load");
        assert!(error.contains("origin"), "{error}");
    }

    /// Publish one valid generation for `somebody/apps` under `state`.
    fn publish_apps(state: &std::path::Path) {
        let store = SourceStore::lock(state, "somebody/apps").expect("locks");
        let provenance = Provenance {
            source: String::from("somebody/apps"),
            origin: String::from("https://github.com/somebody/homebrew-apps"),
            revision: String::from("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
            metadata_sha256: String::from(
                "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            ),
            eligible: 1,
            excluded: 0,
            published_unix: 1,
        };
        let (staging, guard) = store.new_staging().expect("stages");
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        store
            .publish(
                &flake,
                &provenance,
                &index_for("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
            )
            .expect("publishes");
        guard.keep();
    }

    /// A qualified read of one source is isolated from an unrelated
    /// broken tap (review 2026-09-30, P2): loading every source fails on
    /// the broken one, while loading the healthy source alone succeeds.
    /// Bare-name resolution uses the strict whole-set load; qualified
    /// requests use the per-source load.
    #[test]
    fn load_one_isolates_a_qualified_read_from_a_broken_unrelated_tap() {
        let state = tempfile::tempdir().expect("tempdir");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        registry.approve("other/cli", "https://github.com/other/homebrew-cli", 1);
        publish_apps(state.path());

        // The whole-set load still fails on the unrelated broken tap.
        let error = SavedCatalogs::load(&registry, state.path())
            .expect_err("the broken source fails the whole-set load");
        assert!(error.contains("other/cli"), "{error}");

        // The per-source load reads exactly the source it names.
        let catalog = SavedCatalogs::load_one(&registry, state.path(), "somebody/apps")
            .expect("a healthy source loads alone");
        assert_eq!(catalog.source, "somebody/apps");
        assert!(catalog.index.systems.contains_key("x86_64-linux"));

        // The broken source still fails its own qualified read.
        let error = SavedCatalogs::load_one(&registry, state.path(), "other/cli")
            .expect_err("the broken source fails its own load");
        assert!(error.contains("other/cli"), "{error}");

        // An unregistered source is refused, never guessed.
        let error = SavedCatalogs::load_one(&registry, state.path(), "ghost/apps")
            .expect_err("an unregistered source is refused");
        assert!(error.contains("ghost/apps"), "{error}");
    }

    #[test]
    fn token_sources_include_excluded_matches() {
        let state = tempfile::tempdir().expect("tempdir");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        let provenance = Provenance {
            source: String::from("somebody/apps"),
            origin: String::from("https://github.com/somebody/homebrew-apps"),
            revision: String::from("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
            metadata_sha256: String::from(
                "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            ),
            eligible: 1,
            excluded: 1,
            published_unix: 1,
        };
        let raw = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {"somebody/apps": {
                "url": "https://github.com/somebody/homebrew-apps",
                "revision": "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a",
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license",
                "raw": {
                    "archiveUrl": "https://api.github.com/repos/somebody/homebrew-apps/tarball/0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a",
                    "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                    "captureSha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                    "envelopeSchema": "pkg-tap-raw-export/1"}}},
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "somebody/apps/tool": {
                    "source": "somebody/apps", "token": "tool", "name": null,
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary", "reason": null, "detail": null},
                "somebody/apps/blocked": {
                    "source": "somebody/apps", "token": "blocked", "name": null,
                    "description": null, "version": null, "homepage": null,
                    "status": "excluded", "kind": null,
                    "reason": "installer-script", "detail": null}}}}
        });
        let index = crate::nix::decode_catalog_index(&raw).expect("decodes");
        {
            let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            store
                .publish(&flake, &provenance, &index)
                .expect("publishes");
            guard.keep();
        }
        let saved = SavedCatalogs::load(&registry, state.path()).expect("loads");
        // An excluded entry still answers token presence.
        assert_eq!(
            saved.sources_with_token("x86_64-linux", "blocked"),
            vec![String::from("somebody/apps")]
        );
        assert!(
            saved
                .sources_with_token("aarch64-darwin", "tool")
                .is_empty()
        );
        assert_eq!(
            saved.get("somebody/apps").expect("present").reference,
            format!(
                "path:{}/pkg/taps/src/somebody/apps/current",
                state.path().display()
            )
        );
    }

    /// One binding-provenance generator: a valid envelope except where a
    /// chosen field is overridden, so every corruption case is one call.
    fn index_variant(
        revision: &str,
        capture_sha: &str,
        inputs_extra: bool,
        entries: usize,
    ) -> crate::nix::CatalogIndex {
        let mut inputs = serde_json::Map::new();
        inputs.insert(
            String::from("somebody/apps"),
            serde_json::json!({
                "url": "https://github.com/somebody/homebrew-apps",
                "revision": revision,
                "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                "license": "Homebrew data license",
                "raw": {
                    "archiveUrl": format!(
                        "https://api.github.com/repos/somebody/homebrew-apps/tarball/{revision}"),
                    "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
                    "captureSha256": capture_sha,
                    "envelopeSchema": "pkg-tap-raw-export/1"}}),
        );
        if inputs_extra {
            inputs.insert(
                String::from("other/cli"),
                serde_json::json!({
                    "url": "https://github.com/other/homebrew-cli",
                    "revision": revision,
                    "sha256": "3f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529253",
                    "license": "Homebrew data license",
                    "raw": {
                        "archiveUrl": format!(
                            "https://api.github.com/repos/other/homebrew-cli/tarball/{revision}"),
                        "archiveSha256": "4f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529254",
                        "captureSha256": capture_sha,
                        "envelopeSchema": "pkg-tap-raw-export/1"}}),
            );
        }
        let mut section = serde_json::Map::new();
        for index in 0..entries {
            section.insert(
                format!("somebody/apps/tool{index}"),
                serde_json::json!({
                    "source": "somebody/apps", "token": format!("tool{index}"),
                    "name": null, "description": null, "version": "1.0",
                    "homepage": null, "status": "eligible", "kind": "binary",
                    "reason": null, "detail": null}),
            );
        }
        let raw = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": inputs,
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": section}},
        });
        crate::nix::decode_catalog_index(&raw).expect("decodes")
    }

    fn provenance(revision: &str, capture_sha: &str) -> Provenance {
        Provenance {
            source: String::from("somebody/apps"),
            origin: String::from("https://github.com/somebody/homebrew-apps"),
            revision: String::from(revision),
            metadata_sha256: String::from(capture_sha),
            eligible: 1,
            excluded: 0,
            published_unix: 1,
        }
    }

    #[test]
    fn corrupt_saved_captures_fail_closed_before_any_use() {
        const REV: &str = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        const OTHER_REV: &str = "1b56ceb53d693f3f3e0eaea0f9f4d5b8cf5b9b9d1b";
        const CAPTURE: &str = "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251";
        const OTHER_HASH: &str = "5f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529255";

        let corrupt = |index: crate::nix::CatalogIndex, prov: Provenance| {
            let state = tempfile::tempdir().expect("tempdir");
            let mut registry = Registry::empty();
            registry.approve(
                "somebody/apps",
                "https://github.com/somebody/homebrew-apps",
                1,
            );
            {
                let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
                let (staging, guard) = store.new_staging().expect("stages");
                let flake = staging.join("flake");
                std::fs::create_dir(&flake).expect("flake dir");
                store.publish(&flake, &prov, &index).expect("publishes");
                drop(guard);
            }
            SavedCatalogs::load_one(&registry, state.path(), "somebody/apps")
                .err()
                .unwrap_or_else(|| panic!("the corruption must fail the load"))
        };

        // A capture pinned to a different revision than its generation.
        let error = corrupt(
            index_variant(OTHER_REV, CAPTURE, false, 1),
            provenance(REV, CAPTURE),
        );
        assert!(error.contains("revision"), "{error}");
        // A capture hash that disagrees with the recorded provenance.
        let error = corrupt(
            index_variant(REV, OTHER_HASH, false, 1),
            provenance(REV, CAPTURE),
        );
        assert!(error.contains("capture hash"), "{error}");
        // An extra input source smuggled into the saved capture.
        let error = corrupt(
            index_variant(REV, CAPTURE, true, 1),
            provenance(REV, CAPTURE),
        );
        assert!(error.contains("exactly"), "{error}");
        // Entry counts that disagree with the recorded counts.
        let error = corrupt(
            index_variant(REV, CAPTURE, false, 2),
            provenance(REV, CAPTURE),
        );
        assert!(error.contains("counts"), "{error}");
        // The healthy envelope still loads: the checks are not vacuous.
        let state = tempfile::tempdir().expect("tempdir");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        {
            let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            store
                .publish(
                    &flake,
                    &provenance(REV, CAPTURE),
                    &index_variant(REV, CAPTURE, false, 1),
                )
                .expect("publishes");
            drop(guard);
        }
        assert!(SavedCatalogs::load_one(&registry, state.path(), "somebody/apps").is_ok());
    }
}
