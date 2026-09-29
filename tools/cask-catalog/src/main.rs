//! Catalog CLI: fetch pinned snapshots, generate catalogs, and import public taps as Nix flakes.

use cask_catalog::{
    CACHE_DIR, Pin, emit, load_pin, read_verified_input, valid_repo, valid_revision, valid_sha256,
    verify_sha256,
};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

/// Default repository the snapshot is fetched from.
const DEFAULT_REPO: &str = "BatteredBunny/brew-api";
/// Default expected snapshot hash: the current pinned revision's bytes.
const DEFAULT_SHA256: &str = "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251";
/// Upstream data license attribution recorded in the pin.
const DATA_LICENSE: &str = "BSD-2-Clause (Homebrew Cask data)";

/// Generate the pkg Cask catalog from a pinned Homebrew snapshot.
#[derive(Parser)]
#[command(
    name = "cask-catalog",
    version,
    about = "Maintainer tool for the pkg Cask catalog"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download one exact snapshot revision and write the input pin.
    Fetch {
        /// Exact 40-hex-character commit revision of the snapshot.
        #[arg(long)]
        revision: String,
        /// `owner/name` repository hosting `cask.json`.
        #[arg(long, default_value = DEFAULT_REPO)]
        repo: String,
        /// Expected snapshot SHA-256; the download is verified against it
        /// before any pin or cache file is replaced.
        #[arg(long, default_value = DEFAULT_SHA256)]
        sha256: String,
        /// Snapshot cache directory (kept outside the repository).
        #[arg(long, default_value = CACHE_DIR)]
        cache_dir: PathBuf,
        /// Pin file to write.
        #[arg(long, default_value = "nix/casks/catalog/input.json")]
        pin: PathBuf,
    },
    /// Verify the cached snapshot against the pin and write the catalog.
    Generate {
        /// Pin file to read.
        #[arg(long, default_value = "nix/casks/catalog/input.json")]
        pin: PathBuf,
        /// Snapshot file; defaults to `<cache-dir>/<revision>.json`.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Snapshot cache directory for the default input path.
        #[arg(long, default_value = CACHE_DIR)]
        cache_dir: PathBuf,
        /// Output directory for the generated catalog.
        #[arg(long, default_value = "nix/casks/catalog")]
        out_dir: PathBuf,
    },
    /// Import one raw Homebrew tap through the ACTUAL public flow
    /// (GitHub identity check, codeload fetch, sandboxed Nix raw
    /// export) into a self-contained native-only flake tree (schema
    /// pkg-cask-catalog/3). Emits one JSON result on stdout.
    ImportTap {
        /// Tap identity `owner/tap` or
        /// `https://github.com/owner/homebrew-tap`.
        #[arg(long)]
        source: String,
        /// Exact 40-hex commit revision of the tap; omit to use the
        /// default branch head.
        #[arg(long)]
        revision: Option<String>,
        /// Single native target: aarch64-darwin or x86_64-linux. Must
        /// equal this build host.
        #[arg(long)]
        system: String,
        /// Path to the Nix EXECUTABLE (e.g. /nix/store/.../bin/nix)
        /// that builds the embedded raw-export runtime.
        #[arg(long, default_value = "nix")]
        nix: PathBuf,
        /// Output directory for the self-contained flake tree. Must be
        /// caller-owned and empty (precreated is fine).
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() {
    if let Err(message) = run() {
        eprintln!("cask-catalog: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Fetch {
            revision,
            repo,
            sha256,
            cache_dir,
            pin,
        } => fetch(&revision, &repo, &sha256, &cache_dir, &pin),
        Command::Generate {
            pin,
            input,
            cache_dir,
            out_dir,
        } => generate(&pin, input, &cache_dir, &out_dir),
        Command::ImportTap {
            source,
            revision,
            system,
            nix,
            out,
        } => import_tap(&source, revision.as_deref(), &system, &nix, &out),
    }
}

fn fetch(
    revision: &str,
    repo: &str,
    sha256: &str,
    cache_dir: &Path,
    pin_path: &Path,
) -> Result<(), String> {
    if !valid_revision(revision) {
        return Err(format!(
            "revision {revision:?} is not an exact 40-hex commit id"
        ));
    }
    if !valid_repo(repo) {
        return Err(format!("repository {repo:?} is not an owner/name pair"));
    }
    if !valid_sha256(sha256) {
        return Err("expected sha256 is not 64 hex characters".to_string());
    }
    let url = format!("https://raw.githubusercontent.com/{repo}/{revision}/cask.json");
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("cannot create cache {}: {e}", cache_dir.display()))?;
    // Download to a temporary file; nothing is replaced until the bytes
    // verify against the expected hash.
    let staged = tempfile::NamedTempFile::new_in(cache_dir)
        .map_err(|e| format!("cannot stage a download in {}: {e}", cache_dir.display()))?;
    let status = std::process::Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "20",
            "--max-time",
            "120",
            &url,
            "-o",
        ])
        .arg(staged.path())
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed ({status}) fetching {url}"));
    }
    let bytes = std::fs::read(staged.path())
        .map_err(|e| format!("cannot read the staged download: {e}"))?;
    if let Err(actual) = verify_sha256(&bytes, sha256) {
        return Err(format!(
            "downloaded snapshot hashes to {actual} but {sha256} was expected; \
             the prior pin and cache are unchanged"
        ));
    }
    let cached = cache_dir.join(format!("{revision}.json"));
    staged
        .persist(&cached)
        .map_err(|e| format!("cannot publish {}: {e}", cached.display()))?;
    let pin = Pin {
        url,
        revision: revision.to_string(),
        sha256: sha256.to_ascii_lowercase(),
        license: DATA_LICENSE.to_string(),
    };
    write_pin(pin_path, &pin)?;
    println!("fetched {} -> {}", pin.revision, pin_path.display());
    Ok(())
}

fn generate(
    pin_path: &Path,
    input: Option<PathBuf>,
    cache_dir: &Path,
    out_dir: &Path,
) -> Result<(), String> {
    let pin = load_pin(pin_path)?;
    let input = input.unwrap_or_else(|| cache_dir.join(format!("{}.json", pin.revision)));
    let bytes = read_verified_input(&input, &pin)?;
    let snapshot: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("snapshot {} is not valid JSON: {e}", input.display()))?;
    let generated = emit::generate(&snapshot, &pin)?;
    emit::write_catalog(out_dir, &generated.catalog)?;
    print!("{}", generated.coverage);
    println!(
        "catalog written to {}",
        out_dir.join("catalog.json").display()
    );
    Ok(())
}

fn import_tap(
    source: &str,
    revision: Option<&str>,
    system: &str,
    nix: &Path,
    out: &Path,
) -> Result<(), String> {
    let request = cask_catalog::tap::ImportRequest {
        source: source.to_string(),
        revision: revision.map(ToString::to_string),
        system: system.to_string(),
        nix: nix.to_path_buf(),
        output_dir: out.to_path_buf(),
    };
    let result = cask_catalog::tap::import(&request)?;
    // Machine-readable single-line result on stdout; the client parses
    // this after validating the catalog index. Serialized directly from
    // the `Serialize` derive: snake_case field names
    // (`metadata_sha256`, `flake_path`) are the stable client contract.
    let json = serde_json::to_string(&result).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

/// Write the pin through a unique same-directory temporary file.
fn write_pin(path: &Path, pin: &Pin) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let text = serde_json::to_string_pretty(pin).map_err(|e| e.to_string())?;
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("cannot stage the pin: {e}"))?;
    std::io::Write::write_all(&mut tmp.as_file(), format!("{text}\n").as_bytes())
        .map_err(|e| format!("cannot write the staged pin: {e}"))?;
    tmp.persist(path)
        .map_err(|e| format!("cannot publish {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_validation_is_strict() {
        assert!(valid_revision("245947c0b920cbe83f4003bb7c6be737352c8314"));
        for bad in [
            "main",
            "HEAD",
            "245947c0",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ] {
            assert!(!valid_revision(bad), "{bad} must not be a revision");
        }
        assert!(valid_repo("BatteredBunny/brew-api"));
        for bad in ["brew-api", "https://x/y", "a/b/c", "/x/", "a/"] {
            assert!(!valid_repo(bad), "{bad} must not be a repo");
        }
        assert!(valid_sha256(DEFAULT_SHA256));
        assert!(!valid_sha256("no_check"));
    }
}
