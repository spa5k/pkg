//! Installed package output from the native profile.
use super::{CommandError, installed_entries, print_json, session};
use crate::cli::Cli;
use crate::nix::ProfileEntry;
use crate::output::{self, ListRow};

pub(super) fn run(cli: &Cli) -> Result<(), CommandError> {
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
            package_id: crate::installed::package_id(entry, config, state_home),
            name: entry.name(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}
