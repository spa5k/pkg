//! Native profile entries and generation history.
use super::{CommandError, Session};
use crate::cli::Cli;
use crate::commands::{print_json, session};
use crate::nix::{NixError, ProfileEntry};
use crate::output::{self, ListRow};
use std::collections::{BTreeMap, BTreeSet};

pub fn list(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    // A fresh install has no profile yet; that state is empty, not an error.
    let entries = installed_entries(&session)?;
    let rows = list_rows(&entries, &session.config, &session.paths.state_home);
    if cli.json {
        print_json(&output::envelope("list", &rows))?;
    } else {
        print!("{}", output::render_list(&rows));
    }
    Ok(())
}

fn list_rows(
    entries: &std::collections::BTreeMap<String, ProfileEntry>,
    config: &crate::config::Config,
    state_home: &std::path::Path,
) -> Vec<ListRow> {
    // Discovery and display scans only consider active entries.
    entries
        .iter()
        .filter(|(_, entry)| entry.active)
        .map(|(entry_id, entry)| ListRow {
            entry_id: entry_id.clone(),
            package_id: super::installed::package_id(entry, config, state_home),
            name: entry.name(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}

pub fn history(cli: &Cli) -> Result<(), CommandError> {
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

/// Read installed entries, treating a not-yet-created profile as empty.
///
/// A fresh install has no profile on disk; that state is empty, not an error.
pub fn installed_entries(session: &Session) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    Ok(installed_state(session)?
        .map_or_else(BTreeMap::new, crate::nix::CheckedProfile::into_entries))
}

pub(super) fn installed_state(
    session: &Session,
) -> Result<Option<crate::nix::CheckedProfile<'_>>, NixError> {
    match std::fs::symlink_metadata(&session.paths.profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(NixError::Io {
                what: "profile state",
                source: error,
            });
        }
        Ok(_) => {}
    }
    session
        .nix
        .checked_profile(&session.paths.profile)
        .map(Some)
}

/// Read native state even when no local profile path is present.
pub fn entries(
    nix: &crate::nix::Nix,
    profile: &std::path::Path,
) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    nix.profile_list(profile)
}

pub(super) fn optional_history(session: &Session) -> Result<Option<BTreeSet<u64>>, NixError> {
    match session.nix.profile_history(&session.paths.profile) {
        Ok(text) => Ok(Some(history_numbers(&text))),
        Err(error) if error.signal().is_some() => Err(error),
        Err(_) => Ok(None),
    }
}

fn history_numbers(text: &str) -> std::collections::BTreeSet<u64> {
    crate::output::strip_ansi(text)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("Version ")?
                .split_once(" (")?
                .0
                .parse()
                .ok()
        })
        .collect()
}

/// Observe the native profile without acquiring a mutation lock.
pub fn status(
    nix: &crate::nix::Nix,
    paths: &crate::config::Paths,
) -> Result<crate::output::DoctorRow, CommandError> {
    let observed = |status: &str, detail: String| crate::output::DoctorRow {
        component: String::from("pkg profile"),
        status: status.to_owned(),
        detail: Some(detail),
    };
    match std::fs::symlink_metadata(&paths.profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(observed(
            "fresh",
            format!(
                "{} not created yet; the first install creates it",
                paths.profile.display()
            ),
        )),
        Err(error) => Ok(observed("unreadable", error.to_string())),
        Ok(_) => match entries(nix, &paths.profile) {
            Ok(entries) => Ok(observed(
                "ok",
                format!("{} ({} entries)", paths.profile.display(), entries.len()),
            )),
            Err(error) if error.signal().is_some() => Err(error.into()),
            Err(error) => Ok(observed("unreadable", error.to_string())),
        },
    }
}
