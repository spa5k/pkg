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
    let config = Config::load(&paths.config_file)?;
    let mut rows = Vec::new();
    // Doctor shares the session's runtime setup: `--verbose` and
    // `--no-color` apply to its native probes like every other command,
    // while the observations stay read-only.
    match discover_runtime(cli, &config).and_then(|runtime| {
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
            let (sandbox_status, sandbox_detail) = crate::tap::setup::doctor_status(&runtime)?;
            rows.push(observed("tap sandbox", &sandbox_status, sandbox_detail));

            rows.push(crate::profile::status(&runtime, &paths)?);
            // App launcher drift is read through the apps module; doctor
            // reports it and never fixes it (design D7). Auto-skipped on
            // non-macOS systems.
            if cfg!(target_os = "macos") {
                rows.push(app_launchers_row(&runtime, &paths)?);
            } else {
                rows.push(observed(
                    "app launchers",
                    "skipped",
                    String::from("app launchers are exposed on macOS only"),
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
    let config_status = if paths.config_file.exists() {
        "present"
    } else {
        "defaults"
    };
    // Use the query loader so doctor and installation agree on tap health.
    rows.extend(crate::tap::health(&paths.state_home));
    rows.push(observed(
        "config",
        config_status,
        paths.config_file.display().to_string(),
    ));
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
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => observed("background refresh", "off", error.to_string()),
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
