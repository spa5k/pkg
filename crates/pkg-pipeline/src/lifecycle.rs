//! Promotion of resolved, verified outputs into coherent lifecycle state.

use std::fmt;

use pkg_core::lifecycle::LifecycleState;
use pkg_core::state::LockEntry;
use pkg_core::{InstallPackage, InstallResult, Realization, install_packages};

use crate::{ResolvedInstall, VerifiedInstall};

/// Stable refusal while binding pipeline evidence into install state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStateError {
    /// A resolved root or selected verified output was absent or inconsistent.
    IncompleteEvidence,
    /// Exact realization construction rejected inconsistent output identity.
    InvalidRealization,
    /// Desired/locked lifecycle editing refused the requested addition.
    InvalidLifecycle,
}

impl fmt::Display for InstallStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "install state assembly refused: {self:?}")
    }
}
impl std::error::Error for InstallStateError {}

/// Binds every resolved selector to its verified primary output and atomically
/// produces the next manifest/lock state.
pub fn assemble_install_state(
    current: Option<LifecycleState>,
    resolved: &ResolvedInstall,
    verified: &VerifiedInstall,
    uid: u32,
    timestamp: &str,
) -> Result<InstallResult, InstallStateError> {
    let mut packages = Vec::with_capacity(resolved.targets().len());
    for target in resolved.targets() {
        let root = target
            .plan()
            .derivations()
            .iter()
            .find(|derivation| derivation.derivation() == target.plan().root())
            .ok_or(InstallStateError::IncompleteEvidence)?;
        let primary_output = target
            .plan()
            .outputs_to_install()
            .iter()
            .find(|output| output.as_str() == "out")
            .or_else(|| target.plan().outputs_to_install().first())
            .ok_or(InstallStateError::IncompleteEvidence)?;
        let primary_expected = root
            .outputs()
            .get(primary_output)
            .ok_or(InstallStateError::IncompleteEvidence)?;
        let primary = verified
            .outputs()
            .iter()
            .find(|output| {
                output.planned().selector_id() == target.selector().id()
                    && output.planned().output() == primary_output
                    && output.substitute().store_path() == primary_expected
            })
            .ok_or(InstallStateError::IncompleteEvidence)?;
        for output in target.plan().outputs_to_install() {
            let expected = root
                .outputs()
                .get(output)
                .ok_or(InstallStateError::IncompleteEvidence)?;
            if !verified.outputs().iter().any(|verified| {
                verified.planned().selector_id() == target.selector().id()
                    && verified.planned().output() == output
                    && verified.substitute().store_path() == expected
            }) {
                return Err(InstallStateError::IncompleteEvidence);
            }
        }
        let outputs_to_install = target.plan().outputs_to_install().to_vec();
        let realization = Realization::new(
            primary.substitute().store_path().clone(),
            target.plan().root().clone(),
            root.outputs().clone(),
            outputs_to_install,
            resolved.system(),
            resolved.revision().clone(),
            primary.substitute().nar_hash().clone(),
            primary.substitute().closure_size(),
            target.plan().pname().to_owned(),
            target.plan().version().clone(),
        )
        .map_err(|_| InstallStateError::InvalidRealization)?;
        let attribute = target
            .selector()
            .attribute()
            .cloned()
            .ok_or(InstallStateError::IncompleteEvidence)?;
        let signatures = primary
            .substitute()
            .signatures()
            .iter()
            .map(|signature| signature.as_str().to_owned())
            .collect();
        let lock = LockEntry::new(
            attribute,
            realization,
            timestamp.to_owned(),
            "cache:authenticated".to_owned(),
            signatures,
        )
        .map_err(|_| InstallStateError::InvalidLifecycle)?;
        packages.push(
            InstallPackage::new(target.selector().clone(), lock, timestamp, "user:install")
                .map_err(|_| InstallStateError::InvalidLifecycle)?,
        );
    }
    install_packages(
        current,
        resolved.channel_sequence(),
        resolved.system(),
        uid,
        packages,
    )
    .map_err(|_| InstallStateError::InvalidLifecycle)
}
