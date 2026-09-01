//! Exact Linux service assets and authenticated-install allowlist.

pub use crate::platform::linux::LinuxSystemdAssets;

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
        "/nix/var/nix/gcroots/pkg",
        Some(0o700),
    ),
    LinuxInstallAsset::new(
        "nix-gcroots-users",
        LinuxAssetKind::Directory,
        "/nix/var/nix/gcroots/pkg/users",
        Some(0o700),
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

/// Returns true for the Linux assets that remain owned by the product after
/// Determinate takes ownership of native Nix.
#[must_use]
pub fn is_linux_product_asset(asset: LinuxInstallAsset) -> bool {
    !matches!(
        asset.id(),
        "build-group"
            | "build-user-01"
            | "build-user-02"
            | "build-user-03"
            | "build-user-04"
            | "build-user-05"
            | "build-user-06"
            | "build-user-07"
            | "build-user-08"
            | "build-user-09"
            | "build-user-10"
            | "build-user-11"
            | "build-user-12"
            | "build-user-13"
            | "build-user-14"
            | "build-user-15"
            | "build-user-16"
            | "nix-store"
            | "nix-var"
            | "nix-state"
            | "daemon-socket-dir"
    )
}

pub fn is_linux_service_runtime_asset(asset: LinuxInstallAsset) -> bool {
    matches!(
        asset.id(),
        "root-helper-binary"
            | "broker-binary"
            | "helper-socket-unit"
            | "helper-service-unit"
            | "broker-socket-unit"
            | "broker-service-unit"
            | "runtime-tmpfiles"
    )
}

pub fn is_linux_product_gcroots_asset(asset: LinuxInstallAsset) -> bool {
    matches!(asset.id(), "nix-gcroots" | "nix-gcroots-users")
}

pub fn linux_product_install_assets() -> impl DoubleEndedIterator<Item = LinuxInstallAsset> + Clone
{
    ASSETS
        .iter()
        .copied()
        .filter(|asset| is_linux_product_asset(*asset))
}

pub fn linux_product_mutation_assets() -> impl DoubleEndedIterator<Item = LinuxInstallAsset> + Clone
{
    linux_product_install_assets().filter(|asset| asset.id() != "nix-root")
}

#[cfg(test)]
mod tests;
