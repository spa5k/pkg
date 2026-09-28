//! The native adapter's failure modes.

use std::fmt;

/// A native operation failure category with preserved diagnostics.
#[derive(Debug)]
pub enum NixError {
    /// No `nix` executable was found. Includes vendor setup guidance.
    MissingExecutable {
        /// The configured executable name or path.
        requested: String,
    },
    /// A native command could not be started.
    Spawn(String),
    /// A native command failed. Upstream diagnostics are preserved.
    Failed {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The exit status message.
        status: String,
        /// Captured stderr, when available.
        stderr: String,
    },
    /// A native command was terminated by a signal after it started.
    ///
    /// The child was reaped by pkg; the profile may be in an intermediate
    /// state, so callers must re-read native state before reporting.
    Interrupted {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The terminating signal number.
        signal: i32,
    },
    /// Native output could not be decoded into a required shape.
    Decode {
        /// What pkg was decoding.
        what: &'static str,
        /// Why decoding failed.
        detail: String,
    },
}

impl fmt::Display for NixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutable { requested } => write!(
                f,
                "the Nix executable ({requested}) was not found\n\
                 pkg requires an external Determinate Nix installation.\n\
                 Install it from https://docs.determinate.systems/determinate-nix/ \
                 and run this command again.\n\
                 pkg does not install, update, or repair the Nix runtime."
            ),
            Self::Spawn(detail) => write!(f, "could not start the Nix command: {detail}"),
            Self::Failed {
                args,
                status,
                stderr,
            } => {
                write!(
                    f,
                    "native command `nix {}` failed ({status})",
                    args.join(" ")
                )?;
                if !stderr.trim().is_empty() {
                    write!(f, "\n{}", stderr.trim())?;
                }
                Ok(())
            }
            Self::Interrupted { args, signal } => write!(
                f,
                "native command `nix {}` was terminated by signal {signal}; \
                 the operation may have partially applied",
                args.join(" ")
            ),
            Self::Decode { what, detail } => {
                write!(f, "could not decode required {what} from Nix: {detail}")
            }
        }
    }
}

impl std::error::Error for NixError {}
