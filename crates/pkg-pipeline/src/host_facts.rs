//! Production observation of build-host facts used by authenticated replanning.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use pkg_channel::VerifiedChannel;
use pkg_core::System;
use pkg_nix::{BuildReadiness, observe_build_accounts, render_managed_build_nix_conf};

use crate::{BuildHostFacts, BuildHostFactsError, BuildHostFactsProbe};

const MANAGED_NIX_CONF: &str = "/opt/pkg/etc/pkg/nix.conf";
const LINUX_CGROUP_CONTROLLERS: &str = "/sys/fs/cgroup/cgroup.controllers";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CONFIG_ENTRIES: usize = 64;
const LINUX_BUILD_USERS: usize = 32;
const LINUX_BUILD_GID: u32 = 30_000;
const LINUX_BUILD_UID_BASE: u32 = 30_000;
const DARWIN_BUILD_USERS: usize = 32;

/// Fixed-path, fail-closed observer used by the production broker.
#[derive(Clone)]
pub struct ProductionBuildHostFactsProbe {
    system: System,
    expected_config: String,
}

impl std::fmt::Debug for ProductionBuildHostFactsProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionBuildHostFactsProbe")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl ProductionBuildHostFactsProbe {
    /// Binds the fixed-path observer to the authenticated platform host contract.
    pub fn from_verified_channel(channel: &VerifiedChannel) -> Result<Self, BuildHostFactsError> {
        let system = production_native_system()?;
        let expected_config = if matches!(system, System::X8664Linux | System::Aarch64Linux) {
            String::new()
        } else {
            render_managed_build_nix_conf(system, channel.descriptor().cache())
                .map_err(|_| BuildHostFactsError)?
        };
        Ok(Self {
            system,
            expected_config,
        })
    }

    /// Returns the compile-time native system bound into this trusted probe.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }
}

impl BuildHostFactsProbe for ProductionBuildHostFactsProbe {
    fn observe(&self) -> Result<BuildHostFacts, BuildHostFactsError> {
        let source = ProductionHostSource;
        observe_bound(&source, self.system, &self.expected_config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedAccount {
    name: String,
    uid: u32,
    primary_gid: u32,
    home: String,
    shell: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedAccountDirectory {
    group_gid: u32,
    explicit_members: BTreeSet<String>,
    accounts: Vec<ObservedAccount>,
}

trait HostSource {
    fn managed_config(&self) -> Result<String, BuildHostFactsError>;
    fn accounts(&self, system: System) -> Result<ObservedAccountDirectory, BuildHostFactsError>;
    fn linux_cgroup_v2_ready(&self) -> Result<bool, BuildHostFactsError>;
    fn host_cores(&self) -> Result<u32, BuildHostFactsError>;
}

#[derive(Debug, Clone, Copy)]
struct ProductionHostSource;

impl HostSource for ProductionHostSource {
    fn managed_config(&self) -> Result<String, BuildHostFactsError> {
        let path = Path::new(MANAGED_NIX_CONF);
        ensure_safe_ancestors(path.parent().ok_or(BuildHostFactsError)?)?;
        let metadata = path.symlink_metadata().map_err(|_| BuildHostFactsError)?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o777 != 0o640
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return Err(BuildHostFactsError);
        }
        fs::read_to_string(path).map_err(|_| BuildHostFactsError)
    }

    fn accounts(&self, system: System) -> Result<ObservedAccountDirectory, BuildHostFactsError> {
        let directory = observe_build_accounts(system).map_err(|_| BuildHostFactsError)?;
        Ok(ObservedAccountDirectory {
            group_gid: directory.group_gid(),
            explicit_members: directory.explicit_members().clone(),
            accounts: directory
                .accounts()
                .iter()
                .map(|account| ObservedAccount {
                    name: account.name().to_owned(),
                    uid: account.uid(),
                    primary_gid: account.primary_gid(),
                    home: account.home().to_owned(),
                    shell: account.shell().to_owned(),
                })
                .collect(),
        })
    }

    fn linux_cgroup_v2_ready(&self) -> Result<bool, BuildHostFactsError> {
        Ok(is_bounded_regular_file(Path::new(LINUX_CGROUP_CONTROLLERS)))
    }

    fn host_cores(&self) -> Result<u32, BuildHostFactsError> {
        std::thread::available_parallelism()
            .map_err(|_| BuildHostFactsError)
            .and_then(|cores| u32::try_from(cores.get()).map_err(|_| BuildHostFactsError))
    }
}

#[cfg(test)]
fn observe(source: &dyn HostSource, system: System) -> Result<BuildHostFacts, BuildHostFactsError> {
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        observe_bound(source, system, "")
    } else {
        observe_bound(source, system, &source.managed_config()?)
    }
}

fn observe_bound(
    source: &dyn HostSource,
    system: System,
    expected_config: &str,
) -> Result<BuildHostFacts, BuildHostFactsError> {
    let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
    if !linux {
        let actual_config = source.managed_config()?;
        if actual_config != expected_config {
            return Err(BuildHostFactsError);
        }
        validate_config(&parse_config(&actual_config)?)?;
    }
    let expected_users = build_user_names(system);
    validate_build_users(&source.accounts(system)?, &expected_users, linux)?;
    let cgroup_v2_ready = if linux {
        source.linux_cgroup_v2_ready()?
    } else {
        false
    };
    if linux && !cgroup_v2_ready {
        return Err(BuildHostFactsError);
    }
    BuildHostFacts::new(
        system,
        BuildReadiness::new(true, false, true, linux, cgroup_v2_ready),
        source.host_cores()?,
    )
}

fn parse_config(config: &str) -> Result<BTreeMap<String, String>, BuildHostFactsError> {
    if config.len() as u64 > MAX_CONFIG_BYTES {
        return Err(BuildHostFactsError);
    }
    let mut entries = BTreeMap::new();
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(BuildHostFactsError)?;
        let key = key.trim();
        if key.is_empty()
            || entries.len() >= MAX_CONFIG_ENTRIES
            || entries
                .insert(key.to_owned(), value.trim().to_owned())
                .is_some()
        {
            return Err(BuildHostFactsError);
        }
    }
    Ok(entries)
}

fn validate_config(config: &BTreeMap<String, String>) -> Result<(), BuildHostFactsError> {
    for (key, expected) in [
        ("build-users-group", "nixbld"),
        ("trusted-users", "root"),
        ("allowed-users", "pkg-nix-broker"),
        ("sandbox", "true"),
        ("sandbox-fallback", "false"),
        ("allow-import-from-derivation", "false"),
        ("require-sigs", "true"),
        ("builders", ""),
        ("max-jobs", "1"),
    ] {
        if config.get(key).map(String::as_str) != Some(expected) {
            return Err(BuildHostFactsError);
        }
    }
    let features = config
        .get("experimental-features")
        .ok_or(BuildHostFactsError)?
        .split_whitespace()
        .collect::<BTreeSet<_>>();
    if features != BTreeSet::from(["nix-command", "flakes"]) || config.contains_key("use-cgroups") {
        return Err(BuildHostFactsError);
    }
    Ok(())
}

fn build_user_names(system: System) -> Vec<String> {
    let (prefix, count) = match system {
        System::X8664Linux | System::Aarch64Linux => ("nixbld", LINUX_BUILD_USERS),
        System::X8664Darwin | System::Aarch64Darwin => ("_nixbld", DARWIN_BUILD_USERS),
    };
    (1..=count)
        .map(|index| format!("{prefix}{index}"))
        .collect()
}

fn validate_build_users(
    directory: &ObservedAccountDirectory,
    expected: &[String],
    linux: bool,
) -> Result<(), BuildHostFactsError> {
    let expected_set = expected.iter().cloned().collect::<BTreeSet<_>>();
    if directory.explicit_members != expected_set
        || (linux && directory.group_gid != LINUX_BUILD_GID)
    {
        return Err(BuildHostFactsError);
    }
    let primary_members = directory
        .accounts
        .iter()
        .filter(|account| account.primary_gid == directory.group_gid)
        .collect::<Vec<_>>();
    if primary_members.len() != expected.len()
        || primary_members
            .iter()
            .map(|account| account.name.as_str())
            .collect::<BTreeSet<_>>()
            != expected.iter().map(String::as_str).collect::<BTreeSet<_>>()
    {
        return Err(BuildHostFactsError);
    }
    for account in primary_members {
        let uid_uses = directory
            .accounts
            .iter()
            .filter(|candidate| candidate.uid == account.uid)
            .count();
        let wrong_linux_uid = linux
            && account
                .name
                .strip_prefix("nixbld")
                .and_then(|index| index.parse::<u32>().ok())
                .and_then(|index| LINUX_BUILD_UID_BASE.checked_add(index))
                != Some(account.uid);
        if account.uid == 0
            || uid_uses != 1
            || wrong_linux_uid
            || account.home != "/var/empty"
            || !matches!(
                account.shell.as_str(),
                "/bin/false" | "/usr/bin/false" | "/sbin/nologin" | "/usr/sbin/nologin"
            )
        {
            return Err(BuildHostFactsError);
        }
    }
    Ok(())
}

fn is_bounded_regular_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= MAX_CONFIG_BYTES
    })
}

fn ensure_safe_ancestors(path: &Path) -> Result<(), BuildHostFactsError> {
    for ancestor in path.ancestors() {
        let metadata = ancestor
            .symlink_metadata()
            .map_err(|_| BuildHostFactsError)?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(BuildHostFactsError);
        }
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::X8664Linux)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::Aarch64Linux)
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::X8664Darwin)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::Aarch64Darwin)
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
)))]
pub(crate) const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Err(BuildHostFactsError)
}

#[cfg(test)]
mod tests {
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
        fn accounts(
            &self,
            _system: System,
        ) -> Result<ObservedAccountDirectory, BuildHostFactsError> {
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
}
