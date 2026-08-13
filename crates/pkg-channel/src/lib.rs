//! Authenticated channel loading for `pkg`.
//!
//! The public boundary returns only policy that has passed both TUF
//! verification and pkg's V1 semantic checks. Callers cannot select arbitrary
//! transports, disable expiration, weaken metadata limits, or access raw TUF
//! metadata through this crate.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod descriptor;
mod keys;
mod policy;
mod state;
mod tuf;

pub use descriptor::{
    BuildMode, CachePolicy, CachePublicKey, ChannelDescriptor, IndexArtifact, NixRuntimeArtifact,
    NixpkgsPin,
};
pub use keys::TrustedRoot;
pub use policy::{
    AcceptedChannel, ChannelError, RefreshOutcome, VerifiedChannel,
    validate_datastore as validate_private_datastore,
    validate_descriptor as verify_authenticated_descriptor,
    validate_repository_url as validate_https_repository_url,
};
pub use tuf::{AuthenticatedIndexTarget, ChannelClient, ChannelRefresh};
