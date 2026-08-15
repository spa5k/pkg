use std::{fmt, path::Path, process::ExitCode};

use nix::unistd::Uid;
use pkg_channel::{TrustedRoot, validate_https_repository_url};
use pkg_core::System;
use pkg_installer::{
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
        Ok(()) => {
            println!("pkg is installed.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), PublicInstallError> {
    if std::env::args_os().count() != 1 {
        return Err(PublicInstallError::InvalidInvocation);
    }
    if !Uid::effective().is_root() {
        return Err(PublicInstallError::RootRequired);
    }
    let system = host_system().ok_or(PublicInstallError::UnsupportedSystem)?;
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
    } else {
        let mut backend = ProductionLinuxInstallBackend::new(system, groups)
            .map_err(|_| PublicInstallError::InstallFailed)?;
        install_linux_from_bundle(system, trusted_root, &request, &daemon, &mut backend)
            .map_err(|_| PublicInstallError::InstallFailed)?;
    }
    Ok(())
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
    InstallFailed,
}

impl fmt::Display for PublicInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInvocation => "Run pkg-install without options.",
            Self::RootRequired => "Run pkg-install as root.",
            Self::UnsupportedSystem => "This pkg installer does not support this system.",
            Self::InvalidRelease => "This pkg installer package is not valid.",
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
    fn public_failures_are_short_and_do_not_expose_internal_inputs() {
        let messages = [
            PublicInstallError::InvalidInvocation,
            PublicInstallError::RootRequired,
            PublicInstallError::UnsupportedSystem,
            PublicInstallError::InvalidRelease,
            PublicInstallError::InstallFailed,
        ]
        .map(|error| error.to_string());
        assert_eq!(messages[0], "Run pkg-install without options.");
        assert_eq!(messages[1], "Run pkg-install as root.");
        assert!(messages.iter().all(|message| {
            !message.contains("nix")
                && !message.contains("/var/")
                && !message.contains("metadata")
                && !message.contains("target")
        }));
    }
}
