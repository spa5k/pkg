use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::lifecycle::LifecycleState;
use crate::state::{Generation, LockEntry, LockedState, Manifest, body_digest, canonical_digest};
use crate::{
    AttributePath, DerivationPath, GenerationSnapshot, NarHash, NixpkgsRevision, OutputName,
    PackageVersion, Realization, StorePath, System,
};

pub const REV1: &str = "0123456789abcdef0123456789abcdef01234567";
pub const REV2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

pub fn store(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
}

fn drv(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}.drv", hash.to_string().repeat(32))
}

fn manifest_entry(id: &str, name: &str, source_rev: &str, pinned_to: Option<&str>) -> Value {
    json!({
        "id": id,
        "selector": name,
        "attribute": name,
        "versionPref": { "kind": "any" },
        "outputs": null,
        "sourceRev": source_rev,
        "pinned": pinned_to.is_some(),
        "pinnedTo": pinned_to,
        "addedAt": "2026-08-09T00:00:00Z",
        "origin": "user:install"
    })
}

fn lock_entry(name: &str, hash: char, revision: &str, version: &str) -> Value {
    let output = store(hash, name);
    json!({
        "attribute": name,
        "nixpkgsRev": revision,
        "realized": {
            "storePath": output,
            "deriver": drv(hash, name),
            "outputs": { "out": output },
            "outputsToInstall": ["out"],
            "system": "x86_64-linux",
            "narHash": NAR,
            "closureNarSize": 42,
            "pname": name,
            "version": version
        },
        "lockedAt": "2026-08-09T00:00:01Z",
        "provenance": "cache:official",
        "sigsObserved": ["official-1:fixture"]
    })
}

pub fn state() -> LifecycleState {
    let manifest = json!({
        "schemaVersion": 1,
        "channelSeq": 2,
        "uid": 1001,
        "entries": [
            manifest_entry("sel_a", "alpha", "channel:current", None),
            manifest_entry("sel_b", "beta", &format!("rev:{REV1}"), None),
            manifest_entry("sel_c", "charlie", "channel:current", Some(store('2', "charlie").as_str()))
        ],
        "pins": ["sel_c"]
    });
    let locked = json!({
        "schemaVersion": 1,
        "channelSeq": 2,
        "system": "x86_64-linux",
        "uid": 1001,
        "entries": {
            "sel_a": lock_entry("alpha", '0', REV1, "1.0"),
            "sel_b": lock_entry("beta", '1', REV1, "1.0"),
            "sel_c": lock_entry("charlie", '2', REV1, "1.0")
        }
    });
    LifecycleState::new(
        Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        LockedState::from_json(&serde_json::to_vec(&locked).unwrap()).unwrap(),
    )
    .unwrap()
}

pub fn replacement(name: &str, hash: char, revision: &str, version: &str) -> LockEntry {
    let output = StorePath::new(&store(hash, name)).unwrap();
    let output_name = OutputName::new("out").unwrap();
    let realization = Realization::new(
        output.clone(),
        drv(hash, name).parse::<DerivationPath>().unwrap(),
        BTreeMap::from([(output_name.clone(), output)]),
        vec![output_name],
        System::X8664Linux,
        NixpkgsRevision::new(revision).unwrap(),
        NarHash::new(NAR).unwrap(),
        84,
        name.to_owned(),
        PackageVersion::new(version),
    )
    .unwrap();
    LockEntry::new(
        AttributePath::new(name).unwrap(),
        realization,
        "2026-08-09T01:00:00Z".into(),
        "cache:official".into(),
        vec!["official-1:replacement".into()],
    )
    .unwrap()
}

pub fn snapshot(
    id: &str,
    parent: Option<&str>,
    state: LifecycleState,
    operation: &str,
) -> GenerationSnapshot {
    let manifest_bytes = state.manifest().to_json().unwrap();
    let lock_bytes = state.locked().to_json().unwrap();
    let outputs = state
        .manifest()
        .entries()
        .iter()
        .map(|manifest_entry| {
            let locked = &state.locked().entries()[manifest_entry.id()];
            let realization = locked.realization();
            json!({
                "id": manifest_entry.id().as_str(),
                "attribute": locked.attribute().as_str(),
                "nixpkgsRev": realization.nixpkgs_revision().as_str(),
                "storePath": realization.store_path().as_str(),
                "deriver": realization.deriver().as_str(),
                "outputsToInstall": realization.outputs_to_install().iter().map(OutputName::as_str).collect::<Vec<_>>(),
                "narHash": realization.nar_hash().as_str(),
                "closureNarSize": realization.closure_nar_size(),
                "provenance": locked.provenance(),
                "pinned": manifest_entry.is_pinned()
            })
        })
        .collect::<Vec<_>>();
    let mut generation = json!({
        "schemaVersion": 1,
        "uid": state.manifest().uid(),
        "id": id,
        "parent": parent,
        "createdAt": format!("2026-08-09T00:00:{}Z", &id[id.len() - 2..]),
        "channelSeq": state.manifest().channel_seq().get().get(),
        "manifestHash": body_digest(&manifest_bytes).to_string(),
        "lockHash": body_digest(&lock_bytes).to_string(),
        "manifestSnapshot": format!("generations/{id}.manifest.json"),
        "lockSnapshot": format!("generations/{id}.lock.json"),
        "activation": {
            "kind": "pkg-symlink-forest",
            "treePath": format!("activations/{id}"),
            "treeDigest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
            "entryCount": state.selected_output_paths().len(),
            "collisionPolicy": "abort",
            "outputRoots": state.selected_output_paths().iter().map(StorePath::as_str).collect::<Vec<_>>(),
            "collisionResolutions": []
        },
        "outputs": outputs,
        "operation": {
            "opId": format!("op_{id}"),
            "kind": operation,
            "approval": { "build": "not_required" }
        }
    });
    let hash = canonical_digest(&generation).unwrap().to_string();
    generation
        .as_object_mut()
        .unwrap()
        .insert("generationHash".into(), json!(hash));
    let generation = Generation::from_json(&serde_json::to_vec(&generation).unwrap()).unwrap();
    GenerationSnapshot::new(generation, state).unwrap()
}
