//! Configuration, fixed paths, and source references (design D3, D5).
//!
//! The native profile is authoritative. No config or cache file is
//! authoritative for installed packages.

use std::fmt;
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
#[derive(Debug)]
pub enum ConfigError {
    /// The home directory is unset or relative.
    NoHome,
    /// An XDG base variable is set to a relative path.
    RelativeBase {
        /// The variable name, for example `XDG_STATE_HOME`.
        variable: &'static str,
        /// The rejected value.
        value: String,
    },
    /// The config file could not be read.
    ReadFailed(std::io::Error),
    /// The config file is not valid TOML or has invalid values.
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => write!(f, "HOME is unset or relative; cannot derive pkg paths"),
            Self::RelativeBase { variable, value } => write!(
                f,
                "{variable} must be an absolute path when set (got {value:?})"
            ),
            Self::ReadFailed(error) => write!(f, "cannot read config file: {error}"),
            Self::Invalid(detail) => write!(f, "invalid config: {detail}"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn home_dir() -> Result<PathBuf, ConfigError> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() && Path::new(&home).is_absolute() => Ok(PathBuf::from(home)),
        _ => Err(ConfigError::NoHome),
    }
}

fn xdg_base(variable: &'static str, fallback: &str) -> Result<PathBuf, ConfigError> {
    match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(&value);
            if !path.is_absolute() {
                return Err(ConfigError::RelativeBase {
                    variable,
                    value: value.to_string_lossy().into_owned(),
                });
            }
            Ok(path)
        }
        _ => Ok(home_dir()?.join(fallback)),
    }
}

/// Compute the validated path set from the environment (design D3).
///
/// XDG bases must be absolute when set. The profile always points at the
/// dedicated `pkg` profile, never the user's default profile.
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

/// The default Nixpkgs source reference (design D5).
#[must_use]
pub fn default_nixpkgs() -> String {
    String::from("github:NixOS/nixpkgs/nixpkgs-unstable")
}

/// The default Cask product flake reference (design D6).
#[must_use]
pub fn default_casks() -> String {
    String::from("github:spa5k/pkg/main?dir=nix/casks")
}

/// Source references and display settings.
///
/// This is the only persistent client configuration; installed state lives in
/// the native profile.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
pub struct Sources {
    /// The Nixpkgs flake reference.
    #[serde(default = "default_nixpkgs")]
    pub nixpkgs: String,
    /// The Cask product flake reference.
    #[serde(default = "default_casks")]
    pub casks: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            nixpkgs: default_nixpkgs(),
            casks: default_casks(),
        }
    }
}

/// Output preferences.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Display {
    /// Color policy. Only `never` has an effect: it disables colored
    /// output, including for native text pkg passes through. Any other
    /// value is accepted and leaves color handling unchanged; `pkg` has
    /// no setting that forces color on.
    #[serde(default = "default_color")]
    pub color: String,
}

fn default_color() -> String {
    String::from("auto")
}

impl Default for Display {
    fn default() -> Self {
        Self {
            color: default_color(),
        }
    }
}

/// Runtime selection.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Runtime {
    /// Optional absolute or PATH-relative `nix` executable override.
    #[serde(default)]
    pub nix: Option<String>,
}

impl Config {
    /// Load config from `path`, falling back to defaults when absent.
    ///
    /// A present but malformed file is an error, not a silent default.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|error| ConfigError::Invalid(error.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ConfigError::ReadFailed(error)),
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
    fn from_bases_assembles_the_fixed_layout() {
        let ok = Paths::from_bases(
            Path::new("/tmp/pkg-test-state"),
            Path::new("/tmp/pkg-test-config"),
            Path::new("/tmp/pkg-test-cache"),
        );
        assert!(ok.profile.ends_with("nix/profiles/pkg"));
        assert!(ok.helper_profile.ends_with("nix/profiles/pkg-app-tools"));
        assert!(ok.config_file.ends_with("pkg/config.toml"));
        assert!(ok.cache_dir.ends_with("pkg"));
        // `from_bases` trusts already-validated bases; rejection of relative
        // bases from the environment is exercised by the CLI test that runs
        // the real binary with a relative XDG_STATE_HOME.
        assert!(paths().is_ok());
    }

    #[test]
    fn config_defaults_and_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        let loaded = Config::load(&file).expect("missing file means defaults");
        assert_eq!(loaded, Config::default());
        let text = toml::to_string(&Config::default()).expect("serializes");
        std::fs::write(&file, text).expect("write");
        assert_eq!(Config::load(&file).expect("parses"), Config::default());
        std::fs::write(&file, "sources = 3").expect("write");
        assert!(Config::load(&file).is_err());
    }
}
