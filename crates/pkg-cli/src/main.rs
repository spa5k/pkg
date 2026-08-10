use std::process::ExitCode as ProcessExitCode;

use clap::Parser;
use pkg_cli::cli::{Cli, Command, DoctorArgs};
use pkg_cli::commands::doctor::{DoctorInputs, DoctorReport};
use pkg_cli::commands::execute::{UnavailableEngine, execute_command};
use pkg_cli::completion::write_completion;
use pkg_cli::crash::{CrashContext, CrashPhase, CrashReporter};
use pkg_cli::exit::ExitCode;
use pkg_cli::log::{LogConfig, LogLevel, LogRecord, StructuredLog};
use pkg_cli::path::{HostFamily, PathObservation, default_state_root};
use pkg_cli::support::SupportBundle;
use pkg_cli::ux::{CommandError, OutputMode, write_error};
use pkg_nix::{DetectionDisposition, detect_unmanaged_nix};

fn main() -> ProcessExitCode {
    let cli = Cli::parse();
    if let Err(error) = cli.validate() {
        let command_error = CommandError::new(error.exit_code(), error.to_string(), error.hint());
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
        Command::Doctor(args) => return run_doctor(&cli, args),
        _ => {}
    }

    install_crash_reporter(&cli);

    let exit = match execute_command(
        &cli,
        &mut UnavailableEngine,
        std::io::stdout(),
        std::io::stderr(),
    ) {
        Ok(exit) => exit,
        Err(_) => return ProcessExitCode::FAILURE,
    };
    write_command_log(&cli, exit);
    exit.into()
}

fn observability_root(cli: &Cli) -> Option<std::path::PathBuf> {
    cli.state()
        .map(std::path::Path::to_owned)
        .or_else(|| HostFamily::detect().and_then(default_state_root))
}

fn install_crash_reporter(cli: &Cli) {
    let Some(root) = observability_root(cli) else {
        return;
    };
    let Ok(context) = CrashContext::new(CrashPhase::Cli, None, None) else {
        return;
    };
    CrashReporter::new(root.join("crash/latest.json"), context).install();
}

fn write_command_log(cli: &Cli, exit_code: ExitCode) {
    let Some(root) = observability_root(cli) else {
        return;
    };
    let Ok(log) = StructuredLog::open(root.join("logs"), LogConfig::default()) else {
        return;
    };
    let level = if exit_code == ExitCode::Ok {
        LogLevel::Info
    } else {
        LogLevel::Error
    };
    let _ = log.append(&LogRecord::command(
        level,
        "command_finished",
        cli.command_name(),
        Some(exit_code.as_u8()),
    ));
}

fn run_doctor(cli: &Cli, args: &DoctorArgs) -> ProcessExitCode {
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
    let mut inputs = DoctorInputs::local_development(
        state_root,
        PathObservation::inspect(&expected_bin, &path_entries),
    );
    if let Some(system) = inputs.system {
        let environment_keys = std::env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
        let detection = detect_unmanaged_nix(
            std::path::Path::new("/"),
            system,
            &path_entries,
            &environment_keys,
        );
        inputs.unmanaged_nix = if detection.disposition() == DetectionDisposition::Clean {
            pkg_cli::commands::doctor::UnmanagedNixObservation::Clean
        } else {
            pkg_cli::commands::doctor::UnmanagedNixObservation::Refused {
                signals: detection
                    .findings()
                    .iter()
                    .map(|finding| finding.id().to_owned())
                    .collect(),
                definite: detection.has_unmanaged_evidence() && !detection.has_ownership_claim(),
            }
        };
    }
    let report = DoctorReport::evaluate(&inputs);
    if args.support() {
        let bundle = SupportBundle::collect(&report, &inputs.state_root);
        return if bundle.write_preview(std::io::stdout()).is_ok() {
            ExitCode::Ok.into()
        } else {
            ProcessExitCode::FAILURE
        };
    }
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
