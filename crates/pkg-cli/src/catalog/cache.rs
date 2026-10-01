//! The disposable discovery snapshot cache.
//!
//! Derived data only: the one latest complete discovery metadata snapshot
//! per source, system, and kind, re-validated on read and filtered locally
//! per query. Nothing here is authoritative for installed packages. The
//! locked revision lives *inside* each record as an identity field: a
//! record is reused fresh only when its revision equals the freshly
//! resolved one, and a new revision atomically replaces the previous
//! snapshot, so the cache never grows per revision. Legacy per-query
//! `search-*.json` files are never read by the current discovery path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::CatalogError;
use crate::nix::CatalogIndex;

/// The snapshot cache schema version.
///
/// The `kind` field plus this schema version distinguish current
/// snapshots from every older cache shape, so an old per-query
/// `search-*.json` file can never be read as a snapshot.
pub const SNAPSHOT_SCHEMA: u32 = 4;

/// The cask snapshot kind: the complete generated index envelope.
pub const KIND_INDEX: &str = "cask-index";

/// One complete metadata snapshot with full provenance.
///
/// `payload` is the complete generated index envelope for the cask
/// source and system. Queries filter it locally. Native discovery rows
/// use the separate SQLite snapshot module.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot<P> {
    /// The snapshot schema version.
    pub schema: u32,
    /// The snapshot kind; distinguishes payloads and file keys.
    pub kind: String,
    /// The moving source reference the snapshot belongs to.
    pub source: String,
    /// The locked reference the snapshot was produced from.
    pub locked_reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The system the snapshot was produced for.
    pub system: String,
    /// When the snapshot was written (Unix seconds).
    pub saved_unix: u64,
    /// The complete derived payload.
    pub payload: P,
}

/// The latest cask index snapshot shape: the complete index envelope.
pub type IndexSnapshot = Snapshot<CatalogIndex>;

/// Read the latest snapshot of one kind, source, and system, if valid.
///
/// `revision` is `Some(expected)` for fresh reuse: the record's own
/// revision must equal it. `None` is the stale fallback after a metadata
/// or live failure, when the current revision is unknown. Every identity
/// field (schema, kind, source, system, revision when given) is
/// re-checked on read, so a renamed, rewritten, stale-schema, or corrupt
/// file is ignored, never answered as a different identity.
#[must_use]
pub fn read_snapshot<P>(
    cache_dir: &Path,
    kind: &str,
    source: &str,
    system: &str,
    revision: Option<&str>,
) -> Option<Snapshot<P>>
where
    P: serde::de::DeserializeOwned,
{
    let path = cache_dir.join(snapshot_path(kind, source, system));
    let text = std::fs::read_to_string(path).ok()?;
    let snap: Snapshot<P> = serde_json::from_str(&text).ok()?;
    // Only immutable provenance is ever written: a record without its own
    // non-empty revision and locked reference is corrupt and is never
    // presented as valid cache, stale fallback included.
    let provenance = snap
        .revision
        .as_deref()
        .is_some_and(|revision| !revision.is_empty())
        && snap
            .locked_reference
            .as_deref()
            .is_some_and(|locked| !locked.is_empty());
    (snap.schema == SNAPSHOT_SCHEMA
        && snap.kind == kind
        && snap.source == source
        && snap.system == system
        && provenance
        && revision.is_none_or(|expected| snap.revision.as_deref() == Some(expected)))
    .then_some(snap)
}

/// Read the latest cask index snapshot (see [`read_snapshot`]).
#[must_use]
pub fn read_index_snapshot(
    cache_dir: &Path,
    source: &str,
    system: &str,
    revision: Option<&str>,
) -> Option<IndexSnapshot> {
    read_snapshot(cache_dir, KIND_INDEX, source, system, revision)
}

/// Write one snapshot atomically (temp file + rename).
///
/// The one file per kind, source, and system is replaced, so a new
/// revision succeeds without keeping history, and every reader sees
/// either the old or the new complete file, never a torn one. A failed
/// write leaves the old latest snapshot in place for the stale fallback.
pub fn write_snapshot<P: serde::Serialize>(
    cache_dir: &Path,
    snap: &Snapshot<P>,
) -> Result<(), CatalogError> {
    use std::io::Write as _;

    std::fs::create_dir_all(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    let text =
        serde_json::to_string(snap).map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    let name = snapshot_path(&snap.kind, &snap.source, &snap.system);
    let final_path = cache_dir.join(&name);
    let tmp_path = cache_dir.join(format!("{}.tmp-{}", name.display(), std::process::id()));
    // Created new: a preexisting path or symlink is never followed. The
    // temp file is ours only after this open succeeds, so a failure here
    // removes nothing — the preexisting path belongs to someone else.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    let write = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))
        .and_then(|()| {
            std::fs::rename(&tmp_path, &final_path)
                .map_err(|error| CatalogError::CacheWrite(error.to_string()))
        });
    if write.is_err() {
        // Never leave a partial temp file of our own behind on failure.
        let _ = std::fs::remove_file(&tmp_path);
    }
    write
}

/// One snapshot file name for a kind, source, and system.
///
/// Revision is deliberately not part of the key: exactly one latest
/// snapshot is kept per identity, replaced on every new revision.
fn snapshot_path(kind: &str, source: &str, system: &str) -> PathBuf {
    PathBuf::from(format!("catalog-{}.json", cache_key(kind, source, system)))
}

/// Remove all cached discovery data (the `update` refresh path).
///
/// Drops the latest snapshots, any leftover write temp files, and any
/// legacy per-query files; every file this cache owns lives in `cache_dir`
/// with a known prefix.
pub fn invalidate_cache(cache_dir: &Path) -> Result<usize, CatalogError> {
    if !cache_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let entries = std::fs::read_dir(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let owned = (name.starts_with("search-") || name.starts_with("catalog-"))
            && (name.ends_with(".json") || name.contains(".tmp-"));
        if owned {
            std::fs::remove_file(entry.path())
                .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The current Unix time in seconds, for cache records.
pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

pub(super) fn cache_key(kind: &str, source: &str, system: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in kind.bytes().chain(source.bytes()).chain(system.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIND_NATIVE: &str = "fixture-json";
    type NativeSnapshot = Snapshot<std::collections::BTreeMap<String, String>>;

    fn read_native_snapshot(
        cache_dir: &Path,
        source: &str,
        system: &str,
        revision: Option<&str>,
    ) -> Option<NativeSnapshot> {
        read_snapshot(cache_dir, KIND_NATIVE, source, system, revision)
    }

    fn native(source: &str, revision: &str) -> NativeSnapshot {
        Snapshot {
            schema: SNAPSHOT_SCHEMA,
            kind: String::from(KIND_NATIVE),
            source: String::from(source),
            locked_reference: Some(format!("locked-{revision}")),
            revision: Some(String::from(revision)),
            system: String::from("x86_64-linux"),
            saved_unix: 1,
            payload: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn snapshots_are_keyed_by_identity_not_revision_or_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snap = native("github:NixOS/nixpkgs/nixpkgs-unstable", "aaa");
        write_snapshot(dir.path(), &snap).expect("writes");
        // Same identity reads back; the revision is checked on read.
        assert_eq!(
            read_native_snapshot(dir.path(), &snap.source, "x86_64-linux", Some("aaa")),
            Some(snap.clone())
        );
        assert_eq!(
            read_native_snapshot(dir.path(), &snap.source, "x86_64-linux", Some("bbb")),
            None
        );
        // The stale fallback reads without a revision requirement.
        assert_eq!(
            read_native_snapshot(dir.path(), &snap.source, "x86_64-linux", None),
            Some(snap.clone())
        );
        // Identity fields are re-checked on read.
        assert_eq!(
            read_native_snapshot(dir.path(), "other-source", "x86_64-linux", Some("aaa")),
            None
        );
        assert_eq!(
            read_native_snapshot(dir.path(), &snap.source, "aarch64-darwin", Some("aaa")),
            None
        );
    }

    #[test]
    fn a_new_revision_replaces_the_one_latest_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_snapshot(dir.path(), &native("source", "aaa")).expect("writes");
        write_snapshot(dir.path(), &native("source", "bbb")).expect("writes");
        // Exactly one file: no per-revision history is kept.
        let files = std::fs::read_dir(dir.path())
            .expect("cache dir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("catalog-"))
            .count();
        assert_eq!(files, 1, "one latest snapshot only");
        let found =
            read_native_snapshot(dir.path(), "source", "x86_64-linux", None).expect("reads");
        assert_eq!(found.revision.as_deref(), Some("bbb"));
    }

    #[test]
    fn kind_separates_native_and_index_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snap = native("source", "aaa");
        write_snapshot(dir.path(), &snap).expect("writes");
        // An index read for the same identity finds nothing: the kinds key
        // different files, so payloads cannot be confused.
        assert!(read_index_snapshot(dir.path(), "source", "x86_64-linux", Some("aaa")).is_none());
    }

    #[test]
    fn invalid_files_are_ignored_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = snapshot_path(KIND_NATIVE, "source", "x86_64-linux");
        std::fs::write(dir.path().join(path), "not json").expect("writes junk");
        assert_eq!(
            read_native_snapshot(dir.path(), "source", "x86_64-linux", Some("aaa")),
            None
        );
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", None).is_none());
    }

    #[test]
    fn old_schema_snapshots_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut snap = native("source", "aaa");
        snap.schema = 3;
        write_snapshot(dir.path(), &snap).expect("writes");
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", Some("aaa")).is_none());
    }

    #[test]
    fn snapshots_without_full_provenance_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Missing revision: corrupt, even for the stale fallback.
        let mut no_revision = native("source", "aaa");
        no_revision.revision = None;
        no_revision.locked_reference = None;
        write_snapshot(dir.path(), &no_revision).expect("writes");
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", Some("aaa")).is_none());
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", None).is_none());
        // Empty strings are corrupt too, never valid provenance.
        let mut empty = native("source", "aaa");
        empty.revision = Some(String::new());
        write_snapshot(dir.path(), &empty).expect("replaces");
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", None).is_none());
        let mut no_locked = native("source", "aaa");
        no_locked.locked_reference = Some(String::new());
        write_snapshot(dir.path(), &no_locked).expect("replaces");
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", None).is_none());
    }

    #[test]
    fn preexisting_temp_path_is_never_removed_or_clobbered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = snapshot_path(KIND_NATIVE, "source", "x86_64-linux");
        let tmp = dir
            .path()
            .join(format!("{}.tmp-{}", name.display(), std::process::id()));
        // A preexisting temp file blocks the create-new open; the failure
        // must surface and the blocking file must survive unchanged.
        std::fs::write(&tmp, "leftover").expect("blocks");
        let snap = native("source", "aaa");
        assert!(write_snapshot(dir.path(), &snap).is_err());
        assert_eq!(std::fs::read_to_string(&tmp).expect("survives"), "leftover");
        assert!(!dir.path().join(&name).exists());
        // Once the blocker is gone the write succeeds and leaves no temp.
        std::fs::remove_file(&tmp).expect("clears");
        write_snapshot(dir.path(), &snap).expect("writes");
        assert!(dir.path().join(&name).exists());
        assert!(!tmp.exists());
    }

    #[test]
    fn preexisting_temp_symlink_target_survives_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = snapshot_path(KIND_NATIVE, "source", "x86_64-linux");
        let tmp = dir
            .path()
            .join(format!("{}.tmp-{}", name.display(), std::process::id()));
        // A symlink at the temp path also blocks the create-new open;
        // the cleanup must not follow or remove it, and the file it
        // points to must keep its content.
        let victim = dir.path().join("victim.json");
        std::fs::write(&victim, "do not touch").expect("victim");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, &tmp).expect("symlink blocks");
        #[cfg(not(unix))]
        std::fs::write(&tmp, "leftover").expect("blocks");
        let snap = native("source", "aaa");
        assert!(write_snapshot(dir.path(), &snap).is_err());
        #[cfg(unix)]
        assert!(
            std::fs::symlink_metadata(&tmp)
                .expect("symlink metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim survives"),
            "do not touch"
        );
        assert!(!dir.path().join(&name).exists());
    }

    #[test]
    fn rename_failure_cleans_own_temp_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = snapshot_path(KIND_NATIVE, "source", "x86_64-linux");
        let tmp = dir
            .path()
            .join(format!("{}.tmp-{}", name.display(), std::process::id()));
        // A directory at the final path makes the rename fail after our
        // temp file was created; our own temp must be removed and the
        // blocking entry must stay.
        std::fs::create_dir(dir.path().join(&name)).expect("blocks rename");
        let snap = native("source", "aaa");
        assert!(write_snapshot(dir.path(), &snap).is_err());
        assert!(!tmp.exists(), "own temp is cleaned");
        assert!(dir.path().join(&name).is_dir(), "blocker survives");
    }

    #[test]
    fn invalidation_drops_snapshots_and_legacy_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_snapshot(dir.path(), &native("source", "aaa")).expect("writes");
        std::fs::write(dir.path().join("search-deadbeef.json"), "{}").expect("legacy query file");
        std::fs::write(dir.path().join("catalog-0000000000000001.json"), "{}")
            .expect("legacy revision file");
        std::fs::write(dir.path().join("unrelated.txt"), "keep").expect("unrelated");
        assert_eq!(invalidate_cache(dir.path()).expect("invalidates"), 3);
        assert!(read_native_snapshot(dir.path(), "source", "x86_64-linux", None).is_none());
        assert!(dir.path().join("unrelated.txt").exists());
    }
}
