//! Pure corruption- and crash-detection primitives for persisted state.
//!
//! There is deliberately no client-side secret here. Authentication of the
//! signed channel anchor belongs to the channel layer.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::ChannelSequence;

const PREFIX: &str = "sha256-";

/// A product-computed SHA-256 digest rendered as lowercase hexadecimal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Constructs a digest from its exact bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Returns the exact digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl FromStr for Digest {
    type Err = IntegrityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix(PREFIX)
            .ok_or_else(|| IntegrityError::InvalidDigest(value.into()))?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(IntegrityError::InvalidDigest(value.into()));
        }
        let mut out = [0_u8; 32];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            out[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        Ok(Self(out))
    }
}
fn nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => unreachable!("validated hex"),
    }
}

/// Integrity calculation or verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityError {
    /// A digest did not use the exact canonical format.
    InvalidDigest(String),
    /// Exact file bytes did not match their sidecar.
    SidecarMismatch {
        /// Digest recorded by the sidecar.
        expected: Digest,
        /// Digest calculated from the file body.
        actual: Digest,
    },
    /// RFC 8785 serialization failed.
    Canonicalization(String),
}
impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDigest(_) => f.write_str("invalid canonical SHA-256 state digest"),
            Self::SidecarMismatch { expected, actual } => write!(
                f,
                "state sidecar mismatch: expected {expected}, got {actual}"
            ),
            Self::Canonicalization(reason) => {
                write!(f, "could not canonicalize state JSON: {reason}")
            }
        }
    }
}
impl std::error::Error for IntegrityError {}

/// Hashes file body bytes exactly, including whitespace and final newline.
#[must_use]
pub fn body_digest(body: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(body);
    Digest::from_bytes(hasher.finalize().into())
}

/// Verifies a sidecar digest against exact file body bytes.
pub fn verify_sidecar(body: &[u8], sidecar: &str) -> Result<(), IntegrityError> {
    let expected = Digest::from_str(sidecar.trim_end_matches(['\r', '\n']))?;
    let actual = body_digest(body);
    if expected == actual {
        Ok(())
    } else {
        Err(IntegrityError::SidecarMismatch { expected, actual })
    }
}

/// Hashes the RFC 8785 canonical JSON representation of a value.
pub fn canonical_digest(value: &impl Serialize) -> Result<Digest, IntegrityError> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| IntegrityError::Canonicalization(error.to_string()))?;
    Ok(body_digest(&bytes))
}

/// Computes a deterministic Merkle root over generation content hashes.
#[must_use]
pub fn generation_merkle_root(leaves: &[Digest]) -> Digest {
    if leaves.is_empty() {
        return body_digest(&[]);
    }
    let mut level = leaves.to_vec();
    level.sort_unstable();
    while level.len() > 1 {
        let mut parents = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let right = pair.get(1).copied().unwrap_or(pair[0]);
            let mut bytes = [0_u8; 64];
            bytes[..32].copy_from_slice(pair[0].as_bytes());
            bytes[32..].copy_from_slice(right.as_bytes());
            parents.push(body_digest(&bytes));
        }
        level = parents;
    }
    level[0]
}

/// Unsigned local representation of the DR-013 signed-channel state anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStateAnchor {
    channel_seq: u64,
    generation_root: String,
    tree_digest: String,
}
impl ChannelStateAnchor {
    /// Constructs the exact value bound by signed channel metadata.
    #[must_use]
    pub fn new(channel_seq: ChannelSequence, generation_root: Digest, tree_digest: Digest) -> Self {
        Self {
            channel_seq: channel_seq.get().get(),
            generation_root: generation_root.to_string(),
            tree_digest: tree_digest.to_string(),
        }
    }
    /// Returns the canonical digest of this anchor value.
    pub fn digest(&self) -> Result<Digest, IntegrityError> {
        canonical_digest(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_hash_is_byte_sensitive_and_sidecar_fails_closed() {
        let first = body_digest(b"{}\n");
        assert_ne!(first, body_digest(b"{}"));
        verify_sidecar(b"{}\n", &format!("{first}\n")).unwrap();
        assert!(matches!(
            verify_sidecar(b"{}", &first.to_string()),
            Err(IntegrityError::SidecarMismatch { .. })
        ));
    }
    #[test]
    fn parser_requires_lowercase_exact_length() {
        let value = body_digest(b"state").to_string();
        assert_eq!(Digest::from_str(&value).unwrap().to_string(), value);
        assert!(Digest::from_str(&value.to_uppercase()).is_err());
        assert!(Digest::from_str("sha256-00").is_err());
    }
    #[test]
    fn canonical_hash_uses_jcs_key_order() {
        let value = serde_json::json!({"z":1,"a":2});
        assert_eq!(
            canonical_digest(&value).unwrap(),
            body_digest(br#"{"a":2,"z":1}"#)
        );
    }
    #[test]
    fn merkle_root_is_order_independent() {
        let a = body_digest(b"a");
        let b = body_digest(b"b");
        assert_eq!(
            generation_merkle_root(&[a, b]),
            generation_merkle_root(&[b, a])
        );
        assert_ne!(
            generation_merkle_root(&[a]),
            generation_merkle_root(&[a, b])
        );
    }
}
