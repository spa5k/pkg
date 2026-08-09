use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::lifecycle::LifecycleState;
use crate::state::{LockEntry, LockedState, Manifest};
use crate::{
    AttributePath, DerivationPath, NarHash, NixpkgsRevision, OutputName, PackageVersion,
    Realization, StorePath, System,
};

pub(crate) const REV1: &str = "0123456789abcdef0123456789abcdef01234567";
pub(crate) const REV2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

pub(crate) fn store(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
}

fn drv(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}.drv", hash.to_string().repeat(32))
}

fn manifest_entry(id: &str, name: &str, source_rev: &str, pinned_to: Option<String>) -> Value {
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

pub(crate) fn state() -> LifecycleState {
    let manifest = json!({
        "schemaVersion": 1,
        "channelSeq": 2,
        "uid": 1001,
        "entries": [
            manifest_entry("sel_a", "alpha", "channel:current", None),
            manifest_entry("sel_b", "beta", &format!("rev:{REV1}"), None),
            manifest_entry("sel_c", "charlie", "channel:current", Some(store('2', "charlie")))
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

pub(crate) fn replacement(name: &str, hash: char, revision: &str, version: &str) -> LockEntry {
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
