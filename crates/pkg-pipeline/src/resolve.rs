use std::fmt;

use pkg_core::{ChannelSequence, NarHash, NixpkgsRevision, PackageSelector, PolicyVersion, System};
use pkg_index::IndexDocument;
use pkg_nix::{BuildPlanTarget, NixAdapter, VerifiedNixpkgsSource};
use pkg_resolver::{ResolveError, ResolvedPackagePlan, resolve_package};

/// All selectors resolved without realizing a store path.
#[derive(Debug)]
pub struct ResolvedInstall {
    targets: Vec<ResolvedPackagePlan>,
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
    revision: NixpkgsRevision,
    nar_hash: NarHash,
    system: System,
}

impl ResolvedInstall {
    /// Returns targets in caller-specified desired-state order.
    #[must_use]
    pub fn targets(&self) -> &[ResolvedPackagePlan] {
        &self.targets
    }

    /// Returns the authenticated channel sequence used for every target.
    #[must_use]
    pub const fn channel_sequence(&self) -> ChannelSequence {
        self.channel_sequence
    }

    /// Returns the authenticated policy version that selected the source.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the digest of the exact authenticated channel descriptor bytes.
    #[must_use]
    pub const fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }

    /// Returns the exact authenticated Nixpkgs revision used for evaluation.
    #[must_use]
    pub const fn revision(&self) -> &NixpkgsRevision {
        &self.revision
    }

    /// Returns the authenticated normalized source NAR identity.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }

    /// Returns the platform against which every derivation was evaluated.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Promotes all authoritative resolve results into private build-plan targets.
    ///
    /// No raw installable, flake reference, derivation path, or Nix option is
    /// accepted here. Every target comes from the resolver-owned selector and
    /// its validated evaluate-only derivation report.
    pub fn build_plan_targets(&self) -> Result<Vec<BuildPlanTarget>, ResolveError> {
        self.targets
            .iter()
            .map(ResolvedPackagePlan::build_plan_target)
            .collect()
    }
}

/// One target in a multi-selector resolve batch failed closed.
#[derive(Debug)]
pub struct ResolveBatchError {
    target_index: usize,
    source: Option<ResolveError>,
}

impl ResolveBatchError {
    /// Returns the zero-based desired-state position, never raw selector text.
    #[must_use]
    pub const fn target_index(&self) -> usize {
        self.target_index
    }
    /// Returns the redacted resolver error, or `None` for an empty batch.
    #[must_use]
    pub const fn source(&self) -> Option<ResolveError> {
        self.source
    }
}

impl fmt::Display for ResolveBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "install resolve target {} refused",
            self.target_index
        )
    }
}
impl std::error::Error for ResolveBatchError {}

/// Resolves every selector against one authenticated source without mutation.
pub fn resolve_install(
    selectors: &[PackageSelector],
    source: &VerifiedNixpkgsSource,
    system: System,
    index: Option<&IndexDocument>,
    adapter: &dyn NixAdapter,
) -> Result<ResolvedInstall, ResolveBatchError> {
    if selectors.is_empty() {
        return Err(ResolveBatchError {
            target_index: 0,
            source: None,
        });
    }
    let targets = selectors
        .iter()
        .enumerate()
        .map(|(target_index, selector)| {
            resolve_package(selector, source, system, index, adapter).map_err(|source| {
                ResolveBatchError {
                    target_index,
                    source: Some(source),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedInstall {
        targets,
        channel_sequence: source.channel_sequence(),
        policy_version: source.policy_version(),
        descriptor_sha256: source.descriptor_sha256(),
        revision: source.revision().clone(),
        nar_hash: source.nar_hash().clone(),
        system,
    })
}
