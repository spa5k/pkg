//! Package-level reporting of native profile file conflicts.

use std::collections::BTreeMap;

use crate::nix::{NixError, ProfileEntry};

use super::Session;

pub(super) fn report(
    session: &Session,
    error: &NixError,
    entries: Option<&BTreeMap<String, ProfileEntry>>,
    requested: Option<&[String]>,
) -> bool {
    let Some(conflict) = error.profile_conflict() else {
        return false;
    };
    let owners: Vec<_> = entries
        .into_iter()
        .flat_map(|entries| entries.iter())
        .filter(|(_, entry)| {
            entry.active
                && entry.store_paths.iter().any(|path| {
                    conflict
                        .existing_file
                        .strip_prefix(path)
                        .is_ok_and(|file| file == conflict.file)
                })
        })
        .map(|(key, entry)| {
            (
                key,
                package_label(&crate::installed::package_id(
                    entry,
                    &session.config,
                    &session.paths.state_home,
                )),
            )
        })
        .collect();
    let file = conflict.file.display();
    match owners.as_slice() {
        [(_, id)] => eprintln!("Cause: {id} is already installed and provides {file}."),
        [] => eprintln!("Cause: two packages provide {file}."),
        many => {
            eprintln!("Cause: installed packages already provide {file}:");
            for (key, id) in many {
                eprintln!("  {id} (entry: {key})");
            }
        }
    }
    if let Some(requested) = requested {
        let labels: Vec<_> = requested.iter().map(|id| package_label(id)).collect();
        eprintln!("Requested: {}.", labels.join(", "));
    }
    true
}

fn package_label(id: &str) -> String {
    let source = if id.starts_with("nixpkgs:") {
        "Nixpkgs"
    } else if id.starts_with("brew:") {
        "Homebrew Formula"
    } else if id.starts_with("cask:") {
        "Homebrew Cask"
    } else {
        return id.to_string();
    };
    format!("{id} ({source})")
}
