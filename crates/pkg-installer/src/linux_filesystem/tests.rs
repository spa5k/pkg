//! Tests for the `linux_filesystem` module.

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;

use pkg_core::{System, state::Digest};
use tempfile::TempDir;

use super::*;
use crate::linux_install_assets;

struct Fixture {
    temporary: TempDir,
    manager: LinuxFilesystemManager,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        for path in [
            "opt",
            "var",
            "var/lib",
            "run",
            "usr",
            "usr/lib",
            "usr/lib/systemd",
            "usr/lib/systemd/system",
            "usr/lib/tmpfiles.d",
            "usr/local",
            "usr/local/bin",
            "etc",
            "etc/profile.d",
        ] {
            fs::create_dir(temporary.path().join(path))?;
        }
        let uid = nix::unistd::Uid::current().as_raw();
        let gid = nix::unistd::Gid::current().as_raw();
        let principals = PrincipalBindings {
            root_uid: uid,
            root_gid: gid,
            broker_uid: Some(uid),
            broker_gid: gid,
            build_users_gid: gid,
        };
        let payloads =
            LinuxReleasePayloads::from_authenticated_bytes(b"root-helper", b"broker", b"pkg-cli")?;
        Ok(Self {
            manager: LinuxFilesystemManager::with_root(
                temporary.path().to_path_buf(),
                principals,
                payloads,
            ),
            temporary,
        })
    }

    fn asset(id: &str) -> LinuxInstallAsset {
        linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == id)
            .unwrap_or_else(|| unreachable!("test asset is in the closed list"))
    }
}

fn failure_code<T>(
    result: &Result<T, LinuxFilesystemError>,
) -> Result<LinuxFilesystemErrorCode, Box<dyn Error>> {
    match result {
        Ok(_) => Err(std::io::Error::other("expected filesystem failure").into()),
        Err(error) => Ok(error.code()),
    }
}

#[test]
fn creates_verifies_and_rolls_back_nested_directories() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in [
        "nix-root",
        "nix-store",
        "nix-var",
        "nix-state",
        "daemon-socket-dir",
    ] {
        assert!(fixture.manager.ensure_asset(Fixture::asset(id))?);
    }
    let vendor_gcroots = fixture.temporary.path().join("nix/var/nix/gcroots");
    fs::create_dir(&vendor_gcroots)?;
    fs::set_permissions(&vendor_gcroots, fs::Permissions::from_mode(0o755))?;
    for id in ["nix-gcroots", "nix-gcroots-users"] {
        assert!(fixture.manager.ensure_asset(Fixture::asset(id))?);
    }
    assert_eq!(
        fs::metadata(fixture.temporary.path().join("nix/store"))?
            .permissions()
            .mode()
            & 0o7777,
        0o1775
    );
    assert!(!fixture.manager.ensure_asset(Fixture::asset("nix-store"))?);
    for id in ["nix-gcroots-users", "nix-gcroots"] {
        fixture.manager.rollback_asset(Fixture::asset(id))?;
    }
    assert!(vendor_gcroots.is_dir());
    fs::remove_dir(&vendor_gcroots)?;
    for id in [
        "daemon-socket-dir",
        "nix-state",
        "nix-var",
        "nix-store",
        "nix-root",
    ] {
        fixture.manager.rollback_asset(Fixture::asset(id))?;
    }
    assert!(!fixture.temporary.path().join("nix").exists());
    Ok(())
}

#[test]
fn privileged_writes_refuse_unsafe_external_and_managed_ancestors() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fs::set_permissions(
        fixture.temporary.path().join("usr/lib/systemd"),
        fs::Permissions::from_mode(0o777),
    )?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .ensure_asset(Fixture::asset("broker-service-unit"))
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );

    fs::set_permissions(
        fixture.temporary.path().join("usr/lib/systemd"),
        fs::Permissions::from_mode(0o755),
    )?;
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("product-root"))?
    );
    fs::set_permissions(
        fixture.temporary.path().join("opt/pkg"),
        fs::Permissions::from_mode(0o777),
    )?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .ensure_asset(Fixture::asset("service-bin-dir"))
        )?,
        LinuxFilesystemErrorCode::Conflict
    );

    let mut wrong_owner = Fixture::new()?;
    wrong_owner.manager.principals.root_uid = wrong_owner
        .manager
        .principals
        .root_uid
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("uid overflow"))?;
    assert_eq!(
        failure_code(
            &wrong_owner
                .manager
                .ensure_asset(Fixture::asset("product-root"))
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    Ok(())
}

#[test]
fn external_ancestor_group_is_safe_without_write_bits() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fixture.manager.principals.root_gid = fixture
        .manager
        .principals
        .root_gid
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("gid overflow"))?;
    let asset = Fixture::asset("product-root");

    assert!(fixture.manager.verify_asset_absent(asset).is_ok());

    for mode in [0o775, 0o757] {
        fs::set_permissions(
            fixture.temporary.path().join("opt"),
            fs::Permissions::from_mode(mode),
        )?;
        assert_eq!(
            failure_code(&fixture.manager.verify_asset_absent(asset))?,
            LinuxFilesystemErrorCode::UnsafeFilesystemState
        );
    }

    fs::set_permissions(
        fixture.temporary.path().join("opt"),
        fs::Permissions::from_mode(0o755),
    )?;
    fixture.manager.principals.root_uid = fixture
        .manager
        .principals
        .root_uid
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("uid overflow"))?;
    assert_eq!(
        failure_code(&fixture.manager.verify_asset_absent(asset))?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    Ok(())
}

#[test]
fn macos_managed_ancestors_require_exact_metadata() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fixture.manager.managed_assets = ManagedAssetSet::MacOs;
    let directory = fixture.temporary.path().join("managed");
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o711))?;
    let handle = fs::File::open(&directory)?;

    assert!(
        fixture
            .manager
            .verify_ancestor(Path::new("/Library/Application Support/pkg"), &handle)
            .is_ok()
    );
    fixture.manager.principals.broker_gid = fixture
        .manager
        .principals
        .broker_gid
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("gid overflow"))?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .verify_ancestor(Path::new("/Library/Application Support/pkg"), &handle)
        )?,
        LinuxFilesystemErrorCode::Conflict
    );

    fixture.manager.principals.broker_gid = nix::unistd::Gid::current().as_raw();
    fixture.manager.principals.root_uid = fixture
        .manager
        .principals
        .root_uid
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("uid overflow"))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    assert!(
        fixture
            .manager
            .verify_ancestor(
                Path::new("/Library/Application Support/pkg/broker-home"),
                &handle,
            )
            .is_ok()
    );
    Ok(())
}

#[test]
fn installs_exact_release_static_and_authenticated_bytes() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in [
        "product-root",
        "product-config-root",
        "product-config-dir",
        "uninstall-root",
        "service-bin-dir",
    ] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    fixture
        .manager
        .bind_config_bytes(System::X8664Linux, b"sandbox = true\n")?;
    assert!(fixture.manager.ensure_asset(Fixture::asset("nix-config"))?);
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("root-helper-binary"))?
    );
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("broker-binary"))?
    );
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("product-cli"))?
    );
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("profile-snippet"))?
    );
    let profile = fs::read_to_string(fixture.temporary.path().join("etc/profile.d/pkg.sh"))?;
    assert!(profile.contains("__pkg_state=\"$HOME/.local/share/pkg\""));
    assert!(!profile.contains("XDG_DATA_HOME"));
    assert!(fixture.manager.install_static_asset(
        Fixture::asset("broker-service-unit"),
        LinuxSystemdAssets::BROKER_SERVICE,
    )?);
    fixture
        .manager
        .verify_asset(Fixture::asset("broker-service-unit"))?;
    let records = crate::assets::linux_product_install_assets()
        .map(|asset| {
            let record = crate::RecordedAsset::new(asset.id(), crate::RecordedAssetState::Created)?;
            Ok::<_, crate::UninstallError>(
                if asset.kind() == LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                    record.with_content_digest(body_digest(asset.id().as_bytes()))
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let manifest =
        UninstallManifest::new(System::X8664Linux, Digest::from_bytes([9; 32]), records)?;
    fixture.manager.bind_uninstall_manifest(&manifest)?;
    assert!(
        fixture
            .manager
            .ensure_asset(Fixture::asset("uninstall-manifest"))?
    );
    assert_eq!(
        crate::decode_uninstall_manifest(&fs::read(
            fixture
                .temporary
                .path()
                .join("opt/pkg/uninstall/manifest.json"),
        )?)?,
        manifest
    );
    assert_eq!(
        fs::read(fixture.temporary.path().join("opt/pkg/etc/pkg/nix.conf"))?,
        b"sandbox = true\n"
    );
    assert_eq!(
        fs::read(fixture.temporary.path().join("opt/pkg/bin/pkg-root-helper"))?,
        b"root-helper"
    );
    assert!(!fixture.manager.ensure_asset(Fixture::asset("nix-config"))?);
    Ok(())
}

#[test]
fn receipt_transition_requires_the_exact_bound_prior() -> Result<(), Box<dyn Error>> {
    let receipt = |release| -> Result<UninstallManifest, Box<dyn Error>> {
        let records = crate::assets::linux_product_install_assets()
            .map(|asset| {
                let record =
                    crate::RecordedAsset::new(asset.id(), crate::RecordedAssetState::Created)?;
                Ok::<_, crate::UninstallError>(
                    if asset.kind() == LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                        record.with_content_digest(body_digest(asset.id().as_bytes()))
                    } else {
                        record
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UninstallManifest::new(
            System::X8664Linux,
            release,
            records,
        )?)
    };
    let prior = receipt(Digest::from_bytes([1; 32]))?;
    let candidate = receipt(Digest::from_bytes([2; 32]))?;
    let wrong_prior = receipt(Digest::from_bytes([3; 32]))?;
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("uninstall-manifest");
    for id in ["product-root", "uninstall-root"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }

    fixture.manager.bind_uninstall_manifest(&prior)?;
    fixture.manager.bind_uninstall_manifest(&prior)?;
    assert!(fixture.manager.ensure_asset(asset)?);
    assert_eq!(
        failure_code(&fixture.manager.bind_uninstall_manifest(&candidate))?,
        LinuxFilesystemErrorCode::Conflict
    );
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .replace_uninstall_manifest(asset, &wrong_prior, &candidate),
        )?,
        LinuxFilesystemErrorCode::Conflict
    );

    let path = fixture
        .temporary
        .path()
        .join("opt/pkg/uninstall/manifest.json");
    fs::write(&path, crate::encode_uninstall_manifest(&wrong_prior)?)?;
    assert!(
        fixture
            .manager
            .replace_uninstall_manifest(asset, &prior, &candidate)
            .is_err()
    );
    assert_eq!(
        crate::decode_uninstall_manifest(&fs::read(&path)?)?,
        wrong_prior
    );
    assert!(!fixture.manager.replacement_backup_exists(asset)?);

    fs::write(&path, crate::encode_uninstall_manifest(&prior)?)?;
    fixture.manager.bind_uninstall_manifest(&prior)?;
    assert!(
        fixture
            .manager
            .replace_uninstall_manifest(asset, &prior, &candidate)?
    );
    assert_eq!(
        fixture.manager.existing_uninstall_manifest()?,
        Some(candidate.clone())
    );
    fixture.manager.bind_uninstall_manifest(&candidate)?;
    assert_eq!(
        failure_code(&fixture.manager.bind_uninstall_manifest(&prior))?,
        LinuxFilesystemErrorCode::Conflict
    );

    fixture
        .manager
        .rollback_uninstall_manifest_replacement(asset)?;
    assert_eq!(
        fixture.manager.existing_uninstall_manifest()?,
        Some(prior.clone())
    );
    fixture.manager.bind_uninstall_manifest(&prior)?;
    assert_eq!(
        failure_code(&fixture.manager.bind_uninstall_manifest(&candidate))?,
        LinuxFilesystemErrorCode::Conflict
    );
    Ok(())
}

#[test]
fn conflicts_and_symlinked_ancestors_fail_closed() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    fs::write(fixture.temporary.path().join("opt/pkg"), b"foreign")?;
    assert_eq!(
        failure_code(&fixture.manager.ensure_asset(Fixture::asset("product-root")),)?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    fs::remove_file(fixture.temporary.path().join("opt/pkg"))?;
    symlink("/tmp", fixture.temporary.path().join("opt/pkg"))?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .ensure_asset(Fixture::asset("product-config-root")),
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    Ok(())
}

#[test]
fn rollback_refuses_replaced_attempt_owned_file() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "service-bin-dir"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let asset = Fixture::asset("product-cli");
    assert!(fixture.manager.ensure_asset(asset)?);
    let path = fixture.temporary.path().join("usr/local/bin/pkg");
    let replacement = fixture.temporary.path().join("usr/local/bin/pkg.foreign");
    fs::write(&replacement, b"foreign")?;
    fs::remove_file(&path)?;
    fs::rename(replacement, &path)?;
    assert_eq!(
        failure_code(&fixture.manager.rollback_asset(asset))?,
        LinuxFilesystemErrorCode::RollbackConflict
    );
    assert_eq!(fs::read(path)?, b"foreign");
    Ok(())
}

#[test]
fn upgrade_replaces_only_exact_prior_owned_bytes_and_rolls_back() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let path = fixture
        .temporary
        .path()
        .join("usr/lib/systemd/system/pkg-nix-broker.service");
    let prior = b"prior authenticated unit\n";
    fs::write(&path, prior)?;
    let prior_digest = body_digest(prior);

    assert!(fixture.manager.replace_static_owned_file(
        asset,
        LinuxSystemdAssets::BROKER_SERVICE,
        Some(prior_digest),
        false,
    )?);
    assert_eq!(
        fs::read(&path)?,
        LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
    );

    fixture.manager.rollback_asset(asset)?;
    assert_eq!(fs::read(&path)?, prior);

    fs::write(&path, b"locally changed unit\n")?;
    assert_eq!(
        failure_code(&fixture.manager.replace_static_owned_file(
            asset,
            LinuxSystemdAssets::BROKER_SERVICE,
            Some(prior_digest),
            false,
        ))?,
        LinuxFilesystemErrorCode::Conflict
    );
    assert_eq!(fs::read(path)?, b"locally changed unit\n");
    Ok(())
}

#[test]
fn explicit_repair_restores_candidate_bytes_and_rolls_back_on_later_failure()
-> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let path = fixture
        .temporary
        .path()
        .join("usr/lib/systemd/system/pkg-nix-broker.service");
    let modified = b"locally changed unit\n";
    fs::write(&path, modified)?;

    assert!(fixture.manager.replace_static_owned_file(
        asset,
        LinuxSystemdAssets::BROKER_SERVICE,
        None,
        true,
    )?);
    assert_eq!(
        fs::read(&path)?,
        LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
    );

    fixture.manager.rollback_asset(asset)?;
    assert_eq!(fs::read(path)?, modified);
    Ok(())
}

#[test]
fn interrupted_upgrade_restores_prior_bytes_from_the_fixed_backup() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let path = fixture
        .temporary
        .path()
        .join("usr/lib/systemd/system/pkg-nix-broker.service");
    let prior = b"prior authenticated unit\n";
    fs::write(&path, prior)?;
    let prior_digest = body_digest(prior);
    fixture.manager.replace_static_owned_file(
        asset,
        LinuxSystemdAssets::BROKER_SERVICE,
        Some(prior_digest),
        false,
    )?;
    fixture.manager.attempt_owned.clear();

    fixture.manager.recover_owned_file(asset, prior_digest)?;

    assert_eq!(fs::read(&path)?, prior);
    assert!(!fixture.manager.replacement_backup_exists(asset)?);
    Ok(())
}

#[test]
fn interrupted_upgrade_before_exchange_discards_only_the_candidate_backup()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let parent = fixture.temporary.path().join("usr/lib/systemd/system");
    let path = parent.join("pkg-nix-broker.service");
    let backup = parent.join(rollback_name(asset));
    let prior = b"prior authenticated unit\n";
    fs::write(&path, prior)?;
    fs::write(&backup, LinuxSystemdAssets::BROKER_SERVICE)?;

    fixture
        .manager
        .recover_owned_file(asset, body_digest(prior))?;

    assert_eq!(fs::read(&path)?, prior);
    assert!(!backup.exists());

    fs::write(&backup, b"partial candidate")?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    fixture
        .manager
        .recover_owned_file(asset, body_digest(prior))?;
    assert_eq!(fs::read(path)?, prior);
    assert!(!backup.exists());
    Ok(())
}

#[test]
fn partial_upgrade_staging_refuses_when_live_prior_identity_is_wrong() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let parent = fixture.temporary.path().join("usr/lib/systemd/system");
    let path = parent.join("pkg-nix-broker.service");
    let staging = parent.join(rollback_name(asset));
    let authenticated_prior = b"authenticated prior unit\n";
    fs::write(&path, b"unknown live bytes\n")?;
    fs::write(&staging, b"partial candidate")?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;

    assert_eq!(
        failure_code(
            &fixture
                .manager
                .recover_owned_file(asset, body_digest(authenticated_prior)),
        )?,
        LinuxFilesystemErrorCode::Conflict
    );
    assert_eq!(fs::read(path)?, b"unknown live bytes\n");
    assert_eq!(fs::read(staging)?, b"partial candidate");
    Ok(())
}

#[test]
fn deterministic_initial_file_staging_recovers_before_and_after_rename()
-> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("product-cli");
    let parent = fixture.temporary.path().join("usr/local/bin");
    let target = parent.join("pkg");
    let staging = parent.join(rollback_name(asset));

    fs::write(&staging, b"partial candidate")?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    assert!(fixture.manager.ensure_asset(asset).is_err());
    assert_eq!(fs::read(&staging)?, b"partial candidate");
    fixture.manager.recover_created_file(asset)?;
    assert!(!staging.exists());
    assert!(!target.exists());

    assert!(fixture.manager.ensure_asset(asset)?);
    fixture.manager.attempt_owned.clear();
    fixture.manager.recover_created_file(asset)?;
    assert!(!staging.exists());
    assert!(!target.exists());
    Ok(())
}

#[test]
fn deterministic_initial_receipt_staging_recovers_without_residue() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "uninstall-root"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let asset = Fixture::asset("uninstall-manifest");
    let records = crate::assets::linux_product_install_assets()
        .map(|asset| {
            let record = crate::RecordedAsset::new(asset.id(), crate::RecordedAssetState::Created)?;
            Ok::<_, crate::UninstallError>(
                if asset.kind() == LinuxAssetKind::File && asset.id() != "uninstall-manifest" {
                    record.with_content_digest(body_digest(asset.id().as_bytes()))
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let manifest =
        UninstallManifest::new(System::X8664Linux, Digest::from_bytes([0x91; 32]), records)?;
    let release = manifest.ownership_manifest_digest();
    let prior = UninstallManifest::new(
        System::X8664Linux,
        Digest::from_bytes([0x92; 32]),
        manifest.assets().to_vec(),
    )?;
    fixture.manager.bind_uninstall_manifest(&manifest)?;
    let parent = fixture.temporary.path().join("opt/pkg/uninstall");
    let staging = parent.join(rollback_name(asset));
    fs::write(&staging, b"partial receipt")?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;

    fixture
        .manager
        .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)?;

    assert!(!staging.exists());
    assert!(!parent.join("manifest.json").exists());

    fs::write(&staging, crate::encode_uninstall_manifest(&manifest)?)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    fixture
        .manager
        .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)?;
    assert!(!staging.exists());

    fs::write(&staging, crate::encode_uninstall_manifest(&prior)?)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    assert!(
        fixture
            .manager
            .recover_absent_uninstall_manifest_staging(System::X8664Linux, release)
            .is_err()
    );
    assert!(staging.exists());
    Ok(())
}

#[test]
fn repair_recovery_keeps_candidate_and_discards_unknown_prior_bytes() -> Result<(), Box<dyn Error>>
{
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let parent = fixture.temporary.path().join("usr/lib/systemd/system");
    let path = parent.join("pkg-nix-broker.service");
    let backup = parent.join(rollback_name(asset));
    fs::write(&path, LinuxSystemdAssets::BROKER_SERVICE)?;
    fs::write(&backup, b"unknown prior bytes")?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;

    fixture.manager.roll_forward_owned_file(asset)?;
    assert_eq!(
        fs::read(path)?,
        LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
    );
    assert!(!backup.exists());
    Ok(())
}

#[test]
fn repair_roll_forward_replaces_unknown_binaries_and_changed_or_missing_units()
-> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "service-bin-dir"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    for (id, relative, mode) in [
        ("root-helper-binary", "opt/pkg/bin/pkg-root-helper", 0o750),
        ("broker-binary", "opt/pkg/bin/pkg-nix-broker", 0o750),
        (
            "broker-service-unit",
            "usr/lib/systemd/system/pkg-nix-broker.service",
            0o644,
        ),
    ] {
        let asset = Fixture::asset(id);
        let path = fixture.temporary.path().join(relative);
        fs::write(&path, b"unknown damaged bytes")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        fixture.manager.roll_forward_owned_file(asset)?;
        fixture.manager.verify_asset(asset)?;
        assert!(!fixture.manager.replacement_backup_exists(asset)?);
    }

    let missing = Fixture::asset("helper-service-unit");
    fixture.manager.roll_forward_owned_file(missing)?;
    fixture.manager.verify_asset(missing)?;
    assert!(!fixture.manager.replacement_backup_exists(missing)?);
    Ok(())
}

#[test]
fn committed_repair_cleanup_is_exact_and_resumable() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    let asset = Fixture::asset("broker-service-unit");
    let path = fixture
        .temporary
        .path()
        .join("usr/lib/systemd/system/pkg-nix-broker.service");
    fs::write(&path, b"locally changed unit\n")?;
    fixture.manager.replace_static_owned_file(
        asset,
        LinuxSystemdAssets::BROKER_SERVICE,
        None,
        true,
    )?;
    fixture.manager.attempt_owned.clear();

    fixture.manager.finalize_owned_file(asset)?;
    fixture.manager.finalize_owned_file(asset)?;

    assert_eq!(
        fs::read(path)?,
        LinuxSystemdAssets::BROKER_SERVICE.as_bytes()
    );
    assert!(!fixture.manager.replacement_backup_exists(asset)?);
    Ok(())
}

#[test]
fn verified_uninstall_removes_exact_files_and_is_retry_safe() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "service-bin-dir"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let asset = Fixture::asset("product-cli");
    assert!(fixture.manager.ensure_asset(asset)?);
    let path = fixture.temporary.path().join("usr/local/bin/pkg");

    fixture.manager.remove_verified_asset(asset)?;
    fixture
        .manager
        .remove_verified_asset(Fixture::asset("service-bin-dir"))?;
    fixture.manager.remove_verified_asset(asset)?;

    assert!(!path.exists());
    Ok(())
}

#[test]
fn verified_uninstall_refuses_changed_files_and_nonempty_directories() -> Result<(), Box<dyn Error>>
{
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "service-bin-dir"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let file = Fixture::asset("product-cli");
    assert!(fixture.manager.ensure_asset(file)?);
    let file_path = fixture.temporary.path().join("usr/local/bin/pkg");
    fs::write(&file_path, b"foreign")?;
    assert_eq!(
        failure_code(&fixture.manager.remove_verified_asset(file))?,
        LinuxFilesystemErrorCode::Conflict
    );
    assert_eq!(fs::read(file_path)?, b"foreign");

    let directory = Fixture::asset("service-bin-dir");
    fs::write(
        fixture.temporary.path().join("opt/pkg/bin/foreign"),
        b"foreign",
    )?;
    assert_eq!(
        failure_code(&fixture.manager.remove_verified_asset(directory))?,
        LinuxFilesystemErrorCode::RollbackConflict
    );
    assert!(fixture.temporary.path().join("opt/pkg/bin").is_dir());
    Ok(())
}

#[test]
fn broker_channel_cleanup_removes_private_files_and_refuses_links() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in [
        "service-root",
        "broker-home",
        "broker-channel-state",
        "log-root",
        "broker-log-dir",
    ] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let channel = fixture
        .temporary
        .path()
        .join("var/lib/pkg/broker-home/channel");
    let metadata = channel.join("root.json");
    fs::write(&metadata, b"authenticated")?;
    fs::set_permissions(&metadata, fs::Permissions::from_mode(0o644))?;

    fixture
        .manager
        .remove_broker_channel_state(Fixture::asset("broker-channel-state"))?;
    assert!(!channel.exists());
    let cache = fixture
        .temporary
        .path()
        .join("var/lib/pkg/broker-home/.cache/nix");
    fs::create_dir_all(&cache)?;
    fs::write(cache.join("cache.sqlite"), b"cache")?;
    fixture
        .manager
        .remove_private_tree(Fixture::asset("broker-home"))?;
    assert!(
        !fixture
            .temporary
            .path()
            .join("var/lib/pkg/broker-home")
            .exists()
    );
    let audit_directory = fixture.temporary.path().join("var/lib/pkg/log/broker");
    let audit = audit_directory.join("approvals.ndjson");
    fs::write(&audit, b"approved\n")?;
    fs::set_permissions(&audit, fs::Permissions::from_mode(0o600))?;
    fixture
        .manager
        .remove_private_tree(Fixture::asset("broker-log-dir"))?;
    assert!(!audit_directory.exists());

    let helper_home = fixture.temporary.path().join("var/lib/pkg/helper-home");
    fixture
        .manager
        .ensure_asset(Fixture::asset("helper-home"))?;
    let helper_cache = helper_home.join(".cache/nix");
    fs::create_dir_all(&helper_cache)?;
    fs::write(helper_cache.join("binary-cache-v7.sqlite"), b"cache")?;
    fixture
        .manager
        .remove_private_tree(Fixture::asset("helper-home"))?;
    assert!(!helper_home.exists());

    let mut fixture = Fixture::new()?;
    for id in ["service-root", "broker-home", "broker-channel-state"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let channel = fixture
        .temporary
        .path()
        .join("var/lib/pkg/broker-home/channel");
    symlink("/tmp", channel.join("root.json"))?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .remove_broker_channel_state(Fixture::asset("broker-channel-state")),
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    assert!(channel.join("root.json").is_symlink());

    let mut fixture = Fixture::new()?;
    for id in ["service-root", "broker-home", "broker-channel-state"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let channel = fixture
        .temporary
        .path()
        .join("var/lib/pkg/broker-home/channel");
    let foreign = channel.join("foreign.json");
    fs::write(&foreign, b"{}")?;
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o644))?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .remove_broker_channel_state(Fixture::asset("broker-channel-state")),
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    assert_eq!(fs::read(foreign)?, b"{}");
    Ok(())
}

#[test]
fn runtime_cleanup_accepts_only_exact_socket_and_log_residue() -> Result<(), Box<dyn Error>> {
    let mut fixture = Fixture::new()?;
    for id in [
        "service-root",
        "helper-socket-dir",
        "broker-socket-dir",
        "log-root",
    ] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let helper_socket = fixture
        .temporary
        .path()
        .join("run/pkg-helper/root-helper.sock");
    let _helper = UnixListener::bind(&helper_socket)?;
    fs::set_permissions(&helper_socket, fs::Permissions::from_mode(0o660))?;
    let broker_socket = fixture.temporary.path().join("run/pkg/broker.sock");
    let _broker = UnixListener::bind(&broker_socket)?;
    fs::set_permissions(&broker_socket, fs::Permissions::from_mode(0o666))?;
    let log_root = fixture.temporary.path().join("var/lib/pkg/log");
    for name in ["nix-daemon.log", "store-volume.log"] {
        let path = log_root.join(name);
        fs::write(&path, b"runtime output")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .verify_empty_directory(Fixture::asset("log-root")),
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    for id in ["helper-socket-dir", "broker-socket-dir", "log-root"] {
        fixture.manager.remove_runtime_state(Fixture::asset(id))?;
    }
    assert!(!helper_socket.exists());
    assert!(!broker_socket.exists());
    assert!(!log_root.exists());

    let mut fixture = Fixture::new()?;
    for id in ["service-root", "log-root"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    let foreign = fixture.temporary.path().join("var/lib/pkg/log/foreign.log");
    fs::write(&foreign, b"preserve")?;
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600))?;
    assert_eq!(
        failure_code(
            &fixture
                .manager
                .remove_runtime_state(Fixture::asset("log-root")),
        )?,
        LinuxFilesystemErrorCode::UnsafeFilesystemState
    );
    assert_eq!(fs::read(foreign)?, b"preserve");
    Ok(())
}

#[test]
fn missing_or_conflicting_payloads_are_rejected() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        failure_code(&LinuxReleasePayloads::from_authenticated_bytes(
            b"", b"broker", b"pkg",
        ))?,
        LinuxFilesystemErrorCode::MissingPayload
    );
    let mut fixture = Fixture::new()?;
    for id in ["product-root", "product-config-root", "product-config-dir"] {
        fixture.manager.ensure_asset(Fixture::asset(id))?;
    }
    assert_eq!(
        failure_code(&fixture.manager.ensure_asset(Fixture::asset("nix-config")),)?,
        LinuxFilesystemErrorCode::MissingPayload
    );
    Ok(())
}
