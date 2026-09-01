//! Non-mutating shell PATH integration snippets and observations.

use std::fs;
use std::path::{Path, PathBuf};

use nix::unistd::{Uid, User};

const MAX_MANAGED_COMMANDS: usize = 4_096;

/// Host family whose per-user state convention determines the activation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFamily {
    /// Linux fixed per-user state convention.
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

/// Resolve the one production state root for the invoking user.
#[must_use]
pub fn production_state_root(host: HostFamily, home: &Path) -> PathBuf {
    match host {
        HostFamily::Linux => home.join(".local/share/pkg"),
        HostFamily::MacOs => home.join("Library/Application Support/pkg"),
    }
}

/// One resolved state location beneath the invoking user's home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLocation {
    state_root: PathBuf,
    home: PathBuf,
    kind: StateLocationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateLocationKind {
    Production,
    Alternate,
}

impl StateLocation {
    /// Builds an alternate location. Filesystem validation remains mandatory.
    #[must_use]
    pub const fn alternate(state_root: PathBuf, home: PathBuf) -> Self {
        Self {
            state_root,
            home,
            kind: StateLocationKind::Alternate,
        }
    }

    /// The root where pkg reads and writes its private per-user state.
    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// The home boundary that the state root is validated beneath.
    #[must_use]
    pub fn trusted_boundary(&self) -> &Path {
        &self.home
    }

    /// Whether this is the fixed production root.
    #[must_use]
    pub const fn is_production(&self) -> bool {
        matches!(self.kind, StateLocationKind::Production)
    }
}

/// Why no usable state location could be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateLocationError {
    /// An explicit alternate state root was not absolute.
    RelativeAlternateRoot,
    /// The effective uid has no safe system/passwd home.
    SystemHomeUnavailable,
    /// The operating system is not a supported host family.
    UnsupportedHost,
}

/// Resolve the invoking user's state root and trusted boundary together.
///
/// `--state` and non-empty `PKG_STATE_DIR` select alternate roots. They remain
/// bounded by the effective uid's system home and cannot be used for
/// broker-backed mutations.
pub fn resolve_state_location(
    host: HostFamily,
    state_override: Option<&Path>,
) -> Result<StateLocation, StateLocationError> {
    let uid = Uid::effective();
    let home = User::from_uid(uid)
        .map_err(|_| StateLocationError::SystemHomeUnavailable)?
        .filter(|user| user.uid == uid)
        .map(|user| user.dir);
    let override_root = std::env::var_os("PKG_STATE_DIR").map(PathBuf::from);
    resolve_state_location_from(host, state_override, home, override_root)
}

/// Environment-free resolution core, kept pure so tests can exercise the
/// trusted-boundary decision without mutating process state.
fn resolve_state_location_from(
    host: HostFamily,
    state_override: Option<&Path>,
    home: Option<PathBuf>,
    override_root: Option<PathBuf>,
) -> Result<StateLocation, StateLocationError> {
    let home = home_dir(home)?;
    let alternate = state_override
        .map(Path::to_owned)
        .or_else(|| override_root.filter(|root| !root.as_os_str().is_empty()));
    if let Some(state_root) = alternate {
        if !state_root.is_absolute() {
            return Err(StateLocationError::RelativeAlternateRoot);
        }
        return Ok(StateLocation::alternate(state_root, home));
    }
    Ok(StateLocation {
        state_root: production_state_root(host, &home),
        home,
        kind: StateLocationKind::Production,
    })
}

fn home_dir(home: Option<PathBuf>) -> Result<PathBuf, StateLocationError> {
    home.filter(|home| home.is_absolute() && home != Path::new("/"))
        .ok_or(StateLocationError::SystemHomeUnavailable)
}

/// Render a shell snippet that prepends the invoking user's active generation exactly once.
#[must_use]
pub const fn shell_init(host: HostFamily) -> &'static str {
    match host {
        HostFamily::Linux => {
            r#"# managed by pkg — do not edit
__pkg_state="$HOME/.local/share/pkg"
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

/// Whether a raw Nix CLI binary is visible through the supplied PATH.
///
/// This is a closed UX-only warning signal, not a trust decision. It never
/// executes `which`, `nix`, or any external tool, never canonicalizes PATH
/// entries, and never retains or prints a discovered path or entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawNixVisibility {
    /// No raw Nix CLI binary is visible through a readable absolute entry.
    Hidden,
    /// A raw Nix CLI binary is visible through a readable absolute entry.
    Visible,
    /// A PATH entry is relative or unreadable; the verdict is not trustworthy.
    Unknown,
}

/// Inspect the supplied PATH for raw Nix CLI visibility.
///
/// Entries are split with [`std::env::split_paths`] and probed with
/// `is_executable`, never executed. A relative or unreadable entry yields
/// [`RawNixVisibility::Unknown`]. An absolute readable entry that contains an
/// executable `nix` binary yields [`RawNixVisibility::Visible`]. An absolute
/// readable entry without one is not a signal.
#[must_use]
pub fn observe_raw_nix_visibility(path: &str) -> RawNixVisibility {
    let mut unknown = false;
    for entry in std::env::split_paths(path) {
        if !entry.is_absolute() {
            unknown = true;
            continue;
        }
        match is_executable(&entry.join("nix")) {
            Ok(true) => return RawNixVisibility::Visible,
            Ok(false) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unknown = true,
        }
    }
    if unknown {
        RawNixVisibility::Unknown
    } else {
        RawNixVisibility::Hidden
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
mod tests;
