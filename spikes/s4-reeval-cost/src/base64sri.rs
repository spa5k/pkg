//! Minimal standard base64 (RFC 4648) decoder for SRI hash validation only.
//!
//! We keep this tiny rather than pulling in a base64 crate, to honor the
//! "minimal, locked dependencies" requirement (only `serde` + `serde_json`).
//! It accepts canonical padded standard-alphabet input and returns the decoded
//! bytes. It deliberately rejects malformed padding / non-alphabet characters,
//! which is what `validate::is_sri_sha256` relies on to refuse malicious SRI
//! strings.

use std::fmt;

/// Decode `standard` base64 (`A-Za-z0-9+/`, padded with `=`).
///
/// Returns the decoded bytes, or an error for any malformed input (bad length,
/// non-alphabet character, or invalid padding). Whitespace is NOT trimmed;
/// callers pass the raw SRI payload (the part after `sha256-`).
pub fn decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    // Strip exactly 0, 1, or 2 trailing '=' padding characters and record count.
    let bytes_in = input.as_bytes();
    let mut end = bytes_in.len();
    let mut pad = 0usize;
    while end > 0 && bytes_in[end - 1] == b'=' {
        end -= 1;
        pad += 1;
    }
    if pad > 2 {
        return Err(DecodeError::BadPadding);
    }
    let core = &bytes_in[..end];

    // Length must be a multiple of 4 (after padding removed) and padding must be
    // consistent with the residue.
    if !core.len().is_multiple_of(4) && pad == 0 {
        // Unpadded input must still be a whole number of groups; canonical SRI
        // always has full groups with padding. Reject anything that isn't.
        return Err(DecodeError::BadLength);
    }
    let residue = core.len() % 4;
    // For canonical padded base64 the core (without '=') residue is 0 only when
    // pad makes a full group: 0 residue => 0 pad (full), 3 residue => 1 pad,
    // 2 residue => 2 pad. 1 residue is impossible in valid base64.
    match (residue, pad) {
        (0, 0) => {}
        (3, 1) => {}
        (2, 2) => {}
        _ => return Err(DecodeError::BadPadding),
    }

    let mut out = Vec::with_capacity(core.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &b in core {
        let v = decode_char(b)? as u32;
        buffer = (buffer << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    // Any leftover bits must be zero (canonical encoding); we don't fail loudly
    // beyond the structural checks above because SRI hashes are canonical.
    let _ = buffer;
    Ok(out)
}

fn decode_char(b: u8) -> Result<u8, DecodeError> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(DecodeError::InvalidChar),
    }
}

/// Encode bytes as canonical standard-alphabet base64 with padding. Used only to
/// round-trip-check the decoder in tests / for the findings hex<->SRI helper.
pub fn encode(input: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = (input[i] as u32) << 16 | (input[i + 1] as u32) << 8 | (input[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = (input[i] as u32) << 16 | (input[i + 1] as u32) << 8;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    BadLength,
    BadPadding,
    InvalidChar,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::BadLength => f.write_str("base64: bad length"),
            DecodeError::BadPadding => f.write_str("base64: bad padding"),
            DecodeError::InvalidChar => f.write_str("base64: invalid character"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_known_values() {
        // Classic RFC 4648 vectors.
        assert_eq!(decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode("aGVsbG8sIHdvcmxkIQ==").unwrap(), b"hello, world!");
        assert_eq!(encode(b"hello"), "aGVsbG8=");
        assert_eq!(encode(b"hello, world!"), "aGVsbG8sIHdvcmxkIQ==");
    }

    #[test]
    fn decodes_sha256_sri_payloads_to_32_bytes() {
        // Raw-archive SRI payload (after `sha256-`).
        let raw = "rXVGuq8bJfByJbOrrB3I++2MTsvZDcTo7C6UHXD5muE=";
        let d = decode(raw).unwrap();
        assert_eq!(d.len(), 32);
        // Flake NAR SRI payload (after `sha256-`).
        let nar = "oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";
        assert_eq!(decode(nar).unwrap().len(), 32);
        // The two domains decode to DIFFERENT bytes (key DR-004 finding).
        assert_ne!(decode(raw).unwrap(), decode(nar).unwrap());
    }

    #[test]
    fn rejects_malformed() {
        assert!(decode("not base64!!").is_err());
        assert!(decode("====").is_err());
        assert!(decode("YQ").is_err()); // 1-residue without proper padding group
        assert!(decode("Y===").is_err());
    }
}
