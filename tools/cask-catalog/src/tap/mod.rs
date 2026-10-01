//! Raw tap importer: one public Homebrew tap in, one self-contained
//! native-only flake tree out (schema `pkg-cask-catalog/4`).
//!
//! The actual public import flow, end to end:
//!
//! 1. Canonicalize the source (`owner/tap` or
//!    `https://github.com/owner/homebrew-tap`), validate the native
//!    host target, and prepare the caller-owned output directory.
//! 2. Verify repository identity through the GitHub API: `full_name`
//!    must equal `owner/homebrew-<tap>` exactly (lowercased), even
//!    when an explicit revision is given; transfer/rename/cross-repo
//!    redirects are refused.
//! 3. Fetch the exact-revision codeload `tar.gz` with the guarded
//!    `SafeClient` and unpack it with strict limits (`archive`). Tap
//!    Ruby is data here; it is never executed in this process.
//! 4. Set up the fresh host canary and live loopback listener
//!    (positive controls) and run the sandboxed Nix raw-export build
//!    (`runtime`). The in-build probe is authoritative.
//! 5. Validate the `export.json` envelope (identity, revision, native
//!    system, probe marker, file inventory) and convert it into the
//!    flake tree via the private `capture::convert_capture`. The
//!    client validates and publishes.
//!
//! Submodules: `archive` (strict unpacking and tree inventory),
//! `runtime` (canary, bounded child runner, Nix build, asset writes),
//! `config` (local effective nix config gate), and `capture`
//! (envelope validation and conversion). No real tap Ruby ever runs
//! in this process; the only execution of upstream loader code
//! happens inside the sandboxed Nix build.

mod archive;
mod capture;
mod config;
mod runtime;

#[cfg(test)]
mod capture_cases;

use crate::github;
use crate::net::{GITHUB_HOSTS, SafeClient};
use crate::{valid_repo, valid_revision};
use archive::{MAX_ARCHIVE_BYTES, extract_tar_gz, tree_inventory};
use capture::{Capture, MAX_EXPORT_BYTES, convert_capture, validate_export};
use config::{GateError, nix_config_gate};
pub use config::{mac_setup_settings, verify_effective_config};
use runtime::{run_nix_export, setup_canary};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

/// Envelope schema of the raw-export runtime output we consume.
pub const EXPORT_SCHEMA: &str = "pkg-tap-raw-export/1";

/// The typed failure of one raw tap import.
///
/// The category is produced by construction at the one refusal source
/// (the local effective-config gate), never by matching message text;
/// callers format the message at their boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The effective local nix config refuses the raw tap import (weak
    /// sandbox settings, added host paths, impure dependencies). The
    /// client may offer its one-time consent-gated administrator setup
    /// and retry once; the message carries the concrete setup advice.
    SandboxRefused(String),
    /// Any other failure: canonicalization, identity, network, archive,
    /// build, or conversion. Setup cannot fix these.
    Failed(String),
}

impl ImportError {
    /// The full detail message, whatever the category.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::SandboxRefused(detail) | Self::Failed(detail) => detail,
        }
    }

    /// Whether this failure is the typed sandbox refusal.
    #[must_use]
    pub fn is_sandbox_refusal(&self) -> bool {
        matches!(self, Self::SandboxRefused(_))
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ImportError {}

impl From<String> for ImportError {
    /// Every ordinary string failure is an ordinary import failure.
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// One import job: the tap, the optional exact revision, the native
/// target, the Nix executable to drive, and where the self-contained
/// flake tree is written.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// Tap identity: `owner/tap` or
    /// `https://github.com/owner/homebrew-tap` (lowercased and
    /// normalized).
    pub source: String,
    /// Exact 40-hex commit revision. `None` resolves the default
    /// branch head through the GitHub API.
    pub revision: Option<String>,
    /// The single native catalog target (`aarch64-darwin` or
    /// `x86_64-linux`); must equal this build host.
    pub system: String,
    /// Path to the Nix executable used to build `raw-export.nix`.
    pub nix: PathBuf,
    /// Output directory the self-contained flake tree is written into.
    /// Must be caller-owned and empty (precreated is fine).
    pub output_dir: PathBuf,
}

/// The outcome of one successful import. Serialized by the CLI in
/// snake_case (`metadata_sha256`, `flake_path`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    /// Canonical source id `owner/tap`.
    pub source: String,
    /// Canonical approved origin URL
    /// `https://github.com/owner/homebrew-tap`.
    pub origin: String,
    /// Exact revision the metadata was captured at.
    pub revision: String,
    /// SHA-256 of the consumed `export.json` bytes (the capture).
    pub metadata_sha256: String,
    /// `output_dir/flake` (the generated flake DIRECTORY).
    pub flake_path: PathBuf,
    /// Eligible entries on the native target (native-only counts).
    pub eligible: usize,
    /// Excluded entries on the native target.
    pub excluded: usize,
}

/// Whether a path segment is plain (no dot/dotdot, no backslash, no
/// control characters, non-empty).
fn safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('\\')
        && !segment.chars().any(|c| c.is_ascii_control())
}

/// Canonicalize a tap identity to `owner/tap`.
///
/// Accepts the short form `owner/tap` (with or without a `homebrew-`
/// repo prefix) or an explicit `https://github.com/owner/homebrew-tap`
/// URL. Both segments are lowercased; exactly one `homebrew-` prefix
/// is stripped, never more. An explicit GitHub URL MUST name the
/// `homebrew-` repository: `https://github.com/example/foo` is refused
/// instead of being silently rewritten to `homebrew-foo` (the short
/// `example/foo` form stays valid). A repeated `homebrew-homebrew-`
/// prefix is ambiguous and refused. Dot/dot segments, backslashes,
/// control characters, credentials, query strings, fragments, explicit
/// ports, extra slashes, and non-GitHub hosts are refused. The
/// function is idempotent: a canonical id and the canonical origin URL
/// both canonicalize to the same id.
pub fn canonicalize_source(source: &str) -> Result<String, String> {
    if source.trim() != source {
        return Err(format!("source {source:?} has leading/trailing whitespace"));
    }
    let explicit_url = source.starts_with("https://");
    let identity = if explicit_url {
        let url =
            Url::parse(source).map_err(|e| format!("source {source:?} is not a valid URL: {e}"))?;
        if url.scheme() != "https" {
            return Err(format!("source {source:?} is not an https URL"));
        }
        if url.host_str() != Some("github.com") {
            return Err(format!(
                "source {source:?} is not a github.com URL (only GitHub public taps are \
                 importable)"
            ));
        }
        if url.port().is_some() {
            return Err(format!("source {source:?} carries an explicit port"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("source {source:?} carries embedded credentials"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(format!("source {source:?} carries a query or fragment"));
        }
        let segments: Vec<&str> = url
            .path_segments()
            .map(Iterator::collect)
            .ok_or_else(|| format!("source {source:?} has no path segments"))?;
        if segments.len() != 2 || segments.iter().any(|s| !safe_segment(s)) {
            return Err(format!(
                "source {source:?} is not exactly https://github.com/owner/repo"
            ));
        }
        format!("{}/{}", segments[0], segments[1])
    } else if source.contains("://") || source.contains('\\') {
        return Err(format!(
            "source {source:?} is neither owner/tap nor \
             https://github.com/owner/homebrew-tap"
        ));
    } else {
        source.to_string()
    };
    let lowered = identity.to_ascii_lowercase();
    let Some((owner, repo)) = lowered.split_once('/') else {
        return Err(format!(
            "source {source:?} is not an owner/tap pair (expected owner/tap)"
        ));
    };
    if !safe_segment(owner) || !safe_segment(repo) {
        return Err(format!(
            "source {source:?} has an unsafe owner or repo segment (dot, dotdot, \
             backslash, or control characters)"
        ));
    }
    if repo.starts_with("homebrew-homebrew-") {
        return Err(format!(
            "source {source:?} carries a repeated homebrew- prefix; name the repository \
             with exactly one prefix (or use the bare tap short form)"
        ));
    }
    if explicit_url && !repo.starts_with("homebrew-") {
        return Err(format!(
            "source {source:?} is an explicit github.com URL whose repository {repo:?} does \
             not start with homebrew-; use https://github.com/owner/homebrew-tap or the \
             short owner/tap form"
        ));
    }
    let tap = repo.strip_prefix("homebrew-").unwrap_or(repo);
    let canonical = format!("{owner}/{tap}");
    if !valid_repo(&canonical) || !safe_segment(owner) || !safe_segment(tap) {
        return Err(format!(
            "source {source:?} canonicalizes to {canonical:?}, which is not a \
             safe owner/tap pair"
        ));
    }
    Ok(canonical)
}

/// The single native catalog target for THIS build host.
pub fn native_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-darwin"),
        ("linux", "x86_64") => Ok("x86_64-linux"),
        (os, arch) => Err(format!(
            "build host {arch}-{os} is not a catalog native target (need \
             aarch64-darwin or x86_64-linux); run the import on a matching clean host"
        )),
    }
}

/// The canonical approved origin URL for a canonical source id:
/// `https://github.com/owner/homebrew-tap`.
#[must_use = "the result is the catalog inputs url contract"]
pub fn source_origin(source: &str) -> Result<String, String> {
    let canonical = canonicalize_source(source)?;
    let Some((owner, tap)) = canonical.split_once('/') else {
        return Err(format!("canonical source {canonical:?} is not owner/tap"));
    };
    Ok(format!("https://github.com/{owner}/homebrew-{tap}"))
}

/// The exact-revision codeload archive URL (download URL only; never the
/// public `inputs.url`, which stays the canonical origin).
fn archive_url(source: &str, revision: &str) -> Result<String, String> {
    let canonical = canonicalize_source(source)?;
    let Some((owner, tap)) = canonical.split_once('/') else {
        return Err(format!("canonical source {canonical:?} is not owner/tap"));
    };
    Ok(format!(
        "https://codeload.github.com/{owner}/homebrew-{tap}/tar.gz/{revision}"
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Prepare the caller-owned output directory: precreated is fine, but
/// it must be empty and owned by the current user.
fn prepare_output_dir(output_dir: &Path) -> Result<(), String> {
    if output_dir.exists() {
        let meta = std::fs::metadata(output_dir)
            .map_err(|e| format!("cannot stat {}: {e}", output_dir.display()))?;
        if !meta.is_dir() {
            return Err(format!(
                "output {} is not a directory",
                output_dir.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            // Effective uid, derived from a file this process just
            // created inside the directory (portable across Linux and
            // macOS; /proc is consulted first when present).
            let probe = tempfile::NamedTempFile::new_in(output_dir).map_err(|e| {
                format!("cannot probe write access in {}: {e}", output_dir.display())
            })?;
            let euid = probe.as_file().metadata().map_err(|e| e.to_string())?.uid();
            if meta.uid() != euid {
                return Err(format!(
                    "output {} is owned by uid {} but the importer runs as uid {euid}; \
                     refuse to write into a directory the caller does not own",
                    output_dir.display(),
                    meta.uid()
                ));
            }
        }
        let empty = std::fs::read_dir(output_dir)
            .map_err(|e| format!("cannot list {}: {e}", output_dir.display()))?
            .next()
            .is_none();
        if !empty {
            return Err(format!(
                "output {} is not empty; the import writes a fresh self-contained \
                 tree (precreated empty is fine)",
                output_dir.display()
            ));
        }
    } else {
        std::fs::create_dir_all(output_dir)
            .map_err(|e| format!("cannot create {}: {e}", output_dir.display()))?;
    }
    Ok(())
}

/// Import one raw public tap through the ACTUAL public flow.
///
/// Steps: identity check, archive fetch and strict unpack,
/// canary/listener positive controls, sandboxed Nix raw-export build,
/// envelope validation, and flake tree generation into
/// `request.output_dir/flake/`.
pub fn import(request: &ImportRequest) -> Result<ImportResult, ImportError> {
    let source = canonicalize_source(&request.source)?;
    let origin = source_origin(&source)?;
    let native = native_target()?;
    if request.system != native {
        return Err(format!(
            "requested system {:?} does not match this build host's native target \
             {native:?}; raw exports are native-host-only",
            request.system
        )
        .into());
    }
    let revision = match &request.revision {
        Some(revision) => {
            if !valid_revision(revision) {
                return Err(
                    format!("revision {revision:?} is not an exact 40-hex commit id").into(),
                );
            }
            revision.to_ascii_lowercase()
        }
        None => String::new(),
    };
    prepare_output_dir(&request.output_dir)?;
    let staging = tempfile::TempDir::new_in(&request.output_dir).map_err(|e| {
        ImportError::Failed(format!(
            "cannot create staging in {}: {e}",
            request.output_dir.display()
        ))
    })?;

    // 2. Security gate: the EFFECTIVE local nix config must already be
    // fail-closed before the raw-export build starts. This is not a
    // substitute for the in-build probe; the fresh probe remains
    // mandatory and authoritative. Only an actually OBSERVED refusal
    // becomes the typed `SandboxRefused`; a settings query that could
    // not run at all stays an ordinary failure the client must not
    // answer with setup.
    nix_config_gate(&request.nix, staging.path()).map_err(|error| match error {
        GateError::Refused(detail) => ImportError::SandboxRefused(detail),
        GateError::Unverifiable(detail) => ImportError::Failed(detail),
    })?;

    // 3. GitHub API identity verification and revision resolution.
    let Some((owner, tap)) = source.split_once('/') else {
        return Err(format!("canonical source {source:?} is not owner/tap").into());
    };
    let repo = format!("homebrew-{tap}");
    let client = SafeClient::new(&GITHUB_HOSTS)?;
    let identity = github::verify_repo(
        &client,
        owner,
        &repo,
        if revision.is_empty() {
            None
        } else {
            Some(revision.as_str())
        },
    )?;
    let revision = identity.revision;
    let license = identity
        .license_spdx
        .unwrap_or_else(|| "unknown".to_string());

    // 4. Bounded codeload fetch and strict unpack.
    let archive_url_value = archive_url(&source, &revision)?;
    let archive_bytes = client.get(&archive_url_value, None)?;
    if archive_bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "codeload archive is {} bytes; limit is {MAX_ARCHIVE_BYTES}",
            archive_bytes.len()
        )
        .into());
    }
    let archive_sha256 = sha256_hex(&archive_bytes);
    let tap_tree = staging.path().join("tap-tree");
    extract_tar_gz(&archive_bytes, &tap_tree)?;

    // 5. Fresh canary + live listener positive controls (created only
    // after the fetch; held until the build completes).
    let canary = setup_canary()?;

    // 6. Trusted runtime + bounded Nix build of the ordinary derivation.
    let out_link = run_nix_export(
        request,
        staging.path(),
        &tap_tree,
        &source,
        &revision,
        &canary,
    )?;
    // RAII: the canary and listener stop existing on this and every
    // error path (Drop), removing only the paths they created.
    drop(canary);

    // 7. Read and validate the capture, then convert it. The read is
    // bounded BEFORE the JSON parse.
    let mut export_file = std::fs::File::open(out_link.join("export.json")).map_err(|e| {
        format!(
            "cannot read {} : {e}",
            out_link.join("export.json").display()
        )
    })?;
    let mut export_bytes = Vec::new();
    (&mut export_file)
        .take((MAX_EXPORT_BYTES as u64) + 1)
        .read_to_end(&mut export_bytes)
        .map_err(|e| format!("cannot read the export capture: {e}"))?;
    let (export, metadata_sha256) = validate_export(&export_bytes, &source, &revision, native)?;
    let capture = Capture {
        source,
        origin,
        revision: revision.clone(),
        system: native.to_string(),
        export,
        metadata_sha256,
        archive_url: archive_url_value,
        archive_sha256,
        license,
        tree: tree_inventory(&tap_tree)?,
    };
    let result = convert_capture(&capture, &request.output_dir)?;
    drop(staging);
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn canonicalize_accepts_owner_tap_and_github_url_forms() {
        assert_eq!(
            canonicalize_source("Example/Homebrew-Foo").unwrap(),
            "example/foo"
        );
        assert_eq!(canonicalize_source("example/foo").unwrap(), "example/foo");
        assert_eq!(
            canonicalize_source("https://github.com/Example/Homebrew-Foo").unwrap(),
            "example/foo"
        );
    }

    #[test]
    fn canonicalize_rejects_unsafe_sources() {
        for bad in [
            "solo",
            "a/b/c",
            "../escape/pid",
            "a/../b",
            "a/./b",
            "a/b/",
            "/a/b",
            "a\\b/c",
            "https://gitlab.com/a/b",
            "https://github.com/a/b/c",
            "https://github.com/a/b/",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#frag",
            "https://user:pw@github.com/a/b",
            "https://github.com:8443/a/b",
            "http://github.com/a/b",
            "..%2fa",
        ] {
            assert!(canonicalize_source(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn canonicalize_is_idempotent_and_roundtrips_the_exact_origin() {
        for input in [
            "Example/Homebrew-Foo",
            "example/foo",
            "https://github.com/Example/Homebrew-Foo",
        ] {
            let canonical = canonicalize_source(input).unwrap();
            assert_eq!(
                canonicalize_source(&canonical).unwrap(),
                canonical,
                "{input}: a canonical id must canonicalize to itself"
            );
            let origin = source_origin(&canonical).unwrap();
            assert_eq!(
                canonicalize_source(&origin).unwrap(),
                canonical,
                "{origin}: the canonical origin URL must canonicalize back to the id"
            );
        }
    }

    #[test]
    fn canonicalize_rejects_repeated_homebrew_prefixes() {
        for bad in [
            "example/homebrew-homebrew-foo",
            "Example/Homebrew-Homebrew-Foo",
            "https://github.com/example/homebrew-homebrew-foo",
        ] {
            let err = canonicalize_source(bad).unwrap_err();
            assert!(err.contains("repeated homebrew-"), "{bad}: {err}");
        }
    }

    #[test]
    fn canonicalize_rejects_nonprefixed_explicit_github_urls() {
        let err = canonicalize_source("https://github.com/example/foo").unwrap_err();
        assert!(err.contains("homebrew-"), "{err}");
        // The short form without a prefix stays valid.
        assert_eq!(canonicalize_source("example/foo").unwrap(), "example/foo");
    }

    #[test]
    fn source_origin_is_the_approved_canonical_url() {
        assert_eq!(
            source_origin("example/foo").unwrap(),
            "https://github.com/example/homebrew-foo"
        );
    }

    #[test]
    fn archive_url_pins_the_exact_revision() {
        assert_eq!(
            archive_url("example/foo", REVISION).unwrap(),
            format!("https://codeload.github.com/example/homebrew-foo/tar.gz/{REVISION}")
        );
    }
    #[test]
    fn import_rejects_non_native_system_and_missing_revision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let request = ImportRequest {
            source: "example/foo".to_string(),
            revision: Some("main".to_string()),
            system: "aarch64-darwin".to_string(),
            nix: PathBuf::from("/nonexistent/nix"),
            output_dir: dir.path().to_path_buf(),
        };
        let error = import(&request).unwrap_err();
        assert!(
            error.message().contains("native") || error.message().contains("40-hex"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn import_rejects_non_empty_output_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("stale.txt"), b"x").unwrap();
        let request = ImportRequest {
            source: "example/foo".to_string(),
            revision: None,
            system: native_target().unwrap().to_string(),
            nix: PathBuf::from("/nonexistent/nix"),
            output_dir: dir.path().to_path_buf(),
        };
        let error = import(&request).unwrap_err();
        assert!(
            error.message().contains("not empty"),
            "unexpected error: {error}"
        );
        assert!(
            !error.is_sandbox_refusal(),
            "an unusable output dir is no refusal"
        );
    }

    #[test]
    fn import_errors_are_typed_not_textual() {
        // The typed refusal is the only category the client may answer
        // with setup — by construction, never by message text.
        let refused = ImportError::SandboxRefused(String::from("refuses the raw tap import"));
        assert!(refused.is_sandbox_refusal());
        assert_eq!(refused.message(), "refuses the raw tap import");
        // An ordinary failure whose text happens to contain the refusal
        // phrase stays an ordinary failure.
        let lookalike = ImportError::Failed(String::from(
            "importing x failed: effective local nix config refuses the raw \
             tap import: sandbox is false",
        ));
        assert!(!lookalike.is_sandbox_refusal());
        // Ordinary string failures convert to the ordinary category.
        let converted: ImportError = String::from("network down").into();
        assert_eq!(converted, ImportError::Failed(String::from("network down")));
    }
}
