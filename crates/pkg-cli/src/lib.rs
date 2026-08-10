//! Stable command-line grammar and public presentation contracts for `pkg`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cli;
pub mod commands;
pub mod completion;
pub mod crash;
pub mod exit;
pub mod log;
pub mod path;
pub mod progress;
pub mod support;
pub mod telemetry;
pub mod ux;
