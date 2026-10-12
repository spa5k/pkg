//! Error conversion and diagnostics for command execution.

use std::fmt;
use std::process::ExitCode;

use crate::apps::error::AppError;
use crate::catalog::CatalogError;
use crate::config::ConfigError;
use crate::nix::NixError;

/// A command failure, retained until the runner reports it.
#[derive(Debug)]
pub(super) enum CommandError {
    Message(String),
    Native(NixError),
    Catalog(CatalogError),
    Config(ConfigError),
    Apps(AppError),
    /// The handler already printed diagnostics and checked native state.
    Reported(ExitCode),
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<NixError> for CommandError {
    fn from(error: NixError) -> Self {
        Self::Native(error)
    }
}

impl From<CatalogError> for CommandError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ConfigError> for CommandError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<AppError> for CommandError {
    fn from(error: AppError) -> Self {
        Self::Apps(error)
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Native(error) => error.fmt(f),
            Self::Catalog(error) => error.fmt(f),
            Self::Config(error) => error.fmt(f),
            Self::Apps(error) => error.fmt(f),
            Self::Reported(_) => f.write_str("command failure already reported"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(source) => Some(source),
            Self::Catalog(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::Apps(source) => Some(source),
            Self::Message(_) | Self::Reported(_) => None,
        }
    }
}

impl CommandError {
    pub(super) fn is_interrupted(&self) -> bool {
        self.signal().is_some()
    }

    pub(super) fn signal(&self) -> Option<i32> {
        match self {
            Self::Native(NixError::Interrupted { signal, .. })
            | Self::Catalog(CatalogError::Interrupted(signal)) => Some(*signal),
            Self::Apps(error) => error.signal(),
            Self::Native(_)
            | Self::Message(_)
            | Self::Catalog(_)
            | Self::Config(_)
            | Self::Reported(_) => None,
        }
    }
}

pub(super) fn signal_exit(signal: i32) -> ExitCode {
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(super::FAILURE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn command_conversion_keeps_native_and_cache_causes() {
        let native = NixError::Spawn(std::io::Error::from(std::io::ErrorKind::NotFound));
        let error = CommandError::from(CatalogError::from(native));
        let catalog = error
            .source()
            .unwrap()
            .downcast_ref::<CatalogError>()
            .unwrap();
        let native = catalog
            .source()
            .unwrap()
            .downcast_ref::<NixError>()
            .unwrap();
        assert_eq!(
            native
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        let error = CommandError::from(CatalogError::cache_write(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
        let cause = error.source().unwrap().source().unwrap();
        assert_eq!(
            cause.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
