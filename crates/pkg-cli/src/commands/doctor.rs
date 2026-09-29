//! Doctor: read-only runtime, profile, and launcher status.
//!
//! Doctor reports and never repairs (design D7 review rule). Unhealthy
//! required components make the command fail with a nonzero exit; a fresh
//! profile or a skipped platform component does not.

use std::process::ExitCode;

use crate::cli::Cli;
use crate::config::{self, Config};
use crate::nix;
use crate::output::{self, DoctorRow};

use super::{CommandError, FAILURE, discover_runtime, print_json};

/// Doctor statuses that make the overall result unhealthy.
///
/// A missing runtime, an unreachable daemon, an unreadable profile, and
/// unhealthy app launchers all fail doctor with a nonzero exit; a fresh
/// profile or a skipped platform component does not.
const UNHEALTHY_STATUSES: [&str; 4] = ["missing", "unreachable", "unreadable", "unhealthy"];

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
    match discover_runtime(cli, &config) {
        Ok(runtime) => {
            rows.push(observed(
                "nix runtime",
                "ok",
                format!("{} ({})", runtime.executable().display(), runtime.version()),
            ));
            // Daemon reachability through the supported store command;
            // `nix store ping` is a deprecated alias and is not used.
            match runtime.store_info() {
                Ok(info) => rows.push(observed(
                    "nix daemon",
                    "ok",
                    info.lines().next().unwrap_or("reachable").to_string(),
                )),
                Err(error) => rows.push(observed("nix daemon", "unreachable", error.to_string())),
            }
            let (sandbox_status, sandbox_detail) = crate::tap::setup::doctor_status(&runtime);
            rows.push(observed("tap sandbox", &sandbox_status, sandbox_detail));

            if paths.profile.exists() {
                match runtime.profile_list(&paths.profile) {
                    Ok(entries) => rows.push(observed(
                        "pkg profile",
                        "ok",
                        format!("{} ({} entries)", paths.profile.display(), entries.len()),
                    )),
                    Err(error) => {
                        rows.push(observed("pkg profile", "unreadable", error.to_string()));
                    }
                }
            } else {
                rows.push(observed(
                    "pkg profile",
                    "fresh",
                    format!(
                        "{} not created yet; the first install creates it",
                        paths.profile.display()
                    ),
                ));
            }
            // App launcher drift is read through the apps module; doctor
            // reports it and never fixes it (design D7). Auto-skipped on
            // non-macOS systems.
            if cfg!(target_os = "macos") {
                rows.push(app_launchers_row(&runtime, &paths));
            } else {
                rows.push(observed(
                    "app launchers",
                    "skipped",
                    String::from("app launchers are exposed on macOS only"),
                ));
            }
        }
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
    Ok(rows)
}

/// The doctor row for macOS app launchers, mapped from the read-only apps
/// status (`healthy:`/`unhealthy:` prefix, or an operational error).
fn app_launchers_row(runtime: &nix::Nix, paths: &crate::config::Paths) -> DoctorRow {
    match crate::apps::status(runtime, paths) {
        Ok(text) if text.starts_with("healthy:") => observed("app launchers", "healthy", text),
        Ok(text) if text.starts_with("unhealthy:") => observed("app launchers", "unhealthy", text),
        Ok(text) => observed("app launchers", "unreadable", text),
        Err(error) => observed("app launchers", "unreadable", error),
    }
}
