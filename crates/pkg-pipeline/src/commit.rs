use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use pkg_core::state::{
    Digest, Generation, JournalPayload, JournalRow, LockedState, Manifest, PreviousRowHash,
    body_digest, canonical_digest, recover_journal,
};
use pkg_core::{GenerationSnapshot, lifecycle::LifecycleState};
use pkg_nix::{GenerationId, MaintenanceAdapter, RemoveRootSetRequest};
use pkg_store::{
    ActivationEvent, ActivationPlan, PreparedRootSet, RootCandidate, StateLayout,
    activate_generation, prepare_root_set, publish_root_set, verify_recorded_activation,
};
use serde_json::{Value, json};

/// A fully validated set of exact bytes for the next immutable generation.
#[derive(Debug, Clone)]
pub struct CandidateGeneration {
    manifest_bytes: Vec<u8>,
    lock_bytes: Vec<u8>,
    generation_bytes: Vec<u8>,
    generation: Generation,
}

impl CandidateGeneration {
    /// Validates schema, ownership, snapshot paths, body hashes, and generation hash.
    pub fn new(
        manifest_bytes: Vec<u8>,
        lock_bytes: Vec<u8>,
        generation_bytes: Vec<u8>,
    ) -> Result<Self, CommitError> {
        let manifest =
            Manifest::from_json(&manifest_bytes).map_err(|_| CommitError::InvalidCandidate)?;
        let lock =
            LockedState::from_json(&lock_bytes).map_err(|_| CommitError::InvalidCandidate)?;
        let generation =
            Generation::from_json(&generation_bytes).map_err(|_| CommitError::InvalidCandidate)?;
        let lifecycle = LifecycleState::new(manifest.clone(), lock.clone())
            .map_err(|_| CommitError::InvalidCandidate)?;
        GenerationSnapshot::new(generation.clone(), lifecycle)
            .map_err(|_| CommitError::InvalidCandidate)?;
        let id = GenerationId::new(generation.id()).map_err(|_| CommitError::InvalidCandidate)?;
        let expected_manifest_snapshot = format!("generations/{}.manifest.json", id.as_str());
        let expected_lock_snapshot = format!("generations/{}.lock.json", id.as_str());
        let expected_tree = format!("activations/{}", id.as_str());
        if manifest.uid() != lock.uid()
            || manifest.uid() != generation.uid()
            || manifest.channel_seq() != generation.channel_seq()
            || lock.channel_seq() != generation.channel_seq()
            || generation.manifest_snapshot() != expected_manifest_snapshot
            || generation.lock_snapshot() != expected_lock_snapshot
            || generation.activation().tree_path() != expected_tree
            || generation.manifest_hash() != body_digest(&manifest_bytes).to_string()
            || generation.lock_hash() != body_digest(&lock_bytes).to_string()
            || !generation_hash_matches(&generation_bytes, generation.generation_hash())?
        {
            return Err(CommitError::InvalidCandidate);
        }
        Ok(Self {
            manifest_bytes,
            lock_bytes,
            generation_bytes,
            generation,
        })
    }

    /// Returns the validated generation record.
    #[must_use]
    pub const fn generation(&self) -> &Generation {
        &self.generation
    }
}

/// A generation whose snapshots, record, and prepared journal row are durable.
#[derive(Debug)]
pub struct PreparedGeneration {
    layout: StateLayout,
    candidate: CandidateGeneration,
    plan: ActivationPlan,
    roots: Option<PreparedRootSet>,
}

/// A generation already published through the `current` linearization point.
#[derive(Debug)]
pub struct ActivatedGeneration {
    layout: StateLayout,
    candidate: CandidateGeneration,
}

/// Stable, redacted generation commit failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitError {
    /// Candidate bytes or cross-file bindings did not validate.
    InvalidCandidate,
    /// The staged forest did not match the immutable generation record.
    StageMismatch,
    /// The validated state filesystem could not durably complete an operation.
    StateIo,
    /// The hash-chained journal was corrupt or had a torn suffix not yet recovered.
    JournalInvalid,
    /// Root publication/removal or atomic activation failed.
    ActivationFailed,
    /// The atomic current switch may have completed; startup must finish forward.
    ActivatedNeedsRecovery,
}

impl fmt::Display for CommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "generation transaction refused: {self:?}")
    }
}
impl std::error::Error for CommitError {}

impl CommitError {
    /// Returns whether rollback is unsafe because `current` may already be new.
    #[must_use]
    pub const fn requires_forward_recovery(self) -> bool {
        matches!(self, Self::ActivatedNeedsRecovery)
    }
}

/// Result of idempotently reconciling one generation after startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryResult {
    /// No immutable record existed for this generation.
    NoRecord,
    /// Pre-swap state was unreachable and has been discarded.
    DiscardedUnactivated,
    /// An already-activated generation was restored and committed forward.
    FinishedActivated,
    /// The generation was already committed; views were re-restored idempotently.
    AlreadyCommitted,
}

impl PreparedGeneration {
    /// Writes candidate snapshots before the immutable record and journals `prepared`.
    pub fn prepare(
        layout: StateLayout,
        candidate: CandidateGeneration,
        plan: ActivationPlan,
    ) -> Result<Self, CommitError> {
        layout.validate().map_err(|_| CommitError::StateIo)?;
        validate_plan(&candidate, &plan)?;
        let root = layout.state_root();
        validate_directory(&root.join("generations"))?;
        validate_directory(&root.join("journal"))?;
        let generation = candidate.generation();
        let generation_id =
            GenerationId::new(generation.id()).map_err(|_| CommitError::InvalidCandidate)?;
        let roots = if generation.activation().output_roots().is_empty() {
            None
        } else {
            Some(
                prepare_root_set(
                    generation.uid(),
                    generation_id,
                    generation
                        .activation()
                        .output_roots()
                        .iter()
                        .cloned()
                        .map(RootCandidate::from_output_root),
                )
                .map_err(|_| CommitError::InvalidCandidate)?,
            )
        };

        write_with_sidecar(
            root,
            generation.manifest_snapshot(),
            &candidate.manifest_bytes,
        )?;
        write_with_sidecar(root, generation.lock_snapshot(), &candidate.lock_bytes)?;
        let record_path = format!("generations/{}.json", generation.id());
        write_with_sidecar(root, &record_path, &candidate.generation_bytes)?;
        sync_dir(&root.join("generations"))?;
        append_phase(
            root,
            generation.operation().op_id(),
            "commit",
            "prepared",
            [
                ("generationId", json!(generation.id())),
                ("manifestHash", json!(generation.manifest_hash())),
                ("lockHash", json!(generation.lock_hash())),
            ],
        )?;
        Ok(Self {
            layout,
            candidate,
            plan,
            roots,
        })
    }

    /// Publishes roots, retains the forest, and atomically switches `current`.
    pub fn activate(
        self,
        helper: &dyn MaintenanceAdapter,
        nonce: &str,
    ) -> Result<ActivatedGeneration, CommitError> {
        let root = self.layout.state_root().to_path_buf();
        let op_id = self.candidate.generation().operation().op_id().to_owned();
        activate_generation(
            &self.layout,
            &GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            &self.plan,
            self.roots.as_ref(),
            helper,
            nonce,
            |event| {
                let row = match event {
                    ActivationEvent::Rooted => Some(("commit", "rooted")),
                    ActivationEvent::ForestRetained => None,
                    ActivationEvent::Activated => Some(("activate", "activated")),
                };
                if let Some((phase, status)) = row {
                    append_phase(&root, &op_id, phase, status, []).map_err(|_| match event {
                        ActivationEvent::Activated => pkg_store::CurrentError::PostActivation,
                        _ => pkg_store::CurrentError::Filesystem(std::io::Error::other(
                            "journal append failed",
                        )),
                    })?;
                }
                Ok(())
            },
        )
        .map_err(|error| {
            if matches!(error, pkg_store::CurrentError::PostActivation) {
                CommitError::ActivatedNeedsRecovery
            } else {
                CommitError::ActivationFailed
            }
        })?;
        Ok(ActivatedGeneration {
            layout: self.layout,
            candidate: self.candidate,
        })
    }
}

impl ActivatedGeneration {
    /// Restores current views from durable snapshots and appends `committed`.
    pub fn finish(self) -> Result<(), CommitError> {
        restore_current_views(&self.layout, &self.candidate)
            .map_err(|_| CommitError::ActivatedNeedsRecovery)?;
        append_phase(
            self.layout.state_root(),
            self.candidate.generation().operation().op_id(),
            "commit",
            "committed",
            [(
                "nextStateHash",
                json!(self.candidate.generation().generation_hash()),
            )],
        )
        .map_err(|_| CommitError::ActivatedNeedsRecovery)
    }
}

/// Reconciles a prepared/rooted/activated/committed generation idempotently.
pub fn recover_generation(
    layout: &StateLayout,
    generation_id: &GenerationId,
    helper: &dyn MaintenanceAdapter,
) -> Result<RecoveryResult, CommitError> {
    layout.validate().map_err(|_| CommitError::StateIo)?;
    let root = layout.state_root();
    clean_current_temps(root)?;
    let record_relative = format!("generations/{}.json", generation_id.as_str());
    let record_path = root.join(&record_relative);
    let record = match read_regular(&record_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if layout
                .current_generation()
                .map_err(|_| CommitError::StateIo)?
                .is_some_and(|current| current == *generation_id)
            {
                return Err(CommitError::InvalidCandidate);
            }
            return if discard_unprepared(root, generation_id)? {
                Ok(RecoveryResult::DiscardedUnactivated)
            } else {
                Ok(RecoveryResult::NoRecord)
            };
        }
        Err(_) => return Err(CommitError::StateIo),
        Ok(bytes) => bytes,
    };
    let generation = Generation::from_json(&record).map_err(|_| CommitError::InvalidCandidate)?;
    if generation.id() != generation_id.as_str() {
        return Err(CommitError::InvalidCandidate);
    }
    let current_is_generation = layout
        .current_generation()
        .map_err(|_| CommitError::StateIo)?
        .is_some_and(|current| current == *generation_id);
    let record_sidecar_ok = read_regular(&root.join(format!("{record_relative}.sha256")))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|sidecar| sidecar.trim_end() == body_digest(&record).to_string());
    if !record_sidecar_ok {
        if current_is_generation {
            return Err(CommitError::InvalidCandidate);
        }
        helper
            .remove_root_set(&RemoveRootSetRequest::new(
                generation.uid(),
                generation_id.clone(),
            ))
            .map_err(|_| CommitError::ActivationFailed)?;
        discard_generation_paths(root, &generation)?;
        return Ok(RecoveryResult::DiscardedUnactivated);
    }
    let manifest = read_verified_snapshot(
        root,
        generation.manifest_snapshot(),
        generation.manifest_hash(),
    )?;
    let lock = read_verified_snapshot(root, generation.lock_snapshot(), generation.lock_hash())?;
    let candidate = CandidateGeneration::new(manifest, lock, record)?;
    let roots = if generation.activation().output_roots().is_empty() {
        None
    } else {
        Some(
            prepare_root_set(
                generation.uid(),
                generation_id.clone(),
                generation
                    .activation()
                    .output_roots()
                    .iter()
                    .cloned()
                    .map(RootCandidate::from_output_root),
            )
            .map_err(|_| CommitError::InvalidCandidate)?,
        )
    };
    if !current_is_generation {
        helper
            .remove_root_set(&RemoveRootSetRequest::new(
                generation.uid(),
                generation_id.clone(),
            ))
            .map_err(|_| CommitError::ActivationFailed)?;
        discard_candidate(root, &candidate)?;
        append_phase(
            root,
            generation.operation().op_id(),
            "commit",
            "aborted",
            [("generationId", json!(generation.id()))],
        )?;
        return Ok(RecoveryResult::DiscardedUnactivated);
    }

    if let Some(roots) = &roots {
        publish_root_set(roots, helper).map_err(|_| CommitError::ActivationFailed)?;
    }
    let digest = Digest::from_str(generation.activation().tree_digest())
        .map_err(|_| CommitError::InvalidCandidate)?;
    verify_recorded_activation(
        &root.join(generation.activation().tree_path()),
        digest,
        generation.activation().entry_count(),
        generation.activation().output_roots(),
    )
    .map_err(|_| CommitError::StageMismatch)?;
    let committed =
        journal_has_status(root, generation.operation().op_id(), "commit", "committed")?;
    restore_current_views(layout, &candidate)?;
    if committed {
        Ok(RecoveryResult::AlreadyCommitted)
    } else {
        append_phase(
            root,
            generation.operation().op_id(),
            "commit",
            "committed",
            [("nextStateHash", json!(generation.generation_hash()))],
        )?;
        Ok(RecoveryResult::FinishedActivated)
    }
}

fn validate_plan(
    candidate: &CandidateGeneration,
    plan: &ActivationPlan,
) -> Result<(), CommitError> {
    let activation = candidate.generation().activation();
    if activation.tree_digest() != plan.tree_digest().to_string()
        || usize::try_from(activation.entry_count()).ok() != Some(plan.entry_count())
        || activation.output_roots() != plan.output_roots()
    {
        return Err(CommitError::StageMismatch);
    }
    Ok(())
}

fn generation_hash_matches(bytes: &[u8], expected: &str) -> Result<bool, CommitError> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| CommitError::InvalidCandidate)?;
    let object = value.as_object_mut().ok_or(CommitError::InvalidCandidate)?;
    object
        .remove("generationHash")
        .ok_or(CommitError::InvalidCandidate)?;
    let digest = canonical_digest(&value).map_err(|_| CommitError::InvalidCandidate)?;
    Ok(digest.to_string() == expected)
}

fn write_with_sidecar(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), CommitError> {
    write_new(root, relative, bytes)?;
    write_new(
        root,
        &format!("{relative}.sha256"),
        format!("{}\n", body_digest(bytes)).as_bytes(),
    )
}

fn write_new(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), CommitError> {
    let path = joined_relative(root, relative)?;
    if fs::symlink_metadata(&path).is_ok() {
        return Err(CommitError::StateIo);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&path).map_err(|_| CommitError::StateIo)?;
    file.write_all(bytes).map_err(|_| CommitError::StateIo)?;
    file.sync_all().map_err(|_| CommitError::StateIo)?;
    Ok(())
}

fn write_atomic(root: &Path, name: &str, bytes: &[u8]) -> Result<(), CommitError> {
    if name.contains('/') || name.starts_with('.') {
        return Err(CommitError::StateIo);
    }
    let path = root.join(name);
    let temporary = root.join(format!(".{name}.tmp"));
    match fs::symlink_metadata(&temporary) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(CommitError::StateIo),
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(&temporary).map_err(|_| CommitError::StateIo)?;
        }
        Ok(_) => return Err(CommitError::StateIo),
    }
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(CommitError::StateIo),
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(CommitError::StateIo),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temporary).map_err(|_| CommitError::StateIo)?;
    file.write_all(bytes).map_err(|_| CommitError::StateIo)?;
    file.sync_all().map_err(|_| CommitError::StateIo)?;
    fs::rename(&temporary, &path).map_err(|_| CommitError::StateIo)?;
    sync_dir(root)
}

fn append_phase<const N: usize>(
    root: &Path,
    op_id: &str,
    phase: &str,
    status: &str,
    extra: [(&str, Value); N],
) -> Result<(), CommitError> {
    let journal_dir = root.join("journal");
    validate_directory(&journal_dir)?;
    let path = journal_dir.join("journal.ndjson");
    let existing = match read_regular(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err(CommitError::StateIo),
    };
    let recovered = recover_journal(&existing).map_err(|_| CommitError::JournalInvalid)?;
    if !recovered.quarantined_suffix().is_empty() {
        return Err(CommitError::JournalInvalid);
    }
    let (seq, previous) = recovered
        .accepted()
        .last()
        .map_or((1, PreviousRowHash::Genesis), |row| {
            (row.seq() + 1, PreviousRowHash::Row(row.row_hash()))
        });
    let mut fields = BTreeMap::from([
        ("opId".to_owned(), json!(op_id)),
        ("phase".to_owned(), json!(phase)),
        ("status".to_owned(), json!(status)),
    ]);
    fields.extend(
        extra
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    );
    let payload = JournalPayload::new(fields).map_err(|_| CommitError::JournalInvalid)?;
    let row = JournalRow::new(seq, previous, payload).map_err(|_| CommitError::JournalInvalid)?;
    let line = row
        .to_ndjson_line()
        .map_err(|_| CommitError::JournalInvalid)?;
    let mut options = OpenOptions::new();
    options
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).map_err(|_| CommitError::StateIo)?;
    if !file
        .metadata()
        .map_err(|_| CommitError::StateIo)?
        .file_type()
        .is_file()
    {
        return Err(CommitError::StateIo);
    }
    file.write_all(&line).map_err(|_| CommitError::StateIo)?;
    file.sync_all().map_err(|_| CommitError::StateIo)?;
    sync_dir(&journal_dir)
}

fn journal_has_status(
    root: &Path,
    op_id: &str,
    phase: &str,
    status: &str,
) -> Result<bool, CommitError> {
    let bytes = match read_regular(&root.join("journal/journal.ndjson")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(CommitError::StateIo),
    };
    let recovered = recover_journal(&bytes).map_err(|_| CommitError::JournalInvalid)?;
    if !recovered.quarantined_suffix().is_empty() {
        return Err(CommitError::JournalInvalid);
    }
    Ok(recovered.accepted().iter().any(|row| {
        let fields = row.payload().fields();
        fields.get("opId").and_then(Value::as_str) == Some(op_id)
            && fields.get("phase").and_then(Value::as_str) == Some(phase)
            && fields.get("status").and_then(Value::as_str) == Some(status)
    }))
}

fn restore_current_views(
    layout: &StateLayout,
    candidate: &CandidateGeneration,
) -> Result<(), CommitError> {
    layout.validate().map_err(|_| CommitError::StateIo)?;
    write_atomic(
        layout.state_root(),
        "manifest.json",
        &candidate.manifest_bytes,
    )?;
    write_atomic(
        layout.state_root(),
        "manifest.json.sha256",
        format!("{}\n", body_digest(&candidate.manifest_bytes)).as_bytes(),
    )?;
    write_atomic(layout.state_root(), "lock.json", &candidate.lock_bytes)?;
    write_atomic(
        layout.state_root(),
        "lock.json.sha256",
        format!("{}\n", body_digest(&candidate.lock_bytes)).as_bytes(),
    )
}

fn read_verified_snapshot(
    root: &Path,
    relative: &str,
    expected: &str,
) -> Result<Vec<u8>, CommitError> {
    let bytes =
        read_regular(&joined_relative(root, relative)?).map_err(|_| CommitError::StateIo)?;
    let sidecar = read_regular(&joined_relative(root, &format!("{relative}.sha256"))?)
        .map_err(|_| CommitError::StateIo)?;
    if std::str::from_utf8(&sidecar)
        .map_err(|_| CommitError::InvalidCandidate)?
        .trim_end()
        != expected
        || body_digest(&bytes).to_string() != expected
    {
        return Err(CommitError::InvalidCandidate);
    }
    Ok(bytes)
}

fn discard_candidate(root: &Path, candidate: &CandidateGeneration) -> Result<(), CommitError> {
    discard_generation_paths(root, candidate.generation())
}

fn discard_generation_paths(root: &Path, generation: &Generation) -> Result<(), CommitError> {
    let expected_manifest = format!("generations/{}.manifest.json", generation.id());
    let expected_lock = format!("generations/{}.lock.json", generation.id());
    let expected_tree = format!("activations/{}", generation.id());
    if generation.manifest_snapshot() != expected_manifest
        || generation.lock_snapshot() != expected_lock
        || generation.activation().tree_path() != expected_tree
    {
        return Err(CommitError::InvalidCandidate);
    }
    for relative in [
        generation.manifest_snapshot().to_owned(),
        format!("{}.sha256", generation.manifest_snapshot()),
        generation.lock_snapshot().to_owned(),
        format!("{}.sha256", generation.lock_snapshot()),
        format!("generations/{}.json", generation.id()),
        format!("generations/{}.json.sha256", generation.id()),
    ] {
        remove_file_if_present(joined_relative(root, &relative)?)?;
    }
    for path in [
        root.join(format!("activations/{}.staging", generation.id())),
        root.join(generation.activation().tree_path()),
    ] {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CommitError::StateIo),
            Ok(metadata) if metadata.file_type().is_dir() => {
                fs::remove_dir_all(path).map_err(|_| CommitError::StateIo)?;
            }
            Ok(_) => return Err(CommitError::StateIo),
        }
    }
    sync_dir(&root.join("generations"))?;
    sync_dir(&root.join("activations"))
}

fn discard_unprepared(root: &Path, generation_id: &GenerationId) -> Result<bool, CommitError> {
    let mut found = false;
    for relative in [
        format!("generations/{}.manifest.json", generation_id.as_str()),
        format!(
            "generations/{}.manifest.json.sha256",
            generation_id.as_str()
        ),
        format!("generations/{}.lock.json", generation_id.as_str()),
        format!("generations/{}.lock.json.sha256", generation_id.as_str()),
        format!("generations/{}.json.sha256", generation_id.as_str()),
    ] {
        let path = joined_relative(root, &relative)?;
        if fs::symlink_metadata(&path).is_ok() {
            found = true;
        }
        remove_file_if_present(path)?;
    }
    let staging = root.join(format!("activations/{}.staging", generation_id.as_str()));
    match fs::symlink_metadata(&staging) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(CommitError::StateIo),
        Ok(metadata) if metadata.file_type().is_dir() => {
            found = true;
            fs::remove_dir_all(staging).map_err(|_| CommitError::StateIo)?;
        }
        Ok(_) => return Err(CommitError::StateIo),
    }
    match fs::symlink_metadata(root.join(format!("activations/{}", generation_id.as_str()))) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(CommitError::StateIo),
        Ok(_) => return Err(CommitError::InvalidCandidate),
    }
    if found {
        sync_dir(&root.join("generations"))?;
        sync_dir(&root.join("activations"))?;
    }
    Ok(found)
}

fn clean_current_temps(root: &Path) -> Result<(), CommitError> {
    let mut removed = false;
    for entry in fs::read_dir(root).map_err(|_| CommitError::StateIo)? {
        let entry = entry.map_err(|_| CommitError::StateIo)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("current.tmp.") {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|_| CommitError::StateIo)?
            .is_symlink()
        {
            return Err(CommitError::StateIo);
        }
        fs::remove_file(entry.path()).map_err(|_| CommitError::StateIo)?;
        removed = true;
    }
    if removed {
        sync_dir(root)?;
    }
    Ok(())
}

fn joined_relative(root: &Path, relative: &str) -> Result<PathBuf, CommitError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(CommitError::InvalidCandidate);
    }
    Ok(root.join(path))
}

fn validate_directory(path: &Path) -> Result<(), CommitError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CommitError::StateIo)?;
    if !metadata.file_type().is_dir() {
        return Err(CommitError::StateIo);
    }
    Ok(())
}

fn remove_file_if_present(path: PathBuf) -> Result<(), CommitError> {
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CommitError::StateIo),
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(path).map_err(|_| CommitError::StateIo)
        }
        Ok(_) => Err(CommitError::StateIo),
    }
}

fn sync_dir(path: &Path) -> Result<(), CommitError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| CommitError::StateIo)
}

fn read_regular(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state path is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_nix::{InProcessHelper, InProcessPeer};
    use pkg_store::inspect_staged_activation;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use tempfile::{Builder, TempDir};

    const STORE: &str = "/nix/store/00000000000000000000000000000000-demo";
    const DRV: &str = "/nix/store/11111111111111111111111111111111-demo.drv";
    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    struct Fixture {
        _temp: TempDir,
        layout: StateLayout,
        candidate: CandidateGeneration,
        plan: ActivationPlan,
        generation_id: GenerationId,
        maintenance: pkg_nix::CallerMaintenance,
    }

    fn fixture() -> Fixture {
        fixture_with_outputs(true)
    }

    fn empty_fixture() -> Fixture {
        fixture_with_outputs(false)
    }

    fn fixture_with_outputs(has_output: bool) -> Fixture {
        let temp = Builder::new()
            .prefix("pkg-pipeline-")
            .tempdir_in(".")
            .unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        for relative in ["", "generations", "journal", "activations"] {
            let path = state.join(relative);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let staging = state.join("activations/gen-0001.staging");
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        let output_roots = if has_output {
            symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
            vec![pkg_core::StorePath::new(STORE).unwrap()]
        } else {
            Vec::new()
        };
        let plan = inspect_staged_activation(&staging, output_roots).unwrap();

        let manifest_entries = if has_output {
            vec![json!({
                "id": "sel_demo",
                "selector": "demo",
                "attribute": "demo",
                "versionPref": { "kind": "any" },
                "outputs": null,
                "sourceRev": "channel:current",
                "pinned": false,
                "pinnedTo": null,
                "addedAt": "2026-08-09T00:00:00Z",
                "origin": "user:install"
            })]
        } else {
            Vec::new()
        };
        let manifest_bytes = serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "channelSeq": 1,
            "uid": uid,
            "entries": manifest_entries,
            "pins": []
        }))
        .unwrap();
        let lock_entries = if has_output {
            json!({
                "sel_demo": {
                    "attribute": "demo",
                    "nixpkgsRev": REV,
                    "realized": {
                        "storePath": STORE,
                        "deriver": DRV,
                        "outputs": { "out": STORE },
                        "outputsToInstall": ["out"],
                        "system": "x86_64-linux",
                        "narHash": NAR,
                        "closureNarSize": 42,
                        "pname": "demo",
                        "version": "1.0"
                    },
                    "lockedAt": "2026-08-09T00:00:01Z",
                    "provenance": "cache:official",
                    "sigsObserved": ["official-1:fixture"]
                }
            })
        } else {
            json!({})
        };
        let lock_bytes = serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "channelSeq": 1,
            "system": "x86_64-linux",
            "uid": uid,
            "entries": lock_entries
        }))
        .unwrap();
        let generation_outputs = if has_output {
            vec![json!({
                "id": "sel_demo",
                "attribute": "demo",
                "nixpkgsRev": REV,
                "storePath": STORE,
                "deriver": DRV,
                "outputsToInstall": ["out"],
                "narHash": NAR,
                "closureNarSize": 42,
                "provenance": "cache:official",
                "pinned": false
            })]
        } else {
            Vec::new()
        };
        let mut generation = json!({
            "schemaVersion": 1,
            "uid": uid,
            "id": "gen-0001",
            "parent": null,
            "createdAt": "2026-08-09T00:00:00Z",
            "channelSeq": 1,
            "manifestHash": body_digest(&manifest_bytes).to_string(),
            "lockHash": body_digest(&lock_bytes).to_string(),
            "manifestSnapshot": "generations/gen-0001.manifest.json",
            "lockSnapshot": "generations/gen-0001.lock.json",
            "activation": {
                "kind": "pkg-symlink-forest",
                "treePath": "activations/gen-0001",
                "treeDigest": plan.tree_digest().to_string(),
                "entryCount": plan.entry_count(),
                "collisionPolicy": "abort",
                "outputRoots": plan.output_roots().iter().map(pkg_core::StorePath::as_str).collect::<Vec<_>>(),
                "collisionResolutions": []
            },
            "outputs": generation_outputs,
            "operation": {
                "opId": "op_fixture",
                "kind": "install",
                "approval": { "build": "not_required" }
            }
        });
        let generation_hash = canonical_digest(&generation).unwrap().to_string();
        generation
            .as_object_mut()
            .unwrap()
            .insert("generationHash".into(), json!(generation_hash));
        let generation_bytes = serde_json::to_vec(&generation).unwrap();
        let candidate =
            CandidateGeneration::new(manifest_bytes, lock_bytes, generation_bytes).unwrap();
        let generation_id = GenerationId::new("gen-0001").unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(uid);
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        Fixture {
            _temp: temp,
            layout,
            candidate,
            plan,
            generation_id,
            maintenance,
        }
    }

    #[test]
    fn prepared_fault_discards_record_snapshots_and_staging() {
        let fixture = fixture();
        PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
            .unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::DiscardedUnactivated
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("activations/gen-0001.staging")
                .exists()
        );
    }

    #[test]
    fn rooted_fault_removes_roots_and_leaves_current_unchanged() {
        let fixture = fixture();
        let prepared =
            PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
                .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        append_phase(
            fixture.layout.state_root(),
            "op_fixture",
            "commit",
            "rooted",
            [],
        )
        .unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::DiscardedUnactivated
        );
        assert_eq!(fixture.layout.current_generation().unwrap(), None);
    }

    #[test]
    fn activated_fault_restores_views_and_commits_forward() {
        let fixture = fixture();
        let prepared =
            PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
                .unwrap();
        prepared.activate(&fixture.maintenance, "n1").unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::FinishedActivated
        );
        assert!(fixture.layout.state_root().join("manifest.json").is_file());
        assert!(
            journal_has_status(
                fixture.layout.state_root(),
                "op_fixture",
                "commit",
                "committed"
            )
            .unwrap()
        );
    }

    #[test]
    fn committed_recovery_is_idempotent() {
        let fixture = fixture();
        let prepared =
            PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
                .unwrap();
        prepared
            .activate(&fixture.maintenance, "n1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::AlreadyCommitted
        );
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn empty_generation_commits_and_recovers_without_publishing_roots() {
        let fixture = empty_fixture();
        PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
            .unwrap()
            .activate(&fixture.maintenance, "empty1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::AlreadyCommitted
        );
    }

    #[test]
    fn candidate_hash_or_snapshot_binding_tamper_fails_closed() {
        let fixture = fixture();
        let mut generation = fixture.candidate.generation_bytes.clone();
        let position = generation.iter().position(|byte| *byte == b'2').unwrap();
        generation[position] = b'3';
        assert!(matches!(
            CandidateGeneration::new(
                fixture.candidate.manifest_bytes,
                fixture.candidate.lock_bytes,
                generation
            ),
            Err(CommitError::InvalidCandidate)
        ));
    }

    #[test]
    fn candidate_refuses_activation_roots_not_selected_by_lock() {
        let fixture = fixture();
        let mut generation: Value =
            serde_json::from_slice(&fixture.candidate.generation_bytes).unwrap();
        generation["activation"]["outputRoots"] = json!([]);
        generation.as_object_mut().unwrap().remove("generationHash");
        let generation_hash = canonical_digest(&generation).unwrap().to_string();
        generation
            .as_object_mut()
            .unwrap()
            .insert("generationHash".into(), json!(generation_hash));
        assert!(matches!(
            CandidateGeneration::new(
                fixture.candidate.manifest_bytes,
                fixture.candidate.lock_bytes,
                serde_json::to_vec(&generation).unwrap()
            ),
            Err(CommitError::InvalidCandidate)
        ));
    }

    #[test]
    fn candidate_refuses_lock_from_another_channel_sequence() {
        let fixture = fixture();
        let mismatched_lock = format!(
            r#"{{"schemaVersion":1,"channelSeq":2,"system":"x86_64-linux","uid":{},"entries":{{}}}}"#,
            fixture.candidate.generation().uid()
        )
        .into_bytes();
        assert!(matches!(
            CandidateGeneration::new(
                fixture.candidate.manifest_bytes,
                mismatched_lock,
                fixture.candidate.generation_bytes
            ),
            Err(CommitError::InvalidCandidate)
        ));
    }

    #[test]
    fn preprepared_orphans_and_current_temp_are_cleaned_without_publishing() {
        let fixture = fixture();
        PreparedGeneration::prepare(fixture.layout.clone(), fixture.candidate, fixture.plan)
            .unwrap();
        fs::remove_file(
            fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json"),
        )
        .unwrap();
        fs::remove_file(
            fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json.sha256"),
        )
        .unwrap();
        symlink(
            "activations/gen-0001",
            fixture.layout.state_root().join("current.tmp.crash"),
        )
        .unwrap();
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::DiscardedUnactivated
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("current.tmp.crash")
                .exists()
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("generations/gen-0001.manifest.json")
                .exists()
        );
    }

    #[test]
    fn journal_symlink_is_refused_without_touching_target() {
        let fixture = fixture();
        let outside = fixture._temp.path().join("outside");
        fs::write(&outside, b"unchanged").unwrap();
        symlink(
            &outside,
            fixture.layout.state_root().join("journal/journal.ndjson"),
        )
        .unwrap();
        assert!(
            append_phase(
                fixture.layout.state_root(),
                "op_fixture",
                "resolve",
                "started",
                []
            )
            .is_err()
        );
        assert_eq!(fs::read(outside).unwrap(), b"unchanged");
    }
}
