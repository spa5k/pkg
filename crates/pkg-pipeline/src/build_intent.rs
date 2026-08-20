//! Broker-private authenticated local-build replanning.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use pkg_channel::VerifiedChannel;
use pkg_core::{PackageSelector, System};
use pkg_index::IndexDocument;
use pkg_nix::{
    BuildCacheErrorCode, BuildCacheProbe, BuildCacheTarget, BuildPlan, BuildReadiness, NixAdapter,
    NixpkgsFetchSpec, NixpkgsMetadataRunner, TrustedBuildReplanner, TrustedReplanError,
    classify_build_cache, fetch_verified_nixpkgs,
};

use crate::{
    AuthenticatedBuildPolicy, preflight_cache_only, prepare_local_build_plan, resolve_install,
};

const MAX_BUILD_SELECTORS: usize = 4_096;

/// Adapter authority required to reconstruct a build plan inside the broker.
///
/// No caller implements this boundary in production: the contained
/// [`pkg_nix::RealNixAdapter`] supplies all three closed capabilities.
pub trait BuildPlanningAdapter: NixAdapter + BuildCacheProbe + NixpkgsMetadataRunner {}

impl<T> BuildPlanningAdapter for T where T: NixAdapter + BuildCacheProbe + NixpkgsMetadataRunner {}

/// Authenticated policy plus validated package intent retained for exact replanning.
#[derive(Clone)]
pub struct AuthenticatedBuildIntent {
    channel: VerifiedChannel,
    selectors: Vec<PackageSelector>,
    system: System,
    index: Option<IndexDocument>,
}

impl fmt::Debug for AuthenticatedBuildIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBuildIntent")
            .field("channel_sequence", &self.channel.sequence())
            .field("selector_count", &self.selectors.len())
            .field("system", &self.system)
            .field("has_index", &self.index.is_some())
            .finish()
    }
}

impl AuthenticatedBuildIntent {
    /// Retains one bounded validated request under authenticated channel policy.
    pub fn new(
        channel: VerifiedChannel,
        selectors: Vec<PackageSelector>,
        system: System,
        index: Option<IndexDocument>,
    ) -> Result<Self, BuildIntentError> {
        validate_selectors(&selectors)?;
        Ok(Self {
            channel,
            selectors,
            system,
            index,
        })
    }

    /// Reconstructs the complete deterministic plan from current trusted facts.
    ///
    /// This method materializes and verifies only the channel-pinned Nixpkgs
    /// source, evaluates the retained typed selectors without realization,
    /// classifies expected paths through the managed cache probe, and binds the
    /// resulting evidence to current runtime/readiness facts. Calling it again
    /// performs the same checks again; no prior derivation, path, or cache claim
    /// is reused as authority.
    pub fn plan(
        &self,
        adapter: &dyn BuildPlanningAdapter,
        host_system: System,
        readiness: BuildReadiness,
        host_cores: u32,
    ) -> Result<BuildPlan, BuildIntentError> {
        let policy = AuthenticatedBuildPolicy::from_verified_channel(&self.channel)
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::InvalidPolicy))?;
        let source_spec = NixpkgsFetchSpec::from_verified_channel(&self.channel)
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::InvalidPolicy))?;
        let source = fetch_verified_nixpkgs(&source_spec, adapter)
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::SourceUnavailable))?;
        let runtime = adapter
            .version()
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::RuntimeUnavailable))?;
        let resolved = resolve_install(
            &self.selectors,
            &source,
            self.system,
            self.index.as_ref(),
            adapter,
        )
        .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::ResolutionFailed))?;
        let preflight = preflight_cache_only(&resolved)
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::ResolutionFailed))?
            .outputs()
            .to_vec();
        let cache_targets = resolved
            .targets()
            .iter()
            .map(|target| {
                let subjects = target
                    .build_cache_subjects()
                    .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::ResolutionFailed))?;
                let selected_outputs = preflight
                    .iter()
                    .filter(|output| output.selector_id() == target.selector().id())
                    .map(|output| output.store_path().clone())
                    .collect();
                BuildCacheTarget::new(subjects, selected_outputs).map_err(BuildIntentError::cache)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let evidence =
            classify_build_cache(&cache_targets, adapter).map_err(BuildIntentError::cache)?;
        prepare_local_build_plan(
            &policy,
            &resolved,
            &runtime,
            host_system,
            evidence,
            readiness,
            host_cores,
        )
        .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::PlanRejected))
    }
}

/// Stable broker-private authenticated-replanning refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildIntentErrorCode {
    /// The retained selector batch was empty, duplicate, or oversized.
    InvalidIntent,
    /// Authenticated channel policy could not be promoted.
    InvalidPolicy,
    /// The pinned source could not be materialized and independently verified.
    SourceUnavailable,
    /// The managed runtime did not satisfy its closed version contract.
    RuntimeUnavailable,
    /// Evaluate-only resolution failed.
    ResolutionFailed,
    /// Local/cache evidence was unavailable, invalid, or showed no build need.
    CacheClassificationFailed,
    /// Current source, platform, readiness, or plan invariants refused the build.
    PlanRejected,
}

/// Redacted failure from broker-private authenticated replanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildIntentError {
    code: BuildIntentErrorCode,
    cache_code: Option<BuildCacheErrorCode>,
}

impl BuildIntentError {
    const fn new(code: BuildIntentErrorCode) -> Self {
        Self {
            code,
            cache_code: None,
        }
    }

    const fn cache(error: pkg_nix::BuildCacheError) -> Self {
        Self {
            code: BuildIntentErrorCode::CacheClassificationFailed,
            cache_code: Some(error.code()),
        }
    }

    /// Returns the stable private replanning failure category.
    #[must_use]
    pub const fn code(self) -> BuildIntentErrorCode {
        self.code
    }

    /// Returns the closed cache-classification category when that stage ran.
    #[must_use]
    pub const fn cache_code(self) -> Option<BuildCacheErrorCode> {
        self.cache_code
    }
}

impl fmt::Display for BuildIntentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authenticated local-build replanning refused")
    }
}

impl std::error::Error for BuildIntentError {}

/// Current trusted host facts included in deterministic build planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildHostFacts {
    system: System,
    readiness: BuildReadiness,
    host_cores: u32,
}

impl BuildHostFacts {
    /// Constructs one host observation; a zero core count is never authoritative.
    pub fn new(
        system: System,
        readiness: BuildReadiness,
        host_cores: u32,
    ) -> Result<Self, BuildHostFactsError> {
        if host_cores == 0 {
            return Err(BuildHostFactsError);
        }
        Ok(Self {
            system,
            readiness,
            host_cores,
        })
    }

    /// Returns the authenticated native target system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the current fail-closed sandbox and build-user evidence.
    #[must_use]
    pub const fn readiness(&self) -> &BuildReadiness {
        &self.readiness
    }

    /// Returns the observed host core count used by admission policy.
    #[must_use]
    pub const fn host_cores(&self) -> u32 {
        self.host_cores
    }
}

/// Redacted failure to observe current build host facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildHostFactsError;

impl fmt::Display for BuildHostFactsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("build host facts unavailable")
    }
}

impl std::error::Error for BuildHostFactsError {}

/// Trusted observer invoked separately for preview and admission-time replan.
pub trait BuildHostFactsProbe: Send + Sync {
    /// Observes the native system, build readiness, and available core count.
    fn observe(&self) -> Result<BuildHostFacts, BuildHostFactsError>;
}

/// Broker-retained implementation of the trusted replanning capability.
pub struct AuthenticatedBuildReplanner {
    intent: AuthenticatedBuildIntent,
    adapter: Arc<dyn BuildPlanningAdapter>,
    host: Arc<dyn BuildHostFactsProbe>,
}

impl fmt::Debug for AuthenticatedBuildReplanner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedBuildReplanner")
            .field("intent", &self.intent)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedBuildReplanner {
    /// Binds authenticated intent to the contained adapter and host observer.
    #[must_use]
    pub fn new(
        intent: AuthenticatedBuildIntent,
        adapter: Arc<dyn BuildPlanningAdapter>,
        host: Arc<dyn BuildHostFactsProbe>,
    ) -> Self {
        Self {
            intent,
            adapter,
            host,
        }
    }

    /// Produces the initial private plan used for the public preview.
    pub fn initial_plan(&self) -> Result<BuildPlan, BuildIntentError> {
        let facts = self
            .host
            .observe()
            .map_err(|_| BuildIntentError::new(BuildIntentErrorCode::PlanRejected))?;
        self.intent.plan(
            self.adapter.as_ref(),
            facts.system,
            facts.readiness,
            facts.host_cores,
        )
    }
}

impl TrustedBuildReplanner for AuthenticatedBuildReplanner {
    fn replan(&self) -> Result<BuildPlan, TrustedReplanError> {
        self.initial_plan().map_err(|_| TrustedReplanError)
    }
}

fn validate_selectors(selectors: &[PackageSelector]) -> Result<(), BuildIntentError> {
    if selectors.is_empty() || selectors.len() > MAX_BUILD_SELECTORS {
        return Err(BuildIntentError::new(BuildIntentErrorCode::InvalidIntent));
    }
    let mut ids = BTreeSet::new();
    if selectors
        .iter()
        .any(|selector| !ids.insert(selector.id().as_str()))
    {
        return Err(BuildIntentError::new(BuildIntentErrorCode::InvalidIntent));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pkg_core::{OutputSelection, SelectorId, SelectorInput, SourceRevision, VersionPreference};

    use super::*;

    fn selector(id: &str) -> PackageSelector {
        PackageSelector::new(
            SelectorId::new(id).unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        )
    }

    #[test]
    fn selector_batch_is_bounded_and_identity_unique_before_any_nix_work() {
        assert_eq!(
            validate_selectors(&[]).unwrap_err().code(),
            BuildIntentErrorCode::InvalidIntent
        );
        assert_eq!(
            validate_selectors(&[selector("sel_one"), selector("sel_one")])
                .unwrap_err()
                .code(),
            BuildIntentErrorCode::InvalidIntent
        );
        assert!(validate_selectors(&[selector("sel_one"), selector("sel_two")]).is_ok());
    }

    #[test]
    fn host_facts_refuse_unknown_core_capacity() {
        let readiness = BuildReadiness::new(true, false, true, true, true);
        assert!(BuildHostFacts::new(System::X8664Linux, readiness.clone(), 8).is_ok());
        assert_eq!(
            BuildHostFacts::new(System::X8664Linux, readiness, 0),
            Err(BuildHostFactsError)
        );
    }
}
