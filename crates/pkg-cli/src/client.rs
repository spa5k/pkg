//! The installed pkg client: its files and its updates.
//!
//! The installer owns one fixed layout under the user's home directory.
//! An update reads the checksum list of the newest published release,
//! downloads the archive for this system, and verifies it before anything
//! from it runs. The new client starts once from its final directory, then
//! replaces the old client with one rename, so a running pkg keeps working.

use std::path::{Path, PathBuf};

use crate::nix::Outcome;

/// The published releases of the client.
const RELEASES: &str = "https://github.com/spa5k/pkg/releases";

/// The version of this client.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Marker line of the managed block in Bash and Zsh startup files.
pub const BLOCK_BEGIN: &str = "# >>> pkg paths >>>";
/// End line of the managed block in Bash and Zsh startup files.
pub const BLOCK_END: &str = "# <<< pkg paths <<<";
/// First line of the pkg-owned Fish configuration file.
pub const FISH_MARKER: &str = "# Managed by the pkg installer.";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Process(#[from] pkg_process::RunError),
    #[error("{program} was cancelled by signal {signal}")]
    Interrupted { program: String, signal: i32 },
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Message(message.to_string())
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl Error {
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::Process(error) => error.cancelled,
            Self::Interrupted { signal, .. } => Some(*signal),
            Self::Message(_) | Self::Io(_) => None,
        }
    }
}

/// The installer layout for one user.
pub struct Layout {
    home: PathBuf,
}

impl Layout {
    /// The layout under an absolute `HOME`.
    #[must_use]
    pub fn for_user() -> Option<Self> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(|home| Self { home })
    }

    /// The installed client executable.
    #[must_use]
    pub fn binary(&self) -> PathBuf {
        self.home.join(".local/bin/pkg")
    }

    /// The installer's data directory, which holds shell completions.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.home.join(".local/share/pkg")
    }

    /// Bash and Zsh startup files that can hold the managed block.
    #[must_use]
    pub fn startup_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = [".bashrc", ".bash_profile", ".bash_login", ".profile"]
            .iter()
            .map(|name| self.home.join(name))
            .collect();
        if let Some(zdotdir) = std::env::var_os("ZDOTDIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            files.push(zdotdir.join(".zshrc"));
        }
        files.push(self.home.join(".zshrc"));
        files.dedup();
        files
    }

    /// The pkg-owned Fish configuration file.
    #[must_use]
    pub fn fish_file(&self) -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| self.home.join(".config"))
            .join("fish/conf.d/pkg.fish")
    }

    /// Whether this process runs the installed client.
    ///
    /// Updates apply only then, so a development build never replaces
    /// the installed client.
    #[must_use]
    pub fn runs_here(&self) -> bool {
        let installed = std::fs::canonicalize(self.binary());
        let current = std::env::current_exe().and_then(std::fs::canonicalize);
        matches!((installed, current), (Ok(installed), Ok(current)) if installed == current)
    }
}

/// The result of one update check.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// This process does not run the installed client.
    NotInstalled,
    /// The installed client is the newest published release.
    Current,
    /// The installer replaced the client.
    Updated {
        /// The replaced version.
        from: String,
        /// The installed version.
        to: String,
    },
}

/// The release system name of this build.
fn release_system() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-linux")
    } else {
        None
    }
}

/// Update the installed client to the newest published release.
/// # Errors
///
/// Returns an error when a download, the checksum, or the new client
/// fails. The current client stays in place.
pub fn update() -> Result<Update, Error> {
    let (Some(layout), Some(system)) = (Layout::for_user(), release_system()) else {
        return Ok(Update::NotInstalled);
    };
    if !layout.runs_here() {
        return Ok(Update::NotInstalled);
    }
    let directory = tempfile::Builder::new().prefix("pkg-update-").tempdir()?;
    let sums = directory.path().join("SHA256SUMS");
    download(&format!("{RELEASES}/latest/download/SHA256SUMS"), &sums)?;
    let (latest, archive, checksum) = published(&std::fs::read_to_string(&sums)?, system)
        .ok_or_else(|| format!("the newest release has no client for {system}"))?;
    if !newer(&latest, VERSION)? {
        return Ok(Update::Current);
    }
    let archive_path = directory.path().join(&archive);
    download(
        &format!("{RELEASES}/download/v{latest}/{archive}"),
        &archive_path,
    )?;
    cask_catalog::verify_sha256(&std::fs::read(&archive_path)?, &checksum)
        .map_err(|_| "the downloaded client does not match its published checksum")?;
    let member = format!("pkg-{latest}-{system}/bin/pkg");
    let mut tar = std::process::Command::new("tar");
    tar.arg("-xOzf").arg(&archive_path).arg(&member);
    let reaped = pkg_process::run_child(tar, pkg_process::IoMode::Capture, None, None)?;
    check("tar", reaped.classify())?;
    if reaped.output.stdout.is_empty() {
        return Err("the downloaded archive has no client".into());
    }
    install(&layout, &reaped.output.stdout, &latest)?;
    Ok(Update::Updated {
        from: VERSION.to_string(),
        to: latest,
    })
}

/// Run the installed client with `args` on the user's terminal.
///
/// Returns the exit code of the client.
/// # Errors
///
/// Returns an error when the client cannot start or a signal stops it.
pub fn run_installed(args: &[&str]) -> Result<u8, Error> {
    let layout = Layout::for_user().ok_or("the installed client has no home directory")?;
    run_client(&layout.binary(), args)
}

fn run_client(binary: &Path, args: &[&str]) -> Result<u8, Error> {
    let mut command = std::process::Command::new(binary);
    command.args(args);
    let reaped = pkg_process::run_child(command, pkg_process::IoMode::Stream, None, None)?;
    match reaped.classify() {
        Outcome::Success => Ok(0),
        Outcome::Interrupted { signal } => Err(Error::Interrupted {
            program: String::from("pkg"),
            signal,
        }),
        Outcome::Failed { .. } => Ok(reaped
            .output
            .status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .filter(|code| *code != 0)
            .unwrap_or(1)),
    }
}

fn download(url: &str, destination: &Path) -> Result<(), Error> {
    let destination = destination
        .to_str()
        .ok_or("temporary paths must be UTF-8")?;
    let outcome = crate::nix::run_direct(
        Path::new("curl"),
        &[
            "--proto",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--max-time",
            "300",
            "-o",
            destination,
            url,
        ],
    )?;
    check("curl", outcome)
}

/// Start the new client from the install directory, then replace the old
/// client and its completions.
fn install(layout: &Layout, client: &[u8], version: &str) -> Result<(), Error> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let binary = layout.binary();
    let directory = binary.parent().ok_or("the client path has no directory")?;
    let mut file = tempfile::Builder::new()
        .prefix(".pkg-new-")
        .tempfile_in(directory)?;
    file.write_all(client)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    file.as_file().sync_all()?;
    // Close the file before it runs: Linux refuses to run an open file.
    let candidate = file.into_temp_path();
    let (outcome, stdout) = crate::nix::run_direct_captured(&candidate, &["--version"])?;
    check("the new client", outcome)?;
    if stdout.trim() != format!("pkg {version}") {
        return Err(format!("the new client reports {}", stdout.trim()).into());
    }
    let completions = layout.data_dir().join("completions");
    std::fs::create_dir_all(&completions)?;
    for (shell, name) in [("bash", "pkg.bash"), ("zsh", "_pkg"), ("fish", "pkg.fish")] {
        let (outcome, text) = crate::nix::run_direct_captured(&candidate, &["completion", shell])?;
        check("the new client", outcome)?;
        let mut file = tempfile::NamedTempFile::new_in(&completions)?;
        file.write_all(text.as_bytes())?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.persist(completions.join(name))
            .map_err(|error| error.error)?;
    }
    candidate
        .persist(&binary)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn check(program: &str, outcome: Outcome) -> Result<(), Error> {
    match outcome {
        Outcome::Success => Ok(()),
        Outcome::Interrupted { signal } => Err(Error::Interrupted {
            program: program.to_string(),
            signal,
        }),
        Outcome::Failed { status, stderr } => {
            let cause = stderr
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .unwrap_or(&status)
                .to_string();
            Err(format!("{program} failed: {cause}").into())
        }
    }
}

/// The version, archive name, and checksum of the client for `system`
/// in one `SHA256SUMS` file.
fn published(sums: &str, system: &str) -> Option<(String, String, String)> {
    let suffix = format!("-{system}.tar.gz");
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (checksum, archive) = (fields.next()?, fields.next()?);
        let archive = archive.trim_start_matches('*');
        let version = archive.strip_prefix("pkg-")?.strip_suffix(&suffix)?;
        (!version.is_empty() && fields.next().is_none()).then(|| {
            (
                version.to_string(),
                archive.to_string(),
                checksum.to_string(),
            )
        })
    })
}

/// Whether `candidate` is a newer version than `current`.
fn newer(candidate: &str, current: &str) -> Result<bool, Error> {
    let parse = |value: &str| {
        semver::Version::parse(value).map_err(|error| format!("invalid version {value}: {error}"))
    };
    Ok(parse(candidate)? > parse(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_client_for_this_system_from_the_checksum_list() {
        let sums = "aaa  pkg-0.2.0-alpha.17-x86_64-linux.tar.gz\nbbb  pkg-0.2.0-alpha.17-aarch64-darwin.tar.gz\n";
        assert_eq!(
            published(sums, "aarch64-darwin"),
            Some((
                String::from("0.2.0-alpha.17"),
                String::from("pkg-0.2.0-alpha.17-aarch64-darwin.tar.gz"),
                String::from("bbb"),
            ))
        );
        assert_eq!(published(sums, "x86_64-darwin"), None);
        assert_eq!(
            published("ccc  pkg--aarch64-darwin.tar.gz\n", "aarch64-darwin"),
            None
        );
        assert_eq!(published("ccc  other.tar.gz\n", "aarch64-darwin"), None);
    }

    #[test]
    fn only_a_newer_release_is_an_update() {
        assert!(newer("0.2.0-alpha.17", "0.2.0-alpha.16").unwrap());
        assert!(newer("0.2.0", "0.2.0-alpha.16").unwrap());
        assert!(!newer("0.2.0-alpha.12", "0.2.0-alpha.16").unwrap());
        assert!(!newer("0.2.0-alpha.16", "0.2.0-alpha.16").unwrap());
        assert!(newer("not a version", "0.2.0").is_err());
    }

    #[test]
    fn the_new_client_runs_with_the_given_arguments_and_its_exit_code() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let client = dir.path().join("pkg");
        let log = dir.path().join("args");
        std::fs::write(
            &client,
            format!("#!/bin/sh\necho \"$@\" > '{}'\nexit 3\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755)).unwrap();
        let code = run_client(&client, &["update", "--skip-client"]).unwrap();
        assert_eq!(code, 3);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "update --skip-client\n"
        );
    }

    #[test]
    fn a_development_build_is_not_the_installed_client() {
        let layout = Layout {
            home: PathBuf::from("/nonexistent-pkg-home"),
        };
        assert!(!layout.runs_here());
        assert_eq!(
            layout.binary(),
            Path::new("/nonexistent-pkg-home/.local/bin/pkg")
        );
    }
}
