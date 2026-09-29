//! Regression tests over REAL native reader captures.
//!
//! Every test reads committed fixtures under
//! `src/tap/testdata/native-captures/` only: the `capture` envelope is
//! the actual upstream `pkg-tap-raw-export/1` output produced by the
//! pinned reader on native runners, and the `sources/` trees are the
//! exact data-only fixture tap trees that produced them (see the
//! directory README for provenance and normalization). No `/tmp`
//! ambient evidence and no silently-skipped test exists here: if a
//! committed fixture is missing the test FAILS. The classifier result
//! is never mocked; `validate_export` and the private `convert_capture`
//! run against the actual envelope and the actual tree bytes.
//!
//! The native aarch64-darwin captures convert on a Linux host too:
//! conversion is pure over the capture plus the tree inventory and
//! never runs Homebrew or Ruby.

use super::capture::{Capture, convert_capture, validate_export};
use super::{EXPORT_SCHEMA, sha256_hex, source_origin};
use serde_json::Value;
use std::collections::BTreeMap;

const TESTDATA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tap/testdata/native-captures"
);
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Load one committed capture envelope by system and case name.
fn capture_json(system: &str, case: &str) -> Value {
    let path = std::path::Path::new(TESTDATA)
        .join("captures")
        .join(system)
        .join(format!("{case}.json"));
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("committed fixture {} missing: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "committed fixture {} is not valid JSON: {e}",
            path.display()
        )
    })
}

/// SHA-256 inventory of one committed source fixture tree, keyed by
/// source-relative path — exactly what `extract_tar_gz` would hand to
/// the importer for the real archive.
fn source_tree(case: &str) -> BTreeMap<String, String> {
    let root = std::path::Path::new(TESTDATA).join("sources").join(case);
    let mut tree = BTreeMap::new();
    fn walk(dir: &std::path::Path, prefix: &str, tree: &mut BTreeMap<String, String>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("fixture source tree {} missing: {e}", dir.display()));
        for entry in entries {
            let entry = entry.expect("readdir");
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &rel, tree);
            } else {
                let bytes = std::fs::read(&path).expect("fixture file bytes");
                tree.insert(rel, sha256_hex(&bytes));
            }
        }
    }
    walk(&root, "", &mut tree);
    assert!(!tree.is_empty(), "fixture tree {case} is empty");
    tree
}

/// Run the actual validation and conversion over one committed case and
/// return the parsed generated `catalog/catalog.json`.
fn convert_case(system: &str, case: &str) -> Result<Value, String> {
    let capture = capture_json(system, case);
    assert_eq!(
        capture["schema"], EXPORT_SCHEMA,
        "{case}: wrong envelope schema"
    );
    let source = capture["source"].as_str().expect("source").to_string();
    let system_field = capture["system"].as_str().expect("system").to_string();
    assert_eq!(system_field, system, "{case}: capture system mismatch");
    let envelope = serde_json::to_vec(&capture).unwrap();
    let (export, metadata_sha256) = validate_export(&envelope, &source, REVISION, system)
        .unwrap_or_else(|e| panic!("{case}: actual capture must validate: {e}"));
    let dir = tempfile::tempdir().expect("tempdir");
    convert_capture(
        &Capture {
            source: source.clone(),
            origin: source_origin(&source).expect("origin"),
            revision: REVISION.to_string(),
            system: system.to_string(),
            export,
            metadata_sha256,
            archive_url: format!("https://codeload.example/{source}/tar.gz/{REVISION}"),
            archive_sha256: "f".repeat(64),
            license: "NOASSERTION".to_string(),
            tree: source_tree(case),
        },
        dir.path(),
    )
    .map(|_| {
        serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("flake/catalog/catalog.json")).unwrap(),
        )
        .expect("generated catalog parses")
    })
}

fn entry<'a>(catalog: &'a Value, system: &str, case: &str, token: &str) -> &'a Value {
    let capture = capture_json(system, case);
    let source = capture["source"].as_str().expect("source");
    catalog["entries"]
        .get(format!("{source}/{token}"))
        .unwrap_or_else(|| panic!("entry {source}/{token} missing from generated catalog"))
}

/// A duplicate token inside ONE capture is a whole-capture refusal: the
/// generated catalog may never silently keep one arbitrary winner. The
/// real reader exported both `Casks/a/pubtap-dup.rb` and
/// `Casks/b/pubtap-dup.rb`; conversion must refuse them.
#[test]
fn duplicate_token_refuses_the_whole_capture() {
    for system in ["x86_64-linux", "aarch64-darwin"] {
        let err = convert_case(system, "dup")
            .err()
            .unwrap_or_else(|| panic!("{system}: duplicate-token capture must be refused"));
        assert!(
            err.contains("duplicate token"),
            "{system}: refusal must name the duplicate token: {err}"
        );
    }
}

/// Literal local, private, link-local, and IPv6-loopback download URLs
/// are excluded from BOTH native targets. The reader itself exported
/// these records as ok; the destination policy must refuse them without
/// any DNS lookup, so the exclusion has to appear in the generated
/// catalog entry, not only in a fetch-time error.
#[test]
fn local_destination_urls_are_excluded() {
    for system in ["x86_64-linux", "aarch64-darwin"] {
        for case in ["url-localhost", "url-private", "url-linklocal", "url-ipv6"] {
            if system == "aarch64-darwin" && case != "url-localhost" {
                continue; // linux-only fixture committed; mac covers localhost
            }
            let catalog = convert_case(system, case)
                .unwrap_or_else(|e| panic!("{system}/{case}: capture must convert: {e}"));
            let entry = entry(&catalog, system, case, &format!("pubtap-{case}"));
            let target = &entry["targets"][system];
            assert_eq!(
                target["status"], "excluded",
                "{system}/{case}: local destination must never be eligible: {target}"
            );
            assert!(
                target["reason"].is_string(),
                "{system}/{case}: exclusion must carry a reason: {target}"
            );
        }
    }
}

/// Archive and raw-binary records from real captures are eligible with
/// real classifier plans (release archive = app+cli on mac / binary on
/// linux; naked container = raw binary with the rename kept).
#[test]
fn archive_and_raw_binary_captures_are_eligible() {
    for (system, case, token) in [
        ("x86_64-linux", "release-archive", "pubtap-release-archive"),
        ("x86_64-linux", "raw-binary", "pubtap-raw-binary"),
        (
            "aarch64-darwin",
            "release-archive",
            "pubtap-release-archive",
        ),
        ("aarch64-darwin", "raw-binary", "pubtap-raw-binary"),
    ] {
        let catalog = convert_case(system, case).unwrap_or_else(|e| panic!("{system}/{case}: {e}"));
        let entry = entry(&catalog, system, case, token);
        let target = &entry["targets"][system];
        assert_eq!(
            target["status"], "eligible",
            "{system}/{case}: real eligible capture misclassified: {target}"
        );
        assert_eq!(target["reason"], Value::Null, "{system}/{case}");
        assert!(
            target["plan"].is_object(),
            "{system}/{case}: no plan emitted"
        );
        assert_eq!(entry["version"], token_version(token), "{system}/{case}");
        // Eligible entries keep validated provenance.
        assert_eq!(entry["origin"]["path"], origin_path(case));
    }
}

fn token_version(token: &str) -> &str {
    match token {
        "pubtap-release-archive" => "1.2.3",
        _ => "0.9.0",
    }
}

fn origin_path(case: &str) -> &'static str {
    match case {
        "release-archive" => "Casks/r/pubtap-release-archive.rb",
        "raw-binary" => "Casks/b/pubtap-raw-binary.rb",
        _ => "Casks/u/pubtap-url-localhost.rb",
    }
}

/// A `staged_path` load error becomes an excluded row whose ORIGIN is
/// still validated against the tree and retained in the catalog, while
/// the same tree's healthy control stays eligible.
#[test]
fn staged_path_error_row_retains_validated_origin() {
    let catalog = convert_case("x86_64-linux", "staging")
        .unwrap_or_else(|e| panic!("staging capture must convert: {e}"));
    let control = entry(
        &catalog,
        "x86_64-linux",
        "staging",
        "pubtap-staging-control",
    );
    assert_eq!(control["targets"]["x86_64-linux"]["status"], "eligible");
    let staged = entry(&catalog, "x86_64-linux", "staging", "pubtap-staging");
    let target = &staged["targets"]["x86_64-linux"];
    assert_eq!(target["status"], "excluded");
    assert_eq!(target["reason"], "ruby-load-error");
    let detail = target["detail"].as_str().expect("detail");
    assert!(
        detail.contains("staged_path"),
        "detail must name staged_path: {detail}"
    );
    // The error row keeps its provenance: real path + real tree hash.
    assert_eq!(staged["origin"]["path"], "Casks/s/pubtap-staging.rb");
    let real = sha256_hex(
        &std::fs::read(
            std::path::Path::new(TESTDATA).join("sources/staging/Casks/s/pubtap-staging.rb"),
        )
        .unwrap(),
    );
    assert_eq!(staged["origin"]["sha256"], real);
}

/// A helper-file-only change (`lib/pubtap_helper.rb` VERSION
/// 1.4.0 -> 1.4.1) yields a different effective version in the catalog
/// although the cask file itself is byte-identical (same origin hash).
/// Whole-tree identity, not file identity, drives the version.
#[test]
fn helper_only_change_bumps_the_catalog_version() {
    let cask = "Casks/t/pubtap-invalidation.rb";
    let helper = "lib/pubtap_helper.rb";
    let v1_tree = source_tree("invalidation-alpha-v1");
    let v2_tree = source_tree("invalidation-alpha-v2");
    assert_eq!(v1_tree[cask], v2_tree[cask], "cask file must be identical");
    assert_ne!(v1_tree[helper], v2_tree[helper], "helper must differ");

    let mut versions = Vec::new();
    for case in ["invalidation-alpha-v1", "invalidation-alpha-v2"] {
        // On darwin the app artifact is eligible; on linux the same
        // record is excluded (unsupported-artifact), but the effective
        // version is still recorded in BOTH catalogs.
        for system in ["aarch64-darwin", "x86_64-linux"] {
            let catalog = convert_case(system, case)
                .unwrap_or_else(|e| panic!("{system}/{case} must convert: {e}"));
            let entry = entry(&catalog, system, case, "pubtap-invalidation");
            let target = &entry["targets"][system];
            if system == "aarch64-darwin" {
                assert_eq!(target["status"], "eligible", "{case} on darwin");
            } else {
                assert_eq!(target["status"], "excluded", "{case} on linux");
                assert_eq!(target["reason"], "unsupported-artifact", "{case} on linux");
            }
            assert_eq!(entry["origin"]["path"], cask);
            assert_eq!(entry["origin"]["sha256"], v1_tree[cask]);
            versions.push((
                system,
                entry["version"].as_str().expect("version").to_string(),
            ));
        }
    }
    for system in ["aarch64-darwin", "x86_64-linux"] {
        assert_eq!(
            versions
                .iter()
                .filter(|(s, _)| s == &system)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>(),
            ["1.4.0", "1.4.1"],
            "{system}: helper-only change must bump the recorded version"
        );
    }
}

/// The native macOS capture is a first-class fixture: it converts and
/// classifies on a Linux development host without any macOS or Homebrew
/// involvement (pure conversion over committed bytes).
#[test]
fn native_macos_capture_converts_on_linux_host() {
    let catalog = convert_case("aarch64-darwin", "release-archive")
        .expect("native darwin capture converts privately on any host");
    let entry = entry(
        &catalog,
        "aarch64-darwin",
        "release-archive",
        "pubtap-release-archive",
    );
    let target = &entry["targets"]["aarch64-darwin"];
    assert_eq!(target["status"], "eligible");
    assert_eq!(target["kind"], "app");
    assert!(
        target["plan"]["artifacts"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );
}

/// Real NORMAL-MODE macOS capture of a legacy cask whose Ruby DSL uses
/// `preflight do ... end` / `postflight do ... end` callback blocks.
/// The pinned reader emits them as null-valued artifact keys
/// (`{"preflight": null}`, `{"postflight": null}`) around the real
/// `app` artifact. pkg never runs callback code, so those keys make
/// the record uninstallable here: conversion excludes the legacy cask
/// (reason `unsupported-artifact`, detail naming both callback keys)
/// while the healthy binary control from the same tree stays eligible
/// with a real plan. This exercises the private `convert_capture`
/// end to end over committed bytes only.
///
/// Provenance and normalization: same pins as the other fixtures
/// (Homebrew cc9ff03..., reader 4.0.5, fixture revision aaa...a,
/// probe `denied: host-read, host-write, loopback`). The envelope is
/// the runner's `capture` field verbatim; runner stdout/stderr and Nix
/// store paths are dropped (see the directory README). The source
/// tree under `sources/flights` is the exact data-only fixture tap
/// tree that produced it; each `pkg_origin.sha256` was verified
/// against the committed file bytes.
#[test]
fn normal_mode_legacy_preflight_postflight_capture_excludes_the_callback_cask() {
    let catalog =
        convert_case("aarch64-darwin", "flights").expect("normal-mode flights capture converts");
    let control = entry(
        &catalog,
        "aarch64-darwin",
        "flights",
        "pubtap-flights-control",
    );
    assert_eq!(
        control["targets"]["aarch64-darwin"]["status"], "eligible",
        "the healthy binary control must stay eligible: {}",
        control["targets"]["aarch64-darwin"]
    );
    assert!(control["targets"]["aarch64-darwin"]["plan"].is_object());
    let legacy = entry(&catalog, "aarch64-darwin", "flights", "pubtap-flights");
    let target = &legacy["targets"]["aarch64-darwin"];
    assert_eq!(target["status"], "excluded");
    assert_eq!(target["reason"], "unsupported-artifact");
    let detail = target["detail"].as_str().expect("detail names the keys");
    assert!(detail.contains("preflight"), "detail: {detail}");
    assert!(detail.contains("postflight"), "detail: {detail}");
    // The excluded legacy row still carries its validated provenance.
    assert_eq!(legacy["origin"]["path"], "Casks/p/pubtap-flights.rb");
}
