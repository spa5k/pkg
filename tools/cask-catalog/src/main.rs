//! Catalog CLI: fetch signed snapshots, generate catalogs, and import public taps as Nix flakes.

// Tests may abort on broken fixtures or failed setup; production code
// denies explicit unwrap/expect/panic (these Clippy lints are denied by
// the workspace for every non-test target).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
    )
)]

use cask_catalog::tap::ImportError;
use cask_catalog::{
    CACHE_DIR, Pin, emit, homebrew_api, load_pin, read_verified_input, valid_sha256, valid_url,
    verify_sha256,
};
use clap::{Parser, Subcommand};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The largest snapshot or signed API file a fetch accepts.
const MAX_SNAPSHOT_BYTES: u64 = 128 * 1024 * 1024;
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
    /// Download one signed Homebrew API file and write its checked payload.
    ///
    /// Print the payload SHA-256 and its one `tap_git_head` as JSON.
    FetchSigned {
        /// The API file: `cask` or `formula`.
        #[arg(long, value_parser = ["cask", "formula"])]
        name: String,
        /// Payload file to write.
        #[arg(long)]
        out: PathBuf,
    },
    /// Download one stored snapshot and write the input pin.
    Fetch {
        /// Snapshot URL. A `.gz` URL holds the gzip-compressed payload.
        #[arg(long)]
        url: String,
        /// Read this local payload, such as `fetch-signed` output, instead of
        /// the URL. The pin still records the URL, so store the payload there.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Expected payload SHA-256; the download is verified against it
        /// before any pin or cache file is replaced.
        #[arg(long)]
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
    /// Import a public Homebrew tap as a self-contained Nix flake.
    /// Check GitHub identity, fetch the archive, and run the sandboxed export.
    /// Generate a pkg-cask-catalog/4 catalog and print the import result as JSON.
    ImportTap(cask_catalog::tap::ImportRequest),
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cask-catalog: {error}");
        std::process::exit(error.signal().map_or(1, |signal| 128 + signal));
    }
}

fn run() -> Result<(), ImportError> {
    match Cli::parse().command {
        Command::FetchSigned { name, out } => fetch_signed(&name, &out).map_err(ImportError::from),
        Command::Fetch {
            url,
            input,
            sha256,
            cache_dir,
            pin,
        } => fetch(&url, input.as_deref(), &sha256, &cache_dir, &pin).map_err(ImportError::from),
        Command::Generate {
            pin,
            input,
            cache_dir,
            out_dir,
        } => generate(&pin, input, &cache_dir, &out_dir).map_err(ImportError::from),
        Command::ImportTap(request) => import_tap(&request),
    }
}

/// Download `url` to `destination` with curl over HTTPS only.
fn download(url: &str, destination: &Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "20",
            "--max-time",
            "300",
            "--max-filesize",
            &MAX_SNAPSHOT_BYTES.to_string(),
            url,
            "-o",
        ])
        .arg(destination)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed ({status}) fetching {url}"));
    }
    Ok(())
}

/// Write `bytes` to `path` through a same-directory temporary file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("cannot stage {}: {e}", path.display()))?;
    std::io::Write::write_all(staged.as_file_mut(), bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    staged
        .persist(path)
        .map_err(|e| format!("cannot publish {}: {e}", path.display()))?;
    Ok(())
}

fn fetch_signed(name: &str, out: &Path) -> Result<(), String> {
    let staged =
        tempfile::NamedTempFile::new().map_err(|e| format!("cannot stage a download: {e}"))?;
    download(&homebrew_api::url(name), staged.path())?;
    let document = std::fs::read(staged.path())
        .map_err(|e| format!("cannot read the staged download: {e}"))?;
    let payload = homebrew_api::verify(&document)?;
    let revision = homebrew_api::revision(&payload)?;
    write_atomic(out, &payload)?;
    println!(
        "{}",
        serde_json::json!({
            "sha256": cask_catalog::sha256_hex(&payload),
            "revision": revision,
        })
    );
    Ok(())
}

fn fetch(
    url: &str,
    input: Option<&Path>,
    sha256: &str,
    cache_dir: &Path,
    pin_path: &Path,
) -> Result<(), String> {
    if !valid_url(url) || !url.starts_with("https://") {
        return Err(format!("snapshot URL {url:?} is not an https URL"));
    }
    if !valid_sha256(sha256) {
        return Err("expected sha256 is not 64 hex characters".to_string());
    }
    // Download to a temporary file; nothing is replaced until the bytes
    // verify against the expected hash.
    let staged =
        tempfile::NamedTempFile::new().map_err(|e| format!("cannot stage a download: {e}"))?;
    let (source, name) = if let Some(input) = input {
        (input, input)
    } else {
        download(url, staged.path())?;
        (staged.path(), Path::new(url))
    };
    let file = std::fs::File::open(source)
        .map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    let mut bytes = Vec::new();
    let read = if name.extension().is_some_and(|extension| extension == "gz") {
        flate2::read::GzDecoder::new(file)
            .take(MAX_SNAPSHOT_BYTES + 1)
            .read_to_end(&mut bytes)
    } else {
        file.take(MAX_SNAPSHOT_BYTES + 1).read_to_end(&mut bytes)
    };
    read.map_err(|e| format!("cannot read {}: {e}", name.display()))?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(format!("{url} exceeds {MAX_SNAPSHOT_BYTES} bytes"));
    }
    if let Err(actual) = verify_sha256(&bytes, sha256) {
        return Err(format!(
            "downloaded snapshot hashes to {actual} but {sha256} was expected; \
             the prior pin and cache are unchanged"
        ));
    }
    let revision = homebrew_api::revision(&bytes)?;
    write_atomic(&cache_dir.join(format!("{revision}.json")), &bytes)?;
    let pin = Pin {
        url: url.to_string(),
        revision,
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

fn import_tap(request: &cask_catalog::tap::ImportRequest) -> Result<(), ImportError> {
    let result = cask_catalog::tap::import(request)?;
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let text = serde_json::to_string_pretty(pin).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("cannot stage the pin: {e}"))?;
    std::io::Write::write_all(tmp.as_file_mut(), format!("{text}\n").as_bytes())
        .map_err(|e| format!("cannot write the staged pin: {e}"))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| format!("cannot sync the staged pin: {e}"))?;
    tmp.persist(path)
        .map_err(|e| format!("cannot publish {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cask_catalog::valid_revision;

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
        assert!(valid_sha256(
            "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"
        ));
        assert!(!valid_sha256("no_check"));
    }
}
