//! Managed-Nix lifecycle and host-ownership checks.

pub mod accounts;
pub mod daemon;
pub mod detect;
mod installer_bundle;
pub mod ownership;
pub mod provision;
pub(crate) mod runtime_archive;
pub mod uninstall;
