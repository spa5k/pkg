//! Closed lifecycle boundary for the product-managed Nix daemon.

use std::fmt;
use std::path::Path;

use pkg_core::System;

use crate::NixVersion;

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
