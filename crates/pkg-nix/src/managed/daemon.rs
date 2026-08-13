//! Closed lifecycle boundary for the product-managed Nix daemon.

use std::fmt;
use std::path::Path;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt as _;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

use pkg_core::System;

use crate::NixVersion;
#[cfg(unix)]
use crate::real::{
    MANAGED_NIX_CONFIG, MANAGED_NIX_STATE, MANAGED_PATH, PINNED_NIX_VERSION, RealNixAdapter,
    terminate_and_reap,
};

#[cfg(unix)]
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const DAEMON_READY_POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(unix)]
const REGISTRATION_POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const MAX_REGISTRATION_ENTRIES: usize = 64;
#[cfg(unix)]
const STORE_LINKS: &str = "/nix/store/.links";

/// Stable daemon lifecycle failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonErrorCode {
    /// The authenticated upstream registration could not initialize the store database.
    RegistrationFailed,
    /// Attempt-owned store registration state could not be removed safely.
    RegistrationRollbackFailed,
    /// The platform service definition could not be loaded or started.
    StartFailed,
    /// The managed daemon did not answer its bounded store health check.
    ReadinessFailed,
    /// Rollback could not stop the partially activated daemon.
    StopFailed,
}

/// Redacted managed-daemon error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonError {
    code: DaemonErrorCode,
}

impl DaemonError {
    /// Constructs a closed daemon failure without carrying host output.
    #[must_use]
    pub const fn new(code: DaemonErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> DaemonErrorCode {
        self.code
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "managed Nix daemon failed: {:?}", self.code)
    }
}

impl std::error::Error for DaemonError {}

/// Platform-specific service activation hidden behind a closed product API.
///
/// Implementations are provided by the privileged Linux/macOS installer
/// layers. They may invoke the bundled `nix-daemon`, systemd, or launchd, but
/// callers cannot pass argv, environment, sockets, or arbitrary service names.
pub trait ManagedDaemon: Send + Sync {
    /// Registers the authenticated upstream runtime closure in the fixed local store.
    fn register_runtime(
        &self,
        installation_root: &Path,
        system: System,
        version: &NixVersion,
        registration: &Path,
    ) -> Result<(), DaemonError>;

    /// Releases rollback ownership after the platform install commits.
    fn commit_runtime_registration(&self) -> Result<(), DaemonError>;

    /// Removes only the store-database paths created by this install attempt.
    fn rollback_runtime_registration(&self) -> Result<(), DaemonError>;

    /// Starts the one fixed managed service for the authenticated runtime.
    fn start(
        &self,
        installation_root: &Path,
        system: System,
        version: &NixVersion,
    ) -> Result<(), DaemonError>;

    /// Performs the fixed bounded equivalent of `nix ping-store`.
    fn ping_store(&self) -> Result<(), DaemonError>;

    /// Stops the fixed managed service during rollback.
    fn stop(&self) -> Result<(), DaemonError>;
}

/// Production bootstrap lifecycle for the fixed product-managed Nix daemon.
///
/// This process exists only while the privileged installer verifies the new
/// runtime. The platform backend replaces it with the fixed systemd or launchd
/// service after the transaction succeeds.
#[cfg(unix)]
pub struct ProductionManagedDaemon {
    state: Mutex<ProductionDaemonState>,
}

#[cfg(unix)]
struct ProductionDaemonState {
    child: Option<Child>,
    adapter: Option<Arc<RealNixAdapter>>,
    socket: Option<PathBuf>,
    registration: Option<Vec<OwnedRegistrationEntry>>,
}

#[cfg(unix)]
struct OwnedRegistrationEntry {
    path: PathBuf,
    device: u64,
    inode: u64,
    directory: bool,
}

#[cfg(unix)]
struct DaemonLaunchContract {
    binary: PathBuf,
    home: PathBuf,
    temporary: PathBuf,
    socket: PathBuf,
    arguments: [OsString; 1],
}

#[cfg(unix)]
impl fmt::Debug for ProductionManagedDaemon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionManagedDaemon")
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
impl ProductionManagedDaemon {
    /// Constructs the fixed production daemon lifecycle.
    #[must_use]
    pub const fn production() -> Self {
        Self {
            state: Mutex::new(ProductionDaemonState {
                child: None,
                adapter: None,
                socket: None,
                registration: None,
            }),
        }
    }
}

#[cfg(unix)]
impl Default for ProductionManagedDaemon {
    fn default() -> Self {
        Self::production()
    }
}

#[cfg(unix)]
impl ManagedDaemon for ProductionManagedDaemon {
    fn register_runtime(
        &self,
        installation_root: &Path,
        system: System,
        version: &NixVersion,
        registration: &Path,
    ) -> Result<(), DaemonError> {
        let contract = registration_contract(installation_root, system, version, registration)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
        if state.child.is_some() || state.registration.is_some() {
            return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
        }
        require_fresh_registration_state()?;
        let input = File::open(registration)
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
        let mut command = Command::new(&contract.binary);
        command
            .arg("--load-db")
            .env_clear()
            .env("HOME", &contract.home)
            .env("TMPDIR", &contract.temporary)
            .env("NIX_CONFIG", MANAGED_NIX_CONFIG)
            .env("NIX_STATE_DIR", MANAGED_NIX_STATE)
            .env("NIX_USER_CONF_FILES", "")
            .env("PATH", MANAGED_PATH)
            .current_dir(&contract.home)
            .stdin(Stdio::from(input))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < REGISTRATION_TIMEOUT => {
                    thread::sleep(REGISTRATION_POLL);
                }
                Ok(None) | Err(_) => {
                    let _ = terminate_and_reap(&mut child, None);
                    let _ = cleanup_registration_state(None);
                    return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
                }
            }
        };
        let _ = terminate_and_reap(&mut child, Some(status));
        if !status.success() {
            let _ = cleanup_registration_state(None);
            return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
        }
        let entries = capture_registration_state(true).inspect_err(|_| {
            let _ = cleanup_registration_state(None);
        })?;
        state.registration = Some(entries);
        Ok(())
    }

    fn commit_runtime_registration(&self) -> Result<(), DaemonError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
        if state.registration.take().is_none() {
            return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
        }
        Ok(())
    }

    fn rollback_runtime_registration(&self) -> Result<(), DaemonError> {
        let entries = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationRollbackFailed))?;
            if state.child.is_some() {
                return Err(DaemonError::new(
                    DaemonErrorCode::RegistrationRollbackFailed,
                ));
            }
            state.registration.take()
        };
        let store_links = capture_owned_registration_paths([PathBuf::from(STORE_LINKS)], false)
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationRollbackFailed))?;
        cleanup_registration_state(Some(&store_links))
            .and_then(|()| cleanup_registration_state(entries.as_deref()))
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationRollbackFailed))
    }

    fn start(
        &self,
        installation_root: &Path,
        system: System,
        version: &NixVersion,
    ) -> Result<(), DaemonError> {
        let contract = launch_contract(installation_root, system, version)?;
        validate_launch_contract(&contract)?;
        let adapter = Arc::new(
            RealNixAdapter::new_with_daemon_socket(
                &contract.binary,
                &contract.home,
                &contract.socket,
            )
            .map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?,
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?;
        if state.child.is_some() {
            return Err(DaemonError::new(DaemonErrorCode::StartFailed));
        }
        let mut command = Command::new(&contract.binary);
        command
            .args(&contract.arguments)
            .env_clear()
            .env("HOME", &contract.home)
            .env("TMPDIR", &contract.temporary)
            .env("NIX_CONFIG", MANAGED_NIX_CONFIG)
            .env("NIX_DAEMON_SOCKET_PATH", &contract.socket)
            .env("NIX_STATE_DIR", MANAGED_NIX_STATE)
            .env("NIX_USER_CONF_FILES", "")
            .env("PATH", MANAGED_PATH)
            .current_dir(&contract.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        #[cfg(target_os = "linux")]
        {
            let installer_pid = std::process::id();
            // SAFETY: `arm_parent_death_signal` only invokes async-signal-safe
            // rustix syscall wrappers and allocates nothing, so it satisfies the
            // `pre_exec` contract for code run after `fork` and before `exec`.
            unsafe {
                command.pre_exec(move || arm_parent_death_signal(installer_pid));
            }
        }
        let child = command
            .spawn()
            .map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?;
        state.child = Some(child);
        state.adapter = Some(adapter);
        state.socket = Some(contract.socket);
        Ok(())
    }

    fn ping_store(&self) -> Result<(), DaemonError> {
        let started = Instant::now();
        loop {
            let adapter = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| DaemonError::new(DaemonErrorCode::ReadinessFailed))?;
                let child_is_running = state
                    .child
                    .as_mut()
                    .and_then(|child| child.try_wait().ok())
                    .is_some_and(|status| status.is_none());
                child_is_running
                    .then(|| state.adapter.as_ref().map(Arc::clone))
                    .flatten()
            };
            let Some(adapter) = adapter else {
                return Err(self.cleanup_after_readiness_failure());
            };
            if adapter.ping_managed_store().is_ok() {
                return Ok(());
            }
            if started.elapsed() >= DAEMON_READY_TIMEOUT {
                return Err(self.cleanup_after_readiness_failure());
            }
            thread::sleep(DAEMON_READY_POLL);
        }
    }

    fn stop(&self) -> Result<(), DaemonError> {
        let (child, socket) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| DaemonError::new(DaemonErrorCode::StopFailed))?;
            state.adapter = None;
            (state.child.take(), state.socket.take())
        };
        let mut failed = false;
        if let Some(mut child) = child {
            failed = terminate_and_reap(&mut child, None).is_err();
        }
        if let Some(socket) = socket
            && let Err(error) = fs::remove_file(socket)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            failed = true;
        }
        if failed {
            return Err(DaemonError::new(DaemonErrorCode::StopFailed));
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for ProductionManagedDaemon {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(unix)]
impl ProductionManagedDaemon {
    fn cleanup_after_readiness_failure(&self) -> DaemonError {
        self.stop()
            .err()
            .unwrap_or_else(|| DaemonError::new(DaemonErrorCode::ReadinessFailed))
    }
}

/// Pure decision behind the parent-death race guard for the daemon child.
///
/// `PR_SET_PDEATHSIG` fires on the death of whatever process is the child's
/// parent at the moment the signal is armed. If the installer died between
/// `fork` and arming the signal, the kernel has already reparented the child,
/// so the observed parent pid no longer matches the installer. The decision is
/// kept host-independent so the fail-closed semantics can be tested directly.
#[cfg(any(target_os = "linux", test))]
fn parent_death_race_lost(installer_pid: u32, observed_parent: Option<u32>) -> bool {
    observed_parent != Some(installer_pid)
}

/// Arms the Linux parent-death signal on the daemon child before `exec`.
///
/// This runs inside the child via `pre_exec`, after `fork` and before `exec`,
/// so it must stay async-signal-safe: it only calls syscall wrappers and
/// performs no allocation. `PR_SET_PDEATHSIG` makes the kernel deliver
/// `SIGKILL` to the daemon when the installer (its parent) dies, so a daemon
/// started by an installer that is killed can never outlive that installer. The
/// `getppid` guard closes the fork-to-`prctl` race described above.
#[cfg(target_os = "linux")]
fn arm_parent_death_signal(installer_pid: u32) -> std::io::Result<()> {
    use rustix::process::{Signal, getppid, set_parent_process_death_signal};

    set_parent_process_death_signal(Some(Signal::KILL))?;
    let observed_parent = getppid().map(|pid| pid.as_raw_nonzero().get() as u32);
    if parent_death_race_lost(installer_pid, observed_parent) {
        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
    }
    Ok(())
}

#[cfg(unix)]
fn launch_contract(
    installation_root: &Path,
    system: System,
    version: &NixVersion,
) -> Result<DaemonLaunchContract, DaemonError> {
    if installation_root != Path::new("/")
        || version.as_str() != PINNED_NIX_VERSION
        || !native_system(system)
    {
        return Err(DaemonError::new(DaemonErrorCode::StartFailed));
    }
    let binary = Path::new("/opt/pkg/nix")
        .join(version.as_str())
        .join("bin/nix");
    let home = if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        PathBuf::from("/var/lib/pkg/helper-home")
    } else {
        PathBuf::from("/Library/Application Support/pkg/helper-home")
    };
    let socket = home.join("tmp/nix-daemon.socket");
    Ok(DaemonLaunchContract {
        binary,
        temporary: home.join("tmp"),
        home,
        socket,
        arguments: [OsString::from("daemon")],
    })
}

#[cfg(unix)]
fn registration_contract(
    installation_root: &Path,
    system: System,
    version: &NixVersion,
    registration: &Path,
) -> Result<DaemonLaunchContract, DaemonError> {
    let registration_error = || DaemonError::new(DaemonErrorCode::RegistrationFailed);
    let mut contract =
        launch_contract(installation_root, system, version).map_err(|_| registration_error())?;
    contract.binary = Path::new("/opt/pkg/nix")
        .join(version.as_str())
        .join("bin/nix-store");
    validate_file(&contract.binary, true).map_err(|_| registration_error())?;
    validate_file(registration, false).map_err(|_| registration_error())?;
    validate_private_directory(&contract.home).map_err(|_| registration_error())?;
    validate_private_directory(&contract.temporary).map_err(|_| registration_error())?;
    Ok(contract)
}

#[cfg(unix)]
fn registration_roots() -> [PathBuf; 4] {
    [
        PathBuf::from("/nix/var/nix/db"),
        PathBuf::from("/nix/var/nix/profiles"),
        PathBuf::from("/nix/var/nix/temproots"),
        PathBuf::from("/nix/var/nix/gcroots"),
    ]
}

#[cfg(unix)]
fn require_fresh_registration_state() -> Result<(), DaemonError> {
    if registration_roots()
        .iter()
        .any(|path| fs::symlink_metadata(path).is_ok())
        || fs::symlink_metadata(STORE_LINKS).is_ok()
    {
        return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
    }
    Ok(())
}

#[cfg(unix)]
fn capture_registration_state(
    require_every_root: bool,
) -> Result<Vec<OwnedRegistrationEntry>, DaemonError> {
    capture_owned_registration_paths(registration_roots(), require_every_root)
}

#[cfg(unix)]
fn capture_owned_registration_paths<const N: usize>(
    roots: [PathBuf; N],
    require_every_root: bool,
) -> Result<Vec<OwnedRegistrationEntry>, DaemonError> {
    let mut pending = Vec::new();
    for root in roots {
        match fs::symlink_metadata(&root) {
            Ok(_) => pending.push(root),
            Err(error) if !require_every_root && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed)),
        }
    }
    let mut entries = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
        if metadata.uid() != 0
            || metadata.file_type().is_symlink()
            || !(metadata.file_type().is_dir() || metadata.file_type().is_file())
            || metadata.mode() & 0o022 != 0
        {
            return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
        }
        if entries.len() >= MAX_REGISTRATION_ENTRIES {
            return Err(DaemonError::new(DaemonErrorCode::RegistrationFailed));
        }
        if metadata.file_type().is_dir() {
            let children = fs::read_dir(&path)
                .map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
            for child in children {
                let child =
                    child.map_err(|_| DaemonError::new(DaemonErrorCode::RegistrationFailed))?;
                pending.push(child.path());
            }
        }
        entries.push(OwnedRegistrationEntry {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            directory: metadata.file_type().is_dir(),
        });
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    Ok(entries)
}

#[cfg(unix)]
fn cleanup_registration_state(
    captured: Option<&[OwnedRegistrationEntry]>,
) -> Result<(), DaemonError> {
    let discovered;
    let owned = if let Some(entries) = captured {
        entries
    } else {
        discovered = capture_registration_state(false)?;
        &discovered
    };
    let mut failed = false;
    for entry in owned {
        let result = match fs::symlink_metadata(&entry.path) {
            Ok(metadata)
                if metadata.dev() == entry.device
                    && metadata.ino() == entry.inode
                    && metadata.file_type().is_dir() == entry.directory =>
            {
                if entry.directory {
                    fs::remove_dir(&entry.path)
                } else {
                    fs::remove_file(&entry.path)
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) | Err(_) => Err(std::io::Error::other("registration identity changed")),
        };
        failed |= result.is_err();
    }
    if failed {
        Err(DaemonError::new(
            DaemonErrorCode::RegistrationRollbackFailed,
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn validate_launch_contract(contract: &DaemonLaunchContract) -> Result<(), DaemonError> {
    validate_file(&contract.binary, true)?;
    validate_file(Path::new("/opt/pkg/etc/pkg/nix.conf"), false)?;
    validate_private_directory(&contract.home)?;
    validate_private_directory(&contract.temporary)?;
    let socket_parent = contract
        .socket
        .parent()
        .ok_or_else(|| DaemonError::new(DaemonErrorCode::StartFailed))?;
    validate_socket_parent(socket_parent)?;
    prepare_bootstrap_socket(&contract.socket)
}

#[cfg(unix)]
fn prepare_bootstrap_socket(path: &Path) -> Result<(), DaemonError> {
    let metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) => metadata,
        Err(_) => return Err(DaemonError::new(DaemonErrorCode::StartFailed)),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != 0 {
        return Err(DaemonError::new(DaemonErrorCode::StartFailed));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(DaemonError::new(DaemonErrorCode::StartFailed)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path).map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))
        }
        Err(_) => Err(DaemonError::new(DaemonErrorCode::StartFailed)),
    }
}

#[cfg(unix)]
fn validate_file(path: &Path, executable: bool) -> Result<(), DaemonError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || executable && metadata.mode() & 0o111 == 0
    {
        return Err(DaemonError::new(DaemonErrorCode::StartFailed));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<(), DaemonError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?;
    if !metadata.file_type().is_dir() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(DaemonError::new(DaemonErrorCode::StartFailed));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_socket_parent(path: &Path) -> Result<(), DaemonError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| DaemonError::new(DaemonErrorCode::StartFailed))?;
    if !metadata.file_type().is_dir() || metadata.uid() != 0 || metadata.mode() & 0o007 != 0 {
        return Err(DaemonError::new(DaemonErrorCode::StartFailed));
    }
    Ok(())
}

#[cfg(unix)]
const fn native_system(system: System) -> bool {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return matches!(system, System::X8664Linux);
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return matches!(system, System::Aarch64Linux);
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return matches!(system, System::X8664Darwin);
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return matches!(system, System::Aarch64Darwin);
    #[allow(unreachable_code)]
    false
}

/// Closed unsupported-platform form of the production daemon lifecycle.
#[cfg(not(unix))]
pub struct ProductionManagedDaemon;

#[cfg(not(unix))]
impl ProductionManagedDaemon {
    /// Constructs the unsupported-platform lifecycle.
    #[must_use]
    pub const fn production() -> Self {
        Self
    }
}

#[cfg(not(unix))]
impl Default for ProductionManagedDaemon {
    fn default() -> Self {
        Self::production()
    }
}

#[cfg(not(unix))]
impl fmt::Debug for ProductionManagedDaemon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionManagedDaemon")
            .finish_non_exhaustive()
    }
}

#[cfg(not(unix))]
impl ManagedDaemon for ProductionManagedDaemon {
    fn register_runtime(
        &self,
        _installation_root: &Path,
        _system: System,
        _version: &NixVersion,
        _registration: &Path,
    ) -> Result<(), DaemonError> {
        Err(DaemonError::new(DaemonErrorCode::RegistrationFailed))
    }

    fn commit_runtime_registration(&self) -> Result<(), DaemonError> {
        Err(DaemonError::new(DaemonErrorCode::RegistrationFailed))
    }

    fn rollback_runtime_registration(&self) -> Result<(), DaemonError> {
        Err(DaemonError::new(
            DaemonErrorCode::RegistrationRollbackFailed,
        ))
    }

    fn start(
        &self,
        _installation_root: &Path,
        _system: System,
        _version: &NixVersion,
    ) -> Result<(), DaemonError> {
        Err(DaemonError::new(DaemonErrorCode::StartFailed))
    }

    fn ping_store(&self) -> Result<(), DaemonError> {
        Err(DaemonError::new(DaemonErrorCode::ReadinessFailed))
    }

    fn stop(&self) -> Result<(), DaemonError> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn production_launch_contract_is_fixed_native_and_version_pinned() {
        let system = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                System::Aarch64Darwin
            } else {
                System::X8664Darwin
            }
        } else if cfg!(target_arch = "aarch64") {
            System::Aarch64Linux
        } else {
            System::X8664Linux
        };
        let version = NixVersion::new(PINNED_NIX_VERSION).unwrap();
        let contract = launch_contract(Path::new("/"), system, &version).unwrap();

        assert_eq!(contract.binary, Path::new("/opt/pkg/nix/2.34.8/bin/nix"));
        assert_eq!(contract.arguments, [OsString::from("daemon")]);
        assert_ne!(
            contract.socket,
            Path::new(crate::real::MANAGED_DAEMON_SOCKET)
        );
        assert!(launch_contract(Path::new("/tmp/root"), system, &version).is_err());
        assert!(
            launch_contract(Path::new("/"), system, &NixVersion::new("2.34.7").unwrap()).is_err()
        );
    }

    #[test]
    fn production_daemon_debug_exposes_no_process_or_path_state() {
        assert_eq!(
            format!("{:?}", ProductionManagedDaemon::production()),
            "ProductionManagedDaemon { .. }"
        );
    }

    #[test]
    fn parent_death_race_guard_treats_reparenting_as_lost() {
        // The installer is still the child's parent: race not lost.
        assert!(!parent_death_race_lost(1000, Some(1000)));
        // Reparented to init (or a subreaper) while arming the signal: lost.
        assert!(parent_death_race_lost(1000, Some(1)));
        assert!(parent_death_race_lost(1000, Some(42)));
        // A pid of zero (getppid returned None) cannot confirm ownership: lost.
        assert!(parent_death_race_lost(1000, None));
    }
}
