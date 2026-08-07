use std::process::ExitCode as ProcessExitCode;

use clap::Parser;
use pkg_cli::cli::{Cli, Command};
use pkg_cli::commands::doctor::{DoctorInputs, DoctorReport};
use pkg_cli::completion::write_completion;
use pkg_cli::exit::ExitCode;
use pkg_cli::path::{HostFamily, PathObservation, default_state_root};
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

    match cli.parsed_command() {
        Command::Completion(args) => {
            return match write_completion(args.shell(), std::io::stdout()) {
                Ok(()) => ExitCode::Ok.into(),
                Err(_) => ProcessExitCode::FAILURE,
            };
        }
        Command::Doctor => return run_doctor(&cli),
        _ => {}
    }

    // PR-24 replaces this closed response for the remaining command set.
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

fn run_doctor(cli: &Cli) -> ProcessExitCode {
    let Some(host) = HostFamily::detect() else {
        return ExitCode::Config.into();
    };
    let state_root = cli
        .state()
        .map(std::path::Path::to_owned)
        .or_else(|| default_state_root(host));
    let Some(state_root) = state_root else {
        return ExitCode::Config.into();
    };
    let expected_bin = state_root.join("current/bin");
    let path_entries = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let inputs = DoctorInputs::local_development(
        state_root,
        PathObservation::inspect(&expected_bin, &path_entries),
    );
    let report = DoctorReport::evaluate(&inputs);
    let rendered = match OutputMode::from_flags(cli.json(), cli.jsonl()) {
        OutputMode::Human => report.write_human(std::io::stdout()),
        OutputMode::Json => report.write_json(std::io::stdout()),
        OutputMode::JsonLines => report.write_jsonl(std::io::stdout()),
    };
    if rendered.is_err() {
        ProcessExitCode::FAILURE
    } else {
        report.exit_code().into()
    }
}
