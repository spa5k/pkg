//! One-time, consent-gated setup for the tap-import sandbox gate.
//!
//! The importer refuses to run tap Ruby until the Nix daemon is proven
//! sandboxed. The daemon is a privileged, machine-wide service, so pkg
//! never changes it silently: this module prints the exact administrator
//! steps, asks once on the terminal, runs each step through sudo with one
//! argument vector (never a shell string), and verifies the result from
//! this account. A refusal runs nothing. The fresh in-build probe inside
//! the next import stays the authoritative check either way.
//!
//! Fail-closed rule: administrator steps run only when their
//! preconditions are inspectable from this account. A global
//! `/etc/nix/nix.conf` that exists but is not world-readable stops ALL
//! automatic steps: pkg cannot prove what the file holds (it may carry
//! `access-tokens` or other secrets), so the only honest answer is the
//! manual review instruction. No probe, no grep, no automatic chmod.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::Command;

use super::consent::stdin_is_terminal;
use crate::nix::Nix;

/// A sandbox setup failure retained until command reporting.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Native(#[from] crate::nix::NixError),
    #[error("could not {operation}: {source}")]
    Process {
        operation: String,
        #[source]
        source: pkg_process::RunError,
    },
    #[error("{operation} was interrupted by signal {signal}")]
    Interrupted { operation: String, signal: i32 },
}

impl From<String> for SetupError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for SetupError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

impl SetupError {
    pub(crate) fn signal(&self) -> Option<i32> {
        match self {
            Self::Native(error) => error.signal(),
            Self::Process { source, .. } => source.cancelled,
            Self::Interrupted { signal, .. } => Some(*signal),
            Self::Message(_) => None,
        }
    }
}

/// The private build temp directory the daemon must hand to builds.
const BUILDS_DIR: &str = "/nix/var/nix/builds";
/// Determinate supported extension file for daemon settings.
const CUSTOM_CONF: &str = "/etc/nix/nix.custom.conf";
/// The generated global config; the client view needs it world-readable.
const NIX_CONF: &str = "/etc/nix/nix.conf";
const DETERMINATE_PLIST: &str = "/Library/LaunchDaemons/systems.determinate.nix-daemon.plist";

/// One administrator step, printed to the user before anything runs.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    summary: String,
    argv: Vec<String>,
    stdin: Option<String>,
}

/// How the global config looks from this account.
///
/// Only metadata is consulted; the file's contents are never read,
/// piped through grep, or echoed here.
#[derive(Debug, Default, PartialEq, Eq)]
enum GlobalConf {
    /// No global config exists on this host.
    #[default]
    Missing,
    /// Present and readable by every account.
    Readable,
    /// Present but not readable by every account. Automatic setup is
    /// refused because the file may hold secret settings.
    Unreadable,
}

/// What the host looks like right now; every probe is read-only.
#[derive(Debug, Default, PartialEq, Eq)]
struct World {
    builds_dir_ready: bool,
    /// Whether the observed effective settings enable the sandbox and disable fallback.
    sandbox_settings_ready: bool,
    nix_conf: GlobalConf,
    determinate_plist_present: bool,
    grant_settings: Option<String>,
    /// None means no environment dictionary; Some records whether it has TMPDIR.
    environment_tmpdir: Option<bool>,
}

/// Inspect the global config without reading its contents.
fn inspect_global_conf(path: &Path) -> Result<GlobalConf, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GlobalConf::Missing);
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect {}: {error}; an administrator must review it before setup runs",
                path.display()
            ));
        }
    };
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a file; an administrator must review it before setup runs",
            path.display()
        ));
    }
    let readable = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o004 != 0
    };
    Ok(if readable {
        GlobalConf::Readable
    } else {
        GlobalConf::Unreadable
    })
}

/// Check custom-config readability and UTF-8, preserving the refusal cause.
fn check_conf_file(path: &Path) -> Result<(), String> {
    match std::fs::read_to_string(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(match error.kind() {
            std::io::ErrorKind::PermissionDenied => format!("permission denied: {error}"),
            std::io::ErrorKind::InvalidData => format!("not valid UTF-8 text: {error}"),
            _ => format!("{}: {error}", path.display()),
        }),
    }
}

/// Inspect the host with read-only probes only.
///
/// An unreadable or malformed custom settings file is a setup failure
/// with the cause preserved, never a silent "no settings found" that
/// would plan an append into a file this run could not inspect.
fn inspect() -> Result<World, String> {
    inspect_at(Path::new(CUSTOM_CONF), Path::new(NIX_CONF))
}

/// [`inspect`] against explicit paths, so the distinctions are testable.
fn inspect_at(custom_conf: &Path, nix_conf: &Path) -> Result<World, String> {
    check_conf_file(custom_conf).map_err(|cause| format!(
        "cannot inspect {}: {cause}; an administrator must review it by hand before the sandbox setup runs",
        custom_conf.display()
    ))?;
    let nix_conf = inspect_global_conf(nix_conf)?;
    Ok(World {
        builds_dir_ready: Path::new(BUILDS_DIR).is_dir(),
        sandbox_settings_ready: false,
        nix_conf,
        determinate_plist_present: Path::new(DETERMINATE_PLIST).exists(),
        grant_settings: None,
        environment_tmpdir: None,
    })
}

/// One sudo step with a fixed argument vector.
fn step(summary: &str, argv: &[&str]) -> Step {
    Step {
        summary: String::from(summary),
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        stdin: None,
    }
}

impl Step {
    fn with_input(mut self, stdin: String) -> Self {
        self.stdin = Some(stdin);
        self
    }
}

/// What the planned setup does with this host.
#[derive(Debug, PartialEq, Eq)]
enum PlanOutcome {
    /// The exact administrator steps this host still needs, in run order.
    Steps(Vec<Step>),
    /// Automatic setup is refused; the text is the manual review
    /// instruction. Nothing runs.
    ManualReview(String),
}

/// The manual review instruction for an uninspectable global config.
fn manual_review(nix_conf: &str) -> String {
    format!(
        "{nix_conf} is not readable by every account, so pkg will not \
         run administrator steps it cannot verify: the file may hold \
         access-tokens or other secret settings. Ask an administrator to \
         review it (sudo cat {nix_conf}), remove any secrets, and only then \
         make it readable by every account (sudo chmod 0644 {nix_conf}); \
         then run the tap command again"
    )
}

/// The exact administrator steps this host still needs, in run order, or
/// the refusal with manual instructions.
///
/// The refusal comes first: when the global config is not world-readable
/// no administrator step of any kind runs — not the build directory, not
/// the custom settings, not the daemon restart.
fn plan(world: &World) -> PlanOutcome {
    if world.nix_conf == GlobalConf::Unreadable {
        return PlanOutcome::ManualReview(manual_review(NIX_CONF));
    }
    let mut steps = Vec::new();
    if !world.builds_dir_ready {
        steps.push(step(
            "create the private build temp directory /nix/var/nix/builds",
            &["sudo", "mkdir", "-p", BUILDS_DIR],
        ));
        steps.push(step(
            "set its owner to root:wheel",
            &["sudo", "chown", "root:wheel", BUILDS_DIR],
        ));
        steps.push(step(
            "set its mode to 0755",
            &["sudo", "chmod", "0755", BUILDS_DIR],
        ));
    }
    if !world.sandbox_settings_ready {
        steps.push(
            step(
                "append sandbox = true and sandbox-fallback = false to /etc/nix/nix.custom.conf",
                &["sudo", "tee", "-a", CUSTOM_CONF],
            )
            .with_input(String::from("sandbox = true\nsandbox-fallback = false\n")),
        );
    }
    if let Some(settings) = &world.grant_settings {
        steps.push(
            step(
                "remove the known global /private/tmp and /private/var/tmp sandbox grants",
                &["sudo", "tee", "-a", CUSTOM_CONF],
            )
            .with_input(settings.clone()),
        );
    }
    if world.determinate_plist_present {
        if world.environment_tmpdir.is_none() {
            steps.push(step(
                "create the daemon environment dictionary",
                &[
                    "sudo",
                    "plutil",
                    "-insert",
                    "EnvironmentVariables",
                    "-json",
                    "{}",
                    DETERMINATE_PLIST,
                ],
            ));
        }
        steps.push(step(
            "point the daemon TMPDIR at /nix/var/nix/builds (sudo plutil)",
            &[
                "sudo",
                "plutil",
                if world.environment_tmpdir == Some(true) {
                    "-replace"
                } else {
                    "-insert"
                },
                "EnvironmentVariables.TMPDIR",
                "-string",
                BUILDS_DIR,
                DETERMINATE_PLIST,
            ],
        ));
        steps.push(step(
            "unload the old daemon service definition",
            &[
                "sudo",
                "launchctl",
                "bootout",
                "system/systems.determinate.nix-daemon",
            ],
        ));
        steps.push(step(
            "load the changed daemon service definition",
            &[
                "sudo",
                "launchctl",
                "bootstrap",
                "system",
                DETERMINATE_PLIST,
            ],
        ));
    }
    PlanOutcome::Steps(steps)
}

/// Run one step; stdio stays inherited so sudo can ask on the terminal.
///
/// A required piped-stdin write is an error when it fails: a child that
/// exits or closes its input before consuming the payload (for example a
/// `tee` that could not open its target) must not be reported as a step
/// whose input was delivered.
fn run_step(step: &Step) -> Result<bool, SetupError> {
    let run = || {
        let mut command = Command::new(&step.argv[0]);
        command.args(&step.argv[1..]);
        pkg_process::run_child(
            command,
            pkg_process::IoMode::Stream,
            None,
            step.stdin.as_deref().map(str::as_bytes),
        )
        .map_err(|source| SetupError::Process {
            operation: format!(
                "{} {}",
                if step.stdin.is_some() {
                    "feed input to or supervise"
                } else {
                    "supervise"
                },
                step.argv.join(" ")
            ),
            source,
        })
    };
    let mut reaped = run()?;
    if step.argv
        == [
            "sudo",
            "launchctl",
            "bootstrap",
            "system",
            DETERMINATE_PLIST,
        ]
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        if reaped.output.status.code() == Some(5) {
            eprintln!("Waiting for launchd to finish removing the old daemon…");
        }
        while reaped.cancelled.is_none()
            && reaped.output.status.code() == Some(5)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
            reaped = run()?;
        }
    }
    match reaped.classify() {
        pkg_process::Outcome::Success => Ok(true),
        pkg_process::Outcome::Failed { .. } => Ok(false),
        pkg_process::Outcome::Interrupted { signal } => Err(SetupError::Interrupted {
            operation: step.argv.join(" "),
            signal,
        }),
    }
}

/// Read one setup observation without losing cancellation.
fn capture_probe(tool: &str, args: &[&str]) -> Result<Option<String>, SetupError> {
    let operation = format!("{tool} {}", args.join(" "));
    let (outcome, text) =
        pkg_process::run_direct_captured(Path::new(tool), args).map_err(|source| {
            SetupError::Process {
                operation: format!("supervise {operation}"),
                source,
            }
        })?;
    match outcome {
        pkg_process::Outcome::Success => Ok(Some(text)),
        pkg_process::Outcome::Failed { .. } => Ok(None),
        pkg_process::Outcome::Interrupted { signal } => {
            Err(SetupError::Interrupted { operation, signal })
        }
    }
}

/// Verify the sandbox settings as seen from this account.
fn readiness(nix: &Nix) -> Result<(), SetupError> {
    let config = nix.effective_config()?;
    cask_catalog::tap::verify_effective_config(&config, std::env::consts::OS)?;
    if cfg!(target_os = "macos") {
        let text = capture_probe(
            "/bin/launchctl",
            &["print", "system/systems.determinate.nix-daemon"],
        )?;
        if text.is_none_or(|text| {
            !text
                .lines()
                .any(|line| line.trim() == "TMPDIR => /nix/var/nix/builds")
        }) {
            return Err(String::from(
                "The live daemon does not report TMPDIR=/nix/var/nix/builds. Reload its service with sudo launchctl bootout system/systems.determinate.nix-daemon, then sudo launchctl bootstrap system /Library/LaunchDaemons/systems.determinate.nix-daemon.plist.",
            ).into());
        }
    }
    Ok(())
}

fn verify(nix: &Nix) -> Result<bool, SetupError> {
    readiness(nix)?;
    println!(
        "pkg: effective grants and live daemon environment verified. The fresh build probe remains required."
    );
    Ok(true)
}

/// Print the cause and the exact steps, ask once, apply, and verify.
///
/// Without a terminal nothing runs: the caller reports the concise
/// manual path instead. Every command is one fixed argument vector. An
/// uninspectable host (unreadable or malformed settings file) or an
/// unreadable global config runs nothing.
pub(crate) fn offer_and_apply(nix: &Nix) -> Result<bool, SetupError> {
    if !stdin_is_terminal() {
        println!("pkg: run from a terminal to apply the one-time sandbox setup");
        return Ok(false);
    }
    let mut world = inspect()?;
    if world.nix_conf != GlobalConf::Unreadable {
        let config = nix.effective_config()?;
        world.sandbox_settings_ready = config
            .pointer("/sandbox/value")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && config
                .pointer("/sandbox-fallback/value")
                .and_then(serde_json::Value::as_bool)
                == Some(false);
        if cfg!(target_os = "macos") {
            world.grant_settings = cask_catalog::tap::mac_setup_settings(&config).map_err(|error| format!("Automatic setup stopped. Review sandbox-paths, extra-sandbox-paths, and allowed-impure-host-deps in the daemon config by hand. {error}"))?;
            if world.determinate_plist_present {
                if let Some(text) = capture_probe(
                    "/usr/bin/plutil",
                    &[
                        "-extract",
                        "EnvironmentVariables",
                        "json",
                        "-o",
                        "-",
                        DETERMINATE_PLIST,
                    ],
                )? {
                    let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                        String::from("Cannot inspect the daemon environment dictionary.")
                    })?;
                    let environment = value
                        .as_object()
                        .ok_or("The daemon environment is not a dictionary.")?;
                    world.environment_tmpdir = Some(environment.contains_key("TMPDIR"));
                }
            } else {
                return Err(String::from(
                    "The Determinate daemon plist was not found. Set its TMPDIR to /nix/var/nix/builds and reload your custom service by hand.",
                ).into());
            }
        }
    }
    match plan(&world) {
        PlanOutcome::ManualReview(instruction) => {
            println!("pkg: {instruction}");
            Ok(false)
        }
        PlanOutcome::Steps(steps) => apply(nix, &steps),
    }
}

/// Ask once, run the steps, and verify.
fn apply(nix: &Nix, steps: &[Step]) -> Result<bool, SetupError> {
    if steps.is_empty() {
        return verify(nix);
    }
    println!(
        "pkg can apply {} one-time sudo fixes for the Nix sandbox",
        steps.len()
    );
    for step in steps {
        println!("  {}", step.argv.join(" "));
        if let Some(input) = &step.stdin {
            println!("  Settings:\n{input}");
        }
    }
    print!("apply now? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer);
    let approved = read.is_ok()
        && (answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes"));
    if !approved {
        println!("not approved");
        return Ok(false);
    }
    for step in steps {
        println!("pkg: {}", step.summary);
        if !run_step(step)? {
            return Err(format!(
                "this step failed: {}. To restore the service, run sudo launchctl bootstrap system {DETERMINATE_PLIST}",
                step.summary
            ).into());
        }
    }
    verify(nix)
}

/// One doctor observation for the tap sandbox gate; read-only.
pub(crate) fn doctor_status(nix: &Nix) -> Result<(String, String), SetupError> {
    Ok(match readiness(nix) {
        Ok(()) => (
            String::from("ok"),
            String::from(
                "effective grants and live daemon environment pass; the fresh build probe remains required",
            ),
        ),
        Err(error) if error.signal().is_some() => return Err(error),
        Err(error) => (String::from("blocked"), error.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unconfigured() -> World {
        World {
            builds_dir_ready: false,
            sandbox_settings_ready: false,
            nix_conf: GlobalConf::Readable,
            determinate_plist_present: true,
            grant_settings: None,
            environment_tmpdir: Some(true),
        }
    }

    #[test]
    fn plans_every_step_on_an_unconfigured_host() {
        let PlanOutcome::Steps(steps) = plan(&unconfigured()) else {
            panic!("an inspectable host plans steps");
        };
        assert_eq!(steps.len(), 7, "3 dir + 1 conf + 3 daemon");
        assert_eq!(steps[3].argv[1..4], ["tee", "-a", CUSTOM_CONF]);
        assert_eq!(
            steps[5].argv,
            [
                "sudo",
                "launchctl",
                "bootout",
                "system/systems.determinate.nix-daemon",
            ]
        );
    }

    #[test]
    fn plans_nothing_on_a_ready_host() {
        let world = World {
            builds_dir_ready: true,
            sandbox_settings_ready: true,
            nix_conf: GlobalConf::Readable,
            determinate_plist_present: false,
            grant_settings: None,
            environment_tmpdir: None,
        };
        assert_eq!(plan(&world), PlanOutcome::Steps(Vec::new()));
    }

    #[test]
    fn every_step_is_one_sudo_argument_vector() {
        let PlanOutcome::Steps(steps) = plan(&unconfigured()) else {
            panic!("an inspectable host plans steps");
        };
        for step in steps {
            assert_eq!(step.argv[0], "sudo", "{}", step.summary);
            assert!(
                !step.argv.iter().any(|arg| arg.contains(' ')),
                "arguments stay one per element: {:?}",
                step.argv
            );
        }
    }

    #[test]
    fn unreadable_global_config_refuses_all_administrator_steps() {
        // The refusal comes before every step kind: not the build
        // directory, not the custom settings append, not the daemon
        // restart. No failure can reach a chmod of the global config
        // because no such step exists at all.
        let world = World {
            builds_dir_ready: false,
            sandbox_settings_ready: false,
            nix_conf: GlobalConf::Unreadable,
            determinate_plist_present: true,
            grant_settings: None,
            environment_tmpdir: Some(true),
        };
        let PlanOutcome::ManualReview(instruction) = plan(&world) else {
            panic!("an unreadable global config refuses automatic setup");
        };
        assert!(
            instruction.contains("not readable by every account"),
            "{instruction}"
        );
        assert!(instruction.contains("access-tokens"), "{instruction}");
        assert!(instruction.contains("review"), "{instruction}");
        // The instruction names the manual chmod only after review; the
        // automatic plan never contains one.
        assert!(instruction.contains("chmod 0644"), "{instruction}");
        // A missing global config does not refuse: the steps run.
        let mut missing = world;
        missing.nix_conf = GlobalConf::Missing;
        assert!(matches!(plan(&missing), PlanOutcome::Steps(_)));
    }

    #[test]
    fn inspect_distinguishes_missing_unreadable_and_malformed_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let custom = dir.path().join("nix.custom.conf");
        let nix_conf = dir.path().join("nix.conf");

        // Missing files inspect cleanly; effective settings remain unobserved.
        let world = inspect_at(&custom, &nix_conf).expect("missing files inspect");
        assert!(!world.sandbox_settings_ready);
        assert_eq!(world.nix_conf, GlobalConf::Missing);

        // A readable UTF-8 custom config permits later effective-settings observation.
        std::fs::write(&custom, "sandbox = true\nsandbox-fallback = false\n")
            .expect("write settings");
        std::fs::write(&nix_conf, "sandbox = true\n").expect("write global");
        let world = inspect_at(&custom, &nix_conf).expect("readable files inspect");
        assert!(
            !world.sandbox_settings_ready,
            "file readability does not prove effective settings"
        );
        assert_eq!(world.nix_conf, GlobalConf::Readable);
        std::fs::write(&custom, "build-users-group = nixbld\n").expect("write other settings");
        let world = inspect_at(&custom, &nix_conf).expect("readable files inspect");
        assert!(!world.sandbox_settings_ready);

        // A malformed (non-UTF-8) custom config is refused with its
        // cause: the append decision depends on this inspection.
        std::fs::write(&custom, [0xff, 0xfe, 0x00]).expect("write invalid utf-8");
        let error = inspect_at(&custom, &nix_conf).expect_err("malformed config refuses");
        assert!(error.contains("UTF-8"), "{error}");
        assert!(error.contains("review"), "{error}");

        // An unreadable custom config is refused with its cause; root
        // bypasses mode bits, so that part is only checked unprivileged.
        std::fs::write(&custom, "sandbox = true\n").expect("write settings");
        let root = unsafe { libc::getuid() } == 0;
        #[cfg(unix)]
        if !root {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&custom).expect("metadata").permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&custom, permissions).expect("chmod");
            let error = inspect_at(&custom, &nix_conf).expect_err("unreadable config refuses");
            assert!(error.contains("permission denied"), "{error}");
            assert!(error.contains("review"), "{error}");
            let mut permissions = std::fs::metadata(&custom).expect("metadata").permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&custom, permissions).expect("chmod");
        }

        // An existing but unreadable GLOBAL config inspects to the
        // refusal state (it is a World state, not an error: no automatic
        // step may run, and doctor explains why).
        #[cfg(unix)]
        if !root {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&nix_conf)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&nix_conf, permissions).expect("chmod");
            let world = inspect_at(&custom, &nix_conf).expect("global state is not an error");
            assert_eq!(world.nix_conf, GlobalConf::Unreadable);
            let mut permissions = std::fs::metadata(&nix_conf)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o644);
            std::fs::set_permissions(&nix_conf, permissions).expect("chmod");
        }
    }

    #[test]
    fn required_stdin_write_failure_surfaces() {
        // The child closes its stdin immediately while the payload is far
        // larger than the pipe buffer: the required piped write must fail
        // loudly (EPIPE once the read end is gone), never count as a
        // delivered step.
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child-pid");
        let payload = format!("{}\n", "x".repeat(256 * 1024));
        let step = step(
            "feed a child that refuses input",
            &[
                "/bin/sh",
                "-c",
                "echo $$ > \"$1\"; exec 0<&-; exec sleep 30",
                "test-child",
                pid_file.to_str().expect("pid path"),
            ],
        )
        .with_input(payload);
        let error = run_step(&step).expect_err("a closed stdin must fail the step");
        assert_eq!(
            error.signal(),
            None,
            "a delivery failure is not cancellation"
        );
        let error = error.to_string();
        assert!(error.contains("could not feed input"), "{error}");
        let pid: libc::pid_t = std::fs::read_to_string(pid_file)
            .expect("child pid")
            .trim()
            .parse()
            .expect("pid number");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the failed child must be reaped"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn setup_steps_keep_child_cancellation() {
        use std::os::unix::fs::PermissionsExt as _;
        for body in [
            String::from("exit 130"),
            format!("kill -{} $$", libc::SIGINT),
            format!("kill -{} $$", libc::SIGTERM),
            format!("kill -{} $$", libc::SIGHUP),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let child = dir.path().join("step");
            std::fs::write(&child, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).unwrap();
            let expected = if body == "exit 130" {
                libc::SIGINT
            } else {
                body.split_whitespace()
                    .nth(1)
                    .unwrap()
                    .trim_start_matches('-')
                    .parse()
                    .unwrap()
            };
            for input in [None, Some("x".repeat(5 * 1024 * 1024))] {
                let mut request = step("cancelled setup child", &[child.to_str().unwrap()]);
                request.stdin = input;
                let error = run_step(&request).expect_err("cancelled step");
                assert_eq!(error.signal(), Some(expected));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn setup_readiness_keeps_native_probe_cancellation() {
        use std::os::unix::fs::PermissionsExt as _;
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let dir = tempfile::tempdir().expect("tempdir");
            let executable = dir.path().join("nix");
            std::fs::write(&executable, format!("#!/bin/sh\ncase \"$*\" in *--version*) echo 'nix (Nix) 2.35.2'; exit 0;; esac\nkill -{signal} $$\n")).expect("fake nix");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
            let nix = crate::nix::discover(Some(executable.to_str().expect("nix path")))
                .expect("runtime");
            assert_eq!(
                verify(&nix).expect_err("cancelled setup check").signal(),
                Some(signal)
            );
            assert_eq!(
                doctor_status(&nix)
                    .expect_err("cancelled doctor check")
                    .signal(),
                Some(signal)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_settings_queries_keep_their_cause() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let executable = dir.path().join("nix");
        std::fs::write(&executable, "#!/bin/sh\ncase \"$*\" in *--version*) echo 'nix (Nix) 2.35.2'; exit 0;; esac\necho 'fixture settings query failed' >&2\nexit 1\n").expect("fake nix");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let nix = crate::nix::discover(Some(executable.to_str().expect("nix path")))
            .expect("discover fake nix");
        assert!(
            verify(&nix)
                .expect_err("failed query must not become an empty setting")
                .to_string()
                .contains("fixture settings query failed")
        );
        let (status, detail) = doctor_status(&nix).expect("failed query is a doctor observation");
        assert_eq!(status, "blocked");
        assert!(detail.contains("fixture settings query failed"), "{detail}");
    }

    #[cfg(unix)]
    #[test]
    fn global_config_metadata_failure_stops_inspection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let global = dir.path().join("nix.conf");
        std::os::unix::fs::symlink("nix.conf", &global).expect("symlink loop");
        let error = inspect_at(&dir.path().join("missing.custom.conf"), &global)
            .expect_err("failed metadata must not mean missing");
        assert!(error.contains("cannot inspect"), "{error}");
        assert!(error.contains("administrator must review"), "{error}");
        std::fs::remove_file(&global).expect("remove symlink");
        std::fs::create_dir(&global).expect("directory at config path");
        assert!(
            inspect_at(&dir.path().join("missing.custom.conf"), &global)
                .expect_err("config must be a file")
                .contains("not a file")
        );
    }
}
