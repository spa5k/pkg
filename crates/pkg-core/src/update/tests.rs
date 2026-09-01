//! Tests for the `update` module.

use super::*;
use crate::lifecycle_test_support::state;

#[test]
fn metadata_update_changes_only_the_top_level_channel_binding() {
    let current = state();
    let before_manifest = current.manifest().entries().to_vec();
    let before_lock = current.locked().entries().clone();

    let updated = advance_channel(current, ChannelSequence::from_u64(3).unwrap()).unwrap();

    assert_eq!(updated.manifest().channel_seq().get().get(), 3);
    assert_eq!(updated.locked().channel_seq().get().get(), 3);
    assert_eq!(updated.manifest().entries(), before_manifest);
    assert_eq!(updated.locked().entries(), &before_lock);
}

#[test]
fn metadata_update_refuses_sequence_rollback() {
    assert_eq!(
        advance_channel(state(), ChannelSequence::from_u64(1).unwrap()),
        Err(ChannelUpdateError::SequenceRollback)
    );
}
