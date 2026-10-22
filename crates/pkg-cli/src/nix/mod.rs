//! Run native Nix commands and decode their output.
//!
//! Callers request profile operations and receive decoded results.
//!
//! Output formats come from Determinate 3.22.1 and Nix 2.35.2 on aarch64-darwin.
//! Decoding checks compatibility without a minimum runtime version check.
//!
//! `error` defines failures, `manifest` decodes output, and `reference`
//! classifies references. `pkg-process` forwards signals and reaps children.

mod conflict;
mod discovery;
mod error;
mod host;
mod index;
mod manifest;
use pkg_process as process;
mod reference;
mod report;
mod runtime;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use conflict::ProfileConflict;
pub use discovery::{MetadataNode, MetadataValue};
pub use error::NixError;
pub use host::{HostVersionError, in_range, macos_product_version};
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
/// Decoding checks the runtime's output format. An incompatible format
/// produces a decode error.
pub const TESTED_BASELINE: &str = "2.35.2";

/// The resolved native runtime.
#[derive(Debug, Clone)]
pub struct Nix {
    executable: PathBuf,
    configured: Option<String>,
    verbose: bool,
    no_color: bool,
}

/// A decoded profile bound to the runtime and path used for its preflight.
#[derive(Debug)]
pub(crate) struct CheckedProfile<'a> {
    nix: &'a Nix,
    profile: &'a Path,
    entries: BTreeMap<String, ProfileEntry>,
}

impl CheckedProfile<'_> {
    pub(crate) const fn entries(&self) -> &BTreeMap<String, ProfileEntry> {
        &self.entries
    }

    pub(crate) fn into_entries(self) -> BTreeMap<String, ProfileEntry> {
        self.entries
    }

    pub(crate) fn add(&self, installables: &[String]) -> Result<(), NixError> {
        self.nix
            .add_to_checked_profile(self.profile, installables, &[])
    }

    pub(crate) fn rollback(&self, generation: Option<u64>) -> Result<(), NixError> {
        self.nix.rollback_checked_profile(self.profile, generation)
    }
}

impl Nix {
    /// The absolute executable path.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Probe the version for runtime diagnostics.
    /// # Errors
    ///
    /// Returns an error if the version command cannot start, complete, or be reaped.
    pub fn version(&self) -> Result<String, NixError> {
        self.probe().map_err(|error| self.configured_failure(error))
    }

    fn probe(&self) -> Result<String, NixError> {
        let args = ["--version"];
        let reaped = run_child(self.command(&args), IoMode::Capture, None, None)
            .map_err(NixError::Process)?;
        Self::classify(&args, &reaped)?;
        Ok(String::from_utf8_lossy(&reaped.output.stdout)
            .trim()
            .to_string())
    }

    /// A configured runtime that cannot answer `--version` is unusable;
    /// report it against the configured value, not as an anonymous failure.
    fn configured_failure(&self, error: NixError) -> NixError {
        match &self.configured {
            Some(requested) if error.signal().is_none() => NixError::ConfiguredProbe {
                requested: requested.clone(),
                source: Box::new(error),
            },
            _ => error,
        }
    }

    /// Probe a configured runtime only after a failed operation, so
    /// successful commands start no extra child.
    fn diagnose(&self, error: NixError) -> NixError {
        if self.configured.is_none()
            || !matches!(error, NixError::Failed { .. } | NixError::Process(_))
        {
            return error;
        }
        match self.probe() {
            Ok(_) => error,
            Err(probe) => self.configured_failure(probe),
        }
    }

    /// Trace every native command line to stderr before it runs.
    ///
    /// This covers every native invocation for `--verbose` output.
    #[must_use]
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Whether native command lines are traced for `--verbose` output.
    #[must_use]
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }

    /// Ask native children not to color their output (`NO_COLOR=1`).
    #[must_use]
    pub fn no_color(mut self, no_color: bool) -> Self {
        self.no_color = no_color;
        self
    }

    /// The complete argument vector for one native invocation.
    fn argv<'a>(extra_args: &[&'a str]) -> Vec<&'a str> {
        [
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
        .chain(extra_args.iter().copied())
        .collect()
    }

    /// Classify one reaped run into a [`NixError`], attaching the argument
    /// vector pkg ran so failures repeat the exact native command.
    /// Streamed children report no captured stderr (their streams went to
    /// the user's terminal); captured children keep theirs.
    fn classify(args: &[&str], reaped: &Reaped) -> Result<(), NixError> {
        if reaped.cancelled.is_none()
            && let Some(limit) = reaped.resource_limit
        {
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
        self.finish(args, &reaped)?;
        Ok(String::from_utf8_lossy(&reaped.output.stdout).into_owned())
    }

    /// Run a native command with filtered progress output on stderr.
    ///
    /// Fetch, clone, and evaluation chatter is filtered. Warnings and
    /// interactive prompts stay visible. Raw stderr stays captured for
    /// the command's failure report. The verbose flag keeps the
    /// unfiltered stream.
    fn run_filtered(&self, profile: &str, args: &[&str]) -> Result<(), NixError> {
        let mut reporter = report::MutationReporter::new(profile, self.verbose);
        let reaped = self.execute(args, IoMode::Filtered(&mut reporter))?;
        // Keep raw stderr for the command's compact cause selection. It is
        // never replayed as a block in normal output.
        self.finish(args, &reaped)
    }

    fn command(&self, args: &[&str]) -> Command {
        // `--verbose` describes every native call here, captures included,
        // with the exact argument vector that will run.
        let command_line = Self::argv(args);
        if self.verbose {
            let words: Vec<String> = command_line.iter().map(|arg| trace_word(arg)).collect();
            eprintln!(
                "{}",
                crate::output::paint(
                    crate::output::Stream::Err,
                    crate::output::Tone::Muted,
                    &format!("$ nix {}", words.join(" "))
                )
            );
        }
        let mut command = Command::new(&self.executable);
        command.args(&command_line);
        if self.no_color {
            // Any non-empty NO_COLOR value means "no color"; set it only when
            // disabling so colors are never suppressed by accident.
            command.env("NO_COLOR", "1");
        }
        command
    }

    /// Run one native child with its selected stdio and reporting mode.
    fn execute(&self, args: &[&str], mode: IoMode<'_>) -> Result<Reaped, NixError> {
        run_child(self.command(args), mode, None, None)
            .map_err(|error| self.diagnose(NixError::Process(error)))
    }

    fn finish(&self, args: &[&str], reaped: &Reaped) -> Result<(), NixError> {
        Self::classify(args, reaped).map_err(|error| self.diagnose(error))
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
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, Nix fails or is interrupted, or the JSON or profile manifest is invalid.
    pub fn profile_list(&self, profile: &Path) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
        let profile = profile.to_string_lossy();
        let raw = self.json::<serde_json::Value>(
            "profile identity",
            &["profile", "list", "--profile", &profile, "--json"],
        )?;
        manifest::decode_profile_manifest(&raw)
    }

    pub(crate) fn checked_profile<'a>(
        &'a self,
        profile: &'a Path,
    ) -> Result<CheckedProfile<'a>, NixError> {
        let entries = self.profile_list(profile)?;
        Ok(CheckedProfile {
            nix: self,
            profile,
            entries,
        })
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
    /// Returns an error if the profile manifest cannot be read or decoded, command setup or spawn fails, stderr reading or child reaping fails, or Nix fails or is interrupted.
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
    /// Returns an error if the profile manifest cannot be read or decoded, command setup or spawn fails, stderr reading or child reaping fails, or Nix fails or is interrupted.
    pub fn profile_add_with_inputs(
        &self,
        profile: &Path,
        installables: &[String],
        inputs: &[(&str, &str)],
    ) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        self.add_to_checked_profile(profile, installables, inputs)
    }

    fn add_to_checked_profile(
        &self,
        profile: &Path,
        installables: &[String],
        inputs: &[(&str, &str)],
    ) -> Result<(), NixError> {
        let profile = profile.to_string_lossy();
        let mut args: Vec<&str> = vec!["profile", "add", "--profile", &profile];
        for &(input, reference) in inputs {
            args.extend(["--override-input", input, reference]);
        }
        // `--` keeps installables from being read as flags.
        args.push("--");
        args.extend(installables.iter().map(String::as_str));
        self.run_filtered(&profile, &args)
    }

    /// Remove exact entries in one native operation.
    ///
    /// Entry names are the literal native element IDs; they are passed after
    /// `--` so IDs that begin with `-` are never read as flags. Regex
    /// removal is deliberately not offered.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, stderr reading or child reaping fails, or Nix fails or is interrupted.
    pub fn profile_remove(&self, profile: &Path, entries: &[String]) -> Result<(), NixError> {
        let profile = profile.to_string_lossy();
        let mut args: Vec<&str> = vec!["profile", "remove", "--profile", &profile];
        if !entries.is_empty() {
            args.push("--");
            args.extend(entries.iter().map(String::as_str));
        }
        self.run_filtered(&profile, &args)
    }

    /// Upgrade exact entries, or all entries when `entries` is `None`.
    ///
    /// Entry names are the literal native element IDs and are passed after
    /// `--` so IDs that begin with `-` are never read as flags.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, stderr reading or child reaping fails, or Nix fails or is interrupted.
    pub fn profile_upgrade(
        &self,
        profile: &Path,
        entries: Option<&[String]>,
    ) -> Result<(), NixError> {
        let profile = profile.to_string_lossy();
        let mut args: Vec<&str> = vec!["profile", "upgrade", "--profile", &profile];
        match entries {
            Some(list) if !list.is_empty() => {
                args.push("--");
                args.extend(list.iter().map(String::as_str));
            }
            _ => args.push("--all"),
        }
        self.run_filtered(&profile, &args)
    }

    /// Switch a profile to a previously built and verified store profile.
    ///
    /// Native Nix creates a new generation and switches it atomically. The
    /// previous generation remains available if helper setup later fails.
    /// `--no-link` prevents an unrelated `result` link in the working directory.
    /// # Errors
    ///
    /// Returns an error if the profile manifest cannot be read or decoded, command setup or spawn fails, stderr reading or child reaping fails, or Nix fails or is interrupted.
    pub fn profile_replace(&self, profile: &Path, replacement: &Path) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy();
        let replacement = replacement.to_string_lossy();
        self.run_filtered(
            &profile,
            &[
                "build",
                "--no-link",
                "--profile",
                &profile,
                "--",
                &replacement,
            ],
        )
    }

    /// Roll back to the previous or a selected native generation.
    ///
    /// The manifest shape is checked read-only first.
    /// # Errors
    ///
    /// Returns an error if the profile manifest cannot be read or decoded, command setup or spawn fails, child reaping fails, or Nix fails or is interrupted.
    pub fn profile_rollback(
        &self,
        profile: &Path,
        generation: Option<u64>,
    ) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        self.rollback_checked_profile(profile, generation)
    }

    fn rollback_checked_profile(
        &self,
        profile: &Path,
        generation: Option<u64>,
    ) -> Result<(), NixError> {
        let profile = profile.to_string_lossy();
        let generation = generation.map(|n| n.to_string());
        let mut args: Vec<&str> = vec!["profile", "rollback", "--profile", &profile];
        if let Some(number) = generation.as_deref() {
            args.extend(["--to", number]);
        }
        self.run_filtered(&profile, &args)
    }

    /// Delete non-current generations older than an explicit age.
    ///
    /// Eligibility stays with Nix: which generations disappear is native
    /// behavior and is not assumed by pkg. The manifest shape is checked
    /// read-only first.
    /// # Errors
    ///
    /// Returns an error if the profile manifest cannot be read or decoded, command setup or spawn fails, child reaping fails, or Nix fails or is interrupted.
    pub fn profile_wipe_history(&self, profile: &Path, age: &str) -> Result<(), NixError> {
        self.require_decodable_profile(profile)?;
        let profile = profile.to_string_lossy();
        self.run_filtered(
            &profile,
            &[
                "profile",
                "wipe-history",
                "--profile",
                &profile,
                "--older-than",
                age,
            ],
        )
    }

    /// Print native profile generation history.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, or Nix fails or is interrupted.
    pub fn profile_history(&self, profile: &Path) -> Result<String, NixError> {
        let profile = profile.to_string_lossy();
        self.capture(&["profile", "history", "--profile", &profile])
    }

    /// Run a native search and return evaluated metadata.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, Nix fails or is interrupted, evaluation exceeds its limits, or result JSON is invalid.
    pub fn search(
        &self,
        reference: &str,
        pattern: &str,
    ) -> Result<BTreeMap<String, SearchMeta>, NixError> {
        // `--` keeps the reference and the pattern from being read as flags.
        let args = ["search", "--json", "--", reference, pattern];
        let results: BTreeMap<String, SearchMeta> =
            match self.discovery_json("search results", &args, "20") {
                Err(NixError::ResourceLimit(_)) => {
                    self.discovery_json("search results", &args, "100")
                }
                result => result,
            }?;
        Ok(results
            .into_iter()
            .map(|(attribute, meta)| (manifest::normalize_catalog_attribute(attribute), meta))
            .collect())
    }

    /// Evaluate the default installation outputs without realizing them.
    /// # Errors
    ///
    /// Returns an error if output selection is invalid, command setup or spawn fails, output reading or child reaping fails, Nix fails or is interrupted, or the returned JSON is invalid.
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
            &["eval", "--json", "--apply", &apply, "--", installable],
        )
    }

    /// Resolve the current system triple.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, or the config command fails or is interrupted.
    pub fn system(&self) -> Result<String, NixError> {
        Ok(self
            .capture(&["config", "show", "system"])?
            .trim()
            .to_string())
    }
    /// Read effective client settings without printing the config document.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, the config command fails or is interrupted, or its JSON is invalid.
    pub fn effective_config(&self) -> Result<serde_json::Value, NixError> {
        self.json("effective config", &["config", "show", "--json"])
    }

    /// Resolve source identity for a flake reference.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, the metadata command fails or is interrupted, or its JSON is invalid.
    pub fn source_identity(&self, reference: &str) -> Result<SourceIdentity, NixError> {
        self.source_identity_with(reference, false)
    }

    /// Resolve source identity, bypassing cached flake metadata.
    ///
    /// Passes the native `--refresh` option so `pkg update` refreshes
    /// metadata instead of reusing Nix's TTL cache and calling it current.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, the metadata command fails or is interrupted, or its JSON is invalid.
    pub fn refresh_source_identity(&self, reference: &str) -> Result<SourceIdentity, NixError> {
        self.source_identity_with(reference, true)
    }

    fn source_identity_with(
        &self,
        reference: &str,
        refresh: bool,
    ) -> Result<SourceIdentity, NixError> {
        let mut args = vec!["flake", "metadata", "--json"];
        if refresh {
            args.push("--refresh");
        }
        args.extend(["--", reference]);
        let reaped = self.execute(&args, IoMode::Capture)?;
        self.finish(&args, &reaped)?;
        // Offline, Nix warns `unable to download …; using cached version`
        // and succeeds. A refresh that checked nothing must not say current.
        if refresh
            && let Some(cause) = offline_cause(&String::from_utf8_lossy(&reaped.output.stderr))
        {
            return Err(NixError::Offline { cause });
        }
        let value: serde_json::Value =
            serde_json::from_slice(&reaped.output.stdout).map_err(|error| NixError::Json {
                what: "source identity",
                source: error,
            })?;
        Ok(manifest::decode_source_identity(reference, &value))
    }

    /// Probe the local Nix store service (`nix store info`).
    ///
    /// `nix store ping` is a deprecated alias for `info`; the supported
    /// command is used deliberately. Output text is returned for reporting.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, or the store-info command fails or is interrupted.
    pub fn store_info(&self) -> Result<String, NixError> {
        self.capture(&["store", "info"])
    }

    /// Read generated Formula data without evaluating package derivations.
    /// # Errors
    ///
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, the evaluation fails or is interrupted, or its JSON is invalid.
    pub fn formula_index(&self, reference: &str) -> Result<serde_json::Value, NixError> {
        let installable = format!("{reference}#formulaIndex");
        self.json(
            "formula catalog index",
            &["eval", "--json", "--", &installable],
        )
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
    /// Returns an error if command setup or spawn fails, output reading or child reaping fails, the evaluation fails or is interrupted, or the JSON envelope or catalog fields are invalid.
    pub fn catalog_index(&self, reference: &str) -> Result<CatalogIndex, NixError> {
        let installable = format!("{reference}#catalogIndex");
        let value = self.json::<serde_json::Value>(
            "cask catalog index",
            &["eval", "--json", "--", &installable],
        )?;
        decode_catalog_index(&value)
    }
}

/// One argument of a `--verbose` trace, quoted so the line can run in a shell.
fn trace_word(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,#^%".contains(c))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// The warning that says Nix used its saved copy of a source because the
/// download failed. Other warnings, such as a dirty Git tree, can come first.
fn offline_cause(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.contains("using cached version"))?;
    Some(line.strip_prefix("warning: ").unwrap_or(line).to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_offline_cause_is_the_download_warning() {
        let stderr = "warning: Git tree '/src' is dirty\nwarning: unable to download 'https://x/y': Couldn't resolve host name (6); using cached version\n";
        assert_eq!(
            super::offline_cause(stderr).as_deref(),
            Some(
                "unable to download 'https://x/y': Couldn't resolve host name (6); using cached version"
            )
        );
        assert_eq!(
            super::offline_cause("warning: Git tree '/src' is dirty\n"),
            None
        );
    }
}
