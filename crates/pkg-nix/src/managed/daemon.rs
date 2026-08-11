//! Closed lifecycle boundary for the product-managed Nix daemon.

use std::fmt;
use std::path::Path;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
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

/// Stable daemon lifecycle failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonErrorCode {
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
}
