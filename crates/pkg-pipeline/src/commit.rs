use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use pkg_core::identity::StorePath;
use pkg_core::state::{Digest, Generation, LockedState, Manifest, body_digest, canonical_digest};
use pkg_core::{GenerationSnapshot, History, lifecycle::LifecycleState};
use pkg_nix::{
    GenerationId, MaintenanceAdapter, RemoveRootSetRequest, RootSetIntent, RootSetReport,
    RootSetTransitionIntent, RootSetTransitionReport,
};

/// Retained generation snapshots and the active generation id.
type RetainedSnapshots = (Vec<GenerationSnapshot>, Option<GenerationId>);
use pkg_store::{
    ActivationEvent, ActivationPlan, LeaseMode, PreparedRootSet, RootCandidate, StateJournal,
    StateJournalError, StateLayout, StateLease, activate_generation, activate_published_generation,
    activate_transitioned_generation, authorize_generation_root_removal, inspect_staged_activation,
    prepare_root_set, publish_root_set, verify_recorded_activation,
};
use serde_json::{Value, json};

const MAX_STATE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SIDECAR_BYTES: u64 = 256;
const MAX_GENERATION_DIRECTORY_ENTRIES: usize = 16_384;

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
    lease: StateLease,
}

/// A generation already published through the `current` linearization point.
#[derive(Debug)]
pub struct ActivatedGeneration {
    layout: StateLayout,
    candidate: CandidateGeneration,
    _lease: StateLease,
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
    /// The caller did not transfer an exclusive state-mutation lease.
    LeaseRequired,
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

/// Loads the active immutable generation under a caller-held shared or
/// exclusive state lease.
///
/// Every record, snapshot, and sidecar is opened without following the final
/// symlink, bounded before allocation, and cross-validated through the normal
/// candidate and lifecycle constructors.
pub fn load_active_snapshot(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<Option<GenerationSnapshot>, CommitError> {
    require_read_lease(layout, lease)?;
    let Some(generation) = layout
        .current_generation()
        .map_err(|_| CommitError::StateIo)?
    else {
        return Ok(None);
    };
    load_generation_snapshot(layout, &generation).map(Some)
}

/// Loads all retained immutable generations as one validated history view.
///
/// Unknown files in the protected generations directory are refused rather
/// than silently omitted from the history presented to lifecycle commands.
pub fn load_retained_history(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<History, CommitError> {
    let (snapshots, active) = load_retained_snapshots(layout, lease)?;
    History::new(snapshots, active.as_ref().map(GenerationId::as_str))
        .map_err(|_| CommitError::InvalidCandidate)
}

fn load_retained_snapshots(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<RetainedSnapshots, CommitError> {
    require_read_lease(layout, lease)?;
    let active = layout
        .current_generation()
        .map_err(|_| CommitError::StateIo)?;
    let generations = layout.state_root().join("generations");
    let mut ids = Vec::new();
    let mut companion_ids = Vec::new();
    for (index, entry) in fs::read_dir(&generations)
        .map_err(|_| CommitError::StateIo)?
        .enumerate()
    {
        if index >= MAX_GENERATION_DIRECTORY_ENTRIES {
            return Err(CommitError::StateIo);
        }
        let entry = entry.map_err(|_| CommitError::StateIo)?;
        let file_type = entry.file_type().map_err(|_| CommitError::StateIo)?;
        if !file_type.is_file() {
            return Err(CommitError::StateIo);
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| CommitError::StateIo)?;
        if let Some(id) = generation_record_id(&name) {
            ids.push(id);
        } else if let Some(id) = generation_companion_id(&name) {
            companion_ids.push(id);
        } else {
            return Err(CommitError::StateIo);
        }
    }
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    if ids
        .windows(2)
        .any(|pair| pair[0].as_str() == pair[1].as_str())
    {
        return Err(CommitError::InvalidCandidate);
    }
    if companion_ids.iter().any(|companion| {
        ids.binary_search_by(|id| id.as_str().cmp(companion.as_str()))
            .is_err()
    }) {
        return Err(CommitError::InvalidCandidate);
    }
    let snapshots = ids
        .iter()
        .map(|id| load_generation_snapshot(layout, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((snapshots, active))
}

/// Returns the sole fully prepared but uncommitted state-edit generation, if one exists.
pub fn pending_state_edit_generation(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<Option<GenerationId>, CommitError> {
    pending_generation(layout, lease, is_resumable_state_operation)
}

/// Returns the sole fully prepared but uncommitted install generation, if one exists.
pub fn pending_install_generation(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<Option<GenerationId>, CommitError> {
    pending_generation(layout, lease, is_install_operation)
}

/// Returns the sole aborted install generation whose discard is not terminal.
pub fn pending_install_discard_generation(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<Option<GenerationId>, CommitError> {
    let mut prepared = BTreeSet::new();
    let mut aborted = BTreeSet::new();
    let mut pruned = BTreeSet::new();
    for row in StateJournal::open(layout)
        .and_then(|journal| journal.rows(lease))
        .map_err(map_journal_error)?
    {
        let fields = row.payload().fields();
        let (Some(operation_id), Some(generation_id)) = (
            fields.get("opId").and_then(Value::as_str),
            fields.get("generationId").and_then(Value::as_str),
        ) else {
            continue;
        };
        let key = (operation_id.to_owned(), generation_id.to_owned());
        match (
            fields.get("phase").and_then(Value::as_str),
            fields.get("status").and_then(Value::as_str),
        ) {
            (Some("commit"), Some("prepared")) => {
                prepared.insert(key);
            }
            (Some("commit"), Some("aborted")) => {
                if fields
                    .get("operationKind")
                    .and_then(Value::as_str)
                    .is_some_and(is_install_operation)
                {
                    aborted.insert(key);
                }
            }
            (Some("prune"), Some("pruned")) => {
                pruned.insert(generation_id.to_owned());
            }
            _ => {}
        }
    }
    let pending = aborted
        .intersection(&prepared)
        .filter(|(_, generation_id)| !pruned.contains(generation_id))
        .map(|(_, generation_id)| {
            GenerationId::new(generation_id).map_err(|_| CommitError::InvalidCandidate)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if pending.len() > 1 {
        return Err(CommitError::InvalidCandidate);
    }
    Ok(pending.into_iter().next())
}

fn pending_generation(
    layout: &StateLayout,
    lease: &StateLease,
    accepts: impl Fn(&str) -> bool,
) -> Result<Option<GenerationId>, CommitError> {
    let (snapshots, _) = load_retained_snapshots(layout, lease)?;
    let mut pending = Vec::new();
    for snapshot in &snapshots {
        if accepts(snapshot.generation().operation().kind()) {
            let prepared = journal_has_status(
                layout,
                lease,
                snapshot.generation().operation().op_id(),
                "commit",
                "prepared",
            )?;
            let committed = journal_has_status(
                layout,
                lease,
                snapshot.generation().operation().op_id(),
                "commit",
                "committed",
            )?;
            let aborted = journal_has_status(
                layout,
                lease,
                snapshot.generation().operation().op_id(),
                "commit",
                "aborted",
            )?;
            if committed && !prepared {
                return Err(CommitError::InvalidCandidate);
            }
            if !prepared || committed || aborted {
                continue;
            }
            pending.push(
                GenerationId::new(snapshot.generation().id())
                    .map_err(|_| CommitError::InvalidCandidate)?,
            );
        }
    }
    if pending.len() > 1 {
        return Err(CommitError::InvalidCandidate);
    }
    Ok(pending.pop())
}

/// Selects the durable generation whose roots can derive a resumed destination.
///
/// Ordinary state edits derive from their active parent. A rollback instead
/// clones an older retained snapshot, so recovery must use a committed retained
/// generation with the exact destination root mapping—especially when the
/// active parent is empty and has no helper root set.
pub fn pending_state_transition_source(
    layout: &StateLayout,
    lease: &StateLease,
    generation_id: &GenerationId,
) -> Result<GenerationId, CommitError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
    let pending = load_generation_snapshot(layout, generation_id)?;
    let generation = pending.generation();
    if !is_resumable_state_operation(generation.operation().kind()) {
        return Err(CommitError::InvalidCandidate);
    }
    let current = layout
        .current_generation()
        .map_err(|_| CommitError::StateIo)?
        .ok_or(CommitError::InvalidCandidate)?;
    if generation.operation().kind() != "rollback"
        || generation.activation().output_roots().is_empty()
    {
        return Ok(current);
    }
    let history = load_retained_history(layout, lease)?;
    for snapshot in history.snapshots() {
        let candidate = snapshot.generation();
        if candidate.id() == generation.id()
            || candidate.activation().output_roots() != generation.activation().output_roots()
            || !journal_has_status(
                layout,
                lease,
                candidate.operation().op_id(),
                "commit",
                "committed",
            )?
        {
            continue;
        }
        return GenerationId::new(candidate.id()).map_err(|_| CommitError::InvalidCandidate);
    }
    Err(CommitError::InvalidCandidate)
}

/// Discards state-edit snapshots visible before their `prepared` journal row became durable.
pub fn discard_unprepared_state_edits(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<usize, CommitError> {
    discard_unprepared_generations(layout, lease, is_resumable_state_operation)
}

/// Discards install snapshots visible before their `prepared` journal row became durable.
pub fn discard_unprepared_installs(
    layout: &StateLayout,
    lease: &StateLease,
) -> Result<usize, CommitError> {
    discard_unprepared_generations(layout, lease, is_install_operation)
}

fn discard_unprepared_generations(
    layout: &StateLayout,
    lease: &StateLease,
    accepts: impl Fn(&str) -> bool,
) -> Result<usize, CommitError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
    let (snapshots, current) = load_retained_snapshots(layout, lease)?;
    let mut discarded = 0;
    for snapshot in &snapshots {
        let generation = snapshot.generation();
        if !accepts(generation.operation().kind()) {
            continue;
        }
        let prepared = journal_has_status(
            layout,
            lease,
            generation.operation().op_id(),
            "commit",
            "prepared",
        )?;
        let committed = journal_has_status(
            layout,
            lease,
            generation.operation().op_id(),
            "commit",
            "committed",
        )?;
        if committed && !prepared {
            return Err(CommitError::InvalidCandidate);
        }
        if prepared || committed {
            continue;
        }
        let generation_id =
            GenerationId::new(generation.id()).map_err(|_| CommitError::InvalidCandidate)?;
        if current.as_ref() == Some(&generation_id) {
            return Err(CommitError::InvalidCandidate);
        }
        let root = layout.state_root();
        let record = read_verified_body(root, &format!("generations/{}.json", generation.id()))?;
        let manifest = read_verified_snapshot(
            root,
            generation.manifest_snapshot(),
            generation.manifest_hash(),
        )?;
        let lock =
            read_verified_snapshot(root, generation.lock_snapshot(), generation.lock_hash())?;
        let candidate = CandidateGeneration::new(manifest, lock, record)?;
        discard_candidate(root, &candidate)?;
        discarded += 1;
    }
    Ok(discarded)
}

/// Reopens an exact journaled pre-swap candidate for idempotent root transition and activation.
pub fn resume_prepared_state_edit(
    layout: StateLayout,
    lease: StateLease,
    generation_id: &GenerationId,
) -> Result<PreparedGeneration, CommitError> {
    resume_prepared_generation(layout, lease, generation_id, is_resumable_state_operation)
}

/// Reopens an exact journaled install candidate for attested root recovery.
pub fn resume_prepared_install(
    layout: StateLayout,
    lease: StateLease,
    generation_id: &GenerationId,
) -> Result<PreparedGeneration, CommitError> {
    resume_prepared_generation(layout, lease, generation_id, is_install_operation)
}

fn resume_prepared_generation(
    layout: StateLayout,
    lease: StateLease,
    generation_id: &GenerationId,
    accepts: impl Fn(&str) -> bool,
) -> Result<PreparedGeneration, CommitError> {
    if !lease.authorizes(&layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
    let snapshot = load_generation_snapshot(&layout, generation_id)?;
    if !accepts(snapshot.generation().operation().kind())
        || layout
            .current_generation()
            .map_err(|_| CommitError::StateIo)?
            .as_ref()
            .map(GenerationId::as_str)
            != snapshot.generation().parent()
        || !journal_has_status(
            &layout,
            &lease,
            snapshot.generation().operation().op_id(),
            "commit",
            "prepared",
        )?
        || journal_has_status(
            &layout,
            &lease,
            snapshot.generation().operation().op_id(),
            "commit",
            "committed",
        )?
    {
        return Err(CommitError::InvalidCandidate);
    }
    let root = layout.state_root();
    let generation = snapshot.generation();
    let record = read_verified_body(root, &format!("generations/{}.json", generation.id()))?;
    let manifest = read_verified_snapshot(
        root,
        generation.manifest_snapshot(),
        generation.manifest_hash(),
    )?;
    let lock = read_verified_snapshot(root, generation.lock_snapshot(), generation.lock_hash())?;
    let candidate = CandidateGeneration::new(manifest, lock, record)?;
    let staging = root
        .join("activations")
        .join(format!("{}.staging", generation.id()));
    let plan = inspect_staged_activation(&staging, generation.activation().output_roots().to_vec())
        .map_err(|_| CommitError::StageMismatch)?;
    validate_plan(&candidate, &plan)?;
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
    Ok(PreparedGeneration {
        layout,
        candidate,
        plan,
        roots,
        lease,
    })
}

/// Finishes a locally activated state edit whose transitioned roots are already durable.
pub fn recover_transitioned_state_edit(
    layout: &StateLayout,
    lease: &StateLease,
    generation_id: &GenerationId,
) -> Result<RecoveryResult, CommitError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
    clean_current_temps(layout.state_root())?;
    let snapshot = load_generation_snapshot(layout, generation_id)?;
    let generation = snapshot.generation();
    if !is_resumable_state_operation(generation.operation().kind())
        || layout
            .current_generation()
            .map_err(|_| CommitError::StateIo)?
            .as_ref()
            != Some(generation_id)
        || !journal_has_status(
            layout,
            lease,
            generation.operation().op_id(),
            "commit",
            "prepared",
        )?
    {
        return Err(CommitError::InvalidCandidate);
    }
    let root = layout.state_root();
    let record = read_verified_body(root, &format!("generations/{}.json", generation.id()))?;
    let manifest = read_verified_snapshot(
        root,
        generation.manifest_snapshot(),
        generation.manifest_hash(),
    )?;
    let lock = read_verified_snapshot(root, generation.lock_snapshot(), generation.lock_hash())?;
    let candidate = CandidateGeneration::new(manifest, lock, record)?;
    let digest = Digest::from_str(generation.activation().tree_digest())
        .map_err(|_| CommitError::InvalidCandidate)?;
    verify_recorded_activation(
        &root.join(generation.activation().tree_path()),
        digest,
        generation.activation().entry_count(),
        generation.activation().output_roots(),
    )
    .map_err(|_| CommitError::StageMismatch)?;
    restore_current_views(layout, &candidate)?;
    if journal_has_status(
        layout,
        lease,
        generation.operation().op_id(),
        "commit",
        "committed",
    )? {
        return Ok(RecoveryResult::AlreadyCommitted);
    }
    append_phase(
        layout,
        lease,
        generation.operation().op_id(),
        "commit",
        "committed",
        [("nextStateHash", json!(generation.generation_hash()))],
    )?;
    Ok(RecoveryResult::FinishedActivated)
}

fn is_resumable_state_operation(kind: &str) -> bool {
    matches!(kind, "remove" | "pin" | "unpin" | "rollback" | "update")
}

fn is_install_operation(kind: &str) -> bool {
    matches!(kind, "install" | "upgrade")
}

fn require_read_lease(layout: &StateLayout, lease: &StateLease) -> Result<(), CommitError> {
    if !lease.authorizes(layout, LeaseMode::Shared) {
        return Err(CommitError::LeaseRequired);
    }
    layout.validate().map_err(|_| CommitError::StateIo)
}

fn load_generation_snapshot(
    layout: &StateLayout,
    generation_id: &GenerationId,
) -> Result<GenerationSnapshot, CommitError> {
    let root = layout.state_root();
    let record_relative = format!("generations/{}.json", generation_id.as_str());
    let record = read_verified_body(root, &record_relative)?;
    let generation = Generation::from_json(&record).map_err(|_| CommitError::InvalidCandidate)?;
    if generation.id() != generation_id.as_str() {
        return Err(CommitError::InvalidCandidate);
    }
    let manifest = read_verified_snapshot(
        root,
        generation.manifest_snapshot(),
        generation.manifest_hash(),
    )?;
    let lock = read_verified_snapshot(root, generation.lock_snapshot(), generation.lock_hash())?;
    let candidate = CandidateGeneration::new(manifest, lock, record)?;
    let manifest = Manifest::from_json(&candidate.manifest_bytes)
        .map_err(|_| CommitError::InvalidCandidate)?;
    let lock =
        LockedState::from_json(&candidate.lock_bytes).map_err(|_| CommitError::InvalidCandidate)?;
    let state = LifecycleState::new(manifest, lock).map_err(|_| CommitError::InvalidCandidate)?;
    GenerationSnapshot::new(candidate.generation, state).map_err(|_| CommitError::InvalidCandidate)
}

fn generation_record_id(name: &str) -> Option<GenerationId> {
    let stem = name.strip_suffix(".json")?;
    GenerationId::new(stem).ok()
}

fn generation_companion_id(name: &str) -> Option<GenerationId> {
    [
        ".json.sha256",
        ".manifest.json",
        ".manifest.json.sha256",
        ".lock.json",
        ".lock.json.sha256",
    ]
    .iter()
    .find_map(|suffix| {
        name.strip_suffix(suffix)
            .and_then(|stem| GenerationId::new(stem).ok())
    })
}

/// Numeric `gen-NNNN` ordering: true only when `candidate` is a newer
/// generation id than `active`. Length-then-lexicographic comparison of the
/// zero-stripped numbers prevents text order such as `gen-0009` > `gen-0010`.
pub fn strictly_newer(candidate: &str, active: &str) -> bool {
    let Some(candidate) = candidate.strip_prefix("gen-") else {
        return false;
    };
    let Some(active) = active.strip_prefix("gen-") else {
        return false;
    };
    let candidate = candidate.trim_start_matches('0');
    let active = active.trim_start_matches('0');
    let candidate = if candidate.is_empty() { "0" } else { candidate };
    let active = if active.is_empty() { "0" } else { active };
    candidate.len() > active.len() || (candidate.len() == active.len() && candidate > active)
}

impl PreparedGeneration {
    /// Writes candidate snapshots before the immutable record and journals `prepared`.
    pub fn prepare(
        layout: StateLayout,
        candidate: CandidateGeneration,
        plan: ActivationPlan,
        lease: StateLease,
    ) -> Result<Self, CommitError> {
        if !lease.authorizes(&layout, LeaseMode::Exclusive) {
            return Err(CommitError::LeaseRequired);
        }
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
            &layout,
            &lease,
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
            lease,
        })
    }

    /// Publishes roots, retains the forest, and atomically switches `current`.
    pub fn activate(
        self,
        helper: &dyn MaintenanceAdapter,
        nonce: &str,
    ) -> Result<ActivatedGeneration, CommitError> {
        let journal_layout = self.layout.clone();
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
                    append_phase(&journal_layout, &self.lease, &op_id, phase, status, []).map_err(
                        |_| match event {
                            ActivationEvent::Activated => pkg_store::CurrentError::PostActivation,
                            _ => pkg_store::CurrentError::Filesystem(std::io::Error::other(
                                "journal append failed",
                            )),
                        },
                    )?;
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
            _lease: self.lease,
        })
    }

    /// Builds the path-free broker intent for a state-only transition.
    pub fn root_transition_intent(
        &self,
        source_generation: GenerationId,
    ) -> Result<Option<RootSetTransitionIntent>, CommitError> {
        let Some(roots) = self.roots.as_ref() else {
            return Ok(None);
        };
        RootSetTransitionIntent::new(
            source_generation,
            GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            roots
                .request()
                .entries()
                .iter()
                .map(|entry| entry.name().clone())
                .collect(),
        )
        .map(Some)
        .map_err(|_| CommitError::InvalidCandidate)
    }

    /// Builds the exact broker intent for first publication of this generation's roots.
    pub fn root_intent(&self) -> Result<Option<RootSetIntent>, CommitError> {
        let Some(roots) = self.roots.as_ref() else {
            return Ok(None);
        };
        RootSetIntent::new(
            GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            roots.request().entries().to_vec(),
        )
        .map(Some)
        .map_err(|_| CommitError::InvalidCandidate)
    }

    /// Creates a complete root intent that retains unchanged mappings from a
    /// durable source and marks only current acquisition outputs as new.
    pub fn root_intent_from_source(
        &self,
        source_generation: GenerationId,
        added_paths: &[StorePath],
    ) -> Result<Option<RootSetIntent>, CommitError> {
        let Some(roots) = self.roots.as_ref() else {
            return Ok(None);
        };
        let added_paths = added_paths
            .iter()
            .map(StorePath::as_str)
            .collect::<BTreeSet<_>>();
        let added_names = roots
            .request()
            .entries()
            .iter()
            .filter(|entry| added_paths.contains(entry.target().as_str()))
            .map(|entry| entry.name().clone())
            .collect::<Vec<_>>();
        if added_names.len() != added_paths.len() {
            return Err(CommitError::InvalidCandidate);
        }
        RootSetIntent::from_source(
            source_generation,
            GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            roots.request().entries().to_vec(),
            added_names,
        )
        .map(Some)
        .map_err(|_| CommitError::InvalidCandidate)
    }

    /// Finishes local activation after authenticated first root publication.
    pub fn activate_published(
        self,
        report: Option<&RootSetReport>,
        nonce: &str,
    ) -> Result<ActivatedGeneration, CommitError> {
        let journal_layout = self.layout.clone();
        let op_id = self.candidate.generation().operation().op_id().to_owned();
        activate_published_generation(
            &self.layout,
            &GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            &self.plan,
            self.roots.as_ref(),
            report,
            nonce,
            |event| {
                let row = match event {
                    ActivationEvent::Rooted => Some(("commit", "rooted")),
                    ActivationEvent::ForestRetained => None,
                    ActivationEvent::Activated => Some(("activate", "activated")),
                };
                if let Some((phase, status)) = row {
                    append_phase(&journal_layout, &self.lease, &op_id, phase, status, []).map_err(
                        |_| match event {
                            ActivationEvent::Activated => pkg_store::CurrentError::PostActivation,
                            _ => pkg_store::CurrentError::Filesystem(std::io::Error::other(
                                "journal append failed",
                            )),
                        },
                    )?;
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
            _lease: self.lease,
        })
    }

    /// Finishes local activation after an authenticated broker root transition.
    pub fn activate_transitioned(
        self,
        report: Option<&RootSetTransitionReport>,
        nonce: &str,
    ) -> Result<ActivatedGeneration, CommitError> {
        let journal_layout = self.layout.clone();
        let op_id = self.candidate.generation().operation().op_id().to_owned();
        activate_transitioned_generation(
            &self.layout,
            &GenerationId::new(self.candidate.generation().id())
                .map_err(|_| CommitError::InvalidCandidate)?,
            &self.plan,
            self.roots.as_ref(),
            report,
            nonce,
            |event| {
                let row = match event {
                    ActivationEvent::Rooted => Some(("commit", "rooted")),
                    ActivationEvent::ForestRetained => None,
                    ActivationEvent::Activated => Some(("activate", "activated")),
                };
                if let Some((phase, status)) = row {
                    append_phase(&journal_layout, &self.lease, &op_id, phase, status, []).map_err(
                        |_| match event {
                            ActivationEvent::Activated => pkg_store::CurrentError::PostActivation,
                            _ => pkg_store::CurrentError::Filesystem(std::io::Error::other(
                                "journal append failed",
                            )),
                        },
                    )?;
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
            _lease: self.lease,
        })
    }
}

impl ActivatedGeneration {
    /// Restores current views from durable snapshots and appends `committed`.
    pub fn finish(self) -> Result<(), CommitError> {
        restore_current_views(&self.layout, &self.candidate)
            .map_err(|_| CommitError::ActivatedNeedsRecovery)?;
        append_phase(
            &self.layout,
            &self._lease,
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
    lease: &StateLease,
    generation_id: &GenerationId,
    helper: &dyn MaintenanceAdapter,
) -> Result<RecoveryResult, CommitError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
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
        discard_generation_root_last(layout, lease, &generation, helper)?;
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
        discard_generation_root_last(layout, lease, candidate.generation(), helper)?;
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
    let committed = journal_has_status(
        layout,
        lease,
        generation.operation().op_id(),
        "commit",
        "committed",
    )?;
    restore_current_views(layout, &candidate)?;
    if committed {
        Ok(RecoveryResult::AlreadyCommitted)
    } else {
        append_phase(
            layout,
            lease,
            generation.operation().op_id(),
            "commit",
            "committed",
            [("nextStateHash", json!(generation.generation_hash()))],
        )?;
        Ok(RecoveryResult::FinishedActivated)
    }
}

fn discard_generation_root_last(
    layout: &StateLayout,
    lease: &StateLease,
    generation: &Generation,
    helper: &dyn MaintenanceAdapter,
) -> Result<(), CommitError> {
    let generation_id =
        GenerationId::new(generation.id()).map_err(|_| CommitError::InvalidCandidate)?;
    let operation_id = generation.operation().op_id();
    // Abort first. Generic prune recovery must never observe this candidate
    // before the durable commit decision says that it cannot be activated.
    if !journal_has_status(layout, lease, operation_id, "commit", "aborted")? {
        append_phase(
            layout,
            lease,
            operation_id,
            "commit",
            "aborted",
            [
                ("generationId", json!(generation.id())),
                ("operationKind", json!(generation.operation().kind())),
            ],
        )?;
    }
    if !journal_has_status(layout, lease, operation_id, "prune", "intended")? {
        append_phase(
            layout,
            lease,
            operation_id,
            "prune",
            "intended",
            [
                ("generationId", json!(generation.id())),
                (
                    "outputRoots",
                    json!(
                        generation
                            .activation()
                            .output_roots()
                            .iter()
                            .map(StorePath::as_str)
                            .collect::<Vec<_>>()
                    ),
                ),
            ],
        )?;
    }
    discard_generation_paths(layout.state_root(), generation)?;
    authorize_generation_root_removal(layout, &generation_id)
        .map_err(|_| CommitError::ActivationFailed)?;
    helper
        .remove_root_set(&RemoveRootSetRequest::new(
            layout.owner_uid(),
            generation_id,
        ))
        .map_err(|_| CommitError::ActivationFailed)?;
    append_phase(
        layout,
        lease,
        operation_id,
        "prune",
        "pruned",
        [("generationId", json!(generation.id()))],
    )
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
    layout: &StateLayout,
    lease: &StateLease,
    op_id: &str,
    phase: &str,
    status: &str,
    extra: [(&str, Value); N],
) -> Result<(), CommitError> {
    StateJournal::open(layout)
        .and_then(|journal| {
            journal.append(
                lease,
                op_id,
                phase,
                status,
                extra
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value)),
            )
        })
        .map_err(map_journal_error)
}

fn journal_has_status(
    layout: &StateLayout,
    lease: &StateLease,
    op_id: &str,
    phase: &str,
    status: &str,
) -> Result<bool, CommitError> {
    StateJournal::open(layout)
        .and_then(|journal| journal.has_status(lease, op_id, phase, status))
        .map_err(map_journal_error)
}

const fn map_journal_error(error: StateJournalError) -> CommitError {
    match error {
        StateJournalError::LeaseRequired => CommitError::LeaseRequired,
        StateJournalError::InvalidChain | StateJournalError::InvalidRow => {
            CommitError::JournalInvalid
        }
        StateJournalError::UnsafeState | StateJournalError::Io => CommitError::StateIo,
    }
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
    let bytes = read_regular_bounded(&joined_relative(root, relative)?, MAX_STATE_FILE_BYTES)
        .map_err(|_| CommitError::StateIo)?;
    let sidecar = read_regular_bounded(
        &joined_relative(root, &format!("{relative}.sha256"))?,
        MAX_SIDECAR_BYTES,
    )
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

fn read_verified_body(root: &Path, relative: &str) -> Result<Vec<u8>, CommitError> {
    let bytes = read_regular_bounded(&joined_relative(root, relative)?, MAX_STATE_FILE_BYTES)
        .map_err(|_| CommitError::StateIo)?;
    let sidecar = read_regular_bounded(
        &joined_relative(root, &format!("{relative}.sha256"))?,
        MAX_SIDECAR_BYTES,
    )
    .map_err(|_| CommitError::StateIo)?;
    let expected = std::str::from_utf8(&sidecar)
        .map_err(|_| CommitError::InvalidCandidate)?
        .trim_end();
    if body_digest(&bytes).to_string() != expected {
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

/// Best-effort removal of a failed prepare's staging path, ignoring all
/// errors. Unlike install preparation, the state-edit and rollback staging
/// trees are only ever written by this process, so no permission repair is
/// needed before deletion.
pub fn discard_staging(staging: &Path) {
    let Ok(metadata) = fs::symlink_metadata(staging) else {
        return;
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        let _ = fs::remove_dir_all(staging);
    } else {
        let _ = fs::remove_file(staging);
    }
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
    read_regular_bounded(path, MAX_STATE_FILE_BYTES)
}

fn read_regular_bounded(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state path is not a bounded regular file",
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "state file is too large")
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state file grew beyond its limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_nix::{InProcessHelper, InProcessPeer};
    use pkg_store::{LeaseIdentity, inspect_staged_activation};
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

    struct RootLastMaintenance<'a> {
        layout: &'a StateLayout,
        inner: &'a dyn MaintenanceAdapter,
        fail_removal: bool,
        fail_after_removal: bool,
    }

    impl MaintenanceAdapter for RootLastMaintenance<'_> {
        fn publish_root_set(
            &self,
            root_set: &pkg_nix::RootSet,
        ) -> Result<RootSetReport, pkg_nix::MaintenanceError> {
            self.inner.publish_root_set(root_set)
        }

        fn attest_root_set(
            &self,
            request: &pkg_nix::RootSetAttestationRequest,
        ) -> Result<RootSetReport, pkg_nix::MaintenanceError> {
            self.inner.attest_root_set(request)
        }

        fn remove_root_set(
            &self,
            request: &RemoveRootSetRequest,
        ) -> Result<(), pkg_nix::MaintenanceError> {
            assert_eq!(request.owner_uid(), self.layout.owner_uid());
            assert!(
                authorize_generation_root_removal(self.layout, request.generation()).is_ok(),
                "root removal must follow user-state deletion"
            );
            if self.fail_removal {
                return Err(pkg_nix::MaintenanceError::backend_failure());
            }
            self.inner.remove_root_set(request)?;
            if self.fail_after_removal {
                return Err(pkg_nix::MaintenanceError::backend_failure());
            }
            Ok(())
        }

        fn repair_store_paths(
            &self,
            request: &pkg_nix::RepairStorePathsRequest,
        ) -> Result<pkg_nix::RepairStorePathsReport, pkg_nix::MaintenanceError> {
            self.inner.repair_store_paths(request)
        }
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
        for relative in ["", "generations", "journal", "activations", "run"] {
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

    fn mutation_lease(layout: &StateLayout) -> StateLease {
        StateLease::try_exclusive(
            layout,
            &LeaseIdentity::new("op_fixture", "nonce1", "2026-08-09T00:00:00Z").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn prepared_fault_discards_record_snapshots_and_staging() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
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
    fn prepared_and_aborted_without_intent_are_not_generic_prunes() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();

        let maintenance = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: false,
            fail_after_removal: false,
        };
        assert!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );

        append_phase(
            &fixture.layout,
            &prepared.lease,
            "op_fixture",
            "commit",
            "aborted",
            [
                ("generationId", json!("gen-0001")),
                ("operationKind", json!("install")),
            ],
        )
        .unwrap();
        assert!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            Some(fixture.generation_id.clone())
        );
        assert_eq!(
            pending_install_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );

        assert_eq!(
            recover_generation(
                &fixture.layout,
                &prepared.lease,
                &fixture.generation_id,
                &maintenance,
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
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );
    }

    #[test]
    fn failed_root_last_discard_converges_through_generic_recovery() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        let failing = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: true,
            fail_after_removal: false,
        };
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &prepared.lease,
                &fixture.generation_id,
                &failing,
            ),
            Err(CommitError::ActivationFailed)
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            Some(fixture.generation_id.clone())
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );

        let maintenance = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: false,
            fail_after_removal: false,
        };
        assert_eq!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
            vec!["gen-0001".to_owned()]
        );
        assert!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );
    }

    #[test]
    fn cross_operation_prune_is_terminal_for_aborted_install() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        append_phase(
            &fixture.layout,
            &prepared.lease,
            "op_fixture",
            "commit",
            "aborted",
            [
                ("generationId", json!("gen-0001")),
                ("operationKind", json!("install")),
            ],
        )
        .unwrap();
        append_phase(
            &fixture.layout,
            &prepared.lease,
            "op_other_gc",
            "prune",
            "intended",
            [
                ("generationId", json!("gen-0001")),
                ("outputRoots", json!([STORE])),
            ],
        )
        .unwrap();
        let maintenance = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: false,
            fail_after_removal: false,
        };
        assert!(
            fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert_eq!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
            vec!["gen-0001".to_owned()]
        );
        assert!(
            !fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );
        assert!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn helper_removed_before_terminal_row_retries_idempotently() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        let interrupted = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: false,
            fail_after_removal: true,
        };
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &prepared.lease,
                &fixture.generation_id,
                &interrupted,
            ),
            Err(CommitError::ActivationFailed)
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            Some(fixture.generation_id.clone())
        );
        let maintenance = RootLastMaintenance {
            layout: &fixture.layout,
            inner: &fixture.maintenance,
            fail_removal: false,
            fail_after_removal: false,
        };

        type RetainedSnapshots = (Vec<GenerationSnapshot>, Option<GenerationId>);
        assert_eq!(
            pkg_store::recover_prunes(&fixture.layout, &prepared.lease, &maintenance).unwrap(),
            vec!["gen-0001".to_owned()]
        );
        assert_eq!(
            pending_install_discard_generation(&fixture.layout, &prepared.lease).unwrap(),
            None
        );
    }

    #[test]
    fn generation_order_is_numeric_and_discard_staging_removes_debris() {
        assert!(strictly_newer("gen-0010", "gen-0009"));
        assert!(!strictly_newer("gen-0009", "gen-0010"));
        assert!(!strictly_newer("gen-00002", "gen-0002"));
        assert!(!strictly_newer("generation-11", "gen-0010"));
        assert!(!strictly_newer("gen-0010", "generation-11"));
        let temp = Builder::new()
            .prefix("pkg-discard-staging-")
            .tempdir_in(".")
            .unwrap();
        let staging = temp.path().join("gen-0009.staging");
        fs::create_dir(&staging).unwrap();
        symlink(STORE, staging.join("demo")).unwrap();
        discard_staging(&staging);
        assert!(!staging.exists());
        let file = temp.path().join("gen-0010.staging");
        fs::write(&file, b"staging debris").unwrap();
        discard_staging(&file);
        assert!(!file.exists());
        discard_staging(&temp.path().join("missing.staging"));
    }

    #[test]
    fn prepare_requires_and_holds_exclusive_state_lease() {
        let fixture = fixture();
        drop(mutation_lease(&fixture.layout));
        let shared = StateLease::try_shared(&fixture.layout).unwrap();
        assert!(matches!(
            PreparedGeneration::prepare(
                fixture.layout.clone(),
                fixture.candidate,
                fixture.plan,
                shared
            ),
            Err(CommitError::LeaseRequired)
        ));
        assert!(
            !fixture
                .layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
    }

    #[test]
    fn rooted_fault_removes_roots_and_leaves_current_unchanged() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        append_phase(
            &fixture.layout,
            &prepared.lease,
            "op_fixture",
            "commit",
            "rooted",
            [],
        )
        .unwrap();
        drop(prepared);
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
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
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        prepared.activate(&fixture.maintenance, "n1").unwrap();
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
                &fixture.generation_id,
                &fixture.maintenance
            )
            .unwrap(),
            RecoveryResult::FinishedActivated
        );
        assert!(fixture.layout.state_root().join("manifest.json").is_file());
        assert!(
            journal_has_status(
                &fixture.layout,
                &recovery_lease,
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
        let lease = mutation_lease(&fixture.layout);
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap();
        prepared
            .activate(&fixture.maintenance, "n1")
            .unwrap()
            .finish()
            .unwrap();
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
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
    fn committed_generation_loads_as_active_verified_history() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap()
        .activate(&fixture.maintenance, "read1")
        .unwrap()
        .finish()
        .unwrap();

        let lease = StateLease::try_shared(&fixture.layout).unwrap();
        let active = load_active_snapshot(&fixture.layout, &lease)
            .unwrap()
            .unwrap();
        assert_eq!(active.generation().id(), "gen-0001");
        assert_eq!(active.state().manifest().entries().len(), 1);
        let history = load_retained_history(&fixture.layout, &lease).unwrap();
        assert_eq!(history.snapshots().len(), 1);
        assert!(history.summaries()[0].is_active());
    }

    #[test]
    fn broker_transition_receipt_finishes_prepared_generation_without_republication() {
        let fixture = fixture();
        let roots = prepare_root_set(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
            [RootCandidate::from_output_root(
                pkg_core::StorePath::new(STORE).unwrap(),
            )],
        )
        .unwrap();
        let report = RootSetTransitionReport::new(
            publish_root_set(&roots, &fixture.maintenance).unwrap(),
            roots
                .request()
                .entries()
                .iter()
                .map(|entry| entry.name().clone())
                .collect(),
            roots.request().mapping_digest(),
        )
        .unwrap();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        let intent = prepared
            .root_transition_intent(GenerationId::new("gen-0000").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(intent.destination_generation(), &fixture.generation_id);
        assert_eq!(intent.retained_names().len(), 1);
        prepared
            .activate_transitioned(Some(&report), "transitioned1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn broker_publication_receipt_finishes_prepared_generation_without_republication() {
        let fixture = fixture();
        let roots = prepare_root_set(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
            [RootCandidate::from_output_root(
                pkg_core::StorePath::new(STORE).unwrap(),
            )],
        )
        .unwrap();
        let report = publish_root_set(&roots, &fixture.maintenance).unwrap();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        let intent = prepared.root_intent().unwrap().unwrap();
        assert_eq!(intent.generation(), &fixture.generation_id);
        assert_eq!(intent.entries().len(), 1);
        let extended = prepared
            .root_intent_from_source(
                GenerationId::new("gen-0000").unwrap(),
                &[pkg_core::StorePath::new(STORE).unwrap()],
            )
            .unwrap()
            .unwrap();
        assert_eq!(extended.source_generation().unwrap().as_str(), "gen-0000");
        assert_eq!(extended.added_names().len(), 1);
        prepared
            .activate_published(Some(&report), "published1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn broker_attestation_receipt_recovers_prepared_generation_without_republication() {
        let fixture = fixture();
        let roots = prepare_root_set(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
            [RootCandidate::from_output_root(
                pkg_core::StorePath::new(STORE).unwrap(),
            )],
        )
        .unwrap();
        publish_root_set(&roots, &fixture.maintenance).unwrap();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        let report = fixture
            .maintenance
            .attest_root_set(&pkg_nix::RootSetAttestationRequest::new(
                fixture.layout.owner_uid(),
                fixture.generation_id.clone(),
            ))
            .unwrap();
        prepared
            .activate_published(Some(&report), "attested1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn journaled_install_is_discovered_and_resumed_after_restart() {
        let fixture = fixture();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        publish_root_set(prepared.roots.as_ref().unwrap(), &fixture.maintenance).unwrap();
        drop(prepared);

        let resume_lease = mutation_lease(&fixture.layout);
        let pending = pending_install_generation(&fixture.layout, &resume_lease)
            .unwrap()
            .unwrap();
        assert_eq!(pending, fixture.generation_id);
        let resumed =
            resume_prepared_install(fixture.layout.clone(), resume_lease, &pending).unwrap();
        let attested = fixture
            .maintenance
            .attest_root_set(&pkg_nix::RootSetAttestationRequest::new(
                fixture.layout.owner_uid(),
                pending,
            ))
            .unwrap();
        resumed
            .activate_published(Some(&attested), "resumeinstall1")
            .unwrap()
            .finish()
            .unwrap();

        let finished_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            pending_install_generation(&fixture.layout, &finished_lease).unwrap(),
            None
        );
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn published_activation_refuses_a_same_size_wrong_root_mapping() {
        let fixture = fixture();
        let wrong_roots = prepare_root_set(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
            [RootCandidate::from_output_root(
                pkg_core::StorePath::new("/nix/store/22222222222222222222222222222222-other")
                    .unwrap(),
            )],
        )
        .unwrap();
        let report = publish_root_set(&wrong_roots, &fixture.maintenance).unwrap();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        assert!(matches!(
            prepared.activate_published(Some(&report), "mismatch1"),
            Err(CommitError::ActivationFailed)
        ));
        assert_eq!(fixture.layout.current_generation().unwrap(), None);
    }

    #[test]
    fn empty_prepared_generation_needs_neither_transition_nor_receipt() {
        let fixture = empty_fixture();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        assert!(
            prepared
                .root_transition_intent(GenerationId::new("gen-0000").unwrap())
                .unwrap()
                .is_none()
        );
        prepared
            .activate_transitioned(None, "empty1")
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            fixture.layout.current_generation().unwrap(),
            Some(fixture.generation_id)
        );
    }

    #[test]
    fn transitioned_activation_refuses_a_mismatched_broker_receipt() {
        let fixture = fixture();
        let roots = prepare_root_set(
            fixture.layout.owner_uid(),
            fixture.generation_id.clone(),
            [RootCandidate::from_output_root(
                pkg_core::StorePath::new(STORE).unwrap(),
            )],
        )
        .unwrap();
        let published = publish_root_set(&roots, &fixture.maintenance).unwrap();
        let mapping_digest = published.mapping_digest();
        let report = RootSetTransitionReport::new(
            published,
            vec![pkg_nix::RootName::new("wrong-output").unwrap()],
            mapping_digest,
        )
        .unwrap();
        let prepared = PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap();
        assert_eq!(
            prepared
                .activate_transitioned(Some(&report), "wrong1")
                .unwrap_err(),
            CommitError::ActivationFailed
        );
        assert_eq!(fixture.layout.current_generation().unwrap(), None);
    }

    #[test]
    fn retained_history_refuses_unknown_or_oversized_files() {
        let fixture = empty_fixture();
        drop(mutation_lease(&fixture.layout));
        fs::write(
            fixture.layout.state_root().join("generations/unmanaged"),
            b"foreign",
        )
        .unwrap();
        let lease = StateLease::try_shared(&fixture.layout).unwrap();
        assert_eq!(
            load_retained_history(&fixture.layout, &lease).unwrap_err(),
            CommitError::StateIo
        );
        drop(lease);

        fs::remove_file(fixture.layout.state_root().join("generations/unmanaged")).unwrap();
        let orphan = fixture
            .layout
            .state_root()
            .join("generations/gen-9998.lock.json.sha256");
        fs::write(&orphan, b"sha256:orphan\n").unwrap();
        let lease = StateLease::try_shared(&fixture.layout).unwrap();
        assert_eq!(
            load_retained_history(&fixture.layout, &lease).unwrap_err(),
            CommitError::InvalidCandidate
        );
        drop(lease);
        fs::remove_file(orphan).unwrap();

        let oversized = fixture
            .layout
            .state_root()
            .join("generations/gen-9999.json");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_STATE_FILE_BYTES + 1).unwrap();
        fs::write(
            fixture
                .layout
                .state_root()
                .join("generations/gen-9999.json.sha256"),
            b"sha256:0000000000000000000000000000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let lease = StateLease::try_shared(&fixture.layout).unwrap();
        assert_eq!(
            load_retained_history(&fixture.layout, &lease).unwrap_err(),
            CommitError::StateIo
        );
    }

    #[test]
    fn empty_generation_commits_and_recovers_without_publishing_roots() {
        let fixture = empty_fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap()
        .activate(&fixture.maintenance, "empty1")
        .unwrap()
        .finish()
        .unwrap();
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
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
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
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
        let recovery_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_generation(
                &fixture.layout,
                &recovery_lease,
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
    fn interrupted_state_edit_resumes_before_and_after_the_current_switch() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap()
        .activate(&fixture.maintenance, "source1")
        .unwrap()
        .finish()
        .unwrap();

        let edit_lease = mutation_lease(&fixture.layout);
        let source = load_active_snapshot(&fixture.layout, &edit_lease)
            .unwrap()
            .unwrap();
        let next = pkg_core::remove::remove_selectors(
            source.state().clone(),
            &[pkg_core::SelectorId::new("sel_demo").unwrap()],
        )
        .unwrap()
        .into_state();
        let prepared = crate::prepare_state_edit(
            fixture.layout.clone(),
            edit_lease,
            &source,
            &next,
            crate::StateEditMetadata::new(
                "gen-0002",
                "2026-08-11T00:00:00Z",
                "op_remove",
                crate::StateEditKind::Remove,
            ),
        )
        .unwrap();
        drop(prepared);

        let resume_lease = mutation_lease(&fixture.layout);
        let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_str(), "gen-0002");
        let resumed =
            resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
        let activated = resumed.activate_transitioned(None, "resume1").unwrap();
        drop(activated);

        let finish_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
            RecoveryResult::FinishedActivated
        );
        assert_eq!(
            pending_state_edit_generation(&fixture.layout, &finish_lease).unwrap(),
            None
        );
        assert!(
            load_active_snapshot(&fixture.layout, &finish_lease)
                .unwrap()
                .unwrap()
                .state()
                .manifest()
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn interrupted_update_resumes_before_and_after_the_current_switch() {
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap()
        .activate(&fixture.maintenance, "updsource1")
        .unwrap()
        .finish()
        .unwrap();

        let edit_lease = mutation_lease(&fixture.layout);
        let source = load_active_snapshot(&fixture.layout, &edit_lease)
            .unwrap()
            .unwrap();
        let next = pkg_core::advance_channel(
            source.state().clone(),
            pkg_core::ChannelSequence::from_u64(2).unwrap(),
        )
        .unwrap();
        let staging = fixture
            .layout
            .state_root()
            .join("activations/gen-0002.staging");
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
        let plan =
            inspect_staged_activation(&staging, vec![pkg_core::StorePath::new(STORE).unwrap()])
                .unwrap();
        let metadata = crate::StateEditMetadata::new(
            "gen-0002",
            "2026-08-11T00:00:00Z",
            "op_update",
            crate::StateEditKind::Update,
        );
        let candidate = crate::state_edit::build_candidate(
            &source,
            &next,
            &metadata,
            pkg_core::state::CollisionPolicy::Abort,
            &plan,
        )
        .unwrap();
        let prepared =
            PreparedGeneration::prepare(fixture.layout.clone(), candidate, plan, edit_lease)
                .unwrap();
        drop(prepared);

        let resume_lease = mutation_lease(&fixture.layout);
        let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_str(), "gen-0002");
        let resumed =
            resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
        let roots = resumed.roots.as_ref().unwrap();
        let report = RootSetTransitionReport::new(
            publish_root_set(roots, &fixture.maintenance).unwrap(),
            roots
                .request()
                .entries()
                .iter()
                .map(|entry| entry.name().clone())
                .collect(),
            roots.request().mapping_digest(),
        )
        .unwrap();
        let activated = resumed
            .activate_transitioned(Some(&report), "updresume1")
            .unwrap();
        drop(activated);

        let finish_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
            RecoveryResult::FinishedActivated
        );
        assert_eq!(
            pending_state_edit_generation(&fixture.layout, &finish_lease).unwrap(),
            None
        );
        let recovered = load_active_snapshot(&fixture.layout, &finish_lease)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.generation().operation().kind(), "update");
        assert_eq!(recovered.state().manifest().channel_seq().get().get(), 2);
        assert_eq!(recovered.state().manifest().entries().len(), 1);
    }

    #[test]
    fn upgrade_generation_rebinds_channel_outputs_collision_and_approval() {
        const NEXT_STORE: &str = "/nix/store/22222222222222222222222222222222-demo";
        const NEXT_DRV: &str = "/nix/store/33333333333333333333333333333333-demo.drv";
        const NEXT_REV: &str = "89abcdef0123456789abcdef0123456789abcdef";
        let fixture = fixture();
        let lease = mutation_lease(&fixture.layout);
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            lease,
        )
        .unwrap()
        .activate(&fixture.maintenance, "upgradesource1")
        .unwrap()
        .finish()
        .unwrap();

        let lease = mutation_lease(&fixture.layout);
        let source = load_active_snapshot(&fixture.layout, &lease)
            .unwrap()
            .unwrap();
        drop(lease);
        let mut manifest: Value =
            serde_json::from_slice(&source.state().manifest().to_json().unwrap()).unwrap();
        manifest["channelSeq"] = json!(2);
        let mut lock: Value =
            serde_json::from_slice(&source.state().locked().to_json().unwrap()).unwrap();
        lock["channelSeq"] = json!(2);
        lock["entries"]["sel_demo"]["nixpkgsRev"] = json!(NEXT_REV);
        let realized = &mut lock["entries"]["sel_demo"]["realized"];
        realized["storePath"] = json!(NEXT_STORE);
        realized["deriver"] = json!(NEXT_DRV);
        realized["outputs"]["out"] = json!(NEXT_STORE);
        realized["version"] = json!("2.0");
        let next = pkg_core::lifecycle::LifecycleState::new(
            pkg_core::state::Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
            pkg_core::state::LockedState::from_json(&serde_json::to_vec(&lock).unwrap()).unwrap(),
        )
        .unwrap();
        let staging = fixture
            .layout
            .state_root()
            .join("upgrade-candidate.staging");
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(format!("{NEXT_STORE}/bin/demo"), staging.join("demo")).unwrap();
        let plan = inspect_staged_activation(
            &staging,
            vec![pkg_core::StorePath::new(NEXT_STORE).unwrap()],
        )
        .unwrap();
        let metadata = crate::StateEditMetadata::new(
            "gen-0002",
            "2026-08-12T00:00:00Z",
            "op_upgrade",
            crate::StateEditKind::Upgrade,
        )
        .with_collision_policy(pkg_core::state::CollisionPolicy::KeepLast)
        .with_build_approval("yes");
        let candidate = crate::state_edit::build_candidate(
            &source,
            &next,
            &metadata,
            pkg_core::state::CollisionPolicy::KeepLast,
            &plan,
        )
        .unwrap();
        let generation: Value =
            serde_json::from_slice(&candidate.generation().to_json().unwrap()).unwrap();
        assert_eq!(generation["channelSeq"], 2);
        assert_eq!(generation["outputs"][0]["storePath"], NEXT_STORE);
        assert_eq!(generation["outputs"][0]["nixpkgsRev"], NEXT_REV);
        assert_eq!(generation["activation"]["collisionPolicy"], "keep-last");
        assert_eq!(generation["operation"]["kind"], "upgrade");
        assert_eq!(generation["operation"]["approval"]["build"], "yes");
    }

    #[test]
    fn interrupted_rollback_resumes_from_the_retained_target() {
        let fixture = fixture();
        PreparedGeneration::prepare(
            fixture.layout.clone(),
            fixture.candidate,
            fixture.plan,
            mutation_lease(&fixture.layout),
        )
        .unwrap()
        .activate(&fixture.maintenance, "rbsrc1")
        .unwrap()
        .finish()
        .unwrap();

        let edit_lease = mutation_lease(&fixture.layout);
        let source = load_active_snapshot(&fixture.layout, &edit_lease)
            .unwrap()
            .unwrap();
        let empty = pkg_core::remove::remove_selectors(
            source.state().clone(),
            &[pkg_core::SelectorId::new("sel_demo").unwrap()],
        )
        .unwrap()
        .into_state();
        crate::prepare_state_edit(
            fixture.layout.clone(),
            edit_lease,
            &source,
            &empty,
            crate::StateEditMetadata::new(
                "gen-0002",
                "2026-08-11T00:00:00Z",
                "op_remove_before_rollback",
                crate::StateEditKind::Remove,
            ),
        )
        .unwrap()
        .activate_transitioned(None, "rbempty1")
        .unwrap()
        .finish()
        .unwrap();

        let rollback_lease = mutation_lease(&fixture.layout);
        let active = load_active_snapshot(&fixture.layout, &rollback_lease)
            .unwrap()
            .unwrap();
        let history = load_retained_history(&fixture.layout, &rollback_lease).unwrap();
        let retained = history
            .snapshots()
            .iter()
            .filter(|snapshot| snapshot.generation().id() != active.generation().id())
            .cloned()
            .collect::<Vec<_>>();
        let rollback = pkg_core::plan_rollback(
            &active,
            &retained,
            pkg_core::RollbackTarget::Named("gen-0001".to_owned()),
            |_| true,
        )
        .unwrap();
        crate::rollback::prepare_rollback_with(
            fixture.layout.clone(),
            rollback_lease,
            &rollback,
            "gen-0003",
            "2026-08-11T00:00:01Z",
            "op_rollback",
            |staging, inputs, _| {
                fs::create_dir(staging).unwrap();
                fs::set_permissions(staging, fs::Permissions::from_mode(0o700)).unwrap();
                symlink(format!("{STORE}/bin/demo"), staging.join("demo")).unwrap();
                inspect_staged_activation(
                    staging,
                    inputs
                        .iter()
                        .map(|input| input.store_path().clone())
                        .collect(),
                )
            },
        )
        .unwrap();

        let resume_lease = mutation_lease(&fixture.layout);
        let pending = pending_state_edit_generation(&fixture.layout, &resume_lease)
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_str(), "gen-0003");
        assert_eq!(
            pending_state_transition_source(&fixture.layout, &resume_lease, &pending)
                .unwrap()
                .as_str(),
            "gen-0001"
        );
        let resumed =
            resume_prepared_state_edit(fixture.layout.clone(), resume_lease, &pending).unwrap();
        let roots = resumed.roots.as_ref().unwrap();
        let published = publish_root_set(roots, &fixture.maintenance).unwrap();
        let report = RootSetTransitionReport::new(
            published,
            roots
                .request()
                .entries()
                .iter()
                .map(|entry| entry.name().clone())
                .collect(),
            roots.request().mapping_digest(),
        )
        .unwrap();
        let activated = resumed
            .activate_transitioned(Some(&report), "rbresume1")
            .unwrap();
        drop(activated);

        let finish_lease = mutation_lease(&fixture.layout);
        assert_eq!(
            recover_transitioned_state_edit(&fixture.layout, &finish_lease, &pending).unwrap(),
            RecoveryResult::FinishedActivated
        );
        let recovered = load_active_snapshot(&fixture.layout, &finish_lease)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.generation().operation().kind(), "rollback");
        assert_eq!(recovered.state().manifest().entries().len(), 1);
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
        let lease = mutation_lease(&fixture.layout);
        assert!(
            append_phase(
                &fixture.layout,
                &lease,
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
