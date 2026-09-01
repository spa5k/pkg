//! Tests for the macOS platform module.

use super::*;
use super::{assets::*, launchd::*};

use std::collections::{BTreeSet, HashSet};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one closed asset-manifest assertion table"
)]
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
    assert!(
        MacOsLaunchdAssets::BROKER.contains(
            "<key>HOME</key><string>/Library/Application Support/pkg/broker-home</string>"
        )
    );
    assert!(MacOsLaunchdAssets::BROKER.contains(
        "<key>TMPDIR</key><string>/Library/Application Support/pkg/broker-home/tmp</string>"
    ));
    assert!(MacOsLaunchdAssets::ROOT_HELPER.contains("pkg-root-helper"));
    assert!(
        MacOsLaunchdAssets::ROOT_HELPER.contains(
            "<key>HOME</key><string>/Library/Application Support/pkg/helper-home</string>"
        )
    );
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
        _mode: crate::InstallMode,
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
    fn classify_asset(&mut self, asset: MacOsInstallAsset) -> Result<AssetPresence, MacOsError> {
        Ok(if self.existing.contains(asset.id) {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
        })
    }
    fn classify_store_volume(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(if self.store_volume {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
        })
    }
    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::Absent)
    }
    fn classify_services(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::Absent)
    }
    fn classify_ownership_receipt(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(if self.receipt {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
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
        !backend
            .mutations
            .iter()
            .any(|mutation| matches!(*mutation, "store-volume" | "daemon-plist" | "nix-config"))
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
