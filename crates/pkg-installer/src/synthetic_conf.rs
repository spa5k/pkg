//! Closed planner for the product-owned macOS `/nix` synthetic entry.

use std::{error::Error, fmt};

const MAX_SYNTHETIC_CONF_BYTES: usize = 65_536;
const SYNTHETIC_ENTRY: &str = "nix";

/// Stable failures for synthetic-entry planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsSyntheticConfErrorCode {
    /// Existing bytes are oversized, non-UTF-8, or contain forbidden controls.
    InvalidFile,
    /// Existing state contains a noncanonical `nix` entry.
    ConflictingEntry,
}

/// Redacted synthetic-entry planning failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsSyntheticConfError {
    code: MacOsSyntheticConfErrorCode,
}

impl MacOsSyntheticConfError {
    const fn new(code: MacOsSyntheticConfErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsSyntheticConfErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsSyntheticConfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS synthetic configuration is unsafe")
    }
}

impl Error for MacOsSyntheticConfError {}

/// Exact non-mutating plan for `/etc/synthetic.conf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsSyntheticConfPlan {
    changed: bool,
    bytes: Vec<u8>,
}

impl MacOsSyntheticConfPlan {
    /// Returns whether the file must be replaced.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }

    /// Returns the complete bounded replacement bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Plans an exact idempotent `nix` synthetic entry without mutating the host.
///
/// Unrelated bytes are preserved exactly. Any existing line whose first token
/// is `nix` must be the one-byte-token canonical line `nix`; aliases, targets,
/// trailing whitespace, carriage returns, duplicates, and malformed text fail
/// closed rather than being normalized.
///
/// # Errors
///
/// Returns a stable error for oversized/non-UTF-8/control-bearing input or a
/// conflicting/duplicate `nix` entry.
pub fn plan_macos_synthetic_entry(
    existing: Option<&[u8]>,
) -> Result<MacOsSyntheticConfPlan, MacOsSyntheticConfError> {
    let existing = existing.unwrap_or_default();
    if existing.len() > MAX_SYNTHETIC_CONF_BYTES {
        return Err(MacOsSyntheticConfError::new(
            MacOsSyntheticConfErrorCode::InvalidFile,
        ));
    }
    let text = std::str::from_utf8(existing)
        .map_err(|_| MacOsSyntheticConfError::new(MacOsSyntheticConfErrorCode::InvalidFile))?;
    if text
        .bytes()
        .any(|byte| byte == 0 || (byte.is_ascii_control() && !matches!(byte, b'\n' | b'\t')))
    {
        return Err(MacOsSyntheticConfError::new(
            MacOsSyntheticConfErrorCode::InvalidFile,
        ));
    }

    let mut exact_entries = 0usize;
    for line in text.split('\n') {
        if line == SYNTHETIC_ENTRY {
            exact_entries += 1;
            continue;
        }
        if line
            .split_ascii_whitespace()
            .next()
            .is_some_and(|token| token == SYNTHETIC_ENTRY)
        {
            return Err(MacOsSyntheticConfError::new(
                MacOsSyntheticConfErrorCode::ConflictingEntry,
            ));
        }
    }
    if exact_entries > 1 {
        return Err(MacOsSyntheticConfError::new(
            MacOsSyntheticConfErrorCode::ConflictingEntry,
        ));
    }
    if exact_entries == 1 {
        return Ok(MacOsSyntheticConfPlan {
            changed: false,
            bytes: existing.to_vec(),
        });
    }

    let additional =
        usize::from(!existing.is_empty() && !existing.ends_with(b"\n")) + SYNTHETIC_ENTRY.len() + 1;
    if existing.len().saturating_add(additional) > MAX_SYNTHETIC_CONF_BYTES {
        return Err(MacOsSyntheticConfError::new(
            MacOsSyntheticConfErrorCode::InvalidFile,
        ));
    }
    let mut bytes = Vec::with_capacity(existing.len() + additional);
    bytes.extend_from_slice(existing);
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b"nix\n");
    Ok(MacOsSyntheticConfPlan {
        changed: true,
        bytes,
    })
}

#[cfg(test)]
mod tests;
