//! Privileged installation and platform bindings for the managed runtime.
//!
//! This crate owns closed platform operations. It never accepts raw Nix
//! commands, arbitrary install paths, or caller-supplied identities.

mod assets;
mod broker;
mod helper;
mod installer;
pub mod platform;
mod repair;
mod service;
mod uninstall;

pub use assets::{
    LinuxAssetKind, LinuxAssetPrincipal, LinuxInstallAsset, LinuxSystemdAssets,
    linux_install_assets,
};
pub use broker::{
    BrokerTransportError, BrokerTransportErrorCode, serve_broker_connection,
    serve_broker_connection_with_nix,
};
pub use helper::{
    BrokerHelperDispatch, HelperTransportError, HelperTransportErrorCode, LinuxHelperSession,
    serve_helper_connection,
};
pub use installer::{
    InstallError, InstallErrorCode, LinuxInstallBackend, LinuxInstallReport, install_linux,
};
pub use platform::linux::{
    LinuxPeerCredentials, LinuxPlatformError, LinuxPlatformErrorCode, LinuxRootSetStore,
    authenticate_broker_peer, peer_credentials,
};
pub use platform::macos::{
    MacOsAssetKind, MacOsAssetPrincipal, MacOsBuildReadiness, MacOsBuildUsersReadiness, MacOsError,
    MacOsErrorCode, MacOsHelperSession, MacOsInstallAsset, MacOsInstallBackend, MacOsInstallReport,
    MacOsLaunchdAssets, MacOsPeerCredentials, MacOsReleaseStep, MacOsReleaseTarget,
    MacOsRootSetStore, MacOsSandboxReadiness, MacOsSocketContract, MacOsStoreVolumeContract,
    MacOsToolchainReadiness, install_macos, macos_install_assets, macos_release_steps,
};
pub use repair::{
    MemoryRepairJournal, RepairApprovalGate, RepairApprovalScope, RepairCoordinatorError,
    RepairCoordinatorErrorCode, RepairJournal, RepairJournalEntry, RepairJournalStatus,
    RepairRecoveryAction, RepairRequest, RepairResult, recover_repair, repair_generation,
};
pub use service::{
    ServiceError, ServiceErrorCode, run_linux_broker_from_activation,
    run_linux_root_helper_from_activation,
};
pub use uninstall::{
    RecordedAsset, RecordedAssetState, UninstallAction, UninstallAssetKind, UninstallBackend,
    UninstallError, UninstallErrorCode, UninstallManifest, UninstallPlan, UninstallReport,
    execute_uninstall, plan_uninstall,
};
