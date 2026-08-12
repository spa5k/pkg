//! Trusted channel refresh composition for broker-owned build authority.

use std::{error::Error, fmt, sync::Arc};

use pkg_channel::{ChannelClient, ChannelError, RefreshOutcome};
use pkg_core::ChannelSequence;
use pkg_index::verify_index_artifact;

use crate::{
    AuthenticatedBuildAuthority, BuildAuthorityUpdate, BuildPlanningAdapter,
    host_facts::production_native_system,
};

/// Long-lived trusted refresh owner for broker build authority.
pub struct AuthenticatedBuildAuthorityService {
    channel: ChannelClient,
    authority: Arc<AuthenticatedBuildAuthority>,
}

impl fmt::Debug for AuthenticatedBuildAuthorityService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBuildAuthorityService")
            .finish_non_exhaustive()
    }
}

impl AuthenticatedBuildAuthorityService {
    /// Authenticates current channel and index before creating broker authority.
    ///
    /// The native system is compile-target derived inside production code. No
    /// command caller can supply system, channel, target bytes, or index.
    ///
    /// # Errors
    ///
    /// Refuses unsupported hosts, channel/TUF failures, compressed-index
    /// verification failures, or inconsistent authority publication.
    pub async fn bootstrap(
        channel: ChannelClient,
        adapter: Arc<dyn BuildPlanningAdapter>,
    ) -> Result<Self, BuildAuthorityRefreshError> {
        let system = production_native_system().map_err(|_| {
            BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Service)
        })?;
        let refresh = channel
            .refresh_with_index(system, |verified_channel, target| {
                if target.system() != system {
                    return Err(());
                }
                verify_index_artifact(target.bytes(), verified_channel, system).map_err(|_| ())
            })
            .await
            .map_err(map_channel_error)?;
        let (outcome, index) = refresh.into_parts();
        let verified_channel = into_channel(outcome);
        let authority =
            AuthenticatedBuildAuthority::new_with_index(verified_channel, index, adapter).map_err(
                |_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Service),
            )?;
        Ok(Self {
            channel,
            authority: Arc::new(authority),
        })
    }

    /// Returns the opaque authority installed into the broker dispatcher.
    #[must_use]
    pub fn authority(&self) -> Arc<AuthenticatedBuildAuthority> {
        Arc::clone(&self.authority)
    }

    /// Authenticates and atomically publishes the next channel/index pair.
    ///
    /// # Errors
    ///
    /// Refuses without changing live broker authority unless every TUF,
    /// compressed-index, source-identity, and monotonic publication check passes.
    pub async fn refresh(&self) -> Result<BuildAuthorityUpdate, BuildAuthorityRefreshError> {
        self.refresh_with_sequence().await.map(|(update, _)| update)
    }

    /// Authenticates and publishes the next channel/index pair and returns its
    /// sanitized sequence from the same verified capability.
    ///
    /// # Errors
    ///
    /// Has the same fail-closed behavior as [`Self::refresh`].
    pub async fn refresh_with_sequence(
        &self,
    ) -> Result<(BuildAuthorityUpdate, ChannelSequence), BuildAuthorityRefreshError> {
        let system = production_native_system().map_err(|_| {
            BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Service)
        })?;
        let refresh = self
            .channel
            .refresh_with_index(system, |verified_channel, target| {
                if target.system() != system {
                    return Err(());
                }
                verify_index_artifact(target.bytes(), verified_channel, system).map_err(|_| ())
            })
            .await
            .map_err(map_channel_error)?;
        let (outcome, index) = refresh.into_parts();
        let verified_channel = into_channel(outcome);
        let sequence = verified_channel.sequence();
        self.authority
            .refresh_with_index(verified_channel, index)
            .map(|update| (update, sequence))
            .map_err(|_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Service))
    }
}

fn into_channel(outcome: RefreshOutcome) -> pkg_channel::VerifiedChannel {
    match outcome {
        RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
    }
}

fn map_channel_error(error: ChannelError) -> BuildAuthorityRefreshError {
    let code = match error {
        ChannelError::TransportUnavailable => BuildAuthorityRefreshErrorCode::Network,
        ChannelError::DatastoreBusy => BuildAuthorityRefreshErrorCode::Busy,
        ChannelError::DatastoreUnavailable | ChannelError::AcceptedStateUnavailable => {
            BuildAuthorityRefreshErrorCode::Service
        }
        _ => BuildAuthorityRefreshErrorCode::Verification,
    };
    BuildAuthorityRefreshError::new(code)
}

/// Stable trusted-refresh refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAuthorityRefreshErrorCode {
    /// Authenticated repository bytes could not be acquired.
    Network,
    /// Another process owns the durable refresh lease.
    Busy,
    /// TUF, descriptor, target, rollback, or index verification refused.
    Verification,
    /// Host, durable state, or atomic authority publication is unavailable.
    Service,
}

/// Redacted failure at the trusted build-authority refresh boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildAuthorityRefreshError {
    code: BuildAuthorityRefreshErrorCode,
}

impl BuildAuthorityRefreshError {
    const fn new(code: BuildAuthorityRefreshErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable refusal category.
    #[must_use]
    pub const fn code(self) -> BuildAuthorityRefreshErrorCode {
        self.code
    }
}

impl fmt::Display for BuildAuthorityRefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authenticated build authority refresh refused")
    }
}

impl Error for BuildAuthorityRefreshError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_service_boundary_is_opaque_send_sync_and_native() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthenticatedBuildAuthorityService>();
        assert!(production_native_system().is_ok());
        assert_eq!(
            BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Verification)
                .to_string(),
            "authenticated build authority refresh refused"
        );
    }

    #[test]
    fn refresh_failures_keep_public_retry_and_trust_classes_distinct() {
        for (error, expected) in [
            (
                ChannelError::TransportUnavailable,
                BuildAuthorityRefreshErrorCode::Network,
            ),
            (
                ChannelError::DatastoreBusy,
                BuildAuthorityRefreshErrorCode::Busy,
            ),
            (
                ChannelError::AcceptedStateUnavailable,
                BuildAuthorityRefreshErrorCode::Service,
            ),
            (
                ChannelError::TufVerification(String::from("redacted")),
                BuildAuthorityRefreshErrorCode::Verification,
            ),
            (
                ChannelError::IndexVerificationRefused,
                BuildAuthorityRefreshErrorCode::Verification,
            ),
        ] {
            assert_eq!(map_channel_error(error).code(), expected);
        }
    }
}
