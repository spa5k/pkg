//! Offline search and package-info handlers over one verified derived index.

use serde_json::{Map, Value, json};

use pkg_index::{IndexDocument, IndexQuery, InfoLookup, SearchOptions};

use crate::cli::{InfoArgs, SearchArgs};
use crate::commands::execute::CommandResult;
use crate::exit::ExitCode;
use crate::ux::CommandError;

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
                "version": package.version(),
                "description": package.description(),
                "homepage": package.homepage(),
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
                return Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    format!(
                        "package selector is ambiguous across {} candidates",
                        candidates.len()
                    ),
                    "use a canonical package id from `pkg search`",
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

fn summary_value(summary: &pkg_index::PackageSummary) -> Value {
    json!({
        "package": summary.package(),
        "name": summary.name(),
        "version": summary.version(),
        "description": summary.description(),
        "licenses": summary.licenses(),
        "available": summary.available(),
        "broken": summary.broken()
    })
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
    use pkg_core::{ChannelSequence, NixpkgsRevision, System};
    use pkg_index::{BuildMetadata, build_index_from_json};

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

    #[test]
    fn offline_search_maps_verified_index_to_product_fields() {
        let cli = Cli::try_parse(["pkg", "search", "ripgrep", "--license", "MIT"]).unwrap();
        let Command::Search(args) = cli.parsed_command() else {
            unreachable!()
        };
        let index = index();
        let result = search_index(index.document(), true, args).unwrap();
        assert_eq!(result.fields()["stale"], true);
        assert_eq!(result.fields()["entries"][0]["package"], "ripgrep");
        assert_eq!(result.records()[0]["type"], "package");
        let encoded = serde_json::to_string(result.fields()).unwrap();
        assert!(!encoded.contains("aarch64-darwin"));
        assert!(!encoded.contains("/nix/store/"));
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
