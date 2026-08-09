// Publisher-side fixture construction for the S2 spike.
//
// This module builds *signed TUF repositories* on a local filesystem so the
// spike's tests can exercise `tough`'s real client verification path
// (`RepositoryLoader`). It performs NO bespoke cryptography:
//
//   * The bootstrap `root.json` is assembled from `tough::schema` types and
//     signed through `tough::sign::Sign` (see `keys::sign_role`). This is the
//     single narrow test-publisher boundary documented in `keys.rs`; there are
//     no direct cryptographic signing calls anywhere.
//   * The targets / snapshot / timestamp / delegated-targets roles are signed
//     entirely by `tough::editor::RepositoryEditor` reading keys through
//     `tough::key_source::LocalKeySource` (tough's public key source). The
//     editor calls `tough::sign::parse_keypair` + `Sign` internally.
//
// All PKCS#8 key files live ONLY inside the repo's ephemeral temp `keys/`
// directory and are deleted with the `TempDir`.

use crate::descriptor::ChannelDescriptor;
use crate::keys::{SignKey, sign_role};
use aws_lc_rs::digest::{SHA256, digest};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use tough::editor::RepositoryEditor;
use tough::key_source::{KeySource, LocalKeySource};
use tough::schema::decoded::Decoded;
use tough::schema::decoded::Hex;
use tough::schema::key::Key;
use tough::schema::{Hashes, PathPattern, PathSet, RoleKeys, RoleType, Root, Signed, Target};
use url::Url;

/// The authorized keys + threshold for one top-level role.
#[derive(Clone)]
pub struct RoleSpec {
    pub keys: Vec<SignKey>,
    pub threshold: NonZeroU64,
}

impl RoleSpec {
    /// 1-of-1 with a single key.
    pub fn single(key: SignKey) -> Self {
        Self {
            keys: vec![key],
            threshold: NonZeroU64::new(1).unwrap(),
        }
    }

    /// `t`-of-N with the given keys.
    pub fn threshold_of(keys: Vec<SignKey>, t: u64) -> Self {
        Self {
            keys,
            threshold: NonZeroU64::new(t).expect("threshold >= 1"),
        }
    }
}

/// The full root-role specification used to bootstrap a repository.
#[derive(Clone)]
pub struct RootSpec {
    pub root: RoleSpec,
    pub targets: RoleSpec,
    pub snapshot: RoleSpec,
    pub timestamp: RoleSpec,
    pub consistent_snapshot: bool,
    pub version: u64,
    pub expires: jiff::Timestamp,
}

impl RootSpec {
    /// A simple 1-of-1 repo across all four roles with a single key.
    pub fn single(key: SignKey, version: u64, expires: jiff::Timestamp) -> Self {
        Self {
            root: RoleSpec::single(key.clone()),
            targets: RoleSpec::single(key.clone()),
            snapshot: RoleSpec::single(key.clone()),
            timestamp: RoleSpec::single(key),
            consistent_snapshot: true,
            version,
            expires,
        }
    }
}

fn role_keys(rs: &RoleSpec) -> RoleKeys {
    RoleKeys {
        keyids: rs.keys.iter().map(|k| k.key_id().clone()).collect(),
        threshold: rs.threshold,
        _extra: HashMap::new(),
    }
}

/// Build a signed `Root` from a spec, signing it with all of the root role's
/// keys via `tough::sign::Sign` (so the trusted initial root comfortably meets
/// its own threshold). This is the documented bootstrap boundary.
pub async fn build_root(spec: &RootSpec) -> Signed<Root> {
    let mut keys: HashMap<Decoded<Hex>, Key> = HashMap::new();
    for rs in [&spec.root, &spec.targets, &spec.snapshot, &spec.timestamp] {
        for k in &rs.keys {
            keys.entry(k.key_id().clone())
                .or_insert_with(|| k.tuf_key().clone());
        }
    }
    let mut roles = HashMap::new();
    roles.insert(RoleType::Root, role_keys(&spec.root));
    roles.insert(RoleType::Targets, role_keys(&spec.targets));
    roles.insert(RoleType::Snapshot, role_keys(&spec.snapshot));
    roles.insert(RoleType::Timestamp, role_keys(&spec.timestamp));

    let root = Root {
        spec_version: "1.0.0".to_string(),
        consistent_snapshot: spec.consistent_snapshot,
        version: NonZeroU64::new(spec.version).expect("root version >= 1"),
        expires: spec.expires,
        keys,
        roles,
        _extra: HashMap::new(),
    };
    let signing: Vec<&SignKey> = spec.root.keys.iter().collect();
    sign_role(root, &signing).await
}

/// Serialize a `Signed<Root>` to pretty JSON bytes with a trailing newline,
/// matching the on-disk form `tough`'s editor writes.
pub fn root_json_bytes(root: &Signed<Root>) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(root).expect("serialize root");
    bytes.push(b'\n');
    bytes
}

/// SHA-256 of `bytes` (raw). This is CONTENT hashing for target metadata
/// (length + sha256), mirroring `tough::schema::Target::from_path`; it is NOT a
/// signature.
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    digest(&SHA256, bytes).as_ref().to_vec()
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha256(bytes))
}

/// Build a `Target` (length + sha256) directly from in-memory bytes.
pub fn target_from_bytes(bytes: &[u8]) -> Target {
    Target {
        length: bytes.len() as u64,
        hashes: Hashes {
            sha256: sha256(bytes).into(),
            _extra: HashMap::new(),
        },
        custom: HashMap::new(),
        _extra: HashMap::new(),
    }
}

/// A delegated role owned by `key`, governing `paths` and signing `targets`
/// (1-of-1, the only shape slice 1 needs; slice 2 extends this).
#[derive(Clone)]
pub struct DelegationSpec {
    pub role_name: String,
    pub key: SignKey,
    pub paths: Vec<String>,
    pub targets: Vec<(String, Vec<u8>)>,
}

/// Where a built repository lives on disk + the trusted root bytes to pin.
#[derive(Clone)]
pub struct RepoPaths {
    pub repo_dir: PathBuf,
    pub metadata_dir: PathBuf,
    pub targets_dir: PathBuf,
    pub root_bytes: Vec<u8>,
    pub consistent_snapshot: bool,
}

impl RepoPaths {
    pub fn metadata_url(&self) -> Url {
        Url::from_directory_path(&self.metadata_dir).unwrap()
    }
    pub fn targets_url(&self) -> Url {
        Url::from_directory_path(&self.targets_dir).unwrap()
    }
}

/// Write `key`'s PKCS#8 to `keys_dir/<keyid>.key` and return the path.
fn write_key_file(keys_dir: &Path, key: &SignKey) -> PathBuf {
    let path = keys_dir.join(format!("{}.key", key.key_id_hex()));
    std::fs::write(&path, key.pkcs8()).unwrap();
    path
}

/// Write a target's bytes to the targets dir with the correct consistent-
/// snapshot naming (`{sha256}.{name}` when consistent, else `{name}`), creating
/// parent directories for path-like names. This matches the filename tough's
/// client fetches (lib.rs: `format!("{sha}.{name}")`).
fn write_target_file(targets_dir: &Path, name: &str, bytes: &[u8], consistent_snapshot: bool) {
    let file_name = if consistent_snapshot {
        format!("{}.{}", sha256_hex(bytes), name)
    } else {
        name.to_string()
    };
    let path = targets_dir.join(&file_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
}

/// `n` hours from now as a `jiff::Timestamp` (Timestamp arithmetic requires
/// units of hours or smaller).
pub fn hours_from_now(n: i64) -> jiff::Timestamp {
    jiff::Timestamp::now() + jiff::Span::new().try_hours(n).unwrap()
}

/// A small TUF repository builder.
pub struct RepoBuilder {
    repo_dir: PathBuf,
    metadata_dir: PathBuf,
    targets_dir: PathBuf,
    spec: RootSpec,
    top_targets: Vec<(String, Vec<u8>)>,
    delegation: Option<DelegationSpec>,
    targets_version: NonZeroU64,
    snapshot_version: NonZeroU64,
    timestamp_version: NonZeroU64,
    targets_expires: jiff::Timestamp,
    snapshot_expires: jiff::Timestamp,
    timestamp_expires: jiff::Timestamp,
    delegated_version: NonZeroU64,
    delegated_expires: jiff::Timestamp,
}

impl RepoBuilder {
    /// Create a new builder under `repo_dir` (a fresh, empty temp dir). The
    /// bootstrap `{version}.root.json` is written during `write()`.
    pub fn new(repo_dir: PathBuf, spec: RootSpec) -> Self {
        let metadata_dir = repo_dir.join("metadata");
        let targets_dir = repo_dir.join("targets");
        std::fs::create_dir_all(&metadata_dir).unwrap();
        std::fs::create_dir_all(&targets_dir).unwrap();
        Self {
            repo_dir,
            metadata_dir,
            targets_dir,
            spec,
            top_targets: Vec::new(),
            delegation: None,
            targets_version: NonZeroU64::new(1).unwrap(),
            snapshot_version: NonZeroU64::new(1).unwrap(),
            timestamp_version: NonZeroU64::new(1).unwrap(),
            targets_expires: hours_from_now(24 * 30),
            snapshot_expires: hours_from_now(24 * 30),
            timestamp_expires: hours_from_now(24),
            delegated_version: NonZeroU64::new(1).unwrap(),
            delegated_expires: hours_from_now(24 * 30),
        }
    }

    /// Add a top-level target from in-memory bytes (file is written under
    /// `targets/` with the correct consistent-snapshot naming).
    pub fn target(mut self, name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.top_targets.push((name.into(), bytes.into()));
        self
    }

    /// Add a delegated role with its own targets. The delegated role's target
    /// names must match one of `paths` (glob, no `/` crossing by `*`; use `**`
    /// to cross segments).
    pub fn delegated_role(mut self, spec: DelegationSpec) -> Self {
        self.delegation = Some(spec);
        self
    }

    pub fn targets_version(mut self, v: u64) -> Self {
        self.targets_version = NonZeroU64::new(v).unwrap();
        self
    }
    pub fn snapshot_version(mut self, v: u64) -> Self {
        self.snapshot_version = NonZeroU64::new(v).unwrap();
        self
    }
    pub fn timestamp_version(mut self, v: u64) -> Self {
        self.timestamp_version = NonZeroU64::new(v).unwrap();
        self
    }
    pub fn targets_expires(mut self, t: jiff::Timestamp) -> Self {
        self.targets_expires = t;
        self
    }
    pub fn snapshot_expires(mut self, t: jiff::Timestamp) -> Self {
        self.snapshot_expires = t;
        self
    }
    pub fn timestamp_expires(mut self, t: jiff::Timestamp) -> Self {
        self.timestamp_expires = t;
        self
    }

    /// Set the delegated targets-role expiration.
    pub fn delegated_expires(mut self, t: jiff::Timestamp) -> Self {
        self.delegated_expires = t;
        self
    }

    /// Assemble, sign, and write the repository. Returns on-disk paths + the
    /// trusted root bytes. Targets/snapshot/timestamp (and the delegated role)
    /// are signed by `RepositoryEditor` + `LocalKeySource`; the bootstrap root
    /// is signed via `tough::sign::Sign`.
    pub async fn write(self) -> RepoPaths {
        let consistent_snapshot = self.spec.consistent_snapshot;

        // 1. Persist every signing key's PKCS#8 into the repo's ephemeral
        //    `keys/` dir so tough's public `LocalKeySource` can read them.
        let keys_dir = self.repo_dir.join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        // Union of targets+snapshot+timestamp keys (deduped) for the editor's
        // `sign`/`sign_targets_editor` calls (the editor signs all three roles
        // with the same key slice and filters per-role).
        let mut editor_keys: Vec<SignKey> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for k in self
            .spec
            .targets
            .keys
            .iter()
            .chain(self.spec.snapshot.keys.iter())
            .chain(self.spec.timestamp.keys.iter())
        {
            if seen.insert(k.key_id_hex()) {
                editor_keys.push(k.clone());
            }
        }
        let editor_paths: Vec<PathBuf> = editor_keys
            .iter()
            .map(|k| write_key_file(&keys_dir, k))
            .collect();
        let editor_sources: Vec<Box<dyn KeySource>> = editor_paths
            .into_iter()
            .map(|p| Box::new(LocalKeySource { path: p }) as Box<dyn KeySource>)
            .collect();

        let deleg_sources: Vec<Box<dyn KeySource>> = match &self.delegation {
            Some(d) => {
                let p = write_key_file(&keys_dir, &d.key);
                vec![Box::new(LocalKeySource { path: p }) as Box<dyn KeySource>]
            }
            None => Vec::new(),
        };

        // 2. Build + sign the bootstrap root.json (Sign trait) and write it.
        let root = build_root(&self.spec).await;
        let root_bytes = root_json_bytes(&root);
        let root_path = self
            .metadata_dir
            .join(format!("{}.root.json", self.spec.version));
        tokio::fs::write(&root_path, &root_bytes).await.unwrap();

        // 3. RepositoryEditor reads the signed root, then signs targets/
        //    snapshot/timestamp (+ delegated role) via LocalKeySource.
        let mut editor = RepositoryEditor::new(&root_path).await.unwrap();
        editor
            .targets_version(self.targets_version)
            .unwrap()
            .targets_expires(self.targets_expires)
            .unwrap()
            .snapshot_version(self.snapshot_version)
            .snapshot_expires(self.snapshot_expires)
            .timestamp_version(self.timestamp_version)
            .timestamp_expires(self.timestamp_expires);

        for (name, bytes) in &self.top_targets {
            write_target_file(&self.targets_dir, name, bytes, consistent_snapshot);
            editor
                .add_target(name.as_str(), target_from_bytes(bytes))
                .unwrap();
        }

        if let Some(deleg) = self.delegation.clone() {
            let paths = PathSet::Paths(
                deleg
                    .paths
                    .iter()
                    .map(|p| PathPattern::new(p.clone()).unwrap())
                    .collect(),
            );
            editor
                .delegate_role(
                    deleg.role_name.as_str(),
                    &deleg_sources,
                    paths,
                    false, // non-terminating
                    NonZeroU64::new(1).unwrap(),
                    self.delegated_expires,
                    self.delegated_version,
                )
                .await
                .unwrap();
            // Commit the top-level targets (records the delegation).
            editor.sign_targets_editor(&editor_sources).await.unwrap();
            // Switch to the delegated role and add its targets.
            editor
                .change_delegated_targets(deleg.role_name.as_str())
                .unwrap();
            for (name, bytes) in &deleg.targets {
                write_target_file(&self.targets_dir, name, bytes, consistent_snapshot);
                editor
                    .add_target(name.as_str(), target_from_bytes(bytes))
                    .unwrap();
            }
            editor
                .targets_version(self.delegated_version)
                .unwrap()
                .targets_expires(self.delegated_expires)
                .unwrap();
            // Commit the delegated role's targets.
            editor.sign_targets_editor(&deleg_sources).await.unwrap();
        }

        // Sign snapshot + timestamp (and re-affirm top-level targets) via the
        // editor. If a delegation was committed above, the targets editor is
        // already flushed, so `sign` only signs snapshot/timestamp here.
        let signed = editor.sign(&editor_sources).await.unwrap();
        signed.write(&self.metadata_dir).await.unwrap();

        RepoPaths {
            repo_dir: self.repo_dir,
            metadata_dir: self.metadata_dir,
            targets_dir: self.targets_dir,
            root_bytes,
            consistent_snapshot,
        }
    }
}

/// Read a `descriptor.json` TUF target out of a verified repository and parse
/// it as a `ChannelDescriptor`. This is the demo "product consumes verified
/// policy bytes" path.
pub async fn read_descriptor(
    repo: &tough::Repository,
) -> Result<ChannelDescriptor, serde_json::Error> {
    use tough::TargetName;
    let bytes =
        crate::verify::read_target_fully(repo, &TargetName::new("descriptor.json").unwrap())
            .await
            .unwrap()
            .expect("descriptor.json target present");
    serde_json::from_slice(&bytes)
}
