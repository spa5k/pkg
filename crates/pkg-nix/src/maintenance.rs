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
}

/// Closed request to remove one service-resolved generation root set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveRootSetRequest {
    owner_uid: u32,
    generation: GenerationId,
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
}

impl RootSetReport {
    pub(crate) fn new(reference: RootRef, entry_count: usize) -> Self {
        Self {
            reference,
            entry_count,
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
    pub(crate) fn new(path: StorePath, kind: RepairOutcomeKind) -> Self {
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
    pub(crate) fn new(mode: RepairMode, outcomes: Vec<RepairPathOutcome>) -> Self {
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

/// Privileged object-safe helper boundary with exactly three operations.
pub trait MaintenanceAdapter: Send + Sync {
    /// Atomically publishes one complete generation root set.
    fn publish_root_set(&self, root_set: &RootSet) -> Result<RootSetReport, MaintenanceError>;

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

    fn check_epoch(&self, state: &HelperState) -> Result<(), MaintenanceError> {
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

    fn check_caller(&self, uid: u32) -> Result<(), MaintenanceError> {
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
        Ok(RootSetReport::new(reference, root_set.entries.len()))
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
