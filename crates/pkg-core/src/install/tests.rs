//! Tests for the `install` module.

use super::*;
use crate::lifecycle_test_support::{REV2, replacement, state};
use crate::{AttributePath, OutputSelection, SelectorInput, SourceRevision, VersionPreference};

fn package(id: &str, name: &str, hash: char) -> InstallPackage {
    let selector = PackageSelector::new(
        SelectorId::new(id).unwrap(),
        SelectorInput::new(name).unwrap(),
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    )
    .with_attribute(AttributePath::new(name).unwrap())
    .unwrap();
    InstallPackage::new(
        selector,
        replacement(name, hash, REV2, "2.0"),
        "2026-08-09T02:00:00Z",
        "user:install",
    )
    .unwrap()
}

#[test]
fn adds_packages_atomically_and_preserves_existing_locks() {
    let current = state();
    let existing = current.locked().entries().clone();
    let result = install_packages(
        Some(current),
        ChannelSequence::from_u64(3).unwrap(),
        System::X8664Linux,
        1001,
        vec![package("sel_delta", "delta", '3')],
    )
    .unwrap();
    assert_eq!(result.added(), [SelectorId::new("sel_delta").unwrap()]);
    assert_eq!(result.state().manifest().channel_seq().get().get(), 3);
    for (id, entry) in existing {
        assert_eq!(&result.state().locked().entries()[&id], &entry);
    }
    assert_eq!(result.state().manifest().entries().len(), 4);
}

#[test]
fn duplicate_and_existing_requests_leave_original_state_untouched() {
    let current = state();
    let duplicate = package("sel_delta", "delta", '3');
    assert_eq!(
        install_packages(
            Some(current.clone()),
            ChannelSequence::from_u64(3).unwrap(),
            System::X8664Linux,
            1001,
            vec![duplicate.clone(), duplicate],
        ),
        Err(InstallEditError::DuplicateTarget)
    );
    assert_eq!(
        install_packages(
            Some(current.clone()),
            ChannelSequence::from_u64(3).unwrap(),
            System::X8664Linux,
            1001,
            vec![package("sel_a", "delta", '3')],
        ),
        Err(InstallEditError::AlreadyInstalled)
    );
    assert_eq!(current, state());
}

#[test]
fn empty_initial_state_is_supported_but_cross_system_is_refused() {
    let package = package("sel_delta", "delta", '3');
    assert_eq!(
        install_packages(
            None,
            ChannelSequence::from_u64(1).unwrap(),
            System::Aarch64Darwin,
            1001,
            vec![package.clone()],
        ),
        Err(InstallEditError::SystemMismatch)
    );
    let result = install_packages(
        None,
        ChannelSequence::from_u64(1).unwrap(),
        System::X8664Linux,
        1001,
        vec![package],
    )
    .unwrap();
    assert_eq!(result.state().manifest().entries().len(), 1);
}
