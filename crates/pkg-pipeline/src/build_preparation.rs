//! Broker-private production assembly of authenticated local-build authority.

use std::{error::Error, fmt, sync::Arc};

use pkg_channel::VerifiedChannel;
use pkg_core::PackageSelector;
use pkg_index::IndexDocument;
use pkg_nix::{AuthenticatedCaller, BuildPreview, OperationHandle};

use crate::{
    AuthenticatedBuildIntent, AuthenticatedBuildReplanner, BuildPlanningAdapter,
    ProductionBuildHostFactsProbe,
};

/// A private initial plan paired with its only trusted replanning capability.
///
/// This value is deliberately non-serializable and exposes no plan, path,
/// derivation, cache evidence, or host-readiness field.
pub struct AuthenticatedBuildPreparation {
    replanner: Arc<AuthenticatedBuildReplanner>,
    initial_plan: pkg_nix::BuildPlan,
}

impl fmt::Debug for AuthenticatedBuildPreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBuildPreparation")
            .finish_non_exhaustive()
    }
}

impl AuthenticatedBuildPreparation {
    /// Reconstructs the initial private plan from authenticated product intent.
    ///
    /// The native system comes only from the compiled production host probe;
    /// callers cannot supply a target system or readiness assertion.
    pub fn from_verified_channel(
        channel: VerifiedChannel,
        selectors: Vec<PackageSelector>,
        index: Option<IndexDocument>,
        adapter: Arc<dyn BuildPlanningAdapter>,
    ) -> Result<Self, BuildPreparationError> {
        let host = Arc::new(
            ProductionBuildHostFactsProbe::from_verified_channel(&channel)
                .map_err(|_| BuildPreparationError::new(BuildPreparationErrorCode::HostRefused))?,
        );
        let intent = AuthenticatedBuildIntent::new(channel, selectors, host.system(), index)
            .map_err(|_| BuildPreparationError::new(BuildPreparationErrorCode::IntentRefused))?;
        let replanner = Arc::new(AuthenticatedBuildReplanner::new(intent, adapter, host));
        let initial_plan = replanner
            .initial_plan()
            .map_err(|_| BuildPreparationError::new(BuildPreparationErrorCode::PlanningRefused))?;
        Ok(Self {
            replanner,
            initial_plan,
        })
    }

    /// Installs the private plan and retained replanner under one build handle.
    ///
    /// # Errors
    ///
    /// Fails closed unless the handle is live, caller-bound, and authorized for
    /// a build whose private plan has not already been prepared.
    pub fn install(
        self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<BuildPreview, BuildPreparationError> {
        caller
            .prepare_build_with_replanner(handle, self.initial_plan, self.replanner)
            .map_err(|_| BuildPreparationError::new(BuildPreparationErrorCode::BrokerRefused))
    }
}

/// Stable authenticated-build preparation refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPreparationErrorCode {
    /// Current native host facts or managed configuration were unavailable.
    HostRefused,
    /// The typed selector batch was invalid under the verified channel.
    IntentRefused,
    /// Source, resolution, cache classification, or plan construction refused.
    PlanningRefused,
    /// The caller-bound broker handle would not retain this preparation.
    BrokerRefused,
}

/// Redacted failure at the production build-preparation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildPreparationError {
    code: BuildPreparationErrorCode,
}

impl BuildPreparationError {
    const fn new(code: BuildPreparationErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> BuildPreparationErrorCode {
        self.code
    }
}

impl fmt::Display for BuildPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authenticated build preparation refused")
    }
}

impl Error for BuildPreparationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn preparation_is_broker_private_send_sync_and_debug_is_opaque() {
        assert_send_sync::<AuthenticatedBuildPreparation>();
        assert_eq!(
            format!(
                "{:?}",
                BuildPreparationError::new(BuildPreparationErrorCode::HostRefused)
            ),
            "BuildPreparationError { code: HostRefused }"
        );
    }
}
