//! Tests for the `remove` module.

use super::*;
use crate::lifecycle_test_support::state;

#[test]
fn removes_manifest_lock_and_pin_index_as_one_golden_edit() {
    let result = remove_selectors(
        state(),
        &[
            SelectorId::new("sel_a").unwrap(),
            SelectorId::new("sel_c").unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
            result.state().manifest().to_json().unwrap(),
            br#"{"schemaVersion":1,"channelSeq":2,"uid":1001,"entries":[{"id":"sel_b","selector":"beta","attribute":"beta","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"rev:0123456789abcdef0123456789abcdef01234567","pinned":false,"pinnedTo":null,"addedAt":"2026-08-09T00:00:00Z","origin":"user:install"}],"pins":[]}"#
        );
    assert_eq!(
        result
            .state()
            .locked()
            .entries()
            .keys()
            .map(SelectorId::as_str)
            .collect::<Vec<_>>(),
        ["sel_b"]
    );
    assert_eq!(
        result
            .state()
            .selected_output_paths()
            .iter()
            .map(crate::StorePath::as_str)
            .collect::<Vec<_>>(),
        ["/nix/store/11111111111111111111111111111111-beta"]
    );
}

#[test]
fn validates_every_target_before_editing() {
    let original = state();
    assert_eq!(
        remove_selectors(
            original.clone(),
            &[
                SelectorId::new("sel_a").unwrap(),
                SelectorId::new("sel_missing").unwrap(),
            ]
        ),
        Err(RemoveError::NotInstalled)
    );
    assert_eq!(original, state());
}
