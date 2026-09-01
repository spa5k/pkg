//! Tests for the `roots` module.

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
    assert_eq!(
        prepared
            .output_roots()
            .into_iter()
            .map(StorePath::as_str)
            .collect::<Vec<_>>(),
        vec![
            "/nix/store/00000000000000000000000000000000-a",
            "/nix/store/00000000000000000000000000000000-b"
        ]
    );
    assert!(prepared.request().entries().iter().all(|entry| {
        entry.name().as_str().starts_with("out-") && entry.name().as_str().len() == 36
    }));
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
