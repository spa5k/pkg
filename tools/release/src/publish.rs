//! Immutable two-destination publication with the source of truth committed last.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tough::schema::{Root, Signed, Snapshot, Targets, Timestamp as TufTimestamp};

use crate::{
    ReleaseAuthority, ReleaseAuthorization, SignedRelease, SignedTimestampRefresh,
    TimestampAuthority, TimestampAuthorization,
};

/// One immutable byte object in a release publication.
#[derive(Debug, Clone)]
pub struct PublicationObject {
    name: String,
    file: Arc<fs::File>,
    sha256: String,
    length: u64,
}

/// Read-only cursor over an anonymous, sealed publication file.
#[derive(Debug)]
pub struct SealedReader {
    file: Arc<fs::File>,
    offset: u64,
    length: u64,
}

/// Reloadable full-release transaction persisted before any remote mutation.
pub struct DurableRelease {
    release_id: String,
    manifest_digest: String,
    authorization: Box<dyn ReleaseAuthorization>,
    objects: Vec<PublicationObject>,
    directory: PathBuf,
}

/// Reloadable timestamp-refresh transaction persisted before any remote mutation.
pub struct DurableTimestampRefresh {
    release_id: String,
    timestamp_version: u64,
    timestamp_digest: String,
    authorization: Box<dyn TimestampAuthorization>,
    objects: Vec<PublicationObject>,
    directory: PathBuf,
}

/// Remote stable-route state for one exact version and digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStatus {
    /// No value is active at this identity.
    Missing,
    /// The exact requested value is already active.
    Exact,
    /// A different value occupies the identity.
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TransactionKind {
    Release,
    Timestamp,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransactionRecord {
    schema_version: u64,
    kind: TransactionKind,
    release_id: String,
    commit_digest: String,
    timestamp_version: Option<u64>,
    lease_id: String,
    objects: Vec<TransactionObject>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransactionObject {
    name: String,
    blob: String,
    sha256: String,
    length: u64,
}

impl Read for SealedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.length.saturating_sub(self.offset);
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        if requested == 0 {
            return Ok(0);
        }
        let read = self.file.read_at(&mut buffer[..requested], self.offset)?;
        self.offset += read as u64;
        Ok(read)
    }
}

impl PublicationObject {
    /// Hashes and validates one regular non-symlinked source file.
    pub fn from_file(name: &str, source: &Path) -> Result<Self, PublicationError> {
        Self::seal(name, source, None)
    }

    fn from_expected(
        name: &str,
        source: &Path,
        sha256: &str,
        length: u64,
    ) -> Result<Self, PublicationError> {
        if !valid_digest(sha256) {
            return Err(PublicationError::InvalidObject);
        }
        Self::seal(name, source, Some((sha256, length)))
    }

    fn seal(
        name: &str,
        source: &Path,
        expected: Option<(&str, u64)>,
    ) -> Result<Self, PublicationError> {
        if !safe_name(name) {
            return Err(PublicationError::InvalidObject);
        }
        let metadata = fs::symlink_metadata(source).map_err(|_| PublicationError::InvalidObject)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(PublicationError::InvalidObject);
        }
        let mut input = fs::File::open(source).map_err(|_| PublicationError::InvalidObject)?;
        let mut sealed = tempfile::tempfile().map_err(|_| PublicationError::InvalidObject)?;
        let mut hasher = Sha256::new();
        let mut length = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|_| PublicationError::InvalidObject)?;
            if read == 0 {
                break;
            }
            sealed
                .write_all(&buffer[..read])
                .map_err(|_| PublicationError::InvalidObject)?;
            hasher.update(&buffer[..read]);
            length = length
                .checked_add(read as u64)
                .ok_or(PublicationError::InvalidObject)?;
        }
        sealed
            .sync_all()
            .map_err(|_| PublicationError::InvalidObject)?;
        let digest = hex::encode(hasher.finalize());
        if expected.is_some_and(|(expected_digest, expected_length)| {
            digest != expected_digest || length != expected_length
        }) {
            return Err(PublicationError::InvalidObject);
        }
        Ok(Self {
            name: name.to_owned(),
            file: Arc::new(sealed),
            sha256: digest,
            length,
        })
    }

    /// Returns the destination-relative object name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns a new read-only cursor over the exact sealed bytes.
    #[must_use]
    pub fn reader(&self) -> SealedReader {
        SealedReader {
            file: Arc::clone(&self.file),
            offset: 0,
            length: self.length,
        }
    }

    /// Returns the lowercase SHA-256 digest.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the exact byte length.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// Minimal capability required from GitHub Releases or a CDN adapter.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Stable public destination identity used to prevent aliasing both roles.
    fn destination_id(&self) -> &str;

    /// Checks authentication, immutability support, and rejects conflicting objects.
    /// An existing object with the exact expected digest and length is allowed.
    async fn preflight(
        &self,
        release_id: &str,
        objects: &[PublicationObject],
    ) -> Result<(), PublicationError>;

    /// Ensures one immutable object exists with exact bytes.
    ///
    /// This operation must be idempotent: an existing exact object succeeds,
    /// while an existing object with different bytes fails closed.
    async fn ensure_object(
        &self,
        release_id: &str,
        object: &PublicationObject,
    ) -> Result<(), PublicationError>;

    /// Fetches destination metadata and confirms exact digest and length.
    async fn verify_object(
        &self,
        release_id: &str,
        object: &PublicationObject,
    ) -> Result<(), PublicationError>;

    /// Makes the fully verified release discoverable, idempotently for this digest.
    async fn commit_release(
        &self,
        release_id: &str,
        manifest_digest: &str,
    ) -> Result<(), PublicationError>;

    /// Queries the stable release route without mutating it.
    async fn release_status(
        &self,
        release_id: &str,
        manifest_digest: &str,
    ) -> Result<ActivationStatus, PublicationError>;

    /// Atomically routes the stable `timestamp.json` name to a sealed version.
    ///
    /// The update must reject rollback, succeed idempotently for an already
    /// committed identical version/digest, and reject a conflicting digest.
    async fn commit_timestamp(
        &self,
        release_id: &str,
        timestamp_version: u64,
        timestamp_digest: &str,
    ) -> Result<(), PublicationError>;

    /// Queries the stable timestamp route without mutating it.
    async fn timestamp_status(
        &self,
        release_id: &str,
        timestamp_version: u64,
        timestamp_digest: &str,
    ) -> Result<ActivationStatus, PublicationError>;
}

/// Publishes and atomically activates an independently versioned timestamp refresh.
pub async fn publish_timestamp_refresh(
    refresh: &mut DurableTimestampRefresh,
    github: &dyn Publisher,
    cdn: &dyn Publisher,
) -> Result<(), PublicationError> {
    let authoritative_active = match github
        .timestamp_status(
            &refresh.release_id,
            refresh.timestamp_version,
            &refresh.timestamp_digest,
        )
        .await?
    {
        ActivationStatus::Missing => false,
        ActivationStatus::Exact => true,
        ActivationStatus::Conflict => return Err(PublicationError::Destination),
    };
    if !authoritative_active {
        validate_timestamp_freshness(&refresh.objects)?;
    }
    publish_objects_without_commit(&refresh.release_id, &refresh.objects, github, cdn).await?;
    if !authoritative_active {
        validate_timestamp_freshness(&refresh.objects)?;
        github
            .commit_timestamp(
                &refresh.release_id,
                refresh.timestamp_version,
                &refresh.timestamp_digest,
            )
            .await?;
    }
    cdn.commit_timestamp(
        &refresh.release_id,
        refresh.timestamp_version,
        &refresh.timestamp_digest,
    )
    .await?;
    refresh
        .authorization
        .commit()
        .map_err(|_| PublicationError::Destination)?;
    mark_committed(&refresh.directory, &refresh.timestamp_digest)
}

/// Publication validation or remote refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    /// Release id, object set, name, or source is invalid.
    InvalidObject,
    /// GitHub source and CDN mirror resolve to the same destination.
    AliasedDestinations,
    /// A destination refused preflight, create, verification, or commit.
    Destination,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidObject => formatter.write_str("publication object set is invalid"),
            Self::AliasedDestinations => formatter.write_str("publication destinations alias"),
            Self::Destination => formatter.write_str("publication destination refused operation"),
        }
    }
}

impl std::error::Error for PublicationError {}

/// Publishes immutable bytes and activates GitHub source-of-truth before its mirror.
///
/// Both destinations preflight before the first write. Every created object is
/// remotely verified. Once GitHub authoritatively activates the exact digest,
/// recovery may finish the same mirror and authority lease even after expiry.
pub async fn publish_release(
    signed: &mut DurableRelease,
    github: &dyn Publisher,
    cdn: &dyn Publisher,
) -> Result<(), PublicationError> {
    let authoritative_active = match github
        .release_status(&signed.release_id, &signed.manifest_digest)
        .await?
    {
        ActivationStatus::Missing => false,
        ActivationStatus::Exact => true,
        ActivationStatus::Conflict => return Err(PublicationError::Destination),
    };
    if !authoritative_active {
        validate_release_freshness(&signed.objects)?;
    }
    publish_objects_without_commit(&signed.release_id, &signed.objects, github, cdn).await?;
    if !authoritative_active {
        validate_release_freshness(&signed.objects)?;
        github
            .commit_release(&signed.release_id, &signed.manifest_digest)
            .await?;
    }
    cdn.commit_release(&signed.release_id, &signed.manifest_digest)
        .await?;
    signed
        .authorization
        .commit()
        .map_err(|_| PublicationError::Destination)?;
    mark_committed(&signed.directory, &signed.manifest_digest)
}

fn validate_release_freshness(objects: &[PublicationObject]) -> Result<(), PublicationError> {
    let root: Signed<Root> = parse_unique_role(objects, ".root.json")?;
    let targets: Signed<Targets> = parse_unique_role(objects, ".targets.json")?;
    let snapshot: Signed<Snapshot> = parse_unique_role(objects, ".snapshot.json")?;
    let timestamp: Signed<TufTimestamp> = parse_unique_role(objects, "/timestamp.json")?;
    let minimum = Timestamp::now() + jiff::SignedDuration::from_hours(1);
    if [
        root.signed.expires,
        targets.signed.expires,
        snapshot.signed.expires,
        timestamp.signed.expires,
    ]
    .into_iter()
    .any(|expires| expires <= minimum)
    {
        return Err(PublicationError::InvalidObject);
    }
    Ok(())
}

fn validate_timestamp_freshness(objects: &[PublicationObject]) -> Result<(), PublicationError> {
    let timestamp: Signed<TufTimestamp> = parse_unique_role(objects, "/timestamp.json")?;
    if timestamp.signed.expires <= Timestamp::now() + jiff::SignedDuration::from_hours(1) {
        return Err(PublicationError::InvalidObject);
    }
    Ok(())
}

fn parse_unique_role<T: serde::de::DeserializeOwned>(
    objects: &[PublicationObject],
    suffix: &str,
) -> Result<T, PublicationError> {
    let mut matches = objects
        .iter()
        .filter(|object| object.name().ends_with(suffix));
    let object = matches.next().ok_or(PublicationError::InvalidObject)?;
    if matches.next().is_some() {
        return Err(PublicationError::InvalidObject);
    }
    let mut bytes = Vec::new();
    object
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|_| PublicationError::InvalidObject)?;
    serde_json::from_slice(&bytes).map_err(|_| PublicationError::InvalidObject)
}

impl DurableRelease {
    pub(crate) fn persist(
        signed: SignedRelease,
        directory: &Path,
    ) -> Result<Self, PublicationError> {
        let release_id = signed.release.release_id().to_owned();
        let manifest_digest = signed.release.release_digest().to_owned();
        let mut authorization = signed
            .release
            .into_authorization()
            .map_err(|_| PublicationError::InvalidObject)?;
        let record = record(
            TransactionKind::Release,
            &release_id,
            &manifest_digest,
            None,
            authorization.lease_id(),
            &signed.objects,
        )?;
        let manifest = serde_json::to_vec(&record).map_err(|_| PublicationError::InvalidObject)?;
        let transaction_digest = hex::encode(Sha256::digest(&manifest));
        authorization
            .bind_transaction(&transaction_digest)
            .map_err(|_| PublicationError::InvalidObject)?;
        let directory = persist_record(directory, &manifest, &record, &signed.objects)?;
        Ok(Self {
            release_id,
            manifest_digest,
            authorization,
            objects: signed.objects,
            directory,
        })
    }

    /// Reloads and revalidates a release transaction after a process restart.
    pub fn resume(
        directory: &Path,
        authority: &dyn ReleaseAuthority,
    ) -> Result<Self, PublicationError> {
        let (record, transaction_digest, objects, directory) =
            load_record(directory, TransactionKind::Release)?;
        if record.timestamp_version.is_some() {
            return Err(PublicationError::InvalidObject);
        }
        let authorization = authority
            .resume(&record.commit_digest, &transaction_digest, &record.lease_id)
            .map_err(|_| PublicationError::InvalidObject)?;
        Ok(Self {
            release_id: record.release_id,
            manifest_digest: record.commit_digest,
            authorization,
            objects,
            directory,
        })
    }

    /// Returns the durable transaction directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl DurableTimestampRefresh {
    pub(crate) fn persist(
        signed: SignedTimestampRefresh,
        directory: &Path,
    ) -> Result<Self, PublicationError> {
        let mut authorization = signed.authorization;
        let record = record(
            TransactionKind::Timestamp,
            &signed.release_id,
            &signed.timestamp_digest,
            Some(signed.timestamp_version),
            authorization.lease_id(),
            &signed.objects,
        )?;
        let manifest = serde_json::to_vec(&record).map_err(|_| PublicationError::InvalidObject)?;
        let transaction_digest = hex::encode(Sha256::digest(&manifest));
        authorization
            .bind_transaction(&transaction_digest)
            .map_err(|_| PublicationError::InvalidObject)?;
        let directory = persist_record(directory, &manifest, &record, &signed.objects)?;
        Ok(Self {
            release_id: signed.release_id,
            timestamp_version: signed.timestamp_version,
            timestamp_digest: signed.timestamp_digest,
            authorization,
            objects: signed.objects,
            directory,
        })
    }

    /// Reloads and revalidates a timestamp transaction after a process restart.
    pub fn resume(
        directory: &Path,
        authority: &dyn TimestampAuthority,
    ) -> Result<Self, PublicationError> {
        let (record, transaction_digest, objects, directory) =
            load_record(directory, TransactionKind::Timestamp)?;
        let timestamp_version = record
            .timestamp_version
            .filter(|version| *version > 0)
            .ok_or(PublicationError::InvalidObject)?;
        let authorization = authority
            .resume(
                &record.release_id,
                timestamp_version,
                &transaction_digest,
                &record.lease_id,
            )
            .map_err(|_| PublicationError::InvalidObject)?;
        Ok(Self {
            release_id: record.release_id,
            timestamp_version,
            timestamp_digest: record.commit_digest,
            authorization,
            objects,
            directory,
        })
    }

    /// Returns the durable transaction directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

#[cfg(test)]
async fn publish_objects(
    release_id: &str,
    manifest_digest: &str,
    objects: &[PublicationObject],
    github: &dyn Publisher,
    cdn: &dyn Publisher,
) -> Result<(), PublicationError> {
    if !valid_atom(release_id)
        || !valid_digest(manifest_digest)
        || objects.is_empty()
        || github.destination_id() == cdn.destination_id()
    {
        return Err(if github.destination_id() == cdn.destination_id() {
            PublicationError::AliasedDestinations
        } else {
            PublicationError::InvalidObject
        });
    }
    publish_objects_without_commit(release_id, objects, github, cdn).await?;
    github.commit_release(release_id, manifest_digest).await?;
    cdn.commit_release(release_id, manifest_digest).await?;
    Ok(())
}

async fn publish_objects_without_commit(
    release_id: &str,
    objects: &[PublicationObject],
    github: &dyn Publisher,
    cdn: &dyn Publisher,
) -> Result<(), PublicationError> {
    if !valid_atom(release_id)
        || objects.is_empty()
        || github.destination_id() == cdn.destination_id()
    {
        return Err(if github.destination_id() == cdn.destination_id() {
            PublicationError::AliasedDestinations
        } else {
            PublicationError::InvalidObject
        });
    }
    let mut names = BTreeSet::new();
    if objects.iter().any(|object| !names.insert(object.name())) {
        return Err(PublicationError::InvalidObject);
    }
    github.preflight(release_id, objects).await?;
    cdn.preflight(release_id, objects).await?;
    for destination in [cdn, github] {
        for object in objects {
            destination.ensure_object(release_id, object).await?;
        }
        for object in objects {
            destination.verify_object(release_id, object).await?;
        }
    }
    Ok(())
}

pub(crate) fn seal_objects(
    release: &crate::ValidatedRelease,
    output: &Path,
    root_version: u64,
) -> Result<Vec<PublicationObject>, PublicationError> {
    release
        .revalidate_all()
        .map_err(|_| PublicationError::InvalidObject)?;
    let sequence = release.channel_sequence();
    let metadata = output.join("metadata");
    let targets = output.join("targets");
    let mut metadata_names = BTreeSet::new();
    for entry in fs::read_dir(&metadata).map_err(|_| PublicationError::InvalidObject)? {
        let entry = entry.map_err(|_| PublicationError::InvalidObject)?;
        let file_type = entry
            .file_type()
            .map_err(|_| PublicationError::InvalidObject)?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(PublicationError::InvalidObject);
        }
        metadata_names.insert(entry.file_name().to_string_lossy().into_owned());
    }
    let expected_metadata = BTreeSet::from([
        format!("{root_version}.root.json"),
        format!("{sequence}.targets.json"),
        format!("{sequence}.snapshot.json"),
        "timestamp.json".to_owned(),
    ]);
    if metadata_names != expected_metadata {
        return Err(PublicationError::InvalidObject);
    }
    let mut target_files = Vec::new();
    collect_files(&targets, &targets, &mut target_files)?;
    if target_files.len() != 13 {
        return Err(PublicationError::InvalidObject);
    }
    let manifest_path = output.join("release-manifest.json");
    if fs::read(&manifest_path).map_err(|_| PublicationError::InvalidObject)?
        != release.canonical_manifest()
    {
        return Err(PublicationError::InvalidObject);
    }
    let audit_path = output.join("signing-audit.ndjson");
    let audit = fs::read(&audit_path).map_err(|_| PublicationError::InvalidObject)?;
    if !audit.ends_with(b"\n") || serde_json::from_slice::<serde_json::Value>(&audit).is_err() {
        return Err(PublicationError::InvalidObject);
    }

    let mut objects = Vec::new();
    for name in expected_metadata {
        objects.push(PublicationObject::from_file(
            &format!("channel/{}/{name}", release.release_id()),
            &metadata.join(name),
        )?);
    }
    for relative in target_files {
        objects.push(PublicationObject::from_file(
            &format!(
                "channel/{}/targets/{}",
                release.release_id(),
                relative.to_string_lossy()
            ),
            &targets.join(&relative),
        )?);
    }
    for (name, path) in [
        ("release-manifest.json", manifest_path),
        ("signing-audit.ndjson", audit_path),
    ] {
        objects.push(PublicationObject::from_file(
            &format!("channel/{}/{name}", release.release_id()),
            &path,
        )?);
    }
    for (name, path, digest, length) in release.cli_files() {
        objects.push(PublicationObject::from_expected(
            &format!("cli/{}/{name}", release.release_id()),
            &path,
            digest,
            length,
        )?);
    }
    if objects.len() != 25 {
        return Err(PublicationError::InvalidObject);
    }
    Ok(objects)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), PublicationError> {
    for entry in fs::read_dir(directory).map_err(|_| PublicationError::InvalidObject)? {
        let entry = entry.map_err(|_| PublicationError::InvalidObject)?;
        let file_type = entry
            .file_type()
            .map_err(|_| PublicationError::InvalidObject)?;
        if file_type.is_symlink() {
            return Err(PublicationError::InvalidObject);
        }
        if file_type.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| PublicationError::InvalidObject)?
                    .to_path_buf(),
            );
        } else {
            return Err(PublicationError::InvalidObject);
        }
    }
    Ok(())
}

fn record(
    kind: TransactionKind,
    release_id: &str,
    commit_digest: &str,
    timestamp_version: Option<u64>,
    lease_id: &str,
    objects: &[PublicationObject],
) -> Result<TransactionRecord, PublicationError> {
    if !valid_atom(release_id)
        || !valid_digest(commit_digest)
        || !valid_atom(lease_id)
        || !matches!(
            (kind, timestamp_version, objects.len()),
            (TransactionKind::Release, None, 25) | (TransactionKind::Timestamp, Some(1..), 2)
        )
    {
        return Err(PublicationError::InvalidObject);
    }
    let mut names = BTreeSet::new();
    let objects = objects
        .iter()
        .enumerate()
        .map(|(index, object)| {
            if !names.insert(object.name()) {
                return Err(PublicationError::InvalidObject);
            }
            Ok(TransactionObject {
                name: object.name().to_owned(),
                blob: format!("objects/{index:04}"),
                sha256: object.sha256().to_owned(),
                length: object.length(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TransactionRecord {
        schema_version: 1,
        kind,
        release_id: release_id.to_owned(),
        commit_digest: commit_digest.to_owned(),
        timestamp_version,
        lease_id: lease_id.to_owned(),
        objects,
    })
}

fn persist_record(
    destination: &Path,
    manifest: &[u8],
    record: &TransactionRecord,
    objects: &[PublicationObject],
) -> Result<PathBuf, PublicationError> {
    if !destination.is_absolute() || destination.exists() {
        return Err(PublicationError::InvalidObject);
    }
    let parent = destination
        .parent()
        .ok_or(PublicationError::InvalidObject)?;
    let parent_metadata =
        fs::symlink_metadata(parent).map_err(|_| PublicationError::InvalidObject)?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(PublicationError::InvalidObject);
    }
    let staging = tempfile::Builder::new()
        .prefix(".pkg-release-transaction-")
        .tempdir_in(parent)
        .map_err(|_| PublicationError::InvalidObject)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
        .map_err(|_| PublicationError::InvalidObject)?;
    let objects_dir = staging.path().join("objects");
    fs::create_dir(&objects_dir).map_err(|_| PublicationError::InvalidObject)?;
    fs::set_permissions(&objects_dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| PublicationError::InvalidObject)?;
    for (entry, object) in record.objects.iter().zip(objects) {
        let path = staging.path().join(&entry.blob);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| PublicationError::InvalidObject)?;
        let copied = std::io::copy(&mut object.reader(), &mut file)
            .map_err(|_| PublicationError::InvalidObject)?;
        if copied != object.length() {
            return Err(PublicationError::InvalidObject);
        }
        file.sync_all()
            .map_err(|_| PublicationError::InvalidObject)?;
    }
    sync_directory(&objects_dir)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging.path().join("transaction.json"))
        .map_err(|_| PublicationError::InvalidObject)?;
    file.write_all(manifest)
        .map_err(|_| PublicationError::InvalidObject)?;
    file.sync_all()
        .map_err(|_| PublicationError::InvalidObject)?;
    sync_directory(staging.path())?;
    let staging_path = staging.keep();
    fs::rename(&staging_path, destination).map_err(|_| PublicationError::InvalidObject)?;
    sync_directory(parent)?;
    Ok(destination.to_path_buf())
}

fn load_record(
    directory: &Path,
    expected_kind: TransactionKind,
) -> Result<(TransactionRecord, String, Vec<PublicationObject>, PathBuf), PublicationError> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| PublicationError::InvalidObject)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(PublicationError::InvalidObject);
    }
    let directory = fs::canonicalize(directory).map_err(|_| PublicationError::InvalidObject)?;
    let manifest_path = directory.join("transaction.json");
    let manifest_metadata =
        fs::symlink_metadata(&manifest_path).map_err(|_| PublicationError::InvalidObject)?;
    if !manifest_metadata.is_file()
        || manifest_metadata.file_type().is_symlink()
        || manifest_metadata.permissions().mode() & 0o077 != 0
        || manifest_metadata.len() > 256 * 1024
    {
        return Err(PublicationError::InvalidObject);
    }
    let manifest = fs::read(&manifest_path).map_err(|_| PublicationError::InvalidObject)?;
    let transaction_digest = hex::encode(Sha256::digest(&manifest));
    let record: TransactionRecord =
        serde_json::from_slice(&manifest).map_err(|_| PublicationError::InvalidObject)?;
    if record.schema_version != 1
        || record.kind != expected_kind
        || !valid_atom(&record.release_id)
        || !valid_digest(&record.commit_digest)
        || !valid_atom(&record.lease_id)
        || record.objects.is_empty()
        || record.objects.len()
            != match expected_kind {
                TransactionKind::Release => 25,
                TransactionKind::Timestamp => 2,
            }
    {
        return Err(PublicationError::InvalidObject);
    }
    validate_transaction_inventory(&directory, record.objects.len())?;
    let mut names = BTreeSet::new();
    let mut objects = Vec::with_capacity(record.objects.len());
    for (index, entry) in record.objects.iter().enumerate() {
        if entry.blob != format!("objects/{index:04}") || !names.insert(&entry.name) {
            return Err(PublicationError::InvalidObject);
        }
        objects.push(PublicationObject::from_expected(
            &entry.name,
            &directory.join(&entry.blob),
            &entry.sha256,
            entry.length,
        )?);
    }
    Ok((record, transaction_digest, objects, directory))
}

fn validate_transaction_inventory(
    directory: &Path,
    object_count: usize,
) -> Result<(), PublicationError> {
    let objects_metadata = fs::symlink_metadata(directory.join("objects"))
        .map_err(|_| PublicationError::InvalidObject)?;
    if !objects_metadata.is_dir()
        || objects_metadata.file_type().is_symlink()
        || objects_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(PublicationError::InvalidObject);
    }
    let top: BTreeSet<_> = fs::read_dir(directory)
        .map_err(|_| PublicationError::InvalidObject)?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|_| PublicationError::InvalidObject)
        })
        .collect::<Result<_, _>>()?;
    let allowed = BTreeSet::from([
        "transaction.json".to_owned(),
        "objects".to_owned(),
        "COMMITTED".to_owned(),
    ]);
    if !top.is_subset(&allowed) || !top.contains("transaction.json") || !top.contains("objects") {
        return Err(PublicationError::InvalidObject);
    }
    if top.contains("COMMITTED") {
        let committed = fs::symlink_metadata(directory.join("COMMITTED"))
            .map_err(|_| PublicationError::InvalidObject)?;
        if !committed.is_file()
            || committed.file_type().is_symlink()
            || committed.permissions().mode() & 0o077 != 0
        {
            return Err(PublicationError::InvalidObject);
        }
    }
    let object_names: BTreeSet<_> = fs::read_dir(directory.join("objects"))
        .map_err(|_| PublicationError::InvalidObject)?
        .map(|entry| {
            let entry = entry.map_err(|_| PublicationError::InvalidObject)?;
            let file_type = entry
                .file_type()
                .map_err(|_| PublicationError::InvalidObject)?;
            let metadata = entry
                .metadata()
                .map_err(|_| PublicationError::InvalidObject)?;
            if !file_type.is_file()
                || file_type.is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(PublicationError::InvalidObject);
            }
            Ok(entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Result<_, _>>()?;
    let expected: BTreeSet<_> = (0..object_count)
        .map(|index| format!("{index:04}"))
        .collect();
    if object_names != expected {
        return Err(PublicationError::InvalidObject);
    }
    Ok(())
}

fn mark_committed(directory: &Path, digest: &str) -> Result<(), PublicationError> {
    let path = directory.join("COMMITTED");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(digest.as_bytes())
                .map_err(|_| PublicationError::InvalidObject)?;
            file.sync_all()
                .map_err(|_| PublicationError::InvalidObject)?;
            sync_directory(directory)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read_to_string(path).map_err(|_| PublicationError::InvalidObject)?;
            if existing == digest {
                Ok(())
            } else {
                Err(PublicationError::InvalidObject)
            }
        }
        Err(_) => Err(PublicationError::InvalidObject),
    }
}

fn sync_directory(path: &Path) -> Result<(), PublicationError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| PublicationError::InvalidObject)
}

fn safe_name(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && name.len() <= 512
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_atom(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    use super::{
        ActivationStatus, PublicationError, PublicationObject, Publisher, publish_objects,
        publish_timestamp_refresh,
    };
    use crate::{
        DurableTimestampRefresh, SignedTimestampRefresh, TimestampAuthority,
        TimestampAuthorization, ValidationError,
    };

    #[derive(Debug)]
    struct FakePublisher {
        id: &'static str,
        transcript: Mutex<Vec<String>>,
        stored: Mutex<BTreeMap<String, (String, u64)>>,
        release_commits: Mutex<BTreeMap<String, String>>,
        timestamp_commits: Mutex<BTreeMap<String, (u64, String)>>,
        fail_after_store_once: Mutex<bool>,
        fail_verify: bool,
    }

    #[async_trait::async_trait]
    impl Publisher for FakePublisher {
        fn destination_id(&self) -> &str {
            self.id
        }

        async fn preflight(
            &self,
            _: &str,
            objects: &[PublicationObject],
        ) -> Result<(), PublicationError> {
            self.transcript
                .lock()
                .unwrap()
                .push(format!("{}:preflight", self.id));
            let stored = self.stored.lock().unwrap();
            if objects.iter().any(|object| {
                stored.get(object.name()).is_some_and(|(digest, length)| {
                    digest != object.sha256() || *length != object.length()
                })
            }) {
                return Err(PublicationError::Destination);
            }
            Ok(())
        }

        async fn ensure_object(
            &self,
            _: &str,
            object: &PublicationObject,
        ) -> Result<(), PublicationError> {
            self.transcript
                .lock()
                .unwrap()
                .push(format!("{}:create:{}", self.id, object.name()));
            let mut stored = self.stored.lock().unwrap();
            match stored.get(object.name()) {
                Some((digest, length))
                    if digest == object.sha256() && *length == object.length() => {}
                Some(_) => return Err(PublicationError::Destination),
                None => {
                    stored.insert(
                        object.name().to_owned(),
                        (object.sha256().to_owned(), object.length()),
                    );
                    if std::mem::take(&mut *self.fail_after_store_once.lock().unwrap()) {
                        return Err(PublicationError::Destination);
                    }
                }
            }
            Ok(())
        }

        async fn verify_object(
            &self,
            _: &str,
            object: &PublicationObject,
        ) -> Result<(), PublicationError> {
            self.transcript
                .lock()
                .unwrap()
                .push(format!("{}:verify:{}", self.id, object.name()));
            if self.fail_verify {
                Err(PublicationError::Destination)
            } else {
                Ok(())
            }
        }

        async fn commit_release(
            &self,
            release_id: &str,
            digest: &str,
        ) -> Result<(), PublicationError> {
            self.transcript
                .lock()
                .unwrap()
                .push(format!("{}:commit", self.id));
            let mut commits = self.release_commits.lock().unwrap();
            match commits.get(release_id) {
                Some(existing) if existing == digest => Ok(()),
                Some(_) => Err(PublicationError::Destination),
                None => {
                    commits.insert(release_id.to_owned(), digest.to_owned());
                    Ok(())
                }
            }
        }

        async fn release_status(
            &self,
            release_id: &str,
            digest: &str,
        ) -> Result<ActivationStatus, PublicationError> {
            Ok(match self.release_commits.lock().unwrap().get(release_id) {
                None => ActivationStatus::Missing,
                Some(existing) if existing == digest => ActivationStatus::Exact,
                Some(_) => ActivationStatus::Conflict,
            })
        }

        async fn commit_timestamp(
            &self,
            release_id: &str,
            version: u64,
            digest: &str,
        ) -> Result<(), PublicationError> {
            self.transcript
                .lock()
                .unwrap()
                .push(format!("{}:timestamp-commit", self.id));
            let mut commits = self.timestamp_commits.lock().unwrap();
            match commits.get(release_id) {
                Some((existing_version, existing_digest))
                    if *existing_version == version && existing_digest == digest =>
                {
                    Ok(())
                }
                Some(_) => Err(PublicationError::Destination),
                None => {
                    commits.insert(release_id.to_owned(), (version, digest.to_owned()));
                    Ok(())
                }
            }
        }

        async fn timestamp_status(
            &self,
            release_id: &str,
            version: u64,
            digest: &str,
        ) -> Result<ActivationStatus, PublicationError> {
            Ok(
                match self.timestamp_commits.lock().unwrap().get(release_id) {
                    None => ActivationStatus::Missing,
                    Some((existing_version, existing_digest))
                        if *existing_version == version && existing_digest == digest =>
                    {
                        ActivationStatus::Exact
                    }
                    Some(_) => ActivationStatus::Conflict,
                },
            )
        }
    }

    fn publisher(id: &'static str, fail_verify: bool) -> FakePublisher {
        FakePublisher {
            id,
            transcript: Mutex::new(Vec::new()),
            stored: Mutex::new(BTreeMap::new()),
            release_commits: Mutex::new(BTreeMap::new()),
            timestamp_commits: Mutex::new(BTreeMap::new()),
            fail_after_store_once: Mutex::new(false),
            fail_verify,
        }
    }

    #[tokio::test]
    async fn preflights_both_then_verifies_mirror_and_source_before_commits() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("object");
        std::fs::write(&source, b"release bytes").unwrap();
        let object = PublicationObject::from_file("channel/object", &source).unwrap();
        let github = publisher("github", false);
        let cdn = publisher("cdn", false);
        publish_objects("v1.0.0", &"a".repeat(64), &[object], &github, &cdn)
            .await
            .unwrap();
        assert_eq!(cdn.transcript.lock().unwrap().last().unwrap(), "cdn:commit");
        assert!(
            github
                .transcript
                .lock()
                .unwrap()
                .contains(&"github:commit".to_owned())
        );
    }

    #[tokio::test]
    async fn verification_failure_prevents_both_discovery_commits() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("object");
        std::fs::write(&source, b"release bytes").unwrap();
        let object = PublicationObject::from_file("channel/object", &source).unwrap();
        let github = publisher("github", false);
        let cdn = publisher("cdn", true);
        assert_eq!(
            publish_objects("v1.0.0", &"a".repeat(64), &[object], &github, &cdn).await,
            Err(PublicationError::Destination)
        );
        assert!(
            !cdn.transcript
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.ends_with(":commit"))
        );
        assert!(
            github
                .transcript
                .lock()
                .unwrap()
                .iter()
                .all(|line| !line.contains(":create:"))
        );
    }

    #[tokio::test]
    async fn partial_immutable_upload_resumes_by_accepting_exact_objects() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("object");
        std::fs::write(&source, b"release bytes").unwrap();
        let object = PublicationObject::from_file("channel/object", &source).unwrap();
        let github = publisher("github", false);
        let cdn = publisher("cdn", false);
        *cdn.fail_after_store_once.lock().unwrap() = true;
        assert_eq!(
            publish_objects(
                "v1.0.0",
                &"a".repeat(64),
                std::slice::from_ref(&object),
                &github,
                &cdn,
            )
            .await,
            Err(PublicationError::Destination)
        );
        publish_objects("v1.0.0", &"a".repeat(64), &[object], &github, &cdn)
            .await
            .expect("exact partial object resumes");
    }

    #[test]
    fn publication_object_is_independent_of_its_source_path() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("object");
        std::fs::write(&source, b"approved bytes").unwrap();
        let object = PublicationObject::from_file("channel/object", &source).unwrap();
        std::fs::write(&source, b"mutated bytes").unwrap();
        let mut bytes = Vec::new();
        object.reader().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"approved bytes");
    }

    struct RetryAuthorization {
        attempts: Arc<AtomicUsize>,
        bound: Arc<Mutex<Option<String>>>,
    }

    struct RetryAuthority {
        attempts: Arc<AtomicUsize>,
        bound: Arc<Mutex<Option<String>>>,
    }

    impl TimestampAuthorization for RetryAuthorization {
        fn lease_id(&self) -> &str {
            "retry-lease"
        }

        fn signing_actor(&self) -> &str {
            "timestamp-service"
        }

        fn bind_transaction(&mut self, transaction_digest: &str) -> Result<(), ValidationError> {
            if transaction_digest.len() != 64 {
                return Err(ValidationError::InvalidPolicy);
            }
            let mut bound = self.bound.lock().unwrap();
            match bound.as_deref() {
                Some(existing) if existing != transaction_digest => {
                    return Err(ValidationError::InvalidPolicy);
                }
                Some(_) => {}
                None => *bound = Some(transaction_digest.to_owned()),
            }
            Ok(())
        }

        fn commit(&mut self) -> Result<(), ValidationError> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(ValidationError::InvalidPolicy)
            } else {
                Ok(())
            }
        }
    }

    impl TimestampAuthority for RetryAuthority {
        fn authorize(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u64,
            _: u64,
        ) -> Result<Box<dyn TimestampAuthorization>, ValidationError> {
            Ok(Box::new(RetryAuthorization {
                attempts: Arc::clone(&self.attempts),
                bound: Arc::clone(&self.bound),
            }))
        }

        fn resume(
            &self,
            release_id: &str,
            timestamp_version: u64,
            transaction_digest: &str,
            lease_id: &str,
        ) -> Result<Box<dyn TimestampAuthorization>, ValidationError> {
            if release_id != "v1"
                || timestamp_version != 2
                || transaction_digest.len() != 64
                || lease_id != "retry-lease"
                || self.bound.lock().unwrap().as_deref() != Some(transaction_digest)
            {
                return Err(ValidationError::InvalidPolicy);
            }
            Ok(Box::new(RetryAuthorization {
                attempts: Arc::clone(&self.attempts),
                bound: Arc::clone(&self.bound),
            }))
        }
    }

    #[tokio::test]
    async fn authorization_commit_failure_retains_a_retryable_refresh() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("timestamp");
        let timestamp = tough::schema::Signed {
            signed: tough::schema::Timestamp::new(
                "1.0.0".to_owned(),
                std::num::NonZeroU64::new(2).unwrap(),
                jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24),
            ),
            signatures: Vec::new(),
        };
        std::fs::write(&source, serde_json::to_vec(&timestamp).unwrap()).unwrap();
        let object =
            PublicationObject::from_file("channel/v1/timestamp/2/timestamp.json", &source).unwrap();
        let audit_source = temporary.path().join("audit");
        std::fs::write(&audit_source, b"audit bytes\n").unwrap();
        let audit =
            PublicationObject::from_file("channel/v1/timestamp/2/audit.ndjson", &audit_source)
                .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let bound = Arc::new(Mutex::new(None));
        let refresh = SignedTimestampRefresh {
            release_id: "v1".to_owned(),
            timestamp_version: 2,
            timestamp_digest: object.sha256().to_owned(),
            authorization: Box::new(RetryAuthorization {
                attempts: Arc::clone(&attempts),
                bound: Arc::clone(&bound),
            }),
            objects: vec![object, audit],
        };
        let transaction_path = temporary.path().join("durable-refresh");
        let durable = refresh
            .persist(&transaction_path)
            .expect("persist transaction");
        drop(durable);
        let authority = RetryAuthority {
            attempts: Arc::clone(&attempts),
            bound,
        };
        let manifest_path = transaction_path.join("transaction.json");
        let original_manifest = std::fs::read(&manifest_path).unwrap();
        let mut tampered: serde_json::Value = serde_json::from_slice(&original_manifest).unwrap();
        tampered["objects"][0]["name"] = serde_json::json!("channel/v1/timestamp/2/tampered.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(DurableTimestampRefresh::resume(&transaction_path, &authority).is_err());
        std::fs::write(&manifest_path, original_manifest).unwrap();
        let mut refresh = DurableTimestampRefresh::resume(&transaction_path, &authority)
            .expect("resume after process loss");
        let github = publisher("github", false);
        let cdn = publisher("cdn", false);
        assert_eq!(
            publish_timestamp_refresh(&mut refresh, &github, &cdn).await,
            Err(PublicationError::Destination)
        );
        publish_timestamp_refresh(&mut refresh, &github, &cdn)
            .await
            .expect("idempotent retry finishes authorization");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
