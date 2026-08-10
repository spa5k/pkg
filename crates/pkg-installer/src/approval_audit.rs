//! Durable broker-private audit for single-operation build approvals.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pkg_core::state::{JournalPayload, JournalRow, PreviousRowHash, recover_journal};
use pkg_nix::{ApprovalJournal, ApprovalJournalError, ApprovalJournalRecord};
use serde_json::{Value, json};

const AUDIT_FILE: &str = "approvals.ndjson";
const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
struct AuditState {
    directory: PathBuf,
    owner_uid: u32,
    append: Mutex<()>,
}

/// Validated service-private approval audit shared by broker connections.
#[derive(Debug, Clone)]
pub struct BrokerApprovalAudit {
    state: Arc<AuditState>,
}

impl BrokerApprovalAudit {
    /// Opens an existing broker-owned `0700` audit directory.
    ///
    /// # Errors
    /// Returns a redacted failure for a wrong owner, mode, type, or symlink.
    pub fn open(directory: &Path, owner_uid: u32) -> Result<Self, ApprovalJournalError> {
        validate_directory(directory, owner_uid)?;
        Ok(Self {
            state: Arc::new(AuditState {
                directory: directory.to_path_buf(),
                owner_uid,
                append: Mutex::new(()),
            }),
        })
    }

    /// Binds future rows to the kernel-authenticated non-root caller uid.
    ///
    /// # Errors
    /// UID zero is never a normal package caller.
    pub fn for_caller(
        &self,
        caller_uid: u32,
    ) -> Result<BrokerCallerApprovalJournal, ApprovalJournalError> {
        if caller_uid == 0 {
            return Err(ApprovalJournalError::new());
        }
        Ok(BrokerCallerApprovalJournal {
            state: Arc::clone(&self.state),
            caller_uid,
        })
    }
}

/// Caller-bound journal passed only inside the authenticated broker dispatcher.
#[derive(Debug, Clone)]
pub struct BrokerCallerApprovalJournal {
    state: Arc<AuditState>,
    caller_uid: u32,
}

impl ApprovalJournal for BrokerCallerApprovalJournal {
    fn record(&self, record: &ApprovalJournalRecord) -> Result<(), ApprovalJournalError> {
        let build_plan_digest = record.build_plan_digest().to_string();
        append_event(
            &self.state,
            self.caller_uid,
            record.operation_id().as_str(),
            &build_plan_digest,
            record.policy_version().get().get(),
            record.source().as_str(),
            record.timestamp(),
        )
    }
}

fn append_event(
    state: &AuditState,
    caller_uid: u32,
    operation_id: &str,
    build_plan_digest: &str,
    policy_version: u64,
    source: &str,
    timestamp: &str,
) -> Result<(), ApprovalJournalError> {
    let _guard = state
        .append
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    validate_directory(&state.directory, state.owner_uid)?;
    let path = state.directory.join(AUDIT_FILE);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    let mut file = options
        .open(&path)
        .map_err(|_| ApprovalJournalError::new())?;
    validate_file(&file, state.owner_uid)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| ApprovalJournalError::new())?;
    let recovery = recover_journal(&bytes).map_err(|_| ApprovalJournalError::new())?;
    if !recovery.quarantined_suffix().is_empty()
        || recovery.accepted().iter().any(|row| {
            row.payload().fields().get("opId").and_then(Value::as_str) == Some(operation_id)
        })
    {
        return Err(ApprovalJournalError::new());
    }
    let (sequence, previous) =
        recovery
            .accepted()
            .last()
            .map_or(Ok((1, PreviousRowHash::Genesis)), |row| {
                row.seq()
                    .checked_add(1)
                    .map(|sequence| (sequence, PreviousRowHash::Row(row.row_hash())))
                    .ok_or_else(ApprovalJournalError::new)
            })?;
    let payload = JournalPayload::new(BTreeMap::from([
        ("authenticatedUid".to_owned(), json!(caller_uid)),
        ("buildPlanDigest".to_owned(), json!(build_plan_digest)),
        ("opId".to_owned(), json!(operation_id)),
        ("phase".to_owned(), json!("approval")),
        ("policyVersion".to_owned(), json!(policy_version)),
        ("source".to_owned(), json!(source)),
        ("status".to_owned(), json!("granted")),
        ("ts".to_owned(), json!(timestamp)),
    ]))
    .map_err(|_| ApprovalJournalError::new())?;
    let row =
        JournalRow::new(sequence, previous, payload).map_err(|_| ApprovalJournalError::new())?;
    let line = row
        .to_ndjson_line()
        .map_err(|_| ApprovalJournalError::new())?;
    let _next_length = u64::try_from(bytes.len())
        .ok()
        .and_then(|length| length.checked_add(u64::try_from(line.len()).ok()?))
        .filter(|length| *length <= MAX_AUDIT_BYTES)
        .ok_or_else(ApprovalJournalError::new)?;
    file.write_all(&line)
        .and_then(|()| file.sync_all())
        .map_err(|_| ApprovalJournalError::new())?;
    File::open(&state.directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ApprovalJournalError::new())
}

fn validate_directory(path: &Path, owner_uid: u32) -> Result<(), ApprovalJournalError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ApprovalJournalError::new())?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(ApprovalJournalError::new());
    }
    Ok(())
}

fn validate_file(file: &File, owner_uid: u32) -> Result<(), ApprovalJournalError> {
    let metadata = file.metadata().map_err(|_| ApprovalJournalError::new())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > MAX_AUDIT_BYTES
    {
        return Err(ApprovalJournalError::new());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    fn audit_directory() -> (TempDir, PathBuf, u32) {
        let temporary = TempDir::new().unwrap();
        let directory = temporary.path().join("broker");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner_uid = nix::unistd::Uid::effective().as_raw();
        (temporary, directory, owner_uid)
    }

    fn append(state: &AuditState, operation_id: &str) -> Result<(), ApprovalJournalError> {
        append_event(
            state,
            1001,
            operation_id,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            7,
            "interactive",
            "2026-08-11T00:00:00Z",
        )
    }

    #[test]
    fn audit_is_hash_chained_private_durable_and_replay_refusing() {
        let (_temporary, directory, owner_uid) = audit_directory();
        let audit = BrokerApprovalAudit::open(&directory, owner_uid).unwrap();
        assert!(audit.for_caller(0).is_err());
        assert_eq!(audit.for_caller(1001).unwrap().caller_uid, 1001);
        append(&audit.state, "op_one").unwrap();
        append(&audit.state, "op_two").unwrap();
        assert!(append(&audit.state, "op_one").is_err());

        let path = directory.join(AUDIT_FILE);
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let recovery = recover_journal(&bytes).unwrap();
        assert!(recovery.quarantined_suffix().is_empty());
        assert_eq!(recovery.accepted().len(), 2);
        let fields = recovery.accepted()[0].payload().fields();
        assert_eq!(fields.get("authenticatedUid"), Some(&json!(1001)));
        assert_eq!(fields.get("phase"), Some(&json!("approval")));
        assert_eq!(fields.get("status"), Some(&json!("granted")));
        assert_eq!(fields.get("source"), Some(&json!("interactive")));
        assert!(!String::from_utf8_lossy(&bytes).contains("/nix/store"));
    }

    #[test]
    fn unsafe_directory_file_and_torn_tail_fail_closed() {
        let (_temporary, directory, owner_uid) = audit_directory();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(BrokerApprovalAudit::open(&directory, owner_uid).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();

        let audit = BrokerApprovalAudit::open(&directory, owner_uid).unwrap();
        append(&audit.state, "op_one").unwrap();
        OpenOptions::new()
            .append(true)
            .open(directory.join(AUDIT_FILE))
            .unwrap()
            .write_all(b"{torn")
            .unwrap();
        assert!(append(&audit.state, "op_two").is_err());

        let (_temporary, directory, owner_uid) = audit_directory();
        let target = directory.join("target");
        std::fs::write(&target, b"").unwrap();
        symlink(&target, directory.join(AUDIT_FILE)).unwrap();
        let audit = BrokerApprovalAudit::open(&directory, owner_uid).unwrap();
        assert!(append(&audit.state, "op_symlink").is_err());
        assert!(std::fs::read(&target).unwrap().is_empty());
    }
}
