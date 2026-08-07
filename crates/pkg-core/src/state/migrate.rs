//! Forward-only, idempotent state migrations.
//!
//! V1 is the first released schema. The roadmap nevertheless requires an
//! explicit V0 fixture and hook so migration behavior is testable before a
//! release depends on it. V0 is narrowly defined as the V1 manifest/lock wire
//! shape with `schemaVersion: 0` and no `uid`; the caller supplies the owning
//! uid while migrating. No other legacy shape is guessed or repaired.

use serde_json::Value;

use super::schema::parse_unique_json;
use super::{LockedState, Manifest, StateSchemaError};

/// Result of normalizing one persisted document to schema version 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationResult {
    bytes: Vec<u8>,
    migrated: bool,
}

impl MigrationResult {
    /// Returns the validated schema-version 1 JSON bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns whether a forward migration changed the input schema.
    #[must_use]
    pub const fn migrated(&self) -> bool {
        self.migrated
    }
}

/// Validates a manifest as V1 or migrates the narrowly defined V0 form.
pub fn migrate_manifest_to_v1(
    input: &[u8],
    owning_uid: u32,
) -> Result<MigrationResult, StateSchemaError> {
    migrate(input, owning_uid, Manifest::from_json)
}

/// Validates a lock file as V1 or migrates the narrowly defined V0 form.
pub fn migrate_lock_to_v1(
    input: &[u8],
    owning_uid: u32,
) -> Result<MigrationResult, StateSchemaError> {
    migrate(input, owning_uid, LockedState::from_json)
}

fn migrate<T>(
    input: &[u8],
    owning_uid: u32,
    validate: impl Fn(&[u8]) -> Result<T, StateSchemaError>,
) -> Result<MigrationResult, StateSchemaError> {
    let mut value: Value = parse_unique_json(input)?;
    let object = value.as_object_mut().ok_or_else(|| {
        StateSchemaError::InvalidJson("state document must be a JSON object".into())
    })?;
    let version = object
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            StateSchemaError::InvalidJson("schemaVersion must be an unsigned integer".into())
        })?;

    match version {
        1 => {
            validate(input)?;
            Ok(MigrationResult {
                bytes: input.to_vec(),
                migrated: false,
            })
        }
        0 => {
            if object.contains_key("uid") {
                return Err(StateSchemaError::Invariant(
                    "schema version 0 must not contain uid".into(),
                ));
            }
            object.insert("schemaVersion".into(), Value::from(1));
            object.insert("uid".into(), Value::from(owning_uid));
            let bytes = serde_json::to_vec(&value)
                .map_err(|error| StateSchemaError::InvalidJson(error.to_string()))?;
            validate(&bytes)?;
            Ok(MigrationResult {
                bytes,
                migrated: true,
            })
        }
        other => Err(StateSchemaError::UnsupportedSchemaVersion(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_V0: &[u8] = br#"{"schemaVersion":0,"channelSeq":1,"entries":[],"pins":[]}"#;
    const LOCK_V0: &[u8] =
        br#"{"schemaVersion":0,"channelSeq":1,"system":"x86_64-linux","entries":{}}"#;

    #[test]
    fn v0_manifest_migrates_once_and_v1_is_idempotent() {
        let migrated = migrate_manifest_to_v1(MANIFEST_V0, 1001).unwrap();
        assert!(migrated.migrated());
        let again = migrate_manifest_to_v1(migrated.bytes(), 9999).unwrap();
        assert!(!again.migrated());
        assert_eq!(again.bytes(), migrated.bytes());
        assert_eq!(Manifest::from_json(again.bytes()).unwrap().uid(), 1001);
    }

    #[test]
    fn future_and_ambiguous_v0_are_refused() {
        assert_eq!(
            migrate_manifest_to_v1(br#"{"schemaVersion":2}"#, 1).unwrap_err(),
            StateSchemaError::UnsupportedSchemaVersion(2)
        );
        assert!(
            migrate_manifest_to_v1(
                br#"{"schemaVersion":0,"uid":1,"channelSeq":1,"entries":[],"pins":[]}"#,
                1
            )
            .is_err()
        );
    }

    #[test]
    fn v0_lock_migrates_with_the_supplied_owner() {
        let migrated = migrate_lock_to_v1(LOCK_V0, 1001).unwrap();
        assert!(migrated.migrated());
        assert_eq!(
            LockedState::from_json(migrated.bytes()).unwrap().uid(),
            1001
        );
    }
}
