//! The tap registry: origin and consent per source (design: local taps).
//!
//! The registry records only what approval needs: the canonical origin the
//! user approved and when it was added. Revisions and import provenance
//! live inside each published generation, so publishing never races the
//! registry. The registry is untrusted persisted input: it loads strictly
//! and a malformed file fails closed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::strict;

/// The registry schema this client reads and writes.
pub const REGISTRY_SCHEMA: &str = "pkg-tap-registry/1";

/// The durable tap namespace root under the state home: `<state>/pkg/taps`.
///
/// Everything tap-related lives here — the registry file, the per-source
/// store, and the global registry lock — so the state home root stays
/// reserved for the native profile layout.
#[must_use]
pub fn taps_root(state_home: &Path) -> PathBuf {
    state_home.join("pkg").join("taps")
}

/// The durable registry file under the state home.
#[must_use]
pub fn registry_path(state_home: &Path) -> PathBuf {
    taps_root(state_home).join("registry.json")
}

/// One registered source: the approved origin and its consent record.
///
/// Presence of the entry is the consent; the origin is the identity the
/// user approved. A change of canonical origin must not reuse this entry.
/// This is also the persisted wire shape; unknown fields are denied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    /// The approved canonical repository URL,
    /// for example `https://github.com/somebody/homebrew-apps`.
    pub origin: String,
    /// When the source was approved, as Unix seconds.
    pub added_unix: u64,
}

/// The decoded registry; the same serde-derived shape is its wire form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// The schema this file claims; written as [`REGISTRY_SCHEMA`] and
    /// checked on load. Filled by [`Registry::empty`] and never blank.
    schema: String,
    /// One entry per approved source, keyed by `owner/tap`.
    pub sources: BTreeMap<String, RegistryEntry>,
}

impl Registry {
    /// The empty registry, used when no file exists yet.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema: REGISTRY_SCHEMA.to_string(),
            sources: BTreeMap::new(),
        }
    }

    /// Load the registry strictly; a malformed file is a hard error.
    ///
    /// A missing file is the empty registry: a fresh host has no taps.
    /// Everything else — wrong schema, duplicate keys, denied fields, a
    /// source that is not exactly its canonical form, an origin that is
    /// not the canonical origin of that source, or the reserved official
    /// source — fails closed with the reason.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(error) => {
                return Err(format!(
                    "cannot read tap registry {}: {error}",
                    path.display()
                ));
            }
        };
        let registry =
            strict::from_str::<Registry>(&format!("tap registry {}", path.display()), &text)?;
        if registry.schema != REGISTRY_SCHEMA {
            return Err(format!(
                "tap registry {} reports schema {:?}; \
                 schema {REGISTRY_SCHEMA:?} is required",
                path.display(),
                registry.schema
            ));
        }
        for (source, entry) in &registry.sources {
            if source == crate::nix::OFFICIAL_CASK_SOURCE {
                return Err(format!(
                    "tap registry {} holds the reserved official source \
                     {source}; it is built in and cannot be a tap",
                    path.display()
                ));
            }
            if cask_catalog::tap::canonicalize_source(source).as_deref() != Ok(source.as_str()) {
                return Err(format!(
                    "tap registry {} holds noncanonical source identity {source:?}; \
                     the canonical form is required",
                    path.display()
                ));
            }
            if entry.origin.trim().is_empty() {
                return Err(format!(
                    "tap registry {} holds an empty origin for {source:?}",
                    path.display()
                ));
            }
            if cask_catalog::tap::source_origin(source).as_deref() != Ok(entry.origin.as_str()) {
                return Err(format!(
                    "tap registry {} holds origin {:?} for {source:?}; the \
                     canonical origin of that source is required",
                    path.display(),
                    entry.origin
                ));
            }
        }
        Ok(registry)
    }

    /// The approved origin of one source, when it is registered.
    #[must_use]
    pub fn origin(&self, source: &str) -> Option<&str> {
        self.sources.get(source).map(|entry| entry.origin.as_str())
    }

    /// Record consent for one source at a given time.
    pub fn approve(&mut self, source: &str, origin: &str, now_unix: u64) {
        self.sources.insert(
            source.to_string(),
            RegistryEntry {
                origin: origin.to_string(),
                added_unix: now_unix,
            },
        );
    }

    /// Remove one source; returns whether the source was registered.
    pub fn remove(&mut self, source: &str) -> bool {
        self.sources.remove(source).is_some()
    }

    /// Encode the registry to its exact persisted text with serde.
    fn encode(&self) -> Result<String, String> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("cannot encode the tap registry: {error}"))?;
        Ok(format!("{text}\n"))
    }

    /// Write the registry atomically: a fsynced named temporary file in
    /// the registry's own directory, renamed over the registry, then a
    /// best-effort directory flush.
    ///
    /// The caller must hold the [`GlobalLock`] so registry writes across
    /// sources cannot lose each other's updates.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let prepared = self.prepare(path)?;
        commit_prepared(prepared, path)
    }

    /// Prepare the fsynced temporary registry file without replacing the
    /// live registry yet.
    ///
    /// `add` prepares the next registry state *before* publication, so a
    /// publication is only committed when the registry replacement is a
    /// single already-prepared rename away.
    pub fn prepare(&self, path: &Path) -> Result<tempfile::NamedTempFile, String> {
        let text = self.encode()?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("registry path {} has no directory", path.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
            format!(
                "cannot create a registry temporary in {}: {error}",
                parent.display()
            )
        })?;
        std::io::Write::write_all(&mut temp, text.as_bytes())
            .map_err(|error| format!("cannot write the registry temporary: {error}"))?;
        temp.as_file()
            .sync_all()
            .map_err(|error| format!("cannot flush the registry temporary: {error}"))?;
        Ok(temp)
    }
}

/// Replace the registry with one already-prepared temporary file.
///
/// One rename commits the prepared state; the directory flush is best
/// effort. The caller holds the [`GlobalLock`].
pub fn commit_prepared(prepared: tempfile::NamedTempFile, path: &Path) -> Result<(), String> {
    prepared
        .persist(path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        super::atomic::fsync_directory(parent);
    }
    Ok(())
}

/// The one global lock over registry mutations.
///
/// Registry operations that touch different sources still mutate one
/// file, so they serialize on this lock instead of racing lost updates.
/// The fixed lock order is: this global lock first, then any per-source
/// publication lock.
pub struct GlobalLock {
    _file: std::fs::File,
}

impl GlobalLock {
    /// Create the tap namespace if needed and take the global lock.
    pub fn acquire(state_home: &Path) -> Result<Self, String> {
        let dir = taps_root(state_home);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(dir.join("registry.lock"))
            .map_err(|error| format!("cannot open the tap registry lock: {error}"))?;
        file.lock()
            .map_err(|error| format!("cannot lock the tap registry: {error}"))?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_and_malformed_input_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = registry_path(dir.path());
        assert_eq!(
            Registry::load(&path).expect("missing is empty"),
            Registry::empty()
        );

        // A valid registry round-trips.
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            17,
        );
        registry.save(&path).expect("saves");
        let loaded = Registry::load(&path).expect("loads");
        assert_eq!(
            loaded.origin("somebody/apps"),
            Some("https://github.com/somebody/homebrew-apps")
        );

        for (name, text) in [
            (
                "wrong schema",
                r#"{"schema":"pkg-tap-registry/0","sources":{}}"#,
            ),
            ("no schema", r#"{"sources":{}}"#),
            (
                "duplicate key",
                r#"{"schema":"pkg-tap-registry/1","sources":{},"sources":{}}"#,
            ),
            (
                "duplicate entry key",
                r#"{"schema":"pkg-tap-registry/1","sources":{"a/b":{"origin":"https://github.com/a/homebrew-b","added_unix":1},"a/b":{"origin":"x","added_unix":2}}}"#,
            ),
            (
                "unknown field",
                r#"{"schema":"pkg-tap-registry/1","sources":{"a/b":{"origin":"https://github.com/a/homebrew-b","added_unix":1,"evil":true}}}"#,
            ),
            (
                "noncanonical origin",
                r#"{"schema":"pkg-tap-registry/1","sources":{"a/b":{"origin":"https://gitlab.com/a/homebrew-b","added_unix":1}}}"#,
            ),
            (
                "reserved official source",
                r#"{"schema":"pkg-tap-registry/1","sources":{"homebrew/cask":{"origin":"https://github.com/homebrew/homebrew-cask","added_unix":1}}}"#,
            ),
            (
                "noncanonical source identity",
                r#"{"schema":"pkg-tap-registry/1","sources":{"a/homebrew-b":{"origin":"https://github.com/a/homebrew-b","added_unix":1}}}"#,
            ),
            (
                "invalid source identity",
                r#"{"schema":"pkg-tap-registry/1","sources":{"../escape":{"origin":"https://github.com/a/homebrew-b","added_unix":1}}}"#,
            ),
            (
                "empty origin",
                r#"{"schema":"pkg-tap-registry/1","sources":{"a/b":{"origin":"  ","added_unix":1}}}"#,
            ),
            ("not an object", r#"[]"#),
        ] {
            std::fs::write(&path, text).expect("write");
            let error = Registry::load(&path)
                .err()
                .unwrap_or_else(|| panic!("{name} must fail closed"));
            assert!(error.contains("registry"), "{name}: {error}");
        }
        // Removal is registry state, not a filesystem claim.
        std::fs::write(
            &path,
            r#"{"schema":"pkg-tap-registry/1","sources":{"a/b":{"origin":"https://github.com/a/homebrew-b","added_unix":1}}}"#,
        )
        .expect("write");
        let mut loaded = Registry::load(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(loaded.remove("a/b"));
        assert!(!loaded.remove("a/b"));
    }

    #[test]
    fn saves_atomically_under_the_global_lock() {
        let state = tempfile::tempdir().expect("tempdir");
        let path = registry_path(state.path());
        let _global = GlobalLock::acquire(state.path()).expect("locks");
        // A second acquisition in one process would block forever; the
        // guard releases on drop.
        drop(_global);
        let _global = GlobalLock::acquire(state.path()).expect("relocks");
        let mut registry = Registry::empty();
        registry.approve(
            "somebody/apps",
            "https://github.com/somebody/homebrew-apps",
            7,
        );
        registry.save(&path).expect("saves");
        assert!(
            Registry::load(&path)
                .expect("loads")
                .origin("somebody/apps")
                .is_some()
        );
        // The prepared-then-committed split serves the add flow.
        let prepared = registry.prepare(&path).expect("prepares");
        commit_prepared(prepared, &path).expect("commits");
        assert!(
            Registry::load(&path)
                .expect("loads")
                .origin("somebody/apps")
                .is_some()
        );
        // No fixed temporary name is left behind.
        let entries: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !entries.iter().any(|name| name.ends_with(".new")),
            "{entries:?}"
        );
    }
}
