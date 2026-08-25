//! Exact Linux service assets and authenticated-install allowlist.

/// A closed privileged-install artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxAssetKind {
    /// A directory created with the exact recorded mode.
    Directory,
    /// A regular file installed from authenticated release bytes.
    File,
    /// A system account created without an interactive login.
    User,
    /// A system group created for one fixed role.
    Group,
}

/// Fixed filesystem ownership roles resolved to host ids during installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxAssetPrincipal {
    /// The privileged root identity.
    Root,
    /// The unprivileged singleton broker identity.
    Broker,
    /// The isolated Nix build-user group.
    BuildUsers,
}

/// One static Linux install artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxInstallAsset {
    id: &'static str,
    kind: LinuxAssetKind,
    path_or_name: &'static str,
    mode: Option<u32>,
    owner: Option<LinuxAssetPrincipal>,
    group: Option<LinuxAssetPrincipal>,
}

impl LinuxInstallAsset {
    pub(crate) const fn platform_filesystem(
        id: &'static str,
        kind: LinuxAssetKind,
        path: &'static str,
        mode: u32,
        owner: LinuxAssetPrincipal,
        group: LinuxAssetPrincipal,
    ) -> Self {
        Self {
            id,
            kind,
            path_or_name: path,
            mode: Some(mode),
            owner: Some(owner),
            group: Some(group),
        }
    }

    const fn new(
        id: &'static str,
        kind: LinuxAssetKind,
        path_or_name: &'static str,
        mode: Option<u32>,
    ) -> Self {
        let filesystem = matches!(kind, LinuxAssetKind::Directory | LinuxAssetKind::File);
        Self {
            id,
            kind,
            path_or_name,
            mode,
            owner: if filesystem {
                Some(LinuxAssetPrincipal::Root)
            } else {
                None
            },
            group: if filesystem {
                Some(LinuxAssetPrincipal::Root)
            } else {
                None
            },
        }
    }

    const fn with_ownership(
        mut self,
        owner: LinuxAssetPrincipal,
        group: LinuxAssetPrincipal,
    ) -> Self {
        self.owner = Some(owner);
        self.group = Some(group);
        self
    }

    /// Returns the stable install artifact id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.id
    }

    /// Returns the closed artifact kind.
    #[must_use]
    pub const fn kind(self) -> LinuxAssetKind {
        self.kind
    }

    /// Returns an exact absolute path or fixed account/group name.
    #[must_use]
    pub const fn path_or_name(self) -> &'static str {
        self.path_or_name
    }

    /// Returns the exact filesystem mode when the artifact has one.
    #[must_use]
    pub const fn mode(self) -> Option<u32> {
        self.mode
    }

    /// Returns the fixed owner role for filesystem artifacts.
    #[must_use]
    pub const fn owner(self) -> Option<LinuxAssetPrincipal> {
        self.owner
    }

    /// Returns the fixed group role for filesystem artifacts.
    #[must_use]
    pub const fn group(self) -> Option<LinuxAssetPrincipal> {
        self.group
    }
}

const ASSETS: &[LinuxInstallAsset] = &[
    LinuxInstallAsset::new(
        "broker-group",
        LinuxAssetKind::Group,
        "pkg-nix-broker",
        None,
    ),
    LinuxInstallAsset::new("broker-user", LinuxAssetKind::User, "pkg-nix-broker", None),
    LinuxInstallAsset::new("build-group", LinuxAssetKind::Group, "nixbld", None),
    LinuxInstallAsset::new("build-user-01", LinuxAssetKind::User, "nixbld1", None),
    LinuxInstallAsset::new("build-user-02", LinuxAssetKind::User, "nixbld2", None),
    LinuxInstallAsset::new("build-user-03", LinuxAssetKind::User, "nixbld3", None),
    LinuxInstallAsset::new("build-user-04", LinuxAssetKind::User, "nixbld4", None),
    LinuxInstallAsset::new("build-user-05", LinuxAssetKind::User, "nixbld5", None),
    LinuxInstallAsset::new("build-user-06", LinuxAssetKind::User, "nixbld6", None),
    LinuxInstallAsset::new("build-user-07", LinuxAssetKind::User, "nixbld7", None),
    LinuxInstallAsset::new("build-user-08", LinuxAssetKind::User, "nixbld8", None),
    LinuxInstallAsset::new("build-user-09", LinuxAssetKind::User, "nixbld9", None),
    LinuxInstallAsset::new("build-user-10", LinuxAssetKind::User, "nixbld10", None),
    LinuxInstallAsset::new("build-user-11", LinuxAssetKind::User, "nixbld11", None),
    LinuxInstallAsset::new("build-user-12", LinuxAssetKind::User, "nixbld12", None),
    LinuxInstallAsset::new("build-user-13", LinuxAssetKind::User, "nixbld13", None),
    LinuxInstallAsset::new("build-user-14", LinuxAssetKind::User, "nixbld14", None),
    LinuxInstallAsset::new("build-user-15", LinuxAssetKind::User, "nixbld15", None),
    LinuxInstallAsset::new("build-user-16", LinuxAssetKind::User, "nixbld16", None),
    LinuxInstallAsset::new("nix-root", LinuxAssetKind::Directory, "/nix", Some(0o755)),
    LinuxInstallAsset::new(
        "nix-store",
        LinuxAssetKind::Directory,
        "/nix/store",
        Some(0o1775),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::BuildUsers),
    LinuxInstallAsset::new(
        "nix-var",
        LinuxAssetKind::Directory,
        "/nix/var",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "nix-state",
        LinuxAssetKind::Directory,
        "/nix/var/nix",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "nix-gcroots",
        LinuxAssetKind::Directory,
        "/nix/var/nix/gcroots",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "product-root",
        LinuxAssetKind::Directory,
        "/opt/pkg",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "product-config-root",
        LinuxAssetKind::Directory,
        "/opt/pkg/etc",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "product-config-dir",
        LinuxAssetKind::Directory,
        "/opt/pkg/etc/pkg",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "uninstall-root",
        LinuxAssetKind::Directory,
        "/opt/pkg/uninstall",
        Some(0o700),
    ),
    LinuxInstallAsset::new(
        "service-bin-dir",
        LinuxAssetKind::Directory,
        "/opt/pkg/bin",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "service-root",
        LinuxAssetKind::Directory,
        "/var/lib/pkg",
        Some(0o710),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "daemon-socket-dir",
        LinuxAssetKind::Directory,
        "/nix/var/nix/daemon-socket",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "helper-socket-dir",
        LinuxAssetKind::Directory,
        "/run/pkg-helper",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "broker-socket-dir",
        LinuxAssetKind::Directory,
        "/run/pkg",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "log-root",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/log",
        Some(0o710),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "broker-log-dir",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/log/broker",
        Some(0o700),
    )
    .with_ownership(LinuxAssetPrincipal::Broker, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "helper-log-dir",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/log/helper",
        Some(0o700),
    ),
    LinuxInstallAsset::new(
        "broker-home",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/broker-home",
        Some(0o700),
    )
    .with_ownership(LinuxAssetPrincipal::Broker, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "broker-channel-state",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/broker-home/channel",
        Some(0o700),
    )
    .with_ownership(LinuxAssetPrincipal::Broker, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "helper-home",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/helper-home",
        Some(0o700),
    ),
    LinuxInstallAsset::new(
        "helper-tmp",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/helper-home/tmp",
        Some(0o700),
    ),
    LinuxInstallAsset::new(
        "broker-tmp",
        LinuxAssetKind::Directory,
        "/var/lib/pkg/broker-home/tmp",
        Some(0o700),
    )
    .with_ownership(LinuxAssetPrincipal::Broker, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "root-helper-binary",
        LinuxAssetKind::File,
        "/opt/pkg/bin/pkg-root-helper",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "broker-binary",
        LinuxAssetKind::File,
        "/opt/pkg/bin/pkg-nix-broker",
        Some(0o750),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "nix-config",
        LinuxAssetKind::File,
        "/opt/pkg/etc/pkg/nix.conf",
        Some(0o640),
    )
    .with_ownership(LinuxAssetPrincipal::Root, LinuxAssetPrincipal::Broker),
    LinuxInstallAsset::new(
        "daemon-socket-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-nix-daemon.socket",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "daemon-service-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-nix-daemon.service",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "helper-socket-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-root-helper.socket",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "helper-service-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-root-helper.service",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "broker-socket-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-nix-broker.socket",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "broker-service-unit",
        LinuxAssetKind::File,
        "/usr/lib/systemd/system/pkg-nix-broker.service",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "runtime-tmpfiles",
        LinuxAssetKind::File,
        "/usr/lib/tmpfiles.d/pkg.conf",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "profile-snippet",
        LinuxAssetKind::File,
        "/etc/profile.d/pkg.sh",
        Some(0o644),
    ),
    LinuxInstallAsset::new(
        "product-cli",
        LinuxAssetKind::File,
        "/usr/local/bin/pkg",
        Some(0o755),
    ),
    LinuxInstallAsset::new(
        "uninstall-manifest",
        LinuxAssetKind::File,
        "/opt/pkg/uninstall/manifest.json",
        Some(0o600),
    ),
];

/// Returns the exact static privileged-install allowlist.
#[must_use]
pub const fn linux_install_assets() -> &'static [LinuxInstallAsset] {
    ASSETS
}

/// Exact systemd unit bytes installed by the Linux backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxSystemdAssets;

impl LinuxSystemdAssets {
    /// Recreates the private helper socket parent after `/run` is cleared.
    pub const TMPFILES: &'static str =
        "d /run/pkg-helper 0750 root pkg-nix-broker -\nd /run/pkg 0755 root root -\n";

    /// Root daemon socket, reachable only by the broker group.
    pub const DAEMON_SOCKET: &'static str = "[Unit]\nDescription=pkg managed Nix daemon socket\n\n[Socket]\nListenStream=/nix/var/nix/daemon-socket/socket\nSocketUser=root\nSocketGroup=pkg-nix-broker\nSocketMode=0660\nDirectoryMode=0750\nRemoveOnStop=true\n\n[Install]\nWantedBy=sockets.target\n";

    /// Root Nix daemon consuming only the managed configuration.
    pub const DAEMON_SERVICE: &'static str = "[Unit]\nDescription=pkg managed Nix daemon\nRequires=pkg-nix-daemon.socket\nAfter=pkg-nix-daemon.socket\nRequiresMountsFor=/nix/store /nix/var/nix\n\n[Service]\nExecStart=@/opt/pkg/nix/current/bin/nix-daemon nix-daemon --daemon\nEnvironment=NIX_CONF_DIR=/opt/pkg/etc/pkg\nEnvironment=NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket\nEnvironment=NIX_STATE_DIR=/nix/var/nix\nKillMode=process\nLimitNOFILE=1048576\nDelegate=yes\nUMask=0077\nPrivateTmp=true\nProtectHome=true\nProtectSystem=strict\nReadWritePaths=/nix /var/lib/pkg/log\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n";

    /// Broker-only privileged-helper socket; peer credentials remain mandatory.
    pub const HELPER_SOCKET: &'static str = "[Unit]\nDescription=pkg privileged root helper socket\n\n[Socket]\nListenStream=/run/pkg-helper/root-helper.sock\nSocketUser=root\nSocketGroup=pkg-nix-broker\nSocketMode=0660\nDirectoryMode=0750\nRemoveOnStop=true\n\n[Install]\nWantedBy=sockets.target\n";

    /// Narrow root helper; it has no shell and no public command grammar.
    pub const HELPER_SERVICE: &'static str = "[Unit]\nDescription=pkg privileged root helper\nRequires=pkg-root-helper.socket\nAfter=pkg-root-helper.socket\n\n[Service]\nType=simple\nExecStart=/opt/pkg/bin/pkg-root-helper\nUser=root\nGroup=root\nEnvironment=HOME=/var/lib/pkg/helper-home\nEnvironment=TMPDIR=/var/lib/pkg/helper-home/tmp\nWorkingDirectory=/var/lib/pkg/helper-home\nUMask=0077\nPrivateTmp=true\nProtectHome=true\nProtectSystem=strict\nReadWritePaths=/nix/var/nix/gcroots/pkg /nix/store /nix/var/nix /var/lib/pkg/log/helper /var/lib/pkg/helper-home\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n";

    /// End-user broker socket. The broker authenticates every uid with peer creds.
    pub const BROKER_SOCKET: &'static str = "[Unit]\nDescription=pkg broker socket\n\n[Socket]\nListenStream=/run/pkg/broker.sock\nSocketUser=root\nSocketGroup=root\nSocketMode=0666\nDirectoryMode=0755\nRemoveOnStop=true\n\n[Install]\nWantedBy=sockets.target\n";

    /// Singleton unprivileged broker service.
    pub const BROKER_SERVICE: &'static str = "[Unit]\nDescription=pkg package broker\nRequires=pkg-nix-daemon.socket pkg-root-helper.socket\nAfter=pkg-nix-daemon.socket pkg-root-helper.socket\n\n[Service]\nType=simple\nExecStart=/opt/pkg/bin/pkg-nix-broker\nUser=pkg-nix-broker\nGroup=pkg-nix-broker\nEnvironment=HOME=/var/lib/pkg/broker-home\nEnvironment=TMPDIR=/var/lib/pkg/broker-home/tmp\nWorkingDirectory=/var/lib/pkg/broker-home\nUMask=0077\nPrivateTmp=true\nProtectHome=true\nProtectSystem=strict\nReadWritePaths=/var/lib/pkg/log/broker /var/lib/pkg/broker-home\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n";

    /// Returns all unit names and exact text in deterministic order.
    #[must_use]
    pub const fn all() -> [(&'static str, &'static str); 6] {
        [
            ("pkg-nix-daemon.socket", Self::DAEMON_SOCKET),
            ("pkg-nix-daemon.service", Self::DAEMON_SERVICE),
            ("pkg-root-helper.socket", Self::HELPER_SOCKET),
            ("pkg-root-helper.service", Self::HELPER_SERVICE),
            ("pkg-nix-broker.socket", Self::BROKER_SOCKET),
            ("pkg-nix-broker.service", Self::BROKER_SERVICE),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::{collections::BTreeSet, error::Error, fs, process::Command};

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
    #[allow(clippy::too_many_lines)]
    fn socket_and_service_security_contract_is_exact() {
        assert!(
            LinuxSystemdAssets::DAEMON_SOCKET
                .contains("ListenStream=/nix/var/nix/daemon-socket/socket")
        );
        assert!(LinuxSystemdAssets::DAEMON_SOCKET.contains("SocketMode=0660"));
        assert!(LinuxSystemdAssets::DAEMON_SOCKET.contains("SocketGroup=pkg-nix-broker"));
        assert!(LinuxSystemdAssets::HELPER_SOCKET.contains("SocketMode=0660"));
        assert!(LinuxSystemdAssets::HELPER_SOCKET.contains("/run/pkg-helper/"));
        assert!(
            LinuxSystemdAssets::TMPFILES.contains("d /run/pkg-helper 0750 root pkg-nix-broker -")
        );
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
        assert!(
            LinuxSystemdAssets::DAEMON_SERVICE
                .contains("ExecStart=@/opt/pkg/nix/current/bin/nix-daemon nix-daemon --daemon")
        );
        assert!(LinuxSystemdAssets::DAEMON_SERVICE.contains("Delegate=yes"));
        assert!(
            LinuxSystemdAssets::DAEMON_SERVICE
                .contains("Environment=NIX_CONF_DIR=/opt/pkg/etc/pkg")
        );
        assert!(
            LinuxSystemdAssets::DAEMON_SERVICE
                .contains("Environment=NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket")
        );
        assert!(LinuxSystemdAssets::BROKER_SERVICE.contains("User=pkg-nix-broker"));
        assert!(
            LinuxSystemdAssets::BROKER_SERVICE
                .contains("Environment=HOME=/var/lib/pkg/broker-home")
        );
        assert!(
            LinuxSystemdAssets::BROKER_SERVICE
                .contains("Environment=TMPDIR=/var/lib/pkg/broker-home/tmp")
        );
        assert!(LinuxSystemdAssets::HELPER_SERVICE.contains("User=root"));
        assert!(
            LinuxSystemdAssets::HELPER_SERVICE
                .contains("Environment=HOME=/var/lib/pkg/helper-home")
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
}
