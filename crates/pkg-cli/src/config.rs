//! Configuration, fixed paths, and source references.
//!
//! The native profile records installed packages.

use std::path::{Path, PathBuf};

/// The dedicated package profile relative to the state base.
pub const PROFILE_RELATIVE: &str = "nix/profiles/pkg";
/// The separate helper profile for macOS app tools (design D7).
pub const HELPER_PROFILE_RELATIVE: &str = "nix/profiles/pkg-app-tools";

/// Fixed and validated paths for this client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Absolute XDG state base.
    pub state_home: PathBuf,
    /// The authoritative native package profile.
    pub profile: PathBuf,
    /// The separate native helper profile.
    pub helper_profile: PathBuf,
    /// The config file (may not exist yet).
    pub config_file: PathBuf,
    /// Disposable discovery cache directory.
    pub cache_dir: PathBuf,
}

/// A configuration or path error with a useful diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The home directory is unset or relative.
    #[error("HOME is unset or relative; cannot derive pkg paths")]
    NoHome,
    /// The home directory does not exist, which is often a typo.
    #[error("HOME is {}, but that folder does not exist", .0.display())]
    MissingHome(PathBuf),
    /// A base directory exists, but it is a file.
    #[error("{} is not a folder. Check {variable}, or remove the file", path.display())]
    NotFolder {
        /// The variable that selects the base, for example `XDG_CACHE_HOME`.
        variable: &'static str,
        /// The base path.
        path: PathBuf,
    },
    /// An XDG base variable is set to a relative path.
    #[error("{variable} must be an absolute path when set (got {value:?})")]
    RelativeBase {
        /// The variable name, for example `XDG_STATE_HOME`.
        variable: &'static str,
        /// The rejected value.
        value: String,
    },
    /// The config file could not be read.
    #[error("cannot read the settings file {}: {source}", path.display())]
    ReadFailed {
        /// The config file.
        path: PathBuf,
        /// The filesystem cause.
        #[source]
        source: std::io::Error,
    },
    /// The config file is not valid TOML or has invalid values.
    #[error("invalid config in {}. Fix or delete the file. {source}", path.display())]
    Invalid {
        /// The config file.
        path: PathBuf,
        /// The decode cause.
        #[source]
        source: toml::de::Error,
    },
}

fn home_dir() -> Result<PathBuf, ConfigError> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() && Path::new(&home).is_absolute() => {
            let home = PathBuf::from(home);
            if home.is_dir() {
                Ok(home)
            } else {
                Err(ConfigError::MissingHome(home))
            }
        }
        _ => Err(ConfigError::NoHome),
    }
}

fn xdg_base(variable: &'static str, fallback: &str) -> Result<PathBuf, ConfigError> {
    let path = match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(&value);
            if !path.is_absolute() {
                return Err(ConfigError::RelativeBase {
                    variable,
                    value: value.to_string_lossy().into_owned(),
                });
            }
            path
        }
        _ => home_dir()?.join(fallback),
    };
    // A missing base is created later. A file in its place is an error.
    if path.exists() && !path.is_dir() {
        return Err(ConfigError::NotFolder { variable, path });
    }
    Ok(path)
}

/// Compute the validated path set from the environment (design D3).
///
/// XDG bases must be absolute when set. The profile always points at the
/// dedicated `pkg` profile, never the user's default profile.
/// # Errors
///
/// Returns an error when an XDG base is relative or `HOME` is unset or relative.
pub fn paths() -> Result<Paths, ConfigError> {
    let state = xdg_base("XDG_STATE_HOME", ".local/state")?;
    let config = xdg_base("XDG_CONFIG_HOME", ".config")?;
    let cache = xdg_base("XDG_CACHE_HOME", ".cache")?;
    Ok(Paths::from_bases(&state, &config, &cache))
}

impl Paths {
    /// Assemble the path set from validated absolute bases.
    #[must_use]
    pub fn from_bases(state: &Path, config: &Path, cache: &Path) -> Self {
        Self {
            profile: state.join(PROFILE_RELATIVE),
            helper_profile: state.join(HELPER_PROFILE_RELATIVE),
            state_home: state.to_path_buf(),
            config_file: config.join("pkg").join("config.toml"),
            cache_dir: cache.join("pkg"),
        }
    }
}

/// Source references and display settings.
///
/// This is the only persistent client configuration; installed state lives in
/// the native profile.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Catalog source references.
    #[serde(default)]
    pub sources: Sources,
    /// Output preferences.
    #[serde(default)]
    pub display: Display,
    /// Optional explicit `nix` executable path.
    #[serde(default)]
    pub runtime: Runtime,
}

/// Catalog source references.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    /// The Nixpkgs flake reference.
    pub nixpkgs: String,
    /// The Cask product flake reference.
    pub casks: String,
    /// The Formula product flake reference.
    pub formulae: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            nixpkgs: String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
            casks: String::from("github:spa5k/pkg/main?dir=nix/casks"),
            formulae: String::from("github:spa5k/pkg/main?dir=nix/formulae"),
        }
    }
}

/// Output preferences.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    /// `never` disables color, including in text from Nix.
    /// Other values leave color handling unchanged.
    pub color: String,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            color: String::from("auto"),
        }
    }
}

/// Runtime selection.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runtime {
    /// Optional absolute or PATH-relative `nix` executable override.
    pub nix: Option<String>,
}

impl Config {
    /// Load config from `path`, falling back to defaults when absent.
    ///
    /// A present but malformed file is an error, not a silent default.
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be read, decoded, or validated.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Invalid {
                path: path.to_path_buf(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::ReadFailed {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Whether colored output is forced off.
    #[must_use]
    pub fn color_disabled(&self, cli_no_color: bool) -> bool {
        cli_no_color || self.display.color == "never"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_load_retains_the_decode_cause() {
        use std::error::Error as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "sources = 3").expect("write");
        let error = Config::load(&file).expect_err("invalid config must fail");
        assert!(error.source().unwrap().is::<toml::de::Error>());
    }
}
