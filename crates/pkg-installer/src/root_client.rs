//! Root-publication and path-free-transition client for the private helper channel.

use crate::{
    HelperTransportError, HelperTransportErrorCode, RepairCoordinatorError, RepairMaintenance,
    platform::peer_uid,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};
use pkg_nix::{
    BrokerHelperRequest, BrokerHelperResponse, Digest, MAX_REPAIR_EXECUTION_DURATION,
    MaintenanceCapability, ProductFrameCodec, RemoveRootSetRequest, RepairStorePathsReport,
    RepairStorePathsRequest, RootSet, RootSetAttestationRequest, RootSetPublicationRequest,
    RootSetReport, RootSetTransitionReport, RootSetTransitionRequest, VerifiedRepairScope,
};
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    fmt,
    io::{self, Read, Write},
    net::Shutdown,
    os::fd::AsFd,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const REQUEST_ID: u64 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const REPAIR_RESPONSE_GRACE: Duration = Duration::from_mins(1);

#[cfg(target_os = "linux")]
const DEFAULT_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
#[cfg(target_os = "macos")]
const DEFAULT_HELPER_SOCKET: &str = "/Library/Application Support/pkg/run/helper/root-helper.sock";

/// Fixed-policy client for durable roots and capability-gated repair.
pub struct RootHelperClient {
    endpoint: Option<PathBuf>,
    expected_uid: u32,
}

impl fmt::Debug for RootHelperClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RootHelperClient(<fixed-private-endpoint>)")
    }
}

impl RootHelperClient {
    /// Constructs the production client for the compiled platform endpoint.
    #[must_use]
    pub fn production() -> Self {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let endpoint = Some(PathBuf::from(default_endpoint()));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let endpoint = None;
        Self {
            endpoint,
            expected_uid: 0,
        }
    }

    #[cfg(test)]
    const fn at(endpoint: PathBuf, expected_uid: u32) -> Self {
        Self {
            endpoint: Some(endpoint),
            expected_uid,
        }
    }

    /// Publishes one complete, validated root set over a fresh authenticated
    /// connection and accepts only its exact correlated response.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error if the fixed endpoint is unavailable,
    /// the peer is not root, bounded I/O fails, or the response is not the exact
    /// root-publication response for this request.
    pub fn publish_root_set(
        &self,
        request: &RootSetPublicationRequest,
    ) -> Result<RootSetReport, HelperTransportError> {
        let mut stream = self.connect()?;
        let request = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::PublishRootSet(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &request, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RootSetPublished(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Requests one path-free root transition over a fresh authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated transition response.
    pub fn transition_root_set(
        &self,
        request: &RootSetTransitionRequest,
    ) -> Result<RootSetTransitionReport, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::TransitionRootSet(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RootSetTransitioned(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Attests one durable root set over a fresh authenticated connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated attestation response.
    pub fn attest_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::AttestRootSet(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RootSetAttested(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Removes one path-free, authenticated generation root set over a fresh
    /// helper connection.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the helper authenticates as
    /// root and returns the exact correlated idempotent removal response.
    pub fn remove_root_set(
        &self,
        request: &RemoveRootSetRequest,
    ) -> Result<(), HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::RemoveRootSet(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RootSetRemoved => Ok(()),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Loads exact durable roots for broker-private repair planning.
    ///
    /// The request is path-free. The returned paths stay on the authenticated
    /// broker-to-helper channel and never cross the public CLI boundary.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn load_repair_root_set(
        &self,
        request: &RootSetAttestationRequest,
    ) -> Result<RootSet, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::LoadRepairRootSet(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RepairRootSetLoaded(root_set) => Ok(root_set),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Verifies the managed runtime against one authenticated manifest digest.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport error unless the root helper returns the
    /// exact correlated ownership response.
    pub fn verify_managed_ownership(&self, digest: Digest) -> Result<bool, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::VerifyManagedOwnership(digest),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::ManagedOwnership(verified) => Ok(verified),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Issues one opaque capability for a broker-derived verified repair scope.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::IssueRepairCapability(scope.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        let deadline = deadline_after(RESPONSE_TIMEOUT)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RepairCapabilityIssued(capability) => Ok(capability),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    /// Redeems one opaque capability for the helper's fixed repair executor.
    ///
    /// # Errors
    ///
    /// Returns a redacted transport, authentication, framing, or helper error.
    pub fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, HelperTransportError> {
        let mut stream = self.connect()?;
        let frame = ProductFrameCodec::encode_helper_request(
            REQUEST_ID,
            &BrokerHelperRequest::RepairStorePaths(request.clone()),
        )
        .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        // The privileged executor owns a bounded process-group deadline. The
        // broker waits beyond that bound so it does not release build or GC
        // admission while the helper can still mutate the store.
        let deadline = deadline_after(repair_response_timeout()?)?;
        write_all_until(&mut stream, &frame, deadline)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let response = read_frame(&mut stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_helper_response(&response)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
        if response_id != REQUEST_ID {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            ));
        }
        match response {
            BrokerHelperResponse::RepairCompleted(report) => Ok(report),
            _ => Err(HelperTransportError::new(
                HelperTransportErrorCode::InvalidFrame,
            )),
        }
    }

    fn connect(&self) -> Result<UnixStream, HelperTransportError> {
        let endpoint = self
            .endpoint
            .as_deref()
            .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let address = SockAddr::unix(endpoint)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        socket
            .connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let stream: UnixStream = socket.into();
        if peer_uid(&stream).map_err(|()| {
            HelperTransportError::new(HelperTransportErrorCode::UnauthenticatedPeer)
        })? != self.expected_uid
        {
            return Err(HelperTransportError::new(
                HelperTransportErrorCode::UnauthenticatedPeer,
            ));
        }
        stream
            .set_nonblocking(true)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        Ok(stream)
    }
}

impl RepairMaintenance for RootHelperClient {
    fn issue_repair_capability(
        &self,
        scope: &VerifiedRepairScope,
    ) -> Result<MaintenanceCapability, RepairCoordinatorError> {
        Self::issue_repair_capability(self, scope)
            .map_err(|_| RepairCoordinatorError::helper_failure())
    }

    fn repair_store_paths(
        &self,
        request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, RepairCoordinatorError> {
        Self::repair_store_paths(self, request)
            .map_err(|_| RepairCoordinatorError::helper_failure())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn default_endpoint() -> &'static Path {
    Path::new(DEFAULT_HELPER_SOCKET)
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, HelperTransportError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    read_exact_until(stream, &mut header, deadline)?;
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
    read_exact_until(stream, &mut frame[FRAME_HEADER_BYTES..], deadline)?;
    Ok(frame)
}

fn write_all_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), HelperTransportError> {
    while !bytes.is_empty() {
        wait_ready(stream, deadline, PollFlags::POLLOUT)?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
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
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
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
        let timeout = PollTimeout::try_from(remaining(deadline)?)
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
        let mut descriptor = [PollFd::new(stream.as_fd(), required)];
        match poll(&mut descriptor, timeout) {
            Ok(0) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
            Ok(_)
                if descriptor[0]
                    .revents()
                    .is_some_and(|events| events.contains(required)) =>
            {
                return Ok(());
            }
            Err(Errno::EINTR) => {}
            Ok(_) | Err(_) => {
                return Err(HelperTransportError::new(
                    HelperTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
}

fn deadline_after(timeout: Duration) -> Result<Instant, HelperTransportError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
}

fn repair_response_timeout() -> Result<Duration, HelperTransportError> {
    MAX_REPAIR_EXECUTION_DURATION
        .checked_add(REPAIR_RESPONSE_GRACE)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
}

fn remaining(deadline: Instant) -> Result<Duration, HelperTransportError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
    let milliseconds = u64::try_from(remaining.as_millis())
        .ok()
        .filter(|milliseconds| *milliseconds != 0)
        .ok_or_else(|| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{BrokerHelperDispatch, serve_helper_connection};
    use nix::unistd::Uid;
    use pkg_nix::{
        AuthenticatedHelper, GenerationId, InProcessHelper, InProcessPeer, MaintenanceAdapter,
        MaintenanceError, RootName, RootSetAttestationRequest, RootSetEntry,
        RootSetTransitionRequest, StorePath,
    };
    use std::{error::Error, os::unix::net::UnixListener, thread};
    use tempfile::TempDir;

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    struct RootDispatch(AuthenticatedHelper);

    impl BrokerHelperDispatch for RootDispatch {
        fn dispatch(
            &self,
            request: BrokerHelperRequest,
        ) -> Result<BrokerHelperResponse, MaintenanceError> {
            match request {
                BrokerHelperRequest::PublishRootSet(request) => self
                    .0
                    .for_caller(request.root_set().owner_uid())
                    .publish_root_set(request.root_set())
                    .map(BrokerHelperResponse::RootSetPublished),
                BrokerHelperRequest::TransitionRootSet(request) => {
                    let derived = request.derive_from(&root_set())?;
                    let report = self
                        .0
                        .for_caller(derived.owner_uid())
                        .publish_root_set(&derived)?;
                    let mapping_digest = derived.mapping_digest();
                    RootSetTransitionReport::new(
                        report,
                        request.retained_names().to_vec(),
                        mapping_digest,
                    )
                    .map(BrokerHelperResponse::RootSetTransitioned)
                }
                BrokerHelperRequest::RemoveRootSet(request) => {
                    self.0
                        .for_caller(request.owner_uid())
                        .remove_root_set(&request)?;
                    Ok(BrokerHelperResponse::RootSetRemoved)
                }
                BrokerHelperRequest::AttestRootSet(request) => self
                    .0
                    .for_caller(request.owner_uid())
                    .attest_root_set(&request)
                    .map(BrokerHelperResponse::RootSetAttested),
                _ => Err(MaintenanceError::backend_failure()),
            }
        }
    }

    fn root_set() -> RootSet {
        RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
            )],
        )
        .unwrap()
    }

    fn publication() -> RootSetPublicationRequest {
        let root_set = root_set();
        let added_names = root_set
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect();
        RootSetPublicationRequest::new(root_set, None, added_names).unwrap()
    }

    #[test]
    fn fixed_client_authenticates_peer_and_round_trips_only_root_publication()
    -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);

        let report = client.publish_root_set(&publication());
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        let report = report?;
        assert_eq!(report.entry_count(), 1);
        assert!(report.reference().as_str().ends_with("/1001/gen-0007"));
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_transition() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RootSetTransitionRequest::new(
            1001,
            GenerationId::new("gen-0007")?,
            GenerationId::new("gen-0008")?,
            vec![RootName::new("hello-out")?],
        )?;

        let report = client.transition_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        let report = report?;
        assert_eq!(report.root_set().entry_count(), 1);
        assert_eq!(report.retained_names()[0].as_str(), "hello-out");
        assert!(
            report
                .root_set()
                .reference()
                .as_str()
                .ends_with("/1001/gen-0008")
        );
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_attestation() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        let expected = authenticated
            .for_caller(1001)
            .publish_root_set(&root_set())?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RootSetAttestationRequest::new(1001, GenerationId::new("gen-0007")?);

        let report = client.attest_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        assert_eq!(report?, expected);
        Ok(())
    }

    #[test]
    fn fixed_client_round_trips_only_path_free_root_removal() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let broker_uid = Uid::effective().as_raw();
        let helper = InProcessHelper::new(broker_uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(broker_uid))?;
        authenticated
            .for_caller(1001)
            .publish_root_set(&root_set())?;
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            serve_helper_connection(stream, broker_uid, &RootDispatch(authenticated))
        });
        let client = RootHelperClient::at(endpoint, broker_uid);
        let request = RemoveRootSetRequest::new(1001, GenerationId::new("gen-0007")?);

        let removed = client.remove_root_set(&request);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        assert_eq!(removed, Ok(()));
        Ok(())
    }

    #[test]
    fn wrong_response_kind_is_rejected_after_exact_request() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let helper_uid = Uid::effective().as_raw();
        let expected = publication();
        let worker = thread::spawn(move || -> Result<(), HelperTransportError> {
            let (mut stream, _) = listener.accept().map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            let mut header = [0_u8; FRAME_HEADER_BYTES];
            stream.read_exact(&mut header).map_err(|_| {
                HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
            })?;
            let payload_length =
                u32::from_be_bytes(header[16..20].try_into().map_err(|_| {
                    HelperTransportError::new(HelperTransportErrorCode::InvalidFrame)
                })?) as usize;
            let mut request = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
            request.extend_from_slice(&header);
            request.resize(FRAME_HEADER_BYTES + payload_length, 0);
            stream
                .read_exact(&mut request[FRAME_HEADER_BYTES..])
                .map_err(|_| {
                    HelperTransportError::new(HelperTransportErrorCode::TransportFailure)
                })?;
            assert_eq!(
                ProductFrameCodec::decode_helper_request(&request),
                Ok((REQUEST_ID, BrokerHelperRequest::PublishRootSet(expected)))
            );
            let response = ProductFrameCodec::encode_helper_response(
                REQUEST_ID,
                &BrokerHelperResponse::RootSetRemoved,
            )
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::InvalidFrame))?;
            stream
                .write_all(&response)
                .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::TransportFailure))
        });
        let client = RootHelperClient::at(endpoint, helper_uid);

        let result = client
            .publish_root_set(&publication())
            .map_err(HelperTransportError::code);
        let served = worker
            .join()
            .map_err(|_| HelperTransportError::new(HelperTransportErrorCode::HelperFailure))?;
        assert_eq!(served.map_err(HelperTransportError::code), Ok(()));
        assert_eq!(result, Err(HelperTransportErrorCode::InvalidFrame));
        Ok(())
    }

    #[test]
    fn non_root_helper_peer_is_rejected_before_request_bytes() -> Result<(), Box<dyn Error>> {
        if Uid::effective().is_root() {
            return Ok(());
        }
        let temporary = TempDir::new()?;
        let endpoint = temporary.path().join("root-helper.sock");
        let listener = UnixListener::bind(&endpoint)?;
        let worker = thread::spawn(move || listener.accept().map(|_| ()));
        let client = RootHelperClient::at(endpoint, 0);

        assert_eq!(
            client
                .publish_root_set(&publication())
                .map_err(HelperTransportError::code),
            Err(HelperTransportErrorCode::UnauthenticatedPeer)
        );
        worker
            .join()
            .map_err(|_| io::Error::other("peer worker panicked"))??;
        Ok(())
    }

    #[test]
    fn debug_never_exposes_the_private_endpoint() {
        let client = RootHelperClient::at(PathBuf::from("/private/secret.sock"), 0);
        assert_eq!(
            format!("{client:?}"),
            "RootHelperClient(<fixed-private-endpoint>)"
        );
    }
}
