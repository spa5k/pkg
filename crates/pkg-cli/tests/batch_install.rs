#![expect(missing_docs, reason = "integration test crates have no public API")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail on broken fixtures"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

#[test]
fn repeated_install_shares_output_checks_without_per_package_processes() {
    repeated_install(100, "success", 0, false);
}

#[test]
fn unavailable_batch_checks_use_exact_native_checks() {
    for mode in ["absent", "failed"] {
        repeated_install(2, mode, 0, true);
    }
}

#[test]
fn malformed_batch_results_and_cancellation_stop_before_mutation() {
    for (mode, code) in [
        ("short", 1),
        ("malformed", 1),
        ("INT", 130),
        ("TERM", 143),
        ("HUP", 129),
    ] {
        repeated_install(2, mode, code, false);
    }
}

/// A source that moved since the install gives new output paths. The
/// installed entry still counts, so no second entry is added.
#[test]
fn moved_source_install_keeps_one_entry() {
    let root = fixture(2);
    let calls = root.path().join("calls");
    let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
        .args(["install", "nixpkgs:probe-000", "nixpkgs:probe-001"])
        .env("PKG_TEST_BATCH_MODE", "moved")
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    for name in ["probe-000", "probe-001"] {
        assert!(
            stdout.contains(&format!("Already installed: {name}\n")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("run `pkg upgrade {name}`")),
            "{stdout}"
        );
    }
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(!calls.contains("profile add"), "{calls}");
}

fn fixture(count: usize) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("state/nix/profiles/pkg");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(&profile, "unchanged profile").unwrap();
    let manifest = root.path().join("manifest.json");
    let entries: serde_json::Map<_, _> = (0..count)
        .map(|index| {
            let name = format!("probe-{index:03}");
            (
                name.clone(),
                serde_json::json!({
                    "active": true,
                    "attrPath": format!("packages.x86_64-linux.{name}"),
                    "originalUrl": "github:example/packages",
                    "url": "github:example/packages/1111111111111111111111111111111111111111",
                    "storePaths": [format!("/nix/store/old-{name}")]
                }),
            )
        })
        .collect();
    std::fs::write(
        &manifest,
        serde_json::json!({"version": 3, "elements": entries}).to_string(),
    )
    .unwrap();
    let outputs = root.path().join("outputs.json");
    let resolved: Vec<_> = (0..count)
        .map(|index| {
            let name = format!("probe-{index:03}");
            serde_json::json!({
                "attribute": format!("packages.x86_64-linux.{name}"),
                "paths": [format!("/nix/store/old-{name}")]
            })
        })
        .collect();
    std::fs::write(&outputs, serde_json::to_vec(&resolved).unwrap()).unwrap();
    let calls = root.path().join("calls");
    let nix = root.path().join("nix");
    std::fs::write(
        &nix,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'config show system'*) echo x86_64-linux;;
  *'profile list'*) cat '{}';;
  *'flake metadata'*) echo '{{"url":"github:example/packages/1111111111111111111111111111111111111111"}}';;
  *'eval --json --expr'*)
    case "$PKG_TEST_BATCH_MODE" in
      absent) echo '[null,null]';;
      failed) exit 8;;
      short) echo '[]';;
      malformed) echo '[{{"attribute":7}}]';;
      INT|TERM|HUP) kill -"$PKG_TEST_BATCH_MODE" "$$";;
      moved) sed 's/old-/new-/g' '{}';;
      *) cat '{}';;
    esac;;
  *'search '*|*'eval --json '*' -- github:example/packages#'*)
    name=
    for argument in "$@"; do
      case "$argument" in
        github:example/packages#*) name=${{argument#*#}};;
      esac
    done
    case "$*" in
      *'search '*) printf '{{"packages.x86_64-linux.%s":{{"pname":"%s","version":"1","description":"Probe"}}}}\n' "$name" "$name";;
      *) printf '["/nix/store/old-%s"]\n' "$name";;
    esac;;
  *) echo 'unexpected metadata probe' >&2; exit 9;;
esac
"#,
            calls.display(),
            manifest.display(),
            outputs.display(),
            outputs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("config/pkg/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!(
            "[sources]\nnixpkgs = \"github:example/packages\"\n[runtime]\nnix = {}\n",
            serde_json::to_string(&nix).unwrap()
        ),
    )
    .unwrap();
    root
}

fn repeated_install(count: usize, mode: &str, code: i32, fallback: bool) {
    let root = fixture(count);
    let profile = root.path().join("state/nix/profiles/pkg");
    let calls = root.path().join("calls");
    let packages: Vec<_> = (0..count)
        .map(|index| format!("nixpkgs:probe-{index:03}"))
        .collect();
    let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
        .arg("install")
        .args(&packages)
        .env("PKG_TEST_BATCH_MODE", mode)
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(code), "{mode}: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    if code == 0 {
        assert_eq!(stdout.lines().count(), count, "{stdout}");
        for index in 0..count {
            assert!(
                stdout.contains(&format!("Already installed: probe-{index:03}\n")),
                "{stdout}"
            );
        }
    } else {
        assert!(stdout.is_empty(), "{mode}: {stdout}");
    }
    assert_eq!(
        std::fs::read_to_string(&profile).unwrap(),
        "unchanged profile"
    );
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(!calls.contains("profile add"), "{calls}");
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains(" eval --json --expr"))
            .count(),
        1,
        "{calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains(" search "))
            .count(),
        if fallback { count } else { 0 },
        "{mode}: {calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("--version"))
            .count(),
        usize::from(mode == "failed"),
        "only a failed native child probes the configured runtime: {calls}"
    );
}
