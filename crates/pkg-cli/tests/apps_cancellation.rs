//! Compiled CLI cancellation and launcher publication tests.
#![cfg(target_os = "macos")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MARKER: &str = "pkg-apps-owned/1\n";
const SOURCE_MAP: &str = "Old.app\t/old/source/Old.app\n";

struct Fixture {
    root: tempfile::TempDir,
    home: PathBuf,
    app_root: PathBuf,
    helper_root: PathBuf,
    ready: PathBuf,
    nix: PathBuf,
}

impl Fixture {
    fn new(private_app: bool, helper_script: &str, payload_script: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home with spaces");
        let config = root.path().join("config");
        let output = root.path().join("store-apps");
        let helper = root.path().join("store-helper");
        let ready = root.path().join("ready");
        let ready_literal = quote(&ready.display().to_string());
        fs::create_dir_all(home.join("Applications/pkg")).expect("launcher folder");
        fs::create_dir_all(&output).expect("app output");
        fs::create_dir_all(helper.join("bin")).expect("helper output");
        fs::write(home.join("Applications/pkg/.pkg-owned"), MARKER).expect("ownership marker");
        fs::write(
            home.join("Applications/pkg/.pkg-launcher-sources"),
            SOURCE_MAP,
        )
        .expect("source map");
        fs::write(home.join("Applications/pkg/Old.app"), "old launcher").expect("old launcher");
        let helper_path = helper.join("bin/mac-app-util");
        write_executable(
            &helper_path,
            &helper_script.replace("{ready}", &ready_literal),
        );

        let app_path = if private_app {
            let namespace = output.join("libexec/pkg-casks").join("store-apps");
            let bundle = namespace.join("app-sources/Stats.app");
            fs::create_dir_all(bundle.join("Contents")).expect("private app source");
            fs::write(
                namespace.join("apps.json"),
                serde_json::json!({
                    "schema": "pkg-cask-apps/2",
                    "apps": {"Stats.app": {"source": "libexec/pkg-casks/store-apps/app-sources/Stats.app"}}
                })
                .to_string(),
            )
            .expect("private app manifest");
            let prepare = namespace.join("prepare");
            write_executable(&prepare, &payload_script.replace("{ready}", &ready_literal));
            output.clone()
        } else {
            let bundle = output.join("Applications/Stats.app");
            fs::create_dir_all(bundle.join("Contents")).expect("app bundle");
            output.clone()
        };

        let package_manifest = serde_json::json!({
            "version": 3,
            "elements": {
                "stats": {
                    "active": true,
                    "attrPath": "packages.aarch64-darwin.Stats",
                    "originalUrl": "github:example/stats",
                    "url": "github:example/stats/1111111111111111111111111111111111111111",
                    "storePaths": [app_path.display().to_string()]
                }
            }
        });
        let helper_manifest = serde_json::json!({
            "version": 3,
            "elements": {
                "opaque-helper": {
                    "active": true,
                    "attrPath": "packages.aarch64-darwin.default",
                    "originalUrl": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                    "url": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                    "locked": {"type": "github", "owner": "hraban", "repo": "mac-app-util", "rev": "039f33deef21782d4db97087f426504951239887"},
                    "storePaths": [helper.display().to_string()]
                }
            }
        });
        let nix = root.path().join("nix");
        let nix_script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'profile list'*'pkg-app-tools'*) printf '%s\\n' {}; ;\n  *'profile list'*) printf '%s\\n' {}; ;\n  *) echo \"unexpected nix call: $*\" >&2; exit 9;;\nesac\n",
            quote(&helper_manifest.to_string()),
            quote(&package_manifest.to_string()),
        )
        .replace("; ;", ";;");
        write_executable(&nix, &nix_script);
        fs::create_dir_all(config.join("pkg")).expect("config folder");
        fs::write(
            config.join("pkg/config.toml"),
            format!("[runtime]\nnix = '{}'\n", nix.display()),
        )
        .expect("runtime config");
        Self {
            root,
            home,
            app_root: output,
            helper_root: helper,
            ready,
            nix,
        }
    }

    fn command(&self) -> Command {
        self.command_with(&["apps", "sync"])
    }

    fn command_with(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("PKG_APP_CACHE_DIR", self.root.path().join("app-cache"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.nix.parent().unwrap().display()),
            );
        command
    }

    fn interrupt_doctor_app_status(&self) {
        let profile_seen = self.root.path().join("profile-seen");
        let package_manifest = serde_json::json!({
            "version": 3,
            "elements": {
                "stats": {
                    "active": true,
                    "attrPath": "packages.aarch64-darwin.Stats",
                    "originalUrl": "github:example/stats",
                    "url": "github:example/stats/1111111111111111111111111111111111111111",
                    "storePaths": [self.app_root.display().to_string()]
                }
            }
        });
        let helper_manifest = serde_json::json!({
            "version": 3,
            "elements": {
                "opaque-helper": {
                    "active": true,
                    "attrPath": "packages.aarch64-darwin.default",
                    "originalUrl": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                    "url": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                    "storePaths": [self.helper_root.display().to_string()]
                }
            }
        });
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'store info'*) echo 'daemon reachable';;\n  *'profile list'*'pkg-app-tools'*) printf '%s\\n' {}; ;\n  *'profile list'*) if [ -f {} ]; then touch {}.doctor; exec /bin/sleep 60; else touch {}; printf '%s\\n' {}; fi;;\n  *) exit 0;;\nesac\n",
            quote(&helper_manifest.to_string()),
            quote(&profile_seen.display().to_string()),
            quote(&self.ready.display().to_string()),
            quote(&profile_seen.display().to_string()),
            quote(&package_manifest.to_string()),
        )
        .replace("; ;", ";;");
        write_executable(&self.nix, &script);
        fs::create_dir_all(self.root.path().join("state/nix/profiles/pkg"))
            .expect("doctor profile directory");
    }

    fn launchers(&self) -> PathBuf {
        self.home.join("Applications/pkg")
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make executable");
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("stage marker did not appear: {}", path.display());
}

#[test]
fn compiled_cli_preserves_signal_status_and_launcher_state_at_each_child_stage() {
    for (stage, signal) in [
        ("probe", libc::SIGINT),
        ("probe", libc::SIGTERM),
        ("probe", libc::SIGHUP),
        ("prepare", libc::SIGINT),
        ("prepare", libc::SIGTERM),
        ("prepare", libc::SIGHUP),
        ("sync", libc::SIGINT),
        ("sync", libc::SIGTERM),
        ("sync", libc::SIGHUP),
    ] {
        let fixture = Fixture::new(
            stage == "prepare",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = '--help' ]; then touch {ready}.probe; if [ '{stage}' = 'probe' ]; then exec /bin/sleep 60; else exit 0; fi; fi\n[ -L \"$2\" ] && [ -d \"$2\" ] || exit 17\ntouch {ready}.sync\nexec /bin/sleep 60\n",
                ready = "{ready}"
            ),
            "#!/bin/sh\ntouch {ready}.prepare\nexec /bin/sleep 60\n",
        );
        let mut command = fixture.command();
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().expect("spawn compiled pkg");
        let stage_marker = PathBuf::from(format!("{}.{}", fixture.ready.display(), stage));
        wait_for_file(&stage_marker);
        // SAFETY: the PID belongs to this live child process.
        let result = unsafe { libc::kill(child.id().cast_signed(), signal) };
        assert_eq!(result, 0, "send signal {signal}");
        let output = child.wait_with_output().expect("wait for compiled pkg");
        assert_eq!(output.status.code(), Some(128 + signal));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("signal {signal}")), "{stderr}");
        assert!(stderr.contains("The package profile"), "{stderr}");
        if stage == "sync" {
            assert!(stderr.contains("Dock state may have changed"), "{stderr}");
        } else {
            assert!(stderr.contains("Dock state did not change"), "{stderr}");
        }
        assert_eq!(
            fs::read_to_string(fixture.launchers().join(".pkg-owned")).unwrap(),
            MARKER
        );
        assert_eq!(
            fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
            SOURCE_MAP
        );
        assert_eq!(
            fs::read_to_string(fixture.launchers().join("Old.app")).unwrap(),
            "old launcher"
        );
        if stage == "sync" {
            assert!(PathBuf::from(format!("{}.probe", fixture.ready.display())).exists());
        }
    }
}

#[test]
fn failed_helper_replacement_contract_keeps_the_complete_launcher_folder() {
    let fixture = Fixture::new(
        false,
        "#!/bin/sh\nif [ \"$1\" = '--help' ]; then exit 0; fi\n[ -L \"$2\" ] && [ -d \"$2\" ] || exit 17\nrm -rf \"$3\"\nmkdir -p \"$3\"\necho partial > \"$3/Partial.app\"\nexit 1\n",
        "",
    );
    let output = fixture.command().output().expect("run compiled pkg");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Dock state may have changed"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-owned")).unwrap(),
        MARKER
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
        SOURCE_MAP
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join("Old.app")).unwrap(),
        "old launcher"
    );
}

#[test]
fn successful_helper_output_publishes_the_new_launcher_and_source_map() {
    let fixture = Fixture::new(
        false,
        "#!/bin/sh\nif [ \"$1\" = '--help' ]; then exit 0; fi\n[ -L \"$2\" ] && [ -d \"$2\" ] || exit 17\nrm -rf \"$3\"\nmkdir -p \"$3\"\necho new launcher > \"$3/Stats.app\"\n",
        "",
    );
    let output = fixture.command().output().expect("run compiled pkg");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-owned")).unwrap(),
        MARKER
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
        format!(
            "Stats.app\t{}\n",
            fixture.app_root.join("Applications/Stats.app").display()
        )
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join("Stats.app")).unwrap(),
        "new launcher\n"
    );
    assert!(!fixture.launchers().join("Old.app").exists());
}

#[test]
fn unowned_launcher_content_is_never_changed() {
    let fixture = Fixture::new(false, "#!/bin/sh\nexit 0\n", "");
    fs::remove_file(fixture.launchers().join(".pkg-owned")).unwrap();
    fs::write(fixture.launchers().join("Unowned.app"), "keep").unwrap();
    let output = fixture.command().output().expect("run compiled pkg");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(fixture.launchers().join("Unowned.app")).unwrap(),
        "keep"
    );
    assert!(!fixture.ready.exists());
}

#[test]
fn doctor_preserves_interruption_during_app_status() {
    let fixture = Fixture::new(false, "#!/bin/sh\nexit 0\n", "");
    fixture.interrupt_doctor_app_status();
    let mut command = fixture.command_with(&["doctor", "--json"]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn().expect("spawn compiled pkg doctor");
    wait_for_file(&PathBuf::from(format!(
        "{}.doctor",
        fixture.ready.display()
    )));
    // SAFETY: the PID belongs to this live child process.
    let result = unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) };
    assert_eq!(result, 0, "send SIGTERM");
    let output = child.wait_with_output().expect("wait for doctor");
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("signal 15"), "{stderr}");
    assert!(output.stdout.is_empty());
}
