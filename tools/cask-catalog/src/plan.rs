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
    if source.contains('\0') || source.contains('\n') || source.contains('\r') {
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
        || name.contains('\0')
        || name.contains('\n')
        || name.contains('\r')
    {
        return Err(format!("unsafe link name {name:?}"));
    }
    Ok(())
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
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
    // Two artifacts claiming the same link or bundle name would collide
    // in the output prefix; detectable here, so it never reaches a builder.
    let mut claimed: Vec<&str> = Vec::new();
    for artifact in &artifacts {
        if let Some(target) = artifact.target.as_deref() {
            if claimed.contains(&target) {
                return Err(PlanError::Malformed(format!(
                    "duplicate artifact target {target:?}"
                )));
            }
            claimed.push(target);
        }
    }

    let archive_kind = if has_app_image {
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
    })
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
            let named = rename.unwrap_or(source);
            let target = named.strip_suffix(".AppImage").unwrap_or(named);
            safe_target_name(target)?;
            Ok((source.to_string(), Some(target.to_string())))
        }
        // manpage and completion sources are relative payload paths.
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
