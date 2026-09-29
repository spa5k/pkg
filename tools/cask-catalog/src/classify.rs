//! Per-target eligibility: effective record in, bounded status out.
//!
//! OS support comes only from explicit constraints (`supported_platforms`
//! membership, `depends_on` arch entries). Variation presence and artifact
//! kinds are never OS evidence. Every nonempty formula or cask dependency
//! excludes the token in this version (manager contract): no closure
//! engine, no guessed resolution.

use crate::effective::{self, RawRecord};
use crate::plan::{self, Plan};
use serde::Serialize;
use serde_json::Value;
use std::net::IpAddr;

/// One target's published decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetStatus {
    /// `eligible` or `excluded`.
    pub status: &'static str,
    /// Summary kind (app, app+cli, binary, pkg, appimage) or null.
    pub kind: Option<String>,
    /// Exclusion reason code, or null when eligible.
    pub reason: Option<String>,
    /// Human-readable exclusion detail, or null.
    pub detail: Option<String>,
    /// Effective merged version for this target, or null.
    pub version: Option<String>,
    /// Effective merged homepage for this target, or null.
    pub homepage: Option<String>,
    /// The typed plan, present only when eligible.
    pub plan: Option<Plan>,
}

/// Exclude with a reason and no detail.
fn excluded(reason: &str) -> TargetStatus {
    TargetStatus {
        status: "excluded",
        kind: None,
        reason: Some(reason.to_string()),
        detail: None,
        version: None,
        homepage: None,
        plan: None,
    }
}

/// Exclude as a malformed record with detail text.
fn excluded_malformed(detail: &str) -> TargetStatus {
    excluded_with_detail("malformed-record", detail)
}

/// Exclude with a reason and a detail line.
fn excluded_with_detail(reason: &str, detail: &str) -> TargetStatus {
    let mut status = excluded(reason);
    status.detail = Some(detail.to_string());
    status
}

/// Whether `supported_platforms` admits the target system.
///
/// `aarch64-darwin` maps to the exact `arm64_sequoia` platform (the
/// baseline macOS 15.7.7 is Sequoia); any other arm64 tag is not enough.
/// `x86_64-linux` needs its exact platform tag.
fn platform_supported(tags: &[Value], system: &str) -> bool {
    tags.iter()
        .filter_map(Value::as_str)
        .any(|tag| match system {
            "aarch64-darwin" => tag == "arm64_sequoia",
            "x86_64-linux" => tag == "x86_64_linux",
            _ => false,
        })
}

/// Parse a dotted numeric version into components (missing parts are not
/// filled here; comparison fills with zero).
fn version_parts(text: &str) -> Option<Vec<u64>> {
    if text.is_empty() {
        return None;
    }
    text.split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()
}

/// Numeric comparison with missing components as zero (contract rule).
fn version_lt(a: &str, b: &str) -> Option<bool> {
    let a = version_parts(a)?;
    let b = version_parts(b)?;
    let n = a.len().max(b.len());
    let fill = |v: &[u64]| {
        let mut v = v.to_vec();
        v.resize(n, 0);
        v
    };
    Some(fill(&a) < fill(&b))
}

/// A constraint failure: `malformed-record` for shapes the generator
/// cannot evaluate safely, `minimum-os` for a version decision.
///
/// Only single-value `>=`/`<=` arrays are accepted: every other shape
/// (non-object constraint, multi-value arrays, non-strings, unknown
/// operators, non-numeric versions) is rejected on every target, never
/// guessed. The baseline comparison and the `minMacos` output apply only
/// to `aarch64-darwin`: a macOS version requirement says nothing about
/// Linux, so a Linux-supporting record stays Linux-eligible whatever
/// macOS version it demands (and `maximum_macos` limits nothing there).
fn macos_constraints(
    eff: &Value,
    system: &str,
    baseline: &str,
) -> Result<Option<String>, (&'static str, String)> {
    let Some(depends) = eff.get("depends_on").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let mut min: Option<String> = None;
    for (key, mode) in [("macos", "minimum"), ("maximum_macos", "maximum")] {
        let Some(field) = depends.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(field) = field.as_object() else {
            return Err((
                "malformed-record",
                format!("{key} constraint is not an object"),
            ));
        };
        for (op, values) in field {
            let Some([value]) = values.as_array().map(std::vec::Vec::as_slice) else {
                return Err((
                    "malformed-record",
                    format!("{key} {op:?} is not a one-element array"),
                ));
            };
            let Some(value) = value.as_str() else {
                return Err((
                    "malformed-record",
                    format!("{key} {op:?} value is not a string"),
                ));
            };
            if version_parts(value).is_none() {
                return Err((
                    "malformed-record",
                    format!("non-numeric {key} version {value:?}"),
                ));
            }
            match (mode, op.as_str()) {
                ("minimum", ">=") => {
                    if system == "aarch64-darwin" {
                        if version_lt(baseline, value) == Some(true) {
                            return Err((
                                "minimum-os",
                                format!("requires macOS >= {value}; baseline is {baseline}"),
                            ));
                        }
                        min = Some(value.to_string());
                    }
                }
                ("maximum", "<=") => {
                    if system == "aarch64-darwin" && version_lt(value, baseline) == Some(true) {
                        return Err((
                            "minimum-os",
                            format!("requires macOS <= {value}; baseline is {baseline}"),
                        ));
                    }
                }
                _ => {
                    return Err((
                        "malformed-record",
                        format!("unsupported {key} operator {op:?}"),
                    ));
                }
            }
        }
    }
    Ok(min)
}

/// Evaluate `depends_on arch` against the target architecture.
fn arch_detail(eff: &Value, system: &str) -> Option<String> {
    let depends = eff.get("depends_on").filter(|v| !v.is_null())?;
    let arch = depends.get("arch").filter(|v| !v.is_null())?;
    let Some(entries) = arch.as_array().filter(|a| !a.is_empty()) else {
        return Some("arch constraint is not a nonempty array".to_string());
    };
    let (wants_type, wants_bits) = match system {
        "aarch64-darwin" => ("arm", 64),
        "x86_64-linux" => ("intel", 64),
        _ => return None,
    };
    for entry in entries {
        let Some(kind) = entry.get("type").and_then(Value::as_str) else {
            return Some(format!("unsupported arch constraint {entry}"));
        };
        let bits = entry.get("bits").and_then(Value::as_i64);
        let recognized = (kind == "arm" || kind == "intel") && bits == Some(64);
        if !recognized {
            return Some(format!("unsupported arch constraint {entry}"));
        }
        if kind == wants_type && bits == Some(wants_bits) {
            return None;
        }
    }
    Some(format!(
        "record declares {wants_type}/{wants_bits}-incompatible architectures"
    ))
}

/// The summary kind string for an eligible plan.
fn summary_kind(plan: &Plan) -> String {
    let kinds: Vec<&str> = plan.artifacts.iter().map(|a| a.kind).collect();
    let has = |k: &str| kinds.contains(&k);
    if has("app") {
        return if has("binary") {
            "app+cli".into()
        } else {
            "app".into()
        };
    }
    if has("pkg") {
        return "pkg".into();
    }
    if has("appimage") {
        return "appimage".into();
    }
    "binary".into()
}

/// How the classifier learns whether the target platform is supported.
///
/// `Tags` is the official-snapshot path: `supported_platforms` string
/// tags decide. `Bool` is the raw-tap path: the pinned Homebrew exporter
/// answered `platform_supported?(tag)` for the exact target inside the
/// simulated context; the classifier never invents platform tags for
/// records that do not carry them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformGate {
    /// Decide from `supported_platforms` tags (official JSON snapshots).
    Tags,
    /// Decide from the upstream boolean captured for this exact target.
    Bool(bool),
}

/// Local suffixes: single-label names never go to a vendor, and these
/// suffixes are local (mirrors safe-fetch `_LOCAL_SUFFIXES`).
const LOCAL_SUFFIXES: [&str; 7] = [
    "localhost",
    "local",
    "internal",
    "lan",
    "home",
    "corp",
    "localdomain",
];

/// Bound the URL text inside a diagnostic so an oversized record URL
/// cannot flood the catalog detail field.
fn bounded(url: &str) -> &str {
    match url.char_indices().nth(80) {
        Some((i, _)) => &url[..i],
        None => url,
    }
}

/// Whether a vendor URL is a fetchable destination under the safe-fetch policy.
///
/// Policy: https to port 443 only, no credentials, no
/// control characters, whitespace, or backslash, no localhost or local
/// suffix, no single-label domain, no invalid host, and public IP
/// literals only (checked through the typed host so IPv6 brackets and
/// IPv4-mapped addresses cannot hide a local destination). No DNS
/// happens here: names are checked by shape only.
pub fn destination_allowed(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL is empty".to_string());
    }
    if url.chars().any(|c| c.is_ascii_control()) {
        return Err(format!(
            "URL contains control characters: {:?}",
            bounded(url)
        ));
    }
    if url.chars().any(char::is_whitespace) || url.contains('\\') {
        return Err(format!(
            "URL contains whitespace or backslash: {:?}",
            bounded(url)
        ));
    }
    let parsed = url::Url::parse(url)
        .map_err(|e| format!("URL is not parseable ({e}): {:?}", bounded(url)))?;
    if parsed.scheme() != "https" {
        return Err(format!("scheme is not https: {:?}", bounded(url)));
    }
    if parsed.port().is_some_and(|port| port != 443) {
        return Err(format!("only port 443 is allowed: {:?}", bounded(url)));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "URL carries embedded credentials: {:?}",
            bounded(url)
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(format!("URL has an empty host: {:?}", bounded(url)));
    };
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return Err(format!("URL has an empty host: {:?}", bounded(url)));
    }
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) if !crate::net::ip_public(IpAddr::V4(ip)) => {
            return Err(format!(
                "host is a local or non-public IP literal: {:?}",
                bounded(url)
            ));
        }
        Some(url::Host::Ipv6(ip)) if !crate::net::ip_public(IpAddr::V6(ip)) => {
            return Err(format!(
                "host is a local or non-public IP literal: {:?}",
                bounded(url)
            ));
        }
        Some(url::Host::Domain(_)) => {
            let labels: Vec<&str> = host.split('.').collect();
            if labels.len() < 2 {
                return Err(format!(
                    "single-label host is not allowed: {:?}",
                    bounded(url)
                ));
            }
            if LOCAL_SUFFIXES.contains(&labels[labels.len() - 1].to_ascii_lowercase().as_str()) {
                return Err(format!(
                    "local host name is not allowed: {:?}",
                    bounded(url)
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Classify one record for one target system.
///
/// The effective record (base merged with the target variation) is
/// computed first; every decision — flags, platform scope, arch,
/// download specs, dependencies, URL, checksum, macOS minimums, plan —
/// reads the effective record. Display fields are attached last so
/// excluded tokens keep their known version and homepage.
#[must_use]
pub fn classify_target(record: &RawRecord, system: &str, baseline: &str) -> TargetStatus {
    classify_gated(record, system, baseline, PlatformGate::Tags)
}

/// Classify one raw-exported record for one target system.
///
/// Same pipeline as [`classify_target`], but platform evidence comes from
/// the upstream predicate boolean captured for this exact target by the
/// pinned Homebrew exporter, never from an invented tag list.
#[must_use]
pub fn classify_tap_target(
    record: &RawRecord,
    system: &str,
    baseline: &str,
    upstream_supported: bool,
) -> TargetStatus {
    classify_gated(
        record,
        system,
        baseline,
        PlatformGate::Bool(upstream_supported),
    )
}

fn classify_gated(
    record: &RawRecord,
    system: &str,
    baseline: &str,
    gate: PlatformGate,
) -> TargetStatus {
    let Ok(eff) = effective::effective_for(record, system) else {
        return excluded_malformed("variation is not an object");
    };
    let mut status = classify_effective(&eff, system, baseline, gate);
    status.version = effective::str_field(&eff, "version");
    status.homepage = effective::str_field(&eff, "homepage");
    status
}

fn classify_effective(
    eff: &Value,
    system: &str,
    baseline: &str,
    gate: PlatformGate,
) -> TargetStatus {
    // Effective flags: a variation can set or clear them. Any non-null
    // value that is not exactly `false` (a string reason, a number) still
    // means the flag is set; such records are never silently eligible.
    let flagged = |key: &str| {
        eff.get(key)
            .is_some_and(|v| v != &Value::Null && v != &Value::Bool(false))
    };
    if flagged("disabled") {
        return excluded("disabled");
    }
    if flagged("deprecated") {
        return excluded("deprecated");
    }
    match gate {
        PlatformGate::Tags => {
            let Some(tags) = eff.get("supported_platforms").and_then(Value::as_array) else {
                return excluded_malformed("no supported_platforms");
            };
            if tags.iter().any(|tag| !tag.is_string()) {
                return excluded_malformed("supported_platforms has a non-string entry");
            }
            if !platform_supported(tags, system) {
                return excluded("unsupported-platform");
            }
        }
        PlatformGate::Bool(upstream_supported) => {
            if !upstream_supported {
                return excluded("unsupported-platform");
            }
        }
    }
    // A declared linux requirement scopes the cask away from macOS;
    // an explicit null clears it, as null clears fields everywhere.
    if system == "aarch64-darwin"
        && eff
            .get("depends_on")
            .is_some_and(|d| d.get("linux").is_some_and(|v| !v.is_null()))
    {
        return excluded("unsupported-platform");
    }
    if let Some(detail) = arch_detail(eff, system) {
        return excluded_with_detail("arch-unsupported", &detail);
    }
    // Download specs: inert `verified` only; malformed shapes exclude.
    if let Some(specs) = eff.get("url_specs").filter(|v| !v.is_null()) {
        let Some(map) = specs.as_object() else {
            return excluded_malformed("url_specs is not an object");
        };
        let active: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "verified")
            .collect();
        if !active.is_empty() {
            return excluded_with_detail(
                "unsupported-download-spec",
                &format!("url_specs: {}", active.join(", ")),
            );
        }
    }
    // Dependencies: this version excludes rather than resolving anything.
    // Only the six known requirement keys are understood; every other key
    // is an active requirement the generator cannot evaluate, so it is a
    // malformed record, never a silently ignored extra. Non-array or
    // non-string formula/cask lists are malformed shapes, not "no deps".
    if let Some(depends) = eff.get("depends_on").filter(|v| !v.is_null()) {
        let Some(depends) = depends.as_object() else {
            return excluded_malformed("depends_on is not an object");
        };
        const KNOWN_KEYS: [&str; 6] =
            ["formula", "cask", "macos", "maximum_macos", "arch", "linux"];
        for key in depends.keys() {
            if !KNOWN_KEYS.contains(&key.as_str()) {
                return excluded_malformed(&format!("unhandled depends_on key {key:?}"));
            }
        }
        for (key, reason) in [
            ("formula", "formula-dependency"),
            ("cask", "cask-dependency-integration"),
        ] {
            let Some(list) = depends.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let Some(items) = list.as_array() else {
                return excluded_malformed(&format!("depends_on.{key} is not an array"));
            };
            if items.iter().any(|item| !item.is_string()) {
                return excluded_malformed(&format!("depends_on.{key} has a non-string entry"));
            }
            if !items.is_empty() {
                return excluded(reason);
            }
        }
    }
    let url = eff.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return excluded("missing-url");
    }
    // Destination policy mirrors safe-fetch exactly (https/443 to a
    // public destination, no credentials or ambiguous characters), so
    // no plan can advertise a URL the fetcher would refuse.
    if let Err(detail) = destination_allowed(url) {
        return excluded_with_detail("unsupported-url", &detail);
    }
    let sha = eff.get("sha256").and_then(Value::as_str).unwrap_or("");
    if sha.is_empty()
        || sha == "no_check"
        || sha.len() != 64
        || !sha.chars().all(|c| c.is_ascii_hexdigit())
    {
        return excluded("missing-checksum");
    }
    let min_macos = match macos_constraints(eff, system, baseline) {
        Ok(min) => min,
        Err((reason, detail)) => return excluded_with_detail(reason, &detail),
    };
    match plan::build_plan(eff, system) {
        Ok(mut plan) => {
            plan.min_macos = min_macos;
            let kind = summary_kind(&plan);
            TargetStatus {
                status: "eligible",
                kind: Some(kind),
                reason: None,
                detail: None,
                version: None,
                homepage: None,
                plan: Some(plan),
            }
        }
        Err(plan::PlanError::UnsupportedKinds(kinds)) => {
            let mut status = excluded("unsupported-artifact");
            status.detail = Some(format!("artifact kinds: {}", kinds.join(", ")));
            status
        }
        Err(plan::PlanError::UnsupportedContainer(detail)) => {
            let mut status = excluded("unsupported-container");
            status.detail = Some(detail);
            status
        }
        Err(plan::PlanError::InstallerScript(stanza)) => {
            let mut status = excluded("installer-script");
            status.detail = Some(format!("stanza: {stanza}"));
            status
        }
        Err(plan::PlanError::NoInstallableArtifact) => excluded("no-installable-artifact"),
        Err(plan::PlanError::Malformed(detail)) => excluded_malformed(&detail),
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            classify_target(&record(&both, &json!({})), "aarch64-darwin", BASE).status,
            "eligible"
        );
        assert_eq!(
            classify_target(&record(&both, &json!({})), "x86_64-linux", BASE).status,
            "eligible"
        );
        let mac_only = ["arm64_sequoia"];
        let linux_status = classify_target(&record(&mac_only, &json!({})), "x86_64-linux", BASE);
        assert_eq!(linux_status.reason.as_deref(), Some("unsupported-platform"));
        let arm_linux_only = ["arm64_linux"];
        assert_eq!(
            classify_target(&record(&arm_linux_only, &json!({})), "aarch64-darwin", BASE)
                .reason
                .as_deref(),
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
            classify_target(&formula, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("formula-dependency")
        );
        let cask = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"cask": ["docker"]}}),
        );
        assert_eq!(
            classify_target(&cask, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("cask-dependency-integration")
        );
        // Empty dependency lists do not exclude.
        let none = record(&["arm64_sequoia"], &json!({"depends_on": {"formula": []}}));
        assert_eq!(
            classify_target(&none, "aarch64-darwin", BASE).status,
            "eligible"
        );
    }

    #[test]
    fn macos_versions_compare_numerically_with_zero_fill() {
        let ok = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {">=": ["15"]}}}),
        );
        assert_eq!(
            classify_target(&ok, "aarch64-darwin", BASE).status,
            "eligible"
        );
        let too_new = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {">=": ["26"]}}}),
        );
        let status = classify_target(&too_new, "aarch64-darwin", BASE);
        assert_eq!(status.reason.as_deref(), Some("minimum-os"));
        assert!(status.detail.as_deref().unwrap().contains("26"));
        let capped = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"maximum_macos": {"<=": ["14.9"]}}}),
        );
        assert_eq!(
            classify_target(&capped, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("minimum-os")
        );
        let unknown_op = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {"~>": ["12"]}}}),
        );
        assert_eq!(
            classify_target(&unknown_op, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
        // An operator the generator cannot evaluate (here `==` with a
        // version list) is a malformed shape, never a silent pass.
        let exact_list = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {"==": ["15", "26"]}}}),
        );
        assert_eq!(
            classify_target(&exact_list, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
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
            classify_target(&not_object, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
        let multi = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {">=": ["13", "14"]}}}),
        );
        assert_eq!(
            classify_target(&multi, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
        // The satisfied minimum is still carried in the plan.
        let ok = record(
            &["arm64_sequoia"],
            &json!({"depends_on": {"macos": {">=": ["12"]}}}),
        );
        let status = classify_target(&ok, "aarch64-darwin", BASE);
        assert_eq!(status.plan.expect("plan").min_macos.as_deref(), Some("12"));
    }

    #[test]
    fn macos_requirements_never_constrain_linux() {
        // Same record, both platform tags, a macOS minimum above the
        // baseline: darwin is excluded minimum-os, linux stays eligible
        // with a null minMacos. The shape is still validated on linux.
        let both = ["arm64_sequoia", "x86_64_linux"];
        let rec = record(&both, &json!({"depends_on": {"macos": {">=": ["26"]}}}));
        let darwin = classify_target(&rec, "aarch64-darwin", BASE);
        assert_eq!(darwin.reason.as_deref(), Some("minimum-os"));
        let linux = classify_target(&rec, "x86_64-linux", BASE);
        assert_eq!(linux.status, "eligible");
        assert_eq!(linux.plan.expect("plan").min_macos, None);
        // maximum_macos limits nothing on linux either.
        let capped = record(
            &both,
            &json!({"depends_on": {"maximum_macos": {"<=": ["14.9"]}}}),
        );
        assert_eq!(
            classify_target(&capped, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("minimum-os")
        );
        assert_eq!(
            classify_target(&capped, "x86_64-linux", BASE).status,
            "eligible"
        );
        // Malformed shapes still exclude on linux.
        let junk = record(&both, &json!({"depends_on": {"macos": ">= 12"}}));
        assert_eq!(
            classify_target(&junk, "x86_64-linux", BASE)
                .reason
                .as_deref(),
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
            classify_target(&string_dep, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
        // An unknown requirement key is never silently ignored.
        let unknown = record(&["arm64_sequoia"], &json!({"depends_on": {"x11": true}}));
        assert_eq!(
            classify_target(&unknown, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
        // A non-string dependency entry is malformed too.
        let junk = record(&["arm64_sequoia"], &json!({"depends_on": {"cask": [42]}}));
        assert_eq!(
            classify_target(&junk, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
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
            classify_target(&string_disabled, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("disabled")
        );
        let string_deprecated = record(&["arm64_sequoia"], &json!({"deprecated": "yes"}));
        assert_eq!(
            classify_target(&string_deprecated, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("deprecated")
        );
        let junk_tag = record(
            &["arm64_sequoia"],
            &json!({"supported_platforms": ["arm64_sequoia", 42]}),
        );
        assert_eq!(
            classify_target(&junk_tag, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("malformed-record")
        );
    }

    #[test]
    fn missing_checksum_or_url_excludes() {
        let no_sha = record(&["arm64_sequoia"], &json!({"sha256": "no_check"}));
        assert_eq!(
            classify_target(&no_sha, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
            Some("missing-checksum")
        );
        let no_url = record(&["arm64_sequoia"], &json!({"url": null}));
        assert_eq!(
            classify_target(&no_url, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
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
        assert_eq!(status.status, "eligible");
        let plan = status.plan.expect("plan");
        assert_eq!(plan.source.url, "https://example.com/linux.zip");
        assert_eq!(plan.artifacts[0].source, "op-linux");
        // Darwin keeps the base record.
        raw.as_object_mut()
            .unwrap()
            .insert("artifacts".into(), json!([{"binary": ["op"]}]));
        let darwin = classify_target(&RawRecord::new(raw), "aarch64-darwin", BASE);
        assert_eq!(
            darwin.plan.expect("plan").source.url,
            "https://example.com/mac.zip"
        );
    }

    /// Parent probe regressions: all three records share the baseline
    /// fields (URL https://vendor.example/download, sha "a"*64,
    /// platforms arm64_sequoia + x86_64_linux) and all three must be
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
        let nested_target =
            with(json!({"artifacts": [{"binary": ["tool", {"target": "bin/tool"}]}]}));
        for (name, rec) in [
            ("wrong-arch", wrong_arch),
            ("partial-malformed", partial_malformed),
            ("nested-target", nested_target),
        ] {
            let status = classify_target(&rec, "x86_64-linux", BASE);
            assert_eq!(status.status, "excluded", "{name} must be excluded");
            match name {
                "wrong-arch" => assert_eq!(status.reason.as_deref(), Some("arch-unsupported")),
                "partial-malformed" | "nested-target" => {
                    assert_eq!(status.reason.as_deref(), Some("malformed-record"));
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
        assert_eq!(
            classify_target(&rec, "aarch64-darwin", BASE).status,
            "eligible"
        );
        // A non-null linux requirement still excludes darwin.
        let linux_req = record(&both, &json!({"depends_on": {"linux": "true"}}));
        assert_eq!(
            classify_target(&linux_req, "aarch64-darwin", BASE)
                .reason
                .as_deref(),
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
                status.reason.as_deref(),
                Some("unsupported-url"),
                "{bad:?} must not become a fetchable plan URL"
            );
        }
    }

    #[test]
    fn summary_kinds_follow_artifacts() {
        let app = record(
            &["arm64_sequoia"],
            &json!({"artifacts": [{"app": ["A.app"]}]}),
        );
        assert_eq!(
            classify_target(&app, "aarch64-darwin", BASE)
                .kind
                .as_deref(),
            Some("app")
        );
        let pkg = record(
            &["arm64_sequoia"],
            &json!({"artifacts": [{"pkg": ["A.pkg"]}]}),
        );
        assert_eq!(
            classify_target(&pkg, "aarch64-darwin", BASE)
                .kind
                .as_deref(),
            Some("pkg")
        );
    }
}
