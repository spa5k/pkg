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
    about = "Manage packages in one native Nix profile",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Emit versioned JSON for query commands (search, info, list, doctor).
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Show the native Nix command lines that pkg runs.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The retained command set (design D4).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search supported sources.
    Search {
        /// Regex query. The nixpkgs lane passes it to native `nix search`;
        /// the cask lane matches it locally (Rust regex, case-insensitive)
        /// against token, name, and description.
        query: String,
    },
    /// Show source, attribute, revision, and limits for one ID.
    Info {
        /// Catalog ID such as nixpkgs:ripgrep, cask:raycast, or a bare name.
        id: String,
    },
    /// Resolve all IDs, then add them in one native profile operation.
    Install {
        /// Catalog IDs or supported public GitHub flake installables.
        ids: Vec<String>,
    },
    /// List installed entries from the dedicated native profile.
    List,
    /// Remove exact installed entries in one native operation.
    Remove {
        /// Exact native entry IDs as shown by `pkg list`.
        entries: Vec<String>,
    },
    /// Refresh disposable discovery data. Does not change the profile.
    Update,
    /// Upgrade installed entries following their original references.
    Upgrade {
        /// Exact native entry IDs to upgrade.
        entries: Vec<String>,
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

/// Launcher subcommands. Execution is supplied by the apps module.
#[derive(Debug, Subcommand)]
pub enum AppsCommand {
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
