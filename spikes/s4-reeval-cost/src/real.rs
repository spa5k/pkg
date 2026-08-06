//! Spike S4 (PR-6 / DR-004) — REAL slice: the FULLY-WIRED Real (Nix-touching)
//! lane. It assembles the exact pinned argv/env/spec, drives the whole
//! pipeline end-to-end through the fixed command executor, and returns a
//! validated [`crate::report::Report`].
//!
//! # What [`run_real`] does
//! The PUBLIC entry point [`run_real`] takes the absolute `nix` binary path
//! and, against the pinned [`crate::manifest::Manifest`]:
//!   * assembles the EXACT argv for four commands — the `nix --version` probe
//!     ([`VERSION_ARGV`]), the `nix flake prefetch --json <flake_ref>`
//!     ([`prefetch_argv`], ONLINE — no `--offline`), and the single-attribute
//!     and index-meta `nix … --offline eval --json` ([`single_eval_argv`] /
//!     [`index_eval_argv`]) — token-for-token with no re-splitting, and folds
//!     each into a validated [`crate::command::CommandSpec`] via the
//!     `*_command_spec` builders;
//!   * creates exactly ONE private fail-closed workspace per run
//!     ([`RealPrivateHome`]: root + `cache` + `config`, each created atomically
//!     at Unix mode `0o700`, removed best-effort on [`Drop`]) and builds the
//!     COMPLETE five-entry child env ([`real_child_env`]: `LANG=C`, `LC_ALL=C`,
//!     `HOME`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`) rooted at it. Only `HOME`
//!     and the two XDG dirs are redirected — the configured `/nix/store` stays
//!     shared, nothing else is relocated;
//!   * probes the EXACT pinned Nix version (`manifest.nix.version`, e.g.
//!     `2.34.8`) via [`execute_version_probe`]; runs the ONE online prefetch
//!     via [`execute_verified_prefetch`], verifying its `hash` EQUALS the
//!     pinned `manifest.nixpkgs.nar_hash` and its `storePath` is a well-formed
//!     `/nix/store/<basename>`; every later eval carries exactly ONE
//!     `--offline` (a pure local evaluation of the already-fetched flake);
//!   * drives the canonical scenarios via [`execute_real_scenario`]: a FRESH
//!     `nix` process per iteration (no reuse), all warmups then all measured,
//!     every sample labelled
//!     [`crate::report::CacheLabel::SourceWarmProcessCold`] — the flake source
//!     is warm from prefetch while each process is cold (the harness never
//!     clears the store or evaluator caches);
//!   * enforces a per-command timeout ([`crate::runner::select_timeout`] of the
//!     phase budget against an overall wall-clock deadline) plus bounded
//!     stdout/stderr capture ([`NonZeroU64`] caps) on EVERY command, and
//!     re-checks the overall deadline around the final fold.
//!
//! All execution goes through the injected production executor
//! [`crate::execute::run`] — the `/usr/bin/time` wrapper that spawns each spec
//! in its OWN process group under a fail-closed environment, captures
//! stdout/stderr concurrently under bounded caps, enforces the wall-clock
//! timeout, and reaps max-RSS. This module performs NO direct logging and
//! leaks NO child stdout/stderr.
//!
//! # Diagnostics / failure shape
//! Command and scenario failures NEVER panic or surface raw strings: they
//! collapse to the CLOSED, field-free [`RealFailureKind`] table (detect /
//! prefetch command+verification, eval command+outcome, overall timeout,
//! scenario + report assembly) — every message is fixed, bounded, ASCII, and
//! embeds NO [`crate::command::CommandError`], version string, installable, or
//! child output. Honest partial observations are folded into
//! [`crate::report::Scenario`]s ([`assemble_real_scenario`]) and a validated
//! [`crate::report::Report`] ([`assemble_real_report`], with an
//! [`assemble_or_fallback`] safety net): [`crate::report::Completeness::Complete`]
//! only on full success, else [`crate::report::Completeness::Incomplete`]
//! carrying the honest prefix and the closed failure(s). Only a private-home,
//! preparation, or fallback-assembly failure surfaces as a [`RealRunError`].
//!
//! # Invariants (held by every produced argv/env and by the executor)
//!   * NO shell, NO `PATH` search (the absolute `nix` path is `program`), NO
//!     `NIX_PATH`, NO `--impure`, NO `--build` / `nix-build`, NO
//!     `--substituter` override, NO cache clearing.
//!   * Every installable is the pinned `github:…/<rev>?narHash=<encoded>#…`
//!     form built by [`crate::flakeref`] from the validated manifest — the
//!     exact flake pin.
//!   * The index-meta projection ([`INDEX_META_EXPR`]) is embedded at compile
//!     time via [`include_str!`] and passed to `--apply` as a SINGLE argv
//!     token — never split, never interpolated.
//!   * [`real_child_env`] is the COMPLETE child environment (applied by the
//!     executor via `Command::env_clear()` + exactly these entries): nothing
//!     is inherited from the parent — unsafe inherited state is forbidden.
//!   * Diagnostics are bounded: no unbounded or attacker-controlled string
//!     reaches logs or an error.
//!
//! `#![forbid(unsafe_code)]` is inherited from the crate root.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::command::{CommandError, CommandOutcome, CommandSpec, TimeFlavor, UnixStatus};
use crate::flakeref::{self, CheckedSystem};
use crate::manifest::{Manifest, benchmark_manifest};
use crate::report::{
    CacheLabel, Completeness, Failure, Host, Mode, Pin, REPORT_SCHEMA_VERSION, Record, Report,
    Sample, SampleStatistics, Scenario, Statistics,
};
use crate::runner::{
    ManifestField, ScenarioDescriptor, descriptors, host, host_time_flavor, nz_cap, select_timeout,
};
use crate::stats;

/// The maintained index-meta projection expression, embedded at compile time
/// from `nix/index-meta.nix`. It is a function of one argument (the evaluated
/// `legacyPackages.<system>` attrset) returning a JSON-safe, bounded list of
/// per-attribute records. Passed to `nix eval … --apply <EXPR>` as a SINGLE
/// argv token (one [`OsString`]): the whole multi-line file is one contiguous
/// string, never split on whitespace and never interpolated.
pub(crate) const INDEX_META_EXPR: &str = include_str!("../nix/index-meta.nix");

/// The experimental-features pair that enables `nix-command` and `flakes`. The
/// value `"nix-command flakes"` is ONE argv token: Nix accepts a
/// space-separated feature list as a single argument.
const FEATURE_PAIR: &[&str] = &["--extra-experimental-features", "nix-command flakes"];

/// The exact argv for the `nix` version probe (`argv[1..]`): `["--version"]`.
/// The program itself — the absolute `nix` path — is supplied separately as
/// `argv[0]` by the runner, so there is NO shell, NO `PATH` search.
pub(crate) const VERSION_ARGV: &[&str] = &["--version"];

/// Push the `--extra-experimental-features nix-command flakes` pair onto `out`.
fn push_feature_pair(out: &mut Vec<OsString>) {
    for &flag in FEATURE_PAIR {
        out.push(OsString::from(flag));
    }
}

/// Push the shared pure-eval prefix onto `out`:
/// `--extra-experimental-features nix-command flakes --offline eval --json`.
/// `--offline` pins evaluation to the already-fetched flake and forbids any
/// network resolution, so a Real eval is a pure local evaluation of the pinned
/// flake. Used by both the single-attribute and index-meta eval commands.
fn push_eval_prefix(out: &mut Vec<OsString>) {
    push_feature_pair(out);
    out.push(OsString::from("--offline"));
    out.push(OsString::from("eval"));
    out.push(OsString::from("--json"));
}

/// The EXACT argv for the flake prefetch command (`argv[1..]`):
///
/// ```text
/// --extra-experimental-features nix-command flakes flake prefetch --json <flake_ref>
/// ```
///
/// `<flake_ref>` is the pinned, pure flake reference from
/// [`flakeref::flake_ref`]. Prefetch deliberately omits `--offline` (it fetches
/// the flake tarball to populate the local cache), but it remains a pure-flake
/// command: the installable is the pinned `github:…/<rev>?narHash=…` ref.
#[must_use]
pub(crate) fn prefetch_argv(manifest: &Manifest) -> Vec<OsString> {
    let mut out = Vec::with_capacity(FEATURE_PAIR.len() + 4);
    push_feature_pair(&mut out);
    out.push(OsString::from("flake"));
    out.push(OsString::from("prefetch"));
    out.push(OsString::from("--json"));
    out.push(OsString::from(flakeref::flake_ref(manifest)));
    out
}

/// The EXACT argv for the single-attribute reevaluation command (`argv[1..]`):
///
/// ```text
/// --extra-experimental-features nix-command flakes --offline eval --json <single_attr_installable>
/// ```
///
/// `<single_attr_installable>` is the pinned
/// `…#legacyPackages.<system>.<attr>.drvPath` from
/// [`flakeref::single_attr_installable`]. `--offline` makes it a pure local
/// evaluation of the pinned flake; `--json` yields machine-readable output.
#[must_use]
pub(crate) fn single_eval_argv(manifest: &Manifest, system: &CheckedSystem<'_>) -> Vec<OsString> {
    let mut out = Vec::with_capacity(6);
    push_eval_prefix(&mut out);
    out.push(OsString::from(flakeref::single_attr_installable(
        manifest, system,
    )));
    out
}

/// The EXACT argv for the index-meta projection command (`argv[1..]`):
///
/// ```text
/// --extra-experimental-features nix-command flakes --offline eval --json \
///   --apply <INDEX_META_EXPR> <index_installable>
/// ```
///
/// `<index_installable>` is the pinned `…#legacyPackages.<system>` attrset from
/// [`flakeref::index_installable`]. The projection ([`INDEX_META_EXPR`]) is
/// passed to `--apply` as a SINGLE argv token — the whole multi-line Nix
/// expression is one contiguous [`OsString`], never split.
#[must_use]
pub(crate) fn index_eval_argv(manifest: &Manifest, system: &CheckedSystem<'_>) -> Vec<OsString> {
    let mut out = Vec::with_capacity(8);
    push_eval_prefix(&mut out);
    out.push(OsString::from("--apply"));
    out.push(OsString::from(INDEX_META_EXPR));
    out.push(OsString::from(flakeref::index_installable(
        manifest, system,
    )));
    out
}

/// The COMPLETE, deterministic child process environment for a Real (Nix) run,
/// rooted at a PRIVATE `home` directory. Applied by [`crate::execute::run`] via
/// `Command::env_clear()` followed by EXACTLY these entries, so the child sees
/// NOTHING inherited from the parent process — fail-closed.
///
/// The five entries are:
///   * `LANG=C` and `LC_ALL=C` — deterministic, POSIX-locale output;
///   * `HOME=<private_home>` — a throwaway home so Nix writes user-level state
///     (channels, profiles) under it rather than the real user home;
///   * `XDG_CACHE_HOME=<private_home>/cache` — redirects Nix's cache dir;
///   * `XDG_CONFIG_HOME=<private_home>/config` — redirects Nix's config dir.
///
/// There is deliberately NO `PATH` (the runner invokes the absolute `nix`
/// binary directly), NO `NIX_PATH` (pure flake, no channels), and NO other
/// inherited variable.
#[must_use]
pub(crate) fn real_child_env(private_home: &Path) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    env.insert(OsString::from("HOME"), private_home.as_os_str().to_owned());
    env.insert(
        OsString::from("XDG_CACHE_HOME"),
        private_home.join("cache").into_os_string(),
    );
    env.insert(
        OsString::from("XDG_CONFIG_HOME"),
        private_home.join("config").into_os_string(),
    );
    env.insert(OsString::from("LANG"), OsString::from("C"));
    env.insert(OsString::from("LC_ALL"), OsString::from("C"));
    env
}

// === REAL private workspace home =========================================
//
// [`RealPrivateHome`] is the private, throwaway workspace root for a Real
// (Nix) run: a freshly created directory tree (root + `cache` + `config`
// children, all mode 0700) under [`std::env::temp_dir`]. It owns ONLY the
// exact directory it created and removes it (best-effort) on [`Drop`].
// [`run_real_with_executor`] creates exactly one per run and roots
// [`real_child_env`] at [`RealPrivateHome::root`], so Nix writes user-level
// state (channels / profiles / cache / config) under it instead of the real
// user home — fail-closed isolation.
//
// # Security shape
// * The root is always an ABSOLUTE child of [`std::env::temp_dir`] (validated).
// * Candidate names combine a fixed `pkg-s4-real` prefix with the process id,
//   current `SystemTime` wall-clock nanos (the real-time clock since the Unix
//   epoch — NOT a monotonic clock; it may skew or jump backwards), and a
//   process-local [`AtomicU64`] counter; creation is retried a BOUNDED 128 times.
// * The root and both children are created atomically ([`std::fs::DirBuilder`],
//   `recursive = false`) at Unix mode `0o700` via [`DirBuilderExt::mode`]. An
//   already-existing path/file/dir/symlink is NEVER accepted or reused — it is
//   treated as [`std::io::ErrorKind::AlreadyExists`] and the next candidate is
//   tried.
// * After creation, root / cache / config are validated with
//   [`std::fs::symlink_metadata`] to be directories (NOT symlinks) and to carry
//   EXACTLY owner-only mode `0o700` (`mode & 0o777 == 0o700`). Any violation
//   cleans the just-created owned root and returns an error.
// * [`Drop`] removes ONLY the exact owned root via [`std::fs::remove_dir_all`]
//   on the literal path — it never canonicalizes or follows symlinks first.
// * Every error is a fixed, bounded, ASCII message that NEVER embeds a temp
//   path or OS error text.
// * The home reads ONLY [`std::env::temp_dir`]; it never consults `HOME`,
//   `CODEX_HOME`, or any other parent-process environment variable.

/// The fixed prefix for every candidate workspace name.
const HOME_NAME_PREFIX: &str = "pkg-s4-real";

/// Maximum number of unique candidates tried before giving up.
const HOME_MAX_ATTEMPTS: u32 = 128;

/// Process-local counter mixed into candidate names so two calls within the
/// same process at the same nanosecond still differ.
static HOME_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The Unix mode every owned directory is created with: owner-only read/write
/// /execute, no group, no other.
const HOME_DIR_MODE: u32 = 0o700;

/// The two child directories created under the root, in fixed creation order.
const HOME_CHILDREN: [&str; 2] = ["cache", "config"];

/// Build ONE unique candidate workspace name. The name mixes the fixed
/// `pkg-s4-real` prefix, the OS process id, a current wall-clock nanosecond
/// stamp from [`SystemTime`] (the real-time clock since the Unix epoch — NOT a
/// monotonic clock; it may skew or jump backwards), and a process-local
/// [`AtomicU64`] counter so that two calls within the same process at the same
/// nanosecond still differ and so distinct processes cannot collide. PURE: it
/// performs NO filesystem work and never fails.
#[must_use]
fn home_candidate_name() -> String {
    let pid = std::process::id();
    // A non-monotonic clock skew (time before UNIX_EPOCH) maps to 0 rather than
    // panicking; uniqueness then still holds via pid + counter.
    let nanos: u128 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = HOME_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{HOME_NAME_PREFIX}-{pid}-{nanos}-{n}")
}

/// Create a single private directory atomically at `path` with mode `0o700`.
/// `recursive = false`: the parent must already exist and the candidate must
/// not, mirroring a single atomic `mkdir`. Any OS error (including
/// [`std::io::ErrorKind::AlreadyExists`]) is returned to the caller.
fn home_create_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(false).mode(HOME_DIR_MODE);
    builder.create(path)
}

/// Best-effort cleanup of the EXACT owned root only: a single
/// [`std::fs::remove_dir_all`] on the literal `root` path. Deliberately never
/// canonicalizes or follows symlinks first (see the [`Drop`] security note on
/// [`RealPrivateHome`]). The result is discarded: cleanup is best-effort and
/// must never panic.
fn home_cleanup_owned(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// Validate that `root`, `root/cache`, and `root/config` are real directories
/// (not symlinks) at EXACT Unix permission mode `0o700` (owner read/write/
/// execute, no group, no other). Uses [`std::fs::symlink_metadata`] so a
/// symlink is reported as a symlink (NOT a directory). PURE post-creation
/// check: it mutates nothing.
fn home_validate_layout(root: &Path) -> Result<(), ()> {
    let paths: [PathBuf; 3] = [
        root.to_path_buf(),
        root.join(HOME_CHILDREN[0]),
        root.join(HOME_CHILDREN[1]),
    ];
    for p in paths {
        let md = std::fs::symlink_metadata(&p).map_err(|_| ())?;
        // symlink_metadata does not follow: a symlink reports as a symlink, so
        // is_dir() is false for symlinks and for regular files.
        if !md.is_dir() {
            return Err(());
        }
        let mode = md.permissions().mode();
        // Require EXACT mode 0700: owner-only rwx, no group, no other. Masking
        // the low nine permission bits rejects anything looser (group/other
        // bits) AND anything tighter (e.g. 0600, which lacks the owner-execute
        // bit a traversable private directory must have).
        if mode & 0o777 != 0o700 {
            return Err(());
        }
    }
    Ok(())
}

/// Populate the `cache` and `config` children under an already-created owned
/// `root`. On ANY child-creation failure the owned `root` is removed
/// (best-effort, via [`home_cleanup_owned`]) and [`RealPrivateHomeError::ChildCreate`]
/// is returned, so the caller never observes a half-populated tree.
///
/// This is the controlled seam exercised by the child-failure test directly
/// (no global environment mutation): a test creates a read-only owned root and
/// confirms the helper cleans it on failure.
fn home_populate_children(root: &Path) -> Result<(), RealPrivateHomeError> {
    for &name in &HOME_CHILDREN {
        if home_create_dir(&root.join(name)).is_err() {
            home_cleanup_owned(root);
            return Err(RealPrivateHomeError::ChildCreate);
        }
    }
    Ok(())
}

/// Try to create a complete private workspace rooted at EXACTLY `candidate`.
/// Returns:
///   * [`Ok(Some`]` home)` — a fully created, validated workspace rooted at
///     `candidate` (the caller now owns it);
///   * [`Ok(None)`] — `candidate` already existed as ANY entry (directory, file,
///     or symlink); the caller should try the next candidate and MUST NOT reuse
///     or alter the pre-existing entry; or
///   * [`Err`] — root creation, child creation, or post-creation validation
///     failed; on any failure AFTER the root was created, the owned root is
///     cleaned first and the fixed error is returned.
///
/// This is the internal candidate helper tested directly: it proves a
/// pre-existing directory, file, or symlink is never reused or altered.
fn home_create_at(candidate: &Path) -> Result<Option<RealPrivateHome>, RealPrivateHomeError> {
    // 1. Create the root atomically. An existing entry (dir/file/symlink) is
    //    reported as AlreadyExists and signals a retry WITHOUT alteration.
    match home_create_dir(candidate) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(_) => return Err(RealPrivateHomeError::RootCreate),
    }
    // 2. Populate cache/config; on failure the helper cleans the owned root.
    home_populate_children(candidate)?;
    // 3. Post-creation validation; on failure clean the owned root.
    if home_validate_layout(candidate).is_err() {
        home_cleanup_owned(candidate);
        return Err(RealPrivateHomeError::Validate);
    }
    Ok(Some(RealPrivateHome {
        root: candidate.to_path_buf(),
    }))
}

/// The private, throwaway workspace home for a Real run. Owns the exact
/// directory tree it created (root + `cache` + `config`, all mode `0o700`)
/// under [`std::env::temp_dir`] and removes it best-effort on [`Drop`].
pub(crate) struct RealPrivateHome {
    /// The absolute, owned root path. Private: callers reach it via [`Self::root`].
    root: PathBuf,
}

impl RealPrivateHome {
    /// Create a fresh private workspace home under [`std::env::temp_dir`].
    ///
    /// Picks unique candidate names (fixed `pkg-s4-real` prefix + pid + nanos +
    /// [`AtomicU64`] counter), creates each atomically at mode `0o700`, creates
    /// the `cache` / `config` children at mode `0o700`, validates the result,
    /// and retries a BOUNDED [`HOME_MAX_ATTEMPTS`] times on a name collision.
    /// Never accepts or reuses an existing entry. Every error is a fixed,
    /// bounded, ASCII message that never embeds a temp path or OS error text.
    pub(crate) fn create() -> Result<Self, RealPrivateHomeError> {
        let temp = std::env::temp_dir();
        if !temp.is_absolute() {
            return Err(RealPrivateHomeError::TempNotAbsolute);
        }
        for _ in 0..HOME_MAX_ATTEMPTS {
            let name = home_candidate_name();
            let candidate = temp.join(name);
            if let Some(home) = home_create_at(&candidate)? {
                return Ok(home);
            }
        }
        Err(RealPrivateHomeError::Exhausted)
    }

    /// The absolute owned root path of this workspace.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The complete, deterministic five-entry child environment for this
    /// workspace, delegating to [`real_child_env`] rooted at [`Self::root`].
    #[must_use]
    pub(crate) fn child_env(&self) -> BTreeMap<OsString, OsString> {
        real_child_env(self.root())
    }
}

impl Drop for RealPrivateHome {
    fn drop(&mut self) {
        // Best-effort cleanup of the EXACT owned root only. Never canonicalize
        // or follow symlinks first: remove_dir_all receives the literal path.
        home_cleanup_owned(&self.root);
    }
}

/// Error from [`RealPrivateHome::create`]. Every variant's
/// [`std::fmt::Display`] is a fixed, bounded, ASCII message that NEVER embeds a
/// temp path, a candidate name, or OS error text, so an adversarial filesystem
/// state can never reach a produced error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealPrivateHomeError {
    /// [`std::env::temp_dir`] was not absolute.
    TempNotAbsolute,
    /// All [`HOME_MAX_ATTEMPTS`] candidates already existed.
    Exhausted,
    /// The root directory could not be created (non-`AlreadyExists` failure).
    RootCreate,
    /// A `cache` / `config` child could not be created.
    ChildCreate,
    /// Post-creation validation of root / cache / config failed.
    Validate,
}

impl std::fmt::Display for RealPrivateHomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::TempNotAbsolute => "private home base temp directory is not absolute",
            Self::Exhausted => "private home creation exhausted unique candidates",
            Self::RootCreate => "private home root directory could not be created",
            Self::ChildCreate => "private home child directory could not be created",
            Self::Validate => "private home post-creation validation failed",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RealPrivateHomeError {}

// === REAL CommandSpec builders (PURE, non-executing) =====================
//
// The four crate-private builders below turn the established exact argv (the
// `*_argv` helpers above) and the complete five-entry fail-closed environment
// ([`RealPrivateHome::child_env`]) into a ready-to-run [`CommandSpec`] for each
// Real-lane command (version probe, flake prefetch, single-attribute eval,
// index-meta eval). They are PURE, allocation-only construction: they perform
// NO spawning, NO I/O (beyond what the caller already spent to produce `home`
// via [`RealPrivateHome::create`]), NO network/store access, and NO shell or
// global-environment mutation. Every builder:
//   * preserves `nix_bin` EXACTLY as `program` — [`CommandSpec::new`] rejects a
//     non-absolute/empty path, so there is NO `PATH` search at execution time;
//   * sources `args` from the established exact argv
//     ([`VERSION_ARGV`] / [`prefetch_argv`] / [`single_eval_argv`] /
//     [`index_eval_argv`]) token-for-token, with NO re-splitting;
//   * sources `env` from [`RealPrivateHome::child_env`] (the five-entry
//     fail-closed environment) exactly;
//   * preserves the supplied nonzero stdout/stderr caps and [`Duration`] timeout
//     exactly; and
//   * maps every [`crate::command::SpecError`] from [`CommandSpec::new`] to
//     [`CommandError::Spec`] via the existing [`From`] impl — NO new error string.
//
// A single small private helper folds the shared shape so the four builders
// stay one line each and add no needless abstraction. The version and prefetch
// builders feed [`execute_version_probe`] and [`execute_verified_prefetch`];
// the two eval builders feed [`execute_real_scenario`].

/// Fold an exact `args` argv and `home`'s complete environment into a validated
/// [`CommandSpec`] rooted at `nix_bin`. PURE: no spawning, no I/O, no shell. Any
/// [`crate::command::SpecError`] from [`CommandSpec::new`] (a non-absolute/empty
/// program or an out-of-range timeout) is mapped to [`CommandError::Spec`] via
/// the existing [`From`] impl — NO new error string is introduced.
fn build_command_spec(
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    args: Vec<OsString>,
) -> Result<CommandSpec, CommandError> {
    CommandSpec::new(
        nix_bin.to_path_buf(),
        args,
        home.child_env(),
        stdout_cap,
        stderr_cap,
        timeout,
    )
    .map_err(CommandError::from)
}

/// Build the EXACT, validated [`CommandSpec`] for the `nix --version` probe.
/// PURE construction only — no spawning, no I/O, no shell. `args` is
/// [`VERSION_ARGV`] converted token-for-token to [`OsString`]; `env` is
/// [`RealPrivateHome::child_env`]; `program` is `nix_bin` verbatim (must be
/// absolute). The supplied nonzero caps and timeout are preserved exactly.
pub(crate) fn version_command_spec(
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
) -> Result<CommandSpec, CommandError> {
    let args: Vec<OsString> = VERSION_ARGV.iter().copied().map(OsString::from).collect();
    build_command_spec(nix_bin, home, stdout_cap, stderr_cap, timeout, args)
}

/// Build the EXACT, validated [`CommandSpec`] for the pinned flake prefetch
/// (`nix flake prefetch --json <flake_ref>`). PURE construction only — no
/// spawning, no I/O, no network/store. `args` is [`prefetch_argv`] (which stays
/// ONLINE: prefetch fetches the flake tarball, so it deliberately omits
/// `--offline`); `env` is [`RealPrivateHome::child_env`]; `program` is `nix_bin`
/// verbatim (must be absolute). The supplied nonzero caps and timeout are
/// preserved exactly.
pub(crate) fn prefetch_command_spec(
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    manifest: &Manifest,
) -> Result<CommandSpec, CommandError> {
    build_command_spec(
        nix_bin,
        home,
        stdout_cap,
        stderr_cap,
        timeout,
        prefetch_argv(manifest),
    )
}

/// Build the EXACT, validated [`CommandSpec`] for the single-attribute
/// reevaluation (`nix … --offline eval --json <single_attr_installable>`). PURE
/// construction only — no spawning, no I/O, no network/store. `args` is
/// [`single_eval_argv`] for `system` (exactly one `--offline`); `env` is
/// [`RealPrivateHome::child_env`]; `program` is `nix_bin` verbatim (must be
/// absolute). The supplied nonzero caps and timeout are preserved exactly.
pub(crate) fn single_eval_command_spec(
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    manifest: &Manifest,
    system: &CheckedSystem<'_>,
) -> Result<CommandSpec, CommandError> {
    build_command_spec(
        nix_bin,
        home,
        stdout_cap,
        stderr_cap,
        timeout,
        single_eval_argv(manifest, system),
    )
}

/// Build the EXACT, validated [`CommandSpec`] for the index-meta projection
/// (`nix … --offline eval --json --apply <INDEX_META_EXPR> <index_installable>`).
/// PURE construction only — no spawning, no I/O, no network/store. `args` is
/// [`index_eval_argv`] for `system` (exactly one `--offline`, with
/// [`INDEX_META_EXPR`] as a SINGLE `--apply` token); `env` is
/// [`RealPrivateHome::child_env`]; `program` is `nix_bin` verbatim (must be
/// absolute). The supplied nonzero caps and timeout are preserved exactly.
pub(crate) fn index_eval_command_spec(
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    manifest: &Manifest,
    system: &CheckedSystem<'_>,
) -> Result<CommandSpec, CommandError> {
    build_command_spec(
        nix_bin,
        home,
        stdout_cap,
        stderr_cap,
        timeout,
        index_eval_argv(manifest, system),
    )
}

// === REAL child-output parsing (PURE) =====================================
//
// The items below are PURE consumers of Real-lane child stdout: they parse
// `nix --version` and `nix flake prefetch --json` output WITHOUT spawning a
// process, touching the filesystem, or producing a report. They depend only on
// `std` and the already-locked `serde_json` crate (NO new dependency). Every
// error is a fixed, BOUNDED message that NEVER echoes raw child output — no
// unbounded or attacker-controlled string reaches logs — satisfying the
// fail-closed, bounded-output contract.
//
// They are crate-private (`pub(crate)`); [`parse_nix_version`] feeds
// [`execute_version_probe`] and [`verify_prefetch`] feeds
// [`execute_verified_prefetch`].

/// Error from [`parse_nix_version`]. Every variant's [`std::fmt::Display`] is a
/// fixed, bounded message that never echoes the parsed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VersionParseError {
    /// stdout was empty (zero bytes).
    Empty,
    /// stdout was not valid UTF-8.
    InvalidUtf8,
    /// stdout did not begin with the exact `nix (Nix) ` prefix.
    BadPrefix,
    /// The `VERSION` token was absent (zero length).
    EmptyVersion,
    /// The `VERSION` token exceeded 64 bytes.
    OversizeVersion,
    /// The `VERSION` token contained a byte outside `[A-Za-z0-9.+-]`
    /// (including spaces, CR, LF, control, or any non-ASCII byte).
    InvalidVersionChar,
}

impl std::fmt::Display for VersionParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "nix version output is empty",
            Self::InvalidUtf8 => "nix version output is not valid UTF-8",
            Self::BadPrefix => "nix version output has an unexpected prefix",
            Self::EmptyVersion => "nix version output is missing the version",
            Self::OversizeVersion => "nix version is longer than 64 bytes",
            Self::InvalidVersionChar => "nix version contains an invalid character",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for VersionParseError {}

/// The exact, fixed `nix --version` stdout prefix: `nix (Nix) ` (10 ASCII
/// bytes). Everything after it, up to an optional single trailing newline, is
/// the `VERSION` token.
const VERSION_PREFIX: &str = "nix (Nix) ";

/// Maximum accepted length of the `VERSION` token, in bytes.
const VERSION_MAX_LEN: usize = 64;

/// Parse the stdout of `nix --version` into the bare version string.
///
/// Accepts EXACTLY `nix (Nix) VERSION` with zero or one final LF (no CR, no
/// other newline). `VERSION` must be `1..=[`VERSION_MAX_LEN`]` bytes, every byte
/// ASCII alphanumeric, dot, plus, or hyphen (`[A-Za-z0-9.+-]`). Everything else
/// — empty input, garbage, multiple lines, invalid UTF-8, embedded spaces, CR,
/// and oversize versions — is rejected with a bounded [`VersionParseError`].
///
/// The returned [`String`] is the `VERSION` token verbatim; no input is ever
/// echoed in an error.
pub(crate) fn parse_nix_version(stdout: &[u8]) -> Result<String, VersionParseError> {
    if stdout.is_empty() {
        return Err(VersionParseError::Empty);
    }
    // Validate UTF-8 on the FULL buffer first: a trailing LF is ASCII and never
    // participates in a multi-byte sequence, so this also covers the stripped
    // body. Invalid UTF-8 anywhere is rejected before any structural check.
    let text = std::str::from_utf8(stdout).map_err(|_| VersionParseError::InvalidUtf8)?;
    // Tolerate at most one trailing LF. Any further LF (or any CR) survives
    // into the version token and is rejected by the byte-class check below.
    let body = text.strip_suffix('\n').unwrap_or(text);
    let version = body
        .strip_prefix(VERSION_PREFIX)
        .ok_or(VersionParseError::BadPrefix)?;
    let len = version.len();
    if len == 0 {
        return Err(VersionParseError::EmptyVersion);
    }
    if len > VERSION_MAX_LEN {
        return Err(VersionParseError::OversizeVersion);
    }
    if !version
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-')
    {
        return Err(VersionParseError::InvalidVersionChar);
    }
    Ok(version.to_string())
}

/// A verified `nix flake prefetch --json` result: the flake NAR hash and the
/// resulting store path, both validated against the pinned manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedPrefetch {
    /// The flake NAR hash reported by prefetch, EXACTLY equal to
    /// [`crate::manifest::NixpkgsSpec::nar_hash`].
    pub(crate) hash: String,
    /// The resulting store path, EXACTLY `/nix/store/<basename>`.
    pub(crate) store_path: String,
}

/// Error from [`verify_prefetch`]. Every variant's [`std::fmt::Display`] is a
/// fixed, bounded message that never echoes raw prefetch output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrefetchError {
    /// stdout was not valid JSON.
    MalformedJson,
    /// The JSON top-level value was not an object.
    NotAnObject,
    /// The required `hash` field was absent.
    HashMissing,
    /// The required `storePath` field was absent.
    StorePathMissing,
    /// The `hash` field was present but not a JSON string.
    HashNotString,
    /// The `storePath` field was present but not a JSON string.
    StorePathNotString,
    /// The `hash` field did not equal the manifest's pinned NAR hash.
    HashMismatch,
    /// The `storePath` was not exactly `/nix/store/<basename>` with a valid
    /// basename (empty, traversal, extra/trailing slash, non-ASCII, …).
    InvalidStorePath,
}

impl std::fmt::Display for PrefetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::MalformedJson => "prefetch output is not valid JSON",
            Self::NotAnObject => "prefetch output is not a JSON object",
            Self::HashMissing => "prefetch output is missing the hash field",
            Self::StorePathMissing => "prefetch output is missing the storePath field",
            Self::HashNotString => "prefetch output hash field is not a string",
            Self::StorePathNotString => "prefetch output storePath field is not a string",
            Self::HashMismatch => "prefetch hash does not match the pinned manifest",
            Self::InvalidStorePath => "prefetch storePath is not a valid /nix/store path",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for PrefetchError {}

/// The fixed `/nix/store/` prefix that every valid store path must begin with.
const STORE_PREFIX: &str = "/nix/store/";

/// Validate a store path is EXACTLY `/nix/store/<basename>`: the basename is
/// nonempty, is not `.` or `..`, contains no `/`, and every byte is visible
/// ASCII (graphic — so no whitespace and no control) — rejecting empty,
/// traversal (`..`), extra/trailing slashes, and non-ASCII basenames.
fn validate_store_path(path: &str) -> Result<(), PrefetchError> {
    let basename = path
        .strip_prefix(STORE_PREFIX)
        .ok_or(PrefetchError::InvalidStorePath)?;
    if basename.is_empty() {
        return Err(PrefetchError::InvalidStorePath);
    }
    // `.` and `..` are otherwise-legal basenames (all graphic, no slash) that
    // would denote the current/parent store dir entry — reject them outright.
    if basename == "." || basename == ".." {
        return Err(PrefetchError::InvalidStorePath);
    }
    // Visible ASCII only: `is_ascii_graphic` excludes whitespace, control, and
    // non-ASCII; the explicit `/` exclusion forbids extra/trailing slashes and
    // any traversal segment beyond the bare `..` handled above.
    if !basename.bytes().all(|b| b.is_ascii_graphic() && b != b'/') {
        return Err(PrefetchError::InvalidStorePath);
    }
    Ok(())
}

/// Parse and verify `nix flake prefetch --json` stdout against the pinned
/// manifest.
///
/// The top-level JSON value must be an OBJECT with string fields `hash` and
/// `storePath` (unrelated extra fields are tolerated). `hash` must EXACTLY
/// equal [`crate::manifest::NixpkgsSpec::nar_hash`] from `manifest`.
/// `storePath` must be exactly `/nix/store/<basename>` with a valid basename
/// (see [`validate_store_path`]). Malformed JSON, wrong/missing field types, a
/// hash mismatch, and any invalid store path are rejected with a bounded
/// [`PrefetchError`] that never includes the raw values.
pub(crate) fn verify_prefetch(
    stdout: &[u8],
    manifest: &Manifest,
) -> Result<VerifiedPrefetch, PrefetchError> {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|_| PrefetchError::MalformedJson)?;
    let obj = value.as_object().ok_or(PrefetchError::NotAnObject)?;
    let hash_value = obj.get("hash").ok_or(PrefetchError::HashMissing)?;
    let hash = hash_value.as_str().ok_or(PrefetchError::HashNotString)?;
    let path_value = obj
        .get("storePath")
        .ok_or(PrefetchError::StorePathMissing)?;
    let store_path = path_value
        .as_str()
        .ok_or(PrefetchError::StorePathNotString)?;
    if hash != manifest.nixpkgs.nar_hash {
        return Err(PrefetchError::HashMismatch);
    }
    validate_store_path(store_path)?;
    Ok(VerifiedPrefetch {
        hash: hash.to_string(),
        store_path: store_path.to_string(),
    })
}

// === REAL version probe / prefetch execution wrappers =====================
//
// The two generic helpers below are the controlled seam between the PURE argv
// / env / CommandSpec assembly above and the Real runner
// [`run_real_with_executor`]: each
// accepts an executor closure (`FnMut(&CommandSpec, TimeFlavor) ->
// Result<CommandOutcome, CommandError>`) and drives EXACTLY one execution of
// the version probe or the flake prefetch through it. They perform NO spawning
// themselves, NO I/O, NO network/store access, and NO global-environment
// mutation — every such effect is deferred to the injected executor. Each
// builds its validated [`CommandSpec`] first and calls the executor EXACTLY
// ONCE, only after the spec builds; a spec-build failure short-circuits BEFORE
// any execution. Every [`RealFailureKind`] variant is a fixed, bounded message
// that NEVER embeds a [`CommandError`], the child's stdout/stderr, a detected
// version string, or any other dynamic text, preserving the bounded-output /
// fail-closed contract of this module.
//
// The returned [`RealFailureKind`] is the CLOSED failure vocabulary defined
// below in the "REAL failure vocabulary" section: each variant maps to a fixed,
// bounded message that NEVER embeds a [`CommandError`], the child's
// stdout/stderr, a detected version string, or any other dynamic text.

/// Probe the `nix` binary's version through `executor` and verify it against
/// the pinned manifest.
///
/// Builds the validated [`version_command_spec`], then calls `executor` EXACTLY
/// ONCE — only after the spec builds. A spec-build error, an executor error, or
/// a non-success exit maps to [`RealFailureKind::DetectNixCommand`]. A
/// [`parse_nix_version`] error, or a detected version unequal to
/// `manifest.nix.version`, maps to [`RealFailureKind::DetectNixVersion`]. On
/// success returns the detected version [`String`] verbatim. Performs NO
/// spawning itself — every effect is deferred to `executor`.
#[allow(clippy::too_many_arguments)] // explicit params are the injectable, audited execution contract / test seam
pub(crate) fn execute_version_probe<F>(
    manifest: &Manifest,
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    flavor: TimeFlavor,
    executor: &mut F,
) -> Result<String, RealFailureKind>
where
    F: FnMut(&CommandSpec, TimeFlavor) -> Result<CommandOutcome, CommandError>,
{
    let spec = version_command_spec(nix_bin, home, stdout_cap, stderr_cap, timeout)
        .map_err(|_| RealFailureKind::DetectNixCommand)?;
    let outcome = executor(&spec, flavor).map_err(|_| RealFailureKind::DetectNixCommand)?;
    if !outcome.status.is_success() {
        return Err(RealFailureKind::DetectNixCommand);
    }
    let detected =
        parse_nix_version(&outcome.stdout).map_err(|_| RealFailureKind::DetectNixVersion)?;
    if detected != manifest.nix.version {
        return Err(RealFailureKind::DetectNixVersion);
    }
    Ok(detected)
}

/// Run the pinned flake prefetch through `executor` and verify its output
/// against the manifest.
///
/// Builds the validated [`prefetch_command_spec`], then calls `executor`
/// EXACTLY ONCE — only after the spec builds. A spec-build error, an executor
/// error, or a non-success exit maps to [`RealFailureKind::PrefetchCommand`].
/// A [`verify_prefetch`] error against `manifest` maps to
/// [`RealFailureKind::PrefetchVerification`]. On success returns `()`. Performs
/// NO spawning itself — every effect is deferred to `executor`.
#[allow(clippy::too_many_arguments)] // explicit params are the injectable, audited execution contract / test seam
pub(crate) fn execute_verified_prefetch<F>(
    manifest: &Manifest,
    nix_bin: &Path,
    home: &RealPrivateHome,
    stdout_cap: NonZeroU64,
    stderr_cap: NonZeroU64,
    timeout: Duration,
    flavor: TimeFlavor,
    executor: &mut F,
) -> Result<(), RealFailureKind>
where
    F: FnMut(&CommandSpec, TimeFlavor) -> Result<CommandOutcome, CommandError>,
{
    let spec = prefetch_command_spec(nix_bin, home, stdout_cap, stderr_cap, timeout, manifest)
        .map_err(|_| RealFailureKind::PrefetchCommand)?;
    let outcome = executor(&spec, flavor).map_err(|_| RealFailureKind::PrefetchCommand)?;
    if !outcome.status.is_success() {
        return Err(RealFailureKind::PrefetchCommand);
    }
    verify_prefetch(&outcome.stdout, manifest)
        .map_err(|_| RealFailureKind::PrefetchVerification)?;
    Ok(())
}

// === REAL sample assembly (PURE bridge) ===================================
//
// The helper below is the PURE bridge from a completed Real-lane
// [`CommandOutcome`] to a report [`Sample`]: it validates the cache label and
// exit status, then maps an accepted outcome onto a fully-measured sample. Like
// the parsers above, it performs NO filesystem or subprocess work and every
// error is a fixed, BOUNDED message that NEVER echoes raw child output (stdout
// or stderr). It is crate-private; [`assemble_real_scenario`] folds each
// accepted observation through it.

/// Error from [`real_sample_from_outcome`]. Every variant's
/// [`std::fmt::Display`] is a fixed, bounded message that never echoes the
/// child's stdout or stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealSampleError {
    /// The cache label was not [`CacheLabel::SourceWarmProcessCold`]: the only
    /// honest Real cache state the harness can establish.
    WrongCache,
    /// The child exited normally with a nonzero code.
    NonzeroExit,
    /// The child was terminated by a signal.
    Signaled,
}

impl std::fmt::Display for RealSampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::WrongCache => "real sample requires a SourceWarmProcessCold cache label",
            Self::NonzeroExit => "real sample child exited with a nonzero status",
            Self::Signaled => "real sample child was terminated by a signal",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RealSampleError {}

/// Build a measured Real [`Sample`] from a completed [`CommandOutcome`].
///
/// This is the PURE bridge from a Real-lane child outcome to a report sample:
/// it validates the cache label and exit status, then maps an accepted outcome
/// onto a fully-measured (non-skipped, exit 0) [`Sample`]. It performs NO
/// filesystem or subprocess work.
///
/// # Acceptance contract
/// * `cache` must be EXACTLY [`CacheLabel::SourceWarmProcessCold`] (the only
///   honest Real cache state the harness can establish); anything else is
///   rejected with [`RealSampleError::WrongCache`].
/// * `outcome.status` must be [`UnixStatus::Exited`] with a zero code. A nonzero
///   exit is rejected with [`RealSampleError::NonzeroExit`] and a signal with
///   [`RealSampleError::Signaled`]. Every error is a fixed, bounded message that
///   never echoes the child's stdout or stderr.
///
/// On success the sample carries `skipped: false` and `exit: 0`, with `wall_ms`,
/// `rss_kb` (from `max_rss_kib`), and `output_bytes` populated from the
/// outcome. `output_bytes` is the saturating sum of stdout and stderr total
/// bytes, so it never overflows.
pub(crate) fn real_sample_from_outcome(
    index: u32,
    record: Record,
    cache: CacheLabel,
    outcome: &CommandOutcome,
) -> Result<Sample, RealSampleError> {
    if cache != CacheLabel::SourceWarmProcessCold {
        return Err(RealSampleError::WrongCache);
    }
    match outcome.status {
        UnixStatus::Exited(0) => {}
        UnixStatus::Exited(_) => return Err(RealSampleError::NonzeroExit),
        UnixStatus::Signaled(_) => return Err(RealSampleError::Signaled),
    }
    Ok(Sample {
        index,
        record,
        skipped: false,
        wall_ms: Some(outcome.wall_ms),
        rss_kb: Some(outcome.max_rss_kib),
        output_bytes: Some(
            outcome
                .stdout_total_bytes
                .saturating_add(outcome.stderr_total_bytes),
        ),
        exit: 0,
        cache,
    })
}

// === REAL scenario assembly (PURE) =======================================
//
// The items below fold a sequence of captured [`RealObservation`]s into a
// completed Real-lane [`Scenario`] validated against an expected
// [`ScenarioDescriptor`]. They are PURE: no spawning, no I/O, no filesystem
// work, and NO reporting side effects. Every [`RealAssemblyError`] is a fixed,
// bounded, ASCII message that NEVER echoes a descriptor string, an
// installable, or the child's stdout/stderr.
//
// [`assemble_real_scenario`] is deliberately NOT a wrapper around the runner's
// `scenario_from_samples`: that helper rejects any scenario whose warmup /
// measured counts fall short of the declaration, so it cannot preserve the
// honest partial observations captured before a separately recorded child
// failure. The Real lane needs exactly that preservation (under
// [`ScenarioRequirement::PartialAllowed`]), so assembly is reimplemented here
// over the raw observations.

/// One captured Real-lane observation: the descriptor it was produced under,
/// its record kind, its phase-local index, its cache label, and the completed
/// child outcome. Pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RealObservation {
    /// Descriptor the observation was produced under; MUST equal the `expected`
    /// descriptor passed to [`assemble_real_scenario`].
    pub(crate) descriptor: ScenarioDescriptor,
    /// Whether this is a warmup or a measured iteration.
    pub(crate) record: Record,
    /// 0-based index WITHIN its phase (warmup or measured). Warmup and measured
    /// indices are each contiguous zero-based, independent of each other and
    /// independent of the global sample index.
    pub(crate) phase_index: u32,
    /// Cache-state claim; MUST be [`CacheLabel::SourceWarmProcessCold`] (the
    /// only honest Real cache state the harness can establish).
    pub(crate) cache: CacheLabel,
    /// The completed child outcome (exit status, captured byte totals, wall-ms,
    /// max-RSS). Its stdout/stderr content is NEVER echoed in an error.
    pub(crate) outcome: CommandOutcome,
}

/// Whether [`assemble_real_scenario`] accepts a valid prefix or requires the
/// exact declared counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScenarioRequirement {
    /// Accept a valid prefix with fewer observations than declared. This is how
    /// the caller preserves the successful observations captured BEFORE a
    /// separately recorded child failure: the honest prefix is folded into a
    /// partial [`Scenario`], and the failure is recorded alongside it. Excess
    /// or invalid observations are still rejected.
    PartialAllowed,
    /// Require EXACTLY `expected.warmup` warmups and `expected.measured`
    /// measured observations; anything fewer is rejected.
    CompleteRequired,
}

/// Error from [`assemble_real_scenario`]. Every variant's
/// [`std::fmt::Display`] is a fixed, bounded, ASCII message that never echoes
/// a descriptor string, an installable, or the child's stdout/stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealAssemblyError {
    /// An observation's descriptor did not equal `expected`.
    DescriptorMismatch,
    /// An observation's cache label was not [`CacheLabel::SourceWarmProcessCold`].
    WrongCache,
    /// Records were out of order: a warmup appeared after a measured record, or
    /// a measured record appeared before all declared warmups had finished.
    RecordOrder,
    /// A phase-local index was not the expected contiguous zero-based value.
    PhaseIndex,
    /// More warmups were observed than `expected.warmup`.
    ExcessWarmup,
    /// More measured observations were observed than `expected.measured`.
    ExcessMeasured,
    /// Under [`ScenarioRequirement::CompleteRequired`], the warmup and/or
    /// measured counts did not match the declaration exactly.
    IncompleteCounts,
    /// A child outcome was rejected by [`real_sample_from_outcome`] (nonzero
    /// exit or signal), or a produced [`Sample`] was not complete.
    ChildOutcomeRejected,
    /// Statistics could not be computed from the measured samples.
    StatsFailure,
}

impl std::fmt::Display for RealAssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::DescriptorMismatch => "real scenario observation descriptor mismatch",
            Self::WrongCache => "real scenario observation cache is not SourceWarmProcessCold",
            Self::RecordOrder => "real scenario records are out of order",
            Self::PhaseIndex => "real scenario phase index is not contiguous",
            Self::ExcessWarmup => "real scenario has more warmups than declared",
            Self::ExcessMeasured => "real scenario has more measured samples than declared",
            Self::IncompleteCounts => "real scenario sample counts do not match the declaration",
            Self::ChildOutcomeRejected => "real scenario child outcome was rejected",
            Self::StatsFailure => "real scenario statistics could not be computed",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RealAssemblyError {}

/// Adapt [`stats::Stats`] and the measured count into a report
/// [`SampleStatistics`]. Local so this PURE module does not reach into the
/// runner's private adapter.
fn to_real_sample_statistics(s: stats::Stats, count: usize) -> SampleStatistics {
    SampleStatistics {
        count: count as u32,
        min: s.min,
        median: s.median,
        p95: s.p95,
        max: s.max,
    }
}

/// Assemble a validated Real [`Scenario`] from `observations` against the
/// `expected` descriptor.
///
/// This is the PURE fold from a sequence of captured [`RealObservation`]s to a
/// report [`Scenario`]. It performs NO spawning, NO I/O, and NO filesystem
/// work; every error is a fixed, bounded, ASCII message.
///
/// # Validation (in slice order, per observation)
/// * The descriptor MUST equal `expected`
///   ([`RealAssemblyError::DescriptorMismatch`]).
/// * The cache MUST be [`CacheLabel::SourceWarmProcessCold`]
///   ([`RealAssemblyError::WrongCache`]).
/// * Warmup `phase_index` values MUST be contiguous zero-based, and so must
///   measured `phase_index` values ([`RealAssemblyError::PhaseIndex`]).
/// * All `expected.warmup` warmups MUST finish before the first measured
///   observation, and no warmup may follow a measured observation
///   ([`RealAssemblyError::RecordOrder`]).
/// * No more than `expected.warmup` warmups or `expected.measured` measured
///   observations are accepted ([`RealAssemblyError::ExcessWarmup`] /
///   [`RealAssemblyError::ExcessMeasured`]).
/// * Each observation is converted with [`real_sample_from_outcome`]; a nonzero
///   exit or signal is rejected ([`RealAssemblyError::ChildOutcomeRejected`]).
///   Every produced [`Sample`] must be non-skipped, exit 0, carry wall / rss /
///   output values, and be labelled `SourceWarmProcessCold`.
///
/// The report [`Sample::index`] is the GLOBAL running sample count (contiguous
/// zero-based in slice order), NOT the phase-local `phase_index`.
///
/// # Completion
/// * [`ScenarioRequirement::PartialAllowed`] accepts a valid prefix with fewer
///   observations than declared — never excess or invalid ones.
/// * [`ScenarioRequirement::CompleteRequired`] additionally requires EXACTLY
///   `expected.warmup` warmups and `expected.measured` measured observations
///   ([`RealAssemblyError::IncompleteCounts`]).
///
/// # Statistics
/// Statistics are computed from the MEASURED samples only via
/// [`stats::compute`] (warmups never affect them). With zero measured samples
/// both `wall` and `rss` are [`None`]; otherwise both are [`Some`]
/// [`SampleStatistics`] carrying `count` / `min` / `median` / `p95` / `max`.
pub(crate) fn assemble_real_scenario(
    expected: &ScenarioDescriptor,
    observations: &[RealObservation],
    requirement: ScenarioRequirement,
) -> Result<Scenario, RealAssemblyError> {
    let mut samples: Vec<Sample> = Vec::with_capacity(observations.len());
    let mut warmup_count: u32 = 0;
    let mut measured_count: u32 = 0;
    let mut seen_measured = false;

    for obs in observations {
        // Every observation's descriptor must equal the expected one.
        if obs.descriptor != *expected {
            return Err(RealAssemblyError::DescriptorMismatch);
        }
        // Every observation must carry the only honest Real cache label.
        if obs.cache != CacheLabel::SourceWarmProcessCold {
            return Err(RealAssemblyError::WrongCache);
        }
        match obs.record {
            Record::Warmup => {
                // No warmup may follow a measured record.
                if seen_measured {
                    return Err(RealAssemblyError::RecordOrder);
                }
                // No more than the declared warmup count.
                if warmup_count >= expected.warmup {
                    return Err(RealAssemblyError::ExcessWarmup);
                }
                // Warmup phase indices are contiguous zero-based.
                if obs.phase_index != warmup_count {
                    return Err(RealAssemblyError::PhaseIndex);
                }
                warmup_count += 1;
            }
            Record::Measured => {
                // ALL declared warmups must finish before the first measured
                // observation.
                if warmup_count != expected.warmup {
                    return Err(RealAssemblyError::RecordOrder);
                }
                // No more than the declared measured count.
                if measured_count >= expected.measured {
                    return Err(RealAssemblyError::ExcessMeasured);
                }
                // Measured phase indices are contiguous zero-based.
                if obs.phase_index != measured_count {
                    return Err(RealAssemblyError::PhaseIndex);
                }
                measured_count += 1;
                seen_measured = true;
            }
        }

        // Convert via the existing PURE bridge. The global index is the running
        // sample count (contiguous zero-based in slice order), NOT the
        // phase-local `phase_index`.
        let global_index = samples.len() as u32;
        let sample = real_sample_from_outcome(global_index, obs.record, obs.cache, &obs.outcome)
            .map_err(|_| RealAssemblyError::ChildOutcomeRejected)?;

        // Defensive re-check of the assembly contract: the produced sample must
        // be complete (non-skipped / exit 0 / wall + rss + output present) and
        // carry the SourceWarmProcessCold label. This is guaranteed by
        // construction via `real_sample_from_outcome`, but enforced here so the
        // contract holds independently of that helper.
        if sample.skipped
            || sample.exit != 0
            || sample.wall_ms.is_none()
            || sample.rss_kb.is_none()
            || sample.output_bytes.is_none()
            || sample.cache != CacheLabel::SourceWarmProcessCold
        {
            return Err(RealAssemblyError::ChildOutcomeRejected);
        }
        samples.push(sample);
    }

    // CompleteRequired demands the exact declared counts; PartialAllowed may
    // fall short (a valid prefix).
    if requirement == ScenarioRequirement::CompleteRequired
        && (warmup_count != expected.warmup || measured_count != expected.measured)
    {
        return Err(RealAssemblyError::IncompleteCounts);
    }

    // Statistics over MEASURED samples only (warmup excluded). Zero measured
    // samples => both statistic blocks are absent. A measured sample missing
    // wall_ms / rss_kb is a contract failure (not silently dropped): each
    // absent metric maps to `RealAssemblyError::ChildOutcomeRejected` and
    // propagates via `?` rather than panicking.
    let measured_wall: Vec<u64> = samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| s.wall_ms.ok_or(RealAssemblyError::ChildOutcomeRejected))
        .collect::<Result<Vec<u64>, _>>()?;
    let measured_rss: Vec<u64> = samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| s.rss_kb.ok_or(RealAssemblyError::ChildOutcomeRejected))
        .collect::<Result<Vec<u64>, _>>()?;

    let statistics = if measured_wall.is_empty() {
        Statistics {
            wall: None,
            rss: None,
        }
    } else {
        let wall_stats =
            stats::compute(&measured_wall).map_err(|_| RealAssemblyError::StatsFailure)?;
        let rss_stats =
            stats::compute(&measured_rss).map_err(|_| RealAssemblyError::StatsFailure)?;
        Statistics {
            wall: Some(to_real_sample_statistics(wall_stats, measured_wall.len())),
            rss: Some(to_real_sample_statistics(rss_stats, measured_rss.len())),
        }
    };

    // Metadata and declared counts come straight from the expected descriptor.
    Ok(Scenario {
        name: expected.name.clone(),
        system: expected.system.clone(),
        installable: expected.installable.clone(),
        warmup: expected.warmup,
        measured: expected.measured,
        samples,
        statistics,
    })
}

// === REAL failure vocabulary (PURE, closed enum) =========================
//
// The items below are the PURE, closed failure vocabulary for a Real run: a
// fixed enum of failure kinds, each mapped to an EXACT stable stage string and
// a fixed message string. [`real_failure`] turns one kind (plus an optional
// scenario descriptor) into a report [`Failure`]. It performs NO spawning, NO
// I/O, and NO filesystem work; its `stage` and `message` come ONLY from the
// closed enum, NEVER from a child's stdout/stderr or any dynamic error text.
// Consequently an adversarial [`CommandOutcome`] (whose `stdout` /
// `cleaned_stderr` may carry hostile bytes) can NEVER reach a recorded failure:
// [`real_failure`] accepts neither an outcome nor error text.

/// The closed, fixed vocabulary of failure kinds a Real run can record. Each
/// variant maps ([`RealFailureKind::stage`] / [`RealFailureKind::message`]) to
/// an EXACT stable stage string and a fixed message string. The mapping is
/// PURE: it never consults a child's stdout/stderr or any dynamic error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealFailureKind {
    /// `nix --version` could not be executed (binary missing / spawn failed).
    DetectNixCommand,
    /// `nix --version` ran but its output failed validation.
    DetectNixVersion,
    /// `nix flake prefetch --json` could not run.
    PrefetchCommand,
    /// `nix flake prefetch --json` output failed verification against the pin.
    PrefetchVerification,
    /// A `nix eval --json` command could not run.
    EvalCommand,
    /// A `nix eval --json` command ran but did not succeed.
    EvalOutcome,
    /// The overall wall-clock deadline for the run expired.
    OverallTimeout,
    /// Captured Real observations could not be assembled into a scenario.
    ScenarioAssembly,
    /// The assembled Real report failed validation.
    ReportAssembly,
}

impl RealFailureKind {
    /// The EXACT stable pipeline-stage string for this kind. PURE: derived only
    /// from the closed enum, never from child output or a dynamic error.
    #[must_use]
    pub(crate) fn stage(self) -> &'static str {
        match self {
            Self::DetectNixCommand | Self::DetectNixVersion => "detect-nix",
            Self::PrefetchCommand | Self::PrefetchVerification => "prefetch",
            Self::EvalCommand | Self::EvalOutcome => "eval",
            Self::OverallTimeout => "overall-timeout",
            Self::ScenarioAssembly => "assemble-scenario",
            Self::ReportAssembly => "assemble-report",
        }
    }

    /// The fixed message string for this kind. PURE: derived only from the
    /// closed enum, never from child output or a dynamic error.
    #[must_use]
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::DetectNixCommand => "failed to execute the pinned Nix binary",
            Self::DetectNixVersion => "pinned Nix version validation failed",
            Self::PrefetchCommand => "pinned Nixpkgs prefetch command failed",
            Self::PrefetchVerification => "pinned Nixpkgs source verification failed",
            Self::EvalCommand => "Nix evaluation command could not run",
            Self::EvalOutcome => "Nix evaluation command did not succeed",
            Self::OverallTimeout => "overall benchmark deadline expired",
            Self::ScenarioAssembly => "scenario observations failed validation",
            Self::ReportAssembly => "Real report failed validation",
        }
    }
}

/// The scenario string recorded when a failure has NO associated descriptor:
/// the literal stable value `run` (the overall run, not any one scenario).
pub(crate) const RUN_SCENARIO: &str = "run";

/// Build a report [`Failure`] from a [`RealFailureKind`] and an optional
/// scenario descriptor.
///
/// * `kind` supplies the EXACT `stage` and fixed `message` (via
///   [`RealFailureKind::stage`] / [`RealFailureKind::message`]); they come ONLY
///   from the closed enum and NEVER from a child's stdout/stderr or any dynamic
///   error text.
/// * `Some(descriptor)` records `descriptor.name.clone()` as the scenario;
///   `None` records [`RUN_SCENARIO`] (`run`).
///
/// Pure: no spawning, no I/O, no filesystem access. An adversarial
/// [`CommandOutcome`] can never reach a recorded failure through this helper,
/// because it accepts neither an outcome nor error text.
#[must_use]
pub(crate) fn real_failure(
    kind: RealFailureKind,
    descriptor: Option<&ScenarioDescriptor>,
) -> Failure {
    Failure {
        scenario: match descriptor {
            Some(d) => d.name.clone(),
            None => RUN_SCENARIO.to_owned(),
        },
        stage: kind.stage().to_owned(),
        message: kind.message().to_owned(),
    }
}

// === REAL report assembly (PURE) =========================================
//
// The items below are the PURE report-assembly sub-slice for a Real run.
// [`assemble_real_report`] folds a manifest, the captured host, the detected
// Nix version, the assembled scenarios, and the recorded failures into a
// validated [`Report`]. It is PURE: no spawning, no I/O, no filesystem work,
// and NO reporting side effects beyond building the [`Report`] value. Every
// [`RealReportError`] is a fixed, bounded, ASCII message that NEVER echoes a
// descriptor string, a scenario name, an installable, a failure message, or
// any other dynamic text, so an adversarial input can never reach a produced
// error.

/// Error from [`assemble_real_report`]. Every variant's [`std::fmt::Display`]
/// is a fixed, bounded, ASCII message that never echoes a descriptor string, a
/// scenario name, an installable, a failure message, or any other dynamic text,
/// so an adversarial input can never reach a produced error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealReportError {
    /// The canonical descriptor plan could not be derived from the manifest.
    DescriptorPlan,
    /// A recorded failure is outside the closed [`RealFailureKind`] table
    /// (arbitrary stage / message / scenario / pairing).
    UnknownFailure,
    /// The scenario set is not an ordered subset of the canonical plan
    /// (missing, extra, duplicate, out-of-order, or unrecognized).
    ScenarioSet,
    /// A scenario's metadata does not match its descriptor.
    ScenarioMetadata,
    /// A scenario's captured shape is dishonest (counts, indices, record
    /// order, sample completeness, cache label, or statistics).
    ScenarioShape,
    /// A Complete report's detected Nix version does not equal the pin.
    NixVersion,
    /// The assembled report failed [`Report::validate`].
    ReportValidation,
}

impl std::fmt::Display for RealReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::DescriptorPlan => "real report could not derive the descriptor plan",
            Self::UnknownFailure => "real report recorded an unknown failure",
            Self::ScenarioSet => "real report scenario set is not an ordered plan subset",
            Self::ScenarioMetadata => "real report scenario metadata does not match its descriptor",
            Self::ScenarioShape => "real report scenario shape is not an honest capture",
            Self::NixVersion => "real report detected Nix version does not match the pin",
            Self::ReportValidation => "real report failed report validation",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RealReportError {}

/// All nine [`RealFailureKind`] variants, for closed-table classification of
/// recorded failures. The order is fixed so the table is exhaustive.
const ALL_REAL_FAILURE_KINDS: [RealFailureKind; 9] = [
    RealFailureKind::DetectNixCommand,
    RealFailureKind::DetectNixVersion,
    RealFailureKind::PrefetchCommand,
    RealFailureKind::PrefetchVerification,
    RealFailureKind::EvalCommand,
    RealFailureKind::EvalOutcome,
    RealFailureKind::OverallTimeout,
    RealFailureKind::ScenarioAssembly,
    RealFailureKind::ReportAssembly,
];

/// The global-scenario kinds: their recorded [`Failure::scenario`] MUST be
/// [`RUN_SCENARIO`] (`run`), because they are not tied to any one descriptor.
const GLOBAL_FAILURE_KINDS: [RealFailureKind; 6] = [
    RealFailureKind::DetectNixCommand,
    RealFailureKind::DetectNixVersion,
    RealFailureKind::PrefetchCommand,
    RealFailureKind::PrefetchVerification,
    RealFailureKind::OverallTimeout,
    RealFailureKind::ReportAssembly,
];

/// The scope a closed-table (stage, message) pair belongs to: global (scenario
/// [`RUN_SCENARIO`]) or per-scenario (a descriptor name from the canonical plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    /// Scenario must be [`RUN_SCENARIO`].
    Global,
    /// Scenario must be a descriptor name from the canonical plan.
    PerScenario,
}

/// Classify a (stage, message) pair against the closed [`RealFailureKind`]
/// table. Returns [`None`] for any pair outside the table (an arbitrary or
/// adversarial stage or message). PURE: derived only from the closed enum.
fn classify_failure(stage: &str, message: &str) -> Option<FailureScope> {
    let kind = ALL_REAL_FAILURE_KINDS
        .iter()
        .copied()
        .find(|&k| k.stage() == stage && k.message() == message)?;
    if GLOBAL_FAILURE_KINDS.contains(&kind) {
        Some(FailureScope::Global)
    } else {
        Some(FailureScope::PerScenario)
    }
}

/// Validate every recorded failure comes from the closed [`RealFailureKind`]
/// table exactly: the (stage, message) pair must match a kind, and the scenario
/// must be [`RUN_SCENARIO`] for a global kind or a descriptor name from `plan`
/// for a per-scenario kind. Any other pairing, stage, message, or scenario is
/// [`RealReportError::UnknownFailure`].
fn validate_failures(
    failures: &[Failure],
    plan: &[ScenarioDescriptor],
) -> Result<(), RealReportError> {
    for f in failures {
        match classify_failure(&f.stage, &f.message) {
            Some(FailureScope::Global) => {
                if f.scenario != RUN_SCENARIO {
                    return Err(RealReportError::UnknownFailure);
                }
            }
            Some(FailureScope::PerScenario) => {
                if !plan.iter().any(|d| d.name == f.scenario) {
                    return Err(RealReportError::UnknownFailure);
                }
            }
            None => return Err(RealReportError::UnknownFailure),
        }
    }
    Ok(())
}

/// Whether a scenario's metadata (name / system / installable / declared
/// warmup / declared measured) exactly matches its descriptor.
fn metadata_matches(scen: &Scenario, desc: &ScenarioDescriptor) -> bool {
    scen.name == desc.name
        && scen.system == desc.system
        && scen.installable == desc.installable
        && scen.warmup == desc.warmup
        && scen.measured == desc.measured
}

/// Match each scenario to its descriptor by name, verify full metadata, and
/// require strictly-increasing descriptor indices (no duplicate, no
/// out-of-order). Returns the matched descriptors in scenario order. An
/// unrecognized scenario name is [`RealReportError::ScenarioSet`]; a metadata
/// mismatch is [`RealReportError::ScenarioMetadata`]; a duplicate or
/// out-of-order match is [`RealReportError::ScenarioSet`].
fn match_plan<'a>(
    scenarios: &[Scenario],
    plan: &'a [ScenarioDescriptor],
) -> Result<Vec<&'a ScenarioDescriptor>, RealReportError> {
    let mut matched: Vec<&'a ScenarioDescriptor> = Vec::with_capacity(scenarios.len());
    let mut last_idx: Option<usize> = None;
    for scen in scenarios {
        let idx = match plan.iter().position(|d| d.name == scen.name) {
            Some(i) => i,
            None => return Err(RealReportError::ScenarioSet),
        };
        if !metadata_matches(scen, &plan[idx]) {
            return Err(RealReportError::ScenarioMetadata);
        }
        if let Some(prev) = last_idx
            && idx <= prev
        {
            return Err(RealReportError::ScenarioSet);
        }
        last_idx = Some(idx);
        matched.push(&plan[idx]);
    }
    Ok(matched)
}

/// Validate the shape of one scenario against its descriptor, shared by the
/// Complete and Incomplete paths. Every sample must be non-skipped, exit 0,
/// carry wall / rss / output, and be labelled [`CacheLabel::SourceWarmProcessCold`];
/// sample indices must be a contiguous in-order `0..N-1`; warmup records must
/// precede measured records; measured records may appear only after ALL declared
/// warmups; warmup / measured counts must not exceed the declaration (and must
/// match it exactly under [`ScenarioRequirement::CompleteRequired`]); and the
/// wall / rss statistics must be exactly recomputed over the measured samples
/// (both [`None`] when there are zero measured samples). Any violation is
/// [`RealReportError::ScenarioShape`].
fn validate_scenario_shape(
    scen: &Scenario,
    desc: &ScenarioDescriptor,
    requirement: ScenarioRequirement,
) -> Result<(), RealReportError> {
    let complete = requirement == ScenarioRequirement::CompleteRequired;

    // Every sample is complete and carries the only honest Real cache label.
    for sample in &scen.samples {
        if sample.skipped
            || sample.exit != 0
            || sample.wall_ms.is_none()
            || sample.rss_kb.is_none()
            || sample.output_bytes.is_none()
            || sample.cache != CacheLabel::SourceWarmProcessCold
        {
            return Err(RealReportError::ScenarioShape);
        }
    }

    // Contiguous in-order global indices: at position `pos` the index MUST
    // equal `pos` (rejects duplicates, gaps, and out-of-order iteration).
    for (pos, sample) in scen.samples.iter().enumerate() {
        if sample.index != pos as u32 {
            return Err(RealReportError::ScenarioShape);
        }
    }

    // Record ordering, measured-after-all-warmups, and count bounds.
    let mut warmup_count: u32 = 0;
    let mut measured_count: u32 = 0;
    let mut seen_measured = false;
    for sample in &scen.samples {
        match sample.record {
            Record::Warmup => {
                if seen_measured {
                    return Err(RealReportError::ScenarioShape);
                }
                warmup_count = warmup_count.saturating_add(1);
            }
            Record::Measured => {
                // Measured records may appear only after ALL declared warmups.
                if warmup_count != desc.warmup {
                    return Err(RealReportError::ScenarioShape);
                }
                measured_count = measured_count.saturating_add(1);
                seen_measured = true;
            }
        }
    }
    if warmup_count > desc.warmup || measured_count > desc.measured {
        return Err(RealReportError::ScenarioShape);
    }
    if complete && (warmup_count != desc.warmup || measured_count != desc.measured) {
        return Err(RealReportError::ScenarioShape);
    }

    // Statistics over MEASURED samples only: both None when there are zero
    // measured samples, otherwise both present and exactly recomputed.
    let mut measured_wall: Vec<u64> = Vec::new();
    let mut measured_rss: Vec<u64> = Vec::new();
    for sample in &scen.samples {
        if sample.record == Record::Measured {
            measured_wall.push(sample.wall_ms.ok_or(RealReportError::ScenarioShape)?);
            measured_rss.push(sample.rss_kb.ok_or(RealReportError::ScenarioShape)?);
        }
    }
    if measured_wall.is_empty() {
        if scen.statistics.wall.is_some() || scen.statistics.rss.is_some() {
            return Err(RealReportError::ScenarioShape);
        }
    } else {
        let wall_stats =
            stats::compute(&measured_wall).map_err(|_| RealReportError::ScenarioShape)?;
        let rss_stats =
            stats::compute(&measured_rss).map_err(|_| RealReportError::ScenarioShape)?;
        let expected_wall = to_real_sample_statistics(wall_stats, measured_wall.len());
        let expected_rss = to_real_sample_statistics(rss_stats, measured_rss.len());
        if scen.statistics.wall.as_ref() != Some(&expected_wall)
            || scen.statistics.rss.as_ref() != Some(&expected_rss)
        {
            return Err(RealReportError::ScenarioShape);
        }
    }

    Ok(())
}

/// Assemble a validated Real [`Report`] from a manifest, the captured host, the
/// detected Nix version, the assembled scenarios, and the recorded failures.
///
/// This is the PURE report-assembly sub-slice: no spawning, no I/O, no
/// filesystem work. Every error is a fixed, bounded, ASCII message.
///
/// # Validation
/// 1. The canonical expected descriptor plan is obtained from
///    [`crate::runner::descriptors`] (NEVER caller-supplied); a failure is
///    [`RealReportError::DescriptorPlan`].
/// 2. Every [`Failure`] must come from the closed [`RealFailureKind`]
///    stage/message table exactly. Global kinds record scenario
///    [`RUN_SCENARIO`]; per-scenario kinds record a descriptor name from the
///    plan. Any other pairing, stage, message, or scenario is
///    [`RealReportError::UnknownFailure`].
/// 3. `completeness` is [`Completeness::Complete`] exactly when `failures` is
///    empty; otherwise [`Completeness::Incomplete`]. Malformed `Complete` data
///    is never silently downgraded.
/// 4. **Complete**: the detected Nix version must equal `manifest.nix.version`
///    exactly ([`RealReportError::NixVersion`]); the scenarios must equal the
///    exact canonical descriptor set in exact order; each scenario's metadata
///    must match its descriptor ([`RealReportError::ScenarioMetadata`]); and
///    each scenario must be full and honest ([`RealReportError::ScenarioShape`]).
/// 5. **Incomplete**: the detected Nix version may be [`None`] or a parsed
///    value; the scenarios may be empty or an ordered subset of the canonical
///    descriptors; and each partial scenario must be an honest prefix with
///    exactly recomputed statistics (or both [`None`] when there are zero
///    measured samples).
/// 6. The [`Pin`] is built exactly from the manifest fields; `mode` is
///    [`Mode::Real`]; `harness_only` is `false`; the detected version,
///    scenarios, and failures are preserved verbatim.
/// 7. [`Report::validate`] is called before returning; any error is mapped to
///    [`RealReportError::ReportValidation`] WITHOUT embedding its dynamic text.
pub(crate) fn assemble_real_report(
    manifest: &Manifest,
    host: Host,
    nix_version: Option<String>,
    scenarios: Vec<Scenario>,
    failures: Vec<Failure>,
) -> Result<Report, RealReportError> {
    // 1. Canonical descriptor plan from the runner (never caller-supplied).
    let plan = crate::runner::descriptors(manifest).map_err(|_| RealReportError::DescriptorPlan)?;

    // 2. Every failure must come from the closed RealFailureKind table exactly.
    validate_failures(&failures, &plan)?;

    // 3. Completeness is Complete exactly when there are no failures.
    let complete = failures.is_empty();
    let completeness = if complete {
        Completeness::Complete
    } else {
        Completeness::Incomplete
    };

    // 4. Complete requires the detected Nix version to equal the pin exactly.
    if complete {
        let detected = nix_version.as_deref().unwrap_or("");
        if detected != manifest.nix.version {
            return Err(RealReportError::NixVersion);
        }
    }

    // 5. Scenario set: exact ordered set (Complete) or ordered subset
    //    (Incomplete), with full metadata matching against the canonical plan.
    let matched = match_plan(&scenarios, &plan)?;
    if complete && scenarios.len() != plan.len() {
        return Err(RealReportError::ScenarioSet);
    }

    // 6. Per-scenario shape validation (shared Complete / Incomplete path).
    for (scen, desc) in scenarios.iter().zip(matched.iter()) {
        let requirement = if complete {
            ScenarioRequirement::CompleteRequired
        } else {
            ScenarioRequirement::PartialAllowed
        };
        validate_scenario_shape(scen, desc, requirement)?;
    }

    // 7. Build the report: pin from the manifest, mode Real, harness_only false.
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: Mode::Real,
        completeness,
        harness_only: false,
        host,
        pin: Pin {
            nix_version: manifest.nix.version.clone(),
            owner: manifest.nixpkgs.owner.clone(),
            repo: manifest.nixpkgs.repo.clone(),
            rev: manifest.nixpkgs.rev.clone(),
            nar_hash: manifest.nixpkgs.nar_hash.clone(),
            attr: manifest.attr.clone(),
        },
        nix_version,
        scenarios,
        failures,
    };

    // 8. Final validation, mapped without embedding dynamic text.
    report
        .validate()
        .map_err(|_| RealReportError::ReportValidation)?;
    Ok(report)
}

// === REAL single-scenario executor =======================================
//
// [`execute_real_scenario`] is the controlled seam that drives EXACTLY ONE
// Real-lane scenario through an injected executor: it validates the descriptor
// system, runs all warmup then all measured iterations (calling the executor
// exactly once per iteration), and folds the result into a
// [`RealScenarioExecution`]. Like the rest of this module it performs NO
// spawning itself — every effect is deferred to the injected executor — and it
// NEVER inspects or echoes eval stdout/stderr, fabricates samples, clears
// caches, builds, shells out, or mutates global state. On any failure it
// returns the honest prefix of observations captured so far plus a closed
// [`RealFailureKind`]. It is called by [`run_real_with_executor`] for each
// descriptor.

/// One completed (possibly partial) execution of a single Real-lane scenario:
/// the captured [`RealObservation`]s and, if the loop stopped early, the closed
/// [`RealFailureKind`] that stopped it. `None` means every declared iteration
/// succeeded.
#[derive(Debug)]
pub(crate) struct RealScenarioExecution {
    /// All observations captured in order, up to success or the first failure.
    pub(crate) observations: Vec<RealObservation>,
    /// The failure kind that stopped the loop, if any; `None` on full success.
    pub(crate) failure: Option<RealFailureKind>,
}

/// Drive EXACTLY one Real-lane scenario through `executor`.
///
/// Validates `descriptor.system` once via [`flakeref::check_system`], then runs
/// ALL `descriptor.warmup` warmup iterations followed by ALL `descriptor.measured`
/// measured iterations (warmup phase-local indices `0..warmup`, then measured
/// phase-local indices `0..measured`), calling `executor` EXACTLY ONCE per
/// iteration. Before every command the per-command timeout is selected via
/// [`crate::runner::select_timeout`] of `Duration::from_secs(descriptor.timeout_seconds)`,
/// `started.elapsed()`, and `overall_timeout`. A single-attribute eval uses
/// [`single_eval_command_spec`] when `single_attr` is true, otherwise
/// [`index_eval_command_spec`]; both pass `descriptor.stdout_cap_bytes` and the
/// supplied `stderr_cap`.
///
/// Returns [`RealScenarioExecution`] with every captured observation and
/// [`RealScenarioExecution::failure`] `None` on full success; on any failure it
/// returns the observations captured so far plus a closed failure kind:
/// [`RealFailureKind::ScenarioAssembly`] (system check),
/// [`RealFailureKind::OverallTimeout`] (deadline), [`RealFailureKind::EvalCommand`]
/// (spec build or executor error), or [`RealFailureKind::EvalOutcome`] (non-success
/// exit). It NEVER inspects or echoes eval stdout/stderr.
///
/// This function performs NO spawning itself — every effect is deferred to the
/// injected `executor`.
///
/// # Caller contract
/// `single_attr` MUST be `true` ONLY for the canonical descriptor at index 0
/// (the single-attribute host scenario); every other descriptor passes `false`
/// for the index-meta projection. The production `executor` spawns a FRESH
/// `nix` process for every call (no reuse), so each iteration is an independent
/// evaluation.
#[allow(clippy::too_many_arguments)] // explicit params are the injectable, audited execution contract / test seam
pub(crate) fn execute_real_scenario<F>(
    manifest: &Manifest,
    nix_bin: &Path,
    home: &RealPrivateHome,
    descriptor: &ScenarioDescriptor,
    single_attr: bool,
    stderr_cap: NonZeroU64,
    overall_timeout: Duration,
    started: Instant,
    flavor: TimeFlavor,
    executor: &mut F,
) -> RealScenarioExecution
where
    F: FnMut(&CommandSpec, TimeFlavor) -> Result<CommandOutcome, CommandError>,
{
    // 1. Validate descriptor.system once. Failure => empty prefix + ScenarioAssembly.
    let system = match flakeref::check_system(manifest, &descriptor.system) {
        Ok(s) => s,
        Err(_) => {
            return RealScenarioExecution {
                observations: Vec::new(),
                failure: Some(RealFailureKind::ScenarioAssembly),
            };
        }
    };

    let mut observations: Vec<RealObservation> = Vec::new();

    // 2. Loop warmups first (phase_index 0..warmup), then measured
    //    (phase_index 0..measured); each phase's indices are contiguous
    //    zero-based. Do NOT use runner::iteration_plan.
    for (record, count) in [
        (Record::Warmup, descriptor.warmup),
        (Record::Measured, descriptor.measured),
    ] {
        for phase_index in 0..count {
            // 3. Select the per-command timeout against the overall deadline.
            let timeout = match crate::runner::select_timeout(
                Duration::from_secs(descriptor.timeout_seconds),
                started.elapsed(),
                overall_timeout,
            ) {
                Ok(t) => t,
                Err(_) => {
                    return RealScenarioExecution {
                        observations,
                        failure: Some(RealFailureKind::OverallTimeout),
                    };
                }
            };

            // 4. Build the validated eval CommandSpec (single-attr vs index-meta).
            let spec = if single_attr {
                single_eval_command_spec(
                    nix_bin,
                    home,
                    descriptor.stdout_cap_bytes,
                    stderr_cap,
                    timeout,
                    manifest,
                    &system,
                )
            } else {
                index_eval_command_spec(
                    nix_bin,
                    home,
                    descriptor.stdout_cap_bytes,
                    stderr_cap,
                    timeout,
                    manifest,
                    &system,
                )
            };
            let spec = match spec {
                Ok(s) => s,
                Err(_) => {
                    return RealScenarioExecution {
                        observations,
                        failure: Some(RealFailureKind::EvalCommand),
                    };
                }
            };

            // 5. Execute EXACTLY once for this iteration.
            let outcome = match executor(&spec, flavor) {
                Ok(o) => o,
                Err(_) => {
                    return RealScenarioExecution {
                        observations,
                        failure: Some(RealFailureKind::EvalCommand),
                    };
                }
            };
            if !outcome.is_success() {
                return RealScenarioExecution {
                    observations,
                    failure: Some(RealFailureKind::EvalOutcome),
                };
            }

            // 6. Success pushes a SourceWarmProcessCold observation.
            observations.push(RealObservation {
                descriptor: descriptor.clone(),
                record,
                phase_index,
                cache: CacheLabel::SourceWarmProcessCold,
                outcome,
            });
        }
    }

    // 7. Full success: all observations, no failure.
    RealScenarioExecution {
        observations,
        failure: None,
    }
}

// === REAL run orchestration ==============================================
//
// [`run_real`] is the PUBLIC entry point that drives the ENTIRE Real-lane
// pipeline end-to-end: a private fail-closed workspace, a version probe, a
// verified flake prefetch, then the canonical descriptor scenarios — each
// through the production executor [`crate::execute::run`] under
// `/usr/bin/time`. It performs NO direct output logging, leaks NO child
// stdout/stderr, and touches NO Nix/store/network beyond the injected
// production executor. Command / scenario failures always collapse to a
// validated Incomplete [`Report`]; only a private-home failure, a preparation
// failure, or an internal fallback-assembly failure surfaces as a
// [`RealRunError`].
//
// [`run_real_with_executor`] is the private generic core, parameterized over
// an executor closure so the exact composition is exercisable with NO real
// spawning (it performs NO spawning itself — every effect is deferred to the
// injected executor). [`assemble_or_fallback`] is the small private helper
// that first assembles the honest captured data and, on ANY assembly /
// validation failure, discards the candidate and emits ONE minimal validated
// Incomplete fallback [`Report`] — so the lane NEVER returns an unvalidated or
// partially-built [`Report`], and no copy-pasted assembly can drift.

/// The closed, fixed error vocabulary for a Real run's OWN failures (NOT the
/// command / scenario failures folded into an Incomplete [`Report`]). It has
/// no fields and carries no dynamic strings: every variant's
/// [`std::fmt::Display`] is a fixed, bounded, ASCII message.
///
/// * [`RealRunError::PrivateHome`] — the private fail-closed workspace could
///   not be created.
/// * [`RealRunError::Preparation`] — an invariant / preparation step (host
///   system, descriptor plan, or a manifest cap conversion) failed.
/// * [`RealRunError::ReportFallback`] — even the minimal validated fallback
///   [`Report`] could not be assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealRunError {
    /// The private fail-closed workspace could not be created.
    PrivateHome,
    /// An invariant / preparation step (host system, descriptor plan, or a
    /// manifest cap conversion) failed.
    Preparation,
    /// Even the minimal validated fallback [`Report`] could not be assembled.
    ReportFallback,
}

impl std::fmt::Display for RealRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::PrivateHome => "real run could not create a private home directory",
            Self::Preparation => "real run preparation failed",
            Self::ReportFallback => "real run fallback report could not be assembled",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for RealRunError {}

/// Assemble a validated Real [`Report`] from the honest captured data, or — on
/// ANY assembly or validation failure — discard the candidate and emit ONE
/// minimal validated Incomplete fallback [`Report`] (same manifest / host,
/// `nix_version = None`, no scenarios, a single global
/// [`RealFailureKind::ReportAssembly`] failure). Only a failure of that
/// fallback yields [`RealRunError::ReportFallback`].
///
/// Pure with respect to I/O: it never spawns, never logs, and never touches the
/// network or store. It is the single seam through which every Real-lane early
/// return and the final report leave the orchestrator.
fn assemble_or_fallback(
    manifest: &Manifest,
    host: &Host,
    nix_version: Option<&str>,
    scenarios: &[Scenario],
    failures: &[Failure],
) -> Result<Report, RealRunError> {
    if let Ok(report) = assemble_real_report(
        manifest,
        host.clone(),
        nix_version.map(str::to_owned),
        scenarios.to_vec(),
        failures.to_vec(),
    ) {
        return Ok(report);
    }
    // Honest assembly failed: discard the candidate and emit ONE minimal
    // validated Incomplete fallback carrying a single global ReportAssembly
    // failure. Only a failure of THIS fallback yields ReportFallback.
    let fallback_failures = vec![real_failure(RealFailureKind::ReportAssembly, None)];
    assemble_real_report(manifest, host.clone(), None, Vec::new(), fallback_failures)
        .map_err(|_| RealRunError::ReportFallback)
}

/// Drive the ENTIRE Real-lane pipeline end-to-end against the pinned manifest
/// and the production executor [`crate::execute::run`], returning a validated
/// [`Report`] (Complete on full success, Incomplete on any folded failure).
///
/// `nix_bin` is the absolute `nix` binary path, passed VERBATIM to every
/// [`CommandSpec`] (NO shell, NO `PATH` search). The run performs NO direct
/// output logging, leaks NO child stdout/stderr, and touches NO
/// Nix/store/network beyond the injected production executor.
///
/// Only a private-home failure ([`RealRunError::PrivateHome`]), a preparation
/// failure ([`RealRunError::Preparation`]), or an internal fallback-assembly
/// failure ([`RealRunError::ReportFallback`]) surfaces as a [`RealRunError`];
/// every command / scenario failure collapses to a validated Incomplete
/// [`Report`].
pub fn run_real(nix_bin: &Path) -> Result<Report, RealRunError> {
    let started = Instant::now();
    let mut executor = crate::execute::run;
    run_real_with_executor(nix_bin, started, &mut executor)
}

/// The private generic core of [`run_real`], parameterized over an `executor`
/// closure so the exact composition is exercisable with NO real spawning. It
/// performs NO spawning itself — every effect is deferred to `executor`.
///
/// # Composition
/// 1. Preparation: the pinned manifest, the captured host, the canonical
///    descriptor plan, the host `/usr/bin/time` dialect, the shared nonzero
///    single-attr stdout cap and shared nonzero stderr cap, and the overall
///    wall-clock budget. Any invariant / preparation error maps to
///    [`RealRunError::Preparation`]. Exactly ONE private fail-closed workspace
///    is created ([`RealPrivateHome::create`]); a creation failure maps to
///    [`RealRunError::PrivateHome`].
/// 2. Version phase: the per-command budget is the single-attr phase budget
///    selected against the overall deadline; caps are the shared single-attr
///    stdout cap and the shared stderr cap. A timeout-selection failure yields
///    an Incomplete report with a single global [`RealFailureKind::OverallTimeout`].
///    Any version-probe failure kind yields an Incomplete report with that kind
///    as a single global failure, no detected `nix_version`, and no scenarios.
/// 3. Prefetch phase: the per-command budget is the index-meta phase budget (a
///    deliberate 10-minute setup / network ceiling) selected against the
///    overall deadline; caps are the same shared single-attr stdout cap and
///    shared stderr cap. A timeout-selection failure yields an Incomplete
///    report with a single global [`RealFailureKind::OverallTimeout`]. Any
///    prefetch failure kind yields an Incomplete report preserving the EXACT
///    detected `nix_version`, with that kind as a single global failure and no
///    scenarios.
/// 4. Scenario phase: iterate the canonical descriptors in order. The
///    descriptor at position 0 is the host single-attribute scenario
///    (`single_attr = true`); every later descriptor is an index-meta
///    projection (`single_attr = false`). Each shares the same stderr cap,
///    overall budget, start instant, time flavor, and executor.
/// 5. Each descriptor's captured prefix is folded with [`assemble_real_scenario`]:
///    [`ScenarioRequirement::CompleteRequired`] iff the loop succeeded fully
///    (no failure), else [`ScenarioRequirement::PartialAllowed`]. On success
///    the scenario is pushed; on an assembly error a per-scenario
///    [`RealFailureKind::ScenarioAssembly`] is recorded and NO malformed
///    scenario is fabricated or pushed.
/// 6. An execution failure is recorded after the fold: [`RealFailureKind::OverallTimeout`]
///    is a GLOBAL failure and STOPS all remaining descriptors; every other kind
///    is a per-scenario failure and CONTINUES to later descriptors. If BOTH
///    the prefix assembly and the execution failed, both closed failures are
///    recorded.
/// 7. Before the first final report assembly, if the overall deadline has been
///    crossed and no global [`RealFailureKind::OverallTimeout`] was already
///    recorded, one is added.
/// 8. [`assemble_or_fallback`] assembles the honest report from the captured
///    `nix_version` / scenarios / failures.
/// 9. AFTER assembly the deadline is re-checked: if it has now been crossed
///    and no [`RealFailureKind::OverallTimeout`] was already recorded, a global
///    one is added to the ORIGINAL honest data and [`assemble_or_fallback`] is
///    re-run ONCE, returning Incomplete. This prevents a Complete report after
///    the final folding crosses the budget.
fn run_real_with_executor<F>(
    nix_bin: &Path,
    started: Instant,
    executor: &mut F,
) -> Result<Report, RealRunError>
where
    F: FnMut(&CommandSpec, TimeFlavor) -> Result<CommandOutcome, CommandError>,
{
    // 1. Preparation: manifest, host, canonical descriptor plan, host time
    //    dialect, shared single-attr stdout cap, shared stderr cap, overall
    //    budget. Any invariant / preparation error => Preparation. Exactly ONE
    //    private fail-closed workspace; a creation failure => PrivateHome.
    let manifest = benchmark_manifest();
    let host = host().map_err(|_| RealRunError::Preparation)?;
    let descriptors = descriptors(manifest).map_err(|_| RealRunError::Preparation)?;
    let flavor = host_time_flavor();
    let stdout_cap = nz_cap(
        manifest.caps.single_attr_stdout_bytes,
        ManifestField::SingleAttrStdoutCap,
    )
    .map_err(|_| RealRunError::Preparation)?;
    let stderr_cap = nz_cap(manifest.caps.stderr_bytes, ManifestField::StderrCap)
        .map_err(|_| RealRunError::Preparation)?;
    let overall = Duration::from_secs(manifest.timeouts.overall_seconds);
    let home = RealPrivateHome::create().map_err(|_| RealRunError::PrivateHome)?;

    // 2. Version phase: single-attr phase budget, shared single-attr stdout cap
    //    + shared stderr cap. Timeout-selection failure => Incomplete report
    //    with a single global OverallTimeout. Any probe failure kind =>
    //    Incomplete report with that kind as a single global failure, no
    //    detected nix_version, no scenarios.
    let version_timeout = match select_timeout(
        Duration::from_secs(manifest.timeouts.single_attr_seconds),
        started.elapsed(),
        overall,
    ) {
        Ok(t) => t,
        Err(_) => {
            let failures = vec![real_failure(RealFailureKind::OverallTimeout, None)];
            return assemble_or_fallback(manifest, &host, None, &[], &failures);
        }
    };
    let nix_version = match execute_version_probe(
        manifest,
        nix_bin,
        &home,
        stdout_cap,
        stderr_cap,
        version_timeout,
        flavor,
        executor,
    ) {
        Ok(v) => Some(v),
        Err(kind) => {
            let failures = vec![real_failure(kind, None)];
            return assemble_or_fallback(manifest, &host, None, &[], &failures);
        }
    };

    // 3. Prefetch phase: index-meta phase budget (a deliberate 10-minute
    //    setup / network ceiling), same shared single-attr stdout cap + shared
    //    stderr cap. Timeout-selection failure => Incomplete report with a
    //    single global OverallTimeout. Any prefetch failure kind => Incomplete
    //    report preserving the EXACT detected nix_version, that kind as a
    //    single global failure, no scenarios.
    let prefetch_timeout = match select_timeout(
        Duration::from_secs(manifest.timeouts.index_seconds),
        started.elapsed(),
        overall,
    ) {
        Ok(t) => t,
        Err(_) => {
            let failures = vec![real_failure(RealFailureKind::OverallTimeout, None)];
            return assemble_or_fallback(manifest, &host, nix_version.as_deref(), &[], &failures);
        }
    };
    if let Err(kind) = execute_verified_prefetch(
        manifest,
        nix_bin,
        &home,
        stdout_cap,
        stderr_cap,
        prefetch_timeout,
        flavor,
        executor,
    ) {
        let failures = vec![real_failure(kind, None)];
        return assemble_or_fallback(manifest, &host, nix_version.as_deref(), &[], &failures);
    }

    // 4. Scenario phase: iterate canonical descriptors in order. Position 0 is
    //    the host single-attribute scenario (single_attr = true); every later
    //    descriptor is an index-meta projection (single_attr = false). Each
    //    shares the same stderr cap, overall budget, start instant, time
    //    flavor, and executor.
    let mut scenarios: Vec<Scenario> = Vec::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut overall_timeout_recorded = false;
    for (position, descriptor) in descriptors.iter().enumerate() {
        let single_attr = position == 0;
        let execution = execute_real_scenario(
            manifest,
            nix_bin,
            &home,
            descriptor,
            single_attr,
            stderr_cap,
            overall,
            started,
            flavor,
            executor,
        );

        // 5. Fold the captured prefix: CompleteRequired iff the loop succeeded
        //    fully (no failure), else PartialAllowed. On success push the
        //    scenario; on an assembly error push a per-scenario ScenarioAssembly
        //    failure and do NOT fabricate or push a malformed scenario.
        let requirement = if execution.failure.is_none() {
            ScenarioRequirement::CompleteRequired
        } else {
            ScenarioRequirement::PartialAllowed
        };
        match assemble_real_scenario(descriptor, &execution.observations, requirement) {
            Ok(scenario) => scenarios.push(scenario),
            Err(_) => failures.push(real_failure(
                RealFailureKind::ScenarioAssembly,
                Some(descriptor),
            )),
        }

        // 6. Record the execution failure (if any). OverallTimeout is GLOBAL
        //    and STOPS all remaining descriptors; every other kind is
        //    per-scenario and CONTINUES to later descriptors. (If BOTH the
        //    prefix assembly and the execution failed, both closed failures
        //    are recorded.)
        if let Some(kind) = execution.failure {
            if kind == RealFailureKind::OverallTimeout {
                failures.push(real_failure(kind, None));
                overall_timeout_recorded = true;
                break;
            }
            failures.push(real_failure(kind, Some(descriptor)));
        }
    }

    // 7. Before the first final report assembly: if the overall deadline has
    //    been crossed and no global OverallTimeout was already recorded, add
    //    one now.
    if started.elapsed() >= overall && !overall_timeout_recorded {
        failures.push(real_failure(RealFailureKind::OverallTimeout, None));
        overall_timeout_recorded = true;
    }

    // 8. Assemble the honest report (or the minimal fallback) from the
    //    captured nix_version / scenarios / failures, supplied verbatim.
    let report = assemble_or_fallback(
        manifest,
        &host,
        nix_version.as_deref(),
        &scenarios,
        &failures,
    )?;

    // 9. AFTER assembly, re-check the deadline. If it has now been crossed and
    //    no OverallTimeout was already recorded, add a global OverallTimeout
    //    to the ORIGINAL honest data and re-run assemble_or_fallback ONCE,
    //    returning Incomplete. This prevents a Complete report after the final
    //    folding crosses the budget.
    if started.elapsed() >= overall && !overall_timeout_recorded {
        failures.push(real_failure(RealFailureKind::OverallTimeout, None));
        return assemble_or_fallback(
            manifest,
            &host,
            nix_version.as_deref(),
            &scenarios,
            &failures,
        );
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandError, CommandOutcome, CommandSpec, SpecError, UnixStatus};
    use crate::flakeref::check_system;
    use crate::manifest::benchmark_manifest;
    use crate::report::{CacheLabel, Record, SampleStatistics};
    use crate::runner::{ScenarioDescriptor, descriptors, host_system};
    use crate::stats;
    use serde_json::json;
    use std::ffi::OsStr;
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// The pinned flake reference, identical to the literal pinned in
    /// `flakeref.rs`. Hard-coded here so the prefetch/eval tests are
    /// INDEPENDENT regression nets (a drift in `flakeref` would change the
    /// installable, and this literal would break).
    const FLAKE_REF: &str = concat!(
        "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446",
        "?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth%2F3Xuw%3D",
    );

    /// Build a `Vec<OsString>` from `&str` slices for readable expected argv.
    fn oss(parts: &[&str]) -> Vec<OsString> {
        parts.iter().copied().map(OsString::from).collect()
    }

    // === version_argv =======================================================

    #[test]
    fn version_argv_is_exactly_version() {
        assert_eq!(VERSION_ARGV, &["--version"]);
        assert_eq!(VERSION_ARGV.len(), 1);
    }

    // === prefetch_argv ======================================================

    #[test]
    fn prefetch_argv_is_exact() {
        let manifest = benchmark_manifest();
        let expected: Vec<OsString> = oss(&[
            "--extra-experimental-features",
            "nix-command flakes",
            "flake",
            "prefetch",
            "--json",
            FLAKE_REF,
        ]);
        assert_eq!(prefetch_argv(manifest), expected);
    }

    // === single_eval_argv ===================================================

    #[test]
    fn single_eval_argv_is_exact() {
        let manifest = benchmark_manifest();
        let system = check_system(manifest, "x86_64-linux").unwrap();
        let single_attr = format!("{FLAKE_REF}#legacyPackages.x86_64-linux.ripgrep.drvPath");
        let mut expected = oss(&[
            "--extra-experimental-features",
            "nix-command flakes",
            "--offline",
            "eval",
            "--json",
        ]);
        expected.push(OsString::from(single_attr.as_str()));
        assert_eq!(single_eval_argv(manifest, &system), expected);
    }

    // === index_eval_argv ====================================================

    #[test]
    fn index_eval_argv_is_exact() {
        let manifest = benchmark_manifest();
        let system = check_system(manifest, "x86_64-linux").unwrap();
        let index_installable = format!("{FLAKE_REF}#legacyPackages.x86_64-linux");
        let got = index_eval_argv(manifest, &system);
        assert_eq!(got.len(), 8);

        let mut expected = oss(&[
            "--extra-experimental-features",
            "nix-command flakes",
            "--offline",
            "eval",
            "--json",
            "--apply",
        ]);
        expected.push(OsString::from(INDEX_META_EXPR));
        expected.push(OsString::from(index_installable.as_str()));
        assert_eq!(got, expected);

        // The projection expression is a SINGLE argv token (exactly one
        // element), verbatim from the embedded file.
        assert_eq!(got[6], OsString::from(INDEX_META_EXPR));
        // The installable is the final token.
        assert_eq!(got[7], OsString::from(index_installable.as_str()));
    }

    #[test]
    fn eval_argv_wires_every_manifest_system() {
        // For every manifest system, single/index end with the correct
        // flakeref-built installable (cross-checks the system plumbing).
        let manifest = benchmark_manifest();
        for system in &manifest.systems {
            let checked = check_system(manifest, system).unwrap();
            let single = single_eval_argv(manifest, &checked);
            let index = index_eval_argv(manifest, &checked);
            assert_eq!(
                single.last().map(|s| s.as_os_str()),
                Some(OsStr::new(
                    flakeref::single_attr_installable(manifest, &checked).as_str()
                )),
                "single_eval ends with the single-attr installable for {system}"
            );
            assert_eq!(
                index.last().map(|s| s.as_os_str()),
                Some(OsStr::new(
                    flakeref::index_installable(manifest, &checked).as_str()
                )),
                "index_eval ends with the index installable for {system}"
            );
            // Both share the exact 5-token pure-eval prefix.
            assert_eq!(
                &single[..5],
                oss(&[
                    "--extra-experimental-features",
                    "nix-command flakes",
                    "--offline",
                    "eval",
                    "--json",
                ]),
            );
            assert_eq!(single[..5], index[..5]);
        }
    }

    // === INDEX_META_EXPR: wiring + single-token =============================

    #[test]
    fn index_meta_expr_is_wired_to_file_and_single_token() {
        // Re-derive the expression from the file: proves include_str! points at
        // the maintained nix/index-meta.nix (a path/typo drift would break it).
        assert_eq!(INDEX_META_EXPR, include_str!("../nix/index-meta.nix"));
        // Non-empty and recognizable: the projection opens with `pkgs:` (its
        // single function parameter) after the leading comment block.
        assert!(!INDEX_META_EXPR.is_empty());
        assert!(INDEX_META_EXPR.contains("pkgs:"));
        // Single token: no NUL byte (which cannot appear in an argv element on
        // POSIX), so the whole multi-line file is one passable argv value.
        assert!(!INDEX_META_EXPR.contains('\0'));
    }

    // === forbidden tokens (argv skeleton; expression payload excluded) ======

    /// The argv SKELETON must never carry an impurity / build / channel /
    /// substituter / shell / URL token. The `--apply` expression payload is
    /// excluded because it is the maintained Nix projection — it legitimately
    /// *discusses* `--impure`/`NIX_PATH` in its comments — and is covered by
    /// its own wiring/single-token test, not by this command-skeleton purity
    /// check.
    #[test]
    fn argv_skeleton_forbids_impure_build_shell_tokens() {
        const FORBIDDEN: &[&str] = &[
            "--impure",
            "--build",
            "nix-build",
            "--substituter",
            "NIX_PATH",
            "http://",
            "https://",
            "$(",
            "`",
        ];
        let manifest = benchmark_manifest();

        // Collect every argv element across all commands and all manifest
        // systems, EXCLUDING the --apply expression payload.
        let mut skeleton: Vec<String> = Vec::new();
        skeleton.extend(VERSION_ARGV.iter().copied().map(String::from));
        skeleton.extend(
            prefetch_argv(manifest)
                .iter()
                .map(|s| s.to_string_lossy().into_owned()),
        );
        for system in &manifest.systems {
            let checked = check_system(manifest, system).unwrap();
            for e in single_eval_argv(manifest, &checked) {
                skeleton.push(e.to_string_lossy().into_owned());
            }
            for e in index_eval_argv(manifest, &checked) {
                let lossy = e.to_string_lossy().into_owned();
                if lossy != INDEX_META_EXPR {
                    skeleton.push(lossy);
                }
            }
        }

        assert!(!skeleton.is_empty(), "skeleton must cover every command");
        for token in &skeleton {
            for &bad in FORBIDDEN {
                assert!(
                    !token.contains(bad),
                    "argv skeleton token {token:?} contains forbidden token {bad:?}",
                );
            }
        }
    }

    // === real_child_env: exact entries + no inheritance =====================

    #[test]
    fn real_child_env_is_exact_five_entries_no_inheritance() {
        let home = PathBuf::from("/tmp/s4-private-home");
        let env = real_child_env(&home);

        // Exactly five entries — nothing inherited from the parent process.
        assert_eq!(env.len(), 5, "exactly five entries, nothing inherited");

        // Exact values.
        assert_eq!(
            env.get(OsStr::new("LANG")),
            Some(&OsString::from("C")),
            "LANG=C",
        );
        assert_eq!(
            env.get(OsStr::new("LC_ALL")),
            Some(&OsString::from("C")),
            "LC_ALL=C",
        );
        assert_eq!(
            env.get(OsStr::new("HOME")),
            Some(&OsString::from("/tmp/s4-private-home")),
            "HOME=<private_home>",
        );
        assert_eq!(
            env.get(OsStr::new("XDG_CACHE_HOME")),
            Some(&OsString::from("/tmp/s4-private-home/cache")),
            "XDG_CACHE_HOME=<private_home>/cache",
        );
        assert_eq!(
            env.get(OsStr::new("XDG_CONFIG_HOME")),
            Some(&OsString::from("/tmp/s4-private-home/config")),
            "XDG_CONFIG_HOME=<private_home>/config",
        );

        // No PATH, no NIX_PATH, no TMPDIR, no USER/TERM — nothing inherited.
        for absent in ["PATH", "NIX_PATH", "TMPDIR", "TERM", "USER"] {
            assert!(
                !env.contains_key(OsStr::new(absent)),
                "no inherited {absent}",
            );
        }

        // Deterministic iteration order (BTreeMap, sorted by OsString bytes):
        // HOME, LANG, LC_ALL, XDG_CACHE_HOME, XDG_CONFIG_HOME.
        let keys: Vec<&OsString> = env.keys().collect();
        assert_eq!(
            keys,
            vec![
                &OsString::from("HOME"),
                &OsString::from("LANG"),
                &OsString::from("LC_ALL"),
                &OsString::from("XDG_CACHE_HOME"),
                &OsString::from("XDG_CONFIG_HOME"),
            ],
        );
    }

    // === CommandSpec builders (PURE, non-executing) =======================
    //
    // Focused tests for the four `*_command_spec` builders. They discriminate
    // the EXACT program/argv/env/caps/timeout shape, the online/offline split,
    // the relative-path rejection, and the non-execution of the program path —
    // WITHOUT re-deriving the extensive argv-purity coverage already above.

    /// Small, distinguishable nonzero caps plus a short timeout, all within
    /// [`CommandSpec`] bounds (1 ms..=1 h). Distinct values so a swap is caught.
    fn spec_caps() -> (NonZeroU64, NonZeroU64, Duration) {
        (
            NonZeroU64::new(4_096).expect("nonzero stdout cap"),
            NonZeroU64::new(1_024).expect("nonzero stderr cap"),
            Duration::from_secs(2),
        )
    }

    /// A guaranteed-absent ABSOLUTE path (so [`CommandSpec`] validates the
    /// program field) that no Real builder ever needs to exist. It is a direct
    /// child of `home`'s freshly-created root, whose test invariant guarantees
    /// ONLY `cache` and `config` exist under it — so this path is absent and
    /// absolute. Building specs against it must NOT execute or create it.
    fn nonexistent_nix_bin(home: &RealPrivateHome) -> PathBuf {
        home.root().join("missing-nix-bin")
    }

    /// Count occurrences of the exact token `--offline` in an argv.
    fn count_offline(args: &[OsString]) -> usize {
        args.iter()
            .filter(|a| a.as_os_str() == OsStr::new("--offline"))
            .count()
    }

    /// Assert `result` rejects with [`CommandError::Spec`]`(`[`SpecError::ProgramNotAbsolute`]`)`
    /// whose bounded `got` snippet equals `bad`, independent of any OS execution.
    fn assert_program_not_absolute(result: Result<CommandSpec, CommandError>, bad: &str) {
        match result {
            Err(CommandError::Spec(SpecError::ProgramNotAbsolute { got })) => {
                assert_eq!(
                    got, bad,
                    "bounded snippet must equal the short relative path exactly",
                );
            }
            other => panic!("expected ProgramNotAbsolute for {bad:?}, got {other:?}"),
        }
    }

    // 1. Each builder returns the exact program/argv/env/caps/timeout.

    #[test]
    fn version_command_spec_is_exact() {
        let home = RealPrivateHome::create().expect("create home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let spec =
            version_command_spec(&nix_bin, &home, stdout_cap, stderr_cap, timeout).expect("ok");

        assert_eq!(spec.program, nix_bin);
        assert_eq!(spec.args, oss(&["--version"]));
        assert_eq!(spec.env, home.child_env());
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert_eq!(spec.timeout, timeout);
        // The builder did NOT execute the (nonexistent) program.
        assert!(!nix_bin.exists());
    }

    #[test]
    fn prefetch_command_spec_is_exact() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("create home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let spec =
            prefetch_command_spec(&nix_bin, &home, stdout_cap, stderr_cap, timeout, manifest)
                .expect("ok");

        assert_eq!(spec.program, nix_bin);
        assert_eq!(spec.args, prefetch_argv(manifest));
        assert_eq!(spec.env, home.child_env());
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert_eq!(spec.timeout, timeout);
        assert!(!nix_bin.exists());
    }

    #[test]
    fn single_eval_command_spec_is_exact_for_host_system() {
        let manifest = benchmark_manifest();
        let host_sys = host_system().expect("host target is supported");
        let system = check_system(manifest, host_sys).expect("host system is in the manifest");
        let home = RealPrivateHome::create().expect("create home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let spec = single_eval_command_spec(
            &nix_bin, &home, stdout_cap, stderr_cap, timeout, manifest, &system,
        )
        .expect("ok");

        assert_eq!(spec.program, nix_bin);
        assert_eq!(spec.args, single_eval_argv(manifest, &system));
        assert_eq!(spec.env, home.child_env());
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert_eq!(spec.timeout, timeout);
        assert!(!nix_bin.exists());
    }

    #[test]
    fn index_eval_command_spec_is_exact_for_every_manifest_system() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("create home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        for sys in &manifest.systems {
            let checked = check_system(manifest, sys).expect("manifest system");
            let spec = index_eval_command_spec(
                &nix_bin, &home, stdout_cap, stderr_cap, timeout, manifest, &checked,
            )
            .expect("ok");
            assert_eq!(spec.program, nix_bin, "program for {sys}");
            assert_eq!(
                spec.args,
                index_eval_argv(manifest, &checked),
                "argv for {sys}"
            );
            assert_eq!(spec.env, home.child_env(), "env for {sys}");
            assert_eq!(spec.stdout_cap, stdout_cap, "stdout cap for {sys}");
            assert_eq!(spec.stderr_cap, stderr_cap, "stderr cap for {sys}");
            assert_eq!(spec.timeout, timeout, "timeout for {sys}");
        }
        assert!(!nix_bin.exists());
    }

    // 2. Online/offline split: prefetch online, eval exactly one --offline,
    //    version exactly --version.

    #[test]
    fn version_spec_arg_is_exactly_version() {
        let home = RealPrivateHome::create().expect("home");
        let (so, se, t) = spec_caps();
        let spec = version_command_spec(&nonexistent_nix_bin(&home), &home, so, se, t).unwrap();
        assert_eq!(spec.args, oss(&["--version"]));
        assert_eq!(
            count_offline(&spec.args),
            0,
            "version never carries --offline"
        );
    }

    #[test]
    fn prefetch_spec_stays_online_no_offline() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let (so, se, t) = spec_caps();
        let spec =
            prefetch_command_spec(&nonexistent_nix_bin(&home), &home, so, se, t, manifest).unwrap();
        assert_eq!(
            count_offline(&spec.args),
            0,
            "prefetch deliberately stays online (no --offline)",
        );
    }

    #[test]
    fn eval_specs_have_exactly_one_offline_across_all_systems() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let (so, se, t) = spec_caps();
        let nix_bin = nonexistent_nix_bin(&home);

        // Single eval: exactly one --offline for the host system.
        let host_sys = host_system().expect("host");
        let host_checked = check_system(manifest, host_sys).unwrap();
        let single =
            single_eval_command_spec(&nix_bin, &home, so, se, t, manifest, &host_checked).unwrap();
        assert_eq!(
            count_offline(&single.args),
            1,
            "single eval carries exactly one --offline",
        );

        // Index eval: exactly one --offline for every manifest system.
        for sys in &manifest.systems {
            let checked = check_system(manifest, sys).unwrap();
            let index =
                index_eval_command_spec(&nix_bin, &home, so, se, t, manifest, &checked).unwrap();
            assert_eq!(
                count_offline(&index.args),
                1,
                "index eval carries exactly one --offline for {sys}",
            );
        }
    }

    // 3. A relative nix path is rejected as CommandError::Spec(ProgramNotAbsolute)
    //    with a bounded `got` snippet — pattern-matched, no OS execution.

    #[test]
    fn relative_nix_path_is_rejected_as_spec_program_not_absolute() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let (so, se, t) = spec_caps();
        let host_sys = host_system().expect("host");
        let host_checked = check_system(manifest, host_sys).unwrap();
        let checked0 = check_system(manifest, &manifest.systems[0]).unwrap();
        for bad in ["nix", "./nix", "bin/nix", "../nix"] {
            let rel = PathBuf::from(bad);
            assert_program_not_absolute(version_command_spec(&rel, &home, so, se, t), bad);
            assert_program_not_absolute(
                prefetch_command_spec(&rel, &home, so, se, t, manifest),
                bad,
            );
            assert_program_not_absolute(
                single_eval_command_spec(&rel, &home, so, se, t, manifest, &host_checked),
                bad,
            );
            assert_program_not_absolute(
                index_eval_command_spec(&rel, &home, so, se, t, manifest, &checked0),
                bad,
            );
        }
    }

    // 4. Constructing specs does NOT execute the nonexistent absolute fixture
    //    path — construction is pure struct assembly.

    #[test]
    fn building_specs_does_not_execute_nonexistent_absolute_program() {
        // An absolute path that does NOT exist on disk: every builder must
        // construct a spec against it WITHOUT executing or creating it. Its
        // absence before AND after every build proves it was neither executed
        // nor created.
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        assert!(
            !nix_bin.exists(),
            "fixture program is absent before building"
        );

        let (so, se, t) = spec_caps();
        let host_sys = host_system().expect("host");
        let host_checked = check_system(manifest, host_sys).unwrap();

        // Each builder succeeds against the nonexistent absolute path.
        let v =
            version_command_spec(&nix_bin, &home, so, se, t).expect("version builds without spawn");
        let p = prefetch_command_spec(&nix_bin, &home, so, se, t, manifest)
            .expect("prefetch builds without spawn");
        let s = single_eval_command_spec(&nix_bin, &home, so, se, t, manifest, &host_checked)
            .expect("single eval builds without spawn");
        let mut index_specs = Vec::new();
        for sys in &manifest.systems {
            let checked = check_system(manifest, sys).unwrap();
            index_specs.push(
                index_eval_command_spec(&nix_bin, &home, so, se, t, manifest, &checked)
                    .expect("index eval builds without spawn"),
            );
        }

        // Every produced spec points at the exact nonexistent absolute program.
        for spec in std::iter::once(&v)
            .chain(std::iter::once(&p))
            .chain(std::iter::once(&s))
            .chain(index_specs.iter())
        {
            assert_eq!(spec.program, nix_bin, "program preserved verbatim");
        }
        // The program is STILL absent — no builder executed or created it.
        assert!(
            !nix_bin.exists(),
            "building specs did not touch the program"
        );
    }

    // === parse_nix_version: accepted =======================================

    #[test]
    fn parse_nix_version_accepts_realistic_version_without_trailing_lf() {
        assert_eq!(parse_nix_version(b"nix (Nix) 2.34.8").unwrap(), "2.34.8");
    }

    #[test]
    fn parse_nix_version_accepts_with_exactly_one_trailing_lf() {
        assert_eq!(parse_nix_version(b"nix (Nix) 2.34.8\n").unwrap(), "2.34.8");
    }

    #[test]
    fn parse_nix_version_accepts_full_charset_at_sixty_four_byte_ceiling() {
        // Every allowed class — digit, dot, hyphen, plus, lower, upper — and
        // exactly the 64-byte ceiling.
        let head = "0.1.2-rc3+ABCdef.45"; // 19 bytes
        let pad = "a".repeat(VERSION_MAX_LEN - head.len());
        let token = format!("{head}{pad}");
        assert_eq!(token.len(), VERSION_MAX_LEN);
        let stdout = format!("nix (Nix) {token}");
        assert_eq!(parse_nix_version(stdout.as_bytes()).unwrap(), token);
    }

    // === parse_nix_version: rejected =======================================

    #[test]
    fn parse_nix_version_rejects_empty_input() {
        assert_eq!(
            parse_nix_version(b"").unwrap_err(),
            VersionParseError::Empty,
        );
    }

    #[test]
    fn parse_nix_version_rejects_bad_prefix() {
        let bads: &[&[u8]] = &[
            b"nix 2.34.8",
            b"Nix (Nix) 2.34.8",
            b"nix(Nix) 2.34.8",
            b"nix (nix) 2.34.8",
            b" nix (Nix) 2.34.8",
            b"2.34.8",
            b"garbage",
            b"\nnix (Nix) 2.34.8",
        ];
        for &bad in bads {
            assert_eq!(
                parse_nix_version(bad).unwrap_err(),
                VersionParseError::BadPrefix,
                "expected BadPrefix for {bad:?}",
            );
        }
    }

    #[test]
    fn parse_nix_version_rejects_invalid_utf8() {
        assert_eq!(
            parse_nix_version(b"nix (Nix) \xff").unwrap_err(),
            VersionParseError::InvalidUtf8,
        );
        // Invalid UTF-8 is rejected even when a trailing LF is present.
        assert_eq!(
            parse_nix_version(b"nix (Nix) \xff\n").unwrap_err(),
            VersionParseError::InvalidUtf8,
        );
    }

    #[test]
    fn parse_nix_version_rejects_cr_and_crlf() {
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8\r").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8\r\n").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
    }

    #[test]
    fn parse_nix_version_rejects_multiple_lines() {
        // A second trailing LF survives into the version token.
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8\n\n").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
        // A second full line: the embedded LF is an invalid version byte.
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8\nnix (Nix) 1.0\n").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
    }

    #[test]
    fn parse_nix_version_rejects_spaces_in_version() {
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8 x").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
    }

    #[test]
    fn parse_nix_version_rejects_empty_version_token() {
        assert_eq!(
            parse_nix_version(b"nix (Nix) ").unwrap_err(),
            VersionParseError::EmptyVersion,
        );
        // Empty token with the tolerated single trailing LF.
        assert_eq!(
            parse_nix_version(b"nix (Nix) \n").unwrap_err(),
            VersionParseError::EmptyVersion,
        );
    }

    #[test]
    fn parse_nix_version_rejects_oversize_version() {
        let token = "a".repeat(VERSION_MAX_LEN + 1);
        let stdout = format!("nix (Nix) {token}");
        assert_eq!(
            parse_nix_version(stdout.as_bytes()).unwrap_err(),
            VersionParseError::OversizeVersion,
        );
    }

    #[test]
    fn parse_nix_version_rejects_other_invalid_version_bytes() {
        // Underscore is not in [A-Za-z0-9.+-].
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8_rc1").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
        // NUL / control byte.
        assert_eq!(
            parse_nix_version(b"nix (Nix) 2.34.8\x00").unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
        // Non-ASCII multibyte (ö = U+00F6).
        assert_eq!(
            parse_nix_version("nix (Nix) 2.34.8ö".as_bytes()).unwrap_err(),
            VersionParseError::InvalidVersionChar,
        );
    }

    // === verify_prefetch: accepted =========================================

    /// The pinned flake NAR hash, hard-coded so prefetch tests are INDEPENDENT
    /// of `benchmark.json` (mirrors the `FLAKE_REF` pattern above). Cross-checked
    /// against the live manifest in [`prefetch_constants_match_manifest`].
    const NAR_HASH: &str = "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";
    const VALID_STORE_PATH: &str = "/nix/store/abc123-ripgrep-1.0.0";

    fn prefetch_bytes(v: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(v).unwrap()
    }

    fn valid_prefetch_json() -> serde_json::Value {
        json!({
            "hash": NAR_HASH,
            "storePath": VALID_STORE_PATH,
        })
    }

    #[test]
    fn prefetch_constants_match_manifest() {
        assert_eq!(benchmark_manifest().nixpkgs.nar_hash, NAR_HASH);
    }

    #[test]
    fn verify_prefetch_accepts_minimal_valid() {
        let manifest = benchmark_manifest();
        let got = verify_prefetch(&prefetch_bytes(&valid_prefetch_json()), manifest).unwrap();
        assert_eq!(got.hash, NAR_HASH);
        assert_eq!(got.store_path, VALID_STORE_PATH);
        assert_eq!(
            got,
            VerifiedPrefetch {
                hash: NAR_HASH.to_string(),
                store_path: VALID_STORE_PATH.to_string(),
            },
        );
    }

    #[test]
    fn verify_prefetch_accepts_valid_with_unrelated_fields() {
        let manifest = benchmark_manifest();
        let v = json!({
            "hash": NAR_HASH,
            "storePath": VALID_STORE_PATH,
            "extra": 123,
            "foo": ["bar", null],
            "unrelated": { "nested": true },
        });
        let got = verify_prefetch(&prefetch_bytes(&v), manifest).unwrap();
        assert_eq!(got.hash, NAR_HASH);
        assert_eq!(got.store_path, VALID_STORE_PATH);
    }

    #[test]
    fn verify_prefetch_accepts_realistic_store_basename() {
        // A realistic 32-char-hash nix store path: all graphic ASCII, one path
        // component after /nix/store/.
        let manifest = benchmark_manifest();
        let v = json!({
            "hash": NAR_HASH,
            "storePath": "/nix/store/0x2hzvy8m8nw5lidpzq8aggcq7c88jp8-ripgrep-14.1.0",
        });
        let got = verify_prefetch(&prefetch_bytes(&v), manifest).unwrap();
        assert_eq!(
            got.store_path,
            "/nix/store/0x2hzvy8m8nw5lidpzq8aggcq7c88jp8-ripgrep-14.1.0",
        );
    }

    // === verify_prefetch: rejected =========================================

    #[test]
    fn verify_prefetch_rejects_malformed_json() {
        let manifest = benchmark_manifest();
        let bads: &[&[u8]] = &[b"", b"not json", b"{", b"{\"hash\":}", b"\xff\xff"];
        for &bad in bads {
            assert_eq!(
                verify_prefetch(bad, manifest).unwrap_err(),
                PrefetchError::MalformedJson,
                "expected MalformedJson for {bad:?}",
            );
        }
    }

    #[test]
    fn verify_prefetch_rejects_non_object_top_level() {
        let manifest = benchmark_manifest();
        for bad in [
            json!([1, 2, 3]),
            json!(42),
            json!("a string"),
            json!(true),
            json!(null),
        ] {
            assert_eq!(
                verify_prefetch(&prefetch_bytes(&bad), manifest).unwrap_err(),
                PrefetchError::NotAnObject,
            );
        }
    }

    #[test]
    fn verify_prefetch_rejects_missing_fields() {
        let manifest = benchmark_manifest();
        // Missing hash only.
        let no_hash = json!({ "storePath": VALID_STORE_PATH });
        assert_eq!(
            verify_prefetch(&prefetch_bytes(&no_hash), manifest).unwrap_err(),
            PrefetchError::HashMissing,
        );
        // Missing storePath only.
        let no_path = json!({ "hash": NAR_HASH });
        assert_eq!(
            verify_prefetch(&prefetch_bytes(&no_path), manifest).unwrap_err(),
            PrefetchError::StorePathMissing,
        );
        // Missing both: `hash` is checked first.
        let no_both = json!({ "unrelated": 1 });
        assert_eq!(
            verify_prefetch(&prefetch_bytes(&no_both), manifest).unwrap_err(),
            PrefetchError::HashMissing,
        );
    }

    #[test]
    fn verify_prefetch_rejects_wrong_field_types() {
        let manifest = benchmark_manifest();
        // hash present but not a string.
        for bad_hash in [json!(42), json!(null), json!([1]), json!({})] {
            let v = json!({ "hash": bad_hash, "storePath": VALID_STORE_PATH });
            assert_eq!(
                verify_prefetch(&prefetch_bytes(&v), manifest).unwrap_err(),
                PrefetchError::HashNotString,
            );
        }
        // storePath present but not a string (hash is valid, so we reach the
        // storePath check).
        for bad_path in [json!(42), json!(null), json!([1]), json!({})] {
            let v = json!({ "hash": NAR_HASH, "storePath": bad_path });
            assert_eq!(
                verify_prefetch(&prefetch_bytes(&v), manifest).unwrap_err(),
                PrefetchError::StorePathNotString,
            );
        }
    }

    #[test]
    fn verify_prefetch_rejects_hash_mismatch() {
        let manifest = benchmark_manifest();
        let v = json!({
            "hash": "sha256-deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef=",
            "storePath": VALID_STORE_PATH,
        });
        assert_eq!(
            verify_prefetch(&prefetch_bytes(&v), manifest).unwrap_err(),
            PrefetchError::HashMismatch,
        );
        // An empty hash string is type-correct but does not match the pin.
        let v2 = json!({ "hash": "", "storePath": VALID_STORE_PATH });
        assert_eq!(
            verify_prefetch(&prefetch_bytes(&v2), manifest).unwrap_err(),
            PrefetchError::HashMismatch,
        );
    }

    #[test]
    fn verify_prefetch_rejects_invalid_store_paths() {
        let manifest = benchmark_manifest();
        let bad_paths: &[&str] = &[
            "",                      // empty
            "/nix/store/",           // empty basename
            "/nix/store/.",          // dot basename
            "/nix/store/..",         // dotdot basename
            "/nix/store/../foo",     // traversal (slash in remainder)
            "/nix/store/foo/bar",    // extra slash
            "/nix/store//foo",       // double slash
            "/nix/store/foo/",       // trailing slash
            "/nix/store/föö",        // non-ASCII
            "/nix/store/foo bar",    // whitespace
            "/nix/store/foo\x7fbar", // DEL control byte
            "/nix/store/foo\tbar",   // tab
            "/nix/store/foo\nbar",   // embedded LF
            "/tmp/foo",              // wrong prefix
            "nix/store/foo",         // relative (no leading slash)
            "foo",                   // bare (no prefix)
            "/nix/store",            // prefix without trailing slash
        ];
        for &path in bad_paths {
            let v = json!({ "hash": NAR_HASH, "storePath": path });
            assert_eq!(
                verify_prefetch(&prefetch_bytes(&v), manifest).unwrap_err(),
                PrefetchError::InvalidStorePath,
                "expected InvalidStorePath for {path:?}",
            );
        }
    }

    // === bounded error Display =============================================

    #[test]
    fn version_parse_error_display_is_fixed_bounded_and_never_echoes_input() {
        let cases: &[(VersionParseError, &str)] = &[
            (VersionParseError::Empty, "nix version output is empty"),
            (
                VersionParseError::InvalidUtf8,
                "nix version output is not valid UTF-8",
            ),
            (
                VersionParseError::BadPrefix,
                "nix version output has an unexpected prefix",
            ),
            (
                VersionParseError::EmptyVersion,
                "nix version output is missing the version",
            ),
            (
                VersionParseError::OversizeVersion,
                "nix version is longer than 64 bytes",
            ),
            (
                VersionParseError::InvalidVersionChar,
                "nix version contains an invalid character",
            ),
        ];
        for (err, expected) in cases {
            let s = err.to_string();
            assert_eq!(&s, expected);
            // Bounded and never echoes raw child output.
            assert!(!s.contains("2.34.8"));
            assert!(!s.contains("nix (Nix)"));
            assert!(s.is_ascii());
        }
    }

    #[test]
    fn prefetch_error_display_is_fixed_bounded_and_never_echoes_input() {
        let cases: &[(PrefetchError, &str)] = &[
            (
                PrefetchError::MalformedJson,
                "prefetch output is not valid JSON",
            ),
            (
                PrefetchError::NotAnObject,
                "prefetch output is not a JSON object",
            ),
            (
                PrefetchError::HashMissing,
                "prefetch output is missing the hash field",
            ),
            (
                PrefetchError::StorePathMissing,
                "prefetch output is missing the storePath field",
            ),
            (
                PrefetchError::HashNotString,
                "prefetch output hash field is not a string",
            ),
            (
                PrefetchError::StorePathNotString,
                "prefetch output storePath field is not a string",
            ),
            (
                PrefetchError::HashMismatch,
                "prefetch hash does not match the pinned manifest",
            ),
            (
                PrefetchError::InvalidStorePath,
                "prefetch storePath is not a valid /nix/store path",
            ),
        ];
        for (err, expected) in cases {
            let s = err.to_string();
            assert_eq!(&s, expected);
            // Never echoes raw values: the pinned hash and store paths must not
            // appear in any error message.
            assert!(!s.contains(NAR_HASH));
            assert!(!s.contains(VALID_STORE_PATH));
            assert!(s.is_ascii());
        }
    }

    // === execute_version_probe ===========================================
    //
    // Focused tests for the version-probe execution wrapper. They discriminate
    // the EXACT spec the wrapper builds and forwards (program/argv/env/caps/
    // timeout/flavor), the single-call contract, the spec-build short-circuit
    // on a relative nix path, and the closed DetectNixCommand /
    // DetectNixVersion failure mapping — WITHOUT spawning (the executor is a
    // fake that captures the CommandSpec and returns a canned outcome).

    /// Build a [`CommandOutcome`] for the version probe from a supplied
    /// [`UnixStatus`] and stdout payload. The remaining fields are fixed
    /// (empty cleaned stderr, matching stdout byte total, fixed numeric
    /// metrics): the probe consumes ONLY status + stdout.
    fn version_probe_outcome(status: UnixStatus, stdout: &[u8]) -> CommandOutcome {
        CommandOutcome {
            status,
            stdout: stdout.to_vec(),
            cleaned_stderr: String::new(),
            stdout_total_bytes: stdout.len() as u64,
            stderr_total_bytes: 0,
            wall_ms: 7,
            max_rss_kib: 128,
        }
    }

    #[test]
    fn execute_version_probe_success_captures_one_spec_and_returns_version() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut captured_spec: Option<CommandSpec> = None;
        let mut captured_flavor: Option<TimeFlavor> = None;
        let mut executor = |spec: &CommandSpec, flav: TimeFlavor| {
            calls += 1;
            captured_spec = Some(spec.clone());
            captured_flavor = Some(flav);
            Ok(version_probe_outcome(
                UnixStatus::Exited(0),
                b"nix (Nix) 2.34.8\n",
            ))
        };

        let detected = execute_version_probe(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect("matching version succeeds");

        assert_eq!(detected, "2.34.8");
        assert_eq!(calls, 1, "executor called exactly once");
        let spec = captured_spec.expect("spec captured");
        assert_eq!(spec.program, nix_bin);
        assert_eq!(spec.args, oss(&["--version"]));
        assert_eq!(spec.env, home.child_env());
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert_eq!(spec.timeout, timeout);
        assert_eq!(captured_flavor, Some(flavor));
    }

    #[test]
    fn execute_version_probe_relative_nix_short_circuits_before_executor() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(
                UnixStatus::Exited(0),
                b"nix (Nix) 2.34.8\n",
            ))
        };

        let rel = PathBuf::from("nix");
        let err = execute_version_probe(
            manifest,
            &rel,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("relative nix path is rejected before execution");

        assert_eq!(err, RealFailureKind::DetectNixCommand);
        assert_eq!(calls, 0, "spec-build failure short-circuits the executor");
    }

    /// Run [`execute_version_probe`] against a fake executor that returns
    /// `make_result()` each call, and assert it maps to DetectNixCommand after
    /// EXACTLY one executor invocation. Compact driver for the executor-error
    /// / nonzero-exit / signal cases.
    fn probe_runs_once_to_detect_nix_command(
        make_result: impl Fn() -> Result<CommandOutcome, CommandError>,
    ) {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            make_result()
        };

        let err = execute_version_probe(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("command-level failure must map to DetectNixCommand");

        assert_eq!(err, RealFailureKind::DetectNixCommand);
        assert_eq!(calls, 1, "executor called exactly once");
    }

    #[test]
    fn execute_version_probe_command_failures_map_detect_nix_command() {
        // Executor returns an existing CommandError variant: the probe never
        // inspects the child and maps straight to DetectNixCommand.
        probe_runs_once_to_detect_nix_command(|| Err(CommandError::Rss));
        probe_runs_once_to_detect_nix_command(|| Err(CommandError::ReaderPanic));
        probe_runs_once_to_detect_nix_command(|| {
            Err(CommandError::Spawn {
                kind: std::io::ErrorKind::NotFound,
            })
        });
        // Executor succeeds but the child exits nonzero.
        probe_runs_once_to_detect_nix_command(|| {
            Ok(version_probe_outcome(
                UnixStatus::Exited(1),
                b"nix (Nix) 2.34.8\n",
            ))
        });
        // Executor succeeds but the child is terminated by a signal.
        probe_runs_once_to_detect_nix_command(|| {
            Ok(version_probe_outcome(
                UnixStatus::Signaled(9),
                b"nix (Nix) 2.34.8\n",
            ))
        });
    }

    /// Run [`execute_version_probe`] against a fake executor that returns a
    /// successful outcome carrying `stdout`, and assert it maps to
    /// DetectNixVersion after EXACTLY one executor invocation. Compact driver
    /// for the malformed-stdout / mismatched-version cases.
    fn probe_runs_once_to_detect_nix_version(stdout: &[u8]) {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(UnixStatus::Exited(0), stdout))
        };

        let err = execute_version_probe(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("stdout/version failure must map to DetectNixVersion");

        assert_eq!(err, RealFailureKind::DetectNixVersion);
        assert_eq!(calls, 1, "executor called exactly once");
    }

    #[test]
    fn execute_version_probe_malformed_and_mismatched_map_detect_nix_version() {
        // Malformed stdout (empty / bad prefix): parse_nix_version rejects it.
        probe_runs_once_to_detect_nix_version(b"");
        probe_runs_once_to_detect_nix_version(b"garbage");
        // Well-formed version that does NOT match the pinned 2.34.8.
        probe_runs_once_to_detect_nix_version(b"nix (Nix) 2.99.0\n");
    }

    // === execute_verified_prefetch =========================================
    //
    // Focused tests for the flake-prefetch execution wrapper. They mirror the
    // version-probe tests above: they discriminate the EXACT prefetch spec the
    // wrapper builds and forwards (program/argv/env/caps/timeout/flavor, ZERO
    // --offline), the single-call contract, the spec-build short-circuit on a
    // relative nix path, and the closed PrefetchCommand /
    // PrefetchVerification failure mapping — WITHOUT spawning (the executor is
    // a fake that captures the CommandSpec and returns a canned outcome).

    #[test]
    fn execute_verified_prefetch_success_captures_one_spec_and_returns_unit() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut captured_spec: Option<CommandSpec> = None;
        let mut captured_flavor: Option<TimeFlavor> = None;
        let mut executor = |spec: &CommandSpec, flav: TimeFlavor| {
            calls += 1;
            captured_spec = Some(spec.clone());
            captured_flavor = Some(flav);
            Ok(version_probe_outcome(
                UnixStatus::Exited(0),
                &prefetch_bytes(&valid_prefetch_json()),
            ))
        };

        execute_verified_prefetch(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect("prefetch matching the manifest succeeds");

        assert_eq!(calls, 1, "executor called exactly once");
        let spec = captured_spec.expect("spec captured");
        assert_eq!(spec.program, nix_bin);
        assert_eq!(spec.args, prefetch_argv(manifest));
        assert_eq!(spec.env, home.child_env());
        assert_eq!(spec.stdout_cap, stdout_cap);
        assert_eq!(spec.stderr_cap, stderr_cap);
        assert_eq!(spec.timeout, timeout);
        assert_eq!(captured_flavor, Some(flavor));
        // Prefetch deliberately stays online: zero --offline tokens.
        assert_eq!(count_offline(&spec.args), 0);
    }

    #[test]
    fn execute_verified_prefetch_relative_nix_short_circuits_before_executor() {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(
                UnixStatus::Exited(0),
                &prefetch_bytes(&valid_prefetch_json()),
            ))
        };

        let rel = PathBuf::from("nix");
        let err = execute_verified_prefetch(
            manifest,
            &rel,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("relative nix path is rejected before execution");

        assert_eq!(err, RealFailureKind::PrefetchCommand);
        assert_eq!(calls, 0, "spec-build failure short-circuits the executor");
    }

    /// Run [`execute_verified_prefetch`] against a fake executor that returns
    /// `make_result()` each call, and assert it maps to PrefetchCommand after
    /// EXACTLY one executor invocation. Compact driver for the executor-error
    /// / nonzero-exit / signal cases.
    fn prefetch_runs_once_to_prefetch_command(
        make_result: impl Fn() -> Result<CommandOutcome, CommandError>,
    ) {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            make_result()
        };

        let err = execute_verified_prefetch(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("command-level failure must map to PrefetchCommand");

        assert_eq!(err, RealFailureKind::PrefetchCommand);
        assert_eq!(calls, 1, "executor called exactly once");
    }

    #[test]
    fn execute_verified_prefetch_command_failures_map_prefetch_command() {
        // Executor returns an existing CommandError variant: the wrapper never
        // inspects the child and maps straight to PrefetchCommand.
        prefetch_runs_once_to_prefetch_command(|| Err(CommandError::Rss));
        prefetch_runs_once_to_prefetch_command(|| Err(CommandError::ReaderPanic));
        prefetch_runs_once_to_prefetch_command(|| {
            Err(CommandError::Spawn {
                kind: std::io::ErrorKind::NotFound,
            })
        });
        // Executor succeeds but the child exits nonzero.
        prefetch_runs_once_to_prefetch_command(|| {
            Ok(version_probe_outcome(
                UnixStatus::Exited(1),
                &prefetch_bytes(&valid_prefetch_json()),
            ))
        });
        // Executor succeeds but the child is terminated by a signal.
        prefetch_runs_once_to_prefetch_command(|| {
            Ok(version_probe_outcome(
                UnixStatus::Signaled(9),
                &prefetch_bytes(&valid_prefetch_json()),
            ))
        });
    }

    /// Run [`execute_verified_prefetch`] against a fake executor that returns a
    /// successful outcome carrying `stdout`, and assert it maps to
    /// PrefetchVerification after EXACTLY one executor invocation. Compact
    /// driver for the malformed-JSON / hash-mismatch cases.
    fn prefetch_runs_once_to_prefetch_verification(stdout: &[u8]) {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let (stdout_cap, stderr_cap, timeout) = spec_caps();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(UnixStatus::Exited(0), stdout))
        };

        let err = execute_verified_prefetch(
            manifest,
            &nix_bin,
            &home,
            stdout_cap,
            stderr_cap,
            timeout,
            flavor,
            &mut executor,
        )
        .expect_err("stdout failure must map to PrefetchVerification");

        assert_eq!(err, RealFailureKind::PrefetchVerification);
        assert_eq!(calls, 1, "executor called exactly once");
    }

    #[test]
    fn execute_verified_prefetch_malformed_and_hash_mismatch_map_prefetch_verification() {
        // Malformed JSON: verify_prefetch rejects it.
        prefetch_runs_once_to_prefetch_verification(b"");
        prefetch_runs_once_to_prefetch_verification(b"not json");
        // Valid JSON whose hash does NOT match the pinned NAR hash.
        let mismatched = json!({
            "hash": "sha256-deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef=",
            "storePath": VALID_STORE_PATH,
        });
        prefetch_runs_once_to_prefetch_verification(&prefetch_bytes(&mismatched));
    }

    // === real_sample_from_outcome =========================================

    /// Build a `CommandOutcome` with the given status and byte totals. The
    /// stdout/stderr CONTENT is deliberately adversarial: a real sample error
    /// must NEVER echo it.
    fn outcome(
        status: UnixStatus,
        stdout_total_bytes: u64,
        stderr_total_bytes: u64,
        wall_ms: u64,
        max_rss_kib: u64,
    ) -> CommandOutcome {
        CommandOutcome {
            status,
            stdout: b"ADVERSARIAL-stdout-payload\x00\xff".to_vec(),
            cleaned_stderr: "ADVERSARIAL-stderr-payload".to_string(),
            stdout_total_bytes,
            stderr_total_bytes,
            wall_ms,
            max_rss_kib,
        }
    }

    #[test]
    fn real_sample_from_outcome_success_maps_every_field() {
        let out = outcome(UnixStatus::Exited(0), 100, 23, 4_567, 9_876);
        let sample =
            real_sample_from_outcome(7, Record::Measured, CacheLabel::SourceWarmProcessCold, &out)
                .expect("exit 0 with correct cache is accepted");

        assert_eq!(sample.index, 7);
        assert_eq!(sample.record, Record::Measured);
        assert!(!sample.skipped);
        assert_eq!(sample.wall_ms, Some(4_567));
        assert_eq!(sample.rss_kb, Some(9_876));
        assert_eq!(sample.output_bytes, Some(123));
        assert_eq!(sample.exit, 0);
        assert_eq!(sample.cache, CacheLabel::SourceWarmProcessCold);

        // A warmup record passes through unchanged.
        let warm =
            real_sample_from_outcome(0, Record::Warmup, CacheLabel::SourceWarmProcessCold, &out)
                .unwrap();
        assert_eq!(warm.record, Record::Warmup);
        assert_eq!(warm.index, 0);
    }

    #[test]
    fn real_sample_output_total_saturates_at_u64_max() {
        // MAX + MAX saturates back to MAX.
        let out = outcome(UnixStatus::Exited(0), u64::MAX, u64::MAX, 1, 2);
        let sample =
            real_sample_from_outcome(0, Record::Measured, CacheLabel::SourceWarmProcessCold, &out)
                .unwrap();
        assert_eq!(sample.output_bytes, Some(u64::MAX));

        // Near-MAX + overflow saturates to MAX.
        let out = outcome(UnixStatus::Exited(0), u64::MAX - 5, 10, 1, 2);
        let sample =
            real_sample_from_outcome(0, Record::Measured, CacheLabel::SourceWarmProcessCold, &out)
                .unwrap();
        assert_eq!(sample.output_bytes, Some(u64::MAX));

        // A non-overflowing sum at the boundary is exact (== MAX).
        let out = outcome(UnixStatus::Exited(0), u64::MAX - 10, 10, 1, 2);
        let sample =
            real_sample_from_outcome(0, Record::Measured, CacheLabel::SourceWarmProcessCold, &out)
                .unwrap();
        assert_eq!(sample.output_bytes, Some(u64::MAX));
    }

    #[test]
    fn real_sample_rejects_nonzero_exit_and_signal() {
        let cache = CacheLabel::SourceWarmProcessCold;
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Measured,
                cache,
                &outcome(UnixStatus::Exited(1), 0, 0, 1, 2),
            )
            .unwrap_err(),
            RealSampleError::NonzeroExit,
        );
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Measured,
                cache,
                &outcome(UnixStatus::Exited(127), 0, 0, 1, 2),
            )
            .unwrap_err(),
            RealSampleError::NonzeroExit,
        );
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Measured,
                cache,
                &outcome(UnixStatus::Signaled(9), 0, 0, 1, 2),
            )
            .unwrap_err(),
            RealSampleError::Signaled,
        );
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Measured,
                cache,
                &outcome(UnixStatus::Signaled(15), 0, 0, 1, 2),
            )
            .unwrap_err(),
            RealSampleError::Signaled,
        );
    }

    #[test]
    fn real_sample_rejects_wrong_cache() {
        // Exit-0 outcome, but the cache label is not the one Real is allowed.
        let out = outcome(UnixStatus::Exited(0), 0, 0, 1, 2);
        for wrong in [CacheLabel::Fixture, CacheLabel::Unknown] {
            assert_eq!(
                real_sample_from_outcome(0, Record::Measured, wrong, &out).unwrap_err(),
                RealSampleError::WrongCache,
                "expected WrongCache for {wrong:?}",
            );
        }
    }

    #[test]
    fn real_sample_error_display_is_fixed_and_never_echoes_child_output() {
        // The three variant Displays are exactly the fixed, bounded messages.
        let cases: &[(RealSampleError, &str)] = &[
            (
                RealSampleError::WrongCache,
                "real sample requires a SourceWarmProcessCold cache label",
            ),
            (
                RealSampleError::NonzeroExit,
                "real sample child exited with a nonzero status",
            ),
            (
                RealSampleError::Signaled,
                "real sample child was terminated by a signal",
            ),
        ];
        for (err, expected) in cases {
            let s = err.to_string();
            assert_eq!(&s, expected);
            // Fixed and bounded: ASCII, never echoes raw child output.
            assert!(!s.contains("ADVERSARIAL"));
            assert!(s.is_ascii());
        }

        // The error PATH itself must not surface the adversarial child output:
        // each rejection is produced over an outcome carrying hostile
        // stdout/stderr, and its Display must still be the fixed message.
        let hostiles: [(RealSampleError, &str); 3] = [
            (
                real_sample_from_outcome(
                    0,
                    Record::Measured,
                    CacheLabel::Fixture,
                    &outcome(UnixStatus::Exited(3), 9, 9, 1, 2),
                )
                .unwrap_err(),
                "real sample requires a SourceWarmProcessCold cache label",
            ),
            (
                real_sample_from_outcome(
                    0,
                    Record::Measured,
                    CacheLabel::SourceWarmProcessCold,
                    &outcome(UnixStatus::Exited(5), 9, 9, 1, 2),
                )
                .unwrap_err(),
                "real sample child exited with a nonzero status",
            ),
            (
                real_sample_from_outcome(
                    0,
                    Record::Measured,
                    CacheLabel::SourceWarmProcessCold,
                    &outcome(UnixStatus::Signaled(11), 9, 9, 1, 2),
                )
                .unwrap_err(),
                "real sample child was terminated by a signal",
            ),
        ];
        for (err, expected) in hostiles {
            let s = err.to_string();
            assert_eq!(s, expected);
            assert!(!s.contains("ADVERSARIAL"));
            // The error also implements std::error::Error.
            let _: &dyn std::error::Error = &err;
        }
    }

    // === assemble_real_scenario ===========================================

    /// Build a minimal [`ScenarioDescriptor`] for the assembly tests with the
    /// given declared warmup/measured counts.
    fn descriptor(warmup: u32, measured: u32) -> ScenarioDescriptor {
        ScenarioDescriptor {
            name: "single-attr:ripgrep".to_owned(),
            system: "x86_64-linux".to_owned(),
            installable: "github:NixOS/nixpkgs/x#legacyPackages.x86_64-linux.ripgrep.drvPath"
                .to_owned(),
            warmup,
            measured,
            stdout_payload: 4096,
            stdout_cap_bytes: NonZeroU64::new(8192).expect("nonzero"),
            timeout_seconds: 60,
        }
    }

    /// A successful (exit 0) outcome with the given wall-ms / max-RSS. Reuses
    /// the shared adversarial `outcome` helper so any error-echo is caught by
    /// the bounded-Display test below.
    fn ok_outcome(wall_ms: u64, max_rss_kib: u64) -> CommandOutcome {
        outcome(UnixStatus::Exited(0), 4096, 256, wall_ms, max_rss_kib)
    }

    /// Build a [`RealObservation`] carrying the SourceWarmProcessCold cache
    /// label.
    fn observation(
        descriptor: ScenarioDescriptor,
        record: Record,
        phase_index: u32,
        outcome: CommandOutcome,
    ) -> RealObservation {
        RealObservation {
            descriptor,
            record,
            phase_index,
            cache: CacheLabel::SourceWarmProcessCold,
            outcome,
        }
    }

    /// Build a complete warmup-then-measured observation vector for `desc` with
    /// distinct per-measured wall/rss values (so stats-exclude-warmup is
    /// observable) and a separate warmup value range.
    fn full_observations(desc: &ScenarioDescriptor) -> Vec<RealObservation> {
        let mut out = Vec::new();
        for i in 0..desc.warmup {
            out.push(observation(
                desc.clone(),
                Record::Warmup,
                i,
                ok_outcome(100 + i as u64, 1000 + i as u64),
            ));
        }
        for i in 0..desc.measured {
            out.push(observation(
                desc.clone(),
                Record::Measured,
                i,
                ok_outcome(200 + i as u64, 2000 + i as u64 * 10),
            ));
        }
        out
    }

    #[test]
    fn assemble_complete_warmup_plus_measured_succeeds_with_contiguous_global_indices() {
        let desc = descriptor(2, 3);
        let obs = full_observations(&desc);
        let scen = assemble_real_scenario(&desc, &obs, ScenarioRequirement::CompleteRequired)
            .expect("complete warmup + measured assembles");

        // Declared counts come from the expected descriptor.
        assert_eq!(scen.warmup, 2);
        assert_eq!(scen.measured, 3);
        assert_eq!(scen.samples.len(), 5);

        // Global indices are contiguous zero-based in slice order, NOT
        // phase-local: warmups get 0,1 and measured get 2,3,4.
        for (pos, sample) in scen.samples.iter().enumerate() {
            assert_eq!(sample.index, pos as u32, "global index at position {pos}");
        }
        assert_eq!(scen.samples[0].record, Record::Warmup);
        assert_eq!(scen.samples[1].record, Record::Warmup);
        assert_eq!(scen.samples[2].record, Record::Measured);
        assert_eq!(scen.samples[3].record, Record::Measured);
        assert_eq!(scen.samples[4].record, Record::Measured);

        // Measured stats EXCLUDE the warmup samples: measured wall was
        // 200,201,202 (warmup wall 100,101 must not participate).
        let expected_wall = stats::compute(&[200, 201, 202]).unwrap();
        let expected_rss = stats::compute(&[2000, 2010, 2020]).unwrap();
        let wall = scen.statistics.wall.expect("wall stats present");
        let rss = scen.statistics.rss.expect("rss stats present");
        assert_eq!(
            wall,
            SampleStatistics {
                count: 3,
                min: expected_wall.min,
                median: expected_wall.median,
                p95: expected_wall.p95,
                max: expected_wall.max,
            },
        );
        assert_eq!(rss.count, 3);
        assert_eq!(rss.min, expected_rss.min);
        assert_eq!(rss.median, expected_rss.median);
        assert_eq!(rss.p95, expected_rss.p95);
        assert_eq!(rss.max, expected_rss.max);

        // Every sample is complete and correctly labelled.
        for sample in &scen.samples {
            assert!(!sample.skipped);
            assert_eq!(sample.exit, 0);
            assert!(sample.wall_ms.is_some());
            assert!(sample.rss_kb.is_some());
            assert!(sample.output_bytes.is_some());
            assert_eq!(sample.cache, CacheLabel::SourceWarmProcessCold);
        }
    }

    #[test]
    fn assemble_partial_valid_prefix_succeeds_and_preserves_observations() {
        let desc = descriptor(2, 3);
        // A valid prefix: both warmups, then only the first measured.
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
            observation(desc.clone(), Record::Measured, 0, ok_outcome(200, 2000)),
        ];
        let scen = assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed)
            .expect("partial prefix assembles");

        // Three observations preserved; declared counts still come from the
        // expected descriptor.
        assert_eq!(scen.samples.len(), 3);
        assert_eq!(scen.warmup, 2);
        assert_eq!(scen.measured, 3);
        for (pos, sample) in scen.samples.iter().enumerate() {
            assert_eq!(sample.index, pos as u32);
        }
        // Statistics over the single preserved measured sample.
        let wall = scen.statistics.wall.expect("wall stats present");
        assert_eq!(wall.count, 1);
        assert_eq!(wall.min, 200);
        assert_eq!(wall.max, 200);

        // The same prefix is REJECTED under CompleteRequired (short of the
        // declared measured count).
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::CompleteRequired).unwrap_err();
        assert_eq!(err, RealAssemblyError::IncompleteCounts);

        // A prefix of only warmups is also a valid partial prefix.
        let warmups_only = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
        ];
        let scen =
            assemble_real_scenario(&desc, &warmups_only, ScenarioRequirement::PartialAllowed)
                .expect("warmups-only prefix assembles");
        assert_eq!(scen.samples.len(), 2);
    }

    #[test]
    fn assemble_zero_measured_produces_both_stats_none() {
        let desc = descriptor(1, 2);
        // Only the single declared warmup; no measured observations.
        let obs = vec![observation(
            desc.clone(),
            Record::Warmup,
            0,
            ok_outcome(100, 1000),
        )];
        let scen = assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed)
            .expect("zero-measured prefix assembles");
        assert!(scen.statistics.wall.is_none(), "wall must be None");
        assert!(scen.statistics.rss.is_none(), "rss must be None");
        assert_eq!(scen.samples.len(), 1);
    }

    #[test]
    fn assemble_rejects_wrong_descriptor() {
        let desc = descriptor(2, 3);
        let mut other = desc.clone();
        other.name = "index-meta:aarch64-darwin".to_owned();
        let obs = vec![observation(other, Record::Warmup, 0, ok_outcome(100, 1000))];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::DescriptorMismatch);
    }

    #[test]
    fn assemble_rejects_wrong_cache() {
        let desc = descriptor(2, 3);
        let mut obs = observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000));
        obs.cache = CacheLabel::Fixture;
        let err =
            assemble_real_scenario(&desc, &[obs], ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::WrongCache);
        // Unknown cache is also rejected.
        let mut obs = observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000));
        obs.cache = CacheLabel::Unknown;
        let err =
            assemble_real_scenario(&desc, &[obs], ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::WrongCache);
    }

    #[test]
    fn assemble_rejects_measured_before_all_warmups() {
        let desc = descriptor(2, 3);
        // Only one of two warmups, then a measured record.
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Measured, 0, ok_outcome(200, 2000)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::RecordOrder);

        // A measured record with NO warmups (when warmups are declared) is the
        // same ordering violation.
        let obs = vec![observation(
            desc.clone(),
            Record::Measured,
            0,
            ok_outcome(200, 2000),
        )];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::RecordOrder);
    }

    #[test]
    fn assemble_rejects_warmup_after_measured() {
        let desc = descriptor(2, 3);
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
            observation(desc.clone(), Record::Measured, 0, ok_outcome(200, 2000)),
            observation(desc.clone(), Record::Warmup, 2, ok_outcome(102, 1020)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::RecordOrder);
    }

    #[test]
    fn assemble_rejects_wrong_phase_index_for_warmup_and_measured() {
        // Warmup phase index wrong: second warmup claims index 5 (expected 1).
        let desc = descriptor(2, 3);
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 5, ok_outcome(101, 1010)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::PhaseIndex);

        // Measured phase index wrong: first measured claims index 7 (expected 0).
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
            observation(desc.clone(), Record::Measured, 7, ok_outcome(200, 2000)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::PhaseIndex);
    }

    #[test]
    fn assemble_rejects_excess_warmup_and_excess_measured() {
        // Excess warmup: declared 1, observed 2.
        let desc = descriptor(1, 2);
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::ExcessWarmup);

        // Excess measured: declared 1 warmup + 1 measured, observed one extra
        // measured.
        let desc = descriptor(1, 1);
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Measured, 0, ok_outcome(200, 2000)),
            observation(desc.clone(), Record::Measured, 1, ok_outcome(210, 2100)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::ExcessMeasured);

        // Excess is rejected even under CompleteRequired (it is never a valid
        // prefix).
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::CompleteRequired).unwrap_err();
        assert_eq!(err, RealAssemblyError::ExcessMeasured);
    }

    #[test]
    fn assemble_complete_required_rejects_missing_counts() {
        let desc = descriptor(2, 3);
        // Short on measured only.
        let obs = vec![
            observation(desc.clone(), Record::Warmup, 0, ok_outcome(100, 1000)),
            observation(desc.clone(), Record::Warmup, 1, ok_outcome(101, 1010)),
            observation(desc.clone(), Record::Measured, 0, ok_outcome(200, 2000)),
            observation(desc.clone(), Record::Measured, 1, ok_outcome(210, 2100)),
        ];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::CompleteRequired).unwrap_err();
        assert_eq!(err, RealAssemblyError::IncompleteCounts);

        // Short on warmup only (a valid prefix under PartialAllowed).
        let obs = vec![observation(
            desc.clone(),
            Record::Warmup,
            0,
            ok_outcome(100, 1000),
        )];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::CompleteRequired).unwrap_err();
        assert_eq!(err, RealAssemblyError::IncompleteCounts);

        // Empty observations: CompleteRequired rejects; PartialAllowed accepts
        // an empty prefix producing both-None statistics.
        let err =
            assemble_real_scenario(&desc, &[], ScenarioRequirement::CompleteRequired).unwrap_err();
        assert_eq!(err, RealAssemblyError::IncompleteCounts);
        let empty =
            assemble_real_scenario(&desc, &[], ScenarioRequirement::PartialAllowed).unwrap();
        assert!(empty.statistics.wall.is_none());
        assert!(empty.statistics.rss.is_none());
        assert!(empty.samples.is_empty());
    }

    #[test]
    fn assemble_rejects_nonzero_and_signaled_outcomes_via_real_sample_error() {
        let desc = descriptor(1, 1);

        // Nonzero exit: real_sample_from_outcome rejects it with NonzeroExit,
        // and assemble_real_scenario surfaces that as ChildOutcomeRejected.
        let nonzero_outcome = outcome(UnixStatus::Exited(3), 4096, 256, 200, 2000);
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Warmup,
                CacheLabel::SourceWarmProcessCold,
                &nonzero_outcome,
            )
            .unwrap_err(),
            RealSampleError::NonzeroExit,
        );
        let obs = vec![observation(
            desc.clone(),
            Record::Warmup,
            0,
            nonzero_outcome,
        )];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::ChildOutcomeRejected);

        // Signaled: same path through RealSampleError::Signaled.
        let signaled_outcome = outcome(UnixStatus::Signaled(9), 4096, 256, 200, 2000);
        assert_eq!(
            real_sample_from_outcome(
                0,
                Record::Warmup,
                CacheLabel::SourceWarmProcessCold,
                &signaled_outcome,
            )
            .unwrap_err(),
            RealSampleError::Signaled,
        );
        let obs = vec![observation(
            desc.clone(),
            Record::Warmup,
            0,
            signaled_outcome,
        )];
        let err =
            assemble_real_scenario(&desc, &obs, ScenarioRequirement::PartialAllowed).unwrap_err();
        assert_eq!(err, RealAssemblyError::ChildOutcomeRejected);
    }

    #[test]
    fn assemble_error_display_is_ascii_bounded_and_never_echoes_adversarial_input() {
        // Every variant formats to a fixed, bounded, ASCII message.
        let errors = [
            RealAssemblyError::DescriptorMismatch,
            RealAssemblyError::WrongCache,
            RealAssemblyError::RecordOrder,
            RealAssemblyError::PhaseIndex,
            RealAssemblyError::ExcessWarmup,
            RealAssemblyError::ExcessMeasured,
            RealAssemblyError::IncompleteCounts,
            RealAssemblyError::ChildOutcomeRejected,
            RealAssemblyError::StatsFailure,
        ];
        for err in errors {
            let s = err.to_string();
            assert!(s.is_ascii(), "Display must be ASCII: {s:?}");
            assert!(
                s.len() <= 96,
                "Display must be at most 96 bytes (was {}): {s:?}",
                s.len(),
            );
            // Never echoes adversarial descriptor strings or child output.
            assert!(!s.contains("ADVERSARIAL"));
            assert!(!s.contains("ripgrep"));
            assert!(!s.contains("github:NixOS"));
            // It implements std::error::Error.
            let _: &dyn std::error::Error = &err;
        }

        // Drive the rejection paths with adversarial observations and confirm
        // no adversarial content reaches any error's Display.
        let desc = descriptor(2, 3);
        let mut bad_descriptor = desc.clone();
        bad_descriptor.installable =
            "github:EVIL/evil/eeeeeeeeeeee#legacyPackages.ADVERSARIAL".to_owned();
        let cases: Vec<RealAssemblyError> = vec![
            assemble_real_scenario(
                &desc,
                &[observation(
                    bad_descriptor,
                    Record::Warmup,
                    0,
                    outcome(UnixStatus::Exited(0), 4096, 256, 100, 1000),
                )],
                ScenarioRequirement::PartialAllowed,
            )
            .unwrap_err(),
            assemble_real_scenario(
                &desc,
                &[observation(
                    desc.clone(),
                    Record::Warmup,
                    0,
                    outcome(UnixStatus::Exited(7), 4096, 256, 100, 1000),
                )],
                ScenarioRequirement::PartialAllowed,
            )
            .unwrap_err(),
        ];
        for err in cases {
            let s = err.to_string();
            assert!(s.is_ascii());
            assert!(s.len() <= 96);
            assert!(!s.contains("EVIL"));
            assert!(!s.contains("ADVERSARIAL"));
            assert!(!s.contains("github:"));
        }
    }

    // === RealFailureKind + real_failure ===================================

    /// All nine [`RealFailureKind`] variants, for exhaustive iteration.
    const ALL_KINDS: [RealFailureKind; 9] = [
        RealFailureKind::DetectNixCommand,
        RealFailureKind::DetectNixVersion,
        RealFailureKind::PrefetchCommand,
        RealFailureKind::PrefetchVerification,
        RealFailureKind::EvalCommand,
        RealFailureKind::EvalOutcome,
        RealFailureKind::OverallTimeout,
        RealFailureKind::ScenarioAssembly,
        RealFailureKind::ReportAssembly,
    ];

    #[test]
    fn real_failure_kind_stage_and_message_table_is_exact_ascii_and_bounded() {
        // Exhaustive table over every variant: exact stage + exact message,
        // every string ASCII and at most 80 bytes.
        let table: &[(RealFailureKind, &str, &str)] = &[
            (
                RealFailureKind::DetectNixCommand,
                "detect-nix",
                "failed to execute the pinned Nix binary",
            ),
            (
                RealFailureKind::DetectNixVersion,
                "detect-nix",
                "pinned Nix version validation failed",
            ),
            (
                RealFailureKind::PrefetchCommand,
                "prefetch",
                "pinned Nixpkgs prefetch command failed",
            ),
            (
                RealFailureKind::PrefetchVerification,
                "prefetch",
                "pinned Nixpkgs source verification failed",
            ),
            (
                RealFailureKind::EvalCommand,
                "eval",
                "Nix evaluation command could not run",
            ),
            (
                RealFailureKind::EvalOutcome,
                "eval",
                "Nix evaluation command did not succeed",
            ),
            (
                RealFailureKind::OverallTimeout,
                "overall-timeout",
                "overall benchmark deadline expired",
            ),
            (
                RealFailureKind::ScenarioAssembly,
                "assemble-scenario",
                "scenario observations failed validation",
            ),
            (
                RealFailureKind::ReportAssembly,
                "assemble-report",
                "Real report failed validation",
            ),
        ];
        // The table is exhaustive: it has exactly as many rows as there are
        // variants, and every variant is covered (no duplicates, none missing).
        assert_eq!(
            table.len(),
            ALL_KINDS.len(),
            "table must cover all variants"
        );
        for variant in ALL_KINDS {
            assert!(
                table.iter().any(|(k, _, _)| *k == variant),
                "variant {variant:?} must appear in the table",
            );
        }

        // Every (stage, message) matches the enum's pure mapping, and every
        // string is ASCII and at most 80 bytes.
        for &(kind, stage, msg) in table {
            assert_eq!(kind.stage(), stage, "stage for {kind:?}");
            assert_eq!(kind.message(), msg, "message for {kind:?}");
            assert!(stage.is_ascii(), "stage must be ASCII: {stage:?}");
            assert!(msg.is_ascii(), "message must be ASCII: {msg:?}");
            assert!(
                stage.len() <= 80,
                "stage must be at most 80 bytes ({}): {stage:?}",
                stage.len(),
            );
            assert!(
                msg.len() <= 80,
                "message must be at most 80 bytes ({}): {msg:?}",
                msg.len(),
            );
        }
    }

    #[test]
    fn run_scenario_const_is_exactly_run() {
        assert_eq!(RUN_SCENARIO, "run");
        assert_eq!(RUN_SCENARIO.len(), 3);
    }

    #[test]
    fn real_failure_none_uses_run_scenario_exactly() {
        for &kind in &ALL_KINDS {
            let f = real_failure(kind, None);
            assert_eq!(f.scenario, RUN_SCENARIO, "None must use run for {kind:?}");
            assert_eq!(f.scenario, "run");
            assert_eq!(f.stage, kind.stage());
            assert_eq!(f.message, kind.message());
        }
    }

    #[test]
    fn real_failure_some_uses_descriptor_name_exactly() {
        let desc = descriptor(2, 3);
        for &kind in &ALL_KINDS {
            let f = real_failure(kind, Some(&desc));
            assert_eq!(
                f.scenario, desc.name,
                "Some must use descriptor.name exactly for {kind:?}",
            );
            assert_eq!(f.stage, kind.stage());
            assert_eq!(f.message, kind.message());
        }
    }

    #[test]
    fn real_failure_cannot_embed_adversarial_child_output() {
        // `real_failure` accepts NEITHER a [`CommandOutcome`] NOR error text, so
        // an adversarial outcome's stdout/stderr can NEVER reach a produced
        // [`Failure`]. Build a hostile outcome purely to assert its marker
        // bytes never appear in any produced failure.
        let stdout_marker = "ADVERSARIAL-stdout-payload";
        let stderr_marker = "ADVERSARIAL-stderr-payload";
        let hostile = CommandOutcome {
            status: UnixStatus::Exited(0),
            stdout: stdout_marker.as_bytes().to_vec(),
            cleaned_stderr: stderr_marker.to_string(),
            stdout_total_bytes: 9_999,
            stderr_total_bytes: 8_888,
            wall_ms: 1,
            max_rss_kib: 2,
        };
        // The hostile outcome really carries the markers (so a leak would be
        // detectable here); they are simply unreachable from `real_failure`.
        assert!(String::from_utf8_lossy(&hostile.stdout).contains(stdout_marker));
        assert!(hostile.cleaned_stderr.contains(stderr_marker));

        let desc = descriptor(2, 3);
        for &kind in &ALL_KINDS {
            for descriptor_opt in [None, Some(&desc)] {
                let f = real_failure(kind, descriptor_opt);
                assert!(
                    !f.scenario.contains(stdout_marker) && !f.scenario.contains(stderr_marker),
                    "scenario must not carry child output for {kind:?}",
                );
                assert!(
                    !f.stage.contains(stdout_marker) && !f.stage.contains(stderr_marker),
                    "stage must not carry child output for {kind:?}",
                );
                assert!(
                    !f.message.contains(stdout_marker) && !f.message.contains(stderr_marker),
                    "message must not carry child output for {kind:?}",
                );
                // Hostile byte totals never appear either.
                assert!(!f.scenario.contains("9999"));
                assert!(!f.scenario.contains("8888"));
            }
        }
    }

    #[test]
    fn real_failure_roundtrips_deterministically_via_serde() {
        // `Failure` derives `Serialize` + `Deserialize`, so a produced failure
        // MUST round-trip byte-identically via serde, and serializing twice
        // yields identical bytes (deterministic).
        let desc = descriptor(2, 3);
        for &kind in &ALL_KINDS {
            for descriptor_opt in [None, Some(&desc)] {
                let f = real_failure(kind, descriptor_opt);
                let json = serde_json::to_string(&f).expect("serialize Failure");
                let back: Failure = serde_json::from_str(&json).expect("deserialize Failure");
                assert_eq!(f, back, "round-trip for {kind:?} opt={descriptor_opt:?}");
                let json_again = serde_json::to_string(&f).expect("serialize Failure again");
                assert_eq!(json, json_again, "deterministic for {kind:?}");
            }
        }
    }

    // === REAL report assembly (PURE) =====================================
    //
    // The first bounded batch of report tests for [`assemble_real_report`].
    // Small helpers build the canonical five complete scenarios (one per
    // descriptor from [`crate::runner::descriptors`]) plus a literal host and
    // the pinned version, so each test focuses on ONE acceptance/rejection
    // axis without repeating full setup.

    /// A literal, host-independent [`Host`] so the report tests never depend on
    /// the compile-time host triple and remain deterministic.
    fn report_host() -> Host {
        Host {
            system: "x86_64-linux".to_owned(),
            machine: "bench-host".to_owned(),
            cores: 8,
        }
    }

    /// The exact pinned Nix version from the manifest.
    fn pinned_nix_version(manifest: &Manifest) -> String {
        manifest.nix.version.clone()
    }

    /// Build the canonical five assembled COMPLETE scenarios from the manifest:
    /// one per descriptor from [`crate::runner::descriptors`], each assembled
    /// with [`assemble_real_scenario`] under [`ScenarioRequirement::CompleteRequired`]
    /// over the existing [`full_observations`] helper (the same adversarial-
    /// output plumbing as the scenario-assembly tests above).
    fn canonical_complete_scenarios(manifest: &Manifest) -> Vec<Scenario> {
        let descs = crate::runner::descriptors(manifest).expect("descriptors must assemble");
        descs
            .iter()
            .map(|desc| {
                let obs = full_observations(desc);
                assemble_real_scenario(desc, &obs, ScenarioRequirement::CompleteRequired)
                    .expect("complete scenario must assemble")
            })
            .collect()
    }

    #[test]
    fn complete_real_report_assembles_with_exact_pin_and_five_canonical_scenarios() {
        let manifest = benchmark_manifest();
        let descs = crate::runner::descriptors(manifest).expect("descriptors assemble");
        let scenarios = canonical_complete_scenarios(manifest);
        let pinned = pinned_nix_version(manifest);
        let host = report_host();

        let report = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            scenarios,
            Vec::new(),
        )
        .expect("complete report with pinned version and no failures assembles");

        // report.validate succeeds (also exercised inside assemble_real_report).
        report
            .validate()
            .expect("report.validate succeeds for the accepted Complete report");

        // Mode Real, completeness Complete, harness_only false, schema pinned.
        assert_eq!(report.mode, Mode::Real);
        assert_eq!(report.completeness, Completeness::Complete);
        assert!(!report.harness_only);
        assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);

        // Exact Pin fields, built verbatim from the manifest.
        let expected_pin = Pin {
            nix_version: manifest.nix.version.clone(),
            owner: manifest.nixpkgs.owner.clone(),
            repo: manifest.nixpkgs.repo.clone(),
            rev: manifest.nixpkgs.rev.clone(),
            nar_hash: manifest.nixpkgs.nar_hash.clone(),
            attr: manifest.attr.clone(),
        };
        assert_eq!(report.pin, expected_pin);

        // Detected version + host preserved verbatim.
        assert_eq!(report.nix_version.as_deref(), Some(pinned.as_str()));
        assert_eq!(report.host, host);

        // Exactly five scenarios in canonical order, each matching its
        // descriptor exactly (metadata).
        assert_eq!(report.scenarios.len(), descs.len());
        assert_eq!(report.scenarios.len(), 5);
        for (scen, desc) in report.scenarios.iter().zip(descs.iter()) {
            assert_eq!(scen.name, desc.name);
            assert_eq!(scen.system, desc.system);
            assert_eq!(scen.installable, desc.installable);
            assert_eq!(scen.warmup, desc.warmup);
            assert_eq!(scen.measured, desc.measured);
        }
        let names: Vec<&str> = report.scenarios.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "single-attr:ripgrep",
                "index-meta:x86_64-linux",
                "index-meta:aarch64-linux",
                "index-meta:x86_64-darwin",
                "index-meta:aarch64-darwin",
            ],
        );
    }

    #[test]
    fn complete_real_report_rejects_none_and_mismatched_nix_version() {
        let manifest = benchmark_manifest();
        let scenarios = canonical_complete_scenarios(manifest);
        let host = report_host();

        // None detected version: Complete requires the pin exactly.
        let err = assemble_real_report(manifest, host.clone(), None, scenarios.clone(), Vec::new())
            .unwrap_err();
        assert_eq!(err, RealReportError::NixVersion);

        // Mismatched detected version.
        let err = assemble_real_report(
            manifest,
            host,
            Some("9.9.9".to_owned()),
            scenarios,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::NixVersion);
    }

    #[test]
    fn complete_real_report_rejects_missing_duplicate_and_reordered_scenarios() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let pinned = pinned_nix_version(manifest);

        // One missing scenario: drop the last canonical scenario. match_plan
        // succeeds (an ordered subset) but the Complete length check fails.
        let mut missing = canonical_complete_scenarios(manifest);
        let _ = missing.pop();
        let err = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            missing,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);

        // One extra duplicated scenario: match_plan rejects the duplicate
        // (descriptor index not strictly greater than the previous).
        let mut duplicated = canonical_complete_scenarios(manifest);
        duplicated.push(duplicated[0].clone());
        let err = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            duplicated,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);

        // Reordered scenarios: swapping the first two breaks strict ordering.
        let mut reordered = canonical_complete_scenarios(manifest);
        reordered.swap(0, 1);
        let err = assemble_real_report(manifest, host.clone(), Some(pinned), reordered, Vec::new())
            .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);
    }

    #[test]
    fn complete_real_report_rejects_wrong_metadata_and_dishonest_shape() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let pinned = pinned_nix_version(manifest);

        // Wrong scenario metadata: tamper the installable so the name still
        // matches (reaching the metadata check) but metadata_matches fails.
        let mut bad_meta = canonical_complete_scenarios(manifest);
        bad_meta[0].installable.push_str(".tampered");
        let err = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            bad_meta,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioMetadata);

        // Wrong sample counts: declared metadata still matches the descriptor,
        // but dropping the last (measured) sample makes the captured count fall
        // short of the declaration.
        let mut bad_counts = canonical_complete_scenarios(manifest);
        let _ = bad_counts[0].samples.pop();
        let err = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            bad_counts,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioShape);

        // Tampered statistics: counts and indices intact, but the wall median
        // no longer matches the recomputation over the measured samples.
        let mut bad_stats = canonical_complete_scenarios(manifest);
        if let Some(wall) = bad_stats[0].statistics.wall.as_mut() {
            wall.median = wall.median.wrapping_add(1);
        }
        let err =
            assemble_real_report(manifest, host, Some(pinned), bad_stats, Vec::new()).unwrap_err();
        assert_eq!(err, RealReportError::ScenarioShape);
    }

    #[test]
    fn complete_real_report_serializes_and_renders_deterministically() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let pinned = pinned_nix_version(manifest);

        let report = assemble_real_report(
            manifest,
            host.clone(),
            Some(pinned.clone()),
            canonical_complete_scenarios(manifest),
            Vec::new(),
        )
        .expect("complete report assembles");

        // Serde JSON bytes are deterministic for a single value: two
        // serializations are byte-identical.
        let json_a = serde_json::to_vec(&report).expect("serialize report");
        let json_b = serde_json::to_vec(&report).expect("serialize report again");
        assert_eq!(json_a, json_b, "serde JSON bytes must be deterministic");

        // The report round-trips through serde losslessly.
        let back: Report = serde_json::from_slice(&json_a).expect("deserialize report");
        assert_eq!(back, report, "serde round-trip must be lossless");

        // Two independently assembled reports serialize to identical bytes (no
        // nondeterminism in the assembly path).
        let report_b = assemble_real_report(
            manifest,
            host,
            Some(pinned.clone()),
            canonical_complete_scenarios(manifest),
            Vec::new(),
        )
        .expect("second complete report assembles");
        assert_eq!(report_b, report);
        let json_indep = serde_json::to_vec(&report_b).expect("serialize independent report");
        assert_eq!(
            json_a, json_indep,
            "two assembled reports serialize identically",
        );

        // render_markdown is deterministic: two renders are byte-identical.
        let md_a = crate::report::render_markdown(&report);
        let md_b = crate::report::render_markdown(&report);
        assert_eq!(md_a, md_b, "render_markdown must be deterministic");

        // The rendered markdown is non-empty and carries the pinned version and
        // the Real mode label.
        assert!(!md_a.is_empty());
        assert!(
            md_a.contains(pinned.as_str()),
            "markdown contains the pinned version",
        );
        assert!(md_a.contains("real"), "markdown carries the Real mode");
    }

    // === REAL report assembly: the second bounded batch (Incomplete) =========
    //
    // Focused coverage for the Incomplete path of [`assemble_real_report`]:
    // a global failure with zero scenarios, a partial-first + complete-later
    // ordered subset, scenario-set / shape / failure rejection, acceptance of
    // every closed-table failure kind with its correct scope, the exhaustive
    // [`RealReportError`] Display contract, and deterministic serde + markdown.

    /// Build an honest PARTIAL scenario for the first canonical descriptor
    /// (`single-attr:ripgrep`): its single declared warmup plus the first two
    /// measured samples. Used by the Incomplete tests so per-scenario failures
    /// have a real scenario to attach to and the measured-only statistics are
    /// exercised. Reuses [`full_observations`] + [`assemble_real_scenario`].
    fn partial_first_scenario(manifest: &Manifest) -> Scenario {
        let descs = crate::runner::descriptors(manifest).expect("descriptors assemble");
        let desc = &descs[0];
        let mut obs = full_observations(desc);
        obs.truncate(desc.warmup as usize + 2);
        assemble_real_scenario(desc, &obs, ScenarioRequirement::PartialAllowed)
            .expect("partial prefix assembles")
    }

    /// Reset every sample's `index` to its slice position, so a record-order
    /// mutation is not masked by an earlier index check.
    fn reindex(scen: &mut Scenario) {
        for (pos, sample) in scen.samples.iter_mut().enumerate() {
            sample.index = pos as u32;
        }
    }

    // 1. Incomplete with DetectNixCommand / None: zero scenarios + None version.
    #[test]
    fn incomplete_detect_nix_command_none_accepts_zero_scenarios_and_none_version() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let failure = real_failure(RealFailureKind::DetectNixCommand, None);

        let report = assemble_real_report(
            manifest,
            host.clone(),
            None,
            Vec::new(),
            vec![failure.clone()],
        )
        .expect("Incomplete with DetectNixCommand/None assembles");

        assert_eq!(report.mode, Mode::Real);
        assert_eq!(report.completeness, Completeness::Incomplete);
        assert!(!report.harness_only);
        assert_eq!(report.nix_version, None);
        assert!(report.scenarios.is_empty());
        // Exact failure preserved verbatim.
        assert_eq!(report.failures, vec![failure]);
        report.validate().expect("report.validate succeeds");
    }

    // 2. Incomplete with a valid partial first scenario and a later complete
    //    canonical scenario, in increasing descriptor order.
    #[test]
    fn incomplete_partial_first_then_complete_later_succeeds_with_exact_stats() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let descs = crate::runner::descriptors(manifest).expect("descriptors assemble");

        let partial = partial_first_scenario(manifest);
        // A COMPLETE canonical scenario at a strictly later descriptor index.
        let later = canonical_complete_scenarios(manifest)[1].clone();
        // A global failure (scenario "run") keeps the Incomplete state
        // independent of the partial scenario.
        let failure = real_failure(RealFailureKind::OverallTimeout, None);

        let report = assemble_real_report(
            manifest,
            host,
            None,
            vec![partial.clone(), later],
            vec![failure.clone()],
        )
        .expect("partial-first + complete-later Incomplete assembles");

        assert_eq!(report.completeness, Completeness::Incomplete);
        assert_eq!(report.scenarios.len(), 2);
        assert_eq!(report.scenarios[0].name, descs[0].name);
        assert_eq!(report.scenarios[1].name, descs[1].name);
        // Partial measured-only statistics are preserved exactly.
        assert_eq!(report.scenarios[0].statistics, partial.statistics);
        assert_eq!(report.scenarios[0].statistics.wall.expect("wall").count, 2,);
        assert_eq!(report.failures, vec![failure]);
        report.validate().expect("report.validate succeeds");
    }

    // 3. Incomplete rejects duplicate, out-of-order, and unrecognized scenarios.
    #[test]
    fn incomplete_rejects_duplicate_out_of_order_and_unrecognized_scenarios() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let failure = real_failure(RealFailureKind::OverallTimeout, None);
        let partial = partial_first_scenario(manifest);
        let canonical = canonical_complete_scenarios(manifest);

        // Duplicate: the partial scenario (descriptor index 0) twice.
        let err = assemble_real_report(
            manifest,
            host.clone(),
            None,
            vec![partial.clone(), partial.clone()],
            vec![failure.clone()],
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);

        // Out of order: descriptor index 1 then index 0.
        let err = assemble_real_report(
            manifest,
            host.clone(),
            None,
            vec![canonical[1].clone(), partial.clone()],
            vec![failure.clone()],
        )
        .unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);

        // Unrecognized: a scenario name absent from the canonical plan.
        let mut bogus = partial.clone();
        bogus.name = "index-meta:bogus-system".to_owned();
        let err =
            assemble_real_report(manifest, host, None, vec![bogus], vec![failure]).unwrap_err();
        assert_eq!(err, RealReportError::ScenarioSet);
    }

    // 4. Incomplete rejects every category of malformed partial scenario with
    //    ScenarioShape (never ScenarioMetadata or ScenarioSet).
    #[test]
    fn incomplete_rejects_malformed_partial_scenarios() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let failure = real_failure(RealFailureKind::OverallTimeout, None);
        // Base: the canonical COMPLETE first scenario. Its metadata matches the
        // descriptor, its shape is honest, and it carries measured data with
        // matching statistics — so each mutation below corrupts exactly ONE
        // shape aspect.
        let base = canonical_complete_scenarios(manifest)[0].clone();

        // A named mutation-case: the case label and the mutation applied to a
        // base scenario. The small alias keeps the `cases` table readable and
        // resolves clippy::type_complexity on the tuple type.
        type MutationCase = (&'static str, fn(&mut Scenario));

        let cases: &[MutationCase] = &[
            ("fixture cache", |s| {
                s.samples[0].cache = CacheLabel::Fixture
            }),
            ("unknown cache", |s| {
                s.samples[0].cache = CacheLabel::Unknown
            }),
            ("noncontiguous global index", |s| s.samples[1].index = 9),
            ("measured before all warmups", |s| {
                let first_measured = s.samples.remove(1);
                s.samples.insert(0, first_measured);
                reindex(s);
            }),
            ("warmup after measured", |s| {
                s.samples.truncate(2);
                let mut stray = s.samples[0].clone();
                stray.index = 2;
                s.samples.push(stray);
            }),
            ("missing wall_ms", |s| s.samples[1].wall_ms = None),
            ("missing rss_kb", |s| s.samples[1].rss_kb = None),
            ("missing output_bytes", |s| s.samples[1].output_bytes = None),
            ("skipped true", |s| s.samples[1].skipped = true),
            ("nonzero exit", |s| s.samples[1].exit = 7),
            ("statistics present with zero measured", |s| {
                s.samples.retain(|x| x.record == Record::Warmup);
            }),
            ("missing statistics with measured data", |s| {
                s.statistics.wall = None;
                s.statistics.rss = None;
            }),
            ("tampered statistics with measured data", |s| {
                if let Some(wall) = s.statistics.wall.as_mut() {
                    wall.median = wall.median.wrapping_add(1);
                }
            }),
        ];

        for &(name, mutate) in cases {
            let mut scen = base.clone();
            mutate(&mut scen);
            let err = assemble_real_report(
                manifest,
                host.clone(),
                None,
                vec![scen],
                vec![failure.clone()],
            )
            .unwrap_err();
            assert_eq!(
                err,
                RealReportError::ScenarioShape,
                "case {name:?} must be ScenarioShape",
            );
        }
    }

    // 5. Failure validation rejects every adversarial pairing (UnknownFailure).
    #[test]
    fn incomplete_rejects_adversarial_failure_pairings() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let descs = crate::runner::descriptors(manifest).expect("descriptors assemble");

        // A single adversarial failure over an otherwise-valid Incomplete
        // skeleton (empty scenarios, None version) must be UnknownFailure.
        let reject = |failure: Failure| {
            assemble_real_report(manifest, host.clone(), None, Vec::new(), vec![failure])
                .unwrap_err()
        };
        let failure = |scenario: &str, stage: &str, message: &str| Failure {
            scenario: scenario.to_owned(),
            stage: stage.to_owned(),
            message: message.to_owned(),
        };

        // Unknown stage.
        assert_eq!(
            reject(failure(
                RUN_SCENARIO,
                "bogus-stage",
                RealFailureKind::DetectNixCommand.message(),
            )),
            RealReportError::UnknownFailure,
        );
        // Unknown message (valid stage).
        assert_eq!(
            reject(failure(
                RUN_SCENARIO,
                RealFailureKind::DetectNixCommand.stage(),
                "bogus message",
            )),
            RealReportError::UnknownFailure,
        );
        // Valid stage paired with a WRONG valid message: the (stage, message)
        // pair is outside the closed table.
        assert_eq!(
            reject(failure(
                RUN_SCENARIO,
                RealFailureKind::DetectNixCommand.stage(),
                RealFailureKind::PrefetchCommand.message(),
            )),
            RealReportError::UnknownFailure,
        );
        // Global kind attached to a descriptor scenario.
        assert_eq!(
            reject(failure(
                &descs[0].name,
                RealFailureKind::DetectNixCommand.stage(),
                RealFailureKind::DetectNixCommand.message(),
            )),
            RealReportError::UnknownFailure,
        );
        // Per-scenario kind attached to run.
        assert_eq!(
            reject(failure(
                RUN_SCENARIO,
                RealFailureKind::EvalOutcome.stage(),
                RealFailureKind::EvalOutcome.message(),
            )),
            RealReportError::UnknownFailure,
        );
        // Per-scenario kind attached to an unknown scenario.
        assert_eq!(
            reject(failure(
                "index-meta:bogus-system",
                RealFailureKind::EvalOutcome.stage(),
                RealFailureKind::EvalOutcome.message(),
            )),
            RealReportError::UnknownFailure,
        );
    }

    // 6. All nine real_failure outputs are accepted in valid Incomplete reports
    //    with the correct None (global) or Some (per-scenario) scope.
    #[test]
    fn incomplete_accepts_all_nine_real_failures_with_correct_scope() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let descs = crate::runner::descriptors(manifest).expect("descriptors assemble");
        let partial = partial_first_scenario(manifest);

        for &kind in &ALL_KINDS {
            let is_global = GLOBAL_FAILURE_KINDS.contains(&kind);
            // Global kinds carry scenario "run" over empty scenarios;
            // per-scenario kinds carry the descriptor name over an honest
            // partial canonical scenario for that descriptor.
            let (failure, scenarios) = if is_global {
                (real_failure(kind, None), Vec::new())
            } else {
                (real_failure(kind, Some(&descs[0])), vec![partial.clone()])
            };

            let report = assemble_real_report(
                manifest,
                host.clone(),
                None,
                scenarios,
                vec![failure.clone()],
            )
            .expect("valid Incomplete report assembles");
            assert_eq!(report.completeness, Completeness::Incomplete);
            assert_eq!(report.failures, vec![failure]);
            report.validate().expect("report.validate succeeds");
        }
    }

    // 7. Exhaustive RealReportError Display: every variant exact, ASCII, at
    //    most 96 bytes, and free of adversarial fragments.
    #[test]
    fn real_report_error_display_is_exact_ascii_bounded_and_clean() {
        let cases: &[(RealReportError, &str)] = &[
            (
                RealReportError::DescriptorPlan,
                "real report could not derive the descriptor plan",
            ),
            (
                RealReportError::UnknownFailure,
                "real report recorded an unknown failure",
            ),
            (
                RealReportError::ScenarioSet,
                "real report scenario set is not an ordered plan subset",
            ),
            (
                RealReportError::ScenarioMetadata,
                "real report scenario metadata does not match its descriptor",
            ),
            (
                RealReportError::ScenarioShape,
                "real report scenario shape is not an honest capture",
            ),
            (
                RealReportError::NixVersion,
                "real report detected Nix version does not match the pin",
            ),
            (
                RealReportError::ReportValidation,
                "real report failed report validation",
            ),
        ];
        // Exhaustive: every variant appears exactly once.
        let all_variants = [
            RealReportError::DescriptorPlan,
            RealReportError::UnknownFailure,
            RealReportError::ScenarioSet,
            RealReportError::ScenarioMetadata,
            RealReportError::ScenarioShape,
            RealReportError::NixVersion,
            RealReportError::ReportValidation,
        ];
        assert_eq!(cases.len(), all_variants.len());
        for v in all_variants {
            assert!(cases.iter().any(|(k, _)| *k == v), "variant {v:?} missing");
        }

        for &(err, expected) in cases {
            let s = err.to_string();
            assert_eq!(&s, expected, "exact Display for {err:?}");
            assert!(s.is_ascii(), "Display must be ASCII: {s:?}");
            assert!(
                s.len() <= 96,
                "Display must be at most 96 bytes ({}): {s:?}",
                s.len(),
            );
            // No adversarial scenario / installable / failure fragments.
            assert!(!s.contains("ripgrep"));
            assert!(!s.contains("github"));
            assert!(!s.contains("detect-nix"));
            assert!(!s.contains("index-meta"));
            assert!(!s.contains("ADVERSARIAL"));
            let _: &dyn std::error::Error = &err;
        }
    }

    // 8. Deterministic serde JSON bytes and markdown for an accepted Incomplete.
    #[test]
    fn incomplete_report_serializes_and_renders_deterministically() {
        let manifest = benchmark_manifest();
        let host = report_host();
        let failure = real_failure(RealFailureKind::DetectNixCommand, None);

        let report = assemble_real_report(
            manifest,
            host.clone(),
            None,
            Vec::new(),
            vec![failure.clone()],
        )
        .expect("Incomplete report assembles");

        // Serde JSON bytes are deterministic for a single value.
        let json_a = serde_json::to_vec(&report).expect("serialize");
        let json_b = serde_json::to_vec(&report).expect("serialize again");
        assert_eq!(json_a, json_b);

        // Lossless round-trip.
        let back: Report = serde_json::from_slice(&json_a).expect("deserialize");
        assert_eq!(back, report);

        // Two independently assembled reports serialize identically.
        let report_b = assemble_real_report(manifest, host, None, Vec::new(), vec![failure])
            .expect("second Incomplete report assembles");
        assert_eq!(report_b, report);
        assert_eq!(serde_json::to_vec(&report_b).unwrap(), json_a);

        // render_markdown is deterministic.
        let md_a = crate::report::render_markdown(&report);
        let md_b = crate::report::render_markdown(&report);
        assert_eq!(md_a, md_b);
        assert!(!md_a.is_empty());
        // Markdown carries the Real mode, the Incomplete completeness, and the
        // recorded failure stage.
        assert!(md_a.contains("real"));
        assert!(md_a.contains("incomplete"));
        assert!(md_a.contains(RealFailureKind::DetectNixCommand.stage()));
    }

    // === RealPrivateHome (private workspace home) ==========================

    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    static TEST_FIXTURE_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// RAII-owned private temp directory for a test, created under
    /// [`std::env::temp_dir`] with a unique name and removed (best-effort) on
    /// drop — even if an assertion panics — so tests never leak fixtures.
    struct TestFixture {
        dir: PathBuf,
    }

    impl TestFixture {
        fn new(label: &str) -> Self {
            let counter = TEST_FIXTURE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let pid = std::process::id();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let name = format!("pkg-s4-real-test-{label}-{pid}-{now}-{counter}");
            let dir = std::env::temp_dir().join(name);
            std::fs::DirBuilder::new()
                .recursive(false)
                .mode(0o700)
                .create(&dir)
                .expect("test fixture directory must create");
            Self { dir }
        }

        fn path(&self) -> &std::path::Path {
            &self.dir
        }
    }

    impl Drop for TestFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// RAII guard that removes a single file on drop, so a marker created in
    /// the shared [`std::env::temp_dir`] for a test is cleaned even on panic.
    struct RemoveFileOnDrop(PathBuf);
    impl Drop for RemoveFileOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The raw Unix mode of `path` via [`std::fs::symlink_metadata`] (no
    /// symlink follow).
    fn mode_of(path: &std::path::Path) -> u32 {
        std::fs::symlink_metadata(path)
            .expect("symlink_metadata")
            .permissions()
            .mode()
    }

    /// Assert `path` is a real directory (NOT a symlink) at exactly owner-only
    /// mode 0700: the low nine mode bits are `rwx------`.
    fn assert_private_dir(path: &std::path::Path) {
        let md = std::fs::symlink_metadata(path).expect("symlink_metadata");
        assert!(md.is_dir(), "{} is a directory", path.display());
        assert!(
            !md.file_type().is_symlink(),
            "{} is not a symlink",
            path.display(),
        );
        let mode = md.permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "{} has exactly owner-only rwx mode 0700",
            path.display(),
        );
    }

    #[test]
    fn real_private_home_create_yields_absolute_0700_tree() {
        let home = RealPrivateHome::create().expect("create succeeds");
        let root = home.root();

        // Root is absolute and a DIRECT child of temp_dir.
        assert!(root.is_absolute(), "root is absolute");
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().as_path()),
            "root is a direct child of temp_dir",
        );

        // Root, cache, config are real directories at 0700, no symlinks.
        assert_private_dir(root);
        assert_private_dir(&root.join("cache"));
        assert_private_dir(&root.join("config"));

        // Exactly cache and config under the root (no extra entries).
        let mut entries: Vec<String> = std::fs::read_dir(root)
            .expect("read_dir")
            .map(|e| {
                e.expect("entry")
                    .file_name()
                    .to_str()
                    .expect("ascii name")
                    .to_owned()
            })
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["cache".to_string(), "config".to_string()]);
    }

    #[test]
    fn real_private_home_child_env_matches_real_child_env_five_entries() {
        let home = RealPrivateHome::create().expect("create succeeds");
        let got = home.child_env();
        let want = real_child_env(home.root());
        assert_eq!(got, want, "child_env delegates exactly to real_child_env");
        assert_eq!(got.len(), 5, "exactly five entries");
        // Spot-check the fixed locale entries and the HOME=root linkage.
        assert_eq!(got.get(OsStr::new("LANG")), Some(&OsString::from("C")));
        assert_eq!(got.get(OsStr::new("LC_ALL")), Some(&OsString::from("C")));
        assert_eq!(
            got.get(OsStr::new("HOME")),
            Some(&home.root().as_os_str().to_owned()),
        );
    }

    #[test]
    fn real_private_home_drop_removes_root_not_parent_or_sibling() {
        let home = RealPrivateHome::create().expect("create succeeds");
        let root = home.root().to_path_buf();
        let parent = root.parent().expect("root has a parent").to_path_buf();
        // Root's parent IS temp_dir.
        assert_eq!(parent, std::env::temp_dir());

        // An unrelated sibling marker inside temp_dir, RAII-cleaned.
        let sibling = parent.join(format!(
            "pkg-s4-real-sibling-{}-{}",
            std::process::id(),
            TEST_FIXTURE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::write(&sibling, b"marker").expect("write sibling");
        let _sibling_guard = RemoveFileOnDrop(sibling.clone());

        assert!(root.exists(), "root exists before drop");
        assert!(sibling.exists(), "sibling exists before drop");
        drop(home);
        assert!(!root.exists(), "Drop removed the owned root");
        assert!(parent.exists(), "Drop did NOT remove the temp_dir parent");
        assert!(
            sibling.exists(),
            "Drop did NOT remove the unrelated sibling"
        );
    }

    #[test]
    fn real_private_home_repeated_creates_distinct_roots() {
        let a = RealPrivateHome::create().expect("create a");
        let root_a = a.root().to_path_buf();
        let b = RealPrivateHome::create().expect("create b");
        let root_b = b.root().to_path_buf();
        assert_ne!(root_a, root_b, "repeated creates yield distinct roots");
        assert!(root_a.exists(), "first root still exists while alive");
        assert!(root_b.exists(), "second root still exists while alive");
    }

    #[test]
    fn home_create_at_never_reuses_or_alters_preexisting_entries() {
        let fx = TestFixture::new("createat");

        // Pre-existing DIRECTORY candidate carrying a kept file.
        let dir_candidate = fx.path().join("pre-dir");
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&dir_candidate)
            .expect("mkdir pre-dir");
        std::fs::write(dir_candidate.join("inside"), b"kept").expect("write inside");
        let dir_mode_before = mode_of(&dir_candidate);

        // Pre-existing regular FILE candidate.
        let file_candidate = fx.path().join("pre-file");
        std::fs::write(&file_candidate, b"file-body").expect("write pre-file");

        // Pre-existing SYMLINK candidate (dangling — points nowhere).
        let sym_target = fx.path().join("nowhere");
        let sym_candidate = fx.path().join("pre-sym");
        std::os::unix::fs::symlink(&sym_target, &sym_candidate).expect("symlink");

        // create_at must DECLINE each (Ok(None)) without altering anything.
        assert!(
            matches!(home_create_at(&dir_candidate), Ok(None)),
            "preexisting directory is declined",
        );
        assert!(
            matches!(home_create_at(&file_candidate), Ok(None)),
            "preexisting file is declined",
        );
        assert!(
            matches!(home_create_at(&sym_candidate), Ok(None)),
            "preexisting symlink is declined",
        );

        // Directory: still a directory, same mode, kept content.
        let dir_md = std::fs::symlink_metadata(&dir_candidate).expect("md dir");
        assert!(dir_md.is_dir());
        assert!(!dir_md.file_type().is_symlink());
        assert_eq!(dir_md.permissions().mode(), dir_mode_before);
        assert_eq!(
            std::fs::read(dir_candidate.join("inside")).expect("read inside"),
            b"kept",
        );

        // File: still a regular file, same content.
        let file_md = std::fs::symlink_metadata(&file_candidate).expect("md file");
        assert!(file_md.is_file());
        assert!(!file_md.file_type().is_symlink());
        assert_eq!(
            std::fs::read(&file_candidate).expect("read file"),
            b"file-body",
        );

        // Symlink: still a dangling symlink, target unchanged.
        let sym_md = std::fs::symlink_metadata(&sym_candidate).expect("md sym");
        assert!(sym_md.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&sym_candidate).expect("readlink"),
            sym_target,
        );
    }

    #[test]
    fn home_populate_children_failure_cleans_owned_root() {
        let fx = TestFixture::new("childfail");
        // Simulate create_at's root step: an owned root we just created.
        let root = fx.path().join("owned-root");
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&root)
            .expect("mkdir owned-root");
        // Make the root READ-ONLY so child creation fails with EACCES — a
        // controlled failure with NO global environment mutation.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500))
            .expect("chmod root read-only");
        assert!(root.exists(), "owned root exists before populate");

        let err = home_populate_children(&root).expect_err("child creation fails");
        assert_eq!(err, RealPrivateHomeError::ChildCreate);

        // The helper cleaned the just-created owned root on failure.
        assert!(!root.exists(), "owned root cleaned on child failure");
        // The fixture parent is untouched.
        assert!(fx.path().exists(), "fixture parent untouched");
    }

    #[test]
    fn home_validate_layout_rejects_real_dir_with_mode_0600() {
        let fx = TestFixture::new("v0600");
        // Build a COMPLETE otherwise-valid layout: root + cache + config, each
        // created at exactly 0700. Then chmod ONLY the root to 0600 (owner
        // read/write, NO owner-execute, no group, no other) — exactly the
        // "too-tight" directory a looser check would miss.
        //
        // This is a true, discriminating regression test for the exact-0700
        // invariant:
        //   * The OLD group/other-only check (`mode & 0o077 != 0`) accepts
        //     EVERY member of this layout (root 0600, cache 0700, config 0700
        //     all have zero group/other bits) and would return Ok.
        //   * The fixed exact-0700 check (`mode & 0o777 != 0o700`) rejects the
        //     0600 root and must return Err: a private workspace directory must
        //     be traversable, which requires the owner-execute bit that 0600
        //     lacks.
        let root = fx.path().join("too-tight");
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&root)
            .expect("mkdir too-tight");
        // Create BOTH children at exactly 0700 so the layout is otherwise
        // valid; only the root mode is wrong. The OLD check would accept this.
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(root.join("cache"))
            .expect("mkdir cache");
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(root.join("config"))
            .expect("mkdir config");
        assert_eq!(
            mode_of(&root.join("cache")) & 0o777,
            0o700,
            "cache child really is mode 0700",
        );
        assert_eq!(
            mode_of(&root.join("config")) & 0o777,
            0o700,
            "config child really is mode 0700",
        );
        // chmod ONLY root (NOT subject to umask) to the exact wrong mode.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o600))
            .expect("chmod too-tight to 0600");
        assert_eq!(
            mode_of(&root) & 0o777,
            0o600,
            "fixture root really is mode 0600 (children still 0700)",
        );

        assert_eq!(
            home_validate_layout(&root),
            Err(()),
            "complete layout with a 0600 root is rejected (not exactly 0700)",
        );

        // Restore owner-execute so the fixture's remove_dir_all cleanup can
        // traverse and remove this directory. No process-global umask or
        // environment is mutated.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("restore 0700 for cleanup");
    }

    #[test]
    fn real_private_home_error_display_is_ascii_bounded_and_clean() {
        let cases: &[(RealPrivateHomeError, &str)] = &[
            (
                RealPrivateHomeError::TempNotAbsolute,
                "private home base temp directory is not absolute",
            ),
            (
                RealPrivateHomeError::Exhausted,
                "private home creation exhausted unique candidates",
            ),
            (
                RealPrivateHomeError::RootCreate,
                "private home root directory could not be created",
            ),
            (
                RealPrivateHomeError::ChildCreate,
                "private home child directory could not be created",
            ),
            (
                RealPrivateHomeError::Validate,
                "private home post-creation validation failed",
            ),
        ];
        // Exhaustive: every variant appears exactly once.
        let all = [
            RealPrivateHomeError::TempNotAbsolute,
            RealPrivateHomeError::Exhausted,
            RealPrivateHomeError::RootCreate,
            RealPrivateHomeError::ChildCreate,
            RealPrivateHomeError::Validate,
        ];
        assert_eq!(cases.len(), all.len(), "every variant covered exactly once");
        for v in all {
            assert!(cases.iter().any(|(k, _)| *k == v), "variant {v:?} present");
        }

        for &(err, expected) in cases {
            let s = err.to_string();
            assert_eq!(&s, expected, "exact Display for {err:?}");
            assert!(s.is_ascii(), "Display is ASCII: {s:?}");
            assert!(
                s.len() <= 96,
                "Display at most 96 bytes ({}): {s:?}",
                s.len(),
            );
            // No temp path fragments, no candidate name, no OS error text.
            assert!(!s.contains("/tmp"));
            assert!(!s.contains("pkg-s4-real"));
            assert!(!s.contains("os error"));
            let _: &dyn std::error::Error = &err;
        }
    }

    // === execute_real_scenario (success) ===================================
    //
    // Focused SUCCESS tests for the Real-lane scenario driver. They drive
    // EXACTLY one scenario through a FAKE executor that captures every
    // forwarded spec/flavor and returns a canned `Exited(0)` outcome, then
    // assert the full success shape WITHOUT spawning, touching the network
    // /store, or mutating global state: no failure, exactly `warmup + measured`
    // observations in Warmup-then-Measured order with contiguous phase-local
    // indices, every observation an exact descriptor clone carrying
    // `SourceWarmProcessCold` and a success outcome, exactly one executor call
    // per iteration, and every forwarded spec matching the established exact
    // argv (program / home env / descriptor stdout cap / shared stderr cap /
    // descriptor timeout, exactly one `--offline`) plus the forwarded flavor.

    /// Shared driver for the two `execute_real_scenario` success tests. Drives
    /// `descriptor` with `single_attr` set and a fake executor that captures
    /// every forwarded spec/flavor and returns a canned `Exited(0)` outcome,
    /// then asserts the full success contract described above. `expected_argv`
    /// is the established exact eval argv for `descriptor.system` (single-attr
    /// or index-meta, from [`single_eval_argv`] / [`index_eval_argv`]).
    fn run_real_scenario_success(
        descriptor: &ScenarioDescriptor,
        single_attr: bool,
        expected_argv: &[OsString],
    ) {
        let manifest = benchmark_manifest();
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        // Manifest's shared stderr cap, supplied as the shared nonzero cap.
        let stderr_cap = NonZeroU64::new(manifest.caps.stderr_bytes).expect("nonzero stderr cap");
        // Overall deadline straight from the manifest, measured from now.
        let overall_timeout = Duration::from_secs(manifest.timeouts.overall_seconds);
        let started = Instant::now();
        let flavor = TimeFlavor::Gnu;

        let mut calls = 0u32;
        let mut captured_specs: Vec<CommandSpec> = Vec::new();
        let mut captured_flavors: Vec<TimeFlavor> = Vec::new();
        let mut executor = |spec: &CommandSpec, flav: TimeFlavor| {
            calls += 1;
            captured_specs.push(spec.clone());
            captured_flavors.push(flav);
            Ok(version_probe_outcome(UnixStatus::Exited(0), &[]))
        };

        let result = execute_real_scenario(
            manifest,
            &nix_bin,
            &home,
            descriptor,
            single_attr,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        // No failure on full success.
        assert!(
            result.failure.is_none(),
            "no failure on full success, got {:?}",
            result.failure,
        );

        let warmup = descriptor.warmup as usize;
        let measured = descriptor.measured as usize;
        let total = warmup + measured;

        // Exactly warmup + measured observations and executor calls.
        assert_eq!(
            result.observations.len(),
            total,
            "exactly warmup ({warmup}) + measured ({measured}) observations",
        );
        assert_eq!(
            calls as usize, total,
            "exactly one executor call per iteration",
        );
        assert_eq!(captured_specs.len(), total);
        assert_eq!(captured_flavors.len(), total);

        // Record order (Warmup then Measured) and contiguous phase-local
        // indices: warmup [0..warmup], then measured [0..measured].
        for (i, obs) in result.observations.iter().enumerate() {
            let (want_record, want_phase_index) = if i < warmup {
                (Record::Warmup, i as u32)
            } else {
                (Record::Measured, (i - warmup) as u32)
            };
            assert_eq!(obs.record, want_record, "record order at observation {i}");
            assert_eq!(
                obs.phase_index, want_phase_index,
                "contiguous phase-local index at observation {i}",
            );
            // Every observation is an EXACT descriptor clone, carries the only
            // honest Real cache label, and a success outcome.
            assert_eq!(obs.descriptor, *descriptor, "exact descriptor clone at {i}");
            assert_eq!(
                obs.cache,
                CacheLabel::SourceWarmProcessCold,
                "cache label at observation {i}",
            );
            assert_eq!(
                obs.outcome.status,
                UnixStatus::Exited(0),
                "success outcome at observation {i}",
            );
        }

        // Every forwarded spec matches the established exact argv (program /
        // home env / descriptor stdout cap / shared stderr cap / descriptor
        // timeout, exactly one --offline) and the forwarded flavor.
        let expected_env = home.child_env();
        let expected_timeout = Duration::from_secs(descriptor.timeout_seconds);
        for (i, spec) in captured_specs.iter().enumerate() {
            assert_eq!(spec.program, nix_bin, "program at call {i}");
            assert_eq!(spec.args, expected_argv, "argv at call {i}");
            assert_eq!(spec.env, expected_env, "home env at call {i}");
            assert_eq!(
                spec.stdout_cap, descriptor.stdout_cap_bytes,
                "descriptor stdout cap at call {i}",
            );
            assert_eq!(spec.stderr_cap, stderr_cap, "shared stderr cap at call {i}");
            assert_eq!(
                spec.timeout, expected_timeout,
                "descriptor timeout at call {i}",
            );
            assert_eq!(
                count_offline(&spec.args),
                1,
                "exactly one --offline at call {i}",
            );
            assert_eq!(captured_flavors[i], flavor, "forwarded flavor at call {i}");
        }
    }

    #[test]
    fn execute_real_scenario_single_attr_success_captures_every_spec() {
        let manifest = benchmark_manifest();
        let descriptors = descriptors(manifest).expect("canonical descriptors");
        // Canonical descriptor[0]: the host-only single-attr scenario
        // (single_attr=true). 1 warmup + 5 measured = 6 observations; phase-local
        // indices [0] then [0,1,2,3,4].
        let descriptor = &descriptors[0];
        assert_eq!(
            descriptor.warmup + descriptor.measured,
            6,
            "descriptor[0] is the single-attr host scenario (1 + 5)",
        );
        let checked = check_system(manifest, &descriptor.system).expect("host system checked");
        let expected_argv = single_eval_argv(manifest, &checked);
        run_real_scenario_success(descriptor, true, &expected_argv);
    }

    #[test]
    fn execute_real_scenario_index_meta_success_captures_every_spec() {
        let manifest = benchmark_manifest();
        let descriptors = descriptors(manifest).expect("canonical descriptors");
        // Canonical descriptor[1]: the first index-meta scenario
        // (single_attr=false, x86_64-linux). 1 warmup + 3 measured = 4
        // observations; phase-local indices [0] then [0,1,2].
        let descriptor = &descriptors[1];
        assert_eq!(
            descriptor.warmup + descriptor.measured,
            4,
            "descriptor[1] is the first index-meta scenario (1 + 3)",
        );
        let checked = check_system(manifest, &descriptor.system).expect("system checked");
        let expected_argv = index_eval_argv(manifest, &checked);
        run_real_scenario_success(descriptor, false, &expected_argv);
    }

    // === execute_real_scenario (failure) ===================================
    //
    // Focused FAILURE tests for the Real-lane scenario driver. They exercise
    // each of the four closed short-circuit / failure points — spec-build
    // short-circuit (EvalCommand), a nonzero exit mid-run (EvalOutcome, with
    // the honest prefix preserved), a first-call command-level failure
    // (EvalCommand) / signal (EvalOutcome), the per-command overall deadline
    // (OverallTimeout, checked before any execution), and the system check
    // (ScenarioAssembly, checked before any execution) — WITHOUT spawning,
    // touching the network/store, or mutating global state. They reuse the
    // existing private-home / nonexistent-bin / manifest-cap helpers and the
    // shared `version_probe_outcome` canned-outcome builder.

    /// Common inputs for the `execute_real_scenario` failure tests: a private
    /// home, a nonexistent absolute `nix_bin` under it, the manifest's shared
    /// stderr cap, the manifest overall deadline, a fresh start instant, and
    /// the GNU time flavor. Reuses the existing helpers; performs NO spawning,
    /// network, or global mutation.
    fn scenario_driver_inputs(
        manifest: &Manifest,
    ) -> (
        RealPrivateHome,
        PathBuf,
        NonZeroU64,
        Duration,
        Instant,
        TimeFlavor,
    ) {
        let home = RealPrivateHome::create().expect("home");
        let nix_bin = nonexistent_nix_bin(&home);
        let stderr_cap = NonZeroU64::new(manifest.caps.stderr_bytes).expect("nonzero stderr cap");
        let overall_timeout = Duration::from_secs(manifest.timeouts.overall_seconds);
        (
            home,
            nix_bin,
            stderr_cap,
            overall_timeout,
            Instant::now(),
            TimeFlavor::Gnu,
        )
    }

    /// The canonical descriptor[0]: the host-only single-attr scenario
    /// (`single_attr = true`, 1 warmup + 5 measured). Its host system passes
    /// [`check_system`], so the driver reaches the timeout/spec/executor path.
    fn canonical_single_attr_descriptor(manifest: &Manifest) -> ScenarioDescriptor {
        let descriptors = descriptors(manifest).expect("canonical descriptors");
        assert_eq!(
            descriptors[0].warmup + descriptors[0].measured,
            6,
            "descriptor[0] is the single-attr host scenario (1 + 5)",
        );
        descriptors[0].clone()
    }

    #[test]
    fn execute_real_scenario_relative_nix_short_circuits_before_executor() {
        let manifest = benchmark_manifest();
        let descriptor = canonical_single_attr_descriptor(manifest);
        let (home, _nix_bin, stderr_cap, overall_timeout, started, flavor) =
            scenario_driver_inputs(manifest);

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(UnixStatus::Exited(0), &[]))
        };

        // A relative nix path makes `CommandSpec::new` reject the program, so
        // the spec build fails before any execution.
        let rel = PathBuf::from("nix");
        let result = execute_real_scenario(
            manifest,
            &rel,
            &home,
            &descriptor,
            true,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        assert_eq!(result.failure, Some(RealFailureKind::EvalCommand));
        assert!(
            result.observations.is_empty(),
            "no observation before any execution",
        );
        assert_eq!(calls, 0, "spec-build failure short-circuits the executor");
    }

    #[test]
    fn execute_real_scenario_nonzero_exit_preserves_honest_prefix() {
        let manifest = benchmark_manifest();
        // descriptor[0]: 1 warmup + 5 measured. The executor succeeds for calls
        // 1..3 (warmup0, measured0, measured1), then returns Exited(7) on call 4
        // (measured2). The honest 3-observation prefix MUST be preserved and the
        // failed call MUST NOT fabricate an observation.
        let descriptor = canonical_single_attr_descriptor(manifest);
        let (home, nix_bin, stderr_cap, overall_timeout, started, flavor) =
            scenario_driver_inputs(manifest);

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            if calls <= 3 {
                Ok(version_probe_outcome(UnixStatus::Exited(0), &[]))
            } else {
                Ok(version_probe_outcome(UnixStatus::Exited(7), &[]))
            }
        };

        let result = execute_real_scenario(
            manifest,
            &nix_bin,
            &home,
            &descriptor,
            true,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        assert_eq!(result.failure, Some(RealFailureKind::EvalOutcome));
        assert_eq!(
            calls, 4,
            "executor called once per iteration up to and including the failure",
        );
        // Exactly the 3 honest observations captured before the failing call.
        assert_eq!(result.observations.len(), 3, "honest prefix preserved");
        let expected = [
            (Record::Warmup, 0),
            (Record::Measured, 0),
            (Record::Measured, 1),
        ];
        for (i, (want_record, want_phase)) in expected.iter().enumerate() {
            assert_eq!(result.observations[i].record, *want_record, "record at {i}");
            assert_eq!(
                result.observations[i].phase_index, *want_phase,
                "phase index at {i}",
            );
        }
    }

    /// Drive descriptor[0] through a fake executor whose FIRST call returns
    /// `make_result()`, and assert the mapped `expected` failure kind, an EMPTY
    /// observation prefix, and EXACTLY one executor call. Compact driver for the
    /// first-call CommandError (-> EvalCommand) / Signaled (-> EvalOutcome) cases.
    fn real_scenario_first_call_maps(
        make_result: impl Fn() -> Result<CommandOutcome, CommandError>,
        expected: RealFailureKind,
    ) {
        let manifest = benchmark_manifest();
        let descriptor = canonical_single_attr_descriptor(manifest);
        let (home, nix_bin, stderr_cap, overall_timeout, started, flavor) =
            scenario_driver_inputs(manifest);

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            make_result()
        };

        let result = execute_real_scenario(
            manifest,
            &nix_bin,
            &home,
            &descriptor,
            true,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        assert_eq!(result.failure, Some(expected));
        assert!(
            result.observations.is_empty(),
            "no observation before a successful iteration",
        );
        assert_eq!(calls, 1, "executor called exactly once");
    }

    #[test]
    fn execute_real_scenario_first_call_command_error_maps_eval_command() {
        // An executor CommandError on the very first call maps to EvalCommand
        // with an empty prefix.
        real_scenario_first_call_maps(|| Err(CommandError::Rss), RealFailureKind::EvalCommand);
        real_scenario_first_call_maps(
            || {
                Err(CommandError::Spawn {
                    kind: std::io::ErrorKind::NotFound,
                })
            },
            RealFailureKind::EvalCommand,
        );
    }

    #[test]
    fn execute_real_scenario_first_call_signal_maps_eval_outcome() {
        // A Signaled outcome on the very first call maps to EvalOutcome with an
        // empty prefix.
        real_scenario_first_call_maps(
            || Ok(version_probe_outcome(UnixStatus::Signaled(9), &[])),
            RealFailureKind::EvalOutcome,
        );
    }

    #[test]
    fn execute_real_scenario_sub_min_overall_timeout_maps_overall_timeout_before_executor() {
        let manifest = benchmark_manifest();
        // descriptor[0] targets the host system, so the system check (step 1)
        // passes and the driver reaches the per-command deadline check.
        let descriptor = canonical_single_attr_descriptor(manifest);
        let (home, nix_bin, stderr_cap, _overall_timeout, _started, flavor) =
            scenario_driver_inputs(manifest);
        // 1 ns is below command::MIN_TIMEOUT (1 ms). Deterministic regardless of
        // uptime: select_timeout rejects either `elapsed >= overall` OR
        // `remaining < MIN_TIMEOUT`, and one of the two always holds, so the
        // deadline fires BEFORE any execution. No sleep / uptime assumption.
        let overall_timeout = Duration::from_nanos(1);
        let started = Instant::now();

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(UnixStatus::Exited(0), &[]))
        };

        let result = execute_real_scenario(
            manifest,
            &nix_bin,
            &home,
            &descriptor,
            true,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        assert_eq!(result.failure, Some(RealFailureKind::OverallTimeout));
        assert!(
            result.observations.is_empty(),
            "no observation before the deadline check",
        );
        assert_eq!(calls, 0, "deadline is checked before any execution");
    }

    #[test]
    fn execute_real_scenario_invalid_system_maps_scenario_assembly_before_executor() {
        let manifest = benchmark_manifest();
        // Clone descriptor[0] with an INVALID system triple: check_system rejects
        // it, so the system check (step 1) fails before any timeout/spec/exec
        // work. single_attr / installable are irrelevant once the system check
        // fails.
        let mut invalid = canonical_single_attr_descriptor(manifest);
        invalid.system = "nope-x86_64".to_owned();
        let (home, nix_bin, stderr_cap, overall_timeout, started, flavor) =
            scenario_driver_inputs(manifest);

        let mut calls = 0u32;
        let mut executor = |_spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            Ok(version_probe_outcome(UnixStatus::Exited(0), &[]))
        };

        let result = execute_real_scenario(
            manifest,
            &nix_bin,
            &home,
            &invalid,
            true,
            stderr_cap,
            overall_timeout,
            started,
            flavor,
            &mut executor,
        );

        assert_eq!(result.failure, Some(RealFailureKind::ScenarioAssembly));
        assert!(
            result.observations.is_empty(),
            "system check fails before any observation",
        );
        assert_eq!(calls, 0, "system check short-circuits the executor");
    }

    // === run_real_with_executor (setup failures) ===========================
    //
    // Focused tests for the private generic core's SETUP failures — the
    // version-probe and prefetch phases (composition steps 2–3) — exercised
    // with NO real spawning: a fake executor captures every forwarded spec
    // and returns a canned outcome, and an ABSOLUTE fixture nix path (an
    // absent child of a freshly-created private home, hence never executed)
    // is supplied verbatim with `Instant::now()`. Each asserts the closed-
    // shape Incomplete report validates, is Mode::Real + harness_only false,
    // has ZERO scenarios (no fabricated data), and carries exactly ONE
    // GLOBAL failure (`scenario == run`) at the documented scope/kind, and
    // that the executor is invoked exactly the expected number of times so
    // no later phase ran. No spawning, no network/store, no global-env
    // mutation, and no new helpers beyond the three small ones below.

    /// Shared inputs for the setup-failure tests: a freshly-created private
    /// home, its absent absolute fixture nix child, and `Instant::now()`. The
    /// fixture binary is NEVER executed — every effect is deferred to the
    /// injected fake executor. (`run_real_with_executor` creates its OWN home
    /// internally; this one only derives a guaranteed-absent absolute
    /// `nix_bin`, mirroring the full-success test's setup.)
    fn runner_setup_inputs() -> (RealPrivateHome, PathBuf, Instant) {
        let home = RealPrivateHome::create().expect("private home");
        let nix_bin = nonexistent_nix_bin(&home);
        (home, nix_bin, Instant::now())
    }

    /// The single-token version argv (`["--version"]`) as an owned
    /// [`Vec<OsString>`], for asserting the forwarded probe spec.
    fn version_argv_owned() -> Vec<OsString> {
        VERSION_ARGV.iter().copied().map(OsString::from).collect()
    }

    /// Assert the closed-shape Incomplete setup-failure contract for `report`:
    /// it validates, is Mode::Real + Incomplete, `harness_only` false, has ZERO
    /// scenarios (no fabricated data), and carries exactly ONE failure at
    /// GLOBAL scope (`scenario == RUN_SCENARIO`) whose (stage, message) matches
    /// `kind`.
    fn assert_setup_failure_report(report: &Report, kind: RealFailureKind) {
        report
            .validate()
            .expect("Incomplete setup report validates");
        assert_eq!(report.mode, Mode::Real, "mode is Real");
        assert_eq!(
            report.completeness,
            Completeness::Incomplete,
            "setup failure is Incomplete",
        );
        assert!(!report.harness_only, "Real run is not harness-only");
        assert!(report.scenarios.is_empty(), "no fabricated scenarios");
        assert_eq!(report.failures.len(), 1, "exactly one failure");
        let failure = &report.failures[0];
        assert_eq!(failure.scenario, RUN_SCENARIO, "failure is global (run)");
        assert_eq!(failure.stage, kind.stage(), "exact failure stage");
        assert_eq!(failure.message, kind.message(), "exact failure message");
    }

    // 1. First executor call returns a deterministic CommandError (Spawn): the
    //    version probe maps it to DetectNixCommand. EXACTLY one executor call;
    //    no prefetch; `nix_version` None.
    #[test]
    fn run_real_with_executor_version_command_error_maps_detect_nix_command() {
        let (_home, nix_bin, started) = runner_setup_inputs();

        let mut calls = 0u32;
        let mut captured: Vec<CommandSpec> = Vec::new();
        let mut executor = |spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            captured.push(spec.clone());
            Err(CommandError::Spawn {
                kind: std::io::ErrorKind::NotFound,
            })
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor)
            .expect("version command error yields an Incomplete report, not RealRunError");

        assert_setup_failure_report(&report, RealFailureKind::DetectNixCommand);
        assert_eq!(
            report.nix_version, None,
            "no version detected on command error"
        );
        assert_eq!(calls, 1, "version probe called exactly once");
        assert_eq!(captured.len(), 1, "exactly one spec forwarded");
        assert_eq!(
            captured[0].args,
            version_argv_owned(),
            "probe is the version spec"
        );
    }

    // 2. Version exits 0 but stdout is malformed: parse_nix_version rejects it
    //    => DetectNixVersion. EXACTLY one call; no prefetch; `nix_version` None.
    #[test]
    fn run_real_with_executor_version_malformed_stdout_maps_detect_nix_version() {
        let (_home, nix_bin, started) = runner_setup_inputs();

        let mut calls = 0u32;
        let mut captured: Vec<CommandSpec> = Vec::new();
        let mut executor = |spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            captured.push(spec.clone());
            Ok(version_probe_outcome(UnixStatus::Exited(0), b"garbage"))
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor)
            .expect("malformed version stdout yields an Incomplete report");

        assert_setup_failure_report(&report, RealFailureKind::DetectNixVersion);
        assert_eq!(
            report.nix_version, None,
            "no version detected on parse failure"
        );
        assert_eq!(calls, 1, "no prefetch after a version-parse failure");
        assert_eq!(captured.len(), 1, "exactly one spec forwarded");
        assert_eq!(
            captured[0].args,
            version_argv_owned(),
            "probe is the version spec"
        );
    }

    // 3. Version exits 0 but reports a DIFFERENT version (2.99.0 != 2.34.8):
    //    DetectNixVersion. EXACTLY one call; no prefetch; `nix_version` None.
    #[test]
    fn run_real_with_executor_version_mismatch_maps_detect_nix_version() {
        let (_home, nix_bin, started) = runner_setup_inputs();

        let mut calls = 0u32;
        let mut captured: Vec<CommandSpec> = Vec::new();
        let mut executor = |spec: &CommandSpec, _flavor: TimeFlavor| {
            calls += 1;
            captured.push(spec.clone());
            Ok(version_probe_outcome(
                UnixStatus::Exited(0),
                b"nix (Nix) 2.99.0\n",
            ))
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor)
            .expect("version mismatch yields an Incomplete report");

        assert_setup_failure_report(&report, RealFailureKind::DetectNixVersion);
        assert_eq!(
            report.nix_version, None,
            "mismatched version is not preserved"
        );
        assert_eq!(calls, 1, "no prefetch after a version mismatch");
        assert_eq!(captured.len(), 1, "exactly one spec forwarded");
        assert_eq!(
            captured[0].args,
            version_argv_owned(),
            "probe is the version spec"
        );
    }

    // 4. Version succeeds (exact 2.34.8); prefetch exits 0 with syntactically
    //    valid JSON whose `hash` does NOT match the pinned NAR hash =>
    //    PrefetchVerification. `nix_version` preserved Some("2.34.8"); EXACTLY
    //    two calls (version + prefetch); no eval phase; zero scenarios.
    #[test]
    fn run_real_with_executor_prefetch_hash_mismatch_maps_prefetch_verification() {
        let (_home, nix_bin, started) = runner_setup_inputs();
        let manifest = benchmark_manifest();
        // Syntactically valid JSON (parses + has string fields) but a hash that
        // does NOT equal the pinned nar hash => verify_prefetch rejects it.
        let bad_prefetch = json!({
            "hash": "sha256-deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef=",
            "storePath": VALID_STORE_PATH,
        });
        let bad_bytes = prefetch_bytes(&bad_prefetch);

        let mut calls = 0u32;
        let mut captured: Vec<CommandSpec> = Vec::new();
        let mut executor = |spec: &CommandSpec, _flavor: TimeFlavor| {
            let n = calls;
            calls += 1;
            captured.push(spec.clone());
            Ok(match n {
                0 => version_probe_outcome(UnixStatus::Exited(0), b"nix (Nix) 2.34.8\n"),
                1 => version_probe_outcome(UnixStatus::Exited(0), &bad_bytes),
                _ => unreachable!("prefetch verification failure stops before call {n}"),
            })
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor)
            .expect("prefetch verification failure yields an Incomplete report");

        assert_setup_failure_report(&report, RealFailureKind::PrefetchVerification);
        assert_eq!(
            report.nix_version.as_deref(),
            Some("2.34.8"),
            "detected version preserved past the version phase",
        );
        assert_eq!(calls, 2, "version probe + prefetch, then no eval phase");
        assert_eq!(captured.len(), 2, "exactly two specs forwarded");
        assert_eq!(
            captured[0].args,
            version_argv_owned(),
            "call 0 is the version probe"
        );
        assert_eq!(
            captured[1].args,
            prefetch_argv(manifest),
            "call 1 is the prefetch"
        );
    }

    // === run_real_with_executor (comprehensive success) ====================
    //
    // Drives the ENTIRE Real-lane pipeline through the private generic core
    // with NO real spawning: a fake executor indexed by call captures every
    // forwarded spec/flavor and returns canned `Exited(0)` outcomes (version
    // probe, verified prefetch, then every warmup + measured eval iteration).
    // Asserts the full end-to-end success contract WITHOUT touching the
    // network/store or mutating global state: exactly 24 calls, a Complete
    // validated Real report with the exact detected `nix_version` and five
    // canonical scenarios, and every forwarded spec matching the established
    // exact argv / fail-closed env / host time flavor.

    /// Build a deterministic `Exited(0)` eval [`CommandOutcome`] for call index
    /// `n` (2..=23): wall-ms / max-RSS / output totals vary by call so the
    /// measured statistics are non-trivial, but every value is fixed by `n`.
    fn deterministic_eval_outcome(n: u32) -> CommandOutcome {
        let out_bytes = u64::from(n);
        CommandOutcome {
            status: UnixStatus::Exited(0),
            stdout: vec![b' '; out_bytes as usize],
            cleaned_stderr: String::new(),
            stdout_total_bytes: out_bytes,
            stderr_total_bytes: 0,
            wall_ms: 100 + u64::from(n),
            max_rss_kib: 50_000 + u64::from(n) * 7,
        }
    }

    #[test]
    fn run_real_with_executor_full_success_drives_24_calls_and_complete_report() {
        let manifest = benchmark_manifest();
        // Canonical plan: descriptor[0] is the host single-attr scenario, then
        // one index-meta descriptor per manifest system. The checked systems
        // build the established exact eval argv checked below.
        let descriptors = descriptors(manifest).expect("canonical descriptors");
        assert_eq!(descriptors.len(), 5, "1 single-attr + 4 index-meta");
        let checked_single =
            check_system(manifest, &descriptors[0].system).expect("host system checked");
        let single_argv = single_eval_argv(manifest, &checked_single);
        let index_argv: Vec<Vec<OsString>> = descriptors[1..]
            .iter()
            .map(|d| {
                index_eval_argv(
                    manifest,
                    &check_system(manifest, &d.system).expect("checked"),
                )
            })
            .collect();

        // Absolute fixture nix path that is NEVER executed; the only effect is
        // deferred to the fake executor. The home is created INSIDE the runner.
        let home = RealPrivateHome::create().expect("private home");
        let nix_bin = nonexistent_nix_bin(&home);
        let started = Instant::now();
        let flavor = host_time_flavor();
        let version_stdout: &[u8] = b"nix (Nix) 2.34.8\n";
        let prefetch_stdout = prefetch_bytes(&valid_prefetch_json());

        let mut calls: u32 = 0;
        let mut captured_specs: Vec<CommandSpec> = Vec::new();
        let mut captured_flavors: Vec<TimeFlavor> = Vec::new();
        let mut executor = |spec: &CommandSpec, flav: TimeFlavor| {
            let n = calls;
            calls += 1;
            if n >= 24 {
                panic!("executor called beyond 24 calls (call {n})");
            }
            captured_specs.push(spec.clone());
            captured_flavors.push(flav);
            Ok(match n {
                0 => version_probe_outcome(UnixStatus::Exited(0), version_stdout),
                1 => CommandOutcome {
                    status: UnixStatus::Exited(0),
                    stdout: prefetch_stdout.clone(),
                    cleaned_stderr: String::new(),
                    stdout_total_bytes: prefetch_stdout.len() as u64,
                    stderr_total_bytes: 0,
                    wall_ms: 1234,
                    max_rss_kib: 4096,
                },
                _ => deterministic_eval_outcome(n),
            })
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor).expect("Ok report");

        // (1) Exactly 24 calls: 1 version + 1 prefetch + 22 eval (6 single +
        // four*4 index). Queue/count exhausted; never called beyond 24.
        assert_eq!(calls, 24, "exactly 24 calls");
        assert_eq!(captured_specs.len(), 24);
        assert_eq!(captured_flavors.len(), 24);

        // (2) A Complete, validated Real report.
        assert!(report.validate().is_ok(), "report validates");
        assert_eq!(report.mode, Mode::Real);
        assert_eq!(report.completeness, Completeness::Complete);
        assert!(!report.harness_only, "Real run is not harness-only");
        assert_eq!(
            report.nix_version.as_deref(),
            Some(manifest.nix.version.as_str()),
            "detected nix_version matches the pin",
        );
        assert_eq!(report.nix_version.as_deref(), Some("2.34.8"));
        assert!(report.failures.is_empty(), "no failures on full success");
        assert_eq!(
            report.scenarios.len(),
            5,
            "exactly five canonical scenarios"
        );

        // (3) Per-scenario shape: sample totals [6,4,4,4,4]=22, contiguous
        // global indices per scenario, Warmup-then-Measured, only
        // SourceWarmProcessCold, not skipped, complete metrics + output, exit
        // 0; statistics count over MEASURED samples only.
        let expected_totals = [6u32, 4, 4, 4, 4];
        let expected_measured = [5u32, 3, 3, 3, 3];
        for (i, scen) in report.scenarios.iter().enumerate() {
            let desc = &descriptors[i];
            assert_eq!(scen.name, desc.name, "scenario name at {i}");
            assert_eq!(scen.system, desc.system, "scenario system at {i}");
            assert_eq!(scen.warmup, desc.warmup, "warmup count at {i}");
            assert_eq!(scen.measured, desc.measured, "measured count at {i}");
            assert_eq!(
                scen.samples.len(),
                expected_totals[i] as usize,
                "sample totals [6,4,4,4,4] at scenario {i}",
            );
            let warmup = desc.warmup as usize;
            for (j, sample) in scen.samples.iter().enumerate() {
                assert_eq!(
                    sample.index, j as u32,
                    "contiguous global sample index at scenario {i} sample {j}",
                );
                let want = if j < warmup {
                    Record::Warmup
                } else {
                    Record::Measured
                };
                assert_eq!(
                    sample.record, want,
                    "Warmup-then-Measured at scenario {i} sample {j}",
                );
                assert_eq!(
                    sample.cache,
                    CacheLabel::SourceWarmProcessCold,
                    "cache label at scenario {i} sample {j}",
                );
                assert!(!sample.skipped, "not skipped at scenario {i} sample {j}");
                assert_eq!(sample.exit, 0, "exit 0 at scenario {i} sample {j}");
                assert!(sample.wall_ms.is_some(), "complete wall at {i}/{j}");
                assert!(sample.rss_kb.is_some(), "complete rss at {i}/{j}");
                assert!(sample.output_bytes.is_some(), "complete output at {i}/{j}");
            }
            let wall = scen.statistics.wall.as_ref().expect("wall statistics");
            let rss = scen.statistics.rss.as_ref().expect("rss statistics");
            assert_eq!(
                wall.count, expected_measured[i],
                "wall stats count measured-only at {i}",
            );
            assert_eq!(
                rss.count, expected_measured[i],
                "rss stats count measured-only at {i}",
            );
        }

        // (4) Forwarded-spec contract: program is the exact fixture path; every
        // flavor is the host time flavor; spec0 is exactly VERSION_ARGV with no
        // --offline; spec1 is exactly prefetch_argv with no --offline (online);
        // the remaining 22 eval specs each carry exactly one --offline; every
        // env is exactly the same five-entry fail-closed set (no PATH/NIX_PATH)
        // with the SAME HOME across all calls.
        let version_argv: Vec<OsString> =
            VERSION_ARGV.iter().copied().map(OsString::from).collect();
        let prefetch_argv_expected = prefetch_argv(manifest);
        let baseline_env = captured_specs[0].env.clone();
        let baseline_home = baseline_env
            .get(OsStr::new("HOME"))
            .expect("HOME present")
            .clone();
        for (i, spec) in captured_specs.iter().enumerate() {
            assert_eq!(spec.program, nix_bin, "exact fixture program at call {i}");
            assert_eq!(captured_flavors[i], flavor, "host time flavor at call {i}");
            assert_eq!(spec.env.len(), 5, "exactly five env entries at call {i}");
            assert!(
                !spec.env.contains_key(OsStr::new("PATH")),
                "no PATH at call {i}"
            );
            assert!(
                !spec.env.contains_key(OsStr::new("NIX_PATH")),
                "no NIX_PATH at call {i}",
            );
            assert_eq!(spec.env, baseline_env, "same env set at call {i}");
            assert_eq!(
                spec.env.get(OsStr::new("HOME")).unwrap(),
                &baseline_home,
                "same HOME across calls at call {i}",
            );
            match i {
                0 => {
                    assert_eq!(spec.args, version_argv, "spec0 is exactly VERSION_ARGV");
                    assert_eq!(
                        count_offline(&spec.args),
                        0,
                        "no --offline on version probe"
                    );
                }
                1 => {
                    assert_eq!(
                        spec.args, prefetch_argv_expected,
                        "spec1 is exactly prefetch_argv",
                    );
                    assert_eq!(
                        count_offline(&spec.args),
                        0,
                        "no --offline on prefetch (online)",
                    );
                }
                _ => {
                    assert_eq!(
                        count_offline(&spec.args),
                        1,
                        "exactly one --offline on eval at call {i}",
                    );
                }
            }
        }

        // (5) Eval argv order: six single_eval_argv for descriptor[0]'s checked
        // host system (calls 2..=7), then four index_eval_argv for each
        // canonical index descriptor1..4 (calls 8..=23).
        for (i, spec) in captured_specs.iter().enumerate().take(8).skip(2) {
            assert_eq!(
                spec.args, single_argv,
                "single_eval_argv for descriptor0 at call {i}",
            );
        }
        let mut idx = 8_usize;
        for (k, argv) in index_argv.iter().enumerate() {
            let desc_no = k + 1;
            for j in 0..4 {
                let call = idx + j;
                assert_eq!(
                    captured_specs[call].args, *argv,
                    "index_eval_argv for descriptor{desc_no} at call {call} (j={j})",
                );
            }
            idx += 4;
        }
        assert_eq!(idx, 24, "all 24 calls accounted for");
    }

    // === run_real_with_executor (scenario failure continuation) ===========
    //
    // Drives the private generic core through ONE mid-run scenario failure
    // — a nonzero eval exit in the FIRST scenario's SECOND measured attempt
    // (call 4) — with NO real spawning, and proves the four honest
    // continuation guarantees: (a) the failed sample is NEVER fabricated
    // into an observation; (b) the honest successful prefix (1 warmup + 1
    // measured) IS retained under PartialAllowed; (c) the per-scenario
    // EvalOutcome failure CONTINUES to later descriptors (it is NOT global),
    // so the remaining four index-meta scenarios complete fully; and (d) the
    // later index argv runs in canonical order AFTER the failure. Setup uses
    // an ABSOLUTE fixture nix path (never executed), Instant::now(), and an
    // injected fake executor ONLY.
    #[test]
    fn run_real_with_executor_scenario_failure_continues_and_keeps_honest_partial_data() {
        let manifest = benchmark_manifest();
        // Canonical plan: descriptor[0] is the host single-attr scenario;
        // descriptors[1..=4] are the index-meta scenarios, one per system.
        let descriptors = descriptors(manifest).expect("canonical descriptors");
        assert_eq!(descriptors.len(), 5, "1 single-attr + 4 index-meta");
        let checked_single =
            check_system(manifest, &descriptors[0].system).expect("host system checked");
        let single_argv = single_eval_argv(manifest, &checked_single);
        let index_argv: Vec<Vec<OsString>> = descriptors[1..]
            .iter()
            .map(|d| {
                index_eval_argv(
                    manifest,
                    &check_system(manifest, &d.system).expect("checked"),
                )
            })
            .collect();

        // Absolute fixture nix path (an absent child of a private home, never
        // executed), Instant::now(), and an injected fake executor ONLY.
        let (_home, nix_bin, started) = runner_setup_inputs();
        let flavor = host_time_flavor();
        let version_stdout: &[u8] = b"nix (Nix) 2.34.8\n";
        let prefetch_stdout = prefetch_bytes(&valid_prefetch_json());

        let mut calls: u32 = 0;
        let mut captured_specs: Vec<CommandSpec> = Vec::new();
        let mut captured_flavors: Vec<TimeFlavor> = Vec::new();
        let mut executor = |spec: &CommandSpec, flav: TimeFlavor| {
            let n = calls;
            calls += 1;
            if n >= 21 {
                panic!("executor called beyond 21 calls (call {n})");
            }
            captured_specs.push(spec.clone());
            captured_flavors.push(flav);
            Ok(match n {
                // Setup phase: exact-version probe (call 0) + verified prefetch
                // (call 1).
                0 => version_probe_outcome(UnixStatus::Exited(0), version_stdout),
                1 => CommandOutcome {
                    status: UnixStatus::Exited(0),
                    stdout: prefetch_stdout.clone(),
                    cleaned_stderr: String::new(),
                    stdout_total_bytes: prefetch_stdout.len() as u64,
                    stderr_total_bytes: 0,
                    wall_ms: 1234,
                    max_rss_kib: 4096,
                },
                // First scenario (descriptor[0], single-attr): warmup (call 2)
                // and the first measured (call 3) succeed; the SECOND measured
                // attempt (call 4) is a normal nonzero-exit outcome carrying
                // COMPLETE metrics/totals — the attempted sample that must NOT
                // be fabricated into an observation.
                2 | 3 => deterministic_eval_outcome(n),
                4 => {
                    let mut failed = deterministic_eval_outcome(4);
                    failed.status = UnixStatus::Exited(17);
                    failed
                }
                // Later scenarios (descriptor[1..=4], index-meta) all succeed,
                // letting the canonical scenarios 1..=4 complete fully.
                _ => deterministic_eval_outcome(n),
            })
        };

        let report = run_real_with_executor(&nix_bin, started, &mut executor)
            .expect("scenario failure yields an Incomplete report, not RealRunError");

        // (1) Exactly 21 calls: 2 setup + 3 first-scenario attempts (1 warmup
        //     + 2 measured, the second measured failing) + 16 later successful
        //     index-meta evals (4 scenarios x 4 iterations).
        assert_eq!(calls, 21, "exactly 21 calls");
        assert_eq!(captured_specs.len(), 21);
        assert_eq!(captured_flavors.len(), 21);

        // (2) A validated, Incomplete, non-harness-only Real report carrying
        //     the EXACT detected nix_version and exactly five canonical
        //     scenarios retained in order.
        report
            .validate()
            .expect("Incomplete scenario-failure report validates");
        assert_eq!(report.mode, Mode::Real, "mode is Real");
        assert_eq!(
            report.completeness,
            Completeness::Incomplete,
            "scenario failure is Incomplete",
        );
        assert!(!report.harness_only, "Real run is not harness-only");
        assert_eq!(
            report.nix_version.as_deref(),
            Some("2.34.8"),
            "detected version preserved past the version phase",
        );
        assert_eq!(
            report.scenarios.len(),
            5,
            "exactly five canonical scenarios retained",
        );
        for (i, scen) in report.scenarios.iter().enumerate() {
            assert_eq!(
                scen.name, descriptors[i].name,
                "scenarios retained in canonical order at {i}",
            );
        }

        // (3) Exactly ONE failure, scenario-scoped (the FIRST descriptor only,
        //     NOT global "run"), at the EvalOutcome stage/message.
        assert_eq!(report.failures.len(), 1, "exactly one failure");
        let failure = &report.failures[0];
        assert_eq!(
            failure.scenario, descriptors[0].name,
            "failure names only the first descriptor (not global run)",
        );
        assert_ne!(failure.scenario, RUN_SCENARIO, "failure is NOT global");
        assert_eq!(
            failure.stage,
            RealFailureKind::EvalOutcome.stage(),
            "exact failure stage",
        );
        assert_eq!(
            failure.message,
            RealFailureKind::EvalOutcome.message(),
            "exact failure message",
        );

        // (4) First scenario retains the honest successful prefix ONLY:
        //     exactly two samples, global indices 0 (Warmup) then 1 (Measured),
        //     measured statistics count one, and NO sample representing the
        //     failed call 4 (every sample exits 0).
        let first = &report.scenarios[0];
        assert_eq!(
            first.samples.len(),
            2,
            "first scenario keeps exactly the two-prefix samples",
        );
        assert_eq!(first.samples[0].index, 0, "first scenario sample 0 index");
        assert_eq!(first.samples[1].index, 1, "first scenario sample 1 index");
        assert_eq!(first.samples[0].record, Record::Warmup, "prefix[0] Warmup");
        assert_eq!(
            first.samples[1].record,
            Record::Measured,
            "prefix[1] Measured"
        );
        for (j, sample) in first.samples.iter().enumerate() {
            assert_eq!(sample.exit, 0, "no fabricated failed sample at first/{j}");
            assert!(!sample.skipped, "not skipped at first/{j}");
            assert_eq!(
                sample.cache,
                CacheLabel::SourceWarmProcessCold,
                "honest cache at first/{j}",
            );
        }
        let first_wall = first.statistics.wall.as_ref().expect("wall stats");
        let first_rss = first.statistics.rss.as_ref().expect("rss stats");
        assert_eq!(
            first_wall.count, 1,
            "first scenario stats count one measured"
        );
        assert_eq!(first_rss.count, 1, "first scenario rss stats count one");

        // (5) Remaining four scenarios each completed fully: four samples
        //     (warmup then three measured), measured statistics count three,
        //     no skips, all exit 0.
        for (i, scen) in report.scenarios[1..].iter().enumerate() {
            let s = i + 1;
            assert_eq!(scen.samples.len(), 4, "scenario {s} has all four samples");
            assert_eq!(
                scen.samples[0].record,
                Record::Warmup,
                "scenario {s} sample 0 is Warmup",
            );
            assert_eq!(
                scen.samples[1..]
                    .iter()
                    .filter(|x| x.record == Record::Measured)
                    .count(),
                3,
                "scenario {s} has three Measured samples",
            );
            for (j, sample) in scen.samples.iter().enumerate() {
                assert!(!sample.skipped, "no skips at scenario {s} sample {j}");
                assert_eq!(sample.exit, 0, "exit 0 at scenario {s} sample {j}");
            }
            let wall = scen.statistics.wall.as_ref().expect("wall stats");
            let rss = scen.statistics.rss.as_ref().expect("rss stats");
            assert_eq!(wall.count, 3, "scenario {s} stats count three measured");
            assert_eq!(rss.count, 3, "scenario {s} rss stats count three");
        }

        // (6) Forwarded-spec contract: setup specs (calls 0, 1) are exactly
        //     the version / prefetch argv; the first-scenario eval specs
        //     (calls 2..=4, INCLUDING the failed attempt at call 4) are exactly
        //     single_eval_argv; and the later index specs (calls 5..=20) are
        //     index_eval_argv for descriptors[1..=4] in canonical order —
        //     proving the index argv ran in order AFTER the failure. Every
        //     forwarded flavor is the host time flavor and every eval carries
        //     exactly one --offline.
        let prefetch_argv_expected = prefetch_argv(manifest);
        assert_eq!(
            captured_specs[0].args,
            version_argv_owned(),
            "call 0 version"
        );
        assert_eq!(
            captured_specs[1].args, prefetch_argv_expected,
            "call 1 prefetch"
        );
        for (c, spec) in captured_specs.iter().enumerate().take(5).skip(2) {
            assert_eq!(
                spec.args, single_argv,
                "first-scenario single_eval_argv at call {c} (includes failed call 4)",
            );
            assert_eq!(
                count_offline(&spec.args),
                1,
                "exactly one --offline at call {c}",
            );
        }
        // Later index argv in canonical order: calls 5..=20, four per
        // descriptor[1..=4].
        let mut idx = 5_usize;
        for (k, argv) in index_argv.iter().enumerate() {
            let desc_no = k + 1;
            for j in 0..4 {
                let call = idx + j;
                assert_eq!(
                    captured_specs[call].args, *argv,
                    "index_eval_argv for descriptor{desc_no} at call {call} (j={j})",
                );
                assert_eq!(
                    count_offline(&captured_specs[call].args),
                    1,
                    "exactly one --offline at call {call}",
                );
            }
            idx += 4;
        }
        assert_eq!(idx, 21, "all 21 calls accounted for");
        for (i, flav) in captured_flavors.iter().enumerate() {
            assert_eq!(*flav, flavor, "host time flavor at call {i}");
        }
    }
}
