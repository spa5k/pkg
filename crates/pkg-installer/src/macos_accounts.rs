//! Closed macOS Directory Services account installation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::Path,
};

use pkg_nix::ManagedGroupBindings;

use crate::MacOsAssetPresence;
use crate::{MacOsAssetKind, MacOsError, MacOsInstallAsset};

const DSCL: &str = "/usr/bin/dscl";
const LOCK: &str = "/private/var/db/pkg-install-accounts.lock";
const BROKER_NAME: &str = "pkg-nix-broker";
const BUILD_GROUP: &str = "nixbld";
const BROKER_UID: u32 = 333;
const BROKER_GID: u32 = 333;
// Nix 2.34.8 moved this range above IDs reserved by macOS Sequoia.
pub const BUILD_GID: u32 = 350;
const BROKER_HOME: &str = "/Library/Application Support/pkg/broker-home";
const BUILD_HOME: &str = "/var/empty";
const NOLOGIN: &str = "/usr/bin/false";

/// Returns the fixed macOS group bindings for ownership verification.
pub fn macos_group_bindings() -> Result<ManagedGroupBindings, MacOsError> {
    ManagedGroupBindings::new(BROKER_GID, BUILD_GID).map_err(|_| MacOsError::backend_failure())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttemptOwnership {
    Uncertain,
    Verified,
}

#[derive(Clone, Copy)]
enum AccountSpec {
    Group {
        name: &'static str,
        gid: u32,
    },
    User {
        name: &'static str,
        uid: u32,
        gid: u32,
        home: &'static str,
        group: &'static str,
    },
}

impl AccountSpec {
    fn for_asset(
        asset: MacOsInstallAsset,
        groups: ManagedGroupBindings,
    ) -> Result<Self, MacOsError> {
        if groups.broker_gid() != BROKER_GID || groups.build_users_gid() != BUILD_GID {
            return Err(MacOsError::backend_failure());
        }
        match (asset.kind(), asset.id()) {
            (MacOsAssetKind::Group, "broker-group") => Ok(Self::Group {
                name: BROKER_NAME,
                gid: BROKER_GID,
            }),
            (MacOsAssetKind::Group, "build-group") => Ok(Self::Group {
                name: BUILD_GROUP,
                gid: BUILD_GID,
            }),
            (MacOsAssetKind::User, "broker-user") => Ok(Self::User {
                name: BROKER_NAME,
                uid: BROKER_UID,
                gid: BROKER_GID,
                home: BROKER_HOME,
                group: BROKER_NAME,
            }),
            (MacOsAssetKind::User, id) if id.starts_with("build-user-") => {
                let number = id
                    .strip_prefix("build-user-")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|value| (1..=32).contains(value))
                    .ok_or_else(MacOsError::backend_failure)?;
                let name = asset.path_or_name();
                if name != format!("_nixbld{number}") {
                    return Err(MacOsError::backend_failure());
                }
                Ok(Self::User {
                    name,
                    uid: BUILD_GID.saturating_add(number),
                    gid: BUILD_GID,
                    home: BUILD_HOME,
                    group: BUILD_GROUP,
                })
            }
            _ => Err(MacOsError::backend_failure()),
        }
    }
}

/// Owns exact macOS account mutations for one install attempt.
pub struct MacOsAccountManager {
    groups: ManagedGroupBindings,
    attempt_owned: BTreeMap<&'static str, AttemptOwnership>,
    lock: Option<File>,
}

impl MacOsAccountManager {
    pub(crate) const fn new(groups: ManagedGroupBindings) -> Result<Self, MacOsError> {
        if groups.broker_gid() != BROKER_GID || groups.build_users_gid() != BUILD_GID {
            return Err(MacOsError::backend_failure());
        }
        Ok(Self {
            groups,
            attempt_owned: BTreeMap::new(),
            lock: None,
        })
    }

    pub(crate) const fn handles(asset: MacOsInstallAsset) -> bool {
        matches!(asset.kind(), MacOsAssetKind::User | MacOsAssetKind::Group)
    }

    pub(crate) fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.ensure_lock()?;
        let spec = AccountSpec::User {
            name: BROKER_NAME,
            uid: BROKER_UID,
            gid: BROKER_GID,
            home: BROKER_HOME,
            group: BROKER_NAME,
        };
        if Self::verify(spec, true)? {
            Ok(BROKER_UID)
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    pub(crate) fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)?;
        if Self::verify(spec, false)? {
            return Ok(false);
        }
        Self::verify_absent(spec)?;
        self.attempt_owned
            .insert(asset.id(), AttemptOwnership::Uncertain);
        Self::create(spec)?;
        if !Self::verify(spec, true)? {
            return Err(MacOsError::backend_failure());
        }
        self.attempt_owned
            .insert(asset.id(), AttemptOwnership::Verified);
        Ok(true)
    }

    pub(crate) fn verify_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)?;
        if Self::verify(spec, true)? {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    pub(crate) fn verify_asset_absent(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.ensure_lock()?;
        Self::verify_absent(AccountSpec::for_asset(asset, self.groups)?)
    }

    pub(crate) fn classify_for_removal(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)?;
        if Self::verify(spec, false)? {
            Ok(MacOsAssetPresence::ExactPresent)
        } else if Self::is_absent(spec)? {
            Ok(MacOsAssetPresence::Absent)
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    pub(crate) fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        let Some(ownership) = self.attempt_owned.get(asset.id()).copied() else {
            return Ok(());
        };
        if ownership != AttemptOwnership::Verified {
            return Err(MacOsError::backend_failure());
        }
        self.remove_verified_asset(asset)?;
        self.attempt_owned.remove(asset.id());
        Ok(())
    }

    pub(crate) fn remove_verified_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<(), MacOsError> {
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)?;
        if let AccountSpec::User { name, group, .. } = spec {
            if Self::is_absent(spec)? {
                return Ok(());
            }
            if !Self::verify(spec, false)? {
                return Err(MacOsError::backend_failure());
            }
            if Self::group_members(group)?.contains(name) {
                remove_group_member(group, name)?;
            }
        } else if Self::is_absent(spec)? {
            return Ok(());
        } else if !Self::verify(spec, true)? {
            return Err(MacOsError::backend_failure());
        }
        if let AccountSpec::Group { name, .. } = spec
            && !Self::group_members(name)?.is_empty()
        {
            return Err(MacOsError::backend_failure());
        }
        let path = match spec {
            AccountSpec::Group { name, .. } => format!("/Groups/{name}"),
            AccountSpec::User { name, .. } => format!("/Users/{name}"),
        };
        if !Self::is_absent(spec)? {
            run_status(&[".", "-delete", &path])?;
        }
        Self::verify_absent(spec)
    }

    fn ensure_lock(&mut self) -> Result<(), MacOsError> {
        if self.lock.is_none() {
            self.lock = Some(
                crate::linux_accounts::acquire_root_lock(Path::new(LOCK))
                    .map_err(|_| MacOsError::backend_failure())?,
            );
        }
        Ok(())
    }

    fn verify(spec: AccountSpec, require_membership: bool) -> Result<bool, MacOsError> {
        let users = list_ids("/Users", "UniqueID")?;
        let groups = list_ids("/Groups", "PrimaryGroupID")?;
        validate_directory(&users)?;
        validate_directory(&groups)?;
        match spec {
            AccountSpec::Group { name, gid } => Ok(groups.get(name) == Some(&gid)),
            AccountSpec::User {
                name,
                uid,
                gid,
                home,
                group,
            } => {
                if users.get(name) != Some(&uid) || groups.get(group) != Some(&gid) {
                    return Ok(false);
                }
                let path = format!("/Users/{name}");
                let fields = read_fields(
                    &path,
                    &[
                        "UniqueID",
                        "PrimaryGroupID",
                        "NFSHomeDirectory",
                        "UserShell",
                        "IsHidden",
                    ],
                )?;
                let uid = uid.to_string();
                let gid = gid.to_string();
                let exact = field(&fields, "UniqueID") == Some(uid.as_str())
                    && field(&fields, "PrimaryGroupID") == Some(gid.as_str())
                    && field(&fields, "NFSHomeDirectory") == Some(home)
                    && field(&fields, "UserShell") == Some(NOLOGIN)
                    && field(&fields, "IsHidden") == Some("1");
                if !exact {
                    return Ok(false);
                }
                if !require_membership {
                    return Ok(true);
                }
                let members = Self::group_members(group)?;
                if group == BROKER_NAME {
                    return Ok(members == BTreeSet::from([BROKER_NAME.to_owned()]));
                }
                if name == "_nixbld32" {
                    let expected = (1..=32)
                        .map(|number| format!("_nixbld{number}"))
                        .collect::<BTreeSet<_>>();
                    return Ok(members == expected);
                }
                Ok(members.contains(name))
            }
        }
    }

    fn verify_absent(spec: AccountSpec) -> Result<(), MacOsError> {
        if Self::is_absent(spec)? {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    fn is_absent(spec: AccountSpec) -> Result<bool, MacOsError> {
        let users = list_ids("/Users", "UniqueID")?;
        let groups = list_ids("/Groups", "PrimaryGroupID")?;
        validate_directory(&users)?;
        validate_directory(&groups)?;
        Ok(match spec {
            AccountSpec::Group { name, gid } => {
                !groups.contains_key(name) && !groups.values().any(|value| *value == gid)
            }
            AccountSpec::User { name, uid, .. } => {
                !users.contains_key(name) && !users.values().any(|value| *value == uid)
            }
        })
    }

    fn create(spec: AccountSpec) -> Result<(), MacOsError> {
        match spec {
            AccountSpec::Group { name, gid } => {
                let path = format!("/Groups/{name}");
                create_field(&path, "PrimaryGroupID", &gid.to_string())?;
                create_field(&path, "RealName", name)
            }
            AccountSpec::User {
                name,
                uid,
                gid,
                home,
                group,
            } => {
                let path = format!("/Users/{name}");
                for (key, value) in [
                    ("UniqueID", uid.to_string()),
                    ("PrimaryGroupID", gid.to_string()),
                    ("NFSHomeDirectory", home.to_owned()),
                    ("UserShell", NOLOGIN.to_owned()),
                    ("IsHidden", "1".to_owned()),
                    ("Password", "*".to_owned()),
                ] {
                    create_field(&path, key, &value)?;
                }
                let group_path = format!("/Groups/{group}");
                run_status(&[".", "-append", &group_path, "GroupMembership", name])
            }
        }
    }

    fn group_members(name: &str) -> Result<BTreeSet<String>, MacOsError> {
        let fields = read_fields(&format!("/Groups/{name}"), &["GroupMembership"])?;
        Ok(field(&fields, "GroupMembership")
            .map(|members| members.split_whitespace().map(ToOwned::to_owned).collect())
            .unwrap_or_default())
    }
}

pub fn verify_macos_accounts_absent() -> Result<(), MacOsError> {
    let users = list_ids("/Users", "UniqueID")?;
    let groups = list_ids("/Groups", "PrimaryGroupID")?;
    validate_directory(&users)?;
    validate_directory(&groups)?;
    if crate::macos_install_assets()
        .iter()
        .any(|asset| match asset.kind() {
            MacOsAssetKind::User => users.contains_key(asset.path_or_name()),
            MacOsAssetKind::Group => groups.contains_key(asset.path_or_name()),
            MacOsAssetKind::Directory | MacOsAssetKind::File => false,
        })
    {
        Err(MacOsError::backend_failure())
    } else {
        Ok(())
    }
}

pub fn verify_macos_accounts_after_broker_removal(
    groups: ManagedGroupBindings,
) -> Result<(), MacOsError> {
    let mut manager = MacOsAccountManager::new(groups)?;
    for asset in crate::macos_install_assets() {
        match asset.kind() {
            MacOsAssetKind::User => manager.verify_asset_absent(*asset)?,
            MacOsAssetKind::Group => {
                manager.classify_for_removal(*asset)?;
            }
            MacOsAssetKind::Directory | MacOsAssetKind::File => {}
        }
    }
    Ok(())
}

pub fn broker_account_presence(
    groups: ManagedGroupBindings,
) -> Result<MacOsAssetPresence, MacOsError> {
    let _manager = MacOsAccountManager::new(groups)?;
    let spec = AccountSpec::User {
        name: BROKER_NAME,
        uid: BROKER_UID,
        gid: BROKER_GID,
        home: BROKER_HOME,
        group: BROKER_NAME,
    };
    if MacOsAccountManager::verify(spec, true)? {
        Ok(MacOsAssetPresence::ExactPresent)
    } else if MacOsAccountManager::is_absent(spec)? {
        Ok(MacOsAssetPresence::Absent)
    } else {
        Err(MacOsError::backend_failure())
    }
}

fn create_field(path: &str, key: &str, value: &str) -> Result<(), MacOsError> {
    run_status(&[".", "-create", path, key, value])
}

fn run_status(arguments: &[&str]) -> Result<(), MacOsError> {
    crate::linux_accounts::run_status(DSCL, arguments).map_err(|_| MacOsError::backend_failure())
}

fn remove_group_member(group: &str, name: &str) -> Result<(), MacOsError> {
    let path = format!("/Groups/{group}");
    crate::linux_accounts::run_status_allow_absent(
        DSCL,
        &[".", "-delete", &path, "GroupMembership", name],
    )
    .map_err(|_| MacOsError::backend_failure())
}

fn capture(arguments: &[&str]) -> Result<Vec<u8>, MacOsError> {
    crate::linux_accounts::run_capture(DSCL, arguments).map_err(|_| MacOsError::backend_failure())
}

fn list_ids(path: &str, attribute: &str) -> Result<BTreeMap<String, u32>, MacOsError> {
    parse_id_list(&capture(&[".", "-list", path, attribute])?)
}

fn parse_id_list(bytes: &[u8]) -> Result<BTreeMap<String, u32>, MacOsError> {
    let text = std::str::from_utf8(bytes).map_err(|_| MacOsError::backend_failure())?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next().ok_or_else(MacOsError::backend_failure)?;
        let id = parts.next().ok_or_else(MacOsError::backend_failure)?;
        let id = id
            .parse::<u32>()
            .or_else(|_| id.parse::<i32>().map(i32::cast_unsigned))
            .map_err(|_| MacOsError::backend_failure())?;
        if parts.next().is_some() || values.insert(name.to_owned(), id).is_some() {
            return Err(MacOsError::backend_failure());
        }
    }
    if values.is_empty() {
        return Err(MacOsError::backend_failure());
    }
    Ok(values)
}

fn validate_directory(values: &BTreeMap<String, u32>) -> Result<(), MacOsError> {
    let mut ids = BTreeSet::new();
    if values
        .keys()
        .any(|name| name.is_empty() || name.contains('/'))
        || values.values().any(|id| !ids.insert(*id))
    {
        Err(MacOsError::backend_failure())
    } else {
        Ok(())
    }
}

fn read_fields(path: &str, attributes: &[&str]) -> Result<BTreeMap<String, String>, MacOsError> {
    let mut arguments = vec![".", "-read", path];
    arguments.extend_from_slice(attributes);
    parse_fields(&capture(&arguments)?)
}

fn parse_fields(bytes: &[u8]) -> Result<BTreeMap<String, String>, MacOsError> {
    let text = std::str::from_utf8(bytes).map_err(|_| MacOsError::backend_failure())?;
    let mut fields = BTreeMap::new();
    let mut continued_key = None;
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            let key = continued_key
                .take()
                .ok_or_else(MacOsError::backend_failure)?;
            insert_field(&mut fields, key, line.trim())?;
            continue;
        }
        if continued_key.is_some() {
            return Err(MacOsError::backend_failure());
        }
        let (key, value) = line
            .rsplit_once(':')
            .ok_or_else(MacOsError::backend_failure)?;
        let key = match key {
            "dsAttrTypeNative:IsHidden" => "IsHidden",
            key => key,
        };
        if key.is_empty() || key.contains(':') {
            return Err(MacOsError::backend_failure());
        }
        let value = value.trim();
        if value.is_empty() {
            continued_key = Some(key);
        } else {
            insert_field(&mut fields, key, value)?;
        }
    }
    if continued_key.is_some() {
        return Err(MacOsError::backend_failure());
    }
    Ok(fields)
}

fn insert_field(
    fields: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> Result<(), MacOsError> {
    if value.is_empty() || fields.insert(key.to_owned(), value.to_owned()).is_some() {
        return Err(MacOsError::backend_failure());
    }
    Ok(())
}

fn field<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    fields.get(name).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_ids_match_nix_2_34_8_on_sequoia() {
        assert_eq!(BUILD_GID, 350);
        assert_eq!(BUILD_GID + 1, 351);
        assert_eq!(BUILD_GID + 32, 382);
    }

    #[test]
    fn directory_service_parsers_reject_ambiguous_ids_and_fields() -> Result<(), MacOsError> {
        assert_eq!(parse_id_list(b"root 0\nuser 501\n")?["user"], 501);
        assert_eq!(parse_id_list(b"nobody -2\n")?["nobody"], u32::MAX - 1);
        assert_eq!(parse_id_list(b"nogroup -1\n")?["nogroup"], u32::MAX);
        assert!(
            parse_id_list(b"first 501\nsecond 501\n")
                .and_then(|values| validate_directory(&values))
                .is_err()
        );
        assert_eq!(
            parse_fields(b"UniqueID: 333\nUserShell: /usr/bin/false\n")?["UserShell"],
            "/usr/bin/false"
        );
        let fields = parse_fields(
            b"dsAttrTypeNative:IsHidden: 1\nNFSHomeDirectory:\n /Library/Application Support/pkg/broker-home\nPrimaryGroupID: 333\nUniqueID: 333\nUserShell: /usr/bin/false\n",
        )?;
        assert_eq!(fields["IsHidden"], "1");
        assert_eq!(
            fields["NFSHomeDirectory"],
            "/Library/Application Support/pkg/broker-home"
        );
        assert!(parse_fields(b"UniqueID: 333\nUniqueID: 334\n").is_err());
        assert!(parse_fields(b"IsHidden: 1\ndsAttrTypeNative:IsHidden: 1\n").is_err());
        assert!(parse_fields(b"dsAttrTypeNative:UniqueID: 333\n").is_err());
        assert!(parse_fields(b"NFSHomeDirectory:\nnext: value\n").is_err());
        assert!(parse_fields(b" unexpected\n").is_err());
        Ok(())
    }
}
