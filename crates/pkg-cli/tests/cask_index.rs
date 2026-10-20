#![expect(missing_docs, reason = "integration test crates have no public API")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

mod support;
use support::formula_index_fixture;

struct CaskFixture {
    bin: TempDir,
    home: TempDir,
    nix: PathBuf,
}

impl CaskFixture {
    fn new() -> Self {
        let bin = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("tempdir");
        let nix = bin.path().join("nix");
        let formula = formula_index_fixture(bin.path()).display().to_string();
        let script = format!(
                "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'config show system'*) echo 'x86_64-linux';;\n  *'flake metadata'*) echo '{{\"url\":\"github:spa5k/pkg/240304?dir=nix/casks\",\"locked\":{{\"type\":\"github\",\"owner\":\"spa5k\",\"repo\":\"pkg\",\"rev\":\"2403040105060708090a0b0c0d0e0f1011121314\"}}}}';;\n  *'#formulaIndex'*) cat '{formula}';;\n  *'eval --json'*) cat <<'JSON'\n__CATALOG__\nJSON\n;;\n  *'profile list'*) echo '{{\"version\":3,\"elements\":{{}}}}';;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n",
            )
            .replace("__CATALOG__", CATALOG);
        std::fs::write(&nix, script).expect("write fake nix");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&nix, permissions).expect("chmod");
        }
        Self { bin, home, nix }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
            .args(args)
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.bin.path().display()),
            )
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .output()
            .expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn set_nix_script(&self, script: &str) {
        std::fs::write(&self.nix, script).expect("replace fake nix");
    }

    fn profile(&self) -> PathBuf {
        self.home.path().join("state/nix/profiles/pkg")
    }
}

const CATALOG: &str = r#"{"schema":"pkg-cask-catalog/4",
  "generator":{"name":"cask-catalog","version":"0.1.0"},
  "inputs":{"homebrew/cask":{"url":"github:BatteredBunny/brew-api/245947c0","revision":"245947c0",
    "sha256":"9f3b1c47ae5d2801c6a1b74f0e39c4d2a8f60c15d7e3b28a4c05f6e9d1a7b3c2","license":"Homebrew license"}},
  "targets":["aarch64-darwin","x86_64-linux"],"macosBaseline":"15.7.7",
  "systems":{"aarch64-darwin":{"entries":{}},"x86_64-linux":{"entries":{
    "homebrew/cask/1password-cli":{"source":"homebrew/cask","token":"1password-cli","name":"1Password CLI","description":"The 1Password command-line tool","version":"2.39.0","homepage":"https://developer.1password.com/docs/cli/","status":"eligible","kind":"binary","reason":null,"detail":null},
    "homebrew/cask/iterm2":{"source":"homebrew/cask","token":"iterm2","name":"iTerm2","description":null,"version":"3.6.11","homepage":"https://iterm2.com/","status":"excluded","kind":null,"reason":"unsupported-platform","detail":null}}}}}"#;

#[test]
fn upgrade_gates_the_installed_cask_source_after_configuration_changes() {
    let fixture = CaskFixture::new();
    let config = fixture.home.path().join("config/pkg");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[sources]\ncasks = 'github:fixture/current-casks'\n",
    )
    .unwrap();
    let profile = fixture.profile();
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("pkg-1-link", &profile).unwrap();
    let installed = serde_json::json!({"version": 3, "elements": {"opaque": {
        "active": true,
        "attrPath": "packages.x86_64-linux.homebrew/cask/iterm2",
        "originalUrl": "github:fixture/installed-casks",
        "url": "github:fixture/installed-casks/1111111111111111111111111111111111111111",
        "storePaths": []
    }}});
    let calls = fixture.home.path().join("calls");
    let script = std::fs::read_to_string(&fixture.nix)
        .unwrap()
        .replace(
            "echo '{\"version\":3,\"elements\":{}}'",
            &format!("echo '{installed}'"),
        )
        .replace(
            "case \"$*\" in",
            &format!(
                "printf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in",
                calls.display()
            ),
        );
    fixture.set_nix_script(&script);
    let (code, stdout, stderr) = fixture.run(&["upgrade", "opaque", "--entry"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("unsupported-platform"), "{stderr}");
    let calls = std::fs::read_to_string(calls).unwrap();
    assert!(
        calls.lines().any(|line| line.contains("flake metadata")
            && line.contains("github:fixture/installed-casks")),
        "{calls}"
    );
    assert!(!calls.contains("github:fixture/current-casks"), "{calls}");
    assert!(!calls.contains("profile upgrade"), "{calls}");
}

#[test]
fn upgrade_checks_each_formula_branch_before_mutating_the_profile() {
    let fixture = CaskFixture::new();
    let available = formula_index_fixture(fixture.bin.path());
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&available).unwrap()).unwrap();
    let entry = &mut index["systems"]["x86_64-linux"]["entries"]["homebrew/core/awscli"];
    entry["status"] = serde_json::json!("excluded");
    entry["kind"] = serde_json::Value::Null;
    entry["reason"] = serde_json::json!("disabled");
    let excluded = fixture.bin.path().join("excluded-formula-index.json");
    std::fs::write(&excluded, index.to_string()).unwrap();
    let installed = serde_json::json!({"version": 3, "elements": {
        "opaque-a": {"active": true, "attrPath": "packages.x86_64-linux.homebrew/core/awscli",
            "originalUrl": "github:fixture/formulae/stable", "url": "github:fixture/formulae/1111111111111111111111111111111111111111", "storePaths": []},
        "opaque-b": {"active": true, "attrPath": "packages.x86_64-linux.homebrew/core/awscli",
            "originalUrl": "github:fixture/formulae/preview", "url": "github:fixture/formulae/2222222222222222222222222222222222222222", "storePaths": []}
    }});
    let profile = fixture.profile();
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("pkg-1-link", &profile).unwrap();
    let calls = fixture.home.path().join("calls");
    fixture.set_nix_script(&format!(
        r"#!/usr/bin/python3
import json,sys
args=sys.argv[1:]; text=' '.join(args)
with open({calls},'a') as log: log.write(text+'\n')
if '--version' in args: print('nix (Nix) 2.35.2')
elif 'config show system' in text: print('x86_64-linux')
elif 'profile list' in text: print({installed})
elif 'flake metadata' in text:
 revision='2'*40 if '/preview' in text else '1'*40
 print(json.dumps({{'url':'github:fixture/formulae/'+revision}}))
elif '#formulaIndex' in text:
 print(open({excluded} if '2'*40 in text else {available}).read())
elif 'profile upgrade' in text: pass
else: sys.exit(9)
",
        calls = serde_json::to_string(&calls).unwrap(),
        installed = serde_json::to_string(&installed.to_string()).unwrap(),
        excluded = serde_json::to_string(&excluded).unwrap(),
        available = serde_json::to_string(&available).unwrap(),
    ));
    let (code, stdout, stderr) = fixture.run(&["upgrade", "opaque-a", "opaque-b", "--entry"]);
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(stderr.contains("disabled"), "{stderr}");
    let calls = std::fs::read_to_string(calls).unwrap();
    for branch in ["stable", "preview"] {
        assert!(
            calls.lines().any(|line| line.contains("flake metadata")
                && line.contains(&format!("github:fixture/formulae/{branch}"))),
            "{calls}"
        );
    }
    assert!(!calls.contains("profile upgrade"), "{calls}");
    assert_eq!(
        std::fs::read_link(profile).unwrap(),
        Path::new("pkg-1-link")
    );
}

#[test]
fn cask_info_reports_recorded_exclusion() {
    let fixture = CaskFixture::new();
    let (code, stdout, _) = fixture.run(&["info", "cask:iterm2"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Cannot install"), "{stdout}");
    assert!(stdout.contains("unsupported-platform"), "{stdout}");
    let (code, stdout, _) = fixture.run(&["info", "cask:iterm2", "--verbose"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("URL: https://iterm2.com/"), "{stdout}");
}

#[test]
fn cask_install_gates_excluded_and_unknown_tokens() {
    let fixture = CaskFixture::new();
    let (code, _, stderr) = fixture.run(&["install", "cask:iterm2"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unsupported-platform"), "{stderr}");
    let (code, _, stderr) = fixture.run(&["install", "cask:missing-token"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("missing-token"), "{stderr}");
    let (code, _, stderr) = fixture.run(&["install", "cask:homebrew/cask/missing-token"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("not in the generated catalog"), "{stderr}");
}

#[test]
fn cask_preparation_retains_healthy_source_after_native_failure() {
    let fixture = CaskFixture::new();
    let (code, _, stderr) = fixture.run(&["update", "--scheduled"]);
    assert_eq!(code, 1, "native fixture has no package outputs: {stderr}");
    assert!(stderr.contains("Ready: cask"), "{stderr}");
}

#[test]
fn cask_search_respects_eligibility_and_unavailable_option() {
    let fixture = CaskFixture::new();
    let (update_code, _, update_stderr) = fixture.run(&["update", "--scheduled"]);
    assert_eq!(update_code, 1, "{update_stderr}");
    let (code, stdout, _) =
        fixture.run(&["search", "iterm|1password", "--regex", "--source", "cask"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("cask:1password-cli"), "{stdout}");
    assert!(!stdout.contains("iterm2"), "{stdout}");
    assert!(stdout.contains("2.39.0"), "{stdout}");
    let (code, stdout, _) = fixture.run(&["search", "iterm2", "--source", "cask"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Not available here:"), "{stdout}");
    let (code, stdout, _) = fixture.run(&["search", "iterm", "--source", "cask", "--unavailable"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("cask:iterm2"), "{stdout}");
}

#[test]
fn unreadable_profile_reports_unknown_info_and_doctor_state() {
    let fixture = CaskFixture::new();
    let profile = fixture.profile();
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("missing-generation", &profile).unwrap();
    let script = std::fs::read_to_string(&fixture.nix).unwrap().replace(
        "echo '{\"version\":3,\"elements\":{}}'",
        "echo 'error: unreadable profile' >&2; exit 1",
    );
    fixture.set_nix_script(&script);
    let (code, stdout, stderr) = fixture.run(&["info", "cask:homebrew/cask/iterm2", "--json"]);
    assert_eq!(code, 1, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["installed_state"], "unknown");
    assert!(
        value["result"]["installed_error"]
            .as_str()
            .unwrap()
            .contains("unreadable profile")
    );
    let (code, stdout, _) = fixture.run(&["doctor", "--json"]);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let profile_row = value["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["component"] == "pkg profile")
        .unwrap();
    assert_eq!(profile_row["status"], "unreadable");
}

#[test]
fn cask_discovery_cancellation_stops_info_and_install() {
    let fixture = CaskFixture::new();
    let profile = fixture.profile();
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("missing-generation", &profile).unwrap();
    let script = std::fs::read_to_string(&fixture.nix)
        .unwrap()
        .replace("*'eval --json'*) cat", "*'eval --json'*) exit 130; cat");
    fixture.set_nix_script(&script);
    for command in ["info", "install"] {
        for id in ["cask:iterm2", "cask:homebrew/cask/iterm2"] {
            let (code, stdout, stderr) = fixture.run(&[command, id]);
            assert_eq!(code, 130, "{command} {id}: {stderr}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(stderr.contains("Discovery cancelled"), "{stderr}");
            assert_eq!(
                std::fs::read_link(&profile).unwrap(),
                Path::new("missing-generation")
            );
        }
    }
}

#[test]
fn bare_cask_skips_a_source_that_excludes_it() {
    use pkg_cli::tap::{
        Registry, registry,
        store::{Provenance, SourceStore},
    };
    let fixture = CaskFixture::new();
    let state = fixture.home.path().join("state");
    let source = "example/tools";
    let origin = "https://github.com/example/homebrew-tools";
    let revision = "1111111111111111111111111111111111111111";
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let raw = serde_json::json!({
        "schema": "pkg-cask-catalog/4", "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {source: {"url": origin, "revision": revision, "sha256": hash, "license": "Homebrew data license",
            "raw": {"archiveUrl": format!("https://github.com/example/homebrew-tools/archive/{revision}.tar.gz"), "archiveSha256": hash, "captureSha256": hash, "envelopeSchema": "pkg-tap-raw-export/1"}}},
        "targets": ["x86_64-linux"], "macosBaseline": "15.7.7", "systems": {"x86_64-linux": {"entries": {
            "example/tools/iterm2": {"source": source, "token": "iterm2", "name": "Tap terminal", "description": null,
                "version": "1.0", "homepage": null, "status": "eligible", "kind": "binary", "reason": null, "detail": null}
        }}}
    });
    let index = pkg_cli::nix::decode_catalog_index(&raw).unwrap();
    let store = SourceStore::lock(&state, source).unwrap();
    let staged = store.new_staging().unwrap();
    std::fs::create_dir(staged.path().join("flake")).unwrap();
    staged
        .publish(
            &Provenance {
                source: source.to_owned(),
                origin: origin.to_owned(),
                revision: revision.to_owned(),
                metadata_sha256: hash.to_owned(),
                eligible: 1,
                excluded: 0,
                published_unix: 1,
            },
            &index,
        )
        .unwrap();
    let mut registry = Registry::empty();
    registry.approve(source, origin, 1);
    registry.save(&registry::registry_path(&state)).unwrap();
    let (code, output, error) = fixture.run(&["info", "cask:iterm2"]);
    assert_eq!(code, 0, "{error}");
    assert!(output.contains("Tap terminal"), "{output}");
    assert!(output.contains("ID: cask:example/tools/iterm2"), "{output}");
    assert!(output.contains("Status: Available"), "{output}");
}
