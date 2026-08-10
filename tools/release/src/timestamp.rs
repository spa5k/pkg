//! Independent short-lived TUF timestamp refreshes.

use std::collections::HashMap;
use std::fs;
use std::num::NonZeroU64;
use std::path::Path;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use jiff::Timestamp as WallClock;
use tough::editor::signed::SignedRole;
use tough::key_source::KeySource;
use tough::schema::decoded::{Decoded, Hex};
use tough::schema::{Hashes, KeyHolder, Metafile, Root, Signed, Snapshot, Timestamp};

use crate::{
    AuditEvent, DurableTimestampRefresh, PublicationError, PublicationObject, SignError,
    ValidationError, write_audit_log,
};

/// Durable reservation for one independently monotonic timestamp refresh.
pub trait TimestampAuthorization: Send {
    /// Returns the durable opaque lease identity used for crash recovery.
    fn lease_id(&self) -> &str;

    /// Returns the authenticated workload identity allowed to invoke the key.
    fn signing_actor(&self) -> &str;

    /// Durably binds the lease to the exact persisted transaction digest.
    fn bind_transaction(&mut self, transaction_digest: &str) -> Result<(), ValidationError>;

    /// Idempotently commits the refresh after both destinations are active.
    /// The lease remains reacquirable by id until commit or authorized cleanup.
    fn commit(&mut self) -> Result<(), ValidationError>;
}

/// External authority for timestamp rollback protection and workload identity.
pub trait TimestampAuthority: Send + Sync {
    /// Reserves the exact next timestamp version for a verified snapshot.
    fn authorize(
        &self,
        release_id: &str,
        trusted_root_sha256: &str,
        snapshot_digest: &str,
        snapshot_version: u64,
        timestamp_version: u64,
    ) -> Result<Box<dyn TimestampAuthorization>, ValidationError>;

    /// Reacquires an existing lease while resuming a persisted refresh.
    fn resume(
        &self,
        release_id: &str,
        timestamp_version: u64,
        transaction_digest: &str,
        lease_id: &str,
    ) -> Result<Box<dyn TimestampAuthorization>, ValidationError>;
}

/// Sealed output of one timestamp-only refresh.
pub struct SignedTimestampRefresh {
    pub(crate) release_id: String,
    pub(crate) timestamp_version: u64,
    pub(crate) timestamp_digest: String,
    pub(crate) authorization: Box<dyn TimestampAuthorization>,
    pub(crate) objects: Vec<PublicationObject>,
}

impl SignedTimestampRefresh {
    /// Returns the independently monotonic timestamp version.
    #[must_use]
    pub const fn timestamp_version(&self) -> u64 {
        self.timestamp_version
    }

    /// Returns the immutable timestamp and audit objects.
    #[must_use]
    pub fn objects(&self) -> &[PublicationObject] {
        &self.objects
    }

    /// Atomically persists exact bytes and lease identity before publication.
    pub fn persist(self, directory: &Path) -> Result<DurableTimestampRefresh, PublicationError> {
        DurableTimestampRefresh::persist(self, directory)
    }
}

/// Verifies the trusted root and current snapshot, then signs only `timestamp.json`.
pub async fn refresh_timestamp(
    release_id: &str,
    root_path: &Path,
    snapshot_path: &Path,
    timestamp_version: NonZeroU64,
    expires: WallClock,
    authority: &dyn TimestampAuthority,
    timestamp_keys: &[Box<dyn KeySource>],
) -> Result<SignedTimestampRefresh, SignError> {
    if !valid_atom(release_id) {
        return Err(SignError::Filesystem);
    }
    let now = WallClock::now();
    if expires <= now || expires > now + jiff::SignedDuration::from_hours(48) {
        return Err(SignError::Filesystem);
    }
    let root_bytes = safe_read(root_path)?;
    let snapshot_bytes = safe_read(snapshot_path)?;
    let root: Signed<Root> =
        serde_json::from_slice(&root_bytes).map_err(|_| SignError::Filesystem)?;
    root.signed.verify_role(&root)?;
    if root.signed.expires <= expires {
        return Err(SignError::Filesystem);
    }
    let snapshot: Signed<Snapshot> =
        serde_json::from_slice(&snapshot_bytes).map_err(|_| SignError::Filesystem)?;
    root.signed.verify_role(&snapshot)?;
    if snapshot.signed.expires <= expires {
        return Err(SignError::Filesystem);
    }
    let snapshot_digest = hex::encode(digest(&SHA256, &snapshot_bytes).as_ref());
    let trusted_root_sha256 = hex::encode(digest(&SHA256, &root_bytes).as_ref());
    let authorization = authority.authorize(
        release_id,
        &trusted_root_sha256,
        &snapshot_digest,
        snapshot.signed.version.get(),
        timestamp_version.get(),
    )?;
    let actor = authorization.signing_actor().to_owned();
    if !valid_atom(&actor) {
        return Err(SignError::Validation(ValidationError::InvalidPolicy));
    }

    let mut timestamp = Timestamp::new("1.0.0".to_owned(), timestamp_version, expires);
    timestamp.meta.insert(
        "snapshot.json".to_owned(),
        Metafile {
            length: Some(snapshot_bytes.len() as u64),
            hashes: Some(Hashes {
                sha256: Decoded::<Hex>::from(
                    hex::decode(&snapshot_digest).map_err(|_| SignError::Filesystem)?,
                ),
                _extra: HashMap::new(),
            }),
            version: snapshot.signed.version,
            _extra: HashMap::new(),
        },
    );
    let signed = SignedRole::new(
        timestamp,
        &KeyHolder::Root(root.signed),
        timestamp_keys,
        &SystemRandom::new(),
    )
    .await?;
    let temporary = tempfile::tempdir().map_err(|_| SignError::Filesystem)?;
    signed.write(temporary.path(), false).await?;
    let key_ids = public_key_ids(timestamp_keys).await?;
    let signed_at = WallClock::now().to_string();
    let audit_path = temporary.path().join("timestamp-audit.ndjson");
    write_audit_log(
        &audit_path,
        &AuditEvent {
            schema_version: 1,
            release_id,
            release_digest: &snapshot_digest,
            actor: &actor,
            key_ids: &key_ids,
            signed_at: &signed_at,
        },
    )
    .map_err(|_| SignError::Filesystem)?;
    let timestamp_path = temporary.path().join("timestamp.json");
    let timestamp_object = PublicationObject::from_file(
        &format!(
            "channel/{release_id}/timestamp/{}/timestamp.json",
            timestamp_version.get()
        ),
        &timestamp_path,
    )
    .map_err(SignError::Publication)?;
    let timestamp_digest = timestamp_object.sha256().to_owned();
    let audit_object = PublicationObject::from_file(
        &format!(
            "channel/{release_id}/timestamp/{}/audit.ndjson",
            timestamp_version.get()
        ),
        &audit_path,
    )
    .map_err(SignError::Publication)?;
    Ok(SignedTimestampRefresh {
        release_id: release_id.to_owned(),
        timestamp_version: timestamp_version.get(),
        timestamp_digest,
        authorization,
        objects: vec![timestamp_object, audit_object],
    })
}

fn safe_read(path: &Path) -> Result<Vec<u8>, SignError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SignError::Filesystem)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(SignError::Filesystem);
    }
    fs::read(fs::canonicalize(path).map_err(|_| SignError::Filesystem)?)
        .map_err(|_| SignError::Filesystem)
}

async fn public_key_ids(keys: &[Box<dyn KeySource>]) -> Result<Vec<String>, SignError> {
    let mut ids = Vec::new();
    for source in keys {
        let signer = source.as_sign().await.map_err(|_| SignError::KeySource)?;
        ids.push(hex::encode(
            signer
                .tuf_key()
                .key_id()
                .map_err(|_| SignError::KeySource)?
                .as_ref(),
        ));
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn valid_atom(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}
