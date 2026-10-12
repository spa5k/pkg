use crate::nix::NixError;
use std::fmt;

#[derive(Debug)]
pub struct AppError {
    context: String,
    cause: AppErrorCause,
    dock_effect_possible: bool,
}

#[derive(Debug)]
enum AppErrorCause {
    Message(String),
    Native(NixError),
    Interrupted(i32),
}

impl AppError {
    pub(super) fn message(message: impl Into<String>) -> Self {
        Self {
            context: String::new(),
            cause: AppErrorCause::Message(message.into()),
            dock_effect_possible: false,
        }
    }

    pub(super) fn native(context: impl Into<String>, cause: NixError) -> Self {
        Self {
            context: context.into(),
            cause: AppErrorCause::Native(cause),
            dock_effect_possible: false,
        }
    }

    pub(super) fn interrupted(context: impl Into<String>, signal: i32) -> Self {
        Self {
            context: context.into(),
            cause: AppErrorCause::Interrupted(signal),
            dock_effect_possible: false,
        }
    }

    pub(crate) fn signal(&self) -> Option<i32> {
        match &self.cause {
            AppErrorCause::Interrupted(signal)
            | AppErrorCause::Native(NixError::Interrupted { signal, .. }) => Some(*signal),
            AppErrorCause::Message(_) | AppErrorCause::Native(_) => None,
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

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.context.is_empty() {
            write!(formatter, "{}: ", self.context)?;
        }
        match &self.cause {
            AppErrorCause::Message(message) => formatter.write_str(message),
            AppErrorCause::Native(error) => error.fmt(formatter),
            AppErrorCause::Interrupted(signal) => {
                write!(formatter, "was interrupted by signal {signal}")
            }
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.cause {
            AppErrorCause::Native(error) => Some(error),
            AppErrorCause::Message(_) | AppErrorCause::Interrupted(_) => None,
        }
    }
}
