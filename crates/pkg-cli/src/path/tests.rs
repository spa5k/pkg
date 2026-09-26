//! Tests for the `path` module.

use std::fs;

use super::*;

#[test]
fn snippets_are_dynamic_idempotent_and_never_expose_managed_nix() {
    for host in [HostFamily::Linux, HostFamily::MacOs] {
        let snippet = shell_init(host);
        assert!(snippet.contains("current/bin"));
        assert!(snippet.contains("case \":$PATH:\""));
        assert!(snippet.contains("$HOME"));
        assert!(!snippet.contains("XDG_DATA_HOME"));
        assert!(!snippet.contains("/nix/store"));
        assert!(!snippet.contains("/opt/pkg/nix"));
    }
}

#[test]
fn repeated_shell_setup_keeps_one_manpath_entry_and_preserves_system_defaults() {
    let home = std::env::var("HOME").unwrap();
    for host in [HostFamily::Linux, HostFamily::MacOs] {
        let man = production_state_root(host, Path::new(&home))
            .join("current/share/man")
            .to_str()
            .unwrap()
            .to_owned();
        for shell in ["/bin/bash", "/bin/zsh"] {
            // Linux runners do not all include zsh; native macOS covers both.
            if !Path::new(shell).exists() {
                assert_eq!(shell, "/bin/zsh");
                continue;
            }
            for initial in [
                None,
                Some(""),
                Some("/custom/man"),
                Some(":/custom/man:"),
                Some(man.as_str()),
            ] {
                let mut command = std::process::Command::new(shell);
                if shell.ends_with("bash") {
                    command.args(["--noprofile", "--norc"]);
                } else {
                    command.arg("-f");
                }
                command
                    .env_remove("BASH_ENV")
                    .env_remove("ENV")
                    .env_remove("MANPATH");
                if let Some(initial) = initial {
                    command.env("MANPATH", initial);
                }
                let snippet = shell_init(host);
                let output = command
                    .args([
                        "-c",
                        &format!("{snippet}{snippet}{snippet}printf '%s' \"$MANPATH\""),
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{shell}: {:?}", output.stderr);
                let actual = String::from_utf8(output.stdout).unwrap();
                let expected = if initial == Some(man.as_str()) {
                    man.clone()
                } else {
                    format!("{man}:{}", initial.unwrap_or_default())
                };
                assert_eq!(actual, expected, "{shell} {host:?} {initial:?}");
                assert_eq!(actual.split(':').filter(|entry| *entry == man).count(), 1);
            }
        }
    }
}

#[test]
fn production_uses_system_home_not_a_spoofed_environment_home() {
    let spoofed_environment_home = Path::new("/spoofed");
    let location = resolve_state_location_from(
        HostFamily::Linux,
        None,
        Some(PathBuf::from("/home/u")),
        Some(PathBuf::new()),
    )
    .unwrap();
    assert!(location.is_production());
    assert_eq!(location.state_root(), Path::new("/home/u/.local/share/pkg"));
    assert_ne!(
        location.state_root(),
        production_state_root(HostFamily::Linux, spoofed_environment_home)
    );
    assert_eq!(
        production_state_root(HostFamily::MacOs, Path::new("/Users/u")),
        Path::new("/Users/u/Library/Application Support/pkg")
    );
    assert_eq!(location.trusted_boundary(), Path::new("/home/u"));
}

#[test]
fn explicit_roots_are_absolute_alternates() {
    let location = resolve_state_location_from(
        HostFamily::Linux,
        None,
        Some(PathBuf::from("/home/u")),
        Some(PathBuf::from("/custom/pkg")),
    )
    .unwrap();
    assert!(!location.is_production());
    assert_eq!(location.state_root(), Path::new("/custom/pkg"));
    assert_eq!(location.trusted_boundary(), Path::new("/home/u"));

    let relative = resolve_state_location_from(
        HostFamily::Linux,
        Some(Path::new("relative")),
        Some(PathBuf::from("/home/u")),
        None,
    );
    assert_eq!(relative, Err(StateLocationError::RelativeAlternateRoot));
}

#[test]
fn missing_system_home_fails_without_an_environment_fallback() {
    assert_eq!(
        resolve_state_location_from(HostFamily::Linux, None, None, None),
        Err(StateLocationError::SystemHomeUnavailable)
    );
}

#[test]
fn path_observation_uses_exact_components_only() {
    let expected = Path::new("/user/pkg/current/bin");
    let observation = PathObservation::inspect(
        expected,
        [
            Path::new("/usr/bin"),
            expected,
            Path::new("/user/pkg/current/bin-extra"),
            expected,
        ],
    );
    assert_eq!(observation.first_index(), Some(1));
    assert_eq!(observation.duplicate_count(), 2);
}

#[test]
#[cfg(unix)]
fn path_observation_counts_managed_commands_shadowed_earlier() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "pkg-path-shadow-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let earlier = root.join("earlier");
    let expected = root.join("current/bin");
    fs::create_dir_all(&earlier).unwrap();
    fs::create_dir_all(&expected).unwrap();
    for path in [earlier.join("rg"), expected.join("rg"), expected.join("fd")] {
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let observation = PathObservation::inspect(&expected, [&earlier, &expected]);
    assert_eq!(observation.shadowed_count(), 1);
    assert!(observation.shadow_scan_complete());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(unix)]
fn raw_nix_hidden_without_an_executable_in_absolute_readable_dirs() {
    let root = std::env::temp_dir().join(format!(
        "pkg-path-nix-hidden-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();

    let visibility = observe_raw_nix_visibility(&bin.display().to_string());
    assert_eq!(visibility, RawNixVisibility::Hidden);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(unix)]
fn raw_nix_visible_through_an_absolute_readable_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "pkg-path-nix-visible-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let nix = bin.join("nix");
    fs::write(&nix, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o755)).unwrap();

    // A trailing relative entry must not downgrade the visible signal.
    let visibility = observe_raw_nix_visibility(&format!("{}:relative", bin.display()));
    assert_eq!(visibility, RawNixVisibility::Visible);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn raw_nix_unknown_for_relative_path_entries() {
    assert_eq!(
        observe_raw_nix_visibility("relative/bin"),
        RawNixVisibility::Unknown
    );
}

#[test]
#[cfg(unix)]
fn raw_nix_unknown_for_unreadable_directory_entries() {
    use std::os::unix::fs::PermissionsExt;

    if Uid::effective().is_root() {
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "pkg-path-nix-unreadable-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o000)).unwrap();

    let visibility = observe_raw_nix_visibility(&bin.display().to_string());
    assert_eq!(visibility, RawNixVisibility::Unknown);
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn raw_nix_unknown_for_invalid_empty_path() {
    assert_eq!(observe_raw_nix_visibility(""), RawNixVisibility::Unknown);
}
