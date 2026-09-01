//! Tests for the `uninstall` module.

use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use crate::{
    NixVersion,
    managed::ownership::{ManagedGroup, ManagedGroupBindings, verify_ownership_expectation},
};
use pkg_core::state::body_digest;
use tempfile::TempDir;

struct Fixture {
    temporary: TempDir,
    expectation: OwnershipExpectation,
    owner_uid: u32,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::build(true)
    }

    fn new_interrupted() -> Result<Self, Box<dyn std::error::Error>> {
        Self::build(false)
    }

    fn build(write_receipt: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let owner_uid = fs::metadata(temporary.path())?.uid();
        let version = NixVersion::new("2.34.8")?;
        let runtime = b"authenticated nix";
        let artifacts = vec![
            ManagedArtifact::directory("/nix", ManagedGroup::BuildUsers, 0o755)?,
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775)?,
            ManagedArtifact::file(
                "/nix/store/22222222222222222222222222222222-runtime",
                ManagedGroup::BuildUsers,
                0o444,
                runtime.len() as u64,
                body_digest(runtime),
            )?,
            ManagedArtifact::directory(
                "/nix/store/33333333333333333333333333333333-runtime-tree",
                ManagedGroup::BuildUsers,
                0o755,
            )?,
            ManagedArtifact::file(
                "/nix/store/33333333333333333333333333333333-runtime-tree/member",
                ManagedGroup::BuildUsers,
                0o444,
                runtime.len() as u64,
                body_digest(runtime),
            )?,
            ManagedArtifact::directory("/opt/pkg", ManagedGroup::BuildUsers, 0o755)?,
            ManagedArtifact::directory("/opt/pkg/nix", ManagedGroup::BuildUsers, 0o750)?,
            ManagedArtifact::directory("/opt/pkg/nix/2.34.8", ManagedGroup::BuildUsers, 0o750)?,
            ManagedArtifact::file(
                "/opt/pkg/nix/2.34.8/nix",
                ManagedGroup::BuildUsers,
                0o550,
                runtime.len() as u64,
                body_digest(runtime),
            )?,
        ];
        let manifest = encode_ownership_asset_manifest(System::Aarch64Linux, &version, &artifacts)?;
        let expectation = OwnershipExpectation::new(
            System::Aarch64Linux,
            version,
            body_digest(&manifest),
            ManagedGroupBindings::same_gid_for_test(fs::metadata(temporary.path())?.gid()),
            artifacts,
        )?;
        for artifact in expectation.artifacts() {
            let path = rooted(temporary.path(), artifact.path());
            if artifact.kind() == ManagedArtifactKind::Directory {
                fs::create_dir_all(&path)?;
            } else if artifact.kind() == ManagedArtifactKind::File {
                fs::write(&path, runtime)?;
            }
        }
        for artifact in expectation.artifacts().iter().rev() {
            let path = rooted(temporary.path(), artifact.path());
            if matches!(
                artifact.kind(),
                ManagedArtifactKind::Directory | ManagedArtifactKind::File
            ) {
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(artifact.mode().unwrap_or(0o400)),
                )?;
            }
        }
        let metadata_parent = rooted(temporary.path(), Path::new("/var/lib/pkg/managed-nix"));
        fs::create_dir_all(&metadata_parent)?;
        fs::set_permissions(&metadata_parent, fs::Permissions::from_mode(0o700))?;
        if write_receipt {
            let receipt = rooted(
                temporary.path(),
                ownership_receipt_path(System::Aarch64Linux),
            );
            write_private(&receipt, &encode_ownership_receipt(&expectation)?)?;
        }
        let manifest_path = rooted(temporary.path(), asset_manifest_path(System::Aarch64Linux));
        write_private(&manifest_path, &manifest)?;
        let state = rooted(temporary.path(), Path::new("/nix/var/nix"));
        fs::create_dir_all(&state)?;
        let gc_lock = rooted(temporary.path(), Path::new(GC_LOCK));
        write_private(&gc_lock, b"")?;
        if write_receipt {
            verify_with_owner_uid(temporary.path(), &expectation, owner_uid).map_err(|error| {
                format!(
                    "fixture ownership: {:?} at {:?}",
                    error.code(),
                    error.artifact_index()
                )
            })?;
        } else {
            verify_ownership_expectation(temporary.path(), &expectation, owner_uid).map_err(
                |error| {
                    format!(
                        "fixture artifacts: {:?} at {:?}",
                        error.code(),
                        error.artifact_index()
                    )
                },
            )?;
        }
        Ok(Self {
            temporary,
            expectation,
            owner_uid,
        })
    }

    fn prepare(&self) -> Result<ManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
        prepare_with_owner_uid(self.temporary.path(), &self.expectation, self.owner_uid)
    }

    fn prepare_without_receipt(
        &self,
    ) -> Result<Option<ManagedRuntimeRemoval>, ManagedRuntimeRemovalError> {
        prepare_without_receipt_with_owner_uid(
            self.temporary.path(),
            &self.expectation,
            self.owner_uid,
        )
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn collect_authenticated_store(fixture: &Fixture) -> std::io::Result<()> {
    fs::remove_file(rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    ))?;
    fs::remove_dir_all(rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/33333333333333333333333333333333-runtime-tree"),
    ))
}

fn write_dynamic_build_state(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    for path in [
        "/nix/var/log/nix/drvs/ab",
        "/nix/var/nix/builds",
        "/nix/var/nix/userpool",
        "/nix/var/nix/cgroups",
        "/nix/var/nix/db",
        "/nix/var/nix/profiles/per-user",
        "/nix/var/nix/temproots",
        "/nix/var/nix/gcroots/per-user",
        "/nix/var/nix/gcroots/pkg/users",
        "/nix/store/.links",
    ] {
        let path = rooted(fixture.temporary.path(), Path::new(path));
        fs::create_dir_all(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    for (path, contents) in [
        ("/nix/var/nix/db/db.sqlite", b"db".as_slice()),
        ("/nix/var/log/nix/drvs/ab/build.drv.bz2", b"log".as_slice()),
        ("/nix/var/nix/userpool/996", b"".as_slice()),
        ("/nix/var/nix/cgroups/996", b"".as_slice()),
    ] {
        fs::write(rooted(fixture.temporary.path(), Path::new(path)), contents)?;
    }
    Ok(())
}

#[test]
fn exact_runtime_and_registration_state_are_removed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    write_dynamic_build_state(&fixture)?;
    let removal = fixture.prepare()?;
    collect_authenticated_store(&fixture)?;
    assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
    assert!(!rooted(fixture.temporary.path(), Path::new("/nix/var/log")).exists());
    assert!(!rooted(fixture.temporary.path(), Path::new("/nix/var/nix/userpool")).exists());
    assert!(!rooted(fixture.temporary.path(), Path::new("/nix/var/nix/cgroups")).exists());
    assert!(!rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db")).exists());
    assert!(rooted(fixture.temporary.path(), Path::new("/nix/store")).is_dir());
    Ok(())
}

#[test]
fn receipt_free_prepare_captures_an_unreceipted_exact_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    // An interrupted install published no ownership receipt.
    assert!(
        fs::symlink_metadata(rooted(
            fixture.temporary.path(),
            ownership_receipt_path(System::Aarch64Linux),
        ))
        .is_err()
    );
    // The receipt-gated path refuses without a receipt ...
    assert!(fixture.prepare().is_err());
    // ... while the receipt-free path captures the exact authenticated state.
    assert!(fixture.prepare_without_receipt()?.is_some());
    Ok(())
}

#[test]
fn receipt_free_prepare_refuses_a_tampered_artifact() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    let tampered = rooted(
        fixture.temporary.path(),
        Path::new("/opt/pkg/nix/2.34.8/nix"),
    );
    fs::set_permissions(&tampered, fs::Permissions::from_mode(0o777))?;

    assert_eq!(
        fixture
            .prepare_without_receipt()
            .err()
            .map(ManagedRuntimeRemovalError::code),
        Some(ManagedRuntimeRemovalErrorCode::OwnershipRefused)
    );
    // Nothing was deleted.
    assert!(fs::symlink_metadata(&tampered).is_ok());
    Ok(())
}

#[test]
fn receipt_free_prepare_refuses_an_unexpected_runtime_entry()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    let foreign = rooted(fixture.temporary.path(), Path::new("/opt/pkg/nix/foreign"));
    fs::write(&foreign, b"foreign")?;

    assert_eq!(
        fixture
            .prepare_without_receipt()
            .err()
            .map(ManagedRuntimeRemovalError::code),
        Some(ManagedRuntimeRemovalErrorCode::UnsafeState)
    );
    assert!(fs::symlink_metadata(&foreign).is_ok());
    Ok(())
}

#[test]
fn receipt_free_prepare_succeeds_without_a_manifest() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    ))?;
    assert!(fixture.prepare_without_receipt()?.is_some());
    Ok(())
}

#[test]
fn receipt_free_prepare_noops_without_runtime_state() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    fs::remove_dir_all(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)))?;
    collect_authenticated_store(&fixture)?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    ))?;

    assert!(fixture.prepare_without_receipt()?.is_none());
    assert!(rooted(fixture.temporary.path(), Path::new("/nix/store")).is_dir());
    assert!(rooted(fixture.temporary.path(), Path::new("/opt/pkg")).is_dir());
    Ok(())
}

#[test]
fn receipt_free_prepare_noops_when_the_nix_tree_is_absent() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new_interrupted()?;
    fs::remove_dir_all(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)))?;
    collect_authenticated_store(&fixture)?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    ))?;
    fs::remove_dir_all(rooted(fixture.temporary.path(), Path::new("/nix")))?;

    assert!(fixture.prepare_without_receipt()?.is_none());
    assert!(rooted(fixture.temporary.path(), Path::new("/opt/pkg")).is_dir());
    Ok(())
}

#[test]
fn receipt_free_partial_runtime_is_removed_but_outer_roots_remain()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        Path::new("/opt/pkg/nix/2.34.8/nix"),
    ))?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    ))?;

    let removal = fixture
        .prepare_without_receipt()?
        .ok_or_else(|| std::io::Error::other("partial runtime was not captured"))?;
    assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(rooted(fixture.temporary.path(), Path::new("/nix/store")).is_dir());
    assert!(rooted(fixture.temporary.path(), Path::new("/nix/var/nix")).is_dir());
    assert!(rooted(fixture.temporary.path(), Path::new("/opt/pkg")).is_dir());
    Ok(())
}

#[test]
fn receipt_free_prepare_removes_an_exact_late_receipt() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let receipt = rooted(
        fixture.temporary.path(),
        ownership_receipt_path(System::Aarch64Linux),
    );

    let removal = fixture
        .prepare_without_receipt()?
        .ok_or_else(|| std::io::Error::other("late receipt was not captured"))?;
    assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(fs::symlink_metadata(receipt).is_err());
    Ok(())
}

#[test]
fn receipt_free_prepare_refuses_mismatched_metadata() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new_interrupted()?;
    let manifest = rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    );
    fs::write(&manifest, b"mismatched")?;
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600))?;

    assert_eq!(
        fixture
            .prepare_without_receipt()
            .err()
            .map(ManagedRuntimeRemovalError::code),
        Some(ManagedRuntimeRemovalErrorCode::UnsafeState)
    );
    assert!(fs::symlink_metadata(manifest).is_ok());
    Ok(())
}

#[test]
fn receipt_free_prepare_refuses_a_symlinked_metadata_parent()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new_interrupted()?;
    let managed = rooted(
        fixture.temporary.path(),
        Path::new("/var/lib/pkg/managed-nix"),
    );
    let manifest = rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    );
    let expected = fs::read(&manifest)?;
    fs::remove_file(&manifest)?;
    fs::remove_dir(&managed)?;
    let redirected = rooted(
        fixture.temporary.path(),
        Path::new("/var/lib/pkg/redirected"),
    );
    fs::create_dir(&redirected)?;
    fs::set_permissions(&redirected, fs::Permissions::from_mode(0o700))?;
    write_private(&redirected.join("assets-v1.json"), &expected)?;
    symlink(&redirected, &managed)?;

    assert_eq!(
        fixture
            .prepare_without_receipt()
            .err()
            .map(ManagedRuntimeRemovalError::code),
        Some(ManagedRuntimeRemovalErrorCode::UnsafeState)
    );
    Ok(())
}

#[test]
fn foreign_profile_refuses_gc_authorization_even_for_product_store_path()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new()?;
    let profiles = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/profiles/per-user/1000"),
    );
    fs::create_dir_all(&profiles)?;
    fs::set_permissions(&profiles, fs::Permissions::from_mode(0o700))?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );
    symlink(&store_object, profiles.join("profile"))?;

    let error = fixture
        .prepare()?
        .verify_no_foreign_liveness()
        .expect_err("a foreign profile must block product GC");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(store_object.is_file());
    Ok(())
}

#[test]
fn unknown_gc_root_refuses_before_any_store_deletion() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let foreign_root = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/gcroots/foreign"),
    );
    fs::create_dir_all(&foreign_root)?;
    fs::set_permissions(&foreign_root, fs::Permissions::from_mode(0o700))?;
    let removal = fixture.prepare()?;
    collect_authenticated_store(&fixture)?;

    let error = removal.remove().expect_err("unknown GC root must refuse");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(foreign_root.is_dir());
    Ok(())
}

#[test]
fn non_pid_temporary_root_record_refuses_before_any_store_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let temproots = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/temproots"),
    );
    fs::create_dir_all(&temproots)?;
    fs::set_permissions(&temproots, fs::Permissions::from_mode(0o700))?;
    fs::write(temproots.join("active-client"), b"signed store path")?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );
    let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

    let error = fixture
        .prepare()?
        .remove()
        .expect_err("a temporary-root record must refuse direct store deletion");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(store_object.is_file());
    assert!(runtime.is_dir());
    Ok(())
}

#[test]
fn stale_temporary_root_record_is_removed_with_the_store() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new()?;
    let temproots = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/temproots"),
    );
    fs::create_dir_all(&temproots)?;
    fs::set_permissions(&temproots, fs::Permissions::from_mode(0o700))?;
    fs::write(temproots.join("424242"), b"signed store path")?;

    assert_eq!(
        fixture.prepare()?.remove()?,
        ManagedRuntimeRemovalOutcome::Removed
    );
    assert!(!temproots.exists());
    Ok(())
}

#[test]
fn locked_temporary_root_record_refuses_before_any_store_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let temproots = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/temproots"),
    );
    fs::create_dir_all(&temproots)?;
    fs::set_permissions(&temproots, fs::Permissions::from_mode(0o700))?;
    let record = temproots.join("424242");
    fs::write(&record, b"signed store path")?;
    let ready = fixture.temporary.path().join("temporary-root-lock-ready");
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "managed::uninstall::tests::posix_record_lock_holder",
            "--nocapture",
        ])
        .env("PKG_TEST_RECORD_LOCK_PATH", &record)
        .env("PKG_TEST_RECORD_LOCK_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        let _ = child.kill();
        return Err("temporary-root lock helper did not become ready".into());
    }
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );

    let error = fixture
        .prepare()?
        .remove()
        .expect_err("a live temporary root must refuse direct store deletion");
    assert!(child.wait()?.success());
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(store_object.is_file());
    Ok(())
}

#[test]
fn user_profile_refuses_before_any_store_deletion() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let profile = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/profiles/per-user/alice"),
    );
    fs::create_dir_all(&profile)?;
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700))?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );
    let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

    let error = fixture
        .prepare()?
        .remove()
        .expect_err("a user profile must refuse direct store deletion");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(store_object.is_file());
    assert!(runtime.is_dir());
    Ok(())
}

#[test]
fn unexpected_runtime_entry_refuses_before_any_store_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let unexpected = rooted(
        fixture.temporary.path(),
        Path::new("/opt/pkg/nix/unexpected"),
    );
    fs::write(&unexpected, b"foreign")?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );

    let error = fixture
        .prepare()
        .expect_err("unsigned runtime residue must refuse during preparation");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(unexpected.is_file());
    assert!(store_object.is_file());
    Ok(())
}

#[test]
fn unexpected_store_tree_entry_refuses_before_any_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let unexpected = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/33333333333333333333333333333333-runtime-tree/unexpected"),
    );
    fs::write(&unexpected, b"foreign")?;
    let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

    let error = fixture
        .prepare()?
        .remove()
        .expect_err("unsigned store-tree residue must refuse");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(unexpected.is_file());
    assert!(runtime.is_dir());
    Ok(())
}

#[test]
fn complete_store_objects_collected_before_removal_are_accepted()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let removal = fixture.prepare()?;
    collect_authenticated_store(&fixture)?;

    assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
    Ok(())
}

#[test]
fn complete_store_objects_collected_before_preparation_are_accepted()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    collect_authenticated_store(&fixture)?;

    let removal = fixture.prepare()?;
    assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
    Ok(())
}

#[test]
fn partially_collected_store_tree_refuses_before_runtime_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let removal = fixture.prepare()?;
    fs::remove_file(rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/33333333333333333333333333333333-runtime-tree/member"),
    ))?;

    let error = removal
        .remove()
        .expect_err("a partially removed signed store tree must refuse");
    assert_eq!(
        error.code(),
        ManagedRuntimeRemovalErrorCode::IdentityChanged
    );
    assert!(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).is_dir());
    Ok(())
}

#[test]
fn ordinary_runtime_failure_still_removes_metadata_and_store_state()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    if fixture.owner_uid == 0 {
        // Root can unlink entries from a non-writable directory, so this
        // permission-based fault injection has no meaning in root CI.
        return Ok(());
    }
    let removal = fixture.prepare()?;
    collect_authenticated_store(&fixture)?;
    let runtime_version = rooted(fixture.temporary.path(), Path::new("/opt/pkg/nix/2.34.8"));
    fs::set_permissions(&runtime_version, fs::Permissions::from_mode(0o500))?;
    let receipt = rooted(
        fixture.temporary.path(),
        ownership_receipt_path(System::Aarch64Linux),
    );
    let manifest = rooted(
        fixture.temporary.path(),
        asset_manifest_path(System::Aarch64Linux),
    );

    let error = removal
        .remove()
        .expect_err("an undeletable runtime must report incomplete cleanup");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::RemovalFailed);
    assert!(!receipt.exists());
    assert!(!manifest.exists());
    assert_eq!(
        fs::read_dir(rooted(fixture.temporary.path(), Path::new(STORE_PREFIX)))?.count(),
        0
    );
    assert!(runtime_version.join("nix").is_file());
    Ok(())
}

#[test]
fn mounted_dynamic_root_device_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let dynamic = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
    fs::create_dir_all(&dynamic)?;
    fs::set_permissions(&dynamic, fs::Permissions::from_mode(0o700))?;
    let actual_device = fs::symlink_metadata(&dynamic)?.dev();
    let mut entries = Vec::new();

    let error = capture_tree_on_device(
        &dynamic,
        fixture.owner_uid,
        Some(actual_device.wrapping_add(1)),
        &mut entries,
    )
    .expect_err("a mounted state root must refuse");
    assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
    assert!(entries.is_empty());
    Ok(())
}

#[test]
fn gc_lock_waits_for_a_posix_record_lock() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let lock_path = rooted(fixture.temporary.path(), Path::new(GC_LOCK));
    let ready_path = fixture.temporary.path().join("record-lock-ready");
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "managed::uninstall::tests::posix_record_lock_holder",
            "--nocapture",
        ])
        .env("PKG_TEST_RECORD_LOCK_PATH", &lock_path)
        .env("PKG_TEST_RECORD_LOCK_READY", &ready_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !ready_path.exists() && Instant::now() < ready_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !ready_path.exists() {
        let _ = child.kill();
        return Err("record-lock helper did not become ready".into());
    }

    let started = Instant::now();
    let lock = acquire_gc_lock(&lock_path, fixture.owner_uid)?;
    let waited = started.elapsed();
    drop(lock);
    assert!(child.wait()?.success());
    assert!(waited >= Duration::from_millis(250));
    Ok(())
}

#[test]
#[ignore = "helper process for the POSIX record-lock interoperability test"]
fn posix_record_lock_holder() {
    let Ok(lock_path) = std::env::var("PKG_TEST_RECORD_LOCK_PATH") else {
        return;
    };
    let ready_path = std::env::var("PKG_TEST_RECORD_LOCK_READY").unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    fcntl_lock(&file, FlockOperation::LockExclusive).unwrap();
    fs::write(ready_path, b"ready").unwrap();
    thread::sleep(Duration::from_millis(500));
}

#[test]
fn remaining_store_object_preserves_all_nix_state() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let foreign = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/11111111111111111111111111111111-foreign"),
    );
    fs::write(&foreign, b"foreign")?;
    let db = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
    fs::create_dir_all(&db)?;
    fs::set_permissions(&db, fs::Permissions::from_mode(0o700))?;

    assert_eq!(
        fixture.prepare()?.remove()?,
        ManagedRuntimeRemovalOutcome::StorePreserved
    );
    assert!(foreign.is_file());
    assert!(db.is_dir());
    assert!(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).is_dir());
    Ok(())
}

#[test]
fn product_closure_inventory_accepts_only_exact_store_objects()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let product = StorePath::new("/nix/store/44444444444444444444444444444444-product")?;
    let product_path = rooted(fixture.temporary.path(), Path::new(product.as_str()));
    fs::write(&product_path, b"product")?;
    let removal = fixture.prepare()?;

    assert!(removal.store_contains_only_product_objects(std::slice::from_ref(&product))?);

    let foreign = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/11111111111111111111111111111111-foreign"),
    );
    fs::write(foreign, b"foreign")?;
    assert!(!removal.store_contains_only_product_objects(&[product])?);
    Ok(())
}

#[test]
fn exclusive_authority_captures_and_removes_product_closure_under_lock()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new()?;
    let product = StorePath::new("/nix/store/44444444444444444444444444444444-product")?;
    let source = StorePath::new("/nix/store/55555555555555555555555555555555-source")?;
    let product_path = rooted(fixture.temporary.path(), Path::new(product.as_str()));
    let source_path = rooted(fixture.temporary.path(), Path::new(source.as_str()));
    fs::write(&product_path, b"product")?;
    fs::create_dir(&source_path)?;
    fs::write(source_path.join("source"), b"source")?;
    symlink("source", source_path.join("source-link"))?;
    let gc_socket = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/gc-socket/socket"),
    );
    fs::create_dir_all(gc_socket.parent().ok_or("gc socket has no parent")?)?;
    drop(std::os::unix::net::UnixListener::bind(&gc_socket)?);
    fs::set_permissions(&gc_socket, fs::Permissions::from_mode(0o666))?;
    let users = rooted(
        fixture.temporary.path(),
        Path::new("/nix/var/nix/gcroots/pkg/users"),
    );
    fs::create_dir_all(&users)?;
    fs::set_permissions(&users, fs::Permissions::from_mode(0o700))?;
    let root = users.join("1000");
    symlink(product.as_str(), &root)?;
    let mut authority = fixture.prepare()?.begin_exclusive_removal()?;
    authority
        .capture_product_closure(std::slice::from_ref(&product), &[product.clone(), source])?;
    fs::remove_file(root)?;

    assert_eq!(authority.remove()?, ManagedRuntimeRemovalOutcome::Removed);
    assert!(!product_path.exists());
    assert!(!source_path.exists());
    assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
    assert!(!rooted(fixture.temporary.path(), Path::new(GC_SOCKET_DIR)).exists());
    Ok(())
}

#[test]
fn exclusive_authority_refuses_an_unregistered_store_object()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let product = StorePath::new("/nix/store/44444444444444444444444444444444-product")?;
    let foreign = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/55555555555555555555555555555555-foreign"),
    );
    fs::write(
        rooted(fixture.temporary.path(), Path::new(product.as_str())),
        b"product",
    )?;
    fs::write(&foreign, b"foreign")?;
    let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));
    let mut authority = fixture.prepare()?.begin_exclusive_removal()?;

    assert_eq!(
        authority
            .capture_product_closure(
                std::slice::from_ref(&product),
                std::slice::from_ref(&product),
            )
            .map_err(ManagedRuntimeRemovalError::code),
        Err(ManagedRuntimeRemovalErrorCode::UnsafeState)
    );
    assert!(foreign.is_file());
    assert!(runtime.is_dir());
    Ok(())
}

#[test]
fn exclusive_authority_creates_a_missing_gc_lock() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let lock = rooted(fixture.temporary.path(), Path::new(GC_LOCK));
    fs::remove_file(&lock)?;

    let authority = fixture.prepare()?.begin_exclusive_removal()?;

    assert!(lock.is_file());
    drop(authority);
    Ok(())
}

#[test]
fn preexisting_store_policy_never_mutates_nix() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );
    let state = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
    fs::create_dir_all(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    fs::write(state.join("preexisting"), b"keep")?;

    assert_eq!(
        fixture.prepare()?.remove_preserving_store()?,
        ManagedRuntimeRemovalOutcome::StorePreserved
    );
    assert!(store_object.is_file());
    assert_eq!(fs::read(state.join("preexisting"))?, b"keep");
    assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
    Ok(())
}

#[test]
fn changed_runtime_identity_is_never_deleted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let store_object = rooted(
        fixture.temporary.path(),
        Path::new("/nix/store/22222222222222222222222222222222-runtime"),
    );
    let db = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
    fs::create_dir_all(&db)?;
    fs::set_permissions(&db, fs::Permissions::from_mode(0o700))?;
    let removal = fixture.prepare()?;
    let binary = rooted(
        fixture.temporary.path(),
        Path::new("/opt/pkg/nix/2.34.8/nix"),
    );
    let replacement = fixture.temporary.path().join("replacement-runtime");
    fs::write(&replacement, b"replacement")?;
    fs::remove_file(&binary)?;
    fs::rename(&replacement, &binary)?;

    let error = removal.remove().expect_err("replacement must refuse");
    assert_eq!(
        error.code(),
        ManagedRuntimeRemovalErrorCode::IdentityChanged
    );
    assert_eq!(fs::read(binary)?, b"replacement");
    assert!(store_object.is_file());
    assert!(db.is_dir());
    Ok(())
}

#[test]
fn changed_local_manifest_refuses_during_preparation() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    fs::write(
        rooted(
            fixture.temporary.path(),
            asset_manifest_path(System::Aarch64Linux),
        ),
        b"{}",
    )?;

    assert_eq!(
        fixture.prepare().unwrap_err().code(),
        ManagedRuntimeRemovalErrorCode::UnsafeState
    );
    assert!(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).is_dir());
    Ok(())
}
