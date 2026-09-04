//! Production installer entry point.

use std::{ffi::OsString, fmt, path::Path, process::ExitCode};

use nix::unistd::Uid;
use pkg_channel::{TrustedRoot, validate_https_repository_url};
use pkg_core::System;
use pkg_installer::{
    InstallError, InstallErrorCode, InstallMode, LinuxInstallBackend,
    ProductionLinuxInstallBackend, ProductionMacOsInstallBackend, install_linux_from_bundle,
    install_macos_from_bundle, plan_linux_group_bindings,
};
use pkg_nix::{InstallerProvisionRequest, InstallerRepository, ManagedGroupBindings};
use url::Url;

const RELEASE_TUF_ROOT_JSON: Option<&str> = option_env!("PKG_RELEASE_TUF_ROOT_JSON");
const RELEASE_METADATA_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_METADATA_URL");
const RELEASE_TARGETS_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_TARGETS_URL");
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
            if std::env::var_os("PKG_INSTALL_DEBUG").is_some() {
                eprintln!("debug: error={error:?}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<InstallSuccess, PublicInstallError> {
    let invocation = parse_invocation(std::env::args_os().skip(1))?;
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    let system = host_system().ok_or(PublicInstallError::UnsupportedSystem)?;
    validate_invocation_system(invocation, system)?;
    let trusted_root = trusted_root(RELEASE_TUF_ROOT_JSON)?;
    let (metadata_url, targets_url) = release_urls(RELEASE_METADATA_URL, RELEASE_TARGETS_URL)?;
    let (groups, channel_datastore, scratch_parent) =
        if matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
            (
                ManagedGroupBindings::new(333, 350)
                    .map_err(|e| { eprintln!("debug: group-bindings failed: {e:?}"); PublicInstallError::InstallFailed })?,
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
        .map_err(|e| { eprintln!("debug: backend-new failed: {e:?}"); PublicInstallError::InstallFailed })?;
        {
            eprintln!("debug: calling install_macos_from_bundle");
            let result = install_macos_from_bundle(system, trusted_root, &request, &mut backend);
            match &result {
                Ok(_) => eprintln!("debug: install_macos_from_bundle OK"),
                Err(e) => eprintln!("debug: install_macos_from_bundle FAILED: {e:?} code={:?}", e.code()),
            }
            result.map_err(|_| PublicInstallError::InstallFailed)?;
        }
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
        .map_err(|_| PublicInstallError::InstallFailed)?;
        install_linux_from_bundle(system, trusted_root, &request, &mut backend)
            .map_err(public_install_error)?;
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

fn parse_invocation(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Invocation, PublicInstallError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Invocation::InstallOrUpgrade),
        [argument] if argument == "--repair-product-assets" => Ok(Invocation::RepairProductAssets),
        _ => Err(PublicInstallError::InvalidInvocation),
    }
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
                "Run pkg-install without options or with --repair-product-assets."
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
