//! Raw-export envelope validation and conversion into the
//! self-contained flake tree.

use super::runtime::write_asset;
use super::{EXPORT_SCHEMA, ImportResult, safe_segment, sha256_hex};
use crate::classify::{self, TargetStatus};
use crate::effective::{self, RawRecord};
use crate::emit::{self, Entry, InputProvenance, Origin};
use crate::{MACOS_BASELINE, valid_sha256};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Embedded builder assets (our own static files only).
const BUILDERS_NIX: &str = include_str!("../../../../nix/casks/lib/builders.nix");
const PLAN_PY: &str = include_str!("../../../../nix/casks/lib/plan.py");
const PUBLIC_FETCH_NIX: &str = include_str!("../../../../nix/casks/lib/public-fetch.nix");
const SAFE_FETCH_PY: &str = include_str!("../../../../nix/casks/lib/safe-fetch.py");
const FLAKE_TEMPLATE: &str = include_str!("flake-template.nix");
/// Shared catalog projection helper: the SAME `lib/catalog.nix` the
/// official `nix/casks/flake.nix` uses, copied verbatim into the
/// generated tree so both wrappers share one algorithm.
const CATALOG_NIX: &str = include_str!("../../../../nix/casks/lib/catalog.nix");
/// Committed lock for the pinned nixpkgs revision the template names.
/// Copied into the generated tree so evaluation never writes a lock.
const FLAKE_LOCK: &str = include_str!("../../../../nix/casks/flake.lock");
/// Vendored license attributions required by the copied builder assets
/// (brew-nix port in `builders.nix`; Homebrew cask data).
const BREW_NIX_LICENSE: &str = include_str!("../../../../nix/casks/lib/brew-nix-LICENSE");
const HOMEBREW_LICENSE: &str =
    include_str!("../../../../nix/casks/lib/licenses/homebrew-cask-LICENSE.txt");
/// export.json byte limit (bounded read BEFORE the JSON parse).
pub(super) const MAX_EXPORT_BYTES: usize = 32 * 1024 * 1024;

/// Whether a source-relative path is plain and inside the tree (same
/// rule as the snapshot driver; see emit.rs).
fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_ascii_control())
        && path.split('/').all(safe_segment)
}

/// Validate one raw-export `pkg_origin` against the ACTUAL extracted
/// source tree: the path must exist (exact spelling), the SHA-256 must
/// match the file bytes on disk, and the file must name the token.
/// Ruby output is never trusted alone.
fn validate_pkg_origin(
    origin: &Value,
    tree: &BTreeMap<String, String>,
    token: &str,
) -> Result<Origin, String> {
    let path = origin.get("path").and_then(Value::as_str).unwrap_or("");
    let sha = origin.get("sha256").and_then(Value::as_str).unwrap_or("");
    if !valid_sha256(sha) || !safe_relative_path(path) {
        return Err(format!("pkg_origin is malformed (path {path:?})"));
    }
    let sha = sha.to_ascii_lowercase();
    let Some(actual) = tree.get(path) else {
        return Err(format!(
            "pkg_origin path {path:?} is not a file in the extracted source tree"
        ));
    };
    if actual != &sha {
        return Err(format!(
            "pkg_origin sha256 for {path:?} does not match the extracted source tree"
        ));
    }
    let file = path.rsplit('/').next().unwrap_or("");
    let stem = file.strip_suffix(".rb").unwrap_or(file);
    if stem != token {
        return Err(format!(
            "pkg_origin file {path:?} does not name token {token:?}"
        ));
    }
    Ok(Origin {
        path: path.to_string(),
        sha256: sha,
    })
}

// ---------------------------------------------------------------------------
// Envelope validation and capture conversion
// ---------------------------------------------------------------------------

/// Validate the raw-export envelope bytes against the import identity.
pub(super) fn validate_export(
    bytes: &[u8],
    source: &str,
    revision: &str,
    system: &str,
) -> Result<(Value, String), String> {
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(format!(
            "export is {} bytes; refusing envelopes over {MAX_EXPORT_BYTES} bytes",
            bytes.len()
        ));
    }
    let metadata_sha256 = sha256_hex(bytes);
    let export: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("export envelope is malformed JSON: {e}"))?;
    let schema = export
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "export envelope has no schema string".to_string())?;
    if schema != EXPORT_SCHEMA {
        return Err(format!(
            "export envelope schema {schema:?} is not {EXPORT_SCHEMA:?}"
        ));
    }
    let export_source = export
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| "export envelope has no source string".to_string())?;
    if export_source.to_ascii_lowercase() != source {
        return Err(format!(
            "export source {export_source:?} does not match the requested source {source:?}"
        ));
    }
    let export_revision = export
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| "export envelope has no revision string".to_string())?;
    if !export_revision.eq_ignore_ascii_case(revision) {
        return Err(format!(
            "export revision {export_revision:?} does not match the requested revision \
             {revision:?}"
        ));
    }
    let export_system = export
        .get("system")
        .and_then(Value::as_str)
        .ok_or_else(|| "export envelope has no system string".to_string())?;
    if export_system != system {
        return Err(format!(
            "export system {export_system:?} does not match the requested system {system:?}"
        ));
    }
    // The sandbox probe is authoritative: only the exact DENIED marker
    // from the in-build probe is acceptable.
    let probe = export
        .get("probe")
        .and_then(Value::as_str)
        .ok_or_else(|| "export envelope has no probe marker".to_string())?;
    if probe != "denied: host-read, host-write, loopback" {
        return Err(format!(
            "export probe marker {probe:?} is not the authoritative denied marker; \
             refusing an unproven sandbox export"
        ));
    }
    let casks = export
        .get("casks")
        .and_then(Value::as_array)
        .ok_or_else(|| "export envelope has no casks array".to_string())?;
    let files = export
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| "export envelope has no files inventory".to_string())?;
    let total = files
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let ok = files.get("ok").and_then(Value::as_u64).unwrap_or(0);
    let failed = files
        .get("failed")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    if total != casks.len() as u64 || ok.checked_add(failed) != Some(total) {
        return Err(format!(
            "export file inventory (total {total}, ok {ok}, failed {failed}) does not \
             match the {} cask records",
            casks.len()
        ));
    }
    if ok == 0 {
        return Err("every cask file failed to export; refusing an all-error capture".to_string());
    }
    Ok((export, metadata_sha256))
}

/// Classify one raw-exported cask for the single native target.
fn tap_status(cask: &Value, system: &str) -> TargetStatus {
    let Some(supported) = cask
        .get("upstreamPlatformSupported")
        .and_then(Value::as_bool)
    else {
        return TargetStatus {
            status: "excluded",
            kind: None,
            reason: Some("malformed-record".to_string()),
            detail: Some("no upstreamPlatformSupported boolean".to_string()),
            version: None,
            homepage: None,
            plan: None,
        };
    };
    // Flat upstream metadata: the exporter emits every field
    // (name/version/url/...) directly on the cask record.
    let record = RawRecord::new(cask.clone());
    classify::classify_tap_target(&record, system, MACOS_BASELINE, supported)
}

fn excluded_status(reason: &str, detail: &str) -> TargetStatus {
    TargetStatus {
        status: "excluded",
        kind: None,
        reason: Some(reason.to_string()),
        detail: Some(detail.chars().take(200).collect::<String>()),
        version: None,
        homepage: None,
        plan: None,
    }
}

/// Convert a VALIDATED raw-export envelope into the self-contained
/// flake directory tree under `output_dir/flake/`.
///
/// Private helper: the public [`super::import`] drives the whole actual flow;
/// unit tests call this directly with fixture envelopes.
pub(super) struct Capture {
    pub(super) source: String,
    pub(super) origin: String,
    pub(super) revision: String,
    pub(super) system: String,
    pub(super) export: Value,
    pub(super) metadata_sha256: String,
    pub(super) archive_url: String,
    pub(super) archive_sha256: String,
    pub(super) license: String,
    /// SHA-256 inventory of the extracted tap tree; every cask origin
    /// is validated against it (path, hash, and token spelling).
    pub(super) tree: BTreeMap<String, String>,
}

pub(super) fn convert_capture(
    capture: &Capture,
    output_dir: &Path,
) -> Result<ImportResult, String> {
    let Capture {
        source,
        origin,
        revision,
        system,
        export,
        metadata_sha256,
        archive_url: archive_url_value,
        archive_sha256,
        license,
        tree,
    } = capture;
    let casks = export
        .get("casks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut eligible = 0usize;
    let mut excluded = 0usize;

    let mut record_for =
        |token: String, status: TargetStatus, origin_field: Option<Origin>, raw: Option<&Value>| {
            let raw = raw.filter(|v| v.is_object());
            if !crate::token::is_valid_token(&token) {
                return Err(format!(
                    "integrity: exported token {token:?} is not a usable catalog key"
                ));
            }
            let id = emit::qualified_id(source, &token);
            if entries.contains_key(&id) {
                return Err(format!("integrity: duplicate token {token} in export"));
            }
            let record = raw.cloned().map(RawRecord::new);
            let (name, description, version, homepage) = match &record {
                Some(record) => (
                    effective::name_field(&record.raw),
                    effective::str_field(&record.raw, "desc"),
                    effective::str_field(&record.raw, "version"),
                    effective::str_field(&record.raw, "homepage"),
                ),
                None => (None, None, None, None),
            };
            if status.status == "eligible" {
                eligible += 1;
            } else {
                excluded += 1;
            }
            let mut targets = BTreeMap::new();
            targets.insert(system.to_string(), status);
            entries.insert(
                id,
                Entry {
                    source: source.to_string(),
                    token,
                    name,
                    description,
                    version,
                    homepage,
                    origin: origin_field,
                    targets,
                },
            );
            Ok(())
        };

    for cask in &casks {
        let token = cask
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "export cask without a token string".to_string())?
            .to_string();
        // In-band load-error records (upstream "records" array mixes ok
        // and error records) become excluded entries, but their provenance
        // is validated and RETAINED exactly like a success: the exporter
        // computes pkg_origin before the fork, so a missing or mismatched
        // origin means the inventory is incomplete and the capture is
        // refused, never silently accepted.
        let (status, origin_field, raw) = if let Some(error) =
            cask.get("pkg_error").and_then(Value::as_str)
        {
            let origin = match cask.get("pkg_origin") {
                None | Some(Value::Null) => {
                    return Err(format!(
                        "integrity: pkg_error record {token:?} carries no \
                             pkg_origin provenance; refusing an incomplete inventory"
                    ));
                }
                Some(origin) => validate_pkg_origin(origin, tree, &token)
                    .map_err(|reason| format!("integrity: pkg_error record {token:?}: {reason}"))?,
            };
            (
                TargetStatus {
                    status: "excluded",
                    kind: None,
                    reason: Some("ruby-load-error".to_string()),
                    detail: Some(error.chars().take(200).collect::<String>()),
                    version: None,
                    homepage: None,
                    plan: None,
                },
                Some(origin),
                None,
            )
        } else {
            let mut status = tap_status(cask, system);
            // Provenance is verified against the actual source tree, not
            // the Ruby output: eligible entries REQUIRE a matching
            // origin; any hash/token mismatch downgrades the entry.
            let origin_field = match cask.get("pkg_origin") {
                None | Some(Value::Null) => {
                    if status.status == "eligible" {
                        status = excluded_status(
                            "origin-missing",
                            "the export carries no pkg_origin provenance",
                        );
                    }
                    None
                }
                Some(origin) => match validate_pkg_origin(origin, tree, &token) {
                    Ok(origin) => Some(origin),
                    Err(reason) => {
                        status = excluded_status("origin-mismatch", &reason);
                        None
                    }
                },
            };
            (status, origin_field, Some(cask))
        };
        record_for(token, status, origin_field, raw)?;
    }

    let mut inputs = BTreeMap::new();
    inputs.insert(
        source.to_string(),
        InputProvenance {
            // Client agreement: the public url is the canonical approved
            // origin; the exact download URL is separate, below.
            url: origin.to_string(),
            revision: revision.to_string(),
            // Hash of the pinned source ARCHIVE bytes (the codeload
            // tarball), never a handwaved capture hash.
            sha256: archive_sha256.to_string(),
            license: license.to_string(),
            kind: "raw-ruby-tap",
            raw: serde_json::json!({
                // Exact source blob download URL (pinned revision).
                "archiveUrl": archive_url_value,
                // Actual SHA-256 of the downloaded codeload archive.
                "archiveSha256": archive_sha256,
                // Actual SHA-256 of the export.json metadata capture
                // (the client checks it against result.metadata_sha256).
                "captureSha256": metadata_sha256,
                "envelopeSchema": EXPORT_SCHEMA,
            }),
        },
    );
    let flake_dir = output_dir.join("flake");
    std::fs::create_dir_all(&flake_dir)
        .map_err(|e| format!("cannot create {}: {e}", flake_dir.display()))?;
    emit::write_catalog(
        &flake_dir.join("catalog"),
        &emit::assemble_catalog(&inputs, &entries, &[system])?,
    )?;
    write_asset(&flake_dir.join("lib").join("builders.nix"), BUILDERS_NIX)?;
    write_asset(&flake_dir.join("lib").join("plan.py"), PLAN_PY)?;
    // Both vendor-source network fetches route through the shared
    // safe-fetch FOD; the generated flake is self-contained.
    write_asset(
        &flake_dir.join("lib").join("public-fetch.nix"),
        PUBLIC_FETCH_NIX,
    )?;
    write_asset(&flake_dir.join("lib").join("safe-fetch.py"), SAFE_FETCH_PY)?;
    // Shared projection helper + committed lock + license attributions:
    // the generated flake is standalone (evaluates with no outside
    // paths and no implicit lock write).
    write_asset(&flake_dir.join("lib").join("catalog.nix"), CATALOG_NIX)?;
    write_asset(&flake_dir.join("flake.lock"), FLAKE_LOCK)?;
    write_asset(
        &flake_dir.join("lib").join("brew-nix-LICENSE"),
        BREW_NIX_LICENSE,
    )?;
    write_asset(
        &flake_dir
            .join("lib")
            .join("licenses")
            .join("homebrew-cask-LICENSE.txt"),
        HOMEBREW_LICENSE,
    )?;
    let flake_nix = FLAKE_TEMPLATE.replace(
        "@DESCRIPTION@",
        &format!("pkg raw tap import: {source} at {revision} ({system}, native only)"),
    );
    write_asset(&flake_dir.join("flake.nix"), &flake_nix)?;

    // Complete asset inventory: the generated tree must carry every
    // asset the shared helper and builders reference. A missing file
    // fails the import instead of surfacing later at client eval.
    let inventory = [
        "flake.nix",
        "flake.lock",
        "catalog/catalog.json",
        "lib/catalog.nix",
        "lib/builders.nix",
        "lib/plan.py",
        "lib/public-fetch.nix",
        "lib/safe-fetch.py",
        "lib/brew-nix-LICENSE",
        "lib/licenses/homebrew-cask-LICENSE.txt",
    ];
    for asset in inventory {
        let path = flake_dir.join(asset);
        if !path.is_file() {
            return Err(format!(
                "integrity: generated flake asset {asset:?} is missing after the write"
            ));
        }
    }

    Ok(ImportResult {
        source: source.clone(),
        origin: origin.clone(),
        revision: revision.clone(),
        metadata_sha256: metadata_sha256.clone(),
        flake_path: flake_dir,
        eligible,
        excluded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn export_envelope() -> Value {
        json!({
            "schema": EXPORT_SCHEMA,
            "source": "example/foo",
            "revision": REVISION,
            "homebrew": {"revision": "cc9ff034b1d98cdd4061f68cf715bb967c483a8f"},
            "ruby": "4.0",
            "probe": "denied: host-read, host-write, loopback",
            "system": "aarch64-darwin",
            "files": {"total": 3, "ok": 2, "failed": 1},
            "casks": [
                {
                    "token": "good",
                    "name": ["Good"],
                    "desc": "A good cask.",
                    "homepage": "https://good.example",
                    "version": "1.0",
                    "url": "https://example.com/good.zip",
                    "sha256": "a".repeat(64),
                    "artifacts": [{"binary": ["good"]}],
                    "upstreamPlatformSupported": true,
                    "pkg_origin": {"path": "Casks/g/good.rb", "sha256": "b".repeat(64)}
                },
                {
                    "token": "badplat",
                    "name": ["Badplat"],
                    "version": "2",
                    "upstreamPlatformSupported": false,
                    "pkg_origin": {"path": "Casks/badplat.rb", "sha256": "b".repeat(64)}
                },
                {
                    "token": "broken",
                    "pkg_error": "syntax error, unexpected end",
                    "pkg_origin": {"path": "Casks/broken.rb", "sha256": "d".repeat(64)}
                }
            ]
        })
    }

    fn capture(dir: &Path, system: &str) -> ImportResult {
        let envelope = export_envelope();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let (export, metadata_sha256) = validate_export(&bytes, "example/foo", REVISION, system)
            .expect("fixture envelope validates");
        convert_capture(
            &Capture {
                source: "example/foo".to_string(),
                origin: "https://github.com/example/homebrew-foo".to_string(),
                revision: REVISION.to_string(),
                system: system.to_string(),
                export,
                metadata_sha256,
                archive_url: format!(
                    "https://codeload.github.com/example/homebrew-foo/tar.gz/{REVISION}"
                ),
                archive_sha256: "c".repeat(64),
                license: "NOASSERTION".to_string(),
                tree: BTreeMap::from([
                    ("Casks/g/good.rb".to_string(), "b".repeat(64)),
                    ("Casks/badplat.rb".to_string(), "b".repeat(64)),
                    ("Casks/broken.rb".to_string(), "d".repeat(64)),
                ]),
            },
            dir,
        )
        .expect("capture converts")
    }

    #[test]
    fn capture_writes_a_self_contained_native_only_flake_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let system = "aarch64-darwin";
        let result = capture(dir.path(), system);
        assert_eq!(result.source, "example/foo");
        assert_eq!(result.origin, "https://github.com/example/homebrew-foo");
        assert_eq!(result.eligible, 1);
        assert_eq!(result.excluded, 2);
        assert_eq!(result.flake_path, dir.path().join("flake"));
        assert!(result.flake_path.is_dir(), "flake_path is a DIRECTORY");

        let catalog: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("flake/catalog/catalog.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["schema"], "pkg-cask-catalog/4");
        assert_eq!(catalog["targets"], json!([system]));
        let input = &catalog["inputs"]["example/foo"];
        assert_eq!(input["url"], "https://github.com/example/homebrew-foo");
        assert_eq!(input["kind"], "raw-ruby-tap");
        // Top-level sha256 is the ARCHIVE hash; the capture hash is
        // separate raw metadata the client cross-checks.
        assert_eq!(input["sha256"], "c".repeat(64));
        assert_eq!(
            input["raw"]["archiveUrl"],
            format!("https://codeload.github.com/example/homebrew-foo/tar.gz/{REVISION}")
        );
        assert_eq!(input["raw"]["archiveSha256"], "c".repeat(64));
        assert_eq!(input["raw"]["captureSha256"], result.metadata_sha256);

        let good = &catalog["entries"]["example/foo/good"];
        assert_eq!(good["targets"][system]["status"], "eligible");
        // Flat upstream metadata retains name/version/homepage.
        assert_eq!(good["name"], "Good");
        assert_eq!(good["description"], "A good cask.");
        assert_eq!(good["version"], "1.0");
        assert_eq!(good["homepage"], "https://good.example");
        assert_eq!(good["origin"]["path"], "Casks/g/good.rb");
        let badplat = &catalog["entries"]["example/foo/badplat"];
        assert_eq!(badplat["targets"][system]["reason"], "unsupported-platform");
        assert_eq!(badplat["version"], "2");
        let broken = &catalog["entries"]["example/foo/broken"];
        assert_eq!(broken["targets"][system]["reason"], "ruby-load-error");
        // The load-error record keeps its VALIDATED provenance.
        assert_eq!(broken["origin"]["path"], "Casks/broken.rb");

        for asset in [
            "flake/flake.nix",
            "flake/flake.lock",
            "flake/lib/catalog.nix",
            "flake/lib/builders.nix",
            "flake/lib/plan.py",
            "flake/lib/public-fetch.nix",
            "flake/lib/safe-fetch.py",
            "flake/lib/brew-nix-LICENSE",
            "flake/lib/licenses/homebrew-cask-LICENSE.txt",
            "flake/catalog/catalog.json",
        ] {
            assert!(
                dir.path().join(asset).is_file(),
                "asset {asset} must exist in the flake tree"
            );
        }
        let flake = std::fs::read_to_string(dir.path().join("flake/flake.nix")).unwrap();
        assert!(!flake.contains("@DESCRIPTION@"));
        assert!(flake.contains("pkg raw tap import: example/foo"));
        // The wrapper delegates to the SHARED helper (same file the
        // official nix/casks/flake.nix imports) and is fully closed.
        assert!(flake.contains("import ./lib/catalog.nix"));
        let helper = std::fs::read_to_string(dir.path().join("flake/lib/catalog.nix")).unwrap();
        assert_eq!(
            helper,
            include_str!("../../../../nix/casks/lib/catalog.nix")
        );
        let lock = std::fs::read_to_string(dir.path().join("flake/flake.lock")).unwrap();
        assert!(lock.contains("567a49d1913ce81ac6e9582e3553dd90a955875f"));
    }

    /// Locate `nix` on PATH (guarded helper; the eval test skips
    /// without it).
    fn nix_on_path() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("nix"))
            .find(|exe| exe.is_file())
    }

    #[test]
    fn generated_flake_nix_evaluates_drv_path_offline_with_pinned_inputs() {
        // End-to-end eval contract, not just a Nix parse: the generated
        // standalone flake must evaluate one package's drvPath OFFLINE
        // with the committed lock (no implicit lock write, no outside
        // paths). Guarded: needs nix on PATH plus a cached pinned
        // nixpkgs; machines without the cache skip.
        let Some(nix) = nix_on_path() else {
            eprintln!("ignored (no nix on PATH): offline drvPath eval contract not exercised");
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        // Fixture envelope carries its own `system` identity field; the
        // host arch is irrelevant for a drvPath EVALUATION (nothing
        // builds), so pin the fixture system like the other tests.
        let system = "aarch64-darwin";
        let result = capture(dir.path(), system);
        assert_eq!(result.eligible, 1, "fixture must yield one eligible entry");
        let output = std::process::Command::new(&nix)
            .arg("--offline")
            .arg("eval")
            .arg("--raw")
            .arg(format!(".#packages.{system}.\"example/foo/good\".drvPath"))
            .current_dir(&result.flake_path)
            .output()
            .expect("nix eval spawns");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A host without the pinned nixpkgs cached cannot eval
            // offline; that is an environment gap, not a defect in the
            // generated tree. Any other failure is a real regression.
            assert!(
                stderr.contains("offline") || stderr.contains("unable to download"),
                "offline nix eval of the generated flake failed:\n{stderr}"
            );
            eprintln!(
                "ignored (pinned nixpkgs not cached offline): drvPath eval contract \
                 not exercised this run"
            );
            return;
        }
        let drv = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert!(
            drv.starts_with("/nix/store/") && drv.ends_with(".drv"),
            "drvPath must be a store derivation path, got {drv:?}"
        );
        // No lock was written by the eval: the committed copy is the
        // only lock in the generated tree.
        let lock = std::fs::read_to_string(result.flake_path.join("flake.lock"))
            .expect("committed lock survives the eval");
        assert!(lock.contains("567a49d1913ce81ac6e9582e3553dd90a955875f"));
    }

    #[test]
    fn pkg_error_records_require_validated_origin_provenance() {
        // The exporter computes pkg_origin before the fork; an error
        // record WITHOUT provenance means an incomplete inventory and
        // the capture is refused.
        let mut envelope = export_envelope();
        envelope["casks"][2]
            .as_object_mut()
            .unwrap()
            .remove("pkg_origin");
        let err = convert_with(&envelope).unwrap_err();
        assert!(err.contains("no pkg_origin"), "{err}");

        // Mismatched hash against the source tree is refused too.
        let mut envelope = export_envelope();
        envelope["casks"][2]["pkg_origin"]["sha256"] = json!("e".repeat(64));
        let err = convert_with(&envelope).unwrap_err();
        assert!(err.contains("does not match"), "{err}");

        // Wrong token spelling in the origin path is refused too.
        let mut envelope = export_envelope();
        envelope["casks"][2]["pkg_origin"]["path"] = json!("Casks/other.rb");
        let err = convert_with(&envelope).unwrap_err();
        assert!(err.contains("does not name token"), "{err}");
    }

    fn convert_with(envelope: &Value) -> Result<ImportResult, String> {
        let bytes = serde_json::to_vec(envelope).unwrap();
        let (export, metadata_sha256) =
            validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin")
                .expect("fixture envelope validates");
        let dir = tempfile::tempdir().expect("tempdir");
        convert_capture(
            &Capture {
                source: "example/foo".to_string(),
                origin: "https://github.com/example/homebrew-foo".to_string(),
                revision: REVISION.to_string(),
                system: "aarch64-darwin".to_string(),
                export,
                metadata_sha256,
                archive_url: format!(
                    "https://codeload.github.com/example/homebrew-foo/tar.gz/{REVISION}"
                ),
                archive_sha256: "c".repeat(64),
                license: "NOASSERTION".to_string(),
                tree: BTreeMap::from([
                    ("Casks/g/good.rb".to_string(), "b".repeat(64)),
                    ("Casks/badplat.rb".to_string(), "b".repeat(64)),
                    ("Casks/broken.rb".to_string(), "d".repeat(64)),
                    ("Casks/other.rb".to_string(), "d".repeat(64)),
                ]),
            },
            dir.path(),
        )
    }

    #[test]
    fn envelope_validation_fails_closed_on_identity_and_inventory() {
        let mut envelope = export_envelope();
        envelope["source"] = json!("other/bar");
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin").is_err());

        let mut envelope = export_envelope();
        envelope["probe"] = json!("unknown");
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin").is_err());

        let mut envelope = export_envelope();
        envelope["files"] = json!({"total": 9, "ok": 9, "failed": 0});
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin").is_err());
    }
}
