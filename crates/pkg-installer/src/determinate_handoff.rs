//! Durable, fail-closed handoff for the pinned Determinate installer.

use nix::unistd::{Gid, Uid, fchown};
use pkg_core::state::Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
#[cfg(test)]
use std::sync::{Arc, Barrier};
use std::{
    fmt,
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
};

const SCHEMA_VERSION: u32 = 1;
const MAX_HANDOFF_BYTES: u64 = 8 * 1024;
const MAX_RECEIPT_BYTES: u64 = 1024 * 1024;
const LOCK: &str = ".determinate-handoff-v1.lock";
const TEMPORARY: &str = ".determinate-handoff-v1.json.next";
const INSTALLED_INSTALLER: &str = "/nix/nix-installer";
const RECEIPT: &str = "/nix/receipt.json";

#[cfg(target_os = "linux")]
const HANDOFF: &str = "/var/lib/pkg-install/determinate-handoff-v1.json";
#[cfg(target_os = "macos")]
const HANDOFF: &str = "/private/var/db/pkg-install/determinate-handoff-v1.json";
#[cfg(target_os = "linux")]
const RECEIPT_MODE: u32 = 0o600;
#[cfg(target_os = "macos")]
const RECEIPT_MODE: u32 = 0o644;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const INSTALLER_LENGTH: u64 = 58_427_232;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const INSTALLER_SHA256: &str = "90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const INSTALLER_LENGTH: u64 = 69_625_424;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const INSTALLER_SHA256: &str = "9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const INSTALLER_LENGTH: u64 = 74_918_096;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const INSTALLER_SHA256: &str = "9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterminateHandoffState {
    NotStarted,
    Started,
    Accepted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterminateHandoffError {
    UnsupportedSystem,
    InvalidState,
    InvalidReceipt,
    InvalidInstaller,
    IdentityMismatch,
    InvalidTransition,
    InstalledStateUnproved,
    PersistenceFailed,
}

impl fmt::Display for DeterminateHandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedSystem => "unsupported Determinate installer target",
            Self::InvalidState => "invalid Determinate handoff state",
            Self::InvalidReceipt => "invalid Determinate receipt identity",
            Self::InvalidInstaller => "invalid installed Determinate installer identity",
            Self::IdentityMismatch => "accepted Determinate identity changed",
            Self::InvalidTransition => "invalid Determinate handoff transition",
            Self::InstalledStateUnproved => "Determinate installed state is not proved",
            Self::PersistenceFailed => "could not persist Determinate handoff state",
        })
    }
}

impl std::error::Error for DeterminateHandoffError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    length: u64,
    sha256: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Record {
    Started,
    Accepted {
        installer: FileIdentity,
        receipt: FileIdentity,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    schema_version: u32,
    state: WireState,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum WireState {
    Started {},
    Accepted {
        installer: WireIdentity,
        receipt: WireIdentity,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIdentity {
    length: u64,
    sha256: String,
}

pub struct DeterminateHandoff {
    handoff: PathBuf,
    receipt: PathBuf,
    installer: PathBuf,
    trust_root: PathBuf,
    owner: u32,
    group: u32,
    receipt_mode: u32,
    installer_identity: FileIdentity,
    #[cfg(test)]
    pause_after_temp: Option<(Arc<Barrier>, Arc<Barrier>)>,
}

impl DeterminateHandoff {
    pub fn production() -> Result<Self, DeterminateHandoffError> {
        let installer_identity = pinned_installer_identity()?;
        Ok(Self {
            handoff: PathBuf::from(HANDOFF),
            receipt: PathBuf::from(RECEIPT),
            installer: PathBuf::from(INSTALLED_INSTALLER),
            trust_root: PathBuf::from("/"),
            owner: 0,
            group: 0,
            receipt_mode: RECEIPT_MODE,
            installer_identity,
            #[cfg(test)]
            pause_after_temp: None,
        })
    }

    pub fn state(&self) -> Result<DeterminateHandoffState, DeterminateHandoffError> {
        let _lock = self.lock_operation()?;
        match self.load_locked()? {
            None => Ok(DeterminateHandoffState::NotStarted),
            Some(Record::Started) => Ok(DeterminateHandoffState::Started),
            Some(Record::Accepted { installer, receipt }) => {
                let current = self.observe_vendor_identity()?;
                if current != (installer, receipt) {
                    return Err(DeterminateHandoffError::IdentityMismatch);
                }
                Ok(DeterminateHandoffState::Accepted)
            }
        }
    }

    /// Persists the crash boundary that must precede vendor execution.
    pub fn record_started(&self) -> Result<(), DeterminateHandoffError> {
        let _lock = self.lock_operation()?;
        match self.load_locked()? {
            None => self.persist_locked(Record::Started, true),
            Some(Record::Started | Record::Accepted { .. }) => {
                Err(DeterminateHandoffError::InvalidTransition)
            }
        }
    }

    /// Validates vendor-owned identities but cannot accept them before DN09 proof.
    pub fn accept_vendor_result(&self) -> Result<(), DeterminateHandoffError> {
        let _lock = self.lock_operation()?;
        if self.load_locked()? != Some(Record::Started) {
            return Err(DeterminateHandoffError::InvalidTransition);
        }
        self.observe_vendor_identity()?;
        Err(DeterminateHandoffError::InstalledStateUnproved)
    }

    // DN09 can call this only after its standard-daemon installed-state proof passes.
    fn accept_after_installed_state_proof(&self) -> Result<(), DeterminateHandoffError> {
        let _lock = self.lock_operation()?;
        if self.load_locked()? != Some(Record::Started) {
            return Err(DeterminateHandoffError::InvalidTransition);
        }
        let (installer, receipt) = self.observe_vendor_identity()?;
        self.persist_locked(Record::Accepted { installer, receipt }, false)
    }

    // DN12 can call this only after a proved repair or update and fresh DN09 proof.
    fn replace_after_installed_state_proof(&self) -> Result<(), DeterminateHandoffError> {
        let _lock = self.lock_operation()?;
        if !matches!(self.load_locked()?, Some(Record::Accepted { .. })) {
            return Err(DeterminateHandoffError::InvalidTransition);
        }
        let (installer, receipt) = self.observe_vendor_identity()?;
        self.persist_locked(Record::Accepted { installer, receipt }, false)
    }

    fn observe_vendor_identity(
        &self,
    ) -> Result<(FileIdentity, FileIdentity), DeterminateHandoffError> {
        let installer = fingerprint(
            &self.installer,
            &self.trust_root,
            self.owner,
            self.group,
            0o755,
            self.installer_identity.length,
            DeterminateHandoffError::InvalidInstaller,
        )?;
        if installer != self.installer_identity {
            return Err(DeterminateHandoffError::InvalidInstaller);
        }
        let receipt = fingerprint(
            &self.receipt,
            &self.trust_root,
            self.owner,
            self.group,
            self.receipt_mode,
            MAX_RECEIPT_BYTES,
            DeterminateHandoffError::InvalidReceipt,
        )?;
        if receipt.length == 0 {
            return Err(DeterminateHandoffError::InvalidReceipt);
        }
        Ok((installer, receipt))
    }

    fn lock_operation(&self) -> Result<File, DeterminateHandoffError> {
        let parent = self
            .handoff
            .parent()
            .ok_or(DeterminateHandoffError::InvalidState)?;
        validate_parent_chain(
            parent,
            &self.trust_root,
            self.owner,
            DeterminateHandoffError::InvalidState,
        )?;
        let path = lock_path(parent);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| DeterminateHandoffError::InvalidState)?;
        let opened = file
            .metadata()
            .map_err(|_| DeterminateHandoffError::InvalidState)?;
        let current =
            fs::symlink_metadata(&path).map_err(|_| DeterminateHandoffError::InvalidState)?;
        if !valid_file_metadata(&opened, self.owner, self.group, 0o600, 0, 1)
            || !valid_file_metadata(&current, self.owner, self.group, 0o600, 0, 1)
            || opened.dev() != current.dev()
            || opened.ino() != current.ino()
        {
            return Err(DeterminateHandoffError::InvalidState);
        }
        file.lock()
            .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
        validate_parent_chain(
            parent,
            &self.trust_root,
            self.owner,
            DeterminateHandoffError::InvalidState,
        )?;
        let opened = file
            .metadata()
            .map_err(|_| DeterminateHandoffError::InvalidState)?;
        if !valid_file_metadata(&opened, self.owner, self.group, 0o600, 0, 1)
            || !same_path(&path, &opened, 1, DeterminateHandoffError::InvalidState)?
        {
            return Err(DeterminateHandoffError::InvalidState);
        }
        Ok(file)
    }

    fn load_locked(&self) -> Result<Option<Record>, DeterminateHandoffError> {
        let parent = self
            .handoff
            .parent()
            .ok_or(DeterminateHandoffError::InvalidState)?;
        validate_parent_chain(
            parent,
            &self.trust_root,
            self.owner,
            DeterminateHandoffError::InvalidState,
        )?;
        reconcile_temporary(parent, &self.handoff, self.owner, self.group)?;
        match fs::symlink_metadata(&self.handoff) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(DeterminateHandoffError::InvalidState),
            Ok(_) => {}
        }
        let (mut file, opened) = open_regular(
            &self.handoff,
            &self.trust_root,
            self.owner,
            self.group,
            0o600,
            MAX_HANDOFF_BYTES,
            DeterminateHandoffError::InvalidState,
        )?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_HANDOFF_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| DeterminateHandoffError::InvalidState)?;
        if bytes.len() as u64 > MAX_HANDOFF_BYTES
            || !same_path(
                &self.handoff,
                &opened,
                1,
                DeterminateHandoffError::InvalidState,
            )?
        {
            return Err(DeterminateHandoffError::InvalidState);
        }
        decode(&bytes).map(Some)
    }

    fn persist_locked(&self, record: Record, create: bool) -> Result<(), DeterminateHandoffError> {
        let parent = self
            .handoff
            .parent()
            .ok_or(DeterminateHandoffError::PersistenceFailed)?;
        validate_parent_chain(
            parent,
            &self.trust_root,
            self.owner,
            DeterminateHandoffError::PersistenceFailed,
        )?;
        let bytes = encode(record)?;
        let temporary = temporary_path(parent);
        write_private_file(&temporary, &self.trust_root, &bytes, self.owner, self.group)?;
        #[cfg(test)]
        if let Some((ready, release)) = self.pause_after_temp.as_ref() {
            ready.wait();
            release.wait();
        }
        let result = (|| {
            if create {
                fs::hard_link(&temporary, &self.handoff)
                    .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
                fs::remove_file(&temporary)
                    .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
            } else {
                fs::rename(&temporary, &self.handoff)
                    .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
            }
            sync_directory(parent)?;
            if self.load_locked()? != Some(record) {
                return Err(DeterminateHandoffError::PersistenceFailed);
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    #[cfg(test)]
    fn for_test(
        root: &Path,
        receipt_mode: u32,
        installer_identity: FileIdentity,
    ) -> Result<Self, std::io::Error> {
        let metadata = fs::metadata(root)?;
        Ok(Self {
            handoff: root.join("determinate-handoff-v1.json"),
            receipt: root.join("receipt.json"),
            installer: root.join("nix-installer"),
            trust_root: root.to_path_buf(),
            owner: metadata.uid(),
            group: metadata.gid(),
            receipt_mode,
            installer_identity,
            pause_after_temp: None,
        })
    }
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn pinned_installer_identity() -> Result<FileIdentity, DeterminateHandoffError> {
    let sha256 = Digest::from_str(&format!("sha256-{INSTALLER_SHA256}"))
        .map_err(|_| DeterminateHandoffError::InvalidInstaller)?;
    Ok(FileIdentity {
        length: INSTALLER_LENGTH,
        sha256,
    })
}

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn pinned_installer_identity() -> Result<FileIdentity, DeterminateHandoffError> {
    Err(DeterminateHandoffError::UnsupportedSystem)
}

fn encode(record: Record) -> Result<Vec<u8>, DeterminateHandoffError> {
    let state = match record {
        Record::Started => WireState::Started {},
        Record::Accepted { installer, receipt } => WireState::Accepted {
            installer: installer.into(),
            receipt: receipt.into(),
        },
    };
    serde_json::to_vec(&WireRecord {
        schema_version: SCHEMA_VERSION,
        state,
    })
    .map_err(|_| DeterminateHandoffError::PersistenceFailed)
}

fn decode(bytes: &[u8]) -> Result<Record, DeterminateHandoffError> {
    let wire: WireRecord =
        serde_json::from_slice(bytes).map_err(|_| DeterminateHandoffError::InvalidState)?;
    if wire.schema_version != SCHEMA_VERSION {
        return Err(DeterminateHandoffError::InvalidState);
    }
    match wire.state {
        WireState::Started {} => Ok(Record::Started),
        WireState::Accepted { installer, receipt } => Ok(Record::Accepted {
            installer: installer.try_into()?,
            receipt: receipt.try_into()?,
        }),
    }
}

impl From<FileIdentity> for WireIdentity {
    fn from(value: FileIdentity) -> Self {
        Self {
            length: value.length,
            sha256: value.sha256.to_string(),
        }
    }
}

impl TryFrom<WireIdentity> for FileIdentity {
    type Error = DeterminateHandoffError;

    fn try_from(value: WireIdentity) -> Result<Self, Self::Error> {
        let sha256 =
            Digest::from_str(&value.sha256).map_err(|_| DeterminateHandoffError::InvalidState)?;
        Ok(Self {
            length: value.length,
            sha256,
        })
    }
}

fn fingerprint(
    path: &Path,
    trust_root: &Path,
    owner: u32,
    group: u32,
    mode: u32,
    max_length: u64,
    error: DeterminateHandoffError,
) -> Result<FileIdentity, DeterminateHandoffError> {
    let (mut file, opened) = open_regular(path, trust_root, owner, group, mode, max_length, error)?;
    let sha256 = file_digest(&mut file, error)?;
    if !same_path(path, &opened, 1, error)? {
        return Err(error);
    }
    Ok(FileIdentity {
        length: opened.len(),
        sha256,
    })
}

fn open_regular(
    path: &Path,
    trust_root: &Path,
    owner: u32,
    group: u32,
    mode: u32,
    max_length: u64,
    error: DeterminateHandoffError,
) -> Result<(File, fs::Metadata), DeterminateHandoffError> {
    if !path.is_absolute() {
        return Err(error);
    }
    validate_parent_chain(path.parent().ok_or(error)?, trust_root, owner, error)?;
    let path_metadata = fs::symlink_metadata(path).map_err(|_| error)?;
    if !valid_file_metadata(&path_metadata, owner, group, mode, max_length, 1) {
        return Err(error);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| error)?;
    let opened = file.metadata().map_err(|_| error)?;
    if !valid_file_metadata(&opened, owner, group, mode, max_length, 1)
        || opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
    {
        return Err(error);
    }
    Ok((file, opened))
}

fn valid_file_metadata(
    metadata: &fs::Metadata,
    owner: u32,
    group: u32,
    mode: u32,
    max_length: u64,
    links: u64,
) -> bool {
    metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == owner
        && metadata.gid() == group
        && metadata.mode() & 0o7777 == mode
        && metadata.nlink() == links
        && metadata.len() <= max_length
}

fn same_path(
    path: &Path,
    opened: &fs::Metadata,
    links: u64,
    error: DeterminateHandoffError,
) -> Result<bool, DeterminateHandoffError> {
    let current = fs::symlink_metadata(path).map_err(|_| error)?;
    Ok(!current.file_type().is_symlink()
        && current.dev() == opened.dev()
        && current.ino() == opened.ino()
        && current.len() == opened.len()
        && current.mode() == opened.mode()
        && current.nlink() == links)
}

fn validate_parent_chain(
    path: &Path,
    trust_root: &Path,
    owner: u32,
    error: DeterminateHandoffError,
) -> Result<(), DeterminateHandoffError> {
    if !path.is_absolute() || !trust_root.is_absolute() || !path.starts_with(trust_root) {
        return Err(error);
    }
    let mut current = path;
    loop {
        let metadata = fs::symlink_metadata(current).map_err(|_| error)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != owner
            || metadata.mode() & 0o022 != 0
        {
            return Err(error);
        }
        if current == trust_root {
            return Ok(());
        }
        current = current.parent().ok_or(error)?;
    }
}

fn file_digest(
    file: &mut File,
    error: DeterminateHandoffError,
) -> Result<Digest, DeterminateHandoffError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|_| error)?;
        if read == 0 {
            return Ok(Digest::from_bytes(hasher.finalize().into()));
        }
        hasher.update(&buffer[..read]);
    }
}

fn reconcile_temporary(
    parent: &Path,
    handoff: &Path,
    owner: u32,
    group: u32,
) -> Result<(), DeterminateHandoffError> {
    let temporary = temporary_path(parent);
    let temporary_metadata = match fs::symlink_metadata(&temporary) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(DeterminateHandoffError::InvalidState),
        Ok(metadata) => metadata,
    };
    let handoff_metadata = match fs::symlink_metadata(handoff) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(DeterminateHandoffError::InvalidState),
        Ok(metadata) => Some(metadata),
    };
    let linked = handoff_metadata.as_ref().is_some_and(|metadata| {
        metadata.dev() == temporary_metadata.dev() && metadata.ino() == temporary_metadata.ino()
    });
    let links = if linked { 2 } else { 1 };
    if let Some(metadata) = handoff_metadata.as_ref()
        && !valid_file_metadata(metadata, owner, group, 0o600, MAX_HANDOFF_BYTES, links)
    {
        return Err(DeterminateHandoffError::InvalidState);
    }
    if !valid_file_metadata(
        &temporary_metadata,
        owner,
        group,
        0o600,
        MAX_HANDOFF_BYTES,
        links,
    ) {
        return Err(DeterminateHandoffError::InvalidState);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|_| DeterminateHandoffError::InvalidState)?;
    let opened = file
        .metadata()
        .map_err(|_| DeterminateHandoffError::InvalidState)?;
    if !valid_file_metadata(&opened, owner, group, 0o600, MAX_HANDOFF_BYTES, links)
        || opened.dev() != temporary_metadata.dev()
        || opened.ino() != temporary_metadata.ino()
    {
        return Err(DeterminateHandoffError::InvalidState);
    }
    if !same_path(
        &temporary,
        &opened,
        links,
        DeterminateHandoffError::InvalidState,
    )? || linked && !same_path(handoff, &opened, 2, DeterminateHandoffError::InvalidState)?
    {
        return Err(DeterminateHandoffError::InvalidState);
    }
    fs::remove_file(temporary).map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
    sync_directory(parent)
}

fn write_private_file(
    path: &Path,
    trust_root: &Path,
    bytes: &[u8],
    owner: u32,
    group: u32,
) -> Result<(), DeterminateHandoffError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
    let result = (|| {
        fchown(
            &file,
            Some(Uid::from_raw(owner)),
            Some(Gid::from_raw(group)),
        )
        .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
        file.set_permissions(Permissions::from_mode(0o600))
            .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
        file.write_all(bytes)
            .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
        full_sync(&file)?;
        open_regular(
            path,
            trust_root,
            owner,
            group,
            0o600,
            MAX_HANDOFF_BYTES,
            DeterminateHandoffError::PersistenceFailed,
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn temporary_path(parent: &Path) -> PathBuf {
    parent.join(TEMPORARY)
}

fn lock_path(parent: &Path) -> PathBuf {
    parent.join(LOCK)
}

fn sync_directory(path: &Path) -> Result<(), DeterminateHandoffError> {
    let directory = File::open(path).map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
    full_sync(&directory)
}

#[cfg(target_os = "macos")]
fn full_sync(file: &File) -> Result<(), DeterminateHandoffError> {
    nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_FULLFSYNC)
        .map(|_| ())
        .map_err(|_| DeterminateHandoffError::PersistenceFailed)
}

#[cfg(not(target_os = "macos"))]
fn full_sync(file: &File) -> Result<(), DeterminateHandoffError> {
    file.sync_all()
        .map_err(|_| DeterminateHandoffError::PersistenceFailed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::thread;
    use tempfile::TempDir;

    const RECEIPT_SECRET: &[u8] = b"opaque vendor action data must stay private";
    const INSTALLER_BYTES: &[u8] = b"test pinned determinate installer";

    struct Fixture {
        _temporary: TempDir,
        handoff: DeterminateHandoff,
        unrelated: PathBuf,
    }

    fn identity(bytes: &[u8]) -> FileIdentity {
        FileIdentity {
            length: bytes.len() as u64,
            sha256: Digest::from_bytes(Sha256::digest(bytes).into()),
        }
    }

    fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
    }

    fn fixture(receipt_mode: u32) -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
        write_mode(
            &temporary.path().join("nix-installer"),
            INSTALLER_BYTES,
            0o755,
        );
        write_mode(
            &temporary.path().join("receipt.json"),
            RECEIPT_SECRET,
            receipt_mode,
        );
        let unrelated = temporary.path().join("unknown-nix-content");
        fs::write(&unrelated, b"never delete this").unwrap();
        let handoff =
            DeterminateHandoff::for_test(temporary.path(), receipt_mode, identity(INSTALLER_BYTES))
                .unwrap();
        Fixture {
            _temporary: temporary,
            handoff,
            unrelated,
        }
    }

    #[test]
    fn all_handoff_transitions_fail_closed_until_installed_state_is_proved() {
        let fixture = fixture(0o600);
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::NotStarted
        );

        fixture.handoff.record_started().unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Started
        );
        assert_eq!(
            fixture.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InstalledStateUnproved
        );
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Started
        );

        fixture
            .handoff
            .accept_after_installed_state_proof()
            .unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Accepted
        );
        assert_eq!(
            fixture.handoff.record_started().unwrap_err(),
            DeterminateHandoffError::InvalidTransition
        );

        write_mode(&fixture.handoff.receipt, b"proved repair receipt", 0o600);
        assert_eq!(
            fixture.handoff.state().unwrap_err(),
            DeterminateHandoffError::IdentityMismatch
        );
        fixture
            .handoff
            .replace_after_installed_state_proof()
            .unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Accepted
        );
        assert_eq!(fs::read(&fixture.unrelated).unwrap(), b"never delete this");
    }

    #[test]
    fn handoff_record_is_atomic_private_strict_and_contains_no_receipt_data() {
        let fixture = fixture(0o600);
        fixture.handoff.record_started().unwrap();
        let metadata = fs::symlink_metadata(&fixture.handoff.handoff).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        let bytes = fs::read(&fixture.handoff.handoff).unwrap();
        assert!(
            !bytes
                .windows(RECEIPT_SECRET.len())
                .any(|v| v == RECEIPT_SECRET)
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("receipt.json"));

        fs::write(
            &fixture.handoff.handoff,
            br#"{"schema_version":1,"state":{"kind":"started","extra":true}}"#,
        )
        .unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap_err(),
            DeterminateHandoffError::InvalidState
        );

        let receipt_path = fixture.handoff.receipt.to_string_lossy().into_owned();
        let receipt_secret = std::str::from_utf8(RECEIPT_SECRET).unwrap();
        for error in [
            DeterminateHandoffError::InvalidState,
            DeterminateHandoffError::InvalidReceipt,
            DeterminateHandoffError::InvalidInstaller,
            DeterminateHandoffError::IdentityMismatch,
            DeterminateHandoffError::InstalledStateUnproved,
            DeterminateHandoffError::PersistenceFailed,
        ] {
            let message = error.to_string();
            assert!(!message.contains(receipt_path.as_str()));
            assert!(!message.contains(&identity(INSTALLER_BYTES).sha256.to_string()));
            assert!(!message.contains(receipt_secret));
        }
    }

    #[test]
    fn state_links_and_unsafe_parent_chains_are_rejected() {
        let fixture = fixture(0o600);
        fixture.handoff.record_started().unwrap();
        let link = fixture.handoff.handoff.with_extension("link");
        fs::hard_link(&fixture.handoff.handoff, &link).unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap_err(),
            DeterminateHandoffError::InvalidState
        );
        fs::remove_file(link).unwrap();

        fs::set_permissions(
            fixture.handoff.trust_root.as_path(),
            Permissions::from_mode(0o777),
        )
        .unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap_err(),
            DeterminateHandoffError::InvalidState
        );
    }

    #[test]
    fn crash_after_hard_link_reconciles_to_started() {
        let fixture = fixture(0o600);
        let parent = fixture.handoff.trust_root.as_path();
        let temporary = temporary_path(parent);
        write_private_file(
            &temporary,
            parent,
            &encode(Record::Started).unwrap(),
            fixture.handoff.owner,
            fixture.handoff.group,
        )
        .unwrap();
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::NotStarted
        );
        assert!(!temporary.exists());

        write_private_file(
            &temporary,
            parent,
            &encode(Record::Started).unwrap(),
            fixture.handoff.owner,
            fixture.handoff.group,
        )
        .unwrap();
        fs::hard_link(&temporary, &fixture.handoff.handoff).unwrap();
        assert_eq!(fs::metadata(&fixture.handoff.handoff).unwrap().nlink(), 2);
        assert_eq!(fs::metadata(&temporary).unwrap().nlink(), 2);

        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Started
        );
        assert!(!temporary.exists());
        assert_eq!(fs::metadata(&fixture.handoff.handoff).unwrap().nlink(), 1);
    }

    #[test]
    fn concurrent_record_started_serializes_without_temp_theft() {
        let mut fixture = fixture(0o600);
        let parent = fixture.handoff.trust_root.as_path();
        let second =
            DeterminateHandoff::for_test(parent, 0o600, fixture.handoff.installer_identity)
                .unwrap();
        let ready = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        fixture.handoff.pause_after_temp = Some((Arc::clone(&ready), Arc::clone(&release)));
        let (first, second) = thread::scope(|scope| {
            let first = scope.spawn(|| fixture.handoff.record_started());
            ready.wait();
            let directory = File::open(parent).unwrap();
            assert!(directory.try_lock().is_ok());
            let probe = File::open(lock_path(parent)).unwrap();
            assert!(matches!(
                probe.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            let second = scope.spawn(|| second.record_started());
            release.wait();
            (first.join().unwrap(), second.join().unwrap())
        });

        assert_eq!(first, Ok(()));
        assert_eq!(second, Err(DeterminateHandoffError::InvalidTransition));
        assert_eq!(
            fixture.handoff.state().unwrap(),
            DeterminateHandoffState::Started
        );
        assert!(!temporary_path(parent).exists());
        assert_eq!(fs::read(&fixture.unrelated).unwrap(), b"never delete this");
        let lock = fs::symlink_metadata(lock_path(parent)).unwrap();
        assert!(valid_file_metadata(
            &lock,
            fixture.handoff.owner,
            fixture.handoff.group,
            0o600,
            0,
            1
        ));
    }

    #[test]
    fn receipt_contract_rejects_mode_link_size_and_symlink() {
        let wrong_mode = fixture(0o600);
        wrong_mode.handoff.record_started().unwrap();
        fs::set_permissions(&wrong_mode.handoff.receipt, Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            wrong_mode.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );

        let linked = fixture(0o600);
        linked.handoff.record_started().unwrap();
        fs::hard_link(
            &linked.handoff.receipt,
            linked.handoff.receipt.with_extension("link"),
        )
        .unwrap();
        assert_eq!(
            linked.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );

        let oversized = fixture(0o600);
        oversized.handoff.record_started().unwrap();
        write_mode(
            &oversized.handoff.receipt,
            &vec![0_u8; usize::try_from(MAX_RECEIPT_BYTES).unwrap() + 1],
            0o600,
        );
        assert_eq!(
            oversized.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );

        let linked_target = fixture(0o600);
        linked_target.handoff.record_started().unwrap();
        let target = linked_target.handoff.receipt.with_extension("target");
        fs::rename(&linked_target.handoff.receipt, &target).unwrap();
        symlink(&target, &linked_target.handoff.receipt).unwrap();
        assert_eq!(
            linked_target.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );

        let macos_mode = fixture(0o644);
        macos_mode.handoff.record_started().unwrap();
        assert_eq!(
            macos_mode.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InstalledStateUnproved
        );
        fs::set_permissions(&macos_mode.handoff.receipt, Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            macos_mode.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );
    }

    #[test]
    fn empty_receipt_is_rejected() {
        let fixture = fixture(0o600);
        fixture.handoff.record_started().unwrap();
        fs::write(&fixture.handoff.receipt, []).unwrap();

        assert_eq!(
            fixture.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidReceipt
        );
    }

    #[test]
    fn installed_helper_contract_rejects_mode_link_length_digest_and_symlink() {
        let wrong_mode = fixture(0o600);
        wrong_mode.handoff.record_started().unwrap();
        fs::set_permissions(&wrong_mode.handoff.installer, Permissions::from_mode(0o775)).unwrap();
        assert_eq!(
            wrong_mode.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidInstaller
        );

        let linked = fixture(0o600);
        linked.handoff.record_started().unwrap();
        fs::hard_link(
            &linked.handoff.installer,
            linked.handoff.installer.with_extension("link"),
        )
        .unwrap();
        assert_eq!(
            linked.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidInstaller
        );

        for bytes in [
            b"wrong length".as_slice(),
            b"test pinned determinate installeR",
        ] {
            let changed = fixture(0o600);
            changed.handoff.record_started().unwrap();
            write_mode(&changed.handoff.installer, bytes, 0o755);
            assert_eq!(
                changed.handoff.accept_vendor_result().unwrap_err(),
                DeterminateHandoffError::InvalidInstaller
            );
        }

        let linked_target = fixture(0o600);
        linked_target.handoff.record_started().unwrap();
        let target = linked_target.handoff.installer.with_extension("target");
        fs::rename(&linked_target.handoff.installer, &target).unwrap();
        symlink(&target, &linked_target.handoff.installer).unwrap();
        assert_eq!(
            linked_target.handoff.accept_vendor_result().unwrap_err(),
            DeterminateHandoffError::InvalidInstaller
        );
    }
}
