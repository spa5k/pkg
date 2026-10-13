//! Host macOS version reading for the cask runtime range gate.
//!
//! The version comes from the ABSOLUTE `/usr/bin/sw_vers` path through the
//! shared child signal boundary, so an interrupted read is classified and
//! pkg stays alive. No environment override exists in production: the
//! native macOS parent tests this against the real host.

use super::process::Outcome;

/// The absolute path of the host version tool.
const SW_VERS: &str = "/usr/bin/sw_vers";

/// Failure to read the host version, with cancellation retained.
#[derive(Debug, thiserror::Error)]
pub enum HostVersionError {
    /// The version tool could not be supervised.
    #[error("could not read the host macOS version: {0}")]
    Process(#[from] pkg_process::RunError),
    /// The version tool was cancelled.
    #[error("the version read was interrupted by signal {0}")]
    Interrupted(i32),
    /// The tool failed or did not return a usable version.
    #[error("{0}")]
    Failed(String),
}

impl HostVersionError {
    /// The cancellation signal, including a failed supervision.
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::Process(error) => error.cancelled,
            Self::Interrupted(signal) => Some(*signal),
            Self::Failed(_) => None,
        }
    }
}

pub use cask_catalog::version::{in_range, parse as parse_numeric_version};

/// Read the host macOS product version, for example `15.7.7`.
///
/// Returns `Err` when the tool cannot run or answers anything but a
/// single non-empty NUMERIC version line: success output is validated
/// through the same strict parser the bounds use. The caller fails
/// closed on that error. This function must only be called for a macOS
/// cask gate decision — never for Linux, Nixpkgs, or untargeted
/// catalogs.
/// # Errors
///
/// Returns an error if `/usr/bin/sw_vers` cannot start or complete, its output cannot be read, or it does not return one numeric version line.
pub fn macos_product_version() -> Result<String, HostVersionError> {
    let (outcome, stdout) =
        super::process::run_direct_captured(std::path::Path::new(SW_VERS), &["-productVersion"])?;
    match outcome {
        Outcome::Success => {}
        Outcome::Interrupted { signal } => return Err(HostVersionError::Interrupted(signal)),
        Outcome::Failed { status, .. } => {
            return Err(HostVersionError::Failed(format!(
                "{SW_VERS} failed: {status}"
            )));
        }
    }
    let version = stdout.trim();
    if version.is_empty() || version.contains('\n') || version.contains(' ') {
        return Err(HostVersionError::Failed(format!(
            "{SW_VERS} answered an unusable version {version:?}"
        )));
    }
    if parse_numeric_version(version).is_none() {
        return Err(HostVersionError::Failed(format!(
            "{SW_VERS} answered a non-numeric version {version:?}"
        )));
    }
    Ok(version.to_string())
}
