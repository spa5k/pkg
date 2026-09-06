//! Publisher-side release validation, TUF signing, and audit evidence.
//!
//! The client remains in `pkg-channel`. This crate is the narrower release
//! boundary: it validates a closed artifact manifest, requires two distinct
//! approval roles, signs only online TUF roles from an already-signed offline
//! root, and writes a new immutable publication directory.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod audit;
mod manifest;
mod publish;
mod rel;
mod sign;
mod timestamp;

pub use audit::{AuditEvent, write_audit_log};
pub use manifest::{
    Approval, ApprovalRole, ArtifactKind, CliArtifact, CliArtifactKind, ReleaseArtifact,
    ReleaseAuthority, ReleaseAuthorization, ReleaseManifest, ValidatedPreparedRelease,
    ValidatedRelease, ValidationError,
};
pub use publish::{
    ActivationStatus, DurableRelease, DurableTimestampRefresh, PublicationError, PublicationObject,
    Publisher, SealedReader, publish_release, publish_timestamp_refresh,
};
pub use rel::{
    Environment, KeySet, PublishChannel, RelError, ReleaseCard, init_key_set, publish_channel,
};
pub use sign::{MetadataPolicy, SignError, SignedRelease, sign_channel};
pub use timestamp::{
    SignedTimestampRefresh, TimestampAuthority, TimestampAuthorization, refresh_timestamp,
};
