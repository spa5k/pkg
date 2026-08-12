//! Broker-owned current channel and catalog authority for local builds.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex},
};

use pkg_channel::VerifiedChannel;
use pkg_core::{ChannelSequence, PackageSelector, PolicyVersion};
use pkg_index::{IndexQuery, InfoLookup, SearchOptions, VerifiedIndex};
use pkg_nix::{
    AuthenticatedCaller, BrokerErrorCode, BuildPreview, CacheInstallAttempt, CacheInstallOutcome,
    CatalogInfoLookup, CatalogInfoReport, CatalogInfoRequest, CatalogPackageInfo,
    CatalogPackageSummary, CatalogSearchReport, CatalogSearchRequest, InstallDownloadProgress,
    NixAdapter, NixpkgsFetchSpec, OperationHandle, fetch_verified_nixpkgs,
};

use crate::{
    AcquireError, AuthenticatedBuildPreparation, BuildHostFacts, BuildHostFactsProbe,
    BuildPlanningAdapter, acquire_cache_only_with_progress, assemble_cache_install_evidence,
    host_facts::{ProductionBuildHostFactsProbe, production_native_system},
    preflight_cache_only, resolve_install, verify_acquired,
};

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

    /// Starts broker authority from one exact verified channel/index pair.
    ///
    /// # Errors
    ///
    /// Refuses an index capability authenticated for any other descriptor.
    pub fn new_with_index(
        channel: VerifiedChannel,
        index: VerifiedIndex,
        adapter: Arc<dyn BuildPlanningAdapter>,
    ) -> Result<Self, BuildAuthorityError> {
        if !index.matches_channel(&channel) {
            return Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::IndexMismatch,
            ));
        }
        Ok(Self {
            state: Mutex::new(AuthorityState {
                identity: ChannelAuthorityIdentity::from_channel(&channel),
                channel,
                index: Some(index),
            }),
            adapter,
        })
    }

    /// Atomically publishes one exact verified channel/index pair.
    ///
    /// # Errors
    ///
    /// Refuses mismatched index identity, rollback, descriptor reuse, policy
    /// downgrade, or unavailable in-memory state without partial publication.
    pub fn refresh_with_index(
        &self,
        channel: VerifiedChannel,
        index: VerifiedIndex,
    ) -> Result<BuildAuthorityUpdate, BuildAuthorityError> {
        if !index.matches_channel(&channel) {
            return Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::IndexMismatch,
            ));
        }
        let candidate = ChannelAuthorityIdentity::from_channel(&channel);
        let mut state = self.lock_state()?;
        let channel_update = compare_channel_identity(state.identity, candidate)?;
        if channel_update == BuildAuthorityUpdate::Unchanged && state.index.as_ref() == Some(&index)
        {
            return Ok(BuildAuthorityUpdate::Unchanged);
        }
        state.identity = candidate;
        state.channel = channel;
        state.index = Some(index);
        Ok(BuildAuthorityUpdate::Updated)
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

    /// Searches only the broker-owned verified native index.
    ///
    /// # Errors
    ///
    /// Refuses an unavailable index, invalid bounded query, or metadata that
    /// cannot fit the closed product response contract.
    pub fn search_catalog(
        &self,
        request: &CatalogSearchRequest,
    ) -> Result<CatalogSearchReport, BuildAuthorityError> {
        let state = self.lock_state()?;
        let index = state
            .index
            .as_ref()
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let options = SearchOptions::new(
            request.query(),
            usize::from(request.limit()),
            request.exact(),
            request.license(),
        )
        .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let response = IndexQuery::new(index.document(), false)
            .search(&options)
            .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let sequence = ChannelSequence::from_u64(response.channel_seq())
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let results = response
            .results()
            .iter()
            .map(catalog_summary)
            .collect::<Result<Vec<_>, _>>()?;
        CatalogSearchReport::new(sequence, results)
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))
    }

    /// Inspects one selector only through the broker-owned verified native index.
    ///
    /// # Errors
    ///
    /// Refuses an unavailable index, invalid selector, or metadata that cannot
    /// fit the closed product response contract.
    pub fn info_catalog(
        &self,
        request: &CatalogInfoRequest,
    ) -> Result<CatalogInfoReport, BuildAuthorityError> {
        let state = self.lock_state()?;
        let index = state
            .index
            .as_ref()
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let response = IndexQuery::new(index.document(), false)
            .info(request.selector())
            .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let sequence = ChannelSequence::from_u64(response.channel_seq())
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
        let lookup = match response.lookup() {
            InfoLookup::Found { package } => {
                CatalogInfoLookup::Found(Box::new(catalog_info(package)?))
            }
            InfoLookup::Ambiguous { candidates } => CatalogInfoLookup::Ambiguous(
                candidates
                    .iter()
                    .map(catalog_summary)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            InfoLookup::NotFound { .. } => CatalogInfoLookup::NotFound,
        };
        CatalogInfoReport::new(sequence, lookup)
            .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))
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

    /// Acquires one cache-first install from the current authenticated authority snapshot.
    ///
    /// The broker holds GC inhibition across substitution and retains it with
    /// the resulting evidence until the caller publishes the exact root set.
    /// A cache miss is a successful classification, not an authority failure.
    ///
    /// # Errors
    ///
    /// Refuses unavailable state, invalid authenticated source/index data,
    /// resolution drift, substitute verification failure, or broker lifecycle
    /// cancellation through one redacted category.
    pub fn acquire_install(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
    ) -> Result<CacheInstallOutcome, BuildAuthorityError> {
        self.acquire_install_with_progress(selectors, caller, handle, &mut |_| Ok(()))
    }

    /// Acquires one cache-first install and streams sanitized trusted bytes.
    pub fn acquire_install_with_progress(
        &self,
        selectors: Vec<PackageSelector>,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
    ) -> Result<CacheInstallOutcome, BuildAuthorityError> {
        let (channel, index) = {
            let state = self.lock_state()?;
            (state.channel.clone(), state.index.clone())
        };
        let adapter = Arc::clone(&self.adapter);
        caller
            .acquire_cache_install(handle, || {
                let source_spec =
                    NixpkgsFetchSpec::from_verified_channel(&channel).map_err(|_| ())?;
                let source =
                    fetch_verified_nixpkgs(&source_spec, adapter.as_ref()).map_err(|_| ())?;
                let system = production_native_system().map_err(|_| ())?;
                let resolved = resolve_install(
                    &selectors,
                    &source,
                    system,
                    index.as_ref().map(VerifiedIndex::document),
                    adapter.as_ref(),
                )
                .map_err(|_| ())?;
                let preflight = preflight_cache_only(&resolved).map_err(|_| ())?;
                let acquired = match acquire_cache_only_with_progress(
                    &resolved,
                    &preflight,
                    &channel,
                    adapter.as_ref(),
                    adapter.as_ref(),
                    progress,
                ) {
                    Ok(acquired) => acquired,
                    Err(AcquireError::BuildRequired) => {
                        return Ok(CacheInstallAttempt::BuildRequired);
                    }
                    Err(AcquireError::Refused) => return Err(()),
                };
                let verified = verify_acquired(acquired).map_err(|_| ())?;
                let evidence =
                    assemble_cache_install_evidence(&resolved, &verified, adapter.as_ref())
                        .map_err(|_| ())?;
                Ok(CacheInstallAttempt::Acquired(evidence))
            })
            .map_err(|error| {
                let code = match error.code() {
                    BrokerErrorCode::AdmissionCancelled
                    | BrokerErrorCode::SessionRestarted
                    | BrokerErrorCode::OperationExpired => {
                        BuildAuthorityErrorCode::AcquisitionCancelled
                    }
                    BrokerErrorCode::InvalidOperationHandle
                    | BrokerErrorCode::InvalidAdmissionTransition => {
                        BuildAuthorityErrorCode::AcquisitionIntentRefused
                    }
                    BrokerErrorCode::CacheAcquisitionFailed => {
                        BuildAuthorityErrorCode::AcquisitionRefused
                    }
                    _ => BuildAuthorityErrorCode::AcquisitionRefused,
                };
                BuildAuthorityError::new(code)
            })
    }

    /// Returns the exact contained adapter used by this authority for planning.
    ///
    /// Trusted broker composition uses this to prevent planning and execution
    /// from being wired to different managed-runtime backends.
    #[must_use]
    pub fn adapter(&self) -> Arc<dyn NixAdapter> {
        Arc::clone(&self.adapter) as Arc<dyn NixAdapter>
    }

    /// Returns the policy version of the current authenticated channel.
    ///
    /// # Errors
    ///
    /// Returns a closed authority error if the private state lock is poisoned.
    pub fn policy_version(&self) -> Result<PolicyVersion, BuildAuthorityError> {
        Ok(self.lock_state()?.identity.policy_version)
    }

    /// Observes one current, internally consistent repair-build policy and host snapshot.
    pub fn repair_build_context(
        &self,
    ) -> Result<(PolicyVersion, BuildHostFacts), BuildAuthorityError> {
        let (channel, identity) = {
            let state = self.lock_state()?;
            (state.channel.clone(), state.identity)
        };
        let facts = ProductionBuildHostFactsProbe::from_verified_channel(&channel)
            .and_then(|probe| probe.observe())
            .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::PreparationRefused))?;
        if self.lock_state()?.identity != identity {
            return Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::PreparationRefused,
            ));
        }
        Ok((identity.policy_version, facts))
    }

    /// Runs one short approval transition only while the expected policy is current.
    pub fn under_current_policy<T>(
        &self,
        expected: PolicyVersion,
        transition: impl FnOnce() -> T,
    ) -> Result<T, BuildAuthorityError> {
        let state = self.lock_state()?;
        if state.identity.policy_version != expected {
            return Err(BuildAuthorityError::new(
                BuildAuthorityErrorCode::PreparationRefused,
            ));
        }
        let result = transition();
        drop(state);
        Ok(result)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, AuthorityState>, BuildAuthorityError> {
        self.state
            .lock()
            .map_err(|_| BuildAuthorityError::new(BuildAuthorityErrorCode::StateUnavailable))
    }
}

fn catalog_summary(
    value: &pkg_index::PackageSummary,
) -> Result<CatalogPackageSummary, BuildAuthorityError> {
    CatalogPackageSummary::new(
        value.package(),
        value.name(),
        value.version(),
        value.description(),
        value.licenses().to_vec(),
        value.available(),
        value.broken(),
    )
    .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))
}

fn catalog_info(value: &pkg_index::PackageInfo) -> Result<CatalogPackageInfo, BuildAuthorityError> {
    let summary = CatalogPackageSummary::new(
        value.package(),
        value.name(),
        value.version(),
        value.description(),
        value.licenses().to_vec(),
        value.available(),
        value.broken(),
    )
    .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))?;
    CatalogPackageInfo::new(
        summary,
        value.homepage(),
        value.outputs().to_vec(),
        value
            .platforms()
            .iter()
            .map(|platform| (*platform).to_owned())
            .collect(),
        value.catalog_revision(),
        value.catalog_generated_at(),
    )
    .ok_or_else(|| BuildAuthorityError::new(BuildAuthorityErrorCode::CatalogUnavailable))
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
    /// The current authenticated native index or bounded query result was unavailable.
    CatalogUnavailable,
    /// Authenticated planning or caller-bound installation refused.
    PreparationRefused,
    /// Cache-first acquisition or its broker lifecycle failed closed.
    AcquisitionRefused,
    /// Cache-first acquisition was cancelled by the operation lifecycle.
    AcquisitionCancelled,
    /// Cache-first acquisition was requested with an invalid operation intent.
    AcquisitionIntentRefused,
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
