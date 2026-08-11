//! Closed macOS boot-time mount path for the receipt-owned encrypted store.

#[cfg(any(target_os = "macos", test))]
use crate::MacOsStoreVolumeContract;
#[cfg(any(target_os = "macos", test))]
use serde::Deserialize;
use std::{error::Error, fmt};

#[cfg(any(target_os = "macos", test))]
const RECEIPT_SCHEMA_VERSION: u32 = 1;
#[cfg(any(target_os = "macos", test))]
const RECEIPT_PRODUCT: &str = "pkg";
#[cfg(any(target_os = "macos", test))]
const KEYCHAIN_SERVICE: &str = "org.pkg.store-volume";
#[cfg(any(target_os = "macos", test))]
const KEYCHAIN_ACCOUNT: &str = "pkg Nix Store";
#[cfg(target_os = "macos")]
const RECEIPT_PATH: &str = "/Library/Application Support/pkg/managed-nix/store-volume-v1.json";

/// Stable failures for the root-only macOS store mount operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreMountErrorCode {
    /// This operation is unavailable away from macOS.
    InvalidRuntime,
    /// The process does not have the exact launchd root identity.
    InvalidIdentity,
    /// The root-owned dynamic volume receipt is missing, unsafe, or malformed.
    InvalidReceipt,
    /// The receipt-owned APFS volume could not be inspected.
    InspectFailed,
    /// The fixed System-keychain item could not be read.
    KeychainFailed,
    /// The fixed mount or unlock operation failed or timed out.
    MountFailed,
    /// The final mounted-volume properties did not match the receipt contract.
    VerificationFailed,
}

/// Redacted macOS store-mount failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacOsStoreMountError {
    code: MacOsStoreMountErrorCode,
}

impl MacOsStoreMountError {
    const fn new(code: MacOsStoreMountErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> MacOsStoreMountErrorCode {
        self.code
    }
}

impl fmt::Display for MacOsStoreMountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("macOS managed store mount failed")
    }
}

impl Error for MacOsStoreMountError {}

/// Successful boot-time mount disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacOsStoreMountOutcome {
    /// The exact receipt-owned volume was already mounted correctly.
    AlreadyMounted,
    /// The exact receipt-owned volume was mounted and verified.
    Mounted,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoreVolumeReceipt {
    schema_version: u32,
    product: String,
    volume_uuid: String,
    volume_name: String,
    mount_point: String,
    keychain_service: String,
    keychain_account: String,
}

#[cfg(any(target_os = "macos", test))]
impl StoreVolumeReceipt {
    fn decode(bytes: &[u8]) -> Result<Self, MacOsStoreMountError> {
        let receipt: Self = serde_json::from_slice(bytes)
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InvalidReceipt))?;
        if receipt.schema_version != RECEIPT_SCHEMA_VERSION
            || receipt.product != RECEIPT_PRODUCT
            || !canonical_uuid(&receipt.volume_uuid)
            || receipt.volume_name != MacOsStoreVolumeContract::VOLUME_NAME
            || receipt.mount_point != MacOsStoreVolumeContract::MOUNT_POINT
            || receipt.keychain_service != KEYCHAIN_SERVICE
            || receipt.keychain_account != KEYCHAIN_ACCOUNT
        {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::InvalidReceipt,
            ));
        }
        Ok(receipt)
    }
}

#[cfg(any(target_os = "macos", test))]
fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
            }
        })
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreVolumeObservation {
    volume_uuid: String,
    volume_name: String,
    mount_point: Option<String>,
    encrypted: bool,
    ownership_enabled: bool,
    locked: bool,
}

#[cfg(any(target_os = "macos", test))]
trait StoreMountBackend {
    fn inspect(
        &mut self,
        volume_uuid: &str,
    ) -> Result<StoreVolumeObservation, MacOsStoreMountError>;
    fn mount_unlocked(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreMountError>;
    fn unlock_from_keychain(
        &mut self,
        receipt: &StoreVolumeReceipt,
    ) -> Result<(), MacOsStoreMountError>;
}

#[cfg(any(target_os = "macos", test))]
fn mount_with_backend(
    backend: &mut impl StoreMountBackend,
    receipt: &StoreVolumeReceipt,
) -> Result<MacOsStoreMountOutcome, MacOsStoreMountError> {
    let before = backend.inspect(&receipt.volume_uuid)?;
    validate_static_observation(&before, receipt)?;
    match before.mount_point.as_deref() {
        Some(MacOsStoreVolumeContract::MOUNT_POINT) if !before.locked => {
            return Ok(MacOsStoreMountOutcome::AlreadyMounted);
        }
        Some(_) => {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::VerificationFailed,
            ));
        }
        None => {}
    }
    if before.locked {
        backend.unlock_from_keychain(receipt)?;
    } else {
        backend.mount_unlocked(&receipt.volume_uuid)?;
    }
    let after = backend.inspect(&receipt.volume_uuid)?;
    validate_static_observation(&after, receipt)?;
    if after.locked || after.mount_point.as_deref() != Some(MacOsStoreVolumeContract::MOUNT_POINT) {
        return Err(MacOsStoreMountError::new(
            MacOsStoreMountErrorCode::VerificationFailed,
        ));
    }
    Ok(MacOsStoreMountOutcome::Mounted)
}

#[cfg(any(target_os = "macos", test))]
fn validate_static_observation(
    observation: &StoreVolumeObservation,
    receipt: &StoreVolumeReceipt,
) -> Result<(), MacOsStoreMountError> {
    if observation.volume_uuid != receipt.volume_uuid
        || observation.volume_name != receipt.volume_name
        || !observation.encrypted
        || !observation.ownership_enabled
    {
        return Err(MacOsStoreMountError::new(
            MacOsStoreMountErrorCode::VerificationFailed,
        ));
    }
    Ok(())
}

/// Mounts the exact receipt-owned encrypted APFS volume at `/nix`.
///
/// This entry point accepts no dynamic input. It requires the launchd root
/// identity, loads only the compiled root-owned receipt, never places the
/// keychain secret in argv or Rust memory, and verifies the final volume state.
///
/// # Errors
///
/// Returns a redacted, stable error when identity, receipt, keychain, APFS, or
/// final verification fails closed.
#[cfg(target_os = "macos")]
pub fn run_macos_store_mount() -> Result<MacOsStoreMountOutcome, MacOsStoreMountError> {
    use nix::unistd::{Gid, Uid};

    if Uid::effective().as_raw() != 0 || Gid::effective().as_raw() != 0 {
        return Err(MacOsStoreMountError::new(
            MacOsStoreMountErrorCode::InvalidIdentity,
        ));
    }
    let receipt = production::load_receipt()?;
    mount_with_backend(&mut production::ProductionStoreMountBackend, &receipt)
}

/// Fails closed away from macOS.
///
/// # Errors
///
/// Always returns `InvalidRuntime` outside macOS.
#[cfg(not(target_os = "macos"))]
pub const fn run_macos_store_mount() -> Result<MacOsStoreMountOutcome, MacOsStoreMountError> {
    Err(MacOsStoreMountError::new(
        MacOsStoreMountErrorCode::InvalidRuntime,
    ))
}

#[cfg(target_os = "macos")]
mod production {
    use super::{
        MacOsStoreMountError, MacOsStoreMountErrorCode, MacOsStoreVolumeContract, RECEIPT_PATH,
        StoreMountBackend, StoreVolumeObservation, StoreVolumeReceipt,
    };
    use exacl::getfacl;
    use nix::{
        fcntl::{OFlag, open},
        sys::{
            signal::{Signal, killpg},
            stat::Mode,
        },
        unistd::Pid,
    };
    use serde::Deserialize;
    use std::{
        fs::{self, File},
        io::Read,
        os::unix::{fs::MetadataExt, process::CommandExt},
        path::Path,
        process::{Child, Command, ExitStatus, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const RECEIPT_PARENT: &str = "/Library/Application Support/pkg/managed-nix";
    const PRODUCT_PARENT: &str = "/Library/Application Support/pkg";
    const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";
    const DISKUTIL: &str = "/usr/sbin/diskutil";
    const SECURITY: &str = "/usr/bin/security";
    const MAX_RECEIPT_BYTES: u64 = 4096;
    const MAX_PLIST_BYTES: u64 = 262_144;
    const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

    pub(super) struct ProductionStoreMountBackend;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct DiskInfo {
        #[serde(rename = "VolumeUUID")]
        volume_uuid: String,
        volume_name: String,
        #[serde(default)]
        mount_point: Option<String>,
        file_vault: bool,
        global_permissions_enabled: bool,
        #[serde(default)]
        locked: bool,
    }

    impl StoreMountBackend for ProductionStoreMountBackend {
        fn inspect(
            &mut self,
            volume_uuid: &str,
        ) -> Result<StoreVolumeObservation, MacOsStoreMountError> {
            let output = bounded_output(DISKUTIL, &info_arguments(volume_uuid))?;
            let info: DiskInfo = plist::from_bytes(&output)
                .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?;
            Ok(StoreVolumeObservation {
                volume_uuid: info.volume_uuid,
                volume_name: info.volume_name,
                mount_point: info.mount_point,
                encrypted: info.file_vault,
                ownership_enabled: info.global_permissions_enabled,
                locked: info.locked,
            })
        }

        fn mount_unlocked(&mut self, volume_uuid: &str) -> Result<(), MacOsStoreMountError> {
            exact_status(
                DISKUTIL,
                &mount_arguments(volume_uuid),
                MacOsStoreMountErrorCode::MountFailed,
            )
        }

        fn unlock_from_keychain(
            &mut self,
            receipt: &StoreVolumeReceipt,
        ) -> Result<(), MacOsStoreMountError> {
            let mut security = base_command(SECURITY);
            security.args(keychain_arguments(receipt));
            security
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut security_child = security
                .spawn()
                .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::KeychainFailed))?;
            let secret_pipe = security_child.stdout.take().ok_or_else(|| {
                MacOsStoreMountError::new(MacOsStoreMountErrorCode::KeychainFailed)
            })?;

            let mut diskutil = base_command(DISKUTIL);
            diskutil.args(unlock_arguments(receipt));
            diskutil
                .stdin(Stdio::from(secret_pipe))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let Ok(mut diskutil_child) = diskutil.spawn() else {
                terminate(&mut security_child);
                return Err(MacOsStoreMountError::new(
                    MacOsStoreMountErrorCode::MountFailed,
                ));
            };
            let deadline = Instant::now() + COMMAND_TIMEOUT;
            let disk_status = wait_until(&mut diskutil_child, deadline);
            let security_status = wait_until(&mut security_child, deadline);
            if !security_status.is_ok_and(|status| status.success()) {
                return Err(MacOsStoreMountError::new(
                    MacOsStoreMountErrorCode::KeychainFailed,
                ));
            }
            if !disk_status.is_ok_and(|status| status.success()) {
                return Err(MacOsStoreMountError::new(
                    MacOsStoreMountErrorCode::MountFailed,
                ));
            }
            Ok(())
        }
    }

    pub(super) fn load_receipt() -> Result<StoreVolumeReceipt, MacOsStoreMountError> {
        validate_directory(Path::new(PRODUCT_PARENT), 0o711, false)?;
        validate_directory(Path::new(RECEIPT_PARENT), 0o700, true)?;
        let fd = open(
            Path::new(RECEIPT_PATH),
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InvalidReceipt))?;
        let mut file = File::from(fd);
        let metadata = file
            .metadata()
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InvalidReceipt))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o600
            || metadata.len() > MAX_RECEIPT_BYTES
            || !getfacl(RECEIPT_PATH, None).is_ok_and(|acl| acl.is_empty())
        {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::InvalidReceipt,
            ));
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_RECEIPT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InvalidReceipt))?;
        if bytes.len() as u64 > MAX_RECEIPT_BYTES {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::InvalidReceipt,
            ));
        }
        StoreVolumeReceipt::decode(&bytes)
    }

    fn validate_directory(
        path: &Path,
        mode: u32,
        wheel_group: bool,
    ) -> Result<(), MacOsStoreMountError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InvalidReceipt))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || (wheel_group && metadata.gid() != 0)
            || metadata.mode() & 0o7777 != mode
            || !getfacl(path, None).is_ok_and(|acl| acl.is_empty())
        {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::InvalidReceipt,
            ));
        }
        Ok(())
    }

    fn base_command(program: &str) -> Command {
        let mut command = Command::new(program);
        command.env_clear().process_group(0);
        command
    }

    const fn info_arguments(volume_uuid: &str) -> [&str; 3] {
        ["info", "-plist", volume_uuid]
    }

    const fn mount_arguments(volume_uuid: &str) -> [&str; 4] {
        [
            "mount",
            "-mountPoint",
            MacOsStoreVolumeContract::MOUNT_POINT,
            volume_uuid,
        ]
    }

    const fn keychain_arguments(receipt: &StoreVolumeReceipt) -> [&str; 7] {
        [
            "find-generic-password",
            "-a",
            receipt.keychain_account.as_str(),
            "-s",
            receipt.keychain_service.as_str(),
            "-w",
            SYSTEM_KEYCHAIN,
        ]
    }

    const fn unlock_arguments(receipt: &StoreVolumeReceipt) -> [&str; 6] {
        [
            "apfs",
            "unlockVolume",
            receipt.volume_uuid.as_str(),
            "-mountpoint",
            MacOsStoreVolumeContract::MOUNT_POINT,
            "-stdinpassphrase",
        ]
    }

    fn bounded_output(program: &str, arguments: &[&str]) -> Result<Vec<u8>, MacOsStoreMountError> {
        let mut command = base_command(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_PLIST_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let status = wait_until(&mut child, Instant::now() + COMMAND_TIMEOUT)
            .map_err(|()| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?;
        let bytes = reader
            .join()
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?
            .map_err(|_| MacOsStoreMountError::new(MacOsStoreMountErrorCode::InspectFailed))?;
        if !status.success() || bytes.len() as u64 > MAX_PLIST_BYTES {
            return Err(MacOsStoreMountError::new(
                MacOsStoreMountErrorCode::InspectFailed,
            ));
        }
        Ok(bytes)
    }

    fn exact_status(
        program: &str,
        arguments: &[&str],
        code: MacOsStoreMountErrorCode,
    ) -> Result<(), MacOsStoreMountError> {
        let mut command = base_command(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| MacOsStoreMountError::new(code))?;
        if wait_until(&mut child, Instant::now() + COMMAND_TIMEOUT)
            .is_ok_and(|status| status.success())
        {
            Ok(())
        } else {
            Err(MacOsStoreMountError::new(code))
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

    #[cfg(test)]
    mod tests {
        use super::{
            DISKUTIL, DiskInfo, SECURITY, info_arguments, keychain_arguments, mount_arguments,
            unlock_arguments,
        };
        use crate::store_mount::{
            KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE, MacOsStoreVolumeContract, RECEIPT_PRODUCT,
            RECEIPT_SCHEMA_VERSION, StoreVolumeReceipt,
        };

        #[test]
        fn diskutil_plist_uses_the_exact_uuid_key() {
            let info: DiskInfo = plist::from_bytes(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>VolumeUUID</key><string>01234567-89AB-CDEF-0123-456789ABCDEF</string>
<key>VolumeName</key><string>pkg Nix Store</string>
<key>MountPoint</key><string>/nix</string>
<key>FileVault</key><true/>
<key>GlobalPermissionsEnabled</key><true/>
<key>Locked</key><false/>
</dict></plist>"#,
            )
            .expect("diskutil plist should decode");
            assert_eq!(info.volume_uuid, "01234567-89AB-CDEF-0123-456789ABCDEF");
        }

        #[test]
        fn production_commands_are_absolute_fixed_and_secret_free() {
            let receipt = StoreVolumeReceipt {
                schema_version: RECEIPT_SCHEMA_VERSION,
                product: RECEIPT_PRODUCT.to_owned(),
                volume_uuid: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
                volume_name: MacOsStoreVolumeContract::VOLUME_NAME.to_owned(),
                mount_point: MacOsStoreVolumeContract::MOUNT_POINT.to_owned(),
                keychain_service: KEYCHAIN_SERVICE.to_owned(),
                keychain_account: KEYCHAIN_ACCOUNT.to_owned(),
            };
            assert_eq!(DISKUTIL, "/usr/sbin/diskutil");
            assert_eq!(SECURITY, "/usr/bin/security");
            assert_eq!(
                info_arguments(&receipt.volume_uuid),
                ["info", "-plist", receipt.volume_uuid.as_str()]
            );
            assert_eq!(
                mount_arguments(&receipt.volume_uuid),
                ["mount", "-mountPoint", "/nix", receipt.volume_uuid.as_str(),]
            );
            assert_eq!(
                keychain_arguments(&receipt),
                [
                    "find-generic-password",
                    "-a",
                    "pkg Nix Store",
                    "-s",
                    "org.pkg.store-volume",
                    "-w",
                    "/Library/Keychains/System.keychain",
                ]
            );
            assert_eq!(
                unlock_arguments(&receipt),
                [
                    "apfs",
                    "unlockVolume",
                    receipt.volume_uuid.as_str(),
                    "-mountpoint",
                    "/nix",
                    "-stdinpassphrase",
                ]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "01234567-89AB-CDEF-0123-456789ABCDEF";

    fn receipt() -> StoreVolumeReceipt {
        StoreVolumeReceipt {
            schema_version: RECEIPT_SCHEMA_VERSION,
            product: RECEIPT_PRODUCT.to_owned(),
            volume_uuid: UUID.to_owned(),
            volume_name: MacOsStoreVolumeContract::VOLUME_NAME.to_owned(),
            mount_point: MacOsStoreVolumeContract::MOUNT_POINT.to_owned(),
            keychain_service: KEYCHAIN_SERVICE.to_owned(),
            keychain_account: KEYCHAIN_ACCOUNT.to_owned(),
        }
    }

    fn observation(mount_point: Option<&str>, locked: bool) -> StoreVolumeObservation {
        StoreVolumeObservation {
            volume_uuid: UUID.to_owned(),
            volume_name: MacOsStoreVolumeContract::VOLUME_NAME.to_owned(),
            mount_point: mount_point.map(str::to_owned),
            encrypted: true,
            ownership_enabled: true,
            locked,
        }
    }

    struct FakeBackend {
        observations: Vec<StoreVolumeObservation>,
        mounts: usize,
        unlocks: usize,
    }

    impl StoreMountBackend for FakeBackend {
        fn inspect(
            &mut self,
            _volume_uuid: &str,
        ) -> Result<StoreVolumeObservation, MacOsStoreMountError> {
            if self.observations.is_empty() {
                return Err(MacOsStoreMountError::new(
                    MacOsStoreMountErrorCode::InspectFailed,
                ));
            }
            Ok(self.observations.remove(0))
        }

        fn mount_unlocked(&mut self, _volume_uuid: &str) -> Result<(), MacOsStoreMountError> {
            self.mounts += 1;
            Ok(())
        }

        fn unlock_from_keychain(
            &mut self,
            _receipt: &StoreVolumeReceipt,
        ) -> Result<(), MacOsStoreMountError> {
            self.unlocks += 1;
            Ok(())
        }
    }

    #[test]
    fn receipt_schema_is_closed_and_canonical() {
        assert!(canonical_uuid(UUID));
        let valid = format!(
            r#"{{"schemaVersion":1,"product":"pkg","volumeUuid":"{UUID}","volumeName":"pkg Nix Store","mountPoint":"/nix","keychainService":"org.pkg.store-volume","keychainAccount":"pkg Nix Store"}}"#
        );
        assert!(StoreVolumeReceipt::decode(valid.as_bytes()).is_ok());
        for invalid in [
            "01234567-89ab-cdef-0123-456789abcdef",
            "0123456789AB-CDEF-0123-456789ABCDEF",
            "01234567-89AB-CDEF-0123-456789ABCDEG",
        ] {
            assert!(!canonical_uuid(invalid));
        }
        let extended = format!(
            r#"{{"schemaVersion":1,"product":"pkg","volumeUuid":"{UUID}","volumeName":"pkg Nix Store","mountPoint":"/nix","keychainService":"org.pkg.store-volume","keychainAccount":"pkg Nix Store","secret":"no"}}"#
        );
        assert_eq!(
            StoreVolumeReceipt::decode(extended.as_bytes())
                .err()
                .map(MacOsStoreMountError::code),
            Some(MacOsStoreMountErrorCode::InvalidReceipt)
        );
    }

    #[test]
    fn mount_is_idempotent_and_uses_keychain_only_for_locked_volume() {
        let mut already = FakeBackend {
            observations: vec![observation(Some("/nix"), false)],
            mounts: 0,
            unlocks: 0,
        };
        assert_eq!(
            mount_with_backend(&mut already, &receipt()),
            Ok(MacOsStoreMountOutcome::AlreadyMounted)
        );
        assert_eq!((already.mounts, already.unlocks), (0, 0));

        let mut locked = FakeBackend {
            observations: vec![observation(None, true), observation(Some("/nix"), false)],
            mounts: 0,
            unlocks: 0,
        };
        assert_eq!(
            mount_with_backend(&mut locked, &receipt()),
            Ok(MacOsStoreMountOutcome::Mounted)
        );
        assert_eq!((locked.mounts, locked.unlocks), (0, 1));

        let mut unlocked = FakeBackend {
            observations: vec![observation(None, false), observation(Some("/nix"), false)],
            mounts: 0,
            unlocks: 0,
        };
        assert_eq!(
            mount_with_backend(&mut unlocked, &receipt()),
            Ok(MacOsStoreMountOutcome::Mounted)
        );
        assert_eq!((unlocked.mounts, unlocked.unlocks), (1, 0));
    }

    #[test]
    fn wrong_mount_or_volume_properties_fail_before_mutation() {
        for bad in [
            observation(Some("/tmp/nix"), false),
            StoreVolumeObservation {
                encrypted: false,
                ..observation(None, true)
            },
            StoreVolumeObservation {
                ownership_enabled: false,
                ..observation(None, true)
            },
        ] {
            let mut backend = FakeBackend {
                observations: vec![bad],
                mounts: 0,
                unlocks: 0,
            };
            assert_eq!(
                mount_with_backend(&mut backend, &receipt())
                    .err()
                    .map(MacOsStoreMountError::code),
                Some(MacOsStoreMountErrorCode::VerificationFailed)
            );
            assert_eq!((backend.mounts, backend.unlocks), (0, 0));
        }
    }
}
