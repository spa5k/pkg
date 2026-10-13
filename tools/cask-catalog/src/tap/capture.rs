//! Raw-export envelope validation and conversion into the
//! self-contained flake tree.

use super::runtime::write_asset;
use super::{EXPORT_SCHEMA, ImportResult, sha256_hex};
use crate::MACOS_BASELINE;
use crate::classify::{ExclusionReason, PlatformGate, TargetDecision, classify_target};
use crate::effective;
use crate::emit::{self, Entry, InputProvenance, Origin};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Embedded builder assets (our own static files only).
const BUILDERS_NIX: &str = include_str!("../../../../nix/casks/lib/builders.nix");
const PLAN_PY: &str = include_str!("../../../../nix/casks/lib/plan.py");
const APP_RUNTIME_PY: &str = include_str!("../../../../nix/casks/lib/app-runtime.py");
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
const ASSETS: &[(&str, &str)] = &[
    ("lib/builders.nix", BUILDERS_NIX),
    ("lib/plan.py", PLAN_PY),
    ("lib/app-runtime.py", APP_RUNTIME_PY),
    ("lib/public-fetch.nix", PUBLIC_FETCH_NIX),
    ("lib/safe-fetch.py", SAFE_FETCH_PY),
    ("lib/catalog.nix", CATALOG_NIX),
    ("flake.lock", FLAKE_LOCK),
    ("lib/brew-nix-LICENSE", BREW_NIX_LICENSE),
    ("lib/licenses/homebrew-cask-LICENSE.txt", HOMEBREW_LICENSE),
];

/// export.json byte limit (bounded read BEFORE the JSON parse).
pub(super) const MAX_EXPORT_BYTES: usize = 32 * 1024 * 1024;

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
    let origin = Origin::checked(path, sha)
        .ok_or_else(|| format!("pkg_origin is malformed (path {path:?})"))?;
    let Some(actual) = tree.get(path) else {
        return Err(format!(
            "pkg_origin path {path:?} is not a file in the extracted source tree"
        ));
    };
    if actual != &origin.sha256 {
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
    Ok(origin)
}

// ---------------------------------------------------------------------------
// Envelope validation and capture conversion
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ExportEnvelope {
    schema: String,
    source: String,
    revision: String,
    system: String,
    probe: String,
    files: ExportInventory,
    casks: Vec<Value>,
}

#[derive(serde::Deserialize)]
struct ExportInventory {
    total: u64,
    ok: u64,
    failed: u64,
}

/// Validate the raw-export envelope bytes against the import identity.
pub(super) fn validate_export(
    bytes: &[u8],
    source: &str,
    revision: &str,
    system: &str,
) -> Result<(Vec<Value>, String), String> {
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(format!(
            "export is {} bytes; refusing envelopes over {MAX_EXPORT_BYTES} bytes",
            bytes.len()
        ));
    }
    let export: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("export envelope is malformed JSON: {e}"))?;
    if !export.is_object() || !export.get("files").is_some_and(Value::is_object) {
        return Err("export envelope and files inventory must be objects".to_string());
    }
    let export: ExportEnvelope = serde::Deserialize::deserialize(export)
        .map_err(|e| format!("export envelope is malformed JSON: {e}"))?;
    for (field, actual, expected, matches) in [
        (
            "schema",
            export.schema.as_str(),
            EXPORT_SCHEMA,
            export.schema == EXPORT_SCHEMA,
        ),
        (
            "source",
            export.source.as_str(),
            source,
            export.source.eq_ignore_ascii_case(source),
        ),
        (
            "revision",
            export.revision.as_str(),
            revision,
            export.revision.eq_ignore_ascii_case(revision),
        ),
        (
            "system",
            export.system.as_str(),
            system,
            export.system == system,
        ),
    ] {
        if !matches {
            return Err(format!(
                "export {field} {actual:?} does not match the requested {field} {expected:?}"
            ));
        }
    }
    if export.probe != "denied: host-read, host-write, loopback" {
        return Err(format!(
            "export probe marker {:?} is not the authoritative denied marker; refusing an unproven sandbox export",
            export.probe
        ));
    }
    let ExportInventory { total, ok, failed } = export.files;
    if total != export.casks.len() as u64 || ok.checked_add(failed) != Some(total) {
        return Err(format!(
            "export file inventory (total {total}, ok {ok}, failed {failed}) does not match the {} cask records",
            export.casks.len()
        ));
    }
    if ok == 0 {
        return Err("every cask file failed to export; refusing an all-error capture".to_string());
    }
    Ok((export.casks, sha256_hex(bytes)))
}

/// Classify one raw-exported cask for the single native target.
fn tap_status(cask: &Value, system: &str) -> TargetDecision {
    let Some(supported) = cask
        .get("upstreamPlatformSupported")
        .and_then(Value::as_bool)
    else {
        return TargetDecision::bare_with_detail(
            ExclusionReason::MalformedRecord,
            "no upstreamPlatformSupported boolean".to_string(),
        );
    };
    // Flat upstream metadata: the exporter emits every field
    // (name/version/url/...) directly on the cask record.
    classify_target(cask, system, MACOS_BASELINE, PlatformGate::Bool(supported))
}

fn excluded_status(reason: ExclusionReason, detail: &str) -> TargetDecision {
    TargetDecision::bare_with_detail(reason, detail.chars().take(200).collect::<String>())
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
    pub(super) casks: Vec<Value>,
    pub(super) metadata_sha256: String,
    pub(super) archive_url: String,
    pub(super) archive_sha256: String,
    pub(super) license: String,
    /// SHA-256 inventory of the extracted tap tree; every cask origin
    /// is validated against it (path, hash, and token spelling).
    pub(super) tree: BTreeMap<String, String>,
}

fn input_provenance(capture: &Capture) -> InputProvenance {
    InputProvenance {
        url: capture.origin.clone(),
        revision: capture.revision.clone(),
        sha256: capture.archive_sha256.clone(),
        license: capture.license.clone(),
        kind: "raw-ruby-tap",
        raw: serde_json::json!({
            "archiveUrl": capture.archive_url,
            "archiveSha256": capture.archive_sha256,
            "captureSha256": capture.metadata_sha256,
            "envelopeSchema": EXPORT_SCHEMA,
        }),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one conversion loop preserves origin, token, and duplicate refusal order"
)]
pub(super) fn convert_capture(
    capture: &Capture,
    output_dir: &Path,
) -> Result<ImportResult, String> {
    let Capture {
        source,
        origin,
        revision,
        system,
        casks,
        metadata_sha256,
        tree,
        ..
    } = capture;
    let mut entries = BTreeMap::new();
    let mut eligible = 0;
    let mut excluded = 0;

    for cask in casks {
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
                TargetDecision::bare_with_detail(
                    ExclusionReason::RubyLoadError,
                    error.chars().take(200).collect::<String>(),
                ),
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
                    if status.is_eligible() {
                        status = excluded_status(
                            ExclusionReason::OriginMissing,
                            "the export carries no pkg_origin provenance",
                        );
                    }
                    None
                }
                Some(origin) => match validate_pkg_origin(origin, tree, &token) {
                    Ok(origin) => Some(origin),
                    Err(reason) => {
                        status = excluded_status(ExclusionReason::OriginMismatch, &reason);
                        None
                    }
                },
            };
            (status, origin_field, Some(cask))
        };
        if !crate::token::is_valid_token(&token) {
            return Err(format!(
                "integrity: exported token {token:?} is not a usable catalog key"
            ));
        }
        let id = emit::qualified_id(source, &token);
        if entries.contains_key(&id) {
            return Err(format!("integrity: duplicate token {token} in export"));
        }
        let (name, description, version, homepage) = match raw {
            Some(record) => (
                effective::name_field(record),
                effective::str_field(record, "desc"),
                effective::str_field(record, "version"),
                effective::str_field(record, "homepage"),
            ),
            None => (None, None, None, None),
        };
        if status.is_eligible() {
            eligible += 1;
        } else {
            excluded += 1;
        }
        let mut targets = BTreeMap::new();
        targets.insert(system.clone(), status);
        entries.insert(
            id,
            Entry {
                source: source.clone(),
                token,
                name,
                description,
                version,
                homepage,
                origin: origin_field,
                targets,
            },
        );
    }

    let inputs = BTreeMap::from([(source.clone(), input_provenance(capture))]);
    write_flake_tree(capture, output_dir, &inputs, &entries)?;
    let flake_dir = output_dir.join("flake");
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

fn write_flake_tree(
    capture: &Capture,
    output_dir: &Path,
    inputs: &BTreeMap<String, InputProvenance>,
    entries: &BTreeMap<String, Entry>,
) -> Result<(), String> {
    let Capture {
        source,
        revision,
        system,
        ..
    } = capture;
    let flake_dir = output_dir.join("flake");
    std::fs::create_dir_all(&flake_dir)
        .map_err(|e| format!("cannot create {}: {e}", flake_dir.display()))?;
    emit::write_catalog(
        &flake_dir.join("catalog"),
        &emit::assemble_catalog(inputs, entries, &[system])?,
    )?;
    for (path, contents) in ASSETS {
        write_asset(&flake_dir.join(path), contents)?;
    }
    let flake_nix = FLAKE_TEMPLATE.replace(
        "@DESCRIPTION@",
        &format!("pkg raw tap import: {source} at {revision} ({system}, native only)"),
    );
    write_asset(&flake_dir.join("flake.nix"), &flake_nix)?;

    // Complete asset inventory: the generated tree must carry every
    // asset the shared helper and builders reference. A missing file
    // fails the import instead of surfacing later at client eval.
    for asset in ASSETS
        .iter()
        .map(|(path, _)| *path)
        .chain(["flake.nix", "catalog/catalog.json"])
    {
        let path = flake_dir.join(asset);
        if !path.is_file() {
            return Err(format!(
                "integrity: generated flake asset {asset:?} is missing after the write"
            ));
        }
    }

    Ok(())
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
        let (casks, metadata_sha256) = validate_export(&bytes, "example/foo", REVISION, system)
            .expect("fixture envelope validates");
        convert_capture(
            &Capture {
                source: "example/foo".to_string(),
                origin: "https://github.com/example/homebrew-foo".to_string(),
                revision: REVISION.to_string(),
                system: system.to_string(),
                casks,
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
            "flake/lib/app-runtime.py",
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
        assert!(lock.contains("8edc0c72e3a38faf5434e40d1f19431125fe4b30"));
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
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "Nix derivation paths use the exact .drv suffix"
    )]
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
        assert!(lock.contains("8edc0c72e3a38faf5434e40d1f19431125fe4b30"));
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

    #[test]
    fn origin_integrity_failure_precedes_duplicate_token_failure() {
        let mut envelope = export_envelope();
        envelope["casks"][2]["token"] = json!("good");
        envelope["casks"][2]["pkg_origin"] =
            json!({"path": "Casks/g/good.rb", "sha256": "e".repeat(64)});
        assert_eq!(
            convert_with(&envelope).unwrap_err(),
            "integrity: pkg_error record \"good\": pkg_origin sha256 for \"Casks/g/good.rb\" does not match the extracted source tree"
        );

        envelope["casks"][2]["pkg_origin"]["sha256"] = json!("B".repeat(64));
        assert_eq!(
            convert_with(&envelope).unwrap_err(),
            "integrity: duplicate token good in export"
        );
    }

    fn convert_with(envelope: &Value) -> Result<ImportResult, String> {
        let bytes = serde_json::to_vec(envelope).unwrap();
        let (casks, metadata_sha256) =
            validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin")
                .expect("fixture envelope validates");
        let dir = tempfile::tempdir().expect("tempdir");
        convert_capture(
            &Capture {
                source: "example/foo".to_string(),
                origin: "https://github.com/example/homebrew-foo".to_string(),
                revision: REVISION.to_string(),
                system: "aarch64-darwin".to_string(),
                casks,
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
    fn envelope_required_fields_and_counts_fail_closed() {
        for field in [
            "schema", "source", "revision", "system", "probe", "files", "casks",
        ] {
            for replacement in [None, Some(json!(false))] {
                let mut envelope = export_envelope();
                if let Some(value) = replacement {
                    envelope[field] = value;
                } else {
                    envelope.as_object_mut().unwrap().remove(field);
                }
                assert!(
                    validate_export(
                        &serde_json::to_vec(&envelope).unwrap(),
                        "example/foo",
                        REVISION,
                        "aarch64-darwin"
                    )
                    .is_err(),
                    "{field}"
                );
            }
        }
        for files in [
            json!({"total": 3, "ok": 0, "failed": 3}),
            json!({"total": 3, "ok": u64::MAX, "failed": 4}),
            json!({"total": 3, "ok": 2}),
            json!({"total": 3, "ok": "2", "failed": 1}),
        ] {
            let mut envelope = export_envelope();
            envelope["files"] = files;
            assert!(
                validate_export(
                    &serde_json::to_vec(&envelope).unwrap(),
                    "example/foo",
                    REVISION,
                    "aarch64-darwin"
                )
                .is_err()
            );
        }
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

    #[test]
    fn envelope_and_inventory_require_objects() {
        let envelope = export_envelope();
        let positional = json!([
            envelope["schema"],
            envelope["source"],
            envelope["revision"],
            envelope["system"],
            envelope["probe"],
            envelope["files"],
            envelope["casks"]
        ]);
        let mut inventory_array = envelope.clone();
        inventory_array["files"] = json!([
            envelope["files"]["total"],
            envelope["files"]["ok"],
            envelope["files"]["failed"]
        ]);
        for candidate in [positional, inventory_array] {
            let bytes = serde_json::to_vec(&candidate).unwrap();
            assert!(
                validate_export(&bytes, "example/foo", REVISION, "aarch64-darwin").is_err(),
                "positional objects must be refused: {candidate}"
            );
        }
    }

    #[test]
    fn envelope_retains_last_duplicate_key_values() {
        let envelope = export_envelope();
        let text = serde_json::to_string(&envelope).unwrap();
        let expected_casks =
            validate_export(text.as_bytes(), "example/foo", REVISION, "aarch64-darwin")
                .unwrap()
                .0;
        let mut envelopes = Vec::new();
        for (key, wrong) in [
            ("schema", json!("wrong")),
            ("source", json!(null)),
            ("revision", json!(0)),
            ("system", json!(false)),
            ("probe", json!("allowed")),
            ("files", json!([])),
            ("casks", json!([])),
        ] {
            envelopes.push(format!(
                "{{{}:{wrong},{}",
                serde_json::to_string(key).unwrap(),
                &text[1..]
            ));
        }
        for key in ["total", "ok", "failed"] {
            envelopes.push(text.replacen(
                "\"files\":{",
                &format!("\"files\":{{\"{key}\":null,"),
                1,
            ));
        }
        for candidate in envelopes {
            let (casks, digest) = validate_export(
                candidate.as_bytes(),
                "example/foo",
                REVISION,
                "aarch64-darwin",
            )
            .unwrap_or_else(|error| panic!("last duplicate values must be retained: {error}"));
            assert_eq!(casks, expected_casks);
            assert_eq!(digest, sha256_hex(candidate.as_bytes()));
        }
    }
}
