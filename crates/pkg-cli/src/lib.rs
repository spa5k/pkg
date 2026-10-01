//! The `pkg` client library.
//!
//! One crate owns the whole client: the reduced command grammar, configuration
//! and paths, the concrete native Nix adapter, catalog name routing, command
//! execution, and output envelopes (design D1).

// Tests may abort on broken fixtures or failed setup; production code
// denies explicit unwrap/expect/panic (these Clippy lints are denied by
// the workspace for every non-test target).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
    )
)]

pub mod apps;
pub mod catalog;
pub mod cli;
pub mod commands;
pub mod config;
pub mod nix;
pub mod output;
pub mod tap;
