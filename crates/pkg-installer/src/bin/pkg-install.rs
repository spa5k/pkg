//! Production installer entry point.

use std::{ffi::OsString, fmt, path::Path, process::ExitCode};

use nix::unistd::Uid;
use pkg_channel::{TrustedRoot, validate_https_repository_url};
use pkg_core::System;
use pkg_installer::{
    InstallError, InstallErrorCode, InstallMode, LinuxInstallBackend,
    ProductionLinuxInstallBackend, ProductionMacOsInstallBackend, derive_channel_urls,
    install_linux_from_bundle, install_macos_from_bundle, plan_linux_group_bindings,
};
use pkg_nix::{InstallerProvisionRequest, InstallerRepository, ManagedGroupBindings};
use url::Url;

const RELEASE_TUF_ROOT_JSON: Option<&str> = option_env!("PKG_RELEASE_TUF_ROOT_JSON");
const RELEASE_METADATA_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_METADATA_URL");
const RELEASE_TARGETS_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_TARGETS_URL");
const CHANNEL_URL_VARIABLE: &str = "PKG_CHANNEL_URL";
const LINUX_CHANNEL_DATASTORE: &str = "/var/lib/pkg/broker-home/channel";
const LINUX_SCRATCH_PARENT: &str = "/var/lib/pkg/helper-home/tmp";
const MACOS_CHANNEL_DATASTORE: &str = "/Library/Application Support/pkg/broker-home/channel";
const MACOS_SCRATCH_PARENT: &str = "/Library/Application Support/pkg/helper-home/tmp";

#[expect(clippy::print_stdout, reason = "the installer only product output")]
#[expect(clippy::print_stderr, reason = "the installer only failure output")]
fn main() -> ExitCode {
    match run() {
        Ok(success) => {
            println!("{}", success.message());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<InstallSuccess, PublicInstallError> {
    run_inner()
}

#[allow(clippy::too_many_lines, reason = "alpha debug build")]
fn run_inner() -> Result<InstallSuccess, PublicInstallError> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--doctor") {
        return run_doctor();
    }
    if args.iter().any(|a| a == "--reset-failed") {
        return run_reset_failed();
    }
    let (invocation, channel_base) = parse_invocation(args)?;
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    eprintln!(
        "[pkg] starting install on {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let system = host_system().ok_or(PublicInstallError::UnsupportedSystem)?;
    validate_invocation_system(invocation, system)?;
    let trusted_root = trusted_root(RELEASE_TUF_ROOT_JSON)?;
    let environment = match std::env::var(CHANNEL_URL_VARIABLE) {
        Ok(base) => Some(base),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Err(PublicInstallError::InvalidRelease),
    };
    let (metadata_url, targets_url) =
        channel_urls(channel_base.as_deref(), environment.as_deref())?;
    let (groups, channel_datastore, scratch_parent) =
        if matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
            (
                ManagedGroupBindings::new(333, 350)
                    .map_err(|_| PublicInstallError::InstallFailed)?,
                MACOS_CHANNEL_DATASTORE,
                MACOS_SCRATCH_PARENT,
            )
        } else {
            (
                plan_linux_group_bindings().map_err(|_| PublicInstallError::InstallFailed)?,
                LINUX_CHANNEL_DATASTORE,
                LINUX_SCRATCH_PARENT,
            )
        };
    let request = InstallerProvisionRequest {
        repository: InstallerRepository::Remote {
            metadata_url: &metadata_url,
            targets_url: &targets_url,
        },
        datastore: Path::new(channel_datastore),
        installation_root: Path::new("/"),
        scratch_parent: Path::new(scratch_parent),
        system,
        groups,
    };
    if matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
        let mut backend = match invocation {
            Invocation::InstallOrUpgrade => ProductionMacOsInstallBackend::new(system, groups),
            Invocation::RepairProductAssets => {
                ProductionMacOsInstallBackend::new_product_repair(system, groups)
            }
        }
        .map_err(|error| {
            report_backend_error("macos-backend-new", &error);
            PublicInstallError::InstallFailed
        })?;
        eprintln!("[pkg] fetching and verifying channel...");
        install_macos_from_bundle(system, trusted_root, &request, &mut backend).map_err(
            |error| {
                report_backend_error("macos-install", &error);
                eprintln!("[pkg] TIP: run with --doctor to see what state was found, or --reset-failed to clean up");
                PublicInstallError::InstallFailed
            },
        )?;
        eprintln!("[pkg] install complete");
        Ok(match backend.install_mode() {
            pkg_installer::InstallMode::FreshInstall => InstallSuccess::Installed,
            pkg_installer::InstallMode::OfflineUpgrade => InstallSuccess::Upgraded,
            pkg_installer::InstallMode::OfflineRepair => InstallSuccess::Repaired,
        })
    } else {
        let mut backend = match invocation {
            Invocation::InstallOrUpgrade => ProductionLinuxInstallBackend::new(system, groups),
            Invocation::RepairProductAssets => {
                ProductionLinuxInstallBackend::new_product_repair(system, groups)
            }
        }
        .map_err(|error| {
            report_backend_error("linux-backend-new", &error);
            PublicInstallError::InstallFailed
        })?;
        eprintln!("[pkg] fetching and verifying channel...");
        install_linux_from_bundle(system, trusted_root, &request, &mut backend).map_err(
            |error| {
                report_install_error(error);
                eprintln!("[pkg] TIP: run with --doctor to see what state was found, or --reset-failed to clean up");
                public_install_error(error)
            },
        )?;
        eprintln!("[pkg] install complete");
        Ok(match invocation {
            Invocation::RepairProductAssets => InstallSuccess::Repaired,
            Invocation::InstallOrUpgrade
                if backend.install_mode() == InstallMode::OfflineUpgrade =>
            {
                InstallSuccess::Upgraded
            }
            Invocation::InstallOrUpgrade => InstallSuccess::Installed,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallSuccess {
    Installed,
    Upgraded,
    Repaired,
}

impl InstallSuccess {
    const fn message(self) -> &'static str {
        match self {
            Self::Installed => "pkg is installed.",
            Self::Upgraded => "pkg product files are upgraded. Product services remain offline.",
            Self::Repaired => "pkg product files are repaired. Product services remain offline.",
        }
    }
}

const fn public_install_error(error: InstallError) -> PublicInstallError {
    public_install_error_code(error.code())
}

/// Prints the failing phase and its stable code so an operator can act.
/// The public message stays redacted; this line is the diagnosis.
fn report_install_error(error: InstallError) {
    eprintln!("install failure: code={:?}", error.code());
}

fn report_backend_error<E: std::fmt::Debug>(phase: &str, error: &E) {
    eprintln!("install failure: phase={phase} detail={error:?}");
}

const fn public_install_error_code(code: InstallErrorCode) -> PublicInstallError {
    match code {
        InstallErrorCode::OfflineServicesRequired => PublicInstallError::OfflineServicesRequired,
        InstallErrorCode::RecoveryModeMismatch => PublicInstallError::RecoveryModeMismatch,
        InstallErrorCode::UnsupportedRecoverySchema => {
            PublicInstallError::UnsupportedRecoverySchema
        }
        InstallErrorCode::FreshRecoveryRetained => PublicInstallError::FreshRecoveryRetained,
        InstallErrorCode::UnsupportedPlatform
        | InstallErrorCode::UnmanagedNix
        | InstallErrorCode::BackendFailure
        | InstallErrorCode::ServiceUnhealthy
        | InstallErrorCode::ReceiptFailure
        | InstallErrorCode::RollbackIncomplete => PublicInstallError::InstallFailed,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invocation {
    InstallOrUpgrade,
    RepairProductAssets,
}

/// Prints every piece of Nix/pkg state found on this host.
#[allow(clippy::too_many_lines, reason = "alpha diagnostic tool")]
#[allow(clippy::unnecessary_literal_bound, reason = "alpha diagnostic tool")]
fn run_doctor() -> Result<InstallSuccess, PublicInstallError> {
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    println!("pkg install doctor — checking for existing state\n");

    let checks: [(&str, &str); 12] = [
        ("/nix", "Determinate Base Nix"),
        ("/nix/receipt.json", "vendor receipt"),
        ("/etc/nix", "Nix configuration"),
        ("/opt/pkg", "product binaries"),
        ("/var/lib/pkg", "product state (Linux)"),
        ("/var/lib/pkg-install", "install state (Linux)"),
        ("/var/lib/pkg-install-journal", "install journal (Linux)"),
        ("/private/var/db/pkg-install", "install state (macOS)"),
        ("/Library/Application Support/pkg", "product state (macOS)"),
        ("/etc/profile.d/pkg.sh", "shell integration"),
        ("/nix/var/nix/profiles", "Nix profiles"),
        ("/run/pkg-install-handoff.lock", "install handoff lock"),
    ];
    let mut found_any = false;
    for (path, label) in checks {
        if Path::new(path).exists() {
            println!("  FOUND: {path} ({label})");
            found_any = true;
        }
    }

    // Check for nixbld users
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        let nixbld: Vec<&str> = passwd
            .lines()
            .filter(|l| l.starts_with("nixbld") || l.starts_with("_nixbld"))
            .collect();
        if !nixbld.is_empty() {
            println!("  FOUND: {} nixbld users in /etc/passwd", nixbld.len());
            found_any = true;
        }
    }
    let _ = found_any;

    // Check for pkg users
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            if line.contains("pkg-nix-broker") || line.contains("pkg-root-helper") {
                let user = line.split(':').next().unwrap_or("?");
                println!("  FOUND: user {user} in /etc/passwd");
                found_any = true;
            }
        }
    }

    if found_any {
        println!("\n  State found. Run with --reset-failed to clean up.");
    } else {
        println!("\n  Host is clean. Ready to install.");
    }
    Ok(InstallSuccess::Installed)
}

/// Removes all pkg and vendor state so a fresh install can be attempted.
#[allow(clippy::too_many_lines, reason = "alpha reset tool")]
fn run_reset_failed() -> Result<InstallSuccess, PublicInstallError> {
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    println!("pkg reset — removing all pkg and vendor state\n");

    // 1. Run the vendor uninstaller if a receipt exists
    let receipt = Path::new("/nix/receipt.json");
    if receipt.exists() {
        println!("  running Determinate uninstaller...");
        let status = std::process::Command::new("/nix/nix-installer")
            .args(["uninstall", "--no-confirm", "/nix/receipt.json"])
            .status();
        match status {
            Ok(s) if s.success() => println!("    vendor uninstall: OK"),
            Ok(s) => println!(
                "    vendor uninstall: exit {} (continuing cleanup)",
                s.code().unwrap_or(-1)
            ),
            Err(e) => println!("    vendor uninstall: {e} (continuing cleanup)"),
        }
    } else {
        println!("  no vendor receipt found, skipping vendor uninstall");
    }

    // 2. Stop product services (best effort)
    if cfg!(target_os = "linux") {
        for svc in [
            "pkg-nix-broker",
            "pkg-root-helper",
            "nix-daemon",
            "determinate-nixd",
        ] {
            let _ = std::process::Command::new("systemctl")
                .args(["stop", &format!("{svc}.service")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = std::process::Command::new("systemctl")
                .args(["disable", &format!("{svc}.service")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = std::process::Command::new("systemctl")
            .arg("daemon-reload")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    // 3. Remove pkg users and groups (best effort)
    if cfg!(target_os = "linux") {
        for user in ["pkg-nix-broker", "pkg-root-helper"] {
            let _ = std::process::Command::new("userdel")
                .args(["-f", user])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        for group in ["pkg-nix-broker", "pkg-root-helper", "nixbld"] {
            let _ = std::process::Command::new("groupdel")
                .arg(group)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        // Remove nixbld users
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd
                .lines()
                .filter(|l| l.starts_with("nixbld") || l.starts_with("_nixbld"))
            {
                if let Some(user) = line.split(':').next() {
                    let _ = std::process::Command::new("userdel")
                        .args(["-f", user])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
            }
        }
    }

    // 4. Remove all product and vendor paths
    let paths_to_remove: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/nix",
            "/etc/nix",
            "/opt/pkg",
            "/var/lib/pkg",
            "/var/lib/pkg-install",
            "/var/lib/pkg-install-journal",
            "/private/var/db/pkg-install",
            "/private/var/db/pkg-install-journal",
            "/Library/Application Support/pkg",
            "/etc/profile.d/pkg.sh",
            "/run/pkg-install-handoff.lock",
            "/var/log/pkg",
        ]
    } else {
        &[
            "/nix",
            "/etc/nix",
            "/opt/pkg",
            "/var/lib/pkg",
            "/var/lib/pkg-install",
            "/var/lib/pkg-install-journal",
            "/etc/profile.d/pkg.sh",
            "/run/pkg-install-handoff.lock",
            "/run/pkg",
            "/var/log/pkg",
            "/var/log/nix",
        ]
    };
    for path in paths_to_remove {
        if Path::new(path).exists() {
            match std::fs::remove_dir_all(path) {
                Ok(()) => println!("  removed: {path}"),
                Err(_) => match std::fs::remove_file(path) {
                    Ok(()) => println!("  removed: {path}"),
                    Err(e) => println!("  WARNING: could not remove {path}: {e}"),
                },
            }
        }
    }

    // 5. Remove systemd units (Linux)
    if cfg!(target_os = "linux") {
        for unit in [
            "nix-daemon.service",
            "nix-daemon.socket",
            "determinate-nixd.service",
            "determinate-nixd.socket",
            "pkg-nix-broker.service",
            "pkg-nix-broker.socket",
            "pkg-root-helper.service",
            "pkg-root-helper.socket",
        ] {
            for dir in ["/etc/systemd/system", "/lib/systemd/system"] {
                let path = format!("{dir}/{unit}");
                if Path::new(&path).exists() {
                    let _ = std::fs::remove_file(&path);
                    println!("  removed: {path}");
                }
            }
            // Also remove wants symlinks
            for dir in ["multi-user.target.wants", "sockets.target.wants"] {
                let path = format!("/etc/systemd/system/{dir}/{unit}");
                let _ = std::fs::remove_file(&path);
            }
        }
        let _ = std::fs::remove_file("/etc/tmpfiles.d/nix-daemon.conf");
        let _ = std::process::Command::new("systemctl")
            .arg("daemon-reload")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    // 6. Remove root's nix profile links
    for link in [
        "/root/.nix-profile",
        "/root/.nix-defexpr",
        "/root/.nix-channels",
    ] {
        if Path::new(link).exists() || Path::new(link).is_symlink() {
            let _ = std::fs::remove_file(link);
            println!("  removed: {link}");
        }
    }

    println!("\n  reset complete. The host should now be clean.");
    println!(
        "  verify with: {} --doctor",
        std::env::args()
            .next()
            .unwrap_or_else(|| "pkg-install".to_owned())
    );
    Ok(InstallSuccess::Installed)
}

fn parse_invocation(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(Invocation, Option<String>), PublicInstallError> {
    let mut invocation = Invocation::InstallOrUpgrade;
    let mut channel = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let Some(text) = argument.to_str() else {
            return Err(PublicInstallError::InvalidInvocation);
        };
        match text {
            "--repair-product-assets" => {
                if invocation == Invocation::RepairProductAssets {
                    return Err(PublicInstallError::InvalidInvocation);
                }
                invocation = Invocation::RepairProductAssets;
            }
            "--channel" => {
                if channel.is_some() {
                    return Err(PublicInstallError::InvalidInvocation);
                }
                let Some(value) = arguments.next() else {
                    return Err(PublicInstallError::InvalidInvocation);
                };
                let Some(value) = value.to_str() else {
                    return Err(PublicInstallError::InvalidInvocation);
                };
                channel = Some(value.to_owned());
            }
            _ => return Err(PublicInstallError::InvalidInvocation),
        }
    }
    Ok((invocation, channel))
}

const fn validate_invocation_system(
    invocation: Invocation,
    system: System,
) -> Result<(), PublicInstallError> {
    match (invocation, system) {
        (_, System::X8664Darwin) => Err(PublicInstallError::UnsupportedSystem),
        (
            Invocation::InstallOrUpgrade | Invocation::RepairProductAssets,
            System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin,
        ) => Ok(()),
    }
}

fn trusted_root(root_json: Option<&'static str>) -> Result<TrustedRoot, PublicInstallError> {
    TrustedRoot::from_embedded(
        root_json
            .ok_or(PublicInstallError::InvalidRelease)?
            .as_bytes(),
    )
    .map_err(|_| PublicInstallError::InvalidRelease)
}

fn release_urls(
    metadata: Option<&str>,
    targets: Option<&str>,
) -> Result<(Url, Url), PublicInstallError> {
    let metadata = Url::parse(metadata.ok_or(PublicInstallError::InvalidRelease)?)
        .map_err(|_| PublicInstallError::InvalidRelease)?;
    let targets = Url::parse(targets.ok_or(PublicInstallError::InvalidRelease)?)
        .map_err(|_| PublicInstallError::InvalidRelease)?;
    if !metadata.path().ends_with('/')
        || !targets.path().ends_with('/')
        || validate_https_repository_url(&metadata).is_err()
        || validate_https_repository_url(&targets).is_err()
    {
        return Err(PublicInstallError::InvalidRelease);
    }
    Ok((metadata, targets))
}

fn channel_urls(
    command_line: Option<&str>,
    environment: Option<&str>,
) -> Result<(Url, Url), PublicInstallError> {
    if let Some(base) = command_line.or(environment) {
        return derive_channel_urls(base).map_err(|_| PublicInstallError::InvalidRelease);
    }
    release_urls(RELEASE_METADATA_URL, RELEASE_TARGETS_URL)
}

fn host_system() -> Option<System> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Some(System::X8664Linux),
        ("aarch64", "linux") => Some(System::Aarch64Linux),
        ("x86_64", "macos") => Some(System::X8664Darwin),
        ("aarch64", "macos") => Some(System::Aarch64Darwin),
        (_, _) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicInstallError {
    InvalidInvocation,
    RootRequired,
    UnsupportedSystem,
    InvalidRelease,
    OfflineServicesRequired,
    RecoveryModeMismatch,
    UnsupportedRecoverySchema,
    FreshRecoveryRetained,
    InstallFailed,
}

impl fmt::Display for PublicInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInvocation => {
                "Run pkg-install without options or with --repair-product-assets. Use --channel <BASE_URL> to select the release channel."
            }
            Self::RootRequired => "Run pkg-install as root.",
            Self::UnsupportedSystem => "This pkg installer does not support this system.",
            Self::InvalidRelease => "This pkg installer package is not valid.",
            Self::OfflineServicesRequired => {
                "Stop and disable all pkg product services. Remove all product unit drop-ins. Then run pkg-install again."
            }
            Self::RecoveryModeMismatch => {
                "Use the same pkg-install operation that created the pending recovery."
            }
            Self::UnsupportedRecoverySchema => {
                "Use the pkg-install version that created the pending recovery. The recovery file was not changed."
            }
            Self::FreshRecoveryRetained => {
                "Base Nix is ready, but pkg product installation is incomplete. Run pkg-install again."
            }
            Self::InstallFailed => "pkg installation failed.",
        })
    }
}

#[cfg(test)]
#[path = "pkg-install/tests.rs"]
mod tests;
