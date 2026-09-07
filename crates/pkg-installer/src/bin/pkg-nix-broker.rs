//! Production entry point for the unprivileged package broker.

#[allow(clippy::print_stdout, reason = "the broker never prints to stdout")]
#[allow(clippy::print_stderr, reason = "the broker only failure output")]
fn main() {
    if let Err(error) = run_with_reason() {
        eprintln!("broker failure: code={:?}", error.code());
        eprintln!("managed package service failed");
        std::process::exit(1);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_with_reason() -> Result<(), pkg_installer::ServiceError> {
    #[cfg(target_os = "linux")]
    {
        pkg_installer::run_linux_broker_from_activation()
    }
    #[cfg(target_os = "macos")]
    {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        if !requested_macos_mode(&arguments) {
            eprintln!("broker failure: code=Arguments");
            std::process::exit(1);
        }
        pkg_installer::run_macos_broker()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run() -> bool {
    let _ = std::env::args_os();
    false
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
