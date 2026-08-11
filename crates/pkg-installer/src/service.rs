//! Production service entry points with fixed socket and identity contracts.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::{
    BrokerApprovalAudit, serve_broker_connection_with_nix_and_approval, serve_helper_connection,
};
#[cfg(target_os = "linux")]
use crate::{LinuxHelperSession, LinuxRootSetStore};
#[cfg(target_os = "macos")]
use crate::{MacOsHelperSession, MacOsRootSetStore, MacOsSocketContract};
#[cfg(target_os = "macos")]
use exacl::getfacl;
#[cfg(target_os = "linux")]
use listenfd::ListenFd;
#[cfg(target_os = "macos")]
use nix::sys::stat::{Mode, umask};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use nix::unistd::{Gid, Uid, User};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pkg_nix::{
    InProcessBroker, InProcessHelper, InProcessPeer, NixAdapter, RealNixAdapter,
    RootNixRepairExecutor, VerifiedRepairExecutor,
};
use std::{error::Error, fmt};
#[cfg(target_os = "macos")]
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    io,
    os::unix::net::UnixListener,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const BROKER_ACCOUNT: &str = "pkg-nix-broker";
#[cfg(target_os = "linux")]
const LINUX_BROKER_SOCKET: &str = "/run/pkg/broker.sock";
#[cfg(target_os = "linux")]
const LINUX_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
#[cfg(target_os = "linux")]
const LINUX_HELPER_HOME: &str = "/var/lib/pkg/helper-home";
#[cfg(target_os = "linux")]
const LINUX_BROKER_HOME: &str = "/var/lib/pkg/broker-home";
#[cfg(target_os = "linux")]
const LINUX_BROKER_LOG: &str = "/var/lib/pkg/log/broker";
#[cfg(target_os = "macos")]
const MACOS_HELPER_HOME: &str = "/Library/Application Support/pkg/helper-home";
#[cfg(target_os = "macos")]
const MACOS_BROKER_HOME: &str = "/Library/Application Support/pkg/broker-home";
#[cfg(target_os = "macos")]
const MACOS_BROKER_LOG: &str = "/Library/Application Support/pkg/log/broker";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_BROKER_CONNECTIONS: usize = 32;

/// Stable production-service startup/runtime failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceErrorCode {
    /// The process identity does not match the installed service contract.
    WrongIdentity,
    /// The dedicated broker account is missing or invalid.
    BrokerAccountUnavailable,
    /// Socket activation did not supply exactly the expected listener.
    InvalidActivatedSocket,
    /// A fixed self-bound service socket or its managed parent state was unsafe.
    InvalidServiceSocket,
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
    let identity = broker_identity()?;
    if Uid::effective().as_raw() != identity.uid || Gid::effective().as_raw() != identity.gid {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let listener = activated_unix_listener(LINUX_BROKER_SOCKET)?;
    run_broker_listener(
        &listener,
        identity.uid,
        Path::new(LINUX_BROKER_HOME),
        Path::new(LINUX_BROKER_LOG),
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_broker_listener(
    listener: &UnixListener,
    expected_uid: u32,
    home: &Path,
    log: &Path,
) -> Result<(), ServiceError> {
    let broker = InProcessBroker::new()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let approval_audit = BrokerApprovalAudit::open(log, expected_uid)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let adapter: Arc<dyn NixAdapter> = Arc::new(
        RealNixAdapter::new(Path::new(MANAGED_NIX_BINARY), home)
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
                let connection_broker = Arc::clone(&broker);
                let connection_adapter = Arc::clone(&adapter);
                let connection_approval_audit = approval_audit.clone();
                thread::Builder::new()
                    .name(String::from("pkg-broker-client"))
                    .spawn(move || {
                        let _permit = permit;
                        let _ = serve_broker_connection_with_nix_and_approval(
                            stream,
                            &connection_broker,
                            &connection_adapter,
                            &connection_approval_audit,
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
    if !Uid::effective().is_root() || Gid::effective().as_raw() != 0 {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let broker_uid = broker_identity()?.uid;
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

/// Runs the unprivileged macOS broker on its fixed launchd-managed path.
///
/// The launchd job runs as the dedicated broker uid/gid. The service validates
/// the exact root-owned managed directory chain, replaces only an exact stale
/// broker-owned socket, binds the compiled endpoint, and authenticates every
/// client from kernel peer credentials.
///
/// # Errors
///
/// Returns a redacted error for identity, fixed-socket, runtime, listener, or
/// worker-start failures.
#[cfg(target_os = "macos")]
pub fn run_macos_broker() -> Result<(), ServiceError> {
    let identity = broker_identity()?;
    if Uid::effective().as_raw() != identity.uid || Gid::effective().as_raw() != identity.gid {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let parents = macos_socket_parents(
        identity.gid,
        Path::new("/Library/Application Support/pkg/run/broker"),
        0o771,
    );
    let listener = bind_macos_listener(
        Path::new(MacOsSocketContract::BROKER_PATH),
        MacOsSocketContract::BROKER_MODE,
        identity.uid,
        identity.gid,
        &parents,
    )?;
    run_broker_listener(
        &listener,
        identity.uid,
        Path::new(MACOS_BROKER_HOME),
        Path::new(MACOS_BROKER_LOG),
    )
}

/// Reports the macOS broker mode as unavailable on other hosts.
///
/// # Errors
///
/// Always returns `InvalidRuntime` outside macOS.
#[cfg(not(target_os = "macos"))]
pub const fn run_macos_broker() -> Result<(), ServiceError> {
    Err(ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

/// Runs the privileged macOS root helper on its fixed private socket.
///
/// The helper requires root uid plus the installed broker primary gid, binds
/// only inside the validated root-owned private directory, and serves the same
/// authenticated closed maintenance grammar as Linux.
///
/// # Errors
///
/// Returns a redacted error for identity, fixed-socket, runtime,
/// initialization, or listener failures.
#[cfg(target_os = "macos")]
pub fn run_macos_root_helper() -> Result<(), ServiceError> {
    let identity = broker_identity()?;
    if !Uid::effective().is_root() || Gid::effective().as_raw() != identity.gid {
        return Err(ServiceError::new(ServiceErrorCode::WrongIdentity));
    }
    let parents = macos_socket_parents(
        identity.gid,
        Path::new("/Library/Application Support/pkg/run/helper"),
        0o750,
    );
    let listener = bind_macos_listener(
        Path::new(MacOsSocketContract::HELPER_PATH),
        MacOsSocketContract::HELPER_MODE,
        0,
        identity.gid,
        &parents,
    )?;
    let repair_executor: Arc<dyn VerifiedRepairExecutor> = Arc::new(
        RootNixRepairExecutor::new(Path::new(MANAGED_NIX_BINARY), Path::new(MACOS_HELPER_HOME))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let helper = InProcessHelper::with_repair_executor(identity.uid, repair_executor)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let authenticated = helper
        .connect(InProcessPeer::authenticated_uid(identity.uid))
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let roots = MacOsRootSetStore::production()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let session = MacOsHelperSession::new(authenticated, roots);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = serve_helper_connection(stream, identity.uid, &session);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServiceError::new(ServiceErrorCode::ListenerFailed)),
        }
    }
}

/// Reports the macOS helper mode as unavailable on other hosts.
///
/// # Errors
///
/// Always returns `InvalidRuntime` outside macOS.
#[cfg(not(target_os = "macos"))]
pub const fn run_macos_root_helper() -> Result<(), ServiceError> {
    Err(ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
struct DirectoryExpectation<'a> {
    path: &'a Path,
    uid: u32,
    gid: u32,
    mode: u32,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct ScopedUmask(Mode);

#[cfg(target_os = "macos")]
impl ScopedUmask {
    fn for_exact_mode(mode: u32) -> Result<Self, ServiceError> {
        if mode & !0o777 != 0 {
            return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
        }
        let mask = u16::try_from(0o777 & !mode)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        Ok(Self(umask(Mode::from_bits_truncate(mask))))
    }
}

#[cfg(target_os = "macos")]
impl Drop for ScopedUmask {
    fn drop(&mut self) {
        umask(self.0);
    }
}

#[cfg(target_os = "macos")]
fn macos_socket_parents(
    broker_gid: u32,
    endpoint_parent: &Path,
    endpoint_parent_mode: u32,
) -> [DirectoryExpectation<'_>; 3] {
    [
        DirectoryExpectation {
            path: Path::new("/Library/Application Support/pkg"),
            uid: 0,
            gid: broker_gid,
            mode: 0o711,
        },
        DirectoryExpectation {
            path: Path::new("/Library/Application Support/pkg/run"),
            uid: 0,
            gid: broker_gid,
            mode: 0o751,
        },
        DirectoryExpectation {
            path: endpoint_parent,
            uid: 0,
            gid: broker_gid,
            mode: endpoint_parent_mode,
        },
    ]
}

#[cfg(target_os = "macos")]
fn bind_macos_listener(
    path: &Path,
    mode: u32,
    owner_uid: u32,
    group_gid: u32,
    parents: &[DirectoryExpectation<'_>],
) -> Result<UnixListener, ServiceError> {
    if parents.last().map(|parent| parent.path) != path.parent() {
        return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
    }
    for parent in parents {
        let metadata = fs::symlink_metadata(parent.path)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != parent.uid
            || metadata.gid() != parent.gid
            || metadata.mode() & 0o7777 != parent.mode
            || !getfacl(parent.path, None).is_ok_and(|acl| acl.is_empty())
        {
            return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == owner_uid
                && metadata.gid() == group_gid
                && metadata.mode() & 0o7777 == mode =>
        {
            fs::remove_file(path)
                .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) | Err(_) => {
            return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
        }
    }
    // A Unix-domain socket descriptor does not identify its filesystem node on
    // macOS, so `fchmod` cannot secure that node. Set its exact creation mode
    // with a scoped umask instead, avoiding a pathname-based chmod race.
    let listener = {
        let _umask = ScopedUmask::for_exact_mode(mode)?;
        UnixListener::bind(path)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?
    };
    if !socket_metadata_matches(path, mode, owner_uid, group_gid) {
        let _ = fs::remove_file(path);
        return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
    }
    Ok(listener)
}

#[cfg(target_os = "macos")]
fn socket_metadata_matches(path: &Path, mode: u32, uid: u32, gid: u32) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_socket()
            && metadata.uid() == uid
            && metadata.gid() == gid
            && metadata.mode() & 0o7777 == mode
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BrokerIdentity {
    uid: u32,
    gid: u32,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn broker_identity() -> Result<BrokerIdentity, ServiceError> {
    let user = User::from_name(BROKER_ACCOUNT)
        .map_err(|_| ServiceError::new(ServiceErrorCode::BrokerAccountUnavailable))?
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::BrokerAccountUnavailable))?;
    let uid = user.uid.as_raw();
    let gid = user.gid.as_raw();
    if uid == 0 || gid == 0 {
        return Err(ServiceError::new(
            ServiceErrorCode::BrokerAccountUnavailable,
        ));
    }
    Ok(BrokerIdentity { uid, gid })
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ConnectionLimiter {
    active: Mutex<usize>,
    limit: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    use exacl::{AclEntry, Perm, setfacl};
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pkg-macos-service-{}-{sequence}",
                std::process::id()
            ));
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

    fn expectation(path: &Path, mode: u32) -> DirectoryExpectation<'_> {
        DirectoryExpectation {
            path,
            uid: Uid::effective().as_raw(),
            gid: Gid::effective().as_raw(),
            mode,
        }
    }

    #[test]
    fn fixed_listener_binds_exact_mode_and_replaces_only_exact_stale_socket()
    -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let endpoint = scratch.0.join("broker.sock");
        let parents = [expectation(&scratch.0, 0o700)];
        let uid = Uid::effective().as_raw();
        let gid = Gid::effective().as_raw();

        let first = bind_macos_listener(&endpoint, 0o600, uid, gid, &parents)?;
        assert!(socket_metadata_matches(&endpoint, 0o600, uid, gid));
        drop(first);
        let second = bind_macos_listener(&endpoint, 0o600, uid, gid, &parents)?;
        assert!(socket_metadata_matches(&endpoint, 0o600, uid, gid));
        drop(second);
        Ok(())
    }

    #[test]
    fn unsafe_socket_or_parent_is_refused_without_deleting_it() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let endpoint = scratch.0.join("broker.sock");
        fs::write(&endpoint, b"not a socket")?;
        let parents = [expectation(&scratch.0, 0o700)];
        let uid = Uid::effective().as_raw();
        let gid = Gid::effective().as_raw();
        assert_eq!(
            bind_macos_listener(&endpoint, 0o600, uid, gid, &parents)
                .err()
                .map(ServiceError::code),
            Some(ServiceErrorCode::InvalidServiceSocket)
        );
        assert_eq!(fs::read(&endpoint)?, b"not a socket");

        fs::remove_file(&endpoint)?;
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755))?;
        assert_eq!(
            bind_macos_listener(&endpoint, 0o600, uid, gid, &parents)
                .err()
                .map(ServiceError::code),
            Some(ServiceErrorCode::InvalidServiceSocket)
        );
        assert!(!endpoint.exists());
        Ok(())
    }

    #[test]
    fn symlinked_socket_parent_is_refused() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let real = scratch.0.join("real");
        let linked = scratch.0.join("linked");
        fs::create_dir(&real)?;
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700))?;
        symlink(&real, &linked)?;
        let endpoint = linked.join("broker.sock");
        let parents = [expectation(&linked, 0o700)];
        assert_eq!(
            bind_macos_listener(
                &endpoint,
                0o600,
                Uid::effective().as_raw(),
                Gid::effective().as_raw(),
                &parents,
            )
            .err()
            .map(ServiceError::code),
            Some(ServiceErrorCode::InvalidServiceSocket)
        );
        assert!(!real.join("broker.sock").exists());
        Ok(())
    }

    #[test]
    fn extended_acl_on_socket_parent_is_refused() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let endpoint = scratch.0.join("broker.sock");
        let user = User::from_uid(Uid::effective())?
            .ok_or_else(|| io::Error::other("effective user is unavailable"))?;
        let acl = [AclEntry::allow_user(&user.name, Perm::READ, None)];
        setfacl(&[scratch.0.as_path()], &acl, None)?;
        assert!(!getfacl(&scratch.0, None)?.is_empty());

        let parents = [expectation(&scratch.0, 0o700)];
        assert_eq!(
            bind_macos_listener(
                &endpoint,
                0o600,
                Uid::effective().as_raw(),
                Gid::effective().as_raw(),
                &parents,
            )
            .err()
            .map(ServiceError::code),
            Some(ServiceErrorCode::InvalidServiceSocket)
        );
        assert!(!endpoint.exists());
        Ok(())
    }
}
