#![expect(missing_docs, reason = "integration test crates have no public API")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
)]

use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

struct ConflictFixture {
    bin: TempDir,
    home: TempDir,
    failed: PathBuf,
    unreadable: PathBuf,
    manifest: PathBuf,
    profile: PathBuf,
}

impl ConflictFixture {
    fn new() -> Self {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let nix = bin.path().join("nix");
        let profile = home.path().join("state/nix/profiles/pkg");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "profile stays unchanged").unwrap();
        let manifest = bin.path().join("manifest.json");
        let diagnostic = bin.path().join("conflict.txt");
        let failed = bin.path().join("failed");
        let unreadable = bin.path().join("unreadable");
        let formula = bin.path().join("formula.json");
        let index = formula_fixture();
        std::fs::write(&formula, index.to_string()).unwrap();
        std::fs::write(
            &diagnostic,
            concat!(
                "\x1b[31merror:\x1b[0m An existing package already provides the following file:\n\n",
                "  \"/nix/store/old-bat/bin/bat\"\n\n",
                "This is the conflicting file from the new package:\n\n",
                "  \"/nix/store/new-bat/bin/bat\"\n\n",
                "To remove the existing package:\n",
                "  nix profile remove --profile '/tmp/pkg' bat\n",
                "The new package can also be added next to the existing one by assigning a different priority.\n",
                "To prioritise the new package:\n",
                "  nix profile add --profile '/tmp/pkg' github:spa5k/pkg/main?dir=nix/formulae#packages.x86_64-linux.homebrew/core/bat --priority 4\n",
                "warning: a separate warning\n",
                "do you want to allow configuration setting 'extra-substituters' (y/N)? \n",
            ),
        )
        .unwrap();
        let script = format!(
            r#"#!/bin/sh
case "$*" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'config show system'*) echo 'x86_64-linux';;
  *'flake metadata'*) echo '{{"url":"github:spa5k/pkg/main?dir=nix/formulae","locked":{{"type":"github","owner":"spa5k","repo":"pkg","rev":"1111111111111111111111111111111111111111","dir":"nix/formulae"}}}}';;
  *'#formulaIndex'*) cat '{formula}';;
  *'search '*'#bat'*) echo '{{"legacyPackages.x86_64-linux.bat":{{"pname":"bat","version":"0.26.1","description":"Cat with syntax highlighting"}}}}';;
  *'eval --json'*'#bat'*) echo '["/nix/store/old-bat"]';;
  *'profile list'*)
    if [ -f '{failed}' ] && [ -f '{unreadable}' ]; then
      echo 'error: unreadable profile' >&2; exit 1
    fi
    cat '{manifest}';;
  *'profile add'*) cat '{diagnostic}' >&2; touch '{failed}'; exit 1;;
  *) echo 'unexpected nix call' >&2; exit 9;;
esac
"#,
            formula = formula.display(),
            failed = failed.display(),
            unreadable = unreadable.display(),
            manifest = manifest.display(),
            diagnostic = diagnostic.display()
        );
        std::fs::write(&nix, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            bin,
            home,
            failed,
            unreadable,
            manifest,
            profile,
        }
    }

    fn set_entries(&self, entries: &serde_json::Value) {
        std::fs::write(
            &self.manifest,
            serde_json::json!({"version":3,"elements":entries}).to_string(),
        )
        .unwrap();
    }

    fn run(&self, verbose: bool, id: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        if verbose {
            command.arg("--verbose");
        }
        command
            .args(["install", id])
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.bin.path().display()),
            )
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .output()
            .unwrap()
    }

    fn assert_safe_conflict(
        &self,
        output: &Output,
        scenario: &str,
        owner_text: &str,
        state_text: &str,
    ) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{scenario}: {stderr}");
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert!(stderr.contains("bin/bat"), "{scenario}: {stderr}");
        assert!(stderr.contains(owner_text), "{scenario}: {stderr}");
        assert!(stderr.contains(state_text), "{stderr}");
        assert!(
            stderr.contains("Requested: brew:homebrew/core/bat (Homebrew Formula)."),
            "{stderr}"
        );
        for hidden in [
            "/nix/store/",
            "nix profile",
            "priority",
            "To remove",
            "pkg remove",
            "pkg install",
        ] {
            assert!(
                !stderr.contains(hidden),
                "{scenario}: leaked {hidden}: {stderr}"
            );
        }
        assert!(stderr.contains("warning: a separate warning"), "{stderr}");
        assert!(
            stderr.contains("do you want to allow configuration"),
            "{stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(&self.profile).unwrap(),
            "profile stays unchanged"
        );
    }
}

fn formula_fixture() -> serde_json::Value {
    let mut index = serde_json::json!({
        "schema": "pkg-formula-catalog/1",
        "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {"homebrew/core": {
            "url": "https://github.com/homebrew/homebrew-core",
            "revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "license": "Homebrew data license",
            "raw": {
                "archiveUrl": "https://github.com/homebrew/homebrew-core/archive/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tar.gz",
                "archiveSha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "captureSha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "envelopeSchema": "pkg-tap-raw-export/1"
            }
        }},
        "targets": ["x86_64-linux"],
        "macosBaseline": "15.7.7",
        "systems": {"x86_64-linux": {"entries": {
            "homebrew/core/bat": {
                "source":"homebrew/core", "token":"bat", "name":"bat",
                "description":"Cat with syntax highlighting", "version":"1.0",
                "homepage":null, "status":"eligible", "kind":"binary",
                "reason":null, "detail":null
            }
        }}}
    });
    index["aliases"] = serde_json::json!({});
    index["executables"] = serde_json::json!({});
    index
}

fn owner() -> serde_json::Value {
    serde_json::json!({
        "active": true,
        "attrPath": "legacyPackages.x86_64-linux.bat",
        "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
        "url": "github:NixOS/nixpkgs/1111111111111111111111111111111111111111",
        "storePaths": ["/nix/store/old-bat"]
    })
}

#[test]
fn conflicts_name_one_verified_owner_and_hide_replacement_commands() {
    let fixture = ConflictFixture::new();
    fixture.set_entries(&serde_json::json!({"bat": owner()}));
    let output = fixture.run(false, "brew:homebrew/core/bat");
    fixture.assert_safe_conflict(
        &output,
        "one owner",
        "Cause: nixpkgs:bat (Nixpkgs) is already installed and provides bin/bat.",
        "Installed entries unchanged.",
    );
}

#[test]
fn conflicts_name_all_verified_owners() {
    let fixture = ConflictFixture::new();
    fixture.set_entries(&serde_json::json!({"bat": owner(), "bat-1": owner()}));
    let output = fixture.run(false, "brew:homebrew/core/bat");
    fixture.assert_safe_conflict(
        &output,
        "multiple owners",
        "nixpkgs:bat (Nixpkgs) (entry: bat)",
        "Installed entries unchanged.",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nixpkgs:bat (Nixpkgs) (entry: bat-1)"),
        "{stderr}"
    );
}

#[test]
fn conflicts_do_not_claim_unknown_or_unmatched_owners() {
    for scenario in ["unknown", "unmatched"] {
        let fixture = ConflictFixture::new();
        let mut entries = serde_json::json!({"bat": owner()});
        if scenario == "unknown" {
            std::fs::write(&fixture.unreadable, "").unwrap();
        } else {
            entries["bat"]["active"] = serde_json::json!(false);
            entries["lookalike"] = owner();
            entries["lookalike"]["storePaths"] = serde_json::json!(["/nix/store/old"]);
        }
        fixture.set_entries(&entries);
        let output = fixture.run(false, "brew:homebrew/core/bat");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let state = if scenario == "unknown" {
            "Package state is unknown."
        } else {
            "Installed entries unchanged."
        };
        fixture.assert_safe_conflict(
            &output,
            scenario,
            "Cause: two packages provide bin/bat.",
            state,
        );
        assert!(!stderr.contains("nixpkgs:bat"), "{stderr}");
    }
}

#[test]
fn verbose_conflicts_include_native_repair_context() {
    let fixture = ConflictFixture::new();
    fixture.set_entries(&serde_json::json!({"bat": owner()}));
    let output = fixture.run(true, "brew:homebrew/core/bat");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("/nix/store/old-bat/bin/bat"), "{stderr}");
    assert!(stderr.contains("--priority 4"), "{stderr}");
}

#[test]
fn already_installed_package_skips_profile_add() {
    let fixture = ConflictFixture::new();
    fixture.set_entries(&serde_json::json!({"bat": owner()}));
    let output = fixture.run(false, "nixpkgs:bat");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Already installed: bat"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "Resolving: nixpkgs:bat"
    );
    assert!(!fixture.failed.exists());
}
