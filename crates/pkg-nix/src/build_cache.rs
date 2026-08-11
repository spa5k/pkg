//! Broker-private binary-cache classification for local-build planning.

use std::collections::BTreeMap;
use std::fmt;

use pkg_core::state::{Digest, canonical_digest};
use serde::Serialize;

use crate::{CacheClassification, DerivationPath, StorePath};

const MAX_CACHE_PATHS: usize = 16_384;

/// One derivation and all of its evaluate-only expected output paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCacheSubject {
    derivation: DerivationPath,
    outputs: Vec<StorePath>,
}

impl BuildCacheSubject {
    /// Constructs one bounded, canonical cache-classification subject.
    pub fn new(
        derivation: DerivationPath,
        mut outputs: Vec<StorePath>,
    ) -> Result<Self, BuildCacheError> {
        outputs.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if outputs.is_empty()
            || outputs.len() > MAX_CACHE_PATHS
            || outputs.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(BuildCacheError::new(BuildCacheErrorCode::InvalidSubject));
        }
        Ok(Self {
            derivation,
            outputs,
        })
    }
}

/// Exact broker-private observation for one expected store path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachePathObservation {
    path: StorePath,
    status: CachePathStatus,
}

/// Trusted cache observations for one selected root and its complete closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDownloadClosure {
    root: StorePath,
    paths: Vec<CachePathObservation>,
}

impl CacheDownloadClosure {
    /// Constructs one bounded closure with exact, unique path observations.
    pub fn new(
        root: StorePath,
        mut paths: Vec<CachePathObservation>,
    ) -> Result<Self, BuildCacheError> {
        paths.sort_by(|left, right| left.path().as_str().cmp(right.path().as_str()));
        if paths.is_empty()
            || paths.len() > MAX_CACHE_PATHS
            || paths
                .windows(2)
                .any(|pair| pair[0].path() == pair[1].path())
            || !paths.iter().any(|path| path.path() == &root)
        {
            return Err(BuildCacheError::new(BuildCacheErrorCode::InvalidEvidence));
        }
        Ok(Self { root, paths })
    }

    /// Returns the selected root whose closure was inspected.
    #[must_use]
    pub const fn root(&self) -> &StorePath {
        &self.root
    }

    /// Returns the complete sorted closure observations.
    #[must_use]
    pub fn paths(&self) -> &[CachePathObservation] {
        &self.paths
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachePathStatus {
    Hit { download_bytes: u64, nar_bytes: u64 },
    Miss,
}

impl CachePathObservation {
    /// Records an already-local or authenticated-cache hit and its exact known bytes.
    #[must_use]
    pub const fn hit(path: StorePath, download_bytes: u64, nar_bytes: u64) -> Self {
        Self {
            path,
            status: CachePathStatus::Hit {
                download_bytes,
                nar_bytes,
            },
        }
    }

    /// Records absence from both the live local store and managed cache.
    #[must_use]
    pub const fn miss(path: StorePath) -> Self {
        Self {
            path,
            status: CachePathStatus::Miss,
        }
    }

    /// Returns the exact store path inspected by the trusted probe.
    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    /// Returns authenticated cache download bytes, or `None` for a miss.
    ///
    /// An already-local path is a hit with zero download bytes.
    #[must_use]
    pub const fn download_bytes(&self) -> Option<u64> {
        match self.status {
            CachePathStatus::Hit { download_bytes, .. } => Some(download_bytes),
            CachePathStatus::Miss => None,
        }
    }
}

/// Private cache-inspection seam implemented by the managed Real-Nix adapter.
pub trait BuildCacheProbe: Send + Sync {
    /// Returns exactly one observation for every requested canonical path.
    fn inspect(&self, paths: &[StorePath]) -> Result<Vec<CachePathObservation>, BuildCacheError>;

    /// Returns one complete cache-download closure for every selected root.
    ///
    /// Test adapters may use the conservative singleton default. The managed
    /// Real-Nix adapter overrides this with recursive cache metadata.
    fn inspect_download_closures(
        &self,
        roots: &[StorePath],
    ) -> Result<Vec<CacheDownloadClosure>, BuildCacheError> {
        self.inspect(roots)?
            .into_iter()
            .map(|observation| {
                CacheDownloadClosure::new(observation.path().clone(), vec![observation])
            })
            .collect()
    }
}

/// Deterministic evidence consumed by private BuildPlan construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCacheEvidence {
    subjects_digest: Digest,
    classification: CacheClassification,
    missing_derivations: Vec<DerivationPath>,
}

impl BuildCacheEvidence {
    /// Returns whether this evidence was classified for exactly these subjects.
    #[must_use]
    pub fn matches_subjects(&self, subjects: &[BuildCacheSubject]) -> bool {
        normalized_subjects(subjects)
            .and_then(|owners| subjects_digest(&owners))
            .is_ok_and(|digest| digest == self.subjects_digest)
    }

    /// Splits the cache identity from the derivations requiring a local build.
    #[must_use]
    pub fn into_parts(self) -> (CacheClassification, Vec<DerivationPath>) {
        (self.classification, self.missing_derivations)
    }
}

/// Stable private cache-classification refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildCacheErrorCode {
    /// Evaluated derivation/output subjects were empty, duplicate, or oversized.
    InvalidSubject,
    /// The managed local/cache query failed or returned incomplete evidence.
    ProbeFailed,
    /// Every expected path is available; no local-build plan should be created.
    NoBuildRequired,
    /// Counts, byte totals, or canonical identity overflowed or were inconsistent.
    InvalidEvidence,
}

/// Redacted cache-classification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCacheError {
    code: BuildCacheErrorCode,
}

impl BuildCacheError {
    pub(crate) const fn new(code: BuildCacheErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable private failure category.
    #[must_use]
    pub const fn code(self) -> BuildCacheErrorCode {
        self.code
    }
}

impl fmt::Display for BuildCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private build cache classification refused")
    }
}

impl std::error::Error for BuildCacheError {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassificationIdentity<'a> {
    path: &'a str,
    present: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubjectIdentity<'a> {
    path: &'a str,
    derivations: Vec<&'a str>,
}

/// Classifies the union of evaluated output paths without realization.
pub fn classify_build_cache(
    subjects: &[BuildCacheSubject],
    probe: &dyn BuildCacheProbe,
) -> Result<BuildCacheEvidence, BuildCacheError> {
    let owners = normalized_subjects(subjects)?;
    let subjects_digest = subjects_digest(&owners)?;
    let paths = owners
        .values()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let observations = probe.inspect(&paths)?;
    if observations.len() != paths.len() {
        return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
    }

    let mut by_path = BTreeMap::new();
    for observation in observations {
        let key = observation.path.as_str().to_owned();
        if by_path
            .insert(key.clone(), (observation.path, observation.status))
            .is_some()
            || !owners.contains_key(&key)
        {
            return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
        }
    }
    if by_path.len() != owners.len() {
        return Err(BuildCacheError::new(BuildCacheErrorCode::ProbeFailed));
    }

    let mut hits = 0_u64;
    let mut misses = 0_u64;
    let mut download_bytes = 0_u64;
    let mut nar_bytes = 0_u64;
    let mut missing_derivations = BTreeMap::new();
    let mut identity = Vec::with_capacity(by_path.len());
    for (key, (path, status)) in &by_path {
        let present = match status {
            CachePathStatus::Hit {
                download_bytes: download,
                nar_bytes: nar,
            } => {
                hits = hits.checked_add(1).ok_or_else(invalid_evidence)?;
                download_bytes = download_bytes
                    .checked_add(*download)
                    .ok_or_else(invalid_evidence)?;
                nar_bytes = nar_bytes.checked_add(*nar).ok_or_else(invalid_evidence)?;
                true
            }
            CachePathStatus::Miss => {
                misses = misses.checked_add(1).ok_or_else(invalid_evidence)?;
                for derivation in &owners[key].1 {
                    missing_derivations
                        .entry(derivation.as_str().to_owned())
                        .or_insert_with(|| derivation.clone());
                }
                false
            }
        };
        identity.push(ClassificationIdentity {
            path: path.as_str(),
            present,
        });
    }
    if misses == 0 {
        return Err(BuildCacheError::new(BuildCacheErrorCode::NoBuildRequired));
    }
    let classification_digest = canonical_digest(&identity).map_err(|_| invalid_evidence())?;
    let classification = CacheClassification::new(
        classification_digest,
        hits,
        misses,
        download_bytes,
        nar_bytes,
    )
    .map_err(|_| invalid_evidence())?;
    Ok(BuildCacheEvidence {
        subjects_digest,
        classification,
        missing_derivations: missing_derivations.into_values().collect(),
    })
}

fn normalized_subjects(
    subjects: &[BuildCacheSubject],
) -> Result<BTreeMap<String, (StorePath, Vec<DerivationPath>)>, BuildCacheError> {
    if subjects.is_empty() || subjects.len() > MAX_CACHE_PATHS {
        return Err(BuildCacheError::new(BuildCacheErrorCode::InvalidSubject));
    }
    let mut owners = BTreeMap::<String, (StorePath, Vec<DerivationPath>)>::new();
    for subject in subjects {
        for path in &subject.outputs {
            let (_, derivations) = owners
                .entry(path.as_str().to_owned())
                .or_insert_with(|| (path.clone(), Vec::new()));
            if !derivations.contains(&subject.derivation) {
                derivations.push(subject.derivation.clone());
                derivations.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            }
        }
    }
    if owners.is_empty() || owners.len() > MAX_CACHE_PATHS {
        return Err(BuildCacheError::new(BuildCacheErrorCode::InvalidSubject));
    }
    Ok(owners)
}

fn subjects_digest(
    owners: &BTreeMap<String, (StorePath, Vec<DerivationPath>)>,
) -> Result<Digest, BuildCacheError> {
    let identity = owners
        .values()
        .map(|(path, derivations)| SubjectIdentity {
            path: path.as_str(),
            derivations: derivations.iter().map(DerivationPath::as_str).collect(),
        })
        .collect::<Vec<_>>();
    canonical_digest(&identity).map_err(|_| invalid_evidence())
}

fn invalid_evidence() -> BuildCacheError {
    BuildCacheError::new(BuildCacheErrorCode::InvalidEvidence)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn drv(name: &str) -> DerivationPath {
        DerivationPath::from_str(&format!("/nix/store/{HASH}-{name}.drv")).unwrap()
    }

    fn path(name: &str) -> StorePath {
        StorePath::new(&format!("/nix/store/{HASH}-{name}")).unwrap()
    }

    struct Probe(Vec<CachePathObservation>);

    impl BuildCacheProbe for Probe {
        fn inspect(&self, _: &[StorePath]) -> Result<Vec<CachePathObservation>, BuildCacheError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn classification_is_exact_sorted_and_missing_derivations_are_private() {
        let subjects = vec![
            BuildCacheSubject::new(drv("root"), vec![path("root")]).unwrap(),
            BuildCacheSubject::new(drv("dep"), vec![path("dep")]).unwrap(),
        ];
        let first = classify_build_cache(
            &subjects,
            &Probe(vec![
                CachePathObservation::miss(path("root")),
                CachePathObservation::hit(path("dep"), 10, 20),
            ]),
        )
        .unwrap();
        let second = classify_build_cache(
            &subjects,
            &Probe(vec![
                CachePathObservation::hit(path("dep"), 10, 20),
                CachePathObservation::miss(path("root")),
            ]),
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(first.matches_subjects(&subjects));
        assert!(!first.matches_subjects(&[
            BuildCacheSubject::new(drv("other"), vec![path("root")],).unwrap()
        ]));
        let (_, missing) = first.into_parts();
        assert_eq!(missing, vec![drv("root")]);
    }

    #[test]
    fn incomplete_and_all_hit_evidence_fail_closed() {
        let subjects = vec![BuildCacheSubject::new(drv("root"), vec![path("root")]).unwrap()];
        assert_eq!(
            classify_build_cache(&subjects, &Probe(Vec::new()))
                .unwrap_err()
                .code(),
            BuildCacheErrorCode::ProbeFailed
        );
        assert_eq!(
            classify_build_cache(
                &subjects,
                &Probe(vec![CachePathObservation::hit(path("root"), 1, 2)])
            )
            .unwrap_err()
            .code(),
            BuildCacheErrorCode::NoBuildRequired
        );
    }
}
