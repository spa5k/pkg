//! The concrete native Nix adapter (design D1, D2).
//!
//! This module owns process construction and native output decoding. Callers
//! ask for profile operations and never touch Nix JSON fields. There is no
//! broker protocol, no runtime installation, and no generic backend trait.
//!
//! Output shapes in this module were captured from the verified runtime
//! (Determinate 3.22.1, Nix 2.35.2, aarch64-darwin). Only that baseline is
//! claimed as tested; this adapter enforces the shapes it decodes instead of
//! an invented version floor.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The only natively verified runtime baseline.
///
/// pkg does not enforce an invented minimum version. Compatibility is
/// enforced by decoding the exact native output shapes below; a runtime that
/// speaks a different shape fails with a decode diagnostic, not a guess.
pub const TESTED_BASELINE: &str = "2.35.2";

/// The `nix profile list --json` manifest version this client decodes.
///
/// Verified value on Nix 2.35.2 is `3`; entries changed to `elements` with
/// string `attrPath` and locked `url` fields in that version.
pub const PROFILE_MANIFEST_VERSION: u64 = 3;

/// The cask support manifest schema this client decodes (design D6).
pub const CASK_SUPPORT_SCHEMA: &str = "pkg-cask-support/1";

/// A full commit id length in hex characters.
const FULL_COMMIT_LEN: usize = 40;

/// A native operation failure category with preserved diagnostics.
#[derive(Debug)]
pub enum NixError {
    /// No `nix` executable was found. Includes vendor setup guidance.
    MissingExecutable {
        /// The configured executable name or path.
        requested: String,
    },
    /// A native command could not be started.
    Spawn(String),
    /// A native command failed. Upstream diagnostics are preserved.
    Failed {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The exit status message.
        status: String,
        /// Captured stderr, when available.
        stderr: String,
    },
    /// A native command was terminated by a signal after it started.
    ///
    /// The child was reaped by pkg; the profile may be in an intermediate
    /// state, so callers must re-read native state before reporting.
    Interrupted {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The terminating signal number.
        signal: i32,
    },
    /// Native output could not be decoded into a required shape.
    Decode {
        /// What pkg was decoding.
        what: &'static str,
        /// Why decoding failed.
        detail: String,
    },
}

impl fmt::Display for NixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutable { requested } => write!(
                f,
                "the Nix executable ({requested}) was not found\n\
                 pkg requires an external Determinate Nix installation.\n\
                 Install it from https://docs.determinate.systems/determinate-nix/ \
                 and run this command again.\n\
                 pkg does not install, update, or repair the Nix runtime."
            ),
            Self::Spawn(detail) => write!(f, "could not start the Nix command: {detail}"),
            Self::Failed {
                args,
                status,
                stderr,
            } => {
                write!(
                    f,
                    "native command `nix {}` failed ({status})",
                    args.join(" ")
                )?;
                if !stderr.trim().is_empty() {
                    write!(f, "\n{}", stderr.trim())?;
                }
                Ok(())
            }
            Self::Interrupted { args, signal } => write!(
                f,
                "native command `nix {}` was terminated by signal {signal}; \
                 the operation may have partially applied",
                args.join(" ")
            ),
            Self::Decode { what, detail } => {
                write!(f, "could not decode required {what} from Nix: {detail}")
            }
        }
    }
}

impl std::error::Error for NixError {}

/// One installed profile element decoded from `nix profile list --json`.
///
/// Captured shape on Nix 2.35.2: the manifest is version `3`, the top level
/// object holds `elements`, and each element carries `active`, string
/// `attrPath`, `originalUrl`, `outputs` (nullable), `priority`, `storePaths`,
/// and the locked `url`. Element keys are opaque native entry IDs and differ
/// by platform: Linux uses plain source keys while Determinate macOS uses
/// full `originalUrl#attrPath` strings such as
/// `path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default`.
/// Keys must be passed back to remove and upgrade verbatim.
///
/// Unknown optional JSON fields are ignored; required identity fields must be
/// present or decoding fails clearly (design D3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ProfileEntry {
    /// Whether the element is active in the profile.
    ///
    /// Discovery and app-launcher scans must only consider active entries.
    pub active: bool,
    /// The attribute path of the installed output (a dot-separated string).
    #[serde(rename = "attrPath")]
    pub attr_path: String,
    /// The original (moving) source reference.
    #[serde(rename = "originalUrl")]
    pub original_url: String,
    /// The locked source reference Nix resolved.
    #[serde(rename = "url")]
    pub locked_url: String,
    /// The realized store output paths.
    #[serde(rename = "storePaths")]
    pub store_paths: Vec<String>,
}

impl ProfileEntry {
    /// The final attribute path segment, used for display matching.
    #[must_use]
    pub fn name(&self) -> &str {
        self.attr_path.rsplit('.').next().unwrap_or("?")
    }

    /// The locked revision embedded in the locked URL, if any.
    #[must_use]
    pub fn locked_revision(&self) -> Option<&str> {
        locked_revision(&self.locked_url)
    }
}

/// One search result from `nix search --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchMeta {
    /// The package name.
    pub pname: String,
    /// The package version.
    pub version: String,
    /// The package description.
    #[serde(default)]
    pub description: String,
}

/// Resolved source identity from `nix flake metadata --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    /// The moving reference as configured.
    pub reference: String,
    /// The locked reference Nix resolved, when metadata provides one.
    ///
    /// This is the metadata's top-level `url`, which keeps the transport
    /// and parameters, not the nested `locked.url`, which drops them.
    /// Discovery queries this reference so a result set can never mix
    /// revisions; installation keeps using the moving reference.
    pub locked_url: Option<String>,
    /// The locked revision, when the source provides one.
    pub revision: Option<String>,
    /// A short human description such as `github:NixOS/nixpkgs`.
    pub display: String,
}

/// One source record inside the cask support manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSourceRef {
    /// The source URL the record describes.
    pub url: String,
    /// The locked revision, when the source provides one.
    #[serde(default)]
    pub rev: Option<String>,
}

/// One supported cask record (schema `pkg-cask-support/1`).
///
/// Field types follow the actual generated manifest: `override` is a
/// boolean, and `version`/`homepage` may be null by flake interface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSupported {
    /// The cask token.
    pub token: String,
    /// The package kind, for example `app` or `app+cli`.
    pub kind: String,
    /// The upstream version, when the flake interface provides one.
    pub version: Option<String>,
    /// The upstream homepage, when the flake interface provides one.
    pub homepage: Option<String>,
    /// The artifact kinds the package provides.
    #[serde(rename = "artifactKinds")]
    pub artifact_kinds: Vec<String>,
    /// Whether an override is applied on top of upstream.
    #[serde(rename = "override", default)]
    pub override_applied: bool,
}

/// One excluded cask record with the exclusion reason.
///
/// `version` and `detail` may be null in the actual generated manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskExcluded {
    /// The cask token.
    pub token: String,
    /// The upstream version that was excluded, when known.
    pub version: Option<String>,
    /// A short machine-readable reason.
    pub reason: String,
    /// Human-readable detail for the exclusion, when present.
    #[serde(default)]
    pub detail: Option<String>,
}

/// The cask support manifest exported by the casks flake (design D6).
///
/// Decoded from `caskSupport.<system>` with schema `pkg-cask-support/1`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CaskSupport {
    /// The manifest schema identifier; must equal [`CASK_SUPPORT_SCHEMA`].
    pub schema: String,
    /// The system triple the manifest describes.
    pub system: String,
    /// The provenance records for the sources the manifest was built from.
    pub sources: BTreeMap<String, CaskSourceRef>,
    /// The casks supported on this system.
    pub supported: Vec<CaskSupported>,
    /// The casks excluded on this system, with reasons.
    pub excluded: Vec<CaskExcluded>,
}

/// The resolved native runtime.
#[derive(Debug, Clone)]
pub struct Nix {
    executable: PathBuf,
    version: String,
    verbose: bool,
    no_color: bool,
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Discover the external Nix runtime.
///
/// Resolves the executable to an absolute path once and probes its version
/// for diagnostics. Nothing is installed or repaired here, and no invented
/// version floor is enforced: capability is proven by the decoded shapes.
pub fn discover(configured: Option<&str>) -> Result<Nix, NixError> {
    let requested = configured.unwrap_or("nix");
    let absolute = if Path::new(requested).components().count() > 1 {
        Some(PathBuf::from(requested))
    } else {
        find_in_path(requested)
    };
    let found = absolute
        .and_then(|path| std::fs::canonicalize(&path).ok())
        .filter(|path| path.is_absolute());
    let Some(executable) = found else {
        return Err(NixError::MissingExecutable {
            requested: requested.to_string(),
        });
    };
    let version = Nix {
        executable: executable.clone(),
        version: String::new(),
        verbose: false,
        no_color: false,
    }
    .capture(&["--version"])?;
    // Keep the full native version text (for example `nix (Nix) 2.35.2`);
    // trimming to major.minor would not match the tested baseline.
    Ok(Nix {
        executable,
        version: version.stdout.trim().to_string(),
        verbose: false,
        no_color: false,
    })
}

/// How a native child's stdio is wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoMode {
    /// Capture stdout/stderr through pipes.
    Capture,
    /// Stream stdio to the user's terminal.
    Stream,
}

/// The reaped result of one native child.
enum ChildOutcome {
    /// Captured pipes plus exit status.
    Output(Output),
    /// Exit status only.
    Status(std::process::ExitStatus),
}

/// One reaped child together with its cancellation record.
struct Reaped {
    /// The child's captured or streamed outcome.
    outcome: ChildOutcome,
    /// A termination signal pkg received and forwarded to the child during
    /// the run, when one was.
    ///
    /// It is captured before the forwarding dispositions are restored,
    /// because classification happens after restoration and the exit status
    /// alone cannot prove cancellation: the verified runtime handles SIGINT
    /// itself and then exits with a plain code 1.
    cancelled: Option<i32>,
}

struct Captured {
    stdout: String,
    #[allow(dead_code, reason = "kept for symmetry with streamed diagnostics")]
    stderr: String,
}

#[cfg(unix)]
mod forward {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Signals that cancel a native child and are forwarded to it.
    const TERMINATION: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The pid of the child signals are currently forwarded to (0 = none).
    static CHILD_PID: AtomicI32 = AtomicI32::new(0);
    /// The last termination signal received by pkg during a run (0 = none).
    static RECEIVED: AtomicI32 = AtomicI32::new(0);
    /// Whether forwarding dispositions are installed right now.
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    /// Only one child may be registered at a time.
    static RUN_LOCK: Mutex<()> = Mutex::new(());

    extern "C" fn forward_to_child(signum: libc::c_int) {
        // Async-signal-safe: atomic loads/stores and `kill` only.
        let pid = CHILD_PID.load(Ordering::Relaxed);
        if pid > 0 {
            // SAFETY: `kill` sends `signum` to exactly the registered pid.
            unsafe { libc::kill(pid, signum) };
        }
        RECEIVED.store(signum, Ordering::Relaxed);
    }

    fn empty_action(handler: usize) -> libc::sigaction {
        // SAFETY: an all-zero `sigaction` is a valid empty mask and no
        // flags; the handler field is then assigned explicitly.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        action
    }

    /// Install forwarding dispositions for the termination signals.
    ///
    /// A signal sent only to the pkg pid is delivered to the handler and
    /// forwarded to the registered child, so the child is cancelled while
    /// pkg stays alive to reap it and re-read state.
    pub(super) fn install() -> Option<Guard> {
        let mut previous = Vec::new();
        for signum in TERMINATION {
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: registers `forward_to_child` for `signum` and captures
            // the previous disposition in `old`.
            let handler = forward_to_child as *const () as usize;
            if unsafe { libc::sigaction(signum, &empty_action(handler), &mut old) } != 0 {
                for (changed, old) in &previous {
                    // SAFETY: restores dispositions captured above.
                    unsafe { libc::sigaction(*changed, old, std::ptr::null_mut()) };
                }
                return None;
            }
            previous.push((signum, old));
        }
        RECEIVED.store(0, Ordering::Relaxed);
        ACTIVE.store(true, Ordering::Relaxed);
        Some(Guard(previous))
    }

    /// Restores the previous dispositions when the run finishes.
    pub(super) struct Guard(Vec<(libc::c_int, libc::sigaction)>);

    impl Guard {
        pub(super) fn restore(self) {
            for (signum, old) in self.0 {
                // SAFETY: restores a previously captured disposition.
                unsafe { libc::sigaction(signum, &old, std::ptr::null_mut()) };
            }
            ACTIVE.store(false, Ordering::Relaxed);
            RECEIVED.store(0, Ordering::Relaxed);
        }
    }

    /// Block termination signals, closing the spawn/registration race.
    pub(super) fn block() {
        // SAFETY: builds a signal set from constants and blocks it for this
        // thread only; pending signals are delivered on unblock.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signum in TERMINATION {
                libc::sigaddset(&mut set, signum);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
    }

    /// Unblock termination signals after the child is registered.
    pub(super) fn unblock() {
        // SAFETY: unblocks exactly the set `block` blocked.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signum in TERMINATION {
                libc::sigaddset(&mut set, signum);
            }
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        }
    }

    /// Register the child signals are forwarded to. Callers hold the lock.
    pub(super) fn register(pid: u32) {
        CHILD_PID.store(i32::try_from(pid).unwrap_or(-1), Ordering::Relaxed);
    }

    /// Clear the registered child.
    pub(super) fn unregister() {
        CHILD_PID.store(0, Ordering::Relaxed);
    }

    /// The received signal number, when one was received.
    pub(super) fn received_signal() -> Option<i32> {
        let signum = RECEIVED.load(Ordering::Relaxed);
        (signum != 0 && ACTIVE.load(Ordering::Relaxed)).then_some(signum)
    }

    /// Serialize child registration; only one child runs at a time.
    pub(super) fn lock() -> std::sync::MutexGuard<'static, ()> {
        RUN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Nix {
    /// The absolute executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The probed version string.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Trace every native command line to stderr before it runs.
    ///
    /// With this off, `--verbose` would describe only some calls; the
    /// adapter is the single place where every native invocation passes.
    #[must_use]
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Ask native children not to color their output (`NO_COLOR=1`).
    #[must_use]
    pub fn no_color(mut self, no_color: bool) -> Self {
        self.no_color = no_color;
        self
    }

    /// The complete argument vector for one native invocation.
    fn argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = [
            "--extra-experimental-features",
            "nix-command flakes",
            // Explicitly refuse flake-provided nix.conf settings; this is the
            // supported boolean setting form on the tested runtime.
            "--option",
            "accept-flake-config",
            "false",
            // Refuse import-from-derivation everywhere in this client; the
            // cask execution path must not need it, and supported sources
            // are checked with this setting false.
            "--option",
            "allow-import-from-derivation",
            "false",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect();
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        argv
    }

    fn capture(&self, args: &[&str]) -> Result<Captured, NixError> {
        // Captured children get the same forwarding and reaping as streamed
        // ones; a signal sent only to pkg still cancels the child.
        let reaped = self.execute(args, IoMode::Capture)?;
        let Output {
            stdout,
            stderr,
            status,
        } = match reaped.outcome {
            ChildOutcome::Output(output) => output,
            ChildOutcome::Status(_) => {
                return Err(NixError::Spawn(String::from("unexpected streamed child")));
            }
        };
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        if let Some(signal) = cancellation(reaped.cancelled, &status) {
            return Err(NixError::Interrupted {
                args: args.iter().map(ToString::to_string).collect(),
                signal,
            });
        }
        if !status.success() {
            return Err(NixError::Failed {
                args: args.iter().map(ToString::to_string).collect(),
                status: exit_status_text(status),
                stderr,
            });
        }
        Ok(Captured { stdout, stderr })
    }

    /// Run a native command with streamed stdio, reaping the child.
    ///
    /// Termination signals sent to pkg are forwarded to the child, so the
    /// child is cancelled and pkg survives to reap it. A cancelled child is
    /// reported as [`NixError::Interrupted`] even when Nix handles the
    /// signal and exits with a plain nonzero code; callers must re-read
    /// native state after every failed mutation.
    fn run_streamed(&self, args: &[&str]) -> Result<(), NixError> {
        let reaped = self.execute(args, IoMode::Stream)?;
        let status = match reaped.outcome {
            ChildOutcome::Status(status) => status,
            ChildOutcome::Output(_) => {
                return Err(NixError::Spawn(String::from("unexpected captured child")));
            }
        };
        if let Some(signal) = cancellation(reaped.cancelled, &status) {
            return Err(NixError::Interrupted {
                args: args.iter().map(ToString::to_string).collect(),
                signal,
            });
        }
        if !status.success() {
            return Err(NixError::Failed {
                args: args.iter().map(ToString::to_string).collect(),
                status: exit_status_text(status),
                stderr: String::new(),
            });
        }
        Ok(())
    }

    /// Build and run one native child through the shared forward/reap
    /// boundary.
    fn execute(&self, args: &[&str], mode: IoMode) -> Result<Reaped, NixError> {
        // `--verbose` describes every native call here, captures included,
        // with the exact argument vector that will run.
        let argv = self.argv(args);
        if self.verbose {
            eprintln!("pkg: nix {}", argv.join(" "));
        }
        let mut command = Command::new(&self.executable);
        command.args(&argv);
        if self.no_color {
            // Any non-empty NO_COLOR value means "no color"; set it only when
            // disabling so colors are never suppressed by accident.
            command.env("NO_COLOR", "1");
        }
        run_child(command, mode).map_err(NixError::Spawn)
    }

    fn json<T: serde::de::DeserializeOwned>(
        &self,
        what: &'static str,
        args: &[&str],
    ) -> Result<T, NixError> {
        let captured = self.capture(args)?;
        serde_json::from_str(&captured.stdout).map_err(|error| NixError::Decode {
            what,
            detail: error.to_string(),
        })
    }

    /// Read installed elements from the dedicated profile (design D3).
    ///
    /// Keys of the returned map are the literal native entry IDs; pass them
    /// back to [`Nix::profile_remove`] and [`Nix::profile_upgrade`] verbatim.
    pub fn profile_list(&self, profile: &Path) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
        let profile = profile.to_string_lossy().into_owned();
        let raw = self.json::<serde_json::Value>(
            "profile identity",
            &["profile", "list", "--profile", &profile, "--json"],
        )?;
        decode_profile_manifest(&raw)
    }

    /// Check the profile manifest shape read-only before a mutation.
    ///
    /// Required runtime capabilities are checked before any mutation: the
    /// external runtime must already speak the decoded manifest shape. On
    /// the tested runtime an absent profile still lists as manifest
    /// version 3 with empty `elements`, so a fresh host passes and no
    /// profile is created. `remove` and `upgrade` callers pre-read the
    /// profile through [`Nix::profile_list`] themselves.
    fn require_decodable_profile(&self, profile: &Path) -> Result<(), NixError> {
        self.profile_list(profile).map(|_| ())
    }

    /// Add resolved installables in one native operation.
    ///
    /// The manifest shape is checked read-only first; a runtime that
    /// cannot speak it fails before the profile is touched.
    pub fn profile_add(&self, profile: &Path, installables: &[String]) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["profile", "add", "--profile", &profile];
        // `--` keeps installables from being read as flags.
        args.push("--");
        args.extend(installables.iter().map(String::as_str));
        self.run_streamed(&args)
    }

    /// Remove exact entries in one native operation.
    ///
    /// Entry names are the literal native element IDs; they are passed after
    /// `--` so IDs that begin with `-` are never read as flags. Regex
    /// removal is deliberately not offered.
    pub fn profile_remove(&self, profile: &Path, entries: &[String]) -> Result<(), NixError> {
        let profile = profile.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["profile", "remove", "--profile", &profile];
        if !entries.is_empty() {
            args.push("--");
            args.extend(entries.iter().map(String::as_str));
        }
        self.run_streamed(&args)
    }

    /// Upgrade exact entries, or all entries when `entries` is `None`.
    ///
    /// Entry names are the literal native element IDs and are passed after
    /// `--` so IDs that begin with `-` are never read as flags.
    pub fn profile_upgrade(
        &self,
        profile: &Path,
        entries: Option<&[String]>,
    ) -> Result<(), NixError> {
        let profile = profile.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["profile", "upgrade", "--profile", &profile];
        match entries {
            Some(list) if !list.is_empty() => {
                args.push("--");
                args.extend(list.iter().map(String::as_str));
            }
            _ => args.push("--all"),
        }
        self.run_streamed(&args)
    }

    /// Roll back to the previous or a selected native generation.
    ///
    /// The manifest shape is checked read-only first.
    pub fn profile_rollback(
        &self,
        profile: &Path,
        generation: Option<u64>,
    ) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy().into_owned();
        let generation = generation.map(|n| n.to_string());
        let mut args: Vec<&str> = vec!["profile", "rollback", "--profile", &profile];
        if let Some(number) = generation.as_deref() {
            args.extend(["--to", number]);
        }
        self.run_streamed(&args)
    }

    /// Delete non-current generations older than an explicit age.
    ///
    /// Eligibility stays with Nix: which generations disappear is native
    /// behavior and is not assumed by pkg. The manifest shape is checked
    /// read-only first.
    pub fn profile_wipe_history(&self, profile: &Path, age: &str) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy().into_owned();
        self.run_streamed(&[
            "profile",
            "wipe-history",
            "--profile",
            &profile,
            "--older-than",
            age,
        ])
    }

    /// Print native profile generation history.
    pub fn profile_history(&self, profile: &Path) -> Result<String, NixError> {
        let profile = profile.to_string_lossy().into_owned();
        Ok(self
            .capture(&["profile", "history", "--profile", &profile])?
            .stdout)
    }

    /// Run a native search and return evaluated metadata.
    pub fn search(
        &self,
        reference: &str,
        pattern: &str,
    ) -> Result<BTreeMap<String, SearchMeta>, NixError> {
        self.json::<BTreeMap<String, SearchMeta>>(
            "search results",
            &["search", reference, pattern, "--json"],
        )
    }

    /// Resolve the current system triple.
    pub fn system(&self) -> Result<String, NixError> {
        Ok(self
            .capture(&["config", "show", "system"])?
            .stdout
            .trim()
            .to_string())
    }

    /// Resolve source identity for a flake reference.
    pub fn source_identity(&self, reference: &str) -> Result<SourceIdentity, NixError> {
        self.source_identity_with(reference, false)
    }

    /// Resolve source identity, bypassing cached flake metadata.
    ///
    /// Passes the native `--refresh` option so `pkg update` refreshes
    /// metadata instead of reusing Nix's TTL cache and calling it current.
    pub fn refresh_source_identity(&self, reference: &str) -> Result<SourceIdentity, NixError> {
        self.source_identity_with(reference, true)
    }

    fn source_identity_with(
        &self,
        reference: &str,
        refresh: bool,
    ) -> Result<SourceIdentity, NixError> {
        let mut args = vec!["flake", "metadata", reference, "--json"];
        if refresh {
            args.push("--refresh");
        }
        let value = self.json::<serde_json::Value>("source identity", &args)?;
        Ok(decode_source_identity(reference, &value))
    }

    /// Probe the local Nix store service (`nix store info`).
    ///
    /// `nix store ping` is a deprecated alias for `info`; the supported
    /// command is used deliberately. Output text is returned for reporting.
    pub fn store_info(&self) -> Result<String, NixError> {
        Ok(self.capture(&["store", "info"])?.stdout)
    }

    /// Evaluate and decode the cask support manifest for `system`.
    ///
    /// `reference` must be the same source reference used for native search
    /// over the casks source, so search results and support exclusions come
    /// from one source and one locked revision (design D6).
    pub fn cask_support(&self, reference: &str, system: &str) -> Result<CaskSupport, NixError> {
        let installable = format!("{reference}#caskSupport.{system}");
        let support =
            self.json::<CaskSupport>("cask support manifest", &["eval", "--json", &installable])?;
        if support.schema != CASK_SUPPORT_SCHEMA {
            return Err(NixError::Decode {
                what: "cask support manifest",
                detail: format!(
                    "schema {:?} is not the decoded schema {CASK_SUPPORT_SCHEMA:?}",
                    support.schema
                ),
            });
        }
        if support.system != system {
            return Err(NixError::Decode {
                what: "cask support manifest",
                detail: format!(
                    "manifest system {:?} does not match the requested system {system:?}",
                    support.system
                ),
            });
        }
        Ok(support)
    }
}

/// Spawn one prepared command through the shared signal forward/reap
/// boundary, wait for it, and reap it.
///
/// The same boundary serves native Nix children and direct external
/// commands: termination signals sent only to pkg are forwarded to the
/// child, the child is reaped, and pkg stays alive.
fn run_child(mut command: Command, mode: IoMode) -> Result<Reaped, String> {
    #[cfg(unix)]
    let _run = forward::lock();
    #[cfg(unix)]
    let guard = forward::install();
    #[cfg(unix)]
    forward::block();
    let spawn = command
        .stdin(if mode == IoMode::Stream {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(if mode == IoMode::Stream {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .stderr(if mode == IoMode::Stream {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(error) => {
            #[cfg(unix)]
            forward::unblock();
            if let Some(guard) = guard {
                guard.restore();
            }
            return Err(error.to_string());
        }
    };
    #[cfg(unix)]
    forward::register(child.id());
    #[cfg(unix)]
    forward::unblock();
    let waited = if mode == IoMode::Stream {
        child.wait().map(ChildOutcome::Status)
    } else {
        child.wait_with_output().map(ChildOutcome::Output)
    };
    #[cfg(unix)]
    forward::unregister();
    // Read the forwarded-signal record before the dispositions are
    // restored: `restore` clears it, and classification happens later.
    #[cfg(unix)]
    let cancelled = forward::received_signal();
    #[cfg(not(unix))]
    let cancelled: Option<i32> = None;
    if let Some(guard) = guard {
        guard.restore();
    }
    waited
        .map(|outcome| Reaped { outcome, cancelled })
        .map_err(|error| error.to_string())
}

/// The outcome of one direct external command run through the shared
/// signal forward/reap boundary.
#[derive(Debug)]
pub enum DirectOutcome {
    /// The command exited successfully.
    Success,
    /// The command was cancelled by a termination signal during the run.
    ///
    /// The child was reaped and pkg stayed alive; the operation may have
    /// partially applied, so callers must finish their own bookkeeping
    /// and report.
    Interrupted {
        /// The terminating signal number.
        signal: i32,
    },
    /// The command exited with a failure.
    Failure {
        /// The exit status text.
        status: String,
        /// Captured stderr, preserved for the caller's diagnostic.
        stderr: String,
    },
}

/// Run one direct external command through the shared signal forward/reap
/// boundary used for native Nix children.
///
/// Termination signals sent only to pkg are forwarded to the child, the
/// child is reaped, and pkg stays alive, so an interrupted command is
/// reported as [`DirectOutcome::Interrupted`] instead of killing pkg
/// mid-run. Stdout is discarded; stderr is captured for diagnostics.
pub fn run_direct(executable: &Path, args: &[String]) -> Result<DirectOutcome, String> {
    let mut command = Command::new(executable);
    command.args(args);
    let reaped = run_child(command, IoMode::Capture)?;
    let Output { stderr, status, .. } = match reaped.outcome {
        ChildOutcome::Output(output) => output,
        ChildOutcome::Status(_) => {
            return Err(String::from("unexpected streamed child"));
        }
    };
    if let Some(signal) = cancellation(reaped.cancelled, &status) {
        return Ok(DirectOutcome::Interrupted { signal });
    }
    if !status.success() {
        return Ok(DirectOutcome::Failure {
            status: exit_status_text(status),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        });
    }
    Ok(DirectOutcome::Success)
}

/// The cancellation signal for a finished child, when the run was
/// cancelled: a forwarded signal pkg received during the run wins (the
/// runtime handles SIGINT itself and exits 1), then a signalled child,
/// then a child that exited 130 after handling SIGINT itself.
fn cancellation(cancelled: Option<i32>, status: &std::process::ExitStatus) -> Option<i32> {
    cancelled.or_else(|| unix_signal(status))
}

/// Decode one captured `nix flake metadata --json` value.
///
/// The canonical locked URL is the top-level `url` Nix reports for the
/// reference: for a moving `git+file` reference it keeps the transport and
/// the `ref`/`rev` parameters, while the nested `locked.url` drops them
/// and is not a usable query reference. The nested value is only a
/// fallback for shapes without a top-level `url`.
fn decode_source_identity(reference: &str, value: &serde_json::Value) -> SourceIdentity {
    let locked = value
        .get("locked")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let revision = locked
        .get("rev")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let locked_url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .or_else(|| locked.get("url").and_then(serde_json::Value::as_str))
        .map(ToString::to_string);
    let display = [
        locked.get("type").and_then(serde_json::Value::as_str),
        locked.get("owner").and_then(serde_json::Value::as_str),
        locked.get("repo").and_then(serde_json::Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(":");
    SourceIdentity {
        reference: reference.to_string(),
        locked_url,
        revision,
        display: if display.is_empty() {
            reference.to_string()
        } else {
            display
        },
    }
}

/// Decode one captured `nix profile list --json` document.
///
/// The manifest `version` field is required and must equal
/// [`PROFILE_MANIFEST_VERSION`]: the supported native runtime always
/// reports it, so an absent or different version is an unsupported shape,
/// not an invented version floor.
fn decode_profile_manifest(
    raw: &serde_json::Value,
) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    let Some(version) = raw.get("version").and_then(serde_json::Value::as_u64) else {
        return Err(NixError::Decode {
            what: "profile identity",
            detail: format!(
                "`nix profile list --json` reports no manifest version; \
                 version {PROFILE_MANIFEST_VERSION} is required \
                 (tested on Nix {TESTED_BASELINE})"
            ),
        });
    };
    if version != PROFILE_MANIFEST_VERSION {
        return Err(NixError::Decode {
            what: "profile identity",
            detail: format!(
                "profile manifest version {version} is not the decoded version \
                 {PROFILE_MANIFEST_VERSION} (tested on Nix {TESTED_BASELINE})"
            ),
        });
    }
    let Some(elements) = raw.get("elements") else {
        return Err(NixError::Decode {
            what: "profile identity",
            detail: String::from(
                "`elements` is absent from `nix profile list --json` \
                 (expected manifest version 3)",
            ),
        });
    };
    serde_json::from_value(elements.clone()).map_err(|error| NixError::Decode {
        what: "profile identity",
        detail: error.to_string(),
    })
}

fn unix_signal(status: &std::process::ExitStatus) -> Option<i32> {
    if let Some(signal) = platform_signal(status) {
        return Some(signal);
    }
    // A child that exits with status 130 reports a handled SIGINT; treat it
    // as interruption so native state is still re-read.
    (status.code() == Some(130)).then_some(2)
}

fn platform_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

fn exit_status_text(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => format!("terminated by signal: {status}"),
    }
}

/// Detect explicit full-commit semantics in a reference.
///
/// Only a full-length (40 hex character) commit id proves a fixed reference:
/// either a `rev=<commit>` parameter or a full commit as the final path
/// segment. Tags, branch names, and short hex-looking branch names are moving
/// references and never get fixed status here; the locked revision comes from
/// actual flake metadata instead (design D4).
#[must_use]
pub fn is_fixed_reference(reference: &str) -> bool {
    if let Some(rest) = reference.split("rev=").nth(1) {
        let rev = rest.chars().take_while(char::is_ascii_hexdigit).count();
        if rev == FULL_COMMIT_LEN {
            return true;
        }
    }
    let without_query = reference.split(['?', '#']).next().unwrap_or(reference);
    without_query.rsplit('/').next().is_some_and(is_full_commit)
}

fn is_full_commit(text: &str) -> bool {
    text.len() == FULL_COMMIT_LEN && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Extract a locked full commit from a locked URL, when present.
#[must_use]
pub fn locked_revision(url: &str) -> Option<&str> {
    if let Some(rest) = url.split("rev=").nth(1) {
        let rev = rest.split('&').next().unwrap_or(rest);
        if is_full_commit(rev) {
            return Some(rev);
        }
    }
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last = without_query.rsplit('/').next()?;
    is_full_commit(last).then_some(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";

    #[test]
    fn fixed_reference_detection_requires_explicit_full_commits() {
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs/{COMMIT}"
        )));
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs?rev={COMMIT}"
        )));
        assert!(is_fixed_reference(&format!(
            "github:NixOS/nixpkgs?rev={COMMIT}&dir=."
        )));
        // Tags, branches, and hex-looking branch names are not provably fixed.
        assert!(!is_fixed_reference("github:NixOS/nixpkgs/refs/tags/25.05"));
        assert!(!is_fixed_reference("github:NixOS/nixpkgs/nixpkgs-unstable"));
        assert!(!is_fixed_reference("github:spa5k/pkg/main?dir=nix/casks"));
        assert!(!is_fixed_reference("github:owner/repo/0a56ceb"));
        let truncated = &COMMIT[..39];
        assert!(!is_fixed_reference(&format!(
            "github:owner/repo/{truncated}"
        )));
    }

    #[test]
    fn locked_revision_reads_full_commits_only() {
        assert_eq!(
            locked_revision(&format!("github:NixOS/nixpkgs/{COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision(&format!("github:NixOS/nixpkgs?rev={COMMIT}")),
            Some(COMMIT)
        );
        assert_eq!(
            locked_revision("github:NixOS/nixpkgs/nixpkgs-unstable"),
            None
        );
        assert_eq!(locked_revision("path:/tmp/pkg-native-proof.UUZyyl"), None);
    }

    /// Captured manifest shape from Determinate 3.22.1 / Nix 2.35.2: version 3,
    /// top-level `elements`, string `attrPath`, locked `url`, and map keys that
    /// are full `originalUrl#attrPath` strings.
    #[test]
    fn decodes_captured_profile_manifest_and_rejects_other_shapes() {
        // Linux shape uses plain source keys; both are decoded opaquely.
        let linux_captured = r#"{
          "version": 3,
          "elements": {
            "github:NixOS/nixpkgs/nixpkgs-unstable": {
              "active": true,
              "attrPath": "legacyPackages.x86_64-linux.hello",
              "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
              "outputs": null,
              "priority": 5,
              "storePaths": ["/nix/store/Hash-hello-2.12.1"],
              "url": "github:NixOS/nixpkgs/0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"
            }
          }
        }"#;
        let linux: serde_json::Value = serde_json::from_str(linux_captured).expect("valid json");
        let linux_entries: BTreeMap<String, ProfileEntry> =
            serde_json::from_value(linux["elements"].clone()).expect("decodes linux keys");
        let linux_entry = &linux_entries["github:NixOS/nixpkgs/nixpkgs-unstable"];
        assert_eq!(linux_entry.name(), "hello");
        assert_eq!(linux_entry.locked_revision(), Some(COMMIT));

        let captured = format!(
            r#"{{
              "version": 3,
              "elements": {{
                "path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default": {{
                  "active": true,
                  "attrPath": "packages.aarch64-darwin.default",
                  "originalUrl": "path:/tmp/pkg-native-proof.UUZyyl",
                  "outputs": null,
                  "priority": 5,
                  "storePaths": ["/nix/store/Hash-default-1.0"],
                  "url": "path:/tmp/pkg-native-proof.UUZyyl"
                }},
                "github:NixOS/nixpkgs/nixpkgs-unstable#legacyPackages.aarch64-darwin.ripgrep": {{
                  "active": true,
                  "attrPath": "legacyPackages.aarch64-darwin.ripgrep",
                  "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
                  "outputs": null,
                  "priority": 5,
                  "storePaths": ["/nix/store/Hash-ripgrep-14.1.0"],
                  "url": "github:NixOS/nixpkgs/{COMMIT}"
                }}
              }}
            }}"#
        );
        let raw: serde_json::Value = serde_json::from_str(&captured).expect("valid json");
        let elements = raw.get("elements").expect("elements present");
        let entries: BTreeMap<String, ProfileEntry> =
            serde_json::from_value(elements.clone()).expect("decodes captured shape");
        let path_entry =
            &entries["path:/tmp/pkg-native-proof.UUZyyl#packages.aarch64-darwin.default"];
        assert_eq!(path_entry.name(), "default");
        assert_eq!(path_entry.locked_revision(), None);
        assert!(!is_fixed_reference(path_entry.original_url.as_str()));
        let ripgrep =
            &entries["github:NixOS/nixpkgs/nixpkgs-unstable#legacyPackages.aarch64-darwin.ripgrep"];
        assert_eq!(ripgrep.name(), "ripgrep");
        assert_eq!(ripgrep.locked_revision(), Some(COMMIT));

        // Missing required identity fields must fail, not default silently.
        let missing = r#"{"version": 3, "elements": {"ripgrep": {"attrPath": "ripgrep"}}}"#;
        let value: serde_json::Value = serde_json::from_str(missing).expect("valid json");
        assert!(
            serde_json::from_value::<BTreeMap<String, ProfileEntry>>(value["elements"].clone())
                .is_err()
        );
    }

    /// The pre-mutation guard depends on a strict manifest decode: the
    /// `version` field is required, only version 3 decodes, and entry
    /// fields stay mandatory.
    #[test]
    fn profile_manifest_decode_requires_version_three() {
        let empty = serde_json::json!({"version": 3, "elements": {}});
        assert!(decode_profile_manifest(&empty).expect("decodes").is_empty());

        let missing = serde_json::json!({"elements": {}});
        let error = decode_profile_manifest(&missing).expect_err("version is required");
        let text = error.to_string();
        assert!(text.contains("no manifest version"), "{text}");

        let wrong = serde_json::json!({"version": 2, "elements": {}});
        let error = decode_profile_manifest(&wrong).expect_err("other versions are refused");
        let text = error.to_string();
        assert!(text.contains("version 2"), "{text}");

        let element_without_fields = serde_json::json!({
            "version": 3,
            "elements": {"ripgrep": {"attrPath": "ripgrep"}}
        });
        assert!(decode_profile_manifest(&element_without_fields).is_err());
    }

    /// Decodes the actual generated cask support manifest fixture
    /// (Determinate 3.22.1 / Nix 2.35.2 host output), not an invented shape.
    #[test]
    fn decodes_the_real_cask_support_manifest_fixture() {
        let manifest = include_str!("../tests/fixtures/cask-support.json");
        let support: CaskSupport = serde_json::from_str(manifest).expect("decodes real fixture");
        assert_eq!(support.schema, CASK_SUPPORT_SCHEMA);
        assert_eq!(support.system, "aarch64-darwin");
        assert_eq!(support.supported.len(), 3);
        assert_eq!(support.excluded.len(), 5);

        let raycast = &support.excluded[4];
        assert_eq!(raycast.token, "raycast");
        assert_eq!(raycast.reason, "minimum-os");
        // A real on-device exclusion detail; it is a non-null string here.
        assert!(
            raycast
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("LSMinimumSystemVersion"))
        );
        assert_eq!(raycast.version.as_deref(), Some("2.0.5.0"));

        let iterm2 = &support.supported[0];
        assert_eq!(iterm2.token, "iterm2");
        assert_eq!(iterm2.kind, "app");
        assert!(!iterm2.override_applied, "override is a boolean");
        assert_eq!(iterm2.version.as_deref(), Some("3.6.11"));
        assert!(
            iterm2
                .homepage
                .as_deref()
                .is_some_and(|url| url.starts_with("https://"))
        );
        assert!(iterm2.artifact_kinds.contains(&String::from("app")));

        let cursor = &support.supported[2];
        assert_eq!(cursor.token, "cursor");
        assert!(cursor.override_applied, "cursor applies an override");
        // Version carries the appended source revision in the real manifest.
        assert!(cursor.version.as_deref().is_some_and(|v| v.contains(',')));

        let zoom = &support.excluded[0];
        assert_eq!(zoom.token, "zoom");
        assert_eq!(zoom.reason, "native-installer");
        assert_eq!(zoom.detail, None, "excluded detail can be null");
        assert_eq!(zoom.version.as_deref(), Some("7.1.5.84650"));

        let nightly = &support.excluded[3];
        assert_eq!(nightly.token, "1password@nightly");
        assert_eq!(nightly.version.as_deref(), Some("latest"));

        let brew_nix = &support.sources["brew-nix"];
        assert_eq!(
            brew_nix.rev.as_deref(),
            Some("16131ae4126c54b1502aa7eaf6573d7fbf16b656")
        );
        assert!(
            brew_nix
                .url
                .ends_with("16131ae4126c54b1502aa7eaf6573d7fbf16b656")
        );
    }

    /// Captured `nix flake metadata --json` for a moving `git+file`
    /// reference: the top-level `url` is the canonical locked URL and keeps
    /// the transport and parameters; the nested `locked.url` drops them and
    /// must not be used as the query reference.
    #[test]
    fn source_identity_uses_the_canonical_top_level_locked_url() {
        let captured = r#"{
          "originalUrl": "git+file:///tmp/pkg-cask-moving?ref=main",
          "url": "git+file:///tmp/pkg-cask-moving?ref=main&rev=c073a489725bb05621c798827025b2faf7482f6b",
          "locked": {
            "type": "git",
            "url": "file:///tmp/pkg-cask-moving",
            "ref": "main",
            "rev": "c073a489725bb05621c798827025b2faf7482f6b"
          }
        }"#;
        let value: serde_json::Value = serde_json::from_str(captured).expect("valid json");
        let identity = decode_source_identity("git+file:///tmp/pkg-cask-moving?ref=main", &value);
        assert_eq!(
            identity.locked_url.as_deref(),
            Some(
                "git+file:///tmp/pkg-cask-moving?ref=main&rev=c073a489725bb05621c798827025b2faf7482f6b"
            )
        );
        assert_eq!(
            identity.revision.as_deref(),
            Some("c073a489725bb05621c798827025b2faf7482f6b")
        );
        assert_eq!(identity.display, "git");
        // Without a top-level url, the nested locked url stays as fallback.
        let mut bare = value.clone();
        bare.as_object_mut().expect("object").remove("url");
        let fallback = decode_source_identity("git+file:///tmp/pkg-cask-moving?ref=main", &bare);
        assert_eq!(
            fallback.locked_url.as_deref(),
            Some("file:///tmp/pkg-cask-moving")
        );
    }
}
