//! The installed pkg client: its files and its updates.
//!
//! The installer owns one fixed layout under the user's home directory.
//! An update reads the checksum list of the newest published release. When
//! that release is newer, the update checks the list's signature by the
//! release key, then downloads the archive for this system and verifies it
//! before anything from it runs. The new client starts once from its final
//! directory, then replaces the old client with one rename, so a running
//! pkg keeps working.

use std::path::{Path, PathBuf};

use crate::nix::Outcome;

/// The published releases of the client.
const RELEASES: &str = "https://github.com/spa5k/pkg/releases";

/// The OpenSSH Ed25519 public key that signs `SHA256SUMS` for each release.
const RELEASE_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOaFZma99iXoZWruu1raDx1IAPNLJPOF/TYfXbAvyehh";

/// The `ssh-keygen -Y sign -n` namespace of release signatures.
const RELEASE_NAMESPACE: &str = "pkg-release";

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
/// Returns an error when a download, the signature, the checksum, or the
/// new client fails. The current client stays in place.
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
    let sums = std::fs::read(&sums)?;
    let (latest, archive, checksum) = std::str::from_utf8(&sums)
        .ok()
        .and_then(|text| published(text, system))
        .ok_or_else(|| format!("the newest release has no client for {system}"))?;
    if !newer(&latest, VERSION)? {
        return Ok(Update::Current);
    }
    // Trust the version and checksum only after the signature check.
    let signature = directory.path().join("SHA256SUMS.sig");
    download(
        &format!("{RELEASES}/download/v{latest}/SHA256SUMS.sig"),
        &signature,
    )?;
    verify_signature(
        &sums,
        &std::fs::read_to_string(&signature)?,
        RELEASE_KEY,
        RELEASE_NAMESPACE,
    )?;
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

/// The largest release file, in bytes.
const MAX_DOWNLOAD_BYTES: &str = "268435456";

fn download(url: &str, destination: &Path) -> Result<(), Error> {
    let destination = destination
        .to_str()
        .ok_or("temporary paths must be UTF-8")?;
    let outcome = crate::nix::run_direct(
        Path::new("curl"),
        &[
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--max-time",
            "300",
            "--max-filesize",
            MAX_DOWNLOAD_BYTES,
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
///
/// The version must be a canonical semantic version and the checksum 64
/// hex digits, because both become parts of URLs and paths.
fn published(sums: &str, system: &str) -> Option<(String, String, String)> {
    let suffix = format!("-{system}.tar.gz");
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (checksum, archive) = (fields.next()?, fields.next()?);
        let archive = archive.trim_start_matches('*');
        let version = archive.strip_prefix("pkg-")?.strip_suffix(&suffix)?;
        let valid = fields.next().is_none()
            && checksum.len() == 64
            && checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
            && semver::Version::parse(version).is_ok_and(|parsed| parsed.to_string() == version);
        valid.then(|| {
            (
                version.to_string(),
                archive.to_string(),
                checksum.to_string(),
            )
        })
    })
}

/// Check an armored `ssh-keygen -Y sign` signature of `message`.
///
/// Accept only an Ed25519 signature by `key`, a base64 OpenSSH public key
/// blob, in `namespace` over a SHA-512 hash. This is the `ssh-keygen`
/// default for Ed25519 keys. The format is OpenSSH `PROTOCOL.sshsig`.
fn verify_signature(
    message: &[u8],
    armored: &str,
    key: &str,
    namespace: &str,
) -> Result<(), String> {
    use base64::Engine as _;
    let base64 = base64::engine::general_purpose::STANDARD;
    let malformed = || String::from("the release signature is malformed");
    let body = armored
        .trim()
        .strip_prefix("-----BEGIN SSH SIGNATURE-----")
        .and_then(|rest| rest.strip_suffix("-----END SSH SIGNATURE-----"))
        .ok_or_else(malformed)?;
    let blob = base64
        .decode(body.split_whitespace().collect::<String>())
        .map_err(|_| malformed())?;
    let key = base64
        .decode(key)
        .map_err(|_| String::from("the release key is not base64"))?;
    let mut reader = Wire(&blob);
    if reader.take(6) != Some(b"SSHSIG".as_slice()) || reader.number() != Some(1) {
        return Err(malformed());
    }
    let (Some(signer), Some(signed_namespace), Some(reserved), Some(hash), Some(signature)) = (
        reader.string(),
        reader.string(),
        reader.string(),
        reader.string(),
        reader.string(),
    ) else {
        return Err(malformed());
    };
    if !reader.0.is_empty() {
        return Err(malformed());
    }
    if signer != key.as_slice() {
        return Err(String::from(
            "the release is not signed by the pkg release key",
        ));
    }
    if signed_namespace != namespace.as_bytes() || hash != b"sha512" {
        return Err(malformed());
    }
    let mut public = Wire(&key);
    let mut signature = Wire(signature);
    let (Some(b"ssh-ed25519"), Some(public), Some(b"ssh-ed25519"), Some(signature)) = (
        public.string(),
        public.string(),
        signature.string(),
        signature.string(),
    ) else {
        return Err(malformed());
    };
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA512, message);
    let mut data = b"SSHSIG".to_vec();
    for field in [namespace.as_bytes(), reserved, b"sha512", digest.as_ref()] {
        let length = u32::try_from(field.len()).map_err(|_| malformed())?;
        data.extend_from_slice(&length.to_be_bytes());
        data.extend_from_slice(field);
    }
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, public)
        .verify(&data, signature)
        .map_err(|_| String::from("the release signature does not match its checksum list"))
}

/// A reader of the SSH wire encoding: big-endian lengths before data.
struct Wire<'a>(&'a [u8]);

impl<'a> Wire<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(length)?;
        self.0 = tail;
        Some(head)
    }

    fn number(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let length = self.number()?;
        self.take(usize::try_from(length).ok()?)
    }
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
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let sums = format!(
            "{a}  pkg-0.2.0-alpha.17-x86_64-linux.tar.gz\n{b}  pkg-0.2.0-alpha.17-aarch64-darwin.tar.gz\n"
        );
        assert_eq!(
            published(&sums, "aarch64-darwin"),
            Some((
                String::from("0.2.0-alpha.17"),
                String::from("pkg-0.2.0-alpha.17-aarch64-darwin.tar.gz"),
                b,
            ))
        );
        assert_eq!(published(&sums, "x86_64-darwin"), None);
        for line in [
            format!("{a}  pkg--aarch64-darwin.tar.gz"),
            format!("{a}  other.tar.gz"),
            format!("{a}  pkg-1.0.0/../../x-aarch64-darwin.tar.gz"),
            format!("{a}  pkg-01.0.0-aarch64-darwin.tar.gz"),
            format!("{a}  pkg-1.0.0?x=1-aarch64-darwin.tar.gz"),
            String::from("bbb  pkg-1.0.0-aarch64-darwin.tar.gz"),
        ] {
            assert_eq!(published(&line, "aarch64-darwin"), None, "{line}");
        }
    }

    /// A throwaway key and `ssh-keygen -Y sign` output for these tests.
    const TEST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIFwE3tikzJoifU8bnO83SgImfBJcBidTNvWYVcJAGJIc";
    const TEST_SUMS: &str = "aaa  pkg-0.2.0-alpha.18-x86_64-linux.tar.gz\nbbb  pkg-0.2.0-alpha.18-aarch64-darwin.tar.gz\n";
    const TEST_SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgXATe2KTMmiJ9Txuc7zdKAiZ8El
wGJ1M29ZhVwkAYkhwAAAALcGtnLXJlbGVhc2UAAAAAAAAABnNoYTUxMgAAAFMAAAALc3No
LWVkMjU1MTkAAABAGReXQNYsSjBYkftoJQpwOIf8gdxnfKx04lzUzM9g4bp4MRTHrZumg/
ksiGDfKWiRAIGhOxL6vaRwVCpPf42EBQ==
-----END SSH SIGNATURE-----
";
    /// The same message and key, signed in the `git` namespace.
    const GIT_SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAgXATe2KTMmiJ9Txuc7zdKAiZ8El
wGJ1M29ZhVwkAYkhwAAAADZ2l0AAAAAAAAAAZzaGE1MTIAAABTAAAAC3NzaC1lZDI1NTE5
AAAAQIzWr443xKn3ZZVuZxWhAjXxvia+RK+TQYskTNUFN1iLByrT9zZv5n6XipPPp2nnC1
QTWColmNDGwS36DVU1Tgs=
-----END SSH SIGNATURE-----
";

    #[test]
    fn accepts_only_a_release_signature_by_the_release_key() {
        let check = |message: &str, signature: &str, key: &str| {
            verify_signature(message.as_bytes(), signature, key, RELEASE_NAMESPACE)
        };
        assert_eq!(check(TEST_SUMS, TEST_SIGNATURE, TEST_KEY), Ok(()));
        // A changed checksum list fails.
        let changed = TEST_SUMS.replace("bbb", "ccc");
        assert!(check(&changed, TEST_SIGNATURE, TEST_KEY).is_err());
        // Another key fails, so the test key cannot sign a release.
        assert_eq!(
            check(TEST_SUMS, TEST_SIGNATURE, RELEASE_KEY),
            Err(String::from(
                "the release is not signed by the pkg release key"
            ))
        );
        // A signature for Git commits or tags is not a release signature.
        assert!(check(TEST_SUMS, GIT_SIGNATURE, TEST_KEY).is_err());
        assert!(check(TEST_SUMS, "", TEST_KEY).is_err());
        assert!(check(TEST_SUMS, &TEST_SIGNATURE.replace('U', "V"), TEST_KEY).is_err());
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
