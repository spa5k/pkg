//! Durable, broker-private repair journal storage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pkg_core::state::{JournalPayload, JournalRow, PreviousRowHash, recover_journal};
use pkg_nix::{GenerationId, OperationId, RepairMode, StorePath};
use serde_json::{Value, json};

use crate::repair::{
    RepairCoordinatorError, RepairJournal, RepairJournalEntry, RepairJournalStatus,
    RepairRecoveryAction, recover_repair, validate_journal,
};

const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const COMPACT_AT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
struct JournalState {
    directory: PathBuf,
    owner_uid: u32,
    append: Mutex<()>,
}

/// Shared service-private repair journal authority.
#[derive(Debug, Clone)]
pub struct BrokerRepairJournals {
    state: Arc<JournalState>,
}

impl BrokerRepairJournals {
    /// Opens an existing broker-owned `0700` directory.
    ///
    /// # Errors
    ///
    /// Refuses an unsafe, missing, or incorrectly owned directory.
    pub fn open(directory: &Path, owner_uid: u32) -> Result<Self, RepairCoordinatorError> {
        validate_directory(directory, owner_uid)?;
        Ok(Self {
            state: Arc::new(JournalState {
                directory: directory.to_path_buf(),
                owner_uid,
                append: Mutex::new(()),
            }),
        })
    }

    /// Loads the complete accepted journal prefix for one authenticated generation.
    ///
    /// # Errors
    ///
    /// Refuses invalid identity, unsafe storage, a broken hash chain, or invalid state.
    pub fn for_generation(
        &self,
        caller_uid: u32,
        generation: GenerationId,
    ) -> Result<DurableRepairJournal, RepairCoordinatorError> {
        if caller_uid == 0 {
            return Err(RepairCoordinatorError::journal_failure());
        }
        let _guard = self
            .state
            .append
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_directory(&self.state.directory, self.state.owner_uid)?;
        let file_name = journal_file_name(caller_uid, &generation);
        let rows = read_rows(&self.state, &file_name)?;
        let entries = decode_entries(&rows, caller_uid, &generation)?;
        validate_journal(&entries)?;
        Ok(DurableRepairJournal {
            state: Arc::clone(&self.state),
            caller_uid,
            generation,
            file_name,
            entries,
        })
    }
}

/// One caller/generation view over its isolated durable journal.
#[derive(Debug)]
pub struct DurableRepairJournal {
    state: Arc<JournalState>,
    caller_uid: u32,
    generation: GenerationId,
    file_name: String,
    entries: Vec<RepairJournalEntry>,
}

impl RepairJournal for DurableRepairJournal {
    fn entries(&self) -> &[RepairJournalEntry] {
        &self.entries
    }

    fn append(
        &mut self,
        path: StorePath,
        mode: Option<RepairMode>,
        status: RepairJournalStatus,
        approval_operation: Option<OperationId>,
    ) -> Result<(), RepairCoordinatorError> {
        let _guard = self
            .state
            .append
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_directory(&self.state.directory, self.state.owner_uid)?;
        let mut file = open_append_file(&self.state, &self.file_name)?;
        let mut rows = read_rows_from(&mut file, self.state.owner_uid)?;
        let current = decode_entries(&rows, self.caller_uid, &self.generation)?;
        if current != self.entries {
            return Err(RepairCoordinatorError::journal_failure());
        }
        if file
            .metadata()
            .map_err(|_| RepairCoordinatorError::journal_failure())?
            .len()
            >= COMPACT_AT_BYTES
        {
            drop(file);
            self.entries = compact_entries(&self.entries)?;
            rewrite_entries(
                &self.state,
                &self.file_name,
                self.caller_uid,
                &self.generation,
                &self.entries,
            )?;
            file = open_append_file(&self.state, &self.file_name)?;
            rows = read_rows_from(&mut file, self.state.owner_uid)?;
        }

        let sequence = u64::try_from(self.entries.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        let entry =
            RepairJournalEntry::from_parts(sequence, path, mode, status, approval_operation)?;
        let mut candidate = self.entries.clone();
        candidate.push(entry.clone());
        validate_journal(&candidate)?;

        let line = encode_entry_line(rows.last(), self.caller_uid, &self.generation, &entry)?;
        let current_len = file
            .metadata()
            .map_err(|_| RepairCoordinatorError::journal_failure())?
            .len();
        let projected = current_len
            .checked_add(
                u64::try_from(line.len()).map_err(|_| RepairCoordinatorError::journal_failure())?,
            )
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        if projected > MAX_JOURNAL_BYTES {
            return Err(RepairCoordinatorError::journal_failure());
        }
        file.write_all(&line)
            .and_then(|()| file.sync_all())
            .map_err(|_| RepairCoordinatorError::journal_failure())?;
        File::open(&self.state.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepairCoordinatorError::journal_failure())?;
        self.entries = candidate;
        Ok(())
    }
}

fn read_rows(
    state: &JournalState,
    file_name: &str,
) -> Result<Vec<JournalRow>, RepairCoordinatorError> {
    let path = state.directory.join(file_name);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(RepairCoordinatorError::journal_failure()),
    };
    validate_file(&file, state.owner_uid)?;
    read_rows_from(&mut file, state.owner_uid)
}

fn read_rows_from(
    file: &mut File,
    expected_owner_uid: u32,
) -> Result<Vec<JournalRow>, RepairCoordinatorError> {
    validate_file(file, expected_owner_uid)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    let recovery =
        recover_journal(&bytes).map_err(|_| RepairCoordinatorError::journal_failure())?;
    if !recovery.quarantined_suffix().is_empty() {
        return Err(RepairCoordinatorError::journal_failure());
    }
    Ok(recovery.accepted().to_vec())
}

fn open_append_file(state: &JournalState, file_name: &str) -> Result<File, RepairCoordinatorError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let file = options
        .open(state.directory.join(file_name))
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    validate_file(&file, state.owner_uid)?;
    Ok(file)
}

fn journal_file_name(caller_uid: u32, generation: &GenerationId) -> String {
    format!("repair-{caller_uid}-{}.ndjson", generation.as_str())
}

fn encode_entry_line(
    previous_row: Option<&JournalRow>,
    caller_uid: u32,
    generation: &GenerationId,
    entry: &RepairJournalEntry,
) -> Result<Vec<u8>, RepairCoordinatorError> {
    entry_row(previous_row, caller_uid, generation, entry)?
        .to_ndjson_line()
        .map_err(|_| RepairCoordinatorError::journal_failure())
}

fn entry_row(
    previous_row: Option<&JournalRow>,
    caller_uid: u32,
    generation: &GenerationId,
    entry: &RepairJournalEntry,
) -> Result<JournalRow, RepairCoordinatorError> {
    let (global_sequence, previous) =
        previous_row.map_or(Ok((1, PreviousRowHash::Genesis)), |row| {
            row.seq()
                .checked_add(1)
                .map(|next| (next, PreviousRowHash::Row(row.row_hash())))
                .ok_or_else(RepairCoordinatorError::journal_failure)
        })?;
    let payload = JournalPayload::new(BTreeMap::from([
        (
            "approvalOperation".to_owned(),
            json!(entry.approval_operation().map(OperationId::as_str)),
        ),
        ("authenticatedUid".to_owned(), json!(caller_uid)),
        ("generation".to_owned(), json!(generation.as_str())),
        ("journalSchemaVersion".to_owned(), json!(1)),
        ("mode".to_owned(), json!(mode_name(entry.mode()))),
        (
            "opId".to_owned(),
            json!(format!("repair:{caller_uid}:{}", generation.as_str())),
        ),
        ("path".to_owned(), json!(entry.path().as_str())),
        ("phase".to_owned(), json!("repair")),
        ("repairSequence".to_owned(), json!(entry.sequence())),
        ("status".to_owned(), json!(status_name(entry.status()))),
    ]))
    .map_err(|_| RepairCoordinatorError::journal_failure())?;
    JournalRow::new(global_sequence, previous, payload)
        .map_err(|_| RepairCoordinatorError::journal_failure())
}

fn compact_entries(
    entries: &[RepairJournalEntry],
) -> Result<Vec<RepairJournalEntry>, RepairCoordinatorError> {
    let actions = recover_repair(entries)?;
    let mut compacted = Vec::new();
    for action in actions {
        let (path, needs_approval) = match action {
            RepairRecoveryAction::RetryCacheOnly(path) => (path, false),
            RepairRecoveryAction::NeedsFreshApproval(path) => (path, true),
        };
        push_compacted(
            &mut compacted,
            path.clone(),
            None,
            RepairJournalStatus::Detected,
        )?;
        if needs_approval {
            push_compacted(
                &mut compacted,
                path.clone(),
                Some(RepairMode::CacheOnly),
                RepairJournalStatus::Intended,
            )?;
            push_compacted(
                &mut compacted,
                path.clone(),
                Some(RepairMode::CacheOnly),
                RepairJournalStatus::InProgress,
            )?;
            push_compacted(
                &mut compacted,
                path.clone(),
                Some(RepairMode::CacheOnly),
                RepairJournalStatus::PostVerify,
            )?;
            push_compacted(
                &mut compacted,
                path,
                None,
                RepairJournalStatus::NeedsApproval,
            )?;
        }
    }
    validate_journal(&compacted)?;
    Ok(compacted)
}

fn push_compacted(
    entries: &mut Vec<RepairJournalEntry>,
    path: StorePath,
    mode: Option<RepairMode>,
    status: RepairJournalStatus,
) -> Result<(), RepairCoordinatorError> {
    let sequence = u64::try_from(entries.len())
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(RepairCoordinatorError::journal_failure)?;
    entries.push(RepairJournalEntry::from_parts(
        sequence, path, mode, status, None,
    )?);
    Ok(())
}

fn rewrite_entries(
    state: &JournalState,
    file_name: &str,
    caller_uid: u32,
    generation: &GenerationId,
    entries: &[RepairJournalEntry],
) -> Result<(), RepairCoordinatorError> {
    let temporary_name = format!(".{file_name}.compact");
    let temporary_path = state.directory.join(&temporary_name);
    remove_stale_compaction(&temporary_path, state.owner_uid)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let mut file = options
        .open(&temporary_path)
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    let mut previous = None;
    for entry in entries {
        let row = entry_row(previous.as_ref(), caller_uid, generation, entry)?;
        let line = row
            .to_ndjson_line()
            .map_err(|_| RepairCoordinatorError::journal_failure())?;
        file.write_all(&line)
            .map_err(|_| RepairCoordinatorError::journal_failure())?;
        previous = Some(row);
    }
    file.sync_all()
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    std::fs::rename(&temporary_path, state.directory.join(file_name))
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    File::open(&state.directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepairCoordinatorError::journal_failure())
}

fn remove_stale_compaction(path: &Path, owner_uid: u32) -> Result<(), RepairCoordinatorError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(RepairCoordinatorError::journal_failure()),
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(RepairCoordinatorError::journal_failure());
    }
    std::fs::remove_file(path).map_err(|_| RepairCoordinatorError::journal_failure())
}

fn decode_entries(
    rows: &[JournalRow],
    caller_uid: u32,
    generation: &GenerationId,
) -> Result<Vec<RepairJournalEntry>, RepairCoordinatorError> {
    let expected_keys = BTreeSet::from([
        "approvalOperation",
        "authenticatedUid",
        "generation",
        "journalSchemaVersion",
        "mode",
        "opId",
        "path",
        "phase",
        "repairSequence",
        "status",
    ]);
    let mut entries = Vec::new();
    for row in rows {
        let fields = row.payload().fields();
        if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_keys
            || fields.get("phase").and_then(Value::as_str) != Some("repair")
            || fields.get("journalSchemaVersion").and_then(Value::as_u64) != Some(1)
        {
            return Err(RepairCoordinatorError::journal_failure());
        }
        let row_uid = fields
            .get("authenticatedUid")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        let row_generation = fields
            .get("generation")
            .and_then(Value::as_str)
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        if row_uid != caller_uid || row_generation != generation.as_str() {
            return Err(RepairCoordinatorError::journal_failure());
        }
        let sequence = fields
            .get("repairSequence")
            .and_then(Value::as_u64)
            .ok_or_else(RepairCoordinatorError::journal_failure)?;
        let path = StorePath::new(
            fields
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(RepairCoordinatorError::journal_failure)?,
        )
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
        let mode = parse_mode(
            fields
                .get("mode")
                .ok_or_else(RepairCoordinatorError::journal_failure)?,
        )?;
        let status = parse_status(
            fields
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(RepairCoordinatorError::journal_failure)?,
        )?;
        let approval_operation = match fields
            .get("approvalOperation")
            .ok_or_else(RepairCoordinatorError::journal_failure)?
        {
            Value::Null => None,
            Value::String(value) => Some(
                OperationId::new(value).map_err(|_| RepairCoordinatorError::journal_failure())?,
            ),
            _ => return Err(RepairCoordinatorError::journal_failure()),
        };
        entries.push(RepairJournalEntry::from_parts(
            sequence,
            path,
            mode,
            status,
            approval_operation,
        )?);
    }
    Ok(entries)
}

const fn mode_name(mode: Option<RepairMode>) -> Option<&'static str> {
    match mode {
        None => None,
        Some(RepairMode::CacheOnly) => Some("cache-only"),
        Some(RepairMode::Build) => Some("build"),
    }
}

fn parse_mode(value: &Value) -> Result<Option<RepairMode>, RepairCoordinatorError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) if value == "cache-only" => Ok(Some(RepairMode::CacheOnly)),
        Value::String(value) if value == "build" => Ok(Some(RepairMode::Build)),
        _ => Err(RepairCoordinatorError::journal_failure()),
    }
}

const fn status_name(status: RepairJournalStatus) -> &'static str {
    match status {
        RepairJournalStatus::Detected => "detected",
        RepairJournalStatus::Intended => "intended",
        RepairJournalStatus::InProgress => "in-progress",
        RepairJournalStatus::PostVerify => "post-verify",
        RepairJournalStatus::NeedsApproval => "needs-approval",
        RepairJournalStatus::Repaired => "repaired",
    }
}

fn parse_status(value: &str) -> Result<RepairJournalStatus, RepairCoordinatorError> {
    match value {
        "detected" => Ok(RepairJournalStatus::Detected),
        "intended" => Ok(RepairJournalStatus::Intended),
        "in-progress" => Ok(RepairJournalStatus::InProgress),
        "post-verify" => Ok(RepairJournalStatus::PostVerify),
        "needs-approval" => Ok(RepairJournalStatus::NeedsApproval),
        "repaired" => Ok(RepairJournalStatus::Repaired),
        _ => Err(RepairCoordinatorError::journal_failure()),
    }
}

fn validate_directory(path: &Path, owner_uid: u32) -> Result<(), RepairCoordinatorError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| RepairCoordinatorError::journal_failure())?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(RepairCoordinatorError::journal_failure());
    }
    Ok(())
}

fn validate_file(file: &File, owner_uid: u32) -> Result<(), RepairCoordinatorError> {
    let metadata = file
        .metadata()
        .map_err(|_| RepairCoordinatorError::journal_failure())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > MAX_JOURNAL_BYTES
    {
        return Err(RepairCoordinatorError::journal_failure());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Uid;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn setup() -> Result<(tempfile::TempDir, BrokerRepairJournals), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let journals = BrokerRepairJournals::open(directory.path(), Uid::effective().as_raw())?;
        Ok((directory, journals))
    }

    fn generation() -> Result<GenerationId, pkg_nix::MaintenanceError> {
        GenerationId::new("gen-00000001")
    }

    fn path(name: &str) -> Result<StorePath, pkg_core::IdentityError> {
        StorePath::new(&format!(
            "/nix/store/00000000000000000000000000000000-{name}"
        ))
    }

    fn journal_path(directory: &Path) -> Result<PathBuf, pkg_nix::MaintenanceError> {
        Ok(directory.join(journal_file_name(1000, &generation()?)))
    }

    #[test]
    fn append_reloads_a_private_hash_chained_journal() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, journals) = setup()?;
        let mut journal = journals.for_generation(1000, generation()?)?;
        journal.append(path("damaged")?, None, RepairJournalStatus::Detected, None)?;

        let reloaded = journals.for_generation(1000, generation()?)?;
        assert_eq!(reloaded.entries(), journal.entries());
        assert_eq!(
            fs::metadata(journal_path(directory.path())?)?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        Ok(())
    }

    #[test]
    fn stale_concurrent_view_cannot_append() -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, journals) = setup()?;
        let mut first = journals.for_generation(1000, generation()?)?;
        let mut stale = journals.for_generation(1000, generation()?)?;
        first.append(path("first")?, None, RepairJournalStatus::Detected, None)?;
        assert!(
            stale
                .append(path("stale")?, None, RepairJournalStatus::Detected, None)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn unsafe_file_or_torn_suffix_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, journals) = setup()?;
        let mut journal = journals.for_generation(1000, generation()?)?;
        journal.append(path("damaged")?, None, RepairJournalStatus::Detected, None)?;
        let journal_path = journal_path(directory.path())?;

        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o644))?;
        assert!(journals.for_generation(1000, generation()?).is_err());
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))?;
        OpenOptions::new()
            .append(true)
            .open(&journal_path)?
            .write_all(b"{")?;
        assert!(journals.for_generation(1000, generation()?).is_err());
        Ok(())
    }

    #[test]
    fn symlink_journal_is_never_followed() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, journals) = setup()?;
        let outside = directory.path().join("outside");
        fs::write(&outside, b"outside")?;
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
        symlink(&outside, journal_path(directory.path())?)?;
        assert!(journals.for_generation(1000, generation()?).is_err());
        Ok(())
    }

    #[test]
    fn caller_generation_journals_are_failure_isolated() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, journals) = setup()?;
        let mut first = journals.for_generation(1000, generation()?)?;
        first.append(path("first")?, None, RepairJournalStatus::Detected, None)?;
        let other_generation = GenerationId::new("gen-00000002")?;
        let mut second = journals.for_generation(2000, other_generation.clone())?;
        second.append(path("second")?, None, RepairJournalStatus::Detected, None)?;

        OpenOptions::new()
            .append(true)
            .open(journal_path(directory.path())?)?
            .write_all(b"{")?;
        assert!(journals.for_generation(1000, generation()?).is_err());
        assert_eq!(
            journals
                .for_generation(2000, other_generation)?
                .entries()
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn compaction_drops_terminal_history_and_preserves_recovery_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, journals) = setup()?;
        let mut journal = journals.for_generation(1000, generation()?)?;
        let done = path("done")?;
        journal.append(done.clone(), None, RepairJournalStatus::Detected, None)?;
        journal.append(done, None, RepairJournalStatus::Repaired, None)?;
        let pending = path("pending")?;
        journal.append(pending.clone(), None, RepairJournalStatus::Detected, None)?;
        journal.append(
            pending.clone(),
            Some(RepairMode::CacheOnly),
            RepairJournalStatus::Intended,
            None,
        )?;
        journal.append(
            pending.clone(),
            Some(RepairMode::CacheOnly),
            RepairJournalStatus::InProgress,
            None,
        )?;

        let compacted = compact_entries(journal.entries())?;
        assert_eq!(
            recover_repair(&compacted)?,
            vec![RepairRecoveryAction::RetryCacheOnly(pending)]
        );
        rewrite_entries(
            &journals.state,
            &journal.file_name,
            1000,
            &generation()?,
            &compacted,
        )?;
        assert_eq!(
            journals.for_generation(1000, generation()?)?.entries(),
            compacted
        );
        assert!(fs::metadata(journal_path(directory.path())?)?.len() < COMPACT_AT_BYTES);
        Ok(())
    }
}
