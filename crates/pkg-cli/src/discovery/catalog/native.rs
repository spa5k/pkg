//! Complete discovery snapshots, built with bounded external evaluations.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, params};

use crate::nix::{MetadataNode, MetadataValue, Nix, NixError, SourceIdentity};
use crate::profile_lock::ProfileLock;

use super::{CatalogError, cache, exposed_attribute};

const SETTINGS: &str = "PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;";

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Header {
    schema: u32,
    source: String,
    system: String,
    pub(super) reference: String,
    pub(super) revision: Option<String>,
    complete: bool,
    #[serde(default)]
    pub(super) saved_unix: u64,
    #[serde(default)]
    unique_exposed: bool,
    #[serde(default)]
    pub(super) origin: Origin,
}

/// Where the rows of a snapshot come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// An evaluation on this computer.
    #[default]
    Local,
    /// The package list that a NixOS channel publishes.
    Published,
}

pub(super) struct Snapshot {
    connection: Connection,
    pub(super) header: Header,
}

pub(super) struct Package<'a> {
    pub attribute: &'a str,
    pub exposed: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    /// The main command, from `meta.mainProgram`.
    pub command: Option<&'a str>,
    /// Why Nixpkgs cannot install the package on this system.
    pub unavailable: Option<&'a str>,
}

/// One owned snapshot row, for direct lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRow {
    /// The full native attribute.
    pub attribute: String,
    /// The installable ID suffix after `nixpkgs:`.
    pub exposed: String,
    /// The package name.
    pub name: String,
    /// The package version.
    pub version: String,
    /// The package description.
    pub description: String,
    /// The main command, when declared.
    pub command: Option<String>,
    /// Why Nixpkgs cannot install the package on this system.
    pub unavailable: Option<String>,
}

/// The snapshot schema. Version 3 adds the main command column and the
/// attributes whose evaluation failed. Version 4 adds the reason that a
/// package cannot install on this system.
const SCHEMA: u32 = 4;

/// The file keys of older schemas. A new snapshot removes their files.
const OLD_KEYS: [&str; 3] = ["native-sqlite-1", "native-sqlite-2", "native-sqlite-3"];

fn path(cache_dir: &Path, source: &str, system: &str) -> PathBuf {
    keyed_path(cache_dir, "native-sqlite-4", source, system)
}

/// The data of a stopped local preparation. The next preparation for the
/// same source revision continues it.
fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// Paths in one evaluation.
const BATCH: usize = 512;

fn create_tables(connection: &Connection) -> Result<(), CatalogError> {
    Ok(connection.execute_batch(&format!("{SETTINGS}
        PRAGMA journal_mode=DELETE;
        CREATE TABLE metadata(value TEXT NOT NULL);
        CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL, pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL, command TEXT, leaf TEXT NOT NULL, unavailable TEXT) WITHOUT ROWID;
        CREATE TABLE pending(path TEXT PRIMARY KEY, root TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
        CREATE TABLE failures(attribute TEXT PRIMARY KEY) WITHOUT ROWID;"))?)
}

/// Open the partial data of a stopped preparation for the same source
/// revision. Otherwise, start new partial data. Return whether the data
/// continues. New data continues only after `mark_resumable`.
fn open_partial(staging: &Path, header: &Header) -> Result<(Connection, bool), CatalogError> {
    // The file may not follow a link, but the cache folder may be one.
    let real = staging
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .zip(staging.file_name())
        .map(|(parent, name)| parent.join(name));
    let earlier = real
        .and_then(|real| {
            Connection::open_with_flags(
                real,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .ok()
        })
        .filter(|connection| {
            connection
                .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
                .is_ok_and(|check| check == "ok")
                && connection
                    .query_row("SELECT value FROM metadata", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .ok()
                    .and_then(|value| serde_json::from_str::<Header>(&value).ok())
                    .is_some_and(|earlier| earlier == *header)
                && connection.execute_batch(SETTINGS).is_ok()
        });
    if let Some(connection) = earlier {
        return Ok((connection, true));
    }
    let mut journal = staging.as_os_str().to_owned();
    journal.push("-journal");
    for file in [staging, Path::new(&journal)] {
        match std::fs::remove_file(file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(CatalogError::cache_write(error));
            }
            _ => {}
        }
    }
    let connection = Connection::open(staging)?;
    create_tables(&connection)?;
    Ok((connection, false))
}

/// Let a later run continue this partial data. Call it after the roots are
/// stored, because a run that continues does not evaluate them again.
fn mark_resumable(connection: &Connection, header: &Header) -> Result<(), CatalogError> {
    connection.execute(
        "INSERT INTO metadata VALUES (?1)",
        [serde_json::to_string(header).map_err(CatalogError::cache_write)?],
    )?;
    Ok(())
}

/// Evaluations that run at the same time: at most four, at most one for
/// each processor, and one for each 640 MiB of available memory.
fn parallel_evaluations() -> usize {
    let processors = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let memory = pkg_process::available_memory().map_or(1, |bytes| {
        usize::try_from(bytes / (640 * 1024 * 1024)).unwrap_or(usize::MAX)
    });
    processors.min(memory).clamp(1, 4)
}

fn keyed_path(cache_dir: &Path, key: &str, source: &str, system: &str) -> PathBuf {
    cache_dir.join(format!(
        "catalog-{}.sqlite",
        cache::cache_key(key, source, system)
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
        if header.schema != SCHEMA
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
        let mut header = Header {
            schema: SCHEMA,
            source: source.to_string(),
            system: system.to_string(),
            reference: reference.to_string(),
            revision: Some(revision.to_string()),
            complete: false,
            saved_unix: 0,
            unique_exposed: false,
            origin: Origin::Local,
        };
        let staging = partial_path(&destination);
        let (mut connection, resumed) = open_partial(&staging, &header)?;
        let label = crate::output::short_source(source);
        if resumed {
            crate::output::status(&format!("Preparing search data… {label}: continuing"));
        } else {
            crate::output::status(&format!("Preparing search data… {label}"));
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
            mark_resumable(&connection, &header)?;
        }
        let workers = parallel_evaluations();
        let mut last_progress = Instant::now();
        loop {
            let jobs: Vec<(String, String)> = {
                let mut statement = connection
                    .prepare("SELECT path, root FROM pending ORDER BY root, path LIMIT ?1")?;
                statement
                    .query_map(
                        [i64::try_from(BATCH * workers).unwrap_or(i64::MAX)],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?
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
            let batches: Vec<&[Vec<String>]> = paths.chunks(BATCH).collect();
            if let [batch] = batches.as_slice() {
                evaluate(nix, &mut connection, reference, system, root, batch)?;
            } else {
                evaluate_parallel(nix, &mut connection, reference, system, root, &batches)?;
            }
            if last_progress.elapsed() >= Duration::from_secs(1) {
                let count: i64 =
                    connection.query_row("SELECT count(*) FROM packages", [], |row| row.get(0))?;
                crate::output::status(&format!(
                    "Preparing search data… {label}: {} packages",
                    crate::output::count(u64::try_from(count).unwrap_or_default())
                ));
                last_progress = Instant::now();
            }
        }
        header.complete = true;
        header.saved_unix = cache::now_unix();
        Self::publish(connection, &staging, header, cache_dir)
    }

    /// Build a snapshot from a package list that `read` writes into the
    /// new database.
    pub(super) fn from_list(
        cache_dir: &Path,
        source: &str,
        system: &str,
        identity: &SourceIdentity,
        read: impl FnOnce(&mut Connection) -> Result<(), CatalogError>,
    ) -> Result<Self, CatalogError> {
        let (reference, revision) = identity
            .locked_url
            .as_deref()
            .zip(identity.revision.as_deref())
            .ok_or_else(|| {
                CatalogError::Source(String::from("a package list needs a source revision"))
            })?;
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
        create_tables(&connection)?;
        read(&mut connection)?;
        let header = Header {
            schema: SCHEMA,
            source: source.to_string(),
            system: system.to_string(),
            reference: reference.to_string(),
            revision: Some(revision.to_string()),
            complete: true,
            saved_unix: cache::now_unix(),
            unique_exposed: false,
            origin: Origin::Published,
        };
        Self::publish(connection, &staging, header, cache_dir)
    }

    /// The row count and origin of this snapshot.
    pub(super) fn summary(&self) -> Result<(u64, Origin), CatalogError> {
        let count: i64 = self
            .connection
            .query_row("SELECT count(*) FROM packages", [], |row| row.get(0))?;
        Ok((u64::try_from(count).unwrap_or_default(), self.header.origin))
    }

    fn publish(
        mut connection: Connection,
        staging: &Path,
        mut header: Header,
        cache_dir: &Path,
    ) -> Result<Self, CatalogError> {
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM metadata", [])?;
        transaction.execute_batch("INSERT INTO claims SELECT exposed, count(*) FROM packages GROUP BY exposed; DROP TABLE pending; CREATE INDEX package_commands ON packages(command) WHERE command IS NOT NULL; CREATE INDEX package_names ON packages(pname); CREATE INDEX package_leaves ON packages(leaf);")?;
        header.unique_exposed = transaction.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM claims WHERE n > 1)",
            [],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO metadata VALUES (?1)",
            [serde_json::to_string(&header).map_err(CatalogError::cache_write)?],
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
        for key in OLD_KEYS {
            // A missing old file is the normal case.
            let _ =
                std::fs::remove_file(keyed_path(cache_dir, key, &header.source, &header.system));
        }
        // The partial data of a stopped preparation is not needed now.
        let partial = partial_path(&destination);
        let mut journal = partial.as_os_str().to_owned();
        journal.push("-journal");
        for file in [partial.as_path(), Path::new(&journal)] {
            let _ = std::fs::remove_file(file);
        }
        Self::read(cache_dir, &header.source, &header.system)
            .ok_or_else(|| CatalogError::cache_write("published snapshot could not be read"))
    }

    pub(super) fn visit(
        &self,
        mut visitor: impl FnMut(Package<'_>) -> Result<(), CatalogError>,
    ) -> Result<(), CatalogError> {
        let query = format!("{} ORDER BY p.attribute", self.select());
        let mut statement = self.connection.prepare(&query)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            visitor(Package {
                attribute: text(row, 0)?,
                exposed: text(row, 1)?,
                name: text(row, 2)?,
                version: text(row, 3)?,
                description: text(row, 4)?,
                command: optional_text(row, 5)?,
                unavailable: optional_text(row, 6)?,
            })?;
        }
        Ok(())
    }

    /// The row selection with the shared exposed-name rule: a short name
    /// that more than one attribute claims shows the full attribute.
    const fn select(&self) -> &'static str {
        if self.header.unique_exposed {
            "SELECT p.attribute, p.exposed, p.pname, p.version, p.description, p.command, p.unavailable FROM packages p"
        } else {
            "SELECT p.attribute, CASE WHEN c.n>1 THEN p.attribute ELSE p.exposed END, p.pname, p.version, p.description, p.command, p.unavailable FROM packages p JOIN claims c ON c.exposed=p.exposed"
        }
    }

    fn rows(&self, filter: &str, value: &str) -> Result<Vec<NativeRow>, CatalogError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{} WHERE {filter} ORDER BY p.attribute",
            self.select()
        ))?;
        let rows = statement.query_map([value], |row| {
            Ok(NativeRow {
                attribute: row.get(0)?,
                exposed: row.get(1)?,
                name: row.get(2)?,
                version: row.get(3)?,
                description: row.get(4)?,
                command: row.get(5)?,
                unavailable: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The row for one full native attribute.
    pub(super) fn attribute(&self, attribute: &str) -> Result<Option<NativeRow>, CatalogError> {
        Ok(self.rows("p.attribute = ?1", attribute)?.into_iter().next())
    }

    /// Rows whose package name or last attribute segment is `name`.
    pub(super) fn named(&self, name: &str) -> Result<Vec<NativeRow>, CatalogError> {
        self.rows("(p.pname = ?1 OR p.leaf = ?1)", name)
    }

    /// Rows whose main command is `command`.
    pub(super) fn commands(&self, command: &str) -> Result<Vec<NativeRow>, CatalogError> {
        self.rows("p.command = ?1", command)
    }

    /// Whether this full attribute failed to evaluate during preparation,
    /// for example a removed alias that throws.
    pub(super) fn failed(&self, attribute: &str) -> Result<bool, CatalogError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM failures WHERE attribute = ?1)",
            [attribute],
            |row| row.get(0),
        )?)
    }
}

fn optional_text<'a>(
    row: &'a rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<&'a str>> {
    let value = row.get_ref(column)?;
    value.as_str_or_null().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
    })
}

fn text<'a>(row: &'a rusqlite::Row<'_>, column: usize) -> rusqlite::Result<&'a str> {
    let value = row.get_ref(column)?;
    value.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
    })
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
            if matches!(error, NixError::ResourceLimit(_)) && nix.is_verbose() {
                eprintln!("Splitting a search batch of {} entries.", paths.len());
            }
            let (left, right) = paths.split_at(paths.len() / 2);
            evaluate(nix, connection, reference, system, root, left)?;
            evaluate(nix, connection, reference, system, root, right)
        }
        // One entry alone needs more memory than the budget allows. Record
        // it as failed, so the rest of the data stays usable on a small
        // computer. Install and info check such a name through Nix.
        Err(NixError::ResourceLimit(limit)) => {
            if nix.is_verbose() {
                eprintln!("Skipping {} in search data: {limit}.", paths[0].join("."));
            }
            let nodes = vec![MetadataNode {
                path: paths[0].clone(),
                value: MetadataValue::Error,
            }];
            store_nodes(connection, paths, nodes, root, system)
        }
        Err(error) => Err(CatalogError::Native(error)),
    }
}

/// Evaluate several batches of one root at the same time.
///
/// Successful batches are stored first, so a stop keeps their rows. A
/// failed batch runs again alone and splits when it must.
fn evaluate_parallel(
    nix: &Nix,
    connection: &mut Connection,
    reference: &str,
    system: &str,
    root: &str,
    batches: &[&[Vec<String>]],
) -> Result<(), CatalogError> {
    let results = nix.metadata_batches(reference, system, root, batches)?;
    let mut failed = Vec::new();
    let mut interrupted = None;
    for (batch, result) in batches.iter().zip(results) {
        match result {
            Ok(nodes) => store_nodes(connection, batch, nodes, root, system)?,
            Err(NixError::Interrupted { signal, .. }) => interrupted = Some(signal),
            Err(_) => failed.push(*batch),
        }
    }
    if let Some(signal) = interrupted {
        return Err(CatalogError::Interrupted(signal));
    }
    for batch in failed {
        evaluate(nix, connection, reference, system, root, batch)?;
    }
    Ok(())
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
            MetadataValue::Package {
                meta,
                command,
                unavailable,
            } => {
                let attr = node.path.join(".");
                transaction.execute(
                    "INSERT OR REPLACE INTO packages VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        attr,
                        exposed_attribute(&attr, system),
                        meta.pname,
                        meta.version,
                        meta.description,
                        command,
                        node.path.last().map_or("", String::as_str),
                        unavailable
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
            MetadataValue::Error => {
                transaction.execute(
                    "INSERT OR IGNORE INTO failures VALUES (?1)",
                    [node.path.join(".")],
                )?;
            }
            MetadataValue::Skip => {}
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
    fn snapshot_visits_preserve_text_and_reject_invalid_utf8() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE packages(attribute TEXT, exposed TEXT, pname TEXT, version TEXT, description TEXT, command TEXT, unavailable TEXT);
                 CREATE TABLE claims(exposed TEXT, n INTEGER);
                 INSERT INTO packages VALUES ('packages.x86_64-linux.editor', 'editor', 'Éditeur', '1.2', '', 'editor', NULL);
                 INSERT INTO packages VALUES ('legacyPackages.x86_64-linux.editor', 'editor', '別名', '2.0', 'Unicode description', NULL, 'unfree');
                 INSERT INTO claims VALUES ('editor', 2);",
            )
            .unwrap();
        let snapshot = Snapshot {
            connection,
            header: Header {
                schema: SCHEMA,
                source: String::from("fixture"),
                system: String::from(SYSTEM),
                reference: String::from("fixture-reference"),
                revision: Some(String::from("fixture-revision")),
                complete: true,
                saved_unix: 1,
                unique_exposed: false,
                origin: Origin::Local,
            },
        };
        let mut visited = Vec::new();
        snapshot
            .visit(|package| {
                visited.push((
                    package.attribute.to_string(),
                    package.exposed.to_string(),
                    package.unavailable.map(str::to_string),
                    crate::nix::SearchMeta {
                        pname: package.name.to_string(),
                        version: package.version.to_string(),
                        description: package.description.to_string(),
                    },
                ));
                Ok(())
            })
            .unwrap();
        assert_eq!(visited.len(), 2);
        assert_eq!(visited[0].0, "legacyPackages.x86_64-linux.editor");
        assert_eq!(visited[0].1, visited[0].0);
        assert_eq!(visited[0].2.as_deref(), Some("unfree"));
        assert_eq!(visited[0].3.pname, "別名");
        assert_eq!(visited[1].1, visited[1].0);
        assert_eq!(visited[1].2, None);
        assert_eq!(visited[1].3.pname, "Éditeur");
        assert_eq!(visited[1].3.version, "1.2");
        assert_eq!(visited[1].3.description, "");

        snapshot
            .connection
            .execute("UPDATE packages SET pname=CAST(X'ff' AS TEXT)", [])
            .unwrap();
        assert!(snapshot.visit(|_| Ok(())).is_err());
    }

    fn header() -> Header {
        Header {
            schema: SCHEMA,
            source: String::from("fixture"),
            system: String::from(SYSTEM),
            reference: String::from("fixture-reference"),
            revision: Some(String::from("fixture-revision")),
            complete: true,
            saved_unix: 1,
            unique_exposed: false,
            origin: Origin::Local,
        }
    }

    #[test]
    fn legacy_metadata_keeps_collision_resolution() {
        let mut value = serde_json::to_value(header()).unwrap();
        value.as_object_mut().unwrap().remove("unique_exposed");
        let decoded: Header = serde_json::from_value(value).unwrap();
        assert!(!decoded.unique_exposed);
    }

    #[test]
    fn published_snapshots_resolve_unique_and_ambiguous_exposed_names() {
        for ambiguous in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let staging = directory.path().join("staging.sqlite");
            let old = keyed_path(directory.path(), OLD_KEYS[0], "fixture", SYSTEM);
            std::fs::write(&old, "old schema").unwrap();
            let connection = Connection::open(&staging).unwrap();
            connection.execute_batch(
                "CREATE TABLE metadata(value TEXT NOT NULL);
                 CREATE TABLE packages(attribute TEXT PRIMARY KEY, exposed TEXT NOT NULL, pname TEXT NOT NULL, version TEXT NOT NULL, description TEXT NOT NULL, command TEXT, leaf TEXT NOT NULL, unavailable TEXT) WITHOUT ROWID;
                 CREATE TABLE claims(exposed TEXT PRIMARY KEY, n INTEGER NOT NULL) WITHOUT ROWID;
                 CREATE TABLE pending(path TEXT PRIMARY KEY, root TEXT NOT NULL) WITHOUT ROWID;
                 CREATE TABLE failures(attribute TEXT PRIMARY KEY) WITHOUT ROWID;
                 INSERT INTO packages VALUES ('legacyPackages.x86_64-linux.editor', 'editor', 'Éditeur', '1.2', 'Unicode description', NULL, 'editor', NULL);
                 INSERT INTO failures VALUES ('legacyPackages.x86_64-linux.removed');"
            ).unwrap();
            if ambiguous {
                connection.execute_batch("INSERT INTO packages VALUES ('packages.x86_64-linux.editor', 'editor', '別名', '2.0', '', 'editor', 'editor', NULL);").unwrap();
            }
            let snapshot =
                Snapshot::publish(connection, &staging, header(), directory.path()).unwrap();
            assert_eq!(snapshot.header.unique_exposed, !ambiguous);
            assert!(!old.exists(), "the old schema file is removed");
            assert!(
                snapshot
                    .failed("legacyPackages.x86_64-linux.removed")
                    .unwrap()
            );
            assert!(
                !snapshot
                    .failed("legacyPackages.x86_64-linux.editor")
                    .unwrap()
            );
            let mut rows = Vec::new();
            snapshot
                .visit(|package| {
                    rows.push((
                        package.attribute.to_string(),
                        package.exposed.to_string(),
                        package.name.to_string(),
                    ));
                    Ok(())
                })
                .unwrap();
            if ambiguous {
                assert_eq!(
                    rows,
                    vec![
                        (
                            String::from("legacyPackages.x86_64-linux.editor"),
                            String::from("legacyPackages.x86_64-linux.editor"),
                            String::from("Éditeur")
                        ),
                        (
                            String::from("packages.x86_64-linux.editor"),
                            String::from("packages.x86_64-linux.editor"),
                            String::from("別名")
                        ),
                    ]
                );
            } else {
                assert_eq!(
                    rows,
                    vec![(
                        String::from("legacyPackages.x86_64-linux.editor"),
                        String::from("editor"),
                        String::from("Éditeur")
                    )]
                );
            }
        }
    }

    #[test]
    fn unique_snapshot_visits_still_reject_invalid_utf8() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE packages(attribute TEXT, exposed TEXT, pname TEXT, version TEXT, description TEXT, command TEXT, unavailable TEXT);
             INSERT INTO packages VALUES ('legacyPackages.x86_64-linux.editor', 'editor', CAST(X'ff' AS TEXT), '1.2', '', NULL, NULL);"
        ).unwrap();
        let snapshot = Snapshot {
            connection,
            header: Header {
                unique_exposed: true,
                ..header()
            },
        };
        assert!(snapshot.visit(|_| Ok(())).is_err());
    }

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

    #[test]
    fn partial_data_continues_only_for_the_same_source_revision() {
        let directory = tempfile::tempdir().unwrap();
        let staging = directory.path().join("snapshot.sqlite.partial");
        let partial = Header {
            complete: false,
            ..header()
        };
        let (connection, resumed) = open_partial(&staging, &partial).unwrap();
        assert!(!resumed);
        connection
            .execute(
                "INSERT INTO pending VALUES ('[\"legacyPackages\"]', 'legacyPackages')",
                [],
            )
            .unwrap();
        drop(connection);
        // A stop before the roots are stored starts again.
        let (connection, resumed) = open_partial(&staging, &partial).unwrap();
        assert!(!resumed);
        connection
            .execute(
                "INSERT INTO pending VALUES ('[\"legacyPackages\"]', 'legacyPackages')",
                [],
            )
            .unwrap();
        mark_resumable(&connection, &partial).unwrap();
        drop(connection);
        let (connection, resumed) = open_partial(&staging, &partial).unwrap();
        assert!(resumed);
        let pending: i64 = connection
            .query_row("SELECT count(*) FROM pending", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pending, 1);
        drop(connection);
        let moved = Header {
            revision: Some(String::from("newer-revision")),
            ..partial
        };
        let (connection, resumed) = open_partial(&staging, &moved).unwrap();
        assert!(!resumed);
        let pending: i64 = connection
            .query_row("SELECT count(*) FROM pending", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pending, 0);
        drop(connection);
        std::fs::write(&staging, "not a database").unwrap();
        assert!(!open_partial(&staging, &moved).unwrap().1);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a native Nix runtime"]
    fn stopped_preparations_continue_and_large_sets_finish_in_parallel() {
        let nix = crate::nix::discover(None).unwrap();
        let (_fixture, reference) = fixture(
            &nix,
            "{ legacyPackages.x86_64-linux = builtins.listToAttrs (builtins.genList (i: { name = \"p${toString i}\"; value = { type = \"derivation\"; name = \"p${toString i}-1.0\"; meta.description = \"package\"; }; }) 1300); }",
        );
        let cache_dir = tempfile::tempdir().unwrap();
        // A stopped preparation left one finished row and one pending path.
        let destination = path(cache_dir.path(), "fixture", SYSTEM);
        let partial = Header {
            reference: reference.clone(),
            complete: false,
            saved_unix: 0,
            ..header()
        };
        let (connection, _) = open_partial(&partial_path(&destination), &partial).unwrap();
        mark_resumable(&connection, &partial).unwrap();
        connection
            .execute_batch(&format!(
                "INSERT INTO packages VALUES ('legacyPackages.{SYSTEM}.earlier', 'earlier', 'earlier', '1', '', NULL, 'earlier', NULL);
                 INSERT INTO pending VALUES ('[\"legacyPackages\",\"{SYSTEM}\",\"p7\"]', 'legacyPackages');"
            ))
            .unwrap();
        drop(connection);
        let resumed = snapshot(&nix, cache_dir.path(), &reference).unwrap();
        let mut names = Vec::new();
        resumed
            .visit(|package| {
                names.push(package.exposed.to_string());
                Ok(())
            })
            .unwrap();
        assert_eq!(names, ["earlier", "p7"], "the roots do not run again");
        assert!(!partial_path(&destination).exists());

        let cache_dir = tempfile::tempdir().unwrap();
        let fresh = snapshot(&nix, cache_dir.path(), &reference).unwrap();
        assert_eq!(fresh.summary().unwrap().0, 1300);
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
            .visit(|_| {
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
            .visit(|_| {
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
