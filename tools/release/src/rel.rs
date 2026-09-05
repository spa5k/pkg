//! One-command release keys and channel publication.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::num::NonZeroU64;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::Ed25519KeyPair;
use jiff::{SignedDuration, Timestamp};
use olpc_cjson::CanonicalFormatter;
use serde::Serialize;
use serde_json::Value as Json;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tough::key_source::KeySource;
use tough::schema::decoded::{Decoded, Hex};
use tough::schema::key::Key;
use tough::schema::{RoleKeys, RoleType, Root, Signature, Signed};
use tough::sign::{Sign, parse_keypair};
use url::Url;

use crate::{
    Approval, MetadataPolicy, ReleaseAuthority, ReleaseAuthorization, ReleaseManifest, SignError,
    ValidatedRelease, ValidationError, sign_channel,
};

const SYSTEMS: [&str; 2] = ["aarch64-darwin", "x86_64-linux"];
const DEFAULT_LANE: &str = "alpha";
const CARD_SCHEMA: u64 = 1;
const ROOT_VALIDITY_HOURS: i64 = 24 * 365 * 3;
const TIMESTAMP_FRESHNESS_HOURS: u64 = 168;
const SNAPSHOT_FRESHNESS_HOURS: u64 = 24 * 14;
const TARGETS_FRESHNESS_HOURS: u64 = 24 * 60;
const DETERMINATE_VERSION: &str = "3.22.1";
const DETERMINATE_REVISION: &str = "4132ad07a15ee7d88c096ac7172b7afb2672866b";
const DETERMINATE_LICENSE: &str = "LGPL-2.1";

/// One fixed Determinate inventory file that every targets directory must hold.
struct DeterminateFile {
    kind: &'static str,
    system: Option<&'static str>,
    file: &'static str,
    upstream_url: &'static str,
}

const DETERMINATE_FILES: [DeterminateFile; 5] = [
    DeterminateFile {
        kind: "installer",
        system: Some("aarch64-darwin"),
        file: "nix-installer-aarch64-darwin",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-darwin",
    },
    DeterminateFile {
        kind: "installer",
        system: Some("aarch64-linux"),
        file: "nix-installer-aarch64-linux",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux",
    },
    DeterminateFile {
        kind: "installer",
        system: Some("x86_64-linux"),
        file: "nix-installer-x86_64-linux",
        upstream_url: "https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux",
    },
    DeterminateFile {
        kind: "source",
        system: None,
        file: "nix-installer-v3.22.1.tar.gz",
        upstream_url: "https://codeload.github.com/DeterminateSystems/nix-installer/tar.gz/refs/tags/v3.22.1",
    },
    DeterminateFile {
        kind: "license",
        system: None,
        file: "LICENSE",
        upstream_url: "https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE",
    },
];

const CLI_FILES: [(&str, &str, &str); 3] = [
    ("pkg", "aarch64-darwin", "pkg-aarch64-darwin"),
    ("pkg", "x86_64-linux", "pkg-x86_64-linux"),
    ("pkg-install", "x86_64-linux", "pkg-installer-x86_64-linux"),
];

/// pkg-rel refusal reason.
///
/// Every variant carries the exact cause so a release operator can fix the
/// input without reading source code.
#[derive(Debug)]
pub enum RelError {
    /// Input, key material, or the staged targets directory is invalid.
    Invalid(String),
    /// Key generation or signing cryptography failed.
    Crypto(String),
    /// A filesystem operation failed at the given path.
    Filesystem {
        /// The io failure that occurred.
        source: std::io::Error,
        /// The path where the failure occurred.
        path: PathBuf,
    },
    /// The TUF signing transaction failed.
    Sign(SignError),
}

impl fmt::Display for RelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Crypto(message) => write!(formatter, "key crypto failed: {message}"),
            Self::Filesystem { source, path } => {
                write!(formatter, "{}: {source}", path.display())
            }
            Self::Sign(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for RelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sign(source) => Some(source),
            Self::Filesystem { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<SignError> for RelError {
    fn from(source: SignError) -> Self {
        Self::Sign(source)
    }
}

/// One pkg release environment. The environment binds a key set to its lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// The test root: PR, alpha, proof, and smoke lanes.
    Test,
    /// The production root: beta and prod lanes.
    Prod,
}

impl Environment {
    /// Returns the environment name used in the release card.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Prod => "prod",
        }
    }

    /// Parses the `--env` value of `pkg-rel key init`.
    ///
    /// # Errors
    ///
    /// Returns [`RelError::Invalid`] unless the value is `test` or `prod`.
    pub fn parse(value: &str) -> Result<Self, RelError> {
        match value {
            "test" => Ok(Self::Test),
            "prod" => Ok(Self::Prod),
            _ => Err(RelError::Invalid("--env must be test or prod".to_owned())),
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The published identity of one generated key set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySet {
    /// The hex key id of the online ed25519 key.
    pub online_key_id: String,
    /// The SHA-256 digest of the signed root bytes.
    pub root_sha256: String,
}

/// Inputs of one channel publication.
pub struct PublishChannel<'a> {
    /// Directory that holds `root.json` and `online-key.pk8`.
    pub key_dir: &'a Path,
    /// Directory that holds the complete staged release file set.
    pub targets_dir: &'a Path,
    /// Channel output directory; it must not exist yet.
    pub channel_dir: &'a Path,
    /// Metadata version used for targets, snapshot, and timestamp.
    pub sequence: u64,
    /// Release lane; defaults to `alpha`.
    pub lane: Option<&'a str>,
    /// Public channel URL recorded on the release card.
    pub url: Option<&'a str>,
    /// Product commit; defaults to `git rev-parse HEAD`.
    pub commit: Option<&'a str>,
}

/// One published file named by the release card.
#[derive(Debug, Clone, Serialize)]
pub struct CardTarget {
    /// The TUF target name.
    pub name: String,
    /// The SHA-256 digest of the target bytes.
    pub sha256: String,
    /// The exact target length in bytes.
    pub length: u64,
}

/// The metadata freshness policy recorded on the release card.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CardPolicy {
    /// Timestamp validity in hours.
    pub timestamp_hours: u64,
    /// Snapshot validity in hours.
    pub snapshot_hours: u64,
    /// Targets validity in hours.
    pub targets_hours: u64,
}

/// The one-line result of one publish: the numbers that replace hand-copied pins.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseCard {
    /// Release card schema version.
    pub schema: u64,
    /// Environment of the signing key set.
    pub environment: String,
    /// Release lane of this channel.
    pub lane: String,
    /// SHA-256 digest of the trusted root bytes.
    pub root_sha256: String,
    /// Hex key id of the online signing key.
    pub key_id: String,
    /// Product commit published in this channel.
    pub product_commit: String,
    /// Public channel URL, when one was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Metadata version of this publication.
    pub sequence: u64,
    /// Metadata freshness policy of this publication.
    pub policy: CardPolicy,
    /// Every TUF target with its digest and length.
    pub targets: Vec<CardTarget>,
}

impl ReleaseCard {
    /// Returns the card as one compact JSON line.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"schema\":0}".to_owned())
    }
}

#[derive(Clone)]
struct RelKey {
    pkcs8: Vec<u8>,
    key: Key,
    id: Decoded<Hex>,
}

impl fmt::Debug for RelKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RelKey(<redacted>)")
    }
}

impl RelKey {
    fn generate() -> Result<Self, RelError> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| RelError::Crypto("ed25519 key generation failed".to_owned()))?
            .as_ref()
            .to_vec();
        Self::from_pkcs8(pkcs8)
    }

    fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self, RelError> {
        let signer = parse_keypair(&pkcs8)
            .map_err(|_| RelError::Crypto("the ed25519 key bytes are unusable".to_owned()))?;
        let key = signer.tuf_key();
        let id = key
            .key_id()
            .map_err(|_| RelError::Crypto("the ed25519 key id is unusable".to_owned()))?;
        Ok(Self { pkcs8, key, id })
    }

    fn signer(&self) -> Result<Box<dyn Sign>, RelError> {
        let signer = parse_keypair(&self.pkcs8)
            .map_err(|_| RelError::Crypto("the ed25519 key bytes are unusable".to_owned()))?;
        Ok(Box::new(signer))
    }

    fn id_hex(&self) -> String {
        hex::encode(self.id.as_ref())
    }
}

#[async_trait]
impl KeySource for RelKey {
    async fn as_sign(&self) -> Result<Box<dyn Sign>, Box<dyn std::error::Error + Send + Sync>> {
        self.signer().map_err(|error| error.to_string().into())
    }

    async fn write(
        &self,
        _value: &str,
        _key_id_hex: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("pkg-rel keys are file-backed".into())
    }
}

struct PkgRelAuthorization;

impl ReleaseAuthorization for PkgRelAuthorization {
    fn lease_id(&self) -> &'static str {
        "pkg-rel-lease"
    }

    fn signing_actor(&self) -> &'static str {
        "pkg-rel"
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

struct PkgRelAuthority;

impl ReleaseAuthority for PkgRelAuthority {
    fn authorize(
        &self,
        digest: &str,
        sequence: u64,
        timestamp_version: u64,
        approvals: &[Approval],
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
        let evidence: Vec<_> = approvals.iter().map(Approval::evidence).collect();
        if digest.len() != 64
            || sequence == 0
            || sequence != timestamp_version
            || evidence != ["pkg-rel:release", "pkg-rel:security"]
        {
            return Err(ValidationError::InvalidPolicy);
        }
        Ok(Box::new(PkgRelAuthorization))
    }

    fn resume(
        &self,
        digest: &str,
        transaction_digest: &str,
        lease_id: &str,
    ) -> Result<Box<dyn ReleaseAuthorization>, ValidationError> {
        if digest.len() != 64 || transaction_digest.len() != 64 || lease_id != "pkg-rel-lease" {
            return Err(ValidationError::InvalidPolicy);
        }
        Ok(Box::new(PkgRelAuthorization))
    }
}

/// Creates one long-lived key set for one environment.
///
/// Writes `root.json`, `1.root.json` (identical bytes), the mode-0600
/// `online-key.pk8`, and a custody `README.md` into a new `directory`. The
/// root private key signs `root.json` once and is then discarded; it is never
/// written anywhere.
///
/// # Errors
///
/// Returns [`RelError::Invalid`] when the directory already exists, and a
/// filesystem or crypto error when generation, signing, or writing fails.
pub async fn init_key_set(directory: &Path, environment: Environment) -> Result<KeySet, RelError> {
    if directory.exists() {
        return Err(RelError::Invalid(format!(
            "the key directory already exists: {}",
            directory.display()
        )));
    }
    DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .map_err(at(directory))?;
    let root_key = RelKey::generate()?;
    let online_key = RelKey::generate()?;
    let expires = Timestamp::now() + SignedDuration::from_hours(ROOT_VALIDITY_HOURS);
    let root_bytes = signed_root(&root_key, &online_key, environment, expires).await?;
    let root_copy = directory.join("root.json");
    fs::write(&root_copy, &root_bytes).map_err(at(&root_copy))?;
    let versioned_root = directory.join("1.root.json");
    fs::write(&versioned_root, &root_bytes).map_err(at(&versioned_root))?;
    write_private(&directory.join("online-key.pk8"), &online_key.pkcs8)?;
    let key_set = KeySet {
        online_key_id: online_key.id_hex(),
        root_sha256: hex::encode(Sha256::digest(&root_bytes)),
    };
    let readme = directory.join("README.md");
    fs::write(&readme, key_readme(environment, &key_set, expires)).map_err(at(&readme))?;
    Ok(key_set)
}

async fn signed_root(
    root_key: &RelKey,
    online_key: &RelKey,
    environment: Environment,
    expires: Timestamp,
) -> Result<Vec<u8>, RelError> {
    let mut keys = HashMap::new();
    keys.insert(root_key.id.clone(), root_key.key.clone());
    keys.insert(online_key.id.clone(), online_key.key.clone());
    let role = |key: &RelKey| RoleKeys {
        keyids: vec![key.id.clone()],
        threshold: NonZeroU64::MIN,
        _extra: HashMap::new(),
    };
    let mut roles = HashMap::new();
    roles.insert(RoleType::Root, role(root_key));
    for role_type in [RoleType::Targets, RoleType::Snapshot, RoleType::Timestamp] {
        roles.insert(role_type, role(online_key));
    }
    let mut extra = HashMap::new();
    extra.insert("pkgEnvironment".to_owned(), json!(environment.as_str()));
    let root = Root {
        spec_version: "1.0.0".to_owned(),
        consistent_snapshot: true,
        version: NonZeroU64::MIN,
        expires,
        keys,
        roles,
        _extra: extra,
    };
    let mut canonical = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut canonical, CanonicalFormatter::new());
    root.serialize(&mut serializer)
        .map_err(|_| RelError::Crypto("root serialization failed".to_owned()))?;
    let signature = root_key
        .signer()?
        .sign(&canonical, &SystemRandom::new())
        .await
        .map_err(|_| RelError::Crypto("root signing failed".to_owned()))?;
    let signatures = vec![Signature {
        keyid: root_key.id.clone(),
        sig: signature.into(),
    }];
    let mut bytes = serde_json::to_vec_pretty(&Signed {
        signed: root,
        signatures,
    })
    .map_err(|_| RelError::Crypto("root serialization failed".to_owned()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), RelError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(at(path))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(at(path))?;
    Ok(())
}

fn key_readme(environment: Environment, key_set: &KeySet, expires: Timestamp) -> String {
    format!(
        "# pkg-rel key directory ({environment})\n\n\
         This directory holds the long-lived signing identity for one environment.\n\n\
         ## Warnings\n\n\
         - Keep this directory encrypted at rest. Use age, Bitwarden, or an\n\
         encrypted USB copy.\n\
         - Never commit this directory to git.\n\
         - The root private key was discarded after root.json was signed. It\n\
         exists nowhere and is not recoverable.\n\n\
         ## Records\n\n\
         - environment: {environment}\n\
         - online key id: {online_key_id}\n\
         - root sha256: {root_sha256}\n\
         - root expires: {expires}\n\n\
         ## Files\n\n\
         - `root.json` and `1.root.json`: the signed root, identical bytes.\n\
         - `online-key.pk8`: the online signing key, mode 0600.\n",
        online_key_id = key_set.online_key_id,
        root_sha256 = key_set.root_sha256,
    )
}

/// Publishes one signed channel from a staged targets directory.
///
/// Builds the release manifest from the files in the targets directory, signs
/// targets, snapshot, and timestamp with the key set's online key, seals the
/// channel into the output directory, and returns the release card.
///
/// # Errors
///
/// Returns [`RelError::Invalid`] for any refused input or layout, and a
/// filesystem or signing error when the channel cannot be produced.
#[expect(
    clippy::future_not_send,
    reason = "release signing runs single-threaded; key sources stay on one thread"
)]
pub async fn publish_channel(request: PublishChannel<'_>) -> Result<ReleaseCard, RelError> {
    publish_validated_by(request, ReleaseManifest::from_json).await
}

/// Validates serialized manifest bytes against a staged artifact directory.
type ManifestValidator =
    fn(&[u8], &Path, &dyn ReleaseAuthority) -> Result<ValidatedRelease, ValidationError>;

#[expect(
    clippy::future_not_send,
    reason = "release signing runs single-threaded; key sources stay on one thread"
)]
pub async fn publish_validated_by(
    request: PublishChannel<'_>,
    validate: ManifestValidator,
) -> Result<ReleaseCard, RelError> {
    let sequence = NonZeroU64::new(request.sequence)
        .ok_or_else(|| RelError::Invalid("--sequence must be at least 1".to_owned()))?;
    let lane = resolve_lane(request.lane)?;
    validate_url(request.url)?;
    let keys = load_key_dir(request.key_dir)?;
    let product_commit = resolve_commit(request.commit)?;
    let staged = stage_targets(request.targets_dir, sequence.get())?;
    let release_id = format!("{lane}-{}", sequence.get());
    let manifest = manifest_value(&keys.root_sha256, &release_id, sequence.get(), &staged)?;
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|_| RelError::Invalid("release manifest serialization failed".to_owned()))?;
    let release = validate(&manifest_bytes, request.targets_dir, &PkgRelAuthority)
        .map_err(|error| RelError::Invalid(format!("the targets directory is refused: {error}")))?;
    let card_targets = release
        .tuf_targets()
        .map(|(name, _, sha256, length)| CardTarget {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
            length,
        })
        .collect();
    let policy = freshness_policy(sequence)?;
    seal_channel(&request, &keys, release, policy).await?;
    Ok(ReleaseCard {
        schema: CARD_SCHEMA,
        environment: keys.environment.as_str().to_owned(),
        lane: lane.to_owned(),
        root_sha256: keys.root_sha256,
        key_id: keys.online_key_id,
        product_commit,
        url: request.url.map(str::to_owned),
        sequence: sequence.get(),
        policy: CardPolicy {
            timestamp_hours: TIMESTAMP_FRESHNESS_HOURS,
            snapshot_hours: SNAPSHOT_FRESHNESS_HOURS,
            targets_hours: TARGETS_FRESHNESS_HOURS,
        },
        targets: card_targets,
    })
}

#[expect(
    clippy::future_not_send,
    reason = "release signing runs single-threaded; key sources stay on one thread"
)]
async fn seal_channel(
    request: &PublishChannel<'_>,
    keys: &TrustedKeyDir,
    release: ValidatedRelease,
    policy: MetadataPolicy,
) -> Result<(), RelError> {
    let online_keys: Vec<Box<dyn KeySource>> = vec![Box::new(keys.online.clone())];
    sign_channel(
        release,
        &request.key_dir.join("root.json"),
        &online_keys,
        policy,
        request.channel_dir,
    )
    .await
    .map_err(RelError::Sign)?;
    let channel_root = request.channel_dir.join("root.json");
    fs::write(&channel_root, &keys.root_bytes).map_err(at(&channel_root))
}

struct TrustedKeyDir {
    root_bytes: Vec<u8>,
    root_sha256: String,
    environment: Environment,
    online: RelKey,
    online_key_id: String,
}

fn load_key_dir(key_dir: &Path) -> Result<TrustedKeyDir, RelError> {
    let root_path = key_dir.join("root.json");
    let root_bytes = fs::read(&root_path).map_err(at(&root_path))?;
    let root: Signed<Root> = serde_json::from_slice(&root_bytes).map_err(|_| {
        RelError::Invalid(format!("{} is not a signed TUF root", root_path.display()))
    })?;
    root.signed
        .verify_role(&root)
        .map_err(|_| RelError::Invalid("the key directory root.json does not verify".to_owned()))?;
    let environment = root
        .signed
        ._extra
        .get("pkgEnvironment")
        .and_then(Json::as_str)
        .and_then(|value| Environment::parse(value).ok())
        .ok_or_else(|| {
            RelError::Invalid(
                "the key directory root.json carries no valid pkg-rel environment".to_owned(),
            )
        })?;
    let key_path = key_dir.join("online-key.pk8");
    let pkcs8 = fs::read(&key_path).map_err(at(&key_path))?;
    let online = RelKey::from_pkcs8(pkcs8)?;
    Ok(TrustedKeyDir {
        root_sha256: hex::encode(Sha256::digest(&root_bytes)),
        online_key_id: online.id_hex(),
        root_bytes,
        environment,
        online,
    })
}

fn freshness_policy(sequence: NonZeroU64) -> Result<MetadataPolicy, RelError> {
    let hours = |hours: u64| {
        i64::try_from(hours)
            .map_err(|_| RelError::Invalid("the freshness window is out of range".to_owned()))
    };
    let now = Timestamp::now();
    Ok(MetadataPolicy {
        targets_version: sequence,
        snapshot_version: sequence,
        timestamp_version: sequence,
        targets_expires: now + SignedDuration::from_hours(hours(TARGETS_FRESHNESS_HOURS)?),
        snapshot_expires: now + SignedDuration::from_hours(hours(SNAPSHOT_FRESHNESS_HOURS)?),
        timestamp_expires: now + SignedDuration::from_hours(hours(TIMESTAMP_FRESHNESS_HOURS)?),
    })
}

fn resolve_lane(lane: Option<&str>) -> Result<&str, RelError> {
    let lane = lane.unwrap_or(DEFAULT_LANE);
    let valid = (1..=32).contains(&lane.len())
        && lane
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    valid.then_some(lane).ok_or_else(|| {
        RelError::Invalid("--lane must use lowercase letters, digits, and dashes".to_owned())
    })
}

fn validate_url(url: Option<&str>) -> Result<(), RelError> {
    let Some(value) = url else {
        return Ok(());
    };
    match Url::parse(value) {
        Ok(parsed) if parsed.scheme() == "https" => Ok(()),
        _ => Err(RelError::Invalid(
            "--url must be an https:// URL".to_owned(),
        )),
    }
}

fn resolve_commit(commit: Option<&str>) -> Result<String, RelError> {
    match commit {
        Some(value) if valid_commit(value) => Ok(value.to_owned()),
        Some(_) => Err(RelError::Invalid(
            "--commit must be a 40 or 64 character lowercase hex commit".to_owned(),
        )),
        None => git_head_commit(),
    }
}

fn git_head_commit() -> Result<String, RelError> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|source| {
            RelError::Invalid(format!(
                "git rev-parse HEAD failed: {source}; pass --commit explicitly"
            ))
        })?;
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || !valid_commit(&commit) {
        return Err(RelError::Invalid(
            "git rev-parse HEAD failed; pass --commit when publishing outside a repository"
                .to_owned(),
        ));
    }
    Ok(commit)
}

fn valid_commit(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

struct ArtifactSpec {
    kind: &'static str,
    system: Option<&'static str>,
    path: String,
}

struct FileRecord {
    sha256: String,
    length: u64,
}

struct StagedTargets {
    specs: Vec<ArtifactSpec>,
    records: BTreeMap<String, FileRecord>,
}

fn stage_targets(targets: &Path, sequence: u64) -> Result<StagedTargets, RelError> {
    let nix_version = discover_layout(targets, sequence)?;
    let specs = product_artifacts(sequence, &nix_version);
    let expected = expected_paths(&specs);
    verify_closed_tree(targets, &expected)?;
    let mut records = BTreeMap::new();
    for relative in &expected {
        let record = stage_file(targets, relative)?;
        records.insert(relative.clone(), record);
    }
    Ok(StagedTargets { specs, records })
}

fn determinate_target(file: &str) -> String {
    format!("determinate/{DETERMINATE_VERSION}/{file}")
}

fn product_artifacts(sequence: u64, nix_version: &str) -> Vec<ArtifactSpec> {
    let mut specs = vec![ArtifactSpec {
        kind: "descriptor",
        system: None,
        path: "descriptor.json".to_owned(),
    }];
    for system in SYSTEMS {
        let entries = [
            ("managed-nix", format!("nix/{nix_version}/{system}.tar.xz")),
            (
                "managed-nix-assets",
                format!("nix/{nix_version}/{system}.assets.json"),
            ),
            ("index", format!("index/{sequence}/{system}.json.br")),
            ("installer-payload", format!("installer/{system}/pkg")),
            (
                "installer-payload",
                format!("installer/{system}/pkg-nix-broker"),
            ),
            (
                "installer-payload",
                format!("installer/{system}/pkg-root-helper"),
            ),
        ];
        for (kind, path) in entries {
            specs.push(ArtifactSpec {
                kind,
                system: Some(system),
                path,
            });
        }
    }
    specs
}

fn expected_paths(specs: &[ArtifactSpec]) -> BTreeSet<String> {
    let mut paths: BTreeSet<_> = specs.iter().map(|spec| spec.path.clone()).collect();
    for file in DETERMINATE_FILES {
        paths.insert(determinate_target(file.file));
    }
    for (_, _, base) in CLI_FILES {
        paths.insert(format!("cli/{base}"));
        paths.insert(format!("cli/{base}.sigstore.json"));
    }
    paths
}

fn discover_layout(targets: &Path, sequence: u64) -> Result<String, RelError> {
    let nix_version = single_subdirectory(&targets.join("nix"), "nix runtime")?;
    if nix_version.is_empty()
        || nix_version.len() > 64
        || !nix_version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err(RelError::Invalid(format!(
            "the nix runtime version {nix_version} is invalid"
        )));
    }
    let index = single_subdirectory(&targets.join("index"), "index")?;
    if index != sequence.to_string() {
        return Err(RelError::Invalid(format!(
            "the index directory {index} does not match sequence {sequence}"
        )));
    }
    Ok(nix_version)
}

fn single_subdirectory(directory: &Path, what: &str) -> Result<String, RelError> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|_| RelError::Invalid(format!("the targets directory has no {what} directory")))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RelError::Invalid(format!(
            "the {what} directory must be a regular directory"
        )));
    }
    let mut names: Vec<String> = fs::read_dir(directory)
        .map_err(at(directory))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|_| RelError::Invalid(format!("the {what} directory is unreadable")))
        })
        .collect::<Result<_, _>>()?;
    if names.len() != 1 {
        return Err(RelError::Invalid(format!(
            "the {what} directory must contain exactly one subdirectory"
        )));
    }
    let name = names.pop().unwrap_or_default();
    let child = directory.join(&name);
    let child_metadata = fs::symlink_metadata(&child).map_err(at(&child))?;
    if !child_metadata.is_dir() || child_metadata.file_type().is_symlink() {
        return Err(RelError::Invalid(format!(
            "the {what} directory must contain exactly one subdirectory"
        )));
    }
    Ok(name)
}

fn verify_closed_tree(targets: &Path, expected: &BTreeSet<String>) -> Result<(), RelError> {
    let actual = scan_tree(targets)?;
    if let Some(missing) = expected.difference(&actual).next() {
        return Err(RelError::Invalid(format!(
            "the targets directory is missing {missing}"
        )));
    }
    if let Some(unexpected) = actual.difference(expected).next() {
        return Err(RelError::Invalid(format!(
            "the targets directory has an unexpected file {unexpected}"
        )));
    }
    Ok(())
}

fn scan_tree(root: &Path) -> Result<BTreeSet<String>, RelError> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|_| RelError::Invalid("the targets directory is unavailable".to_owned()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RelError::Invalid(
            "the targets directory must be a regular directory".to_owned(),
        ));
    }
    let mut files = BTreeSet::new();
    visit_tree(root, root, &mut files)?;
    Ok(files)
}

fn visit_tree(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<(), RelError> {
    for entry in fs::read_dir(directory).map_err(at(directory))? {
        let entry = entry.map_err(at(directory))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(at(&path))?;
        if metadata.file_type().is_symlink() {
            return Err(RelError::Invalid(
                "the targets directory contains a symlink".to_owned(),
            ));
        }
        if metadata.is_dir() {
            visit_tree(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(RelError::Invalid(
                "the targets directory contains a non-file entry".to_owned(),
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| RelError::Invalid("the targets directory path is invalid".to_owned()))?
            .to_str()
            .ok_or_else(|| RelError::Invalid("the targets directory path is not UTF-8".to_owned()))?
            .replace('\\', "/");
        files.insert(relative);
    }
    Ok(())
}

fn stage_file(targets: &Path, relative: &str) -> Result<FileRecord, RelError> {
    let path = targets.join(relative);
    let bytes = fs::read(&path).map_err(at(&path))?;
    if relative.starts_with("cli/") && relative.ends_with(".sigstore.json") {
        require_sigstore_bundle(&bytes)?;
    }
    Ok(FileRecord {
        sha256: hex::encode(Sha256::digest(&bytes)),
        length: bytes.len() as u64,
    })
}

fn require_sigstore_bundle(bytes: &[u8]) -> Result<(), RelError> {
    let value: Json = serde_json::from_slice(bytes)
        .map_err(|_| RelError::Invalid("a cli sigstore bundle is not JSON".to_owned()))?;
    let invalid = || RelError::Invalid("a cli sigstore bundle envelope is invalid".to_owned());
    let verification = value
        .get("verificationMaterial")
        .and_then(Json::as_object)
        .ok_or_else(invalid)?;
    let certificate = verification
        .get("certificate")
        .and_then(|item| item.get("rawBytes"))
        .and_then(Json::as_str)
        .is_some_and(|value| !value.is_empty());
    let chain = verification
        .get("x509CertificateChain")
        .and_then(|item| item.get("certificates"))
        .and_then(Json::as_array)
        .is_some_and(|certificates| {
            certificates.iter().any(|certificate| {
                certificate
                    .get("rawBytes")
                    .and_then(Json::as_str)
                    .is_some_and(|value| !value.is_empty())
            })
        });
    let signature = value
        .get("messageSignature")
        .and_then(Json::as_object)
        .ok_or_else(invalid)?;
    let digest = signature
        .get("messageDigest")
        .and_then(Json::as_object)
        .ok_or_else(invalid)?;
    if value.get("mediaType").and_then(Json::as_str)
        != Some("application/vnd.dev.sigstore.bundle.v0.3+json")
        || value.get("proofFixture").is_some()
        || (!certificate && !chain)
        || digest.get("algorithm").and_then(Json::as_str) != Some("SHA2_256")
        || digest
            .get("digest")
            .and_then(Json::as_str)
            .is_none_or(str::is_empty)
        || signature
            .get("signature")
            .and_then(Json::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(invalid());
    }
    Ok(())
}

fn record<'a>(staged: &'a StagedTargets, path: &str) -> Result<&'a FileRecord, RelError> {
    staged
        .records
        .get(path)
        .ok_or_else(|| RelError::Invalid(format!("the targets staging is missing {path}")))
}

fn manifest_value(
    root_sha256: &str,
    release_id: &str,
    sequence: u64,
    staged: &StagedTargets,
) -> Result<Json, RelError> {
    let artifacts = product_artifact_entries(staged)?;
    let determinate = determinate_entries(staged)?;
    let cli = cli_entries(staged)?;
    Ok(json!({
        "schemaVersion": 2,
        "releaseId": release_id,
        "channelSequence": sequence,
        "timestampVersion": sequence,
        "trustedRootSha256": root_sha256,
        "policyVersion": 1,
        "determinate": {
            "version": DETERMINATE_VERSION,
            "revision": DETERMINATE_REVISION,
            "license": DETERMINATE_LICENSE,
            "artifacts": determinate,
        },
        "artifacts": artifacts,
        "cliArtifacts": cli,
        "approvals": [
            {"actor": "pkg-rel", "role": "release", "evidence": "pkg-rel:release"},
            {"actor": "pkg-rel-security", "role": "security", "evidence": "pkg-rel:security"},
        ],
    }))
}

fn product_artifact_entries(staged: &StagedTargets) -> Result<Vec<Json>, RelError> {
    staged
        .specs
        .iter()
        .map(|spec| {
            let entry = record(staged, &spec.path)?;
            Ok(json!({
                "kind": spec.kind,
                "system": spec.system,
                "target": spec.path,
                "source": spec.path,
                "sha256": entry.sha256,
                "length": entry.length,
            }))
        })
        .collect()
}

fn determinate_entries(staged: &StagedTargets) -> Result<Vec<Json>, RelError> {
    DETERMINATE_FILES
        .iter()
        .map(|file| {
            let target = determinate_target(file.file);
            let entry = record(staged, &target)?;
            Ok(json!({
                "kind": file.kind,
                "system": file.system,
                "target": target,
                "source": target,
                "upstreamUrl": file.upstream_url,
                "sha256": entry.sha256,
                "length": entry.length,
            }))
        })
        .collect()
}

fn cli_entries(staged: &StagedTargets) -> Result<Vec<Json>, RelError> {
    CLI_FILES
        .iter()
        .map(|&(kind, system, base)| {
            let source = format!("cli/{base}");
            let bundle = format!("{source}.sigstore.json");
            let payload = record(staged, &source)?;
            let bundle_record = record(staged, &bundle)?;
            Ok(json!({
                "kind": kind,
                "system": system,
                "source": source,
                "sha256": payload.sha256,
                "length": payload.length,
                "sigstoreBundle": bundle,
                "sigstoreBundleSha256": bundle_record.sha256,
                "sigstoreBundleLength": bundle_record.length,
            }))
        })
        .collect()
}

fn at(path: &Path) -> impl Fn(std::io::Error) -> RelError {
    move |source| RelError::Filesystem {
        source,
        path: path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::num::NonZeroU64;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tough::schema::Root;
    use tough::schema::decoded::Decoded;
    use tough::schema::decoded::Hex;
    use tough::schema::{RoleType, Signed};
    use tough::{
        ExpirationEnforcement, FilesystemTransport, IntoVec, RepositoryLoader, TargetName,
    };
    use url::Url;

    use super::{
        Environment, ManifestValidator, PublishChannel, init_key_set, publish_validated_by,
        require_sigstore_bundle, resolve_commit, resolve_lane, validate_url,
    };

    const SIGSTORE_BUNDLE: &str = "{\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\",\"verificationMaterial\":{\"certificate\":{\"rawBytes\":\"Y2VydA==\"}},\"messageSignature\":{\"messageDigest\":{\"algorithm\":\"SHA2_256\",\"digest\":\"ZGlnZXN0\"},\"signature\":\"c2lnbmF0dXJl\"}}";
    const PRODUCT_COMMIT: &str = "8ffd325a4be12a998f3a5684097b57841a11540e";
    const DETERMINATE_FIXTURES: [(&str, &str); 5] = [
        (
            "nix-installer-aarch64-darwin",
            "fixture determinate installer aarch64-darwin\n",
        ),
        (
            "nix-installer-aarch64-linux",
            "fixture determinate installer aarch64-linux\n",
        ),
        (
            "nix-installer-x86_64-linux",
            "fixture determinate installer x86_64-linux\n",
        ),
        (
            "nix-installer-v3.22.1.tar.gz",
            "fixture determinate source\n",
        ),
        ("LICENSE", "fixture determinate license\n"),
    ];

    fn write_fixture_file(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(path, bytes).expect("write fixture file");
    }

    fn write_fixture_targets(root: &Path) {
        write_fixture_file(root, "descriptor.json", b"fixture descriptor\n");
        for system in super::SYSTEMS {
            write_fixture_file(
                root,
                &format!("nix/2.34.8/{system}.tar.xz"),
                format!("runtime {system}\n").as_bytes(),
            );
            write_fixture_file(
                root,
                &format!("nix/2.34.8/{system}.assets.json"),
                format!("assets {system}\n").as_bytes(),
            );
            write_fixture_file(
                root,
                &format!("index/1/{system}.json.br"),
                format!("index {system}\n").as_bytes(),
            );
            for name in ["pkg", "pkg-nix-broker", "pkg-root-helper"] {
                write_fixture_file(
                    root,
                    &format!("installer/{system}/{name}"),
                    format!("payload {name} {system}\n").as_bytes(),
                );
            }
        }
        for (file, content) in DETERMINATE_FIXTURES {
            write_fixture_file(
                root,
                &format!("determinate/3.22.1/{file}"),
                content.as_bytes(),
            );
        }
        for (_, _, base) in super::CLI_FILES {
            write_fixture_file(
                root,
                &format!("cli/{base}"),
                format!("cli payload {base}\n").as_bytes(),
            );
            write_fixture_file(
                root,
                &format!("cli/{base}.sigstore.json"),
                SIGSTORE_BUNDLE.as_bytes(),
            );
        }
    }

    fn fixture_validator() -> ManifestValidator {
        crate::manifest::ReleaseManifest::from_json_with_determinate_fixture
    }

    fn fixture_publish<'a>(
        key_dir: &'a Path,
        targets_dir: &'a Path,
        channel_dir: &'a Path,
    ) -> PublishChannel<'a> {
        PublishChannel {
            key_dir,
            targets_dir,
            channel_dir,
            sequence: 1,
            lane: Some("alpha"),
            url: Some("https://channel.kelv.dev/alpha/"),
            commit: Some(PRODUCT_COMMIT),
        }
    }

    #[tokio::test]
    async fn key_init_writes_a_verifiable_three_year_root() {
        let temporary = TempDir::new().expect("temporary key workspace");
        let key_dir = temporary.path().join("keys");
        let key_set = init_key_set(&key_dir, Environment::Test)
            .await
            .expect("key init");
        let root_bytes = fs::read(key_dir.join("root.json")).expect("read root.json");
        let root: Signed<Root> = serde_json::from_slice(&root_bytes).expect("parse root");
        root.signed
            .verify_role(&root)
            .expect("tough verifies the root");
        assert!(root.signed.consistent_snapshot);
        assert_eq!(root.signed.version, NonZeroU64::MIN);
        assert_eq!(
            root.signed._extra.get("pkgEnvironment"),
            Some(&json!("test"))
        );

        let now = jiff::Timestamp::now();
        let expected = jiff::SignedDuration::from_hours(24 * 365 * 3);
        assert!(root.signed.expires > now + expected - jiff::SignedDuration::from_hours(48));
        assert!(root.signed.expires < now + expected + jiff::SignedDuration::from_hours(48));

        assert_eq!(
            fs::read(key_dir.join("1.root.json")).expect("read 1.root.json"),
            root_bytes
        );
        assert_eq!(
            key_set.root_sha256,
            hex::encode(Sha256::digest(&root_bytes))
        );

        let threshold = NonZeroU64::new(1).expect("threshold");
        let online_id = Decoded::<Hex>::from(hex::decode(&key_set.online_key_id).expect("hex id"));
        let roles = &root.signed.roles;
        assert_eq!(roles.len(), 4);
        for role_type in [RoleType::Targets, RoleType::Snapshot, RoleType::Timestamp] {
            let role = &roles[&role_type];
            assert_eq!(role.threshold, threshold);
            assert_eq!(role.keyids, vec![online_id.clone()]);
        }
        let root_role = &roles[&RoleType::Root];
        assert_eq!(root_role.threshold, threshold);
        assert_eq!(root_role.keyids.len(), 1);
        assert_ne!(root_role.keyids[0], online_id);
        assert_eq!(root.signed.keys.len(), 2);

        let mode = fs::metadata(key_dir.join("online-key.pk8"))
            .expect("read key metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        let readme = fs::read_to_string(key_dir.join("README.md")).expect("read README");
        assert!(readme.contains("encrypted at rest"));
        assert!(readme.contains("discarded after root.json was signed"));
        assert!(readme.contains("Never commit this directory"));
    }

    #[tokio::test]
    async fn key_init_refuses_an_existing_directory() {
        let temporary = TempDir::new().expect("temporary key workspace");
        let key_dir = temporary.path().join("keys");
        fs::create_dir(&key_dir).expect("existing key directory");
        assert!(init_key_set(&key_dir, Environment::Prod).await.is_err());
        assert_eq!(
            Environment::parse("staging")
                .expect_err("bad environment")
                .to_string(),
            "--env must be test or prod"
        );
    }

    #[tokio::test]
    async fn publish_prints_a_card_that_matches_a_reloadable_channel() {
        let temporary = TempDir::new().expect("temporary publish workspace");
        let key_dir = temporary.path().join("keys");
        let targets_dir = temporary.path().join("targets");
        let channel_dir = temporary.path().join("channel");
        let key_set = init_key_set(&key_dir, Environment::Test)
            .await
            .expect("key init");
        write_fixture_targets(&targets_dir);

        let card = publish_validated_by(
            fixture_publish(&key_dir, &targets_dir, &channel_dir),
            fixture_validator(),
        )
        .await
        .expect("publish the fixture channel");

        let value: serde_json::Value =
            serde_json::from_str(&card.to_json_line()).expect("parse the card line");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["environment"], "test");
        assert_eq!(value["lane"], "alpha");
        assert_eq!(value["sequence"], 1);
        assert_eq!(value["url"], "https://channel.kelv.dev/alpha/");
        assert_eq!(value["product_commit"], PRODUCT_COMMIT);
        assert_eq!(value["key_id"], key_set.online_key_id);
        assert_eq!(value["policy"]["timestamp_hours"], 168);
        assert_eq!(value["policy"]["snapshot_hours"], 336);
        assert_eq!(value["policy"]["targets_hours"], 1440);
        let root_bytes = fs::read(key_dir.join("root.json")).expect("read root");
        assert_eq!(
            value["root_sha256"],
            hex::encode(Sha256::digest(&root_bytes))
        );

        let targets = value["targets"].as_array().expect("card targets");
        assert_eq!(targets.len(), 18);
        for target in targets {
            let name = target["name"].as_str().expect("target name");
            let bytes = fs::read(targets_dir.join(name)).expect("read staged target");
            assert_eq!(target["sha256"], hex::encode(Sha256::digest(&bytes)));
            assert_eq!(target["length"], bytes.len() as u64);
        }

        assert_eq!(
            fs::read(channel_dir.join("root.json")).expect("channel root"),
            root_bytes
        );
        for name in [
            "metadata/1.root.json",
            "metadata/1.targets.json",
            "metadata/1.snapshot.json",
            "metadata/timestamp.json",
        ] {
            assert!(channel_dir.join(name).is_file(), "missing {name}");
        }

        let datastore = TempDir::new().expect("client datastore");
        let repository = RepositoryLoader::new(
            &root_bytes,
            Url::from_directory_path(channel_dir.join("metadata")).expect("metadata URL"),
            Url::from_directory_path(channel_dir.join("targets")).expect("targets URL"),
        )
        .transport(FilesystemTransport)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .datastore(datastore.path())
        .load()
        .await
        .expect("client reloads the signed channel");
        let stream = repository
            .read_target(&TargetName::new("descriptor.json").expect("target name"))
            .await
            .expect("read target")
            .expect("target exists");
        let bytes = IntoVec::<tough::error::Error>::into_vec(stream)
            .await
            .expect("verified target bytes");
        assert_eq!(bytes, b"fixture descriptor\n");
    }

    #[tokio::test]
    async fn publish_refuses_target_trees_that_are_not_the_closed_set() {
        let temporary = TempDir::new().expect("temporary publish workspace");
        let key_dir = temporary.path().join("keys");
        let targets_dir = temporary.path().join("targets");
        init_key_set(&key_dir, Environment::Test)
            .await
            .expect("key init");
        write_fixture_targets(&targets_dir);
        write_fixture_file(&targets_dir, "extra.txt", b"unexpected\n");
        let channel_dir = temporary.path().join("channel");
        let error = publish_validated_by(
            fixture_publish(&key_dir, &targets_dir, &channel_dir),
            fixture_validator(),
        )
        .await
        .expect_err("extra file must be refused");
        assert!(
            error.to_string().contains("unexpected file"),
            "unexpected refusal: {error}"
        );
        assert!(!channel_dir.exists());
    }

    #[tokio::test]
    async fn publish_refuses_a_placeholder_sigstore_bundle() {
        let temporary = TempDir::new().expect("temporary publish workspace");
        let key_dir = temporary.path().join("keys");
        let targets_dir = temporary.path().join("targets");
        init_key_set(&key_dir, Environment::Test)
            .await
            .expect("key init");
        write_fixture_targets(&targets_dir);
        write_fixture_file(
            &targets_dir,
            "cli/pkg-aarch64-darwin.sigstore.json",
            b"{\"proofFixture\":true}",
        );
        let channel_dir = temporary.path().join("channel");
        let error = publish_validated_by(
            fixture_publish(&key_dir, &targets_dir, &channel_dir),
            fixture_validator(),
        )
        .await
        .expect_err("placeholder bundle must be refused");
        assert!(
            error.to_string().contains("sigstore bundle"),
            "unexpected refusal: {error}"
        );
        assert!(!channel_dir.exists());
    }

    #[test]
    fn helpers_validate_lane_url_commit_and_bundle_envelopes() {
        assert_eq!(resolve_lane(None).expect("default lane"), "alpha");
        assert_eq!(resolve_lane(Some("pr")).expect("pr lane"), "pr");
        assert!(resolve_lane(Some("Beta Lane")).is_err());
        assert!(validate_url(Some("https://channel.kelv.dev/alpha/")).is_ok());
        assert!(validate_url(Some("http://channel.kelv.dev/")).is_err());
        assert!(validate_url(None).is_ok());
        assert_eq!(
            resolve_commit(Some(PRODUCT_COMMIT)).expect("pinned commit"),
            PRODUCT_COMMIT
        );
        assert!(resolve_commit(Some("main")).is_err());
        require_sigstore_bundle(SIGSTORE_BUNDLE.as_bytes()).expect("valid envelope");
        assert!(require_sigstore_bundle(b"{\"proofFixture\":true}").is_err());
        assert!(require_sigstore_bundle(b"plain text").is_err());
    }
}
