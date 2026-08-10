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
    BrokerOperationKind, CliBrokerRequest, CliBrokerResponse, OperationHandle, OperationStatus,
    ProductFrameCodec,
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
    use pkg_installer::serve_broker_connection;
    use pkg_nix::InProcessBroker;
    use std::{fs, net::Shutdown, path::PathBuf, sync::Arc, thread};

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
    fn real_transport_round_trips_lifecycle_and_cleanup() -> Result<(), Box<dyn Error>> {
        let broker = InProcessBroker::new()?;
        let scratch = Scratch::new()?;
        let socket = scratch.0.join("broker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        let server_broker = Arc::clone(&broker);
        let worker = thread::spawn(move || {
            let (server, _) = listener.accept()?;
            serve_broker_connection(server, &server_broker)
                .map_err(|error| io::Error::other(error.to_string()))
        });
        let mut client = BrokerLifecycleClient::connect(&socket)?;

        let handle = client.begin(BrokerOperationKind::Build)?;
        assert_eq!(client.poll(handle.clone())?, OperationStatus::Running);
        client.cancel(handle)?;
        client.stream.shutdown(Shutdown::Write)?;
        worker
            .join()
            .map_err(|_| io::Error::other("broker worker panicked"))??;
        let snapshot = broker.admission_snapshot();
        assert!(!snapshot.build_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
        Ok(())
    }

    #[test]
    fn mismatched_response_poisoning_prevents_stream_reuse() -> Result<(), Box<dyn Error>> {
        let (mut server, client) = UnixStream::pair()?;
        let worker = thread::spawn(move || -> Result<(), io::Error> {
            let deadline = Instant::now()
                .checked_add(RESPONSE_TIMEOUT)
                .ok_or_else(|| io::Error::other("deadline overflow"))?;
            let _ = read_frame(&mut server, deadline);
            server.write_all(
                &ProductFrameCodec::encode_cli_response(999, &CliBrokerResponse::Cancelled)
                    .map_err(io::Error::other)?,
            )
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
        worker
            .join()
            .map_err(|_| io::Error::other("fake broker panicked"))??;
        Ok(())
    }
}
