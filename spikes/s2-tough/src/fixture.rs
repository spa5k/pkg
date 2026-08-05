// Focused fixture builder for slice 1.
//
// Produces an entirely temporary, local, *tiny* signed TUF repository that
// models pkg's real channel target set (plans/02 §6.4 / §7):
//
//   top-level "targets" role (1-of-1) signs:
//     * descriptor.json           — the channel descriptor itself
//     * nix/<ver>/<sys>.tar.xz    — the managed-Nix runtime (one representative
//                                   target per system; tiny bytes)
//     * nixpkgs/<rev>/src.tar.gz  — the pinned Nixpkgs source
//
//   delegated "index" role (1-of-1, paths = "index/**") signs:
//     * index/<seq>/<sys>.json.br — the disposable per-system catalog index
//                                   (all four systems)
//
// The descriptor's declared sha256/narHash values are computed from the actual
// target bytes so they agree with the TUF-authenticated hashes (defense in
// depth, plans/02 §11). The fixture uses tough 0.24.0, `default-features =
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
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::TempDir;

/// The Nix runtime version the fixture ships (matches `descriptor::NIX_RUNTIME_VERSION`).
const NIX_VERSION: &str = "2.24.10";
/// The pinned Nixpkgs revision in the fixture descriptor.
const NIXPKGS_REV: &str = "abc123";
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
    /// The Nixpkgs source target: (name, bytes).
    pub nixpkgs_target: (String, Vec<u8>),
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

fn nixpkgs_target() -> (String, Vec<u8>) {
    (
        format!("nixpkgs/{NIXPKGS_REV}/src.tar.gz"),
        format!("nixpkgs source tarball {NIXPKGS_REV} (fixture)\n").into_bytes(),
    )
}

fn index_target_name(system: &str) -> String {
    format!("index/{SEQUENCE}/{system}.json.br")
}

fn index_target_bytes(system: &str) -> Vec<u8> {
    format!("[ fixture catalog index for {system} ]\n").into_bytes()
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
    let nixpkgs_target = nixpkgs_target();
    let index_targets: Vec<(String, Vec<u8>)> = SUPPORTED_SYSTEMS
        .iter()
        .map(|sys| (index_target_name(sys), index_target_bytes(sys)))
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
    for (sys, (_, bytes)) in SUPPORTED_SYSTEMS.iter().zip(&nix_targets) {
        nix_per_system.insert(
            (*sys).to_string(),
            SystemEntry {
                url: format!(
                    "https://releases.nixos.org/nix/nix-{ver}/nix-{ver}-{sys}.tar.xz",
                    ver = NIX_VERSION,
                    sys = sys,
                ),
                sha256: sha256_hex(bytes),
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
        expires_at: "2025-04-01T00:00:00Z".to_string(),
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
            nar_hash: format!("sha256-{}", sha256_hex(&nixpkgs_target.1)),
            source_target: nixpkgs_target.0.clone(),
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
    let spec = crate::repo::RootSpec::single(key, 1, hours_from_now(24 * 30));

    let mut builder = RepoBuilder::new(repo_dir, spec);
    builder = builder.target("descriptor.json", descriptor_bytes.clone());
    for (name, bytes) in &nix_targets {
        builder = builder.target(name.clone(), bytes.clone());
    }
    let (nixpkgs_name, nixpkgs_bytes) = &nixpkgs_target;
    builder = builder.target(nixpkgs_name.clone(), nixpkgs_bytes.clone());

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
        nixpkgs_target,
        index_targets,
        index_role: "index".to_string(),
    }
}
