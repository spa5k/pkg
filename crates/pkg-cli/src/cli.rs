//! Parse commands and global options for `pkg`.

use clap::{Parser, Subcommand, ValueEnum};

/// A small client over native Nix.
#[derive(Debug, Parser)]
#[command(
    name = "pkg",
    version,
    about = "Find and manage packages.",
    help_template = "{about}\n\nUsage: {usage}\n\n{all-args}",
    disable_help_subcommand = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Print JSON. Works with search, info, list, history, doctor, and tap list.
    #[arg(long, global = true, help_heading = "Global options")]
    pub json: bool,

    /// Print no color.
    #[arg(long, global = true, help_heading = "Global options")]
    pub no_color: bool,

    /// Show the Nix commands and their full output.
    #[arg(long, global = true, help_heading = "Global options")]
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
        /// Build `github:` flakes without a confirmation prompt.
        #[arg(long, short)]
        yes: bool,
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
    /// Turn daily updates of pkg and its search data on or off.
    #[command(subcommand, hide = true)]
    Background(BackgroundCommand),
    /// Show past changes to your packages.
    History,
    /// Undo the last change.
    ///
    /// Without a number, go back to the last generation with other packages.
    Rollback {
        /// Go to this generation from `pkg history`.
        #[arg(value_parser = parse_generation, allow_negative_numbers = true)]
        generation: Option<u64>,
    },
    /// Check your setup.
    ///
    /// Reports problems and how to fix them. Changes nothing.
    Doctor,
    /// Print the PATH settings for Bash and Zsh.
    #[command(hide = true)]
    Shellenv,
    /// Print shell completion code.
    #[command(hide = true)]
    Completion {
        /// The shell.
        shell: Shell,
    },
    /// Manage macOS app launchers.
    #[command(subcommand)]
    Apps(AppsCommand),
    /// Add packages from other Homebrew Cask taps.
    #[command(subcommand)]
    Tap(TapCommand),
    /// Delete history older than an age.
    Prune {
        /// Age such as `30d`.
        #[arg(long, value_name = "AGE", allow_hyphen_values = true)]
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
    #[arg(required = true, value_name = "QUERY")]
    pub query: Vec<String>,
    /// Search matching controls.
    #[command(flatten)]
    pub matching: SearchMatchingOptions,
    /// Show at most this many results. The default is 20, or all with `--json`.
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
    /// Do not show similar names when nothing matches.
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
    /// Turn on daily updates, and update now.
    Enable,
    /// Turn off daily updates.
    Disable,
    /// Show whether daily updates are on.
    Status,
}

/// Launcher subcommands. Execution is supplied by the apps module.
#[derive(Debug, Subcommand)]
pub enum AppsCommand {
    /// Prepare storage for apps that must keep their original signature.
    Setup,
    /// Rebuild the app launchers in ~/Applications/pkg.
    Sync,
}

/// Local public tap subcommands.
#[derive(Debug, Subcommand)]
pub enum TapCommand {
    /// Add a public tap. Asks for your approval first.
    Add {
        /// The tap, such as `owner/tap` or its GitHub URL.
        #[arg(value_name = "OWNER/TAP")]
        source: String,
        /// Use this commit (40 hex characters) instead of the newest one.
        #[arg(long, value_name = "SHA")]
        revision: Option<String>,
        /// Approve the tap without a prompt.
        #[arg(long)]
        trust: bool,
    },
    /// List saved taps.
    List,
    /// Update one saved tap, or all of them.
    Update {
        /// The tap to update. Leave empty to update all taps.
        #[arg(value_name = "OWNER/TAP")]
        source: Option<String>,
        /// Use this commit (40 hex characters) instead of the newest one.
        #[arg(long, value_name = "SHA")]
        revision: Option<String>,
    },
    /// Remove a saved tap. Installed apps stay.
    Remove {
        /// The tap to remove.
        #[arg(value_name = "OWNER/TAP")]
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
/// Returns an error unless `value` is a positive integer followed by `d` or `D`.
/// An age above [`MAX_PRUNE_DAYS`] becomes that limit.
pub fn parse_prune_age(value: &str) -> Result<u64, String> {
    let Some(digits) = value.trim().strip_suffix(['d', 'D']) else {
        return Err(String::from("age must end in `d`, for example `30d`"));
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(String::from(
            "age must be a number of days, for example `30d`",
        ));
    }
    // Only digits remain, so a parse failure means a very large age.
    let days = digits.parse::<u64>().unwrap_or(u64::MAX);
    if days == 0 {
        return Err(String::from(
            "age must be at least 1 day, for example `30d`",
        ));
    }
    Ok(days.min(MAX_PRUNE_DAYS))
}

/// The largest age that Nix accepts safely.
///
/// Nix converts days to seconds in a 32-bit number, so a longer age wraps
/// around and deletes every older generation. No generation is that old, so
/// the limit changes nothing.
pub const MAX_PRUNE_DAYS: u64 = 24_855;

/// Read a generation number for `pkg rollback`.
fn parse_generation(value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse()
        .map_err(|_| String::from("use a generation number from `pkg history`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_ages_above_the_nix_limit_use_the_limit() {
        assert_eq!(parse_prune_age("30d"), Ok(30));
        assert_eq!(parse_prune_age("24856d"), Ok(MAX_PRUNE_DAYS));
        assert_eq!(
            parse_prune_age("99999999999999999999999d"),
            Ok(MAX_PRUNE_DAYS)
        );
    }
}
