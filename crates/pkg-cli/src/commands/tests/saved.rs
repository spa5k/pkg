use super::TapState;
use crate::catalog::Routing;
use crate::config::Sources;
use crate::tap::Registry;
use crate::tap::saved::SavedCatalogs;
use crate::tap::store::{self, Provenance, SourceStore};
use std::collections::BTreeMap;
use std::path::Path;

fn load(registry: &Registry, state_home: &Path) -> Result<SavedCatalogs, String> {
    let mut state = TapState {
        routing: Routing::from_registry(&Sources::default(), registry, state_home)?,
        registry: registry.clone(),
        state_home: state_home.to_owned(),
        saved: BTreeMap::new(),
    };
    state.saved_all()
}

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
    let error = load(&registry, state.path()).expect_err("an unpublished source fails the load");
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
        let staged = store.new_staging().expect("stages");
        let flake = staged.path().join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        staged
            .publish(
                &provenance,
                &index_for("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
            )
            .expect("publishes");
    }
    let error =
        load(&registry, state.path()).expect_err("one broken source still fails the whole load");
    assert!(error.contains("other/cli"), "{error}");

    // A corrupted capture of the healthy source fails closed too.
    let mut registry = Registry::empty();
    registry.approve(
        "somebody/apps",
        "https://github.com/somebody/homebrew-apps",
        1,
    );
    let current = store::source_dir(state.path(), "somebody/apps")
        .expect("valid source")
        .join("current");
    let capture = current.join("index.json");
    std::fs::write(capture, "{ not json").expect("corrupt");
    let error = load(&registry, state.path()).expect_err("a malformed capture fails the load");
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
    let error = load(&registry, state.path()).expect_err("an origin mismatch fails the load");
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
    let staged = store.new_staging().expect("stages");
    let flake = staged.path().join("flake");
    std::fs::create_dir(&flake).expect("flake dir");
    staged
        .publish(
            &provenance,
            &index_for("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a"),
        )
        .expect("publishes");
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
    let error =
        load(&registry, state.path()).expect_err("the broken source fails the whole-set load");
    assert!(error.contains("other/cli"), "{error}");

    // The per-source load reads exactly the source it names.
    let catalog = SavedCatalogs::load_one(&registry, state.path(), "somebody/apps")
        .expect("a healthy source loads alone");
    assert_eq!(catalog.source(), "somebody/apps");
    assert!(catalog.index().systems().contains_key("x86_64-linux"));
    let catalogs = SavedCatalogs::from_catalogs([catalog.clone()]).expect("keys derive");
    assert!(
        catalogs
            .iter()
            .any(|saved| saved.source() == "somebody/apps")
    );
    assert!(SavedCatalogs::from_catalogs([catalog.clone(), catalog]).is_err());

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
        let staged = store.new_staging().expect("stages");
        let flake = staged.path().join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        staged.publish(&provenance, &index).expect("publishes");
    }
    let saved = load(&registry, state.path()).expect("loads");
    // An excluded entry still answers token presence.
    assert_eq!(
        saved.sources_with_token("x86_64-linux", "blocked"),
        vec![String::from("somebody/apps")]
    );
    assert_eq!(
        saved.sources_with_token("aarch64-darwin", "tool"),
        [] as [String; 0]
    );
    assert_eq!(
        saved
            .iter()
            .find(|catalog| catalog.source() == "somebody/apps")
            .expect("present")
            .reference(),
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

    let corrupt = |index: crate::nix::CatalogIndex| {
        let state = tempfile::tempdir().expect("tempdir");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            1,
        );
        {
            let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
            let staged = store.new_staging().expect("stages");
            let flake = staged.path().join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            staged
                .publish(
                    &provenance(REV, CAPTURE),
                    &index_variant(REV, CAPTURE, false, 1),
                )
                .expect("publishes valid generation");
        }
        let index_path = store::source_dir(state.path(), "somebody/apps")
            .expect("valid source")
            .join("current/index.json");
        std::fs::write(
            index_path,
            serde_json::to_vec(&index).expect("encodes corrupt capture"),
        )
        .expect("corrupt saved capture");
        SavedCatalogs::load_one(&registry, state.path(), "somebody/apps")
            .err()
            .unwrap_or_else(|| panic!("the corruption must fail the load"))
    };

    // A capture pinned to a different revision than its generation.
    let error = corrupt(index_variant(OTHER_REV, CAPTURE, false, 1));
    assert!(error.contains("revision"), "{error}");
    // A capture hash that disagrees with the recorded provenance.
    let error = corrupt(index_variant(REV, OTHER_HASH, false, 1));
    assert!(error.contains("capture hash"), "{error}");
    // An extra input source smuggled into the saved capture.
    let error = corrupt(index_variant(REV, CAPTURE, true, 1));
    assert!(error.contains("exactly"), "{error}");
    // Entry counts that disagree with the recorded counts.
    let error = corrupt(index_variant(REV, CAPTURE, false, 2));
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
        let staged = store.new_staging().expect("stages");
        let flake = staged.path().join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        staged
            .publish(
                &provenance(REV, CAPTURE),
                &index_variant(REV, CAPTURE, false, 1),
            )
            .expect("publishes");
    }
    assert!(SavedCatalogs::load_one(&registry, state.path(), "somebody/apps").is_ok());
}
