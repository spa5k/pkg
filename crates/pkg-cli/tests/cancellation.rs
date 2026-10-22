#![expect(missing_docs, reason = "integration test crates have no public API")]
#![cfg(unix)]
#![allow(
    clippy::expect_used,
    reason = "tests fail directly when isolated fixtures cannot be prepared"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Output};

fn run(args: &[&str], failure: &str) -> (Output, bool) {
    let home = tempfile::tempdir().expect("tempdir");
    let nix = home.path().join("nix");
    let marker = home.path().join("mutation");
    std::fs::write(&nix, r#"#!/bin/sh
case "$*" in
*--version*) echo 'nix (Nix) 2.35.2';;
*'config show system'*) echo 'x86_64-linux';;
*'flake metadata'*) echo '{"url":"github:fixture/packages/1111111111111111111111111111111111111111"}';;
*'search'*) echo '{"legacyPackages.x86_64-linux.probe":{"pname":"probe","version":"1","description":"fixture"}}';;
*'profile list'*)
  if [ "$PKG_READ_FAILURE" = info ]; then exit 130; fi
  echo '{"version":3,"elements":{}}';;
*'profile history'*)
  case "$PKG_READ_FAILURE" in
    before) exit 130;;
    unavailable) exit 1;;
    after) if [ -e "$PKG_MUTATION_MARKER" ]; then exit 130; fi;;
  esac
  echo 'Version 1 (2026-10-12)';;
*'profile rollback'*|*'profile wipe-history'*) touch "$PKG_MUTATION_MARKER";;
*) echo "unexpected call: $*" >&2; exit 9;;
esac
"#).expect("write runtime");
    std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).expect("chmod runtime");
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!("[runtime]\nnix = '{}'\n", nix.display()),
    )
    .expect("write config");
    let profile = home.path().join("state/nix/profiles/pkg");
    std::fs::create_dir_all(profile.parent().expect("profile parent")).expect("profile directory");
    if failure == "info" {
        std::fs::write(profile, "fixture").expect("existing profile");
    } else {
        // Two generations, so rollback has a generation to go back to.
        for number in [1, 2] {
            std::os::unix::fs::symlink(
                home.path(),
                profile.with_file_name(format!("pkg-{number}-link")),
            )
            .expect("generation link");
        }
        std::os::unix::fs::symlink("pkg-2-link", &profile).expect("active generation");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
        .args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("PKG_MUTATION_MARKER", &marker)
        .env("PKG_READ_FAILURE", failure)
        .output()
        .expect("run pkg");
    (output, marker.exists())
}

#[test]
fn interrupted_history_prevents_rollback_and_prune() {
    for args in [&["rollback"][..], &["prune", "--older-than", "30d"][..]] {
        let (output, mutated) = run(args, "before");
        assert_eq!(output.status.code(), Some(130), "{args:?}: {output:?}");
        assert!(!mutated, "{args:?} mutated after cancellation");
    }
}

#[test]
fn unavailable_history_remains_best_effort() {
    for args in [&["rollback"][..], &["prune", "--older-than", "30d"][..]] {
        let (output, mutated) = run(args, "unavailable");
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(mutated, "ordinary history failure must not stop {args:?}");
    }
}

#[test]
fn interrupted_post_prune_history_preserves_cancellation_status() {
    let (output, mutated) = run(&["prune", "--older-than", "30d"], "after");
    assert!(mutated, "fixture must reach the post-mutation read");
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Prune complete"));
}

#[test]
fn interrupted_info_read_preserves_cancellation_status() {
    let (output, mutated) = run(&["info", "nixpkgs:probe"], "info");
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(!mutated);
    assert!(
        output.stdout.is_empty(),
        "canceled info must not report completion"
    );
}
