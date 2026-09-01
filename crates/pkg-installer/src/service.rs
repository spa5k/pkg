//! Production service entry points with fixed socket and identity contracts.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::helper::ConnectionLimiter;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::{
    BrokerApprovalAudit, BrokerRepairJournals, ChannelRefreshDispatch, ProductionRepairAuthority,
    RootHelperClient, serve_broker_connection_with_product_authority, serve_helper_connection,
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
use nix::fcntl::{OFlag, open, openat, renameat};
#[cfg(target_os = "macos")]
use nix::sys::stat::{FchmodatFlags, Mode, fchmodat, mkdirat};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use nix::unistd::{Gid, Uid, User};
#[cfg(target_os = "macos")]
use nix::unistd::{UnlinkatFlags, unlinkat};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pkg_channel::{ChannelClient, TrustedRoot};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pkg_nix::{
    ChannelRefreshErrorCode, ChannelRefreshReport, InProcessBroker, InProcessHelper, InProcessPeer,
    RealNixAdapter, RootNixRepairExecutor, VerifiedRepairExecutor,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pkg_pipeline::{
    AuthenticatedBuildAuthorityService, BuildAuthorityRefreshErrorCode, BuildAuthorityUpdate,
    BuildPlanningAdapter,
};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{error::Error, fmt};
#[cfg(target_os = "macos")]
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{io, os::unix::net::UnixListener, path::Path, sync::Arc, thread};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::runtime::{Builder, Handle};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use url::Url;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const BROKER_ACCOUNT: &str = "pkg-nix-broker";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_HELPER_WORKERS: usize = 8;
/// Fixed Linux CLI-to-broker endpoint installed by pkg.
pub const LINUX_BROKER_SOCKET: &str = "/run/pkg/broker.sock";
#[cfg(target_os = "linux")]
const LINUX_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
const RELEASE_TUF_ROOT_JSON: Option<&str> = option_env!("PKG_RELEASE_TUF_ROOT_JSON");
#[cfg(any(target_os = "linux", target_os = "macos"))]
const RELEASE_CHANNEL_METADATA_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_METADATA_URL");
#[cfg(any(target_os = "linux", target_os = "macos"))]
const RELEASE_CHANNEL_TARGETS_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_TARGETS_URL");

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ProductionChannelRefresh {
    service: Arc<AuthenticatedBuildAuthorityService>,
    runtime: Handle,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ChannelRefreshDispatch for ProductionChannelRefresh {
    fn refresh(
        &self,
        mode: pkg_nix::ChannelRefreshMode,
    ) -> Result<ChannelRefreshReport, ChannelRefreshErrorCode> {
        self.runtime
            .block_on(async {
                match mode {
                    pkg_nix::ChannelRefreshMode::Check => self.service.check_with_sequence().await,
                    pkg_nix::ChannelRefreshMode::Apply | pkg_nix::ChannelRefreshMode::Force => {
                        self.service.refresh_with_sequence().await
                    }
                }
            })
            .map(|(update, sequence)| {
                ChannelRefreshReport::new(update == BuildAuthorityUpdate::Updated, sequence)
            })
            .map_err(|error| match error.code() {
                BuildAuthorityRefreshErrorCode::Network => ChannelRefreshErrorCode::Network,
                BuildAuthorityRefreshErrorCode::Busy => ChannelRefreshErrorCode::Busy,
                BuildAuthorityRefreshErrorCode::Verification => {
                    ChannelRefreshErrorCode::Verification
                }
                BuildAuthorityRefreshErrorCode::Service => {
                    ChannelRefreshErrorCode::ServiceUnavailable
                }
            })
    }
}

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
/// Before serving, it requires release-embedded TUF root/origin inputs,
/// authenticates the native channel and index into a long-lived build
/// authority, and installs the fixed privileged root-publication client. No
/// command caller or runtime environment can select those trust inputs.
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
    let repair_journals = BrokerRepairJournals::open(log, expected_uid)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    #[cfg(target_os = "linux")]
    let adapter = Arc::new(
        RealNixAdapter::new_standard_determinate(home)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    #[cfg(target_os = "macos")]
    let adapter = Arc::new(
        RealNixAdapter::new_standard_determinate(home)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let planning_adapter: Arc<dyn BuildPlanningAdapter> =
        Arc::clone(&adapter) as Arc<dyn BuildPlanningAdapter>;
    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("pkg-trust-runtime")
        .enable_all()
        .build()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let channel = production_channel(&home.join("channel"))?;
    let authority_service = Arc::new(
        runtime
            .block_on(AuthenticatedBuildAuthorityService::bootstrap(
                channel,
                planning_adapter,
            ))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let authority = authority_service.authority();
    let refresh = Arc::new(ProductionChannelRefresh {
        service: Arc::clone(&authority_service),
        runtime: runtime.handle().clone(),
    });
    let roots = Arc::new(RootHelperClient::production());
    let repair = Arc::new(ProductionRepairAuthority::new(
        Arc::clone(&adapter),
        Arc::clone(&roots),
        Arc::clone(&authority),
        repair_journals,
    ));
    let limiter = ConnectionLimiter::new(MAX_BROKER_CONNECTIONS);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = limiter.try_acquire() else {
                    drop(stream);
                    continue;
                };
                let connection_broker = Arc::clone(&broker);
                let connection_authority = Arc::clone(&authority);
                let connection_roots = Arc::clone(&roots);
                let connection_approval_audit = approval_audit.clone();
                let connection_refresh = Arc::clone(&refresh);
                let connection_repair = Arc::clone(&repair);
                thread::Builder::new()
                    .name(String::from("pkg-broker-client"))
                    .spawn(move || {
                        let _permit = permit;
                        let _ = serve_broker_connection_with_product_authority(
                            stream,
                            &connection_broker,
                            &connection_approval_audit,
                            &connection_authority,
                            &connection_roots,
                            connection_refresh.as_ref(),
                            connection_repair.as_ref(),
                        );
                    })
                    .map_err(|_| ServiceError::new(ServiceErrorCode::WorkerUnavailable))?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ServiceError::new(ServiceErrorCode::ListenerFailed)),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn production_channel(datastore: &Path) -> Result<ChannelClient, ServiceError> {
    let (trusted_root, metadata_url, targets_url) = production_release_inputs()?;
    ChannelClient::new(trusted_root, metadata_url, targets_url, datastore)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

pub fn production_release_inputs() -> Result<(TrustedRoot, Url, Url), ServiceError> {
    release_inputs(
        RELEASE_TUF_ROOT_JSON,
        RELEASE_CHANNEL_METADATA_URL,
        RELEASE_CHANNEL_TARGETS_URL,
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg(test)]
fn channel_from_compiled_release(
    datastore: &Path,
    root_json: Option<&'static str>,
    metadata_url: Option<&str>,
    targets_url: Option<&str>,
) -> Result<ChannelClient, ServiceError> {
    let (trusted_root, metadata_url, targets_url) =
        release_inputs(root_json, metadata_url, targets_url)?;
    ChannelClient::new(trusted_root, metadata_url, targets_url, datastore)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))
}

fn release_inputs(
    root_json: Option<&'static str>,
    metadata_url: Option<&str>,
    targets_url: Option<&str>,
) -> Result<(TrustedRoot, Url, Url), ServiceError> {
    let root_json = required_release_value(root_json)?;
    let metadata_url = required_release_value(metadata_url)?;
    let targets_url = required_release_value(targets_url)?;
    let trusted_root = TrustedRoot::from_embedded(root_json.as_bytes())
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let metadata_url = Url::parse(metadata_url)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let targets_url =
        Url::parse(targets_url).map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    Ok((trusted_root, metadata_url, targets_url))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn required_release_value(value: Option<&str>) -> Result<&str, ServiceError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::InvalidRuntime))
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
        RootNixRepairExecutor::new_standard_determinate(Path::new(LINUX_HELPER_HOME))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let helper = InProcessHelper::with_repair_executor(broker_uid, repair_executor)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let authenticated = helper
        .connect(InProcessPeer::authenticated_uid(broker_uid))
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let roots = LinuxRootSetStore::production()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let session = Arc::new(LinuxHelperSession::new(authenticated, roots));
    let workers = ConnectionLimiter::new(MAX_HELPER_WORKERS);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = workers.try_acquire() else {
                    continue;
                };
                let session = Arc::clone(&session);
                thread::Builder::new()
                    .name("pkg-root-helper".to_owned())
                    .spawn(move || {
                        let _permit = permit;
                        let _ = serve_helper_connection(stream, broker_uid, session.as_ref());
                    })
                    .map_err(|_| ServiceError::new(ServiceErrorCode::WorkerUnavailable))?;
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
        RootNixRepairExecutor::new_standard_determinate(Path::new(MACOS_HELPER_HOME))
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?,
    );
    let helper = InProcessHelper::with_repair_executor(identity.uid, repair_executor)
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let authenticated = helper
        .connect(InProcessPeer::authenticated_uid(identity.uid))
        .map_err(|_| ServiceError::new(ServiceErrorCode::InitializationFailed))?;
    let roots = MacOsRootSetStore::production()
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidRuntime))?;
    let session = Arc::new(MacOsHelperSession::new(authenticated, roots));
    let workers = ConnectionLimiter::new(MAX_HELPER_WORKERS);

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = workers.try_acquire() else {
                    continue;
                };
                let session = Arc::clone(&session);
                thread::Builder::new()
                    .name("pkg-root-helper".to_owned())
                    .spawn(move || {
                        let _permit = permit;
                        let _ = serve_helper_connection(stream, identity.uid, session.as_ref());
                    })
                    .map_err(|_| ServiceError::new(ServiceErrorCode::WorkerUnavailable))?;
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

/// Distinguishes concurrent staging directories within one process.
#[cfg(target_os = "macos")]
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

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
    // macOS, so `fchmod` cannot secure that node. `umask` is process-global:
    // any parallel thread creating paths during a scoped window would inherit
    // the restricted mask (the parallel test suite proved this race). Bind
    // the socket inside a private staging directory instead, fix its exact
    // mode there, then rename it into place atomically. The final path never
    // carries a permissive mode.
    let endpoint_parent = path
        .parent()
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
    let socket_name = path
        .file_name()
        .ok_or_else(|| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
    let parent_fd = open(
        endpoint_parent,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
    // Keep the name short: macOS limits a Unix socket path to 104 bytes, and
    // the endpoint parent plus socket name already consumes most of it. The
    // pid scopes the name so two overlapping processes (e.g. a launchd
    // restart while the old broker still runs) can never collide; the seq
    // separates repeated binds within one process.
    let staging_name = format!(
        ".pkg-s{:x}-{:x}",
        std::process::id(),
        STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    // A crashed earlier run may have left this pid's stage directory behind.
    let _ = unlinkat(&parent_fd, staging_name.as_str(), UnlinkatFlags::RemoveDir);
    mkdirat(
        &parent_fd,
        staging_name.as_str(),
        Mode::from_bits_truncate(0o700),
    )
    .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
    let staging_fd = openat(
        &parent_fd,
        staging_name.as_str(),
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
    let staging_path = endpoint_parent.join(&staging_name).join(socket_name);
    let result = (|| {
        let listener = UnixListener::bind(&staging_path)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        let socket_mode = u16::try_from(mode)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        fchmodat(
            &staging_fd,
            socket_name,
            Mode::from_bits_truncate(socket_mode),
            FchmodatFlags::NoFollowSymlink,
        )
        .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        if !socket_metadata_matches(&staging_path, mode, owner_uid, group_gid) {
            return Err(ServiceError::new(ServiceErrorCode::InvalidServiceSocket));
        }
        renameat(&staging_fd, socket_name, &parent_fd, socket_name)
            .map_err(|_| ServiceError::new(ServiceErrorCode::InvalidServiceSocket))?;
        Ok(listener)
    })();
    // On success the staging dir is empty. On failure the bound socket may
    // still sit inside it; remove it so the dir cleanup below succeeds and
    // the next run has no residue to trip over.
    if result.is_err() {
        let _ = unlinkat(&staging_fd, socket_name, UnlinkatFlags::NoRemoveDir);
    }
    let _ = unlinkat(&parent_fd, staging_name.as_str(), UnlinkatFlags::RemoveDir);
    let listener = result?;
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pkg-linux-service-{}-{sequence}",
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod release_channel_tests {
    use super::*;
    use std::{error::Error, fs, os::unix::fs::PermissionsExt};

    fn private_datastore() -> Result<tempfile::TempDir, Box<dyn Error>> {
        let datastore = tempfile::tempdir()?;
        fs::set_permissions(datastore.path(), fs::Permissions::from_mode(0o700))?;
        Ok(datastore)
    }

    #[test]
    fn configuration_is_compile_time_complete_or_refused() -> Result<(), Box<dyn Error>> {
        let datastore = private_datastore()?;
        for configuration in [
            (
                None,
                Some("https://metadata.example/"),
                Some("https://targets.example/"),
            ),
            (Some("{}"), None, Some("https://targets.example/")),
            (Some("{}"), Some("https://metadata.example/"), None),
            (Some("{}"), Some("   "), Some("https://targets.example/")),
        ] {
            assert_eq!(
                channel_from_compiled_release(
                    datastore.path(),
                    configuration.0,
                    configuration.1,
                    configuration.2,
                )
                .err()
                .map(ServiceError::code),
                Some(ServiceErrorCode::InvalidRuntime)
            );
        }
        Ok(())
    }

    #[test]
    fn configuration_accepts_only_channel_validated_origins() -> Result<(), Box<dyn Error>> {
        let insecure = private_datastore()?;
        assert_eq!(
            channel_from_compiled_release(
                insecure.path(),
                Some("{}"),
                Some("http://metadata.example/"),
                Some("https://targets.example/"),
            )
            .err()
            .map(ServiceError::code),
            Some(ServiceErrorCode::InvalidRuntime)
        );

        let secure = private_datastore()?;
        let channel = channel_from_compiled_release(
            secure.path(),
            Some("{}"),
            Some("https://metadata.example/"),
            Some("https://targets.example/"),
        )?;
        drop(channel);
        Ok(())
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
