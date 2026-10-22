//! `pkg` binary entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    // A closed pipe, as in `pkg list | head -1`, ends pkg quietly like other
    // shell tools. Rust ignores SIGPIPE by default, so a write would fail.
    // SAFETY: no other thread runs yet, and SIG_DFL is a valid disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    pkg_cli::commands::run(std::env::args_os())
}
