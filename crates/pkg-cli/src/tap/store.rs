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
//! checkout) is removed before the publication is returned. An update
//! exchanges `current` with the inner flake, so the exchanged-out old
//! generation is already sitting at its final immutable path the moment
//! the exchange commits — the returned publication owns that retained
//! state: `finish` clears this run's fetch scratch best effort, `undo`
//! rolls the publication back after a registry failure, and dropping an
//! unfinished publication performs no fallible rollback at all (the old
//! generation is conservatively retained with its scratch).

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
    /// The canonical source identity this store publishes for; a staged
    /// generation or provenance naming any other source is refused.
    source: String,
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
        Ok(Self {
            dir,
            source: source.to_string(),
            lock_file,
        })
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
            store: Self {
                dir,
                source: source.to_string(),
                lock_file,
            },
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
    /// needed and none exists. The returned generation is tied to THIS
    /// source: [`Self::publish`] refuses one staged by another source's
    /// store.
    pub fn new_staging(&self) -> Result<StagedGeneration, String> {
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
        Ok(StagedGeneration {
            guard: StagingGuard {
                path: staging.clone(),
                armed: true,
            },
            source: self.source.clone(),
        })
    }

    /// Validate and publish one staged generation as the active
    /// generation, settling the staging guard internally.
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
    ///   checkout) is removed before the publication is returned;
    /// * an update is one atomic directory exchange of the flake root with
    ///   `current`, after which the exchanged-out old generation already
    ///   sits at its final immutable path inside the kept staging parent,
    ///   which the returned publication owns.
    ///
    /// Nothing fallible runs after the committing rename or exchange; a
    /// failure at any step leaves the previous active generation intact
    /// and removes the failed staging directory. A staged generation or
    /// provenance belonging to another source is refused before any
    /// mutation.
    pub fn publish(
        &self,
        staged: StagedGeneration,
        provenance: &Provenance,
        index: &CatalogIndex,
    ) -> Result<Publication<'_>, String> {
        let StagedGeneration { guard, source } = staged;
        if source != self.source || guard.path.strip_prefix(self.generations()).is_err() {
            return Err(format!(
                "the staged generation for source `{source}` does not belong to \
                 the store of source `{}`; each source publishes only its own \
                 staged generations",
                self.source
            ));
        }
        if provenance.source != self.source {
            return Err(format!(
                "the provenance names source `{}` but this store publishes \
                 `{}`; refusing to publish another source's generation",
                provenance.source, self.source
            ));
        }
        let flake = guard.path.join("flake");
        let published = self.commit(&flake, provenance, index)?;
        match published {
            Published::First => {
                // The inner flake root was renamed to `current`; the armed
                // guard removes only the disposable staging parent (the
                // raw checkout and captures) before the publication is
                // returned.
                drop(guard);
                Ok(Publication {
                    store: self,
                    published,
                    staging: None,
                })
            }
            Published::Replaced { .. } => {
                // The staging parent now holds the preserved old generation
                // at its final immutable path; disarm the guard and hand
                // the retained state to the publication.
                let staging = guard.keep();
                Ok(Publication {
                    store: self,
                    published,
                    staging: Some(staging),
                })
            }
        }
    }

    /// The committing step shared by first and replacing publications.
    fn commit(
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
    fn undo_first_publication(&self) -> Result<(), String> {
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
    fn undo_replaced_publication(&self, saved_generation: &Path) -> Result<(), String> {
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

/// One staged generation of one source, owned by the staging protocol.
///
/// The path is the importer's output directory (the staging parent); the
/// inner guard removes that directory while it is armed. Publication
/// settles the guard internally: [`SourceStore::publish`] drops it after
/// a first publication (removing the disposable parent) and disarms it
/// after a replacing one (retaining the preserved old generation), so no
/// caller can settle a guard by hand.
pub struct StagedGeneration {
    guard: StagingGuard,
    source: String,
}

impl StagedGeneration {
    /// The importer output directory: the staging parent whose `flake`
    /// child is the generated flake root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.guard.path
    }

    /// The canonical source this generation was staged for.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Removes one staging directory when it was not published.
///
/// A successful first publication renames the inner flake root away to
/// `current`, so the dropped guard deletes only the disposable staging
/// parent (the raw checkout). After a successful exchange the staging
/// parent holds the preserved old generation at `flake/`, so the guard
/// is disarmed with [`StagingGuard::keep`] before the publication is
/// returned. An armed guard only ever removes data this run created.
struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    /// Disarm the guard and return the retained staging path: the parent
    /// no longer holds only this run's scratch data (it now holds the
    /// preserved old generation after an exchange), so the eventual drop
    /// must not remove it.
    fn keep(mut self) -> PathBuf {
        self.armed = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// One committed publication owning its retained and scratch state.
///
/// The publication owns everything the commit left behind: the preserved
/// old generation and this run's fetch scratch inside the staging parent
/// of a replacing publication. [`Publication::finish`] clears the scratch
/// best effort after the run fully succeeded; [`Publication::undo`]
/// rolls the publication back after a registry failure. Dropping an
/// unfinished publication performs NO fallible rollback: the preserved
/// old generation and the scratch are conservatively retained, so a
/// drop can never report a rollback it did not verify.
pub struct Publication<'store> {
    store: &'store SourceStore,
    published: Published,
    /// The retained staging parent of a replacing publication (the
    /// preserved old generation plus this run's fetch scratch); `None`
    /// for a first publication, whose parent was already removed.
    staging: Option<PathBuf>,
}

impl Publication<'_> {
    /// What this publication did.
    #[must_use]
    pub fn kind(&self) -> &Published {
        &self.published
    }

    /// Finish the publication: clear this run's fetch scratch best effort.
    ///
    /// After a successful publication (and its registry commit, where one
    /// exists) the staging parent of a replacing publication holds the
    /// preserved old generation at `preserved` and this run's fetch scratch
    /// (the raw checkout, the captures) beside it. Only the immediate
    /// scratch children are removed; the preserved old flake is never
    /// touched. Failures are returned as warnings for the caller to print
    /// and never change the command's success; nothing is cleaned up when
    /// the run failed or rolled back. Calling `finish` again is a no-op.
    pub fn finish(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(staging) = self.staging.take() else {
            return warnings;
        };
        let preserved = match &self.published {
            Published::Replaced { saved_generation } => saved_generation.clone(),
            // Structurally unreachable: a first publication carries no
            // retained staging parent. State it instead of guessing.
            Published::First => return warnings,
        };
        let Ok(entries) = std::fs::read_dir(&staging) else {
            warnings.push(format!(
                "cannot inspect the staging directory {} to clean up its fetch scratch",
                staging.display()
            ));
            return warnings;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == preserved {
                continue;
            }
            let removed = if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(error) = removed {
                warnings.push(format!(
                    "cannot remove the staging scratch at {}: {error}",
                    path.display()
                ));
            }
        }
        warnings
    }

    /// Undo the publication after a registry write failure.
    ///
    /// The publication borrows its originating store and lock, so undo
    /// cannot target another source or run after that lock is released. The
    /// registry state is untouched: the previous disabled state stays
    /// exactly as it was, including the re-add case. Undo errors are
    /// reported, never swallowed: the caller must know when the new
    /// generation may still be active instead of being told it was
    /// undone.
    pub fn undo(self) -> Result<(), String> {
        match self.published {
            Published::First => self.store.undo_first_publication(),
            Published::Replaced { saved_generation } => {
                self.store.undo_replaced_publication(&saved_generation)
            }
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
        let staged = store.new_staging().expect("stages");
        let staging = staged.path().to_path_buf();
        assert!(staging.starts_with(store.generations()));
        // The importer's flake root lives inside the staging parent; the
        // raw checkout and captures stay outside the published root.
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "first").expect("write");
        std::fs::create_dir(staging.join("captures")).expect("captures dir");
        let publication = store
            .publish(
                staged,
                &provenance("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a"),
                &index("0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9d1a"),
            )
            .expect("publishes");
        assert_eq!(publication.kind(), &Published::First);
        // The nested flake root became the active generation itself:
        // `current/flake.nix` exists and `current/flake/` does not.
        assert!(store.current().join("flake.nix").exists());
        assert!(!store.current().join("flake").exists());
        // The disposable staging parent was removed by publish itself:
        // the caller settles no guard by hand.
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
        publication.undo().expect("undoes");
        assert!(!store.is_published());
    }

    #[test]
    fn update_exchanges_and_preserves_the_old_generation() {
        let state = tempfile::tempdir().expect("tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let old_rev = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let new_rev = "1b56ceb53d693f3f3e0eaea0f9f4d5b8cf5b9b9d1b";
        {
            let staged = store.new_staging().expect("stages");
            let flake = staged.path().join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            std::fs::write(flake.join("flake.nix"), "old").expect("write");
            store
                .publish(staged, &provenance(old_rev), &index(old_rev))
                .expect("first");
        }
        let marker = store.current().join("flake.nix");

        let staged = store.new_staging().expect("stages");
        let staging = staged.path().to_path_buf();
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "new").expect("write");
        std::fs::create_dir(staging.join("captures")).expect("captures scratch");
        let mut publication = store
            .publish(staged, &provenance(new_rev), &index(new_rev))
            .expect("exchanges");
        // The publication owns the retained staging parent: the guard was
        // settled inside publish and no caller can mis-settle it.
        let Published::Replaced { saved_generation } = publication.kind() else {
            panic!("an update over an existing generation must exchange");
        };
        let saved_generation = saved_generation.clone();
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
        // publication's retained staging parent.
        assert!(staging.exists());
        assert_eq!(
            std::fs::read_to_string(saved_generation.join("flake.nix")).expect("read"),
            "old"
        );

        // Finishing the publication clears only the fetch scratch: the
        // preserved old generation and its parent stay untouched.
        assert!(publication.finish().is_empty());
        assert!(!staging.join("captures").exists());
        assert_eq!(
            std::fs::read_to_string(saved_generation.join("flake.nix")).expect("read"),
            "old"
        );

        // Undoing the replacement restores the old active generation; the
        // withdrawn new one is removed best effort (normally succeeds in
        // tests) or retained — never a claim of a failed restoration.
        publication.undo().expect("undoes");
        assert_eq!(std::fs::read_to_string(&marker).expect("read"), "old");
        assert_eq!(
            store.read_provenance().expect("provenance").revision,
            old_rev
        );
        assert!(!saved_generation.exists());
    }

    /// The ownership protocol itself: a staged generation bound to one
    /// source cannot be published through another source's store, a
    /// provenance naming another source is refused before any mutation,
    /// and dropping an unfinished publication retains the old generation
    /// instead of attempting a fallible rollback from `Drop`.
    #[test]
    fn undo_targets_its_locked_store_and_keeps_other_roots_intact() {
        let state = tempfile::tempdir().expect("tempdir");
        let other_state = tempfile::tempdir().expect("other tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let other = SourceStore::lock(other_state.path(), "somebody/apps").expect("other locks");
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        let unrelated = other.new_staging().expect("other staging");
        std::fs::create_dir(unrelated.path().join("flake")).expect("other flake");
        std::fs::write(unrelated.path().join("flake/flake.nix"), "unrelated")
            .expect("other content");
        other
            .publish(unrelated, &provenance(revision), &index(revision))
            .expect("other publication")
            .finish();
        for (content, undo) in [("first", true), ("old", false), ("new", true)] {
            let staged = store.new_staging().expect("staging");
            std::fs::create_dir(staged.path().join("flake")).expect("flake");
            std::fs::write(staged.path().join("flake/flake.nix"), content).expect("content");
            let mut publication = store
                .publish(staged, &provenance(revision), &index(revision))
                .expect("publication");
            if undo {
                publication.undo().expect("undo bound to origin");
            } else {
                assert!(publication.finish().is_empty());
            }
            assert_eq!(
                std::fs::read_to_string(other.current().join("flake.nix")).expect("other content"),
                "unrelated"
            );
            if content == "first" {
                assert!(!store.is_published());
            } else {
                assert_eq!(
                    std::fs::read_to_string(store.current().join("flake.nix"))
                        .expect("restored content"),
                    "old"
                );
            }
        }
    }

    #[test]
    fn ownership_refuses_foreign_generations_and_drop_retains() {
        let state = tempfile::tempdir().expect("tempdir");
        let apps = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9d1a";

        // A staged generation of another source's store is refused, with
        // both its own identity and the store's named.
        let cli = SourceStore::lock(state.path(), "other/cli").expect("locks");
        let foreign = cli.new_staging().expect("stages");
        let flake = foreign.path().join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "foreign").expect("write");
        let Err(error) = apps.publish(foreign, &provenance(revision), &index(revision)) else {
            panic!("a foreign staged generation is refused");
        };
        assert!(error.contains("other/cli"), "{error}");
        assert!(error.contains("somebody/apps"), "{error}");
        assert!(!apps.is_published(), "no mutation happened");
        assert!(!apps.current().exists());

        // A provenance naming another source is refused the same way,
        // even for this store's own staged generation.
        let staged = apps.new_staging().expect("stages");
        let flake = staged.path().join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        let mut foreign_provenance = provenance(revision);
        foreign_provenance.source = String::from("other/cli");
        let Err(error) = apps.publish(staged, &foreign_provenance, &index(revision)) else {
            panic!("a foreign provenance is refused");
        };
        assert!(error.contains("other/cli"), "{error}");
        assert!(!apps.is_published(), "no mutation happened");

        // Publish a first and then a replacing generation; drop the
        // publication without finish or undo: the old generation must be
        // retained — Drop performs no fallible rollback — and this run's
        // scratch stays beside it.
        for content in ["old", "new"] {
            let staged = apps.new_staging().expect("stages");
            let flake = staged.path().join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            std::fs::write(flake.join("flake.nix"), content).expect("write");
            std::fs::create_dir(staged.path().join("captures")).expect("captures");
            let publication = apps
                .publish(staged, &provenance(revision), &index(revision))
                .expect("publishes");
            if content == "new" {
                let Published::Replaced { saved_generation } = publication.kind() else {
                    panic!("the second publication must replace");
                };
                let saved_generation = saved_generation.clone();
                drop(publication);
                assert!(
                    saved_generation.join("flake.nix").exists(),
                    "an unfinished publication conservatively retains the old generation"
                );
                assert_eq!(
                    std::fs::read_to_string(apps.current().join("flake.nix")).expect("read"),
                    "new"
                );
            }
        }
    }

    #[test]
    fn publication_failure_and_old_corruption_leave_the_active_generation() {
        let state = tempfile::tempdir().expect("tempdir");
        let store = SourceStore::lock(state.path(), "somebody/apps").expect("locks");
        let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
        {
            let staged = store.new_staging().expect("stages");
            let flake = staged.path().join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            store
                .publish(staged, &provenance(revision), &index(revision))
                .expect("first");
        }
        // A corrupted old provenance must not block publication: nothing
        // reads the old generation during a replacement.
        std::fs::write(store.current().join(PROVENANCE_FILE), "{ broken").expect("corrupt");
        let staged = store.new_staging().expect("stages");
        let staging = staged.path().to_path_buf();
        let flake = staging.join("flake");
        std::fs::create_dir(&flake).expect("flake dir");
        std::fs::write(flake.join("flake.nix"), "new").expect("write");
        // Make the capture write fail without blocking the cleanup that
        // the failed publish itself performs: the provenance target
        // inside the flake root is a directory, so `File::create` on it
        // fails. (The old chmod-0o500 injection needed a hand-rolled
        // restore before the caller-dropped guard could clean up, and it
        // could not fail at all under root.)
        let new_rev = "1b56ceb53d693f3f3e0eaea0f9f4d5b8cf5b9b9d1b";
        std::fs::create_dir(flake.join(PROVENANCE_FILE)).expect("blocking dir");
        let failed = store.publish(staged, &provenance(new_rev), &index(new_rev));
        assert!(
            failed.is_err(),
            "publication must fail on an unwritable provenance capture"
        );
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
            let staged = store.new_staging().expect("stages");
            let flake = staged.path().join("flake");
            std::fs::create_dir(&flake).expect("flake dir");
            let revision = "0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a";
            store
                .publish(staged, &provenance(revision), &index(revision))
                .expect("publishes");
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
