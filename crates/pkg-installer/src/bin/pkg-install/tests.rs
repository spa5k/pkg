//! Tests for the `pkg-install` binary.

use super::*;
const FIXTURE_ROOT: &str = include_str!("../../../../../fixtures/channel-v1/root.json");

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
        .map(|(metadata, targets)| (metadata.scheme().to_owned(), targets.scheme().to_owned(),)),
        Ok(("https".to_owned(), "https".to_owned())),
    );
    assert!(release_urls(Some("http://host/metadata/"), Some("https://host/targets/"),).is_err());
    assert!(release_urls(Some("https://host/metadata"), Some("https://host/targets/"),).is_err());
    assert!(trusted_root(Some(FIXTURE_ROOT)).is_ok());
    assert!(matches!(
        trusted_root(None),
        Err(PublicInstallError::InvalidRelease)
    ));
}

#[test]
fn invocation_requires_the_exact_product_repair_option() {
    assert_eq!(
        parse_invocation([]),
        Ok((Invocation::InstallOrUpgrade, None))
    );
    assert_eq!(
        parse_invocation([OsString::from("--repair-product-assets")]),
        Ok((Invocation::RepairProductAssets, None))
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
        Ok(())
    );
    assert_eq!(
        validate_invocation_system(Invocation::InstallOrUpgrade, System::X8664Darwin,),
        Err(PublicInstallError::UnsupportedSystem)
    );
}

#[test]
fn invocation_accepts_one_optional_channel_base_url() {
    assert_eq!(
        parse_invocation([
            OsString::from("--channel"),
            OsString::from("https://channel.test/n/")
        ]),
        Ok((
            Invocation::InstallOrUpgrade,
            Some("https://channel.test/n/".to_owned())
        ))
    );
    assert_eq!(
        parse_invocation([
            OsString::from("--channel"),
            OsString::from("https://channel.test/n/"),
            OsString::from("--repair-product-assets"),
        ]),
        Ok((
            Invocation::RepairProductAssets,
            Some("https://channel.test/n/".to_owned())
        ))
    );
    for arguments in [
        vec![OsString::from("--channel")],
        vec![
            OsString::from("--channel"),
            OsString::from("https://a.test/n/"),
            OsString::from("https://b.test/n/"),
        ],
        vec![
            OsString::from("--channel"),
            OsString::from("https://a.test/n/"),
            OsString::from("--channel"),
            OsString::from("https://b.test/n/"),
        ],
    ] {
        assert_eq!(
            parse_invocation(arguments),
            Err(PublicInstallError::InvalidInvocation)
        );
    }
}

#[test]
fn channel_urls_prefer_the_command_line_then_the_environment() {
    assert!(matches!(
        channel_urls(Some("http://channel.test/n/"), None),
        Err(PublicInstallError::InvalidRelease)
    ));
    assert!(matches!(
        channel_urls(Some("https://channel.test/n"), Some("https://other.test/n/")),
        Ok((metadata, _)) if metadata.host_str() == Some("channel.test"),
    ));
    assert!(matches!(
        channel_urls(None, Some("https://channel.test/n")),
        Ok((metadata, targets))
            if metadata.as_str() == "https://channel.test/n/metadata/"
                && targets.as_str() == "https://channel.test/n/targets/",
    ));
    assert!(matches!(
        channel_urls(None, Some("http://channel.test/n/")),
        Err(PublicInstallError::InvalidRelease)
    ));
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
        "Run pkg-install without options or with --repair-product-assets. Use --channel <BASE_URL> to select the release channel."
    );
    assert_eq!(messages[1], "Run pkg-install as root.");
    assert!(messages.iter().all(|message| {
        !message.contains("nix")
            && !message.contains("/var/")
            && !message.contains("metadata")
            && !message.contains("target")
    }));
}
