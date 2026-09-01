//! Persistent, authenticated removal of the product-managed Nix runtime.
//!
//! Preparation corroborates the root-owned ownership receipt and complete
//! signed asset set before any uninstall mutation. Removal later uses captured
//! filesystem identities and the signed manifest as its only static allowlist.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use pkg_core::{StorePath, System};
use rustix::{
    fs::{FlockOperation, fcntl_lock},
    io::Errno,
};

use super::ownership::{
    ManagedArtifact, ManagedArtifactKind, OwnershipExpectation, encode_ownership_asset_manifest,
    encode_ownership_receipt, ownership_receipt_path, verify_artifacts_absent_or_exact,
    verify_receipt_ancestors, verify_with_owner_uid,
};

const MAX_METADATA_BYTES: u64 = 1_048_576;
// ponytail: measured preview closure bound; raise only with new clean-host evidence.
const MAX_DYNAMIC_ENTRIES: usize = 2_097_152;
const RUNTIME_PREFIX: &str = "/opt/pkg/nix";
const STORE_PREFIX: &str = "/nix/store";
const STORE_LINKS: &str = "/nix/store/.links";
const GC_LOCK: &str = "/nix/var/nix/gc.lock";
const GC_SOCKET_DIR: &str = "/nix/var/nix/gc-socket";
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
    /// An unrecognized store object remained, so the complete runtime was preserved.
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
    Socket,
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

/// Exclusive store-removal authority holding Nix's GC lock.
#[derive(Debug)]
pub struct ExclusiveManagedRuntimeRemoval {
    removal: ManagedRuntimeRemoval,
    _gc_lock: fs::File,
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
    let receipt_path = ownership_receipt_path(expectation.system());
    let metadata_root = receipt_path.parent().ok_or_else(|| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    let metadata_paths = [
        (receipt_path, expected_receipt),
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
    metadata.push(capture(&rooted(&root, metadata_root), owner_uid, false)?);

    capture_authenticated_state(&root, expectation, owner_uid, metadata)
}

/// Prepares persistent removal of an interrupted authenticated managed runtime
/// whose final ownership receipt was never published.
///
/// Authentication comes from the caller's `expectation` (derived from the
/// authenticated installer bundle), not from local metadata. Missing signed
/// artifacts are accepted. Each present artifact must match exactly. An
/// optional receipt or manifest must match the authenticated bytes. Foreign,
/// tampered, redirected, or otherwise ambiguous state is refused before capture.
///
/// The returned [`ManagedRuntimeRemoval`] is drained by the existing GC-lock,
/// liveness, and store-removal API; this entry never deletes `/nix/store`
/// directly. Callers clean fixed Nix registration state with
/// daemon registration rollback between this prepare and
/// `remove`; the armed `PR_SET_PDEATHSIG` is the barrier against a surviving
/// daemon, so no daemon is started or stopped here.
///
/// Production callers pass `/`.
///
/// # Errors
///
/// Returns `Ok(None)` when no provisioned runtime state exists. Returns a closed
/// error if any present path is not exact or cannot be captured safely.
pub fn prepare_managed_runtime_removal_without_receipt(
    root: &Path,
    expectation: &OwnershipExpectation,
) -> Result<Option<ManagedRuntimeRemoval>, ManagedRuntimeRemovalError> {
    prepare_without_receipt_with_owner_uid(root, expectation, 0)
}

fn prepare_without_receipt_with_owner_uid(
    root: &Path,
    expectation: &OwnershipExpectation,
    owner_uid: u32,
) -> Result<Option<ManagedRuntimeRemoval>, ManagedRuntimeRemovalError> {
    let root = root.canonicalize().map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    verify_artifacts_absent_or_exact(&root, expectation, owner_uid).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::OwnershipRefused)
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
    let mut metadata = Vec::new();
    capture_optional_metadata(
        &root,
        ownership_receipt_path(expectation.system()),
        &expected_receipt,
        owner_uid,
        &mut metadata,
    )?;
    capture_optional_metadata(
        &root,
        asset_manifest_path(expectation.system()),
        &expected_manifest,
        owner_uid,
        &mut metadata,
    )?;

    capture_partial_authenticated_state(&root, expectation, owner_uid, metadata)
}

fn capture_optional_metadata(
    root: &Path,
    path: &Path,
    expected: &[u8],
    owner_uid: u32,
    metadata: &mut Vec<CapturedEntry>,
) -> Result<(), ManagedRuntimeRemovalError> {
    let path = rooted(root, path);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        Ok(_) => {}
    }
    verify_receipt_ancestors(
        root,
        path.parent().ok_or_else(|| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?,
        owner_uid,
    )
    .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?;
    if read_exact_metadata_file(&path, owner_uid)? != expected {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    metadata.push(capture(&path, owner_uid, true)?);
    Ok(())
}

fn capture_partial_authenticated_state(
    root: &Path,
    expectation: &OwnershipExpectation,
    owner_uid: u32,
    metadata: Vec<CapturedEntry>,
) -> Result<Option<ManagedRuntimeRemoval>, ManagedRuntimeRemovalError> {
    let runtime_root = rooted(root, Path::new(RUNTIME_PREFIX));
    let runtime = match fs::symlink_metadata(&runtime_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        Ok(_) => {
            let runtime_device = capture(&runtime_root, owner_uid, false)?.device;
            capture_present_artifacts(
                root,
                expectation,
                owner_uid,
                |artifact| {
                    artifact.path() == Path::new(RUNTIME_PREFIX)
                        || artifact.path().starts_with(Path::new(RUNTIME_PREFIX))
                },
                Some(runtime_device),
            )?
        }
    };
    if !runtime.is_empty() {
        verify_tree_matches_captured(&runtime_root, &runtime)?;
    }

    let nix_root = capture_optional(&rooted(root, Path::new("/nix")), owner_uid)?;
    let store_root = capture_optional(&rooted(root, Path::new(STORE_PREFIX)), owner_uid)?;
    let store = match (nix_root, store_root) {
        (None, None) | (Some(_), None) => Vec::new(),
        (None, Some(_)) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        (Some(nix_root), Some(store_root)) => {
            if nix_root.kind != CapturedKind::Directory
                || store_root.kind != CapturedKind::Directory
                || store_root.device != nix_root.device
            {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::UnsafeState,
                ));
            }
            capture_present_artifacts(
                root,
                expectation,
                owner_uid,
                |artifact| {
                    artifact.path() != Path::new(STORE_PREFIX)
                        && artifact.path().starts_with(Path::new(STORE_PREFIX))
                },
                Some(store_root.device),
            )?
        }
    };

    if runtime.is_empty() && store.is_empty() && metadata.is_empty() {
        return Ok(None);
    }
    Ok(Some(ManagedRuntimeRemoval {
        root: root.to_path_buf(),
        owner_uid,
        store,
        runtime,
        metadata,
    }))
}

fn capture_optional(
    path: &Path,
    owner_uid: u32,
) -> Result<Option<CapturedEntry>, ManagedRuntimeRemovalError> {
    match fs::symlink_metadata(path) {
        Ok(_) => capture(path, owner_uid, false).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        )),
    }
}

fn capture_present_artifacts<F>(
    root: &Path,
    expectation: &OwnershipExpectation,
    owner_uid: u32,
    select: F,
    expected_device: Option<u64>,
) -> Result<Vec<CapturedEntry>, ManagedRuntimeRemovalError>
where
    F: Fn(&ManagedArtifact) -> bool,
{
    expectation
        .artifacts()
        .iter()
        .filter(|artifact| select(artifact))
        .filter_map(|artifact| {
            let path = rooted(root, artifact.path());
            match fs::symlink_metadata(path) {
                Ok(_) => Some(capture_artifact(root, artifact, owner_uid, expected_device)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => Some(Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::UnsafeState,
                ))),
            }
        })
        .collect()
}

fn capture_authenticated_state(
    root: &Path,
    expectation: &OwnershipExpectation,
    owner_uid: u32,
    metadata: Vec<CapturedEntry>,
) -> Result<ManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
    let runtime_root = rooted(root, Path::new(RUNTIME_PREFIX));
    let runtime_device = capture(&runtime_root, owner_uid, false)?.device;
    let runtime = expectation
        .artifacts()
        .iter()
        .filter(|artifact| {
            artifact.path() == Path::new(RUNTIME_PREFIX)
                || artifact.path().starts_with(Path::new(RUNTIME_PREFIX))
        })
        .map(|artifact| capture_artifact(root, artifact, owner_uid, Some(runtime_device)))
        .collect::<Result<Vec<_>, _>>()?;
    if runtime.is_empty() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let nix_root = capture(&rooted(root, Path::new("/nix")), owner_uid, false)?;
    let store_root = capture(&rooted(root, Path::new(STORE_PREFIX)), owner_uid, false)?;
    if nix_root.kind != CapturedKind::Directory
        || store_root.kind != CapturedKind::Directory
        || store_root.device != nix_root.device
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let store = capture_present_artifacts(
        root,
        expectation,
        owner_uid,
        |artifact| {
            artifact.path() != Path::new(STORE_PREFIX)
                && artifact.path().starts_with(Path::new(STORE_PREFIX))
        },
        Some(store_root.device),
    )?;
    verify_tree_matches_captured(&runtime_root, &runtime)?;

    Ok(ManagedRuntimeRemoval {
        root: root.to_path_buf(),
        owner_uid,
        store,
        runtime,
        metadata,
    })
}

impl ManagedRuntimeRemoval {
    /// Acquires exclusive store-removal authority before closure capture.
    pub fn begin_exclusive_removal(
        self,
    ) -> Result<ExclusiveManagedRuntimeRemoval, ManagedRuntimeRemovalError> {
        let gc_lock = acquire_gc_lock(&rooted(&self.root, Path::new(GC_LOCK)), self.owner_uid)?;
        capture_dynamic_store_state(&self.root, self.owner_uid, false)?;
        Ok(ExclusiveManagedRuntimeRemoval {
            removal: self,
            _gc_lock: gc_lock,
        })
    }

    /// Proves that no foreign GC root, profile, or temporary root can keep a
    /// store object live.
    ///
    /// The proof is made while holding Nix's GC lock. Removal repeats the same
    /// proof under a newly acquired lock before it deletes any store object.
    ///
    /// # Errors
    ///
    /// Returns a closed error if a foreign liveness record exists or mutable
    /// Nix state is unsafe.
    pub fn verify_no_foreign_liveness(&self) -> Result<(), ManagedRuntimeRemovalError> {
        let _gc_lock = acquire_gc_lock(&rooted(&self.root, Path::new(GC_LOCK)), self.owner_uid)?;
        capture_dynamic_store_state(&self.root, self.owner_uid, true).map(|_| ())
    }

    /// Proves that every direct store object belongs to the authenticated
    /// runtime or to the exact product closure captured before root removal.
    ///
    /// `.links` is Nix-owned implementation state and is the only additional
    /// direct entry accepted. The method is read-only and bounded.
    ///
    /// # Errors
    ///
    /// Returns a closed error if the store is unsafe, input paths are outside
    /// the canonical store, or directory enumeration exceeds its bound.
    pub fn store_contains_only_product_objects(
        &self,
        product_closure: &[StorePath],
    ) -> Result<bool, ManagedRuntimeRemovalError> {
        store_contains_only_authenticated_objects(
            &rooted(&self.root, Path::new(STORE_PREFIX)),
            &self.store,
            product_closure,
        )
    }

    /// Removes the authenticated product runtime after product roots are gone
    /// and root-local Nix garbage collection has completed.
    ///
    /// If any unrecognized store object remains, the method preserves all
    /// `/nix` state. Signed objects that survived GC are removed only while the
    /// Nix record lock is held and every root/profile liveness directory is
    /// empty. An unrecognized store object preserves `/opt/pkg/nix` and its
    /// ownership metadata so the remaining store is never stranded.
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
        self.remove_with_store_policy(true, false)
    }

    /// Removes only the authenticated product runtime and registration metadata.
    ///
    /// This variant is required when the install manifest records `/nix` as
    /// pre-existing. It never acquires the Nix GC lock, inspects store contents,
    /// or mutates any path below `/nix`.
    ///
    /// # Errors
    ///
    /// Returns a closed error if the captured runtime or metadata changed or
    /// could not be removed completely.
    pub fn remove_preserving_store(
        mut self,
    ) -> Result<ManagedRuntimeRemovalOutcome, ManagedRuntimeRemovalError> {
        self.remove_with_store_policy(false, false)
    }

    fn remove_with_store_policy(
        &mut self,
        allow_store_removal: bool,
        lock_already_held: bool,
    ) -> Result<ManagedRuntimeRemovalOutcome, ManagedRuntimeRemovalError> {
        let _gc_lock = if allow_store_removal && !lock_already_held {
            Some(acquire_gc_lock(
                &rooted(&self.root, Path::new(GC_LOCK)),
                self.owner_uid,
            )?)
        } else {
            None
        };
        let store_is_exclusive = allow_store_removal
            && store_contains_only_authenticated_objects(
                &rooted(&self.root, Path::new(STORE_PREFIX)),
                &self.store,
                &[],
            )?;
        if allow_store_removal && !store_is_exclusive {
            return Ok(ManagedRuntimeRemovalOutcome::StorePreserved);
        }
        let runtime_root = rooted(&self.root, Path::new(RUNTIME_PREFIX));
        if self.runtime.is_empty() {
            match fs::symlink_metadata(runtime_root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) | Ok(_) => {
                    return Err(ManagedRuntimeRemovalError::new(
                        ManagedRuntimeRemovalErrorCode::IdentityChanged,
                    ));
                }
            }
        } else {
            verify_tree_matches_captured(&runtime_root, &self.runtime)?;
        }
        verify_captured(&self.runtime)?;
        verify_captured(&self.metadata)?;
        let mut incomplete = false;
        let mut dynamic = Vec::new();
        if store_is_exclusive {
            verify_authenticated_store_trees(
                &rooted(&self.root, Path::new(STORE_PREFIX)),
                &self.store,
            )?;
            dynamic = capture_dynamic_store_state(&self.root, self.owner_uid, true)?;
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
            if !store_is_empty {
                incomplete = true;
            }
            if incomplete {
                return Err(ManagedRuntimeRemovalError::new(
                    ManagedRuntimeRemovalErrorCode::RemovalFailed,
                ));
            }
        }
        record_cleanup_result(remove_captured(&mut self.runtime), &mut incomplete)?;
        record_cleanup_result(remove_captured(&mut self.metadata), &mut incomplete)?;
        if store_is_exclusive && !incomplete {
            record_cleanup_result(remove_captured(&mut dynamic), &mut incomplete)?;
        }
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

    fn remove_with_store_policy_locked(
        &mut self,
    ) -> Result<ManagedRuntimeRemovalOutcome, ManagedRuntimeRemovalError> {
        self.remove_with_store_policy(true, true)
    }
}

impl ExclusiveManagedRuntimeRemoval {
    /// Captures the exact product closure after the GC lock is held.
    pub fn capture_product_closure(
        &mut self,
        product_closure: &[StorePath],
        registered_paths: &[StorePath],
    ) -> Result<(), ManagedRuntimeRemovalError> {
        let store = rooted(&self.removal.root, Path::new(STORE_PREFIX));
        let registered = registered_paths
            .iter()
            .map(StorePath::as_str)
            .collect::<BTreeSet<_>>();
        if product_closure
            .iter()
            .any(|path| !registered.contains(path.as_str()))
            || !store_contains_only_authenticated_objects(
                &store,
                &self.removal.store,
                registered_paths,
            )?
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        let device = capture(&store, self.removal.owner_uid, false)?.device;
        for path in registered_paths {
            let path = rooted(&self.removal.root, Path::new(path.as_str()));
            if !self.removal.store.iter().any(|entry| entry.path == path) {
                capture_tree_on_device(
                    &path,
                    self.removal.owner_uid,
                    Some(device),
                    &mut self.removal.store,
                )?;
            }
        }
        verify_authenticated_store_trees(&store, &self.removal.store)
    }

    /// Removes the closure and runtime while the pre-root-removal lock is held.
    pub fn remove(&mut self) -> Result<ManagedRuntimeRemovalOutcome, ManagedRuntimeRemovalError> {
        self.removal.remove_with_store_policy_locked()
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
    product_closure: &[StorePath],
) -> Result<bool, ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(store).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let mut allowed = authenticated
        .iter()
        .filter(|entry| entry.path.parent() == Some(store))
        .filter_map(|entry| entry.path.file_name().map(std::ffi::OsStr::to_os_string))
        .collect::<BTreeSet<_>>();
    for path in product_closure {
        let path = Path::new(path.as_str());
        if path.parent() != Some(Path::new(STORE_PREFIX)) {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        let name = path.file_name().ok_or_else(|| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        allowed.insert(name.to_os_string());
    }
    let mut count = 0_usize;
    for child in fs::read_dir(store)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
    {
        count = count.checked_add(1).ok_or_else(|| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if count > MAX_DYNAMIC_ENTRIES {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
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
    let mut trees = BTreeMap::<PathBuf, Vec<&CapturedEntry>>::new();
    for entry in authenticated {
        let relative = entry.path.strip_prefix(store).map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        let Some(Component::Normal(name)) = relative.components().next() else {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        };
        trees.entry(store.join(name)).or_default().push(entry);
    }

    for (root, entries) in trees {
        match fs::symlink_metadata(&root) {
            Ok(_) => verify_tree_matches_captured(&root, entries)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                for entry in entries {
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

fn verify_tree_matches_captured<'a>(
    tree_root: &Path,
    captured: impl IntoIterator<Item = &'a CapturedEntry>,
) -> Result<(), ManagedRuntimeRemovalError> {
    let allowed = captured
        .into_iter()
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
    require_product_roots_empty: bool,
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
                let path = child.path();
                match child.file_name().to_str() {
                    Some("per-user" | "auto") => {
                        entries.push(capture_empty_directory(&path, owner_uid, state.device)?);
                    }
                    Some("pkg") => capture_product_gcroots(
                        &path,
                        owner_uid,
                        state.device,
                        require_product_roots_empty,
                        &mut entries,
                    )?,
                    _ => {
                        return Err(ManagedRuntimeRemovalError::new(
                            ManagedRuntimeRemovalErrorCode::UnsafeState,
                        ));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    }
    if let Some(gcroots) = capture_optional(&gcroots, owner_uid)? {
        entries.push(gcroots);
    }
    require_empty_profiles(
        &rooted(root, Path::new("/nix/var/nix/profiles")),
        owner_uid,
        state.device,
    )?;
    let temporary_root_locks = lock_stale_temporary_roots(
        &rooted(root, Path::new("/nix/var/nix/temproots")),
        owner_uid,
        state.device,
    )?;
    capture_gc_socket_directory(root, owner_uid, state.device, &mut entries)?;
    for (path, device) in [
        ("/nix/var/log", nix.device),
        ("/nix/var/nix/builds", state.device),
        ("/nix/var/nix/userpool", state.device),
        ("/nix/var/nix/cgroups", state.device),
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
    drop(temporary_root_locks);
    Ok(entries)
}

fn capture_gc_socket_directory(
    root: &Path,
    owner_uid: u32,
    expected_device: u64,
    entries: &mut Vec<CapturedEntry>,
) -> Result<(), ManagedRuntimeRemovalError> {
    let directory = rooted(root, Path::new(GC_SOCKET_DIR));
    let metadata = match fs::symlink_metadata(&directory) {
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
    let mut children = fs::read_dir(&directory).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if let Some(child) = children.next() {
        let child = child.map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if child.file_name() != "socket" || children.next().is_some() {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        let socket = child.metadata().map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if !socket.file_type().is_socket()
            || socket.dev() != expected_device
            || socket.uid() != owner_uid
            || socket.mode() & 0o777 != 0o666
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        entries.push(CapturedEntry {
            path: child.path(),
            device: socket.dev(),
            inode: socket.ino(),
            kind: CapturedKind::Socket,
        });
    }
    entries.push(capture_from_metadata(&directory, &metadata)?);
    Ok(())
}

fn capture_empty_directory(
    path: &Path,
    owner_uid: u32,
    expected_device: u64,
) -> Result<CapturedEntry, ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected_device
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o022 != 0
        || fs::read_dir(path)
            .map_err(|_| {
                ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
            })?
            .next()
            .is_some()
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    capture_from_metadata(path, &metadata)
}

fn capture_product_gcroots(
    path: &Path,
    owner_uid: u32,
    expected_device: u64,
    require_empty: bool,
    entries: &mut Vec<CapturedEntry>,
) -> Result<(), ManagedRuntimeRemovalError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected_device
        || metadata.uid() != owner_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    let mut children = fs::read_dir(path).map_err(|_| {
        ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
    })?;
    let users = children
        .next()
        .ok_or_else(|| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?
        .map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
    if users.file_name() != "users" || children.next().is_some() {
        return Err(ManagedRuntimeRemovalError::new(
            ManagedRuntimeRemovalErrorCode::UnsafeState,
        ));
    }
    entries.push(capture_from_metadata(path, &metadata)?);
    if require_empty {
        entries.push(capture_empty_directory(
            &users.path(),
            owner_uid,
            expected_device,
        )?);
    } else {
        let users_metadata = fs::symlink_metadata(users.path()).map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if !users_metadata.file_type().is_dir()
            || users_metadata.dev() != expected_device
            || users_metadata.uid() != owner_uid
            || users_metadata.mode() & 0o022 != 0
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
    }
    Ok(())
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

fn lock_stale_temporary_roots(
    path: &Path,
    owner_uid: u32,
    expected_device: u64,
) -> Result<Vec<fs::File>, ManagedRuntimeRemovalError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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

    let mut locks = Vec::new();
    for child in fs::read_dir(path)
        .map_err(|_| ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState))?
    {
        let child = child.map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if child
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        let child_path = child.path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&child_path)
            .map_err(|_| {
                ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
            })?;
        let file_metadata = file.metadata().map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if !file_metadata.file_type().is_file()
            || file_metadata.dev() != expected_device
            || file_metadata.uid() != owner_uid
            || file_metadata.nlink() != 1
            || file_metadata.mode() & 0o022 != 0
            || fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive).is_err()
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        let path_metadata = fs::symlink_metadata(&child_path).map_err(|_| {
            ManagedRuntimeRemovalError::new(ManagedRuntimeRemovalErrorCode::UnsafeState)
        })?;
        if path_metadata.dev() != file_metadata.dev()
            || path_metadata.ino() != file_metadata.ino()
            || !path_metadata.file_type().is_file()
        {
            return Err(ManagedRuntimeRemovalError::new(
                ManagedRuntimeRemovalErrorCode::UnsafeState,
            ));
        }
        locks.push(file);
    }
    Ok(locks)
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
    let is_symlink = metadata.file_type().is_symlink();
    if metadata.uid() != owner_uid
        || expected_device.is_some_and(|device| metadata.dev() != device)
        || !(metadata.file_type().is_dir() || metadata.file_type().is_file() || is_symlink)
        || !is_symlink && metadata.mode() & 0o022 != 0
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
        .create(true)
        .mode(0o600)
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
    } else if metadata.file_type().is_socket() {
        Some(CapturedKind::Socket)
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
mod tests;
