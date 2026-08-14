use std::process::ExitCode as ProcessExitCode;

use clap::Parser;
use nix::unistd::Uid;
use pkg_cli::cli::{Cli, Command, DoctorArgs};
use pkg_cli::commands::doctor::{DoctorInputs, DoctorReport, observe_production_subsystems};
use pkg_cli::commands::execute::{
    CommandEngine, CommandRequest, CommandResult, CoreEngine, execute_command,
    execute_command_with_operation_log,
};
use pkg_cli::commands::local::{LocalStateOperations, confirm_destructive};
use pkg_cli::completion::write_completion;
use pkg_cli::crash::{CrashContext, CrashPhase, CrashReporter};
use pkg_cli::exit::ExitCode;
use pkg_cli::log::{LogConfig, LogLevel, LogRecord, StructuredLog};
use pkg_cli::path::{HostFamily, PathObservation, default_state_root};
use pkg_cli::support::SupportBundle;
use pkg_cli::ux::{CommandError, OutputMode, write_error};
use pkg_installer::{UninstallErrorCode, uninstall_linux_production, uninstall_macos_production};
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
        Command::Uninstall => return run_uninstall(&cli),
        _ => {}
    }

    install_crash_reporter(&cli);

    let trusted_home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let state_root = observability_root(&cli).unwrap_or_default();
    let mut engine = CoreEngine::new(LocalStateOperations::open(
        &trusted_home,
        &state_root,
        Uid::effective().as_raw(),
    ));

    let exit = match execute_command_with_operation_log(
        &cli,
        &mut engine,
        &state_root.join("logs"),
        std::io::stdout(),
        std::io::stderr(),
    ) {
        Ok(exit) => exit,
        Err(_) => return ProcessExitCode::FAILURE,
    };
    write_command_log(&cli, exit);
    exit.into()
}

struct UninstallEngine;

impl CommandEngine for UninstallEngine {
    fn execute(&mut self, request: &CommandRequest) -> Result<CommandResult, CommandError> {
        if request.command() != &Command::Uninstall {
            return Err(CommandError::new(
                ExitCode::Config,
                "the uninstall command is invalid",
                "run `pkg uninstall`",
            ));
        }
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            return Err(CommandError::new(
                ExitCode::Config,
                "uninstall is not available on this system",
                "do not remove pkg files manually",
            ));
        }
        if !Uid::effective().is_root() {
            return Err(uninstall_command_error(
                UninstallErrorCode::PrivilegeRequired,
            ));
        }
        if !request.dry_run() {
            confirm_destructive(request.yes(), "Uninstall pkg?")?;
        }
        let result = if cfg!(target_os = "macos") {
            uninstall_macos_production(request.dry_run())
        } else {
            uninstall_linux_production(request.dry_run())
        };
        let actions = result.map_err(|error| uninstall_command_error(error.code()))?;
        let (summary, status) = if actions == 0 {
            ("pkg is not installed.", "absent")
        } else if request.dry_run() {
            ("pkg can be safely uninstalled.", "planned")
        } else {
            ("pkg is uninstalled.", "removed")
        };
        CommandResult::new(
            summary,
            serde_json::Map::from_iter([
                ("actions".to_owned(), serde_json::json!(actions)),
                ("status".to_owned(), serde_json::json!(status)),
            ]),
            Vec::new(),
        )
        .map_err(|_| {
            CommandError::new(
                ExitCode::EngineUnavailable,
                "pkg could not report the uninstall result",
                "run `pkg doctor`",
            )
        })
    }
}

fn run_uninstall(cli: &Cli) -> ProcessExitCode {
    match execute_command(
        cli,
        &mut UninstallEngine,
        std::io::stdout(),
        std::io::stderr(),
    ) {
        Ok(exit) => exit.into(),
        Err(_) => ProcessExitCode::FAILURE,
    }
}

fn uninstall_command_error(code: UninstallErrorCode) -> CommandError {
    let (exit, message, hint) = match code {
        UninstallErrorCode::PrivilegeRequired => (
            ExitCode::Permission,
            "administrator access is required",
            "run `sudo pkg uninstall`",
        ),
        UninstallErrorCode::UnmanagedNix => (
            ExitCode::UnmanagedNix,
            "pkg found system state that it does not own",
            "restore the verified pkg installation before you retry",
        ),
        UninstallErrorCode::InvalidManifest | UninstallErrorCode::OwnershipRefused => (
            ExitCode::VerifyFail,
            "pkg could not verify this installation",
            "run `pkg doctor` before you retry",
        ),
        UninstallErrorCode::ServiceStopFailed => (
            ExitCode::EngineUnavailable,
            "pkg could not stop its services",
            "run `pkg uninstall` again",
        ),
        UninstallErrorCode::CleanupIncomplete | UninstallErrorCode::ResidueRemaining => (
            ExitCode::EngineUnavailable,
            "pkg uninstall did not complete",
            "run `pkg uninstall` again",
        ),
    };
    CommandError::new(exit, message, hint)
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
    inputs.expected_state_uid = Some(Uid::effective().as_raw());
    (inputs.managed_runtime, inputs.channel) = observe_production_subsystems();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_errors_are_stable_and_redacted() {
        for code in [
            UninstallErrorCode::InvalidManifest,
            UninstallErrorCode::PrivilegeRequired,
            UninstallErrorCode::OwnershipRefused,
            UninstallErrorCode::UnmanagedNix,
            UninstallErrorCode::ServiceStopFailed,
            UninstallErrorCode::CleanupIncomplete,
            UninstallErrorCode::ResidueRemaining,
        ] {
            let error = uninstall_command_error(code);
            let public = format!("{} {}", error.message(), error.hint());
            for forbidden in ["nix", "/opt/", "/var/", "http", "store path", "trust root"] {
                assert!(!public.to_ascii_lowercase().contains(forbidden));
            }
        }
    }
}
