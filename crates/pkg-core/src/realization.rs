//! The exact realized artifact: a realized store object — input-addressed or
//! content-addressed — with its outputs, derivation, NAR hash, system, and
//! Nixpkgs revision.
//!
//! Per `plans/04` §4.1 and `plans/05` §5.2, a [`Realization`] is the exact
//! thing activated into a generation. Its canonical identity is the store path
//! alone (`plans/05` §6, `plans/00` INV-06): `pname`/`version` are display
//! metadata that **never** affect identity. Accordingly [`Realization`]
//! implements [`Eq`]/[`Hash`] over [`RealizationIdentity`] / the primary store
//! path only (see the type-level docs).
//!
//! This type holds only canonical, exact artifact data. License/broken/insecure
//! metadata and raw nested derivation-output JSON are intentionally **not**
//! modeled here.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::channel::NixpkgsRevision;
use crate::identity::{DerivationPath, NarHash, OutputName, RealizationIdentity, StorePath};
use crate::system::System;
use crate::version::PackageVersion;

/// Error returned when a [`Realization`] is constructed inconsistently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealizationError {
    /// The output map was empty.
    EmptyOutputs,
    /// The outputs-to-install list was empty.
    EmptyOutputsToInstall,
    /// The outputs-to-install list contained a duplicate.
    DuplicateOutputToInstall,
    /// An output selected for installation is absent from the output map.
    SelectedOutputNotPresent,
    /// The primary store path is not among the output values.
    PrimaryStorePathNotAnOutput,
}

impl fmt::Display for RealizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RealizationError::EmptyOutputs => f.write_str("realization has no outputs"),
            RealizationError::EmptyOutputsToInstall => {
                f.write_str("realization has no outputs selected to install")
            }
            RealizationError::DuplicateOutputToInstall => {
                f.write_str("realization outputs-to-install contains a duplicate")
            }
            RealizationError::SelectedOutputNotPresent => {
                f.write_str("a selected output is absent from the realization's outputs")
            }
            RealizationError::PrimaryStorePathNotAnOutput => {
                f.write_str("the primary store path is not among the realization's output values")
            }
        }
    }
}

impl std::error::Error for RealizationError {}

/// The exact realized artifact: a realized store object (input-addressed or
/// content-addressed).
///
/// Fields are private and represent canonical, exact data: the primary store
/// path, its deriver, the per-output store paths, the outputs selected for
/// installation, the target system, the pinned Nixpkgs revision, the NAR hash
/// of the primary store path, the closure NAR size, and the display
/// `pname`/`version`.
///
/// # Identity
///
/// **[`Eq`] and [`Hash`] are intentionally computed over the
/// [`RealizationIdentity`] (the primary store path) only.** Display metadata
/// (`pname`, `version`) and all other fields never affect identity
/// (`plans/05` §6, `plans/00` INV-06). Two realizations with the same store
/// path but different display/provenance metadata are **equal**; see the
/// property tests for a proof.
#[derive(Debug, Clone)]
pub struct Realization {
    store_path: StorePath,
    deriver: DerivationPath,
    outputs: BTreeMap<OutputName, StorePath>,
    outputs_to_install: Vec<OutputName>,
    system: System,
    nixpkgs_revision: NixpkgsRevision,
    nar_hash: NarHash,
    closure_nar_size: u64,
    pname: String,
    version: PackageVersion,
}

impl Realization {
    /// Constructs and validates a realization.
    ///
    /// # Errors
    ///
    /// Returns [`RealizationError`] if:
    /// - `outputs` is empty,
    /// - `outputs_to_install` is empty or contains duplicates,
    /// - a name in `outputs_to_install` is absent from `outputs`,
    /// - the primary `store_path` is not among the `outputs` values.
    #[allow(
        clippy::too_many_arguments,
        reason = "a Realization is one closed wire record; every field is validated independently"
    )]
    pub fn new(
        store_path: StorePath,
        deriver: DerivationPath,
        outputs: BTreeMap<OutputName, StorePath>,
        outputs_to_install: Vec<OutputName>,
        system: System,
        nixpkgs_revision: NixpkgsRevision,
        nar_hash: NarHash,
        closure_nar_size: u64,
        pname: String,
        version: PackageVersion,
    ) -> Result<Self, RealizationError> {
        if outputs.is_empty() {
            return Err(RealizationError::EmptyOutputs);
        }
        if outputs_to_install.is_empty() {
            return Err(RealizationError::EmptyOutputsToInstall);
        }
        // Duplicate-free outputs-to-install.
        {
            let mut seen = std::collections::HashSet::new();
            for name in &outputs_to_install {
                if !seen.insert(name) {
                    return Err(RealizationError::DuplicateOutputToInstall);
                }
            }
        }
        // Every selected name must be a real output.
        for name in &outputs_to_install {
            if !outputs.contains_key(name) {
                return Err(RealizationError::SelectedOutputNotPresent);
            }
        }
        // The primary store path must be one of the output values.
        if !outputs.values().any(|p| p == &store_path) {
            return Err(RealizationError::PrimaryStorePathNotAnOutput);
        }

        Ok(Self {
            store_path,
            deriver,
            outputs,
            outputs_to_install,
            system,
            nixpkgs_revision,
            nar_hash,
            closure_nar_size,
            pname,
            version,
        })
    }

    /// Returns the canonical identity (the primary store path).
    #[must_use]
    pub fn identity(&self) -> RealizationIdentity {
        RealizationIdentity::new(self.store_path.clone())
    }

    /// Returns the primary store path.
    #[must_use]
    pub const fn store_path(&self) -> &StorePath {
        &self.store_path
    }

    /// Returns the derivation that produced this realization.
    #[must_use]
    pub const fn deriver(&self) -> &DerivationPath {
        &self.deriver
    }

    /// Returns the per-output store paths.
    #[must_use]
    pub const fn outputs(&self) -> &BTreeMap<OutputName, StorePath> {
        &self.outputs
    }

    /// Returns the outputs selected for installation.
    #[must_use]
    pub fn outputs_to_install(&self) -> &[OutputName] {
        &self.outputs_to_install
    }

    /// Returns the target system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the pinned Nixpkgs revision this was realized from.
    #[must_use]
    pub const fn nixpkgs_revision(&self) -> &NixpkgsRevision {
        &self.nixpkgs_revision
    }

    /// Returns the NAR hash (sha256 SRI) of the **primary** store path.
    ///
    /// This is the [`StorePath`] returned by [`Realization::store_path`]. NAR
    /// hashes of secondary outputs (other entries in [`Realization::outputs`])
    /// are **not** represented by this PR-2 schema.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }

    /// Returns the closure NAR size in bytes.
    #[must_use]
    pub const fn closure_nar_size(&self) -> u64 {
        self.closure_nar_size
    }

    /// Returns the display `pname` (metadata only; never an identity).
    #[must_use]
    pub fn pname(&self) -> &str {
        &self.pname
    }

    /// Returns the display version (metadata only; never an identity).
    #[must_use]
    pub const fn version(&self) -> &PackageVersion {
        &self.version
    }
}

// NOTE: Eq/Hash are identity-only by design (see the type-level docs). Only the
// primary store path participates.
impl PartialEq for Realization {
    fn eq(&self, other: &Self) -> bool {
        self.store_path == other.store_path
    }
}

impl Eq for Realization {}

impl Hash for Realization {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.store_path.hash(state);
    }
}

#[cfg(test)]
mod tests;
