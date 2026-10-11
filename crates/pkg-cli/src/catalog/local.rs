//! Local discovery without runtime probes or implicit source refresh.

use std::path::Path;

use crate::nix::{CatalogIndex, SourceIdentity};

use super::{
    Availability, CaskHost, CatalogError, CatalogView, SearchQuery, SearchResult, SourceReport,
    SourceStatus, SupportBadge, cache, native,
};

pub(super) fn report(
    source: &str,
    reference: &str,
    revision: Option<String>,
    saved_unix: u64,
) -> SourceReport {
    let age = cache::now_unix().saturating_sub(saved_unix);
    let stale = saved_unix == 0 || age >= 86_400;
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
        support_detail: None,
    }
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
        support_detail: None,
    }
}

/// Visit a complete native catalog using only local files.
pub fn search_local_native(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &SearchQuery,
    mut sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let Some(snapshot) = native::Snapshot::read(cache_dir, source, system) else {
        return Ok(missing_report(
            source,
            "search data is not ready. Run `pkg setup` to prepare it.",
        ));
    };
    let saved = checked_at(
        cache_dir,
        source,
        system,
        &snapshot.header.reference,
        &snapshot.header.revision,
        snapshot.header.saved_unix,
    );
    let report = report(
        source,
        &snapshot.header.reference,
        snapshot.header.revision.clone(),
        saved,
    );
    snapshot.visit(|attribute, exposed, meta| {
        let row = SearchResult {
            id: format!("nixpkgs:{exposed}"),
            attribute,
            name: meta.pname,
            version: meta.version,
            description: meta.description,
            source: String::from("nixpkgs"),
            reference: Some(snapshot.header.reference.clone()),
            revision: snapshot.header.revision.clone(),
            system: system.to_string(),
            stale: report.status == SourceStatus::Stale,
            support: None,
            install_command: None,
        };
        if let Some(rank) = query.rank(&row) {
            sink(row, rank)?;
        }
        Ok(())
    })?;
    Ok(report)
}

/// Visit a complete cached cask index using only local files and host metadata.
pub fn search_local_casks(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let Some(snapshot) = cache::read_index_snapshot(cache_dir, source, system, None) else {
        return Ok(missing_report(
            source,
            "search data is not ready. Run `pkg setup` to prepare it.",
        ));
    };
    let reference = snapshot
        .locked_reference
        .unwrap_or_else(|| source.to_string());
    let mut report = report(
        source,
        &reference,
        snapshot.revision.clone(),
        snapshot.saved_unix,
    );
    report = platform_report(report, &snapshot.payload.targets, system);
    let view = CatalogView::from_parts(
        SourceIdentity {
            reference: source.to_string(),
            locked_url: Some(reference),
            revision: snapshot.revision,
            display: source.to_string(),
        },
        snapshot.payload,
    );
    visit_casks(&view, system, query, host, &report, sink)?;
    Ok(report)
}

/// Visit one saved tap capture with the same search and host rules.
pub fn search_local_saved(
    saved: &crate::tap::SavedCatalog,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let report = platform_report(
        report(
            &saved.source,
            &saved.reference,
            Some(saved.provenance.revision.clone()),
            saved.provenance.published_unix,
        ),
        &saved.index.targets,
        system,
    );
    visit_casks(
        &CatalogView::from_saved(saved),
        system,
        query,
        host,
        &report,
        sink,
    )?;
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
    view: &CatalogView,
    system: &str,
    query: &SearchQuery,
    host: &CaskHost,
    report: &SourceReport,
    mut sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<(), CatalogError> {
    let Some(entries) = view.entries(system) else {
        return Ok(());
    };
    for (full_id, entry) in entries {
        let meta = super::catalog_meta(&entry.token, entry);
        let id = format!("cask:{full_id}");
        let mut row = SearchResult {
            id,
            attribute: super::package_attribute(system, full_id),
            name: meta.pname,
            version: meta.version,
            description: meta.description,
            source: String::from("cask"),
            reference: report.locked_reference.clone(),
            revision: report.revision.clone(),
            system: system.to_string(),
            stale: report.status == SourceStatus::Stale,
            support: None,
            install_command: None,
        };
        let Some(rank) = query.rank(&row) else {
            continue;
        };
        let support = match host.availability(view, system, &entry.source, &entry.token) {
            Availability::Available => SupportBadge::Eligible,
            Availability::Unavailable { reason } => {
                if !query.unavailable && !query.exact(&[&entry.token, &row.name, full_id, &row.id])
                {
                    continue;
                }
                SupportBadge::Excluded {
                    reason,
                    detail: entry.detail.clone(),
                }
            }
            Availability::Unknown { reason } => {
                if !query.unavailable && !query.exact(&[&entry.token, &row.name, full_id, &row.id])
                {
                    continue;
                }
                SupportBadge::Unknown { reason }
            }
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
    let snap = cache::Snapshot {
        schema: cache::SNAPSHOT_SCHEMA,
        kind: String::from("checked"),
        source: source.to_string(),
        locked_reference: identity.locked_url.clone(),
        revision: identity.revision.clone(),
        system: system.to_string(),
        saved_unix: cache::now_unix(),
        payload: (),
    };
    cache::write_snapshot(cache_dir, &snap)
}

fn checked_at(
    cache_dir: &Path,
    source: &str,
    system: &str,
    reference: &str,
    revision: &Option<String>,
    fallback: u64,
) -> u64 {
    let snap =
        cache::read_snapshot::<()>(cache_dir, "checked", source, system, revision.as_deref());
    snap.filter(|snap| snap.locked_reference.as_deref() == Some(reference))
        .map_or(fallback, |snap| snap.saved_unix)
}

/// Prepare and publish a cask index for a freshly resolved immutable source.
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
        &cache::IndexSnapshot {
            schema: cache::SNAPSHOT_SCHEMA,
            kind: String::from(cache::KIND_INDEX),
            source: source.to_string(),
            locked_reference: Some(reference.to_string()),
            revision: Some(revision.to_string()),
            system: system.to_string(),
            saved_unix: cache::now_unix(),
            payload: index,
        },
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
pub fn record_search_system(cache_dir: &Path, system: &str) -> Result<(), CatalogError> {
    use std::io::Write as _;
    std::fs::create_dir_all(cache_dir).map_err(CatalogError::cache_write)?;
    let mut file = tempfile::NamedTempFile::new_in(cache_dir).map_err(CatalogError::cache_write)?;
    serde_json::to_writer(
        &mut file,
        &PreparedHost {
            platform: platform(),
            system: system.to_string(),
        },
    )
    .map_err(CatalogError::cache_write)?;
    file.flush()
        .and_then(|()| file.as_file().sync_all())
        .map_err(CatalogError::cache_write)?;
    file.persist(cache_dir.join("search-host.json"))
        .map_err(CatalogError::cache_write)?;
    Ok(())
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
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json")).unwrap();
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
