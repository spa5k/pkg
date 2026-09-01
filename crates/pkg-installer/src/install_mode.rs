//! The closed set of product operation modes shared by every platform.

use serde::{Deserialize, Serialize};

/// The durable product operation mode for one install or repair attempt.
///
/// Both platform backends run exactly the same three modes. The mode is
/// recorded in the platform journal and validated against the backend before
/// any mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallMode {
    /// Create a new installation and activate its product service set.
    FreshInstall,
    /// Replace an existing installation while its product service set stays
    /// offline.
    OfflineUpgrade,
    /// Keep authenticated same-release candidate bytes during offline repair.
    OfflineRepair,
}
