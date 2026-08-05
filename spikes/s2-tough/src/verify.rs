// The pkg TUF *client* (verifier) built on `tough`.
//
// This is the S2 spike's model of how PR-11 will load a signed channel: a
// pinned trusted root, `FilesystemTransport` over a local repo, `ExpirationEnforcement::Safe`,
// the conservative `Limits`, and a *persistent* datastore. Target reads are
// FULLY CONSUMED before any bytes are handed back, so target hash validation
// always completes before the bytes are used (TRU-INV-01).

use crate::limits::CONSERVATIVE_LIMITS;
use std::path::Path;
use tough::IntoVec;
use tough::error::Error as ToughError;
use tough::{
    ExpirationEnforcement, FilesystemTransport, Limits, Repository, RepositoryLoader, TargetName,
};
use url::Url;

/// A verifier pinned to a trusted root byte blob, loading from `metadata_url`.
#[derive(Clone)]
pub struct Verifier {
    pub root_bytes: Vec<u8>,
    pub metadata_base_url: Url,
    pub targets_base_url: Url,
}

impl Verifier {
    pub fn new(root_bytes: Vec<u8>, metadata_base_url: Url, targets_base_url: Url) -> Self {
        Self {
            root_bytes,
            metadata_base_url,
            targets_base_url,
        }
    }

    /// Load the repository with the spike's conservative defaults:
    /// `FilesystemTransport`, `ExpirationEnforcement::Safe`, the conservative
    /// `Limits`, and a *persistent* datastore at `datastore` (which must exist).
    ///
    /// Passing a persistent datastore is REQUIRED for rollback protection —
    /// see `findings.md` and the `datastore` tests. `ExpirationEnforcement::Safe`
    /// is the ONLY acceptable mode for normal update/install paths; `Unsafe` is
    /// prohibited there.
    pub async fn load(&self, datastore: &Path) -> Result<Repository, ToughError> {
        self.load_with(datastore, CONSERVATIVE_LIMITS, ExpirationEnforcement::Safe)
            .await
    }

    /// Load with explicit overrides (used by the limits/expiry tests).
    pub async fn load_with(
        &self,
        datastore: &Path,
        limits: Limits,
        expiration: ExpirationEnforcement,
    ) -> Result<Repository, ToughError> {
        RepositoryLoader::new(
            &self.root_bytes,
            self.metadata_base_url.clone(),
            self.targets_base_url.clone(),
        )
        .transport(FilesystemTransport)
        .limits(limits)
        .expiration_enforcement(expiration)
        .datastore(datastore)
        .load()
        .await
    }

    /// Load WITHOUT a persistent datastore (a fresh ephemeral TempDir is used
    /// internally). Provided to demonstrate that this LOSES rollback protection
    /// across loads — see the `datastore` tests.
    pub async fn load_no_datastore(&self) -> Result<Repository, ToughError> {
        RepositoryLoader::new(
            &self.root_bytes,
            self.metadata_base_url.clone(),
            self.targets_base_url.clone(),
        )
        .transport(FilesystemTransport)
        .limits(CONSERVATIVE_LIMITS)
        .expiration_enforcement(ExpirationEnforcement::Safe)
        .load()
        .await
    }
}

/// Read a target and FULLY CONSUME its stream before returning the bytes.
///
/// `tough::Repository::read_target` returns a stream whose SHA-256 is validated
/// incrementally by a `DigestAdapter`; the hash check is only complete once the
/// stream has been read to exhaustion. This helper drains the entire stream into
/// a `Vec<u8>` (via `IntoVec`) and returns `Err` if any chunk fails — so callers
/// never receive partially-verified or tampered bytes. This is the contract
/// PR-11 must uphold: never use target bytes from a stream that errored.
pub async fn read_target_fully(
    repo: &Repository,
    name: &TargetName,
) -> Result<Option<Vec<u8>>, ToughError> {
    let Some(stream) = repo.read_target(name).await? else {
        return Ok(None);
    };
    // Drain to completion; hash mismatch / max-size surface as an `Err` here.
    let bytes = IntoVec::<ToughError>::into_vec(stream).await?;
    Ok(Some(bytes))
}
