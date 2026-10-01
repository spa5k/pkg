//! Integration test over the adapted Homebrew fixture records.
//!
//! Loads `fixtures/upstream/records.json` and the manually authored
//! `fixtures/upstream/expectations.json`, runs the real `classify_target`
//! for every record on both published targets, and compares status, kind,
//! reason, and (when present) the typed plan artifacts. Excluded entries
//! must carry no plan. No classifier logic is copied here: the test only
//! reads data and asserts outcomes.

// This integration-test crate is built without `cfg(test)`, so the
// crate-root test opt-out does not apply; the same reasoned exception
// is declared here once.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests abort on broken fixtures; production never panics"
)]

use cask_catalog::MACOS_BASELINE;
use cask_catalog::TARGETS;
use cask_catalog::classify::{self, TargetDecision};
use cask_catalog::effective::RawRecord;
use serde_json::Value;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/fixtures/upstream/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("cannot read fixture {name}: {e}"))
}

/// One (token, system) pair and its manual expectation.
fn expectation<'a>(all: &'a Value, token: &str, system: &str) -> &'a Value {
    all.get(token)
        .and_then(|t| t.get(system))
        .unwrap_or_else(|| panic!("no expectation for token {token:?} on {system}"))
}

fn assert_field(actual: &Value, expected: &Value, field: &str, token: &str, system: &str) {
    assert_eq!(
        actual, expected,
        "{token} / {system}: {field} mismatch ({actual:?} != {expected:?})"
    );
}

fn status_json(status: &TargetDecision) -> Value {
    serde_json::to_value(status).expect("status serializes")
}

#[test]
fn fixture_records_match_manual_expectations_on_both_targets() {
    let records: Vec<Value> =
        serde_json::from_str(&fixture("records.json")).expect("records.json is a JSON array");
    let expected: Value =
        serde_json::from_str(&fixture("expectations.json")).expect("expectations is a JSON object");
    assert!(!records.is_empty(), "fixture records must not be empty");

    // Every fixture token has expectations and vice versa: no unused data.
    let mut tokens: Vec<&str> = records
        .iter()
        .map(|r| r["token"].as_str().expect("record has a token"))
        .collect();
    let mut expected_tokens: Vec<&str> = expected
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        // The `note` key is provenance, not a token expectation.
        .filter(|k| *k != "note")
        .collect();
    tokens.sort_unstable();
    expected_tokens.sort_unstable();
    assert_eq!(
        tokens, expected_tokens,
        "record tokens and expectation keys must agree"
    );

    for record in &records {
        let token = record["token"].as_str().expect("token").to_string();
        for system in TARGETS {
            let exp = expectation(&expected, &token, system);
            let status =
                classify::classify_target(&RawRecord::new(record.clone()), system, MACOS_BASELINE);
            let actual = status_json(&status);

            assert_field(&actual["status"], &exp["status"], "status", &token, system);
            assert_field(&actual["kind"], &exp["kind"], "kind", &token, system);
            assert_field(&actual["reason"], &exp["reason"], "reason", &token, system);

            // Plans: present exactly when the expectation lists artifacts.
            match exp.get("planArtifacts") {
                Some(want) => {
                    let plan = status.plan().unwrap_or_else(|| {
                        panic!("{token} / {system}: expected an eligible plan, got none")
                    });
                    for (i, want_artifact) in
                        want.as_array().expect("artifact list").iter().enumerate()
                    {
                        let got = plan
                            .artifacts
                            .get(i)
                            .unwrap_or_else(|| panic!("{token} / {system}: missing artifact {i}"));
                        assert_eq!(
                            got.kind, want_artifact["kind"],
                            "{token} / {system}: artifact {i} kind"
                        );
                        assert_eq!(
                            got.source, want_artifact["source"],
                            "{token} / {system}: artifact {i} source"
                        );
                        assert_eq!(
                            got.target.as_deref(),
                            want_artifact["target"].as_str(),
                            "{token} / {system}: artifact {i} target"
                        );
                    }
                    assert_eq!(
                        plan.artifacts.len(),
                        want.as_array().expect("list").len(),
                        "{token} / {system}: artifact count"
                    );
                    if let Some(url) = exp.get("planSourceUrl") {
                        assert_eq!(
                            plan.source.url,
                            url.as_str().unwrap(),
                            "{token} / {system}: plan url"
                        );
                    }
                    if let Some(kind) = exp.get("archiveKind") {
                        assert_eq!(
                            plan.archive.kind,
                            kind.as_str().unwrap(),
                            "{token} / {system}: archive kind"
                        );
                    }
                }
                None => {
                    assert!(
                        status.plan().is_none(),
                        "{token} / {system}: excluded entry must have no plan"
                    );
                }
            }
        }
    }
}
