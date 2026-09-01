//! Tests for the `ownership` module.

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_core::state::body_digest;

use super::*;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn manifest_digest(
    system: System,
    nix_version: &NixVersion,
    artifacts: &[ManagedArtifact],
) -> Digest {
    body_digest(&encode_ownership_asset_manifest(system, nix_version, artifacts).unwrap())
}

struct Fixture {
    root: PathBuf,
    owner_uid: u32,
    group_gid: u32,
    expectation: OwnershipExpectation,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pkg-ownership-{}-{serial}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let metadata = fs::metadata(&root).unwrap();
        let owner_uid = metadata.uid();
        let group_gid = metadata.gid();

        let file_path = root.join("opt/pkg/bin/nix");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, b"managed nix\n").unwrap();
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o555)).unwrap();
        let directory_path = root.join("nix/store");
        fs::create_dir_all(&directory_path).unwrap();
        fs::set_permissions(&directory_path, fs::Permissions::from_mode(0o1775)).unwrap();
        let store_object = directory_path.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix");
        fs::create_dir(&store_object).unwrap();
        let store_file = store_object.join("nix");
        fs::write(&store_file, b"store nix\n").unwrap();
        fs::set_permissions(&store_file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&store_object, fs::Permissions::from_mode(0o555)).unwrap();
        let link_path = root.join("opt/pkg/bin/nix-current");
        symlink("nix", &link_path).unwrap();

        let artifacts = vec![
            ManagedArtifact::file(
                "/opt/pkg/bin/nix",
                ManagedGroup::Broker,
                0o555,
                12,
                body_digest(b"managed nix\n"),
            )
            .unwrap(),
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap(),
            ManagedArtifact::directory(
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix",
                ManagedGroup::BuildUsers,
                0o555,
            )
            .unwrap(),
            ManagedArtifact::file(
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix/nix",
                ManagedGroup::BuildUsers,
                0o444,
                10,
                body_digest(b"store nix\n"),
            )
            .unwrap(),
            ManagedArtifact::symlink("/opt/pkg/bin/nix-current", ManagedGroup::Broker, "nix")
                .unwrap(),
        ];
        let nix_version = NixVersion::new("2.34.8").unwrap();
        let asset_manifest_digest =
            manifest_digest(System::Aarch64Darwin, &nix_version, &artifacts);
        let expectation = OwnershipExpectation::new(
            System::Aarch64Darwin,
            nix_version,
            asset_manifest_digest,
            ManagedGroupBindings {
                broker_gid: group_gid,
                build_users_gid: group_gid,
            },
            artifacts,
        )
        .unwrap();

        let receipt = rooted(&root, ownership_receipt_path(expectation.system));
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        fs::set_permissions(receipt.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&receipt, encode_ownership_receipt(&expectation).unwrap()).unwrap();
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();

        Self {
            root,
            owner_uid,
            group_gid,
            expectation,
        }
    }

    fn receipt(&self) -> PathBuf {
        rooted(&self.root, ownership_receipt_path(self.expectation.system))
    }

    fn verify(&self) -> Result<VerifiedOwnership, OwnershipError> {
        verify_with_owner_uid(&self.root, &self.expectation, self.owner_uid)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn valid_receipt_and_complete_artifact_set_verify() {
    let fixture = Fixture::new();
    let verified = fixture.verify().unwrap();
    assert_eq!(verified.system(), System::Aarch64Darwin);
    assert_eq!(verified.nix_version().as_str(), "2.34.8");
    assert_eq!(verified.artifact_count(), 5);
    assert!(
        verify_receipt_against_manifest_with_owner_uid(
            &fixture.root,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            fixture.expectation.asset_manifest_digest(),
            fixture.expectation.groups(),
            fixture.owner_uid,
        )
        .is_ok()
    );
}

#[test]
fn complete_store_object_collected_by_gc_still_verifies() {
    let fixture = Fixture::new();
    let store_object = fixture
        .root
        .join("nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix");
    fs::set_permissions(&store_object, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(store_object).unwrap();

    assert!(fixture.verify().is_ok());
}

#[test]
fn partially_collected_store_object_is_rejected() {
    let fixture = Fixture::new();
    let store_object = fixture
        .root
        .join("nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nix");
    fs::set_permissions(&store_object, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(store_object.join("nix")).unwrap();
    fs::set_permissions(&store_object, fs::Permissions::from_mode(0o555)).unwrap();

    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactMissing
    );
}

#[test]
fn signed_manifest_binds_roles_without_baking_in_host_gids() {
    let fixture = Fixture::new();
    let bytes = encode_ownership_asset_manifest(
        fixture.expectation.system(),
        fixture.expectation.nix_version(),
        fixture.expectation.artifacts(),
    )
    .unwrap();
    let first = decode_ownership_asset_manifest(
        &bytes,
        fixture.expectation.system(),
        fixture.expectation.nix_version(),
        body_digest(&bytes),
        ManagedGroupBindings::new(1001, 1002).unwrap(),
    )
    .unwrap();
    let second = decode_ownership_asset_manifest(
        &bytes,
        fixture.expectation.system(),
        fixture.expectation.nix_version(),
        body_digest(&bytes),
        ManagedGroupBindings::new(2001, 2002).unwrap(),
    )
    .unwrap();
    assert_eq!(first.artifacts(), second.artifacts());
    assert_ne!(first.groups(), second.groups());
    assert_eq!(
        first.asset_manifest_digest(),
        second.asset_manifest_digest()
    );
}

#[test]
fn asset_manifest_unknown_fields_and_digest_mismatch_fail_closed() {
    let fixture = Fixture::new();
    let bytes = encode_ownership_asset_manifest(
        fixture.expectation.system(),
        fixture.expectation.nix_version(),
        fixture.expectation.artifacts(),
    )
    .unwrap();
    assert_eq!(
        decode_ownership_asset_manifest(
            &bytes,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            body_digest(b"other"),
            fixture.expectation.groups(),
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ManifestMismatch
    );
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["unexpected"] = serde_json::json!(true);
    let malformed = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_ownership_asset_manifest(
            &malformed,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            body_digest(&malformed),
            fixture.expectation.groups(),
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ManifestMalformed
    );
}

#[test]
fn authenticated_manifest_digest_rejects_a_truncated_artifact_set() {
    let fixture = Fixture::new();
    let mut truncated = fixture.expectation.artifacts().to_vec();
    truncated.pop();
    let error = OwnershipExpectation::new(
        fixture.expectation.system(),
        fixture.expectation.nix_version().clone(),
        fixture.expectation.asset_manifest_digest(),
        fixture.expectation.groups(),
        truncated,
    )
    .unwrap_err();
    assert_eq!(error.code(), OwnershipErrorCode::ExpectationInvalid);
}

#[test]
fn receipt_is_not_self_authenticating() {
    let fixture = Fixture::new();
    let nix_version = NixVersion::new("2.34.9").unwrap();
    let artifacts = fixture.expectation.artifacts().to_vec();
    let asset_manifest_digest = manifest_digest(System::Aarch64Darwin, &nix_version, &artifacts);
    let other = OwnershipExpectation::new(
        System::Aarch64Darwin,
        nix_version,
        asset_manifest_digest,
        fixture.expectation.groups(),
        artifacts,
    )
    .unwrap();
    let error = verify_with_owner_uid(&fixture.root, &other, fixture.owner_uid).unwrap_err();
    assert_eq!(error.code(), OwnershipErrorCode::ReceiptMismatch);
}

#[test]
fn unsafe_receipt_mode_is_rejected() {
    let fixture = Fixture::new();
    fs::set_permissions(fixture.receipt(), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptUnsafe
    );
}

#[test]
fn oversized_or_symlinked_receipt_is_rejected() {
    let fixture = Fixture::new();
    let receipt = fixture.receipt();
    OpenOptions::new()
        .write(true)
        .open(&receipt)
        .unwrap()
        .set_len(MAX_RECEIPT_BYTES + 1)
        .unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptTooLarge
    );

    fs::remove_file(&receipt).unwrap();
    let target = fixture.root.join("receipt-target");
    fs::write(
        &target,
        encode_ownership_receipt(&fixture.expectation).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(target, receipt).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptUnsafe
    );
}

#[test]
fn malformed_or_extended_receipt_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.receipt();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["unexpected"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptMalformed
    );
}

#[test]
fn changed_file_bytes_are_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("opt/pkg/bin/nix");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&path, b"changed nix\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactDigestMismatch
    );
}

#[test]
fn changed_file_size_or_type_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("opt/pkg/bin/nix");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&path, b"different length\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactSizeMismatch
    );

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactTypeMismatch
    );
}

#[test]
fn changed_artifact_group_is_rejected_after_receipt_match() {
    let mut fixture = Fixture::new();
    fixture.expectation.groups.broker_gid = fixture.group_gid.wrapping_add(1);
    fs::write(
        fixture.receipt(),
        encode_ownership_receipt(&fixture.expectation).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactOwnerMismatch
    );
}

#[test]
fn store_parent_uses_the_store_expectations_group() {
    let mut fixture = Fixture::new();
    fixture.expectation.groups.build_users_gid = fixture.group_gid.wrapping_add(1);
    fs::write(
        fixture.receipt(),
        encode_ownership_receipt(&fixture.expectation).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactOwnerMismatch
    );
}

#[test]
fn changed_artifact_mode_is_rejected() {
    let fixture = Fixture::new();
    fs::set_permissions(
        fixture.root.join("opt/pkg/bin/nix"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactModeMismatch
    );
}

#[test]
fn changed_symlink_target_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("opt/pkg/bin/nix-current");
    fs::remove_file(&path).unwrap();
    symlink("elsewhere", path).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactTargetMismatch
    );
}

#[test]
fn missing_artifact_is_rejected() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("opt/pkg/bin/nix")).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ArtifactMissing
    );
}

#[test]
fn duplicate_and_out_of_scope_paths_are_rejected() {
    assert_eq!(
        ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o775)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
    assert_eq!(
        ManagedArtifact::directory("/opt/pkg/open", ManagedGroup::Root, 0o777)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
    assert_eq!(
        ManagedArtifact::directory("/opt/pkg/../foreign", ManagedGroup::Root, 0o755)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
    let artifact = ManagedArtifact::directory("/tmp/pkg", ManagedGroup::Root, 0o755).unwrap();
    assert_eq!(
        OwnershipExpectation::new(
            System::Aarch64Darwin,
            NixVersion::new("2.34.8").unwrap(),
            body_digest(b"manifest"),
            ManagedGroupBindings::new(20, 21).unwrap(),
            vec![artifact],
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ExpectationInvalid
    );

    let artifact =
        ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap();
    assert_eq!(
        OwnershipExpectation::new(
            System::Aarch64Darwin,
            NixVersion::new("2.34.8").unwrap(),
            body_digest(b"manifest"),
            ManagedGroupBindings::new(20, 21).unwrap(),
            vec![artifact.clone(), artifact],
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
}

#[test]
fn symlink_targets_cannot_escape_managed_prefixes_or_parent_other_assets() {
    assert_eq!(
        ManagedArtifact::symlink(
            "/opt/pkg/bin/escape",
            ManagedGroup::Broker,
            "../../../etc/passwd",
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
    assert_eq!(
        ManagedArtifact::symlink("/opt/pkg/bin/escape", ManagedGroup::Broker, "/etc/passwd",)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ExpectationInvalid
    );

    let store = ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap();
    let link =
        ManagedArtifact::symlink("/opt/pkg/runtime", ManagedGroup::Broker, "nix-2.24.10").unwrap();
    let nested = ManagedArtifact::file(
        "/opt/pkg/runtime/bin/nix",
        ManagedGroup::Broker,
        0o550,
        0,
        body_digest(&[]),
    )
    .unwrap();
    let version = NixVersion::new("2.24.10").unwrap();
    let artifacts = vec![store, link, nested];
    assert_eq!(
        OwnershipExpectation::new(
            System::X8664Linux,
            version,
            body_digest(b"invalid manifest"),
            ManagedGroupBindings::new(1001, 1002).unwrap(),
            artifacts,
        )
        .unwrap_err()
        .code(),
        OwnershipErrorCode::ExpectationInvalid
    );
}

#[test]
fn parent_symlink_escape_is_rejected() {
    let fixture = Fixture::new();
    let outside = fixture.root.with_extension("outside");
    fs::create_dir(&outside).unwrap();
    let escaped = fixture.root.join("opt/pkg/bin");
    fs::remove_file(escaped.join("nix-current")).unwrap();
    fs::remove_file(escaped.join("nix")).unwrap();
    fs::remove_dir(&escaped).unwrap();
    symlink(&outside, &escaped).unwrap();
    let error = fixture.verify().unwrap_err();
    assert_eq!(error.code(), OwnershipErrorCode::ArtifactUnsafe);
    fs::remove_file(&escaped).unwrap();
    fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn artifact_groups_are_bound_by_the_expectation() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.group_gid,
        fs::metadata(&fixture.root).unwrap().gid()
    );
    assert!(fixture.verify().is_ok());
}

#[test]
fn receipt_free_verifier_passes_without_a_receipt() {
    let fixture = Fixture::new();
    // Receipt-bound verification requires the receipt; remove it to prove
    // the new verifier needs none.
    fs::remove_file(fixture.receipt()).unwrap();
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptMissing
    );
    let verified =
        verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
            .unwrap();
    assert_eq!(verified.system(), fixture.expectation.system());
    assert_eq!(
        verified.artifact_count(),
        fixture.expectation.artifacts().len()
    );
}

#[test]
fn receipt_free_verifier_refuses_missing_and_tampered_artifacts() {
    let fixture = Fixture::new();

    // Missing artifact.
    let file = fixture.root.join("opt/pkg/bin/nix");
    fs::remove_file(&file).unwrap();
    assert_eq!(
        verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ArtifactMissing
    );

    // Tampered bytes restore the file but change its content at the
    // exact recorded size, exercising the digest check.
    fs::write(&file, b"tampered ni\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(
        verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
            .unwrap_err()
            .code(),
        OwnershipErrorCode::ArtifactDigestMismatch
    );

    // Exact bytes pass again. Loosen the recorded mode for the rewrite,
    // then restore 0o555 before verification.
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&file, b"managed nix\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(
        verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
            .is_ok()
    );
}

#[test]
fn forged_receipt_is_irrelevant_to_the_receipt_free_verifier() {
    let fixture = Fixture::new();
    // Forge a structurally valid receipt bound to a different digest.
    let mut value: serde_json::Value =
        serde_json::from_slice(&encode_ownership_receipt(&fixture.expectation).unwrap()).unwrap();
    value["assetManifestDigest"] = serde_json::json!(
        "sha256-ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
    );
    fs::write(fixture.receipt(), serde_json::to_vec(&value).unwrap()).unwrap();

    // Receipt-bound verification must reject the forgery.
    assert_eq!(
        fixture.verify().unwrap_err().code(),
        OwnershipErrorCode::ReceiptMismatch
    );
    assert!(
        verify_receipt_against_manifest_with_owner_uid(
            &fixture.root,
            fixture.expectation.system(),
            fixture.expectation.nix_version(),
            fixture.expectation.asset_manifest_digest(),
            fixture.expectation.groups(),
            fixture.owner_uid,
        )
        .is_err()
    );
    // The receipt-free verifier never inspects the receipt.
    assert!(
        verify_ownership_expectation(&fixture.root, &fixture.expectation, fixture.owner_uid)
            .is_ok()
    );
}
