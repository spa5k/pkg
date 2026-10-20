//! Package identities, commands, and selectors derived from the native profile.

use std::collections::{BTreeMap, BTreeSet};

use crate::{catalog, config::Config, nix::ProfileEntry};

/// Executables exposed by this active entry in the current profile.
pub fn commands(entry: &ProfileEntry, profile: &std::path::Path) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut names = BTreeSet::new();
    if !entry.active {
        return Vec::new();
    }
    for output in &entry.store_paths {
        let Ok(files) = std::fs::read_dir(std::path::Path::new(output).join("bin")) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.')
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.+".contains(c))
            {
                continue;
            }
            let path = file.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
            if let Ok(target) = std::fs::canonicalize(&path)
                && std::fs::canonicalize(profile.join("bin").join(name))
                    .ok()
                    .as_ref()
                    == Some(&target)
            {
                names.insert(name.to_string());
            }
        }
    }
    names.into_iter().collect()
}

/// Whether an active entry has an executable `bin/COMMAND`.
fn provides(entry: &ProfileEntry, command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    !command.is_empty()
        && !command.starts_with('.')
        && command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.+".contains(c))
        && entry.store_paths.iter().any(|output| {
            std::fs::metadata(std::path::Path::new(output).join("bin").join(command)).is_ok_and(
                |metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
            )
        })
}

/// The short name that the user types for this entry, such as `ripgrep`
/// or `stats`.
pub fn display_name(entry: &ProfileEntry, config: &Config, state_home: &std::path::Path) -> String {
    crate::output::short_package_name(&package_id(entry, config, state_home))
}

pub fn package_id(entry: &ProfileEntry, config: &Config, state_home: &std::path::Path) -> String {
    let mut parts = entry.attr_path.splitn(3, '.');
    let root = parts.next().unwrap_or_default();
    let system = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    let standard = matches!(root, "packages" | "legacyPackages") && !tail.is_empty();
    let original_source = catalog::canonical_source(&entry.original_url);
    if original_source == catalog::canonical_source(&config.sources.nixpkgs) {
        return format!("nixpkgs:{}", if standard { tail } else { &entry.attr_path });
    }
    if root == "packages"
        && let Ok(full) = cask_catalog::decode_package_attribute(tail)
    {
        if full.starts_with("homebrew/core/")
            && original_source == catalog::canonical_source(&config.sources.formulae)
            && catalog::native_package_attribute(system, &full) == entry.attr_path
        {
            return format!("brew:{full}");
        }
        if let Some((source, _)) = crate::nix::split_entry_id(&full)
            && (original_source == catalog::canonical_source(&config.sources.casks)
                || crate::tap::store::source_of_reference(state_home, &entry.original_url)
                    .as_deref()
                    == Some(source))
            && catalog::native_package_attribute(system, &full) == entry.attr_path
        {
            return format!("cask:{full}");
        }
    }
    format!("{}#{}", entry.original_url, entry.attr_path)
}

pub fn select(
    entries: &BTreeMap<String, ProfileEntry>,
    config: &Config,
    state_home: &std::path::Path,
    requests: &[String],
    exact_entry: bool,
) -> Result<Vec<String>, String> {
    let mut selected = BTreeSet::new();
    for request in requests {
        let matches: Vec<_> = entries
            .iter()
            .filter(|(key, entry)| {
                if !entry.active {
                    return false;
                }
                if exact_entry {
                    return *key == request;
                }
                let id = package_id(entry, config, state_home);
                let name = entry.name();
                *key == request
                    || id == *request
                    || id.starts_with("nixpkgs:")
                        && *request == format!("nixpkgs:{}", entry.attr_path)
                    || name == *request
                    || name.rsplit('/').next() == Some(request.as_str())
                    || id.strip_prefix("nixpkgs:") == Some(request.as_str())
                    || request.strip_prefix("brew:").is_some_and(|name| {
                        id.strip_prefix("brew:homebrew/core/")
                            == Some(name.strip_prefix("homebrew/core/").unwrap_or(name))
                    })
                    || id.strip_prefix("cask:") == Some(request.as_str())
                    || request.strip_prefix("cask:").is_some_and(|token| {
                        id.strip_prefix("cask:")
                            .and_then(|full| full.rsplit('/').next())
                            == Some(token)
                    })
            })
            .collect();
        // A command name selects the one installed package that provides
        // it, so `pkg remove rg` removes ripgrep.
        let matches = if matches.is_empty() && !exact_entry {
            entries
                .iter()
                .filter(|(_, entry)| entry.active && provides(entry, request))
                .collect()
        } else {
            matches
        };
        match matches.as_slice() {
            [] => {
                return Err(format!(
                    "`{request}` is not installed. Run `pkg list` to see installed packages."
                ));
            }
            [(key, _)] => {
                selected.insert((*key).clone());
            }
            // Several entries of the same package from the same source are
            // one package for the user. Select all of them.
            [(_, first), rest @ ..]
                if rest.iter().all(|(_, entry)| {
                    entry.original_url == first.original_url && entry.attr_path == first.attr_path
                }) =>
            {
                selected.extend(matches.iter().map(|(key, _)| (*key).clone()));
            }
            many => {
                let choices = many
                    .iter()
                    .map(|(key, entry)| {
                        format!(
                            "{} (--entry {key}, revision {})",
                            package_id(entry, config, state_home),
                            entry.locked_revision().unwrap_or("unknown")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n  ");
                return Err(format!(
                    "`{request}` matches several installed entries. Select an exact entry:\n  {choices}"
                ));
            }
        }
    }
    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(attribute: &str, source: &str) -> ProfileEntry {
        ProfileEntry {
            active: true,
            attr_path: attribute.to_string(),
            original_url: source.to_string(),
            locked_url: String::from(
                "github:spa5k/pkg/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa?dir=nix/casks",
            ),
            store_paths: Vec::new(),
        }
    }

    #[test]
    fn a_command_name_selects_the_one_package_that_provides_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let config = Config::default();
        let state = Path::new("/tmp/pkg-selector-test-state");
        let store = tempfile::tempdir().unwrap();
        let output = store.path().join("ripgrep-15.2.0");
        std::fs::create_dir_all(output.join("bin")).unwrap();
        std::fs::write(output.join("bin/rg"), "").unwrap();
        std::fs::set_permissions(
            output.join("bin/rg"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let mut ripgrep = entry(
            "legacyPackages.aarch64-darwin.ripgrep",
            &config.sources.nixpkgs,
        );
        ripgrep.store_paths = vec![output.display().to_string()];
        let entries = BTreeMap::from([
            (String::from("ripgrep"), ripgrep),
            (
                String::from("jq"),
                entry("legacyPackages.aarch64-darwin.jq", &config.sources.nixpkgs),
            ),
        ]);
        assert_eq!(
            select(&entries, &config, state, &[String::from("rg")], false).unwrap(),
            ["ripgrep"]
        );
        // An exact entry ID never falls back to a command name.
        assert!(select(&entries, &config, state, &[String::from("rg")], true).is_err());
        assert!(select(&entries, &config, state, &[String::from("fd")], false).is_err());
    }

    #[test]
    fn commands_must_be_executable_and_exposed_by_this_entry() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(output.join("bin/directory")).unwrap();
        std::fs::create_dir_all(profile.join("bin")).unwrap();
        for name in ["fff", ".fff-wrapped", "shadowed", "text", "missing"] {
            let path = output.join("bin").join(name);
            std::fs::write(&path, "fixture").unwrap();
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(if name == "text" { 0o644 } else { 0o755 }),
            )
            .unwrap();
        }
        symlink(output.join("bin/fff"), profile.join("bin/fff")).unwrap();
        symlink(
            output.join("bin/.fff-wrapped"),
            profile.join("bin/.fff-wrapped"),
        )
        .unwrap();
        symlink(output.join("bin/text"), profile.join("bin/text")).unwrap();
        symlink(output.join("bin/fff"), profile.join("bin/shadowed")).unwrap();
        symlink(output.join("bin/absent"), output.join("bin/dangling")).unwrap();
        let mut selected = entry("legacyPackages.aarch64-darwin.fff", "github:NixOS/nixpkgs");
        selected.store_paths = vec![output.to_string_lossy().into_owned()];
        assert_eq!(commands(&selected, &profile), ["fff"]);
        selected.active = false;
        assert_eq!(commands(&selected, &profile), [] as [String; 0]);
    }

    #[test]
    fn installed_names_resolve_and_duplicates_require_explicit_entries() {
        let config = Config::default();
        let state = Path::new("/tmp/pkg-selector-test-state");
        let mut entries = BTreeMap::from([
            (
                String::from("nix/casks"),
                entry(
                    "packages.aarch64-darwin.homebrew/cask/rectangle",
                    &config.sources.casks,
                ),
            ),
            (
                String::from("python"),
                entry(
                    "legacyPackages.aarch64-darwin.python3Packages.requests",
                    &config.sources.nixpkgs,
                ),
            ),
        ]);
        for name in [
            "rectangle",
            "cask:rectangle",
            "cask:homebrew/cask/rectangle",
            "homebrew/cask/rectangle",
        ] {
            assert_eq!(
                select(&entries, &config, state, &[name.to_string()], false).unwrap(),
                ["nix/casks"]
            );
        }
        assert_eq!(
            package_id(&entries["python"], &config, state),
            "nixpkgs:python3Packages.requests"
        );
        assert_eq!(
            select(
                &entries,
                &config,
                state,
                &[String::from("nixpkgs:python3Packages.requests")],
                false
            )
            .unwrap(),
            ["python"]
        );
        let foreign = entry(
            "packages.aarch64-darwin.homebrew/cask/rectangle",
            "github:other/project",
        );
        assert!(package_id(&foreign, &config, state).starts_with("github:other/project#"));

        let attribute =
            catalog::native_package_attribute("aarch64-darwin", "homebrew/core/probe_tool@2.1");
        entries.insert(
            String::from("formulae"),
            entry(&attribute, &config.sources.formulae),
        );
        assert_eq!(
            package_id(&entries["formulae"], &config, state),
            "brew:homebrew/core/probe_tool@2.1"
        );
        for name in ["brew:probe_tool@2.1", "brew:homebrew/core/probe_tool@2.1"] {
            assert_eq!(
                select(&entries, &config, state, &[name.to_string()], false).unwrap(),
                ["formulae"]
            );
        }
        let foreign = entry(&attribute, "github:other/project");
        assert!(package_id(&foreign, &config, state).starts_with("github:other/project#"));
    }

    #[test]
    fn duplicate_entries_of_one_package_select_together() {
        let config = Config::default();
        let state = Path::new("/tmp/pkg-selector-test-state");
        let rectangle = entry(
            "packages.aarch64-darwin.homebrew/cask/rectangle",
            &config.sources.casks,
        );
        let mut entries = BTreeMap::from([(String::from("nix/casks"), rectangle)]);
        // Two entries of one package from one source are one package.
        entries.insert(String::from("nix/casks-1"), entries["nix/casks"].clone());
        assert_eq!(
            select(
                &entries,
                &config,
                state,
                &[String::from("rectangle")],
                false
            )
            .unwrap(),
            ["nix/casks", "nix/casks-1"]
        );
        // The same name from another source needs an exact entry.
        entries.insert(
            String::from("rectangle"),
            entry("packages.aarch64-darwin.rectangle", "github:other/project"),
        );
        let error = select(
            &entries,
            &config,
            state,
            &[String::from("rectangle")],
            false,
        )
        .unwrap_err();
        assert!(error.contains("--entry nix/casks-1"), "{error}");
        assert!(error.contains("--entry rectangle"), "{error}");
        entries.remove("rectangle");
        assert_eq!(
            select(
                &entries,
                &config,
                state,
                &[String::from("nix/casks-1")],
                true
            )
            .unwrap(),
            ["nix/casks-1"]
        );
        entries.get_mut("nix/casks-1").unwrap().active = false;
        assert_eq!(
            select(
                &entries,
                &config,
                state,
                &[String::from("rectangle")],
                false
            )
            .unwrap(),
            ["nix/casks"]
        );
    }
}
