//! Privileged installation and platform bindings for the managed runtime.
//!
//! This crate owns closed platform operations. It never accepts raw Nix
//! commands, arbitrary install paths, or caller-supplied identities.

mod approval_audit;
mod assets;
mod bootstrap;
mod broker;
mod helper;
mod installer;
mod linux_accounts;
pub mod platform;
mod repair;
mod root_client;
mod service;
#[cfg(target_os = "macos")]
mod store_apfs;
mod store_journal;
#[cfg(target_os = "macos")]
mod store_journal_file;
mod store_mount;
mod store_provision;
#[cfg(target_os = "macos")]
mod store_provision_macos;
mod synthetic_conf;
#[cfg(target_os = "macos")]
mod synthetic_file;
mod uninstall;

pub use approval_audit::{BrokerApprovalAudit, BrokerCallerApprovalJournal};
pub use assets::{
    LinuxAssetKind, LinuxAssetPrincipal, LinuxInstallAsset, LinuxSystemdAssets,
    linux_install_assets,
};
pub use bootstrap::{
    LinuxBundleInstallReport, MacOsBundleInstallReport, install_linux_from_bundle,
    install_macos_from_bundle,
};
pub use broker::{
    BrokerTransportError, BrokerTransportErrorCode, serve_broker_connection,
    serve_broker_connection_with_build_and_root_authority,
    serve_broker_connection_with_build_authority, serve_broker_connection_with_nix,
    serve_broker_connection_with_nix_and_approval,
};
pub use helper::{
    BrokerHelperDispatch, HelperTransportError, HelperTransportErrorCode, LinuxHelperSession,
    serve_helper_connection,
};
pub use installer::{
    InstallError, InstallErrorCode, LinuxInstallBackend, LinuxInstallReport, install_linux,
};
pub use linux_accounts::{
    LinuxAccountError, LinuxAccountErrorCode, LinuxAccountManager, plan_linux_group_bindings,
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
pub use root_client::RootHelperClient;
pub use service::{
    ServiceError, ServiceErrorCode, run_linux_broker_from_activation,
    run_linux_root_helper_from_activation, run_macos_broker, run_macos_root_helper,
};
pub use store_journal::{
    MacOsStoreJournalError, MacOsStoreJournalErrorCode, MacOsStoreJournalPhase,
    MacOsStoreProvisionJournal, MacOsStoreRollbackAction,
};
#[cfg(target_os = "macos")]
pub use store_journal_file::{MacOsStoreJournalFileError, MacOsStoreJournalStorage};
pub use store_mount::{
    MacOsStoreMountError, MacOsStoreMountErrorCode, MacOsStoreMountOutcome,
    MacOsStoreRecordOutcome, publish_macos_store_volume_record, run_macos_store_mount,
};
pub use store_provision::{
    MacOsStoreProvisionBackend, MacOsStoreProvisionError, MacOsStoreProvisionErrorCode,
    MacOsStoreProvisionOutcome, provision_macos_store_volume,
};
#[cfg(target_os = "macos")]
pub use store_provision_macos::provision_macos_store_volume_production;
pub use synthetic_conf::{
    MacOsSyntheticConfError, MacOsSyntheticConfErrorCode, MacOsSyntheticConfPlan,
    plan_macos_synthetic_entry,
};
#[cfg(target_os = "macos")]
pub use synthetic_file::{
    MacOsSyntheticFileError, MacOsSyntheticFileStorage, MacOsSyntheticFileTransaction,
};
pub use uninstall::{
    RecordedAsset, RecordedAssetState, UninstallAction, UninstallAssetKind, UninstallBackend,
    UninstallError, UninstallErrorCode, UninstallManifest, UninstallPlan, UninstallReport,
    execute_uninstall, plan_uninstall,
};
