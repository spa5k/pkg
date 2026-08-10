//! Production service entry points with fixed socket and identity contracts.

#[cfg(target_os = "linux")]
use crate::{LinuxHelperSession, LinuxRootSetStore, serve_helper_connection};
#[cfg(target_os = "linux")]
use listenfd::ListenFd;
#[cfg(target_os = "linux")]
use nix::unistd::{Uid, User};
#[cfg(target_os = "linux")]
use pkg_nix::{InProcessHelper, InProcessPeer, RootNixRepairExecutor, VerifiedRepairExecutor};
use std::{error::Error, fmt};
#[cfg(target_os = "linux")]
use std::{io, os::unix::net::UnixListener, path::Path, sync::Arc};

#[cfg(target_os = "linux")]
const BROKER_ACCOUNT: &str = "pkg-nix-broker";
#[cfg(target_os = "linux")]
const LINUX_HELPER_SOCKET: &str = "/run/pkg-helper/root-helper.sock";
#[cfg(target_os = "linux")]
const MANAGED_NIX_BINARY: &str = "/opt/pkg/nix/current/bin/nix";
#[cfg(target_os = "linux")]
const LINUX_HELPER_HOME: &str = "/var/lib/pkg/helper-home";

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
}
