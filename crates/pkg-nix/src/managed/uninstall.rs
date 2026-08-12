//! Persistent, authenticated removal of the product-managed Nix runtime.
//!
//! Preparation corroborates the root-owned ownership receipt and complete
//! signed asset set before any uninstall mutation. Removal later uses captured
//! filesystem identities and the signed manifest as its only static allowlist.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use pkg_core::System;
use rustix::{
    fs::{FlockOperation, fcntl_lock},
    io::Errno,
};

use super::ownership::{
    ManagedArtifact, ManagedArtifactKind, OwnershipExpectation, encode_ownership_asset_manifest,
    encode_ownership_receipt, ownership_receipt_path, verify_with_owner_uid,
};

const MAX_METADATA_BYTES: u64 = 1_048_576;
const MAX_DYNAMIC_ENTRIES: usize = 16_384;
const RUNTIME_PREFIX: &str = "/opt/pkg/nix";
const STORE_PREFIX: &str = "/nix/store";
const STORE_LINKS: &str = "/nix/store/.links";
const GC_LOCK: &str = "/nix/var/nix/gc.lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(25);

/// Stable closed failures for persistent managed-runtime removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedRuntimeRemovalErrorCode {
    /// The root-owned receipt or signed static asset set did not verify.
    OwnershipRefused,
    /// Persistent metadata or mutable Nix state was unsafe or unexpected.
    UnsafeState,
    /// An exact captured product asset changed before removal.
    IdentityChanged,
    /// One or more authenticated assets could not be removed.
    RemovalFailed,
}

/// Redacted persistent managed-runtime removal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedRuntimeRemovalError {
    code: ManagedRuntimeRemovalErrorCode,
}

impl ManagedRuntimeRemovalError {
    const fn new(code: ManagedRuntimeRemovalErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable closed failure code.
    #[must_use]
    pub const fn code(self) -> ManagedRuntimeRemovalErrorCode {
        self.code
    }
}

impl fmt::Display for ManagedRuntimeRemovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "managed Nix removal failed: {:?}", self.code)
    }
}

impl std::error::Error for ManagedRuntimeRemovalError {}

/// Terminal disposition of the managed Nix filesystem removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedRuntimeRemovalOutcome {
    /// The local store was empty and all product-owned runtime state was removed.
    Removed,
    /// An unrecognized store object remained, so all `/nix` state was preserved.
    StorePreserved,
}

#[derive(Debug, Clone)]
struct CapturedEntry {
    path: PathBuf,
    device: u64,
    inode: u64,
    kind: CapturedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturedKind {
    File,
    Directory,
    Symlink,
}

/// Single-use removal authority prepared from authenticated release state.
///
/// The value retains exact identities for the product runtime and its local
/// metadata. It has no public constructor and accepts no path at removal time.
#[derive(Debug)]
pub struct ManagedRuntimeRemoval {
    root: PathBuf,
    owner_uid: u32,
    store: Vec<CapturedEntry>,
    runtime: Vec<CapturedEntry>,
    metadata: Vec<CapturedEntry>,
}

/// Prepares persistent removal after verifying the complete authenticated
/// managed-Nix installation beneath `root`.
///
/// Production callers pass `/`. The expectation must come from authenticated
/// release metadata, never from the local receipt.
///
/// # Errors
///
/// Returns a closed error if ownership cannot be corroborated, local metadata
/// differs from the authenticated expectation, or an exact removal identity
/// cannot be captured.
pub fn prepare_managed_runtime_removal(
    root: &Path,
    expectation: &OwnershipExpectation,
) -> Result<ManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
    prepare_with_owner_uid(root, expectation, 0)
}

fn prepare_with_owner_uid(
    root: &Path,
    expectation: &OwnershipExpectation,
    owner_uid: u32,
) -> Result<ManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
    verify_with_owner_uid(root, expectation, owner_uid).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::OwnershipRefused)
    })?;
    let root = root.canonicalize().map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;

    let expected_manifest = encode_ownership_asset_manifest(
        expectation.system(),
        expectation.nix_version(),
        expectation.artifacts(),
    )
    .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?;
    let expected_receipt = encode_ownership_receipt(expectation).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    let metadata_paths = [
        (
            ownership_receipt_path(expectation.system()),
            expected_receipt,
        ),
        (asset_manifest_path(expectation.system()), expected_manifest),
    ];
    let mut metadata = Vec::with_capacity(metadata_paths.len());
    for (path, expected) in metadata_paths {
        let rooted_path = rooted(&root, path);
        if read_exact_metadata_file(&rooted_path, owner_uid)? != expected {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        metadata.push(capture(&rooted_path, owner_uid, true)?);
    }

    let runtime_root = rooted(&root, Path::new(RUNTIME_PREFIX));
    let runtime_device = capture(&runtime_root, owner_uid, false)?.device;
    let runtime = expectation
        .artifacts()
        .iter()
        .filter(|artifact| {
            artifact.path() == Path::new(RUNTIME_PREFIX)
                || artifact.path().starts_with(Path::new(RUNTIME_PREFIX))
        })
        .map(|artifact| capture_artifact(&root, artifact, owner_uid, Some(runtime_device)))
        .collect::<Result<Vec<_>, _>>()?;
    if runtime.is_empty() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let nix_root = capture(&rooted(&root, Path::new("/nix")), owner_uid, false)?;
    let store_root = capture(&rooted(&root, Path::new(STORE_PREFIX)), owner_uid, false)?;
    if nix_root.kind != CapturedKind::Directory
        || store_root.kind != CapturedKind::Directory
        || store_root.device != nix_root.device
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let store = expectation
        .artifacts()
        .iter()
        .filter(|artifact| {
            artifact.path() != Path::new(STORE_PREFIX)
                && artifact.path().starts_with(Path::new(STORE_PREFIX))
        })
        .map(|artifact| capture_artifact(&root, artifact, owner_uid, Some(store_root.device)))
        .collect::<Result<Vec<_>, _>>()?;
    if store.is_empty() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    verify_tree_matches_captured(&runtime_root, &runtime)?;

    Ok(ManagedRuntimeRemoval {
        root,
        owner_uid,
        store,
        runtime,
        metadata,
    })
}

impl ManagedRuntimeRemoval {
    /// Removes the authenticated product runtime after product roots are gone
    /// and root-local Nix garbage collection has completed.
    ///
    /// If any unrecognized store object remains, the method preserves all
    /// `/nix` state. Signed objects that survived GC are removed only while the
    /// Nix record lock is held and every root/profile liveness directory is
    /// empty. The independently authenticated `/opt/pkg/nix` runtime and local
    /// ownership metadata are still removed through captured identities.
    ///
    /// # Errors
    ///
    /// Returns a closed error if mutable Nix state is unsafe, a captured object
    /// changed identity, or an exact product object cannot be removed. After
    /// the first deletion, ordinary cleanup failures do not stop later exact
    /// cleanup actions. A final `RemovalFailed` means uninstall is incomplete;
    /// the product-level supported recovery is reinstall, as required by the
    /// V1 uninstall contract.
    pub fn remove(mut self) -> Result<ManagedRuntimeRemovalOutcome, ManagedRuntimeRemovalError> {
        let _gc_lock = acquire_gc_lock(&rooted(&self.root, Path::new(GC_LOCK)), self.owner_uid)?;
        let store_is_exclusive = store_contains_only_authenticated_objects(
            &rooted(&self.root, Path::new(STORE_PREFIX)),
            &self.store,
        )?;
        verify_tree_matches_captured(
            &rooted(&self.root, Path::new(RUNTIME_PREFIX)),
            &self.runtime,
        )?;
        verify_captured(&self.runtime)?;
        verify_captured(&self.metadata)?;
        let mut incomplete = false;
        if store_is_exclusive {
            verify_authenticated_store_trees(
                &rooted(&self.root, Path::new(STORE_PREFIX)),
                &self.store,
            )?;
            let mut dynamic = capture_dynamic_store_state(&self.root, self.owner_uid)?;
            verify_captured(&self.store)?;
            verify_captured(&dynamic)?;
            record_cleanup_result(remove_captured(&mut self.store), &mut incomplete)?;
            let store_is_empty =
                match store_has_only_links(&rooted(&self.root, Path::new(STORE_PREFIX))) {
                    Ok(is_empty) => is_empty,
                    Err(_) => {
                        incomplete = true;
                        false
                    }
                };
            if store_is_empty {
                record_cleanup_result(remove_captured(&mut dynamic), &mut incomplete)?;
            } else {
                incomplete = true;
            }
        }
        record_cleanup_result(remove_captured(&mut self.runtime), &mut incomplete)?;
        record_cleanup_result(remove_captured(&mut self.metadata), &mut incomplete)?;
        if incomplete {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::RemovalFailed,
            ));
        }
        Ok(if store_is_exclusive {
            ManagedRuntimeRemovalOutcome::Removed
        } else {
            ManagedRuntimeRemovalOutcome::StorePreserved
        })
    }
}

fn record_cleanup_result(
    result: Result<(), ManagedRuntimeRemovalError>,
    incomplete: &mut bool,
) -> Result<(), ManagedRuntimeRemovalError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ManagedRuntimeRemovalErrorCode::IdentityChanged => Err(error),
        Err(_) => {
            *incomplete = true;
            Ok(())
        }
    }
}

fn store_contains_only_authenticated_objects(
    store: &Path,
    authenticated: &[CapturedEntry],
) -> Result<bool, ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(store).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let allowed = authenticated
        .iter()
        .filter(|entry| entry.path.parent() == Some(store))
        .filter_map(|entry| entry.path.file_name().map(|name| name.to_os_string()))
        .collect::<BTreeSet<_>>();
    for child in fs::read_dir(store)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
    {
        let child = child.map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if child.file_name() != ".links" && !allowed.contains(&child.file_name()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn store_has_only_links(store: &Path) -> Result<bool, ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(store).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    for child in fs::read_dir(store)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
    {
        let child = child.map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if child.file_name() != ".links" {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_authenticated_store_trees(
    store: &Path,
    authenticated: &[CapturedEntry],
) -> Result<(), ManagedRuntimeRemovalError> {
    for root in authenticated
        .iter()
        .filter(|entry| entry.path.parent() == Some(store))
    {
        match fs::symlink_metadata(&root.path) {
            Ok(_) => verify_tree_matches_captured(&root.path, authenticated)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                for entry in authenticated
                    .iter()
                    .filter(|entry| entry.path.starts_with(&root.path))
                {
                    match fs::symlink_metadata(&entry.path) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        _ => {
                            return Err(ManagedRuntimeRemovalError::new(
                                ManagedRuntimeRemovalErrorCode::IdentityChanged,
                            ));
                        }
                    }
                }
            }
            Err(_) => {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::UnsafeState,
                ));
            }
        }
    }
    Ok(())
}

fn verify_tree_matches_captured(
    tree_root: &Path,
    captured: &[CapturedEntry],
) -> Result<(), ManagedRuntimeRemovalError> {
    let allowed = captured
        .iter()
        .filter(|entry| entry.path == tree_root || entry.path.starts_with(tree_root))
        .map(|entry| (entry.path.as_path(), entry))
        .collect::<BTreeMap<_, _>>();
    let root = allowed.get(tree_root).ok_or_else(|| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    let mut observed = BTreeSet::new();
    verify_tree_entry(tree_root, root.device, &allowed, &mut observed)?;
    if observed.len() != allowed.len() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::IdentityChanged,
        ));
    }
    Ok(())
}

fn verify_tree_entry(
    path: &Path,
    expected_device: u64,
    allowed: &BTreeMap<&Path, &CapturedEntry>,
    observed: &mut BTreeSet<PathBuf>,
) -> Result<(), ManagedRuntimeRemovalError> {
    if observed.len() >= MAX_DYNAMIC_ENTRIES {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::IdentityChanged)
    })?;
    let expected = allowed.get(path).ok_or_else(|| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if metadata.dev() != expected_device
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || observed_kind(&metadata) != Some(expected.kind)
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::IdentityChanged,
        ));
    }
    observed.insert(path.to_path_buf());
    if metadata.file_type().is_dir() {
        for child in fs::read_dir(path).map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })? {
            verify_tree_entry(
                &child
                    .map_err(|_| {
                        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
                    })?
                    .path(),
                expected_device,
                allowed,
                observed,
            )?;
        }
    }
    Ok(())
}

fn capture_dynamic_store_state(
    root: &Path,
    owner_uid: u32,
) -> Result<Vec<CapturedEntry>, ManagedRuntimeRemovalError> {
    let nix = capture(&rooted(root, Path::new("/nix")), owner_uid, false)?;
    let store = capture(&rooted(root, Path::new(STORE_PREFIX)), owner_uid, false)?;
    let state = capture(&rooted(root, Path::new("/nix/var/nix")), owner_uid, false)?;
    if nix.kind != CapturedKind::Directory
        || store.kind != CapturedKind::Directory
        || state.kind != CapturedKind::Directory
        || store.device != nix.device
        || state.device != nix.device
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let gcroots = rooted(root, Path::new("/nix/var/nix/gcroots"));
    let mut entries = Vec::new();
    match fs::read_dir(&gcroots) {
        Ok(children) => {
            for child in children {
                let child = child.map_err(|_| {
                    ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
                })?;
                if child.file_name() != "per-user" && child.file_name() != "auto" {
                    return Err(ManagedRuntimeRemovalError::new(
                        ManagedRuntimeRemovalErrorCode::UnsafeState,
                    ));
                }
                let path = child.path();
                let metadata = fs::symlink_metadata(&path).map_err(|_| {
                    ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
                })?;
                if !metadata.file_type().is_dir()
                    || metadata.dev() != state.device
                    || metadata.uid() != owner_uid
                    || metadata.mode() & 0o022 != 0
                    || fs::read_dir(&path)
                        .map_err(|_| {
                            ManagedRuntimeRemovalError::new(
                                ManagedRuntimeRemovalErrorCode::UnsafeState,
                            )
                        })?
                        .next()
                        .is_some()
                {
                    return Err(ManagedRuntimeRemovalError::new(
                        ManagedRuntimeRemovalErrorCode::UnsafeState,
                    ));
                }
                entries.push(capture_from_metadata(&path, &metadata)?);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    }
    require_empty_profiles(
        &rooted(root, Path::new("/nix/var/nix/profiles")),
        owner_uid,
        state.device,
    )?;
    require_empty_directory(
        &rooted(root, Path::new("/nix/var/nix/temproots")),
        owner_uid,
        state.device,
    )?;
    for (path, device) in [
        ("/nix/var/nix/db", state.device),
        ("/nix/var/nix/profiles", state.device),
        ("/nix/var/nix/temproots", state.device),
        (STORE_LINKS, store.device),
        (GC_LOCK, state.device),
    ] {
        capture_tree_on_device(
            &rooted(root, Path::new(path)),
            owner_uid,
            Some(device),
            &mut entries,
        )?;
    }
    Ok(entries)
}

fn require_empty_directory(
    path: &Path,
    owner_uid: u32,
    expected_device: u64,
) -> Result<(), ManagedRuntimeRemovalError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    };
    let first_entry = fs::read_dir(path)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
        .next();
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected_device
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o022 != 0
        || first_entry.is_some()
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    Ok(())
}

fn require_empty_profiles(
    path: &Path,
    owner_uid: u32,
    expected_device: u64,
) -> Result<(), ManagedRuntimeRemovalError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    };
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected_device
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    for child in fs::read_dir(path)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
    {
        let child = child.map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if child.file_name() != "per-user" {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        require_empty_directory(&child.path(), owner_uid, expected_device)?;
    }
    Ok(())
}

fn capture_tree_on_device(
    path: &Path,
    owner_uid: u32,
    expected_device: Option<u64>,
    entries: &mut Vec<CapturedEntry>,
) -> Result<(), ManagedRuntimeRemovalError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    };
    if metadata.uid() != owner_uid
        || expected_device.is_some_and(|device| metadata.dev() != device)
        || metadata.file_type().is_symlink()
        || !(metadata.file_type().is_dir() || metadata.file_type().is_file())
        || metadata.mode() & 0o022 != 0
        || entries.len() >= MAX_DYNAMIC_ENTRIES
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let device = expected_device.unwrap_or_else(|| metadata.dev());
    entries.push(capture_from_metadata(path, &metadata)?);
    if metadata.file_type().is_dir() {
        for child in fs::read_dir(path).map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })? {
            capture_tree_on_device(
                &child
                    .map_err(|_| {
                        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
                    })?
                    .path(),
                owner_uid,
                Some(device),
                entries,
            )?;
        }
    }
    Ok(())
}

fn acquire_gc_lock(path: &Path, owner_uid: u32) -> Result<fs::File, ManagedRuntimeRemovalError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
    let metadata = file.metadata().map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(Errno::AGAIN | Errno::ACCESS) if Instant::now() < deadline => {
                thread::sleep(LOCK_POLL);
            }
            Err(_) => {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::UnsafeState,
                ));
            }
        }
    }
}

fn capture_artifact(
    root: &Path,
    artifact: &ManagedArtifact,
    owner_uid: u32,
    expected_device: Option<u64>,
) -> Result<CapturedEntry, ManagedRuntimeRemovalError> {
    let captured = capture(&rooted(root, artifact.path()), owner_uid, false)?;
    let expected_kind = match artifact.kind() {
        ManagedArtifactKind::File => CapturedKind::File,
        ManagedArtifactKind::Directory => CapturedKind::Directory,
        ManagedArtifactKind::Symlink => CapturedKind::Symlink,
    };
    if captured.kind != expected_kind
        || expected_device.is_some_and(|device| captured.device != device)
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    Ok(captured)
}

fn capture(
    path: &Path,
    owner_uid: u32,
    regular_only: bool,
) -> Result<CapturedEntry, ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if metadata.uid() != owner_uid
        || regular_only && !metadata.file_type().is_file()
        || !(metadata.file_type().is_file()
            || metadata.file_type().is_dir()
            || metadata.file_type().is_symlink())
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    capture_from_metadata(path, &metadata)
}

fn capture_from_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<CapturedEntry, ManagedRuntimeRemovalError> {
    let kind = if metadata.file_type().is_file() {
        CapturedKind::File
    } else if metadata.file_type().is_dir() {
        CapturedKind::Directory
    } else if metadata.file_type().is_symlink() {
        CapturedKind::Symlink
    } else {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    };
    Ok(CapturedEntry {
        path: path.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
        kind,
    })
}

fn remove_captured(entries: &mut Vec<CapturedEntry>) -> Result<(), ManagedRuntimeRemovalError> {
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    let mut failed = false;
    for entry in entries.drain(..) {
        let result = match fs::symlink_metadata(&entry.path) {
            Ok(metadata)
                if metadata.dev() == entry.device
                    && metadata.ino() == entry.inode
                    && observed_kind(&metadata) == Some(entry.kind) =>
            {
                if entry.kind == CapturedKind::Directory {
                    fs::remove_dir(&entry.path)
                } else {
                    fs::remove_file(&entry.path)
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::IdentityChanged,
                ));
            }
            Err(error) => Err(error),
        };
        failed |= result.is_err();
    }
    if failed {
        Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::RemovalFailed,
        ))
    } else {
        Ok(())
    }
}

fn verify_captured(entries: &[CapturedEntry]) -> Result<(), ManagedRuntimeRemovalError> {
    for entry in entries {
        match fs::symlink_metadata(&entry.path) {
            Ok(metadata)
                if metadata.dev() == entry.device
                    && metadata.ino() == entry.inode
                    && observed_kind(&metadata) == Some(entry.kind) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::IdentityChanged,
                ));
            }
            Err(_) => {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::RemovalFailed,
                ));
            }
        }
    }
    Ok(())
}

fn observed_kind(metadata: &fs::Metadata) -> Option<CapturedKind> {
    if metadata.file_type().is_file() {
        Some(CapturedKind::File)
    } else if metadata.file_type().is_dir() {
        Some(CapturedKind::Directory)
    } else if metadata.file_type().is_symlink() {
        Some(CapturedKind::Symlink)
    } else {
        None
    }
}

fn read_exact_metadata_file(
    path: &Path,
    owner_uid: u32,
) -> Result<Vec<u8>, ManagedRuntimeRemovalError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
    let metadata = file.metadata().map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > MAX_METADATA_BYTES
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    Ok(bytes)
}

fn asset_manifest_path(system: System) -> &'static Path {
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        Path::new("/var/lib/pkg/managed-nix/assets-v1.json")
    } else {
        Path::new("/Library/Application Support/pkg/managed-nix/assets-v1.json")
    }
}

fn rooted(root: &Path, absolute: &Path) -> PathBuf {
    root.join(absolute.strip_prefix("/").unwrap_or(absolute))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    use crate::{
        NixVersion,
        managed::ownership::{ManagedGroup, ManagedGroupBindings},
    };
    use pkg_core::state::body_digest;
    use tempfile::TempDir;

    struct Fixture {
        temporary: TempDir,
        expectation: OwnershipExpectation,
        owner_uid: u32,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let owner_uid = fs::metadata(temporary.path())?.uid();
            let version = NixVersion::new("2.34.8")?;
            let runtime = b"authenticated nix";
            let artifacts = vec![
                ManagedArtifact::directory("/nix", ManagedGroup::BuildUsers, 0o755)?,
                ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775)?,
                ManagedArtifact::file(
                    "/nix/store/22222222222222222222222222222222-runtime",
                    ManagedGroup::BuildUsers,
                    0o444,
                    runtime.len() as u64,
                    body_digest(runtime),
                )?,
                ManagedArtifact::directory(
                    "/nix/store/33333333333333333333333333333333-runtime-tree",
                    ManagedGroup::BuildUsers,
                    0o755,
                )?,
                ManagedArtifact::file(
                    "/nix/store/33333333333333333333333333333333-runtime-tree/member",
                    ManagedGroup::BuildUsers,
                    0o444,
                    runtime.len() as u64,
                    body_digest(runtime),
                )?,
                ManagedArtifact::directory("/opt/pkg", ManagedGroup::BuildUsers, 0o755)?,
                ManagedArtifact::directory("/opt/pkg/nix", ManagedGroup::BuildUsers, 0o750)?,
                ManagedArtifact::directory("/opt/pkg/nix/2.34.8", ManagedGroup::BuildUsers, 0o750)?,
                ManagedArtifact::file(
                    "/opt/pkg/nix/2.34.8/nix",
                    ManagedGroup::BuildUsers,
                    0o550,
                    runtime.len() as u64,
                    body_digest(runtime),
                )?,
            ];
            let manifest =
                encode_ownership_asset_manifest(System::Aarch64Linux, &version, &artifacts)?;
            let expectation = OwnershipExpectation::new(
                System::Aarch64Linux,
                version,
                body_digest(&manifest),
                ManagedGroupBindings::same_gid_for_test(fs::metadata(temporary.path())?.gid()),
                artifacts,
            )?;
            for artifact in expectation.artifacts() {
                let path = rooted(temporary.path(), artifact.path());
                if artifact.kind() == ManagedArtifactKind::Directory {
                    fs::create_dir_all(&path)?;
                } else if artifact.kind() == ManagedArtifactKind::File {
                    fs::write(&path, runtime)?;
                }
            }
            for artifact in expectation.artifacts().iter().rev() {
                let path = rooted(temporary.path(), artifact.path());
                if matches!(
                    artifact.kind(),
                    ManagedArtifactKind::Directory | ManagedArtifactKind::File
                ) {
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(artifact.mode().unwrap_or(0o400)),
                    )?;
                }
            }
            let metadata_parent = rooted(temporary.path(), Path::new("/var/lib/pkg/managed-nix"));
            fs::create_dir_all(&metadata_parent)?;
            fs::set_permissions(&metadata_parent, fs::Permissions::from_mode(0o700))?;
            let receipt = rooted(
                temporary.path(),
                ownership_receipt_path(System::Aarch64Linux),
            );
            write_private(&receipt, &encode_ownership_receipt(&expectation)?)?;
            let manifest_path = rooted(temporary.path(), asset_manifest_path(System::Aarch64Linux));
            write_private(&manifest_path, &manifest)?;
            let state = rooted(temporary.path(), Path::new("/nix/var/nix"));
            fs::create_dir_all(&state)?;
            let gc_lock = rooted(temporary.path(), Path::new(GC_LOCK));
            write_private(&gc_lock, b"")?;
            verify_with_owner_uid(temporary.path(), &expectation, owner_uid).map_err(|error| {
                format!(
                    "fixture ownership: {:?} at {:?}",
                    error.code(),
                    error.artifact_index()
                )
            })?;
            Ok(Self {
                temporary,
                expectation,
                owner_uid,
            })
        }

        fn prepare(&self) -> Result<ManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
            prepare_with_owner_uid(self.temporary.path(), &self.expectation, self.owner_uid)
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        Ok(())
    }

    fn collect_authenticated_store(fixture: &Fixture) -> std::io::Result<()> {
        fs::remove_file(rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/22222222222222222222222222222222-runtime"),
        ))?;
        fs::remove_dir_all(rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/33333333333333333333333333333333-runtime-tree"),
        ))
    }

    #[test]
    fn exact_runtime_and_registration_state_are_removed() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = Fixture::new()?;
        for path in [
            "/nix/var/nix/db",
            "/nix/var/nix/profiles/per-user",
            "/nix/var/nix/temproots",
            "/nix/var/nix/gcroots/per-user",
            "/nix/store/.links",
        ] {
            let path = rooted(fixture.temporary.path(), Path::new(path));
            fs::create_dir_all(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        fs::write(
            rooted(
                fixture.temporary.path(),
                Path::new("/nix/var/nix/db/db.sqlite"),
            ),
            b"db",
        )?;
        let removal = fixture.prepare()?;
        collect_authenticated_store(&fixture)?;
        assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
        assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
        assert!(!rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db")).exists());
        assert!(rooted(fixture.temporary.path(), Path::new("/nix/store")).is_dir());
        Ok(())
    }

    #[test]
    fn unknown_gc_root_refuses_before_any_store_deletion() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = Fixture::new()?;
        let foreign_root = rooted(
            fixture.temporary.path(),
            Path::new("/nix/var/nix/gcroots/foreign"),
        );
        fs::create_dir_all(&foreign_root)?;
        fs::set_permissions(&foreign_root, fs::Permissions::from_mode(0o700))?;
        let removal = fixture.prepare()?;
        collect_authenticated_store(&fixture)?;

        let error = removal.remove().expect_err("unknown GC root must refuse");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(foreign_root.is_dir());
        Ok(())
    }

    #[test]
    fn temporary_root_record_refuses_before_any_store_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let temproots = rooted(
            fixture.temporary.path(),
            Path::new("/nix/var/nix/temproots"),
        );
        fs::create_dir_all(&temproots)?;
        fs::set_permissions(&temproots, fs::Permissions::from_mode(0o700))?;
        fs::write(temproots.join("active-client"), b"signed store path")?;
        let store_object = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/22222222222222222222222222222222-runtime"),
        );
        let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

        let error = fixture
            .prepare()?
            .remove()
            .expect_err("a temporary-root record must refuse direct store deletion");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(store_object.is_file());
        assert!(runtime.is_dir());
        Ok(())
    }

    #[test]
    fn user_profile_refuses_before_any_store_deletion() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let profile = rooted(
            fixture.temporary.path(),
            Path::new("/nix/var/nix/profiles/per-user/alice"),
        );
        fs::create_dir_all(&profile)?;
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o700))?;
        let store_object = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/22222222222222222222222222222222-runtime"),
        );
        let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

        let error = fixture
            .prepare()?
            .remove()
            .expect_err("a user profile must refuse direct store deletion");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(store_object.is_file());
        assert!(runtime.is_dir());
        Ok(())
    }

    #[test]
    fn unexpected_runtime_entry_refuses_before_any_store_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let unexpected = rooted(
            fixture.temporary.path(),
            Path::new("/opt/pkg/nix/unexpected"),
        );
        fs::write(&unexpected, b"foreign")?;
        let store_object = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/22222222222222222222222222222222-runtime"),
        );

        let error = fixture
            .prepare()
            .expect_err("unsigned runtime residue must refuse during preparation");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(unexpected.is_file());
        assert!(store_object.is_file());
        Ok(())
    }

    #[test]
    fn unexpected_store_tree_entry_refuses_before_any_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let unexpected = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/33333333333333333333333333333333-runtime-tree/unexpected"),
        );
        fs::write(&unexpected, b"foreign")?;
        let runtime = rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX));

        let error = fixture
            .prepare()?
            .remove()
            .expect_err("unsigned store-tree residue must refuse");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(unexpected.is_file());
        assert!(runtime.is_dir());
        Ok(())
    }

    #[test]
    fn complete_store_objects_collected_before_removal_are_accepted()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let removal = fixture.prepare()?;
        collect_authenticated_store(&fixture)?;

        assert_eq!(removal.remove()?, ManagedRuntimeRemovalOutcome::Removed);
        assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
        Ok(())
    }

    #[test]
    fn partially_collected_store_tree_refuses_before_runtime_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let removal = fixture.prepare()?;
        fs::remove_file(rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/33333333333333333333333333333333-runtime-tree/member"),
        ))?;

        let error = removal
            .remove()
            .expect_err("a partially removed signed store tree must refuse");
        assert_eq!(
            error.code(),
            ManagedRuntimeRemovalErrorCode::IdentityChanged
        );
        assert!(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).is_dir());
        Ok(())
    }

    #[test]
    fn ordinary_runtime_failure_still_removes_metadata_and_store_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let removal = fixture.prepare()?;
        collect_authenticated_store(&fixture)?;
        let runtime_version = rooted(fixture.temporary.path(), Path::new("/opt/pkg/nix/2.34.8"));
        fs::set_permissions(&runtime_version, fs::Permissions::from_mode(0o500))?;
        let receipt = rooted(
            fixture.temporary.path(),
            ownership_receipt_path(System::Aarch64Linux),
        );
        let manifest = rooted(
            fixture.temporary.path(),
            asset_manifest_path(System::Aarch64Linux),
        );

        let error = removal
            .remove()
            .expect_err("an undeletable runtime must report incomplete cleanup");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::RemovalFailed);
        assert!(!receipt.exists());
        assert!(!manifest.exists());
        assert_eq!(
            fs::read_dir(rooted(fixture.temporary.path(), Path::new(STORE_PREFIX)))?.count(),
            0
        );
        assert!(runtime_version.join("nix").is_file());
        Ok(())
    }

    #[test]
    fn mounted_dynamic_root_device_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let dynamic = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
        fs::create_dir_all(&dynamic)?;
        fs::set_permissions(&dynamic, fs::Permissions::from_mode(0o700))?;
        let actual_device = fs::symlink_metadata(&dynamic)?.dev();
        let mut entries = Vec::new();

        let error = capture_tree_on_device(
            &dynamic,
            fixture.owner_uid,
            Some(actual_device.wrapping_add(1)),
            &mut entries,
        )
        .expect_err("a mounted state root must refuse");
        assert_eq!(error.code(), ManagedRuntimeRemovalErrorCode::UnsafeState);
        assert!(entries.is_empty());
        Ok(())
    }

    #[test]
    fn gc_lock_waits_for_a_posix_record_lock() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let lock_path = rooted(fixture.temporary.path(), Path::new(GC_LOCK));
        let ready_path = fixture.temporary.path().join("record-lock-ready");
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "managed::uninstall::tests::posix_record_lock_holder",
                "--nocapture",
            ])
            .env("PKG_TEST_RECORD_LOCK_PATH", &lock_path)
            .env("PKG_TEST_RECORD_LOCK_READY", &ready_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() && Instant::now() < ready_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if !ready_path.exists() {
            let _ = child.kill();
            return Err("record-lock helper did not become ready".into());
        }

        let started = Instant::now();
        let lock = acquire_gc_lock(&lock_path, fixture.owner_uid)?;
        let waited = started.elapsed();
        drop(lock);
        assert!(child.wait()?.success());
        assert!(waited >= Duration::from_millis(250));
        Ok(())
    }

    #[test]
    #[ignore = "helper process for the POSIX record-lock interoperability test"]
    fn posix_record_lock_holder() {
        let Ok(lock_path) = std::env::var("PKG_TEST_RECORD_LOCK_PATH") else {
            return;
        };
        let ready_path = std::env::var("PKG_TEST_RECORD_LOCK_READY").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        fcntl_lock(&file, FlockOperation::LockExclusive).unwrap();
        fs::write(ready_path, b"ready").unwrap();
        thread::sleep(Duration::from_millis(500));
    }

    #[test]
    fn remaining_store_object_preserves_all_nix_state() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let foreign = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/11111111111111111111111111111111-foreign"),
        );
        fs::write(&foreign, b"foreign")?;
        let db = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
        fs::create_dir_all(&db)?;
        fs::set_permissions(&db, fs::Permissions::from_mode(0o700))?;

        assert_eq!(
            fixture.prepare()?.remove()?,
            ManagedRuntimeRemovalOutcome::StorePreserved
        );
        assert!(foreign.is_file());
        assert!(db.is_dir());
        assert!(!rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).exists());
        Ok(())
    }

    #[test]
    fn changed_runtime_identity_is_never_deleted() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let store_object = rooted(
            fixture.temporary.path(),
            Path::new("/nix/store/22222222222222222222222222222222-runtime"),
        );
        let db = rooted(fixture.temporary.path(), Path::new("/nix/var/nix/db"));
        fs::create_dir_all(&db)?;
        fs::set_permissions(&db, fs::Permissions::from_mode(0o700))?;
        let removal = fixture.prepare()?;
        let binary = rooted(
            fixture.temporary.path(),
            Path::new("/opt/pkg/nix/2.34.8/nix"),
        );
        fs::remove_file(&binary)?;
        fs::write(&binary, b"replacement")?;

        let error = removal.remove().expect_err("replacement must refuse");
        assert_eq!(
            error.code(),
            ManagedRuntimeRemovalErrorCode::IdentityChanged
        );
        assert_eq!(fs::read(binary)?, b"replacement");
        assert!(store_object.is_file());
        assert!(db.is_dir());
        Ok(())
    }

    #[test]
    fn changed_local_manifest_refuses_during_preparation() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = Fixture::new()?;
        fs::write(
            rooted(
                fixture.temporary.path(),
                asset_manifest_path(System::Aarch64Linux),
            ),
            b"{}",
        )?;

        assert_eq!(
            fixture.prepare().unwrap_err().code(),
            ManagedRuntimeRemovalErrorCode::UnsafeState
        );
        assert!(rooted(fixture.temporary.path(), Path::new(RUNTIME_PREFIX)).is_dir());
        Ok(())
    }
}
