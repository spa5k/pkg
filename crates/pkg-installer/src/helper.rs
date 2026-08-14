//! Authenticated Unix broker-to-helper framed transport.

use crate::platform::{authenticate_broker, linux::LinuxRootSetStore};
use nix::unistd::{Uid, User};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};
use pkg_nix::{
    AuthenticatedHelper, BrokerHelperRequest, BrokerHelperResponse, CallerMaintenance,
    MaintenanceAdapter, MaintenanceCapability, MaintenanceError, MaintenanceErrorCode,
    ProductFrameCodec, RemoveRootSetRequest, RepairStorePathsRequest, RootSetAttestationRequest,
    RootSetPublicationRequest, RootSetTransitionRequest, VerifiedRepairScope,
};
use pkg_store::{StateLayout, authorize_generation_root_removal};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::fd::AsFd,
    os::unix::net::UnixStream,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(30);
const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable transport/dispatch failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperTransportErrorCode {
    /// Kernel peer authentication failed before reading the frame.
    UnauthenticatedPeer,
    /// The connection ended early or failed bounded I/O.
    TransportFailure,
    /// The fixed frame or strict body was invalid.
    InvalidFrame,
    /// The authenticated closed helper operation failed.
    HelperFailure,
}

/// Redacted helper transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelperTransportError {
    code: HelperTransportErrorCode,
}

impl HelperTransportError {
    pub(crate) const fn new(code: HelperTransportErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> HelperTransportErrorCode {
        self.code
    }
}

impl fmt::Display for HelperTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("privileged helper transport failed")
    }
}

impl Error for HelperTransportError {}

/// Closed dispatch seam consumed by the Linux socket transport.
pub trait BrokerHelperDispatch: Send + Sync {
    /// Dispatches one already authenticated, strictly decoded request.
    ///
    /// # Errors
    ///
    /// Returns a closed maintenance error when authorization, filesystem, or
    /// capability validation rejects the request.
    fn dispatch(
        &self,
        request: BrokerHelperRequest,
    ) -> Result<BrokerHelperResponse, MaintenanceError>;
}

/// Filesystem-backed Linux helper session using PR-39 capability state.
pub struct LinuxHelperSession {
    authenticated: AuthenticatedHelper,
    roots: LinuxRootSetStore,
    root_transactions: Mutex<()>,
    capability_owners: Mutex<BTreeMap<MaintenanceCapability, u32>>,
    authorize_removal: fn(&RemoveRootSetRequest) -> Result<(), MaintenanceError>,
}

impl fmt::Debug for LinuxHelperSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinuxHelperSession(<authenticated-private-state>)")
    }
}

impl LinuxHelperSession {
    /// Binds an authenticated PR-39 helper session to the real root filesystem.
    #[must_use]
    pub const fn new(authenticated: AuthenticatedHelper, roots: LinuxRootSetStore) -> Self {
        Self {
            authenticated,
            roots,
            root_transactions: Mutex::new(()),
            capability_owners: Mutex::new(BTreeMap::new()),
            authorize_removal: authorize_production_removal,
        }
    }

    #[cfg(test)]
    fn new_for_test(authenticated: AuthenticatedHelper, roots: LinuxRootSetStore) -> Self {
        Self {
            authenticated,
            roots,
            root_transactions: Mutex::new(()),
            capability_owners: Mutex::new(BTreeMap::new()),
            authorize_removal: |_| Ok(()),
        }
    }

    fn caller(&self, uid: u32) -> CallerMaintenance {
        self.authenticated.for_caller(uid)
    }

    fn publish(
        &self,
        request: &RootSetPublicationRequest,
    ) -> Result<pkg_nix::RootSetReport, MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        let source = request
            .source_generation()
            .map(|generation| {
                self.roots
                    .load(request.root_set().owner_uid(), generation)
                    .map_err(|_| platform_failure())
            })
            .transpose()?;
        request.validate_source(source.as_ref())?;
        let root_set = request.root_set();
        let caller = self.caller(root_set.owner_uid());
        let report = caller.publish_root_set(root_set)?;
        if self.roots.publish(root_set).is_err() {
            let request =
                RemoveRootSetRequest::new(root_set.owner_uid(), root_set.generation().clone());
            let _ = caller.remove_root_set(&request);
            return Err(platform_failure());
        }
        Ok(report)
    }

    fn remove(&self, request: &RemoveRootSetRequest) -> Result<(), MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        (self.authorize_removal)(request)?;
        self.caller(request.owner_uid()).remove_root_set(request)?;
        self.roots.remove(request).map_err(|_| platform_failure())
    }

    fn transition(
        &self,
        request: &RootSetTransitionRequest,
    ) -> Result<pkg_nix::RootSetTransitionReport, MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        let source = self
            .roots
            .load(request.owner_uid(), request.source_generation())
            .map_err(|_| platform_failure())?;
        let destination = request.derive_from(&source)?;
        let mapping_digest = destination.mapping_digest();
        let caller = self.caller(request.owner_uid());
        match self
            .roots
            .load_optional(request.owner_uid(), request.destination_generation())
            .map_err(|_| platform_failure())?
        {
            Some(existing) if existing == destination => {
                let report = caller.publish_root_set(&destination)?;
                return pkg_nix::RootSetTransitionReport::new(
                    report,
                    request.retained_names().to_vec(),
                    mapping_digest,
                );
            }
            Some(_) => return Err(platform_failure()),
            None => {}
        }
        let report = caller.publish_root_set(&destination)?;
        if self.roots.publish(&destination).is_err() {
            let removal = RemoveRootSetRequest::new(
                request.owner_uid(),
                request.destination_generation().clone(),
            );
            let _ = caller.remove_root_set(&removal);
            return Err(platform_failure());
        }
        pkg_nix::RootSetTransitionReport::new(
            report,
            request.retained_names().to_vec(),
            mapping_digest,
        )
    }

    fn attest(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<pkg_nix::RootSetReport, MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        let durable = self
            .roots
            .load(request.owner_uid(), request.generation())
            .map_err(|_| platform_failure())?;
        self.caller(request.owner_uid()).publish_root_set(&durable)
    }

    fn load_repair_roots(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<pkg_nix::RootSet, MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        self.roots
            .load(request.owner_uid(), request.generation())
            .map_err(|_| platform_failure())
    }

    fn issue(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, MaintenanceError> {
        let _transaction = lock_recover(&self.root_transactions);
        let caller = self.caller(scope.owner_uid());
        let capability = match caller.issue_repair_capability(scope) {
            Ok(capability) => capability,
            Err(error) if error.code() == MaintenanceErrorCode::GenerationNotRooted => {
                let durable = self
                    .roots
                    .load(scope.owner_uid(), scope.generation())
                    .map_err(|_| platform_failure())?;
                caller.publish_root_set(&durable)?;
                caller.issue_repair_capability(scope)?
            }
            Err(error) => return Err(error),
        };
        lock_recover(&self.capability_owners).insert(capability.clone(), scope.owner_uid());
        Ok(capability)
    }

    fn repair(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<pkg_nix::RepairStorePathsReport, MaintenanceError> {
        let owner_uid = lock_recover(&self.capability_owners)
            .remove(request.capability())
            .ok_or_else(platform_failure)?;
        self.caller(owner_uid).repair_store_paths(request)
    }
}

fn authorize_production_removal(request: &RemoveRootSetRequest) -> Result<(), MaintenanceError> {
    let user = User::from_uid(Uid::from_raw(request.owner_uid()))
        .map_err(|_| platform_failure())?
        .ok_or_else(platform_failure)?;
    let home = user.dir;
    if !home.is_absolute() {
        return Err(platform_failure());
    }
    let state_root = match std::env::consts::OS {
        "linux" => home.join(".local/share/pkg"),
        "macos" => home.join("Library/Application Support/pkg"),
        _ => return Err(platform_failure()),
    };
    let layout = StateLayout::open(&home, &state_root, request.owner_uid())
        .map_err(|_| platform_failure())?;
    authorize_generation_root_removal(&layout, request.generation()).map_err(|_| platform_failure())
}

impl BrokerHelperDispatch for LinuxHelperSession {
    fn dispatch(
        &self,
        request: BrokerHelperRequest,
    ) -> Result<BrokerHelperResponse, MaintenanceError> {
        match request {
            BrokerHelperRequest::PublishRootSet(request) => self
                .publish(&request)
                .map(BrokerHelperResponse::RootSetPublished),
            BrokerHelperRequest::RemoveRootSet(request) => {
                self.remove(&request)?;
                Ok(BrokerHelperResponse::RootSetRemoved)
            }
            BrokerHelperRequest::IssueRepairCapability(scope) => self
                .issue(&scope)
                .map(BrokerHelperResponse::RepairCapabilityIssued),
            BrokerHelperRequest::RepairStorePaths(request) => self
                .repair(&request)
                .map(BrokerHelperResponse::RepairCompleted),
            BrokerHelperRequest::TransitionRootSet(request) => self
                .transition(&request)
                .map(BrokerHelperResponse::RootSetTransitioned),
            BrokerHelperRequest::AttestRootSet(request) => self
                .attest(&request)
                .map(BrokerHelperResponse::RootSetAttested),
            BrokerHelperRequest::LoadRepairRootSet(request) => self
                .load_repair_roots(&request)
                .map(BrokerHelperResponse::RepairRootSetLoaded),
        }
    }
}

/// Authenticates first, then reads and dispatches exactly one bounded frame.
///
/// # Errors
///
/// Returns a redacted error when peer authentication, bounded transport I/O,
/// strict frame decoding, closed dispatch, or response encoding fails.
pub fn serve_helper_connection(
    stream: UnixStream,
    broker_uid: u32,
    dispatcher: &dyn BrokerHelperDispatch,
) -> Result<(), HelperTransportError> {
    serve_helper_connection_with_timeouts(
        stream,
        broker_uid,
        dispatcher,
        FRAME_READ_TIMEOUT,
        FRAME_WRITE_TIMEOUT,
    )
}

fn serve_helper_connection_with_timeouts(
    mut stream: UnixStream,
    broker_uid: u32,
    dispatcher: &dyn BrokerHelperDispatch,
    read_timeout: Duration,
    write_timeout: Duration,
) -> Result<(), HelperTransportError> {
    authenticate_broker(&stream, broker_uid)
        .map_err(|()| HelperTransportError::new(HelperTransportErrorCode::UnauthenticatedPeer))?;
    stream
        .set_nonblocking(true)
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;

    let read_deadline = deadline_after(read_timeout)?;
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    read_exact_until(&mut stream, &mut header, read_deadline)?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(HelperTransportError::new(
            HelperTransportErrorCode::InvalidFrame,
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
    frame.extend_from_slice(&header);
    frame.resize(FRAME_HEADER_BYTES + payload_length, 0);
    read_exact_until(&mut stream, &mut frame[FRAME_HEADER_BYTES..], read_deadline)?;
    let (request_id, request) = ProductFrameCodec::decode_helper_request(&frame)
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
    let response = dispatcher
        .dispatch(request)
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
    let encoded = ProductFrameCodec::encode_helper_response(request_id, &response)
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
    let write_deadline = deadline_after(write_timeout)?;
    write_all_until(&mut stream, &encoded, write_deadline)
}

fn write_all_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), HelperTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLOUT)?;
        match stream.write(bytes) {
            Ok(0) => return Err(transport_failure()),
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return Err(transport_failure()),
        }
    }
    Ok(())
}

fn read_exact_until(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), HelperTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLIN)?;
        match stream.read(bytes) {
            Ok(0) => return Err(transport_failure()),
            Ok(read) => bytes = &mut bytes[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return Err(transport_failure()),
        }
    }
    Ok(())
}

fn wait_ready(
    stream: &UnixStream,
    deadline: Instant,
    required: PollFlags,
) -> Result<(), HelperTransportError> {
    loop {
        let timeout =
            PollTimeout::try_from(remaining(deadline)?).map_err(|_| transport_failure())?;
        let mut descriptor = [PollFd::new(stream.as_fd(), required)];
        match poll(&mut descriptor, timeout) {
            Ok(0) => return Err(transport_failure()),
            Ok(_)
                if descriptor[0]
                    .revents()
                    .is_some_and(|events| events.contains(required)) =>
            {
                return Ok(());
            }
            Err(Errno::EINTR) => {}
            Ok(_) | Err(_) => return Err(transport_failure()),
        }
    }
}

fn deadline_after(timeout: Duration) -> Result<Instant, HelperTransportError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(transport_failure)
}

fn remaining(deadline: Instant) -> Result<Duration, HelperTransportError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(transport_failure)?;
    let milliseconds = u64::try_from(remaining.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds != 0)
        .ok_or_else(transport_failure)?;
    Ok(Duration::from_millis(milliseconds))
}

const fn transport_failure() -> HelperTransportError {
    HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const fn platform_failure() -> MaintenanceError {
    // The public maintenance contract intentionally exposes only a stable,
    // redacted backend class; filesystem details stay in the root service log.
    MaintenanceError::backend_failure()
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::platform::linux::LinuxRootSetStore;
    use nix::unistd::Uid;
    use pkg_nix::{
        GenerationId, InProcessHelper, InProcessPeer, PolicyVersion, RepairMode, RootName, RootSet,
        RootSetEntry, StorePath, VerifiedRepairScope,
    };
    use std::{
        io,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
        thread,
    };

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("pkg-helper-{}-{sequence}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn roots() -> Result<RootSet, Box<dyn Error>> {
        Ok(RootSet::new(
            501,
            GenerationId::new("gen-0003")?,
            vec![RootSetEntry::new(
                RootName::new("out")?,
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?,
            )],
        )?)
    }

    fn publication(root_set: RootSet) -> Result<RootSetPublicationRequest, Box<dyn Error>> {
        let added_names = root_set
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect();
        Ok(RootSetPublicationRequest::new(root_set, None, added_names)?)
    }

    fn transition_source() -> Result<RootSet, Box<dyn Error>> {
        Ok(RootSet::new(
            501,
            GenerationId::new("gen-0003")?,
            vec![
                RootSetEntry::new(
                    RootName::new("hello-out")?,
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?,
                ),
                RootSetEntry::new(
                    RootName::new("ripgrep-out")?,
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-ripgrep"))?,
                ),
            ],
        )?)
    }

    fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        stream.read_exact(&mut header)?;
        let length = u32::from_be_bytes(
            header[16..20]
                .try_into()
                .map_err(|_| io::Error::other("invalid frame header"))?,
        ) as usize;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + length);
        frame.extend_from_slice(&header);
        frame.resize(FRAME_HEADER_BYTES + length, 0);
        stream.read_exact(&mut frame[FRAME_HEADER_BYTES..])?;
        Ok(frame)
    }

    #[test]
    fn authenticated_frame_publishes_real_atomic_root_set() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let dispatcher = Arc::new(LinuxHelperSession::new_for_test(authenticated, root_store));
        let request_roots = roots()?;
        let encoded = ProductFrameCodec::encode_helper_request(
            7,
            &BrokerHelperRequest::PublishRootSet(publication(request_roots)?),
        )?;
        let (server, mut client) = UnixStream::pair()?;
        let server_dispatcher = Arc::clone(&dispatcher);
        let worker = thread::spawn(move || {
            serve_helper_connection(server, broker_uid, server_dispatcher.as_ref())
        });
        client.write_all(&encoded)?;
        let response = read_frame(&mut client)?;
        let (request_id, response) = ProductFrameCodec::decode_helper_response(&response)?;
        assert_eq!(request_id, 7);
        assert!(matches!(
            response,
            BrokerHelperResponse::RootSetPublished(_)
        ));
        let worker_result = worker
            .join()
            .map_err(|_| io::Error::other("helper thread panicked"))?;
        worker_result?;
        assert!(
            scratch
                .0
                .join("501/gen-0003/out")
                .symlink_metadata()?
                .file_type()
                .is_symlink()
        );
        Ok(())
    }

    struct CountingDispatch(AtomicUsize);

    impl BrokerHelperDispatch for CountingDispatch {
        fn dispatch(
            &self,
            _request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(BrokerHelperResponse::RootSetRemoved)
        }
    }

    #[test]
    fn wrong_peer_is_rejected_before_any_frame_dispatch() -> Result<(), Box<dyn Error>> {
        let broker_uid = Uid::current().as_raw().saturating_add(1);
        let dispatcher = CountingDispatch(AtomicUsize::new(0));
        let (server, _client) = UnixStream::pair()?;
        let result = serve_helper_connection(server, broker_uid, &dispatcher);
        assert_eq!(
            result.map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::UnauthenticatedPeer)
        );
        assert_eq!(dispatcher.0.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn oversized_header_is_rejected_before_allocation_or_dispatch() -> Result<(), Box<dyn Error>> {
        let broker_uid = Uid::current().as_raw();
        let dispatcher = Arc::new(CountingDispatch(AtomicUsize::new(0)));
        let (server, mut client) = UnixStream::pair()?;
        let server_dispatcher = Arc::clone(&dispatcher);
        let worker = thread::spawn(move || {
            serve_helper_connection(server, broker_uid, server_dispatcher.as_ref())
        });
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        let oversized_length = u32::try_from(MAX_FRAME_PAYLOAD_BYTES)?.saturating_add(1);
        header[16..20].copy_from_slice(&oversized_length.to_be_bytes());
        client.write_all(&header)?;
        let result = worker
            .join()
            .map_err(|_| io::Error::other("helper thread panicked"))?;
        assert_eq!(
            result.map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::InvalidFrame)
        );
        assert_eq!(dispatcher.0.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn stalled_authenticated_peer_expires_before_dispatch() -> Result<(), Box<dyn Error>> {
        let broker_uid = Uid::current().as_raw();
        let dispatcher = Arc::new(CountingDispatch(AtomicUsize::new(0)));
        let (server, _client) = UnixStream::pair()?;
        let server_dispatcher = Arc::clone(&dispatcher);
        let worker = thread::spawn(move || {
            serve_helper_connection_with_timeouts(
                server,
                broker_uid,
                server_dispatcher.as_ref(),
                Duration::from_millis(50),
                Duration::from_secs(1),
            )
        });

        let result = worker
            .join()
            .map_err(|_| io::Error::other("helper thread panicked"))?;
        assert_eq!(
            result.map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::TransportFailure)
        );
        assert_eq!(dispatcher.0.load(Ordering::Relaxed), 0);
        Ok(())
    }

    struct DelayedDispatch(Duration);

    impl BrokerHelperDispatch for DelayedDispatch {
        fn dispatch(
            &self,
            _request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            thread::sleep(self.0);
            Ok(BrokerHelperResponse::RootSetRemoved)
        }
    }

    #[test]
    fn dispatch_time_is_excluded_from_response_write_budget() -> Result<(), Box<dyn Error>> {
        let broker_uid = Uid::current().as_raw();
        let encoded = ProductFrameCodec::encode_helper_request(
            11,
            &BrokerHelperRequest::PublishRootSet(publication(roots()?)?),
        )?;
        let (server, mut client) = UnixStream::pair()?;
        let worker = thread::spawn(move || {
            serve_helper_connection_with_timeouts(
                server,
                broker_uid,
                &DelayedDispatch(Duration::from_millis(150)),
                Duration::from_secs(1),
                Duration::from_millis(75),
            )
        });

        client.write_all(&encoded)?;
        let response = read_frame(&mut client)?;
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&response),
            Ok((11, BrokerHelperResponse::RootSetRemoved))
        );
        worker
            .join()
            .map_err(|_| io::Error::other("helper thread panicked"))??;
        Ok(())
    }

    #[test]
    fn response_write_expires_when_peer_stops_reading() -> Result<(), Box<dyn Error>> {
        let (mut server, _client) = UnixStream::pair()?;
        socket2::SockRef::from(&server).set_send_buffer_size(4096)?;
        server.set_nonblocking(true)?;
        let bytes = vec![0_u8; MAX_FRAME_PAYLOAD_BYTES];
        assert_eq!(
            write_all_until(
                &mut server,
                &bytes,
                deadline_after(Duration::from_millis(50))?,
            )
            .map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::TransportFailure)
        );
        Ok(())
    }

    #[test]
    fn helper_restart_reloads_durable_root_before_capability_issue() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let first = LinuxHelperSession::new_for_test(authenticated, root_store.clone());
        let roots = roots()?;
        first.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            roots.clone(),
        )?))?;

        let replacement = InProcessHelper::new(broker_uid)?;
        let authenticated = replacement.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let restarted = LinuxHelperSession::new_for_test(authenticated, root_store);
        let scope = VerifiedRepairScope::new(
            roots.owner_uid(),
            roots.generation().clone(),
            [StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?],
            None,
            PolicyVersion::from_u64(7).ok_or_else(|| io::Error::other("invalid policy fixture"))?,
            RepairMode::CacheOnly,
        )?;
        assert!(matches!(
            restarted.dispatch(BrokerHelperRequest::IssueRepairCapability(scope))?,
            BrokerHelperResponse::RepairCapabilityIssued(_)
        ));
        Ok(())
    }

    #[test]
    fn helper_restart_attests_exact_durable_root_without_path_input() -> Result<(), Box<dyn Error>>
    {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let first = LinuxHelperSession::new_for_test(authenticated, root_store.clone());
        let roots = roots()?;
        let published = first.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            roots.clone(),
        )?))?;
        let BrokerHelperResponse::RootSetPublished(published) = published else {
            return Err(io::Error::other("unexpected helper response").into());
        };

        let replacement = InProcessHelper::new(broker_uid)?;
        let authenticated = replacement.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let restarted = LinuxHelperSession::new_for_test(authenticated, root_store);
        let request = RootSetAttestationRequest::new(roots.owner_uid(), roots.generation().clone());
        let attested = restarted.dispatch(BrokerHelperRequest::AttestRootSet(request))?;
        let BrokerHelperResponse::RootSetAttested(attested) = attested else {
            return Err(io::Error::other("unexpected helper response").into());
        };
        assert_eq!(attested, published);
        Ok(())
    }

    #[test]
    fn root_transition_derives_targets_only_from_durable_source() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let session = LinuxHelperSession::new_for_test(authenticated, root_store.clone());
        let source = transition_source()?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            source.clone(),
        )?))?;

        let request = RootSetTransitionRequest::new(
            source.owner_uid(),
            source.generation().clone(),
            GenerationId::new("gen-0004")?,
            vec![RootName::new("ripgrep-out")?],
        )?;
        let response = session.dispatch(BrokerHelperRequest::TransitionRootSet(request.clone()))?;
        let BrokerHelperResponse::RootSetTransitioned(report) = response else {
            return Err(io::Error::other("unexpected helper response").into());
        };
        assert_eq!(report.root_set().entry_count(), 1);
        assert_eq!(report.retained_names()[0].as_str(), "ripgrep-out");
        assert_eq!(
            root_store.load(source.owner_uid(), source.generation())?,
            source
        );
        let destination = root_store.load(source.owner_uid(), &GenerationId::new("gen-0004")?)?;
        assert_eq!(destination.entries().len(), 1);
        assert_eq!(destination.entries()[0].name().as_str(), "ripgrep-out");
        assert_eq!(
            destination.entries()[0].target().as_str(),
            format!("/nix/store/{STORE_HASH}-ripgrep")
        );
        assert!(matches!(
            session.dispatch(BrokerHelperRequest::TransitionRootSet(request))?,
            BrokerHelperResponse::RootSetTransitioned(_)
        ));

        let occupied_generation = GenerationId::new("gen-0006")?;
        let occupied = RootSet::new(
            source.owner_uid(),
            occupied_generation.clone(),
            vec![RootSetEntry::new(
                RootName::new("existing-out")?,
                StorePath::new(&format!("/nix/store/{STORE_HASH}-existing"))?,
            )],
        )?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            occupied.clone(),
        )?))?;
        let collision = RootSetTransitionRequest::new(
            source.owner_uid(),
            source.generation().clone(),
            occupied_generation.clone(),
            vec![RootName::new("ripgrep-out")?],
        )?;
        assert_eq!(
            session
                .dispatch(BrokerHelperRequest::TransitionRootSet(collision))
                .map_err(MaintenanceError::code),
            Err(MaintenanceErrorCode::BackendFailure)
        );
        assert_eq!(
            root_store.load(source.owner_uid(), &occupied_generation)?,
            occupied
        );

        let unknown = RootSetTransitionRequest::new(
            source.owner_uid(),
            source.generation().clone(),
            GenerationId::new("gen-0005")?,
            vec![RootName::new("foreign-out")?],
        )?;
        assert_eq!(
            session
                .dispatch(BrokerHelperRequest::TransitionRootSet(unknown))
                .map_err(MaintenanceError::code),
            Err(MaintenanceErrorCode::ValidationFailure)
        );
        assert!(
            root_store
                .load(source.owner_uid(), &GenerationId::new("gen-0005")?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn root_publication_revalidates_retained_targets_from_durable_source()
    -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let session = LinuxHelperSession::new_for_test(authenticated, root_store.clone());
        let source = transition_source()?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            source.clone(),
        )?))?;

        let added_name = RootName::new("fd-out")?;
        let extended = RootSet::new(
            source.owner_uid(),
            GenerationId::new("gen-0008")?,
            vec![
                source.entries()[0].clone(),
                RootSetEntry::new(
                    added_name.clone(),
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-fd"))?,
                ),
            ],
        )?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(
            RootSetPublicationRequest::new(
                extended.clone(),
                Some(source.generation().clone()),
                vec![added_name.clone()],
            )?,
        ))?;
        assert_eq!(
            root_store.load(source.owner_uid(), extended.generation())?,
            extended
        );

        let tampered = RootSet::new(
            source.owner_uid(),
            GenerationId::new("gen-0007")?,
            vec![
                RootSetEntry::new(
                    source.entries()[0].name().clone(),
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-tampered"))?,
                ),
                RootSetEntry::new(
                    added_name.clone(),
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-fd"))?,
                ),
            ],
        )?;
        let request = RootSetPublicationRequest::new(
            tampered,
            Some(source.generation().clone()),
            vec![added_name],
        )?;
        assert_eq!(
            session
                .dispatch(BrokerHelperRequest::PublishRootSet(request))
                .map_err(MaintenanceError::code),
            Err(MaintenanceErrorCode::ValidationFailure)
        );
        Ok(())
    }

    #[test]
    fn stale_logical_session_cannot_delete_the_durable_root() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let session = LinuxHelperSession::new_for_test(authenticated, root_store);
        let roots = roots()?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            roots.clone(),
        )?))?;
        helper.restart()?;

        let request = RemoveRootSetRequest::new(roots.owner_uid(), roots.generation().clone());
        let result = session.dispatch(BrokerHelperRequest::RemoveRootSet(request));
        assert_eq!(
            result.map_err(MaintenanceError::code),
            Err(MaintenanceErrorCode::SessionRestarted)
        );
        assert!(
            scratch
                .0
                .join("501/gen-0003/out")
                .symlink_metadata()?
                .file_type()
                .is_symlink()
        );
        Ok(())
    }

    #[test]
    fn concurrent_root_transactions_keep_logical_and_durable_state_consistent()
    -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let broker_uid = Uid::current().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let root_store = LinuxRootSetStore::new_at(scratch.0.clone(), broker_uid)?;
        let session = Arc::new(LinuxHelperSession::new_for_test(
            authenticated,
            root_store.clone(),
        ));
        let roots = roots()?;
        session.dispatch(BrokerHelperRequest::PublishRootSet(publication(
            roots.clone(),
        )?))?;

        let workers = 32_usize;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let worker_session = Arc::clone(&session);
            let worker_barrier = Arc::clone(&barrier);
            let worker_roots = roots.clone();
            handles.push(thread::spawn(move || {
                worker_barrier.wait();
                if index % 2 == 0 {
                    worker_session.dispatch(BrokerHelperRequest::PublishRootSet(
                        publication(worker_roots)
                            .map_err(|_| MaintenanceError::backend_failure())?,
                    ))
                } else {
                    worker_session.dispatch(BrokerHelperRequest::RemoveRootSet(
                        RemoveRootSetRequest::new(
                            worker_roots.owner_uid(),
                            worker_roots.generation().clone(),
                        ),
                    ))
                }
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| io::Error::other("root transaction thread panicked"))??;
        }

        let durable_present = root_store
            .load(roots.owner_uid(), roots.generation())
            .is_ok();
        let scope = VerifiedRepairScope::new(
            roots.owner_uid(),
            roots.generation().clone(),
            [StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?],
            None,
            PolicyVersion::from_u64(7).ok_or_else(|| io::Error::other("invalid policy fixture"))?,
            RepairMode::CacheOnly,
        )?;
        let logical_present = session
            .dispatch(BrokerHelperRequest::IssueRepairCapability(scope))
            .is_ok();
        assert_eq!(logical_present, durable_present);
        Ok(())
    }
}
