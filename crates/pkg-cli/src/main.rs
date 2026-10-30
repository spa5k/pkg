//! `pkg` binary entry point.

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    // A closed pipe, as in `pkg list | head -1`, ends pkg quietly like other
    // shell tools. Rust ignores SIGPIPE by default, so a write would fail.
    // SAFETY: no other thread runs yet, and SIG_DFL is a valid disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // `println!` and `eprintln!` panic when a write fails, for example on a
    // full disk. That is an error of the output, not a crash of pkg.
    let crash = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if output_failure(info.payload()).is_none() {
            crash(info);
        }
    }));
    match std::panic::catch_unwind(|| pkg_cli::commands::run(std::env::args_os())) {
        Ok(code) => code,
        Err(payload) => {
            let Some(cause) = output_failure(&*payload) else {
                std::panic::resume_unwind(payload)
            };
            // stderr can be the stream that failed.
            let _ = writeln!(std::io::stderr(), "pkg: cannot write the output: {cause}");
            ExitCode::from(pkg_cli::commands::FAILURE)
        }
    }
}

/// The cause of a failed `print!` or `eprint!` panic, or `None` for other
/// panics.
fn output_failure(payload: &(dyn std::any::Any + Send)) -> Option<&str> {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())?;
    let (stream, cause) = message
        .strip_prefix("failed printing to ")?
        .split_once(": ")?;
    ["stdout", "stderr"].contains(&stream).then_some(cause)
}
