//! Command execution for the reduced grammar (design D4).
//!
//! Mutating commands delegate package realization to Nix, show native
//! progress, and print a short final result. Nothing here retries silently
//! or writes product state.

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::{CommandFactory as _, Parser as _};

use crate::catalog::{self, ParsedId};
use crate::cli::{Cli, Command, Shell};
use crate::config::{self, Config, Paths};
use crate::nix::{self, Nix, ProfileEntry};
use crate::output::{self, DoctorRow, ListRow};

/// Exit status for a runtime failure with diagnostics already printed.
pub const FAILURE: u8 = 1;

/// Exit status for a CLI usage error, matching the parser's own exits.
pub const USAGE: u8 = 2;

/// Exit status used when a native operation was interrupted by a signal.
///
/// The child was reaped and native state was re-read before exiting.
pub const INTERRUPTED: u8 = 130;

/// Parse arguments and run the requested command.
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    dispatch(&cli)
}

fn dispatch(cli: &Cli) -> ExitCode {
    // `--json` is a query option only; mutating commands must reject it
    // before touching the profile.
    if cli.json
        && !matches!(
            cli.command,
            Command::Search { .. } | Command::Info { .. } | Command::List | Command::Doctor
        )
    {
        eprintln!(
            "pkg: `--json` is a query option (search, info, list, doctor); \
             it is not accepted here"
        );
        return ExitCode::from(USAGE);
    }
    match &cli.command {
        Command::Search { query } => search(cli, query),
        Command::Info { id } => info(cli, id),
        Command::Install { ids } => install(cli, ids),
        Command::List => list(cli),
        Command::Remove { entries } => remove(cli, entries),
        Command::Update => update(cli),
        Command::Upgrade { entries, all } => upgrade(cli, entries, *all),
        Command::History => history(cli),
        Command::Rollback { generation } => rollback(cli, *generation),
        Command::Prune { older_than } => prune(cli, older_than),
        Command::Doctor => doctor(cli),
        Command::Shellenv => shellenv(),
        Command::Completion { shell } => completion(*shell),
        Command::Apps(crate::cli::AppsCommand::Sync) => apps_sync(cli),
    }
}

struct Session {
    paths: Paths,
    config: Config,
    nix: Nix,
}

fn session(cli: &Cli) -> Result<Session, String> {
    let paths = config::paths().map_err(|error| error.to_string())?;
    let config = Config::load(&paths.config_file).map_err(|error| error.to_string())?;
    let no_color = config.color_disabled(cli.no_color);
    let nix = nix::discover(config.runtime.nix.as_deref())
        .map_err(|error| error.to_string())?
        // `--verbose` traces every native call in the adapter; `--no-color`
        // is forwarded to native children as NO_COLOR.
        .verbose(cli.verbose)
        .no_color(no_color);
    Ok(Session { paths, config, nix })
}

fn fail(message: &str) -> ExitCode {
    eprintln!("pkg: {message}");
    ExitCode::from(FAILURE)
}

/// Report native state after a failed or interrupted mutation.
///
/// The child was reaped inside the adapter; pkg stays alive to re-read the
/// authoritative profile and report it instead of exiting silently. This
/// runs after every failed mutation, not only cancellations, because a
/// failed native operation may also have partially applied. The state is
/// stated, not guessed.
fn report_failed_mutation(
    session: &Session,
    action: &str,
    error: &nix::NixError,
    interrupted: bool,
) -> ExitCode {
    if interrupted {
        eprintln!("pkg: {action} was interrupted; the native profile stays authoritative");
    } else {
        eprintln!("pkg: {action} failed: {error}");
    }
    match installed_entries(session) {
        Ok(entries) if entries.is_empty() => {
            eprintln!("pkg: profile re-read: no installed entries");
        }
        Ok(entries) => {
            eprintln!("pkg: profile re-read: {} installed entries", entries.len());
            for (entry_id, entry) in &entries {
                eprintln!("pkg:   {entry_id} ({})", entry.name());
            }
        }
        Err(error) => eprintln!("pkg: could not re-read the profile: {error}"),
    }
    eprintln!(
        "pkg: the operation may have partially applied; run `pkg list` or `pkg history` for the native state"
    );
    ExitCode::from(if interrupted { INTERRUPTED } else { FAILURE })
}

/// Classify a mutation error and report re-read native state for it.
fn mutation_failed(session: &Session, action: &str, error: &nix::NixError) -> ExitCode {
    let interrupted = matches!(error, nix::NixError::Interrupted { .. });
    report_failed_mutation(session, action, error, interrupted)
}

fn session_or_fail(cli: &Cli) -> Option<Session> {
    session(cli)
        .map_err(|error| {
            let _ = fail(&error);
        })
        .ok()
}

fn search(cli: &Cli, query: &str) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    let system = match session.nix.system() {
        Ok(system) => system,
        Err(error) => return fail(&error.to_string()),
    };
    let mut reports = Vec::new();
    let mut rows = Vec::new();
    for (kind, moving) in [
        (
            catalog::SourceKind::Nixpkgs,
            &session.config.sources.nixpkgs,
        ),
        (catalog::SourceKind::Cask, &session.config.sources.casks),
    ] {
        // The cask source is macOS-only; on other systems it is skipped with
        // an honest status instead of a fatal requirement.
        if kind == catalog::SourceKind::Cask && !catalog::casks_supported_on(&system) {
            reports.push(catalog::SourceReport {
                source: moving.clone(),
                display: None,
                locked_reference: None,
                revision: None,
                status: catalog::SourceStatus::SkippedPlatform,
                detail: Some(format!(
                    "cask source supports {} only",
                    catalog::CASK_SYSTEM
                )),
                support_detail: None,
            });
            continue;
        }
        let (source_rows, report) = catalog::search_source(
            &session.nix,
            &session.paths.cache_dir,
            moving,
            query,
            &system,
            kind,
        );
        reports.push(report);
        rows.extend(source_rows);
    }
    let failed: Vec<&catalog::SourceReport> = reports
        .iter()
        .filter(|report| report.status == catalog::SourceStatus::Failed)
        .collect();
    if cli.json {
        let result = serde_json::json!({
            "system": system,
            "sources": reports,
            "results": rows,
        });
        if print_json(&output::envelope("search", &result)).is_err() {
            return ExitCode::from(FAILURE);
        }
    } else {
        print!("{}", output::render_source_reports(&reports));
        print!("{}", output::render_search(&rows));
    }
    if !failed.is_empty() {
        for report in &failed {
            eprintln!(
                "pkg: source failure for {}: {}",
                report.source,
                report.detail.as_deref().unwrap_or("unknown")
            );
        }
        if rows.is_empty() {
            return ExitCode::from(FAILURE);
        }
        eprintln!("pkg: results above may be reused stale data and are labeled as such");
    }
    ExitCode::SUCCESS
}

/// Collected data for one info result.
#[derive(Debug)]
struct InfoData {
    /// The requested attribute or token.
    attribute: String,
    /// The actual full matched native attribute, when a derivation lookup
    /// ran.
    matched_attribute: Option<String>,
    /// Evaluated package metadata, when a derivation lookup ran.
    meta: Option<nix::SearchMeta>,
    /// Source provenance for the result.
    report: catalog::SourceReport,
    /// Cask classification, when the ID is a cask token.
    cask: Option<CaskDisplay>,
}

fn info(cli: &Cli, id: &str) -> ExitCode {
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let parsed = match catalog::parse_id(id) {
        Ok(parsed) => parsed,
        Err(detail) => return fail(&detail),
    };
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    let system = match session.nix.system() {
        Ok(system) => system,
        Err(error) => return fail(&error.to_string()),
    };
    let resolved = match parsed {
        ParsedId::Qualified(qualified) => qualified,
        ParsedId::Bare(name) => {
            match catalog::resolve_bare(&session.nix, &session.config.sources, &name, &system) {
                Ok(resolved) => resolved,
                Err(error) => return fail(&error.to_string()),
            }
        }
    };
    let installable = resolved.installable(&session.config.sources);
    let (reference, attribute) = match &resolved {
        catalog::CatalogId::Nixpkgs(attr) => (session.config.sources.nixpkgs.clone(), attr.clone()),
        catalog::CatalogId::Cask(token) => (session.config.sources.casks.clone(), token.clone()),
        catalog::CatalogId::Explicit {
            reference,
            attribute,
        } => (reference.clone(), attribute.clone()),
    };
    // Cask platform and support are checked before any derivation lookup, so
    // excluded and unlisted tokens report their recorded reason instead of
    // exiting on a nonexistent derivation.
    let data = if let catalog::CatalogId::Cask(token) = &resolved {
        match cask_info(&session, &system, &reference, token) {
            Ok(data) => data,
            Err(detail) => return fail(&detail),
        }
    } else {
        match catalog::exact_lookup(&session.nix, &reference, &attribute) {
            Ok((exact, report)) => InfoData {
                attribute,
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: None,
            },
            Err(catalog::CatalogError::NotFound(_)) => {
                return fail(&format!(
                    "`{}` has no exact match in `{reference}`; \
                     pkg does not report package identities it cannot evaluate",
                    resolved.qualified()
                ));
            }
            Err(error) => return fail(&error.to_string()),
        }
    };
    // Installed matching compares the actual full matched attribute and the
    // canonical source with its flake subdirectory, never a short request
    // alone.
    let identity_attribute = data
        .matched_attribute
        .clone()
        .unwrap_or_else(|| data.attribute.clone());
    let installed = installed_entries(&session).ok().and_then(|entries| {
        entries.into_iter().find(|(_, entry)| {
            entry.attr_path == identity_attribute
                && catalog::canonical_source(&entry.original_url)
                    == catalog::canonical_source(&reference)
        })
    });
    // The installed record carries the actual original and locked source
    // separately.
    let installed_json = installed.as_ref().map(|(entry_id, entry)| {
        serde_json::json!({
            "entry_id": entry_id,
            "original_url": entry.original_url,
            "locked_url": entry.locked_url,
        })
    });
    let cask_version = match &data.cask {
        Some(CaskDisplay::Status(catalog::CaskStatus::Supported { version, .. })) => {
            version.clone()
        }
        Some(CaskDisplay::Status(catalog::CaskStatus::Excluded { version, .. })) => version.clone(),
        _ => None,
    };
    let name = data
        .meta
        .as_ref()
        .map(|meta| meta.pname.clone())
        .unwrap_or_else(|| data.attribute.clone());
    let version = data
        .meta
        .as_ref()
        .map(|meta| meta.version.clone())
        .or(cask_version);
    let description = data
        .meta
        .as_ref()
        .map(|meta| meta.description.clone())
        .unwrap_or_default();
    let cask_json = data.cask.as_ref().map(|display| match display {
        CaskDisplay::UnsupportedPlatform { system } => serde_json::json!({
            "status": "unsupported-platform",
            "system": system,
            "required": catalog::CASK_SYSTEM,
        }),
        CaskDisplay::Unavailable(detail) => serde_json::json!({
            "status": "unavailable",
            "detail": detail,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Supported {
            kind,
            version,
            homepage,
            artifact_kinds,
            override_applied,
            source_revision,
        }) => serde_json::json!({
            "status": "supported",
            "kind": kind,
            "version": version,
            "homepage": homepage,
            "artifactKinds": artifact_kinds,
            "override": override_applied,
            "sourceRevision": source_revision,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Excluded {
            version,
            reason,
            detail,
            source_revision,
        }) => serde_json::json!({
            "status": "excluded",
            "version": version,
            "reason": reason,
            "detail": detail,
            "sourceRevision": source_revision,
        }),
        CaskDisplay::Status(catalog::CaskStatus::Unlisted { .. }) => serde_json::json!({
            "status": "unlisted",
        }),
    });
    if cli.json {
        let result = serde_json::json!({
            "id": resolved.qualified(),
            "source": data.report.display.clone().unwrap_or_else(|| reference.clone()),
            "reference": reference,
            "locked_reference": data.report.locked_reference,
            "revision": data.report.revision,
            "system": system,
            "attribute": data.attribute,
            "matched_attribute": data.matched_attribute,
            "name": name,
            "version": version,
            "description": description,
            "installed": installed_json,
            "cask": cask_json,
        });
        if print_json(&output::envelope("info", &result)).is_err() {
            return ExitCode::from(FAILURE);
        }
    } else {
        println!(
            "{}  {}  {}",
            resolved.qualified(),
            name,
            version.as_deref().unwrap_or("unknown version")
        );
        println!(
            "source: {} ({})",
            data.report.display.as_deref().unwrap_or(&reference),
            reference
        );
        if let Some(revision) = &data.report.revision {
            println!("revision: {revision}");
        }
        println!(
            "attribute: {}",
            data.matched_attribute.as_deref().unwrap_or(&data.attribute)
        );
        if !description.is_empty() {
            println!("{description}");
        }
        if let Some(display) = &data.cask {
            print!("{}", render_cask_display(display));
        }
        match &installed {
            Some((entry_id, _)) => println!("installed as entry {entry_id}"),
            None => println!("not installed"),
        }
        println!("installable: {installable}");
    }
    ExitCode::SUCCESS
}

/// Build info data for one cask token.
///
/// Platform and the support manifest are consulted before any derivation
/// lookup: excluded and unlisted tokens return their recorded metadata from
/// the same locked source without searching for a derivation that does not
/// exist.
fn cask_info(
    session: &Session,
    system: &str,
    reference: &str,
    token: &str,
) -> Result<InfoData, String> {
    if !catalog::casks_supported_on(system) {
        return Ok(InfoData {
            attribute: token.to_string(),
            matched_attribute: None,
            meta: None,
            report: catalog::SourceReport {
                source: reference.to_string(),
                display: None,
                locked_reference: None,
                revision: None,
                status: catalog::SourceStatus::SkippedPlatform,
                detail: Some(format!(
                    "cask source supports {} only",
                    catalog::CASK_SYSTEM
                )),
                support_detail: None,
            },
            cask: Some(CaskDisplay::UnsupportedPlatform {
                system: system.to_string(),
            }),
        });
    }
    let lookup = match catalog::cask_status(&session.nix, reference, system, token) {
        Ok(lookup) => lookup,
        Err(error) => {
            // The manifest is unavailable. A derivation lookup may still
            // work; the unavailability stays visible in the result.
            let detail = error.to_string();
            let (exact, report) =
                catalog::exact_lookup(&session.nix, reference, token).map_err(|lookup_error| {
                    format!(
                        "cask support manifest unavailable ({detail}); \
                             and no exact derivation match either: {lookup_error}"
                    )
                })?;
            return Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: Some(CaskDisplay::Unavailable(detail)),
            });
        }
    };
    match lookup.status {
        catalog::CaskStatus::Supported { .. } => {
            // Metadata comes from the same locked source as the support
            // decision.
            let (exact, report) =
                catalog::exact_lookup_in(&session.nix, reference, &lookup.identity, token)
                    .map_err(|error| error.to_string())?;
            Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: Some(CaskDisplay::Status(lookup.status)),
            })
        }
        status @ (catalog::CaskStatus::Excluded { .. } | catalog::CaskStatus::Unlisted { .. }) => {
            Ok(InfoData {
                attribute: token.to_string(),
                matched_attribute: None,
                meta: None,
                report: catalog::report_for(reference, &lookup.identity),
                cask: Some(CaskDisplay::Status(status)),
            })
        }
    }
}

/// Cask support display state for one token.
#[derive(Debug)]
enum CaskDisplay {
    /// The system cannot run casks at all.
    UnsupportedPlatform {
        /// The system pkg is running on.
        system: String,
    },
    /// The support manifest could not be evaluated.
    Unavailable(String),
    /// The manifest classify result.
    Status(catalog::CaskStatus),
}

fn render_cask_display(display: &CaskDisplay) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    match display {
        CaskDisplay::UnsupportedPlatform { system } => {
            let _ = writeln!(
                out,
                "cask support: unsupported platform ({system}; casks need {})",
                catalog::CASK_SYSTEM
            );
        }
        CaskDisplay::Unavailable(detail) => {
            let _ = writeln!(out, "cask support: unavailable ({detail})");
        }
        CaskDisplay::Status(catalog::CaskStatus::Supported {
            kind,
            source_revision,
            ..
        }) => {
            let _ = writeln!(out, "cask support: supported ({kind})");
            if let Some(revision) = source_revision {
                let _ = writeln!(out, "cask source revision: {revision}");
            }
        }
        CaskDisplay::Status(catalog::CaskStatus::Excluded {
            version,
            reason,
            detail,
            ..
        }) => {
            let version = version.as_deref().unwrap_or("unknown version");
            let detail = detail.as_deref().unwrap_or("no detail");
            let _ = writeln!(
                out,
                "cask support: excluded at {version} ({reason}: {detail})"
            );
        }
        CaskDisplay::Status(catalog::CaskStatus::Unlisted { .. }) => {
            let _ = writeln!(out, "cask support: not listed for this system");
        }
    }
    out
}

fn install(cli: &Cli, ids: &[String]) -> ExitCode {
    if ids.is_empty() {
        return fail("install needs at least one ID");
    }
    // IDs are validated before the runtime is touched, so malformed input is
    // refused identically with or without Nix present.
    let mut parsed_ids = Vec::new();
    for id in ids {
        match catalog::parse_id(id) {
            Ok(parsed) => parsed_ids.push(parsed),
            Err(detail) => return fail(&detail),
        }
    }
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    let system = match session.nix.system() {
        Ok(system) => system,
        Err(error) => return fail(&error.to_string()),
    };
    let mut installables = Vec::new();
    for parsed in parsed_ids {
        let resolved = match parsed {
            ParsedId::Qualified(qualified) => qualified,
            ParsedId::Bare(name) => {
                match catalog::resolve_bare(&session.nix, &session.config.sources, &name, &system) {
                    Ok(resolved) => resolved,
                    Err(error) => return fail(&error.to_string()),
                }
            }
        };
        if let catalog::CatalogId::Cask(token) = &resolved
            && let Err(detail) = gate_cask(&session, &system, token)
        {
            return fail(&detail);
        }
        installables.push(resolved.installable(&session.config.sources));
    }
    if let Err(error) = session
        .nix
        .profile_add(&session.paths.profile, &installables)
    {
        return mutation_failed(&session, "install", &error);
    }
    match installed_entries(&session) {
        Ok(entries) => {
            report_entries(&entries, installables.len());
            apps_refresh_after_mutation(&session)
        }
        Err(error) => fail(&error.to_string()),
    }
}

/// Refresh derived macOS launchers after a successful profile mutation.
///
/// The package change has already completed in the native profile. When the
/// launcher sync fails, the failure is reported as partial completion with a
/// nonzero exit and `pkg apps sync` named as the retry (design D4). On other
/// systems app exposure does not apply and this is a no-op.
fn apps_refresh_after_mutation(session: &Session) -> ExitCode {
    if !cfg!(target_os = "macos") {
        return ExitCode::SUCCESS;
    }
    match crate::apps::sync(&session.nix, &session.paths) {
        Ok(()) => ExitCode::SUCCESS,
        Err(detail) => {
            eprintln!(
                "pkg: the package change completed, but refreshing app launchers failed: {detail}"
            );
            eprintln!("pkg: retry the derived launchers with `pkg apps sync`");
            ExitCode::from(FAILURE)
        }
    }
}

/// Read installed entries, treating a not-yet-created profile as empty.
///
/// A fresh install has no profile on disk; that state is empty, not an error.
fn installed_entries(session: &Session) -> Result<BTreeMap<String, ProfileEntry>, nix::NixError> {
    if !session.paths.profile.exists() {
        return Ok(BTreeMap::new());
    }
    session.nix.profile_list(&session.paths.profile)
}

/// Refuse cask installs that the platform or the support manifest excludes.
///
/// Returns `Err(message)` when the token must not be installed on `system`.
fn gate_cask(session: &Session, system: &str, token: &str) -> Result<(), String> {
    if !catalog::casks_supported_on(system) {
        return Err(format!(
            "cask:{token} requires macOS ({}); this system is {system}",
            catalog::CASK_SYSTEM
        ));
    }
    match catalog::cask_status(&session.nix, &session.config.sources.casks, system, token) {
        Ok(lookup) => match lookup.status {
            catalog::CaskStatus::Supported { .. } => Ok(()),
            catalog::CaskStatus::Excluded {
                version,
                reason,
                detail,
                ..
            } => Err(format!(
                "cask:{token} {} is excluded on {system}: {reason}: {}",
                version.as_deref().unwrap_or("unknown version"),
                detail.as_deref().unwrap_or("no detail")
            )),
            catalog::CaskStatus::Unlisted { .. } => Err(format!(
                "cask:{token} is not in the cask support manifest for {system}"
            )),
        },
        Err(error) => Err(error.to_string()),
    }
}

fn report_entries(entries: &BTreeMap<String, ProfileEntry>, expected: usize) {
    println!(
        "Installed {expected} entr{} in the pkg profile:",
        if expected == 1 { "y" } else { "ies" }
    );
    for (entry_id, entry) in entries {
        let revision = entry.locked_revision().unwrap_or("no revision");
        println!("  {entry_id}  {}  ({revision})", entry.name());
    }
}

fn list_rows(entries: &BTreeMap<String, ProfileEntry>) -> Vec<ListRow> {
    // Discovery and display scans only consider active entries.
    entries
        .iter()
        .filter(|(_, entry)| entry.active)
        .map(|(entry_id, entry)| ListRow {
            entry_id: entry_id.clone(),
            name: entry.name().to_string(),
            source: entry.original_url.clone(),
            locked_source: entry.locked_url.clone(),
            revision: entry.locked_revision().map(ToString::to_string),
        })
        .collect()
}

fn list(cli: &Cli) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    // A fresh install has no profile yet; that state is empty, not an error.
    match installed_entries(&session) {
        Ok(entries) => {
            let rows = list_rows(&entries);
            if cli.json {
                if print_json(&output::envelope("list", &rows)).is_err() {
                    return ExitCode::from(FAILURE);
                }
            } else {
                print!("{}", output::render_list(&rows));
            }
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn remove(cli: &Cli, entries: &[String]) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    if entries.is_empty() {
        return fail("remove needs at least one installed entry ID");
    }
    let installed = match installed_entries(&session) {
        Ok(installed) => installed,
        Err(error) => return fail(&error.to_string()),
    };
    for entry in entries {
        if !installed.contains_key(entry) {
            return fail(&format!(
                "`{entry}` is not an installed entry; run `pkg list` for exact entry IDs"
            ));
        }
    }
    if let Err(error) = session.nix.profile_remove(&session.paths.profile, entries) {
        return mutation_failed(&session, "remove", &error);
    }
    println!("Removed {} entries from the pkg profile.", entries.len());
    apps_refresh_after_mutation(&session)
}

fn update(cli: &Cli) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    let system = match session.nix.system() {
        Ok(system) => system,
        Err(error) => return fail(&error.to_string()),
    };
    // Refresh metadata natively (--refresh) so TTL-cached metadata is not
    // reported as current, then drop the discovery cache.
    for (kind, source) in [
        (
            catalog::SourceKind::Nixpkgs,
            &session.config.sources.nixpkgs,
        ),
        (catalog::SourceKind::Cask, &session.config.sources.casks),
    ] {
        // The cask source is macOS-only; on other systems it is skipped,
        // not fetched or failed.
        if kind == catalog::SourceKind::Cask && !catalog::casks_supported_on(&system) {
            println!(
                "{source} -> skipped (cask source supports {} only)",
                catalog::CASK_SYSTEM
            );
            continue;
        }
        match session.nix.refresh_source_identity(source) {
            Ok(identity) => {
                let revision = identity
                    .revision
                    .as_deref()
                    .unwrap_or("moving, no revision");
                println!("{source} -> {revision} (metadata refreshed)");
            }
            Err(error) => {
                eprintln!("pkg: source failure for {source}: {error}");
                return ExitCode::from(FAILURE);
            }
        }
    }
    match catalog::invalidate_cache(&session.paths.cache_dir) {
        Ok(removed) => {
            println!(
                "Discovery cache refreshed ({removed} cached result set{} dropped).",
                if removed == 1 { "" } else { "s" }
            );
            println!("Installed packages and their original references are unchanged.");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn upgrade(cli: &Cli, entries: &[String], all: bool) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    let installed = match installed_entries(&session) {
        Ok(installed) => installed,
        Err(error) => return fail(&error.to_string()),
    };
    // `--all` combined with entry names is a usage error rejected by the
    // parser; here the target set is either explicit names or everything.
    let targets: Vec<String> = if all {
        installed.keys().cloned().collect()
    } else {
        if entries.is_empty() {
            return fail("upgrade needs entry IDs or --all");
        }
        for entry in entries {
            if !installed.contains_key(entry) {
                return fail(&format!(
                    "`{entry}` is not an installed entry; run `pkg list`"
                ));
            }
        }
        entries.to_vec()
    };
    if all && targets.is_empty() {
        println!("No installed entries; nothing to upgrade.");
        return ExitCode::SUCCESS;
    }
    // Only explicit full-commit references count as fixed; tags, branches,
    // and hex-looking branch names stay movable (design D4). Fixed entries
    // are reported explicitly in both modes.
    let mut movable: Vec<String> = Vec::new();
    for entry in &targets {
        if nix::is_fixed_reference(installed[entry].original_url.as_str()) {
            println!(
                "pkg: {entry} uses an explicit full-commit reference; upgrade does not replace it. \
                 Remove and install the new reference to change it."
            );
        } else {
            movable.push(entry.clone());
        }
    }
    if !all && movable.is_empty() {
        return fail("nothing to upgrade");
    }
    // With --all, eligibility stays with Nix: fixed references re-lock to the
    // same commit and path sources re-evaluate natively.
    let target = if all { None } else { Some(movable.as_slice()) };
    if let Err(error) = session.nix.profile_upgrade(&session.paths.profile, target) {
        return mutation_failed(&session, "upgrade", &error);
    }
    match installed_entries(&session) {
        Ok(entries) => {
            report_entries(&entries, entries.len());
            apps_refresh_after_mutation(&session)
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn history(cli: &Cli) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    match session.nix.profile_history(&session.paths.profile) {
        Ok(text) => {
            // `--no-color` holds for native text pkg passes through, even if
            // the runtime colorized it anyway.
            let text = if session.config.color_disabled(cli.no_color) {
                output::strip_ansi(&text)
            } else {
                text
            };
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn rollback(cli: &Cli, generation: Option<u64>) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    if let Err(error) = session
        .nix
        .profile_rollback(&session.paths.profile, generation)
    {
        return mutation_failed(&session, "rollback", &error);
    }
    match generation {
        Some(number) => println!("Rolled back to native generation {number}."),
        None => println!("Rolled back to the previous native generation."),
    }
    apps_refresh_after_mutation(&session)
}

fn prune(cli: &Cli, older_than: &str) -> ExitCode {
    let days = match crate::cli::parse_prune_age(older_than) {
        Ok(days) => days,
        Err(error) => return fail(&error),
    };
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    if let Err(error) = session
        .nix
        .profile_wipe_history(&session.paths.profile, &format!("{days}d"))
    {
        return mutation_failed(&session, "prune", &error);
    }
    // Eligibility stays with Nix: which generations are old enough is native
    // behavior and is not assumed by pkg.
    println!("Requested deletion of non-current generations older than {days} days.");
    println!("Nix decides which generations are eligible; run `pkg history` to verify.");
    ExitCode::SUCCESS
}

/// Doctor statuses that make the overall result unhealthy.
///
/// A missing runtime, an unreachable daemon, an unreadable profile, and
/// unhealthy app launchers all fail doctor with a nonzero exit; a fresh
/// profile or a skipped platform component does not.
const UNHEALTHY_STATUSES: [&str; 4] = ["missing", "unreachable", "unreadable", "unhealthy"];

fn doctor(cli: &Cli) -> ExitCode {
    let rows = match doctor_rows() {
        Ok(rows) => rows,
        Err(error) => return fail(&error),
    };
    let healthy = !rows
        .iter()
        .any(|row| UNHEALTHY_STATUSES.contains(&row.status.as_str()));
    if cli.json {
        if print_json(&output::envelope("doctor", &rows)).is_err() {
            return ExitCode::from(FAILURE);
        }
    } else {
        print!("{}", output::render_doctor(&rows));
    }
    if healthy {
        ExitCode::SUCCESS
    } else {
        // Unhealthy required components fail doctor instead of passing
        // silently; doctor itself never repairs anything.
        ExitCode::from(FAILURE)
    }
}

fn doctor_rows() -> Result<Vec<DoctorRow>, String> {
    let paths = config::paths().map_err(|error| error.to_string())?;
    let config = Config::load(&paths.config_file).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    match nix::discover(config.runtime.nix.as_deref()) {
        Ok(runtime) => {
            rows.push(DoctorRow {
                component: String::from("nix runtime"),
                status: String::from("ok"),
                detail: Some(format!(
                    "{} (version {}; tested baseline {})",
                    runtime.executable().display(),
                    runtime.version(),
                    nix::TESTED_BASELINE
                )),
            });
            // Daemon reachability through the supported store command;
            // `nix store ping` is a deprecated alias and is not used.
            match runtime.store_info() {
                Ok(info) => rows.push(DoctorRow {
                    component: String::from("nix daemon"),
                    status: String::from("ok"),
                    detail: Some(info.lines().next().unwrap_or("reachable").to_string()),
                }),
                Err(error) => rows.push(DoctorRow {
                    component: String::from("nix daemon"),
                    status: String::from("unreachable"),
                    detail: Some(error.to_string()),
                }),
            }
            if paths.profile.exists() {
                match runtime.profile_list(&paths.profile) {
                    Ok(entries) => rows.push(DoctorRow {
                        component: String::from("pkg profile"),
                        status: String::from("ok"),
                        detail: Some(format!(
                            "{} ({} entries)",
                            paths.profile.display(),
                            entries.len()
                        )),
                    }),
                    Err(error) => rows.push(DoctorRow {
                        component: String::from("pkg profile"),
                        status: String::from("unreadable"),
                        detail: Some(error.to_string()),
                    }),
                }
            } else {
                rows.push(DoctorRow {
                    component: String::from("pkg profile"),
                    status: String::from("fresh"),
                    detail: Some(format!(
                        "{} not created yet; the first install creates it",
                        paths.profile.display()
                    )),
                });
            }
            // App launcher drift is read through the apps module; doctor
            // reports it and never fixes it (design D7). Auto-skipped on
            // non-macOS systems.
            if cfg!(target_os = "macos") {
                rows.push(app_launchers_row(&runtime, &paths));
            } else {
                rows.push(DoctorRow {
                    component: String::from("app launchers"),
                    status: String::from("skipped"),
                    detail: Some(String::from("app launchers are exposed on macOS only")),
                });
            }
        }
        Err(error) => {
            rows.push(DoctorRow {
                component: String::from("nix runtime"),
                status: String::from("missing"),
                detail: Some(error.to_string()),
            });
            rows.push(DoctorRow {
                component: String::from("app launchers"),
                status: String::from("unknown"),
                detail: Some(String::from("cannot be checked without the Nix runtime")),
            });
        }
    }
    rows.push(DoctorRow {
        component: String::from("config"),
        status: if paths.config_file.exists() {
            String::from("present")
        } else {
            String::from("defaults")
        },
        detail: Some(paths.config_file.display().to_string()),
    });
    rows.push(DoctorRow {
        component: String::from("discovery cache"),
        status: if paths.cache_dir.exists() {
            String::from("present")
        } else {
            String::from("empty")
        },
        detail: Some(paths.cache_dir.display().to_string()),
    });
    Ok(rows)
}

/// The doctor row for macOS app launchers, mapped from the read-only apps
/// status (`healthy:`/`unhealthy:` prefix, or an operational error).
fn app_launchers_row(runtime: &nix::Nix, paths: &crate::config::Paths) -> DoctorRow {
    match crate::apps::status(runtime, paths) {
        Ok(text) if text.starts_with("healthy:") => DoctorRow {
            component: String::from("app launchers"),
            status: String::from("healthy"),
            detail: Some(text),
        },
        Ok(text) if text.starts_with("unhealthy:") => DoctorRow {
            component: String::from("app launchers"),
            status: String::from("unhealthy"),
            detail: Some(text),
        },
        Ok(text) => DoctorRow {
            component: String::from("app launchers"),
            status: String::from("unreadable"),
            detail: Some(text),
        },
        Err(error) => DoctorRow {
            component: String::from("app launchers"),
            status: String::from("unreadable"),
            detail: Some(error),
        },
    }
}

/// Quote `value` as one single-quoted shell word (Bash and Zsh). Single
/// quotes keep spaces, double quotes, `$`, and backticks literal; an
/// embedded single quote is closed, escaped, and reopened.
fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shellenv() -> ExitCode {
    let paths = match config::paths() {
        Ok(paths) => paths,
        Err(error) => return fail(&error.to_string()),
    };
    let bin = shell_word(&paths.profile.join("bin").display().to_string());
    println!("# pkg shellenv (Bash and Zsh; safe to repeat)");
    println!("case \":$PATH:\" in");
    println!("  *:{bin}:*) ;;");
    println!("  *) export PATH={bin}\"${{PATH:+:$PATH}}\" ;;");
    println!("esac");
    ExitCode::SUCCESS
}

fn completion(shell: Shell) -> ExitCode {
    let mut command = Cli::command();
    let name = String::from("pkg");
    let generated = match shell {
        Shell::Bash => {
            let mut buf = Vec::new();
            clap_complete::generate(clap_complete::Shell::Bash, &mut command, &name, &mut buf);
            buf
        }
        Shell::Zsh => {
            let mut buf = Vec::new();
            clap_complete::generate(clap_complete::Shell::Zsh, &mut command, &name, &mut buf);
            buf
        }
        Shell::Fish => {
            let mut buf = Vec::new();
            clap_complete::generate(clap_complete::Shell::Fish, &mut command, &name, &mut buf);
            buf
        }
    };
    use std::io::Write as _;
    match std::io::stdout().write_all(&generated) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error.to_string()),
    }
}

fn apps_sync(cli: &Cli) -> ExitCode {
    let Some(session) = session_or_fail(cli) else {
        return ExitCode::from(FAILURE);
    };
    match crate::apps::sync(&session.nix, &session.paths) {
        Ok(()) => {
            println!("App launchers refreshed from the active profile.");
            ExitCode::SUCCESS
        }
        Err(detail) => fail(&detail),
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("cannot encode JSON output: {error}"))?;
    println!("{text}");
    Ok(())
}
