//! Discovery and preparation of current generated private app payloads.
use super::storage::app_cache_root;
use crate::nix::{Outcome, run_direct_captured};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
/// Read generated app data without executing vendor code or writing state.
pub(super) fn scan_restorable_apps(output: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let namespace = output
        .join("libexec/pkg-casks")
        .join(output.file_name().ok_or("app output has no name")?);
    let path = namespace.join("apps.json");
    let metadata = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("could not inspect {}: {e}", path.display())),
        Ok(metadata) => metadata,
    };
    if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
        return Err(format!("invalid cask app data: {}", path.display()));
    }
    #[derive(serde::Deserialize)]
    struct AppData {
        source: String,
    }
    #[derive(serde::Deserialize)]
    struct Manifest {
        schema: String,
        apps: BTreeMap<String, AppData>,
    }
    let data: Manifest = serde_json::from_slice(
        &fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("invalid cask app data {}: {e}", path.display()))?;
    if data.schema != "pkg-cask-apps/2" || !namespace.join("prepare").is_file() {
        return Err(format!("unsupported cask app data: {}", path.display()));
    }
    let source_dir = format!(
        "libexec/pkg-casks/{}/app-sources",
        output
            .file_name()
            .ok_or("app output has no name")?
            .to_string_lossy()
    );
    let mut apps = BTreeMap::new();
    for (name, entry) in data.apps {
        if name == ".app"
            || !name.ends_with(".app")
            || name.contains(['/', '\0', '\n', '\r'])
            || entry.source != format!("{source_dir}/{name}")
        {
            return Err(format!("invalid cask app payload: {name}"));
        }
        let source = output.join(entry.source);
        if !fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir())
            || !source.join("Contents").is_dir()
        {
            return Err(format!("missing cask app payload: {}", source.display()));
        }
        apps.insert(name, source);
    }
    Ok(apps)
}

/// Locate the shared materializer only for the generated private layout.
fn app_output(bundle: &Path) -> Option<&Path> {
    let sources = bundle.parent()?;
    let pkg = sources.parent()?;
    let libexec = pkg.parent()?;
    if sources.file_name()? == "app-sources" && libexec.file_name()? == "pkg-casks" {
        let directory = libexec.parent()?;
        let output = directory.parent()?;
        if directory.file_name()? == "libexec" && pkg.file_name()? == output.file_name()? {
            return Some(output);
        }
        return None;
    }
    None
}

/// Read-only destination calculation, shared with the builder runtime.
pub(super) fn restored_path(bundle: &Path) -> Result<Option<PathBuf>, String> {
    let Some(output) = app_output(bundle) else {
        return Ok(None);
    };
    Ok(Some(
        app_cache_root()?
            .join(output.file_name().ok_or("app output has no name")?)
            .join(bundle.file_name().ok_or("app bundle has no name")?),
    ))
}

/// Prepare and verify a private app copy through the shared child supervisor.
pub(super) fn prepare_bundle(bundle: &Path) -> Result<PathBuf, String> {
    let Some(expected) = restored_path(bundle)? else {
        return Ok(bundle.to_path_buf());
    };
    let helper = bundle
        .parent()
        .and_then(Path::parent)
        .ok_or("missing cask namespace")?
        .join("prepare");
    let name = bundle
        .file_name()
        .ok_or("missing cask app name")?
        .to_string_lossy()
        .into_owned();
    match run_direct_captured(&helper, &[String::from("prepare"), name])? {
        (Outcome::Success, text)
            if text.trim() == expected.to_string_lossy() && expected.is_dir() =>
        {
            Ok(expected)
        }
        (Outcome::Success, _) => Err(String::from("app preparation returned an unexpected path")),
        (Outcome::Failed { status, stderr }, _) => Err(format!(
            "app preparation failed ({status}): {}",
            stderr.trim()
        )),
        (Outcome::Interrupted { signal }, _) => Err(format!(
            "app preparation was interrupted by signal {signal}"
        )),
    }
}
