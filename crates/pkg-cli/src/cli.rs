//! Derive-based definition of the complete V1 command grammar.

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::exit::ExitCode;

/// Top-level `pkg` command-line parser.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "pkg",
    version,
    about = "Install and manage packages without exposing Nix internals",
    long_about = None,
    propagate_version = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Emit one stable final JSON document.
    #[arg(long, global = true, conflicts_with = "jsonl")]
    json: bool,

    /// Emit versioned public progress records as NDJSON.
    #[arg(long, global = true, conflicts_with = "json")]
    jsonl: bool,

    /// Suppress human progress while retaining the final result.
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Include additional sanitized phase detail.
    #[arg(long, global = true, conflicts_with = "quiet")]
    verbose: bool,

    /// Disable ANSI color even on a terminal.
    #[arg(long, global = true)]
    no_color: bool,

    /// Override the product configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override the per-user state root, primarily for tests.
    #[arg(long, global = true, value_name = "DIR")]
    state: Option<PathBuf>,

    /// Select the invoking user's profile (V1 supports only `default`).
    #[arg(long, global = true, default_value = "default", value_parser = ["default"])]
    profile: String,

    /// Accept ordinary confirmations and pre-approve this operation's one build plan.
    #[arg(long, global = true)]
    yes: bool,

    /// Preview and preflight without desired-state or generation mutation.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Parse a fallible argument iterator without terminating the process.
    pub fn try_parse<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        <Self as Parser>::try_parse_from(args)
    }

    /// Apply grammar rules that depend on the parsed command as a whole.
    pub fn validate(&self) -> Result<(), CliValidationError> {
        let default = std::env::var("PKG_UPGRADE_DEFAULT").ok();
        self.validate_with_upgrade_default(default.as_deref())
    }

    fn validate_with_upgrade_default(
        &self,
        upgrade_default: Option<&str>,
    ) -> Result<(), CliValidationError> {
        if let Command::Doctor(args) = &self.command
            && args.support
            && (self.json || self.jsonl)
        {
            return Err(CliValidationError::SupportOutputConflict);
        }
        if let Command::Upgrade(args) = &self.command
            && args.packages.is_empty()
            && !args.all
            && upgrade_default != Some("all")
        {
            return Err(CliValidationError::UpgradeScopeRequired);
        }
        Ok(())
    }

    /// Whether single-document JSON output was requested.
    #[must_use]
    pub const fn json(&self) -> bool {
        self.json
    }

    /// Whether NDJSON output was requested.
    #[must_use]
    pub const fn jsonl(&self) -> bool {
        self.jsonl
    }

    /// Stable command name used by result envelopes.
    #[must_use]
    pub const fn command_name(&self) -> &'static str {
        self.command.name()
    }

    /// Parsed command payload.
    #[must_use]
    pub const fn parsed_command(&self) -> &Command {
        &self.command
    }

    /// Whether human progress should be suppressed.
    #[must_use]
    pub const fn quiet(&self) -> bool {
        self.quiet
    }

    /// Whether sanitized verbose detail was requested.
    #[must_use]
    pub const fn verbose(&self) -> bool {
        self.verbose
    }

    /// Whether color was explicitly disabled.
    #[must_use]
    pub const fn no_color(&self) -> bool {
        self.no_color
    }

    /// Whether ordinary confirmation/build-plan preapproval was requested.
    #[must_use]
    pub const fn yes(&self) -> bool {
        self.yes
    }

    /// Whether the operation must stop before mutation.
    #[must_use]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Selected V1 profile.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Optional configuration override.
    #[must_use]
    pub fn config(&self) -> Option<&std::path::Path> {
        self.config.as_deref()
    }

    /// Optional state-root override.
    #[must_use]
    pub fn state(&self) -> Option<&std::path::Path> {
        self.state.as_deref()
    }
}

/// Validation failure for a syntactically parsed CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliValidationError {
    /// Support previews are already one exact JSON document.
    SupportOutputConflict,
    /// `upgrade` named neither packages nor `--all` and no explicit environment default applies.
    UpgradeScopeRequired,
}

impl CliValidationError {
    /// Stable process exit code for the validation failure.
    #[must_use]
    pub const fn exit_code(self) -> ExitCode {
        ExitCode::Usage
    }

    /// Product-facing remediation for this grammar failure.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::SupportOutputConflict => {
                "use doctor --support without --json or --jsonl; the preview is already JSON"
            }
            Self::UpgradeScopeRequired => "name one or more installed packages, or pass --all",
        }
    }
}

impl std::fmt::Display for CliValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SupportOutputConflict => {
                formatter.write_str("doctor --support cannot be combined with --json or --jsonl")
            }
            Self::UpgradeScopeRequired => {
                formatter.write_str("upgrade requires package names or --all")
            }
        }
    }
}

impl std::error::Error for CliValidationError {}

/// Complete V1 command set.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Check environment, trust, managed runtime, state, and activation health.
    Doctor(DoctorArgs),
    /// Search the locally verified package index.
    Search(SearchArgs),
    /// Show package metadata.
    Info(InfoArgs),
    /// Add packages and activate a new generation.
    Install(InstallArgs),
    /// Remove packages and activate a new generation.
    Remove(RemoveArgs),
    /// List packages in the active generation.
    List(ListArgs),
    /// Compare installed packages with the accepted source revision.
    Outdated,
    /// Refresh signed metadata and the disposable index without changing packages.
    Update(UpdateArgs),
    /// Upgrade selected packages or all unpinned packages.
    Upgrade(UpgradeArgs),
    /// Freeze installed selectors at their current realized identity.
    Pin(PackageArgs),
    /// Allow installed selectors to move on a future upgrade.
    Unpin(PackageArgs),
    /// Inspect or prune generation history.
    History(HistoryArgs),
    /// Activate a previous generation through a new monotonic history row.
    Rollback(RollbackArgs),
    /// Prune eligible generations and collect unreferenced store content.
    Gc(GcArgs),
    /// Verify and, when necessary, repair an installed generation.
    Repair(RepairArgs),
    /// Emit static completion code for a supported shell.
    Completion(CompletionArgs),
}

impl Command {
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Doctor(_) => "doctor",
            Self::Search(_) => "search",
            Self::Info(_) => "info",
            Self::Install(_) => "install",
            Self::Remove(_) => "remove",
            Self::List(_) => "list",
            Self::Outdated => "outdated",
            Self::Update(_) => "update",
            Self::Upgrade(_) => "upgrade",
            Self::Pin(_) => "pin",
            Self::Unpin(_) => "unpin",
            Self::History(_) => "history",
            Self::Rollback(_) => "rollback",
            Self::Gc(_) => "gc",
            Self::Repair(_) => "repair",
            Self::Completion(_) => "completion",
        }
    }
}

/// Read-only doctor command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct DoctorArgs {
    /// Preview a privacy-minimized support bundle; nothing is uploaded.
    #[arg(long)]
    support: bool,
}

impl DoctorArgs {
    /// Whether to emit the exact support-bundle preview.
    #[must_use]
    pub const fn support(&self) -> bool {
        self.support
    }
}

/// Search command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SearchArgs {
    /// Search terms.
    query: String,
    /// Maximum rows to return.
    #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u16).range(1..))]
    limit: u16,
    /// Select a signed channel by product identifier.
    #[arg(long, value_name = "ID")]
    channel: Option<String>,
    /// Require an exact package-name match.
    #[arg(long)]
    exact: bool,
    /// Filter by SPDX license identifier.
    #[arg(long, value_name = "SPDX")]
    license: Option<String>,
}

impl SearchArgs {
    /// Search text supplied by the user.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }
    /// Maximum requested row count.
    #[must_use]
    pub const fn limit(&self) -> u16 {
        self.limit
    }
    /// Optional product channel identifier.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.channel.as_deref()
    }
    /// Whether only an exact package id may match.
    #[must_use]
    pub const fn exact(&self) -> bool {
        self.exact
    }
    /// Optional SPDX license filter.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }
}

/// Package metadata arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InfoArgs {
    /// Package selectors to inspect.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<String>,
    /// Evaluate the pinned source without realizing or building it.
    #[arg(long)]
    exact: bool,
    /// Select a signed channel by product identifier.
    #[arg(long, value_name = "ID")]
    channel: Option<String>,
}

impl InfoArgs {
    /// Package selectors to inspect.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }
    /// Whether pinned-source evaluation was requested.
    #[must_use]
    pub const fn exact(&self) -> bool {
        self.exact
    }
    /// Optional product channel identifier.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.channel.as_deref()
    }
}

/// Install command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InstallArgs {
    /// Package selectors to install.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<String>,
    /// Select explicit package outputs (comma-delimited; repeatable).
    #[arg(long, value_name = "OUTPUTS", value_delimiter = ',', action = ArgAction::Append)]
    with_outputs: Vec<String>,
    /// Deterministic activation collision policy.
    #[arg(long, value_enum, default_value_t = CollisionPolicy::Abort)]
    on_collision: CollisionPolicy,
    /// Resolve every target but commit nothing if any target fails.
    #[arg(long)]
    keep_going: bool,
    /// Select a signed channel by product identifier.
    #[arg(long, value_name = "ID")]
    channel: Option<String>,
}

impl InstallArgs {
    /// Package selectors to install.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }
    /// Explicit selected outputs.
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.with_outputs
    }
    /// Activation collision policy.
    #[must_use]
    pub const fn collision_policy(&self) -> CollisionPolicy {
        self.on_collision
    }
    /// Whether target resolution should collect all failures before refusing.
    #[must_use]
    pub const fn keep_going(&self) -> bool {
        self.keep_going
    }
    /// Optional product channel identifier.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.channel.as_deref()
    }
}

/// Remove command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RemoveArgs {
    /// Installed package selectors to remove.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<String>,
    /// Preview closures that become collectible after removal.
    #[arg(long)]
    orphan_check: bool,
}

impl RemoveArgs {
    /// Installed selectors to remove.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }
    /// Whether collectible closures should be previewed.
    #[must_use]
    pub const fn orphan_check(&self) -> bool {
        self.orphan_check
    }
}

/// List command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ListArgs {
    /// Print package names only.
    #[arg(long)]
    name_only: bool,
    /// Include selected output names.
    #[arg(long)]
    with_outputs: bool,
    /// Include realized closure bytes.
    #[arg(long)]
    size: bool,
    /// Show only pinned packages.
    #[arg(long)]
    pinned: bool,
    /// Include accepted-channel outdated status.
    #[arg(long)]
    outdated: bool,
}

impl ListArgs {
    /// Whether only package names should be rendered.
    #[must_use]
    pub const fn name_only(&self) -> bool {
        self.name_only
    }
    /// Whether selected output names should be included.
    #[must_use]
    pub const fn with_outputs(&self) -> bool {
        self.with_outputs
    }
    /// Whether realized closure bytes should be included.
    #[must_use]
    pub const fn size(&self) -> bool {
        self.size
    }
    /// Whether to return only pinned selectors.
    #[must_use]
    pub const fn pinned(&self) -> bool {
        self.pinned
    }
    /// Whether accepted-source outdated state should be included.
    #[must_use]
    pub const fn outdated(&self) -> bool {
        self.outdated
    }
}

/// Metadata update arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct UpdateArgs {
    /// Report whether newer signed metadata is available without accepting it.
    #[arg(long)]
    check: bool,
    /// Re-download metadata even when the local copy is fresh.
    #[arg(long)]
    force: bool,
}

impl UpdateArgs {
    /// Whether to check without accepting newer metadata.
    #[must_use]
    pub const fn check(&self) -> bool {
        self.check
    }
    /// Whether to refresh even when local metadata is fresh.
    #[must_use]
    pub const fn force(&self) -> bool {
        self.force
    }
}

/// Upgrade command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
#[command(group(ArgGroup::new("scope").args(["packages", "all"]).multiple(false)))]
pub struct UpgradeArgs {
    /// Installed selectors to upgrade.
    #[arg(num_args = 1..)]
    packages: Vec<String>,
    /// Upgrade every unpinned selector.
    #[arg(long)]
    all: bool,
    /// Permit explicitly pinned selectors to move.
    #[arg(long)]
    bump_pinned: bool,
    /// Refuse if any selected target requires a local build.
    #[arg(long)]
    no_build: bool,
    /// Report and skip selectors removed upstream.
    #[arg(long)]
    include_removed_upstream: bool,
    /// Select explicit package outputs (comma-delimited; repeatable).
    #[arg(long, value_name = "OUTPUTS", value_delimiter = ',', action = ArgAction::Append)]
    with_outputs: Vec<String>,
    /// Deterministic activation collision policy.
    #[arg(long, value_enum, default_value_t = CollisionPolicy::Abort)]
    on_collision: CollisionPolicy,
    /// Resolve every target but commit nothing if any target fails.
    #[arg(long)]
    keep_going: bool,
    /// Select a signed channel by product identifier.
    #[arg(long, value_name = "ID")]
    channel: Option<String>,
}

impl UpgradeArgs {
    /// Named installed selectors to upgrade.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }
    /// Whether every unpinned selector is in scope.
    #[must_use]
    pub const fn all(&self) -> bool {
        self.all
    }
    /// Whether explicitly pinned selectors may move.
    #[must_use]
    pub const fn bump_pinned(&self) -> bool {
        self.bump_pinned
    }
    /// Whether any local-build need must refuse the operation.
    #[must_use]
    pub const fn no_build(&self) -> bool {
        self.no_build
    }
    /// Whether removed upstream selectors should be reported and skipped.
    #[must_use]
    pub const fn include_removed_upstream(&self) -> bool {
        self.include_removed_upstream
    }
    /// Explicit selected outputs.
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.with_outputs
    }
    /// Activation collision policy.
    #[must_use]
    pub const fn collision_policy(&self) -> CollisionPolicy {
        self.on_collision
    }
    /// Whether target resolution should collect all failures before refusing.
    #[must_use]
    pub const fn keep_going(&self) -> bool {
        self.keep_going
    }
    /// Optional product channel identifier.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        self.channel.as_deref()
    }
}

/// One-or-more package selectors.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct PackageArgs {
    /// Installed package selectors.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<String>,
}

impl PackageArgs {
    /// Installed selectors named by the operation.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }
}

/// History command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct HistoryArgs {
    /// Compare two generation identifiers.
    #[arg(long, value_names = ["A", "B"], num_args = 2, conflicts_with = "delete")]
    diff: Vec<String>,
    /// Prune one non-active generation.
    #[arg(long, value_name = "ID", conflicts_with = "diff")]
    delete: Option<String>,
}

impl HistoryArgs {
    /// Optional pair of generation ids to compare.
    #[must_use]
    pub fn diff(&self) -> &[String] {
        &self.diff
    }
    /// Optional non-active generation id to prune.
    #[must_use]
    pub fn delete(&self) -> Option<&str> {
        self.delete.as_deref()
    }
}

/// Rollback command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RollbackArgs {
    /// Generation identifier; defaults to the active generation's parent.
    #[arg(value_name = "ID")]
    generation: Option<String>,
}

impl RollbackArgs {
    /// Explicit rollback target, or `None` for the active parent.
    #[must_use]
    pub fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }
}

/// Garbage-collection command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct GcArgs {
    /// Number of recent generations to preserve.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    keep_generations: Option<u32>,
    /// Preserve generations no older than this many days.
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u32).range(1..))]
    max_age_days: Option<u32>,
}

impl GcArgs {
    /// Optional recent-generation retention override.
    #[must_use]
    pub const fn keep_generations(&self) -> Option<u32> {
        self.keep_generations
    }
    /// Optional age-window retention override in days.
    #[must_use]
    pub const fn max_age_days(&self) -> Option<u32> {
        self.max_age_days
    }
}

/// Repair command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct RepairArgs {
    /// Generation identifier; defaults to the active generation.
    #[arg(value_name = "ID")]
    generation: Option<String>,
    /// Run only the read-only verification phase.
    #[arg(long, conflicts_with_all = ["from_manifest", "from_lock"])]
    verify_only: bool,
    /// Rebuild lock state from one durable generation manifest.
    #[arg(long, value_name = "GENERATION", conflicts_with_all = ["verify_only", "from_lock"])]
    from_manifest: Option<String>,
    /// Rebuild desired manifest state from verified lock/store reality.
    #[arg(long, conflicts_with_all = ["verify_only", "from_manifest"])]
    from_lock: bool,
}

impl RepairArgs {
    /// Explicit generation target, or `None` for active.
    #[must_use]
    pub fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }
    /// Whether only read-only verification may run.
    #[must_use]
    pub const fn verify_only(&self) -> bool {
        self.verify_only
    }
    /// Optional generation whose durable manifest should restore lock state.
    #[must_use]
    pub fn from_manifest(&self) -> Option<&str> {
        self.from_manifest.as_deref()
    }
    /// Whether verified lock/store reality should restore desired state.
    #[must_use]
    pub const fn from_lock(&self) -> bool {
        self.from_lock
    }
}

/// Completion command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct CompletionArgs {
    /// Shell whose static completion source should be emitted.
    #[arg(value_enum)]
    shell: CompletionShell,
}

impl CompletionArgs {
    /// Requested completion shell.
    #[must_use]
    pub const fn shell(&self) -> CompletionShell {
        self.shell
    }
}

/// Supported activation collision policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum CollisionPolicy {
    /// Refuse activation when a relative path has multiple providers.
    #[default]
    Abort,
    /// Keep the first deterministic provider for each colliding path.
    KeepFirst,
    /// Keep the last deterministic provider for each colliding path.
    KeepLast,
}

/// Shells supported by the V1 completion surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// Bourne Again Shell.
    Bash,
    /// Z shell.
    Zsh,
    /// Friendly Interactive Shell.
    Fish,
    /// Microsoft PowerShell.
    Powershell,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, error::ErrorKind};

    use super::*;

    #[test]
    fn clap_contract_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_lists_every_v1_verb_and_no_internal_surface() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        for verb in [
            "doctor",
            "search",
            "info",
            "install",
            "remove",
            "list",
            "outdated",
            "update",
            "upgrade",
            "pin",
            "unpin",
            "history",
            "rollback",
            "gc",
            "repair",
            "completion",
        ] {
            assert!(help.contains(verb), "missing {verb} from help");
        }
        for forbidden in ["--debug", "--tui", "--max-jobs", "--cores", "logs"] {
            assert!(!help.contains(forbidden), "exposed {forbidden}");
        }
    }

    #[test]
    fn minimal_invocations_cover_every_command() {
        for args in [
            vec!["pkg", "doctor"],
            vec!["pkg", "search", "ripgrep"],
            vec!["pkg", "info", "ripgrep"],
            vec!["pkg", "install", "ripgrep"],
            vec!["pkg", "remove", "ripgrep"],
            vec!["pkg", "list"],
            vec!["pkg", "outdated"],
            vec!["pkg", "update"],
            vec!["pkg", "upgrade", "ripgrep"],
            vec!["pkg", "pin", "ripgrep"],
            vec!["pkg", "unpin", "ripgrep"],
            vec!["pkg", "history"],
            vec!["pkg", "rollback"],
            vec!["pkg", "gc"],
            vec!["pkg", "repair"],
            vec!["pkg", "completion", "bash"],
        ] {
            let parsed = Cli::try_parse(args).unwrap();
            assert!(parsed.validate().is_ok());
        }
    }

    #[test]
    fn output_and_verbosity_modes_are_exclusive_usage_errors() {
        for args in [
            ["pkg", "--json", "--jsonl", "doctor"],
            ["pkg", "--quiet", "--verbose", "doctor"],
        ] {
            let error = Cli::try_parse(args).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
            assert_eq!(error.exit_code(), i32::from(ExitCode::Usage.as_u8()));
        }
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let parsed = Cli::try_parse(["pkg", "list", "--json", "--state", "/tmp/state"]).unwrap();
        assert!(parsed.json());
        assert_eq!(parsed.state(), Some(std::path::Path::new("/tmp/state")));
    }

    #[test]
    fn upgrade_requires_explicit_scope_and_forbids_mixed_scope() {
        let parsed = Cli::try_parse(["pkg", "upgrade"]).unwrap();
        assert_eq!(
            parsed.validate_with_upgrade_default(None),
            Err(CliValidationError::UpgradeScopeRequired)
        );
        assert!(parsed.validate_with_upgrade_default(Some("all")).is_ok());
        assert!(Cli::try_parse(["pkg", "upgrade", "ripgrep", "--all"]).is_err());
    }

    #[test]
    fn collision_and_repair_grammars_fail_closed() {
        assert!(Cli::try_parse(["pkg", "install", "x", "--on-collision", "keep-all"]).is_err());
        assert!(Cli::try_parse(["pkg", "install", "x", "--force"]).is_err());
        assert!(Cli::try_parse(["pkg", "search", "x", "--category", "tools"]).is_err());
        assert!(Cli::try_parse(["pkg", "repair", "--verify-only", "--from-lock"]).is_err());
    }
}
