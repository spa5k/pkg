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
pub const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::X8664Linux)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Ok(System::Aarch64Linux)
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const fn production_native_system() -> Result<System, BuildHostFactsError> {
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
pub const fn production_native_system() -> Result<System, BuildHostFactsError> {
    Err(BuildHostFactsError)
}

#[cfg(test)]
mod tests;
