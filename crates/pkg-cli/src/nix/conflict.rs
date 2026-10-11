//! Decoding of native profile file conflicts.

use std::path::{Component, Path, PathBuf};

const HEADER: &str = "error: An existing package already provides the following file:";
const NEW_FILE: &str = "This is the conflicting file from the new package:";

/// A file collision reported by the native profile builder.
#[derive(Debug, PartialEq, Eq)]
pub struct ProfileConflict {
    /// The conflicting file in the existing output.
    pub existing_file: PathBuf,
    /// The file's location in the combined profile.
    pub file: PathBuf,
}

pub(super) fn starts_conflict(line: &str) -> bool {
    line.trim() == HEADER
}

pub(super) fn decode(stderr: &str) -> Option<ProfileConflict> {
    let plain = crate::output::strip_ansi(stderr);
    let mut lines = plain.lines().map(str::trim).filter(|line| !line.is_empty());
    lines.find(|line| starts_conflict(line))?;
    let existing_file = quoted_path(lines.next()?)?;
    lines.find(|line| *line == NEW_FILE)?;
    let incoming = quoted_path(lines.next()?)?;
    let file = profile_file(&existing_file)?;
    if file != profile_file(&incoming)? {
        return None;
    }
    Some(ProfileConflict {
        file: file.to_path_buf(),
        existing_file,
    })
}

fn quoted_path(line: &str) -> Option<PathBuf> {
    let path = line.strip_prefix('"')?.strip_suffix('"')?;
    Some(PathBuf::from(path))
}

fn profile_file(path: &Path) -> Option<&Path> {
    let rest = path.strip_prefix("/nix/store").ok()?;
    if !rest
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let (_, file) = rest.to_str()?.split_once('/')?;
    (!file.is_empty()).then(|| Path::new(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_matching_store_relative_files_decode() {
        let diagnostic = |existing, new| {
            format!("\x1b[31m{HEADER}\x1b[0m\n\n  \"{existing}\"\n\n{NEW_FILE}\n  \"{new}\"\n")
        };
        let existing = "/nix/store/old-bat/bin/bat";
        let decoded = decode(&diagnostic(existing, "/nix/store/new-bat/bin/bat")).unwrap();
        assert_eq!(decoded.existing_file, Path::new(existing));
        assert_eq!(decoded.file, Path::new("bin/bat"));
        for new in [
            "/nix/store/new-bat/bin/other",
            "/tmp/new-bat/bin/bat",
            "/nix/store/new-bat/../bin/bat",
        ] {
            assert!(decode(&diagnostic(existing, new)).is_none());
        }
        assert!(decode("error: unrelated failure").is_none());
    }
}
