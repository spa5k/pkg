//! Package changes between two profile states, with names and versions.
//!
//! Versions come from store path names, so no Nix command runs.

use std::collections::BTreeMap;

use crate::nix::ProfileEntry;
use crate::output::{self, Stream, Tone};

/// One package change between two profile states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    /// A package that only the newer state has.
    Added(Package),
    /// A package that only the older state has.
    Removed(Package),
    /// A package with other outputs in the newer state.
    Changed {
        /// The display name.
        name: String,
        /// The older version, when known.
        from: Option<String>,
        /// The newer version, when known.
        to: Option<String>,
    },
}

/// A package name and its version, when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Package {
    /// The display name.
    pub(super) name: String,
    /// The version from the store path name.
    pub(super) version: Option<String>,
}

impl Package {
    pub(super) fn of(entry: &ProfileEntry, name: String) -> Self {
        Self {
            name,
            version: version(entry),
        }
    }

    /// `ripgrep 15.2.0`, or the name alone when no version is known.
    pub(super) fn label(&self) -> String {
        self.version.as_ref().map_or_else(
            || self.name.clone(),
            |version| format!("{} {version}", self.name),
        )
    }
}

impl Change {
    /// The display name of the changed package.
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Added(package) | Self::Removed(package) => &package.name,
            Self::Changed { name, .. } => name,
        }
    }

    /// One result line, such as `Upgraded ripgrep 15.1.0 → 15.2.0`.
    ///
    /// `verbs` names added, removed, and changed packages.
    pub(super) fn line(&self, verbs: [&str; 3]) -> String {
        let [added, removed, changed] = verbs;
        let (verb, text) = match self {
            Self::Added(package) => (added, package.label()),
            Self::Removed(package) => (removed, package.label()),
            Self::Changed { name, from, to } => (
                changed,
                format!("{name} {}", transition(from.as_deref(), to.as_deref())),
            ),
        };
        format!("{} {text}", output::paint(Stream::Out, Tone::Success, verb))
    }

    /// A compact history cell, such as `+hello 2.12.1`.
    pub(super) fn compact(&self) -> String {
        match self {
            Self::Added(package) => format!("+{}", package.label()),
            Self::Removed(package) => format!("-{}", package.label()),
            Self::Changed { name, from, to } => {
                format!("{name} {}", transition(from.as_deref(), to.as_deref()))
            }
        }
    }
}

/// `15.1.0 → 15.2.0`, or `15.2.0 (new build)` when the version is the same.
fn transition(from: Option<&str>, to: Option<&str>) -> String {
    match (from, to) {
        (Some(from), Some(to)) if from == to => format!("{to} (new build)"),
        (Some(from), Some(to)) => format!("{from} → {to}"),
        (None, Some(to)) => format!("→ {to}"),
        (Some(from), None) => format!("{from} → new build"),
        (None, None) => String::from("(new build)"),
    }
}

/// The package changes from `before` to `after`, sorted by name.
///
/// Entries are matched by their native entry ID. An entry with the same
/// store paths did not change, even when its locked source moved.
pub(super) fn diff(
    before: &BTreeMap<String, ProfileEntry>,
    after: &BTreeMap<String, ProfileEntry>,
    name: impl Fn(&ProfileEntry) -> String,
) -> Vec<Change> {
    let active = |entries: &BTreeMap<String, ProfileEntry>, key: &str| {
        entries.get(key).filter(|entry| entry.active).cloned()
    };
    let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut changes: Vec<Change> = keys
        .into_iter()
        .filter_map(|key| match (active(before, key), active(after, key)) {
            (None, Some(entry)) => Some(Change::Added(Package::of(&entry, name(&entry)))),
            (Some(entry), None) => Some(Change::Removed(Package::of(&entry, name(&entry)))),
            (Some(old), Some(new)) if sorted(&old.store_paths) != sorted(&new.store_paths) => {
                Some(Change::Changed {
                    name: name(&new),
                    from: version(&old),
                    to: version(&new),
                })
            }
            _ => None,
        })
        .collect();
    changes.sort_by(|left, right| left.name().cmp(right.name()));
    changes
}

fn sorted(paths: &[String]) -> Vec<&str> {
    let mut paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    paths.sort_unstable();
    paths
}

/// Output names that store paths can end with, such as `jq-1.8.2-man`.
const OUTPUTS: &[&str] = &[
    "bin", "man", "dev", "lib", "doc", "info", "out", "debug", "devdoc", "devman", "static",
];

/// The version of an entry, read from its store path names.
///
/// `/nix/store/<hash>-ripgrep-15.2.0` gives `15.2.0`. Cask names
/// (`cask-stats-<12 hex>-3.0.13`) drop the catalog hash. A name without a
/// version, such as `helix-term`, gives `None`.
pub(super) fn version(entry: &ProfileEntry) -> Option<String> {
    entry
        .store_paths
        .iter()
        .find_map(|path| store_version(path))
}

fn store_version(path: &str) -> Option<String> {
    let base = path.rsplit('/').next()?;
    let (hash, name) = base.split_once('-')?;
    if hash.len() != 32 {
        return None;
    }
    let mut version = parse_version(name)?;
    // Cask packages put a 12-character catalog hash before the version.
    if let Some((prefix, rest)) = version.split_once('-')
        && prefix.len() == 12
        && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        version = rest;
    }
    if let Some((rest, output)) = version.rsplit_once('-')
        && OUTPUTS.contains(&output)
    {
        version = rest;
    }
    (!version.is_empty()).then(|| version.to_string())
}

/// The version part of a derivation name, by Nix's rule: everything after
/// the first dash that is not followed by a letter.
fn parse_version(name: &str) -> Option<&str> {
    let bytes = name.as_bytes();
    (0..bytes.len().saturating_sub(1))
        .find(|&index| bytes[index] == b'-' && !bytes[index + 1].is_ascii_alphabetic())
        .map(|index| &name[index + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(paths: &[&str]) -> ProfileEntry {
        ProfileEntry {
            active: true,
            attr_path: String::from("legacyPackages.aarch64-darwin.x"),
            original_url: String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
            locked_url: String::from(
                "github:NixOS/nixpkgs/0000000000000000000000000000000000000000",
            ),
            store_paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    const HASH: &str = "n65zrzg7x0kv5mxr1y4wg0xsjmkiy4z4";

    #[test]
    fn versions_come_from_store_path_names() {
        for (name, expected) in [
            ("ripgrep-15.2.0", Some("15.2.0")),
            ("jq-1.8.2-man", Some("1.8.2")),
            ("jq-1.8.2-bin", Some("1.8.2")),
            (
                "bat-extras-2024.08.24-unstable-2025-02-22",
                Some("2024.08.24-unstable-2025-02-22"),
            ),
            ("cask-stats-34fcbb13d1c4-3.0.13", Some("3.0.13")),
            (
                "cask-visual-studio-code-a4fcbb13d1c4-1.134.0",
                Some("1.134.0"),
            ),
            ("formula-python@3.15-3.15.0rc2", Some("3.15.0rc2")),
            ("helix-term", None),
        ] {
            let path = format!("/nix/store/{HASH}-{name}");
            assert_eq!(store_version(&path).as_deref(), expected, "{name}");
        }
    }

    #[test]
    fn diffs_report_added_removed_and_changed_packages_by_name() {
        let before = BTreeMap::from([
            (
                String::from("bat"),
                entry(&["/nix/store/n65zrzg7x0kv5mxr1y4wg0xsjmkiy4z4-bat-0.26.1"]),
            ),
            (
                String::from("ripgrep"),
                entry(&["/nix/store/lw5h7x6kxqdx6m8wnsqjjzj6fd1i9x2j-ripgrep-15.1.0"]),
            ),
            (
                String::from("jq"),
                entry(&["/nix/store/wm7v59a2lhj1w2k8jz53spcs1iizm9lv-jq-1.8.2-bin"]),
            ),
        ]);
        let mut after = before.clone();
        after.remove("bat");
        after.insert(
            String::from("ripgrep"),
            entry(&["/nix/store/aw5h7x6kxqdx6m8wnsqjjzj6fd1i9x2j-ripgrep-15.2.0"]),
        );
        after.insert(
            String::from("hello"),
            entry(&["/nix/store/bw5h7x6kxqdx6m8wnsqjjzj6fd1i9x2j-hello-2.12.1"]),
        );
        // A moved lock with the same outputs is not a package change.
        after.get_mut("jq").unwrap().locked_url = String::from("github:NixOS/nixpkgs/1111");
        let names = |entry: &ProfileEntry| store_name(&entry.store_paths[0]);
        let changes = diff(&before, &after, names);
        let compact: Vec<String> = changes.iter().map(Change::compact).collect();
        assert_eq!(
            compact,
            ["-bat 0.26.1", "+hello 2.12.1", "ripgrep 15.1.0 → 15.2.0"]
        );
        assert_eq!(
            changes[2].line(["Installed", "Removed", "Upgraded"]),
            "Upgraded ripgrep 15.1.0 → 15.2.0"
        );
    }

    fn store_name(path: &str) -> String {
        let base = path.rsplit('/').next().unwrap();
        let name = base.split_once('-').unwrap().1;
        let version = parse_version(name).unwrap_or("");
        name.strip_suffix(version)
            .unwrap_or(name)
            .trim_end_matches('-')
            .to_string()
    }

    #[test]
    fn same_version_with_new_outputs_is_a_new_build() {
        assert_eq!(transition(Some("1.0"), Some("1.0")), "1.0 (new build)");
    }
}
