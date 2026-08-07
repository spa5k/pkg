//! Schema-versioned, fail-closed persisted state contracts.
//!
//! This module owns the PR-10 on-disk vocabulary. JSON is decoded through
//! private wire DTOs and then validated into the strong types from `pkg-core`;
//! malformed paths, selectors, revisions, systems, or cross-field relations
//! therefore never enter the public state model.

mod integrity;
mod journal;
mod migrate;
mod schema;

pub use integrity::{
    ChannelStateAnchor, Digest, IntegrityError, body_digest, canonical_digest,
    generation_merkle_root, verify_sidecar,
};
pub use journal::{
    GENESIS_PREVIOUS_HASH, JournalError, JournalPayload, JournalRecovery, JournalRow,
    PreviousRowHash, recover_journal,
};
pub use migrate::{MigrationResult, migrate_lock_to_v1, migrate_manifest_to_v1};
pub use schema::{
    Activation, CollisionChoice, CollisionPolicy, CollisionResolution, Generation,
    GenerationOperation, GenerationOutput, LockEntry, LockedState, Manifest, ManifestEntry,
    OperationApproval, StateSchemaError,
};
