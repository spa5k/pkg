//! The `pkg` client library.
//!
//! One crate owns the whole client: the reduced command grammar, configuration
//! and paths, the concrete native Nix adapter, catalog name routing, command
//! execution, and output envelopes (design D1).

pub mod apps;
pub mod catalog;
pub mod cli;
pub mod commands;
pub mod config;
pub mod nix;
pub mod output;
pub mod tap;
