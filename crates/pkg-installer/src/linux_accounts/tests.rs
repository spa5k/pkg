//! Tests for the `linux_accounts` module.

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

fn exact_broker_accounts() -> (GroupRecord, UserRecord) {
    (
        group(BROKER_NAME, 31_000, &[]),
        user(BROKER_NAME, 31_001, 31_000, BROKER_HOME),
    )
}

fn fake_manager(groups: ManagedGroupBindings, state: Arc<Mutex<FakeState>>) -> LinuxAccountManager {
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
                let uid = state.broker_uid_after_create.unwrap_or(gid);
                state.users.push(user(name, uid, gid, BROKER_HOME));
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
fn group_planning_reserves_vendor_ids_across_uid_and_gid_namespaces() -> Result<(), Box<dyn Error>>
{
    for state in [
        FakeState {
            groups: vec![group("foreign", FIRST_PRODUCT_ID, &[])],
            ..FakeState::default()
        },
        FakeState {
            users: vec![user("foreign", FIRST_PRODUCT_ID, 42_000, "/var/empty")],
            ..FakeState::default()
        },
        FakeState {
            users: vec![user("foreign", 42_000, FIRST_PRODUCT_ID, "/var/empty")],
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
    let users = vec![user("foreign", 31_000, 42_000, "/var/empty")];
    let spec = AccountSpec::User {
        name: BROKER_NAME,
        gid: 31_000,
        home: BROKER_HOME,
        shell: DEFAULT_NOLOGIN_SHELL,
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
fn group_planning_keeps_existing_broker_bindings_stable() -> Result<(), Box<dyn Error>> {
    let state = Arc::new(Mutex::new(FakeState {
        groups: vec![group("root", 0, &[]), group(BROKER_NAME, 31_234, &[])],
        users: vec![user(BROKER_NAME, 31_235, 31_234, BROKER_HOME)],
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
    for asset in ["broker-group", "broker-user"] {
        assert!(manager.ensure_asset(account_asset(asset))?);
    }
    let state = state.lock().map_err(|_| command_error())?;
    assert_eq!(state.applied.len(), 2);
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
        state.applied[1],
        b"u pkg-nix-broker -:30000 \"pkg Nix broker\" /var/lib/pkg/broker-home /usr/sbin/nologin\n"
    );
    assert!(
        state
            .groups
            .iter()
            .find(|group| group.name == BROKER_NAME)
            .is_some_and(|group| group.members.is_empty())
    );
    assert!(
        state
            .users
            .iter()
            .all(|user| user.name == BROKER_NAME || user.name == "root")
    );
    drop(state);
    Ok(())
}

#[test]
fn production_create_commands_are_fixed_and_non_interactive() -> Result<(), Box<dyn Error>> {
    let bindings = ManagedGroupBindings::new(30_033, 30_000)?;
    let broker =
        AccountSpec::for_asset(account_asset("broker-user"), bindings).ok_or_else(command_error)?;
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
    let broker_group = AccountSpec::for_asset(account_asset("broker-group"), bindings)
        .ok_or_else(command_error)?;
    assert_eq!(
        create_command(
            broker_group,
            "/usr/sbin/groupadd",
            "/usr/sbin/useradd",
            "/usr/sbin/nologin",
        ),
        (
            "/usr/sbin/groupadd",
            vec![
                "--gid".to_owned(),
                "30033".to_owned(),
                "pkg-nix-broker".to_owned()
            ]
        )
    );
    Ok(())
}

#[test]
fn exact_existing_accounts_are_idempotent() -> Result<(), Box<dyn Error>> {
    let bindings = ManagedGroupBindings::new(30_000, 30_001)?;
    let state = Arc::new(Mutex::new(FakeState {
        groups: vec![group("root", 0, &[]), group(BROKER_NAME, 30_000, &[])],
        users: vec![
            user("root", 0, 0, "/root"),
            user(BROKER_NAME, 31_000, 30_000, BROKER_HOME),
        ],
        ..FakeState::default()
    }));
    let mut manager = fake_manager(bindings, Arc::clone(&state));
    for asset in ["broker-group", "broker-user"] {
        assert!(!manager.ensure_asset(account_asset(asset))?);
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
            .ensure_asset(account_asset("broker-user"))
            .map_err(LinuxAccountError::code),
        Err(LinuxAccountErrorCode::CommandFailure)
    );
    assert_eq!(
        manager
            .rollback_asset(account_asset("broker-user"))
            .map_err(LinuxAccountError::code),
        Err(LinuxAccountErrorCode::Conflict)
    );
    let state = state.lock().map_err(|_| command_error())?;
    assert!(state.deleted.is_empty());
    assert!(state.groups.iter().any(|group| group.name == BROKER_NAME));
    assert!(state.users.iter().any(|user| user.name == BROKER_NAME));
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
fn verified_uninstall_removes_only_exact_accounts_and_is_retry_safe() -> Result<(), Box<dyn Error>>
{
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
fn verified_uninstall_refuses_changed_accounts_without_deletion() -> Result<(), Box<dyn Error>> {
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

fn parse_groups_requires_exact_group_shadow_parity() {
    // A foreign group with any mismatch still fails closed.
    assert!(parse_groups(b"devs:x:1001:alice\n", b"devs:!::\n").is_err());
    // Gshadow ahead of the group file still fails closed.
    assert!(parse_groups(b"devs:x:1001:alice\n", b"devs:!::alice,bob\n").is_err());
}
