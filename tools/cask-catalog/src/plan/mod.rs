//! Typed artifact plans with path safety.
//!
//! Plans are uniform `{kind, source, target}` objects. Vendor strings are
//! data only: sources are validated against the recognized anchors and
//! rejections exclude the token with `malformed-record`, never a build.

use crate::classify::{Exclusion, ExclusionReason};
use serde::Serialize;
use serde_json::Value;

/// One typed artifact inside a plan. `target` is null only for `pkg`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanArtifact {
    /// Plan kind: app, binary, pkg, appimage, manpage, or a completion.
    pub kind: &'static str,
    /// Vendor-relative source (may start `$APPDIR/` for binaries).
    pub source: String,
    /// Link or rename target (link basename, bundle name, or null for pkg).
    pub target: Option<String>,
}

/// The fetchable source and its verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanSource {
    /// Vendor URL of the archive or image.
    pub url: String,
    /// SHA-256 of the fetched bytes.
    pub sha256: String,
}

/// The archive container handling the builder must apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanArchive {
    /// `auto` (content-sniffed extraction), `raw-binary`, or `appimage`.
    pub kind: &'static str,
}

/// A full typed plan for one eligible target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Where to fetch and how to verify.
    pub source: PlanSource,
    /// Container handling.
    pub archive: PlanArchive,
    /// Every installable artifact, in metadata order.
    pub artifacts: Vec<PlanArtifact>,
    /// Declared minimum macOS from `depends_on macos >=`, or null.
    #[serde(rename = "minMacos")]
    pub min_macos: Option<String>,
    /// Declared maximum macOS from `depends_on maximum_macos <=`, or
    /// null. Linux records never carry macOS runtime requirements.
    #[serde(rename = "maxMacos")]
    pub max_macos: Option<String>,
}

struct CollectedArtifacts {
    artifacts: Vec<PlanArtifact>,
    naked: bool,
}

/// Artifact kinds ignored as inert lifecycle metadata.
const INERT: [&str; 3] = ["zap", "uninstall", "generate_completions_from_executable"];
/// Execution stanzas: present means the record needs actions pkg never runs.
const EXECUTION: [&str; 6] = [
    "installer",
    "preflight_steps",
    "postflight_steps",
    "uninstall_preflight_steps",
    "uninstall_postflight_steps",
    "generated_script",
];
/// The plan kind when the raw artifact kind is supported on this target.
fn allowed_plan_kind(kind: &str, system: &str) -> Option<&'static str> {
    match kind {
        "app" if system == "aarch64-darwin" => Some("app"),
        "pkg" if system == "aarch64-darwin" => Some("pkg"),
        "app_image" if system == "x86_64-linux" => Some("appimage"),
        "binary" => Some("binary"),
        "manpage" => Some("manpage"),
        "bash_completion" => Some("bash-completion"),
        "zsh_completion" => Some("zsh-completion"),
        "fish_completion" => Some("fish-completion"),
        _ => None,
    }
}

/// Validate one vendor-relative source path.
///
/// Allowed: `$APPDIR/`-anchored paths and plain relative paths. Rejected:
/// absolute paths, `..` traversal, other `$` anchors, NUL/newline, and
/// empty segments around `/`.
fn safe_source(source: &str, allow_appdir: bool) -> Result<(), String> {
    let anchored = source.strip_prefix("$APPDIR/");
    let path = anchored.unwrap_or(source);
    if source.is_empty()
        || source.chars().any(|c| c.is_ascii_control())
        || (anchored.is_some() && !allow_appdir)
        || path.starts_with(['/', '$'])
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(format!("unsafe artifact source {source:?}"));
    }
    Ok(())
}

/// Validate a link/bundle target name: a single safe path segment.
fn safe_target_name(name: &str) -> Result<(), String> {
    // One path component: no slash, no dot/dotdot, no controls.
    if name.is_empty()
        || name.contains('/')
        || name == "."
        || name == ".."
        || name.chars().any(|c| c.is_ascii_control())
    {
        return Err(format!("unsafe link name {name:?}"));
    }
    Ok(())
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Strip the final filename extension, like Ruby's
/// `File.basename(name, File.extname(name))`. A final dot is an extension
/// delimiter only when some non-dot character occurs before it (Ruby
/// `File.extname` skips leading dots until a non-dot appears), so
/// `.bashrc`, `..bashrc`, and `...` have no extension, but `.tar.gz`
/// strips `.gz` and `foo.` strips to `foo`.
fn strip_final_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if name[..dot].contains(|c: char| c != '.') => &name[..dot],
        _ => name,
    }
}

/// The install destination namespace of one artifact kind.
/// Collisions are only real inside the same namespace.
fn destination_namespace(kind: &str) -> &str {
    match kind {
        "binary" | "appimage" => "bin",
        _ => kind,
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactOptions<'a> {
    #[serde(borrow, default, deserialize_with = "non_null_str")]
    source: Option<&'a str>,
    #[serde(borrow, default, deserialize_with = "non_null_str")]
    target: Option<&'a str>,
}

fn non_null_str<'de, D>(deserializer: D) -> Result<Option<&'de str>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Walk one artifact entry's item list as (source, optional rename) pairs.
///
/// Cask JSON pairs a string source with an optional following
/// `{"target": name}` object. A self-contained `{"source","target"}`
/// object is also accepted. Every other option object is active metadata
/// the builders do not represent: installer `choices` are an
/// `installer-script` exclusion, unknown keys and malformed target values
/// are `malformed-record`. Nothing is dropped silently.
fn item_pairs(items: &[Value]) -> Result<Vec<(&str, Option<&str>)>, Exclusion> {
    let mut pairs = Vec::new();
    let mut pending: Option<&str> = None;
    for item in items {
        match item {
            Value::String(source) => {
                if let Some(source) = pending.take() {
                    pairs.push((source, None));
                }
                pending = Some(source);
            }
            Value::Object(map) => {
                if map.is_empty() {
                    return Err(Exclusion::malformed(
                        "empty artifact option object".to_string(),
                    ));
                }
                if map.contains_key("choices") {
                    return Err(Exclusion::installer_script("pkg choices"));
                }
                let ArtifactOptions { source, target } = serde::Deserialize::deserialize(item)
                    .map_err(|error| {
                        Exclusion::malformed(format!("invalid artifact options: {error}"))
                    })?;
                match source {
                    Some(source) => pairs.push((source, target)),
                    None => {
                        if let Some(source) = pending.take() {
                            pairs.push((source, target));
                        } else {
                            return Err(Exclusion::malformed(format!(
                                "rename object without a source: {map:?}"
                            )));
                        }
                    }
                }
            }
            other => {
                return Err(Exclusion::malformed(format!(
                    "unsupported artifact item shape: {other}"
                )));
            }
        }
    }
    if let Some(source) = pending {
        pairs.push((source, None));
    }
    Ok(pairs)
}

/// True when the URL's path (ignoring any query) ends in `.AppImage`.
fn url_path_ends_with_appimage(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| parsed.path().ends_with(".AppImage"))
}

/// Build the plan for one effective record on one target system.
///
/// Every installable artifact in the record must be honored; one bad stanza
/// rejects the whole record with a bounded error. URL and checksum presence
/// are checked by the caller (classify) before this runs. `minMacos` is
/// owned by classify (the single strict parser of `depends_on macos`); the
/// plan is built with it null and classify fills it in.
///
/// # Errors
///
/// Returns a `Exclusion` when the record has malformed, unsupported, or
/// conflicting artifact declarations.
pub fn build_plan(eff: &Value, system: &str) -> Result<Plan, Exclusion> {
    let url = eff
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let sha = eff
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let collected = collect_artifacts(eff, system)?;
    let CollectedArtifacts {
        mut artifacts,
        naked,
    } = collected;

    let has_app_image = artifacts.iter().any(|artifact| artifact.kind == "appimage");
    if has_app_image && artifacts.iter().any(|artifact| artifact.kind != "appimage") {
        return Err(Exclusion::unsupported_kinds(&[
            "app_image mixed with other artifacts".to_string(),
        ]));
    }
    for multiple in ["pkg", "appimage"] {
        if artifacts.iter().filter(|a| a.kind == multiple).count() > 1 {
            return Err(Exclusion::unsupported_kinds(&[format!(
                "multiple {multiple} artifacts are unsupported"
            )]));
        }
    }
    if artifacts.is_empty() {
        return Err(Exclusion::bare(ExclusionReason::NoInstallableArtifact));
    }
    // A naked container is one bare file: it can name exactly one binary.
    if naked && !(artifacts.len() == 1 && artifacts[0].kind == "binary") {
        return Err(Exclusion::with_detail(
            ExclusionReason::UnsupportedContainer,
            "naked container requires exactly one binary artifact".to_string(),
        ));
    }
    // Two artifacts claiming the same destination (namespace + final
    // normalized target) would collide in the output prefix; detectable
    // here, so it never reaches a builder. Different namespaces with the
    // same basename (a binary and a completion) coexist.
    let mut claimed: Vec<(&str, &str)> = Vec::new();
    for artifact in &artifacts {
        if let Some(target) = artifact.target.as_deref() {
            let slot = (destination_namespace(artifact.kind), target);
            if claimed.contains(&slot) {
                return Err(Exclusion::malformed(format!(
                    "duplicate artifact target {target:?} in {}",
                    slot.0
                )));
            }
            claimed.push(slot);
        }
    }

    // On Linux, convert a direct .AppImage download with one matching binary
    // to an AppImage plan. Keep the normalized target. Reject other binary
    // .AppImage forms because running them as raw executables needs host libfuse.
    let promoted_appimage = if system == "x86_64-linux" {
        normalize_appimage_binaries(&mut artifacts, url_path_ends_with_appimage(&url))?
    } else {
        false
    };

    let archive_kind = if has_app_image || promoted_appimage {
        "appimage"
    } else if naked {
        "raw-binary"
    } else {
        "auto"
    };

    Ok(Plan {
        source: PlanSource { url, sha256: sha },
        archive: PlanArchive { kind: archive_kind },
        artifacts,
        min_macos: None,
        max_macos: None,
    })
}

fn collect_artifacts(eff: &Value, system: &str) -> Result<CollectedArtifacts, Exclusion> {
    let raw_entries = eff
        .get("artifacts")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut entries = Vec::new();
    for (index, entry) in raw_entries.iter().enumerate() {
        let Some(map) = entry.as_object() else {
            return Err(Exclusion::malformed(format!(
                "artifact entry {index} is not an object: {entry}"
            )));
        };
        entries.extend(map.iter().map(|(key, value)| (key.clone(), value)));
    }
    let container = eff.get("container").filter(|v| !v.is_null());
    let naked = match container {
        None => false,
        Some(container) => {
            let kind = container.get("type").and_then(Value::as_str);
            let inert = container
                .as_object()
                .is_some_and(|m| m.keys().all(|k| k == "type" || k == "allow_untrusted"));
            if kind == Some("naked") && inert {
                true
            } else if container.get("nested").is_some() {
                let nested = container
                    .get("nested")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                return Err(Exclusion::with_detail(
                    ExclusionReason::UnsupportedContainer,
                    format!("nested: {nested}"),
                ));
            } else {
                return Err(Exclusion::with_detail(
                    ExclusionReason::UnsupportedContainer,
                    container.to_string(),
                ));
            }
        }
    };
    let mut unsupported = Vec::new();
    let mut artifacts = Vec::new();
    for (kind, value) in entries {
        if kind == "target" || kind == "allow_untrusted" || INERT.contains(&kind.as_str()) {
            continue;
        }
        if EXECUTION.contains(&kind.as_str()) {
            return Err(Exclusion::installer_script(&kind));
        }
        let Some(plan_kind) = allowed_plan_kind(&kind, system) else {
            unsupported.push(kind);
            continue;
        };
        let Some(items) = value.as_array() else {
            return Err(Exclusion::malformed(format!(
                "artifact {kind} is not a list"
            )));
        };
        for (source, rename) in item_pairs(items)? {
            let (source, target) =
                normalize_item(&kind, source, rename).map_err(Exclusion::malformed)?;
            artifacts.push(PlanArtifact {
                kind: plan_kind,
                source,
                target,
            });
        }
    }
    if !unsupported.is_empty() {
        unsupported.sort();
        unsupported.dedup();
        return Err(Exclusion::unsupported_kinds(&unsupported));
    }
    Ok(CollectedArtifacts { artifacts, naked })
}

/// Convert a Linux binary to an `AppImage` plan only for a direct `.AppImage`
/// download with one matching, unanchored binary source. Keep its target.
/// Reject archived, mixed, anchored, or mismatched binary `.AppImage` sources.
fn normalize_appimage_binaries(
    artifacts: &mut [PlanArtifact],
    direct_appimage_url: bool,
) -> Result<bool, Exclusion> {
    if artifacts.iter().any(|a| a.kind == "appimage") {
        // Existing explicit app_image plan; nothing to normalize.
        return Ok(false);
    }
    // Any binary `.AppImage` source refuses when the download is not a
    // direct `.AppImage` URL, including `$APPDIR`-anchored ones. Only
    // the direct promotion predicate requires an unanchored source.
    let any_appimage_source = artifacts
        .iter()
        .any(|a| a.kind == "binary" && a.source.ends_with(".AppImage"));
    if !direct_appimage_url {
        if any_appimage_source {
            return Err(Exclusion::with_detail(
                ExclusionReason::UnsupportedContainer,
                "AppImage inside archive unsupported; use a direct .AppImage download".to_string(),
            ));
        }
        return Ok(false);
    }
    let lone_unanchored_appimage =
        artifacts.len() == 1 && any_appimage_source && !artifacts[0].source.starts_with("$APPDIR/");
    if lone_unanchored_appimage {
        artifacts[0].kind = "appimage";
        return Ok(true);
    }
    if artifacts.len() > 1 {
        return Err(Exclusion::unsupported_kinds(&[
            "app_image mixed with other artifacts".to_string(),
        ]));
    }
    Err(Exclusion::with_detail(
        ExclusionReason::UnsupportedContainer,
        "direct .AppImage URL requires exactly one binary source ending .AppImage".to_string(),
    ))
}

/// Normalize one artifact item into (source, target) plan fields.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the app builder requires the exact case-sensitive .app bundle suffix"
)]
fn normalize_item(
    kind: &str,
    source: &str,
    rename: Option<&str>,
) -> Result<(String, Option<String>), String> {
    safe_source(source, kind == "binary")?;
    if kind == "pkg" {
        if let Some(rename) = rename {
            return Err(format!("pkg rename is unsupported: {rename}"));
        }
        return Ok((source.to_string(), None));
    }
    let original = rename.map_or_else(|| basename(source), ToString::to_string);
    let target = match kind {
        "app" if !original.ends_with(".app") => {
            return Err(format!("app target {original:?} does not end in .app"));
        }
        "app_image" => {
            let named = basename(&original);
            named
                .strip_suffix(".AppImage")
                .unwrap_or(&named)
                .to_string()
        }
        "bash_completion" | "zsh_completion" | "fish_completion" => {
            safe_target_name(&original)?;
            let stripped = strip_final_extension(&original);
            match kind {
                "zsh_completion" if !original.starts_with('_') => format!("_{stripped}"),
                "fish_completion" if !original.ends_with(".fish") => format!("{stripped}.fish"),
                "bash_completion" => stripped.to_string(),
                _ => original,
            }
        }
        _ => original,
    };
    safe_target_name(&target)?;
    Ok((source.to_string(), Some(target)))
}

#[cfg(test)]
mod tests;
