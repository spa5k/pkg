//! The durable per-source tap store (state home, never the cache).
//!
//! Layout under the state home's tap namespace (`pkg/taps`), all real
//! directories:
//!
//! ```text
//! pkg/taps/src/{owner}/{tap}/current/     the active generation (REAL dir)
//! pkg/taps/src/{owner}/{tap}/generations/ immutable former generations
//! pkg/taps/src/{owner}/{tap}/lock         the per-source publication lock
//! ```
//!
//! `current` is the stable reference installs and upgrades name
//! (`path:...current`), so a native profile upgrade follows the tap
//! forward while a rollback keeps the previously locked outputs. Because
//! Nix rejects `path:` roots that are symlinks, `current` is a real
//! directory and an update replaces it with one atomic directory exchange
//! (`renameat2(RENAME_EXCHANGE)` / `renameatx_np(RENAME_SWAP)`). The
//! registry records only origin and consent; the revision and provenance
//! live inside the generation, so one atomic exchange commits the entire
//! active generation without a registry race.
//!
//! Ownership rule of publication: only the importer-generated flake root
//! becomes `current`; the untrusted checkout and captures stay outside the
//! active Nix flake root. Staging directories are random directories
//! inside `generations/` (same filesystem) whose inner `flake/` directory
//! is the published root. A first publication renames that inner flake
//! into `current` and the disposable staging parent (holding the raw
//! checkout) is deleted by the guard. An update exchanges `current` with
//! the inner flake, so the exchanged-out old generation is already sitting
//! at its final immutable path the moment the exchange commits — the
//! caller must keep the staging guard immediately after a successful
//! exchange, and there is deliberately no fallible operation after it.

use std::path::{Path, PathBuf};

use crate::nix::{CatalogIndex, OFFICIAL_CASK_SOURCE};

use super::atomic;
use super::registry::taps_root;
use super::strict;

/// The active generation directory name.
const CURRENT: &str = "current";
/// The immutable generations directory name.
const GENERATIONS: &str = "generations";
/// The captured index envelope inside every generation.
const INDEX_FILE: &str = "index.json";
/// The provenance record inside every generation.
const PROVENANCE_FILE: &str = "provenance.json";

/// The store root of every source under the state home.
#[must_use]
pub fn store_root(state_home: &Path) -> PathBuf {
    taps_root(state_home).join("src")
}

/// Validate one source identity for filesystem use and split it.
///
/// Only a strict `owner/tap` slug — never the reserved official source —
/// may become a filesystem path, so the helpers below can never be path
/// traversal gadgets.
fn checked_source(source: &str) -> Result<(String, String), String> {
    if source == OFFICIAL_CASK_SOURCE {
        return Err(format!(
            "{OFFICIAL_CASK_SOURCE} is the reserved official source; \
             it cannot be a local tap"
        ));
    }
    if !crate::nix::valid_catalog_source(source) {
        return Err(format!("`{source}` is not a valid owner/tap source"));
    }
    let (owner, tap) = source
        .split_once('/')
        .expect("a validated source is exactly owner/tap");
    Ok((owner.to_string(), tap.to_string()))
}

/// The per-source directory of one canonical `owner/tap` source.
///
/// Returns `None` for anything that is not a strict nonofficial source,
/// so invalid input can never become a filesystem join.
#[must_use]
pub fn source_dir(state_home: &Path, source: &str) -> Option<PathBuf> {
    let (owner, tap) = checked_source(source).ok()?;
    Some(store_root(state_home).join(owner).join(tap))
}

/// The stable moving reference installs use for one source.
///
/// This is the reference recorded as the profile entry's original URL, so
/// a native `profile upgrade` re-evaluates exactly this path and a
/// rollback keeps the previously locked generation. The path text is
/// percent-encoded: a state home with spaces, `#`, `?`, `%`, or non-ASCII
/// bytes still produces exactly one reference text Nix records verbatim.
pub fn current_ref(state_home: &Path, source: &str) -> Result<String, String> {
    let (owner, tap) = checked_source(source)?;
    let path = store_root(state_home).join(owner).join(tap).join(CURRENT);
    Ok(format!("path:{}", crate::nix::encode_path_reference(&path)))
}

/// Whether a reference is this store's stable reference for some source,
/// returning the source identity when it is.
///
/// The reference must name exactly `pkg/taps/src/{owner}/{tap}/current`
/// below this state home. The percent-encoded reference body is decoded
/// once, safely, and matching is done on path components, never on string
/// prefixes, so a different but similarly-named root cannot match and a
/// malformed escape is never guessed at.
#[must_use]
pub fn source_of_reference(state_home: &Path, reference: &str) -> Option<String> {
    let encoded = reference.strip_prefix("path:")?;
    let path = crate::nix::decode_path_reference(encoded)?;
    let relative = path.strip_prefix(store_root(state_home)).ok()?;
    let mut components = relative.components();
    let owner = components.next()?.as_os_str().to_str()?;
    let tap = components.next()?.as_os_str().to_str()?;
    if components.next()?.as_os_str() != CURRENT {
        return None;
    }
    if components.next().is_some() {
        return None;
    }
    let source = format!("{owner}/{tap}");
    crate::nix::valid_catalog_source(&source).then_some(source)
}

/// The provenance recorded inside one published generation.
///
/// One shape serves the wire and the code: the record is serialized and
/// strict-decoded through these fields directly, with unknown fields
/// denied on load.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The canonical `owner/tap` source identity.
    pub source: String,
    /// The approved canonical repository URL.
    pub origin: String,
    /// The exact imported revision.
    pub revision: String,
    /// The importer's captured metadata provenance hash.
    pub metadata_sha256: String,
    /// The number of eligible entries in the published catalog.
    pub eligible: usize,
    /// The number of excluded entries in the published catalog.
    pub excluded: usize,
    /// When this generation was published, as Unix seconds.
    pub published_unix: u64,
}

/// What one publication did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    /// The first generation became `current` by one plain rename.
    First,
    /// The previous generation was exchanged out and preserved.
    Replaced {
        /// The immutable directory that now holds the former generation.
        saved_generation: PathBuf,
    },
}

/// One locked handle on a source's publication state.
///
/// All publication mutations for one source happen under one exclusive
/// `flock` on the source's lock file, held until the handle drops.
pub struct SourceStore {
    dir: PathBuf,
    /// The held lock file; dropping the store releases the lock, and the
    /// handle is never read otherwise.
    #[allow(dead_code, reason = "the lock is held until the store drops")]
    lock_file: std::fs::File,
}

/// One shared, read-only lock on a source's publication state.
///
/// Queries load provenance and index under this lock so no exchange can
/// happen between the two reads. Taking it never creates or mutates any
/// file: a source without a lock file was never published.
pub struct SourceReader {
    store: SourceStore,
}

impl SourceStore {
    /// Create the source directory tree and take the exclusive
    /// publication lock.
    pub fn lock(state_home: &Path, source: &str) -> Result<Self, String> {
        let (owner, tap) = checked_source(source)?;
        let dir = store_root(state_home).join(owner).join(tap);
        std::fs::create_dir_all(dir.join(GENERATIONS))
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let lock_path = dir.join("lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&lock_path)
            .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
        lock_file
            .lock()
            .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;
        Ok(Self { dir, lock_file })
    }

    /// Take a shared lock on an already-published source, read-only.
    ///
    /// No file or directory is created: a missing lock file means the
    /// source was never published under this store.
    pub fn open_readonly(state_home: &Path, source: &str) -> Result<SourceReader, String> {
        let (owner, tap) = checked_source(source)?;
        let dir = store_root(state_home).join(owner).join(tap);
        let lock_path = dir.join("lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .open(&lock_path)
            .map_err(|_| {
                format!(
                    "tap source `{source}` is registered but has no published \
                     generation; run `pkg tap update {source}` to publish one"
                )
            })?;
        lock_file
            .lock_shared()
            .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;
        Ok(SourceReader {
            store: Self { dir, lock_file },
        })
    }

    /// The active generation directory.
    #[must_use]
    pub fn current(&self) -> PathBuf {
        self.dir.join(CURRENT)
    }

    /// Whether an active generation exists and is a real directory.
    #[must_use]
    pub fn is_published(&self) -> bool {
        std::fs::symlink_metadata(self.current()).is_ok_and(|metadata| metadata.is_dir())
    }

    /// The immutable generations directory.
    #[must_use]
    pub fn generations(&self) -> PathBuf {
        self.dir.join(GENERATIONS)
    }

    /// Stage a new generation: a fresh random directory under
    /// `generations/`, on the same filesystem as `current`.
    ///
    /// The caller hands this directory to the importer as its output root;
    /// the importer's generated flake root lives inside it (the seam
    /// promises `output_dir/flake`). Because staging already lives at a
    /// final path under `generations/`, an update's atomic exchange of the
    /// inner flake with `current` leaves the exchanged-out old generation
    /// at its already-final immutable path: no post-commit rename is
    /// needed and none exists.
    pub fn new_staging(&self) -> Result<(PathBuf, StagingGuard), String> {
        let staging = tempfile::Builder::new()
            .prefix("incoming-")
            .tempdir_in(self.generations())
            .map_err(|error| {
                format!(
                    "cannot create staging under {}: {error}",
                    self.generations().display()
                )
            })?
            .keep();
        Ok((
            staging.clone(),
            StagingGuard {
                path: staging,
                armed: true,
            },
        ))
    }

    /// Validate and publish one staged flake root as the active generation.
    ///
    /// Only the importer-generated flake root (`staging/flake`) becomes
    /// `current`: the raw checkout and capture files stay outside the
    /// active Nix flake root. Validation runs first and fails closed: the
    /// provenance and the strictly decodable index envelope are written
    /// into the flake root, and a real-directory check guards it before
    /// any rename. Then:
    ///
    /// * a first publication is one plain rename of the flake root into
    ///   `current`; the disposable staging parent (still holding the raw
    ///   checkout) is deleted by the dropped guard;
    /// * an update is one atomic directory exchange of the flake root with
    ///   `current`, after which the exchanged-out old generation already
    ///   sits at its final immutable path inside the staging parent — the
    ///   caller must keep the staging guard immediately so drop never
    ///   deletes it.
    ///
    /// Nothing fallible runs after the committing rename or exchange; a
    /// failure at any step leaves the previous active generation intact.
    pub fn publish(
        &self,
        flake: &Path,
        provenance: &Provenance,
        index: &CatalogIndex,
    ) -> Result<Published, String> {
        atomic::verify_directory(flake)?;
        write_synced(&flake.join(PROVENANCE_FILE), "provenance", provenance)?;
        write_synced(&flake.join(INDEX_FILE), "the index capture", index)?;

        let current = self.current();
        if !current.exists() {
            // First publication: one plain rename commits the generation.
            std::fs::rename(flake, &current).map_err(|error| {
                format!(
                    "cannot publish the first generation at {}: {error}",
                    current.display()
                )
            })?;
            atomic::fsync_directory(&self.dir);
            return Ok(Published::First);
        }
        // Prevalidate the old active state before touching it: it must be
        // a real directory (never a symlink), and so must the flake root.
        atomic::exchange_directories(flake, &current)?;
        atomic::fsync_directory(&self.dir);
        // The exchange committed the new generation at `current`; the old
        // generation is now at the flake root, its already-final path
        // inside the kept staging parent.
        Ok(Published::Replaced {
            saved_generation: flake.to_path_buf(),
        })
    }

    /// Undo a first publication whose registry write failed.
    ///
    /// This is only correct immediately after a `Published::First` under
    /// the same store lock, before any install could name the reference.
    /// The registry state is untouched: the previous disabled state stays
    /// exactly as it was, including the re-add case. Removal errors are
    /// reported, never swallowed: the caller must know when the new
    /// generation may still be active instead of being told it was undone.
    pub fn undo_first_publication(&self) -> Result<(), String> {
        let current = self.current();
        if std::fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.is_dir())
            && let Err(error) = std::fs::remove_dir_all(&current)
        {
            return Err(format!(
                "removing the first generation at {} failed: {error}; the new \
                 generation may still be active",
                current.display()
            ));
        }
        atomic::fsync_directory(&self.dir);
        Ok(())
    }

    /// Undo a replacing publication whose registry write failed.
    ///
    /// One exchange puts the former active generation back at `current`;
    /// the withdrawn new generation (at `saved_generation` after the
    /// first exchange) is then removed best effort and otherwise simply
    /// retained. No fallible cleanup runs after the committing exchange:
    /// a successful return states exactly one fact — the previous
    /// generation is active again — and never promises the withdrawn one
    /// was removed. Every failure of the exchange itself reports the exact
    /// known state: the old generation is still preserved at
    /// `saved_generation` and the new one is still active.
    pub fn undo_replaced_publication(&self, saved_generation: &Path) -> Result<(), String> {
        atomic::exchange_directories(saved_generation, &self.current())?;
        // Best effort only: a failed removal retains the withdrawn
        // generation; it must never turn a completed restore into an
        // error that claims the restoration failed.
        let _ = std::fs::remove_dir_all(saved_generation);
        atomic::fsync_directory(&self.dir);
        Ok(())
    }

    /// Read the active generation's provenance.
    pub fn read_provenance(&self) -> Result<Provenance, String> {
        read_provenance_at(&self.current())
    }

    /// Read and strictly decode the active generation's index capture.
    pub fn read_index(&self) -> Result<CatalogIndex, String> {
        read_index_at(&self.current())
    }
}

/// Remove the active generation of one source under its publication lock.
///
/// Registry removal disables the source; removing `current` then blocks
/// native upgrades that would follow the tap. Installed packages, native
/// profile generations, and the immutable tap generations are kept, so
/// rollback keeps working. Creates nothing: a source with no directory is
/// already without an active generation.
pub fn remove_current(state_home: &Path, source: &str) -> Result<(), String> {
    let Some(dir) = source_dir(state_home, source) else {
        return Ok(());
    };
    if !dir.exists() {
        return Ok(());
    }
    let store = SourceStore::lock(state_home, source)?;
    store.undo_first_publication()
}

impl SourceReader {
    /// Whether an active generation exists.
    #[must_use]
    pub fn is_published(&self) -> bool {
        self.store.is_published()
    }

    /// Read the active generation's provenance.
    pub fn read_provenance(&self) -> Result<Provenance, String> {
        self.store.read_provenance()
    }

    /// Read and strictly decode the active generation's index capture.
    pub fn read_index(&self) -> Result<CatalogIndex, String> {
        self.store.read_index()
    }
}

/// Serialize one record and write it, flushed and fsynced, to `path`.
///
/// A write or sync failure is an error, so publication aborts before any
/// committing rename or exchange and the active generation stays unchanged.
/// Only these saved records are synced; full crash durability of every
/// generated asset is not claimed.
fn write_synced<T: serde::Serialize>(path: &Path, what: &str, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("cannot encode {what}: {error}"))?;
    let write = || {
        use std::io::Write as _;
        let mut file = std::fs::File::create(path)?;
        file.write_all(format!("{json}\n").as_bytes())?;
        file.sync_all()
    };
    write().map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Read the provenance recorded in one generation directory.
fn read_provenance_at(generation: &Path) -> Result<Provenance, String> {
    let path = generation.join(PROVENANCE_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    strict::from_str::<Provenance>(&format!("tap provenance {}", path.display()), &text)
}

/// Read and strictly decode one generation's index capture.
fn read_index_at(generation: &Path) -> Result<CatalogIndex, String> {
    let path = generation.join(INDEX_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let raw = strict::from_str::<serde_json::Value>(
        &format!("tap catalog capture {}", path.display()),
        &text,
    )?;
    crate::nix::decode_catalog_index(&raw).map_err(|error| {
        format!(
            "tap catalog capture {} is malformed: {error}",
            path.display()
        )
    })
}

/// Removes one staging directory when it was not published.
///
/// A successful first publication renames the inner flake root away to
/// `current`, so the dropped guard deletes only the disposable staging
/// parent (the raw checkout). After a successful exchange the staging
/// parent holds the preserved old generation at `flake/`, so the caller
/// must disarm the guard with [`StagingGuard::keep`] immediately after
/// the commit — before any fallible registry work — or drop would delete
/// the preserved generation. An armed guard only ever removes data this
/// run created.
pub struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    /// Disarm the guard: the staging parent no longer holds only this
    /// run's scratch data (the inner flake was renamed to `current`, or
    /// now holds the preserved old generation after an exchange), so drop
    /// must not remove it.
    pub fn keep(mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(revision: &str) -> Provenance {
        Provenance {
            source: String::from("somebody/apps"),
            origin: String::from("https://github.com/somebody/homebrew-apps"),
            revision: String::from(revision),
            metadata_sha256: String::from(
                "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            ),
            eligible: 2,
            excluded: 1,
            published_unix: 1_800_000_000,
        }
    }

    /// A minimal valid schema-3 index envelope for one source.
    fn index(revision: &str) -> CatalogIndex {
        let raw = serde_json::json!({
            "schema": crate::nix::CATALOG_INDEX_SCHEMA,
            "generator": {"name": "cask-catalog", "version": "0.1.0"},
            "inputs": {
                "somebody/apps": {
                    "url": "https://github.com/somebody/homebrew-apps",
                    "revision": revision,
                    "sha256": "1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
                    "license": "Homebrew data license"}
            },
            "targets": ["x86_64-linux"],
            "macosBaseline": "15.7.7",
            "systems": {"x86_64-linux": {"entries": {
                "somebody/apps/tool": {
                    "source": "somebody/apps", "token": "tool", "name": "Tool",
                    "description": null, "version": "1.0", "homepage": null,
                    "status": "eligible", "kind": "binary", "reason": null, "detail": null}}}}
        });
        crate::nix::decode_catalog_index(&raw).expect("decodes")
    }

    #[test]
    fn first_publication_renames_and_undo_removes_it() {
        let state = tempfile::tempdir().expect("tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        assert!(!store.is_published());
        let (staging, guard) = store.new_staging().expect("stages");
        assert!(staging.starts_with(store.generations()));
        // The importer's flake root lives inside the staging parent; the
        // raw checkout and captures stay outside the published root.
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "first").expect("write");
        std::fs::create_dir(staging.join("captures")).expect("captures dir");
        let published = store
            .publish(
                &flake,
                &provenance("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"),
                &index("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9d1a"),
            )
            .expect("publishes");
        assert_eq!(published, Published::First);
        // The nested flake root became the active generation itself:
        // `current/flake.nix` exists and `current/flake/` does not.
        assert!(store.current().join("flake.nix").exists());
        assert!(!store.current().join("flake").exists());
        // The disposable staging parent is deleted by the dropped guard;
        // the published generation is complete on its own.
        drop(guard);
        assert!(!staging.exists());
        assert!(store.is_published());
        assert_eq!(
            store.read_provenance().expect("provenance").revision,
            "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"
        );
        assert!(
            store
                .read_index()
                .expect("index")
                .targets
                .contains(&String::from("x86_64-linux"))
        );
        // Only the flake root became the active generation.
        assert!(!store.current().join("captures").exists());

        // Undo removes the first publication and nothing else.
        store.undo_first_publication().expect("undoes");
        assert!(!store.is_published());
    }

    #[test]
    fn update_exchanges_and_preserves_the_old_generation() {
        let state = tempfile::tempdir().expect("tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let old_rev = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let new_rev = "1b56ceb53d693f3f3e0eaea0f9f4d5b8cf5b9b9d1b";
        {
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            std::fs::write(flake.join("flake.nix"), "old").expect("write");
            store
                .publish(&flake, &provenance(old_rev), &index(old_rev))
                .expect("first");
            drop(guard);
        }
        let marker = store.current().join("flake.nix");

        let (staging, guard) = store.new_staging().expect("stages");
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "new").expect("write");
        let published = store
            .publish(&flake, &provenance(new_rev), &index(new_rev))
            .expect("exchanges");
        // The guard must be kept immediately: the staging parent now
        // holds the preserved old generation at its inner flake root.
        guard.keep();
        let Published::Replaced { saved_generation } = published else {
            panic!("an update over an existing generation must exchange");
        };
        // The old generation stays at the exchanged-out flake root (already
        // under generations/) exactly where the exchange left it.
        assert_eq!(saved_generation, flake);
        assert!(saved_generation.starts_with(store.generations()));
        // The active generation is entirely new: content and provenance.
        assert_eq!(std::fs::read_to_string(&marker).expect("read"), "new");
        assert_eq!(
            store.read_provenance().expect("provenance").revision,
            new_rev
        );
        // The old generation survives, complete and immutable, inside the
        // kept staging parent.
        assert!(staging.exists());
        assert_eq!(
            std::fs::read_to_string(saved_generation.join("flake.nix")).expect("read"),
            "old"
        );

        // Undoing the replacement restores the old active generation; the
        // withdrawn new one is removed best effort (normally succeeds in
        // tests) or retained — never a claim of a failed restoration.
        store
            .undo_replaced_publication(&saved_generation)
            .expect("undoes");
        assert_eq!(std::fs::read_to_string(&marker).expect("read"), "old");
        assert_eq!(
            store.read_provenance().expect("provenance").revision,
            old_rev
        );
        assert!(!saved_generation.exists());
    }

    #[test]
    fn publication_failure_and_old_corruption_leave_the_active_generation() {
        let state = tempfile::tempdir().expect("tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        {
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            store
                .publish(&flake, &provenance(revision), &index(revision))
                .expect("first");
            drop(guard);
        }
        // A corrupted old provenance must not block publication: nothing
        // reads the old generation during a replacement.
        std::fs::write(store.current().join(PROVENANCE_FILE), "{ broken").expect("corrupt");
        let (staging, guard) = store.new_staging().expect("stages");
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "new").expect("write");
        // The staging directory was armed; a failed publication removes it.
        let new_rev = "1b56ceb53d693f3f3e0eaea0f9f4d5b8cf5b9b9d1b";
        // Make the capture write fail: the flake root becomes unwritable.
        // As root the mode bit does not block writes, so the injection is
        // skipped.
        let root = unsafe { libc::getuid() } == 0;
        #[cfg(unix)]
        if !root {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&flake).expect("metadata").permissions();
            permissions.set_mode(0o500);
            std::fs::set_permissions(&flake, permissions).expect("chmod");
        }
        let failed = store.publish(&flake, &provenance(new_rev), &index(new_rev));
        if !root {
            assert!(
                failed.is_err(),
                "publication must fail on an unwritable staging directory"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&flake).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&flake, permissions).expect("chmod");
        }
        drop(guard);
        assert!(!staging.exists(), "the failed staging directory is removed");
        // The active generation is untouched, still the old one.
        assert!(store.is_published());
        assert_eq!(
            std::fs::read_to_string(store.current().join(PROVENANCE_FILE)).expect("read"),
            "{ broken"
        );
    }

    #[test]
    fn stable_references_round_trip_and_official_is_refused() {
        let state = tempfile::tempdir().expect("tempdir");
        let reference = current_ref(state.path(), "somebody/apps").expect("ref");
        assert_eq!(
            reference,
            format!(
                "path:{}/pkg/taps/src/somebody/apps/current",
                state.path().display()
            )
        );
        assert_eq!(
            source_of_reference(state.path(), &reference),
            Some(String::from("somebody/apps"))
        );
        // A different root that merely string-contains this root's tail
        // must not match: matching is on components below this root.
        let other_root = state.path().join("elsewhere");
        assert_eq!(source_of_reference(&other_root, &reference), None);
        // Only this store's exact stable references map back to a source.
        assert_eq!(source_of_reference(state.path(), "path:/etc/nix"), None);
        assert_eq!(
            source_of_reference(
                state.path(),
                &format!("path:{}/pkg/taps/src/somebody/apps", state.path().display())
            ),
            None
        );
        // A reference into a different root that merely string-contains
        // this root's tail is decoded once and matched on components only.
        let elsewhere = state
            .path()
            .join("elsewhere")
            .join("pkg")
            .join("taps")
            .join("src");
        assert_eq!(
            source_of_reference(
                state.path(),
                &format!(
                    "path:{}",
                    crate::nix::encode_path_reference(
                        &elsewhere.join("somebody").join("apps").join("current")
                    )
                )
            ),
            None
        );
        // Invalid sources never become paths.
        assert!(current_ref(state.path(), OFFICIAL_CASK_SOURCE).is_err());
        assert!(current_ref(state.path(), "../escape").is_err());
        assert!(current_ref(state.path(), "somebody/apps/extra").is_err());
        assert_eq!(source_dir(state.path(), "../escape"), None);
        let error = SourceStore::lock(state.path(), OFFICIAL_CASK_SOURCE)
            .err()
            .expect("the official source is reserved");
        assert!(error.contains("reserved"), "{error}");
        assert!(SourceStore::lock(state.path(), "some body/apps").is_err());
    }

    #[test]
    fn stable_references_survive_paths_that_need_encoding() {
        // A state home with a space, `#`, `?`, `%`, and non-ASCII bytes
        // still produces exactly one reference text, and the source maps
        // back off the decoded components.
        let state = tempfile::tempdir().expect("tempdir");
        let base = state.path().join("pkg we#ird?x%").join("unicode-dir");
        let reference = current_ref(&base, "somebody/apps").expect("ref");
        assert!(reference.contains("pkg%20we%23ird%3Fx%25"), "{reference}");
        assert!(reference.contains("unicode-dir"), "{reference}");
        assert_eq!(
            source_of_reference(&base, &reference),
            Some(String::from("somebody/apps"))
        );
        // The same text below a different root does not match, and a
        // malformed escape is refused instead of guessed at.
        assert_eq!(source_of_reference(state.path(), &reference), None);
        assert_eq!(source_of_reference(&base, "path:%zz"), None);
        // An unencoded space in a raw reference is decoded literally and
        // still matches, because decoding is uniform.
        let raw = reference.replace("%20", " ");
        assert_eq!(
            source_of_reference(&base, &raw),
            Some(String::from("somebody/apps"))
        );
    }

    #[test]
    #[cfg(unix)]
    fn source_of_reference_detects_non_utf8_state_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        // A state home whose path holds arbitrary non-UTF-8 bytes still
        // yields a stable reference that maps back to its source.
        let state = tempfile::tempdir().expect("tempdir");
        let base = state.path().join(OsString::from_vec(b"st\xffate".to_vec()));
        let reference = current_ref(&base, "somebody/apps").expect("ref");
        assert!(reference.contains("st%FFate"), "{reference}");
        assert_eq!(
            source_of_reference(&base, &reference),
            Some(String::from("somebody/apps"))
        );
        // A malformed escape is still refused.
        assert_eq!(source_of_reference(&base, "path:%zz"), None);
    }

    #[test]
    fn readonly_handles_create_nothing_and_fail_when_unpublished() {
        let state = tempfile::tempdir().expect("tempdir");
        let error = SourceStore::open_readonly(state.path(), "somebody/apps")
            .err()
            .expect("an unpublished source cannot be read");
        assert!(error.contains("somebody/apps"), "{error}");
        assert!(
            !state.path().join("pkg").join("taps").exists(),
            "a read-only open creates no directories"
        );
        {
            let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
            let (staging, guard) = store.new_staging().expect("stages");
            let flake = staging.join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
            store
                .publish(&flake, &provenance(revision), &index(revision))
                .expect("publishes");
            drop(guard);
            // The exclusive handle is dropped before the shared read.
        }
        let reader = SourceStore::open_readonly(state.path(), "somebody/apps").expect("reads");
        assert!(reader.is_published());
        assert_eq!(
            reader.read_provenance().expect("provenance").source,
            "somebody/apps"
        );
        assert!(
            reader
                .read_index()
                .expect("index")
                .inputs
                .contains_key("somebody/apps")
        );
    }
}
