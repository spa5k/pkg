use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_nix::{DetectionDisposition, FindingKind, System, detect_unmanaged_nix};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pkg-managed-detect-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.root.join(relative)).unwrap();
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn scan(fixture: &Fixture, system: System) -> pkg_nix::DetectionReport {
    detect_unmanaged_nix(fixture.root(), system, &[], &[])
}

#[test]
fn clean_root_is_clean() {
    let fixture = Fixture::new();
    let report = scan(&fixture, System::Aarch64Linux);
    assert_eq!(report.disposition(), DetectionDisposition::Clean);
    assert!(report.findings().is_empty());
}

#[test]
fn stray_nix_tree_refuses_with_stable_redacted_signals() {
    let fixture = Fixture::new();
    fixture.mkdir("nix/store/aaaaaaaa-example");
    let report = scan(&fixture, System::Aarch64Linux);
    assert_eq!(report.disposition(), DetectionDisposition::Refuse);
    assert!(report.has_definite_evidence());
    assert!(
        report
            .findings()
            .iter()
            .any(|finding| finding.id() == "NIX_ROOT")
    );
    assert!(
        report
            .findings()
            .iter()
            .any(|finding| finding.id() == "NIX_STORE_POPULATED")
    );
    for finding in report.findings() {
        assert!(!finding.detail().contains(fixture.root().to_str().unwrap()));
    }
}

#[test]
fn ownership_marker_alone_never_authorizes_installation() {
    let fixture = Fixture::new();
    fixture.write("var/lib/pkg/.managed-nix", r#"{"managed":true}"#);
    let report = scan(&fixture, System::X8664Linux);
    assert_eq!(report.disposition(), DetectionDisposition::Refuse);
    let marker = report
        .findings()
        .iter()
        .find(|finding| finding.id() == "PKG_OWNERSHIP_MARKER")
        .unwrap();
    assert_eq!(marker.kind(), FindingKind::OwnershipMarker);
    assert!(report.has_ownership_claim());
    assert!(!report.has_unmanaged_evidence());
}

#[test]
fn ownership_receipt_is_a_claim_but_never_authorizes_installation() {
    let fixture = Fixture::new();
    fixture.mkdir("nix/store");
    fixture.write(
        "var/lib/pkg/managed-nix/ownership-v1.json",
        r#"{"schemaVersion":1}"#,
    );
    let report = scan(&fixture, System::X8664Linux);
    assert_eq!(report.disposition(), DetectionDisposition::Refuse);
    assert!(report.has_unmanaged_evidence());
    assert!(report.has_ownership_claim());
    assert!(report.findings().iter().any(|finding| {
        finding.id() == "PKG_OWNERSHIP_RECEIPT" && finding.kind() == FindingKind::OwnershipMarker
    }));
}

#[test]
fn pkg_broker_configuration_suppresses_removal_without_proving_ownership() {
    let fixture = Fixture::new();
    fixture.mkdir("nix/store");
    fixture.write(
        "etc/nix/nix.conf",
        "allowed-users = pkg-nix-broker\ntrusted-users = root\n",
    );
    let report = scan(&fixture, System::Aarch64Darwin);
    assert!(report.has_unmanaged_evidence());
    assert!(report.has_ownership_claim());
    assert!(report.findings().iter().any(|finding| {
        finding.id() == "PKG_BROKER_CONFIGURATION" && finding.kind() == FindingKind::OwnershipMarker
    }));
}

#[test]
fn environment_detection_checks_names_without_values() {
    let fixture = Fixture::new();
    let keys = [OsString::from("PATH"), OsString::from("NIX_REMOTE")];
    let report = detect_unmanaged_nix(fixture.root(), System::Aarch64Darwin, &[], &keys);
    assert!(
        report
            .findings()
            .iter()
            .any(|finding| finding.id() == "NIX_ENVIRONMENT")
    );
}

#[test]
fn platform_service_checks_do_not_cross_operating_systems() {
    let fixture = Fixture::new();
    fixture.write("etc/systemd/system/nix-daemon.service", "unit");
    fixture.write("Library/LaunchDaemons/org.nixos.nix-daemon.plist", "plist");

    let linux = scan(&fixture, System::X8664Linux);
    assert!(
        linux
            .findings()
            .iter()
            .any(|finding| finding.id() == "SYSTEMD_UNIT")
    );
    assert!(
        !linux
            .findings()
            .iter()
            .any(|finding| finding.id() == "LAUNCHD_PLIST")
    );

    let macos = scan(&fixture, System::Aarch64Darwin);
    assert!(
        macos
            .findings()
            .iter()
            .any(|finding| finding.id() == "LAUNCHD_PLIST")
    );
    assert!(
        !macos
            .findings()
            .iter()
            .any(|finding| finding.id() == "SYSTEMD_UNIT")
    );
}

#[test]
fn build_users_profiles_mounts_and_binaries_are_detected() {
    let fixture = Fixture::new();
    fixture.write(
        "etc/passwd",
        "_nixbld1:*:351:394::/var/empty:/usr/bin/false\n",
    );
    fixture.write("etc/group", "nixbld:*:394:_nixbld1\n");
    fixture.write("etc/fstab", "UUID=example /nix apfs rw 0 0\n");
    fixture.write("Users/test/.nix-profile", "profile");
    fixture.write("usr/local/bin/nix", "binary");

    let report = scan(&fixture, System::Aarch64Darwin);
    for expected in [
        "NIXBLD_USERS",
        "NIXBLD_GROUP",
        "FSTAB_NIX",
        "USER_NIX_PROFILE",
        "NIX_BINARY",
    ] {
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.id() == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn missing_scan_root_refuses_as_ambiguous() {
    let fixture = Fixture::new();
    let missing = fixture.root().join("missing");
    let report = detect_unmanaged_nix(&missing, System::X8664Linux, &[], &[]);
    assert_eq!(report.disposition(), DetectionDisposition::Refuse);
    assert!(!report.has_definite_evidence());
    assert_eq!(report.findings()[0].kind(), FindingKind::Ambiguous);
}

#[test]
fn uninspectable_service_and_binary_paths_remain_ambiguity_only() {
    let fixture = Fixture::new();
    fixture.write("etc/systemd/system", "not a directory");
    fixture.write("blocked-bin", "not a directory");
    let path_entries = [fixture.root().join("blocked-bin")];
    let report = detect_unmanaged_nix(fixture.root(), System::X8664Linux, &path_entries, &[]);
    assert!(!report.has_definite_evidence());
    assert!(report.findings().iter().any(|finding| {
        finding.id() == "SYSTEMD_INSPECTION_FAILED" && finding.kind() == FindingKind::Ambiguous
    }));
    assert!(report.findings().iter().any(|finding| {
        finding.id() == "BINARY_PATH_UNREADABLE" && finding.kind() == FindingKind::Ambiguous
    }));
    assert!(
        !report
            .findings()
            .iter()
            .any(|finding| finding.id() == "SYSTEMD_UNIT" || finding.id() == "NIX_BINARY")
    );
}

#[cfg(unix)]
#[test]
fn unreadable_home_profile_probe_is_ambiguity_not_foreign_evidence() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new();
    // UID 0 bypasses mode-bit denial, so this fixture cannot model the
    // unprivileged early scan in a root-run container.
    if fs::metadata(fixture.root()).unwrap().uid() == 0 {
        return;
    }
    fixture.mkdir("home/test");
    let home = fixture.root().join("home/test");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o000)).unwrap();
    let report = scan(&fixture, System::X8664Linux);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(!report.has_definite_evidence());
    assert!(report.findings().iter().any(|finding| {
        finding.id() == "HOME_PROFILE_UNREADABLE" && finding.kind() == FindingKind::Ambiguous
    }));
    assert!(
        !report
            .findings()
            .iter()
            .any(|finding| finding.id() == "USER_NIX_PROFILE")
    );
}

#[cfg(unix)]
#[test]
fn unreadable_nix_root_and_marker_are_not_definite_evidence() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new();
    // UID 0 bypasses mode-bit denial, so this fixture cannot model the
    // unprivileged early scan in a root-run container.
    if fs::metadata(fixture.root()).unwrap().uid() == 0 {
        return;
    }
    fixture.mkdir("nix");
    fixture.write("var/lib/pkg/.managed-nix", "marker");
    let nix = fixture.root().join("nix");
    let marker_parent = fixture.root().join("var/lib/pkg");
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&marker_parent, fs::Permissions::from_mode(0o000)).unwrap();
    let report = scan(&fixture, System::X8664Linux);
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&marker_parent, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(!report.has_definite_evidence());
    for expected in [
        "NIX_ROOT_UNREADABLE",
        "PKG_OWNERSHIP_MARKER_UNREADABLE",
        "PKG_OWNERSHIP_RECEIPT_UNREADABLE",
    ] {
        assert!(report.findings().iter().any(|finding| {
            finding.id() == expected && finding.kind() == FindingKind::Ambiguous
        }));
    }
}
