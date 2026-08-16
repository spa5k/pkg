//! Closed `diskutil` adapter for the product-owned macOS APFS store volume.

use crate::{MacOsStoreVolumeContract, store_mount::canonical_uuid};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::Deserialize;
use std::{
    error::Error,
    fmt,
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const DISKUTIL: &str = "/usr/sbin/diskutil";
const ROOT_VOLUME: &str = "/";
const FILESYSTEM: &str = "Case-sensitive APFS";
const MAX_PLIST_BYTES: u64 = 262_144;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_ATTEMPTS: usize = 30;
const UNMOUNT_RETRY_DELAY: Duration = Duration::from_secs(1);
const SECRET_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsApfsErrorCode {
    InvalidState,
    CommandFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsApfsError {
    code: MacOsApfsErrorCode,
}

impl MacOsApfsError {
    const fn new(code: MacOsApfsErrorCode) -> Self {
        Self { code }
    }

    #[cfg(test)]
    const fn code(self) -> MacOsApfsErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsApfsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS managed APFS operation failed")
    }
}

impl Error for MacOsApfsError {}

pub trait DiskutilRunner {
    fn output(&mut self, arguments: &[&str]) -> Result<Vec<u8>, MacOsApfsError>;
    fn secret_status(&mut self, arguments: &[&str], secret: &[u8]) -> Result<(), MacOsApfsError>;
    fn status(&mut self, arguments: &[&str]) -> Result<(), MacOsApfsError>;
}

pub struct MacOsApfsAdapter<R = ProductionDiskutilRunner> {
    runner: R,
}

impl MacOsApfsAdapter<ProductionDiskutilRunner> {
    pub(crate) const fn production() -> Self {
        Self {
            runner: ProductionDiskutilRunner,
        }
    }
}

impl<R: DiskutilRunner> MacOsApfsAdapter<R> {
    /// Creates one encrypted, initially unmounted, role-free store volume.
    ///
    /// The unlock secret is accepted only as a fixed-size lowercase hexadecimal
    /// buffer and is streamed to `diskutil` stdin. It never appears in argv,
    /// output, an error, or retained adapter state.
    pub(crate) fn create_encrypted_volume(
        &mut self,
        secret: &[u8],
    ) -> Result<String, MacOsApfsError> {
        if !valid_secret(secret) {
            return Err(invalid_state());
        }
        let container = self.root_container()?;
        if self.discover_in(&container)?.is_some() {
            return Err(invalid_state());
        }
        self.runner
            .secret_status(&add_volume_arguments(&container), secret)?;
        self.discover_in(&container)?
            .map(|volume| volume.uuid)
            .ok_or_else(invalid_state)
    }

    /// Finds the sole exact encrypted product volume in the root APFS container.
    pub(crate) fn discover_volume(&mut self) -> Result<Option<String>, MacOsApfsError> {
        let container = self.root_container()?;
        Ok(self.discover_in(&container)?.map(|volume| volume.uuid))
    }

    /// Enables ownership on the exact product volume.
    pub(crate) fn enable_ownership(&mut self, volume_uuid: &str) -> Result<(), MacOsApfsError> {
        self.require_owned(volume_uuid)?;
        let observation = self.inspect(volume_uuid)?;
        if observation.mount_point.as_deref() != Some(MacOsStoreVolumeContract::MOUNT_POINT) {
            return Err(invalid_state());
        }
        self.runner.status(&enable_ownership_arguments(volume_uuid))
    }

    /// Unlocks and mounts the exact product volume at the compiled `/nix` mount point.
    pub(crate) fn mount(&mut self, volume_uuid: &str, secret: &[u8]) -> Result<(), MacOsApfsError> {
        if !valid_secret(secret) {
            return Err(invalid_state());
        }
        let volume = self.require_owned(volume_uuid)?;
        let before = self.inspect(volume_uuid)?;
        match (before.locked, before.mount_point.as_deref()) {
            (false, Some(MacOsStoreVolumeContract::MOUNT_POINT)) => return Ok(()),
            (true, None) => {}
            _ => return Err(invalid_state()),
        }
        self.runner
            .secret_status(&unlock_arguments(&volume.device_identifier), secret)?;
        let after = self.inspect(volume_uuid)?;
        if !after.locked
            && after.mount_point.as_deref() == Some(MacOsStoreVolumeContract::MOUNT_POINT)
        {
            Ok(())
        } else {
            Err(invalid_state())
        }
    }

    /// Unmounts the exact product volume during rollback.
    pub(crate) fn unmount(&mut self, volume_uuid: &str) -> Result<(), MacOsApfsError> {
        if !canonical_uuid(volume_uuid) {
            return Err(invalid_state());
        }
        for attempt in 0..UNMOUNT_ATTEMPTS {
            let container = self.root_container()?;
            match self.discover_in(&container)? {
                None => return Ok(()),
                Some(volume) if volume.uuid != volume_uuid => return Err(invalid_state()),
                Some(_) => match self.inspect(volume_uuid)?.mount_point.as_deref() {
                    None => return Ok(()),
                    Some(MacOsStoreVolumeContract::MOUNT_POINT) => {
                        match self.runner.status(&unmount_arguments(volume_uuid)) {
                            Ok(()) => return Ok(()),
                            Err(error) if attempt + 1 == UNMOUNT_ATTEMPTS => return Err(error),
                            Err(_) => thread::sleep(UNMOUNT_RETRY_DELAY),
                        }
                    }
                    Some(_) => return Err(invalid_state()),
                },
            }
        }
        Err(command_failed())
    }

    /// Deletes only the exact UUID/name/container/encryption identity.
    pub(crate) fn delete(&mut self, volume_uuid: &str) -> Result<(), MacOsApfsError> {
        if !canonical_uuid(volume_uuid) {
            return Err(invalid_state());
        }
        let container = self.root_container()?;
        match self.discover_in(&container)? {
            None => Ok(()),
            Some(volume) if volume.uuid == volume_uuid => {
                self.runner.status(&delete_arguments(volume_uuid))
            }
            Some(_) => Err(invalid_state()),
        }
    }

    /// Recovers the pre-UUID crash window by deleting one unambiguous exact volume.
    pub(crate) fn discover_and_delete(&mut self) -> Result<(), MacOsApfsError> {
        if let Some(volume_uuid) = self.discover_volume()? {
            self.delete(&volume_uuid)?;
        }
        Ok(())
    }

    fn require_owned(&mut self, volume_uuid: &str) -> Result<OwnedVolume, MacOsApfsError> {
        if !canonical_uuid(volume_uuid) {
            return Err(invalid_state());
        }
        let container = self.root_container()?;
        match self.discover_in(&container)? {
            Some(volume) if volume.uuid == volume_uuid => Ok(volume),
            Some(_) | None => Err(invalid_state()),
        }
    }

    pub(crate) fn verify_final(&mut self, volume_uuid: &str) -> Result<(), MacOsApfsError> {
        self.require_owned(volume_uuid)?;
        let observation = self.inspect(volume_uuid)?;
        if observation.volume_uuid == volume_uuid
            && observation.volume_name == MacOsStoreVolumeContract::VOLUME_NAME
            && observation.apfs_container_reference == self.root_container()?
            && observation.file_vault
            && observation.global_permissions_enabled
            && !observation.locked
            && observation.mount_point.as_deref() == Some(MacOsStoreVolumeContract::MOUNT_POINT)
        {
            Ok(())
        } else {
            Err(invalid_state())
        }
    }

    pub(crate) fn verify_for_removal(&mut self, volume_uuid: &str) -> Result<(), MacOsApfsError> {
        self.require_owned(volume_uuid)?;
        let observation = self.inspect(volume_uuid)?;
        if observation.volume_uuid == volume_uuid
            && observation.volume_name == MacOsStoreVolumeContract::VOLUME_NAME
            && observation.apfs_container_reference == self.root_container()?
            && observation.file_vault
            && observation.global_permissions_enabled
            && !observation.locked
            && matches!(
                observation.mount_point.as_deref(),
                None | Some(MacOsStoreVolumeContract::MOUNT_POINT)
            )
        {
            Ok(())
        } else {
            Err(invalid_state())
        }
    }

    fn inspect(&mut self, volume_uuid: &str) -> Result<VolumeInfo, MacOsApfsError> {
        let bytes = self.runner.output(&["info", "-plist", volume_uuid])?;
        let info: VolumeInfo = plist::from_bytes(&bytes).map_err(|_| invalid_state())?;
        Ok(info)
    }

    fn root_container(&mut self) -> Result<String, MacOsApfsError> {
        let bytes = self.runner.output(&["info", "-plist", ROOT_VOLUME])?;
        let info: RootInfo = plist::from_bytes(&bytes).map_err(|_| invalid_state())?;
        if valid_device_identifier(&info.apfs_container_reference) {
            Ok(info.apfs_container_reference)
        } else {
            Err(invalid_state())
        }
    }

    fn discover_in(&mut self, container: &str) -> Result<Option<OwnedVolume>, MacOsApfsError> {
        if !valid_device_identifier(container) {
            return Err(invalid_state());
        }
        let bytes = self.runner.output(&["apfs", "list", "-plist", container])?;
        let listing: ApfsListing = plist::from_bytes(&bytes).map_err(|_| invalid_state())?;
        let mut matching_containers = listing
            .containers
            .into_iter()
            .filter(|candidate| candidate.container_reference == container);
        let candidate = matching_containers.next().ok_or_else(invalid_state)?;
        if matching_containers.next().is_some() {
            return Err(invalid_state());
        }
        let mut named = candidate
            .volumes
            .unwrap_or_default()
            .into_iter()
            .filter(|volume| volume.name == MacOsStoreVolumeContract::VOLUME_NAME);
        let Some(volume) = named.next() else {
            return Ok(None);
        };
        if named.next().is_some()
            || !canonical_uuid(&volume.uuid)
            || !valid_volume_identifier(&volume.device_identifier)
            || !volume.encryption
            || !volume.roles.is_empty()
        {
            return Err(invalid_state());
        }
        Ok(Some(OwnedVolume {
            uuid: volume.uuid,
            device_identifier: volume.device_identifier,
        }))
    }
}

struct OwnedVolume {
    uuid: String,
    device_identifier: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RootInfo {
    #[serde(rename = "APFSContainerReference")]
    apfs_container_reference: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VolumeInfo {
    #[serde(rename = "APFSContainerReference")]
    apfs_container_reference: String,
    #[serde(rename = "VolumeUUID")]
    volume_uuid: String,
    volume_name: String,
    #[serde(default, deserialize_with = "deserialize_mount_point")]
    mount_point: Option<String>,
    file_vault: bool,
    global_permissions_enabled: bool,
    #[serde(default)]
    locked: bool,
}

fn deserialize_mount_point<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|path| !path.is_empty()))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApfsListing {
    containers: Vec<ApfsContainer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApfsContainer {
    #[serde(rename = "ContainerReference")]
    container_reference: String,
    #[serde(default)]
    volumes: Option<Vec<ApfsVolume>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApfsVolume {
    #[serde(rename = "APFSVolumeUUID")]
    uuid: String,
    device_identifier: String,
    encryption: bool,
    name: String,
    #[serde(default)]
    roles: Vec<String>,
}

fn valid_secret(secret: &[u8]) -> bool {
    secret.len() == SECRET_BYTES
        && secret
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn valid_device_identifier(value: &str) -> bool {
    value.strip_prefix("disk").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn valid_volume_identifier(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("disk") else {
        return false;
    };
    let Some((disk, slice)) = suffix.split_once('s') else {
        return false;
    };
    !disk.is_empty()
        && disk.bytes().all(|byte| byte.is_ascii_digit())
        && !slice.is_empty()
        && slice.bytes().all(|byte| byte.is_ascii_digit())
}

const fn add_volume_arguments(container: &str) -> [&str; 7] {
    [
        "apfs",
        "addVolume",
        container,
        FILESYSTEM,
        MacOsStoreVolumeContract::VOLUME_NAME,
        "-stdinpassphrase",
        "-nomount",
    ]
}

const fn enable_ownership_arguments(volume_uuid: &str) -> [&str; 2] {
    ["enableOwnership", volume_uuid]
}

const fn unlock_arguments(volume_identifier: &str) -> [&str; 6] {
    [
        "apfs",
        "unlockVolume",
        volume_identifier,
        "-stdinpassphrase",
        "-mountpoint",
        MacOsStoreVolumeContract::MOUNT_POINT,
    ]
}

const fn unmount_arguments(volume_uuid: &str) -> [&str; 2] {
    ["unmount", volume_uuid]
}

const fn delete_arguments(volume_uuid: &str) -> [&str; 3] {
    ["apfs", "deleteVolume", volume_uuid]
}

const fn invalid_state() -> MacOsApfsError {
    MacOsApfsError::new(MacOsApfsErrorCode::InvalidState)
}

pub struct ProductionDiskutilRunner;

impl DiskutilRunner for ProductionDiskutilRunner {
    fn output(&mut self, arguments: &[&str]) -> Result<Vec<u8>, MacOsApfsError> {
        let mut command = base_command();
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| command_failed())?;
        let stdout = child.stdout.take().ok_or_else(command_failed)?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_PLIST_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let status = wait_until(&mut child, Instant::now() + COMMAND_TIMEOUT)
            .map_err(|()| command_failed())?;
        let bytes = reader
            .join()
            .map_err(|_| command_failed())?
            .map_err(|_| command_failed())?;
        if !status.success() || bytes.len() as u64 > MAX_PLIST_BYTES {
            return Err(command_failed());
        }
        Ok(bytes)
    }

    fn secret_status(&mut self, arguments: &[&str], secret: &[u8]) -> Result<(), MacOsApfsError> {
        let mut command = base_command();
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| command_failed())?;
        let Some(mut stdin) = child.stdin.take() else {
            terminate(&mut child);
            return Err(command_failed());
        };
        if stdin.write_all(secret).is_err() || stdin.flush().is_err() {
            terminate(&mut child);
            return Err(command_failed());
        }
        drop(stdin);
        successful_status(&mut child)
    }

    fn status(&mut self, arguments: &[&str]) -> Result<(), MacOsApfsError> {
        let mut command = base_command();
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| command_failed())?;
        successful_status(&mut child)
    }
}

fn base_command() -> Command {
    let mut command = Command::new(DISKUTIL);
    command.env_clear().process_group(0);
    command
}

fn successful_status(child: &mut Child) -> Result<(), MacOsApfsError> {
    if wait_until(child, Instant::now() + COMMAND_TIMEOUT).is_ok_and(|status| status.success()) {
        Ok(())
    } else {
        Err(command_failed())
    }
}

fn wait_until(child: &mut Child, deadline: Instant) -> Result<ExitStatus, ()> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                terminate(child);
                return Err(());
            }
        }
    }
}

fn terminate(child: &mut Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(process_group), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

const fn command_failed() -> MacOsApfsError {
    MacOsApfsError::new(MacOsApfsErrorCode::CommandFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const UUID: &str = "01234567-89AB-CDEF-0123-456789ABCDEF";
    const SECRET: &[u8; 64] = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct FakeRunner {
        outputs: VecDeque<Vec<u8>>,
        calls: Vec<(Vec<String>, Option<Vec<u8>>)>,
        status_failures: usize,
    }

    impl DiskutilRunner for FakeRunner {
        fn output(&mut self, arguments: &[&str]) -> Result<Vec<u8>, MacOsApfsError> {
            self.calls
                .push((arguments.iter().map(ToString::to_string).collect(), None));
            self.outputs.pop_front().ok_or_else(command_failed)
        }

        fn secret_status(
            &mut self,
            arguments: &[&str],
            secret: &[u8],
        ) -> Result<(), MacOsApfsError> {
            self.calls.push((
                arguments.iter().map(ToString::to_string).collect(),
                Some(secret.to_vec()),
            ));
            if self.status_failures > 0 {
                self.status_failures -= 1;
                Err(command_failed())
            } else {
                Ok(())
            }
        }

        fn status(&mut self, arguments: &[&str]) -> Result<(), MacOsApfsError> {
            self.calls
                .push((arguments.iter().map(ToString::to_string).collect(), None));
            if self.status_failures > 0 {
                self.status_failures -= 1;
                Err(command_failed())
            } else {
                Ok(())
            }
        }
    }

    fn root_info() -> Vec<u8> {
        br#"<?xml version="1.0"?><plist version="1.0"><dict><key>APFSContainerReference</key><string>disk3</string></dict></plist>"#.to_vec()
    }

    fn listing(volumes: &str) -> Vec<u8> {
        format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>Containers</key><array><dict><key>ContainerReference</key><string>disk3</string><key>Volumes</key><array>{volumes}</array></dict></array></dict></plist>"#).into_bytes()
    }

    fn owned_volume() -> String {
        format!(
            r"<dict><key>APFSVolumeUUID</key><string>{UUID}</string><key>DeviceIdentifier</key><string>disk3s8</string><key>Encryption</key><true/><key>Name</key><string>pkg Nix Store</string><key>Roles</key><array/></dict>"
        )
    }

    fn volume_info(locked: bool, mount_point: Option<&str>) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>APFSContainerReference</key><string>disk3</string><key>VolumeUUID</key><string>{UUID}</string><key>VolumeName</key><string>pkg Nix Store</string><key>FileVault</key><true/><key>GlobalPermissionsEnabled</key><true/><key>Locked</key><{locked}/>{mount}</dict></plist>"#,
            locked = if locked { "true" } else { "false" },
            mount = mount_point.map_or(String::new(), |path| format!(
                "<key>MountPoint</key><string>{path}</string>"
            )),
        )
        .into_bytes()
    }

    #[test]
    fn creation_uses_stdin_and_discovers_uuid_from_plist() -> Result<(), MacOsApfsError> {
        let runner = FakeRunner {
            outputs: VecDeque::from([root_info(), listing(""), listing(&owned_volume())]),
            ..FakeRunner::default()
        };
        let mut adapter = MacOsApfsAdapter { runner };
        assert_eq!(adapter.create_encrypted_volume(SECRET)?, UUID);
        assert_eq!(
            adapter.runner.calls[2],
            (
                vec![
                    "apfs",
                    "addVolume",
                    "disk3",
                    "Case-sensitive APFS",
                    "pkg Nix Store",
                    "-stdinpassphrase",
                    "-nomount"
                ]
                .into_iter()
                .map(ToString::to_string)
                .collect(),
                Some(SECRET.to_vec()),
            )
        );
        assert!(
            !adapter.runner.calls[2]
                .0
                .iter()
                .any(|argument| argument.as_bytes() == SECRET)
        );
        Ok(())
    }

    #[test]
    fn discovery_rejects_ambiguous_or_wrong_identity() {
        for volumes in [
            format!("{}{}", owned_volume(), owned_volume()),
            owned_volume().replace("<true/>", "<false/>"),
            owned_volume().replace("<array/>", "<array><string>Data</string></array>"),
            owned_volume().replace(UUID, "not-a-uuid"),
        ] {
            let runner = FakeRunner {
                outputs: VecDeque::from([root_info(), listing(&volumes)]),
                ..FakeRunner::default()
            };
            let mut adapter = MacOsApfsAdapter { runner };
            assert_eq!(
                adapter.discover_volume().map_err(MacOsApfsError::code),
                Err(MacOsApfsErrorCode::InvalidState)
            );
        }
    }

    #[test]
    fn destructive_commands_require_exact_rediscovered_identity() -> Result<(), MacOsApfsError> {
        let runner = FakeRunner {
            outputs: VecDeque::from([root_info(), listing(&owned_volume())]),
            ..FakeRunner::default()
        };
        let mut adapter = MacOsApfsAdapter { runner };
        adapter.delete(UUID)?;
        assert_eq!(
            adapter.runner.calls.last().map(|call| &call.0),
            Some(&vec![
                "apfs".to_owned(),
                "deleteVolume".to_owned(),
                UUID.to_owned()
            ])
        );
        Ok(())
    }

    #[test]
    fn unmount_retries_and_revalidates_a_transient_failure() -> Result<(), MacOsApfsError> {
        let runner = FakeRunner {
            outputs: VecDeque::from([
                root_info(),
                listing(&owned_volume()),
                volume_info(false, Some("/nix")),
                root_info(),
                listing(&owned_volume()),
                volume_info(false, Some("/nix")),
            ]),
            status_failures: 1,
            ..FakeRunner::default()
        };
        let mut adapter = MacOsApfsAdapter { runner };
        adapter.unmount(UUID)?;
        assert_eq!(
            adapter
                .runner
                .calls
                .iter()
                .filter(|call| call.0 == unmount_arguments(UUID))
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn mount_unlocks_with_stdin_secret_and_verifies_the_result() -> Result<(), MacOsApfsError> {
        let runner = FakeRunner {
            outputs: VecDeque::from([
                root_info(),
                listing(&owned_volume()),
                volume_info(true, Some("")),
                volume_info(false, Some("/nix")),
            ]),
            ..FakeRunner::default()
        };
        let mut adapter = MacOsApfsAdapter { runner };
        adapter.mount(UUID, SECRET)?;
        assert_eq!(
            adapter.runner.calls[3],
            (
                vec![
                    "apfs",
                    "unlockVolume",
                    "disk3s8",
                    "-stdinpassphrase",
                    "-mountpoint",
                    "/nix",
                ]
                .into_iter()
                .map(ToString::to_string)
                .collect(),
                Some(SECRET.to_vec()),
            )
        );
        assert!(
            !adapter.runner.calls[3]
                .0
                .iter()
                .any(|argument| argument.as_bytes() == SECRET)
        );
        Ok(())
    }

    #[test]
    fn invalid_secret_is_rejected_before_any_command() {
        for secret in [b"short".as_slice(), &[b'g'; 64], &[b'0'; 65]] {
            let mut adapter = MacOsApfsAdapter {
                runner: FakeRunner::default(),
            };
            assert_eq!(
                adapter
                    .create_encrypted_volume(secret)
                    .map_err(MacOsApfsError::code),
                Err(MacOsApfsErrorCode::InvalidState)
            );
            assert!(adapter.runner.calls.is_empty());
        }
    }

    #[test]
    fn command_contract_is_absolute_fixed_and_secret_free() {
        assert_eq!(DISKUTIL, "/usr/sbin/diskutil");
        assert_eq!(
            add_volume_arguments("disk3"),
            [
                "apfs",
                "addVolume",
                "disk3",
                "Case-sensitive APFS",
                "pkg Nix Store",
                "-stdinpassphrase",
                "-nomount"
            ]
        );
        assert_eq!(enable_ownership_arguments(UUID), ["enableOwnership", UUID]);
        assert_eq!(
            unlock_arguments("disk3s8"),
            [
                "apfs",
                "unlockVolume",
                "disk3s8",
                "-stdinpassphrase",
                "-mountpoint",
                "/nix"
            ]
        );
        assert_eq!(unmount_arguments(UUID), ["unmount", UUID]);
        assert_eq!(delete_arguments(UUID), ["apfs", "deleteVolume", UUID]);
    }
}
