//! Stable process exit codes shared by human and machine output.

use std::process::ExitCode as ProcessExitCode;

/// Product exit-code contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ExitCode {
    /// Successful completion.
    Ok = 0,
    /// Invalid command-line usage.
    Usage = 2,
    /// Selector resolution failed.
    ResolveFailed = 64,
    /// A preflight policy or capacity check refused the operation.
    PreflightFail = 65,
    /// Required network acquisition failed.
    AcquireNetwork = 66,
    /// No acceptable binary exists and building is impossible or disallowed.
    AcquireNoBinary = 67,
    /// A local build is required but this operation lacks approval.
    AcquireNeedsApproval = 68,
    /// An approved local build failed.
    BuildFailed = 69,
    /// Content, signature, or identity verification failed.
    VerifyFail = 70,
    /// Activation encountered a collision under the abort policy.
    StageCollision = 71,
    /// Another writer holds the state lease.
    StateLocked = 72,
    /// Persisted state or journal integrity failed.
    StateCorrupt = 73,
    /// An unmanaged Nix installation makes exclusive ownership unsafe.
    UnmanagedNix = 74,
    /// The operation was cancelled.
    Cancelled = 75,
    /// `--keep-going` observed target failures and committed nothing.
    PartialFailure = 76,
    /// Required privilege was unavailable or refused.
    Permission = 77,
    /// Product configuration is invalid.
    Config = 78,
    /// The private managed engine or broker cannot be reached.
    EngineUnavailable = 79,
    /// A prior operation was recovered; nonzero only under strict policy.
    Recovered = 80,
}

impl ExitCode {
    /// Numeric process status.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Stable symbolic name rendered in errors.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Usage => "USAGE",
            Self::ResolveFailed => "RESOLVE_FAILED",
            Self::PreflightFail => "PREFLIGHT_FAIL",
            Self::AcquireNetwork => "ACQUIRE_NETWORK",
            Self::AcquireNoBinary => "ACQUIRE_NO_BINARY",
            Self::AcquireNeedsApproval => "ACQUIRE_NEEDS_APPROVAL",
            Self::BuildFailed => "BUILD_FAILED",
            Self::VerifyFail => "VERIFY_FAIL",
            Self::StageCollision => "STAGE_COLLISION",
            Self::StateLocked => "STATE_LOCKED",
            Self::StateCorrupt => "STATE_CORRUPT",
            Self::UnmanagedNix => "UNMANAGED_NIX",
            Self::Cancelled => "CANCELLED",
            Self::PartialFailure => "PARTIAL_FAILURE",
            Self::Permission => "PERMISSION",
            Self::Config => "CONFIG",
            Self::EngineUnavailable => "ENGINE_UNAVAILABLE",
            Self::Recovered => "RECOVERED",
        }
    }

    /// Every public exit status in numeric order.
    pub const ALL: [Self; 19] = [
        Self::Ok,
        Self::Usage,
        Self::ResolveFailed,
        Self::PreflightFail,
        Self::AcquireNetwork,
        Self::AcquireNoBinary,
        Self::AcquireNeedsApproval,
        Self::BuildFailed,
        Self::VerifyFail,
        Self::StageCollision,
        Self::StateLocked,
        Self::StateCorrupt,
        Self::UnmanagedNix,
        Self::Cancelled,
        Self::PartialFailure,
        Self::Permission,
        Self::Config,
        Self::EngineUnavailable,
        Self::Recovered,
    ];
}

impl From<ExitCode> for ProcessExitCode {
    fn from(value: ExitCode) -> Self {
        Self::from(value.as_u8())
    }
}

impl From<ExitCode> for i32 {
    fn from(value: ExitCode) -> Self {
        i32::from(value.as_u8())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn codes_and_symbols_are_unique_and_stable() {
        let expected = [
            0, 2, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80,
        ];
        assert_eq!(ExitCode::ALL.map(ExitCode::as_u8), expected);
        assert_eq!(
            ExitCode::ALL
                .iter()
                .map(|code| code.symbol())
                .collect::<BTreeSet<_>>()
                .len(),
            ExitCode::ALL.len()
        );
    }
}
