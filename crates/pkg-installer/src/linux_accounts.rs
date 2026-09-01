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

/// One capture result: the exit status and the captured stdout bytes.
type CaptureStatus = (Option<i32>, Vec<u8>);

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
const BROKER_HOME: &str = "/var/lib/pkg/broker-home";
const DEFAULT_NOLOGIN_SHELL: &str = "/usr/sbin/nologin";

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
            .filter(|asset| matches!(asset.id(), "broker-group" | "broker-user"))
            .filter(|asset| Some(asset.id()) != missing_id)
        {
            match AccountSpec::for_asset(asset, groups)
                .unwrap_or_else(|| unreachable!("broker asset has a specification"))
            {
                AccountSpec::Group { name, gid } => group_records.push(GroupRecord {
                    name: name.to_owned(),
                    gid,
                    members: BTreeSet::new(),
                    password_locked: true,
                    administrators: BTreeSet::new(),
                }),
                AccountSpec::User {
                    name,
                    gid,
                    home,
                    shell,
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
        let uid = match spec {
            AccountSpec::User { name, gid, .. } => {
                let uid = users
                    .iter()
                    .find(|user| user.name == name)
                    .ok_or_else(|| {
                        LinuxAccountError::new(LinuxAccountErrorCode::VerificationFailure)
                    })?
                    .uid;
                if uid != gid {
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
    },
    User {
        name: &'static str,
        gid: u32,
        home: &'static str,
        shell: &'static str,
    },
}

impl AccountSpec {
    fn for_asset(asset: LinuxInstallAsset, groups: ManagedGroupBindings) -> Option<Self> {
        match (asset.id(), asset.kind(), asset.path_or_name()) {
            ("broker-group", LinuxAssetKind::Group, BROKER_NAME) => Some(Self::Group {
                name: BROKER_NAME,
                gid: groups.broker_gid(),
            }),
            ("broker-user", LinuxAssetKind::User, BROKER_NAME) => Some(Self::User {
                name: BROKER_NAME,
                gid: groups.broker_gid(),
                home: BROKER_HOME,
                shell: DEFAULT_NOLOGIN_SHELL,
            }),
            _ => None,
        }
    }

    #[cfg(test)]
    fn directives(self) -> Vec<u8> {
        match self {
            Self::Group { name, gid } => format!("g {name} {gid}\n").into_bytes(),
            Self::User {
                name,
                gid,
                home,
                shell,
            } => format!("u {name} -:{gid} \"pkg Nix broker\" {home} {shell}\n").into_bytes(),
        }
    }
}

fn create_command(
    spec: AccountSpec,
    groupadd: &'static str,
    useradd: &'static str,
    nologin: &'static str,
) -> (&'static str, Vec<String>) {
    match spec {
        AccountSpec::Group { name, gid } => (
            groupadd,
            vec!["--gid".to_owned(), gid.to_string(), name.to_owned()],
        ),
        AccountSpec::User {
            name,
            gid,
            home,
            shell: _,
        } => {
            let mut arguments = vec![
                "--system".to_owned(),
                "--gid".to_owned(),
                gid.to_string(),
                "--home-dir".to_owned(),
                home.to_owned(),
                "--shell".to_owned(),
                nologin.to_owned(),
                "--comment".to_owned(),
                "pkg Nix broker".to_owned(),
                "--no-create-home".to_owned(),
                "--uid".to_owned(),
                gid.to_string(),
            ];
            arguments.push(name.to_owned());
            (useradd, arguments)
        }
    }
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

const fn determinate_id_reserved(id: u32) -> bool {
    id >= DETERMINATE_BUILD_GID
        && id <= DETERMINATE_BUILD_USER_ID_BASE + DETERMINATE_BUILD_USER_COUNT
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
    if let Some(group) = groups.iter().find(|group| group.name == BROKER_NAME) {
        let aliases = groups
            .iter()
            .filter(|candidate| candidate.gid == group.gid)
            .count();
        if group.gid == 0 || aliases != 1 {
            return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
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
        AccountSpec::Group { name, gid } => {
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
                || !group.members.is_empty()
                || !group.password_locked
                || !group.administrators.is_empty()
                || !users
                    .iter()
                    .filter(|user| user.primary_gid == gid)
                    .all(|user| user.name == BROKER_NAME)
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
        } => {
            let Some(group) = groups.iter().find(|group| group.gid == gid) else {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            };
            if group.name != BROKER_NAME {
                return Err(LinuxAccountError::new(LinuxAccountErrorCode::Conflict));
            }
            let Some(user) = users.iter().find(|user| user.name == name) else {
                if users.iter().any(|user| user.uid == gid) {
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
                || !supplementary_memberships_are_empty(groups, name)
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

fn supplementary_memberships_are_empty(groups: &[GroupRecord], user_name: &str) -> bool {
    !groups.iter().any(|group| group.members.contains(user_name))
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
    vec![BROKER_NAME.to_owned()]
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
) -> Result<CaptureStatus, LinuxAccountError> {
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
mod tests;
