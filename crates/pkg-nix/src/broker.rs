//! In-process broker reference for authenticated operation lifecycle and admission.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use crate::maintenance::random_secret;

const OPERATION_TTL: Duration = Duration::from_secs(30 * 60);

/// Stable in-process broker failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerErrorCode {
    /// Claimed caller identity differed from transport-authenticated identity.
    UnauthenticatedCaller,
    /// The session predates the latest broker restart.
    SessionRestarted,
    /// An operation handle was unknown or belonged to another caller.
    InvalidOperationHandle,
    /// The operation handle exceeded its fixed lifetime.
    OperationExpired,
    /// A machine-global admission gate is currently held by another operation.
    AdmissionBusy,
    /// The requested gate transition violated the operation lifecycle.
    InvalidAdmissionTransition,
    /// A managed child policy contained a non-canonical runtime path.
    InvalidChildPolicy,
    /// Fresh session entropy could not be obtained from the operating system.
    EntropyUnavailable,
}

/// Redacted broker boundary error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerError {
    code: BrokerErrorCode,
}

impl BrokerError {
    const fn new(code: BrokerErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> BrokerErrorCode {
        self.code
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "broker request refused: {:?}", self.code)
    }
}

impl std::error::Error for BrokerError {}

/// Closed operation classes accepted by the broker lifecycle layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerOperationKind {
    /// Readiness and ownership diagnosis.
    Doctor,
    /// Signed channel/source/index refresh.
    Refresh,
    /// Catalog resolution and evaluation.
    Resolve,
    /// Substitution-only acquisition.
    Acquire,
    /// Explicitly approved local build.
    Build,
    /// Activation/generation mutation.
    Activate,
    /// Garbage collection.
    Gc,
    /// User-initiated store repair.
    Repair,
}

impl BrokerOperationKind {
    const ALL: [Self; 8] = [
        Self::Doctor,
        Self::Refresh,
        Self::Resolve,
        Self::Acquire,
        Self::Build,
        Self::Activate,
        Self::Gc,
        Self::Repair,
    ];
}

/// Opaque broker-issued operation handle.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationHandle(pub(crate) String);

impl OperationHandle {
    /// Returns the opaque transport token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OperationHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OperationHandle(<opaque>)")
    }
}

/// Publicly observable operation lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    /// The broker is still processing the operation.
    Running,
    /// The operation completed and released all admission.
    Completed,
    /// Cancellation or caller disconnect released all admission.
    Cancelled,
}

#[derive(Debug, Clone)]
struct OperationRecord {
    owner_uid: u32,
    kind: BrokerOperationKind,
    status: OperationStatus,
    expires_at: Instant,
}

#[derive(Debug)]
struct BrokerState {
    epoch: u64,
    secret: [u8; 32],
    next_operation: u64,
    operations: BTreeMap<OperationHandle, OperationRecord>,
    build_holder: Option<OperationHandle>,
    gc_holder: Option<OperationHandle>,
    gc_inhibitors: BTreeSet<OperationHandle>,
}

/// Read-only admission snapshot for tests, diagnostics, and restart handshakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    operation_count: usize,
    build_held: bool,
    gc_held: bool,
    gc_inhibitor_count: usize,
}

impl AdmissionSnapshot {
    /// Returns the count of in-flight or retained terminal operation handles.
    #[must_use]
    pub const fn operation_count(self) -> usize {
        self.operation_count
    }

    /// Returns whether the machine-wide build lease is held.
    #[must_use]
    pub const fn build_held(self) -> bool {
        self.build_held
    }

    /// Returns whether exclusive garbage collection is admitted.
    #[must_use]
    pub const fn gc_held(self) -> bool {
        self.gc_held
    }

    /// Returns the number of shared GC-inhibit permits.
    #[must_use]
    pub const fn gc_inhibitor_count(self) -> usize {
        self.gc_inhibitor_count
    }
}

/// Simulated CLI transport peer with authenticated and claimed uid lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InProcessCallerPeer {
    authenticated_uid: u32,
    claimed_uid: u32,
}

impl InProcessCallerPeer {
    /// Constructs an honest in-process peer.
    #[must_use]
    pub const fn authenticated(uid: u32) -> Self {
        Self {
            authenticated_uid: uid,
            claimed_uid: uid,
        }
    }

    /// Constructs a peer used to prove identity-claim mismatch rejection.
    #[must_use]
    pub const fn with_claim(authenticated_uid: u32, claimed_uid: u32) -> Self {
        Self {
            authenticated_uid,
            claimed_uid,
        }
    }
}

/// In-process reference broker with memory-only operation and admission state.
pub struct InProcessBroker {
    state: Mutex<BrokerState>,
}

impl fmt::Debug for InProcessBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InProcessBroker(<private-state>)")
    }
}

impl InProcessBroker {
    /// Creates a new broker epoch with fresh handle entropy.
    pub fn new() -> Result<Arc<Self>, BrokerError> {
        Ok(Arc::new(Self {
            state: Mutex::new(BrokerState {
                epoch: 1,
                secret: random_secret()
                    .map_err(|_| BrokerError::new(BrokerErrorCode::EntropyUnavailable))?,
                next_operation: 0,
                operations: BTreeMap::new(),
                build_holder: None,
                gc_holder: None,
                gc_inhibitors: BTreeSet::new(),
            }),
        }))
    }

    /// Authenticates the CLI peer and establishes a restart-bound session.
    pub fn connect(
        self: &Arc<Self>,
        peer: InProcessCallerPeer,
    ) -> Result<AuthenticatedCaller, BrokerError> {
        if peer.authenticated_uid != peer.claimed_uid {
            return Err(BrokerError::new(BrokerErrorCode::UnauthenticatedCaller));
        }
        let epoch = self.lock().epoch;
        Ok(AuthenticatedCaller {
            broker: Arc::clone(self),
            epoch,
            uid: peer.authenticated_uid,
        })
    }

    /// Restarts the broker and empties all handles and admission state.
    pub fn restart(&self) -> Result<(), BrokerError> {
        let mut state = self.lock();
        state.epoch = state.epoch.saturating_add(1);
        state.secret =
            random_secret().map_err(|_| BrokerError::new(BrokerErrorCode::EntropyUnavailable))?;
        state.next_operation = 0;
        state.operations.clear();
        state.build_holder = None;
        state.gc_holder = None;
        state.gc_inhibitors.clear();
        Ok(())
    }

    /// Returns a non-authoritative diagnostic snapshot of in-memory gates.
    #[must_use]
    pub fn admission_snapshot(&self) -> AdmissionSnapshot {
        let mut state = self.lock();
        purge_expired(&mut state, Instant::now());
        AdmissionSnapshot {
            operation_count: state.operations.len(),
            build_held: state.build_holder.is_some(),
            gc_held: state.gc_holder.is_some(),
            gc_inhibitor_count: state.gc_inhibitors.len(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Authenticated CLI-to-broker session bound to one uid and broker epoch.
#[derive(Clone)]
pub struct AuthenticatedCaller {
    broker: Arc<InProcessBroker>,
    epoch: u64,
    uid: u32,
}

impl fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticatedCaller(<authenticated-session>)")
    }
}

impl AuthenticatedCaller {
    /// Opens a fresh opaque operation with the fixed broker lifetime.
    pub fn begin(&self, kind: BrokerOperationKind) -> Result<OperationHandle, BrokerError> {
        self.begin_with_deadline(kind, Instant::now() + OPERATION_TTL)
    }

    /// Polls a caller-bound handle without exposing internal operation data.
    pub fn poll(&self, handle: &OperationHandle) -> Result<OperationStatus, BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        let record = self.record_mut(&mut state, handle)?;
        if Instant::now() >= record.expires_at {
            release_admission(&mut state, handle);
            state.operations.remove(handle);
            return Err(BrokerError::new(BrokerErrorCode::OperationExpired));
        }
        Ok(record.status)
    }

    /// Authorizes one typed adapter call against a running caller-owned handle.
    ///
    /// This never executes Nix and accepts no argv or configuration. It only
    /// proves that the operation class may invoke the closed adapter method.
    pub fn authorize_adapter_call(
        &self,
        handle: &OperationHandle,
        method: crate::MethodKind,
    ) -> Result<(), BrokerError> {
        let allowed = match method {
            crate::MethodKind::Version => &BrokerOperationKind::ALL[..],
            crate::MethodKind::EvaluateDerivation => &[BrokerOperationKind::Resolve],
            crate::MethodKind::PathInfo => &[
                BrokerOperationKind::Doctor,
                BrokerOperationKind::Resolve,
                BrokerOperationKind::Acquire,
                BrokerOperationKind::Build,
                BrokerOperationKind::Activate,
                BrokerOperationKind::Gc,
                BrokerOperationKind::Repair,
            ],
            crate::MethodKind::Substitute => &[BrokerOperationKind::Acquire],
            crate::MethodKind::Build => &[BrokerOperationKind::Build],
            crate::MethodKind::Verify => &[
                BrokerOperationKind::Doctor,
                BrokerOperationKind::Acquire,
                BrokerOperationKind::Build,
                BrokerOperationKind::Repair,
            ],
            crate::MethodKind::Gc => &[BrokerOperationKind::Gc],
        };
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, allowed)
    }

    /// Acquires the machine-wide local-build lease for this operation.
    pub fn acquire_build(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(
            &mut state,
            handle,
            &[BrokerOperationKind::Build, BrokerOperationKind::Repair],
        )?;
        if state
            .build_holder
            .as_ref()
            .is_some_and(|holder| holder != handle)
        {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        state.build_holder = Some(handle.clone());
        Ok(())
    }

    /// Acquires one shared permit that prevents garbage collection.
    pub fn acquire_gc_inhibit(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(
            &mut state,
            handle,
            &[
                BrokerOperationKind::Build,
                BrokerOperationKind::Activate,
                BrokerOperationKind::Repair,
            ],
        )?;
        if state.gc_holder.is_some() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        state.gc_inhibitors.insert(handle.clone());
        Ok(())
    }

    /// Acquires exclusive garbage-collection admission.
    pub fn acquire_gc(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running_kind(&mut state, handle, &[BrokerOperationKind::Gc])?;
        if state.gc_holder.is_some() || !state.gc_inhibitors.is_empty() {
            return Err(BrokerError::new(BrokerErrorCode::AdmissionBusy));
        }
        state.gc_holder = Some(handle.clone());
        Ok(())
    }

    /// Completes an operation and releases every admission gate it holds.
    pub fn complete(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        self.finish(handle, OperationStatus::Completed)
    }

    /// Cancels an operation and releases every admission gate it holds.
    pub fn cancel(&self, handle: &OperationHandle) -> Result<(), BrokerError> {
        self.finish(handle, OperationStatus::Cancelled)
    }

    /// Simulates CLI disconnect: every running operation owned by this caller
    /// is cancelled and all of its admission is released.
    pub fn disconnect(&self) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        let handles = state
            .operations
            .iter()
            .filter(|(_, record)| {
                record.owner_uid == self.uid && record.status == OperationStatus::Running
            })
            .map(|(handle, _)| handle.clone())
            .collect::<Vec<_>>();
        for handle in handles {
            release_admission(&mut state, &handle);
            if let Some(record) = state.operations.get_mut(&handle) {
                record.status = OperationStatus::Cancelled;
            }
        }
        Ok(())
    }

    fn begin_with_deadline(
        &self,
        kind: BrokerOperationKind,
        expires_at: Instant,
    ) -> Result<OperationHandle, BrokerError> {
        let mut state = self.broker.lock();
        self.check_epoch(&state)?;
        purge_expired(&mut state, Instant::now());
        state.next_operation = state.next_operation.saturating_add(1);
        let handle = mint_handle(&state, self.uid, kind);
        state.operations.insert(
            handle.clone(),
            OperationRecord {
                owner_uid: self.uid,
                kind,
                status: OperationStatus::Running,
                expires_at,
            },
        );
        Ok(handle)
    }

    fn finish(&self, handle: &OperationHandle, status: OperationStatus) -> Result<(), BrokerError> {
        let mut state = self.broker.lock();
        self.require_running(&mut state, handle)?;
        release_admission(&mut state, handle);
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        record.status = status;
        Ok(())
    }

    fn check_epoch(&self, state: &BrokerState) -> Result<(), BrokerError> {
        if state.epoch == self.epoch {
            Ok(())
        } else {
            Err(BrokerError::new(BrokerErrorCode::SessionRestarted))
        }
    }

    fn record_mut<'a>(
        &self,
        state: &'a mut BrokerState,
        handle: &OperationHandle,
    ) -> Result<&'a mut OperationRecord, BrokerError> {
        let record = state
            .operations
            .get_mut(handle)
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidOperationHandle))?;
        if record.owner_uid != self.uid {
            return Err(BrokerError::new(BrokerErrorCode::InvalidOperationHandle));
        }
        Ok(record)
    }

    fn require_running(
        &self,
        state: &mut BrokerState,
        handle: &OperationHandle,
    ) -> Result<(), BrokerError> {
        self.check_epoch(state)?;
        purge_expired(state, Instant::now());
        let record = self.record_mut(state, handle)?;
        if record.status != OperationStatus::Running {
            return Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ));
        }
        Ok(())
    }

    fn require_running_kind(
        &self,
        state: &mut BrokerState,
        handle: &OperationHandle,
        allowed: &[BrokerOperationKind],
    ) -> Result<(), BrokerError> {
        self.require_running(state, handle)?;
        let record = self.record_mut(state, handle)?;
        if allowed.contains(&record.kind) {
            Ok(())
        } else {
            Err(BrokerError::new(
                BrokerErrorCode::InvalidAdmissionTransition,
            ))
        }
    }
}

fn release_admission(state: &mut BrokerState, handle: &OperationHandle) {
    if state.build_holder.as_ref() == Some(handle) {
        state.build_holder = None;
    }
    if state.gc_holder.as_ref() == Some(handle) {
        state.gc_holder = None;
    }
    state.gc_inhibitors.remove(handle);
}

fn purge_expired(state: &mut BrokerState, now: Instant) {
    let expired = state
        .operations
        .iter()
        .filter(|(_, record)| now >= record.expires_at)
        .map(|(handle, _)| handle.clone())
        .collect::<Vec<_>>();
    for handle in expired {
        release_admission(state, &handle);
        state.operations.remove(&handle);
    }
}

fn mint_handle(state: &BrokerState, uid: u32, kind: BrokerOperationKind) -> OperationHandle {
    let mut hasher = Sha256::new();
    hasher.update(state.secret);
    hasher.update(state.epoch.to_be_bytes());
    hasher.update(state.next_operation.to_be_bytes());
    hasher.update(uid.to_be_bytes());
    hasher.update([kind as u8]);
    let bytes: [u8; 32] = hasher.finalize().into();
    let mut token = String::from("op_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    OperationHandle(token)
}

/// Fixed environment and absolute executable policy for bundled Nix children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildContainmentPolicy {
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    cancel_grace: Duration,
}

impl ChildContainmentPolicy {
    /// Constructs the fixed child policy for one bundled Nix executable.
    pub fn new(
        executable: impl Into<PathBuf>,
        private_home: impl Into<PathBuf>,
    ) -> Result<Self, BrokerError> {
        let executable = executable.into();
        let private_home = private_home.into();
        if !is_managed_nix_executable(&executable) || !is_managed_broker_home(&private_home) {
            return Err(BrokerError::new(BrokerErrorCode::InvalidChildPolicy));
        }
        let private_home = private_home
            .to_str()
            .ok_or_else(|| BrokerError::new(BrokerErrorCode::InvalidChildPolicy))?;
        let environment = BTreeMap::from([
            ("HOME".to_owned(), private_home.to_owned()),
            (
                "NIX_CONFIG".to_owned(),
                "include /opt/pkg/etc/pkg/nix.conf".to_owned(),
            ),
            (
                "NIX_DAEMON_SOCKET_PATH".to_owned(),
                "/nix/var/nix/daemon-socket/socket".to_owned(),
            ),
            ("NIX_REMOTE".to_owned(), "daemon".to_owned()),
            ("NIX_STATE_DIR".to_owned(), "/nix/var/nix".to_owned()),
            ("NIX_USER_CONF_FILES".to_owned(), String::new()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("TMPDIR".to_owned(), format!("{private_home}/tmp")),
        ]);
        Ok(Self {
            executable,
            environment,
            cancel_grace: Duration::from_secs(5),
        })
    }

    /// Returns the absolute bundled executable; callers cannot replace it per operation.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the complete child environment after `env_clear`.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns the fixed SIGTERM-before-SIGKILL grace for the child group.
    #[must_use]
    pub const fn cancel_grace(&self) -> Duration {
        self.cancel_grace
    }

    /// Returns whether launchers must create and terminate a distinct process group.
    #[must_use]
    pub const fn terminate_process_group(&self) -> bool {
        true
    }
}

fn is_managed_broker_home(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some("/var/lib/pkg/broker-home" | "/Library/Application Support/pkg/broker-home")
    )
}

fn is_managed_nix_executable(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => components.push(value),
            _ => return false,
        }
    }
    components.len() == 6
        && components[0] == "opt"
        && components[1] == "pkg"
        && components[2] == "nix"
        && components[3]
            .to_str()
            .is_some_and(|version| crate::NixVersion::new(version).is_ok())
        && components[4] == "bin"
        && components[5] == "nix"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_identity_is_transport_bound() {
        let broker = InProcessBroker::new().unwrap();
        assert_eq!(
            broker
                .connect(InProcessCallerPeer::with_claim(1001, 1002))
                .unwrap_err()
                .code(),
            BrokerErrorCode::UnauthenticatedCaller
        );
        assert!(
            broker
                .connect(InProcessCallerPeer::authenticated(1001))
                .is_ok()
        );
    }

    #[test]
    fn build_and_gc_admission_release_on_cancel_and_disconnect() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let build = caller.begin(BrokerOperationKind::Build).unwrap();
        caller.acquire_build(&build).unwrap();
        caller.acquire_gc_inhibit(&build).unwrap();
        let gc = caller.begin(BrokerOperationKind::Gc).unwrap();
        assert_eq!(
            caller.acquire_gc(&gc).unwrap_err().code(),
            BrokerErrorCode::AdmissionBusy
        );
        caller.cancel(&build).unwrap();
        caller.acquire_gc(&gc).unwrap();
        caller.disconnect().unwrap();
        let snapshot = broker.admission_snapshot();
        assert!(!snapshot.build_held());
        assert!(!snapshot.gc_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
    }

    #[test]
    fn handles_are_uid_bound_expiring_and_restart_bound() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let other = broker
            .connect(InProcessCallerPeer::authenticated(1002))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Repair).unwrap();
        assert_eq!(
            other.poll(&handle).unwrap_err().code(),
            BrokerErrorCode::InvalidOperationHandle
        );
        let expired = caller
            .begin_with_deadline(BrokerOperationKind::Doctor, Instant::now())
            .unwrap();
        assert_eq!(
            caller.poll(&expired).unwrap_err().code(),
            BrokerErrorCode::OperationExpired
        );
        broker.restart().unwrap();
        assert_eq!(
            caller.poll(&handle).unwrap_err().code(),
            BrokerErrorCode::SessionRestarted
        );
        assert_eq!(broker.admission_snapshot().operation_count(), 0);
    }

    #[test]
    fn expiry_releases_every_admission_gate() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let build = caller
            .begin_with_deadline(
                BrokerOperationKind::Build,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
        caller.acquire_build(&build).unwrap();
        caller.acquire_gc_inhibit(&build).unwrap();

        {
            let mut state = broker.lock();
            purge_expired(&mut state, Instant::now() + Duration::from_secs(61));
        }

        let snapshot = broker.admission_snapshot();
        assert_eq!(snapshot.operation_count(), 0);
        assert!(!snapshot.build_held());
        assert!(!snapshot.gc_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
    }

    #[test]
    fn operation_class_cannot_widen_admission_authority() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let doctor = caller.begin(BrokerOperationKind::Doctor).unwrap();
        assert_eq!(
            caller.acquire_build(&doctor).unwrap_err().code(),
            BrokerErrorCode::InvalidAdmissionTransition
        );
        assert_eq!(
            caller.acquire_gc_inhibit(&doctor).unwrap_err().code(),
            BrokerErrorCode::InvalidAdmissionTransition
        );
        assert_eq!(
            caller.acquire_gc(&doctor).unwrap_err().code(),
            BrokerErrorCode::InvalidAdmissionTransition
        );
        assert_eq!(
            caller
                .authorize_adapter_call(&doctor, crate::MethodKind::Build)
                .unwrap_err()
                .code(),
            BrokerErrorCode::InvalidAdmissionTransition
        );
        caller
            .authorize_adapter_call(&doctor, crate::MethodKind::Version)
            .unwrap();

        let build = caller.begin(BrokerOperationKind::Build).unwrap();
        assert_eq!(
            caller.acquire_gc(&build).unwrap_err().code(),
            BrokerErrorCode::InvalidAdmissionTransition
        );
    }

    #[test]
    fn operation_handle_debug_is_redacted() {
        let broker = InProcessBroker::new().unwrap();
        let caller = broker
            .connect(InProcessCallerPeer::authenticated(1001))
            .unwrap();
        let handle = caller.begin(BrokerOperationKind::Doctor).unwrap();
        assert!(!format!("{handle:?}").contains(handle.as_str()));
    }

    #[test]
    fn contained_child_policy_is_absolute_fixed_and_scrubbed() {
        let policy =
            ChildContainmentPolicy::new("/opt/pkg/nix/2.34.8/bin/nix", "/var/lib/pkg/broker-home")
                .unwrap();
        assert_eq!(
            policy.executable(),
            Path::new("/opt/pkg/nix/2.34.8/bin/nix")
        );
        assert_eq!(
            policy.environment().keys().cloned().collect::<Vec<_>>(),
            [
                "HOME",
                "NIX_CONFIG",
                "NIX_DAEMON_SOCKET_PATH",
                "NIX_REMOTE",
                "NIX_STATE_DIR",
                "NIX_USER_CONF_FILES",
                "PATH",
                "TMPDIR",
            ]
        );
        assert_eq!(policy.environment()["HOME"], "/var/lib/pkg/broker-home");
        assert_eq!(
            policy.environment()["TMPDIR"],
            "/var/lib/pkg/broker-home/tmp"
        );
        assert!(policy.environment()["NIX_USER_CONF_FILES"].is_empty());
        for forbidden in ["NIX_PATH", "NIXPKGS_CONFIG", "http_proxy", "SSH_AUTH_SOCK"] {
            assert!(!policy.environment().contains_key(forbidden));
        }
        assert!(ChildContainmentPolicy::new("nix", "/var/lib/pkg/broker-home").is_err());
        assert!(ChildContainmentPolicy::new("/usr/bin/nix", "/var/lib/pkg/broker-home").is_err());
        assert!(
            ChildContainmentPolicy::new("/opt/pkg/nix/../evil/nix", "/var/lib/pkg/broker-home")
                .is_err()
        );
        assert!(ChildContainmentPolicy::new("/opt/pkg/nix/2.34.8/bin/nix", "/var/empty").is_err());
        assert_eq!(policy.cancel_grace(), Duration::from_secs(5));
        assert!(policy.terminate_process_group());
    }
}
