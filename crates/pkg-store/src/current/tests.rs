//! Tests for the `current` module.

use super::*;
use crate::roots::{RootCandidate, prepare_root_set};
use pkg_core::identity::{OutputName, StorePath};
use pkg_core::selector::SelectorId;
use pkg_core::state::CollisionPolicy;
use pkg_nix::{InProcessHelper, InProcessPeer};
use tempfile::Builder;

#[test]
fn swaps_current_as_an_exact_relative_symlink() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(state.join("activations")).unwrap();
    fs::set_permissions(state.join("activations"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(state.join("activations/gen-0001")).unwrap();
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    let generation = GenerationId::new("gen-0001").unwrap();
    layout.switch_current(&generation, "abc123").unwrap();
    assert_eq!(
        fs::read_link(state.join("current")).unwrap(),
        Path::new("activations/gen-0001")
    );
    assert_eq!(layout.current_generation().unwrap(), Some(generation));
}

#[test]
fn empty_generation_activates_without_an_empty_root_request() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let activations = state.join("activations");
    fs::create_dir(&activations).unwrap();
    fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
    let generation = GenerationId::new("gen-0001").unwrap();
    let plan = crate::activate::stage_from_sources(
        &activations.join("gen-0001.staging"),
        &[],
        CollisionPolicy::Abort,
    )
    .unwrap();
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(uid);
    let mut events = Vec::new();
    activate_generation(
        &layout,
        &generation,
        &plan,
        None,
        &maintenance,
        "empty1",
        |event| {
            events.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        events,
        [
            ActivationEvent::Rooted,
            ActivationEvent::ForestRetained,
            ActivationEvent::Activated
        ]
    );
    assert_eq!(layout.current_generation().unwrap(), Some(generation));
}

#[test]
fn rejects_symlinked_or_world_writable_state_components() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        StateLayout::open(temp.path(), &real, uid),
        Err(CurrentError::UnsafePermissions)
    ));
    fs::remove_dir(&real).unwrap();
    fs::create_dir(&real).unwrap();
    let link = temp.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(matches!(
        StateLayout::open(temp.path(), &link, uid),
        Err(CurrentError::UnsafePath)
    ));
}

#[test]
fn initializes_only_the_fixed_private_state_tree() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("nested/state");
    let layout = StateLayout::initialize(temp.path(), &state, uid).unwrap();
    assert_eq!(layout.state_root(), state);
    for relative in [
        "",
        "generations",
        "journal",
        "activations",
        "run",
        "cache",
        "logs",
    ] {
        let metadata = fs::symlink_metadata(state.join(relative)).unwrap();
        assert!(metadata.file_type().is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }
    let marker = state.join(STATE_OWNERSHIP_MARKER_NAME);
    let metadata = fs::symlink_metadata(&marker).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read(marker).unwrap(), STATE_OWNERSHIP_MARKER_BYTES);
    assert!(StateLayout::initialize(temp.path(), &state, uid).is_ok());

    let escaped = temp.path().parent().unwrap().join("pkg-escaped-state");
    assert!(matches!(
        StateLayout::initialize(temp.path(), &escaped, uid),
        Err(CurrentError::UnsafePath)
    ));
}

#[test]
fn initialization_never_adopts_nonempty_unmarked_state() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(state.join("foreign"), b"keep").unwrap();

    assert!(matches!(
        StateLayout::initialize(temp.path(), &state, uid),
        Err(CurrentError::UnsafePath)
    ));
    assert_eq!(fs::read(state.join("foreign")).unwrap(), b"keep");
    assert!(!state.join(STATE_OWNERSHIP_MARKER_NAME).exists());

    fs::remove_file(state.join("foreign")).unwrap();
    assert!(matches!(
        StateLayout::initialize(temp.path(), &state, uid),
        Err(CurrentError::UnsafePath)
    ));
    assert!(!state.join(STATE_OWNERSHIP_MARKER_NAME).exists());
}

#[test]
fn roots_and_retains_before_switching_current() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let activations = state.join("activations");
    fs::create_dir(&activations).unwrap();
    fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
    let generation = GenerationId::new("gen-0001").unwrap();
    let store_path = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
    let candidate = RootCandidate::new(
        SelectorId::new("sel_a").unwrap(),
        OutputName::new("out").unwrap(),
        store_path.clone(),
    );
    let roots = prepare_root_set(uid, generation.clone(), [candidate]).unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("tool"), b"tool").unwrap();
    let plan = crate::activate::stage_from_sources(
        &activations.join("gen-0001.staging"),
        &[(store_path, source)],
        CollisionPolicy::Abort,
    )
    .unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let session = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap();
    let maintenance = session.for_caller(uid);
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    let mut events = Vec::new();
    activate_rooted_generation(
        &layout,
        &generation,
        &plan,
        &roots,
        &maintenance,
        "n1",
        |event| {
            events.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        events,
        [
            ActivationEvent::Rooted,
            ActivationEvent::ForestRetained,
            ActivationEvent::Activated
        ]
    );
    assert!(!activations.join("gen-0001.staging").exists());
    assert!(activations.join("gen-0001").is_dir());
    assert_eq!(layout.current_generation().unwrap(), Some(generation));
}

#[test]
fn classifies_all_four_recovery_states_and_rejects_impossible_evidence() {
    let evidence = |rooted, current_is_generation, committed| RecoveryEvidence {
        prepared: true,
        rooted,
        current_is_generation,
        committed,
    };
    assert_eq!(
        classify_recovery(evidence(false, false, false)).unwrap(),
        RecoveryAction::DiscardPrepared
    );
    assert_eq!(
        classify_recovery(evidence(true, false, false)).unwrap(),
        RecoveryAction::DiscardRooted
    );
    assert_eq!(
        classify_recovery(evidence(true, true, false)).unwrap(),
        RecoveryAction::FinishActivated
    );
    assert_eq!(
        classify_recovery(evidence(true, true, true)).unwrap(),
        RecoveryAction::None
    );
    assert!(classify_recovery(evidence(false, true, false)).is_err());
    assert!(
        classify_recovery(RecoveryEvidence {
            prepared: false,
            rooted: false,
            current_is_generation: false,
            committed: false
        })
        .is_err()
    );
}

#[test]
fn helper_refusal_never_retains_or_switches_the_forest() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    let activations = state.join("activations");
    fs::create_dir_all(&activations).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
    let generation = GenerationId::new("gen-0001").unwrap();
    let store_path = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
    let roots = prepare_root_set(
        uid,
        generation.clone(),
        [RootCandidate::new(
            SelectorId::new("sel_a").unwrap(),
            OutputName::new("out").unwrap(),
            store_path.clone(),
        )],
    )
    .unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("tool"), b"tool").unwrap();
    let plan = crate::activate::stage_from_sources(
        &activations.join("gen-0001.staging"),
        &[(store_path, source)],
        CollisionPolicy::Abort,
    )
    .unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let session = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap();
    let maintenance = session.for_caller(uid);
    helper.restart().unwrap();
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    assert!(matches!(
        activate_rooted_generation(
            &layout,
            &generation,
            &plan,
            &roots,
            &maintenance,
            "n1",
            |_| Ok(())
        ),
        Err(CurrentError::RootPublication)
    ));
    assert!(activations.join("gen-0001.staging").is_dir());
    assert!(!activations.join("gen-0001").exists());
    assert_eq!(layout.current_generation().unwrap(), None);
}

#[test]
fn mismatched_root_set_is_refused_before_publication() {
    let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
    let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = temp.path().join("state");
    let activations = state.join("activations");
    fs::create_dir_all(&activations).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("tool"), b"tool").unwrap();
    let planned = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
    let plan = crate::activate::stage_from_sources(
        &activations.join("gen-0001.staging"),
        &[(planned, source)],
        CollisionPolicy::Abort,
    )
    .unwrap();
    let generation = GenerationId::new("gen-0001").unwrap();
    let roots = prepare_root_set(
        uid,
        generation.clone(),
        [RootCandidate::new(
            SelectorId::new("sel_b").unwrap(),
            OutputName::new("out").unwrap(),
            StorePath::new("/nix/store/00000000000000000000000000000000-b").unwrap(),
        )],
    )
    .unwrap();
    let helper = InProcessHelper::new(991).unwrap();
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))
        .unwrap()
        .for_caller(uid);
    let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
    assert!(matches!(
        activate_rooted_generation(
            &layout,
            &generation,
            &plan,
            &roots,
            &maintenance,
            "n1",
            |_| Ok(())
        ),
        Err(CurrentError::RootPublication)
    ));
    assert!(activations.join("gen-0001.staging").is_dir());
    assert_eq!(layout.current_generation().unwrap(), None);
}
