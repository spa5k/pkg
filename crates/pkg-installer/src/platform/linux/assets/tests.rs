//! Tests for the Linux platform asset manager.

use super::*;

use pkg_core::state::body_digest;
use std::os::unix::fs::PermissionsExt;

fn manifest(
    system: System,
    release: Digest,
    target: LinuxInstallAsset,
    target_state: RecordedAssetState,
    target_digest: Digest,
) -> Result<UninstallManifest, Box<dyn std::error::Error>> {
    let records = crate::assets::linux_product_install_assets()
        .map(|asset| {
            let record = RecordedAsset::new(
                asset.id(),
                if asset == target {
                    target_state
                } else {
                    RecordedAssetState::Created
                },
            )?;
            Ok::<_, crate::UninstallError>(
                if asset.kind() == crate::LinuxAssetKind::File && asset.id() != "uninstall-manifest"
                {
                    record.with_content_digest(if asset == target {
                        target_digest
                    } else {
                        body_digest(asset.id().as_bytes())
                    })
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UninstallManifest::new(system, release, records)?)
}

fn asset(id: &str) -> LinuxInstallAsset {
    linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.id() == id)
        .unwrap_or_else(|| unreachable!("test asset is in the closed set"))
}

fn restart_fixture_filesystem(
    fixture: &mut ExistingNonFilePreflightAssets,
) -> Result<(), Box<dyn std::error::Error>> {
    let payloads = fixture
        .manager
        .payloads
        .clone()
        .ok_or_else(|| std::io::Error::other("test payloads are absent"))?;
    let mut filesystem = LinuxFilesystemManager::for_existing_preflight_test(
        fixture.temporary.path().to_path_buf(),
        payloads,
    );
    filesystem.bind_config_bytes_for_test(b"test-nix-config");
    fixture.manager.filesystem = Some(filesystem);
    fixture.manager.installed_manifest = InstalledManifest::Unloaded;
    Ok(())
}

fn assert_exact_prior_receipt_recovered(
    fixture: &mut ExistingNonFilePreflightAssets,
    prior: &UninstallManifest,
    candidate: &UninstallManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    let receipt_asset = uninstall_manifest_asset()?;
    assert_eq!(
        fixture
            .manager
            .ensure_filesystem()?
            .existing_uninstall_manifest()?,
        Some(prior.clone())
    );
    assert_eq!(
        fixture.manager.load_installed_manifest()?,
        Some(prior.clone())
    );
    assert!(
        !fixture
            .manager
            .ensure_filesystem()?
            .replacement_backup_exists(receipt_asset)?
    );
    fixture
        .manager
        .ensure_filesystem()?
        .bind_uninstall_manifest(prior)?;
    assert!(
        fixture
            .manager
            .ensure_filesystem()?
            .bind_uninstall_manifest(candidate)
            .is_err()
    );
    Ok(())
}

#[test]
fn exact_release_classification_accepts_only_same_undrifted_assets()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let release = Digest::from_bytes([0xa1; 32]);
    let groups = ManagedGroupBindings::new(30_000, 30_001)?;

    let mut exact = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        groups, system, release, "none",
    )?;
    assert!(exact.manager.classify_exact_release()?);

    let mut different = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        groups, system, release, "none",
    )?;
    different.manager.receipt_binding = Some((system, Digest::from_bytes([0xa2; 32])));
    assert!(!different.manager.classify_exact_release()?);

    let mut drifted = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        groups, system, release, "none",
    )?;
    std::fs::write(
        drifted.temporary.path().join("usr/local/bin/pkg"),
        b"changed",
    )?;
    assert!(drifted.manager.classify_exact_release().is_err());
    Ok(())
}

#[test]
fn offline_upgrade_publishes_and_finalizes_the_candidate_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let prior = Digest::from_bytes([0xb1; 32]);
    let candidate = Digest::from_bytes([0xb2; 32]);
    let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        ManagedGroupBindings::new(30_000, 30_001)?,
        system,
        prior,
        "none",
    )?;
    fixture.manager.receipt_binding = Some((system, candidate));

    fixture.manager.preflight_existing_non_files()?;
    assert!(fixture.manager.publish_uninstall_manifest()?);
    let receipt_asset = uninstall_manifest_asset()?;
    fixture.manager.finalize_replacement_backups(|| Ok(()))?;

    let receipt = fixture
        .manager
        .ensure_filesystem()?
        .existing_uninstall_manifest()?
        .ok_or_else(|| std::io::Error::other("candidate receipt is absent"))?;
    assert_eq!(receipt.ownership_manifest_digest(), candidate);
    assert!(
        !fixture
            .manager
            .ensure_filesystem()?
            .replacement_backup_exists(receipt_asset)?
    );
    Ok(())
}

#[test]
fn failed_offline_upgrade_rollback_restores_the_exact_prior_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let prior = Digest::from_bytes([0xc1; 32]);
    let candidate = Digest::from_bytes([0xc2; 32]);
    let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        ManagedGroupBindings::new(30_000, 30_001)?,
        system,
        prior,
        "none",
    )?;
    fixture.manager.receipt_binding = Some((system, candidate));

    fixture.manager.preflight_existing_non_files()?;
    let prior_receipt = fixture
        .manager
        .load_installed_manifest()?
        .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
    assert!(fixture.manager.publish_uninstall_manifest()?);
    let receipt_asset = uninstall_manifest_asset()?;
    fixture.manager.rollback_asset(receipt_asset)?;

    let receipt = fixture
        .manager
        .ensure_filesystem()?
        .existing_uninstall_manifest()?
        .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
    assert_eq!(receipt.ownership_manifest_digest(), prior);
    assert!(
        !fixture
            .manager
            .ensure_filesystem()?
            .replacement_backup_exists(receipt_asset)?
    );
    fixture
        .manager
        .ensure_filesystem()?
        .bind_uninstall_manifest(&prior_receipt)?;
    assert_eq!(
        fixture.manager.load_installed_manifest()?,
        Some(prior_receipt)
    );
    Ok(())
}

#[test]
fn recovery_before_receipt_exchange_restores_the_prior_disk_cache_and_binding()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let prior_release = Digest::from_bytes([0xd1; 32]);
    let candidate_release = Digest::from_bytes([0xd2; 32]);
    let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        ManagedGroupBindings::new(30_000, 30_001)?,
        system,
        prior_release,
        "none",
    )?;
    fixture.manager.receipt_binding = Some((system, candidate_release));
    fixture.manager.preflight_existing_non_files()?;
    let prior = fixture
        .manager
        .load_installed_manifest()?
        .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
    let candidate = UninstallManifest::new(
        system,
        candidate_release,
        fixture.manager.current_records(Some(&prior))?,
    )?;
    let staging = fixture
        .temporary
        .path()
        .join("opt/pkg/uninstall/.pkg-install-rollback-uninstall-manifest");
    std::fs::write(&staging, crate::encode_uninstall_manifest(&candidate)?)?;
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))?;

    restart_fixture_filesystem(&mut fixture)?;
    fixture.manager.recover_asset(uninstall_manifest_asset()?)?;

    assert_exact_prior_receipt_recovered(&mut fixture, &prior, &candidate)
}

#[test]
fn recovery_after_receipt_exchange_restores_the_prior_disk_cache_and_binding()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let prior_release = Digest::from_bytes([0xe1; 32]);
    let candidate_release = Digest::from_bytes([0xe2; 32]);
    let mut fixture = LinuxPlatformAssetManager::for_existing_non_file_preflight_test(
        ManagedGroupBindings::new(30_000, 30_001)?,
        system,
        prior_release,
        "none",
    )?;
    fixture.manager.receipt_binding = Some((system, candidate_release));
    fixture.manager.preflight_existing_non_files()?;
    let prior = fixture
        .manager
        .load_installed_manifest()?
        .ok_or_else(|| std::io::Error::other("prior receipt is absent"))?;
    assert!(fixture.manager.publish_uninstall_manifest()?);
    let candidate = fixture
        .manager
        .load_installed_manifest()?
        .ok_or_else(|| std::io::Error::other("candidate receipt is absent"))?;

    restart_fixture_filesystem(&mut fixture)?;
    fixture.manager.recover_asset(uninstall_manifest_asset()?)?;

    assert_exact_prior_receipt_recovered(&mut fixture, &prior, &candidate)
}

fn manager_before_broker(
    groups: ManagedGroupBindings,
    payloads: LinuxReleasePayloads,
    reader: LinuxReceiptReader,
    fail_account_read: bool,
) -> (
    LinuxPlatformAssetManager,
    std::rc::Rc<std::cell::Cell<usize>>,
) {
    let (accounts, mutation_calls) =
        LinuxAccountManager::for_fresh_preflight_test(groups, usize::from(fail_account_read));
    (
        LinuxPlatformAssetManager {
            groups,
            accounts,
            filesystem: None,
            payloads: Some(payloads),
            config: None,
            authenticated_config_system_for_test: None,
            receipt_binding: None,
            intent: LinuxProductAssetIntent::InstallOrUpgrade,
            installed_manifest: InstalledManifest::Unloaded,
            states: BTreeMap::new(),
            pre_broker_receipt_reader: Some(reader),
        },
        mutation_calls,
    )
}

#[test]
fn fresh_receipt_preflight_does_not_require_the_broker_uid()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let groups = ManagedGroupBindings::new(30_000, 30_001)?;
    let payloads = LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
    let reader =
        LinuxReceiptReader::for_test(temporary.path().to_path_buf(), groups, payloads.clone());
    let (mut manager, mutation_calls) = manager_before_broker(groups, payloads, reader, false);

    assert!(manager.broker_uid().is_err());
    assert!(manager.ensure_asset(asset("broker-group"))?);
    assert_eq!(mutation_calls.get(), 1);
    assert!(matches!(
        manager.installed_manifest,
        InstalledManifest::Absent
    ));
    Ok(())
}

#[test]
fn account_read_error_is_not_consumed_by_receipt_discovery()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let groups = ManagedGroupBindings::new(30_000, 30_001)?;
    let payloads = LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
    let reader =
        LinuxReceiptReader::for_test(temporary.path().to_path_buf(), groups, payloads.clone());
    let (mut manager, mutation_calls) = manager_before_broker(groups, payloads, reader, true);

    assert!(manager.ensure_asset(asset("broker-group")).is_err());
    assert_eq!(mutation_calls.get(), 0);
    Ok(())
}

#[test]
fn unsafe_pre_broker_receipts_refuse_before_account_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let groups = ManagedGroupBindings::new(30_000, 30_001)?;
    let payloads = LinuxReleasePayloads::from_authenticated_bytes(b"helper", b"broker", b"pkg")?;
    let mut fixtures = Vec::new();

    let unsafe_mode = tempfile::tempdir()?;
    std::fs::set_permissions(unsafe_mode.path(), std::fs::Permissions::from_mode(0o777))?;
    fixtures.push((
        unsafe_mode,
        None,
        nix::unistd::Uid::effective().as_raw(),
        nix::unistd::Gid::effective().as_raw(),
    ));

    let symlinked = tempfile::tempdir()?;
    symlink("/", symlinked.path().join("opt"))?;
    fixtures.push((
        symlinked,
        None,
        nix::unistd::Uid::effective().as_raw(),
        nix::unistd::Gid::effective().as_raw(),
    ));

    let wrong_owner = tempfile::tempdir()?;
    fixtures.push((
        wrong_owner,
        None,
        nix::unistd::Uid::effective().as_raw().saturating_add(1),
        nix::unistd::Gid::effective().as_raw(),
    ));

    let noncanonical = tempfile::tempdir()?;
    std::fs::create_dir(noncanonical.path().join("opt"))?;
    let mut filesystem = LinuxFilesystemManager::for_existing_preflight_test(
        noncanonical.path().to_path_buf(),
        payloads.clone(),
    );
    for directory in crate::assets::linux_product_install_assets()
        .filter(|asset| asset.kind() == crate::LinuxAssetKind::Directory)
        .filter(|asset| asset.path_or_name().starts_with("/opt/pkg"))
    {
        filesystem.ensure_asset(directory)?;
    }
    std::fs::write(
        noncanonical.path().join("opt/pkg/uninstall/manifest.json"),
        b"not canonical json",
    )?;
    std::fs::set_permissions(
        noncanonical.path().join("opt/pkg/uninstall/manifest.json"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    fixtures.push((
        noncanonical,
        Some(filesystem),
        nix::unistd::Uid::effective().as_raw(),
        nix::unistd::Gid::effective().as_raw(),
    ));

    for (temporary, filesystem, root_uid, root_gid) in fixtures {
        drop(filesystem);
        let reader = LinuxReceiptReader::for_test_with_owner(
            temporary.path().to_path_buf(),
            (root_uid, root_gid),
            groups,
            payloads.clone(),
        );
        let (mut manager, mutation_calls) =
            manager_before_broker(groups, payloads.clone(), reader, false);
        assert!(manager.ensure_asset(asset("broker-group")).is_err());
        assert_eq!(mutation_calls.get(), 0);
    }
    Ok(())
}

#[test]
fn ordinary_upgrade_requires_different_release_and_prior_content_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let prior_release = Digest::from_bytes([1; 32]);
    let candidate_release = Digest::from_bytes([2; 32]);
    let prior_content = Digest::from_bytes([3; 32]);
    let target = asset("broker-service-unit");
    let receipt = manifest(
        system,
        prior_release,
        target,
        RecordedAssetState::Created,
        prior_content,
    )?;
    let mut manager = LinuxPlatformAssetManager::new(ManagedGroupBindings::new(100, 101)?);
    manager.bind_authenticated_release_identity(system, candidate_release)?;

    assert_eq!(
        manager.replacement_authority(target, &receipt)?,
        ReplacementAuthority::Upgrade {
            prior_digest: prior_content,
        }
    );

    manager.bind_authenticated_release_identity(system, candidate_release)?;
    let same_release = manifest(
        system,
        candidate_release,
        target,
        RecordedAssetState::Created,
        prior_content,
    )?;
    assert!(
        manager
            .replacement_authority(target, &same_release)
            .is_err()
    );
    Ok(())
}

#[test]
fn repair_requires_same_release_and_created_product_ownership()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let candidate_release = Digest::from_bytes([2; 32]);
    let target = asset("broker-service-unit");
    let mut manager = LinuxPlatformAssetManager::with_intent(
        ManagedGroupBindings::new(100, 101)?,
        LinuxProductAssetIntent::Repair,
    );
    manager.bind_authenticated_release_identity(system, candidate_release)?;
    let owned = manifest(
        system,
        candidate_release,
        target,
        RecordedAssetState::Created,
        Digest::from_bytes([3; 32]),
    )?;
    assert_eq!(
        manager.replacement_authority(target, &owned)?,
        ReplacementAuthority::RepairExisting
    );
    assert_eq!(
        manager.missing_file_replacement_authority(target, &owned)?,
        ReplacementAuthority::RepairMissing
    );

    let prior_release = manifest(
        system,
        Digest::from_bytes([1; 32]),
        target,
        RecordedAssetState::Created,
        Digest::from_bytes([3; 32]),
    )?;
    assert!(
        manager
            .replacement_authority(target, &prior_release)
            .is_err()
    );

    let preexisting = manifest(
        system,
        candidate_release,
        target,
        RecordedAssetState::PreExisting,
        Digest::from_bytes([3; 32]),
    )?;
    assert!(manager.replacement_authority(target, &preexisting).is_err());
    assert!(
        manager
            .missing_file_replacement_authority(target, &preexisting)
            .is_err()
    );
    Ok(())
}

#[test]
fn repair_requires_a_receipt_and_non_files_never_gain_implicit_ownership()
-> Result<(), Box<dyn std::error::Error>> {
    let system = System::X8664Linux;
    let release = Digest::from_bytes([0x41; 32]);
    let account = asset("broker-user");
    let directory = asset("nix-root");
    let preexisting_account = manifest(
        system,
        release,
        account,
        RecordedAssetState::PreExisting,
        Digest::from_bytes([0x42; 32]),
    )?;
    let preexisting_directory = manifest(
        system,
        release,
        directory,
        RecordedAssetState::PreExisting,
        Digest::from_bytes([0x43; 32]),
    )?;
    let created_account = manifest(
        system,
        release,
        account,
        RecordedAssetState::Created,
        Digest::from_bytes([0x44; 32]),
    )?;
    let ordinary = LinuxPlatformAssetManager::new(ManagedGroupBindings::new(100, 101)?);
    assert!(ordinary.non_file_requires_exact(account, Some(&preexisting_account))?);
    assert!(ordinary.non_file_requires_exact(directory, Some(&preexisting_directory))?);

    let mut repair = LinuxPlatformAssetManager::with_intent(
        ManagedGroupBindings::new(100, 101)?,
        LinuxProductAssetIntent::Repair,
    );
    repair.installed_manifest = InstalledManifest::Absent;
    assert!(repair.non_file_requires_exact(account, Some(&created_account))?);
    assert!(repair.non_file_requires_exact(account, None).is_err());
    assert!(repair.file_replacement(asset("broker-binary")).is_err());
    Ok(())
}
