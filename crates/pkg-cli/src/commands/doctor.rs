//! Doctor: read-only runtime, profile, launcher, tap, and setup status.
//!
//! Unhealthy required components cause a nonzero exit. A fresh profile,
//! a home without taps, or a skipped platform component does not. An off
//! background job or a short `PATH` needs attention but does not fail.

use std::process::ExitCode;

use crate::cli::Cli;
use crate::config::{self, Config};
use crate::nix;
use crate::output::{self, DoctorRow, UNHEALTHY_STATUSES};

use super::{CommandError, FAILURE, discover_runtime, print_json};

/// One doctor observation with a detail line; every row carries one.
fn observed(component: &str, status: &str, detail: String) -> DoctorRow {
    DoctorRow {
        component: component.to_string(),
        status: status.to_string(),
        detail: Some(detail),
    }
}

pub(super) fn doctor(cli: &Cli) -> Result<(), CommandError> {
    let rows = doctor_rows(cli)?;
    let healthy = !rows
        .iter()
        .any(|row| UNHEALTHY_STATUSES.contains(&row.status.as_str()));
    if cli.json {
        print_json(&output::envelope("doctor", &rows))?;
    } else {
        print!("{}", output::render_doctor(&rows, cli.verbose));
    }
    if healthy {
        Ok(())
    } else {
        // Unhealthy required components fail doctor instead of passing
        // silently; doctor itself never repairs anything.
        Err(CommandError::Reported(ExitCode::from(FAILURE)))
    }
}

fn doctor_rows(cli: &Cli) -> Result<Vec<DoctorRow>, CommandError> {
    let paths = config::paths()?;
    // A bad settings file is one doctor finding. The settings can name the
    // Nix runtime, so the runtime checks wait for a readable file.
    let (config, config_error) = match Config::load(&paths.config_file) {
        Ok(config) => (config, None),
        Err(error) => (Config::default(), Some(error.to_string())),
    };
    let mut rows = Vec::new();
    if config_error.is_some() {
        rows.push(observed(
            "nix runtime",
            "unknown",
            String::from("cannot be checked until the settings file is fixed"),
        ));
    } else {
        runtime_rows(cli, &config, &paths, &mut rows)?;
    }
    let (config_status, config_detail) = match config_error {
        Some(error) => ("unreadable", error),
        None if paths.config_file.exists() => ("present", paths.config_file.display().to_string()),
        None => ("defaults", paths.config_file.display().to_string()),
    };
    // Use the query loader so doctor and installation agree on tap health.
    rows.extend(crate::tap::health(&paths.state_home));
    rows.push(observed("config", config_status, config_detail));
    let cache_status = if paths.cache_dir.exists() {
        "present"
    } else {
        "empty"
    };
    rows.push(observed(
        "discovery cache",
        cache_status,
        paths.cache_dir.display().to_string(),
    ));
    let system = crate::catalog::search_system(&paths.cache_dir);
    rows.push(
        match crate::catalog::nixpkgs_summary(&paths.cache_dir, &config.sources.nixpkgs, &system) {
            Some(detail) => observed("search data", "ready", detail),
            None => observed(
                "search data",
                "not ready",
                String::from("Nixpkgs: run `pkg update` to prepare it"),
            ),
        },
    );
    rows.push(background_row(&paths)?);
    rows.push(shell_row(&paths));
    Ok(rows)
}

fn background_row(paths: &crate::config::Paths) -> Result<DoctorRow, CommandError> {
    Ok(match crate::background::state(paths) {
        Ok(crate::background::State::On) => observed(
            "background refresh",
            "on",
            String::from("pkg and its search data update daily"),
        ),
        Ok(crate::background::State::Off) => observed(
            "background refresh",
            "off",
            String::from(
                "pkg and its search data do not update. Turn it on: pkg background enable",
            ),
        ),
        Ok(crate::background::State::Unavailable) => observed(
            "background refresh",
            "off",
            crate::background::unavailable_detail(),
        ),
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => observed("background refresh", "off", error.to_string()),
    })
}

/// Rows for the Nix runtime and the checks that need it.
fn runtime_rows(
    cli: &Cli,
    config: &Config,
    paths: &config::Paths,
    rows: &mut Vec<DoctorRow>,
) -> Result<(), CommandError> {
    // Doctor shares the session's runtime setup: `--verbose` and
    // `--no-color` apply to its native probes like every other command,
    // while the observations stay read-only.
    match discover_runtime(cli, config).and_then(|runtime| {
        let version = runtime.version()?;
        Ok((runtime, version))
    }) {
        Ok((runtime, version)) => {
            rows.push(observed(
                "nix runtime",
                "ok",
                format!("{} ({version})", runtime.executable().display()),
            ));
            // Daemon reachability through the supported store command;
            // `nix store ping` is a deprecated alias and is not used.
            match runtime.store_info() {
                Ok(info) => rows.push(observed(
                    "nix daemon",
                    "ok",
                    info.lines().next().unwrap_or("reachable").to_string(),
                )),
                Err(error) if error.signal().is_some() => return Err(error.into()),
                Err(error) => rows.push(observed("nix daemon", "unreachable", error.to_string())),
            }
            rows.push(tap_sandbox_row(&runtime, cli.verbose)?);

            rows.push(crate::profile::status(&runtime, paths)?);
            // App launcher drift is read through the apps module; doctor
            // reports it and never fixes it (design D7). Auto-skipped on
            // non-macOS systems.
            if cfg!(target_os = "macos") {
                rows.push(app_launchers_row(&runtime, paths)?);
            } else {
                rows.push(observed(
                    "app launchers",
                    "skipped",
                    String::from("app launchers work on macOS only"),
                ));
            }
        }
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => {
            rows.push(observed("nix runtime", "missing", error.to_string()));
            rows.push(observed(
                "app launchers",
                "unknown",
                String::from("cannot be checked without the Nix runtime"),
            ));
        }
    }
    Ok(())
}

/// Taps need a one-time Nix sandbox setup, which `pkg tap add` offers.
/// Without it nothing is broken, so the row never needs attention.
fn tap_sandbox_row(runtime: &nix::Nix, verbose: bool) -> Result<DoctorRow, CommandError> {
    Ok(match crate::tap::setup::doctor_status(runtime)? {
        None => observed("tap sandbox", "ok", String::from("taps can be added")),
        Some(cause) => {
            let mut detail = String::from(
                "taps need a one-time Nix sandbox setup. `pkg tap add` offers it in a terminal.",
            );
            if verbose {
                detail.push_str(" Cause: ");
                detail.push_str(&cause);
            }
            observed("tap sandbox", "not set up", detail)
        }
    })
}

fn shell_row(paths: &crate::config::Paths) -> DoctorRow {
    let packages = paths.profile.join("bin");
    let client = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
    let missing: Vec<String> = client
        .into_iter()
        .chain([packages])
        .filter(|directory| !output::on_path(directory))
        .map(|directory| directory.display().to_string())
        .collect();
    if missing.is_empty() {
        observed(
            "shell",
            "ok",
            String::from("pkg and installed packages are on PATH"),
        )
    } else {
        observed(
            "shell",
            "not on path",
            format!(
                "{} {} not on PATH. Open a new terminal window. If that does not help, run the pkg installer again.",
                missing.join(" and "),
                if missing.len() == 1 { "is" } else { "are" }
            ),
        )
    }
}

fn app_launchers_row(
    runtime: &nix::Nix,
    paths: &crate::config::Paths,
) -> Result<DoctorRow, CommandError> {
    match crate::apps::status(runtime, paths) {
        Ok(status) => Ok(observed(
            "app launchers",
            status.health.as_str(),
            status.message,
        )),
        Err(error) if error.signal().is_some() => Err(error.into()),
        Err(error) => Ok(observed("app launchers", "unreadable", error.to_string())),
    }
}
