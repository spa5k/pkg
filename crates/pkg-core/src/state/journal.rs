//! Append-only, sequence-contiguous, hash-chained NDJSON journal contracts.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::schema::parse_unique_json;
use super::{Digest, IntegrityError, canonical_digest};

/// Fixed previous-hash sentinel carried by the first journal row.
pub const GENESIS_PREVIOUS_HASH: &str = "sha256-Genesis";
const MAX_ROW_BYTES: usize = 1024 * 1024;
const RESERVED_FIELDS: [&str; 4] = ["schemaVersion", "seq", "prevRowHash", "rowHash"];

/// Previous accepted row hash, or the fixed genesis sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousRowHash {
    /// The first row in a journal.
    Genesis,
    /// Hash of the immediately preceding accepted row.
    Row(Digest),
}

impl fmt::Display for PreviousRowHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Genesis => f.write_str(GENESIS_PREVIOUS_HASH),
            Self::Row(digest) => fmt::Display::fmt(digest, f),
        }
    }
}

impl FromStr for PreviousRowHash {
    type Err = JournalError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == GENESIS_PREVIOUS_HASH {
            Ok(Self::Genesis)
        } else {
            Digest::from_str(value)
                .map(Self::Row)
                .map_err(|error| JournalError::InvalidRow(error.to_string()))
        }
    }
}

/// Operation-specific fields carried by a journal row.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalPayload {
    fields: BTreeMap<String, Value>,
}

impl JournalPayload {
    /// Validates a payload. `opId`, `phase`, and `status` are mandatory strings.
    pub fn new(fields: BTreeMap<String, Value>) -> Result<Self, JournalError> {
        for reserved in RESERVED_FIELDS {
            if fields.contains_key(reserved) {
                return Err(JournalError::InvalidRow(format!(
                    "payload cannot override reserved field {reserved}"
                )));
            }
        }
        for required in ["opId", "phase", "status"] {
            if fields
                .get(required)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(JournalError::InvalidRow(format!(
                    "payload field {required} must be a nonempty string"
                )));
            }
        }
        Ok(Self { fields })
    }

    /// Returns operation-specific fields in deterministic key order.
    #[must_use]
    pub const fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }
}

/// One validated journal row.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRow {
    seq: u64,
    previous: PreviousRowHash,
    row_hash: Digest,
    payload: JournalPayload,
}

impl JournalRow {
    /// Creates and hashes the next row in a chain.
    pub fn new(
        seq: u64,
        previous: PreviousRowHash,
        payload: JournalPayload,
    ) -> Result<Self, JournalError> {
        if seq == 0 {
            return Err(JournalError::InvalidRow("seq must start at 1".into()));
        }
        if (seq == 1) != matches!(previous, PreviousRowHash::Genesis) {
            return Err(JournalError::InvalidRow(
                "only seq 1 may use the genesis sentinel".into(),
            ));
        }
        let row_hash = hash_body(seq, previous, &payload)?;
        Ok(Self {
            seq,
            previous,
            row_hash,
            payload,
        })
    }

    /// Returns the row sequence number.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }
    /// Returns this row's content hash.
    #[must_use]
    pub const fn row_hash(&self) -> Digest {
        self.row_hash
    }
    /// Returns the operation-specific payload.
    #[must_use]
    pub const fn payload(&self) -> &JournalPayload {
        &self.payload
    }

    /// Encodes one complete newline-terminated NDJSON row.
    pub fn to_ndjson_line(&self) -> Result<Vec<u8>, JournalError> {
        let wire = JournalWire::from(self);
        let mut bytes = serde_json::to_vec(&wire)
            .map_err(|error| JournalError::InvalidRow(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Result of scanning the journal from its head.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRecovery {
    accepted: Vec<JournalRow>,
    quarantined_suffix: Vec<u8>,
}

impl JournalRecovery {
    /// Returns the longest sequence-contiguous, hash-valid prefix.
    #[must_use]
    pub fn accepted(&self) -> &[JournalRow] {
        &self.accepted
    }
    /// Returns the final bytes that must be quarantined before appending.
    #[must_use]
    pub fn quarantined_suffix(&self) -> &[u8] {
        &self.quarantined_suffix
    }
}

/// Journal validation failure that cannot be treated as a torn final suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    /// A new or decoded row is invalid.
    InvalidRow(String),
    /// Corruption occurred before later journal content and must fail closed.
    InteriorCorruption {
        /// One-based physical line number.
        line: usize,
        /// Redacted validation reason.
        reason: String,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRow(reason) => write!(f, "invalid journal row: {reason}"),
            Self::InteriorCorruption { line, reason } => {
                write!(f, "journal corruption at interior line {line}: {reason}")
            }
        }
    }
}
impl std::error::Error for JournalError {}

/// Recovers the longest valid prefix, quarantining only a broken final suffix.
pub fn recover_journal(bytes: &[u8]) -> Result<JournalRecovery, JournalError> {
    if bytes.is_empty() {
        return Ok(JournalRecovery {
            accepted: Vec::new(),
            quarantined_suffix: Vec::new(),
        });
    }
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            spans.push((start, index + 1, true));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        spans.push((start, bytes.len(), false));
    }

    let mut accepted = Vec::new();
    for (index, (start, end, terminated)) in spans.iter().copied().enumerate() {
        let is_tail = index + 1 == spans.len();
        if !terminated {
            return Ok(JournalRecovery {
                accepted,
                quarantined_suffix: bytes[start..].to_vec(),
            });
        }
        let line = &bytes[start..end - 1];
        match decode_row(line, accepted.last()) {
            Ok(row) => accepted.push(row),
            Err(_) if is_tail => {
                return Ok(JournalRecovery {
                    accepted,
                    quarantined_suffix: bytes[start..].to_vec(),
                });
            }
            Err(error) => {
                return Err(JournalError::InteriorCorruption {
                    line: index + 1,
                    reason: error.to_string(),
                });
            }
        }
    }
    Ok(JournalRecovery {
        accepted,
        quarantined_suffix: Vec::new(),
    })
}

fn decode_row(line: &[u8], previous: Option<&JournalRow>) -> Result<JournalRow, JournalError> {
    if line.is_empty() || line.len() > MAX_ROW_BYTES {
        return Err(JournalError::InvalidRow(
            "row is empty or exceeds 1 MiB".into(),
        ));
    }
    let value: Value =
        parse_unique_json(line).map_err(|error| JournalError::InvalidRow(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| JournalError::InvalidRow("row must be an object".into()))?;
    let recorded = object
        .get("rowHash")
        .and_then(Value::as_str)
        .ok_or_else(|| JournalError::InvalidRow("rowHash must be a string".into()))?;
    let recorded =
        Digest::from_str(recorded).map_err(|error| JournalError::InvalidRow(error.to_string()))?;
    let mut body = value.clone();
    let Some(body_object) = body.as_object_mut() else {
        return Err(JournalError::InvalidRow("row must be an object".into()));
    };
    body_object.remove("rowHash");
    let calculated =
        canonical_digest(&body).map_err(|error| JournalError::InvalidRow(error.to_string()))?;
    if recorded != calculated {
        return Err(JournalError::InvalidRow("rowHash mismatch".into()));
    }
    let wire: JournalWire = serde_json::from_value(value)
        .map_err(|error| JournalError::InvalidRow(error.to_string()))?;
    if wire.schema_version != 1 {
        return Err(JournalError::InvalidRow(
            "unsupported journal schemaVersion".into(),
        ));
    }
    let expected_seq = previous.map_or(1, |row| row.seq + 1);
    if wire.seq != expected_seq {
        return Err(JournalError::InvalidRow("non-contiguous seq".into()));
    }
    let expected_previous = previous.map_or(PreviousRowHash::Genesis, |row| {
        PreviousRowHash::Row(row.row_hash)
    });
    let decoded_previous = PreviousRowHash::from_str(&wire.prev_row_hash)?;
    if decoded_previous != expected_previous {
        return Err(JournalError::InvalidRow("prevRowHash mismatch".into()));
    }
    let payload = JournalPayload::new(wire.payload)?;
    Ok(JournalRow {
        seq: wire.seq,
        previous: decoded_previous,
        row_hash: recorded,
        payload,
    })
}

fn hash_body(
    seq: u64,
    previous: PreviousRowHash,
    payload: &JournalPayload,
) -> Result<Digest, JournalError> {
    let mut object = Map::new();
    object.insert("schemaVersion".into(), Value::from(1));
    object.insert("seq".into(), Value::from(seq));
    object.insert("prevRowHash".into(), Value::from(previous.to_string()));
    for (key, value) in &payload.fields {
        object.insert(key.clone(), value.clone());
    }
    canonical_digest(&Value::Object(object))
        .map_err(|error| JournalError::InvalidRow(error.to_string()))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalWire {
    schema_version: u64,
    seq: u64,
    prev_row_hash: String,
    row_hash: String,
    #[serde(flatten)]
    payload: BTreeMap<String, Value>,
}
impl From<&JournalRow> for JournalWire {
    fn from(row: &JournalRow) -> Self {
        Self {
            schema_version: 1,
            seq: row.seq,
            prev_row_hash: row.previous.to_string(),
            row_hash: row.row_hash.to_string(),
            payload: row.payload.fields.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn payload(status: &str) -> JournalPayload {
        JournalPayload::new(BTreeMap::from([
            ("opId".into(), Value::from("op_1")),
            ("phase".into(), Value::from("resolve")),
            ("status".into(), Value::from(status)),
        ]))
        .unwrap()
    }
    fn two_rows() -> Vec<u8> {
        let first = JournalRow::new(1, PreviousRowHash::Genesis, payload("started")).unwrap();
        let second =
            JournalRow::new(2, PreviousRowHash::Row(first.row_hash()), payload("ok")).unwrap();
        [
            first.to_ndjson_line().unwrap(),
            second.to_ndjson_line().unwrap(),
        ]
        .concat()
    }
    #[test]
    fn exact_chain_round_trips() {
        let bytes = two_rows();
        let recovery = recover_journal(&bytes).unwrap();
        assert_eq!(recovery.accepted().len(), 2);
        assert!(recovery.quarantined_suffix().is_empty());
    }
    #[test]
    fn partial_or_bad_final_row_is_quarantined() {
        let mut partial = two_rows();
        partial.extend_from_slice(br#"{"schemaVersion":1"#);
        let recovery = recover_journal(&partial).unwrap();
        assert_eq!(recovery.accepted().len(), 2);
        assert!(!recovery.quarantined_suffix().is_empty());
        let mut bad_complete = two_rows();
        bad_complete.extend_from_slice(b"{}\n");
        assert_eq!(recover_journal(&bad_complete).unwrap().accepted().len(), 2);
    }
    #[test]
    fn interior_tamper_reorder_and_delete_fail_closed() {
        let bytes = two_rows();
        let split = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut tampered = bytes.clone();
        let status = tampered
            .windows(7)
            .position(|window| window == b"started")
            .unwrap();
        tampered[status] = b'S';
        assert!(matches!(
            recover_journal(&tampered),
            Err(JournalError::InteriorCorruption { line: 1, .. })
        ));
        let reordered = [&bytes[split..], &bytes[..split]].concat();
        assert!(matches!(
            recover_journal(&reordered),
            Err(JournalError::InteriorCorruption { line: 1, .. })
        ));
        assert!(
            recover_journal(&bytes[split..])
                .unwrap()
                .accepted()
                .is_empty()
        );

        let accepted = recover_journal(&bytes).unwrap().accepted().to_vec();
        let third = JournalRow::new(
            3,
            PreviousRowHash::Row(accepted[1].row_hash()),
            payload("committed"),
        )
        .unwrap();
        let fourth = JournalRow::new(
            4,
            PreviousRowHash::Row(third.row_hash()),
            payload("committed"),
        )
        .unwrap();
        let with_later_content = [
            bytes.clone(),
            third.to_ndjson_line().unwrap(),
            fourth.to_ndjson_line().unwrap(),
        ]
        .concat();
        let second_end = with_later_content[split..]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap()
            + split
            + 1;
        let deleted_middle = [
            &with_later_content[..split],
            &with_later_content[second_end..],
        ]
        .concat();
        assert!(matches!(
            recover_journal(&deleted_middle),
            Err(JournalError::InteriorCorruption { line: 2, .. })
        ));
    }
}
