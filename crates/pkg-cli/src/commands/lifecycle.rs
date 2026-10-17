use crate::catalog;
use crate::cli::Cli;

use super::change::{ProfileChange, Request};
use super::{CommandError, session};

pub(super) fn install(cli: &Cli, ids: &[String]) -> Result<(), CommandError> {
    if ids.is_empty() {
        return Err(String::from("install needs at least one ID").into());
    }
    let parsed_ids = ids
        .iter()
        .map(|id| catalog::parse_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    eprintln!("Resolving: {}", ids.join(", "));
    let session = session(cli)?;
    let mut resolution = super::resolution::Resolution::new(&session)?;
    let installables = resolution.install_batch(parsed_ids)?;
    let system = resolution.system;
    super::change::execute(
        &session,
        Request::Profile(ProfileChange::Install {
            installables,
            system: &system,
        }),
    )
}

pub(super) fn remove(cli: &Cli, entries: &[String], exact_entry: bool) -> Result<(), CommandError> {
    if entries.is_empty() {
        return Err(String::from("remove needs at least one installed entry ID").into());
    }
    super::change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Remove {
            entries,
            exact_entry,
        }),
    )
}

pub(super) fn upgrade(
    cli: &Cli,
    entries: &[String],
    all: bool,
    exact_entry: bool,
) -> Result<(), CommandError> {
    super::change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Upgrade {
            entries,
            all,
            exact_entry,
        }),
    )
}

pub(super) fn history(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    let text = session.nix.profile_history(&session.paths.profile)?;
    // `--no-color` holds for native text pkg passes through, even if
    // the runtime colorized it anyway.
    let text = if session.config.color_disabled(cli.no_color) {
        crate::output::strip_ansi(&text)
    } else {
        text
    };
    if cli.verbose {
        print!("{text}");
    } else {
        print!(
            "{}",
            render_history(&text, active_generation(&session.paths.profile))
        );
    }
    Ok(())
}

/// Read the active generation from the native profile link when available.
pub(super) fn active_generation(profile: &std::path::Path) -> Option<u64> {
    let link = std::fs::read_link(profile).ok()?;
    let name = link.file_name()?.to_str()?;
    name.strip_suffix("-link")?.rsplit('-').next()?.parse().ok()
}

/// Keep generation numbers and dates; hide per-package native history detail.
fn render_history(text: &str, active: Option<u64>) -> String {
    use std::fmt::Write as _;
    let clean = crate::output::strip_ansi(text);
    let rows: Vec<(u64, &str)> = clean
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Version ")?;
            let (number, rest) = rest.split_once(" (")?;
            let (date, _) = rest.split_once(')')?;
            Some((number.parse().ok()?, date))
        })
        .collect();
    if rows.is_empty() {
        return clean;
    }
    let mut out = String::from("GEN  CREATED  STATUS\n");
    for (number, date) in rows.iter().rev() {
        let status = if Some(*number) == active {
            "current"
        } else {
            ""
        };
        let _ = writeln!(out, "{number}  {date}  {status}");
    }
    if rows.len() == 1 {
        out.push_str("No earlier generation to restore.\n");
    }
    out
}

pub(super) fn rollback(cli: &Cli, generation: Option<u64>) -> Result<(), CommandError> {
    super::change::execute(
        &session(cli)?,
        Request::Profile(ProfileChange::Rollback(generation)),
    )
}

pub(super) fn prune(cli: &Cli, older_than: &str) -> Result<(), CommandError> {
    let days = crate::cli::parse_prune_age(older_than)?;
    super::change::execute(&session(cli)?, Request::Prune { days })
}
