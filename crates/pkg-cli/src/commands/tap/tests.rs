use super::super::{canonical_source, recheck_consent, validate_revision};
use super::*;
use crate::tap::registry;

#[test]
fn typed_refusal_category_survives_text_lookalikes() {
    use cask_catalog::tap::ImportError;
    // Only the typed refusal is carried as a category.
    let refused = stage_error_for_import_error(
        "somebody/apps",
        ImportError::SandboxRefused(String::from("sandbox is false")),
    );
    assert!(matches!(refused, StageError::SandboxRefused(_)));
    // An ordinary failure whose text contains the refusal phrase is
    // an ordinary failure: the setup offer must never trigger.
    let lookalike = stage_error_for_import_error(
        "somebody/apps",
        ImportError::Failed(String::from(
            "effective local nix config refuses the raw tap import: \
             the settings could not be verified",
        )),
    );
    let StageError::Other(error) = lookalike else {
        panic!("a coincidental refusal phrase must not become a refusal");
    };
    let CommandError::Message(text) = error else {
        panic!("the ordinary failure is a message");
    };
    assert!(text.contains("importing somebody/apps failed"), "{text}");
    assert!(text.contains("could not be verified"), "{text}");
    // The refusal detail stays available for the retry path.
    let StageError::SandboxRefused(detail) = stage_error_for_import_error(
        "somebody/apps",
        ImportError::SandboxRefused(String::from("added host path /etc")),
    ) else {
        panic!("the typed refusal is carried");
    };
    assert_eq!(detail, "added host path /etc");
}

#[test]
fn revision_pins_must_be_bare_full_commits() {
    assert_eq!(validate_revision(None).ok(), Some(None));
    assert_eq!(
        validate_revision(Some("0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a0")).ok(),
        Some(Some(String::from(
            "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a0"
        )))
    );
    // A qualified URL that merely contains a full commit is a
    // reference, not a revision pin; a 39-character id and a non-hex
    // string are not full commit ids either.
    for refused in [
        "github:owner/repo?rev=0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1a",
        "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1 ",
        "0a56ceb53d69f3e0eaea0f9f4d5b8cf5b9b9d1g",
        "main",
        "",
    ] {
        assert!(validate_revision(Some(refused)).is_err(), "{refused}");
    }
}

#[test]
fn sources_canonicalize_and_the_official_source_is_reserved() {
    assert_eq!(
        canonical_source("Somebody/homebrew-Apps").ok(),
        Some(String::from("somebody/apps"))
    );
    // The actual backend contract refuses a trailing slash: the URL
    // must be exactly https://github.com/owner/repo.
    let error = canonical_source("https://github.com/somebody/homebrew-apps/")
        .expect_err("a trailing slash is outside the accepted contract");
    let CommandError::Message(text) = error else {
        panic!("the trailing-slash error is a message");
    };
    assert!(text.contains("exactly"), "{text}");
    assert_eq!(
        canonical_source("https://github.com/somebody/homebrew-apps").ok(),
        Some(String::from("somebody/apps"))
    );
    let error = canonical_source("homebrew/cask").expect_err("the official source is reserved");
    let CommandError::Message(text) = error else {
        panic!("the reserved-source error is a message");
    };
    assert!(text.contains("reserved"), "{text}");
    assert!(canonical_source("git@gitlab.com:a/b").is_err());
    assert!(canonical_source("somebody/apps/extra").is_err());
}

const ORIGIN: &str = "https://github.com/somebody/homebrew-apps";
const CAPTURE_SHA: &str = "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251";

/// The text of one unprinted command error.
fn message(error: CommandError) -> String {
    match error {
        CommandError::Message(text) => text,
        CommandError::Reported(_) => panic!("the error is a message"),
    }
}

fn registered_registry() -> registry::Registry {
    let mut registry = registry::Registry::empty();
    registry.approve("somebody/apps", ORIGIN, 1);
    registry
}

#[test]
fn consent_recheck_refuses_a_concurrently_removed_source() {
    // Remembered consent still stands after the lock re-read.
    let registry = registered_registry();
    assert!(recheck_consent(&registry, "somebody/apps", ORIGIN, false).is_ok());
    // Fresh approval also passes when the entry stayed or appeared.
    assert!(recheck_consent(&registry::Registry::empty(), "somebody/apps", ORIGIN, true).is_ok());
    // The race: the entry is gone under the lock and this invocation
    // did not explicitly approve — refuse before anything runs.
    let error = recheck_consent(&registry::Registry::empty(), "somebody/apps", ORIGIN, false)
        .expect_err("a vanished approval must be refused");
    assert!(message(error).contains("removed"));
    // An origin change under the lock is still refused, fresh or not.
    let error = recheck_consent(
        &registered_registry(),
        "somebody/apps",
        "https://github.com/somebody/homebrew-moved",
        true,
    )
    .expect_err("an origin change must be refused");
    assert!(message(error).contains("origin"));
}

/// The one provenance `stage_import` builds for a chosen capture hash.
fn staged_provenance(metadata_sha256: &str) -> store::Provenance {
    store::Provenance {
        source: String::from("somebody/apps"),
        origin: String::from(ORIGIN),
        revision: String::from("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"),
        metadata_sha256: String::from(metadata_sha256),
        eligible: 1,
        excluded: 0,
        published_unix: 17,
    }
}

/// One staged index envelope for the agreed source, with a chosen raw
/// capture hash and target list.
fn staged_index(capture_sha256: &str, targets: &serde_json::Value) -> crate::nix::CatalogIndex {
    let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
    let raw = serde_json::json!({
        "url": ORIGIN,
        "revision": revision,
        "sha256": CAPTURE_SHA,
        "license": "Homebrew data license",
        "raw": {
            "archiveUrl": format!("{ORIGIN}/archive/{revision}.tar.gz"),
            "archiveSha256": "2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
            "captureSha256": capture_sha256,
        },
    });
    let mut systems = serde_json::Map::new();
    for target in targets.as_array().expect("targets list") {
        systems.insert(
            target.as_str().expect("system").to_string(),
            serde_json::json!({"entries": {
                "somebody/apps/tool": {
                    "source": "somebody/apps", "token": "tool", "name": null,
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary", "reason": null,
                    "detail": null}}}),
        );
    }
    let raw_index = serde_json::json!({
        "schema": crate::nix::CATALOG_INDEX_SCHEMA,
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {"somebody/apps": raw},
        "targets": targets.clone(),
        "macosBaseline": "15.7.7",
        "systems": systems,
    });
    crate::nix::decode_catalog_index(&raw_index).expect("decodes")
}

#[test]
fn staged_index_checks_accept_exact_provenance_and_refuse_mismatches() {
    let provenance = staged_provenance(CAPTURE_SHA);
    // The exact contract passes: one input, raw provenance with the
    // importer's capture hash, exactly this system as the only target.
    let index = staged_index(CAPTURE_SHA, &serde_json::json!(["x86_64-linux"]));
    assert!(check_staged_index(&index, &provenance, "x86_64-linux").is_ok());

    // A capture hash that disagrees with the provenance is a
    // publication refusal.
    let mismatch = staged_index(
        "3f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529253",
        &serde_json::json!(["x86_64-linux"]),
    );
    let error = check_staged_index(&mismatch, &provenance, "x86_64-linux")
        .expect_err("a capture-hash mismatch must refuse publication");
    let error = message(error);
    assert!(error.contains("capture hash"), "{error}");

    // Extra targets are refused, even when this system is among them.
    let extra = staged_index(
        CAPTURE_SHA,
        &serde_json::json!(["x86_64-linux", "aarch64-darwin"]),
    );
    let error = check_staged_index(&extra, &provenance, "x86_64-linux")
        .expect_err("extra targets must refuse publication");
    assert!(message(error).contains("exactly"));

    // Duplicated targets are refused the same way.
    let duplicate = staged_index(
        CAPTURE_SHA,
        &serde_json::json!(["x86_64-linux", "x86_64-linux"]),
    );
    assert!(check_staged_index(&duplicate, &provenance, "x86_64-linux").is_err());

    // A local capture without raw provenance is refused.
    let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
    let bare = serde_json::json!({
        "schema": crate::nix::CATALOG_INDEX_SCHEMA,
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {"somebody/apps": {
            "url": ORIGIN, "revision": revision, "sha256": CAPTURE_SHA,
            "license": "Homebrew data license"}},
        "targets": ["x86_64-linux"],
        "macosBaseline": "15.7.7",
        "systems": {"x86_64-linux": {"entries": {}}},
    });
    let bare = crate::nix::decode_catalog_index(&bare).expect("decodes");
    let error = check_staged_index(&bare, &provenance, "x86_64-linux")
        .expect_err("a capture without raw provenance must refuse publication");
    assert!(message(error).contains("raw"));

    // An envelope describing a different source than the provenance is
    // refused by the shared binding check.
    let wrong_source = staged_index(CAPTURE_SHA, &serde_json::json!(["x86_64-linux"]));
    let mut other = staged_provenance(CAPTURE_SHA);
    other.source = String::from("other/apps");
    assert!(check_staged_index(&wrong_source, &other, "x86_64-linux").is_err());
}

#[test]
fn flake_root_must_be_exactly_the_staging_flake_directory() {
    let staging = tempfile::tempdir().expect("tempdir");
    let staging_path = staging.path();
    let flake = staging_path.join("flake");
    std::fs::create_dir(&flake).expect("mkdir flake");
    assert!(
        validated_flake_path(staging_path, &flake).is_ok(),
        "the exact staging/flake layout must be accepted"
    );

    // The staging root itself is refused: `flake == staging` would make
    // later cleanup target the generations parent.
    assert!(validated_flake_path(staging_path, staging_path).is_err());
    // Any other sibling inside staging is refused.
    let other = staging_path.join("other");
    std::fs::create_dir(&other).expect("mkdir other");
    assert!(validated_flake_path(staging_path, &other).is_err());
    // An outside and a root path are refused.
    assert!(validated_flake_path(staging_path, std::path::Path::new("/")).is_err());
    let outside = tempfile::tempdir().expect("tempdir");
    assert!(validated_flake_path(staging_path, outside.path()).is_err());
    // The exact path must be a real directory, not a file.
    std::fs::remove_dir(&flake).expect("rmdir flake");
    std::fs::write(&flake, "x").expect("write file");
    assert!(validated_flake_path(staging_path, &flake).is_err());
    // A missing path is refused.
    std::fs::remove_file(&flake).expect("remove file");
    assert!(validated_flake_path(staging_path, &flake).is_err());
}
