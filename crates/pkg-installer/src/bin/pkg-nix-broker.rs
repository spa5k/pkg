//! Production entry point for the unprivileged package broker.

#[expect(clippy::print_stdout, reason = "the broker never prints to stdout")]
#[expect(clippy::print_stderr, reason = "the broker only failure output")]
fn main() {
    if !run() {
        eprintln!("managed package service failed");
        std::process::exit(1);
    }
}

fn run() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::args_os().count() == 1
            && pkg_installer::run_linux_broker_from_activation().is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        requested_macos_mode(&arguments) && pkg_installer::run_macos_broker().is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = std::env::args_os();
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
