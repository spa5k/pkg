use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::StateLayout;

/// Whether a state lease permits reads only or owns mutation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseMode {
    /// Consistent read of the mutable manifest/lock views.
    Shared,
    /// One state mutation or generation-pruning transaction.
    Exclusive,
}

/// Sanitized identity persisted by an exclusive lease holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseIdentity {
    operation_id: String,
    nonce: String,
    started_at: String,
}

impl LeaseIdentity {
    /// Validates bounded product-owned identifiers for the lease record.
    pub fn new(
        operation_id: impl Into<String>,
        nonce: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Result<Self, LeaseError> {
        let operation_id = operation_id.into();
        let nonce = nonce.into();
        let started_at = started_at.into();
        if !valid_token(&operation_id)
            || !valid_token(&nonce)
            || started_at.is_empty()
            || started_at.len() > 64
            || !started_at.bytes().all(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'T' | b'Z' | b'.' | b'+')
            })
        {
            return Err(LeaseError::InvalidIdentity);
        }
        Ok(Self {
            operation_id,
            nonce,
            started_at,
        })
    }
}

/// A kernel-held lease released automatically when this file handle is dropped.
pub struct StateLease {
    _file: File,
    mode: LeaseMode,
    state_root: PathBuf,
    owner_uid: u32,
}

impl fmt::Debug for StateLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateLease")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl StateLease {
    /// Attempts a non-blocking shared lease for a consistent mutable-view read.
    pub fn try_shared(layout: &StateLayout) -> Result<Self, LeaseError> {
        acquire(layout, LeaseMode::Shared, None)
    }

    /// Attempts a non-blocking exclusive lease and records its holder identity.
    pub fn try_exclusive(
        layout: &StateLayout,
        identity: &LeaseIdentity,
    ) -> Result<Self, LeaseError> {
        acquire(layout, LeaseMode::Exclusive, Some(identity))
    }

    /// Returns the authority held by this lease token.
    #[must_use]
    pub const fn mode(&self) -> LeaseMode {
        self.mode
    }

    /// Returns whether this token grants `mode` for this exact validated layout.
    #[must_use]
    pub fn authorizes(&self, layout: &StateLayout, mode: LeaseMode) -> bool {
        self.authorizes_state(layout.state_root(), layout.owner_uid(), mode)
    }

    pub(crate) fn authorizes_state(&self, root: &Path, owner_uid: u32, mode: LeaseMode) -> bool {
        self.state_root == root
            && self.owner_uid == owner_uid
            && match mode {
                LeaseMode::Shared => matches!(self.mode, LeaseMode::Shared | LeaseMode::Exclusive),
                LeaseMode::Exclusive => self.mode == LeaseMode::Exclusive,
            }
    }
}

/// Stable lease acquisition failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseError {
    /// Another live process or handle holds a conflicting lease.
    Locked,
    /// The lease directory/file failed ownership, type, mode, or symlink checks.
    UnsafeState,
    /// A persisted lease identity violated the closed bounded grammar.
    InvalidIdentity,
    /// A validated filesystem operation failed.
    Io,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "state lease refused: {self:?}")
    }
}
impl std::error::Error for LeaseError {}

fn acquire(
    layout: &StateLayout,
    mode: LeaseMode,
    identity: Option<&LeaseIdentity>,
) -> Result<StateLease, LeaseError> {
    layout.validate().map_err(|_| LeaseError::UnsafeState)?;
    let run = layout.state_root().join("run");
    validate_directory(&run, layout.owner_uid())?;
    let path = run.join("lease");
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&path).map_err(|_| LeaseError::UnsafeState)?;
    validate_file(&file, layout.owner_uid())?;
    let result = match mode {
        LeaseMode::Shared => file.try_lock_shared(),
        LeaseMode::Exclusive => file.try_lock(),
    };
    match result {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(LeaseError::Locked),
        Err(TryLockError::Error(_)) => return Err(LeaseError::Io),
    }
    if let Some(identity) = identity {
        let bytes = serde_json::to_vec(&json!({
            "opId": identity.operation_id,
            "pid": std::process::id(),
            "nonce": identity.nonce,
            "started": identity.started_at,
        }))
        .map_err(|_| LeaseError::Io)?;
        file.set_len(0).map_err(|_| LeaseError::Io)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| LeaseError::Io)?;
        file.write_all(&bytes).map_err(|_| LeaseError::Io)?;
        file.write_all(b"\n").map_err(|_| LeaseError::Io)?;
        file.sync_all().map_err(|_| LeaseError::Io)?;
        File::open(&run)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| LeaseError::Io)?;
    }
    Ok(StateLease {
        _file: file,
        mode,
        state_root: layout.state_root().to_path_buf(),
        owner_uid: layout.owner_uid(),
    })
}

fn validate_directory(path: &std::path::Path, owner_uid: u32) -> Result<(), LeaseError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LeaseError::UnsafeState)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(LeaseError::UnsafeState);
    }
    Ok(())
}

fn validate_file(file: &File, owner_uid: u32) -> Result<(), LeaseError> {
    let metadata = file.metadata().map_err(|_| LeaseError::UnsafeState)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o600 != 0o600
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(LeaseError::UnsafeState);
    }
    Ok(())
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::{Builder, TempDir};

    fn fixture() -> (TempDir, StateLayout) {
        let temp = Builder::new().prefix("pkg-lease-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        let run = state.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        (temp, layout)
    }

    fn identity() -> LeaseIdentity {
        LeaseIdentity::new("op_gc", "nonce1", "2026-08-09T00:00:00Z").unwrap()
    }

    #[test]
    fn exclusive_conflicts_and_drop_releases_without_stale_lock_stealing() {
        let (_temp, layout) = fixture();
        let first = StateLease::try_exclusive(&layout, &identity()).unwrap();
        assert_eq!(first.mode(), LeaseMode::Exclusive);
        assert_eq!(
            StateLease::try_exclusive(&layout, &identity()).unwrap_err(),
            LeaseError::Locked
        );
        assert_eq!(
            StateLease::try_shared(&layout).unwrap_err(),
            LeaseError::Locked
        );
        drop(first);
        StateLease::try_exclusive(&layout, &identity()).unwrap();
    }

    #[test]
    fn shared_readers_coexist_and_block_a_writer() {
        let (_temp, layout) = fixture();
        let first = StateLease::try_shared(&layout).unwrap();
        let second = StateLease::try_shared(&layout).unwrap();
        assert_eq!(first.mode(), LeaseMode::Shared);
        assert_eq!(second.mode(), LeaseMode::Shared);
        let metadata = fs::symlink_metadata(layout.state_root().join("run/lease")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            StateLease::try_exclusive(&layout, &identity()).unwrap_err(),
            LeaseError::Locked
        );
    }

    #[test]
    fn symlink_and_permissive_state_are_refused() {
        let (temp, layout) = fixture();
        let run = layout.state_root().join("run");
        let target = temp.path().join("target");
        fs::write(&target, b"").unwrap();
        symlink(&target, run.join("lease")).unwrap();
        assert_eq!(
            StateLease::try_shared(&layout).unwrap_err(),
            LeaseError::UnsafeState
        );
        fs::remove_file(run.join("lease")).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            StateLease::try_shared(&layout).unwrap_err(),
            LeaseError::UnsafeState
        );
        assert!(fs::read_link(target).is_err());
    }

    #[test]
    fn lease_authority_is_bound_to_one_exact_state_layout() {
        let (_first_temp, first) = fixture();
        let (_second_temp, second) = fixture();
        let lease = StateLease::try_exclusive(&first, &identity()).unwrap();
        assert!(lease.authorizes(&first, LeaseMode::Exclusive));
        assert!(!lease.authorizes(&second, LeaseMode::Shared));
        assert!(!lease.authorizes(&second, LeaseMode::Exclusive));
    }
}
