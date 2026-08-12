//! Promotion of resolved, verified outputs into coherent lifecycle state.

use std::collections::BTreeSet;
use std::fmt;

use pkg_core::lifecycle::LifecycleState;
use pkg_core::state::LockEntry;
use pkg_core::upgrade::{RemovedUpstreamPolicy, UpgradeOutcome, UpgradePlan, UpgradeResult};
use pkg_core::{InstallPackage, InstallResult, PackageSelector, Realization, install_packages};
use pkg_nix::{BuildOutputProvenance, InstallEvidence};

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

/// Applies broker-produced acquisition evidence to one exact upgrade plan.
pub fn assemble_upgrade_evidence_state(
    plan: UpgradePlan,
    evidence: &InstallEvidence,
    timestamp: &str,
) -> Result<UpgradeResult, InstallStateError> {
    if evidence.targets().len() != plan.selectors().len() {
        return Err(InstallStateError::IncompleteEvidence);
    }
    let outcomes = evidence
        .targets()
        .iter()
        .map(|target| {
            lock_entry_from_evidence(target, evidence, timestamp)
                .map(|entry| UpgradeOutcome::resolved(target.selector_id().clone(), entry))
        })
        .collect::<Result<Vec<_>, _>>()?;
    plan.apply(outcomes, RemovedUpstreamPolicy::Refuse)
        .map_err(|_| InstallStateError::InvalidLifecycle)
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

/// Promotes the broker's retained post-build evidence into coherent desired and
/// locked state. No caller-supplied store path participates in this boundary.
pub fn assemble_install_evidence_state(
    current: Option<LifecycleState>,
    evidence: &InstallEvidence,
    uid: u32,
    timestamp: &str,
) -> Result<InstallResult, InstallStateError> {
    let mut packages = Vec::with_capacity(evidence.targets().len());
    for target in evidence.targets() {
        let selector = PackageSelector::new(
            target.selector_id().clone(),
            target.selector().clone(),
            target.version_preference().clone(),
            target.output_selection().clone(),
            target.source_revision().clone(),
        )
        .with_attribute(target.attribute().clone())
        .map_err(|_| InstallStateError::InvalidLifecycle)?;
        let lock = lock_entry_from_evidence(target, evidence, timestamp)?;
        packages.push(
            InstallPackage::new(selector, lock, timestamp, "user:install")
                .map_err(|_| InstallStateError::InvalidLifecycle)?,
        );
    }
    install_packages(
        current,
        evidence.channel_sequence(),
        evidence.system(),
        uid,
        packages,
    )
    .map_err(|_| InstallStateError::InvalidLifecycle)
}

fn lock_entry_from_evidence(
    target: &pkg_nix::InstallTargetEvidence,
    evidence: &InstallEvidence,
    timestamp: &str,
) -> Result<LockEntry, InstallStateError> {
    let primary_output = target
        .outputs_to_install()
        .iter()
        .find(|output| output.as_str() == "out")
        .or_else(|| target.outputs_to_install().first())
        .ok_or(InstallStateError::IncompleteEvidence)?;
    let primary = target
        .acquired()
        .iter()
        .find(|output| output.output_name() == primary_output)
        .ok_or(InstallStateError::IncompleteEvidence)?;
    for output in target.outputs_to_install() {
        let expected = target
            .root_outputs()
            .get(output)
            .ok_or(InstallStateError::IncompleteEvidence)?;
        if !target.acquired().iter().any(|acquired| {
            acquired.output_name() == output && acquired.path_info().store_path() == expected
        }) {
            return Err(InstallStateError::IncompleteEvidence);
        }
    }
    let realization = Realization::new(
        primary.path_info().store_path().clone(),
        target.root_derivation().clone(),
        target.root_outputs().clone(),
        target.outputs_to_install().to_vec(),
        evidence.system(),
        evidence.revision().clone(),
        primary.path_info().nar_hash().clone(),
        primary.path_info().closure_size(),
        target.package_name().to_owned(),
        target.package_version().clone(),
    )
    .map_err(|_| InstallStateError::InvalidRealization)?;
    let provenance = if target
        .acquired()
        .iter()
        .any(|output| output.provenance() == BuildOutputProvenance::LocalBuild)
    {
        "build:local"
    } else {
        "cache:authenticated"
    };
    let signatures = target
        .acquired()
        .iter()
        .flat_map(|output| output.path_info().signatures())
        .map(|signature| signature.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    LockEntry::new(
        target.attribute().clone(),
        realization,
        timestamp.to_owned(),
        provenance.to_owned(),
        signatures,
    )
    .map_err(|_| InstallStateError::InvalidLifecycle)
}

#[cfg(test)]
mod tests {
    use pkg_core::upgrade::{UpgradeScope, select_upgrade};
    use pkg_nix::InstallEvidence;
    use serde_json::json;

    use super::{assemble_install_evidence_state, assemble_upgrade_evidence_state};

    const STORE: &str = "/nix/store/00000000000000000000000000000000-demo";
    const DRV: &str = "/nix/store/11111111111111111111111111111111-demo.drv";
    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn evidence(provenance: &str, signatures: &[&str]) -> InstallEvidence {
        evidence_at(1, REV, STORE, DRV, "1.0", provenance, signatures)
    }

    fn evidence_at(
        sequence: u64,
        revision: &str,
        store: &str,
        derivation: &str,
        version: &str,
        provenance: &str,
        signatures: &[&str],
    ) -> InstallEvidence {
        InstallEvidence::from_json_bytes(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "descriptorHash": format!("sha256-{}", "0".repeat(64)),
                "channelSequence": sequence,
                "policyVersion": 1,
                "revision": revision,
                "sourceNarHash": NAR,
                "system": "x86_64-linux",
                "targets": [{
                    "selectorId": "sel_demo",
                    "selector": "demo",
                    "attribute": "demo",
                    "versionPreference": { "kind": "any" },
                    "requestedOutputs": null,
                    "sourceRevision": "channel:current",
                    "rootDerivation": derivation,
                    "rootOutputs": [{ "name": "out", "storePath": store }],
                    "outputsToInstall": ["out"],
                    "packageName": "demo",
                    "packageVersion": version,
                    "acquired": [{
                        "outputName": "out",
                        "storePath": store,
                        "narHash": NAR,
                        "signatures": signatures,
                        "references": [],
                        "deriver": derivation,
                        "narSize": 20,
                        "closureSize": 42,
                        "provenance": provenance
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn broker_evidence_is_the_complete_install_state_input() {
        let result = assemble_install_evidence_state(
            None,
            &evidence("cacheSigned", &["cache.nixos.org-1:AAAA"]),
            501,
            "2026-08-11T00:00:00Z",
        )
        .unwrap();
        let state = result.state();
        let entry = &state.locked().entries()[state.manifest().entries()[0].id()];
        assert_eq!(entry.realization().store_path().as_str(), STORE);
        assert_eq!(entry.realization().deriver().as_str(), DRV);
        assert_eq!(entry.provenance(), "cache:authenticated");
        assert_eq!(state.manifest().uid(), 501);
    }

    #[test]
    fn local_build_truth_survives_state_promotion() {
        let result = assemble_install_evidence_state(
            None,
            &evidence("localBuild", &[]),
            501,
            "2026-08-11T00:00:00Z",
        )
        .unwrap();
        let state = result.state();
        let entry = &state.locked().entries()[state.manifest().entries()[0].id()];
        assert_eq!(entry.provenance(), "build:local");
    }

    #[test]
    fn upgrade_evidence_advances_only_the_selected_exact_lock() {
        const NEXT_REV: &str = "89abcdef0123456789abcdef0123456789abcdef";
        const NEXT_STORE: &str = "/nix/store/22222222222222222222222222222222-demo";
        const NEXT_DRV: &str = "/nix/store/33333333333333333333333333333333-demo.drv";
        let current = assemble_install_evidence_state(
            None,
            &evidence("cacheSigned", &["cache.nixos.org-1:AAAA"]),
            501,
            "2026-08-11T00:00:00Z",
        )
        .unwrap()
        .into_state();
        let selection = select_upgrade(current, UpgradeScope::All, false).unwrap();
        let next_evidence =
            evidence_at(2, NEXT_REV, NEXT_STORE, NEXT_DRV, "2.0", "localBuild", &[]);
        let plan = selection
            .bind_channel(
                next_evidence.channel_sequence(),
                next_evidence.revision().clone(),
            )
            .unwrap();
        let result =
            assemble_upgrade_evidence_state(plan, &next_evidence, "2026-08-12T00:00:00Z").unwrap();
        let state = result.state();
        let id = state.manifest().entries()[0].id();
        let entry = &state.locked().entries()[id];
        assert_eq!(state.manifest().channel_seq().get().get(), 2);
        assert_eq!(entry.realization().store_path().as_str(), NEXT_STORE);
        assert_eq!(entry.realization().version().as_str(), "2.0");
        assert_eq!(entry.realization().nixpkgs_revision().as_str(), NEXT_REV);
        assert_eq!(entry.provenance(), "build:local");
    }
}
