//! Decide cask eligibility for each target.
//!
//! Use `supported_platforms` and `depends_on` architecture entries as OS
//! evidence. Variations and artifact kinds do not establish OS support.
//! Exclude tokens with nonempty formula or cask dependencies.

use crate::effective;
use crate::plan::{self, Plan};
use crate::version::{less_than as version_lt, parse as version_parts};
use serde::ser::SerializeStruct as _;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::net::IpAddr;

/// Why one target is excluded.
///
/// The official classifier and raw tap capture share these reason codes.
/// [`ExclusionReason::as_str`] supplies the code for output and counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum ExclusionReason {
    /// `disabled` flag set on the effective record.
    Disabled,
    /// `deprecated` flag set on the effective record.
    Deprecated,
    /// Target system is outside the record's platform scope.
    UnsupportedPlatform,
    /// `depends_on arch` excludes the target architecture.
    ArchUnsupported,
    /// Active `url_specs` the generator cannot honor.
    UnsupportedDownloadSpec,
    /// Nonempty `depends_on formula` list.
    FormulaDependency,
    /// Nonempty `depends_on cask` list.
    CaskDependencyIntegration,
    /// No fetchable `url` on the effective record.
    MissingUrl,
    /// `url` fails the safe-fetch destination policy.
    UnsupportedUrl,
    /// Missing or unusable `sha256`.
    MissingChecksum,
    /// Declared macOS range excludes the pinned baseline.
    MinimumOs,
    /// A shape the generator cannot evaluate safely.
    MalformedRecord,
    /// Artifact kinds the plan builder does not install.
    UnsupportedArtifact,
    /// Container shape the plan builder does not unpack.
    UnsupportedContainer,
    /// `installer script` stanza.
    InstallerScript,
    /// No installable artifact at all.
    NoInstallableArtifact,
    /// The upstream Ruby loader failed on this cask (raw taps).
    RubyLoadError,
    /// An eligible raw-tap entry lacked provenance (raw taps).
    OriginMissing,
    /// Raw-tap provenance failed validation against the source tree.
    OriginMismatch,
}

impl ExclusionReason {
    /// The emitted `reason` code (stable `pkg-cask-catalog/4` string).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// A refused Catalog Eligibility decision with one reason and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// The stable reason code for the refusal.
    pub reason: ExclusionReason,
    /// Optional human-readable detail.
    pub detail: Option<String>,
}

impl Exclusion {
    pub(crate) fn bare(reason: ExclusionReason) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    pub(crate) fn with_detail(reason: ExclusionReason, detail: String) -> Self {
        Self {
            reason,
            detail: Some(detail),
        }
    }

    pub(crate) fn malformed(detail: String) -> Self {
        Self::with_detail(ExclusionReason::MalformedRecord, detail)
    }

    pub(crate) fn unsupported_kinds(kinds: &[String]) -> Self {
        Self::with_detail(
            ExclusionReason::UnsupportedArtifact,
            format!("artifact kinds: {}", kinds.join(", ")),
        )
    }

    pub(crate) fn installer_script(stanza: &str) -> Self {
        Self::with_detail(
            ExclusionReason::InstallerScript,
            format!("stanza: {stanza}"),
        )
    }
}

/// An eligible plan or a recorded refusal for one target.
pub type TargetOutcome = Result<Plan, Exclusion>;

/// One target's published decision.
///
/// Keep the effective version and homepage outside the outcome so excluded
/// tokens retain them. The outcome holds the eligibility decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDecision {
    /// Effective merged version for this target, or null.
    pub version: Option<String>,
    /// Effective merged homepage for this target, or null.
    pub homepage: Option<String>,
    /// The decision outcome for this target.
    pub outcome: TargetOutcome,
}

impl TargetDecision {
    /// An excluded decision with no display metadata and a detail line.
    pub(crate) fn bare_with_detail(reason: ExclusionReason, detail: String) -> Self {
        Self {
            version: None,
            homepage: None,
            outcome: Err(Exclusion::with_detail(reason, detail)),
        }
    }

    /// Whether this decision carries an eligible plan.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        self.outcome.is_ok()
    }

    /// The typed plan, present only when eligible.
    #[must_use]
    pub fn plan(&self) -> Option<&Plan> {
        self.outcome.as_ref().ok()
    }

    /// The emitted exclusion reason code, or `None` when eligible.
    #[must_use]
    pub fn reason_code(&self) -> Option<&'static str> {
        self.outcome
            .as_ref()
            .err()
            .map(|exclusion| exclusion.reason.as_str())
    }
}

impl Serialize for TargetDecision {
    /// Manual implementation: the per-target object keeps the exact
    /// field names, order, and null behavior of `pkg-cask-catalog/4`
    /// (`status`, `kind`, `reason`, `detail`, `version`, `homepage`,
    /// `plan`). The summary `kind` is derived from the plan here, never
    /// stored, so it can never disagree with the plan it summarizes.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("TargetDecision", 7)?;
        let (status, kind, reason, detail) = match &self.outcome {
            Ok(plan) => ("eligible", Some(summary_kind(plan)), None, None),
            Err(Exclusion { reason, detail }) => {
                ("excluded", None, Some(reason.as_str()), detail.as_deref())
            }
        };
        object.serialize_field("status", status)?;
        object.serialize_field("kind", &kind)?;
        object.serialize_field("reason", &reason)?;
        object.serialize_field("detail", &detail)?;
        object.serialize_field("version", &self.version)?;
        object.serialize_field("homepage", &self.homepage)?;
        object.serialize_field("plan", &self.plan())?;
        object.end()
    }
}

/// Exclude with a reason and no detail (outcome level).
fn excluded(reason: ExclusionReason) -> TargetOutcome {
    Err(Exclusion::bare(reason))
}

/// Exclude with a reason and a detail line.
fn excluded_with_detail(reason: ExclusionReason, detail: String) -> TargetOutcome {
    Err(Exclusion::with_detail(reason, detail))
}

/// The declared minimum and maximum macOS versions for a target.
pub type MacosBounds = (Option<String>, Option<String>);

/// Parse the macOS constraints and compare them against the target baseline.
///
/// Accept only single-value `>=` and `<=` arrays with numeric versions.
/// Reject malformed or inverted ranges on every target before comparing
/// against the baseline. Use `malformed-record` for invalid constraints and
/// `minimum-os` for a baseline failure.
/// Apply the baseline and return macOS bounds only on `aarch64-darwin`.
/// Valid macOS constraints do not limit Linux plans, whose bounds are null.
fn macos_constraints(eff: &Value, system: &str, baseline: &str) -> Result<MacosBounds, Exclusion> {
    let Some(depends) = eff.get("depends_on").filter(|v| !v.is_null()) else {
        return Ok((None, None));
    };
    let mut min: Option<String> = None;
    let mut max: Option<String> = None;
    for (key, mode) in [("macos", "minimum"), ("maximum_macos", "maximum")] {
        let Some(field) = depends.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let field: std::collections::BTreeMap<&str, [&str; 1]> =
            serde::Deserialize::deserialize(field).map_err(|error| {
                Exclusion::malformed(format!("{key} constraint is malformed: {error}"))
            })?;
        for (op, [value]) in field {
            if version_parts(value).is_none() {
                return Err(Exclusion::malformed(format!(
                    "non-numeric {key} version {value:?}"
                )));
            }
            match (mode, op) {
                ("minimum", ">=") => {
                    min = Some(value.to_string());
                }
                ("maximum", "<=") => {
                    max = Some(value.to_string());
                }
                _ => {
                    return Err(Exclusion::malformed(format!(
                        "unsupported {key} operator {op:?}"
                    )));
                }
            }
        }
    }
    // A declared range must be coherent: an empty minimum above a
    // nonempty maximum is malformed, never a guessable interval.
    if let (Some(min), Some(max)) = (&min, &max)
        && version_lt(max, min) == Some(true)
    {
        return Err(Exclusion::malformed(format!(
            "macOS range is inverted: >= {min} and <= {max}"
        )));
    }
    // Validate the whole range before the Darwin baseline check so a baseline
    // failure cannot hide malformed bounds.
    if system == "aarch64-darwin" {
        if let Some(min) = &min
            && version_lt(baseline, min) == Some(true)
        {
            return Err(Exclusion::with_detail(
                ExclusionReason::MinimumOs,
                format!("requires macOS >= {min}; baseline is {baseline}"),
            ));
        }
        if let Some(max) = &max
            && version_lt(max, baseline) == Some(true)
        {
            return Err(Exclusion::with_detail(
                ExclusionReason::MinimumOs,
                format!("requires macOS <= {max}; baseline is {baseline}"),
            ));
        }
    }
    // macOS runtime requirements bind only the Darwin target; a Linux
    // plan publishes null bounds (the validation above still ran).
    if system != "aarch64-darwin" {
        return Ok((None, None));
    }
    Ok((min, max))
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
fn summary_kind(plan: &Plan) -> &'static str {
    let has = |kind: &str| plan.artifacts.iter().any(|artifact| artifact.kind == kind);
    if has("app") {
        return if has("binary") { "app+cli" } else { "app" };
    }
    if has("pkg") {
        return "pkg";
    }
    if has("appimage") {
        return "appimage";
    }
    "binary"
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
const KNOWN_DEPENDENCY_KEYS: [&str; 6] =
    ["formula", "cask", "macos", "maximum_macos", "arch", "linux"];

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
///
/// # Errors
///
/// Returns a policy reason when the URL is malformed or violates the
/// safe-fetch destination rules.
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
    let public = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => crate::net::ip_public(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => crate::net::ip_public(IpAddr::V6(ip)),
        Some(url::Host::Domain(_)) => {
            let Some((_, suffix)) = host.rsplit_once('.') else {
                return Err(format!(
                    "single-label host is not allowed: {:?}",
                    bounded(url)
                ));
            };
            if LOCAL_SUFFIXES.contains(&suffix.to_ascii_lowercase().as_str()) {
                return Err(format!(
                    "local host name is not allowed: {:?}",
                    bounded(url)
                ));
            }
            true
        }
        _ => true,
    };
    if !public {
        return Err(format!(
            "host is a local or non-public IP literal: {:?}",
            bounded(url)
        ));
    }
    Ok(())
}

/// Classify one record for one target system.
///
/// Merge the target variation into the base before making any decision.
/// Use the gate's platform evidence and retain display metadata for excluded tokens.
#[must_use]
pub fn classify_target(
    record: &Value,
    system: &str,
    baseline: &str,
    gate: PlatformGate,
) -> TargetDecision {
    let Ok(eff) = effective::effective_for(record, system) else {
        return TargetDecision::bare_with_detail(
            ExclusionReason::MalformedRecord,
            "variation is not an object".to_string(),
        );
    };
    let outcome = classify_effective(&eff, system, baseline, gate);
    // Display metadata is attached last, from the effective record, so
    // an excluded token keeps its known version and homepage.
    TargetDecision {
        version: effective::str_field(&eff, "version"),
        homepage: effective::str_field(&eff, "homepage"),
        outcome,
    }
}

fn string_array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], Exclusion> {
    value
        .as_array()
        .filter(|items| items.iter().all(Value::is_string))
        .map(Vec::as_slice)
        .ok_or_else(|| Exclusion::malformed(format!("{field} is not a string array")))
}

fn check_dependencies(eff: &Value) -> Result<(), Exclusion> {
    let Some(depends) = eff.get("depends_on").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let Some(depends) = depends.as_object() else {
        return Err(Exclusion::malformed(
            "depends_on is not an object".to_string(),
        ));
    };
    for key in depends.keys() {
        if !KNOWN_DEPENDENCY_KEYS.contains(&key.as_str()) {
            return Err(Exclusion::malformed(format!(
                "unhandled depends_on key {key:?}"
            )));
        }
    }
    for (key, reason) in [
        ("formula", ExclusionReason::FormulaDependency),
        ("cask", ExclusionReason::CaskDependencyIntegration),
    ] {
        let Some(list) = depends.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let items = string_array(list, key)?;
        if !items.is_empty() {
            return Err(Exclusion::bare(reason));
        }
    }
    Ok(())
}

fn check_download_specs(eff: &Value) -> Result<(), Exclusion> {
    let Some(specs) = eff.get("url_specs").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let Some(map) = specs.as_object() else {
        return Err(Exclusion::malformed(
            "url_specs is not an object".to_string(),
        ));
    };
    let active = map.keys().filter(|key| key.as_str() != "verified");
    let active: Vec<&str> = active.map(String::as_str).collect();
    if active.is_empty() {
        Ok(())
    } else {
        Err(Exclusion::with_detail(
            ExclusionReason::UnsupportedDownloadSpec,
            format!("url_specs: {}", active.join(", ")),
        ))
    }
}

fn classify_effective(
    eff: &Value,
    system: &str,
    baseline: &str,
    gate: PlatformGate,
) -> TargetOutcome {
    // Effective flags: a variation can set or clear them. Any non-null
    // value that is not exactly `false` (a string reason, a number) still
    // means the flag is set; such records are never silently eligible.
    let flagged = |key: &str| {
        eff.get(key)
            .is_some_and(|v| v != &Value::Null && v != &Value::Bool(false))
    };
    if flagged("disabled") {
        return excluded(ExclusionReason::Disabled);
    }
    if flagged("deprecated") {
        return excluded(ExclusionReason::Deprecated);
    }
    match gate {
        PlatformGate::Tags => {
            let tags = string_array(
                eff.get("supported_platforms").unwrap_or(&Value::Null),
                "supported_platforms",
            )?;
            if !effective::variation_key_for(system)
                .is_some_and(|expected| tags.iter().any(|tag| tag == expected))
            {
                return excluded(ExclusionReason::UnsupportedPlatform);
            }
        }
        PlatformGate::Bool(upstream_supported) => {
            if !upstream_supported {
                return excluded(ExclusionReason::UnsupportedPlatform);
            }
        }
    }
    if system == "aarch64-darwin"
        && eff
            .get("depends_on")
            .is_some_and(|d| d.get("linux").is_some_and(|v| !v.is_null()))
    {
        return excluded(ExclusionReason::UnsupportedPlatform);
    }
    if let Some(detail) = arch_detail(eff, system) {
        return excluded_with_detail(ExclusionReason::ArchUnsupported, detail);
    }
    check_download_specs(eff)?;
    check_dependencies(eff)?;
    let url = eff.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return excluded(ExclusionReason::MissingUrl);
    }
    // Destination policy mirrors safe-fetch exactly (https/443 to a
    // public destination, no credentials or ambiguous characters), so
    // no plan can advertise a URL the fetcher would refuse.
    if let Err(detail) = destination_allowed(url) {
        return excluded_with_detail(ExclusionReason::UnsupportedUrl, detail);
    }
    let sha = eff.get("sha256").and_then(Value::as_str).unwrap_or("");
    if !crate::valid_sha256(sha) {
        return excluded(ExclusionReason::MissingChecksum);
    }
    let (min_macos, max_macos) = macos_constraints(eff, system, baseline)?;
    let mut plan = plan::build_plan(eff, system)?;
    plan.min_macos = min_macos;
    plan.max_macos = max_macos;
    Ok(plan)
}

#[cfg(test)]
mod tests;
