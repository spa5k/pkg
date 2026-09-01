//! Tests for the `synthetic_conf` module.

use super::*;

#[test]
fn missing_entry_is_appended_without_rewriting_other_bytes() -> Result<(), MacOsSyntheticConfError>
{
    let empty = plan_macos_synthetic_entry(None)?;
    assert!(empty.changed());
    assert_eq!(empty.bytes(), b"nix\n");

    let existing = b"home\tUsers\nprivate\tSystem/Volumes/Data/private";
    let merged = plan_macos_synthetic_entry(Some(existing))?;
    assert!(merged.changed());
    assert_eq!(
        merged.bytes(),
        b"home\tUsers\nprivate\tSystem/Volumes/Data/private\nnix\n"
    );
    Ok(())
}

#[test]
fn exact_entry_is_idempotent_and_byte_preserving() -> Result<(), MacOsSyntheticConfError> {
    for existing in [b"nix".as_slice(), b"home\tUsers\nnix\n".as_slice()] {
        let plan = plan_macos_synthetic_entry(Some(existing))?;
        assert!(!plan.changed());
        assert_eq!(plan.bytes(), existing);
    }
    Ok(())
}

#[test]
fn conflicting_duplicate_and_malformed_state_fail_closed() {
    for conflicting in [
        b"nix\tSystem/Volumes/Data/nix".as_slice(),
        b"nix ".as_slice(),
        b" nix".as_slice(),
        b"nix\nnix\n".as_slice(),
    ] {
        assert_eq!(
            plan_macos_synthetic_entry(Some(conflicting))
                .err()
                .map(MacOsSyntheticConfError::code),
            Some(MacOsSyntheticConfErrorCode::ConflictingEntry)
        );
    }
    for malformed in [b"nix\r\n".as_slice(), b"bad\0line".as_slice(), &[0xff][..]] {
        assert_eq!(
            plan_macos_synthetic_entry(Some(malformed))
                .err()
                .map(MacOsSyntheticConfError::code),
            Some(MacOsSyntheticConfErrorCode::InvalidFile)
        );
    }
}

#[test]
fn input_and_output_are_bounded() {
    let oversized = vec![b'a'; MAX_SYNTHETIC_CONF_BYTES + 1];
    assert!(plan_macos_synthetic_entry(Some(&oversized)).is_err());

    let full_without_newline = vec![b'a'; MAX_SYNTHETIC_CONF_BYTES];
    assert!(plan_macos_synthetic_entry(Some(&full_without_newline)).is_err());
}
