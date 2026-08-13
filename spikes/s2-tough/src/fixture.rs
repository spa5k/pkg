// Focused fixture builder for slice 1.
//
// Produces an entirely temporary, local, *tiny* signed TUF repository that
// models pkg's real channel target set (plans/02 §6.4 / §7):
//
//   top-level "targets" role (1-of-1) signs:
//     * descriptor.json           — the channel descriptor itself
//     * nix/<ver>/<sys>.tar.xz    — the managed-Nix runtime (one representative
//                                   target per system; tiny bytes)
//     * nix/<ver>/<sys>.assets.json — canonical static privileged-asset manifest
//                                     with stable group roles (one per system)
//     * installer/<sys>/<name>     — fixed product installer payloads
// Nixpkgs source is intentionally not a product TUF target; the descriptor
// authenticates rev/narHash and bundled Nix verifies the direct flake fetch.
//
//   delegated "index" role (1-of-1, paths = "index/**") signs:
//     * index/<seq>/<sys>.json.br — the disposable per-system catalog index
//                                   (all four systems)
//
// The descriptor's runtime/index sha256 values are computed from actual target
// bytes so they agree with TUF metadata (defense in depth, plans/02 §11); its
// Nixpkgs narHash is a separate well-formed synthetic flake pin. The fixture
// uses tough 0.24.0, `default-features =
// false`, `FilesystemTransport`, conservative `Limits`,
// `ExpirationEnforcement::Safe`, consistent snapshots, and a pre-created
// persistent datastore (required for rollback protection).
//
// Everything lives under a single ephemeral `TempDir`; no files escape it.

use crate::descriptor::{
    BuildMode, CACHE_NIXOS_ORG_KEY, CACHE_NIXOS_ORG_URL, ChannelDescriptor, Index, IndexEntry,
    NativeBuildEntry, NixRuntime, Nixpkgs, SUPPORTED_SYSTEMS, Substituters, SystemEntry,
};
use crate::keys::SignKey;
use crate::repo::{DelegationSpec, RepoBuilder, RepoPaths, hours_from_now, sha256_hex};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::path::PathBuf;
use tempfile::TempDir;

/// The Nix runtime version the fixture ships (matches `descriptor::NIX_RUNTIME_VERSION`).
const NIX_VERSION: &str = "2.24.10";
/// The pinned Nixpkgs revision in the fixture descriptor.
const NIXPKGS_REV: &str = "0123456789abcdef0123456789abcdef01234567";
/// Synthetic but well-formed SRI NAR hash for the separately fetched flake.
const NIXPKGS_NAR_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
/// The channel `sequence` (also used in `index/<seq>/...` target paths).
const SEQUENCE: u64 = 42;

/// All the on-disk state and expected bytes for a built fixture.
///
/// Holds the `TempDir` so the repository stays alive for the duration of the
/// test that owns this value.
pub struct Fixture {
    /// Keeps the temp repository + datastore alive. Named `_tmp` because it is
    /// only carried for its destructor.
    pub _tmp: TempDir,
    /// Signed-repository paths + the pinned trusted `root.json` bytes.
    pub repo: RepoPaths,
    /// A pre-created *persistent* datastore directory (required for rollback
    /// protection). Lives inside the temp dir.
    pub datastore: PathBuf,
    /// The exact bytes the `descriptor.json` target carries.
    pub descriptor_bytes: Vec<u8>,
    /// Representative managed-Nix runtime targets (one per system): (name, bytes).
    pub nix_targets: Vec<(String, Vec<u8>)>,
    /// Canonical static privileged-asset manifests (one per system).
    pub asset_manifest_targets: Vec<(String, Vec<u8>)>,
    /// Fixed installer payloads (three per system).
    pub installer_targets: Vec<(String, Vec<u8>)>,
    /// The four per-system index targets (delegated to the "index" role).
    pub index_targets: Vec<(String, Vec<u8>)>,
    /// The delegated role name holding the index targets.
    pub index_role: String,
}

impl Fixture {
    /// Convenience: the trusted root bytes to pin in a `RepositoryLoader`.
    pub fn root_bytes(&self) -> &[u8] {
        &self.repo.root_bytes
    }

    /// Convenience: metadata base URL for `RepositoryLoader`.
    pub fn metadata_url(&self) -> url::Url {
        self.repo.metadata_url()
    }

    /// Convenience: targets base URL for `RepositoryLoader`.
    pub fn targets_url(&self) -> url::Url {
        self.repo.targets_url()
    }
}

fn nix_target_name(system: &str) -> String {
    format!("nix/{NIX_VERSION}/{system}.tar.xz")
}

fn nix_target_bytes(system: &str) -> Vec<u8> {
    format!("managed nix runtime {NIX_VERSION} {system} (fixture)\n").into_bytes()
}

fn asset_manifest_target_name(system: &str) -> String {
    format!("nix/{NIX_VERSION}/{system}.assets.json")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetManifest<'a> {
    schema_version: u32,
    product: &'static str,
    system: &'a str,
    nix_version: &'static str,
    artifacts: Vec<AssetArtifact>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetArtifact {
    path: String,
    kind: &'static str,
    owner_uid: u32,
    group: &'static str,
    mode: Option<u32>,
    size: Option<u64>,
    sha256: Option<String>,
    target: Option<String>,
}

fn asset_manifest_target_bytes(system: &str, runtime_bytes: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&AssetManifest {
        schema_version: 1,
        product: "pkg",
        system,
        nix_version: NIX_VERSION,
        artifacts: vec![
            AssetArtifact {
                path: "/nix/store".to_string(),
                kind: "directory",
                owner_uid: 0,
                group: "buildUsers",
                mode: Some(0o1775),
                size: None,
                sha256: None,
                target: None,
            },
            AssetArtifact {
                path: format!("/opt/pkg/nix/{NIX_VERSION}/runtime.txt"),
                kind: "file",
                owner_uid: 0,
                group: "broker",
                mode: Some(0o550),
                size: Some(runtime_bytes.len() as u64),
                sha256: Some(format!("sha256:{}", sha256_hex(runtime_bytes))),
                target: None,
            },
        ],
    })
    .unwrap()
}

fn index_target_name(system: &str) -> String {
    format!("index/{SEQUENCE}/{system}.json.br")
}

fn installer_targets(system: &str) -> impl Iterator<Item = (String, Vec<u8>)> + '_ {
    ["pkg-root-helper", "pkg-nix-broker", "pkg"]
        .into_iter()
        .map(move |name| {
            (
                format!("installer/{system}/{name}"),
                format!("installer payload {name} {system}\n").into_bytes(),
            )
        })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogIndex<'a> {
    schema_version: u64,
    channel_seq: u64,
    system: &'a str,
    nixpkgs_rev: &'static str,
    generated_at: &'static str,
    source: &'static str,
    records: Vec<serde_json::Value>,
}

fn index_target_bytes(system: &str) -> Vec<u8> {
    let canonical = serde_json_canonicalizer::to_vec(&CatalogIndex {
        schema_version: 1,
        channel_seq: SEQUENCE,
        system,
        nixpkgs_rev: NIXPKGS_REV,
        generated_at: "2025-01-01T00:00:00Z",
        source: "self-built",
        records: Vec::new(),
    })
    .unwrap();
    let mut encoder = brotli::CompressorReader::new(Cursor::new(canonical), 4 * 1024, 5, 22);
    let mut compressed = Vec::new();
    encoder.read_to_end(&mut compressed).unwrap();
    compressed
}

/// Build the fixture: a fresh temp dir, a 1-of-1 signed repo, and a persistent
/// datastore. Async because signing the repository is async.
pub async fn build_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let datastore = tmp.path().join("datastore");
    std::fs::create_dir_all(&datastore).unwrap();

    // --- target bytes (computed first so the descriptor can cite real hashes) ---
    let nix_targets: Vec<(String, Vec<u8>)> = SUPPORTED_SYSTEMS
        .iter()
        .map(|sys| (nix_target_name(sys), nix_target_bytes(sys)))
        .collect();
    let asset_manifest_targets: Vec<(String, Vec<u8>)> = SUPPORTED_SYSTEMS
        .iter()
        .zip(&nix_targets)
        .map(|(sys, (_, runtime_bytes))| {
            (
                asset_manifest_target_name(sys),
                asset_manifest_target_bytes(sys, runtime_bytes),
            )
        })
        .collect();
    let index_targets: Vec<(String, Vec<u8>)> = SUPPORTED_SYSTEMS
        .iter()
        .map(|sys| (index_target_name(sys), index_target_bytes(sys)))
        .collect();
    let installer_targets: Vec<(String, Vec<u8>)> = SUPPORTED_SYSTEMS
        .iter()
        .flat_map(|system| installer_targets(system))
        .collect();

    // --- descriptor with declared hashes matching the target bytes ---
    //
    // Pair each SUPPORTED_SYSTEM directly with its already-built target tuple.
    // Do NOT parse the system name out of the target path: those paths carry
    // the Nix version (`nix/<ver>/<sys>.tar.xz`) or the sequence
    // (`index/<seq>/<sys>.json.br`) in path segment 1, so segment-1 parsing
    // would collapse all four systems onto a single map key. The system is the
    // SUPPORTED_SYSTEMS iteration variable; `nix_targets` and `index_targets`
    // are built in that same order, so the zip pairs each system with its exact
    // target tuple and yields exactly the four keys x86_64-linux, aarch64-linux,
    // x86_64-darwin, aarch64-darwin.
    let mut nix_per_system: BTreeMap<String, SystemEntry> = BTreeMap::new();
    for ((sys, (_, bytes)), (manifest_name, manifest_bytes)) in SUPPORTED_SYSTEMS
        .iter()
        .zip(&nix_targets)
        .zip(&asset_manifest_targets)
    {
        nix_per_system.insert(
            (*sys).to_string(),
            SystemEntry {
                url: format!(
                    "https://releases.nixos.org/nix/nix-{ver}/nix-{ver}-{sys}.tar.xz",
                    ver = NIX_VERSION,
                    sys = sys,
                ),
                sha256: sha256_hex(bytes),
                asset_manifest_target: manifest_name.clone(),
                asset_manifest_sha256: sha256_hex(manifest_bytes),
            },
        );
    }
    let mut index_per_system: BTreeMap<String, IndexEntry> = BTreeMap::new();
    for (sys, (name, bytes)) in SUPPORTED_SYSTEMS.iter().zip(&index_targets) {
        index_per_system.insert(
            (*sys).to_string(),
            IndexEntry {
                target: name.clone(),
                sha256: sha256_hex(bytes),
            },
        );
    }
    let mut native_local_builds: BTreeMap<String, NativeBuildEntry> = BTreeMap::new();
    for sys in SUPPORTED_SYSTEMS {
        native_local_builds.insert(
            (*sys).to_string(),
            NativeBuildEntry {
                mode: BuildMode::AllowWithGates,
            },
        );
    }
    let descriptor = ChannelDescriptor {
        schema_version: 1,
        channel: "pkg-stable-1".to_string(),
        policy_version: 1,
        sequence: SEQUENCE,
        expires_at: "2036-04-01T00:00:00Z".to_string(),
        supported_systems: SUPPORTED_SYSTEMS.iter().map(|s| (*s).to_string()).collect(),
        build_policy: crate::descriptor::BuildPolicy {
            native_local_builds,
        },
        nix_runtime: NixRuntime {
            version: NIX_VERSION.to_string(),
            per_system: nix_per_system,
        },
        nixpkgs: Nixpkgs {
            owner: "NixOS".to_string(),
            repo: "nixpkgs".to_string(),
            rev: NIXPKGS_REV.to_string(),
            nar_hash: NIXPKGS_NAR_HASH.to_string(),
        },
        index: Index {
            source: "self-built".to_string(),
            per_system: index_per_system,
        },
        substituters: Substituters {
            urls: vec![CACHE_NIXOS_ORG_URL.to_string()],
            trusted_public_keys: vec![CACHE_NIXOS_ORG_KEY.to_string()],
        },
    };
    let descriptor_bytes = descriptor.to_json_bytes();

    // --- build the signed repo ---
    let key = SignKey::generate();
    let long_expiry = hours_from_now(24 * 365 * 10);
    let spec = crate::repo::RootSpec::single(key, 1, long_expiry);

    let mut builder = RepoBuilder::new(repo_dir, spec);
    builder = builder
        .targets_expires(long_expiry)
        .snapshot_expires(long_expiry)
        .timestamp_expires(long_expiry)
        .delegated_expires(long_expiry);
    builder = builder.target("descriptor.json", descriptor_bytes.clone());
    for (name, bytes) in &nix_targets {
        builder = builder.target(name.clone(), bytes.clone());
    }
    for (name, bytes) in &asset_manifest_targets {
        builder = builder.target(name.clone(), bytes.clone());
    }
    for (name, bytes) in &installer_targets {
        builder = builder.target(name.clone(), bytes.clone());
    }
    builder = builder.delegated_role(DelegationSpec {
        role_name: "index".to_string(),
        key: SignKey::generate(),
        paths: vec!["index/**".to_string()],
        targets: index_targets
            .iter()
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect(),
    });

    let repo = builder.write().await;

    Fixture {
        _tmp: tmp,
        repo,
        datastore,
        descriptor_bytes,
        nix_targets,
        asset_manifest_targets,
        installer_targets,
        index_targets,
        index_role: "index".to_string(),
    }
}
