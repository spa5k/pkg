//! Build an ephemeral signed publication for clean-host product proofs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use pkg_index::{BuildMetadata, IndexCandidate, build_index};
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
const NIXPKGS: [(&str, &str); 2] = [
    (
        "a62e6edd6d5e1fa0329b8653c801147986f8d446",
        "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=",
    ),
    (
        "a50de1b7d8a586adc18d2395c19de7d6058e6030",
        "sha256-uslt2pqShTIXDdAHRHv2QkYLsVdY8Oqwz0EA48/RSM8=",
    ),
];
const SYSTEMS: [&str; 2] = ["aarch64-darwin", "x86_64-linux"];
const CACHE_URL: &str = "https://cache.nixos.org";
const CACHE_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
const DETERMINATE_REVISION: &str = "4132ad07a15ee7d88c096ac7172b7afb2672866b";

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
            || sequence != timestamp_version
            || !(1..=2).contains(&sequence)
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofInventory {
    schema_version: u64,
    files: Vec<ProofInventoryFile>,
}

#[derive(Serialize)]
struct ProofInventoryFile {
    path: String,
    sha256: String,
    length: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofPair {
    schema_version: u64,
    channels: Vec<ProofPairChannel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofPairChannel {
    name: &'static str,
    release_id: String,
    manifest_schema_version: u64,
    channel_sequence: u64,
    timestamp_version: u64,
    trusted_root_sha256: String,
    inventory: String,
    inventory_sha256: String,
    inventory_length: u64,
    required_metadata_paths: Vec<String>,
    required_target_prefix: &'static str,
}

#[derive(Clone, Copy)]
enum ProofInputMode {
    Dn16Prepared,
    Dn16Sealed,
    LegacyLinuxFixture,
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

fn read_regular_file(source: &Path) -> Result<Vec<u8>, AnyError> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("preview input must be a regular file".into());
    }
    Ok(fs::read(source)?)
}

fn copy_file(root: &Path, relative: &str, source: &Path) -> Result<FileRecord, AnyError> {
    write_file(root, relative, &read_regular_file(source)?)
}

fn copy_sigstore_bundle(
    root: &Path,
    relative: &str,
    source: &Path,
) -> Result<FileRecord, AnyError> {
    let bytes = read_regular_file(source)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Sigstore bundle envelope")?;
    let verification = value
        .get("verificationMaterial")
        .and_then(serde_json::Value::as_object)
        .ok_or("invalid Sigstore bundle envelope")?;
    let certificate = verification
        .get("certificate")
        .and_then(|value| value.get("rawBytes"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.is_empty());
    let certificate_chain = verification
        .get("x509CertificateChain")
        .and_then(|value| value.get("certificates"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|certificates| {
            certificates.iter().any(|certificate| {
                certificate
                    .get("rawBytes")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty())
            })
        });
    let signature = value
        .get("messageSignature")
        .and_then(serde_json::Value::as_object)
        .ok_or("invalid Sigstore bundle envelope")?;
    let digest = signature
        .get("messageDigest")
        .and_then(serde_json::Value::as_object)
        .ok_or("invalid Sigstore bundle envelope")?;
    if value.get("mediaType").and_then(serde_json::Value::as_str)
        != Some("application/vnd.dev.sigstore.bundle.v0.3+json")
        || value.get("proofFixture").is_some()
        || (!certificate && !certificate_chain)
        || digest.get("algorithm").and_then(serde_json::Value::as_str) != Some("SHA2_256")
        || digest
            .get("digest")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.is_empty())
        || signature
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.is_empty())
    {
        return Err("invalid Sigstore bundle envelope".into());
    }
    write_file(root, relative, &bytes)
}

fn prepare_cli_artifacts(
    artifact_root: &Path,
    aarch64_input: &Path,
    x86_64_input: &Path,
    mode: ProofInputMode,
) -> Result<Vec<serde_json::Value>, AnyError> {
    [
        ("pkg", "aarch64-darwin", "pkg-aarch64-darwin"),
        ("pkg", "x86_64-linux", "pkg-x86_64-linux"),
        ("pkg-install", "x86_64-linux", "pkg-installer-x86_64-linux"),
    ]
    .into_iter()
    .map(|(kind, system, name)| {
        let input = if system == "aarch64-darwin" {
            aarch64_input
        } else {
            x86_64_input
        };
        let source = format!("cli/{name}");
        let record = copy_file(artifact_root, &source, &input.join(name))?;
        let mut artifact = serde_json::json!({
            "kind":kind, "system":system, "source":source,
            "sha256":record.digest, "length":record.length,
        });
        if matches!(mode, ProofInputMode::Dn16Prepared) {
            return Ok(artifact);
        }
        let bundle = format!("{source}.sigstore.json");
        let bundle_input = input.join(format!("{name}.sigstore.json"));
        let bundle_record = match mode {
            ProofInputMode::Dn16Sealed => {
                copy_sigstore_bundle(artifact_root, &bundle, &bundle_input)?
            }
            ProofInputMode::LegacyLinuxFixture => copy_file(artifact_root, &bundle, &bundle_input)?,
            ProofInputMode::Dn16Prepared => unreachable!("prepared returned above"),
        };
        artifact["sigstoreBundle"] = serde_json::json!(bundle);
        artifact["sigstoreBundleSha256"] = serde_json::json!(bundle_record.digest);
        artifact["sigstoreBundleLength"] = serde_json::json!(bundle_record.length);
        Ok(artifact)
    })
    .collect()
}

fn prepare_determinate_inventory(
    artifact_root: &Path,
    input: &Path,
) -> Result<serde_json::Value, AnyError> {
    let mut artifacts = Vec::new();
    for (kind, system, name, upstream_url) in [
        (
            "installer",
            Some("aarch64-darwin"),
            "nix-installer-aarch64-darwin",
            "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-darwin",
        ),
        (
            "installer",
            Some("aarch64-linux"),
            "nix-installer-aarch64-linux",
            "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux",
        ),
        (
            "installer",
            Some("x86_64-linux"),
            "nix-installer-x86_64-linux",
            "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux",
        ),
        (
            "source",
            None,
            "nix-installer-v3.22.1.tar.gz",
            "https://codeload.github.com/DeterminateSystems/nix-installer/tar.gz/refs/tags/v3.22.1",
        ),
        (
            "license",
            None,
            "LICENSE",
            "https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE",
        ),
    ] {
        let target = format!("determinate/3.22.1/{name}");
        let record = copy_file(artifact_root, &target, &input.join(name))?;
        artifacts.push(serde_json::json!({
            "kind":kind, "system":system, "target":target, "source":target,
            "upstreamUrl":upstream_url, "sha256":record.digest, "length":record.length,
        }));
    }
    Ok(serde_json::json!({
        "version":"3.22.1", "revision":DETERMINATE_REVISION,
        "license":"LGPL-2.1", "artifacts":artifacts,
    }))
}

fn compress(bytes: &[u8]) -> Result<Vec<u8>, AnyError> {
    let mut reader = brotli::CompressorReader::new(Cursor::new(bytes), 4 * 1024, 5, 22);
    let mut compressed = Vec::new();
    reader.read_to_end(&mut compressed)?;
    Ok(compressed)
}

fn build_proof_index(
    sequence: u64,
    system: System,
    nixpkgs_revision: &str,
) -> Result<Vec<u8>, AnyError> {
    let metadata = BuildMetadata::new(
        ChannelSequence::from_u64(sequence).ok_or("invalid sequence")?,
        system,
        NixpkgsRevision::new(nixpkgs_revision)?,
        "2026-08-14T00:00:00Z",
    )?;
    let candidate =
        |attr_path: &str, pname: &str, version: &str, description: &str| IndexCandidate {
            attr_path: attr_path.into(),
            pname: Some(pname.into()),
            version: Some(version.into()),
            description: Some(description.into()),
            homepage: None,
            licenses: Vec::new(),
            platforms: vec![system.to_string()],
            available_here: true,
            broken: false,
            position: None,
            outputs: vec!["out".into()],
            aliases: Vec::new(),
            skipped: false,
        };
    compress(
        build_index(
            metadata,
            vec![
                candidate("hello", "hello", "2.12.1", "Print a greeting"),
                candidate(
                    "ripgrep",
                    "ripgrep",
                    if sequence == 1 { "13.0.0" } else { "15.1.0" },
                    "Search text",
                ),
                candidate("fd", "fd", "8.7.1", "Find files"),
                candidate("bat", "bat", "0.24.0", "View text files"),
                candidate("tree", "tree", "2.1.1", "List directory trees"),
                candidate("wget", "wget", "1.21.4", "Download files"),
                candidate("git", "git", "2.42.2", "Manage source history"),
                candidate("tmux", "tmux", "3.3a", "Use terminal sessions"),
                candidate(
                    "zoxide",
                    "zoxide",
                    "unstable-2023-11-20",
                    "Navigate directories",
                ),
                candidate("fzf", "fzf", "0.46.0", "Find items with fuzzy search"),
                IndexCandidate {
                    attr_path: "cxx-prettyprint".into(),
                    pname: Some("cxx-prettyprint-unstable".into()),
                    version: Some("2016-04-30".into()),
                    description: Some("Print C++ containers".into()),
                    homepage: Some("https://github.com/louisdx/cxx-prettyprint".into()),
                    licenses: Vec::new(),
                    platforms: vec![system.to_string()],
                    available_here: true,
                    broken: false,
                    position: None,
                    outputs: vec!["out".into()],
                    aliases: Vec::new(),
                    skipped: false,
                },
            ],
        )?
        .bytes(),
    )
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

fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>, AnyError> {
    let mut bytes = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut bytes, CanonicalFormatter::new());
    value.serialize(&mut serializer)?;
    Ok(bytes)
}

fn inventory(root: &Path) -> Result<(ProofInventory, BTreeSet<String>), AnyError> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut Vec<ProofInventoryFile>,
    ) -> Result<(), AnyError> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err("proof publication contains a symlink".into());
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
                continue;
            }
            if !metadata.is_file() {
                return Err("proof publication contains a non-file entry".into());
            }
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("proof publication path is not UTF-8")?
                .replace('\\', "/");
            let mut file = fs::File::open(&path)?;
            let mut digest = Sha256::new();
            let length = std::io::copy(&mut file, &mut digest)?;
            files.push(ProofInventoryFile {
                path: relative,
                sha256: hex::encode(digest.finalize()),
                length,
            });
        }
        Ok(())
    }

    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("proof channel must be a regular directory".into());
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let paths = files.iter().map(|file| file.path.clone()).collect();
    Ok((
        ProofInventory {
            schema_version: 1,
            files,
        },
        paths,
    ))
}

fn bind_proof_pair(
    root: &Path,
    n_release_id: &str,
    n_plus_1_release_id: &str,
) -> Result<(), AnyError> {
    if n_release_id == n_plus_1_release_id {
        return Err("proof releases must be distinct".into());
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("proof pair must be a regular directory".into());
    }
    let entries = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if entries != ["n", "n-plus-1"].into_iter().map(Into::into).collect() {
        return Err("proof pair must contain only n and n-plus-1".into());
    }

    let mut channels = Vec::new();
    let mut inventories = Vec::new();
    let mut trusted_root = None;
    for (name, release_id, version) in [
        ("n", n_release_id, 1_u64),
        ("n-plus-1", n_plus_1_release_id, 2_u64),
    ] {
        let channel = root.join(name);
        let (inventory, paths) = inventory(&channel)?;
        let required_metadata_paths = [
            "metadata/1.root.json".to_owned(),
            format!("metadata/{version}.targets.json"),
            format!("metadata/{version}.snapshot.json"),
            "metadata/timestamp.json".to_owned(),
        ];
        if !required_metadata_paths
            .iter()
            .all(|path| paths.contains(path))
            || !paths.contains("release-manifest.json")
            || !paths.iter().any(|path| path.starts_with("targets/"))
        {
            return Err("proof channel is missing required metadata or targets".into());
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&read_regular_file(&channel.join("release-manifest.json"))?)?;
        let root_digest = manifest
            .get("trustedRootSha256")
            .and_then(serde_json::Value::as_str)
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or("proof manifest has an invalid trusted root")?;
        if manifest
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(2)
            || manifest
                .get("releaseId")
                .and_then(serde_json::Value::as_str)
                != Some(release_id)
            || manifest
                .get("channelSequence")
                .and_then(serde_json::Value::as_u64)
                != Some(version)
            || manifest
                .get("timestampVersion")
                .and_then(serde_json::Value::as_u64)
                != Some(version)
            || trusted_root
                .as_deref()
                .is_some_and(|trusted| trusted != root_digest)
        {
            return Err("proof manifest does not match the pair".into());
        }
        trusted_root = Some(root_digest.to_owned());
        let inventory_bytes = canonical_json(&inventory)?;
        let inventory_name = format!("{name}.inventory.json");
        channels.push(ProofPairChannel {
            name,
            release_id: release_id.to_owned(),
            manifest_schema_version: 2,
            channel_sequence: version,
            timestamp_version: version,
            trusted_root_sha256: root_digest.to_owned(),
            inventory: inventory_name.clone(),
            inventory_sha256: hex::encode(Sha256::digest(&inventory_bytes)),
            inventory_length: inventory_bytes.len() as u64,
            required_metadata_paths: required_metadata_paths.into(),
            required_target_prefix: "targets/",
        });
        inventories.push((inventory_name, inventory_bytes));
    }
    let descriptor = canonical_json(&ProofPair {
        schema_version: 1,
        channels,
    })?;
    for (name, bytes) in inventories {
        write_private(&root.join(name), &bytes)?;
    }
    write_private(&root.join("proof-pair.json"), &descriptor)
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
    if first == "--bind-dn16-pair" {
        let root = required_argument(&mut arguments)?;
        let n_release_id = arguments.next().ok_or("missing N release id")?;
        let n_plus_1_release_id = arguments.next().ok_or("missing N+1 release id")?;
        if arguments.next().is_some() {
            return Err(
                "usage: linux_proof_publication --bind-dn16-pair PAIR_DIR N_RELEASE_ID N_PLUS_1_RELEASE_ID"
                    .into(),
            );
        }
        return bind_proof_pair(&root, &n_release_id, &n_plus_1_release_id);
    }
    let (
        output,
        runtimes,
        aarch64_input,
        x86_64_input,
        signing_state,
        sequence,
        release_id,
        input_mode,
        sealed_manifest,
        legacy_system,
    ) = match first.as_str() {
        "--prepare-dn16-manifest" => {
            let output = required_argument(&mut arguments)?;
            let runtimes = required_argument(&mut arguments)?;
            let aarch64_input = required_argument(&mut arguments)?;
            let x86_64_input = required_argument(&mut arguments)?;
            let signing_state = required_argument(&mut arguments)?;
            let sequence = arguments
                .next()
                .ok_or("missing channel sequence")?
                .parse()?;
            let release_id = arguments.next().ok_or("missing proof release id")?;
            (
                output,
                runtimes,
                aarch64_input,
                x86_64_input,
                signing_state,
                sequence,
                release_id,
                ProofInputMode::Dn16Prepared,
                None,
                None,
            )
        }
        "--publish-dn16" => {
            let output = required_argument(&mut arguments)?;
            let runtimes = required_argument(&mut arguments)?;
            let aarch64_input = required_argument(&mut arguments)?;
            let x86_64_input = required_argument(&mut arguments)?;
            let sealed_manifest = required_argument(&mut arguments)?;
            let signing_state = required_argument(&mut arguments)?;
            let sequence = arguments
                .next()
                .ok_or("missing channel sequence")?
                .parse()?;
            let release_id = arguments.next().ok_or("missing proof release id")?;
            (
                output,
                runtimes,
                aarch64_input,
                x86_64_input,
                signing_state,
                sequence,
                release_id,
                ProofInputMode::Dn16Sealed,
                Some(sealed_manifest),
                None,
            )
        }
        "--legacy-linux-fixture" => {
            let output = required_argument(&mut arguments)?;
            let runtimes = required_argument(&mut arguments)?;
            let binaries = required_argument(&mut arguments)?;
            let system = arguments.next().ok_or("missing system")?;
            let signing_state = required_argument(&mut arguments)?;
            let sequence = arguments
                .next()
                .ok_or("missing channel sequence")?
                .parse()?;
            let release_id = arguments.next().ok_or("missing proof release id")?;
            (
                output,
                runtimes,
                binaries.clone(),
                binaries,
                signing_state,
                sequence,
                release_id,
                ProofInputMode::LegacyLinuxFixture,
                None,
                Some(system),
            )
        }
        _ => return Err("invalid proof publication command".into()),
    };
    if arguments.next().is_some() || output.exists() || !(1..=2).contains(&sequence) {
        return Err("invalid proof publication arguments".into());
    }
    if legacy_system.as_deref().is_some_and(|system| {
        system != "x86_64-linux" || release_id != format!("linux-proof-{sequence}")
    }) {
        return Err("legacy Linux fixtures require x86_64-linux and linux-proof-SEQUENCE".into());
    }
    let (nixpkgs_revision, nixpkgs_nar_hash) = NIXPKGS[(sequence - 1) as usize];

    let artifacts = tempfile::tempdir()?;
    let artifact_root = artifacts.path();
    let determinate =
        prepare_determinate_inventory(artifact_root, &x86_64_input.join("determinate"))?;
    let now = Timestamp::now();
    let root_path = signing_state.join("root.json");
    let root_bytes = fs::read(&root_path)?;
    let root_digest = hex::encode(Sha256::digest(&root_bytes));

    let mut release_artifacts = Vec::new();
    let mut runtime_entries = BTreeMap::new();
    let mut index_entries = BTreeMap::new();
    for candidate in SYSTEMS {
        let runtime_target = format!("nix/{NIX_VERSION}/{candidate}.tar.xz");
        let manifest_target = format!("nix/{NIX_VERSION}/{candidate}.assets.json");
        let index_target = format!("index/{sequence}/{candidate}.json.br");
        let runtime_source = runtimes.join(format!("{candidate}.tar.xz"));
        let runtime_manifest = build_upstream_runtime_asset_manifest(
            &runtime_source,
            System::from_str(candidate)?,
            &NixVersion::new(NIX_VERSION)?,
        )?;
        let runtime_record = copy_file(artifact_root, &runtime_target, &runtime_source)?;
        let manifest_record = write_file(artifact_root, &manifest_target, &runtime_manifest)?;
        let real_index =
            build_proof_index(sequence, System::from_str(candidate)?, nixpkgs_revision)?;
        let index_record = if legacy_system
            .as_deref()
            .is_some_and(|system| candidate != system)
        {
            write_file(artifact_root, &index_target, candidate.as_bytes())?
        } else {
            write_file(artifact_root, &index_target, &real_index)?
        };
        runtime_entries.insert(
            candidate,
            serde_json::json!({
                "url":format!("https://releases.nixos.org/nix/nix-{NIX_VERSION}/nix-{NIX_VERSION}-{candidate}.tar.xz"),
                "sha256":runtime_record.digest,
                "assetManifestTarget":manifest_target,
                "assetManifestSha256":manifest_record.digest,
            }),
        );
        index_entries.insert(
            candidate,
            serde_json::json!({"target": index_target, "sha256": index_record.digest}),
        );
        for (kind, target, record) in [
            ("managed-nix", runtime_target, runtime_record),
            ("managed-nix-assets", manifest_target, manifest_record),
            ("index", index_target, index_record),
        ] {
            release_artifacts.push(serde_json::json!({
                "kind":kind, "system":candidate, "target":target, "source":target,
                "sha256":record.digest, "length":record.length,
            }));
        }
        let input = if candidate == "aarch64-darwin" {
            &aarch64_input
        } else {
            &x86_64_input
        };
        for name in ["pkg-root-helper", "pkg-nix-broker", "pkg"] {
            let target = format!("installer/{candidate}/{name}");
            let record = if legacy_system
                .as_deref()
                .is_some_and(|system| candidate != system)
            {
                write_file(
                    artifact_root,
                    &target,
                    format!("{candidate} {name}\n").as_bytes(),
                )?
            } else {
                copy_file(artifact_root, &target, &input.join(name))?
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
        "schemaVersion":1,
        "channel":if legacy_system.is_some() { "pkg-linux-proof" } else { "pkg-dn16-proof" },
        "policyVersion":1, "sequence":sequence,
        "expiresAt":"2037-01-01T00:00:00Z", "supportedSystems":SYSTEMS,
        "buildPolicy":{"nativeLocalBuilds":build_policy},
        "nixRuntime":{"version":NIX_VERSION,"perSystem":runtime_entries},
        "nixpkgs":{"owner":"NixOS","repo":"nixpkgs","rev":nixpkgs_revision,"narHash":nixpkgs_nar_hash},
        "index":{"source":"self-built","perSystem":index_entries},
        "substituters":{"urls":[CACHE_URL],"trustedPublicKeys":[CACHE_KEY]},
    }))?;
    let descriptor_record = write_file(artifact_root, "descriptor.json", &descriptor)?;
    release_artifacts.push(serde_json::json!({
        "kind":"descriptor", "system":null, "target":"descriptor.json", "source":"descriptor.json",
        "sha256":descriptor_record.digest, "length":descriptor_record.length,
    }));

    let cli_artifacts =
        prepare_cli_artifacts(artifact_root, &aarch64_input, &x86_64_input, input_mode)?;
    let manifest = serde_json::json!({
        "schemaVersion":2, "releaseId":release_id,
        "channelSequence":sequence, "timestampVersion":sequence,
        "trustedRootSha256":root_digest, "policyVersion":1,
        "determinate":determinate,
        "artifacts":release_artifacts, "cliArtifacts":cli_artifacts,
        "approvals":[
            {"actor":"proof-release","role":"release","evidence":"proof:release"},
            {"actor":"proof-security","role":"security","evidence":"proof:security"}
        ]
    });
    let generated_manifest = serde_json::to_vec(&manifest)?;
    if matches!(input_mode, ProofInputMode::Dn16Prepared) {
        let prepared = ReleaseManifest::from_prepared_json(
            &generated_manifest,
            artifact_root,
            &ProofAuthority,
        )?;
        write_private(&output, prepared.manifest())?;
        println!("{}", output.display());
        return Ok(());
    }
    let manifest_bytes = if matches!(input_mode, ProofInputMode::Dn16Sealed) {
        let sealed = read_regular_file(
            sealed_manifest
                .as_deref()
                .ok_or("sealed DN-16 manifest is required")?,
        )?;
        let sealed_value: serde_json::Value = serde_json::from_slice(&sealed)?;
        if sealed_value != manifest {
            return Err("sealed manifest does not match the exact proof inputs".into());
        }
        sealed
    } else {
        generated_manifest
    };
    let release = ReleaseManifest::from_json(&manifest_bytes, artifact_root, &ProofAuthority)?;
    let prepared_manifest_bytes = if matches!(input_mode, ProofInputMode::LegacyLinuxFixture) {
        Some(
            release
                .authorize_prepared_manifest(&ProofAuthority)?
                .manifest()
                .to_vec(),
        )
    } else {
        None
    };
    let online = read_keys(&signing_state, "online")?;
    let online_sources = online
        .into_iter()
        .map(|key| Box::new(key) as Box<dyn KeySource>)
        .collect::<Vec<_>>();
    sign_channel(
        release,
        &root_path,
        &online_sources,
        MetadataPolicy {
            targets_version: NonZeroU64::new(sequence).expect("nonzero targets version"),
            snapshot_version: NonZeroU64::new(sequence).expect("nonzero snapshot version"),
            timestamp_version: NonZeroU64::new(sequence).expect("nonzero timestamp version"),
            targets_expires: now + jiff::SignedDuration::from_hours(24 * 30),
            snapshot_expires: now + jiff::SignedDuration::from_hours(24 * 7),
            timestamp_expires: now + jiff::SignedDuration::from_hours(24),
        },
        &output,
    )
    .await?;
    if let Some(prepared) = prepared_manifest_bytes {
        fs::write(output.join("release-manifest.json"), prepared)?;
    }
    fs::write(output.join("root.json"), root_bytes)?;
    println!("{}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGSTORE_BUNDLE: &str = r#"{
        "mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json",
        "verificationMaterial":{"certificate":{"rawBytes":"Y2VydA=="}},
        "messageSignature":{
            "messageDigest":{"algorithm":"SHA2_256","digest":"ZGlnZXN0"},
            "signature":"c2lnbmF0dXJl"
        }
    }"#;

    fn write_cli_inputs(root: &Path, bundle: &[u8]) {
        for name in [
            "pkg-aarch64-darwin",
            "pkg-x86_64-linux",
            "pkg-installer-x86_64-linux",
        ] {
            fs::write(root.join(name), format!("payload:{name}\n")).expect("write payload");
            fs::write(root.join(format!("{name}.sigstore.json")), bundle).expect("write bundle");
        }
    }

    #[test]
    fn cli_artifacts_bind_exact_input_payloads_and_bundles() {
        let aarch64_input = tempfile::tempdir().expect("Apple Silicon input directory");
        let x86_64_input = tempfile::tempdir().expect("Linux input directory");
        let output = tempfile::tempdir().expect("output directory");
        fs::write(aarch64_input.path().join("pkg-aarch64-darwin"), b"darwin\n")
            .expect("write Apple Silicon payload");
        fs::write(
            aarch64_input
                .path()
                .join("pkg-aarch64-darwin.sigstore.json"),
            SIGSTORE_BUNDLE,
        )
        .expect("write Apple Silicon bundle");
        for name in ["pkg-x86_64-linux", "pkg-installer-x86_64-linux"] {
            fs::write(x86_64_input.path().join(name), format!("linux:{name}\n"))
                .expect("write Linux payload");
            fs::write(
                x86_64_input.path().join(format!("{name}.sigstore.json")),
                SIGSTORE_BUNDLE,
            )
            .expect("write Linux bundle");
        }

        let records = prepare_cli_artifacts(
            output.path(),
            aarch64_input.path(),
            x86_64_input.path(),
            ProofInputMode::Dn16Sealed,
        )
        .expect("CLI artifacts");

        assert_eq!(records.len(), 3);
        for record in records {
            let source = record["source"].as_str().expect("payload source");
            let bundle = record["sigstoreBundle"].as_str().expect("bundle source");
            let payload = fs::read(output.path().join(source)).expect("copied payload");
            let bundle_bytes = fs::read(output.path().join(bundle)).expect("copied bundle");
            assert_eq!(record["sha256"], hex::encode(Sha256::digest(&payload)));
            assert_eq!(record["length"], payload.len() as u64);
            assert_eq!(
                record["sigstoreBundleSha256"],
                hex::encode(Sha256::digest(&bundle_bytes))
            );
            assert_eq!(record["sigstoreBundleLength"], bundle_bytes.len() as u64);
        }
    }

    #[test]
    fn dn16_refuses_placeholder_and_plain_text_bundles() {
        let mut disguised: serde_json::Value =
            serde_json::from_str(SIGSTORE_BUNDLE).expect("Sigstore fixture");
        disguised["proofFixture"] = serde_json::json!(true);
        for bundle in [
            b"{\"proofFixture\":true}".to_vec(),
            b"plain text".to_vec(),
            serde_json::to_vec(&disguised).expect("disguised fixture"),
        ] {
            let input = tempfile::tempdir().expect("input directory");
            let output = tempfile::tempdir().expect("output directory");
            write_cli_inputs(input.path(), &bundle);

            let error = prepare_cli_artifacts(
                output.path(),
                input.path(),
                input.path(),
                ProofInputMode::Dn16Sealed,
            )
            .expect_err("placeholder must be refused");
            assert_eq!(error.to_string(), "invalid Sigstore bundle envelope");
        }
    }

    #[test]
    fn prepared_cli_artifacts_need_no_bundle_files() {
        let input = tempfile::tempdir().expect("input directory");
        let output = tempfile::tempdir().expect("output directory");
        write_cli_inputs(input.path(), SIGSTORE_BUNDLE.as_bytes());
        for name in [
            "pkg-aarch64-darwin",
            "pkg-x86_64-linux",
            "pkg-installer-x86_64-linux",
        ] {
            fs::remove_file(input.path().join(format!("{name}.sigstore.json")))
                .expect("remove bundle");
        }

        let records = prepare_cli_artifacts(
            output.path(),
            input.path(),
            input.path(),
            ProofInputMode::Dn16Prepared,
        )
        .expect("prepared CLI artifacts");
        assert!(records.iter().all(|record| {
            record.get("sigstoreBundle").is_none()
                && record.get("sigstoreBundleSha256").is_none()
                && record.get("sigstoreBundleLength").is_none()
        }));
    }

    #[test]
    fn proof_pair_binds_complete_canonical_inventories() {
        let pair = tempfile::tempdir().expect("proof pair");
        let trusted_root = "a".repeat(64);
        for (name, release_id, version) in [
            ("n", "proof-n", 1_u64),
            ("n-plus-1", "proof-n-plus-1", 2_u64),
        ] {
            let channel = pair.path().join(name);
            for relative in [
                "metadata/1.root.json".to_owned(),
                format!("metadata/{version}.targets.json"),
                format!("metadata/{version}.snapshot.json"),
                "metadata/timestamp.json".to_owned(),
                "targets/payload".to_owned(),
            ] {
                let path = channel.join(relative);
                fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
                fs::write(path, b"proof\n").expect("write proof file");
            }
            fs::write(
                channel.join("release-manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "schemaVersion":2,
                    "releaseId":release_id,
                    "channelSequence":version,
                    "timestampVersion":version,
                    "trustedRootSha256":trusted_root,
                }))
                .expect("manifest"),
            )
            .expect("write manifest");
        }

        bind_proof_pair(pair.path(), "proof-n", "proof-n-plus-1").expect("bind pair");
        let descriptor: serde_json::Value = serde_json::from_slice(
            &fs::read(pair.path().join("proof-pair.json")).expect("pair descriptor"),
        )
        .expect("parse pair descriptor");
        assert_eq!(descriptor["channels"][0]["channelSequence"], 1);
        assert_eq!(descriptor["channels"][1]["channelSequence"], 2);
        assert_eq!(
            descriptor["channels"][0]["trustedRootSha256"],
            descriptor["channels"][1]["trustedRootSha256"]
        );
        for name in ["n", "n-plus-1"] {
            let bytes =
                fs::read(pair.path().join(format!("{name}.inventory.json"))).expect("inventory");
            let inventory: serde_json::Value =
                serde_json::from_slice(&bytes).expect("parse inventory");
            assert!(inventory["files"].as_array().is_some_and(|files| {
                files.len() == 6
                    && files.iter().all(|file| {
                        file["sha256"]
                            .as_str()
                            .is_some_and(|digest| digest.len() == 64)
                            && file["length"].as_u64().is_some_and(|length| length > 0)
                    })
            }));
        }
        assert!(bind_proof_pair(pair.path(), "proof-n", "proof-n-plus-1").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cli_artifacts_refuse_a_symlinked_bundle() {
        use std::os::unix::fs::symlink;

        let input = tempfile::tempdir().expect("input directory");
        let output = tempfile::tempdir().expect("output directory");
        for name in [
            "pkg-aarch64-darwin",
            "pkg-x86_64-linux",
            "pkg-installer-x86_64-linux",
        ] {
            fs::write(input.path().join(name), b"payload\n").expect("write payload");
            fs::write(
                input.path().join(format!("{name}.sigstore.json")),
                b"bundle\n",
            )
            .expect("write bundle");
        }
        fs::remove_file(input.path().join("pkg-aarch64-darwin.sigstore.json"))
            .expect("remove bundle");
        symlink(
            input.path().join("pkg-x86_64-linux.sigstore.json"),
            input.path().join("pkg-aarch64-darwin.sigstore.json"),
        )
        .expect("symlink bundle");

        let error = prepare_cli_artifacts(
            output.path(),
            input.path(),
            input.path(),
            ProofInputMode::LegacyLinuxFixture,
        )
        .expect_err("symlink must be refused");
        assert_eq!(error.to_string(), "preview input must be a regular file");
    }
}
