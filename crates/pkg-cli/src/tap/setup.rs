//! One-time, consent-gated setup for the tap-import sandbox gate.
//!
//! The importer refuses to run tap Ruby until the Nix daemon is proven
//! sandboxed. The daemon is a privileged, machine-wide service, so pkg
//! never changes it silently: this module prints the exact administrator
//! steps, asks once on the terminal, runs each step through sudo with one
//! argument vector (never a shell string), and verifies the result from
//! this account. A refusal runs nothing. The fresh in-build probe inside
//! the next import stays the authoritative check either way.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::consent::stdin_is_terminal;
use crate::nix::Nix;

/// The stable prefix of the importer's sandbox refusal.
pub(crate) const REFUSAL_SIGNATURE: &str = "effective local nix config refuses the raw tap import";

/// The private build temp directory the daemon must hand to builds.
const BUILDS_DIR: &str = "/nix/var/nix/builds";
/// Determinate supported extension file for daemon settings.
const CUSTOM_CONF: &str = "/etc/nix/nix.custom.conf";
/// The generated global config; the client view needs it world-readable.
const NIX_CONF: &str = "/etc/nix/nix.conf";
/// The Determinate daemon LaunchDaemon on macOS.
const DETERMINATE_PLIST: &str = "/Library/LaunchDaemons/systems.determinate.nix-daemon.plist";

/// Whether one error message is the importer's sandbox refusal.
pub(crate) fn is_sandbox_refusal(message: &str) -> bool {
    message.contains(REFUSAL_SIGNATURE)
}

/// How one planned step runs.
#[derive(Debug, PartialEq, Eq)]
enum StepKind {
    /// Run through sudo; any nonzero exit fails the setup.
    Apply,
    /// A sudo grep for secret settings; exit 1 (no match) is success and
    /// exit 0 (a match) aborts the whole setup for manual review.
    SecretsProbe,
}

/// One administrator step, printed to the user before anything runs.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    kind: StepKind,
    summary: String,
    argv: Vec<String>,
    stdin: Option<String>,
}

/// What the host looks like right now; every probe is read-only.
#[derive(Debug, Default, PartialEq, Eq)]
struct World {
    builds_dir_ready: bool,
    custom_conf_has_settings: bool,
    nix_conf_unreadable: bool,
    determinate_plist: Option<PathBuf>,
}

/// Whether one metadata record grants read to other users.
#[cfg(unix)]
fn world_readable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o004 != 0)
        .unwrap_or(true)
}

#[cfg(not(unix))]
fn world_readable(_path: &Path) -> bool {
    true
}

/// Inspect the host with read-only probes only.
fn inspect() -> World {
    let custom = std::fs::read_to_string(CUSTOM_CONF).unwrap_or_default();
    World {
        builds_dir_ready: Path::new(BUILDS_DIR).is_dir(),
        custom_conf_has_settings: custom.contains("sandbox = true")
            && custom.contains("sandbox-fallback = false"),
        nix_conf_unreadable: Path::new(NIX_CONF).exists() && !world_readable(Path::new(NIX_CONF)),
        determinate_plist: Path::new(DETERMINATE_PLIST)
            .exists()
            .then(|| PathBuf::from(DETERMINATE_PLIST)),
    }
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

/// Build one Step from string parts for the two special cases.
fn custom_step(kind: StepKind, summary: &str, parts: &[String], stdin: Option<String>) -> Step {
    Step {
        kind,
        summary: String::from(summary),
        argv: parts.to_vec(),
        stdin,
    }
}

fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// The exact administrator steps this host still needs, in run order.
fn plan(world: &World) -> Vec<Step> {
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
    if !world.custom_conf_has_settings {
        let parts = owned(&["sudo", "tee", "-a", CUSTOM_CONF]);
        steps.push(custom_step(
            StepKind::Apply,
            "append sandbox = true and sandbox-fallback = false to /etc/nix/nix.custom.conf",
            &parts,
            Some(String::from("sandbox = true\nsandbox-fallback = false\n")),
        ));
    }
    if world.nix_conf_unreadable {
        steps.push(custom_step(
            StepKind::SecretsProbe,
            "check /etc/nix/nix.conf for secret settings before making it readable",
            &owned(&["sudo", "grep", "-n", "access-tokens", NIX_CONF]),
            None,
        ));
        steps.push(step(
            "make the global nix config readable by every account (chmod 0644)",
            &["sudo", "chmod", "0644", NIX_CONF],
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
    steps
}

/// Run one step; stdio stays inherited so sudo can ask on the terminal.
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
        let _ = stdin.write_all(text.as_bytes());
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
        println!("pkg: sandbox settings verified from this account");
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
/// manual path instead. Every command is one fixed argument vector.
pub(crate) fn offer_and_apply(nix: &Nix) -> Result<bool, String> {
    if !stdin_is_terminal() {
        println!(
            "pkg: rerun this command from a terminal to be offered the \
             one-time sandbox setup"
        );
        return Ok(false);
    }
    let world = inspect();
    let steps = plan(&world);
    if steps.is_empty() {
        return verify(nix);
    }
    println!("pkg can apply these one-time administrator changes with sudo:");
    for step in &steps {
        println!("  - {}", step.summary);
    }
    if world.determinate_plist.is_none() {
        println!(
            "  - no Determinate daemon found; restart the Nix daemon yourself \
             if the settings changed"
        );
    }
    print!("apply now? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer);
    let approved = read.is_ok()
        && (answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes"));
    if !approved {
        println!("not approved; no administrator change was made");
        return Ok(false);
    }
    for step in &steps {
        let ok = run_step(step)?;
        match step.kind {
            StepKind::Apply => {
                if !ok {
                    return Err(format!("this step failed: {}", step.summary));
                }
            }
            StepKind::SecretsProbe if ok => {
                return Err(String::from(
                    "/etc/nix/nix.conf contains access-tokens; review it by hand \
                     before making it readable",
                ));
            }
            StepKind::SecretsProbe => {}
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
        detail.push_str(
            "; /etc/nix/nix.conf is unreadable here, so the client view cannot see \
             the daemon settings (chmod 0644 restores it)",
        );
    }
    (String::from("blocked"), detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unconfigured() -> World {
        World {
            builds_dir_ready: false,
            custom_conf_has_settings: false,
            nix_conf_unreadable: true,
            determinate_plist: Some(PathBuf::from(DETERMINATE_PLIST)),
        }
    }

    #[test]
    fn detects_the_refusal_signature_only() {
        let refusal = "importing x failed: effective local nix config refuses the raw tap import: sandbox is false";
        assert!(is_sandbox_refusal(refusal));
        assert!(!is_sandbox_refusal("importing x failed: network down"));
    }

    #[test]
    fn plans_every_step_on_an_unconfigured_host() {
        let steps = plan(&unconfigured());
        assert_eq!(steps.len(), 8, "3 dir + 1 conf + 2 chmod path + 2 daemon");
        assert_eq!(steps[3].argv[1..4], ["tee", "-a", CUSTOM_CONF]);
        assert_eq!(steps[4].kind, StepKind::SecretsProbe);
        assert_eq!(
            steps[7].argv,
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
            custom_conf_has_settings: true,
            nix_conf_unreadable: false,
            determinate_plist: None,
        };
        assert!(plan(&world).is_empty());
    }

    #[test]
    fn every_step_is_one_sudo_argument_vector() {
        for step in plan(&unconfigured()) {
            assert_eq!(step.argv[0], "sudo", "{}", step.summary);
            assert!(
                !step.argv.iter().any(|arg| arg.contains(' ')),
                "arguments stay one per element: {:?}",
                step.argv
            );
        }
    }
}
