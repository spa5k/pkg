//! Tests for the `migrate` module.

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
