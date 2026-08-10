//! Broker-owned current channel and catalog authority for local builds.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex},
};

use pkg_channel::VerifiedChannel;
use pkg_core::{ChannelSequence, PackageSelector, PolicyVersion};
use pkg_index::VerifiedIndex;
use pkg_nix::{AuthenticatedCaller, BuildPreview, OperationHandle};

use crate::{AuthenticatedBuildPreparation, BuildPlanningAdapter};

/// Result of publishing authenticated service state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAuthorityUpdate {
    /// The broker's current authority changed.
    Updated,
    /// The exact authenticated authority was already current.
    Unchanged,
}

/// Broker-private owner of the current authenticated build inputs.
///
/// The command transport never supplies a channel, target system, index,
/// derivation, path, or Nix option. A trusted refresh path publishes verified
/// capabilities here; build requests contain only typed package selectors.
pub struct AuthenticatedBuildAuthority {
    state: Mutex<AuthorityState>,
    adapter: Arc<dyn BuildPlanningAdapter>,
}

impl fmt::Debug for AuthenticatedBuildAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBuildAuthority")
            .finish_non_exhaustive()
    }
}

impl AuthenticatedBuildAuthority {
    /// Starts broker authority from one cryptographically verified channel.
    #[must_use]
    pub fn new(channel: VerifiedChannel, adapter: Arc<dyn BuildPlanningAdapter>) -> Self {
        Self {
            state: Mutex::new(AuthorityState {
                identity: ChannelAuthorityIdentity::from_channel(&channel),
                channel,
                index: None,
            }),
            adapter,
        }
    }

    /// Publishes a verified channel monotonically and drops any older index.
    ///
    /// # Errors
    ///
    /// Refuses rollback, same-sequence identity reuse, policy downgrade, or an
    /// unavailable authority lock without changing current state.
    pub fn refresh_channel(
        &self,
        channel: VerifiedChannel,
    ) -> Result<BuildAuthorityUpdate, BuildAuthorityError> {
        let candidate = ChannelAuthorityIdentity::from_channel(&channel);
        let mut state = self.lock_state()?;
        match compare_channel_identity(state.identity, candidate)? {
            BuildAuthorityUpdate::Unchanged => Ok(BuildAuthorityUpdate::Unchanged),
            BuildAuthorityUpdate::Updated => {
                state.identity = candidate;
                state.channel = channel;
                state.index = None;
                Ok(BuildAuthorityUpdate::Updated)
            }
        }
    }

    /// Publishes an index authenticated for the exact current descriptor.
    ///
    /// # Errors
    ///
    /// Refuses an index verified for any other channel identity, preserving the
    /// currently published index.
    pub fn publish_index(
        &self,
        index: VerifiedIndex,
    ) -> Result<BuildAuthorityUpdate, BuildAuthorityError> {
        let mut state = self.lock_state()?;
        if !index.matches_channel(&state.channel) {
            return Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::IndexMismatch,
            ));
        }
        if state.index.as_ref() == Some(&index) {
            return Ok(BuildAuthorityUpdate::Unchanged);
        }
        state.index = Some(index);
        Ok(BuildAuthorityUpdate::Updated)
    }

    /// Prepares and installs a build using a short broker-owned authority snapshot.
    ///
    /// The mutex is released before host observation, Nix evaluation, or cache
    /// I/O. A concurrent refresh affects the next request, while this request
    /// remains bound to one internally consistent channel/index snapshot.
    ///
    /// # Errors
    ///
    /// Refuses unavailable state or any authenticated preparation/install
    /// failure through one redacted category.
    pub fn prepare_and_install(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, BuildAuthorityError> {
        let (channel, index) = {
            let state = self.lock_state()?;
            (state.channel.clone(), state.index.clone())
        };
        AuthenticatedBuildPreparation::from_verified_channel(
            channel,
            selectors,
            index,
            Arc::clone(&self.adapter),
        )
        .and_then(|preparation| preparation.install(caller, handle))
        .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::PreparationRefused))
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, AuthorityState>, BuildAuthorityError> {
        self.state
            .lock()
            .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::StateUnavailable))
    }
}

struct AuthorityState {
    identity: ChannelAuthorityIdentity,
    channel: VerifiedChannel,
    index: Option<VerifiedIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelAuthorityIdentity {
    sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
}

impl ChannelAuthorityIdentity {
    fn from_channel(channel: &VerifiedChannel) -> Self {
        Self {
            sequence: channel.sequence(),
            policy_version: channel.policy_version(),
            descriptor_sha256: channel.descriptor_sha256(),
        }
    }
}

fn compare_channel_identity(
    current: ChannelAuthorityIdentity,
    candidate: ChannelAuthorityIdentity,
) -> Result<BuildAuthorityUpdate, BuildAuthorityError> {
    if candidate.sequence < current.sequence {
        return Err(BuildAuthorityError::new(
            BuildAuthorityErrorCode::ChannelRollback,
        ));
    }
    if candidate.policy_version < current.policy_version {
        return Err(BuildAuthorityError::new(
            BuildAuthorityErrorCode::PolicyRollback,
        ));
    }
    if candidate.sequence == current.sequence {
        return if candidate == current {
            Ok(BuildAuthorityUpdate::Unchanged)
        } else {
            Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::ChannelReuse,
            ))
        };
    }
    Ok(BuildAuthorityUpdate::Updated)
}

/// Stable broker build-authority refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAuthorityErrorCode {
    /// The candidate channel sequence was older than current authority.
    ChannelRollback,
    /// A channel sequence was reused with a different authenticated identity.
    ChannelReuse,
    /// The candidate policy version was older than current authority.
    PolicyRollback,
    /// The index was authenticated for a different channel descriptor.
    IndexMismatch,
    /// The broker's in-memory authority could not be read safely.
    StateUnavailable,
    /// Authenticated planning or caller-bound installation refused.
    PreparationRefused,
}

/// Redacted failure at the broker-owned build-authority boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildAuthorityError {
    code: BuildAuthorityErrorCode,
}

impl BuildAuthorityError {
    const fn new(code: BuildAuthorityErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable refusal category.
    #[must_use]
    pub const fn code(self) -> BuildAuthorityErrorCode {
        self.code
    }
}

impl fmt::Display for BuildAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authenticated build authority refused")
    }
}

impl Error for BuildAuthorityError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(sequence: u64, policy: u64, digest: u8) -> ChannelAuthorityIdentity {
        ChannelAuthorityIdentity {
            sequence: ChannelSequence::from_u64(sequence).unwrap(),
            policy_version: PolicyVersion::from_u64(policy).unwrap(),
            descriptor_sha256: [digest; 32],
        }
    }

    #[test]
    fn channel_publication_is_monotonic_and_same_sequence_is_exact() {
        let current = identity(7, 3, 0x11);
        assert_eq!(
            compare_channel_identity(current, current).unwrap(),
            BuildAuthorityUpdate::Unchanged
        );
        assert_eq!(
            compare_channel_identity(current, identity(6, 3, 0x10))
                .unwrap_err()
                .code(),
            BuildAuthorityErrorCode::ChannelRollback
        );
        assert_eq!(
            compare_channel_identity(current, identity(7, 3, 0x12))
                .unwrap_err()
                .code(),
            BuildAuthorityErrorCode::ChannelReuse
        );
        assert_eq!(
            compare_channel_identity(current, identity(8, 2, 0x13))
                .unwrap_err()
                .code(),
            BuildAuthorityErrorCode::PolicyRollback
        );
        assert_eq!(
            compare_channel_identity(current, identity(8, 4, 0x14)).unwrap(),
            BuildAuthorityUpdate::Updated
        );
    }

    #[test]
    fn authority_is_send_sync_and_debug_is_opaque() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthenticatedBuildAuthority>();
        assert_eq!(
            BuildAuthorityError::new(BuildAuthorityErrorCode::IndexMismatch).to_string(),
            "authenticated build authority refused"
        );
    }
}
