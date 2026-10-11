//! Host macOS version reading for the cask runtime range gate.
//!
//! The version comes from the ABSOLUTE `/usr/bin/sw_vers` path through the
//! shared child signal boundary, so an interrupted read is classified and
//! pkg stays alive. No environment override exists in production: the
//! native macOS parent tests this against the real host.

use super::process::Outcome;

/// The absolute path of the host version tool.
const SW_VERS: &str = "/usr/bin/sw_vers";

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
/// Returns an error if the native command cannot be spawned, fails, is interrupted, or returns invalid output.
pub fn macos_product_version() -> Result<String, String> {
    let args = vec![String::from("-productVersion")];
    let (outcome, stdout) =
        super::process::run_direct_captured(std::path::Path::new(SW_VERS), &args)?;
    match outcome {
        Outcome::Success => {}
        Outcome::Interrupted { .. } => {
            return Err(String::from("the version read was interrupted"));
        }
        Outcome::Failed { status, .. } => {
            return Err(format!("{SW_VERS} failed: {status}"));
        }
    }
    let version = stdout.trim();
    if version.is_empty() || version.contains('\n') || version.contains(' ') {
        return Err(format!(
            "{SW_VERS} answered an unusable version {version:?}"
        ));
    }
    if parse_numeric_version(version).is_none() {
        return Err(format!(
            "{SW_VERS} answered a non-numeric version {version:?}"
        ));
    }
    Ok(version.to_string())
}
