//! Real socket tests for startup waiting and fail-closed health observations.

use super::*;
use pkg_nix::{
    AcceptedFormats, CliBrokerRequest, CliBrokerResponse, FormatVersion, InProcessBroker,
    InProcessCallerPeer, MethodKind, NixAdapterErrorCode, NixVersion, ProductFrameCodec,
    VersionInfo,
};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::mpsc,
    thread,
};

type Exchange = (CliBrokerRequest, CliBrokerResponse);

fn health_script(ownership: bool) -> Vec<Exchange> {
    let handle = InProcessBroker::new()
        .unwrap()
        .connect(InProcessCallerPeer::authenticated(1001))
        .unwrap()
        .begin(BrokerOperationKind::Doctor)
        .unwrap();
    let version = VersionInfo::new(
        NixVersion::new("2.34.8").unwrap(),
        AcceptedFormats::new(FormatVersion::new(1).unwrap()),
    );
    vec![
        (
            CliBrokerRequest::Begin(BrokerOperationKind::Doctor),
            CliBrokerResponse::Started(handle.clone()),
        ),
        (
            CliBrokerRequest::Version(handle.clone()),
            CliBrokerResponse::Version(version),
        ),
        (
            CliBrokerRequest::VerifyManagedOwnership(handle.clone()),
            CliBrokerResponse::ManagedOwnership(ownership),
        ),
        (
            CliBrokerRequest::Complete(handle),
            CliBrokerResponse::Completed,
        ),
    ]
}

fn receive(stream: &mut UnixStream) -> (u64, CliBrokerRequest) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut header = [0_u8; 20];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    assert!(length < 4096);
    let mut frame = header.to_vec();
    frame.resize(20 + length, 0);
    stream.read_exact(&mut frame[20..]).unwrap();
    ProductFrameCodec::decode_cli_request(&frame).unwrap()
}

fn respond(stream: &mut UnixStream, script: Vec<Exchange>) {
    for (expected, response) in script {
        let (id, request) = receive(stream);
        assert_eq!(request, expected);
        stream
            .write_all(&ProductFrameCodec::encode_cli_response(id, &response).unwrap())
            .unwrap();
    }
}

fn wait_for_disconnect(stream: &mut UnixStream) {
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
}

fn once(script: Vec<Exchange>) -> Option<Health> {
    let (client, mut server) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        respond(&mut server, script);
        wait_for_disconnect(&mut server);
    });
    let mut client = Some(BrokerLifecycleClient::from_stream(client));
    let health = observe_until(
        Instant::now() + Duration::from_secs(2),
        |_| Ok(client.take().expect("terminal replies must not retry")),
        || panic!("a ready broker must not show startup waiting"),
    );
    worker.join().unwrap();
    health
}

#[test]
fn ready_broker_completes_without_startup_waiting() {
    let health = once(health_script(true)).unwrap();
    assert_eq!(health.version, "2.34.8");
    assert!(health.managed_ownership);
}

#[test]
fn negative_ownership_is_final() {
    assert!(!once(health_script(false)).unwrap().managed_ownership);
}

#[test]
fn adapter_refusal_cancels_without_retrying() {
    let mut script = health_script(true);
    script[1].1 =
        CliBrokerResponse::AdapterFailure(MethodKind::Version, NixAdapterErrorCode::TrustFailure);
    let CliBrokerRequest::Complete(handle) = script.pop().unwrap().0 else {
        panic!("missing completion")
    };
    script[2] = (
        CliBrokerRequest::Cancel(handle),
        CliBrokerResponse::Cancelled,
    );
    assert!(once(script).is_none());
}

#[test]
fn failed_completion_discards_successful_ownership() {
    let mut script = health_script(true);
    script[3].1 = CliBrokerResponse::Cancelled;
    assert!(once(script).is_none());
}

#[test]
fn ownership_refusal_stays_final_when_completion_disconnects() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let worker = thread::spawn(move || {
        let mut script = health_script(false);
        let completion = script.pop().unwrap().0;
        respond(&mut server, script);
        assert_eq!(receive(&mut server).1, completion);
        // Drop the stream without acknowledging completion.
    });
    let mut client = Some(BrokerLifecycleClient::from_stream(client));
    assert!(
        observe_until(
            Instant::now() + Duration::from_secs(2),
            |_| Ok(client.take().expect("ownership refusal must not reconnect")),
            || panic!("ownership refusal must not retry"),
        )
        .is_none()
    );
    worker.join().unwrap();
}

#[test]
fn invalid_or_uncorrelated_reply_is_final() {
    for frame in [
        vec![0; 20],
        ProductFrameCodec::encode_cli_response(999, &CliBrokerResponse::Completed).unwrap(),
    ] {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            receive(&mut server);
            server.write_all(&frame).unwrap();
            wait_for_disconnect(&mut server);
        });
        let mut client = Some(BrokerLifecycleClient::from_stream(client));
        assert!(
            observe_until(
                Instant::now() + Duration::from_secs(2),
                |_| Ok(client.take().expect("invalid replies must not retry")),
                || panic!("invalid replies must not show startup waiting"),
            )
            .is_none()
        );
        worker.join().unwrap();
    }
}

fn connect(path: &Path, deadline: Instant) -> Result<BrokerLifecycleClient, BrokerClientError> {
    BrokerLifecycleClient::connect_until(path, deadline)
}

#[test]
fn delayed_socket_startup_waits_once_and_recovers() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("broker.sock");
    let (start, started) = mpsc::channel();
    let server_path = path.clone();
    let worker = thread::spawn(move || {
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let listener = UnixListener::bind(server_path).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        respond(&mut server, health_script(true));
        wait_for_disconnect(&mut server);
    });
    let mut notices = 0;
    let health = observe_until(
        Instant::now() + Duration::from_secs(2),
        |deadline| connect(&path, deadline),
        || {
            notices += 1;
            start.send(()).unwrap();
        },
    )
    .unwrap();
    worker.join().unwrap();
    assert_eq!(notices, 1);
    assert!(health.managed_ownership);
}

#[test]
fn disconnected_probe_retries_with_a_fresh_session() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("broker.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let worker = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        let mut script = health_script(true);
        script.truncate(1);
        respond(&mut first, script);
        receive(&mut first);
        drop(first);
        let (mut second, _) = listener.accept().unwrap();
        respond(&mut second, health_script(true));
        wait_for_disconnect(&mut second);
    });
    let mut attempts = 0;
    let mut notices = 0;
    let health = observe_until(
        Instant::now() + Duration::from_secs(2),
        |deadline| {
            attempts += 1;
            connect(&path, deadline)
        },
        || notices += 1,
    )
    .unwrap();
    worker.join().unwrap();
    assert_eq!((attempts, notices), (2, 1));
    assert!(health.managed_ownership);
}

#[test]
fn absent_service_exhausts_one_budget_without_claiming_health() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("absent.sock");
    let deadline = Instant::now() + Duration::from_millis(150);
    let mut notices = 0;
    assert!(observe_until(deadline, |limit| connect(&path, limit), || notices += 1).is_none());
    assert!(Instant::now() >= deadline);
    assert!(Instant::now() < deadline + Duration::from_secs(2));
    assert_eq!(notices, 1);
}

#[test]
fn stalled_reply_at_any_stage_uses_the_session_deadline() {
    for completed_steps in 0..4 {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let path = root.path().join("broker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (release, held) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut server, _) = listener.accept().unwrap();
            let mut script = health_script(true);
            script.truncate(completed_steps);
            respond(&mut server, script);
            receive(&mut server);
            // No response until the client returns, or the test watchdog expires.
            held.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_millis(200);
        let health = observe_until(deadline, |limit| connect(&path, limit), || {});
        let finished = Instant::now();
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(health.is_none());
        assert!(finished >= deadline);
        assert!(finished < deadline + Duration::from_secs(1));
    }
}

#[test]
fn elapsed_session_budget_cannot_start_another_call() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("broker.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let worker = thread::spawn(move || {
        let (mut server, _) = listener.accept().unwrap();
        let mut script = health_script(true);
        script.truncate(1);
        respond(&mut server, script);
        // The expired client must close without sending a version request.
        wait_for_disconnect(&mut server);
    });
    let deadline = Instant::now() + Duration::from_millis(150);
    let mut client = connect(&path, deadline).unwrap();
    let handle = client.begin(BrokerOperationKind::Doctor).unwrap();
    thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert_eq!(
        client.version(handle).unwrap_err().code(),
        BrokerClientErrorCode::TransportFailure
    );
    drop(client);
    worker.join().unwrap();
}

#[test]
fn inaccessible_endpoint_does_not_wait_or_retry() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = tempfile::tempdir_in("/tmp").unwrap();
    if root.path().metadata().unwrap().uid() == 0 {
        return; // Root bypasses Unix directory permissions.
    }
    let path = root.path().join("broker.sock");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut attempts = 0;
    let mut notices = 0;
    let health = observe_until(
        Instant::now() + Duration::from_secs(2),
        |deadline| {
            attempts += 1;
            connect(&path, deadline)
        },
        || notices += 1,
    );
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(health.is_none());
    assert_eq!((attempts, notices), (1, 0));
}
