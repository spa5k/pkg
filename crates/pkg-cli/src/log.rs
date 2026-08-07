//! Bounded structured local logs with an allowlisted schema and denylist redaction.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::ux::PUBLIC_SCHEMA_VERSION;

const MAX_SECRET_COUNT: usize = 64;
const MAX_SECRET_CHARS: usize = 4_096;
const MAX_REDACTED_CHARS: usize = 2_048;

/// Log severity permitted by the product-owned schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Normal lifecycle information.
    Info,
    /// Recoverable or degraded behavior.
    Warning,
    /// A command or subsystem failed.
    Error,
}

/// Redacted text that cannot be constructed without crossing the redaction boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RedactedText(String);

/// Bounded denylist redactor. It never reads process arguments or the environment.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Construct a redactor from explicitly supplied secret values.
    pub fn new(secrets: impl IntoIterator<Item = impl AsRef<str>>) -> io::Result<Self> {
        let mut bounded = Vec::new();
        for secret in secrets {
            let secret = secret.as_ref();
            if secret.is_empty() {
                continue;
            }
            if bounded.len() == MAX_SECRET_COUNT || secret.chars().count() > MAX_SECRET_CHARS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "redactor denylist exceeds its bound",
                ));
            }
            bounded.push(secret.to_owned());
        }
        Ok(Self { secrets: bounded })
    }

    /// Redact known secrets, private runtime identities, credential-like assignments, and controls.
    #[must_use]
    pub fn redact(&self, value: &str) -> RedactedText {
        let mut text = value.to_owned();
        for secret in &self.secrets {
            text = text.replace(secret, "[redacted]");
        }
        let mut escaped = String::new();
        for character in text.chars().take(MAX_REDACTED_CHARS) {
            if character.is_control() {
                use std::fmt::Write as _;
                escaped.push(' ');
                let _ = write!(escaped, "\\u{:04x}", u32::from(character));
                escaped.push(' ');
            } else {
                escaped.push(character);
            }
        }
        RedactedText(
            escaped
                .split_whitespace()
                .map(redact_token)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

fn redact_token(token: &str) -> &str {
    let lower = token.to_ascii_lowercase();
    if token.contains("/nix/store/")
        || token.ends_with(".drv")
        || token.starts_with("github:")
        || token.starts_with("flake:")
        || [
            "token=",
            "password=",
            "secret=",
            "authorization=",
            "cookie=",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        "[redacted]"
    } else {
        token
    }
}

/// One allowlisted structured log record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    schema_version: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp_ms: u64,
    level: LogLevel,
    event: &'static str,
    command: &'static str,
    code: Option<u8>,
    detail: Option<RedactedText>,
}

impl LogRecord {
    /// Record a command lifecycle event. `command` and `event` must be static product values.
    #[must_use]
    pub fn command(
        level: LogLevel,
        event: &'static str,
        command: &'static str,
        code: Option<u8>,
    ) -> Self {
        Self {
            schema_version: PUBLIC_SCHEMA_VERSION,
            kind: "log",
            timestamp_ms: now_ms(),
            level,
            event,
            command,
            code,
            detail: None,
        }
    }

    /// Attach already-redacted bounded diagnostic text.
    #[must_use]
    pub fn with_detail(mut self, detail: RedactedText) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// Rotation and retention bounds for local logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogConfig {
    /// Maximum size of one log file.
    pub max_file_bytes: u64,
    /// Number of files retained, including the active file.
    pub max_files: u8,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: 5 * 1024 * 1024,
            max_files: 10,
        }
    }
}

/// Size-capped structured log writer rooted in a private product directory.
#[derive(Debug)]
pub struct StructuredLog {
    directory: PathBuf,
    config: LogConfig,
}

impl StructuredLog {
    /// Open a log directory after validating the retention bounds and private directory mode.
    pub fn open(directory: impl Into<PathBuf>, config: LogConfig) -> io::Result<Self> {
        if config.max_file_bytes < 1_024 || config.max_file_bytes > 100 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log size bound",
            ));
        }
        if !(1..=20).contains(&config.max_files) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log file count",
            ));
        }
        let directory = directory.into();
        ensure_private_dir(&directory)?;
        Ok(Self { directory, config })
    }

    /// Append exactly one compact JSON record, rotating before the configured cap is exceeded.
    pub fn append(&self, record: &LogRecord) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.config.max_file_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log record exceeds file cap",
            ));
        }
        let active = self.directory.join("pkg.log");
        refuse_symlink(&active)?;
        if fs::metadata(&active)
            .map(|metadata| {
                metadata.len().saturating_add(bytes.len() as u64) > self.config.max_file_bytes
            })
            .unwrap_or(false)
        {
            self.rotate()?;
        }
        let mut file = open_private_append(&active)?;
        file.write_all(&bytes)?;
        file.sync_data()
    }

    fn rotate(&self) -> io::Result<()> {
        if self.config.max_files == 1 {
            let active = self.directory.join("pkg.log");
            if active.exists() {
                fs::remove_file(active)?;
            }
            return Ok(());
        }
        for index in (1..self.config.max_files).rev() {
            let source = if index == 1 {
                self.directory.join("pkg.log")
            } else {
                self.directory.join(format!("pkg.log.{}", index - 1))
            };
            let target = self.directory.join(format!("pkg.log.{index}"));
            refuse_symlink(&source)?;
            refuse_symlink(&target)?;
            if target.exists() {
                fs::remove_file(&target)?;
            }
            if source.exists() {
                fs::rename(source, target)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn write_private_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    ensure_private_dir(parent)?;
    refuse_symlink(path)?;
    let temporary = parent.join(format!(".pkg-{}.tmp", std::process::id()));
    refuse_symlink(&temporary)?;
    let mut file = open_private_create_new(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    let created = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            true
        }
        Err(error) => return Err(error),
    };
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private path is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if created {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        } else if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory permissions are too broad",
            ));
        }
    }
    Ok(())
}

fn refuse_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink refused",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        let file = options.open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    options.open(path)
}

fn open_private_create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pkg-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn redactor_golden_removes_known_and_structural_secrets_and_controls() {
        let redactor = Redactor::new(["known-value"]).unwrap();
        let value =
            redactor.redact("token=abc known-value /nix/store/aaaaaaaa-x github:org/private\nnext");
        assert_eq!(
            value.0,
            "[redacted] [redacted] [redacted] [redacted] \\u000a next"
        );
    }

    #[test]
    #[cfg(unix)]
    fn existing_permissive_directory_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp("permissive");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(StructuredLog::open(&root, LogConfig::default()).is_err());
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_dir(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn log_is_private_structured_and_rotates_with_allowlisted_fields() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp("log");
        let log = StructuredLog::open(
            &root,
            LogConfig {
                max_file_bytes: 1_024,
                max_files: 2,
            },
        )
        .unwrap();
        let redactor = Redactor::new(["secret-value"]).unwrap();
        for _ in 0..20 {
            log.append(
                &LogRecord::command(LogLevel::Info, "command_finished", "install", Some(0))
                    .with_detail(redactor.redact("secret-value")),
            )
            .unwrap();
        }
        let active = root.join("pkg.log");
        assert_eq!(
            fs::metadata(&active).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(root.join("pkg.log.1").is_file());
        let text = fs::read_to_string(active).unwrap();
        assert!(text.contains("command_finished"));
        assert!(!text.contains("secret-value"));
        assert!(!text.contains("argv"));
        assert!(!text.contains("environment"));
        fs::remove_dir_all(root).unwrap();
    }
}
