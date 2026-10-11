use super::*;
use serde_json::json;

fn record(platforms: &[&str], extra: &Value) -> RawRecord {
    let mut raw = json!({
        "token": "t",
        "version": "1",
        "url": "https://example.com/x.zip",
        "sha256": "a".repeat(64),
        "artifacts": [{"binary": ["t"]}],
        "supported_platforms": platforms,
    });
    if let (Some(target), Some(source)) = (raw.as_object_mut(), extra.as_object()) {
        for (k, v) in source {
            target.insert(k.clone(), v.clone());
        }
    }
    RawRecord::new(raw)
}

const BASE: &str = "15.7.7";

#[test]
fn platform_scope_comes_only_from_supported_platforms() {
    let both = ["arm64_sequoia", "x86_64_linux"];
    assert!(classify_target(&record(&both, &json!({})), "aarch64-darwin", BASE).is_eligible());
    assert!(classify_target(&record(&both, &json!({})), "x86_64-linux", BASE).is_eligible());
    let mac_only = ["arm64_sequoia"];
    let linux_status = classify_target(&record(&mac_only, &json!({})), "x86_64-linux", BASE);
    assert_eq!(linux_status.reason_code(), Some("unsupported-platform"));
    let arm_linux_only = ["arm64_linux"];
    assert_eq!(
        classify_target(&record(&arm_linux_only, &json!({})), "aarch64-darwin", BASE).reason_code(),
        Some("unsupported-platform")
    );
}

#[test]
fn all_dependencies_exclude_in_this_version() {
    let formula = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"formula": ["neovim"]}}),
    );
    assert_eq!(
        classify_target(&formula, "aarch64-darwin", BASE).reason_code(),
        Some("formula-dependency")
    );
    let cask = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"cask": ["docker"]}}),
    );
    assert_eq!(
        classify_target(&cask, "aarch64-darwin", BASE).reason_code(),
        Some("cask-dependency-integration")
    );
    // Empty dependency lists do not exclude.
    let none = record(&["arm64_sequoia"], &json!({"depends_on": {"formula": []}}));
    assert!(classify_target(&none, "aarch64-darwin", BASE).is_eligible());
}

#[test]
fn macos_versions_compare_numerically_with_zero_fill() {
    let ok = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {">=": ["15"]}}}),
    );
    assert!(classify_target(&ok, "aarch64-darwin", BASE).is_eligible());
    let too_new = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {">=": ["26"]}}}),
    );
    let status = classify_target(&too_new, "aarch64-darwin", BASE);
    assert_eq!(status.reason_code(), Some("minimum-os"));
    assert!(status.detail().unwrap().contains("26"));
    let capped = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"maximum_macos": {"<=": ["14.9"]}}}),
    );
    assert_eq!(
        classify_target(&capped, "aarch64-darwin", BASE).reason_code(),
        Some("minimum-os")
    );
    let unknown_op = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {"~>": ["12"]}}}),
    );
    assert_eq!(
        classify_target(&unknown_op, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    // An operator the generator cannot evaluate (here `==` with a
    // version list) is a malformed shape, never a silent pass.
    let exact_list = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {"==": ["15", "26"]}}}),
    );
    assert_eq!(
        classify_target(&exact_list, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn macos_constraint_shapes_fail_closed() {
    // A non-object constraint and a multi-value array never pass.
    let not_object = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": ">= 12"}}),
    );
    assert_eq!(
        classify_target(&not_object, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    let multi = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {">=": ["13", "14"]}}}),
    );
    assert_eq!(
        classify_target(&multi, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    // The satisfied minimum is still carried in the plan.
    let ok = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"macos": {">=": ["12"]}}}),
    );
    let status = classify_target(&ok, "aarch64-darwin", BASE);
    assert_eq!(
        status.plan().expect("plan").min_macos.as_deref(),
        Some("12")
    );
}

#[test]
fn macos_requirements_never_constrain_linux() {
    // Same record, both platform tags, a macOS minimum above the
    // baseline: darwin is excluded minimum-os, linux stays eligible
    // with a null minMacos. The shape is still validated on linux.
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(&both, &json!({"depends_on": {"macos": {">=": ["26"]}}}));
    let darwin = classify_target(&rec, "aarch64-darwin", BASE);
    assert_eq!(darwin.reason_code(), Some("minimum-os"));
    let linux = classify_target(&rec, "x86_64-linux", BASE);
    assert!(linux.is_eligible());
    assert_eq!(linux.plan().cloned().expect("plan").min_macos, None);
    // maximum_macos limits nothing on linux either.
    let capped = record(
        &both,
        &json!({"depends_on": {"maximum_macos": {"<=": ["14.9"]}}}),
    );
    assert_eq!(
        classify_target(&capped, "aarch64-darwin", BASE).reason_code(),
        Some("minimum-os")
    );
    assert!(classify_target(&capped, "x86_64-linux", BASE).is_eligible());
    // Malformed shapes still exclude on linux.
    let junk = record(&both, &json!({"depends_on": {"macos": ">= 12"}}));
    assert_eq!(
        classify_target(&junk, "x86_64-linux", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn eligible_maximum_macos_reaches_the_plan() {
    // A tiny synthetic eligible record whose declared maximum is
    // above the baseline: the bound must reach the Darwin plan.
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(
        &both,
        &json!({"depends_on": {"maximum_macos": {"<=": ["15.9"]}}}),
    );
    let darwin = classify_target(&rec, "aarch64-darwin", BASE);
    assert!(darwin.is_eligible());
    let plan = darwin.plan().cloned().expect("plan");
    assert_eq!(plan.min_macos, None);
    assert_eq!(plan.max_macos.as_deref(), Some("15.9"));
    // The same record serializes the bound into the emitted index
    // entry (the client decodes `maxMacos` from exactly this shape).
    let emitted = serde_json::to_value(&darwin).expect("serializes");
    assert_eq!(emitted["plan"]["maxMacos"].as_str(), Some("15.9"));
    // Linux stays eligible with null bounds.
    let linux = classify_target(&rec, "x86_64-linux", BASE);
    assert!(linux.is_eligible());
    let linux_plan = linux.plan().cloned().expect("plan");
    assert_eq!(linux_plan.min_macos, None);
    assert_eq!(linux_plan.max_macos, None);
}

#[test]
fn inverted_macos_range_is_refused_on_every_target() {
    // Bounds are collected on ALL targets before any baseline
    // decision, so an inverted range is malformed on Linux too —
    // previously Linux accepted it because it never collected min.
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(
        &both,
        &json!({"depends_on": {
            "macos": {">=": ["16"]},
            "maximum_macos": {"<=": ["15.9"]}
        }}),
    );
    // Darwin refuses it as malformed too: the range is validated
    // whole before any baseline comparison runs.
    assert_eq!(
        classify_target(&rec, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    assert_eq!(
        classify_target(&rec, "x86_64-linux", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn oversized_macos_versions_are_refused_on_every_target() {
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(
        &both,
        &json!({"depends_on": {"macos": {">=": ["99999999999999999999"]}}}),
    );
    assert_eq!(
        classify_target(&rec, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    assert_eq!(
        classify_target(&rec, "x86_64-linux", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn dependency_shapes_and_unknown_requirements_fail_closed() {
    // A non-array formula list is a malformed record, not "no deps".
    let string_dep = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"formula": "neovim"}}),
    );
    assert_eq!(
        classify_target(&string_dep, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    // An unknown requirement key is never silently ignored.
    let unknown = record(&["arm64_sequoia"], &json!({"depends_on": {"x11": true}}));
    assert_eq!(
        classify_target(&unknown, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
    // A non-string dependency entry is malformed too.
    let junk = record(&["arm64_sequoia"], &json!({"depends_on": {"cask": [42]}}));
    assert_eq!(
        classify_target(&junk, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn nonbool_flags_and_platform_tags_are_never_eligible() {
    let string_disabled = record(
        &["arm64_sequoia"],
        &json!({"disabled": "discontinued upstream"}),
    );
    assert_eq!(
        classify_target(&string_disabled, "aarch64-darwin", BASE).reason_code(),
        Some("disabled")
    );
    let string_deprecated = record(&["arm64_sequoia"], &json!({"deprecated": "yes"}));
    assert_eq!(
        classify_target(&string_deprecated, "aarch64-darwin", BASE).reason_code(),
        Some("deprecated")
    );
    let junk_tag = record(
        &["arm64_sequoia"],
        &json!({"supported_platforms": ["arm64_sequoia", 42]}),
    );
    assert_eq!(
        classify_target(&junk_tag, "aarch64-darwin", BASE).reason_code(),
        Some("malformed-record")
    );
}

#[test]
fn missing_checksum_or_url_excludes() {
    let no_sha = record(&["arm64_sequoia"], &json!({"sha256": "no_check"}));
    assert_eq!(
        classify_target(&no_sha, "aarch64-darwin", BASE).reason_code(),
        Some("missing-checksum")
    );
    let no_url = record(&["arm64_sequoia"], &json!({"url": null}));
    assert_eq!(
        classify_target(&no_url, "aarch64-darwin", BASE).reason_code(),
        Some("missing-url")
    );
}

#[test]
fn linux_variation_merge_selects_linux_payload() {
    let mut raw = json!({
        "token": "op",
        "version": "2",
        "url": "https://example.com/mac.zip",
        "sha256": "a".repeat(64),
        "artifacts": [{"binary": ["op"]}],
        "supported_platforms": ["arm64_sequoia", "x86_64_linux"],
        "variations": {"x86_64_linux": {
            "url": "https://example.com/linux.zip",
            "sha256": "b".repeat(64),
            "artifacts": [{"binary": ["op-linux"]}]
        }}
    });
    let status = classify_target(&RawRecord::new(raw.clone()), "x86_64-linux", BASE);
    assert!(status.is_eligible());
    let plan = status.plan().expect("plan");
    assert_eq!(plan.source.url, "https://example.com/linux.zip");
    assert_eq!(plan.artifacts[0].source, "op-linux");
    // Darwin keeps the base record.
    raw.as_object_mut()
        .unwrap()
        .insert("artifacts".into(), json!([{"binary": ["op"]}]));
    let darwin = classify_target(&RawRecord::new(raw), "aarch64-darwin", BASE);
    assert_eq!(
        darwin.plan().expect("plan").source.url,
        "https://example.com/mac.zip"
    );
}

/// Parent probe regressions: all three records share the baseline
/// fields (URL <https://vendor.example/download>, sha "a"*64,
/// platforms `arm64_sequoia` + `x86_64_linux`) and all three must be
/// EXCLUDED on x86_64-linux.
#[test]
fn probe_regressions_on_linux() {
    let base = json!({
        "url": "https://vendor.example/download",
        "sha256": "a".repeat(64),
    });
    let wrong_arch = record(
        &["arm64_sequoia", "x86_64_linux"],
        &json!({"artifacts": [{"binary": ["tool"]}],
                     "depends_on": {"arch": [{"type": "arm", "bits": 64}]}}),
    );
    let with = |extra: Value| {
        let mut b = base.clone();
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        record(&["arm64_sequoia", "x86_64_linux"], &b)
    };
    let partial_malformed = with(json!({"artifacts": [{"binary": ["tool"]}, 42]}));
    let nested_target = with(json!({"artifacts": [{"binary": ["tool", {"target": "bin/tool"}]}]}));
    for (name, rec) in [
        ("wrong-arch", wrong_arch),
        ("partial-malformed", partial_malformed),
        ("nested-target", nested_target),
    ] {
        let status = classify_target(&rec, "x86_64-linux", BASE);
        assert!(!status.is_eligible(), "{name} must be excluded");
        match name {
            "wrong-arch" => assert_eq!(status.reason_code(), Some("arch-unsupported")),
            "partial-malformed" | "nested-target" => {
                assert_eq!(status.reason_code(), Some("malformed-record"));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn null_linux_requirement_clears_the_darwin_exclusion() {
    // Explicit null clears a requirement everywhere else in the
    // reader; a null linux requirement must not scope darwin away.
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(&both, &json!({"depends_on": {"linux": null}}));
    assert!(classify_target(&rec, "aarch64-darwin", BASE).is_eligible());
    // A non-null linux requirement still excludes darwin.
    let linux_req = record(&both, &json!({"depends_on": {"linux": "true"}}));
    assert_eq!(
        classify_target(&linux_req, "aarch64-darwin", BASE).reason_code(),
        Some("unsupported-platform")
    );
}

#[test]
fn destination_policy_is_public_https_only() {
    for ok in [
        "https://example.com/a.dmg",
        "https://140.82.112.3/x",
        "https://[2607:f8b0::9]/x",
        "https://gh.example.download/v",
        "https://example.com:443/a.dmg",
    ] {
        assert!(
            super::destination_allowed(ok).is_ok(),
            "{ok} must be allowed"
        );
    }
    for bad in [
        "http://example.com/a.dmg",
        "ftp://example.com/a",
        "file:///etc/passwd",
        "https://localhost/x",
        "https://sub.localhost/x",
        "https://host.local/x",
        "https://printer.lan/x",
        "https://box.internal/x",
        "https://127.0.0.1/x",
        "https://10.0.0.5/x",
        "https://192.168.1.10/x",
        "https://169.254.169.254/latest",
        "https://[::1]/x",
        "https://[fe80::1]/x",
        "https://[::ffff:127.0.0.1]/x",
        "https://user:pw@example.com/x",
        "https://224.0.0.1/x",
        "https://example.com:8443/x",
        "https://example.com/x y.zip",
        "https://example.com/x\\y.zip",
        "https:///x",
        "https://singlelabel/x",
        "not a url\u{7}",
    ] {
        assert!(
            super::destination_allowed(bad).is_err(),
            "{bad} must be refused"
        );
    }
}

#[test]
fn urls_with_control_characters_are_not_fetchable() {
    for bad in [
        "https://example.com/a\u{1}b",
        "https://example.com/a\tb",
        "https://example.com/a\u{d}\u{a}b",
    ] {
        let rec = record(&["arm64_sequoia"], &json!({"url": bad}));
        let status = classify_target(&rec, "aarch64-darwin", BASE);
        assert_eq!(
            status.reason_code(),
            Some("unsupported-url"),
            "{bad:?} must not become a fetchable plan URL"
        );
    }
}

#[test]
fn summary_kinds_follow_artifacts() {
    // The kind is derived from the plan at emission, so the check
    // reads the serialized decision (the exact client shape).
    let app = record(
        &["arm64_sequoia"],
        &json!({"artifacts": [{"app": ["A.app"]}]}),
    );
    let app =
        serde_json::to_value(classify_target(&app, "aarch64-darwin", BASE)).expect("serializes");
    assert_eq!(app["kind"], "app");
    assert_eq!(app["status"], "eligible");
    let pkg = record(
        &["arm64_sequoia"],
        &json!({"artifacts": [{"pkg": ["A.pkg"]}]}),
    );
    let pkg =
        serde_json::to_value(classify_target(&pkg, "aarch64-darwin", BASE)).expect("serializes");
    assert_eq!(pkg["kind"], "pkg");
    // An excluded decision always carries a null kind and never a plan.
    let bad = record(
        &["arm64_sequoia"],
        &json!({"depends_on": {"formula": ["neovim"]}}),
    );
    let bad =
        serde_json::to_value(classify_target(&bad, "aarch64-darwin", BASE)).expect("serializes");
    assert_eq!(bad["status"], "excluded");
    assert!(bad["kind"].is_null());
    assert!(bad["plan"].is_null());
    assert_eq!(bad["reason"], "formula-dependency");
}

#[test]
fn serialized_field_order_and_nulls_match_the_catalog_contract() {
    // Exact byte contract of the per-target object: status, kind,
    // reason, detail, version, homepage, plan, with nulls where the
    // flat status emitted them.
    let both = ["arm64_sequoia", "x86_64_linux"];
    let rec = record(
        &both,
        &json!({"artifacts": [{"app": ["A.app"], "binary": ["t"]}]}),
    );
    let eligible =
        serde_json::to_value(classify_target(&rec, "aarch64-darwin", BASE)).expect("serializes");
    // Exact field ORDER comes from the serialized string (to_value
    // sorts keys); the prefix pins the contract order.
    let expected_prefix = concat!(
        "{\"status\":\"eligible\",\"kind\":\"app+cli\",",
        "\"reason\":null,\"detail\":null,",
        "\"version\":\"1\",\"homepage\":null,\"plan\":"
    );
    let eligible_text =
        serde_json::to_string(&classify_target(&rec, "aarch64-darwin", BASE)).expect("serializes");
    assert!(eligible_text.starts_with(expected_prefix));
    assert_eq!(eligible["status"], "eligible");
    assert_eq!(eligible["kind"], "app+cli");
    assert!(eligible["reason"].is_null());
    assert!(eligible["detail"].is_null());
    assert_eq!(eligible["version"], "1");
    assert!(eligible["plan"].is_object());
    let excluded = serde_json::to_value(classify_target(
        &record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {">=": ["26"]}}}),
        ),
        "aarch64-darwin",
        BASE,
    ))
    .expect("serializes");
    assert_eq!(excluded["status"], "excluded");
    assert!(excluded["kind"].is_null());
    assert_eq!(excluded["reason"], "minimum-os");
    assert!(excluded["detail"].is_string());
    assert!(excluded["plan"].is_null());
}
