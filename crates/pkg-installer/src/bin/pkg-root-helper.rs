use pkg_installer::run_linux_root_helper_from_activation;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run_linux_root_helper_from_activation() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
