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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::consent::stdin_is_terminal;
use crate::nix::Nix;

/// The private build temp directory the daemon must hand to builds.
const BUILDS_DIR: &str = "/nix/var/nix/builds";
/// Determinate supported extension file for daemon settings.
const CUSTOM_CONF: &str = "/etc/nix/nix.custom.conf";
/// The generated global config; the client view needs it world-readable.
const NIX_CONF: &str = "/etc/nix/nix.conf";
/// The Determinate daemon LaunchDaemon on macOS.
const DETERMINATE_PLIST: &str = "/Library/LaunchDaemons/systems.determinate.nix-daemon.plist";

/// How one planned step runs.
#[derive(Debug, PartialEq, Eq)]
enum StepKind {
    /// Run through sudo; any nonzero exit fails the setup.
    Apply,
}

/// One administrator step, printed to the user before anything runs.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    kind: StepKind,
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
    /// Present but this account cannot stat or read it. Automatic
    /// administrator setup is refused: the file may hold secret
    /// settings, and pkg will not widen access to content it cannot
    /// inspect.
    Unreadable,
}

/// What the host looks like right now; every probe is read-only.
#[derive(Debug, Default, PartialEq, Eq)]
struct World {
    builds_dir_ready: bool,
    /// Whether the custom settings file exists and carries the
    /// fail-closed settings. `None` means the file is absent.
    custom_conf_settings: Option<bool>,
    nix_conf: GlobalConf,
    determinate_plist: Option<PathBuf>,
}

/// Whether one metadata record grants read to other users.
///
/// An unreadable or unstattable metadata record is NOT world-readable:
/// this account cannot prove readability, so the caller fails closed.
#[cfg(unix)]
fn world_readable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o004 != 0)
}

#[cfg(not(unix))]
fn world_readable(_path: &Path) -> bool {
    std::fs::metadata(_path).is_ok()
}

/// How one settings file looks: missing, readable (with its content), or
/// unreadable/unusable with the cause preserved.
///
/// The cause matters because callers decide control flow from this
/// inspection: a missing file plans the append step, and a file that
/// cannot be inspected must never become "append anyway".
#[derive(Debug, PartialEq, Eq)]
enum ConfFile {
    /// The file does not exist.
    Missing,
    /// The file exists and was read; whether it carries the settings.
    Read { has_settings: bool },
    /// The file exists but could not be read (permission denied, or any
    /// other error with the exact cause).
    Unreadable(String),
    /// The file exists but is not valid UTF-8; it cannot be inspected.
    Malformed(String),
}

/// Read one settings file, distinguishing missing, unreadable, and
/// malformed with the cause preserved.
fn read_conf_file(path: &Path) -> ConfFile {
    match std::fs::read_to_string(path) {
        Ok(text) => ConfFile::Read {
            has_settings: text.contains("sandbox = true")
                && text.contains("sandbox-fallback = false"),
        },
        Err(error) => match error.kind() {
            std::io::ErrorKind::NotFound => ConfFile::Missing,
            std::io::ErrorKind::PermissionDenied => {
                ConfFile::Unreadable(format!("permission denied: {error}"))
            }
            std::io::ErrorKind::InvalidData => {
                ConfFile::Malformed(format!("not valid UTF-8 text: {error}"))
            }
            _ => ConfFile::Unreadable(format!("{path:?}: {error}")),
        },
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
    let custom_conf_settings = match read_conf_file(custom_conf) {
        ConfFile::Missing => None,
        ConfFile::Read { has_settings } => Some(has_settings),
        ConfFile::Unreadable(cause) | ConfFile::Malformed(cause) => {
            return Err(format!(
                "cannot inspect {}: {cause}; an administrator must review \
                 it by hand before the sandbox setup runs",
                custom_conf.display()
            ));
        }
    };
    let nix_conf = if !nix_conf.exists() {
        GlobalConf::Missing
    } else if world_readable(nix_conf) {
        GlobalConf::Readable
    } else {
        GlobalConf::Unreadable
    };
    Ok(World {
        builds_dir_ready: Path::new(BUILDS_DIR).is_dir(),
        custom_conf_settings,
        nix_conf,
        determinate_plist: Path::new(DETERMINATE_PLIST)
            .exists()
            .then(|| PathBuf::from(DETERMINATE_PLIST)),
    })
}

/// One sudo step with a fixed argument vector.
fn step(summary: &str, argv: &[&str]) -> Step {
    Step {
        kind: StepKind::Apply,
        summary: String::from(summary),
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        stdin: None,
    }
}

/// Build one Step from string parts for the stdin-fed case.
fn custom_step(summary: &str, parts: &[String], stdin: Option<String>) -> Step {
    Step {
        kind: StepKind::Apply,
        summary: String::from(summary),
        argv: parts.to_vec(),
        stdin,
    }
}

fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
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
        "{nix_conf} exists but this account cannot read it, so pkg will not \
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
    if world.custom_conf_settings != Some(true) {
        let parts = owned(&["sudo", "tee", "-a", CUSTOM_CONF]);
        steps.push(custom_step(
            "append sandbox = true and sandbox-fallback = false to /etc/nix/nix.custom.conf",
            &parts,
            Some(String::from("sandbox = true\nsandbox-fallback = false\n")),
        ));
    }
    if let Some(plist) = &world.determinate_plist {
        let plist_text = plist.display().to_string();
        steps.push(step(
            "point the daemon TMPDIR at /nix/var/nix/builds (sudo plutil)",
            &[
                "sudo",
                "plutil",
                "-replace",
                "EnvironmentVariables",
                "-json",
                "{\"TMPDIR\":\"/nix/var/nix/builds\"}",
                &plist_text,
            ],
        ));
        steps.push(step(
            "restart the Nix daemon (sudo launchctl kickstart)",
            &[
                "sudo",
                "launchctl",
                "kickstart",
                "-k",
                "system/systems.determinate.nix-daemon",
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
fn run_step(step: &Step) -> Result<bool, String> {
    let mut command = Command::new(&step.argv[0]);
    command.args(&step.argv[1..]);
    if step.stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run {}: {error}", step.argv.join(" ")))?;
    if let Some(text) = &step.stdin
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin
            .write_all(text.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("could not feed input to {}: {error}", step.argv.join(" ")))?;
    }
    let status = child
        .wait()
        .map_err(|error| format!("waiting for {} failed: {error}", step.argv.join(" ")))?;
    Ok(status.success())
}

/// Verify the sandbox settings as seen from this account.
fn verify(nix: &Nix) -> Result<bool, String> {
    let sandbox = nix.setting("sandbox").unwrap_or_default();
    let fallback = nix.setting("sandbox-fallback").unwrap_or_default();
    if sandbox == "true" && fallback == "false" {
        println!("pkg: sandbox settings verified");
        return Ok(true);
    }
    println!(
        "pkg: this account still reports sandbox={sandbox:?} and sandbox-fallback={fallback:?}; \
         the in-build probe stays authoritative"
    );
    Ok(false)
}

/// Print the cause and the exact steps, ask once, apply, and verify.
///
/// Without a terminal nothing runs: the caller reports the concise
/// manual path instead. Every command is one fixed argument vector. An
/// uninspectable host (unreadable or malformed settings file) or an
/// unreadable global config runs nothing.
pub(crate) fn offer_and_apply(nix: &Nix) -> Result<bool, String> {
    if !stdin_is_terminal() {
        println!("pkg: run from a terminal to apply the one-time sandbox setup");
        return Ok(false);
    }
    let world = inspect()?;
    match plan(&world) {
        PlanOutcome::ManualReview(instruction) => {
            println!("pkg: {instruction}");
            Ok(false)
        }
        PlanOutcome::Steps(steps) => apply(nix, &steps),
    }
}

/// Ask once, run the steps, and verify.
fn apply(nix: &Nix, steps: &[Step]) -> Result<bool, String> {
    if steps.is_empty() {
        return verify(nix);
    }
    println!(
        "pkg can apply {} one-time sudo fixes for the Nix sandbox",
        steps.len()
    );
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
        let ok = run_step(step)?;
        match step.kind {
            StepKind::Apply if !ok => {
                return Err(format!("this step failed: {}", step.summary));
            }
            StepKind::Apply => {}
        }
    }
    verify(nix)
}

/// One doctor observation for the tap sandbox gate; read-only.
pub(crate) fn doctor_status(nix: &Nix) -> (String, String) {
    let sandbox = nix.setting("sandbox").unwrap_or_default();
    let fallback = nix.setting("sandbox-fallback").unwrap_or_default();
    if sandbox == "true" && fallback == "false" {
        return (
            String::from("ok"),
            String::from("tap imports may run; the in-build probe stays authoritative"),
        );
    }
    let mut detail =
        format!("this account reports sandbox={sandbox:?} and sandbox-fallback={fallback:?}");
    if Path::new(NIX_CONF).exists() && !world_readable(Path::new(NIX_CONF)) {
        detail.push_str(&format!(
            "; {} is unreadable here, so the client view cannot see the \
             daemon settings; an administrator must review it before it is \
             made readable ({})",
            NIX_CONF,
            manual_review(NIX_CONF)
        ));
    }
    (String::from("blocked"), detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unconfigured() -> World {
        World {
            builds_dir_ready: false,
            custom_conf_settings: None,
            nix_conf: GlobalConf::Readable,
            determinate_plist: Some(PathBuf::from(DETERMINATE_PLIST)),
        }
    }

    #[test]
    fn plans_every_step_on_an_unconfigured_host() {
        let PlanOutcome::Steps(steps) = plan(&unconfigured()) else {
            panic!("an inspectable host plans steps");
        };
        assert_eq!(steps.len(), 6, "3 dir + 1 conf + 2 daemon");
        assert_eq!(steps[3].argv[1..4], ["tee", "-a", CUSTOM_CONF]);
        assert_eq!(
            steps[5].argv,
            [
                "sudo",
                "launchctl",
                "kickstart",
                "-k",
                "system/systems.determinate.nix-daemon",
            ]
        );
    }

    #[test]
    fn plans_nothing_on_a_ready_host() {
        let world = World {
            builds_dir_ready: true,
            custom_conf_settings: Some(true),
            nix_conf: GlobalConf::Readable,
            determinate_plist: None,
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
            custom_conf_settings: None,
            nix_conf: GlobalConf::Unreadable,
            determinate_plist: Some(PathBuf::from(DETERMINATE_PLIST)),
        };
        let PlanOutcome::ManualReview(instruction) = plan(&world) else {
            panic!("an unreadable global config refuses automatic setup");
        };
        assert!(instruction.contains("cannot read it"), "{instruction}");
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

        // Missing files inspect cleanly: no settings, no global config.
        let world = inspect_at(&custom, &nix_conf).expect("missing files inspect");
        assert_eq!(world.custom_conf_settings, None);
        assert_eq!(world.nix_conf, GlobalConf::Missing);

        // A readable custom config reports its settings honestly.
        std::fs::write(&custom, "sandbox = true\nsandbox-fallback = false\n")
            .expect("write settings");
        std::fs::write(&nix_conf, "sandbox = true\n").expect("write global");
        let world = inspect_at(&custom, &nix_conf).expect("readable files inspect");
        assert_eq!(world.custom_conf_settings, Some(true));
        assert_eq!(world.nix_conf, GlobalConf::Readable);
        std::fs::write(&custom, "build-users-group = nixbld\n").expect("write other settings");
        let world = inspect_at(&custom, &nix_conf).expect("readable files inspect");
        assert_eq!(world.custom_conf_settings, Some(false));

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
        let payload = format!("{}\n", "x".repeat(256 * 1024));
        let step = custom_step(
            "feed a child that refuses input",
            &owned(&["/bin/sh", "-c", "exec 0<&-; sleep 2"]),
            Some(payload),
        );
        let error = run_step(&step).expect_err("a closed stdin must fail the step");
        assert!(error.contains("could not feed input"), "{error}");
    }
}
