//! One bounded Linux vendor leaf, recorded only at successful installation.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinuxResidueIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
    sha256: [u8; 32],
}

#[cfg(any(target_os = "linux", test))]
pub(super) use filesystem::{capture, remove, verify};

#[cfg(any(target_os = "linux", test))]
mod filesystem {
    use super::LinuxResidueIdentity;
    use crate::determinate_handoff::DeterminateHandoffError;
    use rustix::fs::{AtFlags, Mode, OFlags, open, openat, unlinkat};
    use sha2::{Digest as _, Sha256};
    use std::{
        fs::{File, Metadata},
        io::Read,
        os::unix::fs::MetadataExt,
        path::Path,
    };

    const LEAF: &str = "sentry-endpoint";
    const MAX_BYTES: u64 = 4096;
    const DIRECTORY: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const REGULAR: OFlags = OFlags::RDONLY
        .union(OFlags::NONBLOCK)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    pub(in crate::determinate_handoff) fn capture(
        root: &Path,
        owner: u32,
        group: u32,
    ) -> Result<Option<LinuxResidueIdentity>, DeterminateHandoffError> {
        let Some(parent) = open_parent(root, owner)? else {
            return Ok(None);
        };
        observe(&parent, owner, group)
    }

    pub(in crate::determinate_handoff) fn verify(
        root: &Path,
        owner: u32,
        group: u32,
        expected: LinuxResidueIdentity,
    ) -> Result<(), DeterminateHandoffError> {
        let Some(parent) = open_parent(root, owner)? else {
            return Ok(());
        };
        verify_at(&parent, owner, group, expected).map(|_| ())
    }

    pub(in crate::determinate_handoff) fn remove(
        root: &Path,
        owner: u32,
        group: u32,
        expected: LinuxResidueIdentity,
    ) -> Result<(), DeterminateHandoffError> {
        let Some(parent) = open_parent(root, owner)? else {
            return Ok(());
        };
        if verify_at(&parent, owner, group, expected)? {
            // Both validation and unlink use the same protected directory fd.
            // A hostile root is outside the handoff's immutability boundary.
            unlinkat(&parent, LEAF, AtFlags::empty()).map_err(|_| invalid())?;
            parent
                .sync_all()
                .map_err(|_| DeterminateHandoffError::PersistenceFailed)?;
        }
        Ok(())
    }

    fn verify_at(
        parent: &File,
        owner: u32,
        group: u32,
        expected: LinuxResidueIdentity,
    ) -> Result<bool, DeterminateHandoffError> {
        match observe(parent, owner, group)? {
            // An earlier consume can remove the leaf and restore Accepted if
            // exec fails. Absence grants no authority over any later file.
            None => Ok(false),
            Some(observed) if observed == expected => Ok(true),
            Some(_) => Err(invalid()),
        }
    }

    fn open_parent(root: &Path, owner: u32) -> Result<Option<File>, DeterminateHandoffError> {
        if !root.is_absolute() {
            return Err(invalid());
        }
        let mut parent = File::from(open(root, DIRECTORY, Mode::empty()).map_err(|_| invalid())?);
        validate_directory(&parent, owner)?;
        for component in ["etc", "nix"] {
            parent = match openat(&parent, component, DIRECTORY, Mode::empty()) {
                Ok(directory) => File::from(directory),
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(_) => return Err(invalid()),
            };
            validate_directory(&parent, owner)?;
        }
        Ok(Some(parent))
    }

    fn validate_directory(directory: &File, owner: u32) -> Result<(), DeterminateHandoffError> {
        let metadata = directory.metadata().map_err(|_| invalid())?;
        if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err(invalid());
        }
        Ok(())
    }

    fn observe(
        parent: &File,
        owner: u32,
        group: u32,
    ) -> Result<Option<LinuxResidueIdentity>, DeterminateHandoffError> {
        let mut file = match openat(parent, LEAF, REGULAR, Mode::empty()) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(invalid()),
        };
        let metadata = file.metadata().map_err(|_| invalid())?;
        validate_leaf(&metadata, owner, group)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() as u64 != metadata.len() {
            return Err(invalid());
        }
        let identity = identity(&metadata, Sha256::digest(&bytes).into());
        // Reopen through the same parent fd to reject path replacement and
        // metadata changes while the bounded content was read.
        let current =
            File::from(openat(parent, LEAF, REGULAR, Mode::empty()).map_err(|_| invalid())?);
        let current_metadata = current.metadata().map_err(|_| invalid())?;
        validate_leaf(&current_metadata, owner, group)?;
        if identity != self::identity(&current_metadata, identity.sha256) {
            return Err(invalid());
        }
        Ok(Some(identity))
    }

    fn validate_leaf(
        metadata: &Metadata,
        owner: u32,
        group: u32,
    ) -> Result<(), DeterminateHandoffError> {
        if !metadata.is_file()
            || metadata.uid() != owner
            || metadata.gid() != group
            || !matches!(metadata.mode() & 0o7777, 0o600 | 0o644)
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > MAX_BYTES
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn identity(metadata: &Metadata, sha256: [u8; 32]) -> LinuxResidueIdentity {
        LinuxResidueIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            length: metadata.len(),
            sha256,
        }
    }

    const fn invalid() -> DeterminateHandoffError {
        DeterminateHandoffError::IdentityMismatch
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests may unwrap")]
mod tests {
    use super::*;
    use std::{
        fs::{self, Permissions},
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::Path,
    };

    fn fixture(mode: u32) -> (tempfile::TempDir, u32, u32) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/nix")).unwrap();
        for path in [
            root.path().to_path_buf(),
            root.path().join("etc"),
            root.path().join("etc/nix"),
        ] {
            fs::set_permissions(path, Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.path().join("etc/nix/sentry-endpoint");
        fs::write(&path, b"https://vendor.invalid/telemetry").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        (root, metadata.uid(), metadata.gid())
    }

    #[test]
    fn exact_owned_leaf_removal_preserves_directory_and_other_files_and_allows_retry() {
        for mode in [0o600, 0o644] {
            let (root, owner, group) = fixture(mode);
            let path = root.path().join("etc/nix/sentry-endpoint");
            let other = root.path().join("etc/nix/operator.conf");
            fs::write(&other, b"operator-owned configuration").unwrap();
            let identity = capture(root.path(), owner, group).unwrap().unwrap();
            verify(root.path(), owner, group, identity).unwrap();
            remove(root.path(), owner, group, identity).unwrap();
            assert!(!path.exists());
            assert!(path.parent().unwrap().is_dir());
            assert_eq!(fs::read(other).unwrap(), b"operator-owned configuration");
            verify(root.path(), owner, group, identity).unwrap();
            remove(root.path(), owner, group, identity).unwrap();
            // A later replacement never inherits the removed leaf's authority.
            fs::write(&path, b"replacement").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            assert!(remove(root.path(), owner, group, identity).is_err());
            assert_eq!(fs::read(path).unwrap(), b"replacement");
        }
    }

    #[test]
    fn changed_file_and_unsafe_parent_are_refused_without_removal() {
        type Tamper = (&'static str, fn(&Path));
        let cases: [Tamper; 6] = [
            ("same-length content", |root| {
                let leaf = root.join("etc/nix/sentry-endpoint");
                let mut bytes = fs::read(&leaf).unwrap();
                bytes[0] ^= 1;
                fs::write(leaf, bytes).unwrap();
            }),
            ("mode", |root| {
                fs::set_permissions(
                    root.join("etc/nix/sentry-endpoint"),
                    Permissions::from_mode(0o644),
                )
                .unwrap();
            }),
            ("replacement inode", |root| {
                let leaf = root.join("etc/nix/sentry-endpoint");
                fs::rename(&leaf, root.join("original")).unwrap();
                fs::copy(root.join("original"), &leaf).unwrap();
            }),
            ("hard link", |root| {
                fs::hard_link(root.join("etc/nix/sentry-endpoint"), root.join("alias")).unwrap();
            }),
            ("leaf symlink", |root| {
                let leaf = root.join("etc/nix/sentry-endpoint");
                fs::rename(&leaf, root.join("original")).unwrap();
                symlink(root.join("original"), leaf).unwrap();
            }),
            ("writable parent", |root| {
                fs::set_permissions(root.join("etc/nix"), Permissions::from_mode(0o777)).unwrap();
            }),
        ];
        for (label, tamper) in cases {
            let (root, owner, group) = fixture(0o600);
            let identity = capture(root.path(), owner, group).unwrap().unwrap();
            tamper(root.path());
            let path = root.path().join("etc/nix/sentry-endpoint");
            let contents = fs::read(&path).unwrap();
            assert!(
                verify(root.path(), owner, group, identity).is_err(),
                "{label}"
            );
            assert!(
                remove(root.path(), owner, group, identity).is_err(),
                "{label}"
            );
            assert_eq!(fs::read(path).unwrap(), contents, "{label}");
        }
    }

    #[test]
    fn capture_is_bounded_and_never_follows_a_directory_symlink() {
        let (root, owner, group) = fixture(0o600);
        let leaf = root.path().join("etc/nix/sentry-endpoint");
        fs::write(&leaf, vec![b'x'; 4097]).unwrap();
        assert!(capture(root.path(), owner, group).is_err());
        fs::write(&leaf, b"endpoint").unwrap();
        fs::rename(root.path().join("etc/nix"), root.path().join("saved-nix")).unwrap();
        symlink(root.path().join("saved-nix"), root.path().join("etc/nix")).unwrap();
        assert!(capture(root.path(), owner, group).is_err());
        assert_eq!(fs::read(leaf).unwrap(), b"endpoint");
    }
}
