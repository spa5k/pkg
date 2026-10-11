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
    assert!(matches!(two_binaries, Err(PlanError::UnsupportedContainer(d)) if d.contains("naked")));
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
