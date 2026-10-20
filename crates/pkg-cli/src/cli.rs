//! Parse commands and global options for `pkg`.

use clap::{Parser, Subcommand, ValueEnum};

/// A small client over native Nix.
#[derive(Debug, Parser)]
#[command(
    name = "pkg",
    version,
    about = "Find and manage packages.",
    help_template = "{about}\n\nUsage: {usage}\n\nCommands:\n{subcommands}\n\nOptions:\n{options}",
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Print JSON. Works with search, info, list, doctor, and tap list.
    #[arg(long, global = true)]
    pub json: bool,

    /// Print no color.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Show the Nix commands and their full output.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Supported commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Find packages.
    Search(SearchOptions),
    /// Show details about one package.
    Info {
        /// Package name or ID, such as ripgrep or cask:raycast.
        #[arg(value_name = "PACKAGE")]
        id: String,
    },
    /// Install packages.
    #[command(visible_alias = "add")]
    Install {
        /// Package names or IDs, such as ripgrep or cask:raycast.
        #[arg(required = true, value_name = "PACKAGES")]
        ids: Vec<String>,
    },
    /// List installed packages.
    List,
    /// Remove packages.
    #[command(visible_alias = "uninstall")]
    Remove {
        /// Installed package names or IDs.
        #[arg(required = true, value_name = "NAMES")]
        entries: Vec<String>,
        /// Treat the names as entry IDs from `pkg list --verbose`.
        #[arg(long)]
        entry: bool,
    },
    /// Upgrade installed packages.
    ///
    /// Without names, upgrades all packages.
    Upgrade {
        /// Installed package names or IDs. Leave empty to upgrade all packages.
        #[arg(value_name = "NAMES")]
        entries: Vec<String>,
        /// Treat the names as entry IDs from `pkg list --verbose`.
        #[arg(long, requires = "entries", conflicts_with = "all")]
        entry: bool,
        /// Upgrade all packages. This is the default.
        #[arg(long, conflicts_with = "entries", hide = true)]
        all: bool,
    },
    /// Update pkg and its search data.
    ///
    /// Installed packages do not change. To upgrade them, run `pkg upgrade`.
    Update {
        /// Skip saved taps. Used by the scheduled user job.
        #[arg(long, hide = true)]
        scheduled: bool,
        /// Skip the client check. Used by a client that just replaced itself.
        #[arg(long, hide = true)]
        skip_client: bool,
    },
    /// Control the daily user job for updates and search data.
    #[command(subcommand, hide = true)]
    Background(BackgroundCommand),
    /// Show past changes to your packages.
    History,
    /// Undo the last change.
    ///
    /// With a generation number from `pkg history`, go back to that change.
    Rollback {
        /// Generation number from `pkg history`. Defaults to the one before the current one.
        generation: Option<u64>,
    },
    /// Check your setup.
    ///
    /// Reports problems and how to fix them. Changes nothing.
    Doctor,
    /// Print idempotent Bash/Zsh path settings for this profile.
    #[command(hide = true)]
    Shellenv,
    /// Generate shell completion for supported commands.
    #[command(hide = true)]
    Completion {
        /// Target shell.
        shell: Shell,
    },
    /// Manage macOS app launchers.
    #[command(subcommand)]
    Apps(AppsCommand),
    /// Manage third-party Homebrew Cask taps.
    #[command(subcommand)]
    Tap(TapCommand),
    /// Delete history older than an age.
    Prune {
        /// Age such as `30d`.
        #[arg(long, value_name = "AGE")]
        older_than: String,
    },
    /// Manage pkg itself.
    #[command(name = "self", subcommand)]
    Client(ClientCommand),
}

/// Commands for the pkg client itself.
#[derive(Debug, Subcommand)]
pub enum ClientCommand {
    /// Remove pkg from this computer. Keeps Nix and installed packages.
    Uninstall {
        /// Remove without a confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },
}

/// Local search inputs and output controls.
#[derive(Debug, clap::Args)]
pub struct SearchOptions {
    /// Package names or words. Also accepts nixpkgs:, brew:, or cask: queries.
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

/// Controls for the scheduled user job.
#[derive(Debug, Subcommand)]
pub enum BackgroundCommand {
    /// Enable the daily job for the current user and start it now.
    Enable,
    /// Disable the daily job and remove pkg-owned job files.
    Disable,
    /// Show whether the user job is enabled.
    Status,
}

/// Launcher subcommands. Execution is supplied by the apps module.
#[derive(Debug, Subcommand)]
pub enum AppsCommand {
    /// Prepare storage for apps that need their original signatures.
    Setup,
    /// Rebuild the app launchers in ~/Applications/pkg.
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
