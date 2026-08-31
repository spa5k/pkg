use std::process::ExitCode;

#[expect(clippy::print_stdout, reason = "the helper never prints to stdout")]
#[expect(clippy::print_stderr, reason = "the helper only failure output")]
fn main() -> ExitCode {
    if run() {
        ExitCode::SUCCESS
    } else {
        #[expect(clippy::print_stderr, reason = "the helper's only failure output")]
        eprintln!("managed package service failed");
        ExitCode::FAILURE
    }
}

fn run() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::args_os().count() == 1
            && pkg_installer::run_linux_root_helper_from_activation().is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        match requested_macos_mode(&arguments) {
            Some(MacOsMode::Serve) => pkg_installer::run_macos_root_helper().is_ok(),
            None => false,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = std::env::args_os();
        false
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacOsMode {
    Serve,
}

#[cfg(target_os = "macos")]
fn requested_macos_mode(arguments: &[std::ffi::OsString]) -> Option<MacOsMode> {
    if arguments.len() != 2 {
        return None;
    }
    match arguments[1].to_str() {
        Some("--serve-macos") => Some(MacOsMode::Serve),
        Some(_) | None => None,
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn launchd_modes_are_exact_and_accept_no_dynamic_input() {
        assert_eq!(
            requested_macos_mode(&["pkg-root-helper".into(), "--serve-macos".into()]),
            Some(MacOsMode::Serve)
        );
        assert_eq!(
            requested_macos_mode(&[
                "pkg-root-helper".into(),
                "--mount-store-volume".into(),
                "01234567-89AB-CDEF-0123-456789ABCDEF".into(),
            ]),
            None
        );
        assert_eq!(
            requested_macos_mode(&[
                "pkg-root-helper".into(),
                "--provision-store-volume".into(),
                "disk3".into(),
            ]),
            None
        );
        assert_eq!(
            requested_macos_mode(&[
                "pkg-root-helper".into(),
                "--serve-macos".into(),
                "/tmp/alternate.sock".into(),
            ]),
            None
        );
    }
}
