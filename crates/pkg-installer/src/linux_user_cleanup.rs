//! Bounded cleanup of product GC roots and matching per-user state trees.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;

/// One opened user state parent and its component name.
type StateParent = (OwnedFd, &'static str);
#[cfg(test)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use nix::unistd::{Uid, User};
use pkg_core::StorePath;
use pkg_store::{STATE_OWNERSHIP_MARKER_BYTES, STATE_OWNERSHIP_MARKER_NAME};
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, fsync, mkdirat, open, openat, statat,
    symlinkat, unlinkat,
};
#[cfg(target_os = "linux")]
use rustix::fs::{StatxFlags, statx};
use rustix::io::Errno;

const USER_ROOTS: &str = "/nix/var/nix/gcroots/pkg/users";
#[cfg(target_os = "macos")]
const USER_STATE_COMPONENTS: &[&str] = &["Library", "Application Support", "pkg"];
#[cfg(not(target_os = "macos"))]
const USER_STATE_COMPONENTS: &[&str] = &[".local", "share", "pkg"];
const MAX_USERS: usize = 4_096;
const MAX_ENTRIES: usize = 65_536;
pub const MAX_DURABLE_USER_SNAPSHOT: usize = 2_048;

pub fn remove_owned_tree(
    root: &Path,
    target: &Path,
    root_owner_uid: u32,
    tree_owner_uid: u32,
) -> Result<(), LinuxUserCleanupError> {
    let relative = target.strip_prefix("/").map_err(|_| unsafe_state())?;
    let parent_path = relative.parent().ok_or_else(unsafe_state)?;
    let name = relative.file_name().ok_or_else(unsafe_state)?;
    let Some(parent) = open_parent_chain(root, parent_path, root_owner_uid)? else {
        return Ok(());
    };
    ensure_target_present(&parent, name)?;
    let expected_mount = mount_boundary_fd(&parent)?;
    let mut count = 0;
    remove_entry(&parent, name, tree_owner_uid, expected_mount, &mut count)?;
    fsync(&parent).map_err(|_| operation_failed())
}

/// Walks `parent_path` under `root`, validating every directory against the
/// root owner. `Ok(None)` means a component is missing, so the tree is
/// already absent.
fn open_parent_chain(
    root: &Path,
    parent_path: &Path,
    root_owner_uid: u32,
) -> Result<Option<OwnedFd>, LinuxUserCleanupError> {
    let mut parent = open(root, directory_flags(), Mode::empty()).map_err(|_| unsafe_state())?;
    validate_directory(
        &fstat(&parent).map_err(|_| operation_failed())?,
        root_owner_uid,
    )?;
    for component in parent_path.components() {
        let Component::Normal(component) = component else {
            return Err(unsafe_state());
        };
        parent = match openat(&parent, component, directory_flags(), Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unsafe_state()),
        };
        validate_directory(
            &fstat(&parent).map_err(|_| operation_failed())?,
            root_owner_uid,
        )?;
    }
    Ok(Some(parent))
}

/// Confirms the target entry exists under `parent` without following links.
fn ensure_target_present(parent: &OwnedFd, name: &OsStr) -> Result<(), LinuxUserCleanupError> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) | Err(Errno::NOENT) => Ok(()),
        Err(_) => Err(operation_failed()),
    }
}

/// Stable failures for Linux per-user uninstall cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxUserCleanupErrorCode {
    /// A fixed root, uid, home, or state tree was unsafe.
    UnsafeState,
    /// A bounded filesystem or account lookup failed.
    OperationFailed,
    /// An object changed after it was captured.
    IdentityChanged,
}

/// Redacted Linux per-user cleanup failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxUserCleanupError {
    code: LinuxUserCleanupErrorCode,
}

impl LinuxUserCleanupError {
    const fn new(code: LinuxUserCleanupErrorCode) -> Self {
        Self { code }
    }
}

impl fmt::Display for LinuxUserCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux user cleanup failed")
    }
}

impl std::error::Error for LinuxUserCleanupError {}

trait HomeDirectoryResolver: Send {
    fn home_for_uid(&mut self, uid: u32) -> Result<PathBuf, LinuxUserCleanupError>;
}

struct ProductionHomeDirectoryResolver;

impl HomeDirectoryResolver for ProductionHomeDirectoryResolver {
    fn home_for_uid(&mut self, uid: u32) -> Result<PathBuf, LinuxUserCleanupError> {
        let user = User::from_uid(Uid::from_raw(uid))
            .map_err(|_| operation_failed())?
            .ok_or_else(unsafe_state)?;
        if user.uid.as_raw() != uid || !user.dir.is_absolute() || user.dir == Path::new("/") {
            return Err(unsafe_state());
        }
        Ok(user.dir)
    }
}

#[cfg(test)]
struct FixedHomeDirectoryResolver {
    uid: u32,
    home: PathBuf,
}

#[cfg(test)]
impl HomeDirectoryResolver for FixedHomeDirectoryResolver {
    fn home_for_uid(&mut self, uid: u32) -> Result<PathBuf, LinuxUserCleanupError> {
        if uid == self.uid {
            Ok(self.home.clone())
        } else {
            Err(unsafe_state())
        }
    }
}

/// Production owner for fixed product GC roots and matching user state.
pub struct LinuxUserCleanup {
    root: PathBuf,
    root_owner_uid: u32,
    resolver: Box<dyn HomeDirectoryResolver>,
    registered_uids: Vec<u32>,
    registered_uids_bound: bool,
    store_roots: Vec<StorePath>,
    root_snapshot: Vec<RootSnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootSnapshotEntry {
    relative: PathBuf,
    kind: RootSnapshotKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RootSnapshotKind {
    Directory { mode: u16 },
    Symlink { target: StorePath },
}

struct RootCapture<'a> {
    owner_uid: u32,
    expected_mount: u64,
    count: &'a mut usize,
    targets: &'a mut BTreeMap<String, StorePath>,
    snapshot: &'a mut Vec<RootSnapshotEntry>,
}

impl fmt::Debug for LinuxUserCleanup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxUserCleanup")
            .field("registered_uid_count", &self.registered_uids.len())
            .finish_non_exhaustive()
    }
}

impl LinuxUserCleanup {
    /// Creates the production cleaner fixed to `/` and the system account view.
    #[must_use]
    pub fn production() -> Self {
        Self {
            root: PathBuf::from("/"),
            root_owner_uid: 0,
            resolver: Box::new(ProductionHomeDirectoryResolver),
            registered_uids: Vec::new(),
            registered_uids_bound: false,
            store_roots: Vec::new(),
            root_snapshot: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: &Path, home: &Path) -> Result<Self, LinuxUserCleanupError> {
        let root = root.canonicalize().map_err(|_| unsafe_state())?;
        let home = home.canonicalize().map_err(|_| unsafe_state())?;
        let uid = home.metadata().map_err(|_| unsafe_state())?.uid();
        Ok(Self {
            root,
            root_owner_uid: uid,
            resolver: Box::new(FixedHomeDirectoryResolver { uid, home }),
            registered_uids: Vec::new(),
            registered_uids_bound: false,
            store_roots: Vec::new(),
            root_snapshot: Vec::new(),
        })
    }

    /// Removes only roots below the fixed product user-root directory.
    ///
    /// UIDs and store targets are captured before mutation. This method never
    /// removes the registered users' state directories.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for unsafe names, ownership, links, mount
    /// boundaries, identity changes, excessive entries, or deletion failures.
    pub fn remove_user_roots(&mut self) -> Result<(), LinuxUserCleanupError> {
        if self.root_snapshot.is_empty() {
            self.capture_user_roots()?;
        }
        let path = rooted(&self.root, Path::new(USER_ROOTS));
        let Some(root) = open_absolute_directory(&path, self.root_owner_uid)? else {
            if !self.registered_uids_bound {
                self.registered_uids_bound = true;
            }
            return Ok(());
        };
        let root_boundary = mount_boundary_fd(&root)?;
        let names = directory_names(&root)?;
        if !self.registered_uids_bound
            || names.iter().any(|name| {
                name.to_str()
                    .and_then(|name| parse_uid(name).ok())
                    .is_none_or(|uid| self.registered_uids.binary_search(&uid).is_err())
            })
        {
            return Err(unsafe_state());
        }
        let mut count = 0;
        for name in names {
            remove_entry(&root, &name, self.root_owner_uid, root_boundary, &mut count)?;
        }
        fsync(&root).map_err(|_| operation_failed())?;
        Ok(())
    }

    /// Captures product root identities and targets without mutation.
    pub fn capture_user_roots(&mut self) -> Result<(), LinuxUserCleanupError> {
        if !self.root_snapshot.is_empty() {
            return Err(unsafe_state());
        }
        let path = rooted(&self.root, Path::new(USER_ROOTS));
        let Some(root) = open_absolute_directory(&path, self.root_owner_uid)? else {
            if !self.registered_uids_bound {
                self.registered_uids_bound = true;
            }
            return Ok(());
        };
        let root_boundary = mount_boundary_fd(&root)?;
        let names = directory_names(&root)?;
        let mut uids = BTreeSet::new();
        for name in &names {
            let text = name.to_str().ok_or_else(unsafe_state)?;
            let uid = parse_uid(text)?;
            if !uids.insert(uid) || uids.len() > MAX_USERS {
                return Err(unsafe_state());
            }
        }
        let observed_uids = uids.iter().copied().collect::<Vec<_>>();
        if self.registered_uids_bound
            && observed_uids
                .iter()
                .any(|uid| self.registered_uids.binary_search(uid).is_err())
        {
            return Err(unsafe_state());
        }
        let mut store_roots = BTreeMap::new();
        let mut root_snapshot = Vec::new();
        let mut root_entry_count = 0;
        let mut capture = RootCapture {
            owner_uid: self.root_owner_uid,
            expected_mount: root_boundary,
            count: &mut root_entry_count,
            targets: &mut store_roots,
            snapshot: &mut root_snapshot,
        };
        for name in &names {
            collect_store_targets(&root, name, Path::new(name), &mut capture)?;
        }
        if !self.registered_uids_bound {
            self.registered_uids = observed_uids;
            self.registered_uids_bound = true;
        }
        self.store_roots = store_roots.into_values().collect();
        self.root_snapshot = root_snapshot;
        Ok(())
    }

    /// Returns the ordered UIDs captured from product roots.
    #[must_use]
    pub fn registered_uids(&self) -> Option<&[u32]> {
        self.registered_uids_bound
            .then_some(self.registered_uids.as_slice())
    }

    /// Binds a durable ordered UID snapshot before retrying root cleanup.
    ///
    /// # Errors
    ///
    /// Returns a closed error for zero, duplicate, unsorted, or excessive UIDs,
    /// or when a different snapshot is already bound.
    pub fn bind_registered_uids(&mut self, uids: &[u32]) -> Result<(), LinuxUserCleanupError> {
        if uids.len() > MAX_DURABLE_USER_SNAPSHOT
            || uids.contains(&0)
            || uids.windows(2).any(|pair| pair[0] >= pair[1])
            || self.registered_uids_bound && self.registered_uids != uids
            || !self.root_snapshot.is_empty()
            || !self.store_roots.is_empty()
        {
            return Err(unsafe_state());
        }
        if !self.registered_uids_bound {
            self.registered_uids = uids.to_vec();
            self.registered_uids_bound = true;
        }
        Ok(())
    }

    /// Restores the exact product root tree captured before removal.
    ///
    /// Existing entries must match the snapshot. Missing entries are recreated
    /// descriptor-relative in parent-before-child order.
    ///
    /// # Errors
    ///
    /// Returns a closed error for a collision, unsafe ancestor, changed mount,
    /// invalid snapshot, or incomplete recreation.
    pub fn restore_user_roots(&mut self) -> Result<(), LinuxUserCleanupError> {
        if self.root_snapshot.is_empty() {
            return Ok(());
        }
        let path = rooted(&self.root, Path::new(USER_ROOTS));
        let root = open_absolute_directory(&path, self.root_owner_uid)?.ok_or_else(unsafe_state)?;
        let boundary = mount_boundary_fd(&root)?;
        self.root_snapshot
            .sort_by_key(|entry| entry.relative.components().count());
        for entry in &self.root_snapshot {
            restore_root_entry(&root, entry, self.root_owner_uid, boundary)?;
        }
        fsync(&root).map_err(|_| operation_failed())
    }

    /// Removes the fixed state tree for every UID captured from product roots.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when a home lookup or exact state-tree removal
    /// cannot be completed safely.
    pub fn remove_registered_user_state(&mut self) -> Result<(), LinuxUserCleanupError> {
        let mut failed = false;
        let mut count = 0;
        for uid in self.registered_uids.iter().copied() {
            if remove_user_state(self.resolver.as_mut(), uid, &mut count).is_err() {
                failed = true;
            }
        }
        if failed {
            Err(operation_failed())
        } else {
            Ok(())
        }
    }

    /// Verifies that captured product roots and user state are absent.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when any captured residue remains.
    pub fn verify_absent(&mut self) -> Result<(), LinuxUserCleanupError> {
        let roots = rooted(&self.root, Path::new(USER_ROOTS));
        if let Some(root) = open_absolute_directory(&roots, self.root_owner_uid)?
            && !directory_names(&root)?.is_empty()
        {
            return Err(unsafe_state());
        }
        for uid in self.registered_uids.iter().copied() {
            if user_state_exists(self.resolver.as_mut(), uid)? {
                return Err(unsafe_state());
            }
        }
        Ok(())
    }

    /// Returns the exact store roots captured from the removed product tree.
    #[must_use]
    pub fn store_roots(&self) -> &[StorePath] {
        &self.store_roots
    }
}

fn remove_user_state(
    resolver: &mut dyn HomeDirectoryResolver,
    uid: u32,
    count: &mut usize,
) -> Result<(), LinuxUserCleanupError> {
    let home_path = resolver.home_for_uid(uid)?;
    let Some(home) = open_absolute_directory(&home_path, uid)? else {
        return Err(unsafe_state());
    };
    let Some((parent, name)) = open_state_parent(&home, uid)? else {
        return Ok(());
    };
    let parent_boundary = mount_boundary_fd(&parent)?;
    match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(()),
        Ok(before) => {
            remove_present_state(&parent, name, &before, uid, parent_boundary, count)?;
        }
        Err(_) => return Err(operation_failed()),
    }
    fsync(&parent).map_err(|_| operation_failed())
}

/// Validates and removes one present per-user state directory, refusing
/// non-directories, identity changes, and entry-limit overruns.
fn remove_present_state(
    parent: &OwnedFd,
    name: &str,
    before: &Stat,
    uid: u32,
    parent_boundary: u64,
    count: &mut usize,
) -> Result<(), LinuxUserCleanupError> {
    let observed_mount = mount_boundary_at(parent, OsStr::new(name))?;
    validate_entry(before, uid, observed_mount, parent_boundary)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
        return Err(unsafe_state());
    }
    let state =
        openat(parent, name, directory_flags(), Mode::empty()).map_err(|_| unsafe_state())?;
    let opened = fstat(&state).map_err(|_| operation_failed())?;
    if !same_identity(before, &opened) || mount_boundary_fd(&state)? != parent_boundary {
        return Err(identity_changed());
    }
    verify_state_ownership_marker(&state, uid, parent_boundary)?;
    if *count >= MAX_ENTRIES {
        return Err(unsafe_state());
    }
    *count += 1;
    remove_opened_directory(
        parent,
        OsStr::new(name),
        &state,
        before,
        uid,
        parent_boundary,
        count,
    )
}

fn verify_state_ownership_marker(
    state: &OwnedFd,
    owner_uid: u32,
    expected_mount: u64,
) -> Result<(), LinuxUserCleanupError> {
    let marker = openat(
        state,
        STATE_OWNERSHIP_MARKER_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| unsafe_state())?;
    let before = fstat(&marker).map_err(|_| operation_failed())?;
    let expected_size =
        i64::try_from(STATE_OWNERSHIP_MARKER_BYTES.len()).map_err(|_| unsafe_state())?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
        || before.st_uid != owner_uid
        || before.st_mode & 0o777 != 0o600
        || before.st_nlink != 1
        || before.st_size != expected_size
        || mount_boundary_fd(&marker)? != expected_mount
    {
        return Err(unsafe_state());
    }
    let mut file = std::fs::File::from(marker);
    let mut bytes = Vec::with_capacity(STATE_OWNERSHIP_MARKER_BYTES.len());
    file.by_ref()
        .take(STATE_OWNERSHIP_MARKER_BYTES.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| operation_failed())?;
    let after = fstat(&file).map_err(|_| operation_failed())?;
    if bytes != STATE_OWNERSHIP_MARKER_BYTES || !same_identity(&before, &after) {
        return Err(identity_changed());
    }
    Ok(())
}

fn user_state_exists(
    resolver: &mut dyn HomeDirectoryResolver,
    uid: u32,
) -> Result<bool, LinuxUserCleanupError> {
    let home_path = resolver.home_for_uid(uid)?;
    let Some(home) = open_absolute_directory(&home_path, uid)? else {
        return Err(unsafe_state());
    };
    let Some((parent, name)) = open_state_parent(&home, uid)? else {
        return Ok(false);
    };
    match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(_) => Err(operation_failed()),
    }
}

fn open_state_parent(
    home: &OwnedFd,
    uid: u32,
) -> Result<Option<StateParent>, LinuxUserCleanupError> {
    let (name, parents) = USER_STATE_COMPONENTS
        .split_last()
        .ok_or_else(unsafe_state)?;
    let mut parent =
        openat(home, ".", directory_flags(), Mode::empty()).map_err(|_| operation_failed())?;
    for component in parents {
        parent = match openat(&parent, *component, directory_flags(), Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unsafe_state()),
        };
        validate_directory(&fstat(&parent).map_err(|_| operation_failed())?, uid)?;
    }
    Ok(Some((parent, name)))
}

fn open_absolute_directory(
    path: &Path,
    final_owner: u32,
) -> Result<Option<OwnedFd>, LinuxUserCleanupError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(unsafe_state());
    }
    let mut current =
        open("/", directory_flags(), Mode::empty()).map_err(|_| operation_failed())?;
    let mut saw_component = false;
    for component in path.components() {
        let Component::Normal(component) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(unsafe_state());
        };
        saw_component = true;
        current = match openat(&current, component, directory_flags(), Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unsafe_state()),
        };
        let stat = fstat(&current).map_err(|_| operation_failed())?;
        validate_ancestor_directory(&stat, final_owner)?;
    }
    if !saw_component {
        return Err(unsafe_state());
    }
    let stat = fstat(&current).map_err(|_| operation_failed())?;
    validate_directory(&stat, final_owner)?;
    Ok(Some(current))
}

fn directory_names(directory: &OwnedFd) -> Result<Vec<OsString>, LinuxUserCleanupError> {
    let mut names = Vec::new();
    let mut stream = Dir::read_from(directory).map_err(|_| operation_failed())?;
    for entry in &mut stream {
        let entry = entry.map_err(|_| operation_failed())?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if bytes.is_empty() || bytes.contains(&b'/') || names.len() >= MAX_ENTRIES {
            return Err(unsafe_state());
        }
        names.push(OsStr::from_bytes(bytes).to_os_string());
    }
    Ok(names)
}

fn remove_entry(
    parent: &OwnedFd,
    name: &OsStr,
    owner_uid: u32,
    expected_mount: u64,
    count: &mut usize,
) -> Result<(), LinuxUserCleanupError> {
    if *count >= MAX_ENTRIES {
        return Err(unsafe_state());
    }
    *count += 1;
    let before = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
        if error == Errno::NOENT {
            LinuxUserCleanupError::new(LinuxUserCleanupErrorCode::IdentityChanged)
        } else {
            operation_failed()
        }
    })?;
    let observed_mount = mount_boundary_at(parent, name)?;
    validate_entry(&before, owner_uid, observed_mount, expected_mount)?;
    let kind = FileType::from_raw_mode(before.st_mode);
    if kind == FileType::Directory {
        let child =
            openat(parent, name, directory_flags(), Mode::empty()).map_err(|_| unsafe_state())?;
        let opened = fstat(&child).map_err(|_| operation_failed())?;
        if !same_identity(&before, &opened) || mount_boundary_fd(&child)? != expected_mount {
            return Err(identity_changed());
        }
        remove_opened_directory(
            parent,
            name,
            &child,
            &before,
            owner_uid,
            expected_mount,
            count,
        )?;
    } else {
        verify_name_identity(parent, name, &before, expected_mount)?;
        unlinkat(parent, name, AtFlags::empty()).map_err(|_| operation_failed())?;
    }
    Ok(())
}

fn remove_opened_directory(
    parent: &OwnedFd,
    name: &OsStr,
    directory: &OwnedFd,
    captured: &Stat,
    owner_uid: u32,
    expected_mount: u64,
    count: &mut usize,
) -> Result<(), LinuxUserCleanupError> {
    for child_name in directory_names(directory)? {
        remove_entry(directory, &child_name, owner_uid, expected_mount, count)?;
    }
    fsync(directory).map_err(|_| operation_failed())?;
    verify_name_identity(parent, name, captured, expected_mount)?;
    unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|_| operation_failed())
}

fn collect_store_targets(
    parent: &OwnedFd,
    name: &OsStr,
    relative: &Path,
    capture: &mut RootCapture<'_>,
) -> Result<(), LinuxUserCleanupError> {
    if *capture.count >= MAX_ENTRIES {
        return Err(unsafe_state());
    }
    *capture.count += 1;
    let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| operation_failed())?;
    let observed_mount = mount_boundary_at(parent, name)?;
    validate_entry(
        &stat,
        capture.owner_uid,
        observed_mount,
        capture.expected_mount,
    )?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Directory => {
            #[cfg(target_os = "linux")]
            let mode = u16::try_from(stat.st_mode & 0o7777).map_err(|_| unsafe_state())?;
            #[cfg(not(target_os = "linux"))]
            let mode = stat.st_mode & 0o7777;
            capture.snapshot.push(RootSnapshotEntry {
                relative: relative.to_path_buf(),
                kind: RootSnapshotKind::Directory { mode },
            });
            let child = openat(parent, name, directory_flags(), Mode::empty())
                .map_err(|_| unsafe_state())?;
            let opened = fstat(&child).map_err(|_| operation_failed())?;
            if !same_identity(&stat, &opened)
                || mount_boundary_fd(&child)? != capture.expected_mount
            {
                return Err(identity_changed());
            }
            for child_name in directory_names(&child)? {
                let child_relative = relative.join(&child_name);
                collect_store_targets(&child, &child_name, &child_relative, capture)?;
            }
        }
        FileType::Symlink => {
            let target =
                rustix::fs::readlinkat(parent, name, Vec::new()).map_err(|_| operation_failed())?;
            let target = target.to_str().map_err(|_| unsafe_state())?;
            let target = StorePath::new(target).map_err(|_| unsafe_state())?;
            capture.snapshot.push(RootSnapshotEntry {
                relative: relative.to_path_buf(),
                kind: RootSnapshotKind::Symlink {
                    target: target.clone(),
                },
            });
            capture.targets.insert(target.as_str().to_owned(), target);
        }
        _ => return Err(unsafe_state()),
    }
    Ok(())
}

fn restore_root_entry(
    root: &OwnedFd,
    entry: &RootSnapshotEntry,
    owner_uid: u32,
    expected_mount: u64,
) -> Result<(), LinuxUserCleanupError> {
    let (parent, name) = open_relative_parent(root, &entry.relative, owner_uid)?;
    match statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => verify_restored_entry(&parent, &name, &stat, entry, owner_uid, expected_mount),
        Err(Errno::NOENT) => {
            match &entry.kind {
                RootSnapshotKind::Directory { mode } => {
                    #[cfg(target_os = "linux")]
                    let mode = u32::from(*mode);
                    #[cfg(not(target_os = "linux"))]
                    let mode = *mode;
                    mkdirat(&parent, &name, Mode::from_raw_mode(mode))
                        .map_err(|_| operation_failed())?;
                }
                RootSnapshotKind::Symlink { target } => {
                    symlinkat(target.as_str(), &parent, &name).map_err(|_| operation_failed())?;
                }
            }
            let stat = statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| operation_failed())?;
            verify_restored_entry(&parent, &name, &stat, entry, owner_uid, expected_mount)?;
            fsync(&parent).map_err(|_| operation_failed())
        }
        Err(_) => Err(operation_failed()),
    }
}

fn open_relative_parent(
    root: &OwnedFd,
    relative: &Path,
    owner_uid: u32,
) -> Result<(OwnedFd, OsString), LinuxUserCleanupError> {
    let mut components = relative.components().peekable();
    let mut parent =
        openat(root, ".", directory_flags(), Mode::empty()).map_err(|_| operation_failed())?;
    loop {
        let component = components.next().ok_or_else(unsafe_state)?;
        let Component::Normal(name) = component else {
            return Err(unsafe_state());
        };
        if components.peek().is_none() {
            return Ok((parent, name.to_os_string()));
        }
        parent =
            openat(&parent, name, directory_flags(), Mode::empty()).map_err(|_| unsafe_state())?;
        validate_directory(&fstat(&parent).map_err(|_| operation_failed())?, owner_uid)?;
    }
}

fn verify_restored_entry(
    parent: &OwnedFd,
    name: &OsStr,
    stat: &Stat,
    entry: &RootSnapshotEntry,
    owner_uid: u32,
    expected_mount: u64,
) -> Result<(), LinuxUserCleanupError> {
    let observed_mount = mount_boundary_at(parent, name)?;
    validate_entry(stat, owner_uid, observed_mount, expected_mount)?;
    match &entry.kind {
        RootSnapshotKind::Directory { mode } => {
            #[cfg(target_os = "linux")]
            let observed_mode = u16::try_from(stat.st_mode & 0o7777).map_err(|_| unsafe_state())?;
            #[cfg(not(target_os = "linux"))]
            let observed_mode = stat.st_mode & 0o7777;
            if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                || observed_mode != *mode
            {
                return Err(unsafe_state());
            }
            let directory = openat(parent, name, directory_flags(), Mode::empty())
                .map_err(|_| unsafe_state())?;
            if !same_identity(stat, &fstat(&directory).map_err(|_| operation_failed())?)
                || mount_boundary_fd(&directory)? != expected_mount
            {
                return Err(identity_changed());
            }
            Ok(())
        }
        RootSnapshotKind::Symlink { target } => {
            if FileType::from_raw_mode(stat.st_mode) != FileType::Symlink {
                return Err(unsafe_state());
            }
            let observed =
                rustix::fs::readlinkat(parent, name, Vec::new()).map_err(|_| operation_failed())?;
            if observed.to_str() == Ok(target.as_str()) {
                Ok(())
            } else {
                Err(identity_changed())
            }
        }
    }
}

fn verify_name_identity(
    parent: &OwnedFd,
    name: &OsStr,
    expected: &Stat,
    expected_mount: u64,
) -> Result<(), LinuxUserCleanupError> {
    let observed =
        statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| identity_changed())?;
    if same_identity(expected, &observed) && mount_boundary_at(parent, name)? == expected_mount {
        Ok(())
    } else {
        Err(identity_changed())
    }
}

fn validate_ancestor_directory(stat: &Stat, final_owner: u32) -> Result<(), LinuxUserCleanupError> {
    let sticky_root_directory = stat.st_uid == 0 && stat.st_mode & 0o1000 != 0;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || !matches!(stat.st_uid, 0) && stat.st_uid != final_owner
        || stat.st_mode & 0o022 != 0 && !sticky_root_directory
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn validate_directory(stat: &Stat, owner_uid: u32) -> Result<(), LinuxUserCleanupError> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || stat.st_uid != owner_uid
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn validate_entry(
    stat: &Stat,
    owner_uid: u32,
    observed_mount: u64,
    expected_mount: u64,
) -> Result<(), LinuxUserCleanupError> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    if stat.st_uid != owner_uid
        || observed_mount != expected_mount
        || !matches!(
            kind,
            FileType::RegularFile | FileType::Directory | FileType::Symlink
        )
        || kind != FileType::Symlink && stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn same_identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && FileType::from_raw_mode(left.st_mode) == FileType::from_raw_mode(right.st_mode)
}

fn parse_uid(name: &str) -> Result<u32, LinuxUserCleanupError> {
    if name.is_empty()
        || name.len() > 10
        || name.len() > 1 && name.starts_with('0')
        || !name.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(unsafe_state());
    }
    name.parse().map_err(|_| unsafe_state())
}

fn rooted(root: &Path, absolute: &Path) -> PathBuf {
    root.join(absolute.strip_prefix("/").unwrap_or(absolute))
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

const fn unsafe_state() -> LinuxUserCleanupError {
    LinuxUserCleanupError::new(LinuxUserCleanupErrorCode::UnsafeState)
}

const fn operation_failed() -> LinuxUserCleanupError {
    LinuxUserCleanupError::new(LinuxUserCleanupErrorCode::OperationFailed)
}

const fn identity_changed() -> LinuxUserCleanupError {
    LinuxUserCleanupError::new(LinuxUserCleanupErrorCode::IdentityChanged)
}

#[cfg(target_os = "linux")]
fn mount_boundary_fd(fd: &OwnedFd) -> Result<u64, LinuxUserCleanupError> {
    let observation = statx(
        fd,
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::MNT_ID | StatxFlags::BASIC_STATS,
    )
    .map_err(|_| unsafe_state())?;
    if observation.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(unsafe_state());
    }
    Ok(observation.stx_mnt_id)
}

#[cfg(target_os = "linux")]
fn mount_boundary_at(parent: &OwnedFd, name: &OsStr) -> Result<u64, LinuxUserCleanupError> {
    let observation = statx(
        parent,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::MNT_ID | StatxFlags::BASIC_STATS,
    )
    .map_err(|_| unsafe_state())?;
    if observation.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(unsafe_state());
    }
    Ok(observation.stx_mnt_id)
}

#[cfg(not(target_os = "linux"))]
fn mount_boundary_fd(fd: &OwnedFd) -> Result<u64, LinuxUserCleanupError> {
    device(&fstat(fd).map_err(|_| operation_failed())?)
}

#[cfg(not(target_os = "linux"))]
fn mount_boundary_at(parent: &OwnedFd, name: &OsStr) -> Result<u64, LinuxUserCleanupError> {
    device(&statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| operation_failed())?)
}

#[cfg(not(target_os = "linux"))]
fn device(stat: &Stat) -> Result<u64, LinuxUserCleanupError> {
    u64::try_from(stat.st_dev).map_err(|_| unsafe_state())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    #[cfg(target_os = "linux")]
    use std::process::Command;
    use tempfile::TempDir;

    fn cleaner(
        temp: &TempDir,
        home: &Path,
    ) -> Result<LinuxUserCleanup, Box<dyn std::error::Error>> {
        Ok(LinuxUserCleanup::for_test(temp.path(), home)?)
    }

    #[test]
    fn removes_roots_and_user_state_without_following_symlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let state = home.join(USER_STATE_COMPONENTS.join("/"));
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&state)?;
        fs::create_dir_all(&roots)?;
        fs::write(
            state.join(STATE_OWNERSHIP_MARKER_NAME),
            STATE_OWNERSHIP_MARKER_BYTES,
        )?;
        fs::set_permissions(
            state.join(STATE_OWNERSHIP_MARKER_NAME),
            fs::Permissions::from_mode(0o600),
        )?;
        let uid = fs::metadata(&home)?.uid();
        let root = roots.join(uid.to_string());
        let outside = temp.path().join("outside");
        fs::write(&outside, b"keep")?;
        symlink("/nix/store/22222222222222222222222222222222-example", &root)?;
        symlink(&outside, state.join("link"))?;
        fs::write(state.join("owned"), b"remove")?;

        let mut cleaner = cleaner(&temp, &home)?;
        cleaner.remove_user_roots()?;
        cleaner.remove_registered_user_state()?;
        cleaner.verify_absent()?;
        assert_eq!(
            cleaner.store_roots(),
            [StorePath::new(
                "/nix/store/22222222222222222222222222222222-example"
            )?]
        );
        assert_eq!(fs::read(outside)?, b"keep");
        Ok(())
    }

    #[test]
    fn durable_uid_snapshot_refuses_a_new_foreign_root_before_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&roots)?;
        let uid = fs::metadata(&home)?.uid();
        if uid == 0 || uid == u32::MAX {
            return Err("test requires a non-root bounded uid".into());
        }
        symlink(
            "/nix/store/22222222222222222222222222222222-example",
            roots.join(uid.to_string()),
        )?;
        let mut first_process = cleaner(&temp, &home)?;
        first_process.capture_user_roots()?;
        let snapshot = first_process
            .registered_uids()
            .ok_or("missing uid snapshot")?
            .to_vec();
        first_process.remove_user_roots()?;
        let foreign = roots.join((uid + 1).to_string());
        symlink(
            "/nix/store/33333333333333333333333333333333-foreign",
            &foreign,
        )?;

        let mut second_process = cleaner(&temp, &home)?;
        second_process.bind_registered_uids(&snapshot)?;
        assert!(second_process.remove_user_roots().is_err());
        assert!(foreign.symlink_metadata().is_ok());
        Ok(())
    }

    #[test]
    fn restores_exact_product_roots_without_overwriting_a_collision()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&roots)?;
        let uid = fs::metadata(&home)?.uid();
        let root = roots.join(uid.to_string());
        let target = "/nix/store/22222222222222222222222222222222-example";
        symlink(target, &root)?;
        let mut cleaner = cleaner(&temp, &home)?;

        cleaner.remove_user_roots()?;
        assert!(fs::symlink_metadata(&root).is_err());
        cleaner.restore_user_roots()?;
        cleaner.restore_user_roots()?;
        assert_eq!(fs::read_link(&root)?, Path::new(target));

        fs::remove_file(&root)?;
        symlink("/nix/store/11111111111111111111111111111111-foreign", &root)?;
        assert!(cleaner.restore_user_roots().is_err());
        assert_eq!(
            fs::read_link(&root)?,
            Path::new("/nix/store/11111111111111111111111111111111-foreign")
        );
        Ok(())
    }

    #[test]
    fn refuses_unmarked_user_state_without_deleting_it() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let state = home.join(USER_STATE_COMPONENTS.join("/"));
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&state)?;
        fs::create_dir_all(&roots)?;
        fs::write(state.join("foreign"), b"keep")?;
        let uid = fs::metadata(&home)?.uid();
        symlink(
            "/nix/store/22222222222222222222222222222222-example",
            roots.join(uid.to_string()),
        )?;
        let mut cleaner = cleaner(&temp, &home)?;

        cleaner.remove_user_roots()?;
        assert!(cleaner.remove_registered_user_state().is_err());
        assert_eq!(fs::read(state.join("foreign"))?, b"keep");
        assert!(fs::symlink_metadata(roots.join(uid.to_string())).is_err());
        Ok(())
    }

    #[test]
    fn renamed_verified_state_never_redirects_recursive_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home_path = temp.path().join("home");
        let state_path = home_path.join(USER_STATE_COMPONENTS.join("/"));
        fs::create_dir_all(&state_path)?;
        fs::write(
            state_path.join(STATE_OWNERSHIP_MARKER_NAME),
            STATE_OWNERSHIP_MARKER_BYTES,
        )?;
        fs::set_permissions(
            state_path.join(STATE_OWNERSHIP_MARKER_NAME),
            fs::Permissions::from_mode(0o600),
        )?;
        fs::write(state_path.join("owned"), b"remove")?;
        let uid = fs::metadata(&home_path)?.uid();
        let home = open(&home_path, directory_flags(), Mode::empty())?;
        validate_directory(&fstat(&home)?, uid)?;
        let (parent, name) = open_state_parent(&home, uid)?.ok_or("state parent missing")?;
        let boundary = mount_boundary_fd(&parent)?;
        let before = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let state = openat(&parent, name, directory_flags(), Mode::empty())?;
        verify_state_ownership_marker(&state, uid, boundary)?;

        let displaced = state_path.with_file_name("pkg.displaced");
        fs::rename(&state_path, &displaced)?;
        fs::create_dir(&state_path)?;
        fs::write(state_path.join("foreign"), b"keep")?;
        let mut count = 1;
        assert!(
            remove_opened_directory(
                &parent,
                OsStr::new(name),
                &state,
                &before,
                uid,
                boundary,
                &mut count,
            )
            .is_err()
        );
        assert_eq!(fs::read(state_path.join("foreign"))?, b"keep");
        Ok(())
    }

    #[test]
    fn rejects_noncanonical_uid_before_mutation() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&roots)?;
        symlink("/nix/store/example", roots.join("01"))?;
        let mut cleaner = cleaner(&temp, &home)?;
        assert_eq!(
            cleaner.remove_user_roots().map_err(|error| error.code),
            Err(LinuxUserCleanupErrorCode::UnsafeState)
        );
        assert!(roots.join("01").exists() || fs::symlink_metadata(roots.join("01")).is_ok());
        Ok(())
    }

    #[test]
    fn refuses_symlinked_state_ancestor() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        let outside = temp.path().join("outside");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&roots)?;
        fs::create_dir_all(&outside)?;
        let uid = fs::metadata(&home)?.uid();
        symlink(
            "/nix/store/22222222222222222222222222222222-example",
            roots.join(uid.to_string()),
        )?;
        symlink(&outside, home.join(USER_STATE_COMPONENTS[0]))?;
        let mut cleaner = cleaner(&temp, &home)?;
        cleaner.remove_user_roots()?;
        assert!(cleaner.remove_registered_user_state().is_err());
        assert!(fs::symlink_metadata(roots.join(uid.to_string())).is_err());
        assert!(outside.exists());
        Ok(())
    }

    #[test]
    fn rejects_writable_state_tree() -> Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let home = temp.path().join("home");
        let state = home.join(USER_STATE_COMPONENTS.join("/"));
        let roots = rooted(temp.path(), Path::new(USER_ROOTS));
        fs::create_dir_all(&state)?;
        fs::create_dir_all(&roots)?;
        let uid = fs::metadata(&home)?.uid();
        symlink(
            "/nix/store/22222222222222222222222222222222-example",
            roots.join(uid.to_string()),
        )?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o777))?;
        let mut cleaner = cleaner(&temp, &home)?;
        cleaner.remove_user_roots()?;
        assert!(cleaner.remove_registered_user_state().is_err());
        assert!(fs::symlink_metadata(roots.join(uid.to_string())).is_err());
        assert!(state.exists());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a privileged Linux mount namespace"]
    fn refuses_same_filesystem_bind_mount_without_touching_target()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Unmount(PathBuf);
        impl Drop for Unmount {
            fn drop(&mut self) {
                let _ = Command::new("umount").arg(&self.0).status();
            }
        }
        if !Uid::effective().is_root() {
            return Err("test requires root".into());
        }

        let temporary = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        let mountpoint = rooted(
            temporary.path(),
            Path::new("/nix/var/nix/gcroots/pkg/users/0/mounted"),
        );
        fs::create_dir_all(&mountpoint)?;
        fs::write(target.path().join("must-remain"), b"foreign")?;
        let status = Command::new("mount")
            .args(["--bind"])
            .arg(target.path())
            .arg(&mountpoint)
            .status()?;
        if !status.success() {
            return Err("bind mount failed".into());
        }
        let _unmount = Unmount(mountpoint);
        let home = temporary.path().join("home");
        fs::create_dir_all(&home)?;
        let mut cleaner = cleaner(&temporary, &home)?;

        assert!(cleaner.remove_user_roots().is_err());
        assert_eq!(fs::read(target.path().join("must-remain"))?, b"foreign");
        Ok(())
    }
}
