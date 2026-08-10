//! A loopback-only, exact-transcript HTTP fixture for cache/CDN fault tests.
//!
//! The fixture accepts only a bounded sequence of exact method/path pairs and
//! supports complete replies, connection drops, and declared-length truncation.
//! It never resolves DNS or connects to a non-loopback address.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_EXCHANGES: usize = 64;
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_POLL: Duration = Duration::from_millis(5);

/// One exact HTTP request expected by the fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedRequest {
    method: &'static str,
    path: String,
}

impl ExpectedRequest {
    /// Creates a validated `GET` expectation.
    pub fn get(path: &str) -> Result<Self, HttpFixtureError> {
        if !path.starts_with('/')
            || path.len() > 1024
            || path.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(HttpFixtureError::InvalidScript);
        }
        Ok(Self {
            method: "GET",
            path: path.to_owned(),
        })
    }
}

/// A bounded HTTP response emitted by the fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl FixtureResponse {
    /// Creates a response with a closed status and content-type vocabulary.
    pub fn new(
        status: u16,
        content_type: &'static str,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, HttpFixtureError> {
        let body = body.into();
        if !(200..=599).contains(&status)
            || !matches!(
                content_type,
                "application/json" | "application/octet-stream" | "text/plain"
            )
            || body.len() > MAX_BODY_BYTES
        {
            return Err(HttpFixtureError::InvalidScript);
        }
        Ok(Self {
            status,
            content_type,
            body,
        })
    }
}

/// The network behavior for one expected request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpFault {
    /// Send a complete bounded response.
    Respond(FixtureResponse),
    /// Accept and immediately close the connection without a response.
    DropConnection,
    /// Declare the complete response length but send only the given body prefix.
    Truncate {
        /// The response whose declared length remains authoritative.
        response: FixtureResponse,
        /// Number of body bytes to send before closing.
        body_bytes: usize,
    },
}

/// One exact request/fault pair in a fixture transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpExchange {
    expected: ExpectedRequest,
    fault: HttpFault,
}

impl HttpExchange {
    /// Binds an exact request expectation to one fault behavior.
    #[must_use]
    pub const fn new(expected: ExpectedRequest, fault: HttpFault) -> Self {
        Self { expected, fault }
    }
}

/// A closed, redacted fixture failure.
#[derive(Debug)]
pub enum HttpFixtureError {
    /// The transcript contains an invalid or unsafe value.
    InvalidScript,
    /// A bounded socket operation failed.
    Io(io::Error),
    /// A request did not match the exact transcript head.
    UnexpectedRequest,
    /// The next transcript request did not arrive before the deadline.
    AcceptTimeout,
    /// The fixture worker panicked.
    WorkerPanicked,
}

impl fmt::Display for HttpFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScript => formatter.write_str("invalid HTTP fixture script"),
            Self::Io(_) => formatter.write_str("HTTP fixture I/O failure"),
            Self::UnexpectedRequest => formatter.write_str("unexpected HTTP fixture request"),
            Self::AcceptTimeout => formatter.write_str("HTTP fixture accept timed out"),
            Self::WorkerPanicked => formatter.write_str("HTTP fixture worker failed"),
        }
    }
}

impl std::error::Error for HttpFixtureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidScript
            | Self::UnexpectedRequest
            | Self::AcceptTimeout
            | Self::WorkerPanicked => None,
        }
    }
}

impl From<io::Error> for HttpFixtureError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// A loopback fixture server whose transcript must be explicitly finished.
#[derive(Debug)]
pub struct FixtureHttpServer {
    address: SocketAddrV4,
    worker: Option<JoinHandle<Result<(), HttpFixtureError>>>,
}

impl FixtureHttpServer {
    /// Starts a loopback-only server for a nonempty exact transcript.
    pub fn start(script: Vec<HttpExchange>) -> Result<Self, HttpFixtureError> {
        validate_script(&script)?;
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = match listener.local_addr()? {
            std::net::SocketAddr::V4(address) if address.ip().is_loopback() => address,
            _ => return Err(HttpFixtureError::InvalidScript),
        };
        let worker = thread::spawn(move || serve(listener, script));
        Ok(Self {
            address,
            worker: Some(worker),
        })
    }

    /// Returns the loopback-only origin URL without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Joins the worker and proves that every scripted exchange completed.
    pub fn finish(mut self) -> Result<(), HttpFixtureError> {
        self.worker
            .take()
            .ok_or(HttpFixtureError::WorkerPanicked)?
            .join()
            .map_err(|_| HttpFixtureError::WorkerPanicked)?
    }
}

fn validate_script(script: &[HttpExchange]) -> Result<(), HttpFixtureError> {
    if script.is_empty() || script.len() > MAX_EXCHANGES {
        return Err(HttpFixtureError::InvalidScript);
    }
    for exchange in script {
        if let HttpFault::Truncate {
            response,
            body_bytes,
        } = &exchange.fault
            && *body_bytes >= response.body.len()
        {
            return Err(HttpFixtureError::InvalidScript);
        }
    }
    Ok(())
}

fn serve(listener: TcpListener, script: Vec<HttpExchange>) -> Result<(), HttpFixtureError> {
    for exchange in script {
        let mut stream = accept_bounded(&listener)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let (method, path) = read_request_line(&mut stream)?;
        if method != exchange.expected.method || path != exchange.expected.path {
            return Err(HttpFixtureError::UnexpectedRequest);
        }
        write_fault(&mut stream, exchange.fault)?;
    }
    Ok(())
}

fn accept_bounded(listener: &TcpListener) -> Result<TcpStream, HttpFixtureError> {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, address)) if address.ip().is_loopback() => return Ok(stream),
            Ok(_) => return Err(HttpFixtureError::UnexpectedRequest),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= ACCEPT_TIMEOUT {
                    return Err(HttpFixtureError::AcceptTimeout);
                }
                thread::sleep(ACCEPT_POLL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_request_line(stream: &mut TcpStream) -> Result<(String, String), HttpFixtureError> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 512];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk)?;
        if count == 0 || bytes.len().saturating_add(count) > MAX_REQUEST_BYTES {
            return Err(HttpFixtureError::UnexpectedRequest);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| HttpFixtureError::UnexpectedRequest)?;
    let line = text
        .split_once("\r\n")
        .map(|(line, _)| line)
        .ok_or(HttpFixtureError::UnexpectedRequest)?;
    let mut parts = line.split(' ');
    let method = parts.next().ok_or(HttpFixtureError::UnexpectedRequest)?;
    let path = parts.next().ok_or(HttpFixtureError::UnexpectedRequest)?;
    let version = parts.next().ok_or(HttpFixtureError::UnexpectedRequest)?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(HttpFixtureError::UnexpectedRequest);
    }
    Ok((method.to_owned(), path.to_owned()))
}

fn write_fault(stream: &mut TcpStream, fault: HttpFault) -> Result<(), HttpFixtureError> {
    match fault {
        HttpFault::Respond(response) => write_response(stream, &response, response.body.len()),
        HttpFault::DropConnection => {
            stream.shutdown(Shutdown::Both)?;
            Ok(())
        }
        HttpFault::Truncate {
            response,
            body_bytes,
        } => write_response(stream, &response, body_bytes),
    }
}

fn write_response(
    stream: &mut TcpStream,
    response: &FixtureResponse,
    body_bytes: usize,
) -> Result<(), HttpFixtureError> {
    let reason = match response.status {
        200 => "OK",
        404 => "Not Found",
        _ => "Fixture",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    )?;
    stream.write_all(&response.body[..body_bytes])?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(server: &FixtureHttpServer, path: &str) -> io::Result<Vec<u8>> {
        let mut stream = TcpStream::connect(server.address)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        Ok(response)
    }

    #[test]
    fn exact_transcript_serves_drop_and_truncate_faults() -> Result<(), Box<dyn std::error::Error>>
    {
        let complete = FixtureResponse::new(200, "application/json", b"{\"ok\":true}".to_vec())?;
        let truncated =
            FixtureResponse::new(200, "application/octet-stream", b"complete-body".to_vec())?;
        let server = FixtureHttpServer::start(vec![
            HttpExchange::new(
                ExpectedRequest::get("/complete")?,
                HttpFault::Respond(complete),
            ),
            HttpExchange::new(ExpectedRequest::get("/drop")?, HttpFault::DropConnection),
            HttpExchange::new(
                ExpectedRequest::get("/truncate")?,
                HttpFault::Truncate {
                    response: truncated,
                    body_bytes: 4,
                },
            ),
        ])?;

        let complete = request(&server, "/complete")?;
        assert!(complete.ends_with(b"{\"ok\":true}"));
        assert!(request(&server, "/drop")?.is_empty());
        let truncated = request(&server, "/truncate")?;
        assert!(truncated.ends_with(b"comp"));
        assert!(String::from_utf8_lossy(&truncated).contains("Content-Length: 13"));
        server.finish()?;
        Ok(())
    }

    #[test]
    fn script_rejects_unsafe_or_non_faulting_inputs() -> Result<(), Box<dyn std::error::Error>> {
        assert!(ExpectedRequest::get("https://example.invalid/").is_err());
        assert!(FixtureResponse::new(199, "text/plain", Vec::new()).is_err());
        let response = FixtureResponse::new(200, "text/plain", b"body".to_vec())?;
        assert!(
            FixtureHttpServer::start(vec![HttpExchange::new(
                ExpectedRequest::get("/not-truncated")?,
                HttpFault::Truncate {
                    response,
                    body_bytes: 4,
                },
            )])
            .is_err()
        );
        Ok(())
    }
}
