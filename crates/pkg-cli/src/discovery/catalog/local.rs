//! Local discovery without runtime probes or implicit source refresh.

use std::path::Path;

use crate::nix::{CatalogIndex, SourceIdentity};

use super::{
    CaskHost, CatalogError, SearchQuery, SearchResult, SourceReport, SourceStatus, cache, native,
};

pub(super) fn report(
    source: &str,
    reference: &str,
    revision: Option<String>,
    saved_unix: u64,
) -> SourceReport {
    let age = cache::now_unix().saturating_sub(saved_unix);
    // The daily job keeps data younger than one day. Warn only when it has
    // missed several runs, not when one run is a few minutes late.
    let stale = saved_unix == 0 || age >= STALE_AFTER;
    SourceReport {
        source: source.to_string(),
        display: None,
        locked_reference: Some(reference.to_string()),
        revision,
        status: if stale {
            SourceStatus::Stale
        } else {
            SourceStatus::Cached
        },
        detail: Some(if saved_unix == 0 {
            String::from("catalog age is unknown; run `pkg update`")
        } else {
            format!(
                "catalog checked {} hour(s) ago{}",
                age / 3600,
                if stale { "; run `pkg update`" } else { "" }
            )
        }),
    }
}

/// Data age in seconds after which search marks a source as stale.
const STALE_AFTER: u64 = 3 * 86_400;

/// The detail of an official source that has no complete local data yet.
pub const NOT_READY: &str = "search data is not ready";

/// Report an official source that has no complete local data yet.
#[must_use]
pub fn not_ready_report(source: &str) -> SourceReport {
    missing_report(source, NOT_READY)
}

/// Report a missing or unreadable source without launching a scan.
#[must_use]
pub fn missing_report(source: &str, detail: &str) -> SourceReport {
    SourceReport {
        source: source.to_string(),
        display: None,
        locked_reference: None,
        revision: None,
        status: SourceStatus::Failed,
        detail: Some(detail.to_string()),
    }
}

/// Visit a complete native catalog using only local files.
/// # Errors
///
/// Returns an error when the cached database cannot be visited or result delivery fails.
pub fn search_local_native(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &SearchQuery,
    mut sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let Some(snapshot) = native::Snapshot::read(cache_dir, source, system) else {
        return Ok(not_ready_report(source));
    };
    let saved = checked_at(
        cache_dir,
        source,
        system,
        &snapshot.header.reference,
        snapshot.header.revision.as_ref(),
        snapshot.header.saved_unix,
    );
    let report = report(
        source,
        &snapshot.header.reference,
        snapshot.header.revision.clone(),
        saved,
    );
    let mut id = String::new();
    snapshot.visit(|package| {
        id.clear();
        id.reserve("nixpkgs:".len() + package.exposed.len());
        id.push_str("nixpkgs:");
        id.push_str(package.exposed);
        let fields = super::matching::SearchFields {
            id: &id,
            attribute: package.attribute,
            name: package.name,
            version: package.version,
            description: package.description,
            source: "nixpkgs",
            system,
        };
        if let Some(rank) = query.rank_fields(&fields) {
            sink(
                SearchResult {
                    id: std::mem::take(&mut id),
                    attribute: package.attribute.to_string(),
                    name: package.name.to_string(),
                    version: package.version.to_string(),
                    description: package.description.to_string(),
                    source: String::from("nixpkgs"),
                    reference: Some(snapshot.header.reference.clone()),
                    revision: snapshot.header.revision.clone(),
                    system: system.to_string(),
                    stale: report.status == SourceStatus::Stale,
                    support: None,
                    install_command: None,
                },
                rank,
            )?;
        }
        Ok(())
    })?;
    Ok(report)
}

/// Visit a complete cached cask index using only local files and host metadata.
/// # Errors
///
/// Returns an error when result delivery fails.
pub fn search_local_casks(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let Some(snapshot) = cache::read_index_snapshot(cache_dir, source, system, None) else {
        return Ok(not_ready_report(source));
    };
    let reference = snapshot.locked_reference.as_deref().unwrap_or(source);
    let mut report = report(
        source,
        reference,
        snapshot.revision.clone(),
        snapshot.saved_unix,
    );
    report = platform_report(report, snapshot.payload.targets(), system);
    visit_casks(&snapshot.payload, system, query, host, &report, sink)?;
    Ok(report)
}

/// Visit one saved tap capture with the same search and host rules.
/// # Errors
///
/// Returns an error when result delivery fails.
pub fn search_local_saved(
    saved: &crate::tap::SavedCatalog,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let report = platform_report(
        report(
            saved.source(),
            saved.reference(),
            Some(saved.provenance().revision.clone()),
            saved.provenance().published_unix,
        ),
        saved.index().targets(),
        system,
    );
    visit_casks(saved.index(), system, query, host, &report, sink)?;
    Ok(report)
}

fn platform_report(mut report: SourceReport, targets: &[String], system: &str) -> SourceReport {
    if !targets.iter().any(|target| target == system) {
        if report.status != SourceStatus::Stale {
            report.status = SourceStatus::SkippedPlatform;
        }
        report.detail = Some(format!(
            "{}; cached catalog targets {} only",
            report.detail.as_deref().unwrap_or(""),
            targets.join(", ")
        ));
    }
    report
}

fn visit_casks(
    index: &CatalogIndex,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    report: &SourceReport,
    mut sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<(), CatalogError> {
    let Some(entries) = index.entries(system) else {
        return Ok(());
    };
    for (full_id, entry) in entries {
        let mut row = SearchResult::catalog(entry, "cask", system, report);
        let Some(rank) = query.rank(&row) else {
            continue;
        };
        let Some(support) = host
            .availability(index, system, &entry.source, &entry.token)?
            .search_badge(
                query.unavailable || query.exact(&[&entry.token, &row.name, full_id, &row.id]),
                entry.detail.as_deref(),
            )
        else {
            continue;
        };
        row.support = Some(support);
        sink(row, rank)?;
    }
    Ok(())
}

pub(super) fn mark_checked(
    cache_dir: &Path,
    source: &str,
    system: &str,
    identity: &SourceIdentity,
) -> Result<(), CatalogError> {
    cache::write_snapshot(
        cache_dir,
        &cache::Snapshot::new("checked", source, system, identity, ()),
    )
}

fn checked_at(
    cache_dir: &Path,
    source: &str,
    system: &str,
    reference: &str,
    revision: Option<&String>,
    fallback: u64,
) -> u64 {
    let snap = cache::read_snapshot::<()>(
        cache_dir,
        "checked",
        source,
        system,
        revision.map(String::as_str),
    );
    snap.filter(|snap| snap.locked_reference.as_deref() == Some(reference))
        .map_or(fallback, |snap| snap.saved_unix)
}

/// Prepare and publish a cask index for a freshly resolved immutable source.
/// # Errors
///
/// Returns an error when the source has no locked reference or revision, Nix evaluation fails, or the cache cannot be written.
pub fn prepare_catalog(
    nix: &crate::nix::Nix,
    cache_dir: &Path,
    source: &str,
    system: &str,
    identity: &SourceIdentity,
) -> Result<(), CatalogError> {
    let reference = identity
        .locked_url
        .as_deref()
        .ok_or_else(|| CatalogError::Source(String::from("source has no locked reference")))?;
    let revision = identity
        .revision
        .as_deref()
        .ok_or_else(|| CatalogError::Source(String::from("source has no revision")))?;
    let existing = cache::read_index_snapshot(cache_dir, source, system, Some(revision))
        .filter(|snap| snap.locked_reference.as_deref() == Some(reference));
    let index: CatalogIndex = match existing {
        Some(snapshot) => snapshot.payload,
        None => nix.catalog_index(reference)?,
    };
    cache::write_snapshot(
        cache_dir,
        &cache::IndexSnapshot::new(cache::KIND_INDEX, source, system, identity, index),
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PreparedHost {
    platform: String,
    system: String,
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Record the system selected by Nix for subsequent runtime-free searches.
/// # Errors
///
/// Returns an error when the host record cannot be created, encoded, or written.
pub fn record_search_system(cache_dir: &Path, system: &str) -> Result<(), CatalogError> {
    cache::write_json(
        cache_dir,
        Path::new("search-host.json"),
        &PreparedHost {
            platform: platform(),
            system: system.to_string(),
        },
    )
}

/// Read the prepared system, or use the platform name for a first search.
#[must_use]
pub fn search_system(cache_dir: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(cache_dir.join("search-host.json"))
        && let Ok(host) = serde_json::from_str::<PreparedHost>(&text)
        && host.platform == platform()
        && !host.system.is_empty()
        && host
            .system
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return host.system;
    }
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_rows_keep_provenance_and_old_platform_data_stays_stale() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/catalog-index.json"))
                .unwrap();
        let index = crate::nix::decode_catalog_index(&raw).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = cache::IndexSnapshot {
            schema: cache::SNAPSHOT_SCHEMA,
            kind: String::from(cache::KIND_INDEX),
            source: String::from("github:example/catalog/main"),
            locked_reference: Some(String::from("locked:old")),
            revision: Some(String::from("old-revision")),
            system: String::from("x86_64-linux"),
            saved_unix: 1,
            payload: index,
        };
        cache::write_snapshot(directory.path(), &snapshot).unwrap();
        let query = SearchQuery::new("koreader", false, false).unwrap();
        let mut rows = Vec::new();
        let report = search_local_casks(
            directory.path(),
            &snapshot.source,
            &snapshot.system,
            &query,
            &CaskHost::default(),
            |row, _rank| {
                rows.push(row);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(report.status, SourceStatus::Stale);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reference, snapshot.locked_reference);
        assert_eq!(rows[0].revision, snapshot.revision);
        assert!(rows[0].stale);
        snapshot.system = String::from("x86_64-darwin");
        cache::write_snapshot(directory.path(), &snapshot).unwrap();
        let report = search_local_casks(
            directory.path(),
            &snapshot.source,
            &snapshot.system,
            &query,
            &CaskHost::default(),
            |_, _| panic!("untargeted platform has no rows"),
        )
        .unwrap();
        assert_eq!(report.status, SourceStatus::Stale);
        assert!(report.detail.unwrap().contains("cached catalog targets"));
        snapshot.saved_unix = cache::now_unix();
        cache::write_snapshot(directory.path(), &snapshot).unwrap();
        let report = search_local_casks(
            directory.path(),
            &snapshot.source,
            &snapshot.system,
            &query,
            &CaskHost::default(),
            |_, _| panic!("untargeted platform has no rows"),
        )
        .unwrap();
        assert_eq!(report.status, SourceStatus::SkippedPlatform);
    }
}
