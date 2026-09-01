//! Read-only, fail-closed detection of pre-existing Nix artifacts.
//!
//! This module deliberately does not decide that an existing installation is
//! managed by `pkg`. PR-12 must authenticate an ownership receipt and verify
//! the complete managed-artifact manifest before it may make that claim. Until
//! then, every observed Nix artifact is foreign or ambiguous and installation
//! is refused.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use pkg_core::System;

const MAX_INSPECTED_FILE_BYTES: u64 = 1_048_576;
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const ACCOUNT_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a host signal prevents installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// Definite evidence of a pre-existing Nix installation.
    Unmanaged,
    /// State could not be inspected safely enough to prove absence.
    Ambiguous,
    /// A product marker exists but is not sufficient proof of ownership.
    OwnershipMarker,
}

/// One bounded, product-owned detection signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionFinding {
    id: &'static str,
    kind: FindingKind,
    detail: &'static str,
}

impl DetectionFinding {
    /// Stable signal identifier suitable for logs and public reports.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Classification used to select safe remediation.
    #[must_use]
    pub const fn kind(&self) -> FindingKind {
        self.kind
    }

    /// Fixed, redacted explanation that never includes a host path or value.
    #[must_use]
    pub const fn detail(&self) -> &'static str {
        self.detail
    }
}

/// Install/preflight decision produced by the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionDisposition {
    /// No Nix or ambiguous artifacts were observed by this scan.
    Clean,
    /// At least one definite, ambiguous, or marker signal requires refusal.
    Refuse,
}

/// Deterministic result of one read-only host scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionReport {
    findings: Vec<DetectionFinding>,
}

impl DetectionReport {
    /// Whether installation may proceed to the privileged recheck.
    #[must_use]
    pub const fn disposition(&self) -> DetectionDisposition {
        if self.findings.is_empty() {
            DetectionDisposition::Clean
        } else {
            DetectionDisposition::Refuse
        }
    }

    /// Ordered, deduplicated detection signals.
    #[must_use]
    pub fn findings(&self) -> &[DetectionFinding] {
        &self.findings
    }

    /// Whether the report contains evidence beyond ambiguity alone.
    #[must_use]
    pub fn has_definite_evidence(&self) -> bool {
        self.findings.iter().any(|finding| {
            matches!(
                finding.kind,
                FindingKind::Unmanaged | FindingKind::OwnershipMarker
            )
        })
    }

    /// Whether a readable or unreadable pkg ownership claim needs authentication.
    #[must_use]
    pub fn has_ownership_claim(&self) -> bool {
        self.findings.iter().any(|finding| {
            matches!(finding.kind, FindingKind::OwnershipMarker)
                || matches!(
                    finding.id,
                    "PKG_OWNERSHIP_MARKER_UNREADABLE" | "PKG_OWNERSHIP_RECEIPT_UNREADABLE"
                )
        })
    }

    /// Whether definite foreign Nix artifacts were observed.
    #[must_use]
    pub fn has_unmanaged_evidence(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.kind == FindingKind::Unmanaged)
    }
}

/// Scan a host root for pre-existing Nix artifacts without mutating it.
///
/// `path_entries` and `environment_keys` are passed explicitly so tests do not
/// depend on the test runner's environment. Environment values are never
/// accepted or inspected. A clean unprivileged result remains advisory: the
/// signed installer/helper must repeat the full scan with privilege immediately
/// before mutation.
#[must_use]
pub fn detect_unmanaged_nix(
    root: &Path,
    system: System,
    path_entries: &[PathBuf],
    environment_keys: &[OsString],
) -> DetectionReport {
    let mut scanner = Scanner {
        root,
        system,
        findings: Vec::new(),
    };
    if !scanner.check_scan_root() {
        return scanner.finish();
    }
    scanner.check_nix_tree();
    scanner.check_nix_config();
    scanner.check_service_files();
    scanner.check_mount_configuration();
    scanner.check_build_users();
    scanner.check_profiles();
    scanner.check_binaries(path_entries);
    scanner.check_environment(environment_keys);
    scanner.check_ownership_markers();
    scanner.finish()
}

struct Scanner<'a> {
    root: &'a Path,
    system: System,
    findings: Vec<DetectionFinding>,
}

impl Scanner<'_> {
    fn finish(self) -> DetectionReport {
        DetectionReport {
            findings: self.findings,
        }
    }

    fn record(&mut self, id: &'static str, kind: FindingKind, detail: &'static str) {
        if self.findings.iter().all(|finding| finding.id != id) {
            self.findings.push(DetectionFinding { id, kind, detail });
        }
    }

    fn check_scan_root(&mut self) -> bool {
        match path_state(self.root) {
            PathState::Directory => match fs::read_dir(self.root) {
                Ok(_) => true,
                Err(_) => {
                    self.record(
                        "SCAN_ROOT_UNREADABLE",
                        FindingKind::Ambiguous,
                        "the host root cannot be read; absence of Nix cannot be established",
                    );
                    false
                }
            },
            PathState::Missing => {
                self.record(
                    "SCAN_ROOT_MISSING",
                    FindingKind::Ambiguous,
                    "the requested host root does not exist",
                );
                false
            }
            PathState::Symlink => {
                self.record(
                    "SCAN_ROOT_SYMLINK",
                    FindingKind::Ambiguous,
                    "the requested host root is a symlink and is not trusted",
                );
                false
            }
            PathState::Unreadable | PathState::File | PathState::Other => {
                self.record(
                    "SCAN_ROOT_INVALID",
                    FindingKind::Ambiguous,
                    "the requested host root is not an inspectable directory",
                );
                false
            }
        }
    }

    fn check_nix_tree(&mut self) {
        match path_state(&self.at("nix")) {
            PathState::Missing => {}
            PathState::Symlink => self.record(
                "NIX_ROOT_SYMLINK",
                FindingKind::Unmanaged,
                "a symlink or synthetic mount occupies the Nix root",
            ),
            PathState::Directory if fs::read_dir(self.at("nix")).is_err() => self.record(
                "NIX_ROOT_UNREADABLE",
                FindingKind::Ambiguous,
                "the Nix root exists but cannot be inspected",
            ),
            PathState::Unreadable => self.record(
                "NIX_ROOT_UNREADABLE",
                FindingKind::Ambiguous,
                "the Nix root exists but cannot be inspected",
            ),
            _ => self.record(
                "NIX_ROOT",
                FindingKind::Unmanaged,
                "a pre-existing Nix root is present",
            ),
        }

        let store = self.at("nix/store");
        match path_state(&store) {
            PathState::Missing => {}
            PathState::Directory => match directory_has_entry(&store) {
                Ok(true) => self.record(
                    "NIX_STORE_POPULATED",
                    FindingKind::Unmanaged,
                    "a populated Nix store is present",
                ),
                Ok(false) => self.record(
                    "NIX_STORE_EMPTY",
                    FindingKind::Unmanaged,
                    "an empty but pre-existing Nix store is present",
                ),
                Err(_) => self.record(
                    "NIX_STORE_UNREADABLE",
                    FindingKind::Ambiguous,
                    "the Nix store exists but cannot be inspected",
                ),
            },
            PathState::Unreadable => self.record(
                "NIX_STORE_UNREADABLE",
                FindingKind::Ambiguous,
                "the Nix store exists but cannot be inspected",
            ),
            _ => self.record(
                "NIX_STORE_INVALID",
                FindingKind::Unmanaged,
                "the Nix store path exists with an unexpected file type",
            ),
        }

        for (relative, id, detail) in [
            ("nix/var/nix", "NIX_VAR", "Nix daemon state is present"),
            (
                "nix/var/nix/daemon-socket/socket",
                "NIX_DAEMON_SOCKET",
                "a Nix daemon socket is present",
            ),
            (
                "nix/var/nix/db",
                "NIX_DB",
                "a Nix store database is present",
            ),
            (
                "nix/var/nix/profiles",
                "NIX_PROFILES",
                "a Nix profile tree is present",
            ),
        ] {
            self.record_presence(relative, id, detail);
        }
    }

    fn check_nix_config(&mut self) {
        self.record_presence(
            "etc/nix",
            "ETC_NIX_DIR",
            "a machine-wide Nix configuration directory is present",
        );
        self.record_presence(
            "etc/nix/nix.conf",
            "NIX_CONF",
            "a machine-wide Nix configuration file is present",
        );
        if let BoundedFile::Contents(bytes) = read_bounded(&self.at("etc/nix/nix.conf"))
            && any_text_line(&bytes, |line| {
                let line = line.trim();
                !line.starts_with('#')
                    && line.split_once('=').is_some_and(|(key, value)| {
                        matches!(key.trim(), "allowed-users" | "trusted-users")
                            && value
                                .split_whitespace()
                                .any(|word| word == "pkg-nix-broker")
                    })
            })
        {
            self.record(
                "PKG_BROKER_CONFIGURATION",
                FindingKind::OwnershipMarker,
                "Nix configuration contains a pkg-specific broker identity that requires authentication",
            );
        }
    }

    fn check_service_files(&mut self) {
        if self.is_linux() {
            let mut found = false;
            for directory in [
                "etc/systemd/system",
                "lib/systemd/system",
                "usr/lib/systemd/system",
                "run/systemd/system",
            ] {
                for name in [
                    "nix-daemon.service",
                    "nix-daemon.socket",
                    "nix.service",
                    "nix-store.service",
                ] {
                    match path_state(&self.at(directory).join(name)) {
                        PathState::Missing => {}
                        PathState::Unreadable => self.record(
                            "SYSTEMD_INSPECTION_FAILED",
                            FindingKind::Ambiguous,
                            "a systemd unit path cannot be inspected",
                        ),
                        _ => found = true,
                    }
                }
            }
            if found {
                self.record(
                    "SYSTEMD_UNIT",
                    FindingKind::Unmanaged,
                    "a Nix systemd unit is installed",
                );
            }
            self.record_presence(
                "etc/tmpfiles.d/nix-daemon.conf",
                "SYSTEMD_TMPFILES",
                "Nix systemd-tmpfiles configuration is present",
            );
        } else {
            for directory in ["Library/LaunchDaemons", "Library/LaunchAgents"] {
                self.check_launchd_directory(directory);
            }
        }
    }

    fn check_launchd_directory(&mut self, relative: &str) {
        let directory = self.at(relative);
        let entries = match bounded_directory_entries(&directory) {
            Ok(Some(entries)) => entries,
            Ok(None) => return,
            Err(_) => {
                self.record(
                    "LAUNCHD_DIR_UNREADABLE",
                    FindingKind::Ambiguous,
                    "a launchd job directory cannot be inspected",
                );
                return;
            }
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                self.record(
                    "LAUNCHD_NAME_INVALID",
                    FindingKind::Ambiguous,
                    "a launchd job has an unrecognizable name",
                );
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if lower.contains("org.nixos.")
                || lower.starts_with("nix-")
                || lower.starts_with("nix.")
                || lower.starts_with("_nixbld")
            {
                self.record(
                    "LAUNCHD_PLIST",
                    FindingKind::Unmanaged,
                    "a Nix launchd job is installed",
                );
                break;
            }
        }
    }

    fn check_mount_configuration(&mut self) {
        match read_bounded(&self.at("etc/synthetic.conf")) {
            BoundedFile::Missing => {}
            BoundedFile::Contents(bytes) => {
                if any_text_line(&bytes, |line| {
                    let line = line.trim();
                    !line.starts_with('#')
                        && line
                            .split_whitespace()
                            .next()
                            .is_some_and(|word| word == "nix")
                }) {
                    self.record(
                        "SYNTHETIC_CONF_NIX",
                        FindingKind::Unmanaged,
                        "the synthetic filesystem configuration defines a Nix root",
                    );
                }
            }
            BoundedFile::Unreadable => self.record(
                "SYNTHETIC_CONF_UNREADABLE",
                FindingKind::Ambiguous,
                "the synthetic filesystem configuration cannot be inspected",
            ),
        }

        match read_bounded(&self.at("etc/fstab")) {
            BoundedFile::Missing => {}
            BoundedFile::Contents(bytes) => {
                if any_text_line(&bytes, |line| {
                    let line = line.trim();
                    !line.starts_with('#') && line.split_whitespace().any(|word| word == "/nix")
                }) {
                    self.record(
                        "FSTAB_NIX",
                        FindingKind::Unmanaged,
                        "the filesystem table references a Nix mount",
                    );
                }
            }
            BoundedFile::Unreadable => self.record(
                "FSTAB_UNREADABLE",
                FindingKind::Ambiguous,
                "the filesystem table cannot be inspected",
            ),
        }
    }

    fn check_build_users(&mut self) {
        match read_bounded(&self.at("etc/passwd")) {
            BoundedFile::Missing => {}
            BoundedFile::Contents(bytes) => {
                if any_text_line(&bytes, |line| {
                    line.split_once(':')
                        .is_some_and(|(name, _)| is_nix_build_user(name))
                }) {
                    self.record(
                        "NIXBLD_USERS",
                        FindingKind::Unmanaged,
                        "one or more Nix build users are configured",
                    );
                }
            }
            BoundedFile::Unreadable => self.record(
                "PASSWD_UNREADABLE",
                FindingKind::Ambiguous,
                "the user database cannot be inspected for Nix build users",
            ),
        }
        match read_bounded(&self.at("etc/group")) {
            BoundedFile::Missing => {}
            BoundedFile::Contents(bytes) => {
                if any_text_line(&bytes, |line| {
                    line.split_once(':')
                        .is_some_and(|(name, _)| matches!(name, "nixbld" | "_nixbld"))
                }) {
                    self.record(
                        "NIXBLD_GROUP",
                        FindingKind::Unmanaged,
                        "a Nix build-users group is configured",
                    );
                }
            }
            BoundedFile::Unreadable => self.record(
                "GROUP_UNREADABLE",
                FindingKind::Ambiguous,
                "the group database cannot be inspected for a Nix build group",
            ),
        }
        self.check_system_account_database();
    }

    fn check_system_account_database(&mut self) {
        if self.root != Path::new("/") {
            return;
        }
        if self.is_linux() {
            self.check_database(&GETENT_PASSWD_CHECK);
            self.check_database(&GETENT_GROUP_CHECK);
        } else {
            self.check_database(&DSCL_USERS_CHECK);
            self.check_database(&DSCL_GROUPS_CHECK);
        }
    }

    fn check_database(&mut self, check: &DatabaseCheck) {
        match run_bounded_command(Path::new(check.tool), check.arguments) {
            CommandOutput::Missing => {
                self.record(check.missing.0, FindingKind::Ambiguous, check.missing.1);
            }
            CommandOutput::Contents(bytes)
                if (check.matches_build)(&bytes, check.colon_separated) =>
            {
                self.record(check.present.0, FindingKind::Unmanaged, check.present.1);
            }
            CommandOutput::Contents(_) => {}
            CommandOutput::Failed => {
                self.record(check.failed.0, FindingKind::Ambiguous, check.failed.1);
            }
        }
    }

    fn check_profiles(&mut self) {
        self.record_presence(
            "nix/var/nix/profiles/default",
            "NIX_DEFAULT_PROFILE",
            "a machine-wide Nix profile is present",
        );
        if self.is_linux() {
            self.check_home(Path::new("root"));
            self.check_home_container(Path::new("home"));
        } else {
            self.check_home(Path::new("var/root"));
            self.check_home_container(Path::new("Users"));
        }
    }

    fn check_home_container(&mut self, relative: &Path) {
        let directory = self.root.join(relative);
        let entries = match bounded_directory_entries(&directory) {
            Ok(Some(entries)) => entries,
            Ok(None) => return,
            Err(_) => {
                self.record(
                    "HOME_ROOT_UNREADABLE",
                    FindingKind::Ambiguous,
                    "a standard home root cannot be inspected for Nix profiles",
                );
                return;
            }
        };
        for entry in entries {
            let Some(name) = entry.file_name() else {
                continue;
            };
            let relative_home = relative.join(name);
            self.check_home(&relative_home);
        }
    }

    fn check_home(&mut self, relative: &Path) {
        let home = self.root.join(relative);
        match path_state(&home) {
            PathState::Missing => return,
            PathState::Symlink if self.safe_macos_home_alias(relative) => return,
            PathState::Symlink | PathState::Unreadable => {
                self.record(
                    "HOME_UNINSPECTABLE",
                    FindingKind::Ambiguous,
                    "a home directory cannot be safely inspected for Nix profiles",
                );
                return;
            }
            PathState::Directory => {}
            PathState::File | PathState::Other => return,
        }
        let mut found = false;
        for name in [".nix-profile", ".nix-defexpr", ".nix-channels"] {
            match path_state(&home.join(name)) {
                PathState::Missing => {}
                PathState::Unreadable => self.record(
                    "HOME_PROFILE_UNREADABLE",
                    FindingKind::Ambiguous,
                    "a home directory cannot be fully inspected for Nix profiles",
                ),
                _ => found = true,
            }
        }
        if found {
            self.record(
                "USER_NIX_PROFILE",
                FindingKind::Unmanaged,
                "a user Nix profile or channel artifact is present",
            );
        }
    }

    fn safe_macos_home_alias(&self, relative: &Path) -> bool {
        if !matches!(self.system, System::X8664Darwin | System::Aarch64Darwin) {
            return false;
        }
        let Some(parent) = relative.parent() else {
            return false;
        };
        let home = self.root.join(relative);
        let Ok(target) = fs::read_link(&home) else {
            return false;
        };
        let target = if target.is_absolute() {
            let Ok(target) = target.strip_prefix(Path::new("/")) else {
                return false;
            };
            target.to_path_buf()
        } else {
            parent.join(target)
        };
        if target == relative
            || target.parent() != Some(parent)
            || target
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return false;
        }
        let Ok(parent_metadata) = fs::symlink_metadata(self.root.join(parent)) else {
            return false;
        };
        let Ok(root_metadata) = fs::symlink_metadata(self.root) else {
            return false;
        };
        let Ok(link_metadata) = fs::symlink_metadata(&home) else {
            return false;
        };
        let Ok(target_metadata) = fs::symlink_metadata(self.root.join(&target)) else {
            return false;
        };
        let Ok(followed_metadata) = fs::metadata(&home) else {
            return false;
        };
        root_metadata.is_dir()
            && root_metadata.mode() & 0o022 == 0
            && parent_metadata.is_dir()
            && parent_metadata.mode() & 0o022 == 0
            && link_metadata.file_type().is_symlink()
            && parent_metadata.uid() == root_metadata.uid()
            && link_metadata.uid() == root_metadata.uid()
            && target_metadata.is_dir()
            && !target_metadata.file_type().is_symlink()
            && followed_metadata.dev() == target_metadata.dev()
            && followed_metadata.ino() == target_metadata.ino()
    }

    fn check_binaries(&mut self, path_entries: &[PathBuf]) {
        let binary_names = ["nix", "nix-daemon", "nix-store", "nix-env", "nix-build"];
        let mut found = false;
        for directory in ["bin", "usr/bin", "usr/local/bin", "opt/homebrew/bin"] {
            for name in binary_names {
                match path_state(&self.at(directory).join(name)) {
                    PathState::Missing => {}
                    PathState::Unreadable => self.record(
                        "BINARY_PATH_UNREADABLE",
                        FindingKind::Ambiguous,
                        "a binary search path cannot be inspected for Nix commands",
                    ),
                    _ => found = true,
                }
            }
        }
        for directory in path_entries {
            if !directory.is_absolute() {
                self.record(
                    "PATH_ENTRY_RELATIVE",
                    FindingKind::Ambiguous,
                    "a relative PATH entry prevents a trustworthy Nix binary scan",
                );
                continue;
            }
            for name in binary_names {
                match path_state(&directory.join(name)) {
                    PathState::Missing => {}
                    PathState::Unreadable => self.record(
                        "BINARY_PATH_UNREADABLE",
                        FindingKind::Ambiguous,
                        "a binary search path cannot be inspected for Nix commands",
                    ),
                    _ => found = true,
                }
            }
        }
        if found {
            self.record(
                "NIX_BINARY",
                FindingKind::Unmanaged,
                "a Nix command is installed or reachable on PATH",
            );
        }
    }

    fn check_environment(&mut self, keys: &[OsString]) {
        let mut nix_key = false;
        for key in keys {
            let Some(key) = key.to_str() else {
                self.record(
                    "ENV_KEY_INVALID",
                    FindingKind::Ambiguous,
                    "an environment key cannot be inspected safely",
                );
                continue;
            };
            nix_key |= key.starts_with("NIX_") || key == "IN_NIX_SHELL";
        }
        if nix_key {
            self.record(
                "NIX_ENVIRONMENT",
                FindingKind::Unmanaged,
                "one or more Nix environment variables are present",
            );
        }
    }

    fn check_ownership_markers(&mut self) {
        for relative in [
            "var/lib/pkg/.managed-nix",
            "Library/Application Support/pkg/.managed-nix",
        ] {
            match path_state(&self.at(relative)) {
                PathState::Missing => {}
                PathState::Unreadable => self.record(
                    "PKG_OWNERSHIP_MARKER_UNREADABLE",
                    FindingKind::Ambiguous,
                    "a possible pkg ownership marker cannot be inspected",
                ),
                _ => self.record(
                    "PKG_OWNERSHIP_MARKER",
                    FindingKind::OwnershipMarker,
                    "a pkg ownership marker is present but cannot authorize takeover",
                ),
            }
        }
        for relative in [
            "var/lib/pkg/managed-nix/ownership-v1.json",
            "Library/Application Support/pkg/managed-nix/ownership-v1.json",
        ] {
            match path_state(&self.at(relative)) {
                PathState::Missing => {}
                PathState::Unreadable => self.record(
                    "PKG_OWNERSHIP_RECEIPT_UNREADABLE",
                    FindingKind::Ambiguous,
                    "a possible pkg ownership receipt cannot be inspected",
                ),
                _ => self.record(
                    "PKG_OWNERSHIP_RECEIPT",
                    FindingKind::OwnershipMarker,
                    "a pkg ownership receipt is present but has not been authenticated",
                ),
            }
        }
    }

    fn record_presence(&mut self, relative: &str, id: &'static str, detail: &'static str) {
        match path_state(&self.at(relative)) {
            PathState::Missing => {}
            PathState::Unreadable => self.record(
                id,
                FindingKind::Ambiguous,
                "a possible Nix artifact exists but cannot be inspected",
            ),
            _ => self.record(id, FindingKind::Unmanaged, detail),
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    const fn is_linux(&self) -> bool {
        matches!(self.system, System::X8664Linux | System::Aarch64Linux)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathState {
    Missing,
    Directory,
    File,
    Symlink,
    Other,
    Unreadable,
}

fn path_state(path: &Path) -> PathState {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => PathState::Symlink,
        Ok(metadata) if metadata.is_dir() => PathState::Directory,
        Ok(metadata) if metadata.is_file() => PathState::File,
        Ok(_) => PathState::Other,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathState::Missing,
        Err(_) => PathState::Unreadable,
    }
}

fn directory_has_entry(path: &Path) -> io::Result<bool> {
    Ok(fs::read_dir(path)?.next().transpose()?.is_some())
}

fn bounded_directory_entries(path: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    match path_state(path) {
        PathState::Missing | PathState::File | PathState::Other => return Ok(None),
        PathState::Symlink | PathState::Unreadable => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "directory cannot be safely inspected",
            ));
        }
        PathState::Directory => {}
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory entry bound exceeded",
            ));
        }
        entries.push(entry?.path());
    }
    Ok(Some(entries))
}

enum BoundedFile {
    Missing,
    Contents(Vec<u8>),
    Unreadable,
}

fn read_bounded(path: &Path) -> BoundedFile {
    match path_state(path) {
        PathState::Missing => return BoundedFile::Missing,
        PathState::File => {}
        PathState::Directory | PathState::Symlink | PathState::Other | PathState::Unreadable => {
            return BoundedFile::Unreadable;
        }
    }
    let Ok(file) = File::open(path) else {
        return BoundedFile::Unreadable;
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_INSPECTED_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_INSPECTED_FILE_BYTES
    {
        return BoundedFile::Unreadable;
    }
    BoundedFile::Contents(bytes)
}

fn any_text_line(bytes: &[u8], predicate: impl FnMut(&str) -> bool) -> bool {
    String::from_utf8_lossy(bytes).lines().any(predicate)
}

fn is_nix_build_user(name: &str) -> bool {
    ["nixbld", "_nixbld"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn account_output_has_build_user(bytes: &[u8], colon_separated: bool) -> bool {
    any_text_line(bytes, |line| {
        let name = if colon_separated {
            line.split_once(':').map(|(name, _)| name)
        } else {
            line.split_whitespace().next()
        };
        name.is_some_and(is_nix_build_user)
    })
}

fn account_output_has_build_group(bytes: &[u8], colon_separated: bool) -> bool {
    any_text_line(bytes, |line| {
        let name = if colon_separated {
            line.split_once(':').map(|(name, _)| name)
        } else {
            line.split_whitespace().next()
        };
        name.is_some_and(|name| matches!(name, "nixbld" | "_nixbld"))
    })
}

enum CommandOutput {
    Missing,
    Contents(Vec<u8>),
    Failed,
}

fn run_bounded_command(program: &Path, arguments: &[&str]) -> CommandOutput {
    if !program.exists() {
        return CommandOutput::Missing;
    }
    let Ok(mut child) = Command::new(program)
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return CommandOutput::Failed;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return CommandOutput::Failed;
    };
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_INSPECTED_FILE_BYTES + 1)
            .read_to_end(&mut bytes);
        (result, bytes)
    });
    let deadline = Instant::now() + ACCOUNT_QUERY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Ok((read_result, bytes)) = reader.join() else {
        return CommandOutput::Failed;
    };
    if !status.is_some_and(|status| status.success())
        || read_result.is_err()
        || bytes.len() as u64 > MAX_INSPECTED_FILE_BYTES
    {
        CommandOutput::Failed
    } else {
        CommandOutput::Contents(bytes)
    }
}

/// Configuration for one system account database query.
struct DatabaseCheck {
    tool: &'static str,
    arguments: &'static [&'static str],
    missing: (&'static str, &'static str),
    present: (&'static str, &'static str),
    failed: (&'static str, &'static str),
    matches_build: fn(&[u8], bool) -> bool,
    colon_separated: bool,
}

const GETENT_PASSWD_CHECK: DatabaseCheck = DatabaseCheck {
    tool: "/usr/bin/getent",
    arguments: &["passwd"],
    missing: (
        "GETENT_MISSING",
        "the Linux account database tool is unavailable",
    ),
    present: (
        "GETENT_NIXBLD_USER",
        "the system account database reports one or more Nix build users",
    ),
    failed: (
        "GETENT_PASSWD_QUERY_FAILED",
        "the system account database could not be checked for Nix build users",
    ),
    matches_build: account_output_has_build_user,
    colon_separated: true,
};

const GETENT_GROUP_CHECK: DatabaseCheck = DatabaseCheck {
    tool: "/usr/bin/getent",
    arguments: &["group"],
    missing: (
        "GETENT_MISSING",
        "the Linux account database tool is unavailable",
    ),
    present: (
        "GETENT_NIXBLD_GROUP",
        "the system account database reports a Nix build-users group",
    ),
    failed: (
        "GETENT_GROUP_QUERY_FAILED",
        "the system account database could not be checked for a Nix build group",
    ),
    matches_build: account_output_has_build_group,
    colon_separated: true,
};

const DSCL_USERS_CHECK: DatabaseCheck = DatabaseCheck {
    tool: "/usr/bin/dscl",
    arguments: &[".", "-list", "/Users"],
    missing: (
        "DSCL_MISSING",
        "the macOS account database tool is unavailable",
    ),
    present: (
        "DSCL_NIXBLD_USER",
        "OpenDirectory reports one or more Nix build users",
    ),
    failed: (
        "DSCL_QUERY_FAILED",
        "OpenDirectory could not be checked for Nix build users",
    ),
    matches_build: account_output_has_build_user,
    colon_separated: false,
};

const DSCL_GROUPS_CHECK: DatabaseCheck = DatabaseCheck {
    tool: "/usr/bin/dscl",
    arguments: &[".", "-list", "/Groups"],
    missing: (
        "DSCL_MISSING",
        "the macOS account database tool is unavailable",
    ),
    present: (
        "DSCL_NIXBLD_GROUP",
        "OpenDirectory reports a Nix build-users group",
    ),
    failed: (
        "DSCL_GROUPS_QUERY_FAILED",
        "OpenDirectory could not be checked for a Nix build group",
    ),
    matches_build: account_output_has_build_group,
    colon_separated: false,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use pkg_core::System;
    use tempfile::TempDir;

    use super::{
        DetectionDisposition, account_output_has_build_group, account_output_has_build_user,
        detect_unmanaged_nix,
    };

    #[test]
    fn account_database_parsers_match_only_nix_build_identities() {
        assert!(account_output_has_build_user(
            b"_nixbld1\nordinary\n",
            false
        ));
        assert!(account_output_has_build_user(
            b"nixbld12:x:30012:30000::/var/empty:/usr/bin/nologin\n",
            true
        ));
        assert!(account_output_has_build_group(b"nixbld:*:350:\n", true));
        assert!(account_output_has_build_group(b"_nixbld\n", false));
        assert!(!account_output_has_build_user(
            b"nixbld\nphoenixbld1\n",
            false
        ));
        assert!(!account_output_has_build_group(b"nixbld-helper\n", false));
    }

    #[test]
    fn macos_scans_a_protected_sibling_home_alias_once() -> Result<(), Box<dyn std::error::Error>> {
        let root = TempDir::new()?;
        let users = root.path().join("Users");
        let admin = users.join("admin");
        fs::create_dir_all(&admin)?;
        symlink("admin", users.join("runner"))?;

        let clean = detect_unmanaged_nix(root.path(), System::Aarch64Darwin, &[], &[]);
        assert_eq!(clean.disposition(), DetectionDisposition::Clean);

        fs::set_permissions(&users, fs::Permissions::from_mode(0o777))?;
        let writable = detect_unmanaged_nix(root.path(), System::Aarch64Darwin, &[], &[]);
        assert_eq!(writable.disposition(), DetectionDisposition::Refuse);
        fs::set_permissions(&users, fs::Permissions::from_mode(0o755))?;

        symlink("/nix/var/nix/profiles/default", admin.join(".nix-profile"))?;
        let managed = detect_unmanaged_nix(root.path(), System::Aarch64Darwin, &[], &[]);
        assert_eq!(managed.disposition(), DetectionDisposition::Refuse);
        Ok(())
    }
}
