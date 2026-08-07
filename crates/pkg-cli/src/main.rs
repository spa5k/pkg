use std::process::ExitCode as ProcessExitCode;

use clap::Parser;
use pkg_cli::cli::Cli;
use pkg_cli::exit::ExitCode;
use pkg_cli::ux::{CommandError, OutputMode, write_error};

fn main() -> ProcessExitCode {
    let cli = Cli::parse();
    if let Err(error) = cli.validate() {
        let command_error = CommandError::new(
            error.exit_code(),
            error.to_string(),
            "name one or more installed packages, or pass --all",
        );
        let mode = OutputMode::from_flags(cli.json(), cli.jsonl());
        if write_error(
            std::io::stdout(),
            std::io::stderr(),
            mode,
            cli.command_name(),
            &command_error,
        )
        .is_err()
        {
            return ProcessExitCode::FAILURE;
        }
        return error.exit_code().into();
    }

    // PR-23 deliberately lands only the public shell. PR-24 replaces this
    // closed development-build response with command dispatch.
    let error = CommandError::new(
        ExitCode::EngineUnavailable,
        "command execution is not available in this development build",
        "use `pkg --help` to inspect the command contract",
    );
    let mode = OutputMode::from_flags(cli.json(), cli.jsonl());
    if write_error(
        std::io::stdout(),
        std::io::stderr(),
        mode,
        cli.command_name(),
        &error,
    )
    .is_err()
    {
        return ProcessExitCode::FAILURE;
    }
    error.exit_code().into()
}
