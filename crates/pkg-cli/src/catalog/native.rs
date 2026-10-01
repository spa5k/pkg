//! Complete discovery snapshots, built with bounded external evaluations.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};

use crate::nix::{MetadataNode, Nix, NixError, SearchMeta, SourceIdentity};
use crate::profile_lock::ProfileLock;

use super::{CatalogError, cache, exposed_attribute};

const SETTINGS: &str = "PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;";
const BATCH_SIZE: usize = 256;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Header {
    schema: u32,
    source: String,
    system: String,
    pub(super) reference: String,
    pub(super) revision: Option<String>,
    complete: bool,
}

pub(super) struct Snapshot {
    connection: Connection,
    pub(super) header: Header,
    _temporary: Option<tempfile::TempDir>,
}

fn path(cache_dir: &Path, source: &str, system: &str) -> PathBuf {
    cache_dir.join(format!(
        "catalog-{}.sqlite",
        cache::cache_key("native-sqlite-1", source, system)
    ))
}

fn cache_error(error: impl std::fmt::Display) -> CatalogError {
    CatalogError::CacheWrite(error.to_string())
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
        Some(Self {
            connection,
            header,
            _temporary: None,
        })
    }

    fn matches(&self, identity: &SourceIdentity) -> bool {
        identity.revision.is_some()
            && self.header.revision == identity.revision
            && Some(&self.header.reference) == identity.locked_url.as_ref()
    }

    pub(super) fn current(
        nix: &Nix,
        cache_dir: &Path,
        source: &str,
        system: &str,
        identity: &SourceIdentity,
    ) -> Result<Self, CatalogError> {
        if let Some(snapshot) = Self::read(cache_dir, source, system)
            && snapshot.matches(identity)
        {
            return Ok(snapshot);
        }
        std::fs::create_dir_all(cache_dir).map_err(cache_error)?;
        let destination = path(cache_dir, source, system);
        let _lock = ProfileLock::acquire(&destination).map_err(cache_error)?;
        if let Some(snapshot) = Self::read(cache_dir, source, system)
            && snapshot.matches(identity)
        {
            return Ok(snapshot);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".pkg-discovery-")
            .tempdir_in(cache_dir)
            .map_err(cache_error)?;
        let staging = temporary.path().join("snapshot.sqlite");
        let mut connection = Connection::open(&staging).map_err(cache_error)?;
        connection.execute_batch(&format!("{SETTINGS}
            PRAGMA journal_mode=DELETE;
            CREATE TABLE metadata(value TEXT NOT NULL);
            CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL, pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE pending(path TEXT PRIMARY KEY, root TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;")).map_err(cache_error)?;
        let reference = identity.locked_url.as_deref().unwrap_or(source);
        eprintln!(
            "Preparing search data for {} ({system})…",
            crate::output::short_source(source)
        );
        let mut roots = 0;
        for root in ["packages", "legacyPackages"] {
            let initial = vec![vec![root.to_string(), system.to_string()]];
            match nix.metadata_batch(reference, system, root, &initial) {
                Ok(nodes) => {
                    store_nodes(&mut connection, &initial, nodes, root, system)?;
                    roots += 1;
                }
                Err(NixError::Failed { stderr, .. })
                    if stderr.contains("does not provide attribute") => {}
                Err(error) => return Err(error.into()),
            }
        }
        if roots == 0 {
            return Err(CatalogError::Source(format!(
                "{reference} has no package outputs for {system}"
            )));
        }
        let mut last_progress = Instant::now();
        loop {
            let jobs: Vec<(String, String)> = {
                let mut statement = connection
                    .prepare("SELECT path, root FROM pending ORDER BY root, path LIMIT 256")
                    .map_err(cache_error)?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(cache_error)?
                    .collect::<Result<_, _>>()
                    .map_err(cache_error)?
            };
            let Some((_, root)) = jobs.first() else {
                break;
            };
            let paths: Vec<Vec<String>> = jobs
                .iter()
                .take_while(|(_, candidate)| candidate == root)
                .take(BATCH_SIZE)
                .map(|(path, _)| serde_json::from_str(path).map_err(cache_error))
                .collect::<Result<_, _>>()?;
            evaluate(nix, &mut connection, reference, system, root, &paths)?;
            if last_progress.elapsed() >= Duration::from_secs(1) {
                let count: i64 = connection
                    .query_row("SELECT count(*) FROM packages", [], |row| row.get(0))
                    .map_err(cache_error)?;
                eprintln!("Search data: {count} packages.");
                last_progress = Instant::now();
            }
        }
        let header = Header {
            schema: 1,
            source: source.to_string(),
            system: system.to_string(),
            reference: reference.to_string(),
            revision: identity.revision.clone(),
            complete: true,
        };
        let transaction = connection.transaction().map_err(cache_error)?;
        transaction.execute_batch("INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed; DROP TABLE pending;").map_err(cache_error)?;
        transaction
            .execute(
                "INSERT INTO metadata VALUES (?1)",
                [serde_json::to_string(&header).map_err(cache_error)?],
            )
            .map_err(cache_error)?;
        transaction.commit().map_err(cache_error)?;
        connection
            .close()
            .map_err(|(_, error)| cache_error(error))?;
        std::fs::File::open(&staging)
            .and_then(|file| file.sync_all())
            .map_err(cache_error)?;
        if identity.revision.is_some() && identity.locked_url.is_some() {
            std::fs::rename(&staging, &destination).map_err(cache_error)?;
            std::fs::File::open(cache_dir)
                .and_then(|file| file.sync_all())
                .map_err(cache_error)?;
            Self::read(cache_dir, source, system)
                .ok_or_else(|| cache_error("published snapshot could not be read"))
        } else {
            let connection =
                Connection::open_with_flags(&staging, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(cache_error)?;
            connection.execute_batch(SETTINGS).map_err(cache_error)?;
            Ok(Self {
                connection,
                header,
                _temporary: Some(temporary),
            })
        }
    }

    pub(super) fn visit(
        &self,
        mut visitor: impl FnMut(String, String, SearchMeta) -> Result<(), CatalogError>,
    ) -> Result<(), CatalogError> {
        let mut statement = self.connection.prepare("SELECT p.attribute, CASE WHEN c.n>1 THEN p.attribute ELSE p.exposed END, p.pname, p.version, p.description FROM packages p JOIN claims c ON c.exposed=p.exposed ORDER BY p.attribute").map_err(cache_error)?;
        let mut rows = statement.query([]).map_err(cache_error)?;
        while let Some(row) = rows.next().map_err(cache_error)? {
            visitor(
                row.get(0).map_err(cache_error)?,
                row.get(1).map_err(cache_error)?,
                SearchMeta {
                    pname: row.get(2).map_err(cache_error)?,
                    version: row.get(3).map_err(cache_error)?,
                    description: row.get(4).map_err(cache_error)?,
                },
            )?;
        }
        Ok(())
    }
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
        Err(NixError::Failed { stderr, .. })
            if root == "legacyPackages" && is_evaluation_error(&stderr) =>
        {
            eprintln!("Skipping an evaluation error at {}.", paths[0].join("."));
            store_nodes(
                connection,
                paths,
                vec![MetadataNode {
                    path: paths[0].clone(),
                    kind: String::from("error"),
                    meta: None,
                    children: Vec::new(),
                }],
                root,
                system,
            )
        }
        Err(error) => Err(CatalogError::Source(format!(
            "{}: {error}",
            paths[0].join(".")
        ))),
    }
}

fn is_evaluation_error(stderr: &str) -> bool {
    [
        "expected a",
        "while evaluating",
        "assertion",
        "infinite recursion",
        "undefined variable",
        "attribute '",
        "cannot coerce",
        "is not a",
    ]
    .iter()
    .any(|text| stderr.contains(text))
        && ![
            "unable to download",
            "cannot connect",
            "No space left",
            "Permission denied",
            "out of memory",
        ]
        .iter()
        .any(|text| stderr.contains(text))
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
    let transaction = connection.transaction().map_err(cache_error)?;
    for node in nodes {
        transaction
            .execute(
                "DELETE FROM pending WHERE path=?1",
                [serde_json::to_string(&node.path).map_err(cache_error)?],
            )
            .map_err(cache_error)?;
        match node.kind.as_str() {
            "package" => {
                let meta = node.meta.ok_or_else(|| {
                    CatalogError::Source(String::from("missing package metadata"))
                })?;
                let attr = node.path.join(".");
                transaction
                    .execute(
                        "INSERT OR REPLACE INTO packages VALUES (?1,?2,?3,?4,?5)",
                        params![
                            attr,
                            exposed_attribute(&attr, system),
                            meta.pname,
                            meta.version,
                            meta.description
                        ],
                    )
                    .map_err(cache_error)?;
            }
            "children" => {
                for child in node.children {
                    let mut path = node.path.clone();
                    path.push(child);
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO pending VALUES (?1,?2)",
                            params![serde_json::to_string(&path).map_err(cache_error)?, root],
                        )
                        .map_err(cache_error)?;
                }
            }
            "skip" | "error" => {}
            _ => return Err(CatalogError::Source(String::from("unknown metadata node"))),
        }
    }
    transaction.commit().map_err(cache_error)
}
