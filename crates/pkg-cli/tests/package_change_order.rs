#![expect(missing_docs, reason = "integration test crates have no public API")]
#![cfg(unix)]
#![allow(
    clippy::expect_used,
    reason = "isolated process fixtures fail directly when setup is unavailable"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::{Command, Output};

struct Fixture {
    home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temporary home");
        let runtime = home.path().join("nix");
        fs::write(&runtime, RUNTIME).expect("write runtime");
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755))
            .expect("executable runtime");
        let config = home.path().join("config/pkg");
        fs::create_dir_all(&config).expect("config directory");
        fs::write(
            config.join("config.toml"),
            format!("[runtime]\nnix = '{}'\n", runtime.display()),
        )
        .expect("runtime config");
        let profiles = home.path().join("state/nix/profiles");
        fs::create_dir_all(&profiles).expect("profile directory");
        symlink("pkg-1-link", profiles.join("pkg")).expect("native generation link");
        Self { home }
    }

    fn run(&self, args: &[&str], stop: &str, signal: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pkg"))
            .args(args)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env("PKG_ORDER_HOME", self.home.path())
            .env("PKG_ORDER_STOP", stop)
            .env("PKG_ORDER_SIGNAL", signal)
            .output()
            .expect("compiled CLI")
    }

    fn calls(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.home.path().join("calls.jsonl"))
            .expect("runtime calls")
            .lines()
            .map(|line| serde_json::from_str(line).expect("call record"))
            .collect()
    }
}

const RUNTIME: &str = r"#!/usr/bin/python3
import fcntl,json,os,signal,sys,time
from pathlib import Path
home=Path(os.environ['PKG_ORDER_HOME'])
args=sys.argv[1:]
text=' '.join(args)
profile=home/'state/nix/profiles/pkg'
changed=(home/'changed').exists()
action=next((name for name in ('list','add','remove','upgrade','rollback','wipe-history','history') if 'profile '+name in text),None)
locked=False
if action:
 with open(str(profile)+'.pkg-lock','a') as lock:
  try: fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
  except BlockingIOError: locked=True
 with open(home/'calls.jsonl','a') as log: log.write(json.dumps({'call':action,'locked':locked,'changed':changed})+'\n')
def interrupt():
 sig=getattr(signal,'SIG'+os.environ['PKG_ORDER_SIGNAL'])
 os.kill(os.getppid(),sig)
 time.sleep(2)
 sys.exit(128+sig)
if '--version' in args: print('nix (Nix) 2.35.2')
elif 'config show system' in text: print('x86_64-linux')
elif action=='list':
 stop=os.environ['PKG_ORDER_STOP']
 if (stop=='before' and not changed) or (stop=='after' and changed): interrupt()
 entries={} if changed else {'opaque':{'active':True,'attrPath':'legacyPackages.x86_64-linux.probe','originalUrl':'github:NixOS/nixpkgs','url':'github:NixOS/nixpkgs/1111111111111111111111111111111111111111','storePaths':[]}}
 print(json.dumps({'version':3,'elements':entries}))
elif action=='history':
 if os.environ['PKG_ORDER_STOP']=='after' and changed: interrupt()
 print('Version 2 (2026-10-12)' if changed else 'Version 1 (2026-10-11)\nVersion 2 (2026-10-12)')
elif action in ('add','remove','upgrade','rollback','wipe-history'):
 (home/'changed').touch()
 if action=='rollback':
  profile.unlink()
  profile.symlink_to('pkg-2-link')
 if os.environ['PKG_ORDER_STOP']=='mutation': interrupt()
elif 'flake metadata' in text: print(json.dumps({'url':'github:fixture/packages/1111111111111111111111111111111111111111'}))
else:
 print('unexpected native call: '+text,file=sys.stderr)
 sys.exit(9)
";

const ACTIONS: &[&[&str]] = &[
    &["install", "github:fixture/packages#probe"],
    &["remove", "opaque", "--entry"],
    &["upgrade", "opaque", "--entry"],
    &["rollback", "1"],
    &["prune", "--older-than", "30d"],
];

#[test]
fn failure_state_cancellation_keeps_its_signal_and_skips_conflict_diagnostics() {
    for (signal, code) in [("INT", 130), ("TERM", 143), ("HUP", 129)] {
        let fixture = Fixture::new();
        let runtime = RUNTIME
            .replace(
                "text=' '.join(args)",
                "text=' '.join(args)\nwith open(home/'all-calls.jsonl','a') as log: log.write(json.dumps(text)+'\\n')",
            )
            .replace(
                "if os.environ['PKG_ORDER_STOP']=='mutation': interrupt()",
                "print('error: An existing package already provides the following file:\\n\\n  \"/nix/store/old-probe/bin/probe\"\\n\\nThis is the conflicting file from the new package:\\n\\n  \"/nix/store/new-probe/bin/probe\"',file=sys.stderr)\n sys.exit(1)",
            );
        fs::write(fixture.home.path().join("nix"), runtime).expect("failing runtime");
        let output = fixture.run(ACTIONS[0], "after", signal);
        assert_eq!(output.status.code(), Some(code), "{signal}: {output:?}");
        assert!(output.stdout.is_empty(), "no completion: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "Interrupted: install stopped.\nPackage state is unknown. Next: run `pkg doctor`.\n"
            ),
            "{signal}: {stderr}"
        );
        assert!(
            stderr.contains("Cause: An existing package already provides the following file:"),
            "original mutation cause: {stderr}"
        );
        assert!(
            !stderr.contains("Cause: two packages provide") && !stderr.contains("Requested:"),
            "no conflict diagnostics after cancellation: {stderr}"
        );
        let calls = fixture.calls();
        assert_eq!(
            calls
                .iter()
                .map(|call| call["call"].as_str().expect("native call name"))
                .collect::<Vec<_>>(),
            ["list", "add", "list"],
            "{signal}: {calls:?}"
        );
        assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
        assert_eq!(
            calls
                .iter()
                .map(|call| call["changed"].as_bool().expect("native change flag"))
                .collect::<Vec<_>>(),
            [false, false, true],
            "{signal}: {calls:?}"
        );
        let all_calls = fs::read_to_string(fixture.home.path().join("all-calls.jsonl"))
            .expect("all native calls");
        assert!(
            all_calls
                .lines()
                .last()
                .expect("final native call")
                .contains("profile list "),
            "no diagnostic Nix work after the reread: {all_calls}"
        );
    }
}

#[test]
fn mutation_signal_wins_over_failure_state_cancellation() {
    for (first, later, code) in [
        ("INT", "TERM", 130),
        ("TERM", "HUP", 143),
        ("HUP", "INT", 129),
    ] {
        let fixture = Fixture::new();
        let runtime = RUNTIME.replace(
            " stop=os.environ['PKG_ORDER_STOP']",
            &format!(
                " if changed:\n  os.environ['PKG_ORDER_SIGNAL']='{later}'\n  interrupt()\n stop=os.environ['PKG_ORDER_STOP']"
            ),
        );
        fs::write(fixture.home.path().join("nix"), runtime).expect("cancelling runtime");
        let output = fixture.run(ACTIONS[1], "mutation", first);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{first}, {later}: {output:?}"
        );
        assert!(output.stdout.is_empty(), "no completion: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "Interrupted: remove stopped.\nPackage state is unknown. Next: run `pkg doctor`.\n"
            ),
            "{first}, {later}: {stderr}"
        );
        assert!(
            !stderr.contains("Cause:"),
            "pure interruptions stay concise: {stderr}"
        );
        let calls = fixture.calls();
        assert_eq!(calls.len(), 3, "{first}, {later}: {calls:?}");
        assert_eq!(calls.last().expect("failure-state read")["call"], "list");
        assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
    }
}

#[test]
fn failed_mutation_reports_observed_profile_state_and_original_cause() {
    for (state, args, evidence) in [
        (
            "unchanged",
            ACTIONS[1],
            "Failed: remove did not complete.\nInstalled entries unchanged.\n",
        ),
        (
            "changed",
            ACTIONS[1],
            "Partial: remove changed the profile before it stopped.\nProfile: 0 added, 1 removed, 0 updated.\nNext: run `pkg list` to inspect the active profile.\n",
        ),
        (
            "unreadable",
            ACTIONS[1],
            "Failed: remove did not complete.\nPackage state is unknown. Next: run `pkg doctor`.\n",
        ),
        (
            "without-before",
            ACTIONS[4],
            "Failed: prune did not complete.\nProfile checked: 0 entries are active.\n",
        ),
    ] {
        let fixture = Fixture::new();
        let failure = format!(
            "{}print('error: fixture mutation failed',file=sys.stderr)\n sys.exit(1)",
            if state == "unchanged" {
                "(home/'changed').unlink()\n "
            } else {
                ""
            }
        );
        let mut runtime = RUNTIME.replace(
            "if os.environ['PKG_ORDER_STOP']=='mutation': interrupt()",
            &failure,
        );
        if state == "unreadable" {
            runtime = runtime.replace(
                "elif action=='list':",
                "elif action=='list' and changed:\n print('error: fixture profile unreadable',file=sys.stderr)\n sys.exit(1)\nelif action=='list':",
            );
        }
        fs::write(fixture.home.path().join("nix"), runtime).expect("failing runtime");
        let output = fixture.run(args, "none", "TERM");
        assert_eq!(output.status.code(), Some(1), "{state}: {output:?}");
        assert!(output.stdout.is_empty(), "no completion: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(evidence), "{state}: {stderr}");
        let cause = if args[0] == "prune" {
            "Cause: native command `nix profile wipe-history"
        } else {
            "Cause: fixture mutation failed"
        };
        assert!(
            stderr.contains(cause) && stderr.contains("fixture mutation failed"),
            "original mutation cause: {stderr}"
        );
        let calls = fixture.calls();
        let expected_calls: &[&str] = if args[0] == "prune" {
            &["history", "list", "wipe-history", "list"]
        } else {
            &["list", "remove", "list"]
        };
        assert_eq!(
            calls
                .iter()
                .map(|call| call["call"].as_str().expect("native call name"))
                .collect::<Vec<_>>(),
            expected_calls,
            "{state}: {calls:?}"
        );
        assert_eq!(calls.last().expect("postfailure read")["call"], "list");
        assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
    }
}

#[test]
fn fixed_only_upgrades_keep_the_profile_without_native_mutation() {
    for (args, message) in [
        (
            &["upgrade", "opaque", "--entry"][..],
            "Nothing to upgrade. The selected packages use a fixed source revision.",
        ),
        (&["upgrade", "--all"][..], "Nothing to upgrade."),
    ] {
        let fixture = Fixture::new();
        let pinned = RUNTIME.replace(
            "'originalUrl':'github:NixOS/nixpkgs'",
            "'originalUrl':'github:NixOS/nixpkgs/1111111111111111111111111111111111111111'",
        );
        fs::write(fixture.home.path().join("nix"), pinned).expect("fixed profile runtime");
        let output = fixture.run(args, "none", "TERM");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Skipped: probe (fixed source revision)."),
            "{stdout}"
        );
        assert!(stdout.contains(message), "{stdout}");
        assert!(!fixture.home.path().join("changed").exists());
        let calls = fixture.calls();
        assert_eq!(calls.len(), 1, "only the locked preflight read may run");
        assert_eq!(calls[0]["call"], "list");
        assert_eq!(calls[0]["locked"], true);
    }
}

#[test]
fn malformed_registry_blocks_moving_and_fixed_upgrades_before_skip_output() {
    for fixed in [false, true] {
        for args in [
            &["upgrade", "opaque", "--entry"][..],
            &["upgrade", "--all"][..],
        ] {
            let fixture = Fixture::new();
            if fixed {
                fs::write(fixture.home.path().join("nix"), RUNTIME.replace(
                    "'originalUrl':'github:NixOS/nixpkgs'",
                    "'originalUrl':'github:NixOS/nixpkgs/1111111111111111111111111111111111111111'",
                )).expect("fixed profile runtime");
            }
            let taps = fixture.home.path().join("state/pkg/taps");
            fs::create_dir_all(&taps).expect("tap namespace");
            fs::write(taps.join("registry.json"), "{ broken").expect("malformed registry");
            let output = fixture.run(args, "none", "TERM");
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("registry"), "{stderr}");
            assert!(!stderr.contains("Upgrading:"), "{stderr}");
            assert!(output.stdout.is_empty(), "no skip output: {output:?}");
            assert!(!fixture.home.path().join("changed").exists());
            let calls = fixture.calls();
            assert_eq!(calls.len(), 1, "only the locked preflight read may run");
            assert_eq!(calls[0]["call"], "list");
            assert_eq!(calls[0]["locked"], true);
        }
    }
}

#[test]
fn mutation_cancellation_preserves_each_signal_and_profile_evidence() {
    for args in ACTIONS {
        for (signal, code) in [("INT", 130), ("TERM", 143), ("HUP", 129)] {
            let fixture = Fixture::new();
            let output = fixture.run(args, "mutation", signal);
            assert_eq!(
                output.status.code(),
                Some(code),
                "{args:?} {signal}: {output:?}"
            );
            assert!(
                fixture.home.path().join("changed").exists(),
                "mutation reached"
            );
            assert!(output.stdout.is_empty(), "no completion: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("Interrupted:") || stderr.contains("Partial:"),
                "{stderr}"
            );
            if args[0] == "prune" {
                assert!(
                    stderr.contains(
                        "Interrupted: prune stopped.\nProfile checked: 0 entries are active.\n"
                    ),
                    "{stderr}"
                );
            } else if args[0] != "rollback" {
                assert!(
                    stderr.contains(&format!(
                        "Interrupted: {} stopped after the profile changed.\nProfile: 0 added, 1 removed, 0 updated.\n",
                        args[0]
                    )),
                    "{stderr}"
                );
            }
            let calls = fixture.calls();
            assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
            if args[0] == "rollback" {
                assert_eq!(
                    fs::read_link(fixture.home.path().join("state/nix/profiles/pkg"))
                        .expect("active generation"),
                    std::path::Path::new("pkg-2-link")
                );
            } else {
                assert_eq!(calls.last().expect("last native call")["call"], "list");
            }
        }
    }
}

#[test]
fn preflight_cancellation_prevents_mutation() {
    for args in &ACTIONS[..4] {
        let fixture = Fixture::new();
        let output = fixture.run(args, "before", "TERM");
        assert_eq!(output.status.code(), Some(143), "{args:?}: {output:?}");
        assert!(!fixture.home.path().join("changed").exists());
        let calls = fixture.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["call"], "list");
        assert_eq!(calls[0]["locked"], true);
    }
}

#[test]
fn postflight_cancellation_reports_committed_change_and_stops_refresh() {
    for args in ACTIONS {
        let fixture = Fixture::new();
        let output = fixture.run(args, "after", "HUP");
        assert_eq!(output.status.code(), Some(129), "{args:?}: {output:?}");
        assert!(fixture.home.path().join("changed").exists());
        assert!(output.stdout.is_empty(), "no completion: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Partial:"),
            "committed native change: {stderr}"
        );
        let recovery = if args[0] == "prune" {
            "pkg history"
        } else {
            "pkg list"
        };
        assert!(stderr.contains(recovery), "recovery: {stderr}");
        let calls = fixture.calls();
        let observation = if args[0] == "prune" {
            "history"
        } else {
            "list"
        };
        assert_eq!(calls.last().expect("last native call")["call"], observation);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["call"] == observation && call["changed"] == true)
                .count(),
            1,
            "canceled postflight is the final native read: {calls:?}"
        );
        assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
    }
}

#[test]
fn success_holds_one_profile_lock_through_ordered_native_reads() {
    for args in ACTIONS {
        let fixture = Fixture::new();
        let output = fixture.run(args, "none", "INT");
        assert!(output.status.success(), "{args:?}: {output:?}");
        let calls = fixture.calls();
        assert!(calls.iter().all(|call| call["locked"] == true), "{calls:?}");
        let operation = match args[0] {
            "install" => "add",
            "prune" => "wipe-history",
            other => other,
        };
        let mutation = calls
            .iter()
            .position(|call| call["call"] == operation)
            .expect("native mutation");
        assert!(mutation > 0, "preflight precedes mutation: {calls:?}");
        assert!(
            mutation + 1 < calls.len(),
            "postflight follows mutation: {calls:?}"
        );
        if args[0] == "prune" {
            assert!(
                calls[mutation + 1..]
                    .iter()
                    .all(|call| call["call"] != "list"),
                "prune skips apps: {calls:?}"
            );
        } else {
            assert_eq!(calls[mutation + 1]["call"], "list", "{calls:?}");
            assert_eq!(
                calls[mutation + 1..]
                    .iter()
                    .filter(|call| call["call"] == "list")
                    .count(),
                1,
                "launcher refresh must reuse the locked postflight read: {calls:?}"
            );
        }
    }
}
