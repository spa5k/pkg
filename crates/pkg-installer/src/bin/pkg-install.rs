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
    let (invocation, channel_base) = parse_invocation(std::env::args_os().skip(1))?;
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    let system = host_system().ok_or(PublicInstallError::UnsupportedSystem)?;
    validate_invocation_system(invocation, system)?;
    eprintln!("pkg setup — {}", env!("CARGO_PKG_VERSION"));
    eprintln!("Downloading and verifying the release. This can take a few minutes...");
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
        run_macos(invocation, trusted_root, &request)
    } else {
        run_linux(invocation, trusted_root, &request)
    }
}

fn run_macos(
    invocation: Invocation,
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<InstallSuccess, PublicInstallError> {
    let (system, groups) = (request.system, request.groups);
    let mut backend = match invocation {
        Invocation::InstallOrUpgrade | Invocation::LeaveServicesOffline => {
            ProductionMacOsInstallBackend::new(system, groups)
        }
        Invocation::ResumeBaseNix => ProductionMacOsInstallBackend::new_resume(system, groups),
        Invocation::RepairProductAssets => {
            ProductionMacOsInstallBackend::new_product_repair(system, groups)
        }
    }
    .map_err(|error| {
        report_backend_error("macos-backend-new", &error);
        PublicInstallError::InstallFailed
    })?;
    if invocation == Invocation::InstallOrUpgrade {
        backend.manage_upgrade_services();
    }
    install_macos_from_bundle(system, trusted_root, request, &mut backend).map_err(|error| {
        report_backend_error("macos-install", &error);
        PublicInstallError::InstallFailed
    })?;
    backend.finish_upgrade_services().map_err(|error| {
        report_backend_error("macos-service-restart", &error);
        PublicInstallError::ServicesNotReady
    })?;
    Ok(match backend.install_mode() {
        pkg_installer::InstallMode::FreshInstall => InstallSuccess::Installed,
        pkg_installer::InstallMode::OfflineUpgrade
            if invocation == Invocation::InstallOrUpgrade =>
        {
            InstallSuccess::Ready
        }
        pkg_installer::InstallMode::OfflineUpgrade => InstallSuccess::Upgraded,
        pkg_installer::InstallMode::OfflineRepair => InstallSuccess::Repaired,
    })
}

fn run_linux(
    invocation: Invocation,
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<InstallSuccess, PublicInstallError> {
    let (system, groups) = (request.system, request.groups);
    let mut backend = match invocation {
        Invocation::InstallOrUpgrade
        | Invocation::LeaveServicesOffline
        | Invocation::ResumeBaseNix => ProductionLinuxInstallBackend::new(system, groups),
        Invocation::RepairProductAssets => {
            ProductionLinuxInstallBackend::new_product_repair(system, groups)
        }
    }
    .map_err(|error| {
        report_backend_error("linux-backend-new", &error);
        PublicInstallError::InstallFailed
    })?;
    if invocation == Invocation::InstallOrUpgrade {
        backend.manage_upgrade_services();
    }
    install_linux_from_bundle(system, trusted_root, request, &mut backend).map_err(|error| {
        report_install_error(error);
        public_install_error(error)
    })?;
    backend.finish_upgrade_services().map_err(|error| {
        report_backend_error("linux-service-restart", &error);
        PublicInstallError::ServicesNotReady
    })?;
    Ok(match invocation {
        Invocation::RepairProductAssets => InstallSuccess::Repaired,
        Invocation::InstallOrUpgrade if backend.install_mode() == InstallMode::OfflineUpgrade => {
            InstallSuccess::Ready
        }
        Invocation::LeaveServicesOffline | Invocation::ResumeBaseNix
            if backend.install_mode() == InstallMode::OfflineUpgrade =>
        {
            InstallSuccess::Upgraded
        }
        Invocation::InstallOrUpgrade
        | Invocation::LeaveServicesOffline
        | Invocation::ResumeBaseNix => InstallSuccess::Installed,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallSuccess {
    Installed,
    Ready,
    Upgraded,
    Repaired,
}

impl InstallSuccess {
    const fn message(self) -> &'static str {
        match self {
            Self::Installed => "pkg is installed.",
            Self::Ready => {
                "pkg is updated and ready. Your packages and Nix installation were kept."
            }
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
    LeaveServicesOffline,
    RepairProductAssets,
    ResumeBaseNix,
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
            "--leave-services-offline" => {
                if invocation != Invocation::InstallOrUpgrade {
                    return Err(PublicInstallError::InvalidInvocation);
                }
                invocation = Invocation::LeaveServicesOffline;
            }
            "--repair-product-assets" => {
                if invocation != Invocation::InstallOrUpgrade {
                    return Err(PublicInstallError::InvalidInvocation);
                }
                invocation = Invocation::RepairProductAssets;
            }
            "--resume" => {
                if invocation != Invocation::InstallOrUpgrade {
                    return Err(PublicInstallError::InvalidInvocation);
                }
                invocation = Invocation::ResumeBaseNix;
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
        (_, System::Aarch64Darwin)
        | (
            Invocation::InstallOrUpgrade
            | Invocation::LeaveServicesOffline
            | Invocation::RepairProductAssets,
            System::X8664Linux | System::Aarch64Linux,
        ) => Ok(()),
        _ => Err(PublicInstallError::UnsupportedSystem),
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
    ServicesNotReady,
    InstallFailed,
}

impl fmt::Display for PublicInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInvocation => {
                "Run pkg-install without options. Use --leave-services-offline for a manual upgrade, --repair-product-assets for repair, or --resume for macOS recovery. Use --channel <BASE_URL> to select the release channel."
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
            Self::ServicesNotReady => {
                "Pkg files are installed, but its services are not ready. Run this installer again. Your packages were kept."
            }
            Self::InstallFailed => "pkg setup could not finish. Keep the install log for support. Do not delete Nix or its installation records.",
        })
    }
}

#[cfg(test)]
#[path = "pkg-install/tests.rs"]
mod tests;
