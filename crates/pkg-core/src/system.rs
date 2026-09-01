//! Nix *system triples* supported by `pkg`.
//!
//! Per `plans/00-overview-and-decisions.md` (D-14) and
//! `plans/07-platform-installation-and-runtime.md`, V1 supports exactly four
//! host systems: `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and
//! `aarch64-darwin`. These are the canonical Nix `system` strings used
//! throughout the channel descriptor (`plans/02` §7), the lock
//! (`plans/01` §10.2 / `plans/05` §5.2), and the realization identity.
//!
//! This module is deliberately **closed**: it accepts only those four exact
//! strings and rejects everything else — including Rust *target* triples such
//! as `x86_64-unknown-linux-gnu` and Nix triples `pkg` never builds for
//! (`i686-linux`, `armv7l-linux`, …). There is intentionally **no host
//! detection** here (see `plans/01` §8 and `plans/06` §6.1: `pkg doctor` owns
//! mapping the running host to a [`System`]).

use std::fmt;
use std::str::FromStr;

/// Error returned when a string cannot be parsed as a [`System`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemError {
    /// The input did not contain any characters.
    Empty,
    /// The input is not one of the four supported system triples.
    Unknown {
        /// The rejected input.
        input: String,
    },
}

impl fmt::Display for SystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SystemError::Empty => f.write_str("empty system triple"),
            SystemError::Unknown { input } => write!(
                f,
                "unknown system triple {input:?}; expected one of {}",
                System::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for SystemError {}

/// A supported Nix system triple.
///
/// Exactly four variants exist (D-14). The canonical string form is available
/// via [`System::as_str`] / [`fmt::Display`] and round-trips through
/// [`FromStr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum System {
    /// `x86_64-linux`.
    X8664Linux,
    /// `aarch64-linux`.
    Aarch64Linux,
    /// `x86_64-darwin`.
    X8664Darwin,
    /// `aarch64-darwin`.
    Aarch64Darwin,
}

impl System {
    /// All four V1-supported systems (D-14).
    pub const ALL: [System; 4] = [
        System::X8664Linux,
        System::Aarch64Linux,
        System::X8664Darwin,
        System::Aarch64Darwin,
    ];

    /// Returns the canonical Nix `system` string for this system.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            System::X8664Linux => "x86_64-linux",
            System::Aarch64Linux => "aarch64-linux",
            System::X8664Darwin => "x86_64-darwin",
            System::Aarch64Darwin => "aarch64-darwin",
        }
    }

    /// Returns the CPU architecture component of this system.
    #[must_use]
    pub const fn architecture(self) -> Architecture {
        match self {
            System::X8664Linux | System::X8664Darwin => Architecture::X8664,
            System::Aarch64Linux | System::Aarch64Darwin => Architecture::Aarch64,
        }
    }

    /// Returns the operating-system component of this system.
    #[must_use]
    pub const fn os(self) -> Os {
        match self {
            System::X8664Linux | System::Aarch64Linux => Os::Linux,
            System::X8664Darwin | System::Aarch64Darwin => Os::Darwin,
        }
    }
}

impl fmt::Display for System {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for System {
    type Err = SystemError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86_64-linux" => Ok(System::X8664Linux),
            "aarch64-linux" => Ok(System::Aarch64Linux),
            "x86_64-darwin" => Ok(System::X8664Darwin),
            "aarch64-darwin" => Ok(System::Aarch64Darwin),
            "" => Err(SystemError::Empty),
            other => Err(SystemError::Unknown {
                input: other.to_owned(),
            }),
        }
    }
}

/// A CPU architecture component of a [`System`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Architecture {
    /// `x86_64`.
    X8664,
    /// `aarch64`.
    Aarch64,
}

impl Architecture {
    /// Returns the canonical Nix architecture string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Architecture::X8664 => "x86_64",
            Architecture::Aarch64 => "aarch64",
        }
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An operating-system component of a [`System`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    /// `linux`.
    Linux,
    /// `darwin`.
    Darwin,
}

impl Os {
    /// Returns the canonical Nix operating-system string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Darwin => "darwin",
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests;
