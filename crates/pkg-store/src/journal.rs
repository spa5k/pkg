use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use pkg_core::state::{JournalPayload, JournalRow, PreviousRowHash, recover_journal};
use serde_json::{Value, json};

use crate::{LeaseMode, StateLayout, StateLease};

const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

/// Validated durable access to the per-user hash-chained operation journal.
#[derive(Debug, Clone)]
pub struct StateJournal {
    directory: PathBuf,
    state_root: PathBuf,
    owner_uid: u32,
}

impl StateJournal {
    /// Opens the journal only beneath a validated state layout.
    pub fn open(layout: &StateLayout) -> Result<Self, StateJournalError> {
        layout
            .validate()
            .map_err(|_| StateJournalError::UnsafeState)?;
        let directory = layout.state_root().join("journal");
        validate_directory(&directory, layout.owner_uid())?;
        Ok(Self {
            directory,
            state_root: layout.state_root().to_path_buf(),
            owner_uid: layout.owner_uid(),
        })
    }

    /// Reads the complete accepted chain, refusing any corrupt/torn suffix.
    pub fn rows(&self, lease: &StateLease) -> Result<Vec<JournalRow>, StateJournalError> {
        if !lease.authorizes_state(&self.state_root, self.owner_uid, LeaseMode::Shared) {
            return Err(StateJournalError::LeaseRequired);
        }
        let path = self.directory.join("journal.ndjson");
        let bytes = match read_regular(&path, self.owner_uid) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(StateJournalError::UnsafeState),
        };
        let recovered = recover_journal(&bytes).map_err(|_| StateJournalError::InvalidChain)?;
        if !recovered.quarantined_suffix().is_empty() {
            return Err(StateJournalError::InvalidChain);
        }
        Ok(recovered.accepted().to_vec())
    }

    /// Appends and fsyncs one validated row after the existing chain.
    pub fn append(
        &self,
        lease: &StateLease,
        operation_id: &str,
        phase: &str,
        status: &str,
        extra: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), StateJournalError> {
        if !lease.authorizes_state(&self.state_root, self.owner_uid, LeaseMode::Exclusive) {
            return Err(StateJournalError::LeaseRequired);
        }
        let rows = self.rows(lease)?;
        let (seq, previous) = rows
            .last()
            .map_or(Ok((1, PreviousRowHash::Genesis)), |row| {
                row.seq()
                    .checked_add(1)
                    .map(|seq| (seq, PreviousRowHash::Row(row.row_hash())))
                    .ok_or(StateJournalError::InvalidChain)
            })?;
        let mut fields = BTreeMap::from([
            ("opId".to_owned(), json!(operation_id)),
            ("phase".to_owned(), json!(phase)),
            ("status".to_owned(), json!(status)),
        ]);
        for (key, value) in extra {
            if fields.insert(key, value).is_some() {
                return Err(StateJournalError::InvalidRow);
            }
        }
        let payload = JournalPayload::new(fields).map_err(|_| StateJournalError::InvalidRow)?;
        let row =
            JournalRow::new(seq, previous, payload).map_err(|_| StateJournalError::InvalidRow)?;
        let line = row
            .to_ndjson_line()
            .map_err(|_| StateJournalError::InvalidRow)?;
        let path = self.directory.join("journal.ndjson");
        let mut options = OpenOptions::new();
        options
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options.open(path).map_err(|_| StateJournalError::Io)?;
        validate_file(&file, self.owner_uid)?;
        file.write_all(&line).map_err(|_| StateJournalError::Io)?;
        file.sync_all().map_err(|_| StateJournalError::Io)?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| StateJournalError::Io)
    }

    /// Returns whether an exact operation phase/status is already durable.
    pub fn has_status(
        &self,
        lease: &StateLease,
        operation_id: &str,
        phase: &str,
        status: &str,
    ) -> Result<bool, StateJournalError> {
        Ok(self.rows(lease)?.iter().any(|row| {
            let fields = row.payload().fields();
            fields.get("opId").and_then(Value::as_str) == Some(operation_id)
                && fields.get("phase").and_then(Value::as_str) == Some(phase)
                && fields.get("status").and_then(Value::as_str) == Some(status)
        }))
    }
}

/// Stable journal storage failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateJournalError {
    /// Appending requires the caller's live exclusive state lease.
    LeaseRequired,
    /// A directory, file type, owner, mode, or symlink check failed.
    UnsafeState,
    /// The existing chain was corrupt or ended in an unquarantined torn row.
    InvalidChain,
    /// The new payload violated the closed journal schema.
    InvalidRow,
    /// A validated filesystem operation failed.
    Io,
}

impl fmt::Display for StateJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "state journal refused: {self:?}")
    }
}
impl std::error::Error for StateJournalError {}

fn read_regular(path: &std::path::Path, owner_uid: u32) -> std::io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o600 != 0o600
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.len() > MAX_JOURNAL_BYTES
    {
        return Err(std::io::Error::other("unsafe journal file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn validate_directory(path: &std::path::Path, owner_uid: u32) -> Result<(), StateJournalError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| StateJournalError::UnsafeState)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(StateJournalError::UnsafeState);
    }
    Ok(())
}

fn validate_file(file: &File, owner_uid: u32) -> Result<(), StateJournalError> {
    let metadata = file
        .metadata()
        .map_err(|_| StateJournalError::UnsafeState)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o600 != 0o600
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(StateJournalError::UnsafeState);
    }
    Ok(())
}
