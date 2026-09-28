//! The disposable discovery cache.
//!
//! Derived data only: cached native search results with the revision they
//! were produced from. Nothing here is authoritative for installed
//! packages, and every entry is re-validated on read.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::CatalogError;
use crate::nix::SearchMeta;

/// The discovery cache schema version.
///
/// Version 2 keys cache files by source, system, and query, and records the
/// locked reference and revision the results came from.
pub const CACHE_SCHEMA: u32 = 2;

/// Cached discovery results with full provenance.
///
/// The cache is keyed by source, system, and query; a file written for one
/// query is never reused to answer a different query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CachedSearch {
    /// The cache schema version.
    pub schema: u32,
    /// The moving source reference the cache belongs to.
    pub source: String,
    /// The locked reference the cached results came from.
    pub locked_reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The system the cached results were produced for.
    pub system: String,
    /// The query the cached results answer.
    pub query: String,
    /// When the cache was written (Unix seconds).
    pub saved_unix: u64,
    /// Attribute path to metadata.
    pub results: BTreeMap<String, SearchMeta>,
}

/// Read the cached results for one source, system, and query, if present.
///
/// The stored identity fields are re-checked on read, so a file that was
/// rewritten or renamed cannot answer as a different query.
#[must_use]
pub fn read_cache(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &str,
) -> Option<CachedSearch> {
    let path = cache_dir.join(format!("search-{}.json", cache_key(source, system, query)));
    let text = std::fs::read_to_string(path).ok()?;
    let cached: CachedSearch = serde_json::from_str(&text).ok()?;
    (cached.schema == CACHE_SCHEMA
        && cached.source == source
        && cached.system == system
        && cached.query == query)
        .then_some(cached)
}

/// Write discovery results to the disposable cache.
pub fn write_cache(cache_dir: &Path, cached: &CachedSearch) -> Result<(), CatalogError> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    let path = cache_dir.join(format!(
        "search-{}.json",
        cache_key(&cached.source, &cached.system, &cached.query)
    ));
    let text = serde_json::to_string_pretty(cached)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    std::fs::write(path, text).map_err(|error| CatalogError::CacheWrite(error.to_string()))
}

/// Remove all cached discovery data (the `update` refresh path).
pub fn invalidate_cache(cache_dir: &Path) -> Result<usize, CatalogError> {
    if !cache_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let entries = std::fs::read_dir(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("search-") && name.ends_with(".json") {
            std::fs::remove_file(entry.path())
                .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn cache_key(source: &str, system: &str, query: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.bytes().chain(system.bytes()).chain(query.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The current Unix time in seconds, for cache records.
pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_keyed_by_source_system_and_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cached = CachedSearch {
            schema: CACHE_SCHEMA,
            source: String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
            locked_reference: Some(String::from("github:NixOS/nixpkgs/aaa")),
            revision: Some(String::from("aaa")),
            system: String::from("aarch64-darwin"),
            query: String::from("query-one"),
            saved_unix: 1,
            results: BTreeMap::new(),
        };
        write_cache(dir.path(), &cached).expect("writes");
        // Same source/system/query reads back; a different query never does.
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-one"),
            Some(cached.clone())
        );
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-two"),
            None
        );
        assert_eq!(
            read_cache(dir.path(), &cached.source, "x86_64-linux", "query-one"),
            None
        );
        // Schema and source fields are re-checked on read.
        assert_eq!(invalidate_cache(dir.path()).expect("invalidates"), 1);
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-one"),
            None
        );
    }
}
