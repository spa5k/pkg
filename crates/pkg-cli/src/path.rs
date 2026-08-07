//! Non-mutating shell PATH integration snippets and observations.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_MANAGED_COMMANDS: usize = 4_096;

/// Host family whose per-user state convention determines the activation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFamily {
    /// Linux/XDG per-user state convention.
    Linux,
    /// macOS Application Support per-user state convention.
    MacOs,
}

impl HostFamily {
    /// Detect the current supported host family.
    #[must_use]
    pub fn detect() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::MacOs),
            _ => None,
        }
    }
}

/// Resolve the invoking user's default product state root without creating it.
#[must_use]
pub fn default_state_root(host: HostFamily) -> Option<PathBuf> {
    if let Some(override_root) = std::env::var_os("PKG_STATE_DIR") {
        return Some(PathBuf::from(override_root));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    match host {
        HostFamily::Linux => Some(
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("pkg"),
        ),
        HostFamily::MacOs => Some(home.join("Library/Application Support/pkg")),
    }
}

/// Render a shell snippet that prepends the invoking user's active generation exactly once.
#[must_use]
pub const fn shell_init(host: HostFamily) -> &'static str {
    match host {
        HostFamily::Linux => {
            r#"# managed by pkg — do not edit
__pkg_state="${XDG_DATA_HOME:-$HOME/.local/share}/pkg"
case ":$PATH:" in
  *":$__pkg_state/current/bin:"*) ;;
  *) PATH="$__pkg_state/current/bin:$PATH" ;;
esac
export MANPATH="$__pkg_state/current/share/man:${MANPATH:-}"
unset __pkg_state
"#
        }
        HostFamily::MacOs => {
            r#"# managed by pkg — do not edit
__pkg_state="$HOME/Library/Application Support/pkg"
case ":$PATH:" in
  *":$__pkg_state/current/bin:"*) ;;
  *) PATH="$__pkg_state/current/bin:$PATH" ;;
esac
export MANPATH="$__pkg_state/current/share/man:${MANPATH:-}"
unset __pkg_state
"#
        }
    }
}

/// Read-only summary of whether the invoking user's activation bin is on PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathObservation {
    expected_bin: PathBuf,
    first_index: Option<usize>,
    duplicate_count: usize,
    shadowed_count: usize,
    shadow_scan_complete: bool,
}

impl PathObservation {
    /// Inspect already-split PATH entries without reading or changing shell files.
    #[must_use]
    pub fn inspect(
        expected_bin: &Path,
        entries: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Self {
        let entries = entries
            .into_iter()
            .map(|entry| entry.as_ref().to_owned())
            .collect::<Vec<_>>();
        let mut first_index = None;
        let mut duplicate_count = 0;
        for (index, entry) in entries.iter().enumerate() {
            if entry == expected_bin {
                first_index.get_or_insert(index);
                duplicate_count += 1;
            }
        }
        let (shadowed_count, shadow_scan_complete) = first_index.map_or((0, true), |index| {
            inspect_shadowing(expected_bin, &entries[..index])
        });
        Self {
            expected_bin: expected_bin.to_owned(),
            first_index,
            duplicate_count,
            shadowed_count,
            shadow_scan_complete,
        }
    }

    /// Expected activation-bin path.
    #[must_use]
    pub fn expected_bin(&self) -> &Path {
        &self.expected_bin
    }

    /// Zero-based position of the first exact entry, when present.
    #[must_use]
    pub const fn first_index(&self) -> Option<usize> {
        self.first_index
    }

    /// Number of exact entries in PATH.
    #[must_use]
    pub const fn duplicate_count(&self) -> usize {
        self.duplicate_count
    }

    /// Number of managed commands shadowed by an executable earlier on PATH.
    #[must_use]
    pub const fn shadowed_count(&self) -> usize {
        self.shadowed_count
    }

    /// Whether the bounded shadow scan inspected the complete managed command inventory.
    #[must_use]
    pub const fn shadow_scan_complete(&self) -> bool {
        self.shadow_scan_complete
    }
}

fn inspect_shadowing(expected_bin: &Path, earlier_entries: &[PathBuf]) -> (usize, bool) {
    let commands = match fs::read_dir(expected_bin) {
        Ok(commands) => commands,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (0, true),
        Err(_) => return (0, false),
    };
    let mut shadowed_count = 0;
    for (index, command) in commands.enumerate() {
        if index == MAX_MANAGED_COMMANDS {
            return (shadowed_count, false);
        }
        let Ok(command) = command else {
            return (shadowed_count, false);
        };
        let command_path = command.path();
        let Some(name) = command_path.file_name() else {
            return (shadowed_count, false);
        };
        match is_executable(&command_path) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(_) => return (shadowed_count, false),
        }
        for entry in earlier_entries {
            match is_executable(&entry.join(name)) {
                Ok(true) => {
                    shadowed_count += 1;
                    break;
                }
                Ok(false) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return (shadowed_count, false),
            }
        }
    }
    (shadowed_count, true)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)?;
    Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> std::io::Result<bool> {
    Ok(fs::metadata(path)?.is_file())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn snippets_are_dynamic_idempotent_and_never_expose_managed_nix() {
        for host in [HostFamily::Linux, HostFamily::MacOs] {
            let snippet = shell_init(host);
            assert!(snippet.contains("current/bin"));
            assert!(snippet.contains("case \":$PATH:\""));
            assert!(snippet.contains("$HOME"));
            assert!(!snippet.contains("/nix/store"));
            assert!(!snippet.contains("/opt/pkg/nix"));
        }
    }

    #[test]
    fn path_observation_uses_exact_components_only() {
        let expected = Path::new("/user/pkg/current/bin");
        let observation = PathObservation::inspect(
            expected,
            [
                Path::new("/usr/bin"),
                expected,
                Path::new("/user/pkg/current/bin-extra"),
                expected,
            ],
        );
        assert_eq!(observation.first_index(), Some(1));
        assert_eq!(observation.duplicate_count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn path_observation_counts_managed_commands_shadowed_earlier() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "pkg-path-shadow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let earlier = root.join("earlier");
        let expected = root.join("current/bin");
        fs::create_dir_all(&earlier).unwrap();
        fs::create_dir_all(&expected).unwrap();
        for path in [earlier.join("rg"), expected.join("rg"), expected.join("fd")] {
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let observation = PathObservation::inspect(&expected, [&earlier, &expected]);
        assert_eq!(observation.shadowed_count(), 1);
        assert!(observation.shadow_scan_complete());
        fs::remove_dir_all(root).unwrap();
    }
}
