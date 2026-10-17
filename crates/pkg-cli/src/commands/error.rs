//! Error conversion and diagnostics for command execution.

use std::process::ExitCode;

use crate::apps::error::AppError;
use crate::catalog::CatalogError;
use crate::config::ConfigError;
use crate::nix::NixError;

/// A command failure, retained until the runner reports it.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    Integer(#[from] std::num::TryFromIntError),
    #[error("{0}")]
    Native(#[from] NixError),
    #[error("{0}")]
    Background(#[from] crate::background::Error),
    #[error("{0}")]
    Catalog(#[from] CatalogError),
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Apps(#[from] AppError),
    #[error(
        "importing {tap} failed; the published generation and the registry are unchanged: {error}"
    )]
    TapImport {
        tap: String,
        #[source]
        error: cask_catalog::tap::ImportError,
    },
    #[error("the sandbox setup failed: {0}")]
    TapSetup(#[from] crate::tap::setup::SetupError),
    #[error("the generated staging flake is invalid: {0}")]
    TapValidation(#[source] NixError),
    /// The handler already printed diagnostics and checked native state.
    #[error("command failure already reported")]
    Reported(ExitCode),
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl CommandError {
    pub fn is_interrupted(&self) -> bool {
        self.signal().is_some()
    }

    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::Native(error) | Self::TapValidation(error) => error.signal(),
            Self::Catalog(error) => error.signal(),
            Self::Apps(error) => error.signal(),
            Self::Background(error) => error.signal(),
            Self::TapImport { error, .. } => error.signal(),
            Self::TapSetup(error) => error.signal(),
            Self::Message(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Database(_)
            | Self::Integer(_)
            | Self::Config(_)
            | Self::Reported(_) => None,
        }
    }
}

pub fn signal_exit(signal: i32) -> ExitCode {
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

    #[test]
    fn command_conversion_keeps_host_and_output_causes() {
        let original = std::sync::Arc::new(crate::nix::HostVersionError::Process(
            pkg_process::RunError {
                cancelled: Some(libc::SIGINT),
                source: std::io::Error::from_raw_os_error(libc::ECHILD),
            },
        ));
        let error = CommandError::from(CatalogError::Host(original.clone()));
        let host = error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<std::sync::Arc<crate::nix::HostVersionError>>()
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(host, &original));
        assert!(matches!(
            host.as_ref(),
            crate::nix::HostVersionError::Process(_)
        ));
        assert_eq!(
            host.source()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(libc::ECHILD),
        );
        let error = CommandError::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::BrokenPipe,
        );
    }

    #[test]
    fn failed_supervision_keeps_cancellation_through_command_wrappers() {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let failure = || pkg_process::RunError {
                cancelled: Some(signal),
                source: std::io::Error::from_raw_os_error(libc::ECHILD),
            };
            for error in [
                CommandError::from(CatalogError::from(NixError::Process(failure()))),
                CommandError::from(crate::background::Error::Process(failure())),
                CommandError::from(crate::background::Error::Schedule(Box::new(
                    crate::background::Error::Process(failure()),
                ))),
                CommandError::from(NixError::ConfiguredProbe {
                    requested: String::from("fixture nix"),
                    source: Box::new(NixError::Process(failure())),
                }),
                CommandError::from(CatalogError::Host(std::sync::Arc::new(
                    crate::nix::HostVersionError::Process(failure()),
                ))),
                CommandError::from(AppError::from(failure())),
                CommandError::from(crate::tap::setup::SetupError::from(NixError::Process(
                    failure(),
                ))),
                CommandError::from(crate::tap::setup::SetupError::Process {
                    operation: String::from("feed input"),
                    source: failure(),
                }),
                CommandError::TapValidation(NixError::Process(failure())),
                CommandError::TapImport {
                    tap: String::from("example/tools"),
                    error: cask_catalog::tap::ImportError::Process {
                        detail: String::from("cleanup failed"),
                        source: failure(),
                    },
                },
            ] {
                assert_eq!(error.signal(), Some(signal));
                let mut cause: &(dyn std::error::Error + 'static) = &error;
                while let Some(source) = cause.source() {
                    cause = source;
                }
                assert_eq!(
                    cause
                        .downcast_ref::<std::io::Error>()
                        .expect("original I/O cause")
                        .raw_os_error(),
                    Some(libc::ECHILD)
                );
            }
        }
    }
}
