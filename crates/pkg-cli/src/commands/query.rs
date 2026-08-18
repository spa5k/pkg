//! Offline search and package-info handlers over one verified derived index.

use serde_json::{Map, Value, json};

use pkg_core::{AttributePath, ChannelSequence, NixpkgsRevision, PackageVersion};
use pkg_index::{IndexDocument, IndexQuery, InfoLookup, SearchOptions};
use pkg_nix::{CatalogInfoLookup, CatalogInfoReport, CatalogPackageSummary, CatalogSearchReport};

use crate::cli::{InfoArgs, SearchArgs};
use crate::commands::execute::CommandResult;
use crate::exit::ExitCode;
use crate::ux::CommandError;

/// One installed package identity used only for a read-only catalog comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCatalogPackage {
    package: AttributePath,
    name: String,
    version: PackageVersion,
    revision: NixpkgsRevision,
    pinned: bool,
}

impl InstalledCatalogPackage {
    /// Retains the product fields from one already-verified active generation.
    #[must_use]
    pub fn new(
        package: AttributePath,
        name: String,
        version: PackageVersion,
        revision: NixpkgsRevision,
        pinned: bool,
    ) -> Self {
        Self {
            package,
            name,
            version,
            revision,
            pinned,
        }
    }

    /// Returns the canonical package id used for the catalog lookup.
    #[must_use]
    pub fn package(&self) -> &str {
        self.package.as_str()
    }
}

/// Executes bounded offline search without treating the index as realization authority.
pub fn search_index(
    document: &IndexDocument,
    stale: bool,
    args: &SearchArgs,
) -> Result<CommandResult, CommandError> {
    if args.channel().is_some() {
        return Err(CommandError::new(
            ExitCode::ResolveFailed,
            "the selected channel is not loaded",
            "run `pkg update` for that channel before searching offline",
        ));
    }
    let options = SearchOptions::new(
        args.query(),
        usize::from(args.limit()),
        args.exact(),
        args.license(),
    )
    .map_err(query_error)?;
    let response = IndexQuery::new(document, stale)
        .search(&options)
        .map_err(query_error)?;
    let entries = response
        .results()
        .iter()
        .map(summary_value)
        .collect::<Vec<_>>();
    let records = entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!("package"));
            record
        })
        .collect();
    CommandResult::new(
        format!("{} package(s) found", entries.len()),
        Map::from_iter([
            ("channelSequence".into(), json!(response.channel_seq())),
            ("catalogGeneratedAt".into(), json!(document.generated_at())),
            ("stale".into(), json!(response.stale())),
            ("entries".into(), Value::Array(entries)),
        ]),
        records,
    )
    .map_err(result_error)
}

/// Executes default index-served package info without evaluation or network access.
pub fn info_index(
    document: &IndexDocument,
    stale: bool,
    args: &InfoArgs,
) -> Result<CommandResult, CommandError> {
    if args.exact() {
        return Err(CommandError::new(
            ExitCode::EngineUnavailable,
            "exact package inspection requires the private package engine",
            "omit `--exact` for verified offline catalog metadata",
        ));
    }
    if args.channel().is_some() {
        return Err(CommandError::new(
            ExitCode::ResolveFailed,
            "the selected channel is not loaded",
            "run `pkg update` for that channel before inspecting it offline",
        ));
    }
    let query = IndexQuery::new(document, stale);
    let mut entries = Vec::with_capacity(args.packages().len());
    let mut channel_sequence = None;
    let mut response_stale = false;
    for selector in args.packages() {
        let response = query.info(selector).map_err(query_error)?;
        channel_sequence = Some(response.channel_seq());
        response_stale |= response.stale();
        match response.lookup() {
            InfoLookup::Found { package } => entries.push(json!({
                "package": package.package(),
                "name": package.name(),
                "version": optional_text(package.version()),
                "description": optional_text(package.description()),
                "homepage": optional_text(package.homepage()),
                "licenses": package.licenses(),
                "outputs": package.outputs(),
                "outputsToInstall": null,
                "platforms": package.platforms(),
                "available": package.available(),
                "broken": package.broken(),
                "sourceRevision": package.catalog_revision(),
                "catalogGeneratedAt": package.catalog_generated_at(),
                "advisoryStatus": "unavailable",
                "installedSizeEstimateBytes": package.installed_size_estimate_bytes(),
                "installed": null,
                "pinned": null
            })),
            InfoLookup::Ambiguous { candidates } => {
                return Err(ambiguous_package_error(
                    candidates.len(),
                    candidates.iter().map(pkg_index::PackageSummary::package),
                ));
            }
            InfoLookup::NotFound { .. } => {
                return Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "package was not found in the verified index",
                    "run `pkg search` or `pkg update` and try again",
                ));
            }
        }
    }
    let records = entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!("package_info"));
            record
        })
        .collect();
    CommandResult::new(
        format!("{} package(s) inspected", entries.len()),
        Map::from_iter([
            (
                "channelSequence".into(),
                json!(channel_sequence.unwrap_or_default()),
            ),
            ("stale".into(), json!(response_stale)),
            ("entries".into(), Value::Array(entries)),
        ]),
        records,
    )
    .map_err(result_error)
}

/// Renders one broker-produced authenticated catalog search report.
pub fn search_catalog_report(report: &CatalogSearchReport) -> Result<CommandResult, CommandError> {
    let entries = report
        .results()
        .iter()
        .map(catalog_summary_value)
        .collect::<Vec<_>>();
    let records = entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!("package"));
            record
        })
        .collect();
    CommandResult::new(
        format!("{} package(s) found", entries.len()),
        Map::from_iter([
            (
                "channelSequence".into(),
                json!(report.sequence().get().get()),
            ),
            ("catalogGeneratedAt".into(), json!(report.generated_at())),
            ("stale".into(), json!(false)),
            ("entries".into(), Value::Array(entries)),
        ]),
        records,
    )
    .map_err(result_error)
}

/// Renders broker-produced authenticated package-info reports.
pub fn info_catalog_reports(reports: &[CatalogInfoReport]) -> Result<CommandResult, CommandError> {
    let sequence = reports.first().map(CatalogInfoReport::sequence);
    if reports
        .iter()
        .any(|report| Some(report.sequence()) != sequence)
    {
        return Err(result_error(
            crate::commands::execute::PublicResultError::InvalidValue,
        ));
    }
    let mut entries = Vec::with_capacity(reports.len());
    for report in reports {
        match report.lookup() {
            CatalogInfoLookup::Found(package) => {
                let summary = package.summary();
                entries.push(json!({
                    "package": summary.package(),
                    "name": summary.name(),
                    "version": optional_text(summary.version()),
                    "description": optional_text(summary.description()),
                    "homepage": optional_text(package.homepage()),
                    "licenses": summary.licenses(),
                    "outputs": package.outputs(),
                    "outputsToInstall": null,
                    "platforms": package.platforms(),
                    "available": summary.available(),
                    "broken": summary.broken(),
                    "sourceRevision": package.catalog_revision(),
                    "catalogGeneratedAt": package.catalog_generated_at(),
                    "advisoryStatus": "unavailable",
                    "installedSizeEstimateBytes": null,
                    "installed": null,
                    "pinned": null
                }));
            }
            CatalogInfoLookup::Ambiguous(candidates) => {
                return Err(ambiguous_package_error(
                    candidates.len(),
                    candidates.iter().map(CatalogPackageSummary::package),
                ));
            }
            CatalogInfoLookup::NotFound => {
                return Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "package was not found in the verified index",
                    "run `pkg search` or `pkg update` and try again",
                ));
            }
        }
    }
    let records = entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!("package_info"));
            record
        })
        .collect();
    CommandResult::new(
        format!("{} package(s) inspected", entries.len()),
        Map::from_iter([
            (
                "channelSequence".into(),
                json!(sequence.map_or(0, |value| value.get().get())),
            ),
            ("stale".into(), json!(false)),
            ("entries".into(), Value::Array(entries)),
        ]),
        records,
    )
    .map_err(result_error)
}

/// Compares verified installed product identities with broker-owned catalog metadata.
pub fn outdated_catalog_reports(
    installed_sequence: ChannelSequence,
    installed: &[InstalledCatalogPackage],
    reports: &[CatalogInfoReport],
) -> Result<CommandResult, CommandError> {
    if installed.len() != reports.len() {
        return Err(result_error(
            crate::commands::execute::PublicResultError::InvalidValue,
        ));
    }
    let catalog_sequence = reports
        .first()
        .map_or(installed_sequence, CatalogInfoReport::sequence);
    if catalog_sequence.get() < installed_sequence.get()
        || reports
            .iter()
            .any(|report| report.sequence() != catalog_sequence)
    {
        return Err(result_error(
            crate::commands::execute::PublicResultError::InvalidValue,
        ));
    }

    let mut entries = Vec::new();
    for (installed, report) in installed.iter().zip(reports) {
        let CatalogInfoLookup::Found(available) = report.lookup() else {
            return Err(CommandError::new(
                ExitCode::ResolveFailed,
                "an installed package is absent from the verified catalog",
                "run `pkg update`; use `pkg info` with the canonical package id if it persists",
            ));
        };
        let summary = available.summary();
        if summary.package() != installed.package() {
            return Err(result_error(
                crate::commands::execute::PublicResultError::InvalidValue,
            ));
        }
        let version_changed = summary.version() != installed.version.as_str();
        let revision_changed = available.catalog_revision() != installed.revision.as_str();
        if !version_changed && !revision_changed {
            continue;
        }
        let kind = if version_changed {
            version_change_kind(installed.version.as_str(), summary.version())
        } else {
            "rev-only"
        };
        entries.push(json!({
            "package": installed.package(),
            "name": installed.name,
            "current": installed.version.as_str(),
            "available": summary.version(),
            "pinned": installed.pinned,
            "kind": kind
        }));
    }
    let records = entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let mut record = entry.clone();
            record.insert("type".into(), json!("outdated_package"));
            record
        })
        .collect();
    CommandResult::new(
        format!("{} package(s) outdated", entries.len()),
        Map::from_iter([
            (
                "channelSequence".into(),
                json!(catalog_sequence.get().get()),
            ),
            ("stale".into(), json!(false)),
            ("entries".into(), Value::Array(entries)),
        ]),
        records,
    )
    .map_err(result_error)
}

fn version_change_kind(current: &str, available: &str) -> &'static str {
    let current = dotted_release(current);
    let available = dotted_release(available);
    match (current, available) {
        (Some((current_major, _, _)), Some((available_major, _, _)))
            if current_major != available_major =>
        {
            "major"
        }
        (Some((_, current_minor, _)), Some((_, available_minor, _)))
            if current_minor != available_minor =>
        {
            "minor"
        }
        _ => "patch",
    }
}

fn dotted_release(value: &str) -> Option<(u64, Option<u64>, Option<u64>)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = value.splitn(4, '.');
    let major = numeric_prefix(parts.next()?)?;
    let minor = parts.next().and_then(numeric_prefix);
    let patch = parts.next().and_then(numeric_prefix);
    Some((major, minor, patch))
}

fn numeric_prefix(value: &str) -> Option<u64> {
    let end = value
        .as_bytes()
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    (end != 0).then(|| value[..end].parse().ok()).flatten()
}

fn summary_value(summary: &pkg_index::PackageSummary) -> Value {
    json!({
        "package": summary.package(),
        "name": summary.name(),
        "version": optional_text(summary.version()),
        "description": optional_text(summary.description()),
        "licenses": summary.licenses(),
        "available": summary.available(),
        "broken": summary.broken()
    })
}

fn catalog_summary_value(summary: &CatalogPackageSummary) -> Value {
    json!({
        "package": summary.package(),
        "name": summary.name(),
        "version": optional_text(summary.version()),
        "description": optional_text(summary.description()),
        "licenses": summary.licenses(),
        "available": summary.available(),
        "broken": summary.broken()
    })
}

fn optional_text(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        json!(value)
    }
}

pub(crate) fn ambiguous_package_error<'a>(
    count: usize,
    packages: impl Iterator<Item = &'a str>,
) -> CommandError {
    let choices = packages.take(3).collect::<Vec<_>>().join(", ");
    let hint = if choices.is_empty() {
        "run `pkg search` to find a package id".to_owned()
    } else if count > 3 {
        format!("choose one: {choices}; run `pkg search` for more matches")
    } else {
        format!("choose one: {choices}")
    };
    CommandError::new(
        ExitCode::ResolveFailed,
        format!("package name matches {count} packages"),
        hint,
    )
}

fn query_error(_: pkg_index::QueryError) -> CommandError {
    CommandError::new(
        ExitCode::ResolveFailed,
        "package query was invalid",
        "use a bounded query and a valid SPDX license identifier",
    )
}

fn result_error(_: crate::commands::execute::PublicResultError) -> CommandError {
    CommandError::new(
        ExitCode::Config,
        "package metadata could not cross the public output boundary",
        "refresh metadata; report the issue if it persists",
    )
}

#[cfg(test)]
mod tests {
    use pkg_core::{AttributePath, ChannelSequence, NixpkgsRevision, PackageVersion, System};
    use pkg_index::{BuildMetadata, build_index_from_json};
    use pkg_nix::CatalogPackageInfo;

    use super::*;
    use crate::cli::{Cli, Command};

    const FIXTURE: &[u8] =
        include_bytes!("../../../../fixtures/nixpkgs-slice-tiny/index-input.json");

    fn index() -> pkg_index::BuiltIndex {
        let metadata = BuildMetadata::new(
            ChannelSequence::from_u64(42).unwrap(),
            System::Aarch64Darwin,
            NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            "2026-08-09T00:00:00Z",
        )
        .unwrap();
        build_index_from_json(metadata, FIXTURE).unwrap()
    }

    fn installed_package(
        package: &str,
        version: &str,
        revision: &str,
        pinned: bool,
    ) -> InstalledCatalogPackage {
        InstalledCatalogPackage::new(
            AttributePath::new(package).unwrap(),
            package.to_owned(),
            PackageVersion::new(version),
            NixpkgsRevision::new(revision).unwrap(),
            pinned,
        )
    }

    fn catalog_info_report(
        package: &str,
        version: &str,
        revision: &str,
        sequence: u64,
    ) -> CatalogInfoReport {
        let summary = CatalogPackageSummary::new(
            package,
            package,
            version,
            "fixture package",
            vec![String::from("MIT")],
            true,
            false,
        )
        .unwrap();
        let info = CatalogPackageInfo::new(
            summary,
            "https://example.invalid",
            vec![String::from("out")],
            vec![String::from("linux-x86-64")],
            revision,
            "2026-08-12T00:00:00Z",
        )
        .unwrap();
        CatalogInfoReport::new(
            ChannelSequence::from_u64(sequence).unwrap(),
            CatalogInfoLookup::Found(Box::new(info)),
        )
        .unwrap()
    }

    #[test]
    fn offline_search_maps_verified_index_to_product_fields() {
        let cli = Cli::try_parse(["pkg", "search", "ripgrep", "--license", "MIT"]).unwrap();
        let Command::Search(args) = cli.parsed_command() else {
            unreachable!()
        };
        let index = index();
        let result = search_index(index.document(), true, args).unwrap();
        assert_eq!(result.fields()["stale"], true);
        assert_eq!(
            result.fields()["catalogGeneratedAt"],
            "2026-08-09T00:00:00Z"
        );
        assert_eq!(result.fields()["entries"][0]["package"], "ripgrep");
        assert_eq!(result.records()[0]["type"], "package");
        let encoded = serde_json::to_string(result.fields()).unwrap();
        assert!(!encoded.contains("aarch64-darwin"));
        assert!(!encoded.contains("/nix/store/"));
    }

    #[test]
    fn absent_catalog_text_is_null_and_ambiguity_lists_safe_ids() {
        let summary = CatalogPackageSummary::new(
            "python3Packages.requests",
            "requests",
            "",
            "",
            Vec::new(),
            true,
            false,
        )
        .unwrap();
        let report = CatalogSearchReport::new(
            ChannelSequence::from_u64(42).unwrap(),
            "2026-08-19T00:00:00Z",
            vec![summary.clone()],
        )
        .unwrap();
        let result = search_catalog_report(&report).unwrap();
        assert_eq!(result.fields()["entries"][0]["version"], Value::Null);
        assert_eq!(result.fields()["entries"][0]["description"], Value::Null);

        let error = ambiguous_package_error(
            2,
            ["python3Packages.requests", "pythonPackages.requests"].into_iter(),
        );
        assert_eq!(error.exit_code(), ExitCode::ResolveFailed);
        assert_eq!(
            error.hint(),
            "choose one: python3Packages.requests, pythonPackages.requests"
        );
    }

    #[test]
    fn outdated_reports_all_version_kinds_and_revision_only_changes() {
        const OLD_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
        const NEW_REVISION: &str = "89abcdef0123456789abcdef0123456789abcdef";
        let installed = vec![
            installed_package("patch", "1.2.3", OLD_REVISION, false),
            installed_package("minor", "1.2.3", OLD_REVISION, true),
            installed_package("major", "1.2.3", OLD_REVISION, false),
            installed_package("recipe", "1.2.3", OLD_REVISION, false),
            installed_package("current", "1.2.3", NEW_REVISION, false),
        ];
        let reports = vec![
            catalog_info_report("patch", "1.2.4", NEW_REVISION, 43),
            catalog_info_report("minor", "1.3.0", NEW_REVISION, 43),
            catalog_info_report("major", "2.0.0", NEW_REVISION, 43),
            catalog_info_report("recipe", "1.2.3", NEW_REVISION, 43),
            catalog_info_report("current", "1.2.3", NEW_REVISION, 43),
        ];

        let result =
            outdated_catalog_reports(ChannelSequence::from_u64(42).unwrap(), &installed, &reports)
                .unwrap();
        assert_eq!(result.fields()["channelSequence"], 43);
        assert_eq!(result.fields()["entries"].as_array().unwrap().len(), 4);
        assert_eq!(result.fields()["entries"][0]["kind"], "patch");
        assert_eq!(result.fields()["entries"][1]["kind"], "minor");
        assert_eq!(result.fields()["entries"][1]["pinned"], true);
        assert_eq!(result.fields()["entries"][2]["kind"], "major");
        assert_eq!(result.fields()["entries"][3]["kind"], "rev-only");
        assert_eq!(result.records()[0]["type"], "outdated_package");
    }

    #[test]
    fn outdated_refuses_catalog_identity_or_sequence_drift() {
        const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
        let installed = vec![installed_package("ripgrep", "14.1.0", REVISION, false)];
        let wrong_package = vec![catalog_info_report("fd", "10.2.0", REVISION, 42)];
        assert_eq!(
            outdated_catalog_reports(
                ChannelSequence::from_u64(42).unwrap(),
                &installed,
                &wrong_package,
            )
            .unwrap_err()
            .exit_code(),
            ExitCode::Config
        );
        let older = vec![catalog_info_report("ripgrep", "14.1.1", REVISION, 41)];
        assert_eq!(
            outdated_catalog_reports(ChannelSequence::from_u64(42).unwrap(), &installed, &older,)
                .unwrap_err()
                .exit_code(),
            ExitCode::Config
        );
    }

    #[test]
    fn default_info_is_offline_and_exact_remains_engine_bound() {
        let index = index();
        let cli = Cli::try_parse(["pkg", "info", "python3Packages.requests"]).unwrap();
        let Command::Info(args) = cli.parsed_command() else {
            unreachable!()
        };
        let result = info_index(index.document(), false, args).unwrap();
        assert_eq!(result.fields()["entries"][0]["installed"], Value::Null);
        assert_eq!(
            result.fields()["entries"][0]["advisoryStatus"],
            "unavailable"
        );

        let exact = Cli::try_parse(["pkg", "info", "ripgrep", "--exact"]).unwrap();
        let Command::Info(args) = exact.parsed_command() else {
            unreachable!()
        };
        assert_eq!(
            info_index(index.document(), false, args)
                .unwrap_err()
                .exit_code(),
            ExitCode::EngineUnavailable
        );
    }
}
