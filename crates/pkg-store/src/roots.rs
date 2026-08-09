use std::fmt;

use pkg_core::identity::{OutputName, StorePath};
use pkg_core::selector::SelectorId;
use pkg_nix::{
    GenerationId, MaintenanceAdapter, MaintenanceError, RootName, RootSet, RootSetEntry,
    RootSetReport,
};
use sha2::{Digest as _, Sha256};

/// One selected output that must remain live for a generation.
#[derive(Debug, Clone)]
pub struct RootCandidate {
    target: StorePath,
}

impl RootCandidate {
    /// Creates a root candidate from validated identity components.
    #[must_use]
    pub fn new(_selector: SelectorId, _output: OutputName, target: StorePath) -> Self {
        Self { target }
    }

    /// Reconstructs a candidate from the canonical identity persisted in
    /// `activation.outputRoots` during crash recovery.
    #[must_use]
    pub const fn from_output_root(target: StorePath) -> Self {
        Self { target }
    }
}

/// A complete validated set ready for privileged publication.
#[derive(Debug)]
pub struct PreparedRootSet {
    root_set: RootSet,
}

impl PreparedRootSet {
    /// Returns the exact sorted output paths protected by this set.
    #[must_use]
    pub fn output_roots(&self) -> Vec<&StorePath> {
        self.root_set
            .entries()
            .iter()
            .map(RootSetEntry::target)
            .collect()
    }

    /// Returns the underlying closed helper request.
    #[must_use]
    pub const fn request(&self) -> &RootSet {
        &self.root_set
    }
}

/// Root-set preparation or publication failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootError {
    /// No outputs were supplied or the closed root-set grammar was violated.
    InvalidSet,
    /// The authenticated maintenance helper refused publication.
    PublicationRefused,
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSet => f.write_str("invalid generation root set"),
            Self::PublicationRefused => f.write_str("generation root publication refused"),
        }
    }
}

impl std::error::Error for RootError {}

/// Builds a deterministic, path-injection-safe root set.
pub fn prepare_root_set(
    owner_uid: u32,
    generation: GenerationId,
    candidates: impl IntoIterator<Item = RootCandidate>,
) -> Result<PreparedRootSet, RootError> {
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.target.as_str().cmp(right.target.as_str()));
    candidates.dedup_by(|left, right| left.target == right.target);

    let entries = candidates
        .into_iter()
        .map(|candidate| {
            let mut hasher = Sha256::new();
            hasher.update(candidate.target.as_str().as_bytes());
            let digest = hasher.finalize();
            let name = format!("out-{}", encode_hex(&digest[..16]));
            let name = RootName::new(&name).map_err(|_| RootError::InvalidSet)?;
            Ok(RootSetEntry::new(name, candidate.target))
        })
        .collect::<Result<Vec<_>, RootError>>()?;

    RootSet::new(owner_uid, generation, entries)
        .map(|root_set| PreparedRootSet { root_set })
        .map_err(|_| RootError::InvalidSet)
}

/// Publishes the full root set through the authenticated helper boundary.
pub fn publish_root_set(
    prepared: &PreparedRootSet,
    helper: &dyn MaintenanceAdapter,
) -> Result<RootSetReport, RootError> {
    let report = helper
        .publish_root_set(prepared.request())
        .map_err(map_maintenance)?;
    let expected = format!(
        "/nix/var/nix/gcroots/pkg/users/{}/{}",
        prepared.request().owner_uid(),
        prepared.request().generation().as_str()
    );
    if report.reference().as_str() != expected
        || report.entry_count() != prepared.request().entries().len()
    {
        return Err(RootError::PublicationRefused);
    }
    Ok(report)
}

fn map_maintenance(_: MaintenanceError) -> RootError {
    RootError::PublicationRefused
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_nix::{InProcessHelper, InProcessPeer};

    fn candidate(selector: &str, output: &str, name: &str) -> RootCandidate {
        RootCandidate::new(
            SelectorId::new(selector).unwrap(),
            OutputName::new(output).unwrap(),
            StorePath::new(&format!(
                "/nix/store/00000000000000000000000000000000-{name}"
            ))
            .unwrap(),
        )
    }

    #[test]
    fn names_are_deterministic_safe_and_publication_is_idempotent() {
        let generation = GenerationId::new("gen-0001").unwrap();
        let prepared = prepare_root_set(
            1001,
            generation,
            [
                candidate("sel_b", "dev", "b"),
                candidate("sel_a", "out", "a"),
            ],
        )
        .unwrap();
        assert_eq!(prepared.request().entries().len(), 2);
        assert!(
            prepared.request().entries().iter().all(|entry| entry
                .name()
                .as_str()
                .starts_with("out-")
                && entry.name().as_str().len() == 36)
        );
        let helper = InProcessHelper::new(991).unwrap();
        let session = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        let maintenance = session.for_caller(1001);
        let first = publish_root_set(&prepared, &maintenance).unwrap();
        let second = publish_root_set(&prepared, &maintenance).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.entry_count(), 2);
    }

    #[test]
    fn empty_set_is_refused_before_helper_call() {
        let error = prepare_root_set(1001, GenerationId::new("gen-0001").unwrap(), []).unwrap_err();
        assert_eq!(error, RootError::InvalidSet);
    }
}
