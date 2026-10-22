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
        print!("{}", output::render_list(&rows, cli.verbose));
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
            version: super::summary::version(entry),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}

pub fn history(cli: &Cli) -> Result<(), CommandError> {
    let session = session(cli)?;
    if cli.verbose && !cli.json {
        let text = session.nix.profile_history(&session.paths.profile)?;
        // `--no-color` holds for native text pkg passes through, even if
        // the runtime colorized it anyway.
        if output::color(output::Stream::Out) {
            print!("{text}");
        } else {
            print!("{}", output::strip_ansi(&text));
        }
        return Ok(());
    }
    let generations = generations(&session.paths.profile);
    let active = active_generation(&session.paths.profile);
    if let Some(message) = broken_profile(&session.paths.profile) {
        eprintln!("Warning: {message}");
    }
    let name = |entry: &ProfileEntry| {
        super::installed::display_name(entry, &session.config, &session.paths.state_home)
    };
    if cli.json {
        return print_json(&output::envelope(
            "history",
            history_rows(&generations, active, name),
        ));
    }
    if generations.is_empty() {
        println!("No changes yet.");
        println!(
            "Next: {}",
            output::paint(
                output::Stream::Out,
                output::Tone::Accent,
                "pkg install NAME"
            )
        );
        return Ok(());
    }
    print!("{}", render_history(&generations, active, name));
    Ok(())
}

/// History rows for JSON, newest first. `changes` is `null` when a
/// manifest cannot be read.
fn history_rows(
    generations: &[Generation],
    active: Option<u64>,
    name: impl Fn(&ProfileEntry) -> String,
) -> Vec<serde_json::Value> {
    let empty = BTreeMap::new();
    let mut previous = Some(&empty);
    let mut rows: Vec<serde_json::Value> = generations
        .iter()
        .map(|generation| {
            let changes = previous
                .zip(generation.entries.as_ref())
                .map(|(before, after)| {
                    super::summary::diff(before, after, &name)
                        .iter()
                        .map(super::summary::Change::compact)
                        .collect::<Vec<_>>()
                });
            previous = generation.entries.as_ref();
            serde_json::json!({
                "generation": generation.number,
                "current": Some(generation.number) == active,
                "created": generation.created.and_then(|time| {
                    time.duration_since(std::time::UNIX_EPOCH).ok().map(|age| age.as_secs())
                }),
                "changes": changes,
            })
        })
        .collect();
    rows.reverse();
    rows
}

/// One retained profile generation, read from local files.
struct Generation {
    number: u64,
    created: Option<std::time::SystemTime>,
    /// The generation's entries; `None` when the manifest is unreadable.
    entries: Option<BTreeMap<String, ProfileEntry>>,
}

/// The decoded part of one generation's `manifest.json`.
///
/// Manifest version 3 keys `elements` by entry ID. Older versions use a
/// list and fail to decode, so their changes show as unknown.
#[derive(serde::Deserialize)]
struct Manifest {
    elements: BTreeMap<String, ProfileEntry>,
}

/// Read the retained generations of `profile`, oldest first.
///
/// Generations are the `<name>-<N>-link` links next to the profile link.
/// No Nix command runs.
fn generations(profile: &std::path::Path) -> Vec<Generation> {
    let (Some(directory), Some(name)) = (
        profile.parent(),
        profile.file_name().and_then(|name| name.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(files) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let prefix = format!("{name}-");
    let mut generations: Vec<Generation> = files
        .flatten()
        .filter_map(|file| {
            let file_name = file.file_name();
            let number = file_name
                .to_str()?
                .strip_prefix(&prefix)?
                .strip_suffix("-link")?
                .parse()
                .ok()?;
            let link = file.path();
            let created = std::fs::symlink_metadata(&link)
                .and_then(|metadata| metadata.modified())
                .ok();
            let entries = std::fs::read(link.join("manifest.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
                .map(|manifest| manifest.elements);
            Some(Generation {
                number,
                created,
                entries,
            })
        })
        .collect();
    generations.sort_by_key(|generation| generation.number);
    generations
}

/// The generation that `pkg rollback` without a number selects.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UndoTarget {
    /// The newest older generation with other packages.
    Generation(u64),
    /// Every older generation has the same packages.
    Nothing,
    /// The generations cannot be read; Nix selects the previous one.
    Unknown,
}

/// Find the newest generation before the active one whose packages differ.
///
/// A generation in which only the source revision moved has the same
/// packages, so undoing it would change nothing.
pub(super) fn undo_target(profile: &std::path::Path) -> UndoTarget {
    let Some(active) = active_generation(profile) else {
        return UndoTarget::Unknown;
    };
    undo_from(&generations(profile), active)
}

fn undo_from(generations: &[Generation], active: u64) -> UndoTarget {
    let Some(current) = generations
        .iter()
        .find(|generation| generation.number == active)
        .and_then(|generation| generation.entries.as_ref())
    else {
        return UndoTarget::Unknown;
    };
    for generation in generations
        .iter()
        .rev()
        .filter(|generation| generation.number < active)
    {
        let Some(entries) = &generation.entries else {
            return UndoTarget::Generation(generation.number);
        };
        if !super::summary::diff(current, entries, ProfileEntry::name).is_empty() {
            return UndoTarget::Generation(generation.number);
        }
    }
    UndoTarget::Nothing
}

/// The generation numbers of the profile, oldest first. Empty when the
/// profile directory cannot be read.
pub(super) fn generation_numbers(profile: &std::path::Path) -> Vec<u64> {
    generations(profile)
        .iter()
        .map(|generation| generation.number)
        .collect()
}

/// Read the active generation from the native profile link when available.
pub(super) fn active_generation(profile: &std::path::Path) -> Option<u64> {
    let link = std::fs::read_link(profile).ok()?;
    let name = link.file_name()?.to_str()?;
    name.strip_suffix("-link")?.rsplit('-').next()?.parse().ok()
}

/// Show generations newest first, with the package changes of each.
fn render_history(
    generations: &[Generation],
    active: Option<u64>,
    name: impl Fn(&ProfileEntry) -> String,
) -> String {
    let empty = BTreeMap::new();
    let mut table = output::Table::new(&["", "GEN", "DATE", "CHANGES"]);
    let mut rows = Vec::new();
    let mut previous = Some(&empty);
    for generation in generations {
        let changes = match (previous, generation.entries.as_ref()) {
            (Some(before), Some(after)) => {
                let changes = super::summary::diff(before, after, &name);
                if changes.is_empty() {
                    output::paint(
                        output::Stream::Out,
                        output::Tone::Muted,
                        "no package changes",
                    )
                } else {
                    counted(changes.iter().map(super::summary::Change::compact))
                }
            }
            _ => output::paint(output::Stream::Out, output::Tone::Muted, "unknown"),
        };
        previous = generation.entries.as_ref();
        let current = Some(generation.number) == active;
        rows.push(vec![
            if current {
                output::paint(output::Stream::Out, output::Tone::Success, "*")
            } else {
                String::new()
            },
            generation.number.to_string(),
            generation.created.map(local_time).unwrap_or_default(),
            changes,
        ]);
    }
    for row in rows.into_iter().rev() {
        table.row(row);
    }
    let mut out = table.render(output::Stream::Out);
    if active.is_some_and(|active| generations.iter().any(|row| row.number == active)) {
        out.push_str("* current generation\n");
    }
    out
}

/// Join cells with commas. Repeated cells, such as duplicate entries of one
/// package, show once with a count: `+stats 3.0.13 (2×)`.
fn counted(cells: impl Iterator<Item = String>) -> String {
    let mut unique: Vec<(String, usize)> = Vec::new();
    for cell in cells {
        match unique.iter_mut().find(|(seen, _)| *seen == cell) {
            Some((_, count)) => *count += 1,
            None => unique.push((cell, 1)),
        }
    }
    unique
        .into_iter()
        .map(|(cell, count)| {
            if count == 1 {
                cell
            } else {
                format!("{cell} ({count}×)")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format a time as local `YYYY-MM-DD HH:MM`.
fn local_time(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let Ok(seconds) = libc::time_t::try_from(seconds) else {
        return String::new();
    };
    // SAFETY: `tm` is a plain C struct; all-zero bytes are a valid value
    // (a null `tm_zone` pointer on macOS). `localtime_r` writes only into
    // the valid pointer and reads the valid `seconds` value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&raw const seconds, &raw mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// Read installed entries, treating a not-yet-created profile as empty.
///
/// A fresh install has no profile on disk; that state is empty, not an error.
pub fn installed_entries(session: &Session) -> Result<BTreeMap<String, ProfileEntry>, NixError> {
    Ok(installed_state(session)?
        .map_or_else(BTreeMap::new, crate::nix::CheckedProfile::into_entries))
}

/// Why the current generation cannot be read, when its link is broken.
///
/// Nix reads a profile link to a missing generation, or to a deleted store
/// path, as an empty profile. A change would then remove every package, so
/// pkg stops and names the way back.
pub fn broken_profile(profile: &std::path::Path) -> Option<String> {
    let link = std::fs::symlink_metadata(profile).ok()?;
    if !link.file_type().is_symlink() || std::fs::metadata(profile).is_ok() {
        return None;
    }
    let active = active_generation(profile);
    let name = profile.file_name()?.to_str()?;
    let readable = generations(profile)
        .into_iter()
        .rev()
        .map(|generation| generation.number)
        .find(|number| {
            Some(*number) != active
                && std::fs::metadata(profile.with_file_name(format!("{name}-{number}-link")))
                    .is_ok()
        });
    let current = active.map_or_else(String::new, |number| format!(" {number}"));
    Some(match readable {
        Some(number) => format!(
            "the current generation{current} is missing. Run `pkg rollback {number}` to go back to generation {number}."
        ),
        None => format!(
            "the current generation{current} is missing. Run `pkg history` to see the generations."
        ),
    })
}

pub(super) fn installed_state(
    session: &Session,
) -> Result<Option<crate::nix::CheckedProfile<'_>>, NixError> {
    if let Some(message) = broken_profile(&session.paths.profile) {
        return Err(NixError::Io {
            what: "the package profile",
            source: std::io::Error::other(message),
        });
    }
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
    if let Err(error) = crate::profile_lock::ProfileLock::check(&paths.profile) {
        return Ok(observed("unreadable", error));
    }
    match std::fs::symlink_metadata(&paths.profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(observed(
            "fresh",
            format!(
                "{} not created yet; the first install creates it",
                paths.profile.display()
            ),
        )),
        Err(error) => Ok(observed("unreadable", error.to_string())),
        Ok(_) if let Some(message) = broken_profile(&paths.profile) => {
            Ok(observed("unreadable", message))
        }
        Ok(_) => match entries(nix, &paths.profile) {
            Ok(entries) => Ok(observed(
                "ok",
                format!(
                    "{} ({} {})",
                    paths.profile.display(),
                    entries.len(),
                    if entries.len() == 1 {
                        "entry"
                    } else {
                        "entries"
                    }
                ),
            )),
            Err(error) if error.signal().is_some() => Err(error.into()),
            Err(error) => Ok(observed("unreadable", error.to_string())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(attribute: &str, source: &str, path: &str) -> ProfileEntry {
        ProfileEntry {
            active: true,
            attr_path: attribute.to_string(),
            original_url: source.to_string(),
            locked_url: format!("{source}/1111111111111111111111111111111111111111"),
            store_paths: vec![format!(
                "/nix/store/n65zrzg7x0kv5mxr1y4wg0xsjmkiy4z4-{path}"
            )],
        }
    }

    #[test]
    fn list_shows_names_versions_and_sources_and_entry_ids_only_for_duplicates() {
        let config = crate::config::Config::default();
        let state_home = std::path::Path::new("/tmp/pkg-list-test-state");
        let stats = entry(
            "packages.aarch64-darwin.homebrew/cask/stats",
            &config.sources.casks,
            "cask-stats-34fcbb13d1c4-3.0.13",
        );
        let mut entries = BTreeMap::from([
            (
                String::from("ripgrep"),
                entry(
                    "legacyPackages.aarch64-darwin.ripgrep",
                    &config.sources.nixpkgs,
                    "ripgrep-15.2.0",
                ),
            ),
            (
                String::from("helix"),
                entry(
                    "packages.aarch64-darwin.helix",
                    "github:helix-editor/helix",
                    "helix-term",
                ),
            ),
            (String::from("nix/casks"), stats.clone()),
        ]);
        let text = output::render_list(&list_rows(&entries, &config, state_home), false);
        assert_eq!(
            text,
            "NAME     VERSION  SOURCE\n\
             helix             github:helix-editor/helix\n\
             ripgrep  15.2.0\n\
             stats    3.0.13   cask\n"
        );
        entries.insert(String::from("nix/casks-1"), stats);
        let text = output::render_list(&list_rows(&entries, &config, state_home), false);
        assert!(
            text.starts_with("NAME     VERSION  SOURCE                     ENTRY ID\n"),
            "{text}"
        );
        assert!(
            text.contains("stats    3.0.13   cask                       nix/casks-1\n"),
            "{text}"
        );
    }

    #[test]
    fn undo_skips_generations_with_the_same_packages() {
        let source = crate::config::Config::default().sources.nixpkgs;
        let hello = entry("legacyPackages.x86_64-linux.hello", &source, "hello-2.12.1");
        let mut moved = hello.clone();
        moved.locked_url = format!("{source}/2222222222222222222222222222222222222222");
        let generation = |number, entries: &[(&str, &ProfileEntry)]| Generation {
            number,
            created: None,
            entries: Some(
                entries
                    .iter()
                    .map(|(key, entry)| ((*key).to_string(), (*entry).clone()))
                    .collect(),
            ),
        };
        let generations = [
            generation(1, &[]),
            generation(2, &[("hello", &hello)]),
            generation(3, &[("hello", &moved)]),
        ];
        assert_eq!(undo_from(&generations, 3), UndoTarget::Generation(1));
        assert_eq!(undo_from(&generations, 2), UndoTarget::Generation(1));
        assert_eq!(undo_from(&generations[1..], 3), UndoTarget::Nothing);
        assert_eq!(undo_from(&generations, 9), UndoTarget::Unknown);
    }

    #[test]
    fn history_shows_changes_newest_first_and_marks_the_current_generation() {
        let source = crate::config::Config::default().sources.nixpkgs;
        let hello = entry("legacyPackages.x86_64-linux.hello", &source, "hello-2.12.1");
        let stats = entry("legacyPackages.x86_64-linux.stats", &source, "stats-3.0.13");
        let generations = [
            Generation {
                number: 1,
                created: None,
                entries: Some(BTreeMap::from([(String::from("hello"), hello)])),
            },
            Generation {
                number: 2,
                created: None,
                entries: Some(BTreeMap::from([
                    (String::from("stats"), stats.clone()),
                    (String::from("stats-1"), stats),
                ])),
            },
            Generation {
                number: 3,
                created: None,
                entries: None,
            },
        ];
        let name = |entry: &ProfileEntry| entry.name();
        assert_eq!(
            render_history(&generations, Some(2), name),
            "   GEN  DATE  CHANGES\n\
             \x20  3          unknown\n\
             *  2          -hello 2.12.1, +stats 3.0.13 (2×)\n\
             \x20  1          +hello 2.12.1\n\
             * current generation\n"
        );
    }
}
