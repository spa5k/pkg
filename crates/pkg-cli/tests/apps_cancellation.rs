#![expect(missing_docs, reason = "integration test crates have no public API")]
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
use std::process::{Child, Command, Stdio};
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

    fn repair_helper_at(&self, stage: &str) {
        write_executable(
            &self.helper_root.join("bin/mac-app-util"),
            "#!/bin/sh\necho 'failed to allocate 1048576 bytes at 0x300100000' >&2\nexit 1\n",
        );
        let repaired = self.root.path().join("repaired-helper");
        fs::create_dir_all(repaired.join("bin")).expect("repaired helper directory");
        write_executable(
            &repaired.join("bin/mac-app-util"),
            &format!(
                "#!/bin/sh\ntouch {}.retry\nexec /bin/sleep 60\n",
                quote(&self.ready.display().to_string())
            ),
        );
        let manifest = serde_json::json!({
            "version": 3,
            "elements": {"helper": {
                "active": true,
                "attrPath": "packages.aarch64-darwin.default",
                "originalUrl": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                "url": "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887",
                "storePaths": [repaired.display().to_string()]
            }}
        });
        let original = self.root.path().join("nix-original");
        fs::rename(&self.nix, &original).expect("save original runtime");
        write_executable(
            &self.nix,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 *'profile add'*) touch {ready}.build; \
                   if [ '{stage}' = build ]; then exec /bin/sleep 60; fi; \
                   while [ \"$1\" != --profile ]; do shift; done; mkdir -p \"$2\";;\n\
                 *'profile list'*'pkg-app-tools'*) exec {original} \"$@\";;\n\
                 *'profile list'*'/helper'*) printf '%s\\n' {manifest};;\n\
                 *'build --no-link'*) touch {ready}.promoted;;\n\
                 *) exec {original} \"$@\";;\nesac\n",
                ready = quote(&self.ready.display().to_string()),
                original = quote(&original.display().to_string()),
                manifest = quote(&manifest.to_string()),
            ),
        );
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make executable");
}

fn wait_for_child_stage(mut child: Child, path: &Path) -> Child {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return child;
        }
        if child.try_wait().expect("poll fixture child").is_some() {
            let output = child.wait_with_output().expect("collect early exit");
            panic!("child exited before {}: {output:?}", path.display());
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().expect("stop timed-out fixture child");
    let output = child
        .wait_with_output()
        .expect("collect timed-out fixture child");
    panic!(
        "stage marker did not appear: {}: {output:?}",
        path.display()
    );
}

#[test]
fn compiled_cli_preserves_signal_status_and_launcher_state_at_each_child_stage() {
    for (stage, signal) in [
        ("prepare", libc::SIGINT),
        ("prepare", libc::SIGTERM),
        ("prepare", libc::SIGHUP),
        ("sync", libc::SIGINT),
        ("sync", libc::SIGTERM),
        ("sync", libc::SIGHUP),
    ] {
        let fixture = Fixture::new(
            stage == "prepare",
            "#!/bin/sh\n[ \"$1\" = 'sync-trampolines' ] || exit 18\n[ -L \"$2\" ] && [ -d \"$2\" ] || exit 17\ntouch {ready}.sync\nexec /bin/sleep 60\n",
            "#!/bin/sh\ntouch {ready}.prepare\nexec /bin/sleep 60\n",
        );
        let mut command = fixture.command();
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().expect("spawn compiled pkg");
        let stage_marker = PathBuf::from(format!("{}.{}", fixture.ready.display(), stage));
        let child = wait_for_child_stage(child, &stage_marker);
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
    }
}

#[test]
fn canceled_helper_repair_keeps_the_selected_profile_and_launchers() {
    for stage in ["build", "retry"] {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let fixture = Fixture::new(false, "#!/bin/sh\nexit 0\n", "");
            fixture.repair_helper_at(stage);
            let mut command = fixture.command();
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            let child = command.spawn().expect("spawn repair");
            let child = wait_for_child_stage(
                child,
                &PathBuf::from(format!("{}.{stage}", fixture.ready.display())),
            );
            assert_eq!(unsafe { libc::kill(child.id().cast_signed(), signal) }, 0);
            let output = child.wait_with_output().expect("collect repair");
            assert_eq!(output.status.code(), Some(128 + signal));
            let stderr = String::from_utf8_lossy(&output.stderr);
            let dock = if stage == "retry" {
                "may have changed"
            } else {
                "did not change"
            };
            assert!(stderr.contains(&format!("Dock state {dock}")), "{stderr}");
            assert!(!PathBuf::from(format!("{}.promoted", fixture.ready.display())).exists());
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
    let child = wait_for_child_stage(
        child,
        &PathBuf::from(format!("{}.doctor", fixture.ready.display())),
    );
    let result = unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) };
    assert_eq!(result, 0, "send SIGTERM");
    let output = child.wait_with_output().expect("wait for doctor");
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("signal 15"), "{stderr}");
    assert_eq!(output.stdout, [] as [u8; 0]);
}

#[test]
fn doctor_reports_launcher_drift_and_health_without_writing() {
    let fixture = Fixture::new(false, "#!/bin/sh\nexit 0\n", "");
    let observe = || {
        let output = fixture
            .command_with(&["doctor", "--json"])
            .output()
            .expect("run doctor");
        let result: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("doctor JSON");
        result["result"]
            .as_array()
            .expect("doctor rows")
            .iter()
            .find(|row| row["component"] == "app launchers")
            .expect("launcher observation")
            .clone()
    };
    let drift = observe();
    assert_eq!(drift["status"], "unhealthy");
    assert_eq!(
        drift["detail"],
        format!(
            "app launcher drift in {} (missing: [Stats.app]; stale: [Old.app]; changed source: []); run `pkg apps sync`",
            fixture.launchers().display()
        )
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
        SOURCE_MAP
    );
    assert!(!fixture.launchers().join("Stats.app").exists());

    fs::write(
        fixture.launchers().join(".pkg-launcher-sources"),
        "Stats.app\t/old/source/Stats.app\n",
    )
    .unwrap();
    let changed = observe();
    assert_eq!(changed["status"], "unhealthy");
    assert_eq!(
        changed["detail"],
        format!(
            "app launcher drift in {} (missing: []; stale: []; changed source: [Stats.app]); run `pkg apps sync`",
            fixture.launchers().display()
        )
    );
    assert!(!fixture.launchers().join("Stats.app").exists());

    let source_map = format!(
        "Stats.app\t{}\n",
        fixture.app_root.join("Applications/Stats.app").display()
    );
    fs::write(
        fixture.launchers().join(".pkg-launcher-sources"),
        &source_map,
    )
    .unwrap();
    fs::write(fixture.launchers().join("Stats.app"), "current launcher").unwrap();
    let healthy = observe();
    assert_eq!(healthy["status"], "healthy");
    assert_eq!(
        healthy["detail"],
        format!("1 launcher in {}", fixture.launchers().display())
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
        source_map
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join("Stats.app")).unwrap(),
        "current launcher"
    );
    assert!(!fixture.ready.exists());
}

#[test]
fn first_sync_creates_the_user_applications_directory() {
    let fixture = Fixture::new(
        false,
        "#!/bin/sh\nif [ \"$1\" = '--help' ]; then exit 0; fi\nmkdir -p \"$3\"\necho new launcher > \"$3/Stats.app\"\n",
        "",
    );
    fs::remove_dir_all(fixture.home.join("Applications")).unwrap();
    let output = fixture.command().output().expect("run compiled pkg");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join(".pkg-owned")).unwrap(),
        MARKER
    );
    assert_eq!(
        fs::read_to_string(fixture.launchers().join("Stats.app")).unwrap(),
        "new launcher\n"
    );
}

#[test]
fn canceled_refresh_reports_the_completed_native_upgrade() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let fixture = Fixture::new(
            false,
            "#!/bin/sh\nif [ \"$1\" = '--help' ]; then exit 0; fi\ntouch {ready}.sync\nexec /bin/sleep 60\n",
            "",
        );
        fs::create_dir_all(fixture.root.path().join("state/nix/profiles/pkg")).unwrap();
        let runtime = fs::read_to_string(&fixture.nix).unwrap();
        let mutation = fixture.root.path().join("upgraded");
        let branch = format!(
            "  *'profile upgrade'*) touch {};;\n  *) echo",
            quote(&mutation.display().to_string())
        );
        write_executable(&fixture.nix, &runtime.replace("  *) echo", &branch));
        let mut command = fixture.command_with(&["upgrade", "stats", "--entry"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().expect("spawn compiled pkg upgrade");
        let child = wait_for_child_stage(
            child,
            &PathBuf::from(format!("{}.sync", fixture.ready.display())),
        );
        assert!(mutation.exists());
        assert_eq!(unsafe { libc::kill(child.id().cast_signed(), signal) }, 0);
        let output = child.wait_with_output().expect("wait for upgrade");
        assert_eq!(output.status.code(), Some(128 + signal));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Partial: upgrade"), "{stderr}");
        assert!(stderr.contains("pkg apps sync"), "{stderr}");
        assert!(stderr.contains("Dock state may have changed"), "{stderr}");
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert_eq!(
            fs::read_to_string(fixture.launchers().join(".pkg-launcher-sources")).unwrap(),
            SOURCE_MAP
        );
        assert_eq!(
            fs::read_to_string(fixture.launchers().join("Old.app")).unwrap(),
            "old launcher"
        );
    }
}
