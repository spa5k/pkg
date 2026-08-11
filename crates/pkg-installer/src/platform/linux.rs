//! Linux peer authentication and atomic GC-root filesystem operations.

#[cfg(target_os = "linux")]
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use pkg_nix::{GenerationId, RemoveRootSetRequest, RootName, RootSet, RootSetEntry, StorePath};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs, io,
    os::unix::{
        fs::{MetadataExt, PermissionsExt, symlink},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const PRODUCTION_GCROOTS: &str = "/nix/var/nix/gcroots";
const MAX_ROOT_ENTRIES: usize = 4096;
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

/// Stable Linux platform/helper failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxPlatformErrorCode {
    /// The kernel would not provide peer credentials.
    PeerCredentialsUnavailable,
    /// The socket peer is not the configured broker service uid.
    UnauthenticatedPeer,
    /// A root path, parent, or existing root set is unsafe or conflicting.
    UnsafeFilesystemState,
    /// A bounded filesystem operation failed.
    FilesystemFailure,
}

/// Redacted Linux platform error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxPlatformError {
    code: LinuxPlatformErrorCode,
}

impl LinuxPlatformError {
    const fn new(code: LinuxPlatformErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxPlatformErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxPlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux helper platform operation failed")
    }
}

impl Error for LinuxPlatformError {}

/// Kernel-authenticated Unix-socket peer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxPeerCredentials {
    pid: i32,
    uid: u32,
    gid: u32,
}

impl LinuxPeerCredentials {
    /// Returns the kernel-authenticated process id.
    #[must_use]
    pub const fn pid(self) -> i32 {
        self.pid
    }

    /// Returns the kernel-authenticated user id.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the kernel-authenticated primary group id.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

/// Reads Linux `SO_PEERCRED` before any framed payload is consumed.
///
/// # Errors
///
/// Returns `PeerCredentialsUnavailable` if the kernel query fails.
#[cfg(target_os = "linux")]
pub fn peer_credentials(stream: &UnixStream) -> Result<LinuxPeerCredentials, LinuxPlatformError> {
    let credentials = getsockopt(stream, PeerCredentials)
        .map_err(|_| LinuxPlatformError::new(LinuxPlatformErrorCode::PeerCredentialsUnavailable))?;
    Ok(LinuxPeerCredentials {
        pid: credentials.pid(),
        uid: credentials.uid(),
        gid: credentials.gid(),
    })
}

/// Reports the Linux-only contract as unavailable on non-Linux build hosts.
///
/// # Errors
///
/// Always returns `PeerCredentialsUnavailable` outside Linux.
#[cfg(not(target_os = "linux"))]
pub const fn peer_credentials(
    _stream: &UnixStream,
) -> Result<LinuxPeerCredentials, LinuxPlatformError> {
    Err(LinuxPlatformError::new(
        LinuxPlatformErrorCode::PeerCredentialsUnavailable,
    ))
}

/// Authenticates the sole configured broker peer using kernel credentials.
///
/// # Errors
///
/// Returns a stable error if credentials are unavailable or the uid differs.
pub fn authenticate_broker_peer(
    stream: &UnixStream,
    broker_uid: u32,
) -> Result<LinuxPeerCredentials, LinuxPlatformError> {
    let credentials = peer_credentials(stream)?;
    if credentials.uid == broker_uid {
        Ok(credentials)
    } else {
        Err(LinuxPlatformError::new(
            LinuxPlatformErrorCode::UnauthenticatedPeer,
        ))
    }
}

/// Real filesystem implementation for per-user generation root sets.
#[derive(Debug, Clone)]
pub struct LinuxRootSetStore {
    root: PathBuf,
    required_owner_uid: u32,
}

impl LinuxRootSetStore {
    /// Opens the production root-set tree and requires root ownership.
    ///
    /// # Errors
    ///
    /// Returns a stable error if any ancestor is symlinked, writable by a
    /// non-root principal, missing, or not root-owned.
    pub fn production() -> Result<Self, LinuxPlatformError> {
        let gcroots = Path::new(PRODUCTION_GCROOTS);
        ensure_safe_ancestors(gcroots, 0)?;
        provision_product_root(gcroots, 0)
    }

    pub(crate) fn new_at(
        root: PathBuf,
        required_owner_uid: u32,
    ) -> Result<Self, LinuxPlatformError> {
        if !root.is_absolute() {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        }
        ensure_trusted_directory(&root, required_owner_uid)?;
        Ok(Self {
            root,
            required_owner_uid,
        })
    }

    /// Stages, fsyncs, atomically renames, and parent-fsyncs one complete set.
    ///
    /// # Errors
    ///
    /// Returns a stable error for unsafe/conflicting state or bounded I/O failure.
    pub fn publish(&self, root_set: &RootSet) -> Result<(), LinuxPlatformError> {
        ensure_trusted_directory(&self.root, self.required_owner_uid)?;
        let owner = self.root.join(root_set.owner_uid().to_string());
        ensure_owned_child_directory(&owner, self.required_owner_uid)?;
        sync_directory(&self.root)?;
        let destination = owner.join(root_set.generation().as_str());
        if destination.symlink_metadata().is_ok() {
            return verify_root_set(&destination, root_set, self.required_owner_uid);
        }

        let sequence = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
        let staging = owner.join(format!(
            ".{}.tmp-{}-{sequence}",
            root_set.generation().as_str(),
            std::process::id()
        ));
        fs::create_dir(&staging).map_err(fs_error)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).map_err(fs_error)?;
        let mut cleanup = StagingDirectory::new(staging.clone());
        for entry in root_set.entries() {
            symlink(entry.target().as_str(), staging.join(entry.name().as_str()))
                .map_err(fs_error)?;
        }
        sync_directory(&staging)?;
        match fs::rename(&staging, &destination) {
            Ok(()) => cleanup.disarm(),
            Err(_) if destination.symlink_metadata().is_ok() => {
                verify_root_set(&destination, root_set, self.required_owner_uid)?;
                fs::remove_dir_all(&staging).map_err(fs_error)?;
                cleanup.disarm();
            }
            Err(_) => {
                return Err(LinuxPlatformError::new(
                    LinuxPlatformErrorCode::FilesystemFailure,
                ));
            }
        }
        sync_directory(&owner)?;
        Ok(())
    }

    /// Removes only one exact generated root-set directory and fsyncs its parent.
    ///
    /// # Errors
    ///
    /// Returns a stable error if the exact directory is unsafe or removal fails.
    pub fn remove(&self, request: &RemoveRootSetRequest) -> Result<(), LinuxPlatformError> {
        ensure_trusted_directory(&self.root, self.required_owner_uid)?;
        let owner = self.root.join(request.owner_uid().to_string());
        match owner.symlink_metadata() {
            Ok(_) => ensure_trusted_directory(&owner, self.required_owner_uid)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(fs_error(error)),
        }
        let destination = owner.join(request.generation().as_str());
        let metadata = match destination.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(fs_error(error)),
        };
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        }
        ensure_trusted_directory(&destination, self.required_owner_uid)?;
        let entries = bounded_entries(&destination)?;
        for entry in entries.values() {
            if !entry.file_type().is_symlink() {
                return Err(LinuxPlatformError::new(
                    LinuxPlatformErrorCode::UnsafeFilesystemState,
                ));
            }
        }
        for name in entries.keys() {
            fs::remove_file(destination.join(name)).map_err(fs_error)?;
        }
        fs::remove_dir(&destination).map_err(fs_error)?;
        sync_directory(&owner)?;
        Ok(())
    }

    /// Reloads one durable set after a helper restart using only typed links.
    ///
    /// # Errors
    ///
    /// Returns a stable error if the directory, names, or store targets are unsafe.
    pub fn load(
        &self,
        owner_uid: u32,
        generation: &GenerationId,
    ) -> Result<RootSet, LinuxPlatformError> {
        self.load_optional(owner_uid, generation)?
            .ok_or_else(|| LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState))
    }

    /// Loads a generation when present while distinguishing a clean absence.
    ///
    /// # Errors
    ///
    /// Returns a stable error for every unsafe, malformed, or unavailable
    /// destination; only an exact missing final generation is `Ok(None)`.
    pub(crate) fn load_optional(
        &self,
        owner_uid: u32,
        generation: &GenerationId,
    ) -> Result<Option<RootSet>, LinuxPlatformError> {
        ensure_trusted_directory(&self.root, self.required_owner_uid)?;
        let owner = self.root.join(owner_uid.to_string());
        ensure_trusted_directory(&owner, self.required_owner_uid)?;
        let path = owner.join(generation.as_str());
        match path.symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(fs_error(error)),
        }
        ensure_trusted_directory(&path, self.required_owner_uid)?;
        let actual = bounded_entries(&path)?;
        let mut entries = Vec::with_capacity(actual.len());
        for (name, metadata) in actual {
            if !metadata.file_type().is_symlink() {
                return Err(LinuxPlatformError::new(
                    LinuxPlatformErrorCode::UnsafeFilesystemState,
                ));
            }
            let target = fs::read_link(path.join(&name)).map_err(fs_error)?;
            let target = target.to_str().ok_or_else(|| {
                LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState)
            })?;
            entries.push(RootSetEntry::new(
                RootName::new(&name).map_err(|_| {
                    LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState)
                })?,
                StorePath::new(target).map_err(|_| {
                    LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState)
                })?,
            ));
        }
        RootSet::new(owner_uid, generation.clone(), entries)
            .map(Some)
            .map_err(|_| LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState))
    }
}

fn provision_product_root(
    gcroots: &Path,
    owner_uid: u32,
) -> Result<LinuxRootSetStore, LinuxPlatformError> {
    ensure_trusted_directory(gcroots, owner_uid)?;
    let product = gcroots.join("pkg");
    ensure_owned_child_directory(&product, owner_uid)?;
    let users = product.join("users");
    ensure_owned_child_directory(&users, owner_uid)?;
    sync_directory(&product)?;
    sync_directory(gcroots)?;
    LinuxRootSetStore::new_at(users, owner_uid)
}

fn ensure_trusted_directory(path: &Path, owner_uid: u32) -> Result<(), LinuxPlatformError> {
    let metadata = path.symlink_metadata().map_err(fs_error)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(LinuxPlatformError::new(
            LinuxPlatformErrorCode::UnsafeFilesystemState,
        ));
    }
    Ok(())
}

fn ensure_safe_ancestors(path: &Path, owner_uid: u32) -> Result<(), LinuxPlatformError> {
    for ancestor in path.ancestors() {
        ensure_trusted_directory(ancestor, owner_uid)?;
    }
    Ok(())
}

fn ensure_owned_child_directory(path: &Path, owner_uid: u32) -> Result<(), LinuxPlatformError> {
    match path.symlink_metadata() {
        Ok(_) => ensure_trusted_directory(path, owner_uid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(fs_error)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(fs_error)?;
            ensure_trusted_directory(path, owner_uid)
        }
        Err(error) => Err(fs_error(error)),
    }
}

fn verify_root_set(
    path: &Path,
    expected: &RootSet,
    required_owner_uid: u32,
) -> Result<(), LinuxPlatformError> {
    ensure_trusted_directory(path, required_owner_uid)?;
    let actual = bounded_entries(path)?;
    if actual.len() != expected.entries().len() {
        return Err(LinuxPlatformError::new(
            LinuxPlatformErrorCode::UnsafeFilesystemState,
        ));
    }
    for entry in expected.entries() {
        let Some(existing) = actual.get(entry.name().as_str()) else {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        };
        if !existing.file_type().is_symlink()
            || fs::read_link(path.join(entry.name().as_str())).map_err(fs_error)?
                != Path::new(entry.target().as_str())
        {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        }
    }
    Ok(())
}

fn bounded_entries(path: &Path) -> Result<BTreeMap<String, fs::Metadata>, LinuxPlatformError> {
    let mut entries = BTreeMap::new();
    for item in path.read_dir().map_err(fs_error)? {
        if entries.len() == MAX_ROOT_ENTRIES {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        }
        let item = item.map_err(fs_error)?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| LinuxPlatformError::new(LinuxPlatformErrorCode::UnsafeFilesystemState))?;
        let metadata = item.path().symlink_metadata().map_err(fs_error)?;
        if entries.insert(name, metadata).is_some() {
            return Err(LinuxPlatformError::new(
                LinuxPlatformErrorCode::UnsafeFilesystemState,
            ));
        }
    }
    Ok(entries)
}

fn sync_directory(path: &Path) -> Result<(), LinuxPlatformError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(fs_error)
}

fn fs_error(_error: io::Error) -> LinuxPlatformError {
    LinuxPlatformError::new(LinuxPlatformErrorCode::FilesystemFailure)
}

struct StagingDirectory {
    path: PathBuf,
    armed: bool,
}

impl StagingDirectory {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Uid;
    use pkg_nix::{GenerationId, RootName, RootSetEntry, StorePath};

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, io::Error> {
            let sequence = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pkg-installer-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root_set() -> Result<RootSet, Box<dyn Error>> {
        Ok(RootSet::new(
            501,
            GenerationId::new("gen-0007")?,
            vec![
                RootSetEntry::new(
                    RootName::new("bin")?,
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-hello"))?,
                ),
                RootSetEntry::new(
                    RootName::new("man")?,
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-man"))?,
                ),
            ],
        )?)
    }

    #[test]
    fn root_sets_publish_idempotently_then_remove_without_recursive_input()
    -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new("roots")?;
        let store = LinuxRootSetStore::new_at(scratch.0.clone(), Uid::current().as_raw())?;
        let roots = root_set()?;
        store.publish(&roots)?;
        store.publish(&roots)?;

        let generation = scratch.0.join("501/gen-0007");
        assert!(
            generation
                .join("bin")
                .symlink_metadata()?
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(generation.join("man"))?,
            PathBuf::from(format!("/nix/store/{STORE_HASH}-hello-man"))
        );

        store.remove(&RemoveRootSetRequest::new(
            501,
            GenerationId::new("gen-0007")?,
        ))?;
        assert!(!generation.exists());
        store.remove(&RemoveRootSetRequest::new(
            501,
            GenerationId::new("gen-0007")?,
        ))?;
        store.remove(&RemoveRootSetRequest::new(
            777,
            GenerationId::new("gen-0001")?,
        ))?;
        Ok(())
    }

    #[test]
    fn conflicting_or_unsafe_root_state_fails_closed() -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new("conflict")?;
        let store = LinuxRootSetStore::new_at(scratch.0.clone(), Uid::current().as_raw())?;
        let roots = root_set()?;
        store.publish(&roots)?;
        let generation = scratch.0.join("501/gen-0007");
        fs::remove_file(generation.join("bin"))?;
        symlink(
            format!("/nix/store/{STORE_HASH}-different"),
            generation.join("bin"),
        )?;
        assert_eq!(
            store.publish(&roots).map_err(LinuxPlatformError::code),
            Err(LinuxPlatformErrorCode::UnsafeFilesystemState)
        );

        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o777))?;
        assert_eq!(
            store
                .remove(&RemoveRootSetRequest::new(
                    501,
                    GenerationId::new("gen-0007")?,
                ))
                .map_err(LinuxPlatformError::code),
            Err(LinuxPlatformErrorCode::UnsafeFilesystemState)
        );
        Ok(())
    }

    #[test]
    fn product_root_initialization_is_narrow_idempotent_and_symlink_safe()
    -> Result<(), Box<dyn Error>> {
        let scratch = Scratch::new("product-root")?;
        let owner = Uid::current().as_raw();
        let first = provision_product_root(&scratch.0, owner)?;
        let second = provision_product_root(&scratch.0, owner)?;
        let users = scratch.0.join("pkg/users");
        assert_eq!(first.root, users);
        assert_eq!(second.root, users);
        assert_eq!(
            fs::metadata(scratch.0.join("pkg"))?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&users)?.permissions().mode() & 0o777, 0o700);

        fs::remove_dir(&users)?;
        symlink("/tmp", &users)?;
        assert!(matches!(
            provision_product_root(&scratch.0, owner).map_err(LinuxPlatformError::code),
            Err(LinuxPlatformErrorCode::UnsafeFilesystemState)
        ));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unix_pair_reports_kernel_peer_identity() -> Result<(), Box<dyn Error>> {
        let (left, _right) = UnixStream::pair()?;
        let credentials = peer_credentials(&left)?;
        assert_eq!(credentials.uid(), Uid::current().as_raw());
        assert!(credentials.pid() > 0);
        Ok(())
    }
}
