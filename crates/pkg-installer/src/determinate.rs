//! Closed process boundary for the pinned Determinate Nix Installer.

use pkg_core::state::Digest;
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const DIAGNOSTIC_ENDPOINT: &str = "http://127.0.0.1:18080";
const INSTALLED_INSTALLER: &str = "/nix/nix-installer";
const RECEIPT: &str = "/nix/receipt.json";
const OUTPUT_LIMIT: usize = 256 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_hours(2);

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
const TMPDIR: &str = "/private/var/db/pkg-install/tmp";

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
    /// executable and fixed opaque receipt.
    pub fn uninstall(&self) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
        run_production(Path::new(INSTALLED_INSTALLER), self, Operation::Uninstall)
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
    /// Whether the process was still running when the fixed deadline passed.
    pub timed_out: bool,
    /// Whether standard output exceeded the retained 256 KiB diagnostic cap.
    pub stdout_truncated: bool,
    /// Whether standard error exceeded the retained 256 KiB diagnostic cap.
    pub stderr_truncated: bool,
}

impl fmt::Display for DeterminateProcessOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal={:?}, timed_out={}, stdout_truncated={}, stderr_truncated={}",
            self.terminal, self.timed_out, self.stdout_truncated, self.stderr_truncated
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
    timeout: Duration,
}

struct CapturedOutcome {
    public: DeterminateProcessOutcome,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
        timeout: COMMAND_TIMEOUT,
    }
}

fn run_production(
    executable: &Path,
    installer: &DeterminateInstaller,
    operation: Operation,
) -> Result<DeterminateProcessOutcome, DeterminateProcessError> {
    let captured = run(executable, installer, operation, &production_settings())?;
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
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| DeterminateProcessError::SpawnFailed)?;
    let started = Instant::now();
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
    let waited = child.wait();
    let timed_out = started.elapsed() >= settings.timeout;
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
            timed_out,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        },
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
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
    if fs::symlink_metadata(path).map_err(|_| ())?.mode() & 0o7777 == 0o700 {
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
mod tests {
    use super::*;
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn write_script(root: &Path, body: &str) -> Result<std::path::PathBuf, io::Error> {
        let directory = root.join("bin");
        fs::create_dir_all(&directory)?;
        let path = directory.join("nix-installer");
        if path.exists() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
        Ok(path)
    }

    fn identity(path: &Path) -> Result<DeterminateInstaller, io::Error> {
        let bytes = fs::read(path)?;
        Ok(DeterminateInstaller::new(
            u64::try_from(bytes.len()).map_err(io::Error::other)?,
            Digest::from_bytes(Sha256::digest(bytes).into()),
        ))
    }

    fn settings(root: &Path, timeout: Duration) -> Result<ProcessSettings<'_>, io::Error> {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        Ok(ProcessSettings {
            home: OsStr::new("/fixed-root-home"),
            path: OsStr::new("/usr/bin:/bin"),
            tmpdir: root,
            trust_root: root,
            owner: nix::unistd::Uid::effective().as_raw(),
            timeout,
        })
    }

    #[test]
    fn operations_use_exact_argv_and_cleared_environment() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(
            temporary.path(),
            r#"
printf 'HOME=%s\nPATH=%s\nTMPDIR=%s\nTELEMETRY=%s\nAMBIENT=%s\n' "$HOME" "$PATH" "$TMPDIR" "$DETSYS_IDS_TELEMETRY" "${PKG_C06_AMBIENT_SECRET-unset}"
for argument in "$@"; do printf 'ARG=<%s>\n' "$argument"; done
"#,
        )?;
        let identity = identity(&executable)?;
        let settings = settings(temporary.path(), Duration::from_secs(2))?;
        for (operation, expected) in [
            (
                Operation::Install,
                "ARG=<--diagnostic-endpoint>\nARG=<http://127.0.0.1:18080>\nARG=<install>\nARG=<--determinate>\nARG=<--no-confirm>\nARG=<--no-modify-profile>\n",
            ),
            (
                Operation::Uninstall,
                "ARG=<--diagnostic-endpoint>\nARG=<http://127.0.0.1:18080>\nARG=<uninstall>\nARG=<--no-confirm>\nARG=</nix/receipt.json>\n",
            ),
        ] {
            let result = run(&executable, &identity, operation, &settings)?;
            let output = String::from_utf8(result.stdout)?;
            assert!(output.starts_with(&format!(
                "HOME=/fixed-root-home\nPATH=/usr/bin:/bin\nTMPDIR={}\nTELEMETRY=disabled\nAMBIENT=unset\n",
                temporary.path().display()
            )));
            assert!(output.ends_with(expected));
            assert!(result.stderr.is_empty());
            assert_eq!(result.public.terminal, DeterminateTerminal::Exited(0));
        }
        Ok(())
    }

    #[test]
    fn executable_authentication_rejects_every_invalid_shape()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(temporary.path(), "exit 0")?;
        let valid = identity(&executable)?;
        let owner = nix::unistd::Uid::effective().as_raw();
        let root = temporary.path();

        assert!(authenticate_executable(Path::new("relative"), &valid, owner, root).is_err());
        assert!(authenticate_executable(&root.join("missing"), &valid, owner, root).is_err());
        let directory = root.join("not-a-file");
        fs::create_dir(&directory)?;
        assert!(authenticate_executable(&directory, &valid, owner, root).is_err());
        assert!(authenticate_executable(&executable, &valid, owner.wrapping_add(1), root).is_err());
        assert!(
            authenticate_executable(
                &executable,
                &DeterminateInstaller::new(valid.length + 1, valid.sha256),
                owner,
                root,
            )
            .is_err()
        );
        assert!(
            authenticate_executable(
                &executable,
                &DeterminateInstaller::new(valid.length, Digest::from_bytes([0; 32])),
                owner,
                root,
            )
            .is_err()
        );

        let link = root.join("linked-installer");
        symlink(&executable, &link)?;
        assert!(authenticate_executable(&link, &valid, owner, root).is_err());

        let ancestor_link = root.join("linked-dir");
        symlink(root.join("bin"), &ancestor_link)?;
        assert!(
            authenticate_executable(&ancestor_link.join("nix-installer"), &valid, owner, root)
                .is_err()
        );

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600))?;
        assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o522))?;
        assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;

        let parent = executable.parent().ok_or("missing parent")?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o722))?;
        assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;

        let unsafe_ancestor = root.join("unsafe-ancestor");
        let nested = write_script(&unsafe_ancestor, "exit 0")?;
        let nested_identity = identity(&nested)?;
        fs::set_permissions(&unsafe_ancestor, fs::Permissions::from_mode(0o722))?;
        assert!(authenticate_executable(&nested, &nested_identity, owner, root).is_err());

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        fs::write(
            &executable,
            fs::read(&executable)?
                .into_iter()
                .map(|byte| byte ^ 1)
                .collect::<Vec<_>>(),
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;
        assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
        Ok(())
    }

    #[test]
    fn trusted_non_writable_chain_is_the_spawn_immutability_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(temporary.path(), "exit 0")?;
        let owner = nix::unistd::Uid::effective().as_raw();
        authenticate_executable(
            &executable,
            &identity(&executable)?,
            owner,
            temporary.path(),
        )?;
        let mut current = executable.parent().ok_or("missing parent")?;
        loop {
            let metadata = fs::symlink_metadata(current)?;
            assert_eq!(metadata.uid(), owner);
            assert_eq!(metadata.mode() & 0o022, 0);
            if current == temporary.path() {
                break;
            }
            current = current.parent().ok_or("missing trusted root")?;
        }
        Ok(())
    }

    #[test]
    fn both_large_streams_are_drained_and_capped() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(
            temporary.path(),
            "i=0; while [ $i -lt 20000 ]; do printf '1234567890123456'; printf 'abcdefghijklmnop' >&2; i=$((i + 1)); done",
        )?;
        let result = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(10))?,
        )?;
        assert_eq!(result.stdout.len(), OUTPUT_LIMIT);
        assert_eq!(result.stderr.len(), OUTPUT_LIMIT);
        assert!(result.public.stdout_truncated);
        assert!(result.public.stderr_truncated);
        assert_eq!(result.public.terminal, DeterminateTerminal::Exited(0));
        Ok(())
    }

    #[test]
    fn exit_nonzero_and_signal_are_distinct() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(temporary.path(), "exit 23")?;
        let nonzero = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(2))?,
        )?;
        assert_eq!(nonzero.public.terminal, DeterminateTerminal::Exited(23));

        let executable = write_script(temporary.path(), "kill -TERM $$")?;
        let signaled = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(2))?,
        )?;
        assert_eq!(signaled.public.terminal, DeterminateTerminal::Signaled(15));
        Ok(())
    }

    #[test]
    fn timeout_observes_without_signaling_and_reaps() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(temporary.path(), "printf '%s' $$; sleep 0.1; exit 7")?;
        let started = Instant::now();
        let result = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_millis(10))?,
        )?;
        assert!(started.elapsed() >= Duration::from_millis(90));
        let pid = String::from_utf8(result.stdout)?.parse::<i32>()?;
        assert!(result.public.timed_out);
        assert_eq!(result.public.terminal, DeterminateTerminal::Exited(7));
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
        Ok(())
    }

    #[test]
    fn dropping_a_returned_result_cannot_orphan_the_child() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(temporary.path(), "printf '%s' $$; exit 0")?;
        let result = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(2))?,
        )?;
        let pid = std::str::from_utf8(&result.stdout)?.parse::<i32>()?;
        drop(result);
        assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
        Ok(())
    }

    #[test]
    fn private_tmpdir_requires_mode_0700() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let tmpdir = temporary.path().join("tmp");
        fs::create_dir(&tmpdir)?;
        let owner = nix::unistd::Uid::effective().as_raw();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o755))?;
        assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_err());
        fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o700))?;
        assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_ok());
        Ok(())
    }

    #[test]
    fn child_starts_as_its_own_process_group() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(
            temporary.path(),
            "pid=$$; pgid=$(ps -o pgid= -p \"$pid\" | tr -d ' '); printf '%s %s' \"$pid\" \"$pgid\"",
        )?;
        let result = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(2))?,
        )?;
        let output = String::from_utf8(result.stdout)?;
        let mut fields = output.split_whitespace();
        assert_eq!(fields.next(), fields.next());
        assert_eq!(fields.next(), None);
        Ok(())
    }

    #[test]
    fn diagnostics_never_expose_captured_bytes_or_paths() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let executable = write_script(
            temporary.path(),
            "printf 'fake-secret'; printf 'fake-secret' >&2; exit 4",
        )?;
        let result = run(
            &executable,
            &identity(&executable)?,
            Operation::Install,
            &settings(temporary.path(), Duration::from_secs(2))?,
        )?;
        for rendered in [format!("{:?}", result.public), result.public.to_string()] {
            assert!(!rendered.contains("fake-secret"));
            assert!(!rendered.contains(&executable.display().to_string()));
            assert!(!rendered.contains("--diagnostic-endpoint"));
        }
        let secret_path = Path::new("/fake-secret/missing");
        let Err(error) =
            authenticate_executable(secret_path, &identity(&executable)?, 0, Path::new("/"))
        else {
            return Err("missing executable did not fail".into());
        };
        for rendered in [format!("{error:?}"), error.to_string()] {
            assert!(!rendered.contains("fake-secret"));
        }
        Ok(())
    }

    #[test]
    fn operation_surface_has_no_update_route() {
        let operations = [Operation::Install, Operation::Uninstall];
        assert_eq!(operations.len(), 2);
        assert!(
            operations
                .iter()
                .all(|operation| !operation.arguments().contains(&"update"))
        );
        assert!(
            operations
                .iter()
                .all(|operation| !operation.arguments().contains(&"upgrade"))
        );
    }
}
