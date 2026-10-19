//! `pkg self uninstall`: remove the client and the files the installer owns.
//!
//! Nix, the native package profile, and saved taps stay. The command
//! removes the scheduled job, the managed shell setup, completions,
//! settings, the discovery cache, the job log, and the client itself.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use crate::client::{BLOCK_BEGIN, BLOCK_END, FISH_MARKER, Layout};

use super::{CommandError, FAILURE, USAGE};

pub(super) fn run(yes: bool) -> Result<(), CommandError> {
    let paths = crate::config::paths()?;
    let layout = Layout::for_user()
        .ok_or_else(|| CommandError::from(String::from("HOME must be an absolute path")))?;
    if !yes && !confirmed()? {
        return Err(CommandError::Reported(ExitCode::from(USAGE)));
    }
    let mut failures = Vec::new();
    let mut fail = |item: &str, error: &dyn std::fmt::Display| {
        failures.push(format!("{item}: {error}"));
    };
    if let Err(error) = crate::background::disable(&paths) {
        if error.signal().is_some() {
            return Err(error.into());
        }
        fail("background job", &error);
    }
    for file in layout.startup_files() {
        if let Err(error) = remove_block(&file) {
            fail(&file.display().to_string(), &error);
        }
    }
    if let Err(error) = remove_fish_file(&layout.fish_file()) {
        fail(&layout.fish_file().display().to_string(), &error);
    }
    let config_dir = paths.config_file.parent().map(Path::to_path_buf);
    for directory in [
        Some(layout.data_dir()),
        Some(paths.cache_dir.clone()),
        config_dir,
    ]
    .into_iter()
    .flatten()
    {
        if let Err(error) = remove_dir(&directory) {
            fail(&directory.display().to_string(), &error);
        }
    }
    for file in [
        paths.state_home.join("pkg/catalog-refresh.log"),
        layout.binary(),
    ] {
        if let Err(error) = remove_file(&file) {
            fail(&file.display().to_string(), &error);
        }
    }
    if !failures.is_empty() {
        eprintln!("pkg was removed in part. These items remain:");
        for failure in &failures {
            eprintln!("  {failure}");
        }
        return Err(CommandError::Reported(ExitCode::from(FAILURE)));
    }
    println!("Removed pkg.");
    println!(
        "Nix and your installed packages were not removed. Packages: {}",
        paths.profile.display()
    );
    println!("Open a new terminal window to finish.");
    Ok(())
}

fn confirmed() -> Result<bool, CommandError> {
    if !std::io::stdin().is_terminal() {
        eprintln!("pkg: run `pkg self uninstall --yes` to remove pkg without a prompt.");
        return Ok(false);
    }
    print!("Remove pkg from this computer? Nix and your installed packages stay. [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
        Ok(true)
    } else {
        println!("Nothing was removed.");
        Ok(false)
    }
}

/// Remove the managed block from one startup file. Other lines stay.
fn remove_block(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) => metadata,
    };
    if !metadata.is_file() {
        let content = std::fs::read(path).unwrap_or_default();
        if without_block(&content).is_ok_and(|result| result.is_none()) {
            return Ok(());
        }
        return Err(String::from(
            "this file is a link. Remove the pkg block from the file it points to.",
        ));
    }
    let content = std::fs::read(path).map_err(|error| error.to_string())?;
    let Some(content) = without_block(&content)? else {
        return Ok(());
    };
    replace(path, &content, &metadata).map_err(|error| error.to_string())
}

/// The content without the managed block, or `None` if no block exists.
fn without_block(content: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let (mut begins, mut ends) = (Vec::new(), Vec::new());
    let mut offset = 0;
    for line in content.split_inclusive(|byte| *byte == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        if text == BLOCK_BEGIN.as_bytes() {
            begins.push(offset);
        }
        if text == BLOCK_END.as_bytes() {
            ends.push(offset + line.len());
        }
        offset += line.len();
    }
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([begin], [end]) if begin < end => {
            // The installer separates its block from earlier lines with one
            // blank line; remove that line too.
            let start = if content[..*begin].ends_with(b"\n\n") {
                begin - 1
            } else {
                *begin
            };
            let mut result = content[..start].to_vec();
            result.extend_from_slice(&content[*end..]);
            Ok(Some(result))
        }
        _ => Err(format!(
            "pkg marker lines are in an unexpected form. Remove the lines from {BLOCK_BEGIN} to {BLOCK_END} yourself."
        )),
    }
}

fn replace(path: &Path, content: &[u8], metadata: &std::fs::Metadata) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(content)?;
    file.as_file().set_permissions(metadata.permissions())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Remove the Fish file only when the installer wrote it.
fn remove_fish_file(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.is_file() => {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            if content.lines().next() == Some(FISH_MARKER) {
                std::fs::remove_file(path).map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        }
        Ok(_) => Ok(()),
    }
}

fn remove_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> String {
        format!("{BLOCK_BEGIN}\nexport PATH='/x'\"${{PATH:+:$PATH}}\"\n{BLOCK_END}\n")
    }

    #[test]
    fn removes_only_the_managed_block_and_its_separator() {
        let content = format!("alias ll='ls -l'\n\n{}after\n", block());
        let result = without_block(content.as_bytes()).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(result).unwrap(),
            "alias ll='ls -l'\nafter\n"
        );
        let result = without_block(block().as_bytes()).unwrap().unwrap();
        assert_eq!(result, Vec::<u8>::new());
        let content = format!("first\n{}", block().trim_end());
        let result = without_block(content.as_bytes()).unwrap().unwrap();
        assert_eq!(String::from_utf8(result).unwrap(), "first\n");
    }

    #[test]
    fn leaves_files_without_or_with_broken_markers_alone() {
        assert_eq!(without_block(b"plain\n").unwrap(), None);
        let twice = format!("{}{}", block(), block());
        assert!(without_block(twice.as_bytes()).is_err());
        let reversed = format!("{BLOCK_END}\n{BLOCK_BEGIN}\n");
        assert!(without_block(reversed.as_bytes()).is_err());
    }

    #[test]
    fn rewrites_a_startup_file_in_place_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join(".zshrc");
        std::fs::write(&file, format!("keep\n\n{}", block())).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        remove_block(&file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep\n");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let link = directory.path().join(".bashrc");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        remove_block(&link).unwrap();
        std::fs::write(&file, block()).unwrap();
        assert!(remove_block(&link).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), block());
    }

    #[test]
    fn removes_only_a_fish_file_that_pkg_wrote() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("pkg.fish");
        std::fs::write(&file, "set -gx PATH /mine $PATH\n").unwrap();
        remove_fish_file(&file).unwrap();
        assert!(file.exists());
        std::fs::write(&file, format!("{FISH_MARKER}\nset -gx PATH /x $PATH\n")).unwrap();
        remove_fish_file(&file).unwrap();
        assert!(!file.exists());
    }
}
