//! Failure-atomic orchestration for the managed macOS APFS store volume.

use std::{error::Error, fmt};

/// Stable failures for managed-store provisioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreProvisionErrorCode {
    /// Existing product state was incomplete or internally inconsistent.
    InvalidState,
    /// A closed privileged backend operation failed.
    BackendFailure,
    /// Provisioning failed and exact rollback did not complete.
    RollbackIncomplete,
}

/// Redacted managed-store provisioning failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsStoreProvisionError {
    code: MacOsStoreProvisionErrorCode,
}

impl MacOsStoreProvisionError {
    const fn new(code: MacOsStoreProvisionErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a redacted backend failure for adapters and tests.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(MacOsStoreProvisionErrorCode::BackendFailure)
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsStoreProvisionErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsStoreProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS managed store provisioning failed")
    }
}

impl Error for MacOsStoreProvisionError {}

/// Successful provisioning disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreProvisionOutcome {
    /// The complete exact product-owned volume state already existed.
    AlreadyProvisioned,
    /// A new exact product-owned volume state was committed.
    Provisioned,
}

/// Closed privileged operations used by the macOS volume provisioner.
///
/// The backend owns a durable before-mutation journal. In particular,
/// `create_encrypted_volume` must generate the unlock secret internally, pass
/// it to `diskutil` through stdin, store it in the fixed System-keychain item,
/// and never return or log it.
pub trait MacOsStoreProvisionBackend {
    /// Replays any uncommitted durable journal before normal state inspection.
    ///
    /// This is a no-op when no journal exists. When one exists, the backend
    /// attempts every recorded cleanup in reverse and verifies the before-state.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when cleanup or before-state verification is incomplete.
    fn recover_pending_journal(&mut self) -> Result<(), MacOsStoreProvisionError>;

    /// Returns the UUID only when the complete exact product state already exists.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for partial, foreign, ambiguous, or unreadable state.
    fn existing_volume_uuid(&mut self) -> Result<Option<String>, MacOsStoreProvisionError>;

    /// Opens and durably syncs an empty exact rollback journal before mutation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error without mutation when the journal cannot be created.
    fn begin_journal(&mut self) -> Result<(), MacOsStoreProvisionError>;

    /// Journals prior state, then merges only the compiled `nix` synthetic entry.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for any unsafe or conflicting file state.
    fn ensure_synthetic_entry(&mut self) -> Result<(), MacOsStoreProvisionError>;

    /// Creates the encrypted APFS volume and its fixed System-keychain item.
    ///
    /// # Errors
    ///
    /// Returns a redacted error after recording every completed sub-mutation.
    fn create_encrypted_volume(&mut self) -> Result<String, MacOsStoreProvisionError>;

    /// Enables ownership on the exact newly created volume.
    ///
    /// # Errors
    ///
    /// Returns a redacted error after journaling the attempted transition.
    fn enable_ownership(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError>;

    /// Mounts the exact newly created volume at the compiled `/nix` mount point.
    ///
    /// # Errors
    ///
    /// Returns a redacted error after journaling the attempted transition.
    fn mount_volume(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError>;

    /// Publishes the fixed root-only dynamic record for the exact UUID.
    ///
    /// # Errors
    ///
    /// Returns a redacted error without accepting any other dynamic field.
    fn publish_record(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError>;

    /// Re-inspects the volume, keychain selector, synthetic entry, and record.
    ///
    /// # Errors
    ///
    /// Returns a redacted error unless the complete final contract matches.
    fn verify_final(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreProvisionError>;

    /// Marks the synced journal committed and removes it durably.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the success boundary cannot be persisted.
    fn commit_journal(&mut self) -> Result<(), MacOsStoreProvisionError>;

    /// Replays the durable journal in reverse and attempts every recorded cleanup.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when any recorded mutation or residue remains.
    fn rollback_journal(&mut self) -> Result<(), MacOsStoreProvisionError>;
}

/// Provisions or verifies the exact encrypted managed-store volume contract.
///
/// # Errors
///
/// Returns `InvalidState` for a backend-supplied noncanonical UUID,
/// `BackendFailure` when a step fails and rollback succeeds, or
/// `RollbackIncomplete` when cleanup cannot restore the before-state.
pub fn provision_macos_store_volume(
    backend: &mut dyn MacOsStoreProvisionBackend,
) -> Result<MacOsStoreProvisionOutcome, MacOsStoreProvisionError> {
    backend.recover_pending_journal().map_err(|_| {
        MacOsStoreProvisionError::new(MacOsStoreProvisionErrorCode::RollbackIncomplete)
    })?;
    if let Some(existing) = backend.existing_volume_uuid()? {
        validate_uuid(&existing)?;
        backend.verify_final(&existing)?;
        return Ok(MacOsStoreProvisionOutcome::AlreadyProvisioned);
    }

    backend.begin_journal()?;
    let result = (|| {
        backend.ensure_synthetic_entry()?;
        let volume_uuid = backend.create_encrypted_volume()?;
        validate_uuid(&volume_uuid)?;
        backend.enable_ownership(&volume_uuid)?;
        backend.mount_volume(&volume_uuid)?;
        backend.publish_record(&volume_uuid)?;
        backend.verify_final(&volume_uuid)?;
        backend.commit_journal()?;
        Ok(MacOsStoreProvisionOutcome::Provisioned)
    })();
    match result {
        Ok(outcome) => Ok(outcome),
        Err(_) if backend.rollback_journal().is_err() => Err(MacOsStoreProvisionError::new(
            MacOsStoreProvisionErrorCode::RollbackIncomplete,
        )),
        Err(error) => Err(error),
    }
}

fn validate_uuid(value: &str) -> Result<(), MacOsStoreProvisionError> {
    if crate::store_mount::canonical_uuid(value) {
        Ok(())
    } else {
        Err(MacOsStoreProvisionError::new(
            MacOsStoreProvisionErrorCode::InvalidState,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "01234567-89AB-CDEF-0123-456789ABCDEF";

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<&'static str>,
        existing: Option<String>,
        created_uuid: Option<String>,
        fail: Option<&'static str>,
        rollback_fails: bool,
        pending_recovery: bool,
    }

    impl FakeBackend {
        fn step(&mut self, name: &'static str) -> Result<(), MacOsStoreProvisionError> {
            self.calls.push(name);
            if self.fail == Some(name) {
                Err(MacOsStoreProvisionError::backend_failure())
            } else {
                Ok(())
            }
        }
    }

    impl MacOsStoreProvisionBackend for FakeBackend {
        fn recover_pending_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
            self.step("recover")?;
            self.pending_recovery = false;
            Ok(())
        }

        fn existing_volume_uuid(&mut self) -> Result<Option<String>, MacOsStoreProvisionError> {
            self.step("inspect")?;
            Ok(self.existing.clone())
        }

        fn begin_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
            self.step("begin")
        }

        fn ensure_synthetic_entry(&mut self) -> Result<(), MacOsStoreProvisionError> {
            self.step("synthetic")
        }

        fn create_encrypted_volume(&mut self) -> Result<String, MacOsStoreProvisionError> {
            self.step("create")?;
            Ok(self.created_uuid.clone().unwrap_or_else(|| UUID.to_owned()))
        }

        fn enable_ownership(&mut self, _volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
            self.step("ownership")
        }

        fn mount_volume(&mut self, _volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
            self.step("mount")
        }

        fn publish_record(&mut self, _volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
            self.step("record")
        }

        fn verify_final(&mut self, _volume_uuid: &str) -> Result<(), MacOsStoreProvisionError> {
            self.step("verify")
        }

        fn commit_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
            self.step("commit")
        }

        fn rollback_journal(&mut self) -> Result<(), MacOsStoreProvisionError> {
            self.calls.push("rollback");
            if self.rollback_fails {
                Err(MacOsStoreProvisionError::backend_failure())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn exact_existing_state_is_idempotent() -> Result<(), MacOsStoreProvisionError> {
        let mut backend = FakeBackend {
            existing: Some(UUID.to_owned()),
            ..FakeBackend::default()
        };
        assert_eq!(
            provision_macos_store_volume(&mut backend)?,
            MacOsStoreProvisionOutcome::AlreadyProvisioned
        );
        assert_eq!(backend.calls, ["recover", "inspect", "verify"]);
        Ok(())
    }

    #[test]
    fn new_state_is_ordered_and_receipt_last() -> Result<(), MacOsStoreProvisionError> {
        let mut backend = FakeBackend::default();
        assert_eq!(
            provision_macos_store_volume(&mut backend)?,
            MacOsStoreProvisionOutcome::Provisioned
        );
        assert_eq!(
            backend.calls,
            [
                "recover",
                "inspect",
                "begin",
                "synthetic",
                "create",
                "ownership",
                "mount",
                "record",
                "verify",
                "commit"
            ]
        );
        Ok(())
    }

    #[test]
    fn interrupted_journal_is_recovered_before_state_inspection() {
        let mut recovered = FakeBackend {
            pending_recovery: true,
            ..FakeBackend::default()
        };
        assert_eq!(
            provision_macos_store_volume(&mut recovered),
            Ok(MacOsStoreProvisionOutcome::Provisioned)
        );
        assert_eq!(recovered.calls.first(), Some(&"recover"));
        assert!(!recovered.pending_recovery);

        let mut incomplete = FakeBackend {
            fail: Some("recover"),
            ..FakeBackend::default()
        };
        assert_eq!(
            provision_macos_store_volume(&mut incomplete)
                .err()
                .map(MacOsStoreProvisionError::code),
            Some(MacOsStoreProvisionErrorCode::RollbackIncomplete)
        );
        assert_eq!(incomplete.calls, ["recover"]);
    }

    #[test]
    fn every_post_journal_failure_rolls_back() {
        for step in [
            "synthetic",
            "create",
            "ownership",
            "mount",
            "record",
            "verify",
            "commit",
        ] {
            let mut backend = FakeBackend {
                fail: Some(step),
                ..FakeBackend::default()
            };
            assert_eq!(
                provision_macos_store_volume(&mut backend)
                    .err()
                    .map(MacOsStoreProvisionError::code),
                Some(MacOsStoreProvisionErrorCode::BackendFailure)
            );
            assert_eq!(backend.calls.last(), Some(&"rollback"));
        }
    }

    #[test]
    fn invalid_uuid_rolls_back_and_cleanup_failure_has_priority() {
        let mut invalid = FakeBackend {
            created_uuid: Some("not-a-uuid".to_owned()),
            ..FakeBackend::default()
        };
        assert_eq!(
            provision_macos_store_volume(&mut invalid)
                .err()
                .map(MacOsStoreProvisionError::code),
            Some(MacOsStoreProvisionErrorCode::InvalidState)
        );
        assert_eq!(invalid.calls.last(), Some(&"rollback"));

        let mut incomplete = FakeBackend {
            fail: Some("mount"),
            rollback_fails: true,
            ..FakeBackend::default()
        };
        assert_eq!(
            provision_macos_store_volume(&mut incomplete)
                .err()
                .map(MacOsStoreProvisionError::code),
            Some(MacOsStoreProvisionErrorCode::RollbackIncomplete)
        );
    }
}
