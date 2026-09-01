//! Crash-durable product channel rollback memory beside the TUF datastore.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use pkg_core::{ChannelSequence, PolicyVersion};
use serde::{Deserialize, Serialize};

use crate::{AcceptedChannel, ChannelError};

const STATE_FILE: &str = "accepted-channel.json";
const TEMP_FILE: &str = ".accepted-channel.json.tmp";
const INITIALIZING_FILE: &str = "accepted-channel.initializing";
pub const LOCK_FILE: &str = "pkg-channel.lock";
const MAX_STATE_BYTES: u64 = 1024;

#[derive(Debug, Clone)]
pub struct AcceptedChannelStore {
    directory: PathBuf,
}

impl AcceptedChannelStore {
    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    pub(crate) fn load(&self) -> Result<Option<AcceptedChannel>, ChannelError> {
        let path = self.directory.join(STATE_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ChannelError::AcceptedStateUnavailable),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_STATE_BYTES
        {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o177 != 0 {
                return Err(ChannelError::AcceptedStateUnavailable);
            }
        }
        let file = open_read_nofollow(&path)?;
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let wire: AcceptedChannelWire =
            serde_json::from_slice(&bytes).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        wire.promote().map(Some)
    }

    pub(crate) fn initialize(&self, legacy: Option<&AcceptedChannel>) -> Result<(), ChannelError> {
        if let Some(current) = self.load()? {
            return if legacy.is_none_or(|legacy| legacy == &current) {
                Ok(())
            } else {
                Err(ChannelError::AcceptedStateUnavailable)
            };
        }
        let marker = self.directory.join(INITIALIZING_FILE);
        match fs::symlink_metadata(&marker) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() == 0 =>
            {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if metadata.permissions().mode() & 0o177 != 0 {
                        return Err(ChannelError::AcceptedStateUnavailable);
                    }
                }
                return if legacy.is_none() {
                    Ok(())
                } else {
                    Err(ChannelError::AcceptedStateUnavailable)
                };
            }
            Ok(_) => return Err(ChannelError::AcceptedStateUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ChannelError::AcceptedStateUnavailable),
        }
        let entries =
            fs::read_dir(&self.directory).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        let mut established = false;
        for entry in entries {
            let entry = entry.map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            let name = entry.file_name();
            if name != LOCK_FILE {
                established = true;
            }
        }
        if established {
            return if let Some(legacy) = legacy {
                self.persist(legacy)
            } else {
                Err(ChannelError::AcceptedStateUnavailable)
            };
        }
        if legacy.is_some() {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let marker_file = open_create_new_nofollow(&marker)?;
        marker_file
            .sync_all()
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        sync_directory(&self.directory)
    }

    pub(crate) fn persist(&self, state: &AcceptedChannel) -> Result<(), ChannelError> {
        let wire = AcceptedChannelWire::from_state(state);
        let mut bytes =
            serde_json::to_vec(&wire).map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(ChannelError::AcceptedStateUnavailable);
        }

        let temporary = self.directory.join(TEMP_FILE);
        remove_stale_regular_temp(&temporary)?;
        let mut file = open_create_new_nofollow(&temporary)?;
        let result = (|| {
            file.write_all(&bytes)
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            file.sync_all()
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            fs::rename(&temporary, self.directory.join(STATE_FILE))
                .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
            sync_directory(&self.directory)?;
            remove_initializing_marker(&self.directory.join(INITIALIZING_FILE))?;
            sync_directory(&self.directory)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn remove_initializing_marker(path: &Path) -> Result<(), ChannelError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|_| ChannelError::AcceptedStateUnavailable)
        }
        Ok(_) => Err(ChannelError::AcceptedStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ChannelError::AcceptedStateUnavailable),
    }
}

fn sync_directory(path: &Path) -> Result<(), ChannelError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ChannelError::AcceptedStateUnavailable)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptedChannelWire {
    schema_version: u64,
    sequence: u64,
    policy_version: u64,
    descriptor_sha256: String,
}

impl AcceptedChannelWire {
    fn from_state(state: &AcceptedChannel) -> Self {
        Self {
            schema_version: 1,
            sequence: state.sequence().get().get(),
            policy_version: state.policy_version().get().get(),
            descriptor_sha256: hex::encode(state.descriptor_sha256()),
        }
    }

    fn promote(self) -> Result<AcceptedChannel, ChannelError> {
        if self.schema_version != 1
            || self.descriptor_sha256.len() != 64
            || !self
                .descriptor_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ChannelError::AcceptedStateUnavailable);
        }
        let sequence = ChannelSequence::from_u64(self.sequence)
            .ok_or(ChannelError::AcceptedStateUnavailable)?;
        let policy_version = PolicyVersion::from_u64(self.policy_version)
            .ok_or(ChannelError::AcceptedStateUnavailable)?;
        let digest = hex::decode(self.descriptor_sha256)
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        let descriptor_sha256: [u8; 32] = digest
            .try_into()
            .map_err(|_| ChannelError::AcceptedStateUnavailable)?;
        Ok(AcceptedChannel::new(
            sequence,
            policy_version,
            descriptor_sha256,
        ))
    }
}

fn remove_stale_regular_temp(path: &Path) -> Result<(), ChannelError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|_| ChannelError::AcceptedStateUnavailable)
        }
        Ok(_) => Err(ChannelError::AcceptedStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ChannelError::AcceptedStateUnavailable),
    }
}

fn open_read_nofollow(path: &Path) -> Result<File, ChannelError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| ChannelError::AcceptedStateUnavailable)
}

fn open_create_new_nofollow(path: &Path) -> Result<File, ChannelError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| ChannelError::AcceptedStateUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn state(sequence: u64, digest: u8) -> AcceptedChannel {
        AcceptedChannel::new(
            ChannelSequence::from_u64(sequence).unwrap(),
            PolicyVersion::from_u64(1).unwrap(),
            [digest; 32],
        )
    }

    #[test]
    fn exact_state_round_trips_and_replaces_atomically() {
        let temp = TempDir::new().unwrap();
        let store = AcceptedChannelStore::new(temp.path());
        assert_eq!(store.load().unwrap(), None);
        store.initialize(None).unwrap();
        assert!(temp.path().join(INITIALIZING_FILE).is_file());
        store.persist(&state(7, 0x11)).unwrap();
        assert!(!temp.path().join(INITIALIZING_FILE).exists());
        assert_eq!(store.load().unwrap(), Some(state(7, 0x11)));
        store.persist(&state(8, 0x22)).unwrap();
        assert_eq!(store.load().unwrap(), Some(state(8, 0x22)));
        assert!(!temp.path().join(TEMP_FILE).exists());
    }

    #[test]
    fn interrupted_first_refresh_marker_allows_retry_but_not_state_deletion() {
        let temp = TempDir::new().unwrap();
        let store = AcceptedChannelStore::new(temp.path());
        fs::write(temp.path().join(LOCK_FILE), b"").unwrap();
        store.initialize(None).unwrap();
        fs::write(temp.path().join("root.json"), b"tuf state").unwrap();
        store.initialize(None).unwrap();
        fs::remove_file(temp.path().join(INITIALIZING_FILE)).unwrap();
        assert!(matches!(
            store.initialize(None),
            Err(ChannelError::AcceptedStateUnavailable)
        ));
    }

    #[test]
    fn legacy_seed_is_only_accepted_for_established_markerless_state() {
        let temp = TempDir::new().unwrap();
        let store = AcceptedChannelStore::new(temp.path());
        fs::write(temp.path().join(LOCK_FILE), b"").unwrap();
        assert!(matches!(
            store.initialize(Some(&state(7, 0x11))),
            Err(ChannelError::AcceptedStateUnavailable)
        ));
        fs::write(temp.path().join("root.json"), b"legacy tuf state").unwrap();
        store.initialize(Some(&state(7, 0x11))).unwrap();
        assert_eq!(store.load().unwrap(), Some(state(7, 0x11)));
        assert!(matches!(
            store.initialize(Some(&state(8, 0x22))),
            Err(ChannelError::AcceptedStateUnavailable)
        ));
    }

    #[test]
    fn malformed_permissive_and_symlinked_state_fail_closed() {
        let temp = TempDir::new().unwrap();
        let store = AcceptedChannelStore::new(temp.path());
        let path = temp.path().join(STATE_FILE);
        fs::write(&path, b"{}\n").unwrap();
        assert!(matches!(
            store.load(),
            Err(ChannelError::AcceptedStateUnavailable)
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt as _, symlink};
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(
                store.load(),
                Err(ChannelError::AcceptedStateUnavailable)
            ));
            fs::remove_file(&path).unwrap();
            let target = temp.path().join("target");
            fs::write(&target, b"untouched").unwrap();
            symlink(&target, &path).unwrap();
            assert!(matches!(
                store.load(),
                Err(ChannelError::AcceptedStateUnavailable)
            ));
            assert_eq!(fs::read(target).unwrap(), b"untouched");
        }
    }
}
