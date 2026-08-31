use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use pkg_channel::VerifiedChannel;
use pkg_core::{ChannelSequence, PolicyVersion};
use pkg_nix::{
    BuildCacheProbe, Digest, InstallDownloadProgress, InstallEvidence, NixAdapter,
    NixAdapterErrorCode, SubstituteErrorCode, SubstituteResult, VerifiedSubstitute,
    acquire_substitute,
};

use crate::{PlannedOutput, PreflightInstall, ResolvedInstall, VerifiedInstall};

const MAX_DOWNLOAD_CLOSURE_PATHS: usize = 16_384;

/// One selected output proven present and trusted after substitution.
#[derive(Debug)]
pub struct AcquiredOutput {
    planned: PlannedOutput,
    substitute: VerifiedSubstitute,
}

impl AcquiredOutput {
    /// Returns the desired-state/output binding.
    #[must_use]
    pub const fn planned(&self) -> &PlannedOutput {
        &self.planned
    }
    /// Returns the cryptographically verified cache result.
    #[must_use]
    pub const fn substitute(&self) -> &VerifiedSubstitute {
        &self.substitute
    }
}

/// Every exact output acquired for this operation.
#[derive(Debug)]
pub struct AcquiredInstall {
    outputs: Vec<AcquiredOutput>,
    authority: CacheAuthorityIdentity,
}
impl AcquiredInstall {
    /// Returns acquired outputs in preflight order.
    #[must_use]
    pub fn outputs(&self) -> &[AcquiredOutput] {
        &self.outputs
    }

    pub(crate) fn into_parts(self) -> (Vec<AcquiredOutput>, CacheAuthorityIdentity) {
        (self.outputs, self.authority)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheAuthorityIdentity {
    channel_sequence: ChannelSequence,
    policy_version: PolicyVersion,
    descriptor_sha256: [u8; 32],
}

impl CacheAuthorityIdentity {
    const fn from_channel(channel: &VerifiedChannel) -> Self {
        Self {
            channel_sequence: channel.sequence(),
            policy_version: channel.policy_version(),
            descriptor_sha256: channel.descriptor_sha256(),
        }
    }

    pub(crate) fn matches_resolved(&self, resolved: &ResolvedInstall) -> bool {
        self.channel_sequence == resolved.channel_sequence()
            && self.policy_version == resolved.policy_version()
            && self.descriptor_sha256 == resolved.descriptor_sha256()
    }
}

/// Cache-only acquisition failure; a normal miss is explicitly build-required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    /// At least one output needs the PR-26 approved local-build path.
    BuildRequired,
    /// Cache-only acquisition failed closed.
    Refused,
    /// Cache download probing failed closed.
    ProbeRefused,
    /// Cache substitution failed with stable, redacted nested categories.
    SubstituteRefused(SubstituteErrorCode, Option<NixAdapterErrorCode>),
    /// Trusted progress accounting failed closed.
    ProgressRefused,
}
impl AcquireError {
    pub(crate) const fn stage(self) -> &'static str {
        match self {
            Self::BuildRequired => "build-required",
            Self::Refused => "preflight",
            Self::ProbeRefused => "probe",
            Self::SubstituteRefused(..) => "substitute",
            Self::ProgressRefused => "progress",
        }
    }
}
impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "install acquisition refused: {self:?}")
    }
}
impl std::error::Error for AcquireError {}

/// Cache evidence could not be bound to the authenticated resolve result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheEvidenceError;

impl fmt::Display for CacheEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cache install evidence refused")
    }
}

impl std::error::Error for CacheEvidenceError {}

/// Substitutes every selected output; any miss stops before staging.
///
/// The caller must hold the broker-issued GC-inhibit permit for the complete
/// operation so successfully substituted outputs cannot be collected before
/// roots are durably published during activation.
pub fn acquire_cache_only(
    resolved: &ResolvedInstall,
    preflight: &PreflightInstall,
    channel: &VerifiedChannel,
    adapter: &dyn NixAdapter,
) -> Result<AcquiredInstall, AcquireError> {
    let authority = CacheAuthorityIdentity::from_channel(channel);
    let expected = crate::preflight_cache_only(resolved).map_err(|_| AcquireError::Refused)?;
    if !authority.matches_resolved(resolved) || expected.outputs() != preflight.outputs() {
        return Err(AcquireError::Refused);
    }
    let mut outputs = Vec::with_capacity(preflight.outputs().len());
    for planned in preflight.outputs() {
        match acquire_substitute(planned.store_path(), channel.descriptor().cache(), adapter)
            .map_err(|error| AcquireError::SubstituteRefused(error.code(), error.adapter_code()))?
        {
            SubstituteResult::Fetched(substitute) => outputs.push(AcquiredOutput {
                planned: planned.clone(),
                substitute,
            }),
            SubstituteResult::Miss(_) => return Err(AcquireError::BuildRequired),
        }
    }
    Ok(AcquiredInstall { outputs, authority })
}

/// Probes every selected cache object before mutation and emits trusted bytes
/// around each real substitution.
///
/// A miss is detected before any copy begins. Already-local objects carry zero
/// download bytes and do not emit a counter. Shared output paths are copied and
/// counted once, then rebound to each resolver-owned selected output.
pub fn acquire_cache_only_with_progress(
    resolved: &ResolvedInstall,
    preflight: &PreflightInstall,
    channel: &VerifiedChannel,
    adapter: &dyn NixAdapter,
    probe: &dyn BuildCacheProbe,
    progress: &mut dyn FnMut(InstallDownloadProgress) -> Result<(), ()>,
) -> Result<AcquiredInstall, AcquireError> {
    let authority = CacheAuthorityIdentity::from_channel(channel);
    let expected = crate::preflight_cache_only(resolved).map_err(|_| AcquireError::Refused)?;
    if !authority.matches_resolved(resolved) || expected.outputs() != preflight.outputs() {
        return Err(AcquireError::Refused);
    }

    let selectors = selector_map(resolved)?;
    let planned_paths = deduped_planned_paths(preflight);
    let download_bytes = trusted_download_bytes(&planned_paths, probe)?;
    let (root_owners, selector_totals) = owner_totals(preflight, &selectors, &download_bytes)?;

    let mut fetched = BTreeMap::<String, VerifiedSubstitute>::new();
    let mut selector_completed = BTreeMap::<String, u64>::new();
    let mut outputs = Vec::with_capacity(preflight.outputs().len());
    for planned in preflight.outputs() {
        let path_key = planned.store_path().as_str();
        let substitute = if let Some(existing) = fetched.get(path_key) {
            existing.clone()
        } else {
            let total = download_bytes
                .get(path_key)
                .copied()
                .ok_or(AcquireError::Refused)?;
            let owner = root_owners.get(path_key).ok_or(AcquireError::Refused)?;
            let (selector, selector_total) =
                selector_totals.get(owner).ok_or(AcquireError::Refused)?;
            if *selector_total > 0 && !selector_completed.contains_key(owner) {
                progress(
                    InstallDownloadProgress::new(selector.clone(), 0, *selector_total)
                        .map_err(|_| AcquireError::ProgressRefused)?,
                )
                .map_err(|()| AcquireError::ProgressRefused)?;
                selector_completed.insert(owner.clone(), 0);
            }
            let substitute = match acquire_substitute(
                planned.store_path(),
                channel.descriptor().cache(),
                adapter,
            )
            .map_err(|error| AcquireError::SubstituteRefused(error.code(), error.adapter_code()))?
            {
                SubstituteResult::Fetched(substitute) => substitute,
                SubstituteResult::Miss(_) => return Err(AcquireError::BuildRequired),
            };
            if total > 0 {
                let completed = selector_completed
                    .get_mut(owner)
                    .ok_or(AcquireError::ProgressRefused)?;
                *completed = completed
                    .checked_add(total)
                    .ok_or(AcquireError::ProgressRefused)?;
                if *completed == *selector_total {
                    progress(
                        InstallDownloadProgress::new(
                            selector.clone(),
                            *selector_total,
                            *selector_total,
                        )
                        .map_err(|_| AcquireError::ProgressRefused)?,
                    )
                    .map_err(|()| AcquireError::ProgressRefused)?;
                } else if *completed > *selector_total {
                    return Err(AcquireError::ProgressRefused);
                }
            }
            fetched.insert(path_key.to_owned(), substitute.clone());
            substitute
        };
        outputs.push(AcquiredOutput {
            planned: planned.clone(),
            substitute,
        });
    }
    if selector_totals
        .iter()
        .any(|(selector, (_, total))| *total > 0 && selector_completed.get(selector) != Some(total))
    {
        return Err(AcquireError::ProgressRefused);
    }
    Ok(AcquiredInstall { outputs, authority })
}

/// Builds the one-to-one selector map, refusing duplicate selector ids.
fn selector_map(
    resolved: &ResolvedInstall,
) -> Result<BTreeMap<String, pkg_core::SelectorInput>, AcquireError> {
    let mut selectors = BTreeMap::new();
    for target in resolved.targets() {
        if selectors
            .insert(
                target.selector().id().as_str().to_owned(),
                target.selector().selector().clone(),
            )
            .is_some()
        {
            return Err(AcquireError::Refused);
        }
    }
    Ok(selectors)
}

/// Deduplicates the planned output store paths in first-seen order.
fn deduped_planned_paths(preflight: &PreflightInstall) -> Vec<pkg_core::StorePath> {
    let mut seen_paths = BTreeSet::new();
    let mut planned_paths = Vec::new();
    for planned in preflight.outputs() {
        if seen_paths.insert(planned.store_path().as_str().to_owned()) {
            planned_paths.push(planned.store_path().clone());
        }
    }
    planned_paths
}

/// Maps each planned root path to its selector and sums trusted bytes per
/// selector, refusing unknown selectors or missing byte totals.
fn owner_totals(
    preflight: &PreflightInstall,
    selectors: &BTreeMap<String, pkg_core::SelectorInput>,
    download_bytes: &BTreeMap<String, u64>,
) -> Result<
    (
        BTreeMap<String, String>,
        BTreeMap<String, (pkg_core::SelectorInput, u64)>,
    ),
    AcquireError,
> {
    let mut root_owners = BTreeMap::new();
    let mut selector_totals = BTreeMap::<String, (pkg_core::SelectorInput, u64)>::new();
    for planned in preflight.outputs() {
        let path = planned.store_path().as_str().to_owned();
        if root_owners.contains_key(&path) {
            continue;
        }
        let selector = selectors
            .get(planned.selector_id().as_str())
            .ok_or(AcquireError::Refused)?
            .clone();
        let bytes = download_bytes
            .get(&path)
            .copied()
            .ok_or(AcquireError::Refused)?;
        root_owners.insert(path, selector.as_str().to_owned());
        let entry = selector_totals
            .entry(selector.as_str().to_owned())
            .or_insert((selector, 0));
        entry.1 = entry.1.checked_add(bytes).ok_or(AcquireError::Refused)?;
    }
    Ok((root_owners, selector_totals))
}

fn trusted_download_bytes(
    planned_paths: &[pkg_core::StorePath],
    probe: &dyn BuildCacheProbe,
) -> Result<BTreeMap<String, u64>, AcquireError> {
    if planned_paths.is_empty() || planned_paths.len() > MAX_DOWNLOAD_CLOSURE_PATHS {
        return Err(AcquireError::ProbeRefused);
    }
    let closures = probe
        .inspect_download_closures(planned_paths)
        .map_err(|_| AcquireError::ProbeRefused)?;
    if closures.len() != planned_paths.len() {
        return Err(AcquireError::ProbeRefused);
    }
    let expected = planned_paths
        .iter()
        .map(|path| path.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if expected.len() != planned_paths.len() {
        return Err(AcquireError::ProbeRefused);
    }
    let mut by_root = BTreeMap::new();
    for closure in closures {
        let key = closure.root().as_str().to_owned();
        if !expected.contains(&key) || by_root.insert(key, closure).is_some() {
            return Err(AcquireError::ProbeRefused);
        }
    }
    if by_root.len() != planned_paths.len() {
        return Err(AcquireError::ProbeRefused);
    }
    let mut claimed = BTreeMap::<String, ()>::new();
    let mut download_bytes = BTreeMap::new();
    for root in planned_paths {
        let root_key = root.as_str();
        let closure = by_root.get(root_key).ok_or(AcquireError::ProbeRefused)?;
        let mut total = 0_u64;
        for observation in closure.paths() {
            let Some(bytes) = observation.download_bytes() else {
                return Err(AcquireError::BuildRequired);
            };
            let path = observation.path().as_str().to_owned();
            if !claimed.contains_key(&path) {
                if claimed.len() == MAX_DOWNLOAD_CLOSURE_PATHS {
                    return Err(AcquireError::ProbeRefused);
                }
                claimed.insert(path, ());
                total = total.checked_add(bytes).ok_or(AcquireError::ProbeRefused)?;
            }
        }
        download_bytes.insert(root_key.to_owned(), total);
    }
    Ok(download_bytes)
}

/// Binds verified cache outputs to the exact authenticated resolve result.
///
/// This is the cache-hit counterpart to post-build evidence creation. It does
/// not accept raw store paths or caller-supplied provenance.
///
/// # Errors
///
/// Refuses any target, source identity, output, or fresh metadata mismatch.
pub fn assemble_cache_install_evidence(
    resolved: &ResolvedInstall,
    verified: &VerifiedInstall,
    adapter: &dyn NixAdapter,
) -> Result<InstallEvidence, CacheEvidenceError> {
    if !verified.authority().matches_resolved(resolved) {
        return Err(CacheEvidenceError);
    }
    let targets = resolved
        .build_plan_targets()
        .map_err(|_| CacheEvidenceError)?;
    let substitutes = verified
        .outputs()
        .iter()
        .map(|output| output.substitute().clone())
        .collect();
    InstallEvidence::from_cache_substitutes(
        Digest::from_bytes(resolved.descriptor_sha256()),
        resolved.channel_sequence(),
        resolved.policy_version(),
        resolved.revision().clone(),
        resolved.nar_hash().clone(),
        resolved.system(),
        &targets,
        substitutes,
        adapter,
    )
    .map_err(|_| CacheEvidenceError)
}

#[cfg(test)]
mod tests {
    use pkg_nix::{BuildCacheError, CacheDownloadClosure, CachePathObservation};

    use super::*;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn path(name: &str) -> pkg_core::StorePath {
        pkg_core::StorePath::new(&format!("/nix/store/{HASH}-{name}")).unwrap()
    }

    struct Probe(Vec<CachePathObservation>);

    impl BuildCacheProbe for Probe {
        fn inspect(
            &self,
            _: &[pkg_core::StorePath],
        ) -> Result<Vec<CachePathObservation>, BuildCacheError> {
            Ok(self.0.clone())
        }
    }

    struct ClosureProbe(Vec<CacheDownloadClosure>);

    impl BuildCacheProbe for ClosureProbe {
        fn inspect(
            &self,
            _: &[pkg_core::StorePath],
        ) -> Result<Vec<CachePathObservation>, BuildCacheError> {
            Ok(Vec::new())
        }

        fn inspect_download_closures(
            &self,
            _: &[pkg_core::StorePath],
        ) -> Result<Vec<CacheDownloadClosure>, BuildCacheError> {
            Ok(self.0.clone())
        }
    }

    fn planned() -> Vec<pkg_core::StorePath> {
        vec![path("a"), path("b")]
    }

    #[test]
    fn trusted_download_probe_requires_exact_all_hit_coverage() {
        let planned = planned();
        let bytes = trusted_download_bytes(
            &planned,
            &Probe(vec![
                CachePathObservation::hit(path("b"), 0, 20),
                CachePathObservation::hit(path("a"), 11, 30),
            ]),
        )
        .unwrap();
        assert_eq!(bytes[path("a").as_str()], 11);
        assert_eq!(bytes[path("b").as_str()], 0);

        assert_eq!(
            trusted_download_bytes(
                &planned,
                &Probe(vec![
                    CachePathObservation::hit(path("a"), 11, 30),
                    CachePathObservation::miss(path("b")),
                ]),
            ),
            Err(AcquireError::BuildRequired)
        );
    }

    #[test]
    fn trusted_download_probe_rejects_duplicate_incomplete_and_foreign_evidence() {
        let planned = planned();
        for observations in [
            vec![CachePathObservation::hit(path("a"), 1, 2)],
            vec![
                CachePathObservation::hit(path("a"), 1, 2),
                CachePathObservation::hit(path("a"), 1, 2),
            ],
            vec![
                CachePathObservation::hit(path("a"), 1, 2),
                CachePathObservation::hit(path("foreign"), 1, 2),
            ],
        ] {
            assert_eq!(
                trusted_download_bytes(&planned, &Probe(observations)),
                Err(AcquireError::ProbeRefused)
            );
        }
    }

    #[test]
    fn shared_closure_bytes_follow_actual_substitution_order() {
        let planned = vec![path("b"), path("a")];
        let dep = path("dep");
        let bytes = trusted_download_bytes(
            &planned,
            &ClosureProbe(vec![
                CacheDownloadClosure::new(
                    path("a"),
                    vec![
                        CachePathObservation::hit(path("a"), 11, 30),
                        CachePathObservation::hit(dep.clone(), 5, 10),
                    ],
                )
                .unwrap(),
                CacheDownloadClosure::new(
                    path("b"),
                    vec![
                        CachePathObservation::hit(path("b"), 7, 20),
                        CachePathObservation::hit(dep, 5, 10),
                    ],
                )
                .unwrap(),
            ]),
        )
        .unwrap();
        assert_eq!(bytes[path("b").as_str()], 12);
        assert_eq!(bytes[path("a").as_str()], 11);
    }
}
