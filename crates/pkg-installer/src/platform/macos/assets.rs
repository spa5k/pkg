//! macOS install assets, release steps, and readiness contracts.

use super::*;
/// Closed macOS privileged-install artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsAssetKind {
    /// Fixed system group.
    Group,
    /// Fixed non-login system user.
    User,
    /// Directory with exact mode and principal roles.
    Directory,
    /// File installed from authenticated release/config bytes.
    File,
}

/// Host principal roles resolved by the privileged installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsAssetPrincipal {
    /// uid 0.
    Root,
    /// macOS `wheel` group.
    Wheel,
    /// macOS `admin` group.
    Admin,
    /// Dedicated `pkg-nix-broker` user/group.
    Broker,
    /// Nix `nixbld` group.
    Build,
}

/// One exact macOS install artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsInstallAsset {
    pub(super) id: &'static str,
    pub(super) kind: MacOsAssetKind,
    pub(super) path_or_name: &'static str,
    pub(super) mode: Option<u32>,
    pub(super) owner: Option<MacOsAssetPrincipal>,
    pub(super) group: Option<MacOsAssetPrincipal>,
}

impl MacOsInstallAsset {
    pub(super) const fn account(
        id: &'static str,
        kind: MacOsAssetKind,
        name: &'static str,
    ) -> Self {
        Self {
            id,
            kind,
            path_or_name: name,
            mode: None,
            owner: None,
            group: None,
        }
    }

    pub(super) const fn path(
        id: &'static str,
        kind: MacOsAssetKind,
        path: &'static str,
        mode: u32,
        owner: MacOsAssetPrincipal,
        group: MacOsAssetPrincipal,
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

    /// Stable artifact id recorded in the ownership receipt.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.id
    }

    /// Closed kind.
    #[must_use]
    pub const fn kind(self) -> MacOsAssetKind {
        self.kind
    }

    /// Exact absolute path or account name.
    #[must_use]
    pub const fn path_or_name(self) -> &'static str {
        self.path_or_name
    }

    /// Exact filesystem mode when applicable.
    #[must_use]
    pub const fn mode(self) -> Option<u32> {
        self.mode
    }

    /// Exact owner role when applicable.
    #[must_use]
    pub const fn owner(self) -> Option<MacOsAssetPrincipal> {
        self.owner
    }

    /// Exact group role when applicable.
    #[must_use]
    pub const fn group(self) -> Option<MacOsAssetPrincipal> {
        self.group
    }
}

pub(super) const MACOS_ASSETS: &[MacOsInstallAsset] = &[
    MacOsInstallAsset::account("broker-group", MacOsAssetKind::Group, "pkg-nix-broker"),
    MacOsInstallAsset::account("broker-user", MacOsAssetKind::User, "pkg-nix-broker"),
    MacOsInstallAsset::account("build-group", MacOsAssetKind::Group, "nixbld"),
    MacOsInstallAsset::account("build-user-01", MacOsAssetKind::User, "_nixbld1"),
    MacOsInstallAsset::account("build-user-02", MacOsAssetKind::User, "_nixbld2"),
    MacOsInstallAsset::account("build-user-03", MacOsAssetKind::User, "_nixbld3"),
    MacOsInstallAsset::account("build-user-04", MacOsAssetKind::User, "_nixbld4"),
    MacOsInstallAsset::account("build-user-05", MacOsAssetKind::User, "_nixbld5"),
    MacOsInstallAsset::account("build-user-06", MacOsAssetKind::User, "_nixbld6"),
    MacOsInstallAsset::account("build-user-07", MacOsAssetKind::User, "_nixbld7"),
    MacOsInstallAsset::account("build-user-08", MacOsAssetKind::User, "_nixbld8"),
    MacOsInstallAsset::account("build-user-09", MacOsAssetKind::User, "_nixbld9"),
    MacOsInstallAsset::account("build-user-10", MacOsAssetKind::User, "_nixbld10"),
    MacOsInstallAsset::account("build-user-11", MacOsAssetKind::User, "_nixbld11"),
    MacOsInstallAsset::account("build-user-12", MacOsAssetKind::User, "_nixbld12"),
    MacOsInstallAsset::account("build-user-13", MacOsAssetKind::User, "_nixbld13"),
    MacOsInstallAsset::account("build-user-14", MacOsAssetKind::User, "_nixbld14"),
    MacOsInstallAsset::account("build-user-15", MacOsAssetKind::User, "_nixbld15"),
    MacOsInstallAsset::account("build-user-16", MacOsAssetKind::User, "_nixbld16"),
    MacOsInstallAsset::account("build-user-17", MacOsAssetKind::User, "_nixbld17"),
    MacOsInstallAsset::account("build-user-18", MacOsAssetKind::User, "_nixbld18"),
    MacOsInstallAsset::account("build-user-19", MacOsAssetKind::User, "_nixbld19"),
    MacOsInstallAsset::account("build-user-20", MacOsAssetKind::User, "_nixbld20"),
    MacOsInstallAsset::account("build-user-21", MacOsAssetKind::User, "_nixbld21"),
    MacOsInstallAsset::account("build-user-22", MacOsAssetKind::User, "_nixbld22"),
    MacOsInstallAsset::account("build-user-23", MacOsAssetKind::User, "_nixbld23"),
    MacOsInstallAsset::account("build-user-24", MacOsAssetKind::User, "_nixbld24"),
    MacOsInstallAsset::account("build-user-25", MacOsAssetKind::User, "_nixbld25"),
    MacOsInstallAsset::account("build-user-26", MacOsAssetKind::User, "_nixbld26"),
    MacOsInstallAsset::account("build-user-27", MacOsAssetKind::User, "_nixbld27"),
    MacOsInstallAsset::account("build-user-28", MacOsAssetKind::User, "_nixbld28"),
    MacOsInstallAsset::account("build-user-29", MacOsAssetKind::User, "_nixbld29"),
    MacOsInstallAsset::account("build-user-30", MacOsAssetKind::User, "_nixbld30"),
    MacOsInstallAsset::account("build-user-31", MacOsAssetKind::User, "_nixbld31"),
    MacOsInstallAsset::account("build-user-32", MacOsAssetKind::User, "_nixbld32"),
    MacOsInstallAsset::path(
        "nix-root",
        MacOsAssetKind::Directory,
        "/nix",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Root,
    ),
    MacOsInstallAsset::path(
        "nix-store",
        MacOsAssetKind::Directory,
        "/nix/store",
        0o1775,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Build,
    ),
    MacOsInstallAsset::path(
        "nix-var",
        MacOsAssetKind::Directory,
        "/nix/var",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Build,
    ),
    MacOsInstallAsset::path(
        "nix-state",
        MacOsAssetKind::Directory,
        "/nix/var/nix",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Build,
    ),
    MacOsInstallAsset::path(
        "daemon-socket-dir",
        MacOsAssetKind::Directory,
        "/nix/var/nix/daemon-socket",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "product-root",
        MacOsAssetKind::Directory,
        "/opt/pkg",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "product-config-root",
        MacOsAssetKind::Directory,
        "/opt/pkg/etc",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "product-config-dir",
        MacOsAssetKind::Directory,
        "/opt/pkg/etc/pkg",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "product-bin",
        MacOsAssetKind::Directory,
        "/opt/pkg/bin",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "uninstall-root",
        MacOsAssetKind::Directory,
        "/opt/pkg/uninstall",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "runtime-root",
        MacOsAssetKind::Directory,
        "/opt/pkg/nix",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "service-root",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg",
        0o711,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "managed-nix-state",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/managed-nix",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "run-root",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/run",
        0o751,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "broker-socket-dir",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/run/broker",
        0o771,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "helper-socket-dir",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/run/helper",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "log-root",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/log",
        0o710,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "broker-log-dir",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/log/broker",
        0o700,
        MacOsAssetPrincipal::Broker,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "helper-log-dir",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/log/helper",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "broker-home",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/broker-home",
        0o700,
        MacOsAssetPrincipal::Broker,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "broker-channel-state",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/broker-home/channel",
        0o700,
        MacOsAssetPrincipal::Broker,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "broker-tmp",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/broker-home/tmp",
        0o700,
        MacOsAssetPrincipal::Broker,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "helper-home",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/helper-home",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "helper-tmp",
        MacOsAssetKind::Directory,
        "/Library/Application Support/pkg/helper-home/tmp",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "broker-binary",
        MacOsAssetKind::File,
        "/opt/pkg/bin/pkg-nix-broker",
        0o750,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "helper-binary",
        MacOsAssetKind::File,
        "/opt/pkg/bin/pkg-root-helper",
        0o700,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "product-cli",
        MacOsAssetKind::File,
        "/usr/local/bin/pkg",
        0o755,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "nix-config",
        MacOsAssetKind::File,
        "/opt/pkg/etc/pkg/nix.conf",
        0o640,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Broker,
    ),
    MacOsInstallAsset::path(
        "store-volume-plist",
        MacOsAssetKind::File,
        "/Library/LaunchDaemons/org.pkg.store-volume.plist",
        0o644,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "daemon-plist",
        MacOsAssetKind::File,
        "/Library/LaunchDaemons/org.pkg.nix-daemon.plist",
        0o644,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "helper-plist",
        MacOsAssetKind::File,
        "/Library/LaunchDaemons/org.pkg.root-helper.plist",
        0o644,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "broker-plist",
        MacOsAssetKind::File,
        "/Library/LaunchDaemons/org.pkg.nix-broker.plist",
        0o644,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "path-file",
        MacOsAssetKind::File,
        "/private/etc/paths.d/pkg",
        0o644,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
    MacOsInstallAsset::path(
        "uninstall-manifest",
        MacOsAssetKind::File,
        "/opt/pkg/uninstall/manifest.json",
        0o600,
        MacOsAssetPrincipal::Root,
        MacOsAssetPrincipal::Wheel,
    ),
];

/// Exact macOS install artifact allowlist.
#[must_use]
pub const fn macos_install_assets() -> &'static [MacOsInstallAsset] {
    MACOS_ASSETS
}

/// Returns the exact assets owned by pkg on the Determinate macOS route.
///
/// `/nix` is included only as pre-existing Base Nix evidence. The iterator
/// excludes every Base Nix account, directory, configuration, and service.
#[must_use]
pub fn macos_product_install_assets() -> impl DoubleEndedIterator<Item = MacOsInstallAsset> {
    MACOS_ASSETS
        .iter()
        .copied()
        .filter(|asset| is_macos_product_asset(*asset))
}

/// Returns whether one legacy allowlist entry is owned by the product.
#[must_use]
pub fn is_macos_product_asset(asset: MacOsInstallAsset) -> bool {
    !matches!(
        asset.id,
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
            | "build-user-17"
            | "build-user-18"
            | "build-user-19"
            | "build-user-20"
            | "build-user-21"
            | "build-user-22"
            | "build-user-23"
            | "build-user-24"
            | "build-user-25"
            | "build-user-26"
            | "build-user-27"
            | "build-user-28"
            | "build-user-29"
            | "build-user-30"
            | "build-user-31"
            | "build-user-32"
            | "nix-store"
            | "nix-var"
            | "nix-state"
            | "daemon-socket-dir"
            | "product-config-root"
            | "product-config-dir"
            | "runtime-root"
            | "managed-nix-state"
            | "nix-config"
            | "store-volume-plist"
            | "daemon-plist"
    )
}

/// Fixed encrypted APFS store-volume ownership contract.
///
/// The privileged backend creates an encrypted, ownership-enabled APFS volume,
/// adds only the `nix` entry to `/etc/synthetic.conf`, places the generated
/// unlock secret in the System keychain with root-only access, and records its
/// dynamic UUID plus fixed keychain selector in root-only mount state. The final
/// ownership receipt remains the authenticated static artifact claim. No secret
/// or dynamic UUID crosses this public Rust API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsStoreVolumeContract;

impl MacOsStoreVolumeContract {
    /// APFS volume display name.
    pub const VOLUME_NAME: &'static str = "pkg Nix Store";
    /// Synthetic mount point.
    pub const MOUNT_POINT: &'static str = "/nix";
    /// Exact owned line merged into `/etc/synthetic.conf`.
    pub const SYNTHETIC_ENTRY: &'static str = "nix";
    /// Fixed root-helper verb used by the boot-time mount job.
    pub const MOUNT_HELPER_VERB: &'static str = "--mount-store-volume";
    /// Fixed root-helper verb used by the privileged installer.
    pub const PROVISION_HELPER_VERB: &'static str = "--provision-store-volume";
}

/// Exact product socket paths and post-bind modes.
///
/// The broker creates its public socket as `0666`; the root helper creates its
/// socket as `0660` inside a traversal-restricted `root:pkg-nix-broker`
/// directory. Creation uses a scoped umask, with no pathname chmod race. Both
/// servers still authenticate every accepted peer before reading a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsSocketContract;

impl MacOsSocketContract {
    /// Public CLI-to-broker socket.
    pub const BROKER_PATH: &'static str = "/Library/Application Support/pkg/run/broker/broker.sock";
    /// Public connect mode; directory writes remain broker-only.
    pub const BROKER_MODE: u32 = 0o666;
    /// Private broker-to-root-helper socket.
    pub const HELPER_PATH: &'static str =
        "/Library/Application Support/pkg/run/helper/root-helper.sock";
    /// Broker-only connect mode inside the private parent.
    pub const HELPER_MODE: u32 = 0o660;
}

/// Observed Nix sandbox state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsSandboxReadiness {
    /// `sandbox=true` and `sandbox-fallback=false` were both verified.
    Enforced,
    /// Sandboxing was disabled or could not be verified.
    Disabled,
    /// Sandbox fallback was enabled or could not be proven false.
    FallbackAllowed,
}

/// Observed `_nixbld*` isolation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsBuildUsersReadiness {
    /// The exact group and all 32 users were verified.
    Ready,
    /// The `nixbld` group was absent or unreadable.
    GroupMissing,
    /// The group did not contain the exact 32 managed users.
    UserSetMismatch,
}

impl MacOsBuildUsersReadiness {
    /// Classifies the exact managed group member count after the backend has
    /// verified every member name matches `_nixbld1..32`.
    #[must_use]
    pub const fn from_verified_members(group_present: bool, member_count: usize) -> Self {
        if !group_present {
            Self::GroupMissing
        } else if member_count == BUILD_USER_COUNT {
            Self::Ready
        } else {
            Self::UserSetMismatch
        }
    }

    /// Exact V1 managed build-user count.
    #[must_use]
    pub const fn expected_member_count() -> usize {
        BUILD_USER_COUNT
    }
}

/// Observed native Apple toolchain state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsToolchainReadiness {
    /// Xcode or Command Line Tools can supply native build tools.
    Ready,
    /// The selected developer directory/toolchain was absent or unreadable.
    Missing,
}

/// Observed Darwin readiness values.
///
/// Callers may not manufacture readiness by omitting one of the required checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsBuildReadiness {
    pub(super) system: System,
    pub(super) sandbox: MacOsSandboxReadiness,
    pub(super) build_users: MacOsBuildUsersReadiness,
    pub(super) toolchain: MacOsToolchainReadiness,
}

impl MacOsBuildReadiness {
    /// Captures the complete closed readiness probe result.
    #[must_use]
    pub const fn observed(
        system: System,
        sandbox: MacOsSandboxReadiness,
        build_users: MacOsBuildUsersReadiness,
        toolchain: MacOsToolchainReadiness,
    ) -> Self {
        Self {
            system,
            sandbox,
            build_users,
            toolchain,
        }
    }

    /// Validates Darwin-native, fail-closed readiness and returns the shared
    /// PR-26 engine value (Darwin deliberately has no cgroup flags).
    ///
    /// # Errors
    ///
    /// Returns `BuildReadinessFailed` unless every exact invariant holds.
    pub fn into_engine(self) -> Result<BuildReadiness, MacOsError> {
        if self.system != System::Aarch64Darwin
            || self.sandbox != MacOsSandboxReadiness::Enforced
            || self.build_users != MacOsBuildUsersReadiness::Ready
            || self.toolchain != MacOsToolchainReadiness::Ready
        {
            return Err(MacOsError::new(MacOsErrorCode::BuildReadinessFailed));
        }
        Ok(BuildReadiness::new(true, false, true, false, false))
    }
}

/// Release operation target. Store outputs are intentionally not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsReleaseTarget {
    /// Product CLI, broker, helper, and bundled runtime executables.
    Runtime,
    /// Final flat `.pkg` installer.
    Installer,
}

/// Fixed release stage; arguments are exact templates and contain no secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsReleaseStep {
    pub(super) tool: &'static str,
    pub(super) target: MacOsReleaseTarget,
    pub(super) arguments: &'static [&'static str],
}

impl MacOsReleaseStep {
    /// Absolute Apple tool path.
    #[must_use]
    pub const fn tool(self) -> &'static str {
        self.tool
    }

    /// Product artifact class.
    #[must_use]
    pub const fn target(self) -> MacOsReleaseTarget {
        self.target
    }

    /// Fixed placeholder-bearing argv. Secret values are resolved only by the
    /// isolated release runner and never persisted in a plan or product log.
    #[must_use]
    pub const fn arguments(self) -> &'static [&'static str] {
        self.arguments
    }
}

pub(super) const RELEASE_STEPS: &[MacOsReleaseStep] = &[
    MacOsReleaseStep {
        tool: "/usr/bin/codesign",
        target: MacOsReleaseTarget::Runtime,
        arguments: &[
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--sign",
            "<developer-id-application>",
            "<runtime-artifact>",
        ],
    },
    MacOsReleaseStep {
        tool: "/usr/bin/codesign",
        target: MacOsReleaseTarget::Runtime,
        arguments: &["--verify", "--strict", "--verbose=2", "<runtime-artifact>"],
    },
    MacOsReleaseStep {
        tool: "/usr/bin/pkgbuild",
        target: MacOsReleaseTarget::Installer,
        arguments: &[
            "--root",
            "<signed-payload-root>",
            "--identifier",
            "org.pkg.installer",
            "--version",
            "<release-version>",
            "<component-pkg>",
        ],
    },
    MacOsReleaseStep {
        tool: "/usr/bin/productsign",
        target: MacOsReleaseTarget::Installer,
        arguments: &[
            "--sign",
            "<developer-id-installer>",
            "<component-pkg>",
            "<signed-installer-pkg>",
        ],
    },
    MacOsReleaseStep {
        tool: "/usr/bin/xcrun",
        target: MacOsReleaseTarget::Installer,
        arguments: &[
            "notarytool",
            "submit",
            "<signed-installer-pkg>",
            "--keychain-profile",
            "<notary-profile>",
            "--wait",
            "--output-format",
            "json",
        ],
    },
    MacOsReleaseStep {
        tool: "/usr/bin/xcrun",
        target: MacOsReleaseTarget::Installer,
        arguments: &["stapler", "staple", "<signed-installer-pkg>"],
    },
    MacOsReleaseStep {
        tool: "/usr/sbin/spctl",
        target: MacOsReleaseTarget::Installer,
        arguments: &[
            "--assess",
            "--type",
            "install",
            "--verbose=2",
            "<signed-installer-pkg>",
        ],
    },
];

/// Exact signing → packaging → notarization → staple → Gatekeeper contract.
#[must_use]
pub const fn macos_release_steps() -> &'static [MacOsReleaseStep] {
    RELEASE_STEPS
}
