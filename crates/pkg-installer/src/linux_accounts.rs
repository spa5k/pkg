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
const DETERMINATE_BUILD_GID: u32 = 30_000;
const DETERMINATE_BUILD_USER_ID_BASE: u32 = 30_000;
const DETERMINATE_BUILD_USER_COUNT: u32 = 32;
const FIRST_PRODUCT_ID: u32 = DETERMINATE_BUILD_USER_ID_BASE + DETERMINATE_BUILD_USER_COUNT + 1;
const LAST_MANAGED_GID: u32 = 39_999;
const BROKER_NAME: &str = "pkg-nix-broker";
const BUILD_GROUP_NAME: &str = "nixbld";
const BROKER_HOME: &str = "/var/lib/pkg/broker-home";
const BUILD_HOME: &str = "/var/empty";
const DEFAULT_NOLOGIN_SHELL: &str = "/usr/sbin/nologin";
const BUILD_USER_COUNT: u8 = 16;

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

#[cfg(test)]
struct PreflightAccountSystem {
    groups: Vec<GroupRecord>,
    users: Vec<UserRecord>,
    mutation_calls: std::rc::Rc<std::cell::Cell<usize>>,
    create_groups: bool,
    read_failures: usize,
}

#[cfg(test)]
impl AccountSystem for PreflightAccountSystem {
    fn acquire_lock(&mut self) -> Result<Option<File>, LinuxAccountError> {
        Ok(None)
    }

    fn groups(&mut self) -> Result<Vec<GroupRecord>, LinuxAccountError> {
        if self.read_failures > 0 {
            self.read_failures = self.read_failures.saturating_sub(1);
            return Err(LinuxAccountError::new(
                LinuxAccountErrorCode::CommandFailure,
            ));
        }
        Ok(self.groups.clone())
    }

    fn users(&mut self) -> Result<Vec<UserRecord>, LinuxAccountError> {
        Ok(self.users.clone())
    }

    fn create(&mut self, spec: AccountSpec) -> Result<(), LinuxAccountError> {
        self.mutation_calls
            .set(self.mutation_calls.get().saturating_add(1));
        if self.create_groups
            && let AccountSpec::Group { name, gid, .. } = spec
        {
            self.groups.push(GroupRecord {
                name: name.to_owned(),
                gid,
                members: BTreeSet::new(),
                password_locked: true,
                administrators: BTreeSet::new(),
            });
            return Ok(());
        }
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }

    fn delete_user(&mut self, _name: &'static str) -> Result<(), LinuxAccountError> {
        self.mutation_calls
            .set(self.mutation_calls.get().saturating_add(1));
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }

    fn delete_group(&mut self, _name: &'static str) -> Result<(), LinuxAccountError> {
        self.mutation_calls
            .set(self.mutation_calls.get().saturating_add(1));
        Err(LinuxAccountError::new(
            LinuxAccountErrorCode::CommandFailure,
        ))
    }
}

/// Selects the stable vendor build id and one free product account id.
///
/// An exact existing product account keeps its current ids. A new product
/// account receives the first id above Determinate's reserved group and user
/// ids that is unoccupied in either namespace. The later creation step rechecks
/// every collision, so a concurrent administrator change fails closed.
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

    #[cfg(test)]
    pub(crate) fn for_existing_preflight_test(
        groups: ManagedGroupBindings,
        missing_id: Option<&str>,
    ) -> (Self, std::rc::Rc<std::cell::Cell<usize>>) {
        let mut group_records = Vec::new();
        let mut user_records = Vec::new();
        for asset in crate::linux_install_assets()
            .iter()
            .copied()
            .filter(|asset| Self::handles(*asset))
            .filter(|asset| Some(asset.id()) != missing_id)
        {
            match AccountSpec::for_asset(asset, groups)
                .unwrap_or_else(|| unreachable!("closed account asset has a specification"))
            {
                AccountSpec::Group { name, gid, .. } => group_records.push(GroupRecord {
                    name: name.to_owned(),
                    gid,
                    members: if name == BUILD_GROUP_NAME {
                        managed_build_users()
                    } else {
                        BTreeSet::new()
                    },
                    password_locked: true,
                    administrators: BTreeSet::new(),
                }),
                AccountSpec::User {
                    name,
                    gid,
                    home,
                    shell,
                    ..
                } => user_records.push(UserRecord {
                    name: name.to_owned(),
                    uid: 31_000_u32.saturating_add(
                        u32::try_from(user_records.len()).unwrap_or_else(|_| unreachable!()),
                    ),
                    primary_gid: gid,
                    home: home.to_owned(),
                    shell: shell.to_owned(),
                    locked: true,
                }),
            }
        }
        let mutation_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Self::with_system(
                groups,
                Box::new(PreflightAccountSystem {
                    groups: group_records,
                    users: user_records,
                    mutation_calls: mutation_calls.clone(),
                    create_groups: false,
                    read_failures: 0,
                }),
            ),
            mutation_calls,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_fresh_preflight_test(
        groups: ManagedGroupBindings,
        read_failures: usize,
    ) -> (Self, std::rc::Rc<std::cell::Cell<usize>>) {
        let mutation_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Self::with_system(
                groups,
                Box::new(PreflightAccountSystem {
                    groups: Vec::new(),
                    users: Vec::new(),
                    mutation_calls: mutation_calls.clone(),
                    create_groups: true,
                    read_failures,
                }),
            ),
            mutation_calls,
        )
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
            AccountSpec::User {
                name,
                gid,
                build_number,
                ..
            } => {
                let uid = users
                    .iter()
                    .find(|user| user.name == name)
                    .ok_or_else(|| {
                        LinuxAccountError::new(LinuxAccountErrorCode::VerificationFailure)
                    })?
                    .uid;
                if build_number.is_none() && uid != gid {
                    return Err(LinuxAccountError::new(
                        LinuxAccountErrorCode::VerificationFailure,
                    ));
                }
                Some(uid)
            }
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
            .find(|user| {
                user.name == BROKER_NAME && user.uid != 0 && !determinate_id_reserved(user.uid)
            })
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
            } else {
                arguments.extend(["--uid".to_owned(), gid.to_string()]);
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
    occupied.extend(users.iter().map(|user| user.uid));
    validate_determinate_accounts(&groups, &users, &occupied)?;
    let existing_broker_gid = existing_group_gid(&groups, BROKER_NAME)?;
    let broker_user = users.iter().find(|user| user.name == BROKER_NAME);
    if existing_broker_gid.is_none() && broker_user.is_some() {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    if broker_user.is_none()
        && groups.iter().any(|group| {
            group.members.contains(BROKER_NAME) || group.administrators.contains(BROKER_NAME)
        })
    {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    if let Some(gid) = existing_broker_gid {
        if !(FIRST_PRODUCT_ID..=LAST_MANAGED_GID).contains(&gid)
            || !verify_existing(
                &AccountSpec::Group {
                    name: BROKER_NAME,
                    gid,
                    permitted_members: BuildMembers::None,
                },
                &groups,
                &users,
            )?
        {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        if let Some(user) = broker_user
            && (determinate_id_reserved(user.uid)
                || !verify_existing(
                    &AccountSpec::User {
                        name: BROKER_NAME,
                        gid,
                        home: BROKER_HOME,
                        shell: DEFAULT_NOLOGIN_SHELL,
                        build_number: None,
                    },
                    &groups,
                    &users,
                )?)
        {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        if broker_user.is_none() && users.iter().any(|user| user.uid == gid) {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
    }
    let broker_gid =
        existing_broker_gid.unwrap_or_else(|| first_free_product_id(&occupied).unwrap_or(0));
    ManagedGroupBindings::new(broker_gid, DETERMINATE_BUILD_GID)
        .map_err(|_| LinuxAccountError::new(LinuxAccountErrorCode::Conflict))
}

fn validate_determinate_accounts(
    groups: &[GroupRecord],
    users: &[UserRecord],
    occupied: &BTreeSet<u32>,
) -> Result<(), LinuxAccountError> {
    let build_group = groups.iter().find(|group| group.name == BUILD_GROUP_NAME);
    match build_group {
        Some(group) if group.gid == DETERMINATE_BUILD_GID => {}
        Some(_) => return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict)),
        None if occupied.contains(&DETERMINATE_BUILD_GID) => {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        None => {}
    }
    let mut present_build_users = BTreeSet::new();
    for number in 1..=DETERMINATE_BUILD_USER_COUNT {
        let name = determinate_build_user_name(number);
        let uid = DETERMINATE_BUILD_USER_ID_BASE + number;
        let mut claims = users
            .iter()
            .filter(|user| user.name == name || user.uid == uid);
        let existing = claims.next();
        if claims.next().is_some()
            || existing.is_some_and(|user| {
                user.name != name
                    || user.uid != uid
                    || user.primary_gid != DETERMINATE_BUILD_GID
                    || user.home != BUILD_HOME
                    || !NOLOGIN_PATHS.contains(&user.shell.as_str())
                    || !user.locked
            })
        {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        if existing.is_some() {
            present_build_users.insert(name);
        }
    }
    let expected_build_users = determinate_build_users();
    match build_group {
        Some(group)
            if !group.password_locked
                || !group.administrators.is_empty()
                || present_build_users != expected_build_users
                || group.members != expected_build_users =>
        {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        None if !present_build_users.is_empty() => {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
        _ => {}
    }
    let complete_build_users = build_group.is_some();
    for name in &expected_build_users {
        let memberships_exact = if complete_build_users {
            supplementary_memberships_are_exact(groups, name, true)
        } else {
            groups.iter().all(|group| !group.members.contains(name))
        };
        if !memberships_exact
            || groups
                .iter()
                .any(|group| group.administrators.contains(name))
        {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
        }
    }
    let primary_build_users = users
        .iter()
        .filter(|user| user.primary_gid == DETERMINATE_BUILD_GID)
        .map(|user| user.name.clone())
        .collect::<BTreeSet<_>>();
    if primary_build_users
        != if complete_build_users {
            expected_build_users
        } else {
            BTreeSet::new()
        }
    {
        return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
    }
    Ok(())
}

const fn determinate_id_reserved(id: u32) -> bool {
    id >= DETERMINATE_BUILD_GID
        && id <= DETERMINATE_BUILD_USER_ID_BASE + DETERMINATE_BUILD_USER_COUNT
}

fn determinate_build_user_name(number: u32) -> String {
    format!("nixbld{number}")
}

fn determinate_build_users() -> BTreeSet<String> {
    (1..=DETERMINATE_BUILD_USER_COUNT)
        .map(determinate_build_user_name)
        .collect()
}

fn first_free_product_id(occupied: &BTreeSet<u32>) -> Option<u32> {
    (FIRST_PRODUCT_ID..=LAST_MANAGED_GID).find(|id| !occupied.contains(id))
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
                if build_number.is_none() && users.iter().any(|user| user.uid == gid) {
                    return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
                }
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
        let locked = inspected_user_names()
            .into_iter()
            .filter_map(
                |name| match run_capture_allow_absent(getent, &["shadow", &name]) {
                    Ok(Some(bytes)) => {
                        Some(parse_shadow_lock(&name, &bytes).map(|locked| (name, locked)))
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

fn inspected_user_names() -> Vec<String> {
    let mut names = vec![BROKER_NAME.to_owned()];
    names.extend((1..=DETERMINATE_BUILD_USER_COUNT).map(determinate_build_user_name));
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
        if shadow.is_some_and(|shadow| shadow.members != members)
            && !gshadow_lags_build_group(fields[0], &members, shadow)
        {
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

/// Accepts the exact account-database crash signature of interrupted build-user
/// creation: `useradd` appends the supplementary member to `/etc/group` before
/// `/etc/gshadow`, so a power loss in that window leaves gshadow lagging by
/// managed build users only. The group-side list stays authoritative because
/// every managed contract check still compares exact member sets, and any other
/// mismatch (foreign groups, gshadow ahead, non-managed differences) keeps
/// failing closed.
fn gshadow_lags_build_group(
    name: &str,
    members: &BTreeSet<String>,
    shadow: Option<&ShadowGroupRecord>,
) -> bool {
    let Some(shadow) = shadow else {
        return false;
    };
    if name != BUILD_GROUP_NAME || !shadow.members.is_subset(members) {
        return false;
    }
    let managed = managed_build_users();
    members
        .difference(&shadow.members)
        .all(|member| managed.contains(member))
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
    locked: &BTreeMap<String, bool>,
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
        broker_uid_after_create: Option<u32>,
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

    fn exact_determinate_accounts() -> (GroupRecord, Vec<UserRecord>) {
        let users = (1..=DETERMINATE_BUILD_USER_COUNT)
            .map(|number| {
                user(
                    &determinate_build_user_name(number),
                    DETERMINATE_BUILD_USER_ID_BASE + number,
                    DETERMINATE_BUILD_GID,
                    BUILD_HOME,
                )
            })
            .collect::<Vec<_>>();
        let members = users
            .iter()
            .map(|user| user.name.as_str())
            .collect::<Vec<_>>();
        (
            group(BUILD_GROUP_NAME, DETERMINATE_BUILD_GID, &members),
            users,
        )
    }

    fn exact_broker_accounts() -> (GroupRecord, UserRecord) {
        (
            group(BROKER_NAME, 31_000, &[]),
            user(BROKER_NAME, 31_001, 31_000, BROKER_HOME),
        )
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
                    let uid = if *name == BROKER_NAME {
                        state.broker_uid_after_create.unwrap_or(gid)
                    } else {
                        31_000_u32.saturating_add(
                            u32::try_from(state.users.len()).map_err(|_| command_error())?,
                        )
                    };
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
    fn group_planning_reserves_determinate_gid_on_a_clean_host() -> Result<(), Box<dyn Error>> {
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group("root", 0, &[])],
            users: vec![user("root", 0, 0, "/root")],
            ..FakeState::default()
        }));
        let bindings = plan_group_bindings(&mut FakeSystem(state))?;
        assert_eq!(bindings.broker_gid(), 30_033);
        assert_eq!(bindings.build_users_gid(), DETERMINATE_BUILD_GID);
        Ok(())
    }

    #[test]
    fn group_planning_refuses_foreign_determinate_gid_occupants() {
        for state in [
            FakeState {
                groups: vec![group("foreign", DETERMINATE_BUILD_GID, &[])],
                ..FakeState::default()
            },
            FakeState {
                users: vec![user("foreign", 42_000, DETERMINATE_BUILD_GID, "/var/empty")],
                ..FakeState::default()
            },
        ] {
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(Arc::new(Mutex::new(state))))
                    .map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_wrong_existing_nixbld_gid() {
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BUILD_GROUP_NAME, 30_001, &[])],
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn group_planning_refuses_foreign_determinate_build_user_ids() {
        for number in 1..=DETERMINATE_BUILD_USER_COUNT {
            let (build_group, mut users) = exact_determinate_accounts();
            users.push(user(
                "foreign",
                DETERMINATE_BUILD_USER_ID_BASE + number,
                42_000,
                BUILD_HOME,
            ));
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![build_group],
                users,
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_wrong_existing_determinate_build_user_binding() {
        for (uid, gid) in [
            (DETERMINATE_BUILD_USER_ID_BASE, DETERMINATE_BUILD_GID),
            (
                DETERMINATE_BUILD_USER_ID_BASE + 1,
                DETERMINATE_BUILD_GID + 1,
            ),
        ] {
            let (build_group, mut users) = exact_determinate_accounts();
            users[0].uid = uid;
            users[0].primary_gid = gid;
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![build_group],
                users,
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_inexact_existing_determinate_user_state() {
        let (_, exact_users) = exact_determinate_accounts();
        let exact = exact_users[16].clone();
        for invalid in [
            UserRecord {
                home: "/home/nixbld17".to_owned(),
                ..exact.clone()
            },
            UserRecord {
                shell: "/bin/sh".to_owned(),
                ..exact.clone()
            },
            UserRecord {
                locked: false,
                ..exact
            },
        ] {
            let (build_group, mut users) = exact_determinate_accounts();
            users[16] = invalid;
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![build_group],
                users,
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_inexact_existing_determinate_group_state() {
        let (exact, users) = exact_determinate_accounts();
        for invalid in [
            GroupRecord {
                password_locked: false,
                ..exact.clone()
            },
            GroupRecord {
                administrators: BTreeSet::from(["root".to_owned()]),
                ..exact.clone()
            },
            GroupRecord {
                members: BTreeSet::from(["foreign".to_owned()]),
                ..exact
            },
        ] {
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![invalid],
                users: users.clone(),
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }

        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BUILD_GROUP_NAME, DETERMINATE_BUILD_GID, &[])],
            users: vec![user(
                "nixbld17",
                DETERMINATE_BUILD_USER_ID_BASE + 17,
                DETERMINATE_BUILD_GID,
                BUILD_HOME,
            )],
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn group_planning_refuses_dangling_vendor_names_in_foreign_groups() {
        let member = group("foreign", 42_000, &["nixbld17"]);
        let mut administrator = group("foreign", 42_000, &[]);
        administrator.administrators.insert("nixbld17".to_owned());
        for foreign in [member, administrator] {
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![foreign],
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_complete_vendor_users_in_foreign_groups() {
        let member = group("docker", 42_000, &["nixbld17"]);
        let mut administrator = group("sudo", 42_001, &[]);
        administrator.administrators.insert("nixbld18".to_owned());
        for foreign in [member, administrator] {
            let (build_group, users) = exact_determinate_accounts();
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![build_group, foreign],
                users,
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_foreign_primary_members_of_determinate_group() {
        let (build_group, mut users) = exact_determinate_accounts();
        users.push(user("foreign", 42_000, DETERMINATE_BUILD_GID, BUILD_HOME));
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![build_group],
            users,
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn group_planning_refuses_partial_users_17_through_32() {
        let mut users = (17..=DETERMINATE_BUILD_USER_COUNT)
            .map(|number| {
                user(
                    &determinate_build_user_name(number),
                    DETERMINATE_BUILD_USER_ID_BASE + number,
                    DETERMINATE_BUILD_GID,
                    BUILD_HOME,
                )
            })
            .collect::<Vec<_>>();
        for user in &mut users {
            user.shell = "/sbin/nologin".to_owned();
        }
        let members = users
            .iter()
            .map(|user| user.name.as_str())
            .collect::<Vec<_>>();
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BUILD_GROUP_NAME, DETERMINATE_BUILD_GID, &members)],
            users,
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn shadow_preflight_covers_all_determinate_build_users() {
        let names = inspected_user_names();
        assert_eq!(names.len(), 33);
        assert_eq!(names.first().map(String::as_str), Some(BROKER_NAME));
        for number in 1..=DETERMINATE_BUILD_USER_COUNT {
            assert!(names.contains(&determinate_build_user_name(number)));
        }
    }

    #[test]
    fn group_planning_reserves_vendor_ids_across_uid_and_gid_namespaces()
    -> Result<(), Box<dyn Error>> {
        for state in [
            FakeState {
                groups: vec![group("foreign", FIRST_PRODUCT_ID, &[])],
                ..FakeState::default()
            },
            FakeState {
                users: vec![user("foreign", FIRST_PRODUCT_ID, 42_000, BUILD_HOME)],
                ..FakeState::default()
            },
            FakeState {
                users: vec![user("foreign", 42_000, FIRST_PRODUCT_ID, BUILD_HOME)],
                ..FakeState::default()
            },
        ] {
            let bindings = plan_group_bindings(&mut FakeSystem(Arc::new(Mutex::new(state))))?;
            assert_eq!(bindings.broker_gid(), FIRST_PRODUCT_ID + 1);
        }
        Ok(())
    }

    #[test]
    fn group_planning_refuses_old_or_incomplete_broker_ids() {
        for state in [
            FakeState {
                groups: vec![group(BROKER_NAME, DETERMINATE_BUILD_GID + 1, &[])],
                ..FakeState::default()
            },
            FakeState {
                groups: vec![group(BROKER_NAME, LAST_MANAGED_GID + 1, &[])],
                ..FakeState::default()
            },
            FakeState {
                groups: vec![group(BROKER_NAME, 31_000, &[])],
                users: vec![user(
                    BROKER_NAME,
                    DETERMINATE_BUILD_USER_ID_BASE + 1,
                    31_000,
                    BROKER_HOME,
                )],
                ..FakeState::default()
            },
            FakeState {
                users: vec![user(BROKER_NAME, 31_000, 31_000, BROKER_HOME)],
                ..FakeState::default()
            },
            FakeState {
                groups: vec![group(BROKER_NAME, 31_000, &[])],
                users: vec![user(BROKER_NAME, 31_001, 31_001, BROKER_HOME)],
                ..FakeState::default()
            },
        ] {
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(Arc::new(Mutex::new(state))))
                    .map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_inexact_existing_broker_group() {
        let (exact, _) = exact_broker_accounts();
        for invalid in [
            GroupRecord {
                password_locked: false,
                ..exact.clone()
            },
            GroupRecord {
                members: BTreeSet::from(["foreign".to_owned()]),
                ..exact.clone()
            },
            GroupRecord {
                administrators: BTreeSet::from(["root".to_owned()]),
                ..exact
            },
        ] {
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![invalid],
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn group_planning_refuses_dangling_broker_privileges() {
        for broker_group in [None, Some(group(BROKER_NAME, 31_000, &[]))] {
            let member = group("docker", 42_000, &[BROKER_NAME]);
            let mut administrator = group("sudo", 42_001, &[]);
            administrator.administrators.insert(BROKER_NAME.to_owned());
            for foreign in [member, administrator] {
                let mut groups = Vec::from_iter(broker_group.clone());
                groups.push(foreign);
                let state = Arc::new(Mutex::new(FakeState {
                    groups,
                    ..FakeState::default()
                }));
                assert_eq!(
                    plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                    Err(LinuxAccountErrorCode::Conflict)
                );
            }
        }
    }

    #[test]
    fn group_planning_keeps_an_exact_broker_group_without_its_user() -> Result<(), Box<dyn Error>> {
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 31_000, &[])],
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(state))?,
            ManagedGroupBindings::new(31_000, DETERMINATE_BUILD_GID)?
        );
        Ok(())
    }

    #[test]
    fn group_planning_refuses_inexact_existing_broker_user() {
        let (_, exact) = exact_broker_accounts();
        for invalid in [
            UserRecord {
                home: "/home/broker".to_owned(),
                ..exact.clone()
            },
            UserRecord {
                shell: "/bin/sh".to_owned(),
                ..exact.clone()
            },
            UserRecord {
                locked: false,
                ..exact
            },
        ] {
            let (broker_group, _) = exact_broker_accounts();
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![broker_group],
                users: vec![invalid],
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }

        for foreign in [group("docker", 42_000, &[BROKER_NAME]), {
            let mut sudo = group("sudo", 42_001, &[]);
            sudo.administrators.insert(BROKER_NAME.to_owned());
            sudo
        }] {
            let (broker_group, broker_user) = exact_broker_accounts();
            let state = Arc::new(Mutex::new(FakeState {
                groups: vec![broker_group, foreign],
                users: vec![broker_user],
                ..FakeState::default()
            }));
            assert_eq!(
                plan_group_bindings(&mut FakeSystem(state)).map_err(LinuxAccountError::code),
                Err(LinuxAccountErrorCode::Conflict)
            );
        }
    }

    #[test]
    fn absent_broker_rechecks_the_planned_uid() {
        let (broker_group, _) = exact_broker_accounts();
        let users = vec![user("foreign", 31_000, 42_000, BUILD_HOME)];
        let spec = AccountSpec::User {
            name: BROKER_NAME,
            gid: 31_000,
            home: BROKER_HOME,
            shell: DEFAULT_NOLOGIN_SHELL,
            build_number: None,
        };
        assert_eq!(
            verify_existing(&spec, &[broker_group], &users).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
    }

    #[test]
    fn broker_uid_refuses_a_post_plan_vendor_range_uid() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(31_000, DETERMINATE_BUILD_GID)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 31_000, &[])],
            users: vec![user(
                BROKER_NAME,
                DETERMINATE_BUILD_USER_ID_BASE + 1,
                31_000,
                BROKER_HOME,
            )],
            ..FakeState::default()
        }));
        assert_eq!(
            fake_manager(bindings, state)
                .broker_uid()
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );
        Ok(())
    }

    #[test]
    fn group_planning_keeps_existing_broker_and_determinate_bindings_stable()
    -> Result<(), Box<dyn Error>> {
        let (build_group, build_users) = exact_determinate_accounts();
        let mut users = build_users;
        users.push(user(BROKER_NAME, 31_235, 31_234, BROKER_HOME));
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![
                group("root", 0, &[]),
                group(BROKER_NAME, 31_234, &[]),
                build_group,
            ],
            users,
            ..FakeState::default()
        }));
        let first = plan_group_bindings(&mut FakeSystem(Arc::clone(&state)))?;
        let retry = plan_group_bindings(&mut FakeSystem(state))?;
        assert_eq!(first, retry);
        assert_eq!(retry.broker_gid(), 31_234);
        assert_eq!(retry.build_users_gid(), DETERMINATE_BUILD_GID);
        Ok(())
    }

    #[test]
    fn group_planning_refuses_managed_gid_aliases_and_exhaustion() {
        let alias = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 30_033, &[]), group("other", 30_033, &[])],
            ..FakeState::default()
        }));
        assert_eq!(
            plan_group_bindings(&mut FakeSystem(alias)).map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::Conflict)
        );

        let full = Arc::new(Mutex::new(FakeState {
            groups: (FIRST_PRODUCT_ID..=LAST_MANAGED_GID)
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
            state
                .users
                .iter()
                .find(|user| user.name == BROKER_NAME)
                .map(|user| user.uid),
            Some(30_000)
        );
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
        let bindings = ManagedGroupBindings::new(30_033, 30_000)?;
        let broker = AccountSpec::for_asset(account_asset("broker-user"), bindings)
            .ok_or_else(command_error)?;
        assert_eq!(
            create_command(
                broker,
                "/usr/sbin/groupadd",
                "/usr/sbin/useradd",
                "/usr/sbin/nologin",
            ),
            (
                "/usr/sbin/useradd",
                vec![
                    "--system".to_owned(),
                    "--gid".to_owned(),
                    "30033".to_owned(),
                    "--home-dir".to_owned(),
                    "/var/lib/pkg/broker-home".to_owned(),
                    "--shell".to_owned(),
                    "/usr/sbin/nologin".to_owned(),
                    "--comment".to_owned(),
                    "pkg Nix broker".to_owned(),
                    "--no-create-home".to_owned(),
                    "--uid".to_owned(),
                    "30033".to_owned(),
                    "pkg-nix-broker".to_owned(),
                ]
            )
        );
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
                vec!["--gid".to_owned(), "30000".to_owned(), "nixbld".to_owned()]
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
                    "30000".to_owned(),
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
    fn new_broker_user_requires_the_planned_uid() -> Result<(), Box<dyn Error>> {
        let bindings = ManagedGroupBindings::new(31_000, DETERMINATE_BUILD_GID)?;
        let state = Arc::new(Mutex::new(FakeState {
            groups: vec![group(BROKER_NAME, 31_000, &[])],
            broker_uid_after_create: Some(31_001),
            ..FakeState::default()
        }));
        let mut manager = fake_manager(bindings, Arc::clone(&state));
        let broker = account_asset("broker-user");
        assert_eq!(
            manager
                .ensure_asset(broker)
                .map_err(LinuxAccountError::code),
            Err(LinuxAccountErrorCode::VerificationFailure)
        );
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
        let locked = BTreeMap::from([("nixbld1".to_owned(), true)]);
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

    #[test]
    fn parse_groups_accepts_only_the_build_group_gshadow_crash_lag() -> Result<(), LinuxAccountError>
    {
        // Power loss between useradd's /etc/group and /etc/gshadow member
        // writes leaves gshadow lagging by managed build users. The group-side
        // list stays authoritative so recovery can reconcile the account.
        let groups = parse_groups(b"nixbld:x:30001:nixbld1,nixbld2\n", b"nixbld:!::nixbld1\n")?;
        assert_eq!(
            groups[0].members,
            BTreeSet::from(["nixbld1".to_owned(), "nixbld2".to_owned()])
        );

        // A foreign group with any mismatch still fails closed.
        assert!(parse_groups(b"devs:x:1001:alice\n", b"devs:!::\n").is_err());
        // A non-managed extra member on the build group still fails closed.
        assert!(parse_groups(b"nixbld:x:30001:nixbld1,root\n", b"nixbld:!::nixbld1\n").is_err());
        // Gshadow ahead of the group file still fails closed.
        assert!(parse_groups(b"nixbld:x:30001:nixbld1\n", b"nixbld:!::nixbld1,nixbld2\n").is_err());
        Ok(())
    }
}
