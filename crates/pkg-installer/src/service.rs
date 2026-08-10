//! Production service entry points with fixed socket and identity contracts.

#[cfg(target_os = "linux")]
use crate::{
    LinuxHelperSession, LinuxRootSetStore, serve_broker_connection_with_nix,
    serve_helper_connection,
};
#[cfg(target_os = "linux")]
use listenfd::ListenFd;
#[cfg(target_os = "linux")]
use nix::unistd::{Uid, User};
#[cfg(target_os = "linux")]
use pkg_nix::{
    InProcessBroker, InProcessHelper, InProcessPeer, NixAdapter, RealNixAdapter,
    RootNixRepairExecutor, VerifiedRepairExecutor,
};
use std::{error::Error, fmt};
#[cfg(target_os = "linux")]
use std::{
    io,
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[cfg(target_os = "linux")]
const BROKER_ACCOUNT: &str = "pkg-nix-broker";
#[cfg(target_os = "linux")]
const LINUX_BROKER_SOCKET: &str = "/run/pkg/broker.sock";
#[cfg(target_os = "linux")]
const LINUX_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
#[cfg(target_os = "linux")]
const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
#[cfg(target_os = "linux")]
const LINUX_HELPER_HOME: &str = "/var/lib/pkg/helper-home";
#[cfg(target_os = "linux")]
const LINUX_BROKER_HOME: &str = "/var/lib/pkg/broker-home";
#[cfg(target_os = "linux")]
const MAX_BROKER_CONNECTIONS: usize = 32;
#[cfg(target_os = "linux")]
const BROKER_READ_TIMEOUT: Duration = Duration::from_mins(5);
#[cfg(target_os = "linux")]
const BROKER_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable production-service startup/runtime failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceErrorCode {
    /// The process identity does not match the installed service contract.
    WrongIdentity,
    /// The dedicated broker account is missing or invalid.
    BrokerAccountUnavailable,
    /// Socket activation did not supply exactly the expected listener.
    InvalidActivatedSocket,
    /// Root-owned runtime state or the managed Nix executable failed validation.
    InvalidRuntime,
    /// The fixed helper state could not be initialized.
    InitializationFailed,
    /// The activated listener failed while accepting connections.
    ListenerFailed,
    /// A bounded broker connection worker could not be started.
    WorkerUnavailable,
}

/// Redacted production-service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceError {
    code: ServiceErrorCode,
}

impl ServiceError {
    const fn new(code: ServiceErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> ServiceErrorCode {
        self.code
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("managed package service failed")
    }
}

impl Error for ServiceError {}

/// Runs the unprivileged Linux broker from one systemd-activated Unix listener.
///
/// The service must run as the installed broker account. It validates the exact
/// public endpoint, authenticates every caller from kernel peer credentials,
/// and limits concurrent sessions before starting a connection worker. Idle
/// readers and blocked writers have finite deadlines. Invalid clients are
/// connection-local failures and do not terminate the broker.
///
/// This entry point currently serves the authenticated operation-lifecycle
/// protocol. Product-command dispatch to the managed Nix adapter is a separate
/// wiring step and is not synthesized here.
///
/// # Errors
///
/// Returns a redacted error for identity, activation, initialization, listener,
/// or worker-start failures.
#[cfg(target_os = "linux")]
pub fn run_linux_broker_from_activation() -> Result<(), ServiceError> {
    let expected_uid = broker_uid()?;
    if Uid::effective().as_raw() != expected_uid {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let listener = activated_unix_listener(LINUX_BROKER_SOCKET)?;
    let broker = InProcessBroker::new()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let adapter: Arc<dyn NixAdapter> = Arc::new(
        RealNixAdapter::new(Path::new(MANAGED_NIX_BINARY), Path::new(LINUX_BROKER_HOME))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let limiter = ConnectionLimiter::new(MAX_BROKER_CONNECTIONS);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = limiter.try_acquire() else {
                    drop(stream);
                    continue;
                };
                if configure_broker_stream(&stream).is_err() {
                    drop(permit);
                    continue;
                }
                let connection_broker = Arc::clone(&broker);
                let connection_adapter = Arc::clone(&adapter);
                thread::Builder::new()
                    .name(String::from("pkg-broker-client"))
                    .spawn(move || {
                        let _permit = permit;
                        let _ = serve_broker_connection_with_nix(
                            stream,
                            &connection_broker,
                            &connection_adapter,
                        );
                    })
                    .map_err(|_| ServiceError::new(ServiceErrorCode::WorkerUnavailable))?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServiceError::new(ServiceErrorCode::ListenerFailed)),
        }
    }
}

/// Reports the Linux broker entry point as unavailable on other hosts.
///
/// # Errors
///
/// Always returns `InvalidRuntime` outside Linux.
#[cfg(not(target_os = "linux"))]
pub const fn run_linux_broker_from_activation() -> Result<(), ServiceError> {
    Err(ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

/// Runs the Linux root helper from exactly one systemd-activated Unix listener.
///
/// The helper validates its root identity, resolves the broker uid from the
/// installed account database, binds the capability engine to the real fixed
/// local-store repair executor, and never binds or accepts an alternate path.
/// Malformed or unauthenticated connections are connection-local failures and
/// do not terminate the service.
///
/// # Errors
///
/// Returns a redacted error for identity, activation, runtime, initialization,
/// or listener failures.
#[cfg(target_os = "linux")]
pub fn run_linux_root_helper_from_activation() -> Result<(), ServiceError> {
    if !Uid::effective().is_root() {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let broker_uid = broker_uid()?;
    let listener = activated_unix_listener(LINUX_HELPER_SOCKET)?;
    let repair_executor: Arc<dyn VerifiedRepairExecutor> = Arc::new(
        RootNixRepairExecutor::new(Path::new(MANAGED_NIX_BINARY), Path::new(LINUX_HELPER_HOME))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let helper = InProcessHelper::with_repair_executor(broker_uid, repair_executor)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let authenticated = helper
        .connect(InProcessPeer::authenticated_uid(broker_uid))
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let roots = LinuxRootSetStore::production()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let session = LinuxHelperSession::new(authenticated, roots);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = serve_helper_connection(stream, broker_uid, &session);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServiceError::new(ServiceErrorCode::ListenerFailed)),
        }
    }
}

/// Reports the Linux service entry point as unavailable on other hosts.
///
/// # Errors
///
/// Always returns `InvalidRuntime` outside Linux.
#[cfg(not(target_os = "linux"))]
pub const fn run_linux_root_helper_from_activation() -> Result<(), ServiceError> {
    Err(ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

#[cfg(target_os = "linux")]
fn broker_uid() -> Result<u32, ServiceError> {
    let user = User::from_name(BROKER_ACCOUNT)
        .map_err(|_| ServiceError::new(ServiceErrorCode::BrokerAccountUnavailable))?
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::BrokerAccountUnavailable))?;
    let uid = user.uid.as_raw();
    if uid == 0 {
        return Err(ServiceError::new(
            ServiceErrorCode::BrokerAccountUnavailable,
        ));
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn activated_unix_listener(expected: &str) -> Result<UnixListener, ServiceError> {
    let mut inherited = ListenFd::from_env();
    if inherited.len() != 1 {
        return Err(ServiceError::new(ServiceErrorCode::InvalidActivatedSocket));
    }
    let listener = inherited
        .take_unix_listener(0)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidActivatedSocket))?
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::InvalidActivatedSocket))?;
    validate_listener_path(&listener, Path::new(expected))?;
    Ok(listener)
}

#[cfg(target_os = "linux")]
fn validate_listener_path(listener: &UnixListener, expected: &Path) -> Result<(), ServiceError> {
    let address = listener
        .local_addr()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidActivatedSocket))?;
    if address.as_pathname() == Some(expected) {
        Ok(())
    } else {
        Err(ServiceError::new(ServiceErrorCode::InvalidActivatedSocket))
    }
}

#[cfg(target_os = "linux")]
fn configure_broker_stream(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(BROKER_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(BROKER_WRITE_TIMEOUT))
}

#[cfg(target_os = "linux")]
struct ConnectionLimiter {
    active: Mutex<usize>,
    limit: usize,
}

#[cfg(target_os = "linux")]
impl ConnectionLimiter {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: Mutex::new(0),
            limit,
        })
    }

    fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *active >= self.limit {
            return None;
        }
        *active += 1;
        drop(active);
        Some(ConnectionPermit {
            limiter: Arc::clone(self),
        })
    }
}

#[cfg(target_os = "linux")]
struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
}

#[cfg(target_os = "linux")]
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut active = self
            .limiter
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().join(format!("phs-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn activated_listener_must_match_the_exact_installed_endpoint() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let actual = scratch.0.join("actual.sock");
        let listener = UnixListener::bind(&actual)?;
        validate_listener_path(&listener, &actual)?;
        let error = match validate_listener_path(&listener, &scratch.0.join("other.sock")) {
            Ok(()) => return Err(io::Error::other("wrong endpoint was accepted").into()),
            Err(error) => error,
        };
        assert_eq!(error.code(), ServiceErrorCode::InvalidActivatedSocket);
        Ok(())
    }

    #[test]
    fn broker_connection_limit_is_exact_and_permits_return_on_drop() {
        let limiter = ConnectionLimiter::new(2);
        let first = limiter.try_acquire();
        let second = limiter.try_acquire();
        assert!(first.is_some());
        assert!(second.is_some());
        assert!(limiter.try_acquire().is_none());
        drop(first);
        assert!(limiter.try_acquire().is_some());
    }
}
