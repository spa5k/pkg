//! Fail-closed client for the private broker lifecycle protocol.

use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

use pkg_nix::{
    BrokerOperationKind, CliBrokerRequest, CliBrokerResponse, DerivationPlanReport,
    EvaluateDerivationRequest, GcReport, OperationHandle, OperationStatus, PathInfoReport,
    ProductFrameCodec, StorePath, SubstituteReport, VerifyReport, VerifyRequest, VersionInfo,
};
use socket2::{Domain, SockAddr, Socket, Type};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
const DEFAULT_BROKER_SOCKET: &str = "/run/pkg/broker.sock";
#[cfg(target_os = "macos")]
const DEFAULT_BROKER_SOCKET: &str = "/Library/Application Support/pkg/run/broker/broker.sock";

/// Stable private-broker connector failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerClientErrorCode {
    /// This operating system has no V1 broker endpoint.
    UnsupportedPlatform,
    /// The installed broker endpoint could not be connected.
    Unavailable,
    /// Bounded stream I/O failed or ended unexpectedly.
    TransportFailure,
    /// The peer returned an invalid product frame.
    InvalidFrame,
    /// The response id or response kind did not match the request.
    UnexpectedResponse,
    /// The connection exhausted its nonzero request-id space.
    RequestIdExhausted,
    /// A prior protocol or transport failure made reuse unsafe.
    ConnectionFailed,
}

/// Redacted failure from the private broker connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerClientError {
    code: BrokerClientErrorCode,
}

impl BrokerClientError {
    const fn new(code: BrokerClientErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> BrokerClientErrorCode {
        self.code
    }
}

impl fmt::Display for BrokerClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private package broker failed")
    }
}

impl Error for BrokerClientError {}

/// One authenticated connection to the private broker lifecycle API.
///
/// Caller identity is never serialized. The broker derives it from kernel peer
/// credentials on this Unix stream.
#[derive(Debug)]
pub struct BrokerLifecycleClient {
    stream: UnixStream,
    next_request_id: u64,
    healthy: bool,
}

impl BrokerLifecycleClient {
    /// Connects to the single fixed broker endpoint for the current V1 host.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the platform is unsupported, the endpoint is
    /// unavailable, or finite I/O deadlines cannot be configured.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn connect_default() -> Result<Self, BrokerClientError> {
        Self::connect(Path::new(DEFAULT_BROKER_SOCKET))
    }

    /// Refuses unsupported Unix hosts without accepting an alternate endpoint.
    ///
    /// # Errors
    ///
    /// Always returns `UnsupportedPlatform` outside Linux and macOS.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub const fn connect_default() -> Result<Self, BrokerClientError> {
        Err(BrokerClientError::new(
            BrokerClientErrorCode::UnsupportedPlatform,
        ))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn connect(path: &Path) -> Result<Self, BrokerClientError> {
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        let address = SockAddr::unix(path)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        socket
            .connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::Unavailable))?;
        let stream: UnixStream = socket.into();
        Ok(Self::from_stream(stream))
    }

    const fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            next_request_id: 1,
            healthy: true,
        }
    }

    /// Opens one caller-bound lifecycle operation.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn begin(
        &mut self,
        kind: BrokerOperationKind,
    ) -> Result<OperationHandle, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Begin(kind))? {
            CliBrokerResponse::Started(handle) => Ok(handle),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Reads the sanitized status of one operation handle.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn poll(&mut self, handle: OperationHandle) -> Result<OperationStatus, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Poll(handle))? {
            CliBrokerResponse::Status(status) => Ok(status),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Cancels one operation and waits for its acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// or response-kind failures.
    pub fn cancel(&mut self, handle: OperationHandle) -> Result<(), BrokerClientError> {
        match self.transact(&CliBrokerRequest::Cancel(handle))? {
            CliBrokerResponse::Cancelled => Ok(()),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Queries the validated pinned managed-runtime version under one handle.
    ///
    /// # Errors
    ///
    /// Returns a redacted connector error for framing, transport, correlation,
    /// authorization, adapter, or response-kind failures.
    pub fn version(&mut self, handle: OperationHandle) -> Result<VersionInfo, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Version(handle))? {
            CliBrokerResponse::Version(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Evaluates one closed derivation request under a live resolve handle.
    pub fn evaluate_derivation(
        &mut self,
        handle: OperationHandle,
        request: EvaluateDerivationRequest,
    ) -> Result<DerivationPlanReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::EvaluateDerivation(handle, request))? {
            CliBrokerResponse::DerivationPlan(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Queries validated metadata for one promoted store path.
    pub fn path_info(
        &mut self,
        handle: OperationHandle,
        path: StorePath,
    ) -> Result<PathInfoReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::PathInfo(handle, path))? {
            CliBrokerResponse::PathInfo(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Attempts substitution for one promoted store path.
    pub fn substitute(
        &mut self,
        handle: OperationHandle,
        path: StorePath,
    ) -> Result<SubstituteReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Substitute(handle, path))? {
            CliBrokerResponse::Substitute(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Verifies one validated closed request.
    pub fn verify(
        &mut self,
        handle: OperationHandle,
        request: VerifyRequest,
    ) -> Result<VerifyReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Verify(handle, request))? {
            CliBrokerResponse::Verify(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    /// Collects unreachable paths using only the managed on-disk roots.
    pub fn gc(&mut self, handle: OperationHandle) -> Result<GcReport, BrokerClientError> {
        match self.transact(&CliBrokerRequest::Gc(handle))? {
            CliBrokerResponse::Gc(report) => Ok(report),
            _ => Err(self.fail(BrokerClientErrorCode::UnexpectedResponse)),
        }
    }

    fn transact(
        &mut self,
        request: &CliBrokerRequest,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        if !self.healthy {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::ConnectionFailed,
            ));
        }
        let result = self.transact_healthy(request);
        if result.is_err() {
            self.healthy = false;
        }
        result
    }

    fn transact_healthy(
        &mut self,
        request: &CliBrokerRequest,
    ) -> Result<CliBrokerResponse, BrokerClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::RequestIdExhausted))?;
        let frame = ProductFrameCodec::encode_cli_request(request_id, request)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        let deadline = Instant::now()
            .checked_add(RESPONSE_TIMEOUT)
            .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        write_all_until(&mut self.stream, &frame, deadline)?;
        let response = read_frame(&mut self.stream, deadline)?;
        let (response_id, response) = ProductFrameCodec::decode_cli_response(&response)
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?;
        if response_id != request_id {
            return Err(BrokerClientError::new(
                BrokerClientErrorCode::UnexpectedResponse,
            ));
        }
        Ok(response)
    }

    fn fail(&mut self, code: BrokerClientErrorCode) -> BrokerClientError {
        self.healthy = false;
        BrokerClientError::new(code)
    }
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, BrokerClientError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    read_exact_until(stream, &mut header, deadline)?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(BrokerClientError::new(BrokerClientErrorCode::InvalidFrame));
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
) -> Result<(), BrokerClientError> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
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
) -> Result<(), BrokerClientError> {
    while !bytes.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err(BrokerClientError::new(
                    BrokerClientErrorCode::TransportFailure,
                ));
            }
        }
    }
    Ok(())
}

fn remaining(deadline: Instant) -> Result<Duration, BrokerClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BrokerClientError::new(BrokerClientErrorCode::TransportFailure))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_installer::serve_broker_connection_with_nix;
    use pkg_nix::{
        AcceptedFormats, AttributePath, DerivationPath, Digest, EvaluatedDerivation, FormatVersion,
        GcStatus, InProcessBroker, NarHash, NarIntegrity, NixAdapter, NixVersion, NixpkgsRevision,
        OutputName, OutputSelection, PackageVersion, PathVerifyResult, Signature,
        SubstituteReceipt, System, TrustStatus, VerifyMode, VersionInfo,
    };
    use pkg_testkit::FakeNix;
    use std::{
        collections::BTreeMap,
        fs,
        net::Shutdown,
        path::PathBuf,
        str::FromStr,
        sync::{Arc, mpsc},
        thread,
    };

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const REV: &str = "0123456789abcdef0123456789abcdef01234567";

    fn store_path(name: &str) -> StorePath {
        StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
    }

    fn drv(name: &str) -> DerivationPath {
        DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-{name}.drv")).unwrap()
    }

    fn nar_hash() -> NarHash {
        NarHash::new(NAR).unwrap()
    }

    fn version_info() -> VersionInfo {
        VersionInfo::new(
            NixVersion::new("2.34.8").unwrap(),
            AcceptedFormats::new(FormatVersion::new(1).unwrap()),
        )
    }

    fn eval_request() -> EvaluateDerivationRequest {
        EvaluateDerivationRequest::new(
            AttributePath::new("hello").unwrap(),
            System::X8664Linux,
            NixpkgsRevision::new(REV).unwrap(),
            nar_hash(),
            OutputSelection::default_selection(),
        )
        .unwrap()
    }

    fn derivation_plan() -> DerivationPlanReport {
        let root = drv("hello-1.0");
        let mut outputs = BTreeMap::new();
        outputs.insert(OutputName::new("out").unwrap(), store_path("hello-1.0"));
        let evaluated = EvaluatedDerivation::new(
            root.clone(),
            "hello-1.0".into(),
            System::X8664Linux,
            outputs,
            Digest::from_bytes([1; 32]),
            false,
        )
        .unwrap();
        DerivationPlanReport::new(
            4,
            root,
            vec![OutputName::new("out").unwrap()],
            vec![evaluated],
            Digest::from_bytes([2; 32]),
            "hello".into(),
            PackageVersion::new("1.0"),
        )
        .unwrap()
    }

    fn path_info_report() -> PathInfoReport {
        PathInfoReport::new(
            store_path("hello-1.0"),
            nar_hash(),
            vec![Signature::new("cache:BBBBBBBB").unwrap()],
            vec![],
            Some(drv("hello-1.0")),
            1024,
            4096,
        )
        .unwrap()
    }

    fn substitute_report() -> SubstituteReport {
        SubstituteReport::fetched(
            store_path("hello-1.0"),
            SubstituteReceipt::new(
                "https://cache.nixos.org",
                nar_hash(),
                vec![Signature::new("cache:BBBBBBBB").unwrap()],
            )
            .unwrap(),
        )
    }

    fn verify_request() -> VerifyRequest {
        VerifyRequest::new(vec![store_path("hello-1.0")], VerifyMode::Recursive).unwrap()
    }

    fn verify_report() -> VerifyReport {
        VerifyReport::new(vec![PathVerifyResult::new(
            store_path("hello-1.0"),
            NarIntegrity::Intact,
            TrustStatus::Trusted,
        )])
        .unwrap()
    }

    fn gc_report() -> GcReport {
        GcReport::new(
            GcStatus::Collected,
            vec![store_path("unreachable-1")],
            12_345,
        )
        .unwrap()
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "pkg-broker-client-{}-{:?}",
                std::process::id(),
                thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn real_transport_round_trips_all_exposed_typed_calls_and_cleanup() -> Result<(), Box<dyn Error>>
    {
        let broker = InProcessBroker::new()?;
        let scratch = Scratch::new()?;
        let socket = scratch.0.join("broker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        let server_broker = Arc::clone(&broker);
        let expected_version = version_info();
        let expected_eval_request = eval_request();
        let expected_plan = derivation_plan();
        let expected_path = store_path("hello-1.0");
        let expected_path_info = path_info_report();
        let expected_substitute = substitute_report();
        let expected_verify_request = verify_request();
        let expected_verify = verify_report();
        let expected_gc = gc_report();
        let fake = Arc::new(FakeNix::new());
        fake.expect_version(Ok(expected_version.clone()))
            .expect_evaluate_derivation(expected_eval_request.clone(), Ok(expected_plan.clone()))
            .expect_path_info(expected_path.clone(), Ok(expected_path_info.clone()))
            .expect_substitute(expected_path.clone(), Ok(expected_substitute.clone()))
            .expect_verify(expected_verify_request.clone(), Ok(expected_verify.clone()))
            .expect_gc(Ok(expected_gc.clone()));
        let server_adapter: Arc<dyn NixAdapter> = fake.clone();
        let worker = thread::spawn(move || {
            let (server, _) = listener.accept()?;
            serve_broker_connection_with_nix(server, &server_broker, &server_adapter)
                .map_err(|error| io::Error::other(error.to_string()))
        });
        let mut client = BrokerLifecycleClient::connect(&socket)?;

        let resolve_handle = client.begin(BrokerOperationKind::Resolve)?;
        assert_eq!(client.version(resolve_handle.clone())?, expected_version);
        assert_eq!(
            client.poll(resolve_handle.clone())?,
            OperationStatus::Running
        );
        assert_eq!(
            client.evaluate_derivation(resolve_handle.clone(), expected_eval_request)?,
            expected_plan
        );
        assert_eq!(
            client.path_info(resolve_handle.clone(), expected_path.clone())?,
            expected_path_info
        );
        client.cancel(resolve_handle)?;

        let acquire_handle = client.begin(BrokerOperationKind::Acquire)?;
        assert_eq!(
            client.substitute(acquire_handle.clone(), expected_path)?,
            expected_substitute
        );
        assert_eq!(
            client.verify(acquire_handle.clone(), expected_verify_request)?,
            expected_verify
        );
        client.cancel(acquire_handle)?;

        let gc_handle = client.begin(BrokerOperationKind::Gc)?;
        assert_eq!(client.gc(gc_handle.clone())?, expected_gc);
        client.cancel(gc_handle)?;
        client.stream.shutdown(Shutdown::Write)?;
        worker
            .join()
            .map_err(|_| io::Error::other("broker worker panicked"))??;
        let snapshot = broker.admission_snapshot();
        assert!(!snapshot.build_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
        fake.assert_exhausted()?;
        Ok(())
    }

    #[test]
    fn mismatched_response_poisoning_prevents_stream_reuse() -> Result<(), Box<dyn Error>> {
        let (mut server, client) = UnixStream::pair()?;
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let worker = thread::spawn(move || -> Result<(), io::Error> {
            let deadline = Instant::now()
                .checked_add(RESPONSE_TIMEOUT)
                .ok_or_else(|| io::Error::other("deadline overflow"))?;
            let _ = read_frame(&mut server, deadline);
            server.write_all(
                &ProductFrameCodec::encode_cli_response(999, &CliBrokerResponse::Cancelled)
                    .map_err(io::Error::other)?,
            )?;
            release_rx
                .recv()
                .map_err(|_| io::Error::other("client dropped before release"))
        });
        let mut client = BrokerLifecycleClient::from_stream(client);
        assert_eq!(
            client
                .begin(BrokerOperationKind::Resolve)
                .map_err(|error| error.code()),
            Err(BrokerClientErrorCode::UnexpectedResponse)
        );
        assert_eq!(
            client
                .begin(BrokerOperationKind::Resolve)
                .map_err(|error| error.code()),
            Err(BrokerClientErrorCode::ConnectionFailed)
        );
        release_tx.send(())?;
        worker
            .join()
            .map_err(|_| io::Error::other("fake broker panicked"))??;
        Ok(())
    }
}
