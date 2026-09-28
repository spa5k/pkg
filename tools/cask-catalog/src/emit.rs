//! Whole-catalog generation: integrity checks, one deterministic catalog,
//! and a printable coverage summary.

use crate::classify::TargetStatus;
use crate::effective::{self, RawRecord};
use crate::{CATALOG_SCHEMA, GENERATOR_NAME, GENERATOR_VERSION, MACOS_BASELINE, Pin, TARGETS};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Provenance at the top of the catalog envelope.
#[derive(Serialize)]
struct Provenance<'a> {
    schema: &'a str,
    generator: Generator<'a>,
    input: PinRef<'a>,
    targets: [&'a str; 2],
    #[serde(rename = "macosBaseline")]
    macos_baseline: &'a str,
}

#[derive(Serialize)]
struct Generator<'a> {
    name: &'a str,
    version: &'a str,
}

#[derive(Serialize)]
struct PinRef<'a> {
    url: &'a str,
    revision: &'a str,
    sha256: &'a str,
    license: &'a str,
}

/// One catalog entry: base display fields plus per-target decisions.
#[derive(Serialize)]
struct Entry {
    token: String,
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    homepage: Option<String>,
    targets: BTreeMap<&'static str, TargetStatus>,
}

/// The single committed document plus its printable coverage summary.
pub struct Generated {
    /// The full catalog envelope with plans.
    pub catalog: String,
    /// Human-readable coverage counts (printed, never committed).
    pub coverage: String,
}

/// Run whole-catalog generation over the decoded snapshot.
///
/// Integrity failures abort before any output exists: a snapshot where no
/// record carries `supported_platforms` (the field family is gone),
/// duplicate tokens, records without a token, and tokens that cannot be
/// catalog keys. One malformed record is isolated: it is excluded, never
/// a whole-catalog failure.
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
    let mut by_token: BTreeMap<String, RawRecord> = BTreeMap::new();
    for raw in records {
        let record = RawRecord::new(raw.clone());
        let Some(token) = record.token() else {
            return Err("integrity: snapshot record without a token string".to_string());
        };
        if !crate::token::is_valid_token(token) {
            return Err(format!(
                "integrity: token {token:?} is not a usable catalog key \
                 (allowed: [a-z0-9][a-z0-9+._@-]* without '..')"
            ));
        }
        if by_token.contains_key(token) {
            return Err(format!("integrity: duplicate token {token}"));
        }
        by_token.insert(token.to_string(), record);
    }

    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut counts: BTreeMap<&'static str, BTreeMap<String, usize>> = TARGETS
        .iter()
        .map(|system| (*system, BTreeMap::new()))
        .collect();
    let mut eligible_counts: BTreeMap<&'static str, usize> =
        TARGETS.iter().map(|system| (*system, 0)).collect();

    for (token, record) in &by_token {
        let mut targets: BTreeMap<&'static str, TargetStatus> = BTreeMap::new();
        for system in TARGETS {
            let status = crate::classify::classify_target(record, system, MACOS_BASELINE);
            if status.status == "eligible" {
                if let Some(count) = eligible_counts.get_mut(system) {
                    *count += 1;
                }
            } else if let Some(reasons) = counts.get_mut(system) {
                let reason = status
                    .reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                *reasons.entry(reason).or_insert(0) += 1;
            }
            targets.insert(system, status);
        }
        entries.insert(
            token.clone(),
            Entry {
                token: token.clone(),
                name: effective::name_field(&record.raw),
                description: effective::str_field(&record.raw, "desc"),
                version: effective::str_field(&record.raw, "version"),
                homepage: effective::str_field(&record.raw, "homepage"),
                targets,
            },
        );
    }

    #[derive(Serialize)]
    struct Catalog<'a> {
        #[serde(flatten)]
        provenance: Provenance<'a>,
        entries: BTreeMap<String, Entry>,
    }
    fn make_provenance<'a>(schema: &'a str, pin: &'a Pin) -> Provenance<'a> {
        Provenance {
            schema,
            generator: Generator {
                name: GENERATOR_NAME,
                version: GENERATOR_VERSION,
            },
            input: PinRef {
                url: &pin.url,
                revision: &pin.revision,
                sha256: &pin.sha256,
                license: &pin.license,
            },
            targets: TARGETS,
            macos_baseline: MACOS_BASELINE,
        }
    }

    let catalog = Catalog {
        provenance: make_provenance(CATALOG_SCHEMA, pin),
        entries,
    };
    let catalog = serde_json::to_string(&catalog).map_err(|e| e.to_string())?;

    let mut coverage = String::new();
    use std::fmt::Write as _;
    for system in TARGETS {
        let eligible = eligible_counts.get(system).copied().unwrap_or(0);
        let _ = writeln!(
            coverage,
            "{system}: {} records, {eligible} eligible",
            by_token.len()
        );
        if let Some(reasons) = counts.get(system) {
            for (reason, count) in reasons {
                let _ = writeln!(coverage, "  {reason}: {count}");
            }
        }
    }

    Ok(Generated {
        catalog: format!("{catalog}\n"),
        coverage,
    })
}

/// Publish the catalog atomically: one unique same-directory temporary
/// file, persisted over the target once the document is complete.
///
/// A failure leaves the previously committed catalog untouched.
pub fn write_catalog(dir: &Path, catalog: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("cannot stage a temporary file in {}: {e}", dir.display()))?;
    std::io::Write::write_all(&mut tmp.as_file(), catalog.as_bytes())
        .map_err(|e| format!("cannot write the staged catalog: {e}"))?;
    let target = dir.join("catalog.json");
    tmp.persist(&target)
        .map_err(|e| format!("cannot publish {}: {e}", target.display()))?;
    // NamedTempFile keeps 0600; committed data is world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let permissions = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&target, permissions)
            .map_err(|e| format!("cannot make {} readable: {e}", target.display()))?;
    }
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
            "token": token, "version": "1", "url": "https://v/x.zip",
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
                {"artifacts": [{"app": ["A.app"]}]})
            ),
            record(
                "zoom",
                &json!(["arm64_sequoia"]),
                &json!(
                {"artifacts": [{"installer": [{"script": {}}]}]})
            ),
        ]);
        let one = generate(&snapshot, &pin()).expect("generates");
        let two = generate(&snapshot, &pin()).expect("generates");
        assert_eq!(one.catalog, two.catalog);

        let catalog: Value = serde_json::from_str(&one.catalog).unwrap();
        assert_eq!(catalog["schema"], "pkg-cask-catalog/2");
        assert_eq!(catalog["macosBaseline"], "15.7.7");
        assert_eq!(
            catalog["targets"],
            json!(["aarch64-darwin", "x86_64-linux"])
        );
        assert_eq!(catalog["generator"]["version"], "0.1.0");
        assert_eq!(catalog["input"]["revision"], "rev");
        assert_eq!(
            catalog["entries"]["app-ok"]["targets"]["aarch64-darwin"]["status"],
            "eligible"
        );
        assert!(
            catalog["entries"]["app-ok"]["targets"]["aarch64-darwin"]["plan"]["artifacts"][0]["target"]
                == "A.app"
        );
        assert_eq!(
            catalog["entries"]["zoom"]["targets"]["aarch64-darwin"]["reason"],
            "installer-script"
        );
        for token in ["app-ok", "zoom"] {
            assert!(catalog["entries"][token]["targets"]["x86_64-linux"].is_object());
        }
        assert!(
            one.coverage
                .contains("aarch64-darwin: 2 records, 1 eligible")
        );
        assert!(one.coverage.contains("installer-script: 1"));
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
            catalog["entries"]["broken"]["targets"]["aarch64-darwin"]["reason"],
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
            catalog["entries"]["1password@nightly"]["targets"]["aarch64-darwin"]["plan"]
                .is_object()
        );
    }

    #[test]
    fn failed_publish_leaves_the_previous_catalog_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("catalog.json"), "previous").expect("write previous");
        // A read-only directory makes the staged write fail before rename.
        let subdir = dir.path().join("locked");
        std::fs::create_dir(&subdir).expect("mkdir");
        let mut permissions = std::fs::metadata(&subdir).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt as _;
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
    }
}
