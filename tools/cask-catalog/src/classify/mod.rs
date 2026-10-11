//! Per-target eligibility: effective record in, bounded status out.
//!
//! OS support comes only from explicit constraints (`supported_platforms`
//! membership, `depends_on` arch entries). Variation presence and artifact
//! kinds are never OS evidence. Every nonempty formula or cask dependency
//! excludes the token in this version (manager contract): no closure
//! engine, no guessed resolution.

use crate::effective::{self, RawRecord};
use crate::plan::{self, Plan};
use crate::version::{less_than as version_lt, parse as version_parts};
use serde::ser::SerializeStruct as _;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::net::IpAddr;

/// Why one target is excluded.
///
/// One closed vocabulary shared by the official classifier and the raw
/// tap capture; [`ExclusionReason::as_str`] is the emitted `reason`
/// code. A typed reason keeps the code and the counting in emit from
/// ever disagreeing about a spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        match self {
            Self::Disabled => "disabled",
            Self::Deprecated => "deprecated",
            Self::UnsupportedPlatform => "unsupported-platform",
            Self::ArchUnsupported => "arch-unsupported",
            Self::UnsupportedDownloadSpec => "unsupported-download-spec",
            Self::FormulaDependency => "formula-dependency",
            Self::CaskDependencyIntegration => "cask-dependency-integration",
            Self::MissingUrl => "missing-url",
            Self::UnsupportedUrl => "unsupported-url",
            Self::MissingChecksum => "missing-checksum",
            Self::MinimumOs => "minimum-os",
            Self::MalformedRecord => "malformed-record",
            Self::UnsupportedArtifact => "unsupported-artifact",
            Self::UnsupportedContainer => "unsupported-container",
            Self::InstallerScript => "installer-script",
            Self::NoInstallableArtifact => "no-installable-artifact",
            Self::RubyLoadError => "ruby-load-error",
            Self::OriginMissing => "origin-missing",
            Self::OriginMismatch => "origin-mismatch",
        }
    }
}

/// The one outcome for a target: an eligible typed plan, or an
/// exclusion owning its reason and optional detail.
///
/// This replaces the old flat `TargetStatus` whose `status` string plus
/// optional `kind`/`reason`/`detail`/`plan` fields could disagree. Now
/// eligibility IS the variant, an exclusion always carries a typed
/// reason, and the summary kind is derived from the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetOutcome {
    /// Eligible for this target; owns the typed plan.
    Eligible {
        /// The install plan for this target.
        plan: Plan,
    },
    /// Excluded for this target; owns the reason code and detail.
    Excluded {
        /// Why the target is excluded.
        reason: ExclusionReason,
        /// Optional human-readable detail line.
        detail: Option<String>,
    },
}

/// One target's published decision.
///
/// The effective version and homepage are shared display metadata kept
/// outside the outcome (excluded tokens keep their known version and
/// homepage); the outcome owns everything eligibility-specific.
///
/// Renamed from `TargetStatus` when the invalid string/optional
/// combinations were made unrepresentable; every in-repo consumer is
/// in this workspace (emit, tap capture, tests). External users of the
/// maintainer tool's Rust API, if any exist, are unknown.
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
            outcome: TargetOutcome::Excluded {
                reason,
                detail: Some(detail),
            },
        }
    }

    /// Whether this decision carries an eligible plan.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        matches!(self.outcome, TargetOutcome::Eligible { .. })
    }

    /// The typed plan, present only when eligible.
    #[must_use]
    pub fn plan(&self) -> Option<&Plan> {
        match &self.outcome {
            TargetOutcome::Eligible { plan } => Some(plan),
            TargetOutcome::Excluded { .. } => None,
        }
    }

    /// The emitted exclusion reason code, or `None` when eligible.
    #[must_use]
    pub fn reason_code(&self) -> Option<&'static str> {
        match &self.outcome {
            TargetOutcome::Eligible { .. } => None,
            TargetOutcome::Excluded { reason, .. } => Some(reason.as_str()),
        }
    }

    /// The exclusion detail line, or `None` when eligible or absent.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match &self.outcome {
            TargetOutcome::Eligible { .. } => None,
            TargetOutcome::Excluded { detail, .. } => detail.as_deref(),
        }
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
        match &self.outcome {
            TargetOutcome::Eligible { plan } => {
                object.serialize_field("status", "eligible")?;
                object.serialize_field("kind", &Some(summary_kind(plan)))?;
                object.serialize_field("reason", &None::<&str>)?;
                object.serialize_field("detail", &None::<String>)?;
            }
            TargetOutcome::Excluded { reason, detail } => {
                object.serialize_field("status", "excluded")?;
                object.serialize_field("kind", &None::<&str>)?;
                object.serialize_field("reason", &Some(reason.as_str()))?;
                object.serialize_field("detail", detail)?;
            }
        }
        object.serialize_field("version", &self.version)?;
        object.serialize_field("homepage", &self.homepage)?;
        object.serialize_field("plan", &self.plan())?;
        object.end()
    }
}

/// Exclude with a reason and no detail (outcome level).
fn excluded(reason: ExclusionReason) -> TargetOutcome {
    TargetOutcome::Excluded {
        reason,
        detail: None,
    }
}

/// Exclude as a malformed record with detail text.
fn excluded_malformed(detail: String) -> TargetOutcome {
    excluded_with_detail(ExclusionReason::MalformedRecord, detail)
}

/// Exclude with a reason and a detail line.
fn excluded_with_detail(reason: ExclusionReason, detail: String) -> TargetOutcome {
    TargetOutcome::Excluded {
        reason,
        detail: Some(detail),
    }
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
///
/// The bounds are COLLECTED and VALIDATED on every target first
/// (numeric shape, single-value operators, coherent ordering); only
/// then is the baseline decision applied, and only on Darwin, so an
/// inverted or malformed range is consistently malformed on both
/// targets. A Linux plan therefore carries null macOS bounds while an
/// inverted or malformed range is still refused there.
/// The macOS bounds a record declares for one target (min, max).
pub type MacosBounds = (Option<String>, Option<String>);

fn macos_constraints(
    eff: &Value,
    system: &str,
    baseline: &str,
) -> Result<MacosBounds, (ExclusionReason, String)> {
    let Some(depends) = eff.get("depends_on").filter(|v| !v.is_null()) else {
        return Ok((None, None));
    };
    let mut min: Option<String> = None;
    let mut max: Option<String> = None;
    for (key, mode) in [("macos", "minimum"), ("maximum_macos", "maximum")] {
        let Some(field) = depends.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(field) = field.as_object() else {
            return Err((
                ExclusionReason::MalformedRecord,
                format!("{key} constraint is not an object"),
            ));
        };
        for (op, values) in field {
            let Some([value]) = values.as_array().map(std::vec::Vec::as_slice) else {
                return Err((
                    ExclusionReason::MalformedRecord,
                    format!("{key} {op:?} is not a one-element array"),
                ));
            };
            let Some(value) = value.as_str() else {
                return Err((
                    ExclusionReason::MalformedRecord,
                    format!("{key} {op:?} value is not a string"),
                ));
            };
            if version_parts(value).is_none() {
                return Err((
                    ExclusionReason::MalformedRecord,
                    format!("non-numeric {key} version {value:?}"),
                ));
            }
            match (mode, op.as_str()) {
                ("minimum", ">=") => {
                    min = Some(value.to_string());
                }
                ("maximum", "<=") => {
                    max = Some(value.to_string());
                }
                _ => {
                    return Err((
                        ExclusionReason::MalformedRecord,
                        format!("unsupported {key} operator {op:?}"),
                    ));
                }
            }
        }
    }
    // A declared range must be coherent: an empty minimum above a
    // nonempty maximum is malformed, never a guessable interval.
    if let (Some(min), Some(max)) = (&min, &max)
        && version_lt(max, min) == Some(true)
    {
        return Err((
            ExclusionReason::MalformedRecord,
            format!("macOS range is inverted: >= {min} and <= {max}"),
        ));
    }
    // Only now, with the whole declared range parsed and coherent, is
    // the baseline decision applied — and only on Darwin. A malformed
    // or inverted range is malformed on every target; the baseline
    // never fires first and masks it.
    if system == "aarch64-darwin" {
        if let Some(min) = &min
            && version_lt(baseline, min) == Some(true)
        {
            return Err((
                ExclusionReason::MinimumOs,
                format!("requires macOS >= {min}; baseline is {baseline}"),
            ));
        }
        if let Some(max) = &max
            && version_lt(max, baseline) == Some(true)
        {
            return Err((
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
    let kinds: Vec<&str> = plan.artifacts.iter().map(|a| a.kind).collect();
    let has = |k: &str| kinds.contains(&k);
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
pub fn classify_target(record: &RawRecord, system: &str, baseline: &str) -> TargetDecision {
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
) -> TargetDecision {
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

fn dependency_exclusion(eff: &Value, system: &str) -> Option<TargetOutcome> {
    if system == "aarch64-darwin"
        && eff
            .get("depends_on")
            .is_some_and(|d| d.get("linux").is_some_and(|v| !v.is_null()))
    {
        return Some(excluded(ExclusionReason::UnsupportedPlatform));
    }
    let depends = eff.get("depends_on").filter(|v| !v.is_null())?;
    let Some(depends) = depends.as_object() else {
        return Some(excluded_malformed(
            "depends_on is not an object".to_string(),
        ));
    };
    for key in depends.keys() {
        if !KNOWN_DEPENDENCY_KEYS.contains(&key.as_str()) {
            return Some(excluded_malformed(format!(
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
        let Some(items) = list.as_array() else {
            return Some(excluded_malformed(format!(
                "depends_on.{key} is not an array"
            )));
        };
        if items.iter().any(|item| !item.is_string()) {
            return Some(excluded_malformed(format!(
                "depends_on.{key} has a non-string entry"
            )));
        }
        if !items.is_empty() {
            return Some(excluded(reason));
        }
    }
    None
}

fn unsupported_download_spec(eff: &Value) -> Option<TargetOutcome> {
    let specs = eff.get("url_specs").filter(|v| !v.is_null())?;
    let Some(map) = specs.as_object() else {
        return Some(excluded_malformed("url_specs is not an object".to_string()));
    };
    let active = map.keys().filter(|key| key.as_str() != "verified");
    let active: Vec<&str> = active.map(String::as_str).collect();
    if active.is_empty() {
        None
    } else {
        Some(excluded_with_detail(
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
            let Some(tags) = eff.get("supported_platforms").and_then(Value::as_array) else {
                return excluded_malformed("no supported_platforms".to_string());
            };
            if tags.iter().any(|tag| !tag.is_string()) {
                return excluded_malformed(
                    "supported_platforms has a non-string entry".to_string(),
                );
            }
            if !platform_supported(tags, system) {
                return excluded(ExclusionReason::UnsupportedPlatform);
            }
        }
        PlatformGate::Bool(upstream_supported) => {
            if !upstream_supported {
                return excluded(ExclusionReason::UnsupportedPlatform);
            }
        }
    }
    if let Some(detail) = arch_detail(eff, system) {
        return excluded_with_detail(ExclusionReason::ArchUnsupported, detail);
    }
    if let Some(outcome) = unsupported_download_spec(eff) {
        return outcome;
    }
    if let Some(outcome) = dependency_exclusion(eff, system) {
        return outcome;
    }
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
    if sha.is_empty()
        || sha == "no_check"
        || sha.len() != 64
        || !sha.chars().all(|c| c.is_ascii_hexdigit())
    {
        return excluded(ExclusionReason::MissingChecksum);
    }
    let (min_macos, max_macos) = match macos_constraints(eff, system, baseline) {
        Ok(bounds) => bounds,
        Err((reason, detail)) => return excluded_with_detail(reason, detail),
    };
    match plan::build_plan(eff, system) {
        Ok(mut plan) => {
            plan.min_macos = min_macos;
            plan.max_macos = max_macos;
            TargetOutcome::Eligible { plan }
        }
        Err(plan::PlanError::UnsupportedKinds(kinds)) => excluded_with_detail(
            ExclusionReason::UnsupportedArtifact,
            format!("artifact kinds: {}", kinds.join(", ")),
        ),
        Err(plan::PlanError::UnsupportedContainer(detail)) => {
            excluded_with_detail(ExclusionReason::UnsupportedContainer, detail)
        }
        Err(plan::PlanError::InstallerScript(stanza)) => excluded_with_detail(
            ExclusionReason::InstallerScript,
            format!("stanza: {stanza}"),
        ),
        Err(plan::PlanError::NoInstallableArtifact) => {
            excluded(ExclusionReason::NoInstallableArtifact)
        }
        Err(plan::PlanError::Malformed(detail)) => excluded_malformed(detail),
    }
}

#[cfg(test)]
mod tests;
