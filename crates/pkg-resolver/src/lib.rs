//! Pure orchestration from package intent to an evaluate-only Nix plan.
//!
//! The disposable index may select an attribute path, but never supplies
//! authoritative version or store identity. The authenticated Nixpkgs source
//! supplies all source identity, and the adapter may only evaluate here: it
//! cannot substitute, build, or claim that expected paths are realized.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

use pkg_core::{
    AttributePath, ChannelSequence, NarHash, NixpkgsRevision, PackageSelector, SourceRevision,
    System,
};
use pkg_index::{IndexDocument, IndexQuery, InfoLookup};
use pkg_nix::{
    BuildCacheSubject, BuildPlanTarget, DerivationPlanReport, EvaluateDerivationRequest,
    NixAdapter, NixAdapterError, VerifiedNixpkgsSource,
};

/// A selector paired with the authoritative result of pinned evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackagePlan {
    selector: PackageSelector,
    plan: DerivationPlanReport,
}

impl ResolvedPackagePlan {
    /// Returns the selector with its canonical attribute filled in.
    #[must_use]
    pub const fn selector(&self) -> &PackageSelector {
        &self.selector
    }

    /// Returns the evaluate-only derivation plan.
    #[must_use]
    pub const fn plan(&self) -> &DerivationPlanReport {
        &self.plan
    }

    /// Promotes this opaque resolver-owned pair into one private build target.
    ///
    /// The selector and report cannot be rebound independently because both are
    /// recovered from this resolver-produced value.
    pub fn build_plan_target(&self) -> Result<BuildPlanTarget, ResolveError> {
        let attribute = self
            .selector
            .attribute()
            .ok_or_else(|| ResolveError::new(ResolveErrorCode::InvalidSelector))?;
        Ok(BuildPlanTarget::new(
            self.selector.id().clone(),
            self.selector.selector().clone(),
            attribute.clone(),
            self.selector.version_preference().clone(),
            self.selector.outputs().clone(),
            self.selector.source_revision().clone(),
            self.plan.clone(),
        ))
    }

    /// Returns cache subjects from this same opaque resolver-owned pair.
    pub fn build_cache_subjects(&self) -> Result<Vec<BuildCacheSubject>, ResolveError> {
        self.plan
            .derivations()
            .iter()
            .map(|derivation| {
                BuildCacheSubject::new(
                    derivation.derivation().clone(),
                    derivation.outputs().values().cloned().collect(),
                )
                .map_err(|_| ResolveError::new(ResolveErrorCode::InvalidSelector))
            })
            .collect()
    }
}

/// Closed, redacted resolution failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveErrorCode {
    /// The selector requested a different authenticated source.
    SourceMismatch,
    /// The selector already carries realized identity and belongs in the
    /// acquire/verify path rather than evaluate-only resolution.
    AlreadyRealized,
    /// The discovery index produced more than one canonical package.
    AmbiguousSelector,
    /// Neither the usable index nor direct attribute syntax resolved input.
    PackageNotFound,
    /// The resolved selector could not be promoted safely.
    InvalidSelector,
    /// Evaluate-only Nix execution failed closed.
    EvaluationFailed,
    /// The authoritative evaluated version violated user intent.
    VersionMismatch,
}

/// A bounded resolution error that never includes paths or raw Nix output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveError {
    code: ResolveErrorCode,
    candidate_count: Option<usize>,
}

impl ResolveError {
    const fn new(code: ResolveErrorCode) -> Self {
        Self {
            code,
            candidate_count: None,
        }
    }

    const fn ambiguous(candidate_count: usize) -> Self {
        Self {
            code: ResolveErrorCode::AmbiguousSelector,
            candidate_count: Some(candidate_count),
        }
    }

    /// Returns the stable public failure category.
    #[must_use]
    pub const fn code(self) -> ResolveErrorCode {
        self.code
    }

    /// Returns a bounded ambiguity count when applicable.
    #[must_use]
    pub const fn candidate_count(self) -> Option<usize> {
        self.candidate_count
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "package resolution refused: {:?}", self.code)
    }
}

impl std::error::Error for ResolveError {}

/// Resolves one selector against an authenticated source and evaluates it.
///
/// A matching index is optional and discovery-only. Missing, stale-identity,
/// or not-found index data falls back to interpreting the selector as the
/// conservative V1 attribute-path grammar. Ambiguity is never guessed.
pub fn resolve_package(
    selector: &PackageSelector,
    source: &VerifiedNixpkgsSource,
    system: System,
    index: Option<&IndexDocument>,
    adapter: &dyn NixAdapter,
) -> Result<ResolvedPackagePlan, ResolveError> {
    resolve_with_source(
        selector,
        source.channel_sequence(),
        source.revision(),
        source.nar_hash(),
        system,
        index,
        adapter,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resolution boundary needs the full authenticated source context; no coherent subset exists"
)]
fn resolve_with_source(
    selector: &PackageSelector,
    channel_sequence: ChannelSequence,
    revision: &NixpkgsRevision,
    nar_hash: &NarHash,
    system: System,
    index: Option<&IndexDocument>,
    adapter: &dyn NixAdapter,
) -> Result<ResolvedPackagePlan, ResolveError> {
    if selector.pin_state().is_pinned() {
        return Err(ResolveError::new(ResolveErrorCode::AlreadyRealized));
    }
    verify_source_revision(selector.source_revision(), channel_sequence, revision)?;
    let attribute = match selector.attribute() {
        Some(attribute) => attribute.clone(),
        None => discover_attribute(selector, channel_sequence, revision, system, index)?,
    };
    let request = EvaluateDerivationRequest::new(
        attribute.clone(),
        system,
        revision.clone(),
        nar_hash.clone(),
        selector.outputs().clone(),
    )
    .map_err(|_| ResolveError::new(ResolveErrorCode::InvalidSelector))?;
    let plan = adapter
        .evaluate_derivation(&request)
        .map_err(map_adapter_error)?;
    if !selector.version_preference().matches(plan.version()) {
        return Err(ResolveError::new(ResolveErrorCode::VersionMismatch));
    }
    let resolved_selector = if selector.attribute().is_some() {
        selector.clone()
    } else {
        selector
            .clone()
            .with_attribute(attribute)
            .map_err(|_| ResolveError::new(ResolveErrorCode::InvalidSelector))?
    };
    Ok(ResolvedPackagePlan {
        selector: resolved_selector,
        plan,
    })
}

fn verify_source_revision(
    requested: &SourceRevision,
    channel_sequence: ChannelSequence,
    revision: &NixpkgsRevision,
) -> Result<(), ResolveError> {
    let matches = match requested {
        SourceRevision::CurrentChannel => true,
        SourceRevision::PinnedChannel(sequence) => sequence == &channel_sequence,
        SourceRevision::ExactRevision(requested) => requested == revision,
    };
    if matches {
        Ok(())
    } else {
        Err(ResolveError::new(ResolveErrorCode::SourceMismatch))
    }
}

fn discover_attribute(
    selector: &PackageSelector,
    channel_sequence: ChannelSequence,
    revision: &NixpkgsRevision,
    system: System,
    index: Option<&IndexDocument>,
) -> Result<AttributePath, ResolveError> {
    let usable_index = index.filter(|document| {
        document.channel_seq() == channel_sequence.get().get()
            && document.nixpkgs_rev() == revision.as_str()
            && document.system() == system.as_str()
    });
    if let Some(document) = usable_index
        && let Ok(response) = IndexQuery::new(document, false).info(selector.selector().as_str())
    {
        match response.lookup() {
            InfoLookup::Found { package } => {
                return AttributePath::new(package.package())
                    .map_err(|_| ResolveError::new(ResolveErrorCode::InvalidSelector));
            }
            InfoLookup::Ambiguous { candidates } => {
                return Err(ResolveError::ambiguous(candidates.len()));
            }
            InfoLookup::NotFound { .. } => {}
        }
    }
    AttributePath::new(selector.selector().as_str())
        .map_err(|_| ResolveError::new(ResolveErrorCode::PackageNotFound))
}

fn map_adapter_error(_: NixAdapterError) -> ResolveError {
    ResolveError::new(ResolveErrorCode::EvaluationFailed)
}

#[cfg(test)]
mod tests;
