//! Production entry point for the unprivileged package broker.

fn main() {
    if !run() {
        eprintln!("managed package service failed");
        std::process::exit(1);
    }
}

fn run() -> bool {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    {
        arguments.len() == 1 && pkg_installer::run_linux_broker_from_activation().is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        requested_macos_mode(&arguments) && pkg_installer::run_macos_broker().is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = arguments;
        false
    }
}

#[cfg(target_os = "macos")]
fn requested_macos_mode(arguments: &[std::ffi::OsString]) -> bool {
    arguments.len() == 2 && arguments[1] == "--serve-macos"
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn launchd_mode_is_exact_and_closed() {
        assert!(requested_macos_mode(&[
            "pkg-nix-broker".into(),
            "--serve-macos".into(),
        ]));
        assert!(!requested_macos_mode(&["pkg-nix-broker".into()]));
        assert!(!requested_macos_mode(&[
            "pkg-nix-broker".into(),
            "--serve-macos".into(),
            "extra".into(),
        ]));
        assert!(!requested_macos_mode(&[
            "pkg-nix-broker".into(),
            "--socket".into(),
        ]));
    }
}
