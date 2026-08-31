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
use pkg_cli::path::{
    self, HostFamily, PathObservation, RawNixVisibility, StateLocation, StateLocationError,
    observe_raw_nix_visibility,
};
use pkg_cli::support::SupportBundle;
use pkg_cli::ux::{CommandError, OutputMode, write_error};
use pkg_installer::{UninstallErrorCode, uninstall_linux_production, uninstall_macos_production};
use pkg_nix::{DetectionDisposition, detect_unmanaged_nix};

fn main() -> ProcessExitCode {
    let cli = Cli::parse();
    if let Err(error) = cli.validate() {
        let command_error = CommandError::new(error.exit_code(), error.to_string(), error.hint());
        return write_command_error(&cli, &command_error);
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

    let location = match resolve_cli_state_location(&cli) {
        Ok(location) => location,
        Err(error) => return write_state_location_error(&cli, error),
    };

    let operations = match LocalStateOperations::open(&location, Uid::effective().as_raw()) {
        Ok(operations) => operations,
        Err(error) => return write_command_error(&cli, &error),
    };
    install_crash_reporter(&location);
    let mut engine = CoreEngine::new(operations);

    let exit = match execute_command_with_operation_log(
        &cli,
        &mut engine,
        &location.state_root().join("logs"),
        std::io::stdout(),
        std::io::stderr(),
    ) {
        Ok(exit) => exit,
        Err(_) => return ProcessExitCode::FAILURE,
    };
    write_command_log(&location, cli.command_name(), exit);
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
    if let Err(error) = validate_live_uninstall_output(cli) {
        return write_command_error(cli, &error);
    }
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

fn validate_live_uninstall_output(cli: &Cli) -> Result<(), CommandError> {
    if cfg!(any(
        target_os = "linux",
        all(target_os = "macos", target_arch = "aarch64")
    )) && !cli.dry_run()
        && (cli.json() || cli.jsonl())
    {
        return Err(CommandError::new(
            ExitCode::Config,
            "live uninstall requires plain output",
            "remove --json or --jsonl, or use --dry-run",
        ));
    }
    Ok(())
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

fn resolve_cli_state_location(cli: &Cli) -> Result<StateLocation, StateLocationError> {
    let host = HostFamily::detect().ok_or(StateLocationError::UnsupportedHost)?;
    path::resolve_state_location(host, cli.state())
}

fn write_state_location_error(cli: &Cli, error: StateLocationError) -> ProcessExitCode {
    let command_error = match error {
        StateLocationError::RelativeAlternateRoot => CommandError::new(
            ExitCode::Config,
            "the alternate state root must be an absolute path",
            "use an absolute path for --state or PKG_STATE_DIR",
        ),
        StateLocationError::SystemHomeUnavailable => CommandError::new(
            ExitCode::Config,
            "the invoking user's system home directory is unavailable",
            "repair the effective user's system account and retry",
        ),
        StateLocationError::UnsupportedHost => CommandError::new(
            ExitCode::Config,
            "this operating system is not supported",
            "run pkg on Linux or macOS",
        ),
    };
    write_command_error(cli, &command_error)
}

fn write_command_error(cli: &Cli, error: &CommandError) -> ProcessExitCode {
    let mode = OutputMode::from_flags(cli.json(), cli.jsonl());
    if write_error(
        std::io::stdout(),
        std::io::stderr(),
        mode,
        cli.command_name(),
        error,
    )
    .is_err()
    {
        return ProcessExitCode::FAILURE;
    }
    error.exit_code().into()
}

fn install_crash_reporter(location: &StateLocation) {
    let Ok(context) = CrashContext::new(CrashPhase::Cli, None, None) else {
        return;
    };
    CrashReporter::new(location.state_root().join("crash/latest.json"), context).install();
}

fn write_command_log(location: &StateLocation, command_name: &'static str, exit_code: ExitCode) {
    let Ok(log) = StructuredLog::open(location.state_root().join("logs"), LogConfig::default())
    else {
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
        command_name,
        Some(exit_code.as_u8()),
    ));
}

fn run_doctor(cli: &Cli, args: &DoctorArgs) -> ProcessExitCode {
    let location = match resolve_cli_state_location(cli) {
        Ok(location) => location,
        Err(error) => return write_state_location_error(cli, error),
    };
    let state_root = location.state_root().to_owned();
    let expected_bin = state_root.join("current/bin");
    let path = std::env::var_os("PATH");
    let path_entries = path
        .as_ref()
        .map(|value| std::env::split_paths(value).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut inputs = DoctorInputs::local_development(
        state_root,
        PathObservation::inspect(&expected_bin, &path_entries),
    );
    inputs.raw_nix_visibility = path
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        .map_or(RawNixVisibility::Unknown, observe_raw_nix_visibility);
    inputs.expected_state_uid = Some(Uid::effective().as_raw());
    let (managed_runtime, channel, managed_ownership) = observe_production_subsystems();
    (inputs.managed_runtime, inputs.channel) = (managed_runtime, channel);
    if let Some(system) = inputs.system {
        let environment_keys = std::env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
        let detection = detect_unmanaged_nix(
            std::path::Path::new("/"),
            system,
            &path_entries,
            &environment_keys,
        );
        inputs.unmanaged_nix = if detection.disposition() == DetectionDisposition::Clean
            || managed_ownership
        {
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
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use tempfile::TempDir;

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

    #[test]
    fn live_uninstall_accepts_only_plain_output() {
        let plain = Cli::try_parse_from(["pkg", "uninstall", "--yes"]).unwrap();
        assert!(validate_live_uninstall_output(&plain).is_ok());

        for flag in ["--json", "--jsonl"] {
            let live = Cli::try_parse_from(["pkg", flag, "uninstall", "--yes"]).unwrap();
            assert_eq!(
                validate_live_uninstall_output(&live).is_err(),
                cfg!(any(
                    target_os = "linux",
                    all(target_os = "macos", target_arch = "aarch64")
                ))
            );

            let dry_run = Cli::try_parse_from(["pkg", flag, "--dry-run", "uninstall"]).unwrap();
            assert!(validate_live_uninstall_output(&dry_run).is_ok());
        }
    }

    #[test]
    fn invalid_alternates_create_no_state_or_observability_files() {
        let home = TempDir::new().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let outside = TempDir::new().unwrap();
        fs::set_permissions(outside.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(home.path()).unwrap().uid();

        let outside_state = outside.path().join("outside-state");
        let location = StateLocation::alternate(outside_state.clone(), home.path().to_path_buf());
        assert!(LocalStateOperations::open(&location, uid).is_err());
        assert!(!outside_state.exists());

        let link = home.path().join("linked");
        symlink(outside.path(), &link).unwrap();
        let linked_state = link.join("linked-state");
        let location = StateLocation::alternate(linked_state, home.path().to_path_buf());
        assert!(LocalStateOperations::open(&location, uid).is_err());
        assert!(!outside.path().join("linked-state").exists());
    }
}
