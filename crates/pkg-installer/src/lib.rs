//! Privileged installation and platform bindings for the managed runtime.
//!
//! This crate owns closed platform operations. It never accepts raw Nix
//! commands, arbitrary install paths, or caller-supplied identities.

mod assets;
mod broker;
mod helper;
mod installer;
pub mod platform;

pub use assets::{
    LinuxAssetKind, LinuxAssetPrincipal, LinuxInstallAsset, LinuxSystemdAssets,
    linux_install_assets,
};
pub use broker::{BrokerTransportError, BrokerTransportErrorCode, serve_broker_connection};
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
