//! Privileged, closed-grammar maintenance contract and in-process reference helper.
//!
//! This module is deliberately separate from [`crate::NixAdapter`]. The
//! unprivileged adapter can inspect and realize store state, but only an
//! authenticated helper session may publish/remove generation root sets or
//! perform the fixed repair operation defined here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pkg_core::channel::PolicyVersion;
use pkg_core::identity::StorePath;
use pkg_core::state::Digest;
use sha2::{Digest as _, Sha256};

use crate::{RootName, RootRef};

const MAX_GENERATION_ID_BYTES: usize = 36;
const MAX_ROOT_SET_ENTRIES: usize = 4096;
const MAX_REPAIR_PATHS: usize = 4096;
const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);

/// Stable privileged-maintenance failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceErrorCode {
    /// A typed request violated its closed grammar or size limits.
    ValidationFailure,
    /// The transport peer was not the configured broker identity.
    UnauthenticatedPeer,
    /// The helper session predates the latest helper restart.
    SessionRestarted,
    /// The opaque capability is unknown to this helper session.
    CapabilityMissing,
    /// The capability exceeded its fixed lifetime.
    CapabilityExpired,
    /// The single-use capability was already consumed.
    CapabilityReplayed,
    /// Caller, generation, policy, mode, plan, or path binding drifted.
    CapabilityMismatch,
    /// The requested rooted generation is not present in helper state.
    GenerationNotRooted,
    /// The in-process reference backend could not complete the operation.
    BackendFailure,
}

/// Redacted error returned by the privileged maintenance boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceError {
    code: MaintenanceErrorCode,
}

impl MaintenanceError {
    const fn new(code: MaintenanceErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> MaintenanceErrorCode {
        self.code
    }

    /// Creates the closed failure used by platform helper implementations.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(MaintenanceErrorCode::BackendFailure)
    }
}

impl fmt::Display for MaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "maintenance request refused: {:?}", self.code)
    }
}

impl std::error::Error for MaintenanceError {}

/// Canonical monotonic generation identifier (`gen-<at least four digits>`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(String);

impl GenerationId {
    /// Validates and constructs a generation identifier.
    pub fn new(value: &str) -> Result<Self, MaintenanceError> {
        let digits = value.strip_prefix("gen-").unwrap_or_default();
        if value.len() <= MAX_GENERATION_ID_BYTES
            && digits.len() >= 4
            && digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ))
        }
    }

    /// Returns the canonical identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One validated safe-name to store-path mapping in a generation root set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetEntry {
    name: RootName,
    target: StorePath,
}

impl RootSetEntry {
    /// Constructs an entry from already validated components.
    #[must_use]
    pub const fn new(name: RootName, target: StorePath) -> Self {
        Self { name, target }
    }

    /// Returns the safe root name.
    #[must_use]
    pub const fn name(&self) -> &RootName {
        &self.name
    }

    /// Returns the exact store target.
    #[must_use]
    pub const fn target(&self) -> &StorePath {
        &self.target
    }
}

/// Sorted, generation-scoped set of privileged GC-root mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSet {
    owner_uid: u32,
    generation: GenerationId,
    entries: Vec<RootSetEntry>,
}

/// Complete root publication bound to exact new outputs and an optional
/// durable source generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetPublicationRequest {
    root_set: RootSet,
    source_generation: Option<GenerationId>,
    added_names: Vec<RootName>,
}

/// Path-free request to derive one generation root set from a durable source.
///
/// Only existing root names may be retained. Store targets are recovered by
/// the privileged helper from its trusted source generation, so neither the
/// CLI nor broker request can introduce or rewrite a store path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetTransitionRequest {
    owner_uid: u32,
    source_generation: GenerationId,
    destination_generation: GenerationId,
    retained_names: Vec<RootName>,
}

/// Ownerless transition intent accepted from the authenticated CLI channel.
///
/// The broker injects the peer uid when promoting this value to a privileged
/// [`RootSetTransitionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetTransitionIntent {
    source_generation: GenerationId,
    destination_generation: GenerationId,
    retained_names: Vec<RootName>,
}

impl RootSetTransitionIntent {
    /// Validates and canonicalizes one ownerless, path-free transition intent.
    pub fn new(
        source_generation: GenerationId,
        destination_generation: GenerationId,
        retained_names: Vec<RootName>,
    ) -> Result<Self, MaintenanceError> {
        let request = RootSetTransitionRequest::new(
            0,
            source_generation,
            destination_generation,
            retained_names,
        )?;
        Ok(Self {
            source_generation: request.source_generation,
            destination_generation: request.destination_generation,
            retained_names: request.retained_names,
        })
    }

    /// Returns the trusted source generation selected by verified local state.
    #[must_use]
    pub const fn source_generation(&self) -> &GenerationId {
        &self.source_generation
    }

    /// Returns the fresh destination generation selected by the state transaction.
    #[must_use]
    pub const fn destination_generation(&self) -> &GenerationId {
        &self.destination_generation
    }

    /// Returns retained root names in canonical order.
    #[must_use]
    pub fn retained_names(&self) -> &[RootName] {
        &self.retained_names
    }

    pub(crate) fn into_request(
        self,
        owner_uid: u32,
    ) -> Result<RootSetTransitionRequest, MaintenanceError> {
        RootSetTransitionRequest::new(
            owner_uid,
            self.source_generation,
            self.destination_generation,
            self.retained_names,
        )
    }
}

impl RootSetTransitionRequest {
    /// Validates and canonicalizes one non-empty, path-free transition.
    pub fn new(
        owner_uid: u32,
        source_generation: GenerationId,
        destination_generation: GenerationId,
        mut retained_names: Vec<RootName>,
    ) -> Result<Self, MaintenanceError> {
        retained_names.sort();
        if source_generation == destination_generation
            || retained_names.is_empty()
            || retained_names.len() > MAX_ROOT_SET_ENTRIES
            || retained_names.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            owner_uid,
            source_generation,
            destination_generation,
            retained_names,
        })
    }

    /// Returns the authenticated user identity owning both generations.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Returns the trusted durable generation from which targets are loaded.
    #[must_use]
    pub const fn source_generation(&self) -> &GenerationId {
        &self.source_generation
    }

    /// Returns the new generation receiving the derived root set.
    #[must_use]
    pub const fn destination_generation(&self) -> &GenerationId {
        &self.destination_generation
    }

    /// Returns retained root names in canonical order.
    #[must_use]
    pub fn retained_names(&self) -> &[RootName] {
        &self.retained_names
    }

    /// Derives the destination without accepting any target outside `source`.
    pub fn derive_from(&self, source: &RootSet) -> Result<RootSet, MaintenanceError> {
        if source.owner_uid != self.owner_uid || source.generation != self.source_generation {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        let retained = self.retained_names.iter().collect::<BTreeSet<_>>();
        let entries = source
            .entries
            .iter()
            .filter(|entry| retained.contains(entry.name()))
            .cloned()
            .collect::<Vec<_>>();
        if entries.len() != self.retained_names.len() {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        RootSet::new(self.owner_uid, self.destination_generation.clone(), entries)
    }
}

/// Caller-owned generation root intent with no serialized owner identity.
///
/// The broker promotes this value into a [`RootSet`] only after injecting the
/// uid authenticated from the CLI transport. This prevents payload identity
/// from selecting another user's durable root namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetIntent {
    source_generation: Option<GenerationId>,
    generation: GenerationId,
    entries: Vec<RootSetEntry>,
    added_names: Vec<RootName>,
}

impl RootSetIntent {
    /// Validates and canonicalizes a complete generation root intent.
    pub fn new(
        generation: GenerationId,
        entries: Vec<RootSetEntry>,
    ) -> Result<Self, MaintenanceError> {
        let validated = RootSet::new(0, generation, entries)?;
        let added_names = validated
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        Ok(Self {
            source_generation: None,
            generation: validated.generation,
            entries: validated.entries,
            added_names,
        })
    }

    /// Validates a complete generation that retains roots from one durable
    /// source and adds only the named outputs from the current acquisition.
    pub fn from_source(
        source_generation: GenerationId,
        generation: GenerationId,
        entries: Vec<RootSetEntry>,
        added_names: Vec<RootName>,
    ) -> Result<Self, MaintenanceError> {
        let request = RootSetPublicationRequest::new(
            RootSet::new(0, generation, entries)?,
            Some(source_generation),
            added_names,
        )?;
        Ok(Self {
            source_generation: request.source_generation,
            generation: request.root_set.generation,
            entries: request.root_set.entries,
            added_names: request.added_names,
        })
    }

    /// Returns the durable source generation for retained roots, if any.
    #[must_use]
    pub const fn source_generation(&self) -> Option<&GenerationId> {
        self.source_generation.as_ref()
    }

    /// Returns the validated generation identifier.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }

    /// Returns entries in canonical safe-name order.
    #[must_use]
    pub fn entries(&self) -> &[RootSetEntry] {
        &self.entries
    }

    /// Returns the exact entries added by the current acquisition.
    #[must_use]
    pub fn added_names(&self) -> &[RootName] {
        &self.added_names
    }

    pub(crate) fn into_publication_request(
        self,
        owner_uid: u32,
    ) -> Result<RootSetPublicationRequest, MaintenanceError> {
        RootSetPublicationRequest::new(
            RootSet::new(owner_uid, self.generation, self.entries)?,
            self.source_generation,
            self.added_names,
        )
    }
}

impl RootSetPublicationRequest {
    /// Validates and canonicalizes one complete protected publication.
    pub fn new(
        root_set: RootSet,
        source_generation: Option<GenerationId>,
        mut added_names: Vec<RootName>,
    ) -> Result<Self, MaintenanceError> {
        added_names.sort();
        let entry_names = root_set
            .entries
            .iter()
            .map(RootSetEntry::name)
            .collect::<BTreeSet<_>>();
        if added_names.is_empty()
            || added_names.len() > root_set.entries.len()
            || added_names.windows(2).any(|pair| pair[0] == pair[1])
            || added_names.iter().any(|name| !entry_names.contains(name))
            || source_generation
                .as_ref()
                .is_some_and(|source| source == &root_set.generation)
            || (source_generation.is_none() && added_names.len() != root_set.entries.len())
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            root_set,
            source_generation,
            added_names,
        })
    }

    /// Returns the complete destination root set.
    #[must_use]
    pub const fn root_set(&self) -> &RootSet {
        &self.root_set
    }

    /// Returns the durable source generation for retained roots, if any.
    #[must_use]
    pub const fn source_generation(&self) -> Option<&GenerationId> {
        self.source_generation.as_ref()
    }

    /// Returns the exact names added by the current acquisition.
    #[must_use]
    pub fn added_names(&self) -> &[RootName] {
        &self.added_names
    }

    /// Revalidates each retained mapping against the durable source loaded by
    /// the privileged helper.
    pub fn validate_source(&self, source: Option<&RootSet>) -> Result<(), MaintenanceError> {
        let added = self.added_names.iter().collect::<BTreeSet<_>>();
        match (&self.source_generation, source) {
            (None, None) => Ok(()),
            (Some(source_generation), Some(source))
                if source.owner_uid == self.root_set.owner_uid
                    && &source.generation == source_generation =>
            {
                let source_entries = source
                    .entries
                    .iter()
                    .map(|entry| (entry.name(), entry.target()))
                    .collect::<BTreeMap<_, _>>();
                if self.root_set.entries.iter().any(|entry| {
                    !added.contains(entry.name())
                        && source_entries.get(entry.name()) != Some(&entry.target())
                }) {
                    return Err(MaintenanceError::new(
                        MaintenanceErrorCode::ValidationFailure,
                    ));
                }
                Ok(())
            }
            _ => Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            )),
        }
    }
}

impl RootSet {
    /// Validates, sorts, and constructs a complete root set.
    pub fn new(
        owner_uid: u32,
        generation: GenerationId,
        mut entries: Vec<RootSetEntry>,
    ) -> Result<Self, MaintenanceError> {
        if entries.is_empty() || entries.len() > MAX_ROOT_SET_ENTRIES {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        entries.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        if entries
            .windows(2)
            .any(|pair| pair[0].name.as_str() == pair[1].name.as_str())
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            owner_uid,
            generation,
            entries,
        })
    }

    /// Returns the authenticated user identity owning this generation.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Returns the validated generation identifier.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }

    /// Returns entries in canonical safe-name order.
    #[must_use]
    pub fn entries(&self) -> &[RootSetEntry] {
        &self.entries
    }

    /// Digests the complete canonical name-to-store-path mapping without exposing its paths.
    #[must_use]
    pub fn mapping_digest(&self) -> Digest {
        let mut hasher = Sha256::new();
        hasher.update(b"pkg-root-set-mapping-v1\0");
        hasher.update((self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            let name = entry.name().as_str().as_bytes();
            let target = entry.target().as_str().as_bytes();
            hasher.update((name.len() as u64).to_be_bytes());
            hasher.update(name);
            hasher.update((target.len() as u64).to_be_bytes());
            hasher.update(target);
        }
        Digest::from_bytes(hasher.finalize().into())
    }
}

/// Closed request to remove one service-resolved generation root set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveRootSetRequest {
    owner_uid: u32,
    generation: GenerationId,
}

/// Closed request to attest one service-resolved durable generation root set.
///
/// The request contains no root names or store paths. The privileged helper
/// reconstructs the complete mapping from its trusted durable namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetAttestationRequest {
    owner_uid: u32,
    generation: GenerationId,
}

impl RootSetAttestationRequest {
    /// Constructs a path-free root-set attestation request.
    #[must_use]
    pub const fn new(owner_uid: u32, generation: GenerationId) -> Self {
        Self {
            owner_uid,
            generation,
        }
    }

    /// Returns the authenticated owner uid.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Returns the service-resolved generation id.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }
}

impl RemoveRootSetRequest {
    /// Constructs a path-free root-set removal request.
    #[must_use]
    pub const fn new(owner_uid: u32, generation: GenerationId) -> Self {
        Self {
            owner_uid,
            generation,
        }
    }

    /// Returns the authenticated owner uid.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Returns the service-resolved generation id.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }
}

/// Durable result of publishing a complete generation root set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetReport {
    reference: RootRef,
    entry_count: usize,
    mapping_digest: Digest,
}

/// Authenticated receipt binding a root transition to its exact retained names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSetTransitionReport {
    root_set: RootSetReport,
    retained_names: Vec<RootName>,
    mapping_digest: Digest,
}

impl RootSetTransitionReport {
    /// Validates a destination receipt against its exact canonical retained names.
    pub fn new(
        root_set: RootSetReport,
        retained_names: Vec<RootName>,
        mapping_digest: Digest,
    ) -> Result<Self, MaintenanceError> {
        if retained_names.is_empty()
            || retained_names.len() != root_set.entry_count()
            || retained_names.windows(2).any(|pair| pair[0] >= pair[1])
            || mapping_digest != root_set.mapping_digest()
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            root_set,
            retained_names,
            mapping_digest,
        })
    }

    /// Returns the canonical destination root report.
    #[must_use]
    pub const fn root_set(&self) -> &RootSetReport {
        &self.root_set
    }

    /// Returns the exact canonical retained names authenticated by the helper response.
    #[must_use]
    pub fn retained_names(&self) -> &[RootName] {
        &self.retained_names
    }

    /// Returns a domain-separated digest of the complete canonical name-to-store-path mapping.
    #[must_use]
    pub const fn mapping_digest(&self) -> Digest {
        self.mapping_digest
    }
}

impl RootSetReport {
    pub(crate) const fn new(
        reference: RootRef,
        entry_count: usize,
        mapping_digest: Digest,
    ) -> Self {
        Self {
            reference,
            entry_count,
            mapping_digest,
        }
    }

    /// Returns the canonical managed generation-root directory.
    #[must_use]
    pub const fn reference(&self) -> &RootRef {
        &self.reference
    }

    /// Returns the number of atomically published root entries.
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Returns a domain-separated digest of the exact published root mapping.
    #[must_use]
    pub const fn mapping_digest(&self) -> Digest {
        self.mapping_digest
    }
}

/// Fixed privileged repair mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMode {
    /// Substitution only: `max-jobs=0` and no remote builders.
    CacheOnly,
    /// Explicitly approved local repair build with bounded nonzero jobs.
    Build,
}

/// Server-validated repair scope captured before capability issuance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRepairScope {
    owner_uid: u32,
    generation: GenerationId,
    paths: Vec<StorePath>,
    build_plan_digest: Option<Digest>,
    policy_version: PolicyVersion,
    mode: RepairMode,
}

impl VerifiedRepairScope {
    /// Constructs a server-side scope from the full verified damage set.
    pub fn new(
        owner_uid: u32,
        generation: GenerationId,
        paths: impl IntoIterator<Item = StorePath>,
        build_plan_digest: Option<Digest>,
        policy_version: PolicyVersion,
        mode: RepairMode,
    ) -> Result<Self, MaintenanceError> {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        paths.dedup_by(|left, right| left.as_str() == right.as_str());
        if paths.is_empty()
            || paths.len() > MAX_REPAIR_PATHS
            || (mode == RepairMode::Build) != build_plan_digest.is_some()
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::ValidationFailure,
            ));
        }
        Ok(Self {
            owner_uid,
            generation,
            paths,
            build_plan_digest,
            policy_version,
            mode,
        })
    }

    /// Returns the caller uid bound into the capability.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Returns the rooted generation bound into the capability.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }

    /// Returns the full sorted, de-duplicated verified damage set.
    #[must_use]
    pub fn paths(&self) -> &[StorePath] {
        &self.paths
    }

    /// Returns the approved build-plan digest for build mode.
    #[must_use]
    pub const fn build_plan_digest(&self) -> Option<Digest> {
        self.build_plan_digest
    }

    /// Returns the policy version bound into the capability.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the fixed repair mode.
    #[must_use]
    pub const fn mode(&self) -> RepairMode {
        self.mode
    }
}

/// Opaque helper-issued, expiring, single-use maintenance capability.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaintenanceCapability(pub(crate) String);

impl MaintenanceCapability {
    /// Returns the opaque transport token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MaintenanceCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MaintenanceCapability(<opaque>)")
    }
}

/// The only request accepted by the privileged repair method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairStorePathsRequest {
    capability: MaintenanceCapability,
}

impl RepairStorePathsRequest {
    /// Constructs a request carrying no caller-selected repair grammar.
    #[must_use]
    pub const fn new(capability: MaintenanceCapability) -> Self {
        Self { capability }
    }

    /// Returns the opaque helper-issued capability.
    #[must_use]
    pub const fn capability(&self) -> &MaintenanceCapability {
        &self.capability
    }
}

/// Sanitized per-path repair outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairOutcomeKind {
    /// The path was restored using the fixed selected repair mode.
    Restored,
    /// Fresh verification found the path already intact.
    Unchanged,
    /// Cache-only repair could not substitute and stopped before building.
    CacheMiss,
}

/// One typed, sanitized repair outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPathOutcome {
    path: StorePath,
    kind: RepairOutcomeKind,
}

impl RepairPathOutcome {
    pub(crate) const fn new(path: StorePath, kind: RepairOutcomeKind) -> Self {
        Self { path, kind }
    }

    /// Returns the validated store path.
    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    /// Returns the closed sanitized outcome.
    #[must_use]
    pub const fn kind(&self) -> RepairOutcomeKind {
        self.kind
    }
}

/// Sanitized report for one fixed repair operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairStorePathsReport {
    mode: RepairMode,
    outcomes: Vec<RepairPathOutcome>,
}

/// Executes one capability-validated repair scope without accepting raw Nix
/// arguments, paths, or policy overrides from the transport request.
///
/// Implementations return exactly one outcome kind for each path in the
/// supplied, sorted scope. The helper reconstructs the report from those
/// trusted paths and refuses a cardinality mismatch.
pub trait VerifiedRepairExecutor: Send + Sync {
    /// Runs the fixed repair mode selected by the verified scope.
    fn execute(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<Vec<RepairOutcomeKind>, MaintenanceError>;
}

#[derive(Debug, Default)]
struct ReferenceRepairExecutor;

impl VerifiedRepairExecutor for ReferenceRepairExecutor {
    fn execute(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<Vec<RepairOutcomeKind>, MaintenanceError> {
        Ok(vec![RepairOutcomeKind::Restored; scope.paths().len()])
    }
}

impl RepairStorePathsReport {
    pub(crate) const fn new(mode: RepairMode, outcomes: Vec<RepairPathOutcome>) -> Self {
        Self { mode, outcomes }
    }

    /// Returns the helper-enforced repair mode.
    #[must_use]
    pub const fn mode(&self) -> RepairMode {
        self.mode
    }

    /// Returns path-sorted sanitized outcomes.
    #[must_use]
    pub fn outcomes(&self) -> &[RepairPathOutcome] {
        &self.outcomes
    }
}

/// Privileged object-safe helper boundary with four closed operations.
pub trait MaintenanceAdapter: Send + Sync {
    /// Atomically publishes one complete generation root set.
    fn publish_root_set(&self, root_set: &RootSet) -> Result<RootSetReport, MaintenanceError>;

    /// Attests one durable root set without accepting names or store paths.
    fn attest_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, MaintenanceError>;

    /// Removes exactly one service-resolved generation root set.
    fn remove_root_set(&self, request: &RemoveRootSetRequest) -> Result<(), MaintenanceError>;

    /// Redeems one opaque capability for the helper's fixed repair operation.
    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, MaintenanceError>;
}

/// Transport-derived identity used by the in-process reference channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InProcessPeer {
    uid: u32,
}

impl InProcessPeer {
    /// Simulates an OS-authenticated peer uid for reference tests.
    #[must_use]
    pub const fn authenticated_uid(uid: u32) -> Self {
        Self { uid }
    }
}

#[derive(Debug, Clone)]
struct CapabilityRecord {
    scope: VerifiedRepairScope,
    expires_at: Instant,
}

#[derive(Debug)]
struct HelperState {
    epoch: u64,
    broker_epoch: u64,
    secret: [u8; 32],
    next_capability: u64,
    root_sets: BTreeMap<(u32, GenerationId), RootSet>,
    capabilities: BTreeMap<MaintenanceCapability, CapabilityRecord>,
    consumed: BTreeSet<MaintenanceCapability>,
}

/// In-process reference helper used to validate authentication and restart semantics.
pub struct InProcessHelper {
    broker_uid: u32,
    state: Mutex<HelperState>,
    repair_executor: Arc<dyn VerifiedRepairExecutor>,
}

impl fmt::Debug for InProcessHelper {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InProcessHelper(<private-state>)")
    }
}

impl InProcessHelper {
    /// Creates a helper pinned to one broker service uid.
    pub fn new(broker_uid: u32) -> Result<Arc<Self>, MaintenanceError> {
        Self::with_repair_executor(broker_uid, Arc::new(ReferenceRepairExecutor))
    }

    /// Creates a helper pinned to one broker uid and one closed repair
    /// executor. Production services use this seam for the fixed root-owned
    /// Nix repair operation; tests may supply a deterministic executor.
    pub fn with_repair_executor(
        broker_uid: u32,
        repair_executor: Arc<dyn VerifiedRepairExecutor>,
    ) -> Result<Arc<Self>, MaintenanceError> {
        Ok(Arc::new(Self {
            broker_uid,
            state: Mutex::new(HelperState {
                epoch: 1,
                broker_epoch: 1,
                secret: random_secret()?,
                next_capability: 0,
                root_sets: BTreeMap::new(),
                capabilities: BTreeMap::new(),
                consumed: BTreeSet::new(),
            }),
            repair_executor,
        }))
    }

    /// Authenticates the configured broker peer and establishes a session.
    pub fn connect(
        self: &Arc<Self>,
        peer: InProcessPeer,
    ) -> Result<AuthenticatedHelper, MaintenanceError> {
        if peer.uid != self.broker_uid {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::UnauthenticatedPeer,
            ));
        }
        let state = self.lock();
        let epoch = state.epoch;
        let broker_epoch = state.broker_epoch;
        drop(state);
        Ok(AuthenticatedHelper {
            helper: Arc::clone(self),
            epoch,
            broker_epoch,
        })
    }

    /// Simulates a helper restart: durable root sets remain, all sessions and
    /// capabilities become invalid, and new entropy is installed.
    pub fn restart(&self) -> Result<(), MaintenanceError> {
        let mut state = self.lock();
        state.epoch = state.epoch.saturating_add(1);
        state.secret = random_secret()?;
        state.next_capability = 0;
        state.capabilities.clear();
        state.consumed.clear();
        Ok(())
    }

    /// Applies the broker-side restart handshake, invalidating every existing
    /// broker session and maintenance capability while preserving root sets.
    pub fn broker_restarted(&self) -> Result<(), MaintenanceError> {
        let mut state = self.lock();
        state.broker_epoch = state.broker_epoch.saturating_add(1);
        state.secret = random_secret()?;
        state.next_capability = 0;
        state.capabilities.clear();
        state.consumed.clear();
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HelperState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Authenticated broker-to-helper session established by the restart handshake.
#[derive(Clone)]
pub struct AuthenticatedHelper {
    helper: Arc<InProcessHelper>,
    epoch: u64,
    broker_epoch: u64,
}

impl fmt::Debug for AuthenticatedHelper {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticatedHelper(<authenticated-session>)")
    }
}

impl AuthenticatedHelper {
    /// Binds all maintenance calls from this client to one authenticated user.
    #[must_use]
    pub fn for_caller(&self, caller_uid: u32) -> CallerMaintenance {
        CallerMaintenance {
            session: self.clone(),
            caller_uid,
        }
    }

    const fn check_epoch(&self, state: &HelperState) -> Result<(), MaintenanceError> {
        if state.epoch == self.epoch && state.broker_epoch == self.broker_epoch {
            Ok(())
        } else {
            Err(MaintenanceError::new(
                MaintenanceErrorCode::SessionRestarted,
            ))
        }
    }
}

/// Caller-bound maintenance client; the uid is authenticated by the broker,
/// not read from any serialized maintenance request.
#[derive(Clone)]
pub struct CallerMaintenance {
    session: AuthenticatedHelper,
    caller_uid: u32,
}

impl fmt::Debug for CallerMaintenance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CallerMaintenance(<caller-bound-session>)")
    }
}

impl CallerMaintenance {
    /// Issues a fixed-lifetime capability after verifying the rooted generation.
    pub fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, MaintenanceError> {
        self.issue_with_deadline(scope, Instant::now() + CAPABILITY_TTL)
    }

    fn issue_with_deadline(
        &self,
        scope: &VerifiedRepairScope,
        expires_at: Instant,
    ) -> Result<MaintenanceCapability, MaintenanceError> {
        let mut state = self.session.helper.lock();
        self.session.check_epoch(&state)?;
        if scope.owner_uid != self.caller_uid {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::CapabilityMismatch,
            ));
        }
        if !state
            .root_sets
            .contains_key(&(scope.owner_uid, scope.generation.clone()))
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::GenerationNotRooted,
            ));
        }
        state.next_capability = state.next_capability.saturating_add(1);
        let capability = mint_capability(&state, scope);
        state.capabilities.insert(
            capability.clone(),
            CapabilityRecord {
                scope: scope.clone(),
                expires_at,
            },
        );
        Ok(capability)
    }

    const fn check_caller(&self, uid: u32) -> Result<(), MaintenanceError> {
        if uid == self.caller_uid {
            Ok(())
        } else {
            Err(MaintenanceError::new(
                MaintenanceErrorCode::CapabilityMismatch,
            ))
        }
    }
}

impl MaintenanceAdapter for CallerMaintenance {
    fn publish_root_set(&self, root_set: &RootSet) -> Result<RootSetReport, MaintenanceError> {
        self.check_caller(root_set.owner_uid)?;
        let mut state = self.session.helper.lock();
        self.session.check_epoch(&state)?;
        let key = (root_set.owner_uid, root_set.generation.clone());
        if let Some(existing) = state.root_sets.get(&key) {
            if existing != root_set {
                return Err(MaintenanceError::new(MaintenanceErrorCode::BackendFailure));
            }
        } else {
            state.root_sets.insert(key, root_set.clone());
        }
        let reference = RootRef::new(&format!(
            "/nix/var/nix/gcroots/pkg/users/{}/{}",
            root_set.owner_uid,
            root_set.generation.as_str()
        ))
        .map_err(|_| MaintenanceError::new(MaintenanceErrorCode::BackendFailure))?;
        Ok(RootSetReport::new(
            reference,
            root_set.entries.len(),
            root_set.mapping_digest(),
        ))
    }

    fn attest_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, MaintenanceError> {
        self.check_caller(request.owner_uid)?;
        let state = self.session.helper.lock();
        self.session.check_epoch(&state)?;
        let root_set = state
            .root_sets
            .get(&(request.owner_uid, request.generation.clone()))
            .ok_or_else(|| MaintenanceError::new(MaintenanceErrorCode::GenerationNotRooted))?;
        let reference = RootRef::new(&format!(
            "/nix/var/nix/gcroots/pkg/users/{}/{}",
            request.owner_uid,
            request.generation.as_str()
        ))
        .map_err(|_| MaintenanceError::new(MaintenanceErrorCode::BackendFailure))?;
        Ok(RootSetReport::new(
            reference,
            root_set.entries.len(),
            root_set.mapping_digest(),
        ))
    }

    fn remove_root_set(&self, request: &RemoveRootSetRequest) -> Result<(), MaintenanceError> {
        self.check_caller(request.owner_uid)?;
        let mut state = self.session.helper.lock();
        self.session.check_epoch(&state)?;
        state
            .root_sets
            .remove(&(request.owner_uid, request.generation.clone()));
        state.capabilities.retain(|_, record| {
            record.scope.owner_uid != request.owner_uid
                || record.scope.generation != request.generation
        });
        Ok(())
    }

    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, MaintenanceError> {
        self.redeem_at(request, Instant::now())
    }
}

impl CallerMaintenance {
    fn redeem_at(
        &self,
        request: &RepairStorePathsRequest,
        now: Instant,
    ) -> Result<RepairStorePathsReport, MaintenanceError> {
        let mut state = self.session.helper.lock();
        self.session.check_epoch(&state)?;
        if state.consumed.contains(&request.capability) {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::CapabilityReplayed,
            ));
        }
        let record = state
            .capabilities
            .remove(&request.capability)
            .ok_or_else(|| MaintenanceError::new(MaintenanceErrorCode::CapabilityMissing))?;
        state.consumed.insert(request.capability.clone());
        if now >= record.expires_at {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::CapabilityExpired,
            ));
        }
        if record.scope.owner_uid != self.caller_uid
            || !state
                .root_sets
                .contains_key(&(record.scope.owner_uid, record.scope.generation.clone()))
        {
            return Err(MaintenanceError::new(
                MaintenanceErrorCode::CapabilityMismatch,
            ));
        }
        let kinds = self.session.helper.repair_executor.execute(&record.scope)?;
        if kinds.len() != record.scope.paths.len() {
            return Err(MaintenanceError::new(MaintenanceErrorCode::BackendFailure));
        }
        let mode = record.scope.mode;
        let outcomes = record
            .scope
            .paths
            .into_iter()
            .zip(kinds)
            .map(|(path, kind)| RepairPathOutcome::new(path, kind))
            .collect();
        Ok(RepairStorePathsReport::new(mode, outcomes))
    }
}

pub(crate) fn random_secret() -> Result<[u8; 32], MaintenanceError> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| MaintenanceError::new(MaintenanceErrorCode::BackendFailure))?;
    Ok(bytes)
}

fn mint_capability(state: &HelperState, scope: &VerifiedRepairScope) -> MaintenanceCapability {
    let mut hasher = Sha256::new();
    hasher.update(state.secret);
    hasher.update(state.epoch.to_be_bytes());
    hasher.update(state.next_capability.to_be_bytes());
    hasher.update(scope.owner_uid.to_be_bytes());
    hasher.update(scope.generation.as_str().as_bytes());
    hasher.update(scope.policy_version.get().get().to_be_bytes());
    hasher.update([match scope.mode {
        RepairMode::CacheOnly => 0,
        RepairMode::Build => 1,
    }]);
    if let Some(digest) = scope.build_plan_digest {
        hasher.update(digest.as_bytes());
    }
    for path in &scope.paths {
        hasher.update(path.as_str().as_bytes());
        hasher.update([0]);
    }
    let bytes: [u8; 32] = hasher.finalize().into();
    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    MaintenanceCapability(token)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pkg_core::state::body_digest;

    use super::*;

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn path(name: &str) -> StorePath {
        StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
    }

    fn root_set(uid: u32) -> RootSet {
        RootSet::new(
            uid,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("hello-1.0"),
            )],
        )
        .unwrap()
    }

    fn scope(uid: u32, mode: RepairMode) -> VerifiedRepairScope {
        VerifiedRepairScope::new(
            uid,
            GenerationId::new("gen-0007").unwrap(),
            [path("hello-1.0")],
            (mode == RepairMode::Build).then(|| body_digest(b"repair plan")),
            PolicyVersion::new(NonZeroU64::new(1).unwrap()),
            mode,
        )
        .unwrap()
    }

    fn client(uid: u32) -> (Arc<InProcessHelper>, CallerMaintenance) {
        let helper = InProcessHelper::new(991).unwrap();
        let session = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        (helper, session.for_caller(uid))
    }

    #[derive(Debug)]
    struct RecordingRepairExecutor {
        calls: AtomicUsize,
        outcomes: Vec<RepairOutcomeKind>,
    }

    impl VerifiedRepairExecutor for RecordingRepairExecutor {
        fn execute(
            &self,
            _scope: &VerifiedRepairScope,
        ) -> Result<Vec<RepairOutcomeKind>, MaintenanceError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcomes.clone())
        }
    }

    #[test]
    fn publication_revalidates_retained_roots_against_durable_source() {
        let source = root_set(1001);
        let added_name = RootName::new("ripgrep-out").unwrap();
        let destination = RootSet::new(
            1001,
            GenerationId::new("gen-0008").unwrap(),
            vec![
                source.entries()[0].clone(),
                RootSetEntry::new(added_name.clone(), path("ripgrep-14.1")),
            ],
        )
        .unwrap();
        let request = RootSetPublicationRequest::new(
            destination,
            Some(source.generation().clone()),
            vec![added_name.clone()],
        )
        .unwrap();
        request.validate_source(Some(&source)).unwrap();

        let tampered = RootSet::new(
            1001,
            GenerationId::new("gen-0008").unwrap(),
            vec![
                RootSetEntry::new(RootName::new("hello-out").unwrap(), path("other-1.0")),
                RootSetEntry::new(added_name.clone(), path("ripgrep-14.1")),
            ],
        )
        .unwrap();
        let tampered = RootSetPublicationRequest::new(
            tampered,
            Some(source.generation().clone()),
            vec![added_name],
        )
        .unwrap();
        assert_eq!(
            tampered.validate_source(Some(&source)).unwrap_err().code(),
            MaintenanceErrorCode::ValidationFailure
        );
    }

    #[test]
    fn root_sets_are_sorted_idempotent_and_caller_bound() {
        let (_, caller) = client(1001);
        let set = root_set(1001);
        let report = caller.publish_root_set(&set).unwrap();
        assert_eq!(report.entry_count(), 1);
        assert_eq!(
            report.reference().as_str(),
            "/nix/var/nix/gcroots/pkg/users/1001/gen-0007"
        );
        assert_eq!(caller.publish_root_set(&set).unwrap(), report);
        let (_, other) = client(1002);
        assert_eq!(
            other.publish_root_set(&set).unwrap_err().code(),
            MaintenanceErrorCode::CapabilityMismatch
        );
    }

    #[test]
    fn root_attestation_is_path_free_restart_safe_and_caller_bound() {
        let (helper, caller) = client(1001);
        let set = root_set(1001);
        let published = caller.publish_root_set(&set).unwrap();
        let request = RootSetAttestationRequest::new(1001, GenerationId::new("gen-0007").unwrap());
        assert_eq!(caller.attest_root_set(&request).unwrap(), published);

        helper.restart().unwrap();
        assert_eq!(
            caller.attest_root_set(&request).unwrap_err().code(),
            MaintenanceErrorCode::SessionRestarted
        );
        let restarted = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(1001);
        assert_eq!(restarted.attest_root_set(&request).unwrap(), published);
        assert_eq!(
            helper
                .connect(InProcessPeer::authenticated_uid(991))
                .unwrap()
                .for_caller(1002)
                .attest_root_set(&request)
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::CapabilityMismatch
        );
    }

    #[test]
    fn root_set_identity_cannot_be_republished_with_different_content() {
        let (_, caller) = client(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let changed = RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("hello-2.0"),
            )],
        )
        .unwrap();
        assert_eq!(
            caller.publish_root_set(&changed).unwrap_err().code(),
            MaintenanceErrorCode::BackendFailure
        );
    }

    #[test]
    fn root_transition_can_only_retain_exact_source_names_and_targets() {
        let source = RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![
                RootSetEntry::new(RootName::new("hello-out").unwrap(), path("hello-1.0")),
                RootSetEntry::new(RootName::new("ripgrep-out").unwrap(), path("ripgrep-14.1")),
            ],
        )
        .unwrap();
        let request = RootSetTransitionRequest::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            GenerationId::new("gen-0008").unwrap(),
            vec![RootName::new("ripgrep-out").unwrap()],
        )
        .unwrap();
        let derived = request.derive_from(&source).unwrap();
        assert_eq!(derived.owner_uid(), 1001);
        assert_eq!(derived.generation().as_str(), "gen-0008");
        assert_eq!(derived.entries().len(), 1);
        assert_eq!(derived.entries()[0].name().as_str(), "ripgrep-out");
        assert_eq!(derived.entries()[0].target(), &path("ripgrep-14.1"));

        let foreign = RootSetTransitionRequest::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            GenerationId::new("gen-0009").unwrap(),
            vec![RootName::new("foreign-out").unwrap()],
        )
        .unwrap();
        assert_eq!(
            foreign.derive_from(&source).unwrap_err().code(),
            MaintenanceErrorCode::ValidationFailure
        );
    }

    #[test]
    fn root_transition_rejects_alias_reuse_empty_and_identity_drift() {
        let source = root_set(1001);
        let same = GenerationId::new("gen-0007").unwrap();
        assert_eq!(
            RootSetTransitionRequest::new(
                1001,
                same.clone(),
                same,
                vec![RootName::new("hello-out").unwrap()],
            )
            .unwrap_err()
            .code(),
            MaintenanceErrorCode::ValidationFailure
        );
        assert_eq!(
            RootSetTransitionRequest::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                Vec::new(),
            )
            .unwrap_err()
            .code(),
            MaintenanceErrorCode::ValidationFailure
        );
        let duplicate = RootName::new("hello-out").unwrap();
        assert_eq!(
            RootSetTransitionRequest::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                vec![duplicate.clone(), duplicate],
            )
            .unwrap_err()
            .code(),
            MaintenanceErrorCode::ValidationFailure
        );
        let wrong_owner = RootSetTransitionRequest::new(
            1002,
            GenerationId::new("gen-0007").unwrap(),
            GenerationId::new("gen-0008").unwrap(),
            vec![RootName::new("hello-out").unwrap()],
        )
        .unwrap();
        assert_eq!(
            wrong_owner.derive_from(&source).unwrap_err().code(),
            MaintenanceErrorCode::ValidationFailure
        );
    }

    #[test]
    fn unauthenticated_peer_and_stale_session_fail_closed() {
        let helper = InProcessHelper::new(991).unwrap();
        assert_eq!(
            helper
                .connect(InProcessPeer::authenticated_uid(992))
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::UnauthenticatedPeer
        );
        let session = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        let caller = session.for_caller(1001);
        helper.restart().unwrap();
        assert_eq!(
            caller.publish_root_set(&root_set(1001)).unwrap_err().code(),
            MaintenanceErrorCode::SessionRestarted
        );
    }

    #[test]
    fn capability_is_single_use_expiring_and_cross_uid_bound() {
        let (_, caller) = client(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::CacheOnly))
            .unwrap();
        let request = RepairStorePathsRequest::new(capability);
        assert_eq!(
            caller
                .repair_store_paths(&request)
                .unwrap()
                .outcomes()
                .len(),
            1
        );
        assert_eq!(
            caller.repair_store_paths(&request).unwrap_err().code(),
            MaintenanceErrorCode::CapabilityReplayed
        );

        let expired = caller
            .issue_with_deadline(
                &scope(1001, RepairMode::CacheOnly),
                Instant::now() - Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            caller
                .repair_store_paths(&RepairStorePathsRequest::new(expired))
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::CapabilityExpired
        );

        let cross_uid = caller
            .issue_repair_capability(&scope(1001, RepairMode::Build))
            .unwrap();
        let other = caller.session.for_caller(1002);
        assert_eq!(
            other
                .repair_store_paths(&RepairStorePathsRequest::new(cross_uid))
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::CapabilityMismatch
        );
    }

    #[test]
    fn validated_scope_is_the_only_input_to_the_repair_executor() {
        let executor = Arc::new(RecordingRepairExecutor {
            calls: AtomicUsize::new(0),
            outcomes: vec![RepairOutcomeKind::CacheMiss],
        });
        let helper = InProcessHelper::with_repair_executor(991, executor.clone()).unwrap();
        let caller = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::CacheOnly))
            .unwrap();
        let report = caller
            .repair_store_paths(&RepairStorePathsRequest::new(capability))
            .unwrap();
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.mode(), RepairMode::CacheOnly);
        assert_eq!(report.outcomes()[0].path(), &path("hello-1.0"));
        assert_eq!(report.outcomes()[0].kind(), RepairOutcomeKind::CacheMiss);
    }

    #[test]
    fn executor_cardinality_mismatch_fails_closed_and_consumes_capability() {
        let executor = Arc::new(RecordingRepairExecutor {
            calls: AtomicUsize::new(0),
            outcomes: Vec::new(),
        });
        let helper = InProcessHelper::with_repair_executor(991, executor.clone()).unwrap();
        let caller = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::Build))
            .unwrap();
        let request = RepairStorePathsRequest::new(capability);
        assert_eq!(
            caller.repair_store_paths(&request).unwrap_err().code(),
            MaintenanceErrorCode::BackendFailure
        );
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            caller.repair_store_paths(&request).unwrap_err().code(),
            MaintenanceErrorCode::CapabilityReplayed
        );
    }

    #[test]
    fn maintenance_capability_debug_is_redacted() {
        let (_, caller) = client(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::CacheOnly))
            .unwrap();
        assert!(!format!("{capability:?}").contains(capability.as_str()));
    }

    #[test]
    fn helper_restart_invalidates_capability_but_preserves_root_set() {
        let (helper, caller) = client(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::Build))
            .unwrap();
        helper.restart().unwrap();
        assert_eq!(
            caller
                .repair_store_paths(&RepairStorePathsRequest::new(capability))
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::SessionRestarted
        );
        let fresh = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(1001);
        let fresh_capability = fresh
            .issue_repair_capability(&scope(1001, RepairMode::Build))
            .unwrap();
        assert!(
            fresh
                .repair_store_paths(&RepairStorePathsRequest::new(fresh_capability))
                .is_ok()
        );
    }

    #[test]
    fn broker_restart_handshake_invalidates_session_and_capability() {
        let (helper, caller) = client(1001);
        caller.publish_root_set(&root_set(1001)).unwrap();
        let capability = caller
            .issue_repair_capability(&scope(1001, RepairMode::CacheOnly))
            .unwrap();
        helper.broker_restarted().unwrap();
        assert_eq!(
            caller
                .repair_store_paths(&RepairStorePathsRequest::new(capability))
                .unwrap_err()
                .code(),
            MaintenanceErrorCode::SessionRestarted
        );
        let fresh = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(1001);
        assert!(
            fresh
                .issue_repair_capability(&scope(1001, RepairMode::CacheOnly))
                .is_ok()
        );
    }

    #[test]
    fn closed_scope_rejects_empty_paths_and_mode_plan_mismatch() {
        assert_eq!(
            VerifiedRepairScope::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                [],
                None,
                PolicyVersion::new(NonZeroU64::new(1).unwrap()),
                RepairMode::CacheOnly,
            )
            .unwrap_err()
            .code(),
            MaintenanceErrorCode::ValidationFailure
        );
        assert!(
            VerifiedRepairScope::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                [path("hello-1.0")],
                None,
                PolicyVersion::new(NonZeroU64::new(1).unwrap()),
                RepairMode::Build,
            )
            .is_err()
        );
    }
}
