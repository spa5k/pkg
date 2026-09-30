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
//!
//! Layout: `error` holds the failure modes, `process` the shared signal
//! forward/reap boundary for every external child, `manifest` the decoded
//! native output shapes, and the `reference` module the pure reference
//! rules.

mod error;
mod manifest;
mod process;
mod reference;
mod report;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use error::NixError;
pub use manifest::OFFICIAL_CASK_SOURCE;
pub use manifest::{
    CATALOG_INDEX_SCHEMA, CatalogEntry, CatalogGenerator, CatalogIndex, CatalogInput,
    CatalogSystem, EntryStatus, ProfileEntry, RawInputProvenance, SearchMeta, SourceIdentity,
    decode_catalog_index, full_entry_id, split_entry_id, valid_catalog_source, valid_catalog_token,
};
use process::{IoMode, Reaped, run_child};
pub use process::{Outcome, run_direct, run_direct_captured};
pub use reference::{
    decode_path_reference, encode_path_reference, is_fixed_reference, is_full_commit_id,
};

/// The only natively verified runtime baseline.
///
/// pkg does not enforce an invented minimum version. Compatibility is
/// enforced by decoding the exact native output shapes below; a runtime that
/// speaks a different shape fails with a decode diagnostic, not a guess.
pub const TESTED_BASELINE: &str = "2.35.2";

/// The resolved native runtime.
#[derive(Debug, Clone)]
pub struct Nix {
    executable: PathBuf,
    version: String,
    verbose: bool,
    no_color: bool,
}

/// The stable system-wide Nix profile executable used by the Determinate
/// Nix installer and the multi-user Nix installer. Checked first.
const SYSTEM_PROFILE_NIX: &str = "/nix/var/nix/profiles/default/bin/nix";

/// The modern per-user Nix profile used by current Nix releases
/// (the same location the shipped `nix-daemon.sh` profile script uses).
const STATE_PROFILE_NIX: &str = "nix/profile/bin/nix";
const STATE_BASE: &str = ".local/state";

/// The legacy per-user profile of older single-user Nix installations.
const LEGACY_PROFILE_NIX: &str = ".nix-profile/bin/nix";

/// Whether `path` is a regular file with an executable bit.
///
/// `metadata` follows symlinks, so a link to an executable counts and a
/// dangling or directory candidate does not.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// An environment value usable as an absolute search base. Empty and
/// relative values never become a search base: a relative base would
/// silently depend on the current directory.
fn absolute_base(value: Option<&std::ffi::OsStr>) -> Option<&Path> {
    let base = Path::new(value?);
    base.is_absolute().then_some(base)
}

/// The stable Nix installation profile candidates for one environment,
/// in check order: the system profile, the modern per-user state profile
/// (absolute `XDG_STATE_HOME` first, else `$HOME/.local/state`), then the
/// legacy `$HOME/.nix-profile`. Only these fixed locations are checked;
/// the disk is never scanned.
fn standard_candidates(
    state: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut standard = vec![PathBuf::from(SYSTEM_PROFILE_NIX)];
    let state_base = absolute_base(state)
        .map(Path::to_path_buf)
        .or_else(|| absolute_base(home).map(|home| home.join(STATE_BASE)));
    if let Some(state_base) = state_base {
        standard.push(state_base.join(STATE_PROFILE_NIX));
    }
    if let Some(home) = absolute_base(home) {
        standard.push(home.join(LEGACY_PROFILE_NIX));
    }
    standard
}

/// Resolve the runtime executable from pure search inputs (design D1, D2).
///
/// A configured value is the only candidate and never falls back. Default
/// discovery takes the first executable-file candidate from the `PATH`
/// entries, then the stable profile paths. The inputs are parameters so the
/// selection rules are testable without touching the process environment.
fn resolve(
    configured: Option<&str>,
    path: Option<&std::ffi::OsStr>,
    standard: &[PathBuf],
) -> Result<PathBuf, NixError> {
    let path_entries: Vec<PathBuf> = path
        .map(std::env::split_paths)
        .map(Iterator::collect)
        .unwrap_or_default();
    if let Some(requested) = configured {
        let candidate = if Path::new(requested).components().count() > 1 {
            Some(PathBuf::from(requested))
        } else {
            path_entries
                .iter()
                .map(|dir| dir.join(requested))
                .find(|candidate| is_executable_file(candidate))
        };
        return candidate.filter(|c| is_executable_file(c)).ok_or_else(|| {
            NixError::ConfiguredUnavailable {
                requested: requested.to_string(),
                detail: if Path::new(requested).components().count() > 1 {
                    String::from("the path holds no executable regular file")
                } else {
                    String::from("no executable regular file with this name is on PATH")
                },
            }
        });
    }
    let from_path = path_entries
        .iter()
        .map(|dir| dir.join("nix"))
        .find(|candidate| is_executable_file(candidate));
    let from_standard = standard
        .iter()
        .find(|candidate| is_executable_file(candidate))
        .cloned();
    from_path
        .or(from_standard)
        .ok_or_else(|| NixError::NotFound {
            path_entries: path_entries.len(),
            standard: standard
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        })
}

/// Discover the external Nix runtime.
///
/// Default discovery searches the executable `PATH` entries, then the
/// stable Nix installation profile paths (system profile, modern per-user
/// state profile, legacy per-user profile). A configured value stays the
/// only candidate and never falls back. Nothing is installed or repaired
/// here, and no invented version floor is enforced: capability is proven
/// by the decoded shapes.
pub fn discover(configured: Option<&str>) -> Result<Nix, NixError> {
    let path = std::env::var_os("PATH");
    let state = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty());
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty());
    let standard = standard_candidates(state.as_deref(), home.as_deref());
    let resolved = resolve(configured, path.as_deref(), &standard)?;
    // The runtime keeps an absolute executable path from here on: a
    // candidate found through a relative PATH entry is pinned against the
    // current directory before any symlink is resolved away.
    let absolute = if resolved.is_absolute() {
        resolved
    } else {
        let cwd = std::env::current_dir().map_err(|error| NixError::Spawn(error.to_string()))?;
        cwd.join(resolved)
    };
    let executable = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let version = Nix {
        executable: executable.clone(),
        version: String::new(),
        verbose: false,
        no_color: false,
    }
    .capture(&["--version"])
    .map(|text| text.trim().to_string())
    .map_err(|error| match configured {
        // A configured runtime that cannot even answer `--version` is
        // unusable; report it against the configured value, not as an
        // anonymous native failure.
        Some(requested) => NixError::ConfiguredUnavailable {
            requested: requested.to_string(),
            detail: format!("the version probe failed: {error}"),
        },
        None => error,
    })?;
    // Keep the full native version text (for example `nix (Nix) 2.35.2`);
    // trimming to major.minor would not match the tested baseline.
    Ok(Nix {
        executable,
        version,
        verbose: false,
        no_color: false,
    })
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

    /// Classify one reaped run into a [`NixError`], attaching the argument
    /// vector pkg ran so failures repeat the exact native command.
    /// Streamed children report no captured stderr (their streams went to
    /// the user's terminal); captured children keep theirs.
    fn finish(&self, args: &[&str], reaped: &Reaped) -> Result<(), NixError> {
        match reaped.classify() {
            Outcome::Success => Ok(()),
            Outcome::Interrupted { signal } => Err(NixError::Interrupted {
                args: args.iter().map(ToString::to_string).collect(),
                signal,
            }),
            Outcome::Failed { status, stderr } => Err(NixError::Failed {
                args: args.iter().map(ToString::to_string).collect(),
                status,
                stderr,
            }),
        }
    }

    fn capture(&self, args: &[&str]) -> Result<String, NixError> {
        // Captured children get the same forwarding and reaping as streamed
        // ones; a signal sent only to pkg still cancels the child.
        let reaped = self.execute(args, IoMode::Capture)?;
        self.finish(args, &reaped)?;
        Ok(String::from_utf8_lossy(&reaped.output.stdout).into_owned())
    }

    /// Run a native command with stderr reduced to progress and signal.
    ///
    /// Fetch, clone, and evaluation chatter is filtered; warnings,
    /// errors, and interactive prompts stay visible, and the raw stderr
    /// stays captured for failure diagnostics. The verbose flag keeps
    /// the unfiltered stream.
    fn run_filtered(&self, args: &[&str]) -> Result<(), NixError> {
        if self.verbose {
            return self.run_streamed(args);
        }
        let mut reporter = report::mutation_reporter();
        let reaped = self.execute_with(args, IoMode::Filtered, Some(&mut reporter))?;
        // Keep raw stderr for the command's compact cause selection. It is
        // never replayed as a block in normal output.
        self.finish(args, &reaped)
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
        self.finish(args, &reaped)
    }

    /// Build and run one native child through the shared forward/reap
    /// boundary.
    /// Run one native child through the shared boundary without a sink.
    fn execute(&self, args: &[&str], mode: IoMode) -> Result<Reaped, NixError> {
        self.execute_with(args, mode, None)
    }

    /// Run one native child with an optional live stderr sink.
    fn execute_with(
        &self,
        args: &[&str],
        mode: IoMode,
        sink: Option<&mut dyn process::StderrSink>,
    ) -> Result<Reaped, NixError> {
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
        run_child(command, mode, sink).map_err(NixError::Spawn)
    }

    fn json<T: serde::de::DeserializeOwned>(
        &self,
        what: &'static str,
        args: &[&str],
    ) -> Result<T, NixError> {
        let captured = self.capture(args)?;
        serde_json::from_str(&captured).map_err(|error| NixError::Decode {
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
        manifest::decode_profile_manifest(&raw)
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
        self.profile_add_with_inputs(profile, installables, &[])
    }

    /// Add installables with explicit, fixed flake input overrides.
    ///
    /// Overrides apply only to this operation and never write a lockfile.
    /// The app helper uses this to select a macOS-compatible Lisp runtime
    /// without changing the user's package sources.
    pub fn profile_add_with_inputs(
        &self,
        profile: &Path,
        installables: &[String],
        inputs: &[(&str, &str)],
    ) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["profile", "add", "--profile", &profile];
        for &(input, reference) in inputs {
            args.extend(["--override-input", input, reference]);
        }
        // `--` keeps installables from being read as flags.
        args.push("--");
        args.extend(installables.iter().map(String::as_str));
        self.run_filtered(&args)
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
        self.run_filtered(&args)
    }

    /// Switch a profile to a previously built and verified store profile.
    ///
    /// Native Nix creates a new generation and switches it atomically. The
    /// previous generation remains available if helper setup later fails.
    /// `--no-link` prevents an unrelated `result` link in the working directory.
    pub fn profile_replace(&self, profile: &Path, replacement: &Path) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy().into_owned();
        let replacement = replacement.to_string_lossy().into_owned();
        self.run_filtered(&[
            "build",
            "--no-link",
            "--profile",
            &profile,
            "--",
            &replacement,
        ])
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
        self.capture(&["profile", "history", "--profile", &profile])
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
            .trim()
            .to_string())
    }
    /// Read one effective client-side setting, such as the sandbox flag.
    pub fn setting(&self, name: &str) -> Result<String, NixError> {
        Ok(self.capture(&["config", "show", name])?.trim().to_string())
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
        Ok(manifest::decode_source_identity(reference, &value))
    }

    /// Probe the local Nix store service (`nix store info`).
    ///
    /// `nix store ping` is a deprecated alias for `info`; the supported
    /// command is used deliberately. Output text is returned for reporting.
    pub fn store_info(&self) -> Result<String, NixError> {
        self.capture(&["store", "info"])
    }

    /// Evaluate and decode the cask catalog index envelope.
    ///
    /// `reference` must be the same source reference used for search rows
    /// over the casks source, so index status and search rows always
    /// describe one locked revision (design D6). The attribute is generated
    /// JSON data: evaluating it forces no package derivations, and it is not
    /// per-system, so a system outside the catalog targets is detected from
    /// the envelope's own target list instead of a failed attribute probe.
    pub fn catalog_index(&self, reference: &str) -> Result<CatalogIndex, NixError> {
        let installable = format!("{reference}#catalogIndex");
        let value = self
            .json::<serde_json::Value>("cask catalog index", &["eval", "--json", &installable])?;
        manifest::decode_catalog_index(&value)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;

    /// Write one executable `/bin/sh` script and return its path together
    /// with the directory that keeps it alive for the test.
    fn fake_runtime(body: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nix");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        make_executable(&path);
        (path, dir)
    }

    /// Write one non-executable regular file.
    fn plain_file(path: &Path) {
        std::fs::write(path, "not executable").expect("write plain file");
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod");
    }

    /// `resolve` default discovery: PATH entries first, then the stable
    /// profile paths in order; non-executable files are skipped everywhere.
    #[test]
    fn resolve_searches_path_then_standard_and_skips_non_executables() {
        let path_dir = tempfile::tempdir().expect("tempdir");
        let modern_dir = tempfile::tempdir().expect("tempdir");
        let legacy_dir = tempfile::tempdir().expect("tempdir");
        let path_nix = path_dir.path().join("nix");
        let modern_nix = modern_dir.path().join("nix");
        let legacy_nix = legacy_dir.path().join("nix");
        let standard = vec![modern_nix.clone(), legacy_nix.clone()];

        // Nothing usable: plain files on PATH and at the standard
        // locations are all ignored, and the error reports the searched
        // locations in order.
        plain_file(&path_nix);
        plain_file(&modern_nix);
        plain_file(&legacy_nix);
        let error = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect_err("no executable candidate");
        let NixError::NotFound {
            path_entries,
            standard: reported,
        } = &error
        else {
            panic!("expected not-found, got: {error}");
        };
        assert_eq!(*path_entries, 1);
        assert_eq!(
            reported,
            &[
                modern_nix.display().to_string(),
                legacy_nix.display().to_string()
            ]
        );

        // The first usable standard candidate in list order wins when
        // PATH holds nothing usable: the modern profile beats the legacy
        // one even though both are executable.
        make_executable(&legacy_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("a standard profile candidate is found");
        assert_eq!(found, legacy_nix);
        make_executable(&modern_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("a standard profile candidate is found");
        assert_eq!(found, modern_nix);

        // A usable PATH candidate wins over the standard paths.
        make_executable(&path_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("path candidate is found");
        assert_eq!(found, path_nix);
    }

    /// A configured value is the only candidate: it wins when usable and
    /// fails alone when not, even with usable candidates elsewhere
    /// (design D2).
    #[test]
    fn resolve_never_falls_back_from_a_configured_value() {
        let (good, _keep) = fake_runtime("exit 0");
        let other_dir = tempfile::tempdir().expect("tempdir");
        let (other, _keep_other) = {
            let path = other_dir.path().join("nix");
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write script");
            make_executable(&path);
            (path, ())
        };

        // Usable configured value: it is used, not the PATH candidate.
        let path_env = other_dir.path().as_os_str();
        let standard = std::slice::from_ref(&other);
        let found = resolve(
            Some(good.to_str().expect("utf8 path")),
            Some(path_env),
            standard,
        )
        .expect("configured candidate is used");
        assert_eq!(found, good);

        // Unusable configured value: the error names it, and the usable
        // PATH and standard candidates are not selected.
        let bad = other_dir.path().join("not-nix");
        plain_file(&bad);
        let error = resolve(
            Some(bad.to_str().expect("utf8 path")),
            Some(path_env),
            standard,
        )
        .expect_err("configured value must fail alone");
        let NixError::ConfiguredUnavailable { requested, .. } = &error else {
            panic!("expected configured-unavailable, got: {error}");
        };
        assert_eq!(requested, &bad.display().to_string());

        // A configured bare name is searched on PATH only, and an
        // executable file at a standard path is not a fallback for it.
        plain_file(&other_dir.path().join("weirdname"));
        let error = resolve(Some("weirdname"), Some(path_env), standard)
            .expect_err("bare configured name must fail when not on PATH");
        assert!(
            matches!(error, NixError::ConfiguredUnavailable { .. }),
            "got: {error}"
        );
    }

    /// The stable candidate list always holds the system profile first,
    /// then the modern per-user state profile, then the legacy per-user
    /// profile; only absolute bases apply.
    #[test]
    fn standard_candidates_follow_the_environment_bases() {
        // No usable bases: only the system profile remains.
        assert_eq!(
            standard_candidates(None, None),
            [PathBuf::from(SYSTEM_PROFILE_NIX)]
        );
        // Relative bases never become search bases.
        let relative = OsString::from("relative/home");
        assert_eq!(
            standard_candidates(Some(relative.as_os_str()), Some(relative.as_os_str())),
            [PathBuf::from(SYSTEM_PROFILE_NIX)]
        );
        // Absolute HOME without XDG_STATE_HOME: the default state base.
        let home = OsString::from("/home/u");
        assert_eq!(
            standard_candidates(None, Some(home.as_os_str())),
            [
                PathBuf::from(SYSTEM_PROFILE_NIX),
                Path::new("/home/u/.local/state").join(STATE_PROFILE_NIX),
                Path::new("/home/u").join(LEGACY_PROFILE_NIX),
            ]
        );
        // Absolute XDG_STATE_HOME wins over the HOME default.
        let state = OsString::from("/var/lib/pkg-state");
        assert_eq!(
            standard_candidates(Some(state.as_os_str()), Some(home.as_os_str())),
            [
                PathBuf::from(SYSTEM_PROFILE_NIX),
                Path::new("/var/lib/pkg-state").join(STATE_PROFILE_NIX),
                Path::new("/home/u").join(LEGACY_PROFILE_NIX),
            ]
        );
    }

    /// Regression: `discover` and every captured operation run a real child
    /// through the shared forward/reap boundary. A capture path that
    /// misclassifies its reaped child fails here instead of failing on every
    /// installed machine.
    #[test]
    fn discover_captures_the_version_from_a_real_child() {
        let (path, _keep) = fake_runtime("echo 'nix (Nix) 2.35.2'");
        let runtime = discover(Some(path.to_str().expect("utf8 path")))
            .expect("discovery captures the child's version");
        assert_eq!(runtime.version(), "nix (Nix) 2.35.2");
        assert_eq!(
            runtime.executable(),
            path.canonicalize().expect("canonical")
        );
    }

    /// Captured success, captured failure with stderr, and streamed success
    /// classify exactly through one real child each.
    #[test]
    fn captured_and_streamed_children_classify_exactly() {
        let profile = tempfile::tempdir().expect("tempdir").keep();
        let profile = profile.join("profile");

        // A captured success returns its stdout.
        let (path, _keep) = fake_runtime("echo 'history text'");
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        assert_eq!(
            runtime.profile_history(&profile).expect("captures stdout"),
            "history text\n"
        );

        // A captured failure keeps the exit status and stderr. The probe
        // still answers `--version`; every other command fails.
        let (path, _keep) = fake_runtime(
            "case \"$*\" in *--version) echo 'nix (Nix) 2.35.2'; exit 0;; \
             *) echo 'native diagnostic' >&2; exit 7;; esac",
        );
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        let error = runtime.profile_history(&profile).expect_err("fails");
        let NixError::Failed {
            status,
            stderr,
            args,
        } = &error
        else {
            panic!("expected a failed native command, got: {error}");
        };
        assert_eq!(status, "exit code 7");
        assert!(stderr.contains("native diagnostic"), "{stderr}");
        assert!(
            args.contains(&String::from("history")),
            "the argument vector is preserved: {args:?}"
        );

        // A streamed child runs to completion with inherited stdio; the
        // marker proves it ran, and the empty buffers still classify.
        let marker = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("streamed-marker");
        let body = format!("printf ran >> {}", marker.display());
        let (path, _keep) = fake_runtime(&body);
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        runtime
            .profile_remove(&profile, &[])
            .expect("streamed child is reaped and classified");
        assert!(marker.is_file(), "the streamed child must have run");
    }

    /// Direct helper commands share the boundary and the classification.
    #[test]
    fn direct_commands_share_the_classification() {
        let (ok, _keep) = fake_runtime("exit 0");
        assert_eq!(
            run_direct(&ok, &[String::from("sync-trampolines")]).expect("runs"),
            Outcome::Success
        );

        let (fail, _keep) = fake_runtime("echo 'helper diagnostic' >&2; exit 3");
        assert_eq!(
            run_direct(&fail, &[]).expect("runs"),
            Outcome::Failed {
                status: String::from("exit code 3"),
                stderr: String::from("helper diagnostic\n"),
            }
        );

        // The captured variant returns the child's stdout next to the
        // outcome; `run_direct` is its discard-stdout wrapper.
        let (capture, _keep) = fake_runtime("echo 'status text'");
        let (outcome, stdout) = run_direct_captured(&capture, &[]).expect("runs and captures");
        assert_eq!(outcome, Outcome::Success);
        assert_eq!(stdout, "status text\n");
    }
}
