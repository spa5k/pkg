//! macOS installation, launchd, peer-authentication, and release contracts.
//!
//! The public CLI never receives a Nix executable or daemon socket. A
//! launchd-managed broker authenticates callers with `getpeereid`; the root
//! helper repeats the same kernel check for its sole broker peer. Apple
//! signing/notarization applies to product runtime artifacts only, never to
//! locally built Nix store outputs.

use crate::{
    BrokerHelperDispatch, LinuxHelperSession,
    platform::linux::{LinuxRootSetStore, provision_product_root_if_absent},
};
#[cfg(target_os = "macos")]
use nix::unistd::getpeereid;
use pkg_core::{System, state::Digest};
use pkg_nix::{
    AuthenticatedHelper, AuthenticatedInstallerPayloads, AuthenticatedManagedNixConfig,
    BrokerHelperRequest, BrokerHelperResponse, BuildReadiness, MaintenanceError,
};
use std::{error::Error, fmt, os::unix::net::UnixStream};

const BUILD_USER_COUNT: usize = 32;

/// Stable macOS platform/installer failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsErrorCode {
    /// `getpeereid` was unavailable or failed.
    PeerCredentialsUnavailable,
    /// The peer was not the configured service identity.
    UnauthenticatedPeer,
    /// The requested target was not a native Darwin system.
    UnsupportedPlatform,
    /// Existing Nix state was unmanaged or ambiguous.
    UnmanagedNix,
    /// The fixed privileged backend operation failed.
    BackendFailure,
    /// Sandbox, build users, or Apple toolchain readiness failed closed.
    BuildReadinessFailed,
    /// Installed product code failed its Developer ID verification contract.
    CodeSignatureInvalid,
    /// Service activation or the daemon readiness check failed.
    ServiceUnhealthy,
    /// Receipt-last publication failed.
    ReceiptFailure,
    /// Exact reverse-order rollback did not fully succeed.
    RollbackIncomplete,
}

/// Redacted macOS platform failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsError {
    code: MacOsErrorCode,
}

impl MacOsError {
    const fn new(code: MacOsErrorCode) -> Self {
        Self { code }
    }

    /// Constructs a closed backend failure for platform implementations.
    #[must_use]
    pub const fn backend_failure() -> Self {
        Self::new(MacOsErrorCode::BackendFailure)
    }

    pub(crate) const fn rollback_incomplete() -> Self {
        Self::new(MacOsErrorCode::RollbackIncomplete)
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS managed-runtime operation failed")
    }
}

impl Error for MacOsError {}

/// Kernel-authenticated effective identity for a connected Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsPeerCredentials {
    uid: u32,
    gid: u32,
}

impl MacOsPeerCredentials {
    /// Returns the peer's effective uid.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the peer's effective gid.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

/// Reads the effective uid/gid before any product frame is consumed.
///
/// # Errors
///
/// Returns `PeerCredentialsUnavailable` if the kernel query fails or this is
/// not a macOS build.
#[cfg(target_os = "macos")]
pub fn peer_credentials(stream: &UnixStream) -> Result<MacOsPeerCredentials, MacOsError> {
    let (uid, gid) = getpeereid(stream)
        .map_err(|_| MacOsError::new(MacOsErrorCode::PeerCredentialsUnavailable))?;
    Ok(MacOsPeerCredentials {
        uid: uid.as_raw(),
        gid: gid.as_raw(),
    })
}

/// Reports the Darwin-only contract as unavailable on other build hosts.
///
/// # Errors
///
/// Always returns `PeerCredentialsUnavailable` outside macOS.
#[cfg(not(target_os = "macos"))]
pub const fn peer_credentials(_stream: &UnixStream) -> Result<MacOsPeerCredentials, MacOsError> {
    Err(MacOsError::new(MacOsErrorCode::PeerCredentialsUnavailable))
}

/// Requires the connected peer to be the singleton broker service uid.
///
/// # Errors
///
/// Returns a closed error when credentials are unavailable or the uid differs.
pub fn authenticate_broker_peer(
    stream: &UnixStream,
    broker_uid: u32,
) -> Result<MacOsPeerCredentials, MacOsError> {
    let peer = peer_credentials(stream)?;
    if peer.uid == broker_uid {
        Ok(peer)
    } else {
        Err(MacOsError::new(MacOsErrorCode::UnauthenticatedPeer))
    }
}

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
    id: &'static str,
    kind: MacOsAssetKind,
    path_or_name: &'static str,
    mode: Option<u32>,
    owner: Option<MacOsAssetPrincipal>,
    group: Option<MacOsAssetPrincipal>,
}

impl MacOsInstallAsset {
    const fn account(id: &'static str, kind: MacOsAssetKind, name: &'static str) -> Self {
        Self {
            id,
            kind,
            path_or_name: name,
            mode: None,
            owner: None,
            group: None,
        }
    }

    const fn path(
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

const MACOS_ASSETS: &[MacOsInstallAsset] = &[
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

/// Exact launchd definitions.
///
/// The daemon socket is created by Nix inside its traversal-restricted parent.
/// Product sockets are bound by their fixed jobs; both transports still
/// authenticate every accepted connection in-kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsLaunchdAssets;

impl MacOsLaunchdAssets {
    /// Mounts/unlocks the receipt-owned encrypted APFS store before daemon use.
    /// The helper reads the dynamic UUID/keychain handle from root-only state;
    /// neither appears in this plist or process arguments.
    pub const STORE_VOLUME: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.store-volume</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-root-helper</string><string>--mount-store-volume</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Standard</string>
<key>UserName</key><string>root</string><key>GroupName</key><string>wheel</string>
<key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/store-volume.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/store-volume.log</string>
</dict></plist>
"#;

    /// Root managed Nix daemon. No launchd resource-limit claim is made: those
    /// are inherited per-process RLIMITs, not a per-build or subtree cap.
    pub const NIX_DAEMON: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.nix-daemon</string>
<key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string><string>/bin/wait4path /nix/var/nix/daemon-socket &amp;&amp; exec /opt/pkg/nix/current/bin/nix-daemon</string></array>
<key>EnvironmentVariables</key><dict><key>NIX_CONF_DIR</key><string>/opt/pkg/etc/pkg</string><key>NIX_DAEMON_SOCKET_PATH</key><string>/nix/var/nix/daemon-socket/socket</string><key>NIX_STATE_DIR</key><string>/nix/var/nix</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>root</string><key>GroupName</key><string>wheel</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/nix-daemon.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/nix-daemon.log</string>
</dict></plist>
"#;

    /// Root helper with only the private broker-traversable socket path.
    pub const ROOT_HELPER: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.root-helper</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-root-helper</string><string>--serve-macos</string></array>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>/Library/Application Support/pkg/helper-home</string><key>TMPDIR</key><string>/Library/Application Support/pkg/helper-home/tmp</string></dict>
<key>WorkingDirectory</key><string>/Library/Application Support/pkg/helper-home</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>root</string><key>GroupName</key><string>pkg-nix-broker</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/helper/root-helper.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/helper/root-helper.log</string>
</dict></plist>
"#;

    /// Singleton unprivileged broker. Its only writable service-tree leaf is
    /// the private broker log directory; the socket directory is root-owned
    /// and group-writable solely by the singleton broker group.
    pub const BROKER: &'static str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>org.pkg.nix-broker</string>
<key>ProgramArguments</key><array><string>/opt/pkg/bin/pkg-nix-broker</string><string>--serve-macos</string></array>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>/Library/Application Support/pkg/broker-home</string><key>TMPDIR</key><string>/Library/Application Support/pkg/broker-home/tmp</string></dict>
<key>WorkingDirectory</key><string>/Library/Application Support/pkg/broker-home</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>UserName</key><string>pkg-nix-broker</string><key>GroupName</key><string>pkg-nix-broker</string>
<key>ProcessType</key><string>Standard</string><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/Library/Application Support/pkg/log/broker/broker.log</string>
<key>StandardErrorPath</key><string>/Library/Application Support/pkg/log/broker/broker.log</string>
</dict></plist>
"#;

    /// Deterministic label/text set.
    #[must_use]
    pub const fn all() -> [(&'static str, &'static str); 4] {
        [
            ("org.pkg.store-volume", Self::STORE_VOLUME),
            ("org.pkg.nix-daemon", Self::NIX_DAEMON),
            ("org.pkg.root-helper", Self::ROOT_HELPER),
            ("org.pkg.nix-broker", Self::BROKER),
        ]
    }
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
    system: System,
    sandbox: MacOsSandboxReadiness,
    build_users: MacOsBuildUsersReadiness,
    toolchain: MacOsToolchainReadiness,
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
    tool: &'static str,
    target: MacOsReleaseTarget,
    arguments: &'static [&'static str],
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

const RELEASE_STEPS: &[MacOsReleaseStep] = &[
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

/// macOS wrapper over the shared crash-durable `/nix` GC-root implementation.
#[derive(Debug, Clone)]
pub struct MacOsRootSetStore {
    inner: LinuxRootSetStore,
}

impl MacOsRootSetStore {
    /// Opens the product root tree and verifies root-owned safe ancestors.
    ///
    /// # Errors
    ///
    /// Returns a closed filesystem failure on unsafe or unavailable state.
    pub fn production() -> Result<Self, MacOsError> {
        provision_product_root_if_absent(std::path::Path::new("/nix/var/nix/gcroots"), 0)
            .map(|inner| Self { inner })
            .map_err(|_| MacOsError::new(MacOsErrorCode::BackendFailure))
    }

    #[cfg(all(test, target_os = "macos"))]
    fn new_at(path: std::path::PathBuf, owner_uid: u32) -> Result<Self, MacOsError> {
        LinuxRootSetStore::new_at(path, owner_uid)
            .map(|inner| Self { inner })
            .map_err(|_| MacOsError::new(MacOsErrorCode::BackendFailure))
    }
}

/// PR-39 helper state bound to the Darwin peer-authenticated transport.
pub struct MacOsHelperSession {
    inner: LinuxHelperSession,
}

impl fmt::Debug for MacOsHelperSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MacOsHelperSession(<authenticated-private-state>)")
    }
}

impl MacOsHelperSession {
    /// Binds authenticated capability state to the durable Darwin root store.
    #[must_use]
    pub fn new(authenticated: AuthenticatedHelper, roots: MacOsRootSetStore) -> Self {
        Self {
            inner: LinuxHelperSession::new(authenticated, roots.inner),
        }
    }
}

impl BrokerHelperDispatch for MacOsHelperSession {
    fn dispatch(
        &self,
        request: BrokerHelperRequest,
    ) -> Result<BrokerHelperResponse, MaintenanceError> {
        self.inner.dispatch(request)
    }

    fn dispatch_build(
        &self,
        request: &pkg_nix::BuildRequest,
        deadline: std::time::Instant,
        cancelled: &std::sync::atomic::AtomicBool,
        progress: &mut dyn FnMut(
            pkg_nix::BuildProgressEstimate,
        ) -> Result<(), pkg_nix::NixAdapterError>,
    ) -> pkg_nix::RootNixResponse {
        self.inner
            .dispatch_build(request, deadline, cancelled, progress)
    }

    fn dispatch_root_nix(
        &self,
        request: pkg_nix::RootNixRequest,
        deadline: std::time::Instant,
    ) -> pkg_nix::RootNixResponse {
        self.inner.dispatch_root_nix(request, deadline)
    }
}

/// Whether one fixed macOS object is exact-present or absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsAssetPresence {
    /// The object exists and matches the complete closed contract.
    ExactPresent,
    /// The object does not exist.
    Absent,
}

/// Closed privileged operations used by the macOS installer.
pub trait MacOsInstallBackend {
    /// Returns the durable product operation mode.
    fn install_mode(&self) -> crate::MacOsInstallMode {
        crate::MacOsInstallMode::FreshInstall
    }
    /// Rechecks the offline barrier before one product mutation.
    ///
    /// # Errors
    /// Returns a closed error when the product jobs are not offline.
    fn preflight_product_mutation(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    /// Binds exact authenticated product executable bytes in memory.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError>;
    /// Binds the exact authenticated managed-Nix configuration in memory.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError>;

    /// Binds the authenticated release identity in memory.
    ///
    /// This must not mutate the host. It runs before privileged preflight.
    ///
    /// # Errors
    /// Returns a closed error for a wrong-platform or conflicting binding.
    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        release_identity_digest: Digest,
    ) -> Result<(), MacOsError>;

    /// Records that an authenticated install journal will be recovered.
    ///
    /// # Errors
    /// Returns a closed error when recovery state cannot be bound.
    fn begin_authenticated_recovery(
        &mut self,
        mode: crate::MacOsInstallMode,
    ) -> Result<(), MacOsError>;

    /// Verifies AuthorizationServices/sudo authority.
    ///
    /// # Errors
    /// Returns a closed error when privilege is unavailable.
    fn preflight_privilege(&mut self) -> Result<(), MacOsError>;
    /// Scans the production host, including `/nix`, profiles, Homebrew, and launchd.
    ///
    /// # Errors
    /// Returns a closed error for unmanaged, ambiguous, or unreadable evidence.
    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError>;
    /// Returns the fixed broker uid after exact account observation.
    ///
    /// # Errors
    /// Returns a closed error when the broker account is absent or changed.
    fn broker_uid(&mut self) -> Result<u32, MacOsError>;
    /// Classifies one fixed artifact without mutation.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError>;
    /// Classifies the complete APFS/keychain/synthetic/record contract.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError>;
    /// Classifies the authenticated managed runtime.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or ambiguous state.
    fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError>;
    /// Classifies the four fixed launchd jobs.
    ///
    /// # Errors
    /// Returns a closed error for partial, unreadable, or ambiguous state.
    fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError>;
    /// Classifies the authenticated root ownership receipt.
    ///
    /// # Errors
    /// Returns a closed error for unsafe, changed, or unbound state.
    fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError>;
    /// Removes one revalidated artifact during interrupted-install recovery.
    ///
    /// # Errors
    /// Returns a closed error unless the artifact is exact or safely absent.
    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError>;
    /// Removes the exact product-owned APFS state during recovery.
    ///
    /// # Errors
    /// Returns a closed error for foreign state or incomplete removal.
    fn recover_store_volume(&mut self) -> Result<(), MacOsError>;
    /// Removes the exact launchd activation state during recovery.
    ///
    /// # Errors
    /// Returns a closed error for partial state or incomplete deactivation.
    fn recover_services(&mut self) -> Result<(), MacOsError>;
    /// Removes the exact authenticated ownership receipt during recovery.
    ///
    /// # Errors
    /// Returns a closed error unless the receipt is exact or safely absent.
    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError>;
    /// Verifies authenticated release hashes before the first mutation.
    ///
    /// # Errors
    /// Returns a closed error when release authentication fails.
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError>;
    /// Creates/mounts the product-owned encrypted APFS store and journals the
    /// synthetic.conf/keychain/volume state before every mutation.
    ///
    /// # Errors
    /// Returns a closed error when the exact encrypted-volume contract fails.
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError>;
    /// Reverts only the APFS/keychain/synthetic state created by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact volume rollback is incomplete.
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError>;
    /// Creates or verifies one fixed artifact; journals before mutation.
    ///
    /// # Errors
    /// Returns a closed error when exact creation or verification fails.
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError>;
    /// Installs one exact compiled-in launchd plist; journals before mutation.
    ///
    /// # Errors
    /// Returns a closed error when exact installation fails.
    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError>;
    /// Installs the complete authenticated per-platform Nix configuration.
    ///
    /// # Errors
    /// Returns a closed error when rendering or atomic installation fails.
    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError>;
    /// Provisions the authenticated pinned Nix runtime.
    ///
    /// # Errors
    /// Returns a closed error when authenticated provisioning fails.
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError>;
    /// Rolls back runtime state created by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact rollback is incomplete.
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError>;
    /// Accepts Base Nix after the standard adapter proves readiness.
    ///
    /// # Errors
    /// Returns a closed error for an invalid or incomplete handoff.
    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError>;
    /// Verifies installed product executables' Developer ID requirement.
    ///
    /// # Errors
    /// Returns a closed error when any required signature is invalid.
    fn verify_installed_code(&mut self) -> Result<(), MacOsError>;
    /// Bootstraps fixed jobs and journals prior launchd state.
    ///
    /// # Errors
    /// Returns a closed error when a fixed launchd mutation fails.
    fn activate_services(&mut self) -> Result<bool, MacOsError>;
    /// Reverts only service state changed by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact launchd rollback is incomplete.
    fn rollback_services(&mut self) -> Result<(), MacOsError>;
    /// Performs the bounded managed-store health check.
    ///
    /// # Errors
    /// Returns a closed error when the daemon is not ready.
    fn check_managed_daemon(&mut self) -> Result<(), MacOsError>;
    /// Observes sandbox/config/build-user/toolchain readiness after activation.
    ///
    /// # Errors
    /// Returns a closed error when a required probe cannot be completed.
    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError>;
    /// Publishes the root-owned ownership receipt last.
    ///
    /// # Errors
    /// Returns a closed error when atomic receipt publication fails.
    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError>;
    /// Removes one exact artifact owned by this attempt.
    ///
    /// # Errors
    /// Returns a closed error when exact rollback is incomplete.
    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError>;
    /// Returns the prior receipt digest for one owned file during upgrade.
    ///
    /// # Errors
    /// Returns a closed error when authenticated prior state is unavailable.
    fn prior_file_digest(
        &mut self,
        _asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        Ok(None)
    }
    /// Restores an authenticated prior product file.
    ///
    /// # Errors
    /// Returns a closed error when exact replacement recovery fails.
    fn recover_replaced_asset(
        &mut self,
        _asset: MacOsInstallAsset,
        _prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Keeps authenticated release bytes after interrupted explicit repair.
    ///
    /// # Errors
    /// Returns a closed error when exact replacement recovery fails.
    fn roll_forward_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Removes a replacement backup after receipt durability.
    ///
    /// # Errors
    /// Returns a closed error when exact backup removal fails.
    fn finalize_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Ok(())
    }
    /// Returns the digest of the authenticated prior product receipt.
    ///
    /// # Errors
    /// Returns a closed error when authenticated prior state is unavailable.
    fn prior_ownership_receipt_digest(&mut self) -> Result<Option<Digest>, MacOsError> {
        Ok(None)
    }
    /// Restores the authenticated prior product receipt.
    ///
    /// # Errors
    /// Returns a closed error when exact receipt recovery fails.
    fn recover_replaced_ownership_receipt(
        &mut self,
        _prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
    /// Keeps the authenticated candidate receipt after interrupted repair.
    ///
    /// # Errors
    /// Returns a closed error when exact receipt recovery fails.
    fn roll_forward_replaced_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        Err(MacOsError::backend_failure())
    }
}

/// Sanitized idempotent installation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsInstallReport {
    created_artifacts: usize,
    existing_artifacts: usize,
}

impl MacOsInstallReport {
    /// Count created by this attempt.
    #[must_use]
    pub const fn created_artifacts(self) -> usize {
        self.created_artifacts
    }

    /// Count already exact before this attempt.
    #[must_use]
    pub const fn existing_artifacts(self) -> usize {
        self.existing_artifacts
    }
}

#[derive(Debug, Clone, Copy)]
enum InstallMutation {
    Asset(MacOsInstallAsset),
    Runtime,
    Services,
    OwnershipReceipt,
}

/// Executes authenticated, failure-atomic, receipt-last macOS installation.
///
/// # Errors
///
/// Returns a stable error for unsupported/unmanaged hosts, signature or
/// readiness failure, unhealthy services, receipt failure, or incomplete rollback.
#[allow(clippy::too_many_lines)]
pub fn install_macos(
    system: System,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<MacOsInstallReport, MacOsError> {
    preflight_macos(system, backend)?;
    let mut mutations = Vec::new();
    let mut created = 0_usize;
    let mut existing = 0_usize;
    let result = (|| {
        for asset in macos_product_install_assets()
            .filter(|asset| asset.kind != MacOsAssetKind::File && asset.id != "nix-root")
        {
            record_asset_result(backend, asset, &mut mutations, &mut created, &mut existing)?;
        }
        mutations.push(InstallMutation::Runtime);
        if !backend.provision_managed_runtime()? {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?;
        backend.accept_base_nix_handoff()?;
        let nix_root = macos_product_install_assets()
            .find(|asset| asset.id == "nix-root")
            .ok_or_else(MacOsError::backend_failure)?;
        record_asset_result(
            backend,
            nix_root,
            &mut mutations,
            &mut created,
            &mut existing,
        )?;
        for asset in macos_product_install_assets()
            .filter(|asset| asset.kind == MacOsAssetKind::File && asset.id != "uninstall-manifest")
        {
            mutations.push(InstallMutation::Asset(asset));
            let was_created = match asset.id {
                "helper-plist" => {
                    backend.install_launchd_plist(asset, MacOsLaunchdAssets::ROOT_HELPER)?
                }
                "broker-plist" => {
                    backend.install_launchd_plist(asset, MacOsLaunchdAssets::BROKER)?
                }
                _ => backend.ensure_asset(asset)?,
            };
            if was_created {
                created = created.saturating_add(1);
            } else {
                let _ = mutations.pop();
                existing = existing.saturating_add(1);
            }
        }
        backend
            .verify_installed_code()
            .map_err(|_| MacOsError::new(MacOsErrorCode::CodeSignatureInvalid))?;
        mutations.push(InstallMutation::Services);
        if !backend
            .activate_services()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?
        {
            let _ = mutations.pop();
        }
        backend
            .check_managed_daemon()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ServiceUnhealthy))?;
        let receipt_presence = backend.classify_ownership_receipt()?;
        if receipt_presence == MacOsAssetPresence::Absent
            || backend.install_mode() == crate::MacOsInstallMode::OfflineUpgrade
        {
            mutations.push(InstallMutation::OwnershipReceipt);
        }
        let receipt_created = backend
            .publish_ownership_receipt()
            .map_err(|_| MacOsError::new(MacOsErrorCode::ReceiptFailure))?;
        let expected_receipt_change = match backend.install_mode() {
            crate::MacOsInstallMode::FreshInstall => receipt_presence == MacOsAssetPresence::Absent,
            crate::MacOsInstallMode::OfflineUpgrade => true,
            crate::MacOsInstallMode::OfflineRepair => false,
        };
        if receipt_created != expected_receipt_change {
            return Err(MacOsError::backend_failure());
        }
        Ok(MacOsInstallReport {
            created_artifacts: created,
            existing_artifacts: existing,
        })
    })();

    if result.is_err() {
        let mut rollback_incomplete = false;
        for mutation in mutations.into_iter().rev() {
            let rollback = match mutation {
                InstallMutation::Asset(asset) => backend.rollback_asset(asset),
                InstallMutation::Runtime => backend.rollback_managed_runtime(),
                InstallMutation::Services => backend.rollback_services(),
                InstallMutation::OwnershipReceipt => backend.recover_ownership_receipt(),
            };
            if rollback.is_err() {
                rollback_incomplete = true;
            }
        }
        if rollback_incomplete {
            return Err(MacOsError::new(MacOsErrorCode::RollbackIncomplete));
        }
    }
    result
}

/// Reverts one interrupted authenticated macOS installation from durable state.
///
/// # Errors
///
/// Returns a redacted failure for changed, foreign, ambiguous, or incomplete state.
pub fn recover_macos_install(
    journal: &mut crate::MacOsInstallJournal,
    backend: &mut dyn MacOsInstallBackend,
    recover_runtime: &mut dyn FnMut() -> Result<(), MacOsError>,
    persist_progress: &mut dyn FnMut(&crate::MacOsInstallJournal) -> Result<(), MacOsError>,
) -> Result<(), MacOsError> {
    while let Some((mutation, disposition, prior_digest)) =
        journal
            .recovery_actions()
            .first()
            .map(|action| match action {
                crate::MacOsInstallRecoveryAction::RevalidateIntended(mutation) => {
                    ((*mutation).clone(), 0_u8, None)
                }
                crate::MacOsInstallRecoveryAction::RevertCreated(mutation) => {
                    ((*mutation).clone(), 1, None)
                }
                crate::MacOsInstallRecoveryAction::RestoreReplaced(mutation, digest) => {
                    ((*mutation).clone(), 2, Some(*digest))
                }
                crate::MacOsInstallRecoveryAction::RollForwardReplaced(mutation) => {
                    ((*mutation).clone(), 3, None)
                }
            })
    {
        match &mutation {
            crate::MacOsInstallMutation::Asset { id } => {
                let asset = macos_asset_by_id(id)?;
                match disposition {
                    0 if backend.classify_asset(asset)? == MacOsAssetPresence::ExactPresent => {
                        backend.recover_asset(asset)?;
                    }
                    0 => {}
                    1 => backend.recover_asset(asset)?,
                    2 => backend.recover_replaced_asset(
                        asset,
                        prior_digest.ok_or_else(MacOsError::backend_failure)?,
                    )?,
                    3 => backend.roll_forward_replaced_asset(asset)?,
                    _ => return Err(MacOsError::backend_failure()),
                }
            }
            crate::MacOsInstallMutation::StoreVolume => {
                if disposition == 1
                    || backend.classify_store_volume()? == MacOsAssetPresence::ExactPresent
                {
                    backend.recover_store_volume()?;
                }
            }
            crate::MacOsInstallMutation::ManagedRuntime => recover_runtime()?,
            crate::MacOsInstallMutation::Services => {
                if disposition == 1
                    || backend.classify_services()? == MacOsAssetPresence::ExactPresent
                {
                    backend.recover_services()?;
                }
            }
            crate::MacOsInstallMutation::OwnershipReceipt => match disposition {
                0 if backend.classify_ownership_receipt()? == MacOsAssetPresence::ExactPresent => {
                    backend.recover_ownership_receipt()?;
                }
                0 => {}
                1 => backend.recover_ownership_receipt()?,
                2 => backend.recover_replaced_ownership_receipt(
                    prior_digest.ok_or_else(MacOsError::backend_failure)?,
                )?,
                3 => backend.roll_forward_replaced_ownership_receipt()?,
                _ => return Err(MacOsError::backend_failure()),
            },
        }
        journal
            .complete_recovery_action(&mutation)
            .map_err(|_| MacOsError::backend_failure())?;
        persist_progress(journal)?;
    }
    Ok(())
}

fn macos_asset_by_id(id: &str) -> Result<MacOsInstallAsset, MacOsError> {
    MACOS_ASSETS
        .iter()
        .copied()
        .find(|asset| asset.id == id)
        .ok_or_else(MacOsError::backend_failure)
}

pub(crate) fn store_volume_prerequisite(id: &str) -> bool {
    id.starts_with("build-user-")
        || matches!(
            id,
            "broker-group"
                | "broker-user"
                | "build-group"
                | "product-root"
                | "product-bin"
                | "service-root"
                | "managed-nix-state"
                | "helper-binary"
        )
}

fn preflight_macos(
    system: System,
    backend: &mut dyn MacOsInstallBackend,
) -> Result<(), MacOsError> {
    if system != System::Aarch64Darwin {
        return Err(MacOsError::new(MacOsErrorCode::UnsupportedPlatform));
    }
    backend.preflight_privilege()?;
    backend
        .preflight_clean_host(system)
        .map_err(|_| MacOsError::new(MacOsErrorCode::UnmanagedNix))?;
    backend.verify_release_bundle()
}

fn record_asset_result(
    backend: &mut dyn MacOsInstallBackend,
    asset: MacOsInstallAsset,
    mutations: &mut Vec<InstallMutation>,
    created: &mut usize,
    existing: &mut usize,
) -> Result<(), MacOsError> {
    mutations.push(InstallMutation::Asset(asset));
    if backend.ensure_asset(asset)? {
        *created = created.saturating_add(1);
    } else {
        let _ = mutations.pop();
        *existing = existing.saturating_add(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    #[test]
    #[allow(clippy::too_many_lines)]
    fn asset_manifest_is_closed_unique_and_has_exact_build_users() -> Result<(), Box<dyn Error>> {
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        let build_users = MACOS_ASSETS
            .iter()
            .filter(|asset| asset.id.starts_with("build-user-"))
            .collect::<Vec<_>>();
        assert_eq!(build_users.len(), BUILD_USER_COUNT);
        for (index, asset) in build_users.iter().enumerate() {
            assert_eq!(asset.path_or_name, format!("_nixbld{}", index + 1));
        }
        for asset in MACOS_ASSETS {
            assert!(ids.insert(asset.id));
            if matches!(asset.kind, MacOsAssetKind::Directory | MacOsAssetKind::File) {
                assert!(paths.insert(asset.path_or_name));
                assert!(asset.path_or_name.starts_with('/'));
                assert!(asset.mode.is_some());
                assert!(asset.owner.is_some());
                assert!(asset.group.is_some());
            }
        }
        let broker_dir = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "broker-socket-dir")
            .ok_or_else(|| std::io::Error::other("missing broker socket fixture"))?;
        assert_eq!(broker_dir.owner, Some(MacOsAssetPrincipal::Root));
        assert_eq!(broker_dir.group, Some(MacOsAssetPrincipal::Broker));
        assert_eq!(broker_dir.mode, Some(0o771));
        let service_root = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "service-root")
            .ok_or_else(|| std::io::Error::other("missing service root fixture"))?;
        assert_eq!(service_root.mode, Some(0o711));
        let run_root = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "run-root")
            .ok_or_else(|| std::io::Error::other("missing run root fixture"))?;
        assert_eq!(run_root.mode, Some(0o751));
        let nix_var = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "nix-var")
            .ok_or_else(|| std::io::Error::other("missing Nix var directory"))?;
        assert_eq!(nix_var.path_or_name, "/nix/var");
        assert_eq!(nix_var.mode, Some(0o755));
        assert_eq!(nix_var.owner, Some(MacOsAssetPrincipal::Root));
        assert_eq!(nix_var.group, Some(MacOsAssetPrincipal::Build));
        assert_eq!(MacOsSocketContract::BROKER_MODE, 0o666);
        assert_eq!(MacOsSocketContract::HELPER_MODE, 0o660);
        let helper = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "helper-binary")
            .ok_or_else(|| std::io::Error::other("missing helper fixture"))?;
        assert_eq!(helper.mode, Some(0o700));
        assert_eq!(helper.owner, Some(MacOsAssetPrincipal::Root));
        assert_eq!(helper.group, Some(MacOsAssetPrincipal::Wheel));
        for (id, path, mode) in [
            ("product-config-dir", "/opt/pkg/etc/pkg", 0o750),
            ("nix-config", "/opt/pkg/etc/pkg/nix.conf", 0o640),
        ] {
            let asset = MACOS_ASSETS
                .iter()
                .find(|asset| asset.id == id)
                .ok_or_else(|| std::io::Error::other("missing private config asset"))?;
            assert_eq!(asset.path_or_name, path);
            assert_eq!(asset.mode, Some(mode));
            assert_eq!(asset.owner, Some(MacOsAssetPrincipal::Root));
            assert_eq!(asset.group, Some(MacOsAssetPrincipal::Broker));
        }
        for (id, path, owner) in [
            (
                "broker-home",
                "/Library/Application Support/pkg/broker-home",
                MacOsAssetPrincipal::Broker,
            ),
            (
                "broker-channel-state",
                "/Library/Application Support/pkg/broker-home/channel",
                MacOsAssetPrincipal::Broker,
            ),
            (
                "broker-tmp",
                "/Library/Application Support/pkg/broker-home/tmp",
                MacOsAssetPrincipal::Broker,
            ),
            (
                "helper-home",
                "/Library/Application Support/pkg/helper-home",
                MacOsAssetPrincipal::Root,
            ),
            (
                "helper-tmp",
                "/Library/Application Support/pkg/helper-home/tmp",
                MacOsAssetPrincipal::Root,
            ),
            (
                "helper-log-dir",
                "/Library/Application Support/pkg/log/helper",
                MacOsAssetPrincipal::Root,
            ),
        ] {
            let asset = MACOS_ASSETS
                .iter()
                .find(|asset| asset.id == id)
                .ok_or_else(|| std::io::Error::other("missing managed runtime asset"))?;
            assert_eq!(asset.path_or_name, path);
            assert_eq!(asset.owner, Some(owner));
        }
        Ok(())
    }

    #[test]
    fn nix_root_matches_authenticated_runtime_ownership() -> Result<(), Box<dyn Error>> {
        let nix_root = MACOS_ASSETS
            .iter()
            .find(|asset| asset.id == "nix-root")
            .ok_or_else(|| std::io::Error::other("missing Nix root directory"))?;
        assert_eq!(nix_root.group, Some(MacOsAssetPrincipal::Root));
        Ok(())
    }

    #[test]
    fn launchd_contract_has_exact_roles_and_no_false_resource_or_gc_claims() {
        for (label, plist) in MacOsLaunchdAssets::all() {
            assert!(plist.contains(label));
            assert!(!plist.contains("StartInterval"));
            assert!(!plist.contains("StartCalendarInterval"));
            assert!(!plist.contains("HardResourceLimits"));
            assert!(!plist.contains("SoftResourceLimits"));
        }
        assert!(MacOsLaunchdAssets::NIX_DAEMON.contains("<string>root</string>"));
        assert!(
            MacOsLaunchdAssets::NIX_DAEMON
                .contains("<key>NIX_CONF_DIR</key><string>/opt/pkg/etc/pkg</string>")
        );
        assert!(MacOsLaunchdAssets::NIX_DAEMON.contains(
            "<key>NIX_DAEMON_SOCKET_PATH</key><string>/nix/var/nix/daemon-socket/socket</string>"
        ));
        assert!(MacOsLaunchdAssets::BROKER.contains("<string>pkg-nix-broker</string>"));
        assert!(MacOsLaunchdAssets::BROKER.contains(
            "<key>HOME</key><string>/Library/Application Support/pkg/broker-home</string>"
        ));
        assert!(MacOsLaunchdAssets::BROKER.contains(
            "<key>TMPDIR</key><string>/Library/Application Support/pkg/broker-home/tmp</string>"
        ));
        assert!(MacOsLaunchdAssets::ROOT_HELPER.contains("pkg-root-helper"));
        assert!(MacOsLaunchdAssets::ROOT_HELPER.contains(
            "<key>HOME</key><string>/Library/Application Support/pkg/helper-home</string>"
        ));
        assert!(MacOsLaunchdAssets::ROOT_HELPER.contains(
            "<key>TMPDIR</key><string>/Library/Application Support/pkg/helper-home/tmp</string>"
        ));
        assert!(
            MacOsLaunchdAssets::ROOT_HELPER
                .contains("<key>GroupName</key><string>pkg-nix-broker</string>")
        );
        assert!(MacOsLaunchdAssets::STORE_VOLUME.contains("--mount-store-volume"));
        assert!(!MacOsLaunchdAssets::STORE_VOLUME.contains("security "));
        assert!(!MacOsLaunchdAssets::STORE_VOLUME.contains("diskutil "));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plutil_accepts_every_launchd_definition() -> Result<(), Box<dyn Error>> {
        use std::{
            fs,
            process::Command,
            sync::atomic::{AtomicU64, Ordering},
        };

        static NEXT: AtomicU64 = AtomicU64::new(0);
        for (label, plist) in MacOsLaunchdAssets::all() {
            let path = std::env::temp_dir().join(format!(
                "pkg-plist-{}-{}-{}.plist",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                label
            ));
            fs::write(&path, plist)?;
            let status = Command::new("/usr/bin/plutil")
                .args(["-lint", "--"])
                .arg(&path)
                .status()?;
            fs::remove_file(path)?;
            assert!(status.success());
        }
        Ok(())
    }

    #[test]
    fn darwin_readiness_requires_every_fail_closed_gate() {
        let ready = MacOsBuildReadiness::observed(
            System::Aarch64Darwin,
            MacOsSandboxReadiness::Enforced,
            MacOsBuildUsersReadiness::Ready,
            MacOsToolchainReadiness::Ready,
        );
        assert!(ready.into_engine().is_ok());
        for not_ready in [
            MacOsBuildReadiness::observed(
                System::Aarch64Darwin,
                MacOsSandboxReadiness::Disabled,
                MacOsBuildUsersReadiness::Ready,
                MacOsToolchainReadiness::Ready,
            ),
            MacOsBuildReadiness::observed(
                System::Aarch64Darwin,
                MacOsSandboxReadiness::FallbackAllowed,
                MacOsBuildUsersReadiness::Ready,
                MacOsToolchainReadiness::Ready,
            ),
            MacOsBuildReadiness::observed(
                System::Aarch64Darwin,
                MacOsSandboxReadiness::Enforced,
                MacOsBuildUsersReadiness::GroupMissing,
                MacOsToolchainReadiness::Ready,
            ),
            MacOsBuildReadiness::observed(
                System::Aarch64Darwin,
                MacOsSandboxReadiness::Enforced,
                MacOsBuildUsersReadiness::UserSetMismatch,
                MacOsToolchainReadiness::Ready,
            ),
            MacOsBuildReadiness::observed(
                System::Aarch64Darwin,
                MacOsSandboxReadiness::Enforced,
                MacOsBuildUsersReadiness::Ready,
                MacOsToolchainReadiness::Missing,
            ),
            MacOsBuildReadiness::observed(
                System::Aarch64Linux,
                MacOsSandboxReadiness::Enforced,
                MacOsBuildUsersReadiness::Ready,
                MacOsToolchainReadiness::Ready,
            ),
        ] {
            assert_eq!(
                not_ready.into_engine().map_err(MacOsError::code),
                Err(MacOsErrorCode::BuildReadinessFailed)
            );
        }
    }

    #[test]
    fn release_plan_is_product_only_ordered_and_never_accepts_passwords() {
        assert_eq!(
            RELEASE_STEPS.first().map(|step| step.target),
            Some(MacOsReleaseTarget::Runtime)
        );
        assert_eq!(
            RELEASE_STEPS.last().map(|step| step.tool),
            Some("/usr/sbin/spctl")
        );
        let rendered = RELEASE_STEPS
            .iter()
            .flat_map(|step| step.arguments)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(rendered.contains("--keychain-profile"));
        assert!(!rendered.contains("--password"));
        assert!(!rendered.contains("/nix/store"));
    }

    struct FakeBackend {
        existing: BTreeSet<&'static str>,
        mutations: Vec<&'static str>,
        rollback: Vec<&'static str>,
        fail_on: Option<&'static str>,
        readiness: MacOsBuildReadiness,
        store_volume: bool,
        rollback_failures: BTreeSet<&'static str>,
        receipt: bool,
    }

    impl FakeBackend {
        fn clean() -> Self {
            Self {
                existing: BTreeSet::new(),
                mutations: Vec::new(),
                rollback: Vec::new(),
                fail_on: None,
                readiness: MacOsBuildReadiness::observed(
                    System::Aarch64Darwin,
                    MacOsSandboxReadiness::Enforced,
                    MacOsBuildUsersReadiness::Ready,
                    MacOsToolchainReadiness::Ready,
                ),
                store_volume: false,
                rollback_failures: BTreeSet::new(),
                receipt: false,
            }
        }

        fn ensure(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            if self.existing.contains(asset.id) {
                return Ok(false);
            }
            self.existing.insert(asset.id);
            self.mutations.push(asset.id);
            if self.fail_on == Some(asset.id) {
                Err(MacOsError::backend_failure())
            } else {
                Ok(true)
            }
        }
    }

    impl MacOsInstallBackend for FakeBackend {
        fn bind_authenticated_installer_payloads(
            &mut self,
            _payloads: &AuthenticatedInstallerPayloads,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn bind_authenticated_nix_config(
            &mut self,
            _config: &AuthenticatedManagedNixConfig,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn bind_authenticated_release_identity(
            &mut self,
            _system: System,
            _release_identity_digest: Digest,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn begin_authenticated_recovery(
            &mut self,
            _mode: crate::MacOsInstallMode,
        ) -> Result<(), MacOsError> {
            Ok(())
        }

        fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn preflight_clean_host(&mut self, _system: System) -> Result<(), MacOsError> {
            if self.fail_on == Some("preflight") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn broker_uid(&mut self) -> Result<u32, MacOsError> {
            Ok(333)
        }
        fn classify_asset(
            &mut self,
            asset: MacOsInstallAsset,
        ) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(if self.existing.contains(asset.id) {
                MacOsAssetPresence::ExactPresent
            } else {
                MacOsAssetPresence::Absent
            })
        }
        fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(if self.store_volume {
                MacOsAssetPresence::ExactPresent
            } else {
                MacOsAssetPresence::Absent
            })
        }
        fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(MacOsAssetPresence::Absent)
        }
        fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(MacOsAssetPresence::Absent)
        }
        fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
            Ok(if self.receipt {
                MacOsAssetPresence::ExactPresent
            } else {
                MacOsAssetPresence::Absent
            })
        }
        fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            self.rollback_asset(asset)
        }
        fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
            self.rollback_store_volume()
        }
        fn recover_services(&mut self) -> Result<(), MacOsError> {
            self.rollback_services()
        }
        fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
            if self.receipt {
                self.receipt = false;
                self.rollback.push("ownership-receipt");
            }
            Ok(())
        }
        fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
            if self.fail_on == Some("release") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
            if self.store_volume {
                return Ok(false);
            }
            self.store_volume = true;
            self.mutations.push("store-volume");
            if self.fail_on == Some("store-volume") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(true)
            }
        }
        fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
            self.store_volume = false;
            self.rollback.push("store-volume");
            if self.rollback_failures.contains("store-volume") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            self.ensure(asset)
        }
        fn install_launchd_plist(
            &mut self,
            asset: MacOsInstallAsset,
            _contents: &'static str,
        ) -> Result<bool, MacOsError> {
            self.ensure(asset)
        }
        fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
            self.ensure(asset)
        }
        fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
            self.mutations.push("runtime");
            if self.fail_on == Some("runtime") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(true)
            }
        }
        fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
            self.rollback.push("runtime");
            if self.rollback_failures.contains("runtime") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
            Ok(())
        }
        fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
            if self.fail_on == Some("codesign") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn activate_services(&mut self) -> Result<bool, MacOsError> {
            self.mutations.push("services");
            if self.fail_on == Some("services") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(true)
            }
        }
        fn rollback_services(&mut self) -> Result<(), MacOsError> {
            self.rollback.push("services");
            if self.rollback_failures.contains("services") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
            if self.fail_on == Some("daemon") {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
        fn observe_build_readiness(
            &mut self,
            _system: System,
        ) -> Result<MacOsBuildReadiness, MacOsError> {
            Ok(self.readiness)
        }
        fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
            if self.fail_on == Some("receipt") {
                Err(MacOsError::backend_failure())
            } else {
                let created = !self.receipt;
                self.receipt = true;
                Ok(created)
            }
        }
        fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
            self.existing.remove(asset.id);
            self.rollback.push(asset.id);
            if self.rollback_failures.contains(asset.id) {
                Err(MacOsError::backend_failure())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn install_is_receipt_last_and_idempotent() -> Result<(), Box<dyn Error>> {
        let mut backend = FakeBackend::clean();
        let report = install_macos(System::Aarch64Darwin, &mut backend)?;
        let product_assets = macos_product_install_assets().count();
        assert_eq!(report.created_artifacts(), product_assets - 1);
        assert_eq!(report.existing_artifacts(), 0);
        let second = install_macos(System::Aarch64Darwin, &mut backend)?;
        assert_eq!(second.created_artifacts(), 0);
        assert_eq!(second.existing_artifacts(), product_assets - 1);
        Ok(())
    }

    #[test]
    fn partial_file_mutation_rolls_back_in_reverse_order() {
        let mut backend = FakeBackend::clean();
        backend.fail_on = Some("helper-plist");
        assert!(install_macos(System::Aarch64Darwin, &mut backend).is_err());
        assert_eq!(backend.rollback.first().copied(), Some("helper-plist"));
        assert_eq!(backend.rollback.last().copied(), Some("broker-group"));
        let helper = backend
            .mutations
            .iter()
            .position(|mutation| *mutation == "helper-binary");
        let nix_root = backend
            .mutations
            .iter()
            .position(|mutation| *mutation == "nix-root");
        let runtime = backend
            .mutations
            .iter()
            .position(|mutation| *mutation == "runtime");
        assert!(matches!(
            (runtime, nix_root, helper),
            (Some(runtime), Some(nix_root), Some(helper))
                if runtime < nix_root && nix_root < helper
        ));
        assert_eq!(
            backend
                .mutations
                .iter()
                .filter(|mutation| **mutation == "runtime")
                .count(),
            1
        );
        assert!(
            !backend.mutations.iter().any(|mutation| matches!(
                *mutation,
                "store-volume" | "daemon-plist" | "nix-config"
            ))
        );
    }

    #[test]
    fn rollback_attempts_every_older_mutation_after_failures() {
        let mut backend = FakeBackend::clean();
        backend.fail_on = Some("receipt");
        backend
            .rollback_failures
            .extend(["services", "runtime", "daemon-plist"]);
        let result = install_macos(System::Aarch64Darwin, &mut backend);
        assert_eq!(
            result.map_err(MacOsError::code),
            Err(MacOsErrorCode::RollbackIncomplete)
        );
        assert_eq!(backend.rollback.first().copied(), Some("services"));
        assert_eq!(backend.rollback.last().copied(), Some("broker-group"));
        assert!(!backend.store_volume);
        assert!(backend.existing.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn getpeereid_authenticates_before_transport_data() -> Result<(), Box<dyn Error>> {
        use nix::unistd::Uid;
        let (server, _client) = UnixStream::pair()?;
        let peer = authenticate_broker_peer(&server, Uid::current().as_raw())?;
        assert_eq!(peer.uid(), Uid::current().as_raw());
        assert_eq!(peer.gid(), nix::unistd::Gid::current().as_raw());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_helper_session_binds_shared_durable_store() -> Result<(), Box<dyn Error>> {
        use nix::unistd::Uid;
        use pkg_nix::{InProcessHelper, InProcessPeer};
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            sync::atomic::{AtomicU64, Ordering},
        };

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pkg-macos-roots-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        let uid = Uid::current().as_raw();
        let root_store = MacOsRootSetStore::new_at(path.clone(), uid)?;
        let helper = InProcessHelper::new(uid)?;
        let authenticated = helper.connect(InProcessPeer::authenticated_uid(uid))?;
        let session = MacOsHelperSession::new(authenticated, root_store);
        assert!(format!("{session:?}").starts_with("MacOsHelperSession("));
        fs::remove_dir(path)?;
        Ok(())
    }
}
