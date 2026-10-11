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

mod conflict;
mod discovery;
mod error;
mod host;
mod index;
mod manifest;
mod process;
mod reference;
mod report;
mod runtime;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use conflict::ProfileConflict;
pub use discovery::{MetadataNode, MetadataValue};
pub use error::NixError;
pub use host::{in_range, macos_product_version};
pub use index::OFFICIAL_CASK_SOURCE;
pub use index::{
    CATALOG_INDEX_SCHEMA, CatalogEntry, CatalogGenerator, CatalogIndex, CatalogInput,
    CatalogSystem, EntryStatus, RawInputProvenance, decode_catalog_index, decode_formula_index,
    full_entry_id, split_entry_id, valid_catalog_source, valid_catalog_token,
};
pub use manifest::{ProfileEntry, SearchMeta, SourceIdentity};
use process::{IoMode, Reaped, run_child};
pub use process::{Outcome, run_direct, run_direct_captured};
pub use reference::{
    decode_path_reference, encode_path_reference, is_fixed_reference, is_full_commit_id,
};
pub use runtime::discover;

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
    fn argv(extra_args: &[&str]) -> Vec<String> {
        let mut command_args: Vec<String> = [
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
        command_args.extend(extra_args.iter().map(|arg| (*arg).to_string()));
        command_args
    }

    /// Classify one reaped run into a [`NixError`], attaching the argument
    /// vector pkg ran so failures repeat the exact native command.
    /// Streamed children report no captured stderr (their streams went to
    /// the user's terminal); captured children keep theirs.
    fn finish(args: &[&str], reaped: &Reaped) -> Result<(), NixError> {
        if let Some(signal) = reaped.cancelled {
            return Err(NixError::Interrupted {
                args: args.iter().map(ToString::to_string).collect(),
                signal,
            });
        }
        if let Some(limit) = reaped.resource_limit {
            return Err(NixError::ResourceLimit(limit.to_string()));
        }
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
        Self::finish(args, &reaped)?;
        Ok(String::from_utf8_lossy(&reaped.output.stdout).into_owned())
    }

    /// Run a native command with stderr reduced to progress and signal.
    ///
    /// Fetch, clone, and evaluation chatter is filtered. Warnings and
    /// interactive prompts stay visible. Raw stderr stays captured for
    /// the command's failure report. The verbose flag keeps the
    /// unfiltered stream.
    fn run_filtered(&self, args: &[&str]) -> Result<(), NixError> {
        let profile = args
            .iter()
            .position(|arg| *arg == "--profile")
            .and_then(|index| args.get(index + 1))
            .copied();
        let mut reporter = report::mutation_reporter().scoped(profile, self.verbose);
        let reaped = self.execute_with(args, IoMode::Filtered, Some(&mut reporter))?;
        // Keep raw stderr for the command's compact cause selection. It is
        // never replayed as a block in normal output.
        Self::finish(args, &reaped)
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
        Self::finish(args, &reaped)
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
        let command_line = Self::argv(args);
        if self.verbose {
            eprintln!("pkg: nix {}", command_line.join(" "));
        }
        let mut command = Command::new(&self.executable);
        command.args(&command_line);
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
        serde_json::from_str(&captured).map_err(|error| NixError::Json {
            what,
            source: error,
        })
    }

    /// Read installed elements from the dedicated profile (design D3).
    ///
    /// Keys of the returned map are the literal native entry IDs; pass them
    /// back to [`Nix::profile_remove`] and [`Nix::profile_upgrade`] verbatim.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn profile_add(&self, profile: &Path, installables: &[String]) -> Result<(), NixError> {
        self.profile_add_with_inputs(profile, installables, &[])
    }

    /// Add installables with explicit, fixed flake input overrides.
    ///
    /// Overrides apply only to this operation and never write a lockfile.
    /// The app helper uses this to select a macOS-compatible Lisp runtime
    /// without changing the user's package sources.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn profile_remove(&self, profile: &Path, entries: &[String]) -> Result<(), NixError> {
        let profile = profile.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["profile", "remove", "--profile", &profile];
        if !entries.is_empty() {
            args.push("--");
            args.extend(entries.iter().map(String::as_str));
        }
        self.run_filtered(&args)
    }

    /// Upgrade exact entries, or all entries when `entries` is `None`.
    ///
    /// Entry names are the literal native element IDs and are passed after
    /// `--` so IDs that begin with `-` are never read as flags.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn profile_history(&self, profile: &Path) -> Result<String, NixError> {
        let profile = profile.to_string_lossy().into_owned();
        self.capture(&["profile", "history", "--profile", &profile])
    }

    /// Run a native search and return evaluated metadata.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn search(
        &self,
        reference: &str,
        pattern: &str,
    ) -> Result<BTreeMap<String, SearchMeta>, NixError> {
        let args = ["search", reference, pattern, "--json"];
        match self.discovery_json("search results", &args, "20") {
            Err(NixError::ResourceLimit(_)) => self.discovery_json("search results", &args, "100"),
            result => result,
        }
    }

    /// Evaluate the default installation outputs without realizing them.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn default_output_paths(&self, installable: &str) -> Result<Vec<String>, NixError> {
        let (installable, selection) = installable
            .split_once('^')
            .map_or((installable, None), |(base, outputs)| (base, Some(outputs)));
        let outputs = match selection {
            None => String::from("p.meta.outputsToInstall or [ (p.outputName or \"out\") ]"),
            Some("*") => String::from("p.outputs or [ (p.outputName or \"out\") ]"),
            Some(names) => {
                let values: Vec<_> = names.split(',').collect();
                format!("builtins.fromJSON {}", discovery::nix_json(&values)?)
            }
        };
        let apply = format!("p: map (n: p.${{n}}.outPath) ({outputs})");
        self.json(
            "installable outputs",
            &["eval", "--json", installable, "--apply", &apply],
        )
    }

    /// Resolve the current system triple.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn system(&self) -> Result<String, NixError> {
        Ok(self
            .capture(&["config", "show", "system"])?
            .trim()
            .to_string())
    }
    /// Read one effective client-side setting, such as the sandbox flag.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn setting(&self, name: &str) -> Result<String, NixError> {
        Ok(self.capture(&["config", "show", name])?.trim().to_string())
    }

    /// Read effective client settings without printing the config document.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn effective_config(&self) -> Result<serde_json::Value, NixError> {
        self.json("effective config", &["config", "show", "--json"])
    }

    /// Resolve source identity for a flake reference.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn source_identity(&self, reference: &str) -> Result<SourceIdentity, NixError> {
        self.source_identity_with(reference, false)
    }

    /// Resolve source identity, bypassing cached flake metadata.
    ///
    /// Passes the native `--refresh` option so `pkg update` refreshes
    /// metadata instead of reusing Nix's TTL cache and calling it current.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
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
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn store_info(&self) -> Result<String, NixError> {
        self.capture(&["store", "info"])
    }

    /// Read generated Formula data without evaluating package derivations.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn formula_index(&self, reference: &str) -> Result<serde_json::Value, NixError> {
        let installable = format!("{reference}#formulaIndex");
        self.json("formula catalog index", &["eval", "--json", &installable])
    }

    /// Evaluate and decode the cask catalog index envelope.
    ///
    /// `reference` must be the same source reference used for search rows
    /// over the casks source, so index status and search rows always
    /// describe one locked revision (design D6). The attribute is generated
    /// JSON data: evaluating it forces no package derivations, and it is not
    /// per-system, so a system outside the catalog targets is detected from
    /// the envelope's own target list instead of a failed attribute probe.
    /// # Errors
    ///
    /// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
    pub fn catalog_index(&self, reference: &str) -> Result<CatalogIndex, NixError> {
        let installable = format!("{reference}#catalogIndex");
        let value = self
            .json::<serde_json::Value>("cask catalog index", &["eval", "--json", &installable])?;
        decode_catalog_index(&value)
    }
}
