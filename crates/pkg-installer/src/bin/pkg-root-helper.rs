use std::process::ExitCode;

fn main() -> ExitCode {
    if run() {
        ExitCode::SUCCESS
    } else {
        eprintln!("managed package service failed");
        ExitCode::FAILURE
    }
}

fn run() -> bool {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    {
        arguments.len() == 1 && pkg_installer::run_linux_root_helper_from_activation().is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        requested_macos_mode(&arguments) && pkg_installer::run_macos_root_helper().is_ok()
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
    fn launchd_serve_mode_does_not_widen_to_mount_or_custom_paths() {
        assert!(requested_macos_mode(&[
            "pkg-root-helper".into(),
            "--serve-macos".into(),
        ]));
        assert!(!requested_macos_mode(&[
            "pkg-root-helper".into(),
            "--mount-store-volume".into(),
        ]));
        assert!(!requested_macos_mode(&[
            "pkg-root-helper".into(),
            "--serve-macos".into(),
            "/tmp/alternate.sock".into(),
        ]));
    }
}
