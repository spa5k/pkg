//! Unix CLI-to-broker transport with kernel-derived caller identity.

use crate::platform::peer_uid;
use pkg_nix::{
    BrokerError, CliBrokerRequest, CliBrokerResponse, InProcessBroker, InProcessCallerPeer,
    ProductFrameCodec,
};
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
};

const FRAME_HEADER_BYTES: usize = 20;
const MAX_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Stable CLI-to-broker transport failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerTransportErrorCode {
    /// Kernel peer credentials were unavailable.
    UnauthenticatedPeer,
    /// The byte stream ended mid-frame or response I/O failed.
    TransportFailure,
    /// The strict product frame was invalid.
    InvalidFrame,
    /// The authenticated operation lifecycle rejected the request.
    BrokerFailure,
}

/// Redacted CLI-to-broker transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerTransportError {
    code: BrokerTransportErrorCode,
}

impl BrokerTransportError {
    const fn new(code: BrokerTransportErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> BrokerTransportErrorCode {
        self.code
    }
}

impl fmt::Display for BrokerTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("package broker transport failed")
    }
}

impl Error for BrokerTransportError {}

/// Authenticates one connection and serves lifecycle frames until disconnect.
///
/// The uid is obtained once from `SO_PEERCRED` (Linux) or `getpeereid`
/// (macOS); no payload identity is read.
/// Disconnect always invokes broker-owned cleanup for this caller session.
///
/// # Errors
///
/// Returns a redacted error for peer-credential, bounded transport, strict
/// framing, or authenticated lifecycle failure.
pub fn serve_broker_connection(
    mut stream: UnixStream,
    broker: &Arc<InProcessBroker>,
) -> Result<(), BrokerTransportError> {
    let uid = peer_uid(&stream)
        .map_err(|()| BrokerTransportError::new(BrokerTransportErrorCode::UnauthenticatedPeer))?;
    let caller = broker
        .connect(InProcessCallerPeer::authenticated(uid))
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
    let result = serve_frames(&mut stream, |request| match request {
        CliBrokerRequest::Begin(kind) => caller.begin(kind).map(CliBrokerResponse::Started),
        CliBrokerRequest::Poll(handle) => caller.poll(&handle).map(CliBrokerResponse::Status),
        CliBrokerRequest::Cancel(handle) => {
            caller.cancel(&handle)?;
            Ok(CliBrokerResponse::Cancelled)
        }
    });
    let disconnected = caller.disconnect();
    match (result, disconnected) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(_)) => Err(BrokerTransportError::new(
            BrokerTransportErrorCode::BrokerFailure,
        )),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn serve_frames(
    stream: &mut UnixStream,
    mut dispatch: impl FnMut(CliBrokerRequest) -> Result<CliBrokerResponse, BrokerError>,
) -> Result<(), BrokerTransportError> {
    loop {
        let Some(frame) = read_frame(stream)? else {
            return Ok(());
        };
        let (request_id, request) = ProductFrameCodec::decode_cli_request(&frame)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
        let response = dispatch(request)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::BrokerFailure))?;
        let encoded = ProductFrameCodec::encode_cli_response(request_id, &response)
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?;
        stream
            .write_all(&encoded)
            .and_then(|()| stream.flush())
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    }
}

fn read_frame(stream: &mut UnixStream) -> Result<Option<Vec<u8>>, BrokerTransportError> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    loop {
        match stream.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            _ => {
                return Err(BrokerTransportError::new(
                    BrokerTransportErrorCode::TransportFailure,
                ));
            }
        }
    }
    stream
        .read_exact(&mut header[1..])
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    let payload_length = u32::from_be_bytes(
        header[16..20]
            .try_into()
            .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::InvalidFrame))?,
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_BYTES {
        return Err(BrokerTransportError::new(
            BrokerTransportErrorCode::InvalidFrame,
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload_length);
    frame.extend_from_slice(&header);
    frame.resize(FRAME_HEADER_BYTES + payload_length, 0);
    stream
        .read_exact(&mut frame[FRAME_HEADER_BYTES..])
        .map_err(|_| BrokerTransportError::new(BrokerTransportErrorCode::TransportFailure))?;
    Ok(Some(frame))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use pkg_nix::{BrokerOperationKind, OperationStatus};
    use std::{io, net::Shutdown, thread};

    fn read_response(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        stream.read_exact(&mut header)?;
        let length = u32::from_be_bytes(
            header[16..20]
                .try_into()
                .map_err(|_| io::Error::other("invalid response header"))?,
        ) as usize;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + length);
        frame.extend_from_slice(&header);
        frame.resize(FRAME_HEADER_BYTES + length, 0);
        stream.read_exact(&mut frame[FRAME_HEADER_BYTES..])?;
        Ok(frame)
    }

    #[test]
    fn peer_authenticated_connection_serves_lifecycle_until_disconnect()
    -> Result<(), Box<dyn Error>> {
        let broker = InProcessBroker::new()?;
        let (server, mut client) = UnixStream::pair()?;
        let server_broker = Arc::clone(&broker);
        let worker = thread::spawn(move || serve_broker_connection(server, &server_broker));

        client.write_all(&ProductFrameCodec::encode_cli_request(
            1,
            &CliBrokerRequest::Begin(BrokerOperationKind::Resolve),
        )?)?;
        let (_, started) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        let CliBrokerResponse::Started(handle) = started else {
            return Err(io::Error::other("expected started response").into());
        };
        client.write_all(&ProductFrameCodec::encode_cli_request(
            2,
            &CliBrokerRequest::Poll(handle.clone()),
        )?)?;
        let (_, status) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        assert_eq!(status, CliBrokerResponse::Status(OperationStatus::Running));
        client.write_all(&ProductFrameCodec::encode_cli_request(
            3,
            &CliBrokerRequest::Cancel(handle),
        )?)?;
        let (_, cancelled) = ProductFrameCodec::decode_cli_response(&read_response(&mut client)?)?;
        assert_eq!(cancelled, CliBrokerResponse::Cancelled);
        client.shutdown(Shutdown::Write)?;
        worker
            .join()
            .map_err(|_| io::Error::other("broker thread panicked"))??;
        let snapshot = broker.admission_snapshot();
        assert_eq!(snapshot.operation_count(), 1);
        assert!(!snapshot.build_held());
        assert!(!snapshot.gc_held());
        assert_eq!(snapshot.gc_inhibitor_count(), 0);
        Ok(())
    }
}
