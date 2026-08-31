//! Narrow safe boundary around legacy macOS System Keychain ACL APIs.
//!
//! The product installer needs a generic-password item that can be read
//! unattended by only the protected root helper. The safe `security-framework`
//! crate does not currently expose item ACL assignment, so this crate contains
//! the minimal audited FFI required to set that closed access policy.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use core_foundation::{
        array::CFArray,
        base::{CFRelease, CFType, CFTypeRef, TCFType},
        string::CFString,
    };
    use security_framework::{
        os::macos::{keychain::SecKeychain, keychain_item::SecKeychainItem},
        random::SecRandom,
    };
    use security_framework_sys::{
        base::{
            SecAccessRef, SecKeychainAttribute, SecKeychainAttributeList, SecKeychainItemRef,
            SecKeychainRef,
        },
        keychain::SecKeychainFindGenericPassword,
        keychain_item::{SecKeychainItemDelete, SecKeychainItemFreeContent},
    };
    use std::{error::Error, ffi::c_char, fmt, os::raw::c_void, ptr, slice};
    use zeroize::Zeroize;

    const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";
    const ROOT_HELPER: &[u8] = b"/opt/pkg/bin/pkg-root-helper\0";
    const SERVICE: &str = "org.pkg.store-volume";
    const ACCOUNT: &str = "pkg Nix Store";
    const DESCRIPTION: &str = "pkg encrypted Nix store";
    const GENERIC_PASSWORD_ITEM_CLASS: u32 = u32::from_be_bytes(*b"genp");
    const ACCOUNT_ITEM_ATTRIBUTE: u32 = u32::from_be_bytes(*b"acct");
    const SERVICE_ITEM_ATTRIBUTE: u32 = u32::from_be_bytes(*b"svce");
    const RANDOM_BYTES: usize = 32;
    const HEX_BYTES: usize = RANDOM_BYTES * 2;
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;

    /// A generated fixed-size APFS passphrase whose bytes are zeroed on drop.
    ///
    /// This type deliberately does not implement `Debug`, `Display`, `Clone`,
    /// serialization, or any conversion into an owned string.
    pub struct StoreVolumeSecret {
        bytes: [u8; HEX_BYTES],
    }

    impl StoreVolumeSecret {
        /// Generates 256 bits with macOS `SecRandomCopyBytes` and hex-encodes
        /// them into a `diskutil`-safe, newline-free passphrase.
        ///
        /// # Errors
        ///
        /// Returns a redacted failure when the operating-system CSPRNG fails.
        pub fn generate() -> Result<Self, SystemKeychainError> {
            let mut random = [0_u8; RANDOM_BYTES];
            SecRandom::default()
                .copy_bytes(&mut random)
                .map_err(|_| keychain_failure())?;
            let mut bytes = [0_u8; HEX_BYTES];
            for (index, byte) in random.iter().copied().enumerate() {
                bytes[index * 2] = hex(byte >> 4);
                bytes[index * 2 + 1] = hex(byte & 0x0f);
            }
            random.zeroize();
            Ok(Self { bytes })
        }

        /// Borrows the passphrase only for direct transfer into a closed sink.
        #[must_use]
        pub const fn expose_for_stdin(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl Drop for StoreVolumeSecret {
        fn drop(&mut self) {
            self.bytes.zeroize();
        }
    }

    /// Stable failures from the closed System Keychain adapter.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SystemKeychainErrorCode {
        InvalidState,
        OperationFailed,
    }

    /// Redacted System Keychain failure.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SystemKeychainError {
        code: SystemKeychainErrorCode,
    }

    impl SystemKeychainError {
        /// Returns the stable failure class.
        #[must_use]
        pub const fn code(self) -> SystemKeychainErrorCode {
            self.code
        }
    }

    impl fmt::Display for SystemKeychainError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("managed System Keychain operation failed")
        }
    }

    impl Error for SystemKeychainError {}

    /// Fixed System Keychain operations for the managed store secret.
    pub struct SystemKeychainStore;

    impl SystemKeychainStore {
        /// Adds the absent fixed item and restricts unattended access to the
        /// protected root helper. Existing items are never overwritten.
        ///
        /// # Errors
        ///
        /// Returns a redacted failure for existing or inaccessible state, or
        /// when creation, ACL assignment, verification, or cleanup fails.
        pub fn create(secret: &StoreVolumeSecret) -> Result<(), SystemKeychainError> {
            let keychain = open_system_keychain()?;
            if find_item(&keychain)?.is_some() {
                return Err(invalid_state());
            }
            create_with_root_helper_access(&keychain, secret)
        }

        /// Returns whether the exact fixed selector exists in System.keychain.
        ///
        /// # Errors
        ///
        /// Returns a redacted failure when System.keychain cannot be queried.
        pub fn exists() -> Result<bool, SystemKeychainError> {
            let keychain = open_system_keychain()?;
            Ok(find_item(&keychain)?.is_some())
        }

        /// Copies the secret into a zeroizing value for direct stdin transfer.
        ///
        /// # Errors
        ///
        /// Returns a redacted failure when the fixed item is absent,
        /// inaccessible, or does not contain the exact secret shape.
        pub fn read() -> Result<StoreVolumeSecret, SystemKeychainError> {
            let keychain = open_system_keychain()?;
            let password = read_raw_password(&keychain)?;
            let value = password.as_ref();
            if value.len() != HEX_BYTES
                || !value
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            {
                return Err(invalid_state());
            }
            let mut bytes = [0_u8; HEX_BYTES];
            bytes.copy_from_slice(value);
            Ok(StoreVolumeSecret { bytes })
        }

        /// Deletes only the exact fixed item. Absence is idempotent.
        ///
        /// # Errors
        ///
        /// Returns a redacted failure when System.keychain cannot be queried
        /// or the exact item cannot be deleted.
        pub fn delete() -> Result<(), SystemKeychainError> {
            let keychain = open_system_keychain()?;
            delete_item_if_present(&keychain)
        }
    }

    fn open_system_keychain() -> Result<SecKeychain, SystemKeychainError> {
        SecKeychain::open(SYSTEM_KEYCHAIN).map_err(|_| keychain_failure())
    }

    fn delete_item_if_present(keychain: &SecKeychain) -> Result<(), SystemKeychainError> {
        find_item(keychain)?.map_or(Ok(()), |item| {
            // The safe crate's `delete` discards the OSStatus, so use the
            // same reference with the raw API and verify the result.
            // SAFETY: `item` is an owned SecKeychainItem reference produced by
            // `find_item` and is valid for the duration of this call.
            let status = unsafe { SecKeychainItemDelete(item.as_concrete_TypeRef()) };
            status_result(status)
        })
    }

    fn find_item(keychain: &SecKeychain) -> Result<Option<SecKeychainItem>, SystemKeychainError> {
        let mut item = ptr::null_mut();
        // A later SecKeychainItemSetAccess call requires an interactive prompt.
        // Create the item with its closed ACL in the same Security.framework call.
        // SAFETY: the service and account pointers reference the live, fixed
        // ASCII constants; the lengths are exact u32 conversions; `item` is
        // null-initialized and written only through `&raw mut`.
        let status = unsafe {
            SecKeychainFindGenericPassword(
                keychain.as_CFTypeRef(),
                u32::try_from(SERVICE.len()).map_err(|_| keychain_failure())?,
                SERVICE.as_ptr().cast(),
                u32::try_from(ACCOUNT.len()).map_err(|_| keychain_failure())?,
                ACCOUNT.as_ptr().cast(),
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut item,
            )
        };
        if status == ERR_SEC_ITEM_NOT_FOUND {
            return Ok(None);
        }
        status_result(status)?;
        if item.is_null() {
            return Err(keychain_failure());
        }
        // SAFETY: `item` is non-null here, and Find returns a +1 reference
        // owned by this scope under the CoreFoundation Create Rule.
        Ok(Some(unsafe {
            SecKeychainItem::wrap_under_create_rule(item)
        }))
    }

    struct ZeroizingKeychainPassword {
        data: *mut c_void,
        len: usize,
    }

    impl AsRef<[u8]> for ZeroizingKeychainPassword {
        fn as_ref(&self) -> &[u8] {
            // SAFETY: `data` and `len` come from one successful
            // SecKeychainFindGenericPassword call; the struct owns the
            // allocation, and no mutable alias exists while `self` is borrowed.
            unsafe { slice::from_raw_parts(self.data.cast(), self.len) }
        }
    }

    impl Drop for ZeroizingKeychainPassword {
        fn drop(&mut self) {
            if !self.data.is_null() {
                // SAFETY: `data` and `len` describe the keychain-owned buffer
                // returned by the matching Find call; drop runs once and is the
                // only writer.
                unsafe { slice::from_raw_parts_mut(self.data.cast::<u8>(), self.len) }.zeroize();
                // SAFETY: `data` is the same keychain-owned buffer; FreeContent
                // is its matching release function.
                let _ = unsafe { SecKeychainItemFreeContent(ptr::null_mut(), self.data) };
            }
        }
    }

    fn read_raw_password(
        keychain: &SecKeychain,
    ) -> Result<ZeroizingKeychainPassword, SystemKeychainError> {
        let mut length = 0_u32;
        let mut data = ptr::null_mut();
        // SAFETY: the service and account pointers reference the live, fixed
        // ASCII constants; `length` and `data` are null-initialized and written
        // only through `&raw mut`.
        let status = unsafe {
            SecKeychainFindGenericPassword(
                keychain.as_CFTypeRef(),
                u32::try_from(SERVICE.len()).map_err(|_| keychain_failure())?,
                SERVICE.as_ptr().cast(),
                u32::try_from(ACCOUNT.len()).map_err(|_| keychain_failure())?,
                ACCOUNT.as_ptr().cast(),
                &raw mut length,
                &raw mut data,
                ptr::null_mut(),
            )
        };
        let password = ZeroizingKeychainPassword {
            data,
            len: length as usize,
        };
        status_result(status)?;
        if password.data.is_null() {
            return Err(keychain_failure());
        }
        Ok(password)
    }

    fn create_with_root_helper_access(
        keychain: &SecKeychain,
        secret: &StoreVolumeSecret,
    ) -> Result<(), SystemKeychainError> {
        let service_length = u32::try_from(SERVICE.len()).map_err(|_| keychain_failure())?;
        let account_length = u32::try_from(ACCOUNT.len()).map_err(|_| keychain_failure())?;
        let secret = secret.expose_for_stdin();
        let secret_length = u32::try_from(secret.len()).map_err(|_| keychain_failure())?;
        let mut trusted_application: CFTypeRef = ptr::null();
        // SAFETY: ROOT_HELPER is a fixed, NUL-terminated byte path;
        // `trusted_application` is null-initialized and written only through
        // `&raw mut`.
        let status = unsafe {
            SecTrustedApplicationCreateFromPath(
                ROOT_HELPER.as_ptr().cast::<c_char>(),
                &raw mut trusted_application,
            )
        };
        status_result(status)?;
        if trusted_application.is_null() {
            return Err(keychain_failure());
        }
        // SAFETY: `trusted_application` is non-null here, and the Create call
        // returns a +1 reference under the CoreFoundation Create Rule.
        let trusted = unsafe { CFType::wrap_under_create_rule(trusted_application.cast()) };
        let trusted_list = CFArray::from_CFTypes(&[trusted]);
        let description = CFString::new(DESCRIPTION);
        let mut access = ptr::null_mut();
        // SAFETY: `description` and `trusted_list` are live CF references that
        // outlive the call; `access` is null-initialized and written only
        // through `&raw mut`.
        let status = unsafe {
            SecAccessCreate(
                description.as_concrete_TypeRef(),
                trusted_list.as_concrete_TypeRef(),
                &raw mut access,
            )
        };
        status_result(status)?;
        if access.is_null() {
            return Err(keychain_failure());
        }

        let mut attributes = [
            SecKeychainAttribute {
                tag: SERVICE_ITEM_ATTRIBUTE,
                length: service_length,
                data: SERVICE.as_ptr().cast_mut().cast(),
            },
            SecKeychainAttribute {
                tag: ACCOUNT_ITEM_ATTRIBUTE,
                length: account_length,
                data: ACCOUNT.as_ptr().cast_mut().cast(),
            },
        ];
        let mut attribute_list = SecKeychainAttributeList {
            count: 2,
            attr: attributes.as_mut_ptr(),
        };
        let mut item = ptr::null_mut();
        // SAFETY: the attribute list references the two live fixed string
        // constants; the secret buffer and keychain reference are live for the
        // call; `access` is an owned +1 reference that the call borrows;
        // `item` is null-initialized and written only through `&raw mut`.
        let status = unsafe {
            SecKeychainItemCreateFromContent(
                GENERIC_PASSWORD_ITEM_CLASS,
                &raw mut attribute_list,
                secret_length,
                secret.as_ptr().cast(),
                keychain.as_CFTypeRef().cast_mut().cast(),
                access,
                &raw mut item,
            )
        };
        // SAFETY: this balances the +1 reference from SecAccessCreate; the
        // item-create call above already retained what it needs.
        unsafe { CFRelease(access.cast()) };
        status_result(status)?;
        if item.is_null() {
            let _ = delete_item_if_present(keychain);
            return Err(keychain_failure());
        }
        // SAFETY: `item` is non-null here, and ItemCreateFromContent returns
        // a +1 reference under the CoreFoundation Create Rule.
        let item = unsafe { SecKeychainItem::wrap_under_create_rule(item) };
        if !matches!(find_item(keychain), Ok(Some(_))) {
            item.delete();
            return Err(keychain_failure());
        }
        Ok(())
    }

    const fn hex(nibble: u8) -> u8 {
        match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        }
    }

    const fn status_result(status: i32) -> Result<(), SystemKeychainError> {
        if status == 0 {
            Ok(())
        } else {
            Err(keychain_failure())
        }
    }

    const fn invalid_state() -> SystemKeychainError {
        SystemKeychainError {
            code: SystemKeychainErrorCode::InvalidState,
        }
    }

    const fn keychain_failure() -> SystemKeychainError {
        SystemKeychainError {
            code: SystemKeychainErrorCode::OperationFailed,
        }
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecTrustedApplicationCreateFromPath(
            path: *const c_char,
            application: *mut CFTypeRef,
        ) -> i32;
        fn SecAccessCreate(
            descriptor: core_foundation::string::CFStringRef,
            trusted_list: core_foundation::array::CFArrayRef,
            access: *mut SecAccessRef,
        ) -> i32;
        fn SecKeychainItemCreateFromContent(
            item_class: u32,
            attributes: *mut SecKeychainAttributeList,
            length: u32,
            data: *const c_void,
            keychain: SecKeychainRef,
            initial_access: SecAccessRef,
            item: *mut SecKeychainItemRef,
        ) -> i32;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn generated_secret_has_closed_shape() -> Result<(), SystemKeychainError> {
            let secret = StoreVolumeSecret::generate()?;
            assert_eq!(secret.expose_for_stdin().len(), 64);
            assert!(secret.expose_for_stdin().iter().all(u8::is_ascii_hexdigit));
            Ok(())
        }

        #[test]
        fn selectors_and_trusted_application_are_fixed() {
            assert_eq!(SYSTEM_KEYCHAIN, "/Library/Keychains/System.keychain");
            assert_eq!(SERVICE, "org.pkg.store-volume");
            assert_eq!(ACCOUNT, "pkg Nix Store");
            assert_eq!(ROOT_HELPER, b"/opt/pkg/bin/pkg-root-helper\0");
            assert_eq!(GENERIC_PASSWORD_ITEM_CLASS, 0x6765_6e70);
            assert_eq!(ACCOUNT_ITEM_ATTRIBUTE, 0x6163_6374);
            assert_eq!(SERVICE_ITEM_ATTRIBUTE, 0x7376_6365);
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{
    StoreVolumeSecret, SystemKeychainError, SystemKeychainErrorCode, SystemKeychainStore,
};
