//! Whole-catalog generation (schema `pkg-cask-catalog/4`).
//!
//! Integrity checks, one deterministic catalog with per-source `inputs`
//! provenance and source-qualified entry identities, and a printable
//! coverage summary.
//!
//! Entry map keys are the single identity `source/token` (for the
//! official snapshot source `homebrew/cask/token`; for raw taps
//! `owner/tap/token`). Each entry repeats its bare `token` and `source`
//! fields. The official driver reads the pinned JSON snapshot; the raw
//! tap driver lives in `tap.rs` and feeds the same entry shape.

use crate::classify::{PlatformGate, TargetDecision, classify_target};
use crate::effective;
use crate::{
    CATALOG_SCHEMA, GENERATOR_NAME, GENERATOR_VERSION, MACOS_BASELINE, OFFICIAL_SOURCE, Pin,
    TARGETS,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// Provenance for one source under the catalog's `inputs` map.
#[derive(Serialize, Clone)]
pub struct InputProvenance {
    /// Canonical approved origin URL. The meaning depends on `kind`:
    /// for `json-snapshot` inputs this IS the exact pinned snapshot
    /// download URL (`raw` is null); for `raw-ruby-tap` inputs it is the
    /// public canonical repository URL and the exact pinned download
    /// URL lives at `raw.archiveUrl` instead.
    pub url: String,
    /// Exact revision the url names.
    pub revision: String,
    /// SHA-256 of the pinned source bytes (`url` for JSON snapshots;
    /// for raw taps the exact-revision codeload archive —
    /// `raw.archiveSha256` repeats it and `raw.captureSha256` is the
    /// separate export.json metadata capture hash).
    pub sha256: String,
    /// Upstream data license attribution.
    pub license: String,
    /// Input class: `json-snapshot` (official pinned snapshot) or
    /// `raw-ruby-tap` (public tap captured through the sandboxed
    /// upstream Ruby loader).
    pub kind: &'static str,
    /// `null` for `json-snapshot` inputs (the url/sha256 fields above
    /// already name the exact pinned bytes; the client requires null
    /// or absent). For `raw-ruby-tap` inputs: `archiveUrl` (exact
    /// source blob download URL), `archiveSha256` (downloaded archive
    /// bytes), `captureSha256` (export.json metadata bytes),
    /// `envelopeSchema`.
    pub raw: Value,
}

/// Source-relative file provenance for a raw cask file.
#[derive(Serialize, Clone)]
pub struct Origin {
    /// Source-tree-relative path of the cask file.
    pub path: String,
    /// SHA-256 of the cask file bytes.
    pub sha256: String,
}

impl Origin {
    /// Check source-relative path and SHA-256 syntax, and normalize the hash.
    pub(super) fn checked(path: &str, sha256: &str) -> Option<Self> {
        if !crate::valid_sha256(sha256) || !safe_relative_path(path) {
            return None;
        }
        Some(Self {
            path: path.to_string(),
            sha256: sha256.to_ascii_lowercase(),
        })
    }
}

/// One catalog entry: source identity plus per-target decisions.
#[derive(Serialize)]
pub struct Entry {
    /// Source ID this entry came from.
    pub source: String,
    /// Bare cask token inside its source.
    pub token: String,
    /// Display name.
    pub name: Option<String>,
    /// Short description.
    pub description: Option<String>,
    /// Effective version (may be null on excluded entries).
    pub version: Option<String>,
    /// Homepage.
    pub homepage: Option<String>,
    /// Source file provenance when known, else null.
    pub origin: Option<Origin>,
    /// Per-target decisions keyed by target system.
    pub targets: BTreeMap<String, TargetDecision>,
}

/// The single committed document plus its printable coverage summary.
pub struct Generated {
    /// The full catalog envelope with plans.
    pub catalog: String,
    /// Human-readable coverage counts (printed, never committed).
    pub coverage: String,
}

#[derive(Serialize)]
struct Generator<'a> {
    name: &'a str,
    version: &'a str,
}

/// The complete catalog envelope with its source provenance and entries.
#[derive(Serialize)]
struct Catalog<'a> {
    schema: &'a str,
    generator: Generator<'a>,
    inputs: &'a BTreeMap<String, InputProvenance>,
    targets: &'a [&'a str],
    #[serde(rename = "macosBaseline")]
    macos_baseline: &'a str,
    entries: &'a BTreeMap<String, Entry>,
}

/// Assemble the final catalog document from classified entries.
///
/// Shared by the official snapshot driver and the raw tap driver so both
/// emit byte-identical envelope shapes. The document always ends with
/// exactly one newline.
///
/// # Errors
///
/// Returns an error if the catalog cannot be encoded as JSON.
pub fn assemble_catalog(
    inputs: &BTreeMap<String, InputProvenance>,
    entries: &BTreeMap<String, Entry>,
    targets: &[&str],
) -> Result<String, String> {
    let catalog = Catalog {
        schema: CATALOG_SCHEMA,
        generator: Generator {
            name: GENERATOR_NAME,
            version: GENERATOR_VERSION,
        },
        inputs,
        targets,
        macos_baseline: MACOS_BASELINE,
        entries,
    };
    let catalog = serde_json::to_string(&catalog).map_err(|e| e.to_string())?;
    Ok(format!("{catalog}\n"))
}

/// The official snapshot's `inputs` entry.
#[must_use]
pub fn official_input(pin: &Pin) -> (String, InputProvenance) {
    (
        OFFICIAL_SOURCE.to_string(),
        InputProvenance {
            url: pin.url.clone(),
            revision: pin.revision.clone(),
            sha256: pin.sha256.clone(),
            license: pin.license.clone(),
            kind: "json-snapshot",
            // Client contract: the client requires null/absent raw for
            // json-snapshot inputs — `url` and `sha256` above already
            // name the exact pinned snapshot bytes, so a `raw` object
            // would only repeat provenance with no new meaning.
            raw: Value::Null,
        },
    )
}

/// File origin from a snapshot record's `ruby_source_*` fields, when the
/// mirror preserved them and the checksum is a real SHA-256.
fn snapshot_origin(record: &Value) -> Option<Origin> {
    let path = record.get("ruby_source_path")?.as_str()?;
    let sha = record
        .get("ruby_source_checksum")?
        .get("sha256")?
        .as_str()?;
    Origin::checked(path, sha)
}

/// Whether a source-relative path is plain and inside the tree: no
/// absolute form, no empty/`.`/`..` component, no control characters,
/// and no backslash that could smuggle a separator past a later split.
fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_ascii_control())
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Run whole-catalog generation over the decoded official snapshot.
///
/// Integrity failures abort before any output exists: a snapshot where no
/// record carries `supported_platforms` (the field family is gone),
/// duplicate tokens, records without a token, and tokens that cannot be
/// catalog keys. One malformed record is isolated: it is excluded, never
/// a whole-catalog failure.
///
/// # Errors
///
/// Returns an error when the snapshot root is invalid, identity checks fail,
/// or the final catalog cannot be encoded.
pub fn generate(snapshot: &Value, pin: &Pin) -> Result<Generated, String> {
    let records = snapshot
        .as_array()
        .ok_or_else(|| "snapshot root is not a JSON array".to_string())?;
    if records.is_empty() {
        return Err("integrity: snapshot is empty".to_string());
    }
    if !records
        .iter()
        .any(|r| r.get("supported_platforms").is_some_and(|v| !v.is_null()))
    {
        return Err(
            "integrity: no record carries supported_platforms; the accepted \
             input schema requires the field family"
                .to_string(),
        );
    }
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    for record in records {
        let Some(token) = record.get("token").and_then(Value::as_str) else {
            return Err("integrity: snapshot record without a token string".to_string());
        };
        if !crate::token::is_valid_token(token) {
            return Err(format!(
                "integrity: token {token:?} is not a usable catalog key \
                 (allowed: [a-z0-9][a-z0-9+._@-]* without '..')"
            ));
        }
        let id = qualified_id(OFFICIAL_SOURCE, token);
        if entries.contains_key(&id) {
            return Err(format!("integrity: duplicate token {token}"));
        }
        let mut targets: BTreeMap<String, TargetDecision> = BTreeMap::new();
        for system in TARGETS {
            let decision = classify_target(record, system, MACOS_BASELINE, PlatformGate::Tags);
            targets.insert((*system).to_string(), decision);
        }
        entries.insert(
            id,
            Entry {
                source: OFFICIAL_SOURCE.to_string(),
                token: token.to_string(),
                name: effective::name_field(record),
                description: effective::str_field(record, "desc"),
                version: effective::str_field(record, "version"),
                homepage: effective::str_field(record, "homepage"),
                origin: snapshot_origin(record),
                targets,
            },
        );
    }

    let mut inputs = BTreeMap::new();
    let (id, input) = official_input(pin);
    inputs.insert(id, input);
    let catalog = assemble_catalog(&inputs, &entries, &TARGETS)?;

    let mut coverage = String::new();
    for system in TARGETS {
        let mut eligible = 0;
        let mut reasons = BTreeMap::new();
        for decision in entries
            .values()
            .filter_map(|entry| entry.targets.get(system))
        {
            match decision.reason_code() {
                None => eligible += 1,
                Some(reason) => *reasons.entry(reason).or_insert(0) += 1,
            }
        }
        let _ = writeln!(
            coverage,
            "{system}: {} records, {eligible} eligible",
            entries.len()
        );
        for (reason, count) in reasons {
            let _ = writeln!(coverage, "  {reason}: {count}");
        }
    }

    Ok(Generated { catalog, coverage })
}

pub use crate::token::full_entry_id as qualified_id;

/// Publish the catalog atomically: one unique same-directory temporary
/// file, persisted over the target once the document is complete.
///
/// A failure leaves the previously committed catalog untouched.
///
/// # Errors
///
/// Returns an error when the directory cannot be created, the staged file
/// cannot be written or synced, or publication fails.
pub fn write_catalog(dir: &Path, catalog: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("cannot stage a temporary file in {}: {e}", dir.display()))?;
    std::io::Write::write_all(tmp.as_file_mut(), catalog.as_bytes())
        .map_err(|e| format!("cannot write the staged catalog: {e}"))?;
    let target = dir.join("catalog.json");
    // Set public data permissions before replacing the old catalog.
    {
        let permissions = std::fs::Permissions::from_mode(0o644);
        tmp.as_file()
            .set_permissions(permissions)
            .map_err(|e| format!("cannot make {} readable: {e}", target.display()))?;
    }
    tmp.as_file()
        .sync_all()
        .map_err(|e| format!("cannot sync the staged catalog: {e}"))?;
    tmp.persist(&target)
        .map_err(|e| format!("cannot publish {}: {e}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pin() -> Pin {
        Pin {
            url: "https://raw.example/rev/cask.json".into(),
            revision: "rev".into(),
            sha256: "a".repeat(64),
            license: "BSD-2-Clause (Homebrew Cask data)".into(),
        }
    }

    fn record(token: &str, platforms: &Value, extra: &Value) -> Value {
        let mut raw = json!({
            "token": token, "version": "1", "url": "https://example.com/x.zip",
            "sha256": "a".repeat(64),
            "artifacts": [{"binary": ["t"]}],
            "supported_platforms": platforms,
        });
        if let (Some(target), Some(source)) = (raw.as_object_mut(), extra.as_object()) {
            for (k, v) in source {
                target.insert(k.clone(), v.clone());
            }
        }
        raw
    }

    #[test]
    fn generation_is_deterministic_and_shaped_per_contract() {
        let snapshot = json!([
            record(
                "app-ok",
                &json!(["arm64_sequoia"]),
                &json!(
                    {"artifacts": [{"app": ["A.app"]}], "ruby_source_path": "Casks/a/app-ok.rb",
                     "ruby_source_checksum": {"sha256": "B".repeat(64)}}
                ),
            ),
            record(
                "zoom",
                &json!(["arm64_sequoia"]),
                &json!(
                    {"artifacts": [{"installer": [{"script": {}}]}]}
                ),
            ),
            record(
                "disabled",
                &json!(["arm64_sequoia", "x86_64_linux"]),
                &json!({"disabled": true}),
            ),
        ]);
        let one = generate(&snapshot, &pin()).expect("generates");
        let two = generate(&snapshot, &pin()).expect("generates");
        assert_eq!(one.catalog, two.catalog);
        assert!(one.catalog.ends_with('\n') && !one.catalog.ends_with("\n\n"));

        let catalog: Value = serde_json::from_str(&one.catalog).unwrap();
        assert_eq!(catalog["schema"], "pkg-cask-catalog/4");
        assert_eq!(catalog["macosBaseline"], "15.7.7");
        assert_eq!(
            catalog["targets"],
            json!(["aarch64-darwin", "x86_64-linux"])
        );
        assert_eq!(catalog["generator"]["version"], "0.1.0");
        // inputs map keyed by source id with kind; raw is NULL for
        // json-snapshot inputs (client contract: url/sha256 already
        // name the exact pinned bytes).
        let input = &catalog["inputs"]["homebrew/cask"];
        assert_eq!(input["revision"], "rev");
        assert_eq!(input["kind"], "json-snapshot");
        assert!(input["raw"].is_null(), "raw must be null for snapshots");
        assert_eq!(input["license"], "BSD-2-Clause (Homebrew Cask data)");
        // entries keyed by the single qualified identity.
        let entry = &catalog["entries"]["homebrew/cask/app-ok"];
        assert_eq!(entry["source"], "homebrew/cask");
        assert_eq!(entry["token"], "app-ok");
        assert_eq!(
            entry["origin"],
            json!({"path": "Casks/a/app-ok.rb", "sha256": "b".repeat(64)})
        );
        assert_eq!(entry["targets"]["aarch64-darwin"]["status"], "eligible");
        assert_eq!(
            entry["targets"]["aarch64-darwin"]["plan"]["artifacts"][0]["target"],
            "A.app"
        );
        let zoom = &catalog["entries"]["homebrew/cask/zoom"];
        assert_eq!(
            zoom["targets"]["aarch64-darwin"]["reason"],
            "installer-script"
        );
        // Snapshot origin fields that are absent leave origin null.
        assert!(zoom["origin"].is_null());
        for token in ["app-ok", "zoom"] {
            assert!(
                catalog["entries"][format!("homebrew/cask/{token}")]["targets"]["x86_64-linux"]
                    .is_object()
            );
        }
        assert_eq!(
            one.coverage,
            "aarch64-darwin: 3 records, 1 eligible\n  disabled: 1\n  installer-script: 1\n\
             x86_64-linux: 3 records, 0 eligible\n  disabled: 1\n  unsupported-platform: 2\n"
        );
    }

    #[test]
    fn one_malformed_record_is_isolated_not_fatal() {
        // No supported_platforms on one record: excluded, generation lives.
        let snapshot = json!([
            record("ok", &json!(["arm64_sequoia"]), &json!({})),
            record("broken", &json!(null), &json!({})),
        ]);
        let out = generate(&snapshot, &pin()).expect("generates");
        let catalog: Value = serde_json::from_str(&out.catalog).unwrap();
        assert_eq!(
            catalog["entries"]["homebrew/cask/broken"]["targets"]["aarch64-darwin"]["reason"],
            "malformed-record"
        );
    }

    #[test]
    fn integrity_failures_abort_without_output() {
        let pin = pin();
        let dup = json!([
            record("a", &json!(["arm64_sequoia"]), &json!({})),
            record("a", &json!(["arm64_sequoia"]), &json!({})),
        ]);
        assert!(generate(&dup, &pin).is_err());
        let no_family = json!([
            record("a", &json!(null), &json!({})),
            record("b", &json!(null), &json!({})),
        ]);
        assert!(
            generate(&no_family, &pin).is_err(),
            "a snapshot without the platform field family fails closed"
        );
        let bad_token = json!([record("Bad Token", &json!(["arm64_sequoia"]), &json!({}))]);
        assert!(generate(&bad_token, &pin).is_err());
        let no_token = json!([{"version": "1"}]);
        assert!(generate(&no_token, &pin).is_err());
    }

    #[test]
    fn at_tokens_are_valid_keys() {
        let snapshot = json!([record(
            "1password@nightly",
            &json!(["arm64_sequoia"]),
            &json!(
                {"artifacts": [{"app": ["1Password.app"]}]}
            )
        )]);
        let out = generate(&snapshot, &pin()).expect("generates");
        let catalog: Value = serde_json::from_str(&out.catalog).unwrap();
        assert!(
            catalog["entries"]["homebrew/cask/1password@nightly"]["targets"]["aarch64-darwin"]
                ["plan"]
                .is_object()
        );
        assert_eq!(
            out.coverage,
            "aarch64-darwin: 1 records, 1 eligible\nx86_64-linux: 1 records, 0 eligible\n  unsupported-platform: 1\n"
        );
    }

    #[test]
    fn bad_snapshot_origin_is_dropped_not_guessed() {
        for bad in [
            "",
            "../escape.rb",
            "a/../../escape.rb",
            "/abs.rb",
            "Casks//x.rb",
            "Casks/./x.rb",
            "Casks/x.rb/",
            "Casks\\x.rb",
            "Casks/\u{7f}x.rb",
        ] {
            let snapshot = json!([record(
                "x",
                &json!(["arm64_sequoia"]),
                &json!({"ruby_source_path": bad,
                         "ruby_source_checksum": {"sha256": "b".repeat(64)}})
            )]);
            let out = generate(&snapshot, &pin()).expect("generates");
            let catalog: Value = serde_json::from_str(&out.catalog).unwrap();
            assert!(
                catalog["entries"]["homebrew/cask/x"]["origin"].is_null(),
                "{bad}"
            );
        }
    }

    #[test]
    fn failed_publish_leaves_the_previous_catalog_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("catalog.json"), "previous").expect("write previous");
        // A read-only directory makes the staged write fail before rename.
        let subdir = dir.path().join("locked");
        std::fs::create_dir(&subdir).expect("mkdir");
        let mut permissions = std::fs::metadata(&subdir).unwrap().permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(&subdir, permissions).unwrap();
        let failed = write_catalog(&subdir, "new");
        assert!(
            failed.is_err(),
            "publishing into a read-only directory must fail"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("catalog.json")).unwrap(),
            "previous"
        );
        let ok = write_catalog(dir.path(), "new");
        assert!(ok.is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("catalog.json")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("catalog.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }
}
