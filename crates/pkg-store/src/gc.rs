use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use pkg_core::GenerationSnapshot;
use pkg_nix::{GcReport, GenerationId, MaintenanceAdapter, NixAdapter, RemoveRootSetRequest};
use serde_json::{Value, json};

use crate::{LeaseMode, StateJournal, StateLayout, StateLease};

const MAX_GENERATIONS: usize = 10_000;
const MAX_KEEP_GENERATIONS: usize = 1_000;
const MAX_AGE_DAYS: u64 = 36_525;
const SECONDS_PER_DAY: u64 = 86_400;

/// Bounded retention policy for retired generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcPolicy {
    keep_generations: usize,
    max_age_days: u64,
}

impl GcPolicy {
    /// Validates count and age bounds. The active generation is always extra.
    pub const fn new(keep_generations: usize, max_age_days: u64) -> Result<Self, GcError> {
        if keep_generations > MAX_KEEP_GENERATIONS || max_age_days > MAX_AGE_DAYS {
            return Err(GcError::InvalidPolicy);
        }
        Ok(Self {
            keep_generations,
            max_age_days,
        })
    }

    /// Returns how many newest retired generations are protected.
    #[must_use]
    pub const fn keep_generations(self) -> usize {
        self.keep_generations
    }

    /// Returns the maximum age that remains protected.
    #[must_use]
    pub const fn max_age_days(self) -> u64 {
        self.max_age_days
    }
}

/// One verified retired generation eligible for root-last pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneCandidate {
    snapshot: GenerationSnapshot,
}

impl PruneCandidate {
    /// Returns the immutable generation selected for pruning.
    #[must_use]
    pub const fn snapshot(&self) -> &GenerationSnapshot {
        &self.snapshot
    }

    /// Returns the bounded generation id.
    #[must_use]
    pub fn generation_id(&self) -> &str {
        self.snapshot.generation().id()
    }
}

/// A deterministic, mutation-free GC preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    active_generation: String,
    candidates: Vec<PruneCandidate>,
    estimated_reclaimable_bytes: u64,
}

impl GcPlan {
    /// Returns the active generation that must still match under the lease.
    #[must_use]
    pub fn active_generation(&self) -> &str {
        &self.active_generation
    }

    /// Returns eligible generations in ascending numeric id order.
    #[must_use]
    pub fn candidates(&self) -> &[PruneCandidate] {
        &self.candidates
    }

    /// Returns the approximate sum of unique selected-root closure sizes.
    #[must_use]
    pub const fn estimated_reclaimable_bytes(&self) -> u64 {
        self.estimated_reclaimable_bytes
    }
}

/// Builds a conservative retention plan without changing roots or state.
///
/// A retired generation is eligible only when it is both outside the newest
/// count window and older than the age window. `now_unix_seconds` is supplied
/// by the caller so tests and previews use one stable clock observation.
pub fn plan_gc(
    active: &GenerationSnapshot,
    generations: &[GenerationSnapshot],
    policy: GcPolicy,
    now_unix_seconds: u64,
) -> Result<GcPlan, GcError> {
    if generations.is_empty() || generations.len() > MAX_GENERATIONS {
        return Err(GcError::InvalidArchive);
    }
    let active_id = active.generation().id();
    let active_uid = active.generation().uid();
    let active_system = active.state().locked().system();
    let mut ids = BTreeSet::new();
    let mut numeric_ids = BTreeSet::new();
    let mut active_count = 0;
    let mut retired = Vec::new();
    for snapshot in generations {
        if !ids.insert(snapshot.generation().id())
            || !numeric_ids.insert(normalized_id(snapshot.generation().id()))
            || snapshot.generation().uid() != active_uid
            || snapshot.state().locked().system() != active_system
        {
            return Err(GcError::InvalidArchive);
        }
        if snapshot.generation().id() == active_id {
            if snapshot != active {
                return Err(GcError::InvalidArchive);
            }
            active_count += 1;
        } else {
            let created = parse_utc_seconds(snapshot.generation().created_at())?;
            if created > now_unix_seconds {
                return Err(GcError::InvalidTimestamp);
            }
            retired.push((snapshot.clone(), now_unix_seconds - created));
        }
    }
    if active_count != 1 {
        return Err(GcError::InvalidArchive);
    }
    retired
        .sort_by(|left, right| numeric_id_cmp(right.0.generation().id(), left.0.generation().id()));
    let maximum_age = policy
        .max_age_days
        .checked_mul(SECONDS_PER_DAY)
        .ok_or(GcError::InvalidPolicy)?;
    let mut candidates = retired
        .iter()
        .enumerate()
        .filter(|(index, (_, age))| *index >= policy.keep_generations && *age > maximum_age)
        .map(|(_, (snapshot, _))| PruneCandidate {
            snapshot: snapshot.clone(),
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| numeric_id_cmp(left.generation_id(), right.generation_id()));

    let candidate_ids = candidates
        .iter()
        .map(PruneCandidate::generation_id)
        .collect::<BTreeSet<_>>();
    let protected_paths = generations
        .iter()
        .filter(|snapshot| !candidate_ids.contains(snapshot.generation().id()))
        .flat_map(|snapshot| snapshot.generation().outputs())
        .map(|output| output.store_path().as_str())
        .collect::<BTreeSet<_>>();
    let mut counted = BTreeSet::new();
    let mut estimated_reclaimable_bytes = 0_u64;
    for candidate in &candidates {
        for output in candidate.snapshot.generation().outputs() {
            let path = output.store_path().as_str();
            if !protected_paths.contains(path) && counted.insert(path) {
                estimated_reclaimable_bytes = estimated_reclaimable_bytes
                    .checked_add(output.closure_nar_size())
                    .ok_or(GcError::SizeOverflow)?;
            }
        }
    }
    Ok(GcPlan {
        active_generation: active_id.to_owned(),
        candidates,
        estimated_reclaimable_bytes,
    })
}

/// Selects one explicit retired generation after validating the same archive
/// invariants as retention GC.
pub fn plan_generation_prune(
    active: &GenerationSnapshot,
    generations: &[GenerationSnapshot],
    generation_id: &str,
    now_unix_seconds: u64,
) -> Result<PruneCandidate, GcError> {
    GenerationId::new(generation_id).map_err(|_| GcError::InvalidArchive)?;
    let validation_policy = GcPolicy::new(MAX_KEEP_GENERATIONS, MAX_AGE_DAYS)?;
    let _ = plan_gc(active, generations, validation_policy, now_unix_seconds)?;
    if active.generation().id() == generation_id {
        return Err(GcError::CurrentChanged);
    }
    generations
        .iter()
        .find(|snapshot| snapshot.generation().id() == generation_id)
        .cloned()
        .map(|snapshot| PruneCandidate { snapshot })
        .ok_or(GcError::InvalidArchive)
}

/// Result of one idempotent prune transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneOutcome {
    /// User metadata and the privileged root set were retired root-last.
    Pruned,
    /// A durable `pruned` row already completed this exact operation/id.
    AlreadyPruned,
}

/// Prunes one planned generation while its output roots remain live until last.
pub fn prune_generation(
    layout: &StateLayout,
    lease: &StateLease,
    candidate: &PruneCandidate,
    helper: &dyn MaintenanceAdapter,
    operation_id: &str,
) -> Result<PruneOutcome, GcError> {
    require_exclusive(layout, lease)?;
    let journal = StateJournal::open(layout).map_err(|_| GcError::Journal)?;
    let id = candidate.generation_id();
    let existing = prune_state(&journal, lease, id)?;
    if matches!(existing, Some((_, true))) {
        return Ok(PruneOutcome::AlreadyPruned);
    }
    refuse_active(layout, id)?;
    match existing {
        Some((existing_operation, false)) if existing_operation != operation_id => {
            return Err(GcError::Journal);
        }
        Some(_) => {}
        None => journal
            .append(
                lease,
                operation_id,
                "prune",
                "intended",
                [
                    ("kind".into(), json!("gc")),
                    ("generationId".into(), json!(id)),
                    (
                        "outputRoots".into(),
                        json!(
                            candidate
                                .snapshot
                                .generation()
                                .activation()
                                .output_roots()
                                .iter()
                                .map(pkg_core::StorePath::as_str)
                                .collect::<Vec<_>>()
                        ),
                    ),
                ],
            )
            .map_err(|_| GcError::Journal)?,
    }
    delete_user_generation(layout.state_root(), layout.owner_uid(), id)?;
    authorize_generation_root_removal(
        layout,
        &GenerationId::new(id).map_err(|_| GcError::UnsafeState)?,
    )?;
    remove_roots(layout, id, helper)?;
    journal
        .append(
            lease,
            operation_id,
            "prune",
            "pruned",
            [("generationId".into(), json!(id))],
        )
        .map_err(|_| GcError::Journal)?;
    Ok(PruneOutcome::Pruned)
}

/// Completes every durable prune intent that lacks a terminal row.
pub fn recover_prunes(
    layout: &StateLayout,
    lease: &StateLease,
    helper: &dyn MaintenanceAdapter,
) -> Result<Vec<String>, GcError> {
    require_exclusive(layout, lease)?;
    let journal = StateJournal::open(layout).map_err(|_| GcError::Journal)?;
    let rows = journal.rows(lease).map_err(|_| GcError::Journal)?;
    let mut pending = BTreeMap::<String, String>::new();
    let mut terminal = BTreeSet::new();
    for row in rows {
        let fields = row.payload().fields();
        if fields.get("phase").and_then(Value::as_str) != Some("prune") {
            continue;
        }
        let Some(operation_id) = fields.get("opId").and_then(Value::as_str) else {
            return Err(GcError::Journal);
        };
        let Some(id) = fields.get("generationId").and_then(Value::as_str) else {
            continue;
        };
        GenerationId::new(id).map_err(|_| GcError::Journal)?;
        match fields.get("status").and_then(Value::as_str) {
            Some("intended") if terminal.contains(id) => return Err(GcError::Journal),
            Some("intended") => match pending.get(id) {
                Some(existing) if existing != operation_id => {
                    return Err(GcError::Journal);
                }
                Some(_) => {}
                None => {
                    pending.insert(id.to_owned(), operation_id.to_owned());
                }
            },
            Some("pruned") => {
                pending.remove(id);
                terminal.insert(id.to_owned());
            }
            _ => {}
        }
    }
    let mut recovered = Vec::new();
    for (id, operation_id) in pending {
        refuse_active(layout, &id)?;
        delete_user_generation(layout.state_root(), layout.owner_uid(), &id)?;
        authorize_generation_root_removal(
            layout,
            &GenerationId::new(&id).map_err(|_| GcError::UnsafeState)?,
        )?;
        remove_roots(layout, &id, helper)?;
        journal
            .append(
                lease,
                &operation_id,
                "prune",
                "pruned",
                [("generationId".into(), json!(id))],
            )
            .map_err(|_| GcError::Journal)?;
        recovered.push(id);
    }
    Ok(recovered)
}

/// Successful generation pruning followed by one broker-mediated Nix GC.
#[derive(Debug)]
pub struct GcRunReport {
    pruned_generations: Vec<String>,
    nix_report: GcReport,
}

impl GcRunReport {
    /// Returns generation ids whose root-last transactions completed now.
    #[must_use]
    pub fn pruned_generations(&self) -> &[String] {
        &self.pruned_generations
    }

    /// Returns Nix's authoritative collector outcome.
    #[must_use]
    pub const fn nix_report(&self) -> &GcReport {
        &self.nix_report
    }
}

/// Executes a previously previewed plan and invokes the collector exactly once.
///
/// The caller is responsible for holding the broker's exclusive GC admission
/// permit around this function; PR-22 deliberately owns only the per-user lease.
pub fn execute_gc(
    layout: &StateLayout,
    lease: &StateLease,
    plan: &GcPlan,
    helper: &dyn MaintenanceAdapter,
    nix: &dyn NixAdapter,
    operation_id: &str,
) -> Result<GcRunReport, GcError> {
    require_exclusive(layout, lease)?;
    if layout
        .current_generation()
        .map_err(|_| GcError::UnsafeState)?
        .as_ref()
        .map(GenerationId::as_str)
        != Some(plan.active_generation())
    {
        return Err(GcError::CurrentChanged);
    }
    let mut pruned_generations = Vec::new();
    for candidate in &plan.candidates {
        if prune_generation(layout, lease, candidate, helper, operation_id)? == PruneOutcome::Pruned
        {
            pruned_generations.push(candidate.generation_id().to_owned());
        }
    }
    let nix_report = nix.gc().map_err(|_| GcError::Nix)?;
    Ok(GcRunReport {
        pruned_generations,
        nix_report,
    })
}

/// Stable GC planning, pruning, and execution failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcError {
    /// Retention bounds were outside the supported V1 envelope.
    InvalidPolicy,
    /// The archive was missing active, duplicated ids, cross-owner, or cross-system.
    InvalidArchive,
    /// A generation timestamp was malformed or later than the preview clock.
    InvalidTimestamp,
    /// A byte estimate overflowed.
    SizeOverflow,
    /// A caller attempted mutation without an exclusive state lease.
    LeaseRequired,
    /// `current` changed or named the prune target.
    CurrentChanged,
    /// User state paths failed closed validation or durable deletion.
    UnsafeState,
    /// The durable journal was unavailable, corrupt, or invalid.
    Journal,
    /// Privileged root removal was refused.
    RootRemoval,
    /// User generation metadata still exists or `current` names the target.
    PruneNotAuthorized,
    /// The broker-mediated Nix collector failed.
    Nix,
}

impl fmt::Display for GcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "garbage collection refused: {self:?}")
    }
}
impl std::error::Error for GcError {}

fn require_exclusive(layout: &StateLayout, lease: &StateLease) -> Result<(), GcError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(GcError::LeaseRequired);
    }
    Ok(())
}

fn prune_state(
    journal: &StateJournal,
    lease: &StateLease,
    generation_id: &str,
) -> Result<Option<(String, bool)>, GcError> {
    let mut state = None;
    for row in journal.rows(lease).map_err(|_| GcError::Journal)? {
        let fields = row.payload().fields();
        if fields.get("phase").and_then(Value::as_str) != Some("prune")
            || fields.get("generationId").and_then(Value::as_str) != Some(generation_id)
        {
            continue;
        }
        let operation = fields
            .get("opId")
            .and_then(Value::as_str)
            .ok_or(GcError::Journal)?;
        match fields.get("status").and_then(Value::as_str) {
            Some("intended") => match &state {
                Some((existing, false)) if existing != operation => return Err(GcError::Journal),
                Some((_, true)) => return Err(GcError::Journal),
                _ => state = Some((operation.to_owned(), false)),
            },
            Some("pruned") => state = Some((operation.to_owned(), true)),
            _ => {}
        }
    }
    Ok(state)
}

fn refuse_active(layout: &StateLayout, generation_id: &str) -> Result<(), GcError> {
    if layout
        .current_generation()
        .map_err(|_| GcError::UnsafeState)?
        .is_some_and(|current| current.as_str() == generation_id)
    {
        return Err(GcError::CurrentChanged);
    }
    Ok(())
}

/// Revalidates the root-last authorization state immediately before a
/// privileged helper removes one generation root set.
///
/// The active generation and every user-owned generation/activation artifact
/// must already be absent. This lets the privileged helper independently
/// reject a forged raw removal request without accepting a caller-selected
/// path or trusting the CLI's earlier planning result.
pub fn authorize_generation_root_removal(
    layout: &StateLayout,
    generation: &GenerationId,
) -> Result<(), GcError> {
    layout.validate().map_err(|_| GcError::UnsafeState)?;
    if layout
        .current_generation()
        .map_err(|_| GcError::UnsafeState)?
        .is_some_and(|current| current == *generation)
    {
        return Err(GcError::PruneNotAuthorized);
    }
    let id = generation.as_str();
    for path in [
        layout.state_root().join("activations").join(id),
        layout
            .state_root()
            .join("activations")
            .join(format!("{id}.staging")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.json")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.json.sha256")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.manifest.json")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.manifest.json.sha256")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.lock.json")),
        layout
            .state_root()
            .join("generations")
            .join(format!("{id}.lock.json.sha256")),
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err(GcError::PruneNotAuthorized),
            Err(_) => return Err(GcError::UnsafeState),
        }
    }
    Ok(())
}

fn remove_roots(
    layout: &StateLayout,
    generation_id: &str,
    helper: &dyn MaintenanceAdapter,
) -> Result<(), GcError> {
    let generation = GenerationId::new(generation_id).map_err(|_| GcError::UnsafeState)?;
    helper
        .remove_root_set(&RemoveRootSetRequest::new(layout.owner_uid(), generation))
        .map_err(|_| GcError::RootRemoval)
}

fn delete_user_generation(root: &Path, owner_uid: u32, generation_id: &str) -> Result<(), GcError> {
    let activations = root.join("activations");
    validate_owned_directory(&activations, owner_uid)?;
    remove_directory_if_present(&activations.join(generation_id))?;
    remove_directory_if_present(&activations.join(format!("{generation_id}.staging")))?;
    sync_directory(&activations)?;
    let generations = root.join("generations");
    validate_owned_directory(&generations, owner_uid)?;
    for suffix in [
        ".json",
        ".json.sha256",
        ".manifest.json",
        ".manifest.json.sha256",
        ".lock.json",
        ".lock.json.sha256",
    ] {
        remove_file_if_present(&generations.join(format!("{generation_id}{suffix}")))?;
    }
    sync_directory(&generations)
}

fn remove_directory_if_present(path: &Path) -> Result<(), GcError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(GcError::UnsafeState),
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|_| GcError::UnsafeState)
        }
        Ok(_) => Err(GcError::UnsafeState),
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), GcError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(GcError::UnsafeState),
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(path).map_err(|_| GcError::UnsafeState)
        }
        Ok(_) => Err(GcError::UnsafeState),
    }
}

fn sync_directory(path: &Path) -> Result<(), GcError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| GcError::UnsafeState)
}

fn validate_owned_directory(path: &Path, owner_uid: u32) -> Result<(), GcError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| GcError::UnsafeState)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(GcError::UnsafeState);
    }
    Ok(())
}

fn numeric_id_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left = normalized_id(left);
    let right = normalized_id(right);
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn normalized_id(value: &str) -> &str {
    let digits = value
        .strip_prefix("gen-")
        .unwrap_or(value)
        .trim_start_matches('0');
    if digits.is_empty() { "0" } else { digits }
}

fn parse_utc_seconds(value: &str) -> Result<u64, GcError> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err(GcError::InvalidTimestamp);
    }
    let year = digits(bytes, 0, 4)? as i64;
    let month = digits(bytes, 5, 7)? as i64;
    let day = digits(bytes, 8, 10)? as i64;
    let hour = digits(bytes, 11, 13)? as i64;
    let minute = digits(bytes, 14, 16)? as i64;
    let second = digits(bytes, 17, 19)? as i64;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(GcError::InvalidTimestamp);
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second)
        .map_err(|_| GcError::InvalidTimestamp)
}

fn digits(bytes: &[u8], start: usize, end: usize) -> Result<u64, GcError> {
    bytes[start..end].iter().try_fold(0_u64, |value, byte| {
        if byte.is_ascii_digit() {
            Ok(value * 10 + u64::from(byte - b'0'))
        } else {
            Err(GcError::InvalidTimestamp)
        }
    })
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_core::state::{Generation, LockedState, Manifest, body_digest, canonical_digest};
    use pkg_nix::{GcStatus, InProcessHelper, InProcessPeer};
    use pkg_testkit::FakeNix;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use tempfile::{Builder, TempDir};

    #[test]
    fn retention_requires_both_count_and_age_expiry_and_never_selects_active() {
        let old = snapshot("gen-0001", None, "2026-06-01T00:00:00Z");
        let recent = snapshot("gen-0002", Some("gen-0001"), "2026-08-09T00:00:00Z");
        let active = snapshot("gen-0003", Some("gen-0002"), "2026-08-10T00:00:00Z");
        let archive = vec![old, active.clone(), recent];
        let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
        let plan = plan_gc(&active, &archive, GcPolicy::new(1, 30).unwrap(), now).unwrap();
        assert_eq!(
            plan.candidates()
                .iter()
                .map(PruneCandidate::generation_id)
                .collect::<Vec<_>>(),
            ["gen-0001"]
        );
        assert_eq!(plan.active_generation(), "gen-0003");
        assert_eq!(plan.estimated_reclaimable_bytes(), 0);

        let age_only = plan_gc(&active, &archive, GcPolicy::new(0, 30).unwrap(), now).unwrap();
        assert_eq!(age_only.candidates()[0].generation_id(), "gen-0001");
        assert!(
            !age_only
                .candidates()
                .iter()
                .any(|candidate| candidate.generation_id() == "gen-0002")
        );
    }

    #[test]
    fn manual_prune_selects_only_a_retired_retained_generation() {
        let retired = snapshot("gen-0001", None, "2026-08-09T00:00:00Z");
        let active = snapshot("gen-0002", Some("gen-0001"), "2026-08-10T00:00:00Z");
        let archive = vec![retired.clone(), active.clone()];
        let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();

        let candidate = plan_generation_prune(&active, &archive, "gen-0001", now).unwrap();
        assert_eq!(candidate.snapshot(), &retired);
        assert_eq!(
            plan_generation_prune(&active, &archive, "gen-0002", now),
            Err(GcError::CurrentChanged)
        );
        assert_eq!(
            plan_generation_prune(&active, &archive, "gen-9999", now),
            Err(GcError::InvalidArchive)
        );
    }

    #[test]
    fn root_removal_authority_requires_root_last_state_and_refuses_active() {
        let (_temp, layout) = layout_fixture();
        let retired = GenerationId::new("gen-0001").unwrap();
        let active = GenerationId::new("gen-0002").unwrap();
        write_generation_assets(layout.state_root(), retired.as_str());

        assert_eq!(
            authorize_generation_root_removal(&layout, &retired),
            Err(GcError::PruneNotAuthorized)
        );
        assert_eq!(
            authorize_generation_root_removal(&layout, &active),
            Err(GcError::PruneNotAuthorized)
        );

        delete_user_generation(layout.state_root(), layout.owner_uid(), retired.as_str()).unwrap();
        assert_eq!(authorize_generation_root_removal(&layout, &retired), Ok(()));
    }

    #[test]
    fn planning_rejects_duplicate_missing_active_and_future_timestamps() {
        let active = snapshot("gen-0002", None, "2026-08-10T00:00:00Z");
        let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
        assert_eq!(
            plan_gc(
                &active,
                &[active.clone(), active.clone()],
                GcPolicy::new(0, 0).unwrap(),
                now
            ),
            Err(GcError::InvalidArchive)
        );
        assert_eq!(
            plan_gc(&active, &[], GcPolicy::new(0, 0).unwrap(), now),
            Err(GcError::InvalidArchive)
        );
        let future = snapshot("gen-0003", None, "2026-08-11T00:00:00Z");
        assert_eq!(
            plan_gc(
                &active,
                &[active.clone(), future],
                GcPolicy::new(0, 0).unwrap(),
                now
            ),
            Err(GcError::InvalidTimestamp)
        );
    }

    #[test]
    fn execute_prunes_metadata_root_last_then_calls_fake_nix_once() {
        let (temp, layout) = layout_fixture();
        let old = snapshot("gen-0001", None, "2026-06-01T00:00:00Z");
        let active = snapshot("gen-0002", Some("gen-0001"), "2026-08-10T00:00:00Z");
        write_generation_assets(layout.state_root(), "gen-0001");
        let now = parse_utc_seconds("2026-08-10T00:00:00Z").unwrap();
        let plan = plan_gc(
            &active,
            &[old, active.clone()],
            GcPolicy::new(0, 0).unwrap(),
            now,
        )
        .unwrap();
        let lease = StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(layout.owner_uid());
        let report = GcReport::new(GcStatus::Collected, Vec::new(), 4096).unwrap();
        let fake = FakeNix::new();
        fake.expect_gc(Ok(report));
        let result = execute_gc(&layout, &lease, &plan, &maintenance, &fake, "op_gc").unwrap();
        assert_eq!(result.pruned_generations(), ["gen-0001"]);
        assert_eq!(result.nix_report().freed_bytes(), 4096);
        assert!(
            !layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert!(!layout.state_root().join("activations/gen-0001").exists());
        assert_eq!(
            layout.current_generation().unwrap().unwrap().as_str(),
            "gen-0002"
        );
        let rows = StateJournal::open(&layout).unwrap().rows(&lease).unwrap();
        assert_eq!(
            rows.iter()
                .filter_map(|row| row.payload().fields().get("status"))
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            ["intended", "pruned"]
        );
        fake.assert_exhausted().unwrap();
        drop(temp);
    }

    #[test]
    fn active_target_and_shared_lease_are_refused_before_deletion() {
        let (_temp, layout) = layout_fixture();
        write_generation_assets(layout.state_root(), "gen-0002");
        let active = snapshot("gen-0002", None, "2026-08-10T00:00:00Z");
        let candidate = PruneCandidate { snapshot: active };
        drop(
            StateLease::try_exclusive(
                &layout,
                &crate::LeaseIdentity::new("op_seed", "nonce0", "2026-08-10T00:00:00Z").unwrap(),
            )
            .unwrap(),
        );
        let shared = StateLease::try_shared(&layout).unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(layout.owner_uid());
        assert_eq!(
            prune_generation(&layout, &shared, &candidate, &maintenance, "op_gc"),
            Err(GcError::LeaseRequired)
        );
        drop(shared);
        let exclusive = StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap();
        assert_eq!(
            prune_generation(&layout, &exclusive, &candidate, &maintenance, "op_gc"),
            Err(GcError::CurrentChanged)
        );
        assert!(
            layout
                .state_root()
                .join("generations/gen-0002.json")
                .exists()
        );
    }

    #[test]
    fn recovery_finishes_an_intended_prune_idempotently() {
        let (_temp, layout) = layout_fixture();
        write_generation_assets(layout.state_root(), "gen-0001");
        let lease = StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_recover", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap();
        let journal = StateJournal::open(&layout).unwrap();
        journal
            .append(
                &lease,
                "op_gc",
                "prune",
                "intended",
                [
                    ("kind".into(), json!("gc")),
                    ("generationId".into(), json!("gen-0001")),
                    ("outputRoots".into(), json!([])),
                ],
            )
            .unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(layout.owner_uid());
        assert_eq!(
            recover_prunes(&layout, &lease, &maintenance).unwrap(),
            ["gen-0001"]
        );
        assert!(
            recover_prunes(&layout, &lease, &maintenance)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn root_removal_failure_leaves_only_a_recoverable_rooted_intent() {
        let (_temp, layout) = layout_fixture();
        write_generation_assets(layout.state_root(), "gen-0001");
        let lease = StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap();
        let candidate = PruneCandidate {
            snapshot: snapshot("gen-0001", None, "2026-06-01T00:00:00Z"),
        };
        let helper = InProcessHelper::new(991).unwrap();
        let authenticated = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        let wrong_caller = authenticated.for_caller(layout.owner_uid().saturating_add(1));
        assert_eq!(
            prune_generation(&layout, &lease, &candidate, &wrong_caller, "op_gc"),
            Err(GcError::RootRemoval)
        );
        assert!(
            !layout
                .state_root()
                .join("generations/gen-0001.json")
                .exists()
        );
        assert_eq!(
            layout.current_generation().unwrap().unwrap().as_str(),
            "gen-0002"
        );

        let maintenance = authenticated.for_caller(layout.owner_uid());
        assert_eq!(
            recover_prunes(&layout, &lease, &maintenance).unwrap(),
            ["gen-0001"]
        );
    }

    #[test]
    fn symlinked_generation_parent_is_refused_before_cross_tree_deletion() {
        let (temp, layout) = layout_fixture();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("gen-0001.json"), b"keep").unwrap();
        fs::remove_dir(layout.state_root().join("generations")).unwrap();
        symlink(&outside, layout.state_root().join("generations")).unwrap();
        let lease = StateLease::try_exclusive(
            &layout,
            &crate::LeaseIdentity::new("op_gc", "nonce1", "2026-08-10T00:00:00Z").unwrap(),
        )
        .unwrap();
        let candidate = PruneCandidate {
            snapshot: snapshot("gen-0001", None, "2026-06-01T00:00:00Z"),
        };
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(layout.owner_uid());
        assert_eq!(
            prune_generation(&layout, &lease, &candidate, &maintenance, "op_gc"),
            Err(GcError::UnsafeState)
        );
        assert_eq!(fs::read(outside.join("gen-0001.json")).unwrap(), b"keep");
    }

    fn layout_fixture() -> (TempDir, StateLayout) {
        let temp = Builder::new().prefix("pkg-gc-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        for relative in [
            "",
            "run",
            "journal",
            "generations",
            "activations",
            "activations/gen-0002",
        ] {
            let path = state.join(relative);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        symlink("activations/gen-0002", state.join("current")).unwrap();
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        (temp, layout)
    }

    fn write_generation_assets(root: &Path, id: &str) {
        let activation = root.join("activations").join(id);
        fs::create_dir_all(&activation).unwrap();
        fs::set_permissions(&activation, fs::Permissions::from_mode(0o700)).unwrap();
        for suffix in [
            ".json",
            ".json.sha256",
            ".manifest.json",
            ".manifest.json.sha256",
            ".lock.json",
            ".lock.json.sha256",
        ] {
            fs::write(
                root.join("generations").join(format!("{id}{suffix}")),
                b"fixture",
            )
            .unwrap();
        }
    }

    fn snapshot(id: &str, parent: Option<&str>, created_at: &str) -> GenerationSnapshot {
        let uid = fs::symlink_metadata(".").unwrap().uid();
        let manifest_bytes = serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "channelSeq": 1,
            "uid": uid,
            "entries": [],
            "pins": []
        }))
        .unwrap();
        let lock_bytes = serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "channelSeq": 1,
            "system": "x86_64-linux",
            "uid": uid,
            "entries": {}
        }))
        .unwrap();
        let lifecycle = pkg_core::lifecycle::LifecycleState::new(
            Manifest::from_json(&manifest_bytes).unwrap(),
            LockedState::from_json(&lock_bytes).unwrap(),
        )
        .unwrap();
        let mut generation = json!({
            "schemaVersion": 1,
            "uid": uid,
            "id": id,
            "parent": parent,
            "createdAt": created_at,
            "channelSeq": 1,
            "manifestHash": body_digest(&manifest_bytes).to_string(),
            "lockHash": body_digest(&lock_bytes).to_string(),
            "manifestSnapshot": format!("generations/{id}.manifest.json"),
            "lockSnapshot": format!("generations/{id}.lock.json"),
            "activation": {
                "kind": "pkg-symlink-forest",
                "treePath": format!("activations/{id}"),
                "treeDigest": body_digest(b"").to_string(),
                "entryCount": 0,
                "collisionPolicy": "abort",
                "outputRoots": [],
                "collisionResolutions": []
            },
            "outputs": [],
            "operation": {
                "opId": format!("op_{id}"),
                "kind": "install",
                "approval": { "build": "not_required" }
            }
        });
        let hash = canonical_digest(&generation).unwrap().to_string();
        generation
            .as_object_mut()
            .unwrap()
            .insert("generationHash".into(), json!(hash));
        let generation = Generation::from_json(&serde_json::to_vec(&generation).unwrap()).unwrap();
        GenerationSnapshot::new(generation, lifecycle).unwrap()
    }
}
