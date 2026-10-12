//! Typed artifact plans with path safety.
//!
//! Plans are uniform `{kind, source, target}` objects. Vendor strings are
//! data only: sources are validated against the recognized anchors and
//! rejections exclude the token with `malformed-record`, never a build.

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
    has_app_image: bool,
    has_other_installable: bool,
    naked: bool,
}

/// Why a plan could not be built for the effective record's artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// An artifact kind outside the allowed set for the target OS.
    UnsupportedKinds(Vec<String>),
    /// An execution stanza that pkg must not run.
    InstallerScript(String),
    /// A container the builders do not handle (nested or unknown).
    UnsupportedContainer(String),
    /// Nothing installable in the record.
    NoInstallableArtifact,
    /// A path or link-name failed validation; detail names the field.
    Malformed(String),
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
/// Installable metadata kinds and the plan kind each maps to.
const KIND_MAP: &[(&str, &str)] = &[
    ("app", "app"),
    ("binary", "binary"),
    ("pkg", "pkg"),
    ("app_image", "appimage"),
    ("manpage", "manpage"),
    ("bash_completion", "bash-completion"),
    ("zsh_completion", "zsh-completion"),
    ("fish_completion", "fish-completion"),
];
/// Artifact kinds installable per target OS.
fn kind_allowed(kind: &str, system: &str) -> bool {
    match kind {
        "app" | "pkg" => system == "aarch64-darwin",
        "app_image" => system == "x86_64-linux",
        "binary" | "manpage" | "bash_completion" | "zsh_completion" | "fish_completion" => true,
        _ => false,
    }
}

fn plan_kind(kind: &str) -> Option<&'static str> {
    KIND_MAP
        .iter()
        .find(|(raw, _)| *raw == kind)
        .map(|(_, plan)| *plan)
}

/// Validate one vendor-relative source path.
///
/// Allowed: `$APPDIR/`-anchored paths and plain relative paths. Rejected:
/// absolute paths, `..` traversal, other `$` anchors, NUL/newline, and
/// empty segments around `/`.
fn safe_source(source: &str, allow_appdir: bool) -> Result<(), String> {
    if source.is_empty() {
        return Err("empty source".to_string());
    }
    if source.chars().any(|c| c.is_ascii_control()) {
        return Err(format!("control character in source {source:?}"));
    }
    let anchored = source.strip_prefix("$APPDIR/");
    if anchored.is_some() && !allow_appdir {
        return Err(format!(
            "$APPDIR anchor is only valid for binary sources: {source:?}"
        ));
    }
    let path = anchored.unwrap_or(source);
    if path.starts_with('/') || path.starts_with('$') {
        return Err(format!("absolute or unknown anchor in source {source:?}"));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("traversal or empty segment in source {source:?}"));
        }
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

/// Walk one artifact entry's item list as (source, optional rename) pairs.
///
/// Cask JSON pairs a string source with an optional following
/// `{"target": name}` object. A self-contained `{"source","target"}`
/// object is also accepted. Every other option object is active metadata
/// the builders do not represent: installer `choices` are an
/// `installer-script` exclusion, unknown keys and malformed target values
/// are `malformed-record`. Nothing is dropped silently.
fn item_pairs(items: &[Value]) -> Result<Vec<(String, Option<String>)>, PlanError> {
    let mut pairs = Vec::new();
    let mut pending: Option<String> = None;
    for item in items {
        match item {
            Value::String(source) => {
                if let Some(source) = pending.take() {
                    pairs.push((source, None));
                }
                pending = Some(source.clone());
            }
            Value::Object(map) => {
                if map.is_empty() {
                    return Err(PlanError::Malformed(
                        "empty artifact option object".to_string(),
                    ));
                }
                if map.contains_key("choices") {
                    return Err(PlanError::InstallerScript("pkg choices".to_string()));
                }
                let unknown: Vec<&str> = map
                    .keys()
                    .map(String::as_str)
                    .filter(|k| *k != "source" && *k != "target")
                    .collect();
                if !unknown.is_empty() {
                    return Err(PlanError::Malformed(format!(
                        "unsupported artifact options: {}",
                        unknown.join(", ")
                    )));
                }
                let target = match map.get("target") {
                    None => None,
                    Some(Value::String(target)) => Some(target.clone()),
                    Some(other) => {
                        return Err(PlanError::Malformed(format!(
                            "artifact target is not a string: {other}"
                        )));
                    }
                };
                match map.get("source") {
                    Some(Value::String(source)) => pairs.push((source.clone(), target)),
                    Some(other) => {
                        return Err(PlanError::Malformed(format!(
                            "artifact source is not a string: {other}"
                        )));
                    }
                    None => {
                        if let Some(source) = pending.take() {
                            pairs.push((source, target));
                        } else {
                            return Err(PlanError::Malformed(format!(
                                "rename object without a source: {map:?}"
                            )));
                        }
                    }
                }
            }
            other => {
                return Err(PlanError::Malformed(format!(
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
/// Returns a `PlanError` when the record has malformed, unsupported, or
/// conflicting artifact declarations.
pub fn build_plan(eff: &Value, system: &str) -> Result<Plan, PlanError> {
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
        has_app_image,
        has_other_installable,
        naked,
    } = collected;

    if has_app_image && has_other_installable {
        return Err(PlanError::UnsupportedKinds(vec![
            "app_image mixed with other artifacts".to_string(),
        ]));
    }
    for multiple in ["pkg", "appimage"] {
        if artifacts.iter().filter(|a| a.kind == multiple).count() > 1 {
            return Err(PlanError::UnsupportedKinds(vec![format!(
                "multiple {multiple} artifacts are unsupported"
            )]));
        }
    }
    if artifacts.is_empty() {
        return Err(PlanError::NoInstallableArtifact);
    }
    // A naked container is one bare file: it can name exactly one binary.
    if naked && !(artifacts.len() == 1 && artifacts[0].kind == "binary") {
        return Err(PlanError::UnsupportedContainer(
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
                return Err(PlanError::Malformed(format!(
                    "duplicate artifact target {target:?} in {}",
                    slot.0
                )));
            }
            claimed.push(slot);
        }
    }

    // The unambiguous direct AppImage download: on Linux, a URL whose
    // path ends `.AppImage` with exactly one binary whose source also
    // ends `.AppImage` is promoted to the appimage plan and archive,
    // keeping the already-normalized binary target verbatim. Every
    // other recognizable AppImage-as-binary shape refuses closed with a
    // bounded error instead of producing a raw executable that needs
    // host libfuse. Explicit `app_image` handling is untouched, and
    // non-Linux systems never enter this path.
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

fn collect_artifacts(eff: &Value, system: &str) -> Result<CollectedArtifacts, PlanError> {
    let raw_entries = eff
        .get("artifacts")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut entries = Vec::new();
    for (index, entry) in raw_entries.iter().enumerate() {
        let Some(map) = entry.as_object() else {
            return Err(PlanError::Malformed(format!(
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
                return Err(PlanError::UnsupportedContainer(format!("nested: {nested}")));
            } else {
                return Err(PlanError::UnsupportedContainer(container.to_string()));
            }
        }
    };
    let mut unsupported = Vec::new();
    let mut artifacts = Vec::new();
    let mut has_app_image = false;
    let mut has_other_installable = false;
    for (kind, value) in entries {
        if kind == "target" || kind == "allow_untrusted" || INERT.contains(&kind.as_str()) {
            continue;
        }
        if EXECUTION.contains(&kind.as_str()) {
            return Err(PlanError::InstallerScript(kind));
        }
        let Some(plan_kind) = plan_kind(&kind) else {
            unsupported.push(kind);
            continue;
        };
        if !kind_allowed(&kind, system) {
            unsupported.push(kind);
            continue;
        }
        let Some(items) = value.as_array() else {
            return Err(PlanError::Malformed(format!(
                "artifact {kind} is not a list"
            )));
        };
        for (source, rename) in item_pairs(items)? {
            let (source, target) =
                normalize_item(&kind, &source, rename.as_deref()).map_err(PlanError::Malformed)?;
            if kind == "app_image" {
                has_app_image = true;
            } else {
                has_other_installable = true;
            }
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
        return Err(PlanError::UnsupportedKinds(unsupported));
    }
    Ok(CollectedArtifacts {
        artifacts,
        has_app_image,
        has_other_installable,
        naked,
    })
}

/// Fail-closed AppImage-as-binary normalization on Linux. A direct
/// `.AppImage` download whose only installable is exactly one binary
/// with a matching source is promoted to the appimage plan (target
/// preserved verbatim). Any other binary `.AppImage` source — including
/// inside archives, mixed artifact sets, anchored sources, and mismatched
/// lone sources on a direct URL — refuses with a bounded error.
fn normalize_appimage_binaries(
    artifacts: &mut [PlanArtifact],
    direct_appimage_url: bool,
) -> Result<bool, PlanError> {
    if artifacts.iter().any(|a| a.kind == "appimage") {
        // Existing explicit app_image plan; nothing to normalize.
        return Ok(false);
    }
    // Any binary `.AppImage` source refuses when the download is not a
    // direct `.AppImage` URL — including `$APPDIR`-anchored ones. Only
    // the direct promotion predicate requires an unanchored source.
    let any_appimage_source = artifacts
        .iter()
        .any(|a| a.kind == "binary" && a.source.ends_with(".AppImage"));
    if !direct_appimage_url {
        if any_appimage_source {
            return Err(PlanError::UnsupportedContainer(
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
        return Err(PlanError::UnsupportedKinds(vec![
            "app_image mixed with other artifacts".to_string(),
        ]));
    }
    Err(PlanError::UnsupportedContainer(
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
    match kind {
        "app" => {
            safe_source(source, false)?;
            let bundle = rename.map_or_else(|| basename(source), ToString::to_string);
            if !bundle.ends_with(".app") {
                return Err(format!("app target {bundle:?} does not end in .app"));
            }
            safe_target_name(&bundle)?;
            Ok((source.to_string(), Some(bundle)))
        }
        "binary" => {
            safe_source(source, true)?;
            let target = rename.map_or_else(|| basename(source), ToString::to_string);
            safe_target_name(&target)?;
            Ok((source.to_string(), Some(target)))
        }
        "pkg" => {
            safe_source(source, false)?;
            if let Some(rename) = rename {
                return Err(format!("pkg rename is unsupported: {rename}"));
            }
            Ok((source.to_string(), None))
        }
        "app_image" => {
            safe_source(source, false)?;
            // The target is the basename of the metadata name without
            // the `.AppImage` suffix; renames stay single-segment names.
            let named = basename(rename.unwrap_or(source));
            let target = named.strip_suffix(".AppImage").unwrap_or(&named);
            safe_target_name(target)?;
            Ok((source.to_string(), Some(target.to_string())))
        }
        // Completions follow the pinned Homebrew rename rules: the
        // declared (or default) name is validated as-is first, then the
        // shell-specific normalization is applied and re-validated.
        "bash_completion" | "zsh_completion" | "fish_completion" => {
            safe_source(source, false)?;
            let original = rename.map_or_else(|| basename(source), ToString::to_string);
            safe_target_name(&original)?;
            let stripped = strip_final_extension(&original).to_string();
            let target = match kind {
                "zsh_completion" if !original.starts_with('_') => format!("_{stripped}"),
                "fish_completion" if !original.ends_with(".fish") => {
                    format!("{stripped}.fish")
                }
                "bash_completion" => stripped,
                _ => original,
            };
            safe_target_name(&target)?;
            Ok((source.to_string(), Some(target)))
        }
        // manpage sources are relative payload paths; the whole filename
        // is kept as the target.
        _ => {
            safe_source(source, false)?;
            let target = rename.map_or_else(|| basename(source), ToString::to_string);
            safe_target_name(&target)?;
            Ok((source.to_string(), Some(target)))
        }
    }
}

#[cfg(test)]
mod tests;
