use std::{ffi::OsString, fmt, path::Path, process::ExitCode};

use nix::unistd::Uid;
use pkg_channel::{TrustedRoot, validate_https_repository_url};
use pkg_core::System;
use pkg_installer::{
    InstallError, InstallErrorCode, LinuxInstallBackend, LinuxInstallMode,
    ProductionLinuxInstallBackend, ProductionMacOsInstallBackend, install_linux_from_bundle,
    install_macos_from_bundle, plan_linux_group_bindings,
};
use pkg_nix::{
    InstallerProvisionRequest, InstallerRepository, ManagedGroupBindings, ProductionManagedDaemon,
};
use url::Url;

const RELEASE_TUF_ROOT_JSON: Option<&str> = option_env!("PKG_RELEASE_TUF_ROOT_JSON");
const RELEASE_METADATA_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_METADATA_URL");
const RELEASE_TARGETS_URL: Option<&str> = option_env!("PKG_RELEASE_CHANNEL_TARGETS_URL");
const LINUX_CHANNEL_DATASTORE: &str = "/var/lib/pkg/broker-home/channel";
const LINUX_SCRATCH_PARENT: &str = "/var/lib/pkg/helper-home/tmp";
const MACOS_CHANNEL_DATASTORE: &str = "/Library/Application Support/pkg/broker-home/channel";
const MACOS_SCRATCH_PARENT: &str = "/Library/Application Support/pkg/helper-home/tmp";

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
    let daemon = ProductionManagedDaemon::production();
    if matches!(system, System::X8664Darwin | System::Aarch64Darwin) {
        let mut backend = ProductionMacOsInstallBackend::new(system, groups)
            .map_err(|_| PublicInstallError::InstallFailed)?;
        install_macos_from_bundle(system, trusted_root, &request, &daemon, &mut backend)
            .map_err(|_| PublicInstallError::InstallFailed)?;
        Ok(InstallSuccess::Installed)
    } else {
        let mut backend = match invocation {
            Invocation::InstallOrUpgrade => ProductionLinuxInstallBackend::new(system, groups),
            Invocation::RepairProductAssets => {
                ProductionLinuxInstallBackend::new_product_repair(system, groups)
            }
        }
        .map_err(|_| PublicInstallError::InstallFailed)?;
        install_linux_from_bundle(system, trusted_root, &request, &daemon, &mut backend)
            .map_err(public_install_error)?;
        Ok(match invocation {
            Invocation::RepairProductAssets => InstallSuccess::Repaired,
            Invocation::InstallOrUpgrade
                if backend.install_mode() == LinuxInstallMode::OfflineUpgrade =>
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

fn validate_invocation_system(
    invocation: Invocation,
    system: System,
) -> Result<(), PublicInstallError> {
    if invocation == Invocation::RepairProductAssets
        && matches!(system, System::X8664Darwin | System::Aarch64Darwin)
    {
        Err(PublicInstallError::UnsupportedSystem)
    } else {
        Ok(())
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
mod tests {
    use super::*;

    const FIXTURE_ROOT: &str = include_str!("../../../../fixtures/channel-v1/root.json");

    #[test]
    fn package_boundary_and_trust_inputs_are_fixed() {
        assert_eq!(LINUX_CHANNEL_DATASTORE, "/var/lib/pkg/broker-home/channel");
        assert_eq!(LINUX_SCRATCH_PARENT, "/var/lib/pkg/helper-home/tmp");
        assert!(pkg_installer::linux_install_assets().iter().any(|asset| {
            asset.path_or_name() == LINUX_SCRATCH_PARENT
                && asset.owner() == Some(pkg_installer::LinuxAssetPrincipal::Root)
                && asset.group() == Some(pkg_installer::LinuxAssetPrincipal::Root)
                && asset.mode() == Some(0o700)
        }));
        assert!(pkg_installer::linux_install_assets().iter().any(|asset| {
            asset.path_or_name() == LINUX_CHANNEL_DATASTORE
                && asset.owner() == Some(pkg_installer::LinuxAssetPrincipal::Broker)
                && asset.group() == Some(pkg_installer::LinuxAssetPrincipal::Broker)
                && asset.mode() == Some(0o700)
        }));
        assert!(pkg_installer::macos_install_assets().iter().any(|asset| {
            asset.path_or_name() == MACOS_SCRATCH_PARENT && asset.mode() == Some(0o700)
        }));
        assert!(pkg_installer::macos_install_assets().iter().any(|asset| {
            asset.path_or_name() == MACOS_CHANNEL_DATASTORE && asset.mode() == Some(0o700)
        }));
        assert_eq!(
            release_urls(
                Some("https://releases.pkg.example/v1/metadata/"),
                Some("https://releases.pkg.example/v1/targets/"),
            )
            .map(
                |(metadata, targets)| (metadata.scheme().to_owned(), targets.scheme().to_owned(),)
            ),
            Ok(("https".to_owned(), "https".to_owned())),
        );
        assert!(
            release_urls(Some("http://host/metadata/"), Some("https://host/targets/"),).is_err()
        );
        assert!(
            release_urls(Some("https://host/metadata"), Some("https://host/targets/"),).is_err()
        );
        assert!(trusted_root(Some(FIXTURE_ROOT)).is_ok());
        assert!(matches!(
            trusted_root(None),
            Err(PublicInstallError::InvalidRelease)
        ));
    }

    #[test]
    fn invocation_requires_the_exact_product_repair_option() {
        assert_eq!(parse_invocation([]), Ok(Invocation::InstallOrUpgrade));
        assert_eq!(
            parse_invocation([OsString::from("--repair-product-assets")]),
            Ok(Invocation::RepairProductAssets)
        );
        for arguments in [
            vec![OsString::from("--repair")],
            vec![OsString::from("--repair-product-assets=yes")],
            vec![
                OsString::from("--repair-product-assets"),
                OsString::from("extra"),
            ],
        ] {
            assert_eq!(
                parse_invocation(arguments),
                Err(PublicInstallError::InvalidInvocation)
            );
        }
        assert_eq!(
            validate_invocation_system(Invocation::RepairProductAssets, System::X8664Linux,),
            Ok(())
        );
        assert_eq!(
            validate_invocation_system(Invocation::RepairProductAssets, System::Aarch64Darwin,),
            Err(PublicInstallError::UnsupportedSystem)
        );
    }

    #[test]
    fn public_results_keep_distinct_safe_operator_actions() {
        assert_eq!(InstallSuccess::Installed.message(), "pkg is installed.");
        assert!(InstallSuccess::Upgraded.message().contains("upgraded"));
        assert!(InstallSuccess::Repaired.message().contains("repaired"));
        for (code, expected) in [
            (
                InstallErrorCode::OfflineServicesRequired,
                PublicInstallError::OfflineServicesRequired,
            ),
            (
                InstallErrorCode::RecoveryModeMismatch,
                PublicInstallError::RecoveryModeMismatch,
            ),
            (
                InstallErrorCode::UnsupportedRecoverySchema,
                PublicInstallError::UnsupportedRecoverySchema,
            ),
            (
                InstallErrorCode::FreshRecoveryRetained,
                PublicInstallError::FreshRecoveryRetained,
            ),
        ] {
            assert_eq!(public_install_error_code(code), expected);
            assert_ne!(expected.to_string(), "pkg installation failed.");
        }
    }

    #[test]
    fn public_failures_are_short_and_do_not_expose_internal_inputs() {
        let messages = [
            PublicInstallError::InvalidInvocation,
            PublicInstallError::RootRequired,
            PublicInstallError::UnsupportedSystem,
            PublicInstallError::InvalidRelease,
            PublicInstallError::InstallFailed,
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages[0],
            "Run pkg-install without options or with --repair-product-assets."
        );
        assert_eq!(messages[1], "Run pkg-install as root.");
        assert!(messages.iter().all(|message| {
            !message.contains("nix")
                && !message.contains("/var/")
                && !message.contains("metadata")
                && !message.contains("target")
        }));
    }
}
