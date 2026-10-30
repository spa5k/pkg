//! The `pkg` client library.
//!
//! Parse commands, load configuration, run Nix, resolve catalog names,
//! and format command results.

#![cfg(unix)]
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
mod background;
pub use discovery::catalog;
pub mod cli;
mod client;
pub mod commands;
pub mod config;
pub mod discovery;
pub mod nix;
pub mod output;
mod profile;
mod profile_lock;
pub mod tap;

/// Entry points for the fuzz targets in `fuzz/`. Only `cargo fuzz` builds them.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz {
    pub use crate::client::fuzz::{newer, release_archive, release_signature, release_sums};
    pub use crate::discovery::catalog::fuzz::{Row, read_list};
}
