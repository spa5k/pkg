//! App bundles in active native profile outputs.
use super::payload::scan_restorable_apps;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
/// Collects intact bundles and declared private payloads from active outputs.
///
/// A bundle counts as intact when it is a directory whose name ends in
/// `.app` and that contains a `Contents` directory. Inactive profile
/// entries are skipped. Two active packages providing the same basename
/// are a hard error, because the launcher name would be ambiguous.
pub(super) fn scan_apps<'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a crate::nix::ProfileEntry)>,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut apps: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (entry_id, entry) in entries {
        if !entry.active {
            continue;
        }
        let mut store_paths = entry.store_paths.clone();
        store_paths.sort();
        for store_path in &store_paths {
            for (name, path) in scan_restorable_apps(Path::new(store_path))? {
                if let Some(existing) = apps.insert(name.clone(), path.clone())
                    && existing != path
                {
                    return Err(format!(
                        "two active packages provide {name}; remove one before syncing app launchers"
                    ));
                }
            }
            let applications = Path::new(store_path).join("Applications");
            let read = match fs::read_dir(&applications) {
                Ok(read) => read,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "could not read {} (profile entry {entry_id}): {e}",
                        applications.display()
                    ));
                }
            };
            for item in read {
                let item =
                    item.map_err(|e| format!("could not read {}: {e}", applications.display()))?;
                let path = item.path();
                let name = match item.file_name().into_string() {
                    Ok(name) => name,
                    Err(_) => continue,
                };
                if !name.ends_with(".app") || name == ".app" {
                    continue;
                }
                let is_bundle_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && path.join("Contents").is_dir();
                if !is_bundle_dir {
                    continue;
                }
                if let Some(existing) = apps.get(&name) {
                    if existing != &path {
                        return Err(format!(
                            "two active packages provide Applications/{name}: {} and {}; \
                             remove one before syncing app launchers",
                            existing.display(),
                            path.display()
                        ));
                    }
                    continue;
                }
                apps.insert(name, path);
            }
        }
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    const APP_MANIFEST: &str = "apps.json";

    #[test]
    fn private_cask_payload_is_discovered_without_executing_its_helper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path();
        let namespace = output
            .join("libexec/pkg-casks")
            .join(output.file_name().unwrap());
        let bundle = namespace.join("app-sources/Vendor.app");
        fs::create_dir_all(bundle.join("Contents")).expect("bundle");
        fs::write(namespace.join("prepare"), "must not run").expect("helper");
        let entry = crate::nix::ProfileEntry {
            active: true,
            attr_path: String::from("packages.aarch64-darwin.vendor"),
            original_url: String::new(),
            locked_url: String::new(),
            store_paths: vec![output.display().to_string()],
        };
        let entries = BTreeMap::from([(String::from("opaque-native-id"), entry)]);
        let manifest = |source: &str| {
            serde_json::json!({
                "schema": "pkg-cask-apps/2", "apps": {"Vendor.app": {"source": source}}
            })
        };
        fs::write(
            namespace.join(APP_MANIFEST),
            manifest(&format!(
                "libexec/pkg-casks/{}/app-sources/Vendor.app",
                output.file_name().unwrap().to_string_lossy()
            ))
            .to_string(),
        )
        .expect("manifest");
        assert_eq!(
            scan_apps(entries.iter()).expect("apps"),
            BTreeMap::from([(String::from("Vendor.app"), bundle)])
        );
        fs::write(
            namespace.join(APP_MANIFEST),
            manifest("../outside.app").to_string(),
        )
        .expect("invalid manifest");
        assert!(
            scan_apps(entries.iter())
                .expect_err("unsafe path")
                .contains("invalid cask app payload")
        );
        fs::write(
            namespace.join(APP_MANIFEST),
            serde_json::json!({"schema": "pkg-cask-apps/1", "apps": {}}).to_string(),
        )
        .expect("old schema");
        assert!(
            scan_apps(entries.iter())
                .expect_err("old schema")
                .contains("unsupported cask app data")
        );
    }
}
