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

#[expect(
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
mod tests {
    use super::*;
    use pkg_core::state::Digest;
    use pkg_core::{
        OutputName, OutputSelection, PackageVersion, RealizationIdentity, SelectorId,
        SelectorInput, VersionPreference,
    };
    use pkg_index::{BuildMetadata, build_index_from_json};
    use pkg_nix::{EvaluatedDerivation, StorePath};
    use pkg_testkit::FakeNix;
    use std::collections::BTreeMap;
    use std::str::FromStr;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn sequence() -> ChannelSequence {
        ChannelSequence::from_u64(7).unwrap()
    }

    fn revision() -> NixpkgsRevision {
        NixpkgsRevision::new(REVISION).unwrap()
    }

    fn nar_hash() -> NarHash {
        NarHash::new(NAR_HASH).unwrap()
    }

    fn selector(input: &str, preference: VersionPreference) -> PackageSelector {
        PackageSelector::new(
            SelectorId::new("sel_test").unwrap(),
            SelectorInput::new(input).unwrap(),
            preference,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        )
    }

    fn request(attribute: &str) -> EvaluateDerivationRequest {
        EvaluateDerivationRequest::new(
            AttributePath::new(attribute).unwrap(),
            System::X8664Linux,
            revision(),
            nar_hash(),
            OutputSelection::default_selection(),
        )
        .unwrap()
    }

    fn plan(version: &str) -> DerivationPlanReport {
        let derivation = pkg_core::DerivationPath::from_str(&format!(
            "/nix/store/{STORE_HASH}-ripgrep-{version}.drv"
        ))
        .unwrap();
        let mut outputs = BTreeMap::new();
        outputs.insert(
            OutputName::new("out").unwrap(),
            StorePath::new(&format!("/nix/store/{STORE_HASH}-ripgrep-{version}")).unwrap(),
        );
        let evaluated = EvaluatedDerivation::new(
            derivation.clone(),
            format!("ripgrep-{version}"),
            System::X8664Linux,
            outputs,
            Digest::from_bytes([1; 32]),
            false,
        )
        .unwrap();
        DerivationPlanReport::new(
            4,
            derivation,
            vec![OutputName::new("out").unwrap()],
            vec![evaluated],
            Digest::from_bytes([2; 32]),
            "ripgrep".into(),
            PackageVersion::new(version),
        )
        .unwrap()
    }

    fn resolve_for_test(
        selector: &PackageSelector,
        index: Option<&IndexDocument>,
        fake: &FakeNix,
    ) -> Result<ResolvedPackagePlan, ResolveError> {
        resolve_with_source(
            selector,
            sequence(),
            &revision(),
            &nar_hash(),
            System::X8664Linux,
            index,
            fake,
        )
    }

    #[test]
    fn missing_index_direct_attribute_evaluates_once_without_realizing() {
        let fake = FakeNix::new();
        fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
        let resolved =
            resolve_for_test(&selector("ripgrep", VersionPreference::Any), None, &fake).unwrap();
        assert_eq!(resolved.selector().attribute().unwrap().as_str(), "ripgrep");
        assert_eq!(resolved.plan().version().as_str(), "14.1.0");
        assert!(resolved.build_plan_target().is_ok());
        assert_eq!(resolved.build_cache_subjects().unwrap().len(), 1);
        assert_eq!(fake.assert_exhausted(), Ok(()));
    }

    #[test]
    fn index_alias_selects_attribute_but_evaluation_owns_version() {
        let metadata = BuildMetadata::new(
            sequence(),
            System::X8664Linux,
            revision(),
            "2026-08-09T00:00:00Z",
        )
        .unwrap();
        let projection = br#"[{"attrPath":"ripgrep","pname":"ripgrep","version":"old-display-value","aliases":["rg"]}]"#;
        let built = build_index_from_json(metadata, projection).unwrap();
        let fake = FakeNix::new();
        fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
        let resolved = resolve_for_test(
            &selector(
                "rg",
                VersionPreference::Exact(PackageVersion::new("14.1.0")),
            ),
            Some(built.document()),
            &fake,
        )
        .unwrap();
        assert_eq!(resolved.selector().attribute().unwrap().as_str(), "ripgrep");
        assert_eq!(resolved.plan().version().as_str(), "14.1.0");
        assert_eq!(fake.assert_exhausted(), Ok(()));
    }

    #[test]
    fn ambiguity_and_version_mismatch_fail_closed() {
        let metadata = BuildMetadata::new(
            sequence(),
            System::X8664Linux,
            revision(),
            "2026-08-09T00:00:00Z",
        )
        .unwrap();
        let projection = br#"[
            {"attrPath":"one","pname":"one","aliases":["tool"]},
            {"attrPath":"two","pname":"two","aliases":["tool"]}
        ]"#;
        let built = build_index_from_json(metadata, projection).unwrap();
        let fake = FakeNix::new();
        let error = resolve_for_test(
            &selector("tool", VersionPreference::Any),
            Some(built.document()),
            &fake,
        )
        .unwrap_err();
        assert_eq!(error.code(), ResolveErrorCode::AmbiguousSelector);
        assert_eq!(error.candidate_count(), Some(2));

        let fake = FakeNix::new();
        fake.expect_evaluate_derivation(request("ripgrep"), Ok(plan("14.1.0")));
        let error = resolve_for_test(
            &selector(
                "ripgrep",
                VersionPreference::Exact(PackageVersion::new("13.0.0")),
            ),
            None,
            &fake,
        )
        .unwrap_err();
        assert_eq!(error.code(), ResolveErrorCode::VersionMismatch);
        assert_eq!(fake.assert_exhausted(), Ok(()));
    }

    #[test]
    fn source_mismatch_and_adapter_failure_are_redacted() {
        let selector = PackageSelector::new(
            SelectorId::new("sel_test").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::ExactRevision(
                NixpkgsRevision::new("1123456789abcdef0123456789abcdef01234567").unwrap(),
            ),
        );
        let fake = FakeNix::new();
        assert_eq!(
            resolve_for_test(&selector, None, &fake).unwrap_err().code(),
            ResolveErrorCode::SourceMismatch
        );

        let fake = FakeNix::new();
        let error = resolve_for_test(
            &super::tests::selector("ripgrep", VersionPreference::Any),
            None,
            &fake,
        )
        .unwrap_err();
        assert_eq!(error.code(), ResolveErrorCode::EvaluationFailed);
        assert!(!error.to_string().contains("/nix/store"));

        let pinned = super::tests::selector("ripgrep", VersionPreference::Any)
            .with_attribute(AttributePath::new("ripgrep").unwrap())
            .unwrap()
            .pinned_to(RealizationIdentity::new(
                StorePath::new(&format!("/nix/store/{STORE_HASH}-ripgrep-14.1.0")).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            resolve_for_test(&pinned, None, &FakeNix::new())
                .unwrap_err()
                .code(),
            ResolveErrorCode::AlreadyRealized
        );
    }
}
