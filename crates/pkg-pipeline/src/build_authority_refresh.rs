//! Trusted channel refresh composition for broker-owned build authority.

use std::{error::Error, fmt, sync::Arc};

use pkg_channel::{ChannelClient, ChannelError, RefreshOutcome};
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
        let system = production_native_system()
            .map_err(|_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Host))?;
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
                |_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Authority),
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
        let system = production_native_system()
            .map_err(|_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Host))?;
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
        self.authority
            .refresh_with_index(verified_channel, index)
            .map_err(|_| BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Authority))
    }
}

fn into_channel(outcome: RefreshOutcome) -> pkg_channel::VerifiedChannel {
    match outcome {
        RefreshOutcome::Updated(channel) | RefreshOutcome::Unchanged(channel) => channel,
    }
}

fn map_channel_error(error: ChannelError) -> BuildAuthorityRefreshError {
    let code = if matches!(error, ChannelError::IndexVerificationRefused) {
        BuildAuthorityRefreshErrorCode::Index
    } else {
        BuildAuthorityRefreshErrorCode::Channel
    };
    BuildAuthorityRefreshError::new(code)
}

/// Stable trusted-refresh refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildAuthorityRefreshErrorCode {
    /// The compiled host is outside the four V1 systems.
    Host,
    /// TUF, transport, descriptor, target, or durable rollback checks refused.
    Channel,
    /// Compressed index bytes or source identity refused promotion.
    Index,
    /// The broker authority rejected atomic publication.
    Authority,
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
            BuildAuthorityRefreshError::new(BuildAuthorityRefreshErrorCode::Index).to_string(),
            "authenticated build authority refresh refused"
        );
    }
}
