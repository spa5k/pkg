//! Package identities and selectors derived from the authoritative profile.

use std::collections::{BTreeMap, BTreeSet};

use crate::{catalog, config::Config, nix::ProfileEntry};

pub fn package_id(entry: &ProfileEntry, config: &Config, state_home: &std::path::Path) -> String {
    let mut parts = entry.attr_path.splitn(3, '.');
    let root = parts.next().unwrap_or_default();
    let system = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    let standard = matches!(root, "packages" | "legacyPackages") && !tail.is_empty();
    if catalog::canonical_source(&entry.original_url)
        == catalog::canonical_source(&config.sources.nixpkgs)
    {
        return format!("nixpkgs:{}", if standard { tail } else { &entry.attr_path });
    }
    if root == "packages"
        && let Ok(full) = cask_catalog::decode_package_attribute(tail)
        && let Some((source, _)) = crate::nix::split_entry_id(&full)
        && (catalog::canonical_source(&entry.original_url)
            == catalog::canonical_source(&config.sources.casks)
            || crate::tap::store::source_of_reference(state_home, &entry.original_url).as_deref()
                == Some(source.as_str()))
        && catalog::native_package_attribute(system, &full) == entry.attr_path
    {
        return format!("cask:{full}");
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
                    || id.strip_prefix("cask:") == Some(request.as_str())
                    || request.strip_prefix("cask:").is_some_and(|token| {
                        id.strip_prefix("cask:")
                            .and_then(|full| full.rsplit('/').next())
                            == Some(token)
                    })
            })
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(format!(
                    "`{request}` is not installed. Run `pkg list` for package IDs."
                ));
            }
            [(key, _)] => {
                selected.insert((*key).clone());
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
        entries.insert(String::from("nix/casks-1"), entries["nix/casks"].clone());
        let error = select(
            &entries,
            &config,
            state,
            &[String::from("rectangle")],
            false,
        )
        .unwrap_err();
        assert!(error.contains("--entry nix/casks-1"), "{error}");
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
        let foreign = entry(
            "packages.aarch64-darwin.homebrew/cask/rectangle",
            "github:other/project",
        );
        assert!(package_id(&foreign, &config, state).starts_with("github:other/project#"));
    }
}
