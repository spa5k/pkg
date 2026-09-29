//! Local public Homebrew Cask taps: registry, consent, publication, and
//! the saved catalog data the query paths read.
//!
//! The client owns consent and publication; the backend library
//! (`cask_catalog::tap`) owns fetching, sandboxed Ruby, and generation.
//! Nothing in this module executes Ruby: queries read only the saved,
//! strictly validated catalog captures published here.
//!
//! Publication model (see `store`): one stable REAL `current` directory
//! per source, replaced by one atomic directory exchange, with the
//! exchanged-out generation preserved immutably for native rollback. The
//! registry records only origin and consent; revision and provenance live
//! inside the generation, so one exchange commits the whole active
//! generation without a registry race.

pub mod atomic;
pub mod consent;
pub mod registry;
pub mod saved;
pub mod store;
pub mod strict;

pub use consent::Decision;
pub use registry::{
    GlobalLock, REGISTRY_SCHEMA, Registry, RegistryEntry, commit_prepared, registry_path,
};
pub use saved::{SavedCatalog, SavedCatalogs, load_all, validate_binding};
pub use store::{Provenance, Published, SourceStore, current_ref};

/// Validate one staged import by native evaluation of the staging flake.
///
/// The evaluation uses only our fixed builder over trusted generated
/// assets; the captured `catalogIndex` output is decoded by the ordinary
/// strict schema-3 decoder, so no second classifier exists in the client.
/// The decoded envelope is returned for the generation's capture file.
pub fn validate_staging(
    nix: &crate::nix::Nix,
    flake_path: &std::path::Path,
) -> Result<crate::nix::CatalogIndex, String> {
    // The same percent-encoding as the stable `current` reference: a
    // staging path with spaces, `#`, `?`, or `%` must stay exactly one
    // reference text Nix evaluates verbatim, never a raw path string that
    // splits or truncates the reference.
    let reference = format!("path:{}", crate::nix::encode_path_reference(flake_path));
    nix.catalog_index(&reference)
        .map_err(|error| format!("the generated staging flake is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// One executable fake Nix that answers the version probe and echoes
    /// every argument line of an `eval` call into `capture`, then prints a
    /// minimal valid catalog index envelope.
    fn fake_nix(capture: &Path) -> (crate::nix::Nix, tempfile::TempDir) {
        let bin = tempfile::tempdir().expect("tempdir");
        let script = bin.path().join("nix");
        let catalog = r#"{"schema":"pkg-cask-catalog/3",
          "generator":{"name":"cask-catalog","version":"0.1.0"},
          "inputs":{"somebody/apps":{"url":"https://github.com/somebody/homebrew-apps",
            "revision":"0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a",
            "sha256":"1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251",
            "license":"Homebrew data license",
            "raw":{"archiveUrl":"https://api.github.com/repos/somebody/homebrew-apps/tarball/0a56",
              "archiveSha256":"2f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529252",
              "captureSha256":"1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251"}}},
          "targets":["x86_64-linux"],
          "macosBaseline":"15.7.7",
          "systems":{"x86_64-linux":{"entries":{}}}}"#;
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'eval --json'*) printf '%s\\n' \"$@\" > '{capture}'; cat <<'JSON'\n{catalog}\nJSON\n;;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n",
                capture = capture.display()
            )
        )
        .expect("write fake nix");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("chmod");
        }
        let nix = crate::nix::discover(Some(script.as_os_str().to_str().expect("utf-8")))
            .expect("discovers the fake runtime");
        (nix, bin)
    }

    #[test]
    fn staging_reference_is_encoded_for_paths_with_special_characters() {
        let capture = tempfile::tempdir().expect("tempdir");
        let capture_file = capture.path().join("args");
        let (nix, _bin) = fake_nix(&capture_file);
        // A staging path with a space, `#`, `?`, and `%` must reach Nix as
        // one percent-encoded reference text, never a raw path string.
        let flake = PathBuf::from("/tmp/staging we#ird?50%/flake");
        let index = validate_staging(&nix, &flake).expect("validates");
        assert!(index.inputs.contains_key("somebody/apps"));
        let args = std::fs::read_to_string(&capture_file).expect("captured args");
        let installable = args
            .lines()
            .find(|line| line.starts_with("path:"))
            .expect("one path reference is passed");
        assert_eq!(
            installable,
            format!(
                "path:{}#catalogIndex",
                crate::nix::encode_path_reference(&flake)
            )
        );
        assert!(
            installable.contains("staging%20we%23ird%3F50%25/flake"),
            "{installable}"
        );
        assert!(!installable.contains(' '), "{installable}");
    }
}
