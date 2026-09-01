//! Tests for the `assets` module.

use super::*;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{collections::BTreeSet, error::Error, fs, path::Path, process::Command};

#[test]
fn assets_are_unique_absolute_or_fixed_accounts_and_never_schedule_gc() {
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for asset in linux_install_assets() {
        assert!(ids.insert(asset.id()));
        if matches!(
            asset.kind(),
            LinuxAssetKind::Directory | LinuxAssetKind::File
        ) {
            assert!(paths.insert(asset.path_or_name()));
            assert!(asset.path_or_name().starts_with('/'));
            assert!(asset.mode().is_some());
            assert!(asset.owner().is_some());
            assert!(asset.group().is_some());
        }
    }
    let unit_text = LinuxSystemdAssets::all()
        .into_iter()
        .map(|(_, text)| text)
        .collect::<String>();
    assert!(!unit_text.contains(".timer"));
    assert!(!unit_text.to_ascii_lowercase().contains("auto-gc"));
}

#[test]
fn linux_cutover_keeps_vendor_nix_out_of_product_assets_and_services() {
    let assets = linux_product_install_assets()
        .map(LinuxInstallAsset::id)
        .collect::<BTreeSet<_>>();
    let mutations = linux_product_mutation_assets()
        .map(LinuxInstallAsset::id)
        .collect::<BTreeSet<_>>();
    assert!(assets.contains("nix-root"));
    assert!(!mutations.contains("nix-root"));
    for vendor_owned in [
        "build-group",
        "build-user-01",
        "nix-store",
        "nix-var",
        "nix-state",
        "daemon-socket-dir",
    ] {
        assert!(!assets.contains(vendor_owned));
    }
    assert!(linux_install_assets().iter().all(|asset| {
        !matches!(asset.id(), "daemon-socket-unit" | "daemon-service-unit")
            && !asset.path_or_name().contains("pkg-nix-daemon")
    }));
    let units = LinuxSystemdAssets::all();
    assert!(units.iter().all(|(name, _)| !name.contains("nix-daemon")));
    assert!(LinuxSystemdAssets::BROKER_SERVICE.contains("Requires=nix-daemon.socket"));
    assert!(!LinuxSystemdAssets::BROKER_SERVICE.contains("pkg-nix-daemon.socket"));
    let product_gcroots = linux_product_install_assets()
        .filter(|asset| is_linux_product_gcroots_asset(*asset))
        .collect::<Vec<_>>();
    assert_eq!(
        product_gcroots
            .iter()
            .map(|asset| (
                asset.id(),
                asset.path_or_name(),
                asset.mode(),
                asset.owner(),
                asset.group(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                "nix-gcroots",
                "/nix/var/nix/gcroots/pkg",
                Some(0o700),
                Some(LinuxAssetPrincipal::Root),
                Some(LinuxAssetPrincipal::Root),
            ),
            (
                "nix-gcroots-users",
                "/nix/var/nix/gcroots/pkg/users",
                Some(0o700),
                Some(LinuxAssetPrincipal::Root),
                Some(LinuxAssetPrincipal::Root),
            ),
        ]
    );
    assert!(
        linux_product_install_assets()
            .all(|asset| { asset.path_or_name() != "/nix/var/nix/gcroots" })
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one closed security-contract assertion table"
)]
fn socket_and_service_security_contract_is_exact() {
    assert!(LinuxSystemdAssets::HELPER_SOCKET.contains("SocketMode=0660"));
    assert!(LinuxSystemdAssets::HELPER_SOCKET.contains("/run/pkg-helper/"));
    assert!(LinuxSystemdAssets::TMPFILES.contains("d /run/pkg-helper 0750 root pkg-nix-broker -"));
    assert!(LinuxSystemdAssets::BROKER_SOCKET.contains("/run/pkg/broker.sock"));
    assert!(linux_install_assets().iter().any(|asset| {
        asset.path_or_name() == "/opt/pkg/bin/pkg-root-helper" && asset.mode() == Some(0o750)
    }));
    assert!(linux_install_assets().iter().any(|asset| {
        asset.path_or_name() == "/opt/pkg/bin/pkg-nix-broker" && asset.mode() == Some(0o750)
    }));
    assert!(linux_install_assets().iter().any(|asset| {
        asset.path_or_name() == "/var/lib/pkg/broker-home/channel"
            && asset.mode() == Some(0o700)
            && asset.owner() == Some(LinuxAssetPrincipal::Broker)
            && asset.group() == Some(LinuxAssetPrincipal::Broker)
    }));
    assert!(LinuxSystemdAssets::BROKER_SERVICE.contains("User=pkg-nix-broker"));
    assert!(
        LinuxSystemdAssets::BROKER_SERVICE.contains("Environment=HOME=/var/lib/pkg/broker-home")
    );
    assert!(
        LinuxSystemdAssets::BROKER_SERVICE
            .contains("Environment=TMPDIR=/var/lib/pkg/broker-home/tmp")
    );
    assert!(LinuxSystemdAssets::HELPER_SERVICE.contains("User=root"));
    let helper_protect_home = LinuxSystemdAssets::HELPER_SERVICE
        .lines()
        .filter(|line| line.starts_with("ProtectHome="))
        .collect::<Vec<_>>();
    let broker_protect_home = LinuxSystemdAssets::BROKER_SERVICE
        .lines()
        .filter(|line| line.starts_with("ProtectHome="))
        .collect::<Vec<_>>();
    assert_eq!(helper_protect_home, ["ProtectHome=read-only"]);
    assert!(!helper_protect_home.contains(&"ProtectHome=true"));
    assert_eq!(broker_protect_home, ["ProtectHome=true"]);
    assert!(!broker_protect_home.contains(&"ProtectHome=read-only"));
    let helper_write_paths = LinuxSystemdAssets::HELPER_SERVICE
        .lines()
        .filter_map(|line| line.strip_prefix("ReadWritePaths="))
        .collect::<Vec<_>>();
    assert_eq!(helper_write_paths.len(), 1);
    for path in helper_write_paths[0].split_ascii_whitespace() {
        let path = Path::new(path.trim_start_matches(['-', '+', '!']));
        for protected in ["/home", "/root", "/run/user"] {
            assert!(path != Path::new(protected) && !path.starts_with(protected));
        }
    }
    assert!(
        LinuxSystemdAssets::HELPER_SERVICE.contains("Environment=HOME=/var/lib/pkg/helper-home")
    );
    assert!(
        LinuxSystemdAssets::HELPER_SERVICE
            .contains("Environment=TMPDIR=/var/lib/pkg/helper-home/tmp")
    );
    assert!(
        !LinuxSystemdAssets::all()
            .into_iter()
            .any(|(_, text)| text.contains("MemoryMax=") || text.contains("CPUQuota="))
    );
    let daemon_dir = linux_install_assets()
        .iter()
        .find(|asset| asset.id() == "daemon-socket-dir");
    assert_eq!(
        daemon_dir.map(|asset| (asset.path_or_name(), asset.owner(), asset.group())),
        Some((
            "/nix/var/nix/daemon-socket",
            Some(LinuxAssetPrincipal::Root),
            Some(LinuxAssetPrincipal::Broker)
        ))
    );
    for (id, path) in [
        ("broker-home", "/var/lib/pkg/broker-home"),
        ("broker-tmp", "/var/lib/pkg/broker-home/tmp"),
        ("helper-home", "/var/lib/pkg/helper-home"),
        ("helper-tmp", "/var/lib/pkg/helper-home/tmp"),
        ("helper-log-dir", "/var/lib/pkg/log/helper"),
    ] {
        assert!(linux_install_assets().iter().any(|asset| {
            asset.id() == id && asset.path_or_name() == path && asset.mode() == Some(0o700)
        }));
    }
    for (id, path, mode) in [
        ("product-config-dir", "/opt/pkg/etc/pkg", 0o750),
        ("nix-config", "/opt/pkg/etc/pkg/nix.conf", 0o640),
    ] {
        assert!(linux_install_assets().iter().any(|asset| {
            asset.id() == id
                && asset.path_or_name() == path
                && asset.mode() == Some(mode)
                && asset.owner() == Some(LinuxAssetPrincipal::Root)
                && asset.group() == Some(LinuxAssetPrincipal::Broker)
        }));
    }
    let service_root = linux_install_assets()
        .iter()
        .find(|asset| asset.id() == "service-root");
    assert_eq!(
        service_root.map(|asset| (asset.mode(), asset.owner(), asset.group())),
        Some((
            Some(0o710),
            Some(LinuxAssetPrincipal::Root),
            Some(LinuxAssetPrincipal::Broker)
        ))
    );
}

#[test]
fn systemd_analyze_accepts_exact_units_when_requested() -> Result<(), Box<dyn Error>> {
    if std::env::var_os("PKG_VERIFY_SYSTEMD").is_none() {
        return Ok(());
    }
    let root = std::env::temp_dir().join(format!("pkg-systemd-units-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;
    let mut paths = Vec::new();
    for (name, contents) in LinuxSystemdAssets::all() {
        let path = root.join(name);
        fs::write(&path, contents)?;
        paths.push(path);
    }
    let status = Command::new("systemd-analyze")
        .arg("verify")
        .args(&paths)
        .status()?;
    let _ = fs::remove_dir_all(&root);
    assert!(status.success());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn tmpfiles_recreates_private_helper_parent_when_requested() -> Result<(), Box<dyn Error>> {
    if std::env::var_os("PKG_VERIFY_SYSTEMD").is_none() {
        return Ok(());
    }
    let root = std::env::temp_dir().join(format!("pkg-tmpfiles-root-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("etc"))?;
    fs::create_dir_all(root.join("usr/lib/tmpfiles.d"))?;
    fs::write(
        root.join("etc/passwd"),
        "root:x:0:0::/:/bin/false\npkg-nix-broker:x:1234:1234::/:/bin/false\n",
    )?;
    fs::write(
        root.join("etc/group"),
        "root:x:0:\npkg-nix-broker:x:1234:\n",
    )?;
    fs::write(
        root.join("usr/lib/tmpfiles.d/pkg.conf"),
        LinuxSystemdAssets::TMPFILES,
    )?;
    let status = Command::new("systemd-tmpfiles")
        .arg(format!("--root={}", root.display()))
        .arg("--create")
        .arg("pkg.conf")
        .status()?;
    assert!(status.success());
    let metadata = root.join("run/pkg-helper").metadata()?;
    assert_eq!(metadata.permissions().mode() & 0o777, 0o750);
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.gid(), 1234);
    let _ = fs::remove_dir_all(&root);
    Ok(())
}
