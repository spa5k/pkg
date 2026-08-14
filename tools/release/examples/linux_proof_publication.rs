//! Build an ephemeral signed publication for the Linux product proof.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Cursor, Read as _, Write as _};
use std::num::NonZeroU64;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use async_trait::async_trait;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::Ed25519KeyPair;
use jiff::Timestamp;
use olpc_cjson::CanonicalFormatter;
use pkg_core::{ChannelSequence, NixpkgsRevision, System};
use pkg_index::{BuildMetadata, build_index};
use pkg_nix::{NixVersion, build_upstream_runtime_asset_manifest};
use pkg_release::{
    Approval, MetadataPolicy, ReleaseAuthority, ReleaseAuthorization, ReleaseManifest,
    ValidationError, sign_channel,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tough::key_source::KeySource;
use tough::schema::decoded::{Decoded, Hex};
use tough::schema::key::Key;
use tough::schema::{RoleKeys, RoleType, Root, Signature, Signed};
use tough::sign::{Sign, parse_keypair};

type AnyError = Box<dyn Error + Send + Sync>;

const NIX_VERSION: &str = "2.34.8";
const NIXPKGS_REVISION: &str = "a62e6edd6d5e1fa0329b8653c801147986f8d446";
const NIXPKGS_NAR_HASH: &str = "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";
const SYSTEMS: [&str; 4] = [
    "aarch64-darwin",
    "aarch64-linux",
    "x86_64-darwin",
    "x86_64-linux",
];
const CACHE_URL: &str = "https://cache.nixos.org";
const CACHE_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

#[derive(Clone)]
struct ProofKey {
    pkcs8: Vec<u8>,
    key: Key,
    id: Decoded<Hex>,
}

impl std::fmt::Debug for ProofKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProofKey(<redacted>)")
    }
}

impl ProofKey {
    fn generate() -> Result<Self, AnyError> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())?
            .as_ref()
            .to_vec();
        let signer = parse_keypair(&pkcs8)?;
        let key = signer.tuf_key();
        let id = key.key_id()?;
        Ok(Self { pkcs8, key, id })
    }

    fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self, AnyError> {
        let signer = parse_keypair(&pkcs8)?;
        let key = signer.tuf_key();
        let id = key.key_id()?;
        Ok(Self { pkcs8, key, id })
    }

    fn signer(&self) -> Result<Box<dyn Sign>, AnyError> {
        Ok(Box::new(parse_keypair(&self.pkcs8)?))
    }
}

#[async_trait]
impl KeySource for ProofKey {
    async fn as_sign(&self) -> Result<Box<dyn Sign>, Box<dyn Error + Send + Sync + 'static>> {
        self.signer().map_err(|error| error.to_string().into())
    }

    async fn write(
        &self,
        _value: &str,
        _key_id_hex: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
        Err("proof keys are memory-only".into())
    }
}

struct ProofAuthority;
struct ProofAuthorization;

impl ReleaseAuthorization for ProofAuthorization {
    fn lease_id(&self) -> &str {
        "linux-proof-lease"
    }

    fn signing_actor(&self) -> &str {
        "linux-proof"
    }

    fn bind_transaction(&mut self, digest: &str) -> Result<(), ValidationError> {
        (digest.len() == 64)
            .then_some(())
            .ok_or(ValidationError::InvalidPolicy)
    }

    fn commit(&mut self) -> Result<(), ValidationError> {
        Ok(())
    }
}

impl ReleaseAuthority for ProofAuthority {
    fn authorize(
        &self,
        digest: &str,
        sequence: u64,
        timestamp_version: u64,
        approvals: &[Approval],
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
        let evidence = approvals.iter().map(Approval::evidence).collect::<Vec<_>>();
        if digest.len() != 64
            || sequence != 1
            || timestamp_version != 1
            || evidence != ["proof:release", "proof:security"]
        {
            return Err(ValidationError::InvalidPolicy);
        }
        Ok(Box::new(ProofAuthorization))
    }

    fn resume(
        &self,
        digest: &str,
        transaction_digest: &str,
        lease_id: &str,
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
        if digest.len() != 64 || transaction_digest.len() != 64 || lease_id != "linux-proof-lease" {
            return Err(ValidationError::InvalidPolicy);
        }
        Ok(Box::new(ProofAuthorization))
    }
}

struct FileRecord {
    digest: String,
    length: u64,
}

fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<FileRecord, AnyError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(FileRecord {
        digest: hex::encode(Sha256::digest(bytes)),
        length: bytes.len() as u64,
    })
}

fn copy_file(root: &Path, relative: &str, source: &Path) -> Result<FileRecord, AnyError> {
    let bytes = fs::read(source)?;
    write_file(root, relative, &bytes)
}

fn compress(bytes: &[u8]) -> Result<Vec<u8>, AnyError> {
    let mut reader = brotli::CompressorReader::new(Cursor::new(bytes), 4 * 1024, 5, 22);
    let mut compressed = Vec::new();
    reader.read_to_end(&mut compressed)?;
    Ok(compressed)
}

async fn signed_root(
    root_keys: &[ProofKey],
    online: &[ProofKey],
    expires: Timestamp,
) -> Result<Vec<u8>, AnyError> {
    let mut keys = HashMap::new();
    for key in root_keys.iter().chain(online) {
        keys.insert(key.id.clone(), key.key.clone());
    }
    let role = |items: &[ProofKey], threshold: u64| RoleKeys {
        keyids: items.iter().map(|key| key.id.clone()).collect(),
        threshold: NonZeroU64::new(threshold).expect("nonzero proof threshold"),
        _extra: HashMap::new(),
    };
    let mut roles = HashMap::new();
    roles.insert(RoleType::Root, role(root_keys, 2));
    roles.insert(RoleType::Targets, role(&online[0..1], 1));
    roles.insert(RoleType::Snapshot, role(&online[1..2], 1));
    roles.insert(RoleType::Timestamp, role(&online[2..3], 1));
    let root = Root {
        spec_version: "1.0.0".into(),
        consistent_snapshot: true,
        version: NonZeroU64::new(1).expect("nonzero root version"),
        expires,
        keys,
        roles,
        _extra: HashMap::new(),
    };
    let mut canonical = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut canonical, CanonicalFormatter::new());
    root.serialize(&mut serializer)?;
    let mut signatures = Vec::new();
    for key in root_keys.iter().take(2) {
        signatures.push(Signature {
            keyid: key.id.clone(),
            sig: key
                .signer()?
                .sign(&canonical, &SystemRandom::new())
                .await?
                .into(),
        });
    }
    let mut bytes = serde_json::to_vec_pretty(&Signed {
        signed: root,
        signatures,
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn required_argument(arguments: &mut impl Iterator<Item = String>) -> Result<PathBuf, AnyError> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "missing argument".into())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AnyError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_keys(state: &Path, prefix: &str, keys: &[ProofKey]) -> Result<(), AnyError> {
    for (index, key) in keys.iter().enumerate() {
        write_private(&state.join(format!("{prefix}-{index}.pk8")), &key.pkcs8)?;
    }
    Ok(())
}

fn read_keys(state: &Path, prefix: &str) -> Result<Vec<ProofKey>, AnyError> {
    (0..3)
        .map(|index| {
            fs::read(state.join(format!("{prefix}-{index}.pk8")))
                .map_err(AnyError::from)
                .and_then(ProofKey::from_pkcs8)
        })
        .collect()
}

async fn prepare_signing_state(state: &Path) -> Result<(), AnyError> {
    DirBuilder::new().mode(0o700).create(state)?;
    let root_keys = (0..3)
        .map(|_| ProofKey::generate())
        .collect::<Result<Vec<_>, _>>()?;
    let online = (0..3)
        .map(|_| ProofKey::generate())
        .collect::<Result<Vec<_>, _>>()?;
    let root_bytes = signed_root(
        &root_keys,
        &online,
        Timestamp::now() + jiff::SignedDuration::from_hours(24 * 365),
    )
    .await?;
    write_keys(state, "online", &online)?;
    write_private(&state.join("root.json"), &root_bytes)?;
    println!("{}", state.join("root.json").display());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut arguments = std::env::args().skip(1);
    let first = arguments.next().ok_or("missing argument")?;
    if first == "--prepare" {
        let state = required_argument(&mut arguments)?;
        if arguments.next().is_some() || state.exists() {
            return Err("usage: linux_proof_publication --prepare STATE_DIR".into());
        }
        return prepare_signing_state(&state).await;
    }
    let output = PathBuf::from(first);
    let archive = required_argument(&mut arguments)?;
    let binaries = required_argument(&mut arguments)?;
    let system_name = arguments.next().ok_or("missing system")?;
    let signing_state = required_argument(&mut arguments)?;
    if arguments.next().is_some() || output.exists() || !SYSTEMS.contains(&system_name.as_str()) {
        return Err(
            "usage: linux_proof_publication OUTPUT ARCHIVE BIN_DIR SYSTEM STATE_DIR".into(),
        );
    }
    let system = System::from_str(&system_name)?;
    if !matches!(system, System::Aarch64Linux | System::X8664Linux) {
        return Err("the proof system must be Linux".into());
    }

    let artifacts = tempfile::tempdir()?;
    let artifact_root = artifacts.path();
    let online = read_keys(&signing_state, "online")?;
    let now = Timestamp::now();
    let root_path = signing_state.join("root.json");
    let root_bytes = fs::read(&root_path)?;
    let root_digest = hex::encode(Sha256::digest(&root_bytes));

    let runtime_manifest =
        build_upstream_runtime_asset_manifest(&archive, system, &NixVersion::new(NIX_VERSION)?)?;
    let metadata = BuildMetadata::new(
        ChannelSequence::from_u64(1).ok_or("invalid sequence")?,
        system,
        NixpkgsRevision::new(NIXPKGS_REVISION)?,
        "2026-08-14T00:00:00Z",
    )?;
    let index = compress(build_index(metadata, Vec::new())?.bytes())?;

    let mut release_artifacts = Vec::new();
    let mut runtime_entries = BTreeMap::new();
    let mut index_entries = BTreeMap::new();
    for candidate in SYSTEMS {
        let runtime_target = format!("nix/{NIX_VERSION}/{candidate}.tar.xz");
        let manifest_target = format!("nix/{NIX_VERSION}/{candidate}.assets.json");
        let index_target = format!("index/1/{candidate}.json.br");
        let (runtime, manifest, index_record) = if candidate == system_name {
            (
                copy_file(artifact_root, &runtime_target, &archive)?,
                write_file(artifact_root, &manifest_target, &runtime_manifest)?,
                write_file(artifact_root, &index_target, &index)?,
            )
        } else {
            (
                write_file(artifact_root, &runtime_target, candidate.as_bytes())?,
                write_file(artifact_root, &manifest_target, candidate.as_bytes())?,
                write_file(artifact_root, &index_target, candidate.as_bytes())?,
            )
        };
        runtime_entries.insert(
            candidate,
                serde_json::json!({
                    "url": format!("https://releases.nixos.org/nix/nix-{NIX_VERSION}/nix-{NIX_VERSION}-{candidate}.tar.xz"),
                    "sha256": runtime.digest,
                    "assetManifestTarget": manifest_target,
                "assetManifestSha256": manifest.digest,
            }),
        );
        index_entries.insert(
            candidate,
            serde_json::json!({"target": index_target, "sha256": index_record.digest}),
        );
        for (kind, target, record) in [
            ("managed-nix", runtime_target, runtime),
            ("managed-nix-assets", manifest_target, manifest),
            ("index", index_target, index_record),
        ] {
            release_artifacts.push(serde_json::json!({
                "kind": kind, "system": candidate, "target": target, "source": target,
                "sha256": record.digest, "length": record.length,
            }));
        }
        for name in ["pkg-root-helper", "pkg-nix-broker", "pkg"] {
            let target = format!("installer/{candidate}/{name}");
            let record = if candidate == system_name {
                copy_file(artifact_root, &target, &binaries.join(name))?
            } else {
                write_file(
                    artifact_root,
                    &target,
                    format!("{candidate} {name}\n").as_bytes(),
                )?
            };
            release_artifacts.push(serde_json::json!({
                "kind":"installer-payload", "system":candidate, "target":target, "source":target,
                "sha256":record.digest, "length":record.length,
            }));
        }
    }

    let build_policy = SYSTEMS
        .into_iter()
        .map(|candidate| (candidate, serde_json::json!({"mode":"allow-with-gates"})))
        .collect::<BTreeMap<_, _>>();
    let descriptor = serde_json::to_vec_pretty(&serde_json::json!({
        "schemaVersion":1, "channel":"pkg-linux-proof", "policyVersion":1, "sequence":1,
        "expiresAt":"2037-01-01T00:00:00Z", "supportedSystems":SYSTEMS,
        "buildPolicy":{"nativeLocalBuilds":build_policy},
        "nixRuntime":{"version":NIX_VERSION,"perSystem":runtime_entries},
        "nixpkgs":{"owner":"NixOS","repo":"nixpkgs","rev":NIXPKGS_REVISION,"narHash":NIXPKGS_NAR_HASH},
        "index":{"source":"self-built","perSystem":index_entries},
        "substituters":{"urls":[CACHE_URL],"trustedPublicKeys":[CACHE_KEY]},
    }))?;
    let descriptor_record = write_file(artifact_root, "descriptor.json", &descriptor)?;
    release_artifacts.push(serde_json::json!({
        "kind":"descriptor", "system":null, "target":"descriptor.json", "source":"descriptor.json",
        "sha256":descriptor_record.digest, "length":descriptor_record.length,
    }));

    let mut cli_artifacts = Vec::new();
    for (kind, candidate, source) in [
        ("pkg", "aarch64-darwin", "cli/pkg-aarch64-darwin"),
        ("pkg", "aarch64-linux", "cli/pkg-aarch64-linux"),
        ("pkg", "x86_64-linux", "cli/pkg-x86_64-linux"),
        (
            "pkg-install",
            "aarch64-linux",
            "cli/pkg-installer-aarch64-linux",
        ),
        (
            "pkg-install",
            "x86_64-linux",
            "cli/pkg-installer-x86_64-linux",
        ),
    ] {
        let record = if kind == "pkg" && candidate == system_name {
            copy_file(artifact_root, source, &binaries.join("pkg"))?
        } else {
            write_file(
                artifact_root,
                source,
                format!("{kind} {candidate}\n").as_bytes(),
            )?
        };
        let bundle = format!("{source}.sigstore.json");
        let bundle_record = write_file(artifact_root, &bundle, b"linux proof only\n")?;
        cli_artifacts.push(serde_json::json!({
            "kind":kind, "system":candidate, "source":source,
            "sha256":record.digest, "length":record.length,
            "sigstoreBundle":bundle, "sigstoreBundleSha256":bundle_record.digest,
            "sigstoreBundleLength":bundle_record.length,
        }));
    }
    let manifest = serde_json::json!({
        "schemaVersion":1, "releaseId":"linux-proof-1", "channelSequence":1,
        "timestampVersion":1, "trustedRootSha256":root_digest, "policyVersion":1,
        "artifacts":release_artifacts, "cliArtifacts":cli_artifacts,
        "approvals":[
            {"actor":"proof-release","role":"release","evidence":"proof:release"},
            {"actor":"proof-security","role":"security","evidence":"proof:security"}
        ]
    });
    let release = ReleaseManifest::from_json(
        &serde_json::to_vec(&manifest)?,
        artifact_root,
        &ProofAuthority,
    )?;
    let online_sources = online
        .into_iter()
        .map(|key| Box::new(key) as Box<dyn KeySource>)
        .collect::<Vec<_>>();
    sign_channel(
        release,
        &root_path,
        &online_sources,
        MetadataPolicy {
            targets_version: NonZeroU64::new(1).expect("nonzero targets version"),
            snapshot_version: NonZeroU64::new(1).expect("nonzero snapshot version"),
            timestamp_version: NonZeroU64::new(1).expect("nonzero timestamp version"),
            targets_expires: now + jiff::SignedDuration::from_hours(24 * 30),
            snapshot_expires: now + jiff::SignedDuration::from_hours(24 * 7),
            timestamp_expires: now + jiff::SignedDuration::from_hours(24),
        },
        &output,
    )
    .await?;
    fs::write(output.join("root.json"), root_bytes)?;
    println!("{}", output.display());
    Ok(())
}
