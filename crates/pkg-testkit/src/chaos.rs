//! Deterministic process fault injection for crash-recovery tests.
//!
//! The child cooperates only long enough to publish a named checkpoint marker.
//! The parent owns the actual termination and always reaps the child. This keeps
//! production code free of test-only signal handling and avoids timing-only
//! sleeps as the synchronization boundary.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CHECKPOINT_ENV: &str = "PKG_TESTKIT_CHAOS_CHECKPOINT";
const MARKER_ENV: &str = "PKG_TESTKIT_CHAOS_MARKER";
const READY_ENV: &str = "PKG_TESTKIT_CHAOS_READY";
const NONCE_ENV: &str = "PKG_TESTKIT_CHAOS_NONCE";
const FSYNC_ENV: &str = "PKG_TESTKIT_CHAOS_FSYNC";
const MAX_CHECKPOINT_LEN: usize = 64;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
static NEXT_NONCE: AtomicU64 = AtomicU64::new(0);

/// A bounded, log-safe name for one injected crash checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChaosCheckpoint(String);

impl ChaosCheckpoint {
    /// Validates a lowercase ASCII checkpoint name.
    pub fn new(value: &str) -> Result<Self, ChaosConfigError> {
        if value.is_empty()
            || value.len() > MAX_CHECKPOINT_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ChaosConfigError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the validated checkpoint name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a cooperative checkpoint marker is durably synchronized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsyncMode {
    /// Call `sync_all` before publishing checkpoint readiness.
    Enabled,
    /// Deliberately skip `sync_all` to model a durability fault.
    Disabled,
}

impl FsyncMode {
    const fn as_env(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

/// A closed validation error for unsafe chaos configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChaosConfigError;

impl fmt::Display for ChaosConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid chaos configuration")
    }
}

impl std::error::Error for ChaosConfigError {}

/// Failure while synchronizing with or terminating a chaos child.
#[derive(Debug)]
pub enum ChaosError {
    /// An operating-system process or filesystem operation failed.
    Io(io::Error),
    /// The child exited before publishing its configured checkpoint.
    ExitedBeforeCheckpoint(ExitStatus),
    /// The bounded checkpoint wait elapsed.
    CheckpointTimeout,
}

impl fmt::Display for ChaosError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("chaos harness I/O failure"),
            Self::ExitedBeforeCheckpoint(_) => {
                formatter.write_str("chaos child exited before checkpoint")
            }
            Self::CheckpointTimeout => formatter.write_str("chaos checkpoint timed out"),
        }
    }
}

impl std::error::Error for ChaosError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::ExitedBeforeCheckpoint(_) | Self::CheckpointTimeout => None,
        }
    }
}

impl From<io::Error> for ChaosError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// A scrubbed child command configured for one cooperative checkpoint.
#[derive(Debug)]
pub struct ChaosCommand {
    command: Command,
    checkpoint: ChaosCheckpoint,
    marker: PathBuf,
    fsync: FsyncMode,
}

impl ChaosCommand {
    /// Creates a command for an absolute executable path and clears its inherited environment.
    pub fn new(
        program: impl AsRef<Path>,
        checkpoint: ChaosCheckpoint,
        marker: impl Into<PathBuf>,
        fsync: FsyncMode,
    ) -> Result<Self, ChaosConfigError> {
        let program = program.as_ref();
        let marker = marker.into();
        if !program.is_absolute()
            || !marker.is_absolute()
            || marker.parent().is_none()
            || marker.file_name().is_none()
        {
            return Err(ChaosConfigError);
        }
        let mut command = Command::new(program);
        command.env_clear();
        Ok(Self {
            command,
            checkpoint,
            marker,
            fsync,
        })
    }

    /// Appends one opaque child argument.
    pub fn arg(&mut self, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.arg(value);
        self
    }

    /// Adds one explicitly allowlisted environment entry for the fixture child.
    pub fn env(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.env(name, value);
        self
    }

    /// Spawns the configured child and returns the checkpoint-aware handle.
    pub fn spawn(&mut self) -> Result<ChaosChild, ChaosError> {
        let readiness = companion_path(&self.marker, ".ready")?;
        if self.marker.try_exists()? || readiness.try_exists()? {
            return Err(ChaosError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "chaos rendezvous already exists",
            )));
        }
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            NEXT_NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let marker_contents = format!("checkpoint={};nonce={nonce}\n", self.checkpoint.as_str());
        let ready_contents = format!("ready={nonce}\n");
        self.command
            .env(CHECKPOINT_ENV, self.checkpoint.as_str())
            .env(MARKER_ENV, &self.marker)
            .env(READY_ENV, &readiness)
            .env(NONCE_ENV, &nonce)
            .env(FSYNC_ENV, self.fsync.as_env());
        Ok(ChaosChild {
            child: Some(self.command.spawn()?),
            marker: self.marker.clone(),
            readiness,
            marker_contents: marker_contents.into_bytes(),
            ready_contents: ready_contents.into_bytes(),
        })
    }
}

/// A child process that is reaped on every explicit exit path and on drop.
#[derive(Debug)]
pub struct ChaosChild {
    child: Option<Child>,
    marker: PathBuf,
    readiness: PathBuf,
    marker_contents: Vec<u8>,
    ready_contents: Vec<u8>,
}

impl ChaosChild {
    /// Waits until the child publishes its configured checkpoint marker.
    pub fn wait_for_checkpoint(&mut self, timeout: Duration) -> Result<(), ChaosError> {
        let started = Instant::now();
        loop {
            if read_exact_if_present(&self.readiness, &self.ready_contents)? {
                if read_exact_if_present(&self.marker, &self.marker_contents)? {
                    return Ok(());
                }
                return Err(ChaosError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "chaos readiness preceded checkpoint",
                )));
            }
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| ChaosError::Io(io::Error::other("chaos child already consumed")))?;
            if let Some(status) = child.try_wait()? {
                self.child = None;
                return Err(ChaosError::ExitedBeforeCheckpoint(status));
            }
            if started.elapsed() >= timeout {
                return Err(ChaosError::CheckpointTimeout);
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Waits for the checkpoint, terminates the child, and reaps it.
    pub fn kill_at_checkpoint(&mut self, timeout: Duration) -> Result<ExitStatus, ChaosError> {
        self.wait_for_checkpoint(timeout)?;
        self.terminate()
    }

    /// Terminates and reaps the child, or returns its already-completed status.
    pub fn terminate(&mut self) -> Result<ExitStatus, ChaosError> {
        let mut child = self
            .child
            .take()
            .ok_or_else(|| ChaosError::Io(io::Error::other("chaos child already consumed")))?;
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        child.kill()?;
        Ok(child.wait()?)
    }
}

impl Drop for ChaosChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Publishes a durable marker, signals readiness, and parks at the checkpoint.
///
/// Returns `false` when the process was not configured for `checkpoint`. A
/// configured process does not return: after signaling, it remains parked until
/// the parent terminates it through [`ChaosChild::kill_at_checkpoint`].
pub fn publish_checkpoint(checkpoint: &ChaosCheckpoint) -> Result<bool, ChaosError> {
    if std::env::var_os(CHECKPOINT_ENV).as_deref() != Some(OsStr::new(checkpoint.as_str())) {
        return Ok(false);
    }
    let marker = std::env::var_os(MARKER_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos marker missing")))?;
    let readiness = std::env::var_os(READY_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos readiness path missing")))?;
    let nonce = std::env::var(NONCE_ENV)
        .map_err(|_| ChaosError::Io(io::Error::other("chaos nonce missing")))?;
    if readiness != companion_path(&marker, ".ready")? {
        return Err(ChaosError::Io(io::Error::other(
            "chaos readiness path mismatch",
        )));
    }
    let marker_contents = format!("checkpoint={};nonce={nonce}\n", checkpoint.as_str());
    let ready_contents = format!("ready={nonce}\n");
    let durable = std::env::var_os(FSYNC_ENV).as_deref() == Some(OsStr::new("enabled"));
    atomic_publish(&marker, marker_contents.as_bytes(), &nonce, durable)?;
    atomic_publish(&readiness, ready_contents.as_bytes(), &nonce, false)?;
    loop {
        thread::park();
    }
}

fn companion_path(path: &Path, suffix: &str) -> Result<PathBuf, ChaosError> {
    let parent = path
        .parent()
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos path parent missing")))?;
    let mut name = path
        .file_name()
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos path name missing")))?
        .to_os_string();
    name.push(suffix);
    Ok(parent.join(name))
}

fn atomic_publish(
    target: &Path,
    contents: &[u8],
    nonce: &str,
    durable: bool,
) -> Result<(), ChaosError> {
    let parent = target
        .parent()
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos target parent missing")))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| ChaosError::Io(io::Error::other("chaos target name missing")))?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".{nonce}.tmp"));
    let temporary = parent.join(temporary_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(contents)?;
    if durable {
        file.sync_all()?;
    }
    drop(file);
    if let Err(error) = std::fs::hard_link(&temporary, target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    std::fs::remove_file(&temporary)?;
    if durable {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_exact_if_present(path: &Path, expected: &[u8]) -> Result<bool, ChaosError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes == expected => Ok(true),
        Ok(_) => Err(ChaosError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "chaos rendezvous mismatch",
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

    fn marker() -> PathBuf {
        let sequence = NEXT_MARKER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pkg-chaos-{}-{sequence}", std::process::id()))
    }

    #[test]
    fn checkpoint_helper() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PKG_TESTKIT_HELPER").is_none() {
            return Ok(());
        }
        let checkpoint = ChaosCheckpoint::new("after-write")?;
        let _ = publish_checkpoint(&checkpoint)?;
        if let Some(path) = std::env::var_os("PKG_TESTKIT_POST_CHECKPOINT") {
            std::fs::write(path, b"continued\n")?;
        }
        Ok(())
    }

    #[test]
    fn child_is_killed_only_after_durable_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
        let marker = marker();
        let post_checkpoint = marker.with_extension("post");
        let executable = std::env::current_exe()?;
        let mut command = ChaosCommand::new(
            executable,
            ChaosCheckpoint::new("after-write")?,
            marker.clone(),
            FsyncMode::Enabled,
        )?;
        command
            .arg("--exact")
            .arg("chaos::tests::checkpoint_helper")
            .arg("--nocapture")
            .env("PKG_TESTKIT_HELPER", "1")
            .env("PKG_TESTKIT_POST_CHECKPOINT", &post_checkpoint);
        let mut child = command.spawn()?;
        let status = child.kill_at_checkpoint(Duration::from_secs(5))?;
        assert!(!status.success());
        let marker_bytes = std::fs::read(&marker)?;
        assert!(String::from_utf8(marker_bytes)?.starts_with("checkpoint=after-write;nonce="));
        let readiness = companion_path(&marker, ".ready")?;
        assert!(String::from_utf8(std::fs::read(&readiness)?)?.starts_with("ready="));
        assert!(!post_checkpoint.exists());
        std::fs::remove_file(marker)?;
        std::fs::remove_file(readiness)?;
        Ok(())
    }

    #[test]
    fn checkpoint_names_and_paths_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        assert!(ChaosCheckpoint::new("After_Write").is_err());
        assert!(ChaosCheckpoint::new(&"a".repeat(MAX_CHECKPOINT_LEN + 1)).is_err());
        let executable = std::env::current_exe()?;
        assert!(
            ChaosCommand::new(
                &executable,
                ChaosCheckpoint::new("valid")?,
                PathBuf::from("relative-marker"),
                FsyncMode::Disabled,
            )
            .is_err()
        );

        let stale_marker = marker();
        std::fs::write(&stale_marker, b"stale\n")?;
        let mut command = ChaosCommand::new(
            &executable,
            ChaosCheckpoint::new("valid")?,
            stale_marker.clone(),
            FsyncMode::Enabled,
        )?;
        assert!(command.spawn().is_err());
        std::fs::remove_file(stale_marker)?;

        let marker = marker();
        let readiness = companion_path(&marker, ".ready")?;
        std::fs::write(&readiness, b"stale\n")?;
        let mut command = ChaosCommand::new(
            &executable,
            ChaosCheckpoint::new("valid")?,
            marker,
            FsyncMode::Enabled,
        )?;
        assert!(command.spawn().is_err());
        std::fs::remove_file(readiness)?;
        Ok(())
    }
}
