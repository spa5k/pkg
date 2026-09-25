use pkg_core::state::{LockedState, Manifest};

use super::*;
use crate::cli::{Cli, Command};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn store(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
}
fn drv(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}.drv", hash.to_string().repeat(32))
}

fn state() -> LifecycleState {
    let alpha = store('0', "alpha");
    let beta = store('1', "beta");
    let manifest = json!({
        "schemaVersion": 1, "channelSeq": 2, "uid": 1001,
        "entries": [
            {"id":"sel_alpha","selector":"alpha","attribute":"alpha","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"channel:current","pinned":false,"pinnedTo":null,"addedAt":"2026-08-09T00:00:00Z","origin":"user:install"},
            {"id":"sel_beta","selector":"beta","attribute":"beta","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"channel:current","pinned":false,"pinnedTo":null,"addedAt":"2026-08-09T00:00:00Z","origin":"user:install"}
        ], "pins": []
    });
    let locked = json!({
        "schemaVersion":1,"channelSeq":2,"system":"x86_64-linux","uid":1001,
        "entries": {
            "sel_alpha":{"attribute":"alpha","nixpkgsRev":REVISION,"realized":{"storePath":alpha,"deriver":drv('0',"alpha"),"outputs":{"out":store('0',"alpha")},"outputsToInstall":["out"],"system":"x86_64-linux","narHash":NAR,"closureNarSize":42,"pname":"alpha","version":"1.0"},"lockedAt":"2026-08-09T00:00:01Z","provenance":"cache:official","sigsObserved":["official-1:fixture"]},
            "sel_beta":{"attribute":"beta","nixpkgsRev":REVISION,"realized":{"storePath":beta,"deriver":drv('1',"beta"),"outputs":{"out":store('1',"beta")},"outputsToInstall":["out"],"system":"x86_64-linux","narHash":NAR,"closureNarSize":84,"pname":"beta","version":"2.0"},"lockedAt":"2026-08-09T00:00:01Z","provenance":"cache:official","sigsObserved":["official-1:fixture"]}
        }
    });
    LifecycleState::new(
        Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        LockedState::from_json(&serde_json::to_vec(&locked).unwrap()).unwrap(),
    )
    .unwrap()
}

#[test]
fn list_maps_active_state_without_private_identity() {
    let cli = Cli::try_parse(["pkg", "list", "--with-outputs", "--size"]).unwrap();
    let Command::List(args) = cli.parsed_command() else {
        unreachable!()
    };
    let result = list_state(&state(), args, None).unwrap();
    assert_eq!(result.fields()["entries"].as_array().unwrap().len(), 2);
    assert_eq!(result.fields()["entries"][0]["closureBytes"], 42);
    let encoded = serde_json::to_string(result.fields()).unwrap();
    assert!(!encoded.contains("/nix/store/"));
    assert!(!encoded.contains("x86_64-linux"));
}

#[test]
fn remove_and_pin_use_core_atomic_lifecycle_editors() {
    let remove = Cli::try_parse(["pkg", "remove", "beta", "--orphan-check"]).unwrap();
    let Command::Remove(args) = remove.parsed_command() else {
        unreachable!()
    };
    let removed = remove_state(state(), args).unwrap();
    assert_eq!(removed.state().manifest().entries().len(), 1);
    assert_eq!(removed.result().fields()["orphanCheckRequested"], true);

    let pin = Cli::try_parse(["pkg", "pin", "alpha"]).unwrap();
    let Command::Pin(args) = pin.parsed_command() else {
        unreachable!()
    };
    let pinned = edit_pin_state(state(), args, PinAction::Pin).unwrap();
    assert!(pinned.state().manifest().entries()[0].is_pinned());
    assert_eq!(pinned.result().fields()["changed"][0], "sel_alpha");
}

#[test]
fn empty_history_is_offline_but_delete_stays_engine_bound() {
    let history = History::new(vec![], None).unwrap();
    let list = Cli::try_parse(["pkg", "history"]).unwrap();
    let Command::History(args) = list.parsed_command() else {
        unreachable!()
    };
    assert!(
        read_history(&history, args).unwrap().fields()["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let delete = Cli::try_parse(["pkg", "history", "--delete", "gen-0001"]).unwrap();
    let Command::History(args) = delete.parsed_command() else {
        unreachable!()
    };
    assert_eq!(
        read_history(&history, args).unwrap_err().exit_code(),
        ExitCode::EngineUnavailable
    );
}

use pkg_core::state::{Generation, body_digest, canonical_digest};
use pkg_core::{NixpkgsRevision, OutputName, StorePath};
fn snapshot(
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
                "nixpkgsRev": realization.source_commit().as_str(),
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

fn flake_state(ambiguous: bool) -> LifecycleState {
    let state = state();
    let mut manifest: Value = serde_json::from_slice(&state.manifest().to_json().unwrap()).unwrap();
    let mut locked: Value = serde_json::from_slice(&state.locked().to_json().unwrap()).unwrap();
    for (index, id, reference) in [
        (0, "sel_alpha", "github:casey/just#default"),
        (1, "sel_beta", "github:other/just#default"),
    ] {
        if index == 1 && !ambiguous {
            continue;
        }
        let flake = pkg_core::LockedFlake::new(
            pkg_core::PublicFlakeRef::new(reference).unwrap(),
            NixpkgsRevision::new(REVISION).unwrap(),
            pkg_core::NarHash::new(NAR).unwrap(),
            r#"{"version":7,"root":"root","nodes":{"root":{}}}"#,
        )
        .unwrap();
        manifest["entries"][index]["selector"] = json!(reference);
        manifest["entries"][index]["sourceRev"] =
            json!(pkg_core::SourceRevision::PublicFlake(flake.clone()).to_canonical_string());
        locked["entries"][id]["flake"] = json!(flake.to_json());
        locked["entries"][id]["realized"]["pname"] = json!("just");
    }
    LifecycleState::new(
        Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        LockedState::from_json(&serde_json::to_vec(&locked).unwrap()).unwrap(),
    )
    .unwrap()
}

#[test]
fn installed_flake_names_resolve_uniquely_without_changing_saved_intent() {
    let state = flake_state(false);
    let cli = Cli::try_parse(["pkg", "uninstall", "just", "github:casey/just#default"]).unwrap();
    let Command::Remove(args) = cli.parsed_command() else {
        panic!("not removal")
    };
    let removed = remove_state(state.clone(), args).unwrap();
    assert_eq!(removed.result().fields()["removed"], json!(["sel_alpha"]));
    assert_eq!(removed.result().fields()["packages"][0]["name"], "just");
    assert_eq!(
        removed.result().fields()["packages"][0]["selector"],
        "github:casey/just#default"
    );
    assert_eq!(removed.state().manifest().entries().len(), 1);
    let cli = Cli::try_parse(["pkg", "pin", "just"]).unwrap();
    let Command::Pin(args) = cli.parsed_command() else {
        panic!("not pin")
    };
    let pinned = edit_pin_state(state, args, PinAction::Pin).unwrap();
    assert_eq!(
        pinned.state().manifest().entries()[0].selector().as_str(),
        "github:casey/just#default"
    );
    assert!(pinned.state().manifest().entries()[0].is_pinned());
}

#[test]
fn ambiguous_or_partly_missing_requests_fail_atomically_with_choices() {
    let state = flake_state(true);
    let error = resolve_targets(&state, &["just".into()]).unwrap_err();
    assert_eq!(error.exit_code(), ExitCode::ResolveFailed);
    assert!(error.hint().contains("sel_alpha"));
    assert!(error.hint().contains("sel_beta"));
    assert_eq!(
        resolve_targets(&state, &["github:casey/just#default".into()]).unwrap(),
        [SelectorId::new("sel_alpha").unwrap()]
    );
    assert!(resolve_targets(&state, &["sel_alpha".into(), "missing".into()]).is_err());
    // Exact catalog selectors remain supported.
    assert_eq!(
        resolve_targets(&self::state(), &["alpha".into()]).unwrap(),
        [SelectorId::new("sel_alpha").unwrap()]
    );
}

#[test]
fn history_and_rollback_preserve_package_versions_sources_and_pin_changes() {
    let first = snapshot("gen-0001", None, flake_state(false), "install");
    let pinned = edit_pins(
        first.state().clone(),
        &[SelectorId::new("sel_alpha").unwrap()],
        PinAction::Pin,
    )
    .unwrap()
    .into_state();
    let second = snapshot("gen-0002", Some("gen-0001"), pinned, "pin");
    let archive = History::new(vec![first.clone(), second.clone()], Some("gen-0002")).unwrap();
    let cli = Cli::try_parse(["pkg", "history"]).unwrap();
    let Command::History(args) = cli.parsed_command() else {
        panic!("not history")
    };
    let result = read_history(&archive, args).unwrap();
    let change = &result.fields()["entries"][0]["packageChanges"][0];
    assert_eq!(change["name"], "just");
    assert_eq!(change["beforeVersion"], "1.0");
    assert_eq!(change["afterVersion"], "1.0");
    assert_eq!(change["afterPinned"], true);
    let cli = Cli::try_parse(["pkg", "history", "gen-0002"]).unwrap();
    let Command::History(args) = cli.parsed_command() else {
        panic!("not history")
    };
    assert_eq!(
        read_history(&archive, args).unwrap().fields()["packages"][0]["selector"],
        "github:casey/just#default"
    );
    let cli = Cli::try_parse(["pkg", "rollback"]).unwrap();
    let Command::Rollback(args) = cli.parsed_command() else {
        panic!("not rollback")
    };
    let (_, preview) = rollback_state(&second, &archive, args)
        .unwrap()
        .into_parts();
    assert_eq!(preview.fields()["packageChanges"][0]["afterPinned"], false);
    assert_eq!(preview.fields()["targetGeneration"], "gen-0001");
    // After pruning the parent, retain inventory without inventing an operation diff.
    let archive = History::new(vec![second], Some("gen-0002")).unwrap();
    let cli = Cli::try_parse(["pkg", "history"]).unwrap();
    let Command::History(args) = cli.parsed_command() else {
        panic!("not history")
    };
    let result = read_history(&archive, args).unwrap();
    assert!(result.fields()["entries"][0]["packageChanges"].is_null());
    assert_eq!(result.fields()["entries"][0]["packages"][0]["name"], "just");
}

#[test]
fn exact_catalog_selector_wins_over_an_installed_flake_name() {
    let state = flake_state(false);
    let mut manifest: Value = serde_json::from_slice(&state.manifest().to_json().unwrap()).unwrap();
    let mut locked: Value = serde_json::from_slice(&state.locked().to_json().unwrap()).unwrap();
    manifest["entries"][1]["selector"] = json!("just");
    manifest["entries"][1]["attribute"] = json!("just");
    locked["entries"]["sel_beta"]["attribute"] = json!("just");
    locked["entries"]["sel_beta"]["realized"]["pname"] = json!("just");
    let state = LifecycleState::new(
        Manifest::from_json(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        LockedState::from_json(&serde_json::to_vec(&locked).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resolve_targets(&state, &["just".into()]).unwrap(),
        [SelectorId::new("sel_beta").unwrap()]
    );
    assert_eq!(
        resolve_targets(&state, &["github:casey/just#default".into()]).unwrap(),
        [SelectorId::new("sel_alpha").unwrap()]
    );
}

#[test]
fn list_outdated_uses_catalog_results_and_keeps_name_only_output() {
    let cli = Cli::try_parse(["pkg", "list", "--outdated", "--name-only"]).unwrap();
    let Command::List(args) = cli.parsed_command() else {
        panic!("not list")
    };
    let attributes = std::collections::BTreeSet::from(["beta".into()]);
    let result = list_state(&state(), args, Some(&attributes)).unwrap();
    assert_eq!(result.fields()["entries"].as_array().unwrap().len(), 1);
    assert_eq!(result.fields()["entries"][0]["name"], "beta");
    assert_eq!(result.fields()["nameOnly"], true);
}

#[test]
fn invalid_installed_names_cannot_emit_terminal_controls() {
    let state = state();
    let name = "missing\u{001b}]52;c;payload\u{0007}\u{009b}31m".to_owned();
    let error = resolve_targets(&state, std::slice::from_ref(&name)).unwrap_err();
    assert!(!error.message().chars().any(char::is_control));
    assert!(!error.hint().chars().any(char::is_control));
    for command in ["remove", "pin", "unpin", "upgrade"] {
        let cli = Cli::try_parse(["pkg", command, &name]).unwrap();
        assert!(cli.validate().is_ok());
    }
}
