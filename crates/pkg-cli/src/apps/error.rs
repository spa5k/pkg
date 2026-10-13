use crate::nix::NixError;

#[derive(Debug, thiserror::Error)]
#[error("{context}{separator}{cause}", separator = if .context.is_empty() { "" } else { ": " })]
pub struct AppError {
    context: String,
    #[source]
    cause: AppErrorCause,
    dock_effect_possible: bool,
}

#[derive(Debug, thiserror::Error)]
enum AppErrorCause {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Native(#[source] NixError),
    #[error("{0}")]
    Process(#[source] pkg_process::RunError),
    #[error("was interrupted by signal {0}")]
    Interrupted(i32),
}

impl AppError {
    fn new(context: impl Into<String>, cause: AppErrorCause) -> Self {
        Self {
            context: context.into(),
            cause,
            dock_effect_possible: false,
        }
    }

    pub(super) fn message(message: impl Into<String>) -> Self {
        Self::new("", AppErrorCause::Message(message.into()))
    }

    pub(super) fn native(context: impl Into<String>, cause: NixError) -> Self {
        Self::new(context, AppErrorCause::Native(cause))
    }

    pub(super) fn interrupted(context: impl Into<String>, signal: i32) -> Self {
        Self::new(context, AppErrorCause::Interrupted(signal))
    }

    pub(super) fn process(context: impl Into<String>, error: pkg_process::RunError) -> Self {
        Self::new(context, AppErrorCause::Process(error))
    }

    pub(crate) fn signal(&self) -> Option<i32> {
        match &self.cause {
            AppErrorCause::Interrupted(signal) => Some(*signal),
            AppErrorCause::Process(error) => error.cancelled,
            AppErrorCause::Native(error) => error.signal(),
            AppErrorCause::Message(_) => None,
        }
    }

    pub(crate) fn dock_effect_possible(&self) -> bool {
        self.dock_effect_possible
    }

    pub(super) fn with_dock_effect_possible(mut self) -> Self {
        self.dock_effect_possible = true;
        self
    }
}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        Self::message(message)
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        Self::message(message)
    }
}

impl From<pkg_process::RunError> for AppError {
    fn from(error: pkg_process::RunError) -> Self {
        Self::process("", error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn native_error_preserves_the_native_cause_and_cancellation() {
        let error = AppError::native(
            "read helper profile",
            NixError::Interrupted {
                args: vec!["profile".to_string(), "list".to_string()],
                signal: libc::SIGTERM,
            },
        );
        let cause = error
            .source()
            .expect("app cause")
            .source()
            .expect("native cause")
            .downcast_ref::<NixError>()
            .expect("original Nix error type");
        assert_eq!(error.signal(), cause.signal());
        assert_eq!(error.signal(), Some(libc::SIGTERM));
        assert_eq!(error.to_string(), format!("read helper profile: {cause}"));
        assert!(!error.dock_effect_possible());
    }

    #[test]
    fn process_error_preserves_cancellation_and_the_io_cause_after_dock_work() {
        let error = AppError::process(
            "run app helper",
            pkg_process::RunError {
                cancelled: Some(libc::SIGINT),
                source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe failed"),
            },
        )
        .with_dock_effect_possible();
        let cause = error
            .source()
            .expect("app cause")
            .source()
            .expect("supervision cause")
            .downcast_ref::<pkg_process::RunError>()
            .expect("original supervision error type");
        let io = cause
            .source()
            .expect("I/O cause")
            .downcast_ref::<std::io::Error>()
            .expect("original I/O error type");
        assert_eq!(error.signal(), Some(libc::SIGINT));
        assert_eq!(io.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "run app helper: pipe failed");
        assert!(error.dock_effect_possible());
    }
}
