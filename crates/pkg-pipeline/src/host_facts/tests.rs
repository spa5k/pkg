//! Tests for the `host_facts` module.

use std::cell::Cell;

use super::*;

struct FakeSource {
    config: String,
    accounts: ObservedAccountDirectory,
    cgroup_ready: bool,
    cores: u32,
    config_calls: Cell<u32>,
    cgroup_calls: Cell<u32>,
}

impl HostSource for FakeSource {
    fn managed_config(&self) -> Result<String, BuildHostFactsError> {
        self.config_calls.set(self.config_calls.get() + 1);
        Ok(self.config.clone())
    }
    fn accounts(&self, _system: System) -> Result<ObservedAccountDirectory, BuildHostFactsError> {
        Ok(self.accounts.clone())
    }
    fn linux_cgroup_v2_ready(&self) -> Result<bool, BuildHostFactsError> {
        self.cgroup_calls.set(self.cgroup_calls.get() + 1);
        Ok(self.cgroup_ready)
    }
    fn host_cores(&self) -> Result<u32, BuildHostFactsError> {
        Ok(self.cores)
    }
}

fn source(system: System) -> FakeSource {
    let names = build_user_names(system);
    FakeSource {
            config: "build-users-group = nixbld\ntrusted-users = root\nallowed-users = pkg-nix-broker\nexperimental-features = nix-command flakes\nsandbox = true\nsandbox-fallback = false\nallow-import-from-derivation = false\nrequire-sigs = true\nbuilders =\nmax-jobs = 1\n".to_owned(),
            accounts: ObservedAccountDirectory {
                group_gid: 30000,
                explicit_members: names.iter().cloned().collect(),
                accounts: names
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| ObservedAccount {
                        name,
                        uid: 30001 + u32::try_from(index).unwrap_or_default(),
                        primary_gid: 30000,
                        home: "/var/empty".to_owned(),
                        shell: "/usr/sbin/nologin".to_owned(),
                    })
                    .collect(),
            },
            cgroup_ready: matches!(system, System::X8664Linux | System::Aarch64Linux),
            cores: 8,
            config_calls: Cell::new(0),
            cgroup_calls: Cell::new(0),
        }
}

#[test]
fn exact_linux_and_darwin_observations_are_accepted() {
    let linux = source(System::X8664Linux);
    assert_eq!(linux.accounts.explicit_members.len(), 32);
    assert!(observe(&linux, System::X8664Linux).is_ok());

    let darwin = source(System::Aarch64Darwin);
    assert!(observe(&darwin, System::Aarch64Darwin).is_ok());
    assert_eq!(darwin.cgroup_calls.get(), 0);
}

#[test]
fn linux_ignores_managed_config_and_requires_cgroup_availability() {
    let mut source = source(System::X8664Linux);
    source.config = "malformed and irrelevant".to_owned();
    assert!(observe(&source, System::X8664Linux).is_ok());
    assert_eq!(source.config_calls.get(), 0);
    assert_eq!(source.cgroup_calls.get(), 1);

    let mut no_cgroup = source;
    no_cgroup.cgroup_ready = false;
    assert!(observe(&no_cgroup, System::X8664Linux).is_err());
}

#[test]
fn authenticated_config_binding_refuses_any_byte_change() {
    let source = source(System::Aarch64Darwin);
    let expected = source.config.clone();
    assert!(observe_bound(&source, System::Aarch64Darwin, &expected).is_ok());
    assert!(
        observe_bound(
            &source,
            System::Aarch64Darwin,
            &(expected + "connect-timeout = 11\n"),
        )
        .is_err()
    );
}

#[test]
fn darwin_config_and_max_jobs_remain_exact_without_cgroup_observation() {
    let mut bad_config = source(System::Aarch64Darwin);
    bad_config.config = bad_config
        .config
        .replace("sandbox = true", "sandbox = false");
    assert!(observe(&bad_config, System::Aarch64Darwin).is_err());
    assert_eq!(bad_config.cgroup_calls.get(), 0);

    let mut bad_jobs = source(System::Aarch64Darwin);
    bad_jobs.config = bad_jobs.config.replace("max-jobs = 1", "max-jobs = 2");
    assert!(observe(&bad_jobs, System::Aarch64Darwin).is_err());
    assert_eq!(bad_jobs.cgroup_calls.get(), 0);
}

#[test]
fn linux_builder_count_membership_and_account_identity_are_exact() {
    let mut missing = source(System::X8664Linux);
    missing.accounts.accounts.pop();
    assert!(observe(&missing, System::X8664Linux).is_err());

    let mut extra = source(System::X8664Linux);
    extra
        .accounts
        .explicit_members
        .insert("nixbld33".to_owned());
    extra.accounts.accounts.push(ObservedAccount {
        name: "nixbld33".to_owned(),
        uid: 30033,
        primary_gid: 30000,
        home: "/var/empty".to_owned(),
        shell: "/usr/sbin/nologin".to_owned(),
    });
    assert!(observe(&extra, System::X8664Linux).is_err());

    let mut omitted_member = source(System::X8664Linux);
    omitted_member.accounts.explicit_members.remove("nixbld32");
    assert!(observe(&omitted_member, System::X8664Linux).is_err());

    let mut duplicate_uid = source(System::X8664Linux);
    duplicate_uid.accounts.accounts[1].uid = duplicate_uid.accounts.accounts[0].uid;
    assert!(observe(&duplicate_uid, System::X8664Linux).is_err());

    let mut wrong_uid = source(System::X8664Linux);
    wrong_uid.accounts.accounts[0].uid = 40001;
    assert!(observe(&wrong_uid, System::X8664Linux).is_err());

    let mut wrong_gid = source(System::X8664Linux);
    wrong_gid.accounts.group_gid = 40000;
    for account in &mut wrong_gid.accounts.accounts {
        account.primary_gid = 40000;
    }
    assert!(observe(&wrong_gid, System::X8664Linux).is_err());

    let mut root_builder = source(System::X8664Linux);
    root_builder.accounts.accounts[0].uid = 0;
    assert!(observe(&root_builder, System::X8664Linux).is_err());

    let mut bad_home = source(System::X8664Linux);
    bad_home.accounts.accounts[0].home = "/tmp".to_owned();
    assert!(observe(&bad_home, System::X8664Linux).is_err());

    let mut login_builder = source(System::X8664Linux);
    login_builder.accounts.accounts[0].shell = "/bin/sh".to_owned();
    assert!(observe(&login_builder, System::X8664Linux).is_err());
}
