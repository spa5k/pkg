//! `pkg` binary entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    pkg_cli::commands::run(std::env::args_os())
}
