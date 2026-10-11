//! Reduced command parser (design D4).
//!
//! The grammar lists only the retained commands. Removed alpha commands and
//! global options are rejected as ordinary usage errors.

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

/// The retained command set (design D4).
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
    /// Generate shell completion for the reduced grammar.
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
    /// Treat the query as a regular expression.
    #[arg(long)]
    pub regex: bool,
    /// Include libraries, development packages, variants, and substring matches.
    #[arg(long)]
    pub broad: bool,
    /// Disable name typo alternatives when literal results are absent.
    #[arg(long, conflicts_with = "regex")]
    pub no_fuzzy: bool,
    /// Maximum total results. Human sections share 20 by default; JSON defaults to all.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "all")]
    pub limit: Option<u32>,
    /// Show all matching results.
    #[arg(long)]
    pub all: bool,
    /// Search nixpkgs, brew formulae, homebrew, cask, homebrew/core, homebrew/cask, or a saved tap.
    #[arg(long)]
    pub source: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_reduced_grammar() {
        use clap::Parser as _;
        let cli =
            Cli::try_parse_from(["pkg", "prune", "--older-than", "30d"]).expect("prune parses");
        match &cli.command {
            Command::Prune { older_than } => assert_eq!(older_than, "30d"),
            other => panic!("unexpected command {other:?}"),
        }
        assert!(Cli::try_parse_from(["pkg", "apps", "sync"]).is_ok());
        assert!(Cli::try_parse_from(["pkg", "upgrade", "--all"]).is_ok());
    }

    #[test]
    fn lifecycle_aliases_keep_arguments_and_options() {
        for name in ["install", "add"] {
            let cli = Cli::try_parse_from(["pkg", name, "nixpkgs:fff", "brew:bat"]).unwrap();
            let Command::Install { ids } = cli.command else {
                panic!("install handler")
            };
            assert_eq!(ids, ["nixpkgs:fff", "brew:bat"]);
            assert!(Cli::try_parse_from(["pkg", name]).is_err());
        }
        for name in ["remove", "uninstall"] {
            let cli =
                Cli::try_parse_from(["pkg", "--verbose", name, "--entry", "--", "-fff"]).unwrap();
            let Command::Remove { entries, entry } = cli.command else {
                panic!("remove handler")
            };
            assert_eq!(entries, ["-fff"]);
            assert!(entry && cli.verbose);
            assert!(Cli::try_parse_from(["pkg", name]).is_err());
        }
    }

    #[test]
    fn upgrade_all_conflicts_with_entry_names() {
        use clap::Parser as _;
        // Silently ignoring the names would hide a user mistake; clap turns
        // the conflict into a normal usage error.
        assert!(Cli::try_parse_from(["pkg", "upgrade", "--all", "ripgrep"]).is_err());
        assert!(Cli::try_parse_from(["pkg", "upgrade", "ripgrep"]).is_ok());
    }

    #[test]
    fn rejects_removed_commands_and_options() {
        use clap::Parser as _;
        for args in [
            vec!["pkg", "pin", "ripgrep"],
            vec!["pkg", "unpin", "ripgrep"],
            vec!["pkg", "outdated"],
            vec!["pkg", "repair"],
            vec!["pkg", "gc"],
            vec!["pkg", "system", "uninstall"],
            vec!["pkg", "--dry-run", "install", "nixpkgs:ripgrep"],
            vec!["pkg", "install", "--keep-first", "nixpkgs:ripgrep"],
        ] {
            let joined = args.join(" ");
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{joined} should not parse"
            );
        }
    }

    #[test]
    fn prune_age_must_be_explicit_and_positive() {
        assert_eq!(parse_prune_age("30d"), Ok(30));
        assert!(parse_prune_age("0d").is_err());
        assert!(parse_prune_age("30").is_err());
        assert!(parse_prune_age("-1d").is_err());
        assert!(parse_prune_age("").is_err());
    }
}
