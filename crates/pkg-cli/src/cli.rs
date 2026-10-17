//! Parse commands and global options for `pkg`.

use clap::{Parser, Subcommand, ValueEnum};

/// A small client over native Nix.
#[derive(Debug, Parser)]
#[command(
    name = "pkg",
    version,
    about = "Find and manage packages.",
    help_template = "{about}\n\nFind:     search, info, list\nChange:   install/add, remove/uninstall, upgrade, rollback\nMaintain: setup, update, background, history, prune, doctor, apps\nSources:  tap add, tap list, tap update, tap remove\n\nUsage: {usage}\n\nCommands:\n{subcommands}\n\nOptions:\n{options}",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Emit JSON for search, info, list, doctor, and tap list.
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Show native commands and detailed output.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Supported commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search local package catalogs.
    Search(SearchOptions),
    /// Show source, attribute, revision, and limits for one ID.
    Info {
        /// Catalog ID such as nixpkgs:ripgrep, cask:raycast, or a bare name.
        id: String,
    },
    /// Resolve all IDs, then add them in one native profile operation.
    #[command(visible_alias = "add")]
    Install {
        /// Catalog IDs or supported public GitHub flake installables.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// List installed entries from the dedicated native profile.
    List,
    /// Remove installed packages in one native operation.
    #[command(visible_alias = "uninstall")]
    Remove {
        /// Package IDs, unique names, or native entry IDs from `pkg list`.
        #[arg(required = true)]
        entries: Vec<String>,
        /// Select exact native entry IDs instead of package names.
        #[arg(long)]
        entry: bool,
    },
    /// Refresh disposable discovery data. Does not change the profile.
    Update {
        /// Refresh official catalogs only. Used by the scheduled user job.
        #[arg(long, hide = true)]
        catalogs_only: bool,
    },
    /// Prepare local search and enable daily background refresh.
    Setup {
        /// Prepare catalogs without a scheduled user job.
        #[arg(long)]
        no_background: bool,
    },
    /// Control the daily user job for official catalog refresh.
    #[command(subcommand)]
    Background(BackgroundCommand),
    /// Upgrade installed entries following their original references.
    Upgrade {
        /// Package IDs, unique names, or native entry IDs to upgrade.
        #[arg(required_unless_present = "all")]
        entries: Vec<String>,
        /// Select exact native entry IDs instead of package names.
        #[arg(long, conflicts_with = "all")]
        entry: bool,
        /// Upgrade every installed entry, reporting fixed ones explicitly.
        #[arg(long, conflicts_with = "entries")]
        all: bool,
    },
    /// Display native profile generations.
    History,
    /// Switch to the previous or a selected native generation.
    Rollback {
        /// Generation number to select. Defaults to the previous generation.
        generation: Option<u64>,
    },
    /// Delete non-current profile generations older than an explicit age.
    Prune {
        /// Retention age such as `30d`. Required and strictly positive.
        #[arg(long, value_name = "AGE")]
        older_than: String,
    },
    /// Read runtime, profile, and launcher status. Never repairs.
    Doctor,
    /// Print idempotent Bash/Zsh path settings for this profile.
    Shellenv,
    /// Generate shell completion for supported commands.
    Completion {
        /// Target shell.
        shell: Shell,
    },
    /// Derived macOS app launcher operations.
    #[command(subcommand)]
    Apps(AppsCommand),
    /// Manage local public Homebrew Cask taps.
    #[command(subcommand)]
    Tap(TapCommand),
}

/// Local search inputs and output controls.
#[derive(Debug, clap::Args)]
pub struct SearchOptions {
    /// Package names or search words. Also accepts nixpkgs:, brew:, or cask: queries.
    pub query: String,
    /// Search matching controls.
    #[command(flatten)]
    pub matching: SearchMatchingOptions,
    /// Maximum total results. Human sections share 20 by default; JSON defaults to all.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "all")]
    pub limit: Option<u32>,
    /// Result filtering and output controls.
    #[command(flatten)]
    pub results: SearchResultOptions,
    /// Search nixpkgs, brew formulae, homebrew, cask, homebrew/core, homebrew/cask, or a saved tap.
    #[arg(long)]
    pub source: Option<String>,
}

/// Controls for matching local search results.
#[derive(Debug, clap::Args)]
pub struct SearchMatchingOptions {
    /// Treat the query as a regular expression.
    #[arg(long)]
    pub regex: bool,
    /// Include libraries, development packages, variants, and substring matches.
    #[arg(long)]
    pub broad: bool,
    /// Disable name typo alternatives when literal results are absent.
    #[arg(long, conflicts_with = "regex")]
    pub no_fuzzy: bool,
}

/// Controls which matching results are shown.
#[derive(Debug, clap::Args)]
pub struct SearchResultOptions {
    /// Show all matching results.
    #[arg(long)]
    pub all: bool,
    /// Include unavailable casks and disabled formulae. Exact names are always shown.
    #[arg(long)]
    pub unavailable: bool,
}

/// Controls for the scheduled official catalog refresh.
#[derive(Debug, Subcommand)]
pub enum BackgroundCommand {
    /// Enable daily refresh for the current user.
    Enable,
    /// Disable daily refresh and remove pkg-owned job files.
    Disable,
    /// Show whether the user job is enabled.
    Status,
}

/// Launcher subcommands. Execution is supplied by the apps module.
#[derive(Debug, Subcommand)]
pub enum AppsCommand {
    /// Set up private app storage that shares file data with the Nix store.
    Setup,
    /// Rebuild pkg-owned launchers from the active profile.
    Sync,
}

/// Local public tap subcommands.
#[derive(Debug, Subcommand)]
pub enum TapCommand {
    /// Approve and import a public Homebrew Cask tap. Asks for consent
    /// before any tap code runs; `--trust` approves without a prompt.
    Add {
        /// Tap source: `owner/tap` or `https://github.com/owner/homebrew-tap`.
        source: String,
        /// Exact 40-hex commit id to import instead of the default branch.
        #[arg(long, value_name = "SHA")]
        revision: Option<String>,
        /// Approve the tap origin without an interactive prompt.
        #[arg(long)]
        trust: bool,
    },
    /// List registered taps and their published revisions.
    List,
    /// Re-import one tap or every registered tap at a newer revision.
    /// Runs tap code again; recorded consent covers it.
    Update {
        /// Tap source to update. Updates every tap when omitted.
        source: Option<String>,
        /// Exact 40-hex commit id to import instead of the default branch.
        #[arg(long, value_name = "SHA")]
        revision: Option<String>,
    },
    /// Unregister a tap. Keeps installed packages and old generations.
    Remove {
        /// Tap source to remove.
        source: String,
    },
}

/// Supported completion targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// Bourne Again Shell.
    Bash,
    /// Z Shell.
    Zsh,
    /// Friendly Interactive Shell.
    Fish,
}

/// Validate the `prune --older-than` age format.
///
/// The design requires an explicit strictly positive age expressed in days.
/// # Errors
///
/// Returns an error unless `value` is a positive integer followed by `d` or `D`, or if the integer exceeds `u64`.
pub fn parse_prune_age(value: &str) -> Result<u64, String> {
    let Some(digits) = value.strip_suffix(['d', 'D']) else {
        return Err(String::from("age must end in `d`, for example `30d`"));
    };
    let Ok(days) = digits.parse::<u64>() else {
        return Err(String::from(
            "age must be a number of days, for example `30d`",
        ));
    };
    if days == 0 {
        return Err(String::from(
            "age must be strictly positive, for example `30d`",
        ));
    }
    Ok(days)
}
