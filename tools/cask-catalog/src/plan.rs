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
    url::Url::parse(url)
        .map(|parsed| parsed.path().ends_with(".AppImage"))
        .unwrap_or(false)
}

/// Build the plan for one effective record on one target system.
///
/// Every installable artifact in the record must be honored; one bad stanza
/// rejects the whole record with a bounded error. URL and checksum presence
/// are checked by the caller (classify) before this runs. `minMacos` is
/// owned by classify (the single strict parser of `depends_on macos`); the
/// plan is built with it null and classify fills it in.
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
    let raw_entries: Vec<&Value> = eff
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // Every artifact entry must be an object; malformed pieces never
    // disappear silently.
    let mut entries: Vec<(String, &Value)> = Vec::new();
    for (index, entry) in raw_entries.iter().enumerate() {
        let Some(map) = entry.as_object() else {
            return Err(PlanError::Malformed(format!(
                "artifact entry {index} is not an object: {entry}"
            )));
        };
        for (key, value) in map {
            entries.push((key.clone(), value));
        }
    }

    let mut unsupported: Vec<String> = Vec::new();
    let mut artifacts: Vec<PlanArtifact> = Vec::new();
    let mut has_app_image = false;
    let mut has_other_installable = false;

    // Container handling: `naked` means the download is a bare file
    // (raw-binary); `nested` selects a path inside a wrapping archive the
    // builders do not extract, so the record is excluded, not guessed.
    // Only the clearly inert `allow_untrusted` flag is ignored here.
    let container = eff.get("container").filter(|v| !v.is_null());
    let mut naked = false;
    if let Some(container) = container {
        let kind = container.get("type").and_then(Value::as_str);
        let inert = container
            .as_object()
            .map(|m| m.keys().all(|k| k == "type" || k == "allow_untrusted"))
            .unwrap_or(false);
        if kind == Some("naked") && inert {
            naked = true;
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

    for (kind, value) in &entries {
        // `target` is the outer destination anchor; `allow_untrusted` is
        // clearly inert for payload-only extraction. No other side key is
        // handled: active options (for example pkg choices) must not
        // silently disappear.
        if kind == "target" || kind == "allow_untrusted" || INERT.contains(&kind.as_str()) {
            continue;
        }
        if EXECUTION.contains(&kind.as_str()) {
            return Err(PlanError::InstallerScript(kind.clone()));
        }
        let Some(plan_kind) = plan_kind(kind) else {
            unsupported.push(kind.clone());
            continue;
        };
        if !kind_allowed(kind, system) {
            unsupported.push(kind.clone());
            continue;
        }
        let Some(items) = value.as_array() else {
            return Err(PlanError::Malformed(format!(
                "artifact {kind} is not a list"
            )));
        };
        for (source, rename) in item_pairs(items)? {
            let (source, target) =
                normalize_item(kind, &source, rename.as_deref()).map_err(PlanError::Malformed)?;
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
fn normalize_item(
    kind: &str,
    source: &str,
    rename: Option<&str>,
) -> Result<(String, Option<String>), String> {
    match kind {
        "app" => {
            safe_source(source, false)?;
            let bundle = rename
                .map(ToString::to_string)
                .unwrap_or_else(|| basename(source));
            if !bundle.ends_with(".app") {
                return Err(format!("app target {bundle:?} does not end in .app"));
            }
            safe_target_name(&bundle)?;
            Ok((source.to_string(), Some(bundle)))
        }
        "binary" => {
            safe_source(source, true)?;
            let target = rename
                .map(ToString::to_string)
                .unwrap_or_else(|| basename(source));
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
            let original = rename
                .map(ToString::to_string)
                .unwrap_or_else(|| basename(source));
            safe_target_name(&original)?;
            let stripped = strip_final_extension(&original).to_string();
            let target = match kind {
                "zsh_completion" if !original.starts_with('_') => format!("_{stripped}"),
                "fish_completion" if !original.ends_with(".fish") => {
                    format!("{stripped}.fish")
                }
                "bash_completion" => stripped,
                _ => original.clone(),
            };
            safe_target_name(&target)?;
            Ok((source.to_string(), Some(target)))
        }
        // manpage sources are relative payload paths; the whole filename
        // is kept as the target.
        _ => {
            safe_source(source, false)?;
            let target = rename
                .map(ToString::to_string)
                .unwrap_or_else(|| basename(source));
            safe_target_name(&target)?;
            Ok((source.to_string(), Some(target)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eff(artifacts: &Value) -> Value {
        json!({"url": "https://v/x.zip", "sha256": "a".repeat(64), "artifacts": artifacts})
    }

    #[test]
    fn app_plus_binary_preserves_rename_from_metadata() {
        let plan = build_plan(
            &eff(&json!([
                {"app": ["Cursor.app"], "target": "/Applications/Cursor.app"},
                {"binary": ["$APPDIR/Cursor.app/Contents/Resources/app/bin/code", {"target": "cursor"}],
                 "target": "$HOMEBREW_PREFIX/bin/cursor"}
            ])),
            "aarch64-darwin",
        )
        .expect("plan builds");
        assert_eq!(
            plan.artifacts,
            vec![
                PlanArtifact {
                    kind: "app",
                    source: "Cursor.app".into(),
                    target: Some("Cursor.app".into())
                },
                PlanArtifact {
                    kind: "binary",
                    source: "$APPDIR/Cursor.app/Contents/Resources/app/bin/code".into(),
                    target: Some("cursor".into())
                },
            ]
        );
        assert_eq!(plan.archive.kind, "auto");
    }

    #[test]
    fn plain_binary_target_comes_from_source_basename() {
        let plan = build_plan(&eff(&json!([{"binary": ["op"]}])), "x86_64-linux").expect("plan");
        assert_eq!(plan.artifacts[0].target.as_deref(), Some("op"));
    }

    #[test]
    fn appimage_target_strips_suffix() {
        let plan = build_plan(
            &eff(&json!([{"app_image": ["koreader-v1-aarch64.AppImage", {"target": "KOReader.AppImage"}]}])),
            "x86_64-linux",
        )
        .expect("plan");
        assert_eq!(plan.artifacts[0].kind, "appimage");
        assert_eq!(plan.artifacts[0].target.as_deref(), Some("KOReader"));
        assert_eq!(plan.archive.kind, "appimage");
    }

    #[test]
    fn traversal_and_unknown_anchors_are_malformed() {
        for bad in [
            "../x",
            "/etc/passwd",
            "$PREFIX/x",
            "a/../b",
            "$APPDIR/../escape",
        ] {
            let err = build_plan(&eff(&json!([{"binary": [bad]}])), "x86_64-linux");
            assert!(
                matches!(err, Err(PlanError::Malformed(_))),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn execution_stanzas_and_foreign_kinds_reject_whole_record() {
        let installer = build_plan(
            &eff(&json!([{"app": ["A.app"]}, {"installer": [{"script": "x"}]}])),
            "aarch64-darwin",
        );
        assert!(matches!(installer, Err(PlanError::InstallerScript(_))));
        let font = build_plan(
            &eff(&json!([{"app": ["A.app"]}, {"font": ["F.ttf"]}])),
            "aarch64-darwin",
        );
        assert!(matches!(font, Err(PlanError::UnsupportedKinds(k)) if k == ["font"]));
        let empty = build_plan(&eff(&json!([{"zap": [{"trash": "~"}]}])), "aarch64-darwin");
        assert_eq!(empty, Err(PlanError::NoInstallableArtifact));
        let app_on_linux = build_plan(&eff(&json!([{"app": ["A.app"]}])), "x86_64-linux");
        assert!(matches!(app_on_linux, Err(PlanError::UnsupportedKinds(k)) if k == ["app"]));
    }

    #[test]
    fn container_naked_is_raw_binary_and_nested_is_unsupported() {
        let naked = build_plan(
            &json!({"url": "https://v/tool.zip", "sha256": "a".repeat(64),
                     "container": {"type": "naked"},
                     "artifacts": [{"binary": ["tool"]}]}),
            "aarch64-darwin",
        )
        .expect("plan");
        assert_eq!(naked.archive.kind, "raw-binary");
        let nested = build_plan(
            &json!({"url": "https://v/wrap.dmg", "sha256": "a".repeat(64),
                     "container": {"nested": "inner/Real.dmg"},
                     "artifacts": [{"app": ["A.app"]}]}),
            "aarch64-darwin",
        );
        assert!(
            matches!(nested, Err(PlanError::UnsupportedContainer(d)) if d.contains("inner/Real.dmg"))
        );
    }

    #[test]
    fn extensionless_binary_only_url_is_auto_unless_naked() {
        // minMacos is owned by classify; build_plan leaves it null.
        let plan = build_plan(
            &json!({"url": "https://v/tool", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["tool"]}], "depends_on": {"macos": {">=": ["12"]}}}),
            "aarch64-darwin",
        )
        .expect("plan");
        assert_eq!(plan.archive.kind, "auto");
        assert_eq!(plan.min_macos, None);
    }

    #[test]
    fn active_item_options_never_disappear_silently() {
        // Installer choices drive `/usr/sbin/installer`; excluded as an
        // execution stanza, never dropped from an eligible plan.
        let choices = build_plan(
            &json!({"url": "https://v/x.pkg", "sha256": "a".repeat(64),
                     "artifacts": [{"pkg": ["A.pkg", {"choices": [{"choiceIdentifier": "c"}]}]}]}),
            "aarch64-darwin",
        );
        assert!(matches!(choices, Err(PlanError::InstallerScript(d)) if d.contains("choices")));
        // Unknown option keys and malformed target values are malformed.
        let unknown = build_plan(
            &json!({"url": "https://v/x", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["tool", {"only_if": true}]}]}),
            "x86_64-linux",
        );
        assert!(matches!(unknown, Err(PlanError::Malformed(d)) if d.contains("only_if")));
        let bad_target = build_plan(
            &json!({"url": "https://v/x", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["tool", {"target": 7}]}]}),
            "x86_64-linux",
        );
        assert!(matches!(bad_target, Err(PlanError::Malformed(d)) if d.contains("target")));
        // A pkg rename cannot be represented (pkg target is null by
        // contract), so it is rejected instead of ignored.
        let pkg_rename = build_plan(
            &json!({"url": "https://v/x.pkg", "sha256": "a".repeat(64),
                     "artifacts": [{"pkg": ["A.pkg", {"target": "B.pkg"}]}]}),
            "aarch64-darwin",
        );
        assert!(matches!(pkg_rename, Err(PlanError::Malformed(d)) if d.contains("pkg rename")));
    }

    #[test]
    fn naked_needs_exactly_one_binary_and_targets_must_not_collide() {
        let two_binaries = build_plan(
            &json!({"url": "https://v/tool", "sha256": "a".repeat(64),
                     "container": {"type": "naked"},
                     "artifacts": [{"binary": ["a/tool", "b/tool"]}]}),
            "x86_64-linux",
        );
        assert!(
            matches!(two_binaries, Err(PlanError::UnsupportedContainer(d)) if d.contains("naked"))
        );
        let collision = build_plan(
            &json!({"url": "https://v/x.zip", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["dir/tool", {"target": "t"}]},
                                    {"binary": ["other/tool", {"target": "t"}]}]}),
            "x86_64-linux",
        );
        assert!(matches!(collision, Err(PlanError::Malformed(d)) if d.contains("duplicate")));
    }

    #[test]
    fn malformed_and_nested_targets_reject_the_record() {
        // A non-object artifact piece never disappears silently.
        let partial = build_plan(
            &json!({"url": "https://v/x", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["tool"]}, 42]}),
            "x86_64-linux",
        );
        assert!(matches!(partial, Err(PlanError::Malformed(d)) if d.contains("artifact entry 1")));
        // A slash inside a link target is not one component.
        let nested = build_plan(
            &json!({"url": "https://v/x", "sha256": "a".repeat(64),
                     "artifacts": [{"binary": ["tool", {"target": "bin/tool"}]}]}),
            "x86_64-linux",
        );
        assert!(matches!(nested, Err(PlanError::Malformed(d)) if d.contains("bin/tool")));
    }

    #[test]
    fn appimage_target_derives_from_basename_for_nested_sources() {
        // Renames are upstream metadata; a nested AppImage source is safe
        // and its target is the basename without the suffix (contract 3).
        let plan = build_plan(
            &eff(&json!([{"app_image": ["linux/koreader-v2026.AppImage"]}])),
            "x86_64-linux",
        )
        .expect("nested AppImage source must plan with a basename target");
        assert_eq!(plan.artifacts[0].kind, "appimage");
        assert_eq!(plan.artifacts[0].source, "linux/koreader-v2026.AppImage");
        assert_eq!(plan.artifacts[0].target.as_deref(), Some("koreader-v2026"));
    }

    #[test]
    fn control_characters_in_link_names_and_sources_are_rejected() {
        for bad in ["to\u{1}ol", "tool\t", "tool\u{7f}", "tool\u{1b}[31m"] {
            let err = build_plan(
                &eff(&json!([{"binary": ["tool", {"target": bad}]}])),
                "x86_64-linux",
            );
            assert!(
                matches!(err, Err(PlanError::Malformed(_))),
                "{bad:?}: {err:?}"
            );
        }
        let src = build_plan(&eff(&json!([{"binary": ["a\u{b}tool"]}])), "x86_64-linux");
        assert!(matches!(src, Err(PlanError::Malformed(_))));
    }

    fn linux_eff(url: &str, artifacts: &Value) -> Value {
        json!({"url": url, "sha256": "a".repeat(64), "artifacts": artifacts})
    }

    #[test]
    fn direct_appimage_url_promotes_one_matching_binary() {
        // Target with an explicit rename and the default basename target
        // are both preserved verbatim; no suffix-stripping normalization.
        let cases = [
            (
                "https://v/t-2.2.AppImage",
                json!([{"binary": ["t-2.2.AppImage", {"target": "tool.AppImage"}]}]),
                Some("tool.AppImage"),
            ),
            (
                "https://v/t-1.0.AppImage?token=x",
                json!([{"binary": ["t-1.0.AppImage"]}]),
                Some("t-1.0.AppImage"),
            ),
            // Query string does not hide a direct .AppImage path.
            (
                "https://v/dl/t.AppImage?token=x",
                json!([{"binary": ["t.AppImage"]}]),
                Some("t.AppImage"),
            ),
            // A naked container plus a direct AppImage binary is still
            // the direct download case, not a container error.
            (
                "https://v/t.AppImage",
                json!([{"binary": ["t.AppImage"]}]),
                Some("t.AppImage"),
            ),
        ];
        for (url, artifacts, expected) in cases {
            let mut eff = linux_eff(url, &artifacts);
            if url == "https://v/t.AppImage" {
                eff["container"] = json!({"type": "naked"});
            }
            let plan = build_plan(&eff, "x86_64-linux").unwrap_or_else(|e| panic!("{url}: {e:?}"));
            assert_eq!(plan.archive.kind, "appimage", "{url}");
            assert_eq!(plan.artifacts.len(), 1, "{url}");
            assert_eq!(plan.artifacts[0].kind, "appimage", "{url}");
            assert_eq!(plan.artifacts[0].target.as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn appimage_as_binary_refuses_closed() {
        // (url, artifacts, expected error needle)
        let container_cases = [
            // AppImage binary inside a ZIP, lone or mixed with a manpage
            // or another binary, never stays eligible for archive:auto.
            (
                "https://v/x.zip",
                json!([{"binary": ["t.AppImage"]}]),
                "AppImage inside archive unsupported",
            ),
            (
                "https://v/x.zip",
                json!([{"binary": ["t.AppImage"]}, {"manpage": ["t.1"]}]),
                "AppImage inside archive unsupported",
            ),
            (
                "https://v/x.zip",
                json!([{"binary": ["t.AppImage"]}, {"binary": ["u"]}]),
                "AppImage inside archive unsupported",
            ),
            // Direct .AppImage URL with a wrong or anchored source.
            (
                "https://v/t.AppImage",
                json!([{"binary": ["tool"]}]),
                "direct .AppImage URL",
            ),
            (
                "https://v/t.AppImage",
                json!([{"binary": ["$APPDIR/T.app/Contents/t.AppImage"]}]),
                "direct .AppImage URL",
            ),
        ];
        for (url, artifacts, needle) in container_cases {
            let err = build_plan(&linux_eff(url, &artifacts), "x86_64-linux")
                .expect_err(&format!("{url} must refuse"));
            assert!(
                matches!(&err, PlanError::UnsupportedContainer(d) if d.contains(needle)),
                "{url}: {err:?}"
            );
        }
        // Direct .AppImage URL with mixed artifact sets refuses as mixed.
        let mixed_cases = [
            (
                "https://v/t.AppImage",
                json!([{"binary": ["t.AppImage"]}, {"binary": ["u.AppImage"]}]),
            ),
            (
                "https://v/t.AppImage",
                json!([{"binary": ["t.AppImage"]}, {"manpage": ["t.1"]}]),
            ),
        ];
        for (url, artifacts) in mixed_cases {
            let err = build_plan(&linux_eff(url, &artifacts), "x86_64-linux")
                .expect_err(&format!("{url} must refuse"));
            assert!(
                matches!(&err, PlanError::UnsupportedKinds(k) if k.len() == 1),
                "{url}: {err:?}"
            );
        }
    }

    #[test]
    fn appimage_promotion_never_triggers_on_lookalikes() {
        // Query-only .AppImage mention: the path is what counts, so an
        // ordinary binary stays an ordinary auto-archive binary.
        let plan = build_plan(
            &linux_eff(
                "https://v/dl?file=t.AppImage",
                &json!([{"binary": ["tool"]}]),
            ),
            "x86_64-linux",
        )
        .expect("ordinary binary stays ordinary");
        assert_eq!(plan.artifacts[0].kind, "binary");
        assert_eq!(plan.artifacts[0].target.as_deref(), Some("tool"));
        assert_eq!(plan.archive.kind, "auto");
        // Non-Linux: same records never gain appimage handling.
        for url in ["https://v/t.AppImage", "https://v/x.zip"] {
            let plan = build_plan(
                &linux_eff(url, &json!([{"binary": ["t.AppImage"]}])),
                "aarch64-darwin",
            )
            .unwrap_or_else(|e| panic!("{url}: {e:?}"));
            assert_eq!(plan.artifacts[0].kind, "binary", "{url}");
            assert_eq!(
                plan.artifacts[0].target.as_deref(),
                Some("t.AppImage"),
                "{url}"
            );
            assert_eq!(plan.archive.kind, "auto", "{url}");
        }
    }

    #[test]
    fn completion_targets_are_normalized_per_shell() {
        // (kind, source, rename, expected final target)
        let cases = [
            // Defaults come from the source basename.
            (
                "bash_completion",
                "share/goreleaser.bash",
                None,
                "goreleaser",
            ),
            (
                "zsh_completion",
                "share/goreleaser.zsh",
                None,
                "_goreleaser",
            ),
            (
                "fish_completion",
                "share/goreleaser.fish",
                None,
                "goreleaser.fish",
            ),
            // Already-normalized names stay verbatim.
            ("zsh_completion", "s/x", Some("_goreleaser"), "_goreleaser"),
            ("fish_completion", "s/x", Some("g.fish"), "g.fish"),
            ("bash_completion", "s/x", Some("noext"), "noext"),
            // Declared renames apply the same rules.
            ("bash_completion", "s/x.bash", Some("tool.bash"), "tool"),
            ("zsh_completion", "s/x", Some("tool.zsh"), "_tool"),
            ("fish_completion", "s/x", Some("tool"), "tool.fish"),
            // Dotfiles and multi-dot names strip only the final extension.
            ("bash_completion", "s/x", Some(".bashrc"), ".bashrc"),
            ("bash_completion", "s/x", Some(".hidden.ext"), ".hidden"),
            ("fish_completion", "s/x", Some(".hidden"), ".hidden.fish"),
            // All-leading-dot names have no extension (Ruby File.extname
            // skips leading dots until a non-dot occurs).
            ("bash_completion", "s/x", Some("..bashrc"), "..bashrc"),
            ("bash_completion", "s/x", Some("..."), "..."),
            ("bash_completion", "s/x", Some("..foo.bar"), "..foo"),
            // A trailing dot still strips to the stem.
            ("bash_completion", "s/x", Some("foo."), "foo"),
            ("bash_completion", "s/x", Some("foo.."), "foo."),
            // Already shell-shaped names stay verbatim.
            ("zsh_completion", "s/x", Some("_foo.zsh"), "_foo.zsh"),
            ("fish_completion", "s/x", Some(".fish"), ".fish"),
        ];
        for (kind, source, rename, expected) in cases {
            let items = match rename {
                Some(r) => json!([source, {"target": r}]),
                None => json!([source]),
            };
            let plan = build_plan(&eff(&json!([{kind: items}])), "x86_64-linux")
                .unwrap_or_else(|e| panic!("{kind} {source}: {e:?}"));
            assert_eq!(plan.artifacts[0].target.as_deref(), Some(expected));
            assert_eq!(plan.artifacts[0].source, source);
        }
    }

    #[test]
    fn completion_normalization_never_hides_unsafe_targets() {
        // The ORIGINAL declared name is validated before any rename.
        for bad in ["../x", "a/b", "", "to\u{1}ol"] {
            let err = build_plan(
                &eff(&json!([{"zsh_completion": ["s/x", {"target": bad}]}])),
                "x86_64-linux",
            );
            assert!(matches!(err, Err(PlanError::Malformed(_))), "{bad:?}");
            let err = build_plan(
                &eff(&json!([{"bash_completion": ["s/x", {"target": bad}]}])),
                "x86_64-linux",
            );
            assert!(matches!(err, Err(PlanError::Malformed(_))), "{bad:?}");
        }
    }

    #[test]
    fn post_normalization_collisions_still_refuse() {
        // goreleaser.bash and goreleaser (bash) land in the same file.
        let err = build_plan(
            &eff(&json!([
                {"bash_completion": ["share/goreleaser.bash"]},
                {"bash_completion": ["share/goreleaser", {"target": "goreleaser.txt"}]},
            ])),
            "x86_64-linux",
        );
        assert!(matches!(err, Err(PlanError::Malformed(d)) if d.contains("duplicate")));
        // Same shell, both rename to _tool.
        let err = build_plan(
            &eff(&json!([
                {"zsh_completion": ["a/tool.zsh"]},
                {"zsh_completion": ["b/tool"]},
            ])),
            "x86_64-linux",
        );
        assert!(matches!(err, Err(PlanError::Malformed(d)) if d.contains("duplicate")));
    }

    #[test]
    fn binary_plus_all_completions_coexist() {
        // The real GoReleaser shape: one binary and its three completions.
        let plan = build_plan(
            &eff(&json!([
                {"binary": ["bin/goreleaser"]},
                {"bash_completion": ["completions/goreleaser.bash"]},
                {"zsh_completion": ["completions/goreleaser.zsh"]},
                {"fish_completion": ["completions/goreleaser.fish"]},
            ])),
            "x86_64-linux",
        )
        .expect("distinct namespaces coexist");
        assert_eq!(
            plan.artifacts
                .iter()
                .map(|a| a.target.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("goreleaser"),
                Some("goreleaser"),
                Some("_goreleaser"),
                Some("goreleaser.fish")
            ]
        );
    }

    #[test]
    fn distinct_namespaces_do_not_falsely_collide() {
        // Manpages in distinct sections and same-name artifacts across
        // different destinations are different files.
        let plan = build_plan(
            &eff(&json!([
                {"manpage": ["man/tool.1"]},
                {"manpage": ["man/tool.3"]},
                {"manpage": ["man/tool.8.gz"]},
                {"binary": ["bin/tool"]},
                {"bash_completion": ["completions/tool.bash"]},
            ])),
            "x86_64-linux",
        )
        .expect("distinct destinations do not collide");
        assert_eq!(plan.artifacts.len(), 5);
        // But the same section twice is a real collision.
        let err = build_plan(
            &eff(&json!([
                {"manpage": ["man/tool.1"]},
                {"manpage": ["other/tool.1.gz", {"target": "tool.1"}]},
            ])),
            "x86_64-linux",
        );
        assert!(matches!(err, Err(PlanError::Malformed(d)) if d.contains("duplicate")));
    }

    #[test]
    fn archive_with_anchored_appimage_binary_refuses() {
        // The AppImage fix consistency correction: a $APPDIR-anchored
        // .AppImage binary inside an archive still refuses closed; only
        // the direct promotion path requires an unanchored source.
        let err = build_plan(
            &linux_eff(
                "https://v/x.zip",
                &json!([{"binary": ["$APPDIR/T.app/Contents/t.AppImage"]}]),
            ),
            "x86_64-linux",
        );
        assert!(
            matches!(err, Err(PlanError::UnsupportedContainer(ref d)) if d.contains("AppImage inside archive")),
            "{err:?}"
        );
    }

    #[test]
    fn multiple_pkg_or_appimage_artifacts_are_unsupported() {
        let two_pkgs = build_plan(
            &json!({"url": "https://v/x.dmg", "sha256": "a".repeat(64),
                     "artifacts": [{"pkg": ["A.pkg"]}, {"pkg": ["B.pkg"]}]}),
            "aarch64-darwin",
        );
        assert!(matches!(two_pkgs, Err(PlanError::UnsupportedKinds(k)) if k.len() == 1));
        let two_images = build_plan(
            &json!({"url": "https://v/x.AppImage", "sha256": "a".repeat(64),
                     "artifacts": [{"app_image": ["A.AppImage"]}, {"app_image": ["B.AppImage"]}]}),
            "x86_64-linux",
        );
        assert!(matches!(two_images, Err(PlanError::UnsupportedKinds(k)) if k.len() == 1));
    }
}
