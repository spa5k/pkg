//! Closed Linux account planning and attempt-owned mutation.

use crate::{LinuxAssetKind, LinuxInstallAsset};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use pkg_nix::ManagedGroupBindings;
use rustix::{
    fs::{FlockOperation, flock},
    io::Errno,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt},
    process::CommandExt,
};

const GETENT_PATHS: &[&str] = &["/usr/bin/getent", "/bin/getent"];
const GROUPADD_PATHS: &[&str] = &["/usr/sbin/groupadd", "/usr/bin/groupadd", "/sbin/groupadd"];
const USERADD_PATHS: &[&str] = &["/usr/sbin/useradd", "/usr/bin/useradd", "/sbin/useradd"];
const USERDEL_PATHS: &[&str] = &["/usr/sbin/userdel", "/usr/bin/userdel", "/sbin/userdel"];
const GROUPDEL_PATHS: &[&str] = &["/usr/sbin/groupdel", "/usr/bin/groupdel", "/sbin/groupdel"];
const NOLOGIN_PATHS: &[&str] = &[
    "/usr/sbin/nologin",
    "/usr/bin/nologin",
    "/sbin/nologin",
    "/bin/nologin",
];
const INSTALL_LOCK: &str = "/run/pkg-install-accounts.lock";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const FIRST_MANAGED_GID: u32 = 30_000;
const LAST_MANAGED_GID: u32 = 39_999;
const BROKER_NAME: &str = "pkg-nix-broker";
const BUILD_GROUP_NAME: &str = "nixbld";
const BROKER_HOME: &str = "/var/lib/pkg/broker-home";
const BUILD_HOME: &str = "/var/empty";
const DEFAULT_NOLOGIN_SHELL: &str = "/usr/sbin/nologin";
const BUILD_USER_COUNT: u8 = 16;
const BUILD_NAMES: [&str; 16] = [
    "nixbld1", "nixbld2", "nixbld3", "nixbld4", "nixbld5", "nixbld6", "nixbld7", "nixbld8",
    "nixbld9", "nixbld10", "nixbld11", "nixbld12", "nixbld13", "nixbld14", "nixbld15", "nixbld16",
];

/// Stable Linux account failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxAccountErrorCode {
    /// A non-account or unknown account asset was supplied.
    UnsupportedAsset,
    /// An existing identity or numeric id conflicts with the managed plan.
    Conflict,
    /// A fixed account-directory or mutation command failed.
    CommandFailure,
    /// The post-mutation account state did not match the managed plan.
    VerificationFailure,
}

/// Redacted Linux account-management error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxAccountError {
    code: LinuxAccountErrorCode,
}

impl LinuxAccountError {
    const fn new(code: LinuxAccountErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure class.
    #[must_use]
    pub const fn code(self) -> LinuxAccountErrorCode {
        self.code
    }
}

impl fmt::Display for LinuxAccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("linux account operation failed")
    }
}

impl Error for LinuxAccountError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupRecord {
    name: String,
    gid: u32,
    members: BTreeSet<String>,
    password_locked: bool,
    administrators: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserRecord {
    name: String,
    uid: u32,
    primary_gid: u32,
    home: String,
    shell: String,
    locked: bool,
}

trait AccountSystem {
    fn acquire_lock(&mut self) -> Result<Option<File>, LinuxAccountError>;
    fn groups(&mut self) -> Result<Vec<GroupRecord>, LinuxAccountError>;
    fn users(&mut self) -> Result<Vec<UserRecord>, LinuxAccountError>;
    fn create(&mut self, spec: AccountSpec) -> Result<(), LinuxAccountError>;
    fn delete_user(&mut self, name: &'static str) -> Result<(), LinuxAccountError>;
    fn delete_group(&mut self, name: &'static str) -> Result<(), LinuxAccountError>;
}

struct ProductionAccountSystem;

/// Selects two free, stable group ids from the configured Linux account view.
///
/// An exact existing product group keeps its current id. A new group receives
/// the first unoccupied id in the fixed managed range. The later creation step
/// rechecks every collision, so a concurrent administrator change fails closed.
///
/// # Errors
///
/// Returns a redacted error for unreadable, duplicate, root-valued, aliased, or
/// exhausted group-directory state.
pub fn plan_linux_group_bindings() -> Result<ManagedGroupBindings, LinuxAccountError> {
    plan_group_bindings(&mut ProductionAccountSystem)
}

pub fn verify_linux_accounts_absent() -> Result<(), LinuxAccountError> {
    let mut system = ProductionAccountSystem;
    let groups = system.groups()?;
    let users = system.users()?;
    validate_group_directory(&groups)?;
    validate_user_directory(&users)?;
    for asset in crate::linux_install_assets() {
        let present = match asset.kind() {
            LinuxAssetKind::Group => groups
                .iter()
                .any(|group| group.name == asset.path_or_name()),
            LinuxAssetKind::User => users.iter().any(|user| user.name == asset.path_or_name()),
            LinuxAssetKind::Directory | LinuxAssetKind::File => false,
        };
        if present {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::VerificationFailure,
            ));
        }
    }
    Ok(())
}

/// Owns Linux account mutations made by one installer attempt.
///
/// Dropping this value does not delete accounts. The enclosing installer calls
/// [`Self::rollback_asset`] in reverse asset order when its transaction fails.
pub struct LinuxAccountManager {
    groups: ManagedGroupBindings,
    attempt_owned: BTreeMap<&'static str, AccountOwnership>,
    system: Box<dyn AccountSystem>,
    lock: Option<File>,
    lock_acquired: bool,
}

impl LinuxAccountManager {
    /// Creates a production manager bound to the authenticated ownership ids.
    #[must_use]
    pub fn new(groups: ManagedGroupBindings) -> Self {
        Self {
            groups,
            attempt_owned: BTreeMap::new(),
            system: Box::new(ProductionAccountSystem),
            lock: None,
            lock_acquired: false,
        }
    }

    #[cfg(test)]
    fn with_system(groups: ManagedGroupBindings, system: Box<dyn AccountSystem>) -> Self {
        Self {
            groups,
            attempt_owned: BTreeMap::new(),
            system,
            lock: None,
            lock_acquired: false,
        }
    }

    /// Returns true when this manager owns the fixed account asset kind.
    #[must_use]
    pub const fn handles(asset: LinuxInstallAsset) -> bool {
        matches!(asset.kind(), LinuxAssetKind::User | LinuxAssetKind::Group)
    }

    /// Verifies or creates one fixed account asset.
    ///
    /// A machine-wide lock serializes product installers. An uncertain claim is
    /// recorded only after the locked precheck proves the fixed name absent.
    /// The full directory postcondition promotes it to verified ownership. An
    /// uncertain identity is never deleted because an administrator may own it.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for an unsupported asset, a conflict, a command
    /// failure, or a postcondition failure.
    pub fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, LinuxAccountError> {
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)
            .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::UnsupportedAsset))?;
        let groups = self.system.groups()?;
        let users = self.system.users()?;
        if verify_existing(&spec, &groups, &users)? {
            return Ok(false);
        }

        self.attempt_owned
            .insert(asset.id(), AccountOwnership::Uncertain);
        self.system.create(spec)?;

        let groups = match self.system.groups() {
            Ok(groups) => groups,
            Err(error) => {
                self.attempt_owned
                    .insert(asset.id(), AccountOwnership::Uncertain);
                return Err(error);
            }
        };
        let users = match self.system.users() {
            Ok(users) => users,
            Err(error) => {
                self.attempt_owned
                    .insert(asset.id(), AccountOwnership::Uncertain);
                return Err(error);
            }
        };
        if !verify_existing(&spec, &groups, &users)? {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::VerificationFailure,
            ));
        }
        if spec.is_last_build_user() {
            verify_complete_build_group(&groups, &users, self.groups.build_users_gid())?;
        }
        let uid = match spec {
            AccountSpec::User { name, .. } => Some(
                users
                    .iter()
                    .find(|user| user.name == name)
                    .ok_or_else(|| {
                        LinuxAccountError::new(LinuxAccountErrorCode::VerificationFailure)
                    })?
                    .uid,
            ),
            AccountSpec::Group { .. } => None,
        };
        self.attempt_owned
            .insert(asset.id(), AccountOwnership::Verified { uid });
        Ok(true)
    }

    fn ensure_lock(&mut self) -> Result<(), LinuxAccountError> {
        if !self.lock_acquired {
            self.lock = self.system.acquire_lock()?;
            self.lock_acquired = true;
        }
        Ok(())
    }

    /// Returns the verified non-root broker uid for filesystem ownership binding.
    ///
    /// # Errors
    ///
    /// Returns a redacted conflict if the complete broker account contract is not met.
    pub fn broker_uid(&mut self) -> Result<u32, LinuxAccountError> {
        self.ensure_lock()?;
        let spec = AccountSpec::User {
            name: BROKER_NAME,
            gid: self.groups.broker_gid(),
            home: BROKER_HOME,
            shell: DEFAULT_NOLOGIN_SHELL,
            build_number: None,
        };
        let groups = self.system.groups()?;
        let users = self.system.users()?;
        if !verify_existing(&spec, &groups, &users)? {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        users
            .iter()
            .find(|user| user.name == BROKER_NAME && user.uid != 0)
            .map(|user| user.uid)
            .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::Conflict))
    }

    /// Verifies one fixed account against the complete managed contract.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the account is absent, conflicting, or
    /// cannot be read through the fixed account database commands.
    pub fn verify_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), LinuxAccountError> {
        if !Self::handles(asset) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::UnsupportedAsset,
            ));
        }
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)
            .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::UnsupportedAsset))?;
        let groups = self.system.groups()?;
        let users = self.system.users()?;
        if verify_existing(&spec, &groups, &users)? {
            Ok(())
        } else {
            Err(LinuxAccountError::new(
                LinuxAccountErrorCode::VerificationFailure,
            ))
        }
    }

    /// Removes one account only when this exact attempt recorded ownership.
    ///
    /// # Errors
    ///
    /// Returns a redacted command error. Ownership remains recorded after a
    /// failed deletion so a caller can retry rollback safely.
    pub fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), LinuxAccountError> {
        if !Self::handles(asset) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::UnsupportedAsset,
            ));
        }
        let Some(ownership) = self.attempt_owned.get(asset.id()).copied() else {
            return Ok(());
        };
        self.ensure_lock()?;
        if ownership == AccountOwnership::Uncertain {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        if let AccountOwnership::Verified { uid } = ownership {
            let spec = AccountSpec::for_asset(asset, self.groups)
                .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::UnsupportedAsset))?;
            let groups = self.system.groups()?;
            let users = self.system.users()?;
            if !verify_existing(&spec, &groups, &users)? {
                self.attempt_owned.remove(asset.id());
                return Ok(());
            }
            if let Some(expected_uid) = uid {
                let AccountSpec::User { name, .. } = spec else {
                    return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
                };
                if users
                    .iter()
                    .find(|user| user.name == name)
                    .is_none_or(|user| user.uid != expected_uid)
                {
                    return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
                }
            }
        }
        match asset.kind() {
            LinuxAssetKind::User => self.system.delete_user(asset.path_or_name())?,
            LinuxAssetKind::Group => self.system.delete_group(asset.path_or_name())?,
            LinuxAssetKind::Directory | LinuxAssetKind::File => {
                return Err(LinuxAccountError::new(
                    LinuxAccountErrorCode::UnsupportedAsset,
                ));
            }
        }
        self.attempt_owned.remove(asset.id());
        Ok(())
    }

    /// Removes one manifest-owned account after revalidating its exact contract.
    ///
    /// This path is for a later uninstall process, so it does not depend on
    /// in-memory installation-attempt state. An already absent account is an
    /// idempotent success. A conflicting account is never removed.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the account database is unsafe, the
    /// recorded contract no longer matches, or deletion cannot be verified.
    pub fn remove_verified_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxAccountError> {
        if !Self::handles(asset) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::UnsupportedAsset,
            ));
        }
        self.ensure_lock()?;
        let spec = AccountSpec::for_asset(asset, self.groups)
            .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::UnsupportedAsset))?;
        let groups = self.system.groups()?;
        let users = self.system.users()?;
        validate_group_directory(&groups)?;
        validate_user_directory(&users)?;
        if let AccountSpec::User { name, .. } = spec
            && users.iter().all(|user| user.name != name)
        {
            return Ok(());
        }
        if !verify_existing(&spec, &groups, &users)? {
            return Ok(());
        }

        match asset.kind() {
            LinuxAssetKind::User => self.system.delete_user(asset.path_or_name())?,
            LinuxAssetKind::Group => self.system.delete_group(asset.path_or_name())?,
            LinuxAssetKind::Directory | LinuxAssetKind::File => {
                return Err(LinuxAccountError::new(
                    LinuxAccountErrorCode::UnsupportedAsset,
                ));
            }
        }

        let groups = self.system.groups()?;
        let users = self.system.users()?;
        if let AccountSpec::User { name, .. } = spec
            && users.iter().all(|user| user.name != name)
        {
            return Ok(());
        }
        if verify_existing(&spec, &groups, &users)? {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::VerificationFailure,
            ));
        }
        Ok(())
    }

    /// Verifies that one fixed account or group is absent.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the account database is unsafe, unreadable,
    /// or still contains the fixed identity.
    pub fn verify_asset_absent(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<(), LinuxAccountError> {
        if !Self::handles(asset) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::UnsupportedAsset,
            ));
        }
        self.ensure_lock()?;
        let groups = self.system.groups()?;
        let users = self.system.users()?;
        validate_group_directory(&groups)?;
        validate_user_directory(&users)?;
        let present = match asset.kind() {
            LinuxAssetKind::User => users.iter().any(|user| user.name == asset.path_or_name()),
            LinuxAssetKind::Group => groups
                .iter()
                .any(|group| group.name == asset.path_or_name()),
            LinuxAssetKind::Directory | LinuxAssetKind::File => true,
        };
        if present {
            Err(LinuxAccountError::new(
                LinuxAccountErrorCode::VerificationFailure,
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountOwnership {
    Uncertain,
    Verified { uid: Option<u32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountSpec {
    Group {
        name: &'static str,
        gid: u32,
        permitted_members: BuildMembers,
    },
    User {
        name: &'static str,
        gid: u32,
        home: &'static str,
        shell: &'static str,
        build_number: Option<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildMembers {
    None,
    ManagedSubset,
}

impl AccountSpec {
    fn for_asset(asset: LinuxInstallAsset, groups: ManagedGroupBindings) -> Option<Self> {
        match (asset.id(), asset.kind(), asset.path_or_name()) {
            ("broker-group", LinuxAssetKind::Group, BROKER_NAME) => Some(Self::Group {
                name: BROKER_NAME,
                gid: groups.broker_gid(),
                permitted_members: BuildMembers::None,
            }),
            ("broker-user", LinuxAssetKind::User, BROKER_NAME) => Some(Self::User {
                name: BROKER_NAME,
                gid: groups.broker_gid(),
                home: BROKER_HOME,
                shell: DEFAULT_NOLOGIN_SHELL,
                build_number: None,
            }),
            ("build-group", LinuxAssetKind::Group, BUILD_GROUP_NAME) => Some(Self::Group {
                name: BUILD_GROUP_NAME,
                gid: groups.build_users_gid(),
                permitted_members: BuildMembers::ManagedSubset,
            }),
            (id, LinuxAssetKind::User, name) => {
                build_user_number(id, name).map(|number| Self::User {
                    name,
                    gid: groups.build_users_gid(),
                    home: BUILD_HOME,
                    shell: DEFAULT_NOLOGIN_SHELL,
                    build_number: Some(number),
                })
            }
            _ => None,
        }
    }

    #[cfg(test)]
    fn directives(self) -> Vec<u8> {
        match self {
            Self::Group { name, gid, .. } => format!("g {name} {gid}\n").into_bytes(),
            Self::User {
                name,
                gid,
                home,
                shell,
                build_number,
            } => {
                let description = build_number.map_or_else(
                    || "pkg Nix broker".to_owned(),
                    |number| format!("pkg Nix build user {number}"),
                );
                let membership = build_number
                    .map(|_| format!("m {name} {BUILD_GROUP_NAME}\n"))
                    .unwrap_or_default();
                format!("u {name} -:{gid} \"{description}\" {home} {shell}\n{membership}")
                    .into_bytes()
            }
        }
    }

    const fn is_last_build_user(self) -> bool {
        matches!(
            self,
            Self::User {
                build_number: Some(BUILD_USER_COUNT),
                ..
            }
        )
    }
}

fn create_command(
    spec: AccountSpec,
    groupadd: &'static str,
    useradd: &'static str,
    nologin: &'static str,
) -> (&'static str, Vec<String>) {
    match spec {
        AccountSpec::Group { name, gid, .. } => (
            groupadd,
            vec!["--gid".to_owned(), gid.to_string(), name.to_owned()],
        ),
        AccountSpec::User {
            name,
            gid,
            home,
            shell: _,
            build_number,
        } => {
            let description = build_number.map_or_else(
                || "pkg Nix broker".to_owned(),
                |number| format!("pkg Nix build user {number}"),
            );
            let mut arguments = vec![
                "--system".to_owned(),
                "--gid".to_owned(),
                gid.to_string(),
                "--home-dir".to_owned(),
                home.to_owned(),
                "--shell".to_owned(),
                nologin.to_owned(),
                "--comment".to_owned(),
                description,
                "--no-create-home".to_owned(),
            ];
            if build_number.is_some() {
                arguments.extend(["--groups".to_owned(), BUILD_GROUP_NAME.to_owned()]);
            }
            arguments.push(name.to_owned());
            (useradd, arguments)
        }
    }
}

fn build_user_number(id: &str, name: &str) -> Option<u8> {
    (1..=BUILD_USER_COUNT)
        .find(|number| id == format!("build-user-{number:02}") && name == format!("nixbld{number}"))
}

fn managed_build_users() -> BTreeSet<String> {
    (1..=BUILD_USER_COUNT)
        .map(|number| format!("nixbld{number}"))
        .collect()
}

fn plan_group_bindings(
    system: &mut dyn AccountSystem,
) -> Result<ManagedGroupBindings, LinuxAccountError> {
    let groups = system.groups()?;
    let users = system.users()?;
    validate_group_directory(&groups)?;
    validate_user_directory(&users)?;
    let mut occupied = groups
        .iter()
        .map(|group| group.gid)
        .collect::<BTreeSet<_>>();
    occupied.extend(users.iter().map(|user| user.primary_gid));
    let broker_gid = existing_group_gid(&groups, BROKER_NAME)?
        .unwrap_or_else(|| first_free_gid(&occupied, &BTreeSet::new()).unwrap_or(0));
    let reserved = BTreeSet::from([broker_gid]);
    let build_gid = existing_group_gid(&groups, BUILD_GROUP_NAME)?
        .unwrap_or_else(|| first_free_gid(&occupied, &reserved).unwrap_or(0));
    ManagedGroupBindings::new(broker_gid, build_gid)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::Conflict))
}

fn first_free_gid(occupied: &BTreeSet<u32>, reserved: &BTreeSet<u32>) -> Option<u32> {
    (FIRST_MANAGED_GID..=LAST_MANAGED_GID)
        .find(|gid| !occupied.contains(gid) && !reserved.contains(gid))
}

fn existing_group_gid(
    groups: &[GroupRecord],
    name: &str,
) -> Result<Option<u32>, LinuxAccountError> {
    let matches = groups
        .iter()
        .filter(|group| group.name == name)
        .collect::<Vec<_>>();
    if matches.len() > 1 || matches.first().is_some_and(|group| group.gid == 0) {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    Ok(matches.first().map(|group| group.gid))
}

fn validate_group_directory(groups: &[GroupRecord]) -> Result<(), LinuxAccountError> {
    let mut names = BTreeSet::new();
    for group in groups {
        if group.name.is_empty() || !names.insert(group.name.as_str()) {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
    }
    for managed_name in [BROKER_NAME, BUILD_GROUP_NAME] {
        if let Some(group) = groups.iter().find(|group| group.name == managed_name) {
            let aliases = groups
                .iter()
                .filter(|candidate| candidate.gid == group.gid)
                .count();
            if group.gid == 0 || aliases != 1 {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
        }
    }
    Ok(())
}

fn verify_existing(
    spec: &AccountSpec,
    groups: &[GroupRecord],
    users: &[UserRecord],
) -> Result<bool, LinuxAccountError> {
    validate_group_directory(groups)?;
    validate_user_directory(users)?;
    match *spec {
        AccountSpec::Group {
            name,
            gid,
            permitted_members,
        } => {
            if groups
                .iter()
                .any(|group| group.gid == gid && group.name != name)
            {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
            let Some(group) = groups.iter().find(|group| group.name == name) else {
                if users.iter().any(|user| user.primary_gid == gid) {
                    return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
                }
                return Ok(false);
            };
            if group.gid != gid
                || !members_permitted(&group.members, permitted_members)
                || !primary_members_permitted(users, gid, permitted_members)
                || !group.password_locked
                || !group.administrators.is_empty()
            {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
            Ok(true)
        }
        AccountSpec::User {
            name,
            gid,
            home,
            shell,
            build_number,
        } => {
            let Some(group) = groups.iter().find(|group| group.gid == gid) else {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            };
            let expected_group = if build_number.is_some() {
                BUILD_GROUP_NAME
            } else {
                BROKER_NAME
            };
            if group.name != expected_group {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
            let Some(user) = users.iter().find(|user| user.name == name) else {
                return Ok(false);
            };
            let uid_uses = users
                .iter()
                .filter(|candidate| candidate.uid == user.uid)
                .count();
            if user.uid == 0
                || uid_uses != 1
                || user.primary_gid != gid
                || user.home != home
                || user.shell != shell && !NOLOGIN_PATHS.contains(&user.shell.as_str())
                || !user.locked
                || build_number.is_some() && !group.members.contains(name)
                || !supplementary_memberships_are_exact(groups, name, build_number.is_some())
                || groups
                    .iter()
                    .any(|candidate| candidate.administrators.contains(name))
            {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
            Ok(true)
        }
    }
}

fn supplementary_memberships_are_exact(
    groups: &[GroupRecord],
    user_name: &str,
    is_build_user: bool,
) -> bool {
    let actual = groups
        .iter()
        .filter(|group| group.members.contains(user_name))
        .map(|group| group.name.as_str())
        .collect::<BTreeSet<_>>();
    if is_build_user {
        actual == BTreeSet::from([BUILD_GROUP_NAME])
    } else {
        actual.is_empty()
    }
}

fn members_permitted(members: &BTreeSet<String>, policy: BuildMembers) -> bool {
    match policy {
        BuildMembers::None => members.is_empty(),
        BuildMembers::ManagedSubset => members.is_subset(&managed_build_users()),
    }
}

fn primary_members_permitted(users: &[UserRecord], gid: u32, policy: BuildMembers) -> bool {
    let primary = users
        .iter()
        .filter(|user| user.primary_gid == gid)
        .map(|user| user.name.clone())
        .collect::<BTreeSet<_>>();
    match policy {
        BuildMembers::None => primary.is_subset(&BTreeSet::from([BROKER_NAME.to_owned()])),
        BuildMembers::ManagedSubset => primary.is_subset(&managed_build_users()),
    }
}

fn validate_user_directory(users: &[UserRecord]) -> Result<(), LinuxAccountError> {
    let mut names = BTreeSet::new();
    if users
        .iter()
        .any(|user| user.name.is_empty() || !names.insert(user.name.as_str()))
    {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    Ok(())
}

fn verify_complete_build_group(
    groups: &[GroupRecord],
    users: &[UserRecord],
    gid: u32,
) -> Result<(), LinuxAccountError> {
    let expected = managed_build_users();
    let group = groups
        .iter()
        .find(|group| group.name == BUILD_GROUP_NAME && group.gid == gid)
        .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::VerificationFailure))?;
    let primary = users
        .iter()
        .filter(|user| user.primary_gid == gid)
        .map(|user| user.name.clone())
        .collect::<BTreeSet<_>>();
    if group.members != expected || primary != expected {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::VerificationFailure,
        ));
    }
    Ok(())
}

impl AccountSystem for ProductionAccountSystem {
    fn acquire_lock(&mut self) -> Result<Option<File>, LinuxAccountError> {
        acquire_install_lock().map(Some)
    }

    fn groups(&mut self) -> Result<Vec<GroupRecord>, LinuxAccountError> {
        let getent = resolve_program(GETENT_PATHS)?;
        let output = run_capture(getent, &["group"])?;
        let gshadow = run_capture(getent, &["gshadow"])?;
        parse_groups(&output, &gshadow)
    }

    fn users(&mut self) -> Result<Vec<UserRecord>, LinuxAccountError> {
        let getent = resolve_program(GETENT_PATHS)?;
        let passwd = run_capture(getent, &["passwd"])?;
        let locked = managed_user_names()
            .into_iter()
            .filter_map(
                |name| match run_capture_allow_absent(getent, &["shadow", name]) {
                    Ok(Some(bytes)) => {
                        Some(parse_shadow_lock(name, &bytes).map(|locked| (name, locked)))
                    }
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        parse_users(&passwd, &locked)
    }

    fn create(&mut self, spec: AccountSpec) -> Result<(), LinuxAccountError> {
        let (program, arguments) = create_command(
            spec,
            resolve_program(GROUPADD_PATHS)?,
            resolve_program(USERADD_PATHS)?,
            resolve_program(NOLOGIN_PATHS)?,
        );
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        run_status(program, &arguments)
    }

    fn delete_user(&mut self, name: &'static str) -> Result<(), LinuxAccountError> {
        run_status_allow_absent(resolve_program(USERDEL_PATHS)?, &[name])
    }

    fn delete_group(&mut self, name: &'static str) -> Result<(), LinuxAccountError> {
        run_status_allow_absent(resolve_program(GROUPDEL_PATHS)?, &[name])
    }
}

fn managed_user_names() -> Vec<&'static str> {
    let mut names = vec![BROKER_NAME];
    names.extend(BUILD_NAMES);
    names
}

fn parse_groups(bytes: &[u8], gshadow_bytes: &[u8]) -> Result<Vec<GroupRecord>, LinuxAccountError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let gshadow = parse_gshadow(gshadow_bytes)?;
    let mut groups = Vec::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 4 || fields[0].is_empty() {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
        let members = fields[3]
            .split(',')
            .filter(|member| !member.is_empty())
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let shadow = gshadow.get(fields[0]);
        if shadow.is_some_and(|shadow| shadow.members != members) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
        groups.push(GroupRecord {
            name: fields[0].to_owned(),
            gid: fields[2]
                .parse()
                .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?,
            members,
            password_locked: shadow.is_some_and(|shadow| shadow.password_locked),
            administrators: shadow
                .map(|shadow| shadow.administrators.clone())
                .unwrap_or_default(),
        });
    }
    if groups.is_empty() {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(groups)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShadowGroupRecord {
    password_locked: bool,
    administrators: BTreeSet<String>,
    members: BTreeSet<String>,
}

fn parse_gshadow(bytes: &[u8]) -> Result<BTreeMap<String, ShadowGroupRecord>, LinuxAccountError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let mut groups = BTreeMap::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 4 || fields[0].is_empty() {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
        let administrators = parse_comma_set(fields[2])?;
        let members = parse_comma_set(fields[3])?;
        let record = ShadowGroupRecord {
            password_locked: fields[1].is_empty()
                || fields[1].starts_with('!')
                || fields[1].starts_with('*'),
            administrators,
            members,
        };
        if groups.insert(fields[0].to_owned(), record).is_some() {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
    }
    if groups.is_empty() {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(groups)
}

fn parse_comma_set(value: &str) -> Result<BTreeSet<String>, LinuxAccountError> {
    let mut values = BTreeSet::new();
    for item in value.split(',').filter(|item| !item.is_empty()) {
        if !values.insert(item.to_owned()) {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
    }
    Ok(values)
}

fn parse_users(
    bytes: &[u8],
    locked: &BTreeMap<&str, bool>,
) -> Result<Vec<UserRecord>, LinuxAccountError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let mut users = Vec::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 7 || fields[0].is_empty() {
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
        users.push(UserRecord {
            name: fields[0].to_owned(),
            uid: fields[2]
                .parse()
                .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?,
            primary_gid: fields[3]
                .parse()
                .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?,
            home: fields[5].to_owned(),
            shell: fields[6].to_owned(),
            locked: locked.get(fields[0]).copied().unwrap_or(false),
        });
    }
    if users.is_empty() {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(users)
}

fn parse_shadow_lock(name: &str, bytes: &[u8]) -> Result<bool, LinuxAccountError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let mut lines = text.lines();
    let fields = lines
        .next()
        .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?
        .split(':')
        .collect::<Vec<_>>();
    if fields.len() != 9 || fields[0] != name || lines.next().is_some() {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(fields[1].starts_with('!') || fields[1].starts_with('*'))
}

pub fn run_capture(program: &str, arguments: &[&str]) -> Result<Vec<u8>, LinuxAccountError> {
    let (code, bytes) = run_capture_status(program, arguments)?;
    if code == Some(0) {
        Ok(bytes)
    } else {
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }
}

fn run_capture_allow_absent(
    program: &str,
    arguments: &[&str],
) -> Result<Option<Vec<u8>>, LinuxAccountError> {
    let (code, bytes) = run_capture_status(program, arguments)?;
    if code == Some(0) {
        Ok(Some(bytes))
    } else if code == Some(2) {
        Ok(None)
    } else {
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }
}

pub fn run_capture_status(
    program: &str,
    arguments: &[&str],
) -> Result<(Option<i32>, Vec<u8>), LinuxAccountError> {
    require_program(program)?;
    let mut child = base_command(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes);
        (result, bytes)
    });
    let status = wait_bounded(&mut child)?;
    let (read_result, bytes) = reader
        .join()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    if read_result.is_err() || bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok((status.code(), bytes))
}

pub fn run_status(program: &str, arguments: &[&str]) -> Result<(), LinuxAccountError> {
    require_program(program)?;
    let mut child = base_command(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    if !wait_bounded(&mut child)?.success() {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(())
}

fn acquire_install_lock() -> Result<File, LinuxAccountError> {
    acquire_root_lock(Path::new(INSTALL_LOCK))
}

pub fn acquire_root_lock(path: &Path) -> Result<File, LinuxAccountError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let metadata = file
        .metadata()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(Errno::WOULDBLOCK) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                return Err(LinuxAccountError::new(
                    LinuxAccountErrorCode::CommandFailure,
                ));
            }
        }
    }
}

pub fn run_status_allow_absent(program: &str, arguments: &[&str]) -> Result<(), LinuxAccountError> {
    require_program(program)?;
    let mut child = base_command(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    let status = wait_bounded(&mut child)?;
    if status.success() || status.code() == Some(6) {
        Ok(())
    } else {
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }
}

fn require_program(program: &str) -> Result<(), LinuxAccountError> {
    let path = Path::new(program);
    let metadata = path
        .metadata()
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))?;
    if !path.is_absolute()
        || !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ));
    }
    Ok(())
}

fn resolve_program(candidates: &'static [&'static str]) -> Result<&'static str, LinuxAccountError> {
    candidates
        .iter()
        .copied()
        .find(|candidate| require_program(candidate).is_ok())
        .ok_or_else(|| LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure))
}

fn base_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    #[cfg(unix)]
    command.process_group(0);
    command
}

fn wait_bounded(child: &mut Child) -> Result<ExitStatus, LinuxAccountError> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                terminate(child);
                return Err(LinuxAccountError::new(
                    LinuxAccountErrorCode::CommandFailure,
                ));
            }
        }
    }
}

fn terminate(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        groups: Vec<GroupRecord>,
        users: Vec<UserRecord>,
        applied: Vec<Vec<u8>>,
        deleted: Vec<String>,
        fail_create_after_mutation: bool,
    }

    struct FakeSystem(Arc<Mutex<FakeState>>);

    impl AccountSystem for FakeSystem {
        fn acquire_lock(&mut self) -> Result<Option<File>, LinuxAccountError> {
            Ok(None)
        }

        fn groups(&mut self) -> Result<Vec<GroupRecord>, LinuxAccountError> {
            Ok(self.0.lock().map_err(|_| command_error())?.groups.clone())
        }

        fn users(&mut self) -> Result<Vec<UserRecord>, LinuxAccountError> {
            Ok(self.0.lock().map_err(|_| command_error())?.users.clone())
        }

        fn create(&mut self, spec: AccountSpec) -> Result<(), LinuxAccountError> {
            let directives = spec.directives();
            let mut state = self.0.lock().map_err(|_| command_error())?;
            state.applied.push(directives.clone());
            apply_fake(&mut state, &directives)?;
            if state.fail_create_after_mutation {
                return Err(command_error());
            }
            drop(state);
            Ok(())
        }

        fn delete_user(&mut self, name: &'static str) -> Result<(), LinuxAccountError> {
            let mut state = self.0.lock().map_err(|_| command_error())?;
            state.users.retain(|user| user.name != name);
            for group in &mut state.groups {
                group.members.remove(name);
            }
            if state
                .groups
                .iter()
                .find(|group| group.name == name)
                .is_some_and(|group| {
                    group.members.is_empty()
                        && state.users.iter().all(|user| user.primary_gid != group.gid)
                })
            {
                state.groups.retain(|group| group.name != name);
            }
            state.deleted.push(format!("user:{name}"));
            drop(state);
            Ok(())
        }

        fn delete_group(&mut self, name: &'static str) -> Result<(), LinuxAccountError> {
            let mut state = self.0.lock().map_err(|_| command_error())?;
            state.groups.retain(|group| group.name != name);
            state.deleted.push(format!("group:{name}"));
            drop(state);
            Ok(())
        }
    }

    fn command_error() -> LinuxAccountError {
        LinuxAccountError::new(LinuxAccountErrorCode::CommandFailure)
    }

    fn group(name: &str, gid: u32, members: &[&str]) -> GroupRecord {
        GroupRecord {
            name: name.to_owned(),
            gid,
            members: members.iter().map(|member| (*member).to_owned()).collect(),
            password_locked: true,
            administrators: BTreeSet::new(),
        }
    }

    fn user(name: &str, uid: u32, gid: u32, home: &str) -> UserRecord {
        UserRecord {
            name: name.to_owned(),
            uid,
            primary_gid: gid,
            home: home.to_owned(),
            shell: DEFAULT_NOLOGIN_SHELL.to_owned(),
            locked: true,
        }
    }

    fn fake_manager(
        groups: ManagedGroupBindings,
        state: Arc<Mutex<FakeState>>,
    ) -> LinuxAccountManager {
        LinuxAccountManager::with_system(groups, Box::new(FakeSystem(state)))
    }

    fn account_asset(id: &str) -> LinuxInstallAsset {
        crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == id)
            .unwrap_or_else(|| unreachable!())
    }

    fn apply_fake(state: &mut FakeState, directives: &[u8]) -> Result<(), LinuxAccountError> {
        let text = std::str::from_utf8(directives).map_err(|_| command_error())?;
        for line in text.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            match fields.as_slice() {
                ["g", name, gid] => {
                    state
                        .groups
                        .push(group(name, gid.parse().map_err(|_| command_error())?, &[]));
                }
                ["u", name, uid_gid, ..] => {
                    let gid = uid_gid
                        .strip_prefix("-:")
                        .ok_or_else(command_error)?
                        .parse()
                        .map_err(|_| command_error())?;
                    let home = if *name == BROKER_NAME {
                        BROKER_HOME
                    } else {
                        BUILD_HOME
                    };
                    let uid = 31_000_u32.saturating_add(
                        u32::try_from(state.users.len()).map_err(|_| command_error())?,
                    );
                    state.users.push(user(name, uid, gid, home));
                }
                ["m", name, group_name] => {
                    state
                        .groups
                        .iter_mut()
                        .find(|group| group.name == *group_name)
                        .ok_or_else(command_error)?
                        .members
                        .insert((*name).to_owned());
                }
                _ => return Err(command_error()),
            }
        }
        Ok(())
    }

    #[test]
    fn group_planning_reuses_exact_groups_and_fills_first_free_ids() -> Result<(), Box<dyn Error>> {
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group("root", 0, &[]), group(BROKER_NAME, 31_234, &[])],
            users: vec![user("foreign", 42_000, 30_000, "/var/empty")],
            ..FakeState::default()
        }));
        let bindings = plan_group_bindings(&mut FakeSystem(state))?;
        assert_eq!(bindings.broker_gid(), 31_234);
        assert_eq!(bindings.build_users_gid(), 30_001);
        Ok(())
    }

    #[test]
    fn group_planning_refuses_managed_gid_aliases_and_exhaustion() {
        let alias = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_000, &[]), group("other", 30_000, &[])],
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(alias)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );

        let full = Arc::new(Mutex::new(FakeState {
            groups: (FIRST_MANAGED_GID..=LAST_MANAGED_GID)
                .map(|gid| group(&format!("g{gid}"), gid, &[]))
                .collect(),
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(full)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn exact_directives_create_locked_primary_and_explicit_members() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group("root", 0, &[])],
            users: vec![user("root", 0, 0, "/root")],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        for asset in crate::linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| LinuxAccountManager::handles(*asset))
        {
            assert!(manager.ensure_asset(asset)?);
        }
        let state = state.lock().map_err(|_| command_error())?;
        assert_eq!(state.applied.len(), 19);
        assert_eq!(state.applied[0], b"g pkg-nix-broker 30000\n");
        assert_eq!(
            state.applied[3],
            b"u nixbld1 -:30001 \"pkg Nix build user 1\" /var/empty /usr/sbin/nologin\nm nixbld1 nixbld\n"
        );
        assert_eq!(state.groups[2].members, managed_build_users());
        drop(state);
        Ok(())
    }

    #[test]
    fn production_create_commands_are_fixed_and_non_interactive() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let group = AccountSpec::for_asset(account_asset("build-group"), bindings)
            .ok_or_else(command_error)?;
        assert_eq!(
            create_command(
                group,
                "/usr/sbin/groupadd",
                "/usr/sbin/useradd",
                "/usr/sbin/nologin",
            ),
            (
                "/usr/sbin/groupadd",
                vec!["--gid".to_owned(), "30001".to_owned(), "nixbld".to_owned()]
            )
        );
        let user = AccountSpec::for_asset(account_asset("build-user-01"), bindings)
            .ok_or_else(command_error)?;
        assert_eq!(
            create_command(
                user,
                "/usr/sbin/groupadd",
                "/usr/sbin/useradd",
                "/usr/sbin/nologin",
            ),
            (
                "/usr/sbin/useradd",
                vec![
                    "--system".to_owned(),
                    "--gid".to_owned(),
                    "30001".to_owned(),
                    "--home-dir".to_owned(),
                    "/var/empty".to_owned(),
                    "--shell".to_owned(),
                    "/usr/sbin/nologin".to_owned(),
                    "--comment".to_owned(),
                    "pkg Nix build user 1".to_owned(),
                    "--no-create-home".to_owned(),
                    "--groups".to_owned(),
                    "nixbld".to_owned(),
                    "nixbld1".to_owned(),
                ]
            )
        );
        Ok(())
    }

    #[test]
    fn exact_existing_accounts_are_idempotent() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let build_names = managed_build_users();
        let mut users = vec![user(BROKER_NAME, 31_000, 30_000, BROKER_HOME)];
        users.extend((1..=BUILD_USER_COUNT).map(|number| {
            user(
                &format!("nixbld{number}"),
                31_000 + u32::from(number),
                30_001,
                BUILD_HOME,
            )
        }));
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![
                group(BROKER_NAME, 30_000, &[]),
                GroupRecord {
                    name: BUILD_GROUP_NAME.to_owned(),
                    gid: 30_001,
                    members: build_names,
                    password_locked: true,
                    administrators: BTreeSet::new(),
                },
            ],
            users,
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        for asset in crate::linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| LinuxAccountManager::handles(*asset))
        {
            assert!(!manager.ensure_asset(asset)?);
        }
        assert_eq!(manager.broker_uid()?, 31_000);
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .applied
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn conflicts_refuse_without_mutation() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_002, &[])],
            users: vec![user("root", 0, 0, "/root")],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        assert_eq!(
            manager
                .ensure_asset(account_asset("broker-group"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .applied
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn foreign_primary_group_members_refuse_without_mutation() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_000, &[])],
            users: vec![user("foreign", 31_500, 30_000, "/var/empty")],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        assert_eq!(
            manager
                .ensure_asset(account_asset("broker-group"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .applied
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn unexpected_supplementary_memberships_refuse_adoption() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![
                group(BROKER_NAME, 30_000, &[]),
                group("docker", 999, &[BROKER_NAME]),
            ],
            users: vec![user(BROKER_NAME, 31_000, 30_000, BROKER_HOME)],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        assert_eq!(
            manager
                .ensure_asset(account_asset("broker-user"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .applied
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn uncertain_creation_is_never_deleted_and_existing_assets_are_never_deleted()
    -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_000, &[])],
            users: vec![user("root", 0, 0, "/root")],
            fail_create_after_mutation: true,
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        assert!(!manager.ensure_asset(account_asset("broker-group"))?);
        assert_eq!(
            manager
                .ensure_asset(account_asset("build-group"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::CommandFailure)
        );
        assert_eq!(
            manager
                .rollback_asset(account_asset("build-group"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        manager.rollback_asset(account_asset("broker-group"))?;
        let state = state.lock().map_err(|_| command_error())?;
        assert!(state.deleted.is_empty());
        assert!(state.groups.iter().any(|group| group.name == BROKER_NAME));
        drop(state);
        Ok(())
    }

    #[test]
    fn rollback_refuses_to_delete_a_changed_identity() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_000, &[])],
            users: vec![user("root", 0, 0, "/root")],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        let broker = account_asset("broker-user");
        assert!(manager.ensure_asset(broker)?);
        state
            .lock()
            .map_err(|_| command_error())?
            .users
            .iter_mut()
            .find(|user| user.name == BROKER_NAME)
            .ok_or_else(command_error)?
            .uid = 49_999;
        assert_eq!(
            manager
                .rollback_asset(broker)
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .deleted
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn verified_uninstall_removes_only_exact_accounts_and_is_retry_safe()
    -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            users: vec![user("root", 0, 0, "/root")],
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        let group = account_asset("broker-group");
        let user = account_asset("broker-user");
        assert!(manager.ensure_asset(group)?);
        assert!(manager.ensure_asset(user)?);

        manager.remove_verified_asset(user)?;
        manager.remove_verified_asset(group)?;
        manager.remove_verified_asset(user)?;
        manager.remove_verified_asset(group)?;

        let state = state.lock().map_err(|_| command_error())?;
        assert_eq!(state.deleted, ["user:pkg-nix-broker"]);
        assert!(state.users.iter().all(|user| user.name != BROKER_NAME));
        assert!(state.groups.iter().all(|group| group.name != BROKER_NAME));
        drop(state);
        Ok(())
    }

    #[test]
    fn verified_uninstall_refuses_changed_accounts_without_deletion() -> Result<(), Box<dyn Error>>
    {
        let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_000, &[])],
            users: vec![
                user("root", 0, 0, "/root"),
                user(BROKER_NAME, 31_000, 30_000, BROKER_HOME),
            ],
            ..FakeState::default()
        }));
        state
            .lock()
            .map_err(|_| command_error())?
            .users
            .iter_mut()
            .find(|user| user.name == BROKER_NAME)
            .ok_or_else(command_error)?
            .shell = "/bin/sh".to_owned();
        let mut manager = fake_manager(bindings, Arc::clone(&state));

        assert_eq!(
            manager
                .remove_verified_asset(account_asset("broker-user"))
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        assert!(
            state
                .lock()
                .map_err(|_| command_error())?
                .deleted
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn parsers_require_locked_exact_directory_records() -> Result<(), Box<dyn Error>> {
        let groups = parse_groups(
            b"root:x:0:\nnixbld:x:30001:nixbld1\n",
            b"root:*::\nnixbld:!::nixbld1\n",
        )?;
        assert_eq!(groups[1].members, BTreeSet::from(["nixbld1".to_owned()]));
        let locked = BTreeMap::from([("nixbld1", true)]);
        let users = parse_users(
            b"root:x:0:0:root:/root:/bin/sh\nnixbld1:x:31001:30001::/var/empty:/usr/sbin/nologin\n",
            &locked,
        )?;
        assert!(users[1].locked);
        assert!(parse_shadow_lock("nixbld1", b"nixbld1:!:1:2:3:4:5:6:7\n")?);
        assert!(!parse_shadow_lock(
            "nixbld1",
            b"nixbld1:$6$hash:1:2:3:4:5:6:7\n"
        )?);
        Ok(())
    }
}
