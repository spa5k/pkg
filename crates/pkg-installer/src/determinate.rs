//! Closed process boundary for the pinned Determinate Nix Installer.

use pkg_core::state::Digest;
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
};

const DIAGNOSTIC_ENDPOINT: &str = "http://127.0.0.1:18080";
const INSTALLED_INSTALLER: &str = "/nix/nix-installer";
const RECEIPT: &str = "/nix/receipt.json";
const OUTPUT_LIMIT: usize = 256 * 1024;

#[cfg(target_os = "linux")]
const HOME: &str = "/root";
#[cfg(target_os = "macos")]
const HOME: &str = "/var/root";
#[cfg(target_os = "linux")]
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
#[cfg(target_os = "macos")]
const PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(target_os = "linux")]
const TMPDIR: &str = "/var/lib/pkg-install/tmp";
#[cfg(target_os = "macos")]
const TMPDIR: &str = "/private/var/db/pkg-install-tmp";

/// Closed adapter for one exact Determinate Nix Installer executable.
pub struct DeterminateInstaller {
    length: u64,
    sha256: Digest,
}

impl DeterminateInstaller {
    /// Binds the adapter to authenticated release metadata.
    #[must_use]
    pub const fn new(length: u64, sha256: Digest) -> Self {
        Self { length, sha256 }
    }

    /// Runs the closed install operation from its staged executable.
    pub fn install(
        &self,
        staged_executable: &Path,
    ) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
        run_production(staged_executable, self, Operation::Install)
    }

    /// Runs the closed uninstall operation through the fixed installed vendor
    /// executable and fixed opaque receipt for install rollback only.
    pub fn uninstall(&self) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
        run_production(Path::new(INSTALLED_INSTALLER), self, Operation::Uninstall)
    }

    /// Replaces the current process with the fixed vendor uninstall operation.
    ///
    /// This terminal boundary is used only after all product state is removed.
    /// On success, this function does not return.
    pub fn exec_uninstall(&self) -> Result<(), DeterminateProcessError> {
        let executable = Path::new(INSTALLED_INSTALLER);
        authenticate_executable(executable, self, 0, Path::new("/"))?;
        let error =
            terminal_uninstall_command(executable, OsStr::new(HOME), OsStr::new(PATH)).exec();
        Err(match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                DeterminateProcessError::InvalidExecutable
            }
            _ => DeterminateProcessError::SpawnFailed,
        })
    }
}

/// How the vendor process terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterminateTerminal {
    /// The process called exit or returned from `main`.
    Exited(i32),
    /// The process was terminated by a signal.
    Signaled(i32),
}

/// A process observation. It is not proof that Base Nix is installed or absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeterminateProcessOutcome {
    /// The final process terminal state.
    pub terminal: DeterminateTerminal,
    /// Whether standard output exceeded the retained 256 KiB diagnostic cap.
    pub stdout_truncated: bool,
    /// Whether standard error exceeded the retained 256 KiB diagnostic cap.
    pub stderr_truncated: bool,
}

impl fmt::Display for DeterminateProcessOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal={:?}, stdout_truncated={}, stderr_truncated={}",
            self.terminal, self.stdout_truncated, self.stderr_truncated
        )
    }
}

/// Redacted process-boundary failure classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterminateProcessError {
    /// The executable failed identity or filesystem checks.
    InvalidExecutable,
    /// The fixed private temporary directory failed filesystem checks.
    InvalidEnvironment,
    /// The authenticated executable could not be spawned.
    SpawnFailed,
    /// The child could not be waited for and reaped.
    WaitFailed,
    /// A captured stream could not be drained.
    OutputFailed,
}

impl fmt::Display for DeterminateProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidExecutable => "invalid vendor executable",
            Self::InvalidEnvironment => "invalid process environment",
            Self::SpawnFailed => "vendor process spawn failed",
            Self::WaitFailed => "vendor process wait failed",
            Self::OutputFailed => "vendor process output failed",
        })
    }
}

impl std::error::Error for DeterminateProcessError {}

#[derive(Clone, Copy)]
enum Operation {
    Install,
    Uninstall,
}

impl Operation {
    const fn arguments(self) -> &'static [&'static str] {
        match self {
            Self::Install => &[
                "--diagnostic-endpoint",
                DIAGNOSTIC_ENDPOINT,
                "install",
                "--determinate",
                "--no-confirm",
                "--no-modify-profile",
            ],
            Self::Uninstall => &[
                "--diagnostic-endpoint",
                DIAGNOSTIC_ENDPOINT,
                "uninstall",
                "--no-confirm",
                RECEIPT,
            ],
        }
    }
}

struct ProcessSettings<'a> {
    home: &'a OsStr,
    path: &'a OsStr,
    tmpdir: &'a Path,
    trust_root: &'a Path,
    owner: u32,
}

struct CapturedOutcome {
    public: DeterminateProcessOutcome,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CapturedOutcome {
    fn write_failure_diagnostics(&self, writer: &mut impl Write) {
        if self.public.terminal == DeterminateTerminal::Exited(0) {
            return;
        }
        let _ = writeln!(writer, "determinate installer outcome: {}", self.public);
    }
}

fn write_process_error_diagnostic(error: DeterminateProcessError, writer: &mut impl Write) {
    let _ = writeln!(writer, "determinate installer error: {error}");
}

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

fn production_settings() -> ProcessSettings<'static> {
    ProcessSettings {
        home: OsStr::new(HOME),
        path: OsStr::new(PATH),
        tmpdir: Path::new(TMPDIR),
        trust_root: Path::new("/"),
        owner: 0,
    }
}

fn run_production(
    executable: &Path,
    installer: &DeterminateInstaller,
    operation: Operation,
) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
    let captured = match run(executable, installer, operation, &production_settings()) {
        Ok(captured) => captured,
        Err(error) => {
            write_process_error_diagnostic(error, &mut io::stderr().lock());
            return Err(error);
        }
    };
    captured.write_failure_diagnostics(&mut io::stderr().lock());
    drop(captured.stdout);
    drop(captured.stderr);
    Ok(captured.public)
}

fn run(
    executable: &Path,
    installer: &DeterminateInstaller,
    operation: Operation,
    settings: &ProcessSettings<'_>,
) -> Result<CapturedOutcome, DeterminateProcessError> {
    run_with_process(
        executable,
        installer,
        operation,
        settings,
        Command::spawn,
        std::process::Child::wait,
    )
}

fn run_with_process(
    executable: &Path,
    installer: &DeterminateInstaller,
    operation: Operation,
    settings: &ProcessSettings<'_>,
    spawn: impl FnOnce(&mut Command) -> io::Result<std::process::Child>,
    wait: impl FnOnce(&mut std::process::Child) -> io::Result<ExitStatus>,
) -> Result<CapturedOutcome, DeterminateProcessError> {
    authenticate_executable(executable, installer, settings.owner, settings.trust_root)?;
    validate_private_tmpdir(settings.tmpdir, settings.owner, settings.trust_root)
        .map_err(|()| DeterminateProcessError::InvalidEnvironment)?;

    let mut command = Command::new(executable);
    #[cfg(test)]
    command.env("PKG_C06_AMBIENT_SECRET", "must-not-survive-env-clear");
    command
        .env_clear()
        .env("HOME", settings.home)
        .env("PATH", settings.path)
        .env("TMPDIR", settings.tmpdir)
        .env("DETSYS_IDS_TELEMETRY", "disabled")
        .args(operation.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(&mut command).map_err(|_| DeterminateProcessError::SpawnFailed)?;
    let (stdout, stderr) = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => (stdout, stderr),
        pipes => {
            drop(pipes);
            let _ = child.wait();
            return Err(DeterminateProcessError::OutputFailed);
        }
    };
    let Ok(stdout_reader) = thread::Builder::new()
        .name("determinate-stdout".into())
        .spawn(move || drain(stdout))
    else {
        drop(stderr);
        let _ = child.wait();
        return Err(DeterminateProcessError::OutputFailed);
    };
    let Ok(stderr_reader) = thread::Builder::new()
        .name("determinate-stderr".into())
        .spawn(move || drain(stderr))
    else {
        let _ = child.wait();
        let _ = stdout_reader.join();
        return Err(DeterminateProcessError::OutputFailed);
    };
    let waited = wait(&mut child);
    if waited.is_err() {
        let _ = child.wait();
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| DeterminateProcessError::OutputFailed)
        .and_then(|result| result.map_err(|_| DeterminateProcessError::OutputFailed));
    let stderr = stderr_reader
        .join()
        .map_err(|_| DeterminateProcessError::OutputFailed)
        .and_then(|result| result.map_err(|_| DeterminateProcessError::OutputFailed));
    let status = waited.map_err(|_| DeterminateProcessError::WaitFailed)?;
    let stderr = stderr?;
    let stdout = stdout?;
    let terminal = terminal(status)?;
    Ok(CapturedOutcome {
        public: DeterminateProcessOutcome {
            terminal,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        },
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "test double keeps the production visibility contract"
)]
pub(super) fn run_test_install_with_process(
    executable: &Path,
    installer: &DeterminateInstaller,
    root: &Path,
    spawn: impl FnOnce(&mut Command) -> io::Result<std::process::Child>,
    wait: impl FnOnce(&mut std::process::Child) -> io::Result<ExitStatus>,
) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
    let owner = fs::metadata(root)
        .map_err(|_| DeterminateProcessError::InvalidEnvironment)?
        .uid();
    run_with_process(
        executable,
        installer,
        Operation::Install,
        &ProcessSettings {
            home: OsStr::new("/root"),
            path: OsStr::new("/usr/bin:/bin"),
            tmpdir: root,
            trust_root: root,
            owner,
        },
        spawn,
        wait,
    )
    .map(|captured| captured.public)
}

fn terminal_uninstall_command(executable: &Path, home: &OsStr, path: &OsStr) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("DETSYS_IDS_TELEMETRY", "disabled")
        .args(["uninstall", "--no-confirm", RECEIPT]);
    command
}

/// Root ownership plus a non-writable full parent chain is the immutability
/// boundary between this check and spawn. A hostile root process is out of the
/// product threat model because it can control either operation.
fn authenticate_executable(
    path: &Path,
    installer: &DeterminateInstaller,
    owner: u32,
    trust_root: &Path,
) -> Result<(), DeterminateProcessError> {
    if !path.is_absolute() || !trust_root.is_absolute() || !path.starts_with(trust_root) {
        return Err(DeterminateProcessError::InvalidExecutable);
    }
    validate_directory_chain(
        path.parent()
            .ok_or(DeterminateProcessError::InvalidExecutable)?,
        owner,
        trust_root,
    )
    .map_err(|()| DeterminateProcessError::InvalidExecutable)?;
    let path_metadata =
        fs::symlink_metadata(path).map_err(|_| DeterminateProcessError::InvalidExecutable)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.uid() != owner
        || path_metadata.mode() & 0o022 != 0
        || path_metadata.mode() & 0o111 == 0
        || path_metadata.len() != installer.length
    {
        return Err(DeterminateProcessError::InvalidExecutable);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| DeterminateProcessError::InvalidExecutable)?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| DeterminateProcessError::InvalidExecutable)?;
    let digest = file_digest(&mut file)?;
    let current_metadata =
        fs::symlink_metadata(path).map_err(|_| DeterminateProcessError::InvalidExecutable)?;
    if opened_metadata.dev() != path_metadata.dev()
        || opened_metadata.ino() != path_metadata.ino()
        || current_metadata.file_type().is_symlink()
        || current_metadata.dev() != opened_metadata.dev()
        || current_metadata.ino() != opened_metadata.ino()
        || current_metadata.len() != installer.length
        || digest != installer.sha256
    {
        return Err(DeterminateProcessError::InvalidExecutable);
    }
    Ok(())
}

fn validate_private_tmpdir(path: &Path, owner: u32, trust_root: &Path) -> Result<(), ()> {
    validate_directory_chain(path, owner, trust_root)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.mode() & 0o022 == 0 {
        Ok(())
    } else {
        Err(())
    }
}

fn validate_directory_chain(path: &Path, owner: u32, trust_root: &Path) -> Result<(), ()> {
    if !path.is_absolute() || !trust_root.is_absolute() || !path.starts_with(trust_root) {
        return Err(());
    }
    let mut current = path;
    loop {
        let metadata = fs::symlink_metadata(current).map_err(|_| ())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.mode() & 0o022 != 0
        {
            return Err(());
        }
        if current == trust_root {
            return Ok(());
        }
        current = current.parent().ok_or(())?;
    }
}

fn file_digest(file: &mut File) -> Result<Digest, DeterminateProcessError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| DeterminateProcessError::InvalidExecutable)?;
        if read == 0 {
            return Ok(Digest::from_bytes(hasher.finalize().into()));
        }
        hasher.update(&buffer[..read]);
    }
}

fn drain(mut reader: impl Read) -> io::Result<CapturedStream> {
    let mut bytes = Vec::with_capacity(OUTPUT_LIMIT);
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(CapturedStream { bytes, truncated });
        }
        let retained = read.min(OUTPUT_LIMIT.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained != read;
    }
}

fn terminal(status: ExitStatus) -> Result<DeterminateTerminal, DeterminateProcessError> {
    match (status.code(), status.signal()) {
        (Some(code), _) => Ok(DeterminateTerminal::Exited(code)),
        (None, Some(signal)) => Ok(DeterminateTerminal::Signaled(signal)),
        (None, None) => Err(DeterminateProcessError::WaitFailed),
    }
}

#[cfg(test)]
mod tests;
