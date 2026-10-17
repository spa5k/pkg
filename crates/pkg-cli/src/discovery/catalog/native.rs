//! Complete discovery snapshots, built with bounded external evaluations.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};

use crate::nix::{MetadataNode, MetadataValue, Nix, NixError, SearchMeta, SourceIdentity};
use crate::profile_lock::ProfileLock;

use super::{CatalogError, cache, exposed_attribute};

const SETTINGS: &str = "PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Header {
    schema: u32,
    source: String,
    system: String,
    pub(super) reference: String,
    pub(super) revision: Option<String>,
    complete: bool,
    #[serde(default)]
    pub(super) saved_unix: u64,
}

pub(super) struct Snapshot {
    connection: Connection,
    pub(super) header: Header,
}

fn path(cache_dir: &Path, source: &str, system: &str) -> PathBuf {
    cache_dir.join(format!(
        "catalog-{}.sqlite",
        cache::cache_key("native-sqlite-1", source, system)
    ))
}

impl Snapshot {
    pub(super) fn read(cache_dir: &Path, source: &str, system: &str) -> Option<Self> {
        let filename = path(cache_dir, source, system);
        let filename = filename
            .parent()?
            .canonicalize()
            .ok()?
            .join(filename.file_name()?);
        let connection = Connection::open_with_flags(
            filename,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .ok()?;
        connection.execute_batch(SETTINGS).ok()?;
        let check: String = connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .ok()?;
        if check != "ok" {
            return None;
        }
        let value: String = connection
            .query_row("SELECT value FROM metadata", [], |row| row.get(0))
            .ok()?;
        let header: Header = serde_json::from_str(&value).ok()?;
        if header.schema != 1
            || !header.complete
            || header.source != source
            || header.system != system
            || header.revision.as_deref().is_none_or(str::is_empty)
            || header.reference.is_empty()
        {
            return None;
        }
        Some(Self { connection, header })
    }

    fn matches(&self, identity: &SourceIdentity) -> bool {
        self.header.revision == identity.revision
            && Some(&self.header.reference) == identity.locked_url.as_ref()
    }

    pub(super) fn prepared(
        cache_dir: &Path,
        source: &str,
        system: &str,
        identity: &SourceIdentity,
    ) -> Option<Self> {
        Self::read(cache_dir, source, system).filter(|snapshot| snapshot.matches(identity))
    }

    pub(super) fn current(
        nix: &Nix,
        cache_dir: &Path,
        source: &str,
        system: &str,
        identity: &SourceIdentity,
    ) -> Result<Self, CatalogError> {
        let (reference, revision) = identity
            .locked_url
            .as_deref()
            .zip(identity.revision.as_deref())
            .filter(|(reference, revision)| !reference.is_empty() && !revision.is_empty())
            .ok_or_else(|| {
                CatalogError::Source(String::from(
                    "catalog preparation requires an immutable source revision",
                ))
            })?;
        if let Some(snapshot) = Self::prepared(cache_dir, source, system, identity) {
            return Ok(snapshot);
        }
        std::fs::create_dir_all(cache_dir).map_err(CatalogError::cache_write)?;
        let destination = path(cache_dir, source, system);
        let _lock = ProfileLock::acquire(&destination).map_err(CatalogError::cache_write)?;
        if let Some(snapshot) = Self::prepared(cache_dir, source, system, identity) {
            return Ok(snapshot);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".pkg-discovery-")
            .tempdir_in(cache_dir)
            .map_err(CatalogError::cache_write)?;
        let staging = temporary.path().join("snapshot.sqlite");
        let mut connection = Connection::open(&staging)?;
        connection.execute_batch(&format!("{SETTINGS}
            PRAGMA journal_mode=DELETE;
            CREATE TABLE metadata(value TEXT NOT NULL);
            CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL, pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE pending(path TEXT PRIMARY KEY, root TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;"))?;
        eprintln!(
            "Preparing search data for {} ({system})…",
            crate::output::short_source(source)
        );
        for root in ["packages", "legacyPackages"] {
            let initial = vec![vec![root.to_string(), system.to_string()]];
            match nix.metadata_batch(reference, system, root, &initial) {
                Ok(nodes) if is_absent_at(&nodes, &initial[0]) => {}
                Ok(nodes) => {
                    store_nodes(&mut connection, &initial, nodes, root, system)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut last_progress = Instant::now();
        loop {
            let jobs: Vec<(String, String)> = {
                let mut statement = connection
                    .prepare("SELECT path, root FROM pending ORDER BY root, path LIMIT 256")?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?
            };
            let Some((_, root)) = jobs.first() else {
                break;
            };
            let paths: Vec<Vec<String>> = jobs
                .iter()
                .take_while(|(_, candidate)| candidate == root)
                .map(|(path, _)| serde_json::from_str(path).map_err(CatalogError::cache_write))
                .collect::<Result<_, _>>()?;
            evaluate(nix, &mut connection, reference, system, root, &paths)?;
            if last_progress.elapsed() >= Duration::from_secs(1) {
                let count: i64 =
                    connection.query_row("SELECT count(*) FROM packages", [], |row| row.get(0))?;
                eprintln!("Search data: {count} packages.");
                last_progress = Instant::now();
            }
        }
        let header = Header {
            schema: 1,
            source: source.to_string(),
            system: system.to_string(),
            reference: reference.to_string(),
            revision: Some(revision.to_string()),
            complete: true,
            saved_unix: cache::now_unix(),
        };
        Self::publish(connection, &staging, &header, cache_dir)
    }

    fn publish(
        mut connection: Connection,
        staging: &Path,
        header: &Header,
        cache_dir: &Path,
    ) -> Result<Self, CatalogError> {
        let transaction = connection.transaction()?;
        transaction.execute_batch("INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed; DROP TABLE pending;")?;
        transaction.execute(
            "INSERT INTO metadata VALUES (?1)",
            [serde_json::to_string(header).map_err(CatalogError::cache_write)?],
        )?;
        transaction.commit()?;
        connection
            .close()
            .map_err(|(_, error)| CatalogError::cache_write(error))?;
        std::fs::File::open(staging)
            .and_then(|file| file.sync_all())
            .map_err(CatalogError::cache_write)?;
        let destination = path(cache_dir, &header.source, &header.system);
        std::fs::rename(staging, &destination).map_err(CatalogError::cache_write)?;
        std::fs::File::open(cache_dir)
            .and_then(|file| file.sync_all())
            .map_err(CatalogError::cache_write)?;
        Self::read(cache_dir, &header.source, &header.system)
            .ok_or_else(|| CatalogError::cache_write("published snapshot could not be read"))
    }

    pub(super) fn visit(
        &self,
        mut visitor: impl FnMut(String, String, SearchMeta) -> Result<(), CatalogError>,
    ) -> Result<(), CatalogError> {
        let mut statement = self.connection.prepare("SELECT p.attribute, CASE WHEN c.n>1 THEN p.attribute ELSE p.exposed END, p.pname, p.version, p.description FROM packages p JOIN claims c ON c.exposed=p.exposed ORDER BY p.attribute")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            visitor(
                row.get(0)?,
                row.get(1)?,
                SearchMeta {
                    pname: row.get(2)?,
                    version: row.get(3)?,
                    description: row.get(4)?,
                },
            )?;
        }
        Ok(())
    }
}

fn is_absent_at(nodes: &[MetadataNode], requested_path: &[String]) -> bool {
    matches!(
        nodes,
        [MetadataNode {
            path,
            value: MetadataValue::Absent,
        }] if path == requested_path
    )
}

fn evaluate(
    nix: &Nix,
    connection: &mut Connection,
    reference: &str,
    system: &str,
    root: &str,
    paths: &[Vec<String>],
) -> Result<(), CatalogError> {
    match nix.metadata_batch(reference, system, root, paths) {
        Ok(nodes) => store_nodes(connection, paths, nodes, root, system),
        Err(NixError::Interrupted { signal, .. }) => Err(CatalogError::Interrupted(signal)),
        Err(error @ (NixError::ResourceLimit(_) | NixError::Failed { .. })) if paths.len() > 1 => {
            if matches!(error, NixError::ResourceLimit(_)) {
                eprintln!("Splitting a search batch of {} entries.", paths.len());
            }
            let (left, right) = paths.split_at(paths.len() / 2);
            evaluate(nix, connection, reference, system, root, left)?;
            evaluate(nix, connection, reference, system, root, right)
        }
        Err(error) => Err(CatalogError::Native(error)),
    }
}

fn store_nodes(
    connection: &mut Connection,
    paths: &[Vec<String>],
    nodes: Vec<MetadataNode>,
    root: &str,
    system: &str,
) -> Result<(), CatalogError> {
    let expected: std::collections::BTreeSet<_> = paths.iter().collect();
    let actual: std::collections::BTreeSet<_> = nodes.iter().map(|node| &node.path).collect();
    if expected != actual || nodes.len() != paths.len() {
        return Err(CatalogError::Source(String::from(
            "incomplete metadata batch",
        )));
    }
    let transaction = connection.transaction()?;
    for node in nodes {
        transaction.execute(
            "DELETE FROM pending WHERE path=?1",
            [serde_json::to_string(&node.path).map_err(CatalogError::cache_write)?],
        )?;
        match node.value {
            MetadataValue::Package { meta } => {
                let attr = node.path.join(".");
                transaction.execute(
                    "INSERT OR REPLACE INTO packages VALUES (?1,?2,?3,?4,?5)",
                    params![
                        attr,
                        exposed_attribute(&attr, system),
                        meta.pname,
                        meta.version,
                        meta.description
                    ],
                )?;
            }
            MetadataValue::Children { children } => {
                for child in children {
                    let mut path = node.path.clone();
                    path.push(child);
                    transaction.execute(
                        "INSERT OR IGNORE INTO pending VALUES (?1,?2)",
                        params![
                            serde_json::to_string(&path).map_err(CatalogError::cache_write)?,
                            root
                        ],
                    )?;
                }
            }
            MetadataValue::Skip | MetadataValue::Error => {}
            MetadataValue::Absent => {
                return Err(CatalogError::Source(String::from(
                    "absent output returned for a requested metadata path",
                )));
            }
        }
    }
    Ok(transaction.commit()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM: &str = "x86_64-linux";

    #[test]
    fn absent_metadata_for_a_different_path_fails_batch_validation() {
        let mut connection = Connection::open_in_memory().unwrap();
        let requested = vec![vec![String::from("packages"), String::from(SYSTEM)]];
        let absent = vec![MetadataNode {
            path: vec![String::from("legacyPackages"), String::from(SYSTEM)],
            value: MetadataValue::Absent,
        }];
        assert!(!is_absent_at(&absent, &requested[0]));

        let error =
            store_nodes(&mut connection, &requested, absent, "packages", SYSTEM).unwrap_err();

        assert!(matches!(
            error,
            CatalogError::Source(detail) if detail == "incomplete metadata batch"
        ));

        let matching_absent = vec![MetadataNode {
            path: requested[0].clone(),
            value: MetadataValue::Absent,
        }];
        assert!(is_absent_at(&matching_absent, &requested[0]));
    }

    fn fixture(nix: &Nix, outputs: &str) -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("flake.nix"),
            format!("{{ outputs = {{ self }}: {outputs}; }}"),
        )
        .unwrap();
        let hash = std::process::Command::new(nix.executable())
            .args(["hash", "path", "--sri"])
            .arg(directory.path())
            .output()
            .unwrap();
        assert!(
            hash.status.success(),
            "{}",
            String::from_utf8_lossy(&hash.stderr)
        );
        let hash = String::from_utf8(hash.stdout).unwrap();
        let reference = format!(
            "path:{}?narHash={}",
            crate::nix::encode_path_reference(directory.path()),
            hash.trim()
                .replace('+', "%2B")
                .replace('/', "%2F")
                .replace('=', "%3D")
        );
        (directory, reference)
    }

    fn snapshot(nix: &Nix, cache_dir: &Path, reference: &str) -> Result<Snapshot, CatalogError> {
        Snapshot::current(
            nix,
            cache_dir,
            "fixture",
            SYSTEM,
            &SourceIdentity {
                reference: reference.to_string(),
                locked_url: Some(reference.to_string()),
                revision: Some(String::from("fixture-revision")),
                display: String::from("fixture"),
            },
        )
    }

    fn native_path() -> Vec<String> {
        vec![String::from("packages"), String::from(SYSTEM)]
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a native Nix runtime"]
    fn native_flake_discovery_reports_absent_roots_and_systems() {
        let nix = crate::nix::discover(None).unwrap();
        let path = native_path();
        let (_fixture, reference) = fixture(&nix, "{ legacyPackages.x86_64-linux = {}; }");
        let absent_root = nix
            .metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path))
            .unwrap();
        assert!(matches!(
            absent_root.as_slice(),
            [MetadataNode {
                value: MetadataValue::Absent,
                ..
            }]
        ));

        let (_fixture, reference) = fixture(
            &nix,
            "{ packages.aarch64-linux = {}; legacyPackages.x86_64-linux = {}; }",
        );
        let absent_system = nix
            .metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path))
            .unwrap();
        assert!(matches!(
            absent_system.as_slice(),
            [MetadataNode {
                value: MetadataValue::Absent,
                ..
            }]
        ));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a native Nix runtime"]
    fn native_flake_discovery_preserves_present_root_and_system_errors() {
        let nix = crate::nix::discover(None).unwrap();
        let path = native_path();
        let (_fixture, reference) = fixture(
            &nix,
            "{ packages = throw \"does not provide attribute ROOT_THROW\"; legacyPackages.x86_64-linux = {}; }",
        );
        let root_error =
            nix.metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path));
        assert!(matches!(
            root_error,
            Err(NixError::Failed { stderr, .. })
                if stderr.contains("does not provide attribute ROOT_THROW")
        ));

        let (_fixture, reference) = fixture(
            &nix,
            "{ packages.x86_64-linux = throw \"SYSTEM_THROW\"; legacyPackages.x86_64-linux = {}; }",
        );
        let system_error =
            nix.metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path));
        assert!(matches!(
            system_error,
            Err(NixError::Failed { stderr, .. }) if stderr.contains("SYSTEM_THROW")
        ));

        let (_fixture, reference) = fixture(
            &nix,
            "{ packages = \"malformed-root\"; legacyPackages.x86_64-linux = {}; }",
        );
        let malformed_root =
            nix.metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path));
        assert!(matches!(
            malformed_root,
            Err(NixError::Failed { stderr, .. }) if stderr.contains("must be an attribute set")
        ));

        let (_fixture, reference) = fixture(
            &nix,
            "{ packages.x86_64-linux = \"malformed-system\"; legacyPackages.x86_64-linux = {}; }",
        );
        let malformed_system =
            nix.metadata_batch(&reference, SYSTEM, "packages", std::slice::from_ref(&path));
        assert!(matches!(
            malformed_system,
            Err(NixError::Failed { stderr, .. }) if stderr.contains("must be an attribute set")
        ));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a native Nix runtime"]
    fn native_flake_discovery_completes_empty_snapshots_and_skips_legacy_errors() {
        let nix = crate::nix::discover(None).unwrap();
        let (_fixture, reference) = fixture(&nix, "{}");
        let cache_dir = tempfile::tempdir().unwrap();
        let empty = snapshot(&nix, cache_dir.path(), &reference).unwrap();
        assert!(empty.header.complete);
        let mut packages = 0;
        empty
            .visit(|_, _, _| {
                packages += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(packages, 0);

        let (_fixture, reference) = fixture(
            &nix,
            "{ packages.x86_64-linux = {}; legacyPackages.x86_64-linux.broken = throw \"LEGACY_DESCENDANT_THROW\"; }",
        );
        let cache_dir = tempfile::tempdir().unwrap();
        let snapshot = snapshot(&nix, cache_dir.path(), &reference).unwrap();
        let mut packages = 0;
        snapshot
            .visit(|_, _, _| {
                packages += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(packages, 0);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a native Nix runtime"]
    fn native_flake_discovery_preserves_descendant_nix_error_cause() {
        let nix = crate::nix::discover(None).unwrap();
        let (_fixture, reference) = fixture(
            &nix,
            "{ packages.x86_64-linux.broken = throw \"PACKAGE_DESCENDANT_THROW\"; legacyPackages.x86_64-linux = {}; }",
        );
        let cache_dir = tempfile::tempdir().unwrap();
        let Err(error) = snapshot(&nix, cache_dir.path(), &reference) else {
            panic!("a packages descendant throw must fail snapshot discovery");
        };
        assert!(matches!(
            &error,
            CatalogError::Native(NixError::Failed { stderr, .. })
                if stderr.contains("PACKAGE_DESCENDANT_THROW")
        ));
        assert!(std::error::Error::source(&error).is_some());
    }
}
