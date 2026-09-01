//! Tests for the `lifecycle` module.

use pkg_core::upgrade::{UpgradeScope, select_upgrade};
use pkg_nix::InstallEvidence;
use serde_json::json;

use super::{InstallStateError, assemble_install_evidence_state, assemble_upgrade_evidence_state};

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
fn repeated_install_evidence_reports_already_installed() {
    let evidence = evidence("cacheSigned", &["cache.nixos.org-1:AAAA"]);
    let current = assemble_install_evidence_state(None, &evidence, 501, "2026-08-11T00:00:00Z")
        .unwrap()
        .into_state();

    assert_eq!(
        assemble_install_evidence_state(Some(current), &evidence, 501, "2026-08-12T00:00:00Z",),
        Err(InstallStateError::AlreadyInstalled)
    );
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
    let next_evidence = evidence_at(2, NEXT_REV, NEXT_STORE, NEXT_DRV, "2.0", "localBuild", &[]);
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
