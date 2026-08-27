//! Production observation of build-host facts used by authenticated replanning.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Read,
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use pkg_channel::VerifiedChannel;
use pkg_core::System;
use pkg_nix::{BuildReadiness, observe_build_accounts, render_managed_build_nix_conf};

use crate::{BuildHostFacts, BuildHostFactsError, BuildHostFactsProbe};

const MANAGED_NIX_CONF: &str = "/opt/pkg/etc/pkg/nix.conf";
const LINUX_CGROUP_CONTROLLERS: &str = "/sys/fs/cgroup/cgroup.controllers";
const LINUX_DAEMON_CGROUP: &str =
    "/sys/fs/cgroup/system.slice/nix-daemon.service/nix-daemon/cgroup.procs";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CONFIG_ENTRIES: usize = 64;
const LINUX_BUILD_USERS: usize = 16;
const DARWIN_BUILD_USERS: usize = 32;
const MAX_CGROUP_PIDS: usize = 1_024;
const MAX_PROC_BYTES: u64 = 4_096;

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
    /// Binds the fixed-path observer to the exact authenticated managed config.
    pub fn from_verified_channel(channel: &VerifiedChannel) -> Result<Self, BuildHostFactsError> {
        let system = production_native_system()?;
        let expected_config = render_managed_build_nix_conf(system, channel.descriptor().cache())
            .map_err(|_| BuildHostFactsError)?;
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
        Ok(is_bounded_regular_file(Path::new(LINUX_CGROUP_CONTROLLERS))
            && daemon_cgroup_has_managed_daemon(Path::new(LINUX_DAEMON_CGROUP))?)
    }

    fn host_cores(&self) -> Result<u32, BuildHostFactsError> {
        std::thread::available_parallelism()
            .map_err(|_| BuildHostFactsError)
            .and_then(|cores| u32::try_from(cores.get()).map_err(|_| BuildHostFactsError))
    }
}

#[cfg(test)]
fn observe(source: &dyn HostSource, system: System) -> Result<BuildHostFacts, BuildHostFactsError> {
    observe_configured(source, system, source.managed_config()?)
}

fn observe_bound(
    source: &dyn HostSource,
    system: System,
    expected_config: &str,
) -> Result<BuildHostFacts, BuildHostFactsError> {
    let actual_config = source.managed_config()?;
    if actual_config != expected_config {
        return Err(BuildHostFactsError);
    }
    observe_configured(source, system, actual_config)
}

fn observe_configured(
    source: &dyn HostSource,
    system: System,
    managed_config: String,
) -> Result<BuildHostFacts, BuildHostFactsError> {
    let config = parse_config(&managed_config)?;
    let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
    validate_config(&config, linux)?;
    let expected_users = build_user_names(system);
    validate_build_users(&source.accounts(system)?, &expected_users)?;
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

fn validate_config(
    config: &BTreeMap<String, String>,
    linux: bool,
) -> Result<(), BuildHostFactsError> {
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
    let expected_features = if linux {
        BTreeSet::from(["nix-command", "flakes", "cgroups"])
    } else {
        BTreeSet::from(["nix-command", "flakes"])
    };
    if features != expected_features
        || linux != (config.get("use-cgroups").map(String::as_str) == Some("true"))
        || (!linux && config.contains_key("use-cgroups"))
    {
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
) -> Result<(), BuildHostFactsError> {
    let expected_set = expected.iter().cloned().collect::<BTreeSet<_>>();
    if directory.explicit_members != expected_set {
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
    let mut uids = BTreeSet::new();
    for account in primary_members {
        let uid_uses = directory
            .accounts
            .iter()
            .filter(|candidate| candidate.uid == account.uid)
            .count();
        if account.uid == 0
            || uid_uses != 1
            || !uids.insert(account.uid)
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

fn daemon_cgroup_has_managed_daemon(path: &Path) -> Result<bool, BuildHostFactsError> {
    let pids = read_bounded(path, MAX_CONFIG_BYTES)?;
    let text = std::str::from_utf8(&pids).map_err(|_| BuildHostFactsError)?;
    let mut count = 0_usize;
    for line in text.lines() {
        count = count.checked_add(1).ok_or(BuildHostFactsError)?;
        if count > MAX_CGROUP_PIDS {
            return Err(BuildHostFactsError);
        }
        let pid = line.parse::<u32>().map_err(|_| BuildHostFactsError)?;
        if pid == 0 {
            return Err(BuildHostFactsError);
        }
        if is_managed_daemon_process(pid).unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_managed_daemon_process(pid: u32) -> Result<bool, BuildHostFactsError> {
    let process = Path::new("/proc").join(pid.to_string());
    let command = read_bounded(&process.join("cmdline"), MAX_PROC_BYTES)?;
    Ok(is_managed_daemon_command(&command))
}

fn is_managed_daemon_command(command: &[u8]) -> bool {
    let mut arguments = command.split(|byte| *byte == 0);
    let Some(executable_argument) = arguments.next() else {
        return false;
    };
    if executable_argument == b"/run/rosetta/rosetta" {
        return arguments.next() == Some(&b"/opt/pkg/nix/current/bin/nix-daemon"[..])
            && arguments.next() == Some(&b"nix-daemon"[..])
            && arguments.next() == Some(&b"--daemon"[..])
            && arguments.next() == Some(&b""[..])
            && arguments.next().is_none();
    }
    !executable_argument.is_empty()
        && Path::new(OsStr::from_bytes(executable_argument)).file_name()
            == Some(OsStr::new("nix-daemon"))
        && arguments.next() == Some(&b"--daemon"[..])
        && arguments.next() == Some(&b""[..])
        && arguments.next().is_none()
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, BuildHostFactsError> {
    let metadata = path.symlink_metadata().map_err(|_| BuildHostFactsError)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(BuildHostFactsError);
    }
    let file = fs::File::open(path).map_err(|_| BuildHostFactsError)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BuildHostFactsError)?;
    if bytes.len() as u64 > max_bytes {
        return Err(BuildHostFactsError);
    }
    Ok(bytes)
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
pub(crate) const fn production_native_system() -> Result<System, BuildHostFactsError> {
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
    use super::*;

    struct FakeSource {
        config: String,
        accounts: ObservedAccountDirectory,
        cgroup_ready: bool,
        cores: u32,
    }

    impl HostSource for FakeSource {
        fn managed_config(&self) -> Result<String, BuildHostFactsError> {
            Ok(self.config.clone())
        }
        fn accounts(
            &self,
            _system: System,
        ) -> Result<ObservedAccountDirectory, BuildHostFactsError> {
            Ok(self.accounts.clone())
        }
        fn linux_cgroup_v2_ready(&self) -> Result<bool, BuildHostFactsError> {
            Ok(self.cgroup_ready)
        }
        fn host_cores(&self) -> Result<u32, BuildHostFactsError> {
            Ok(self.cores)
        }
    }

    fn source(system: System) -> FakeSource {
        let linux = matches!(system, System::X8664Linux | System::Aarch64Linux);
        let names = build_user_names(system);
        FakeSource {
            config: format!(
                "build-users-group = nixbld\ntrusted-users = root\nallowed-users = pkg-nix-broker\nexperimental-features = nix-command flakes{}\nsandbox = true\nsandbox-fallback = false\nallow-import-from-derivation = false\n{}require-sigs = true\nbuilders =\nmax-jobs = 1\n",
                if linux { " cgroups" } else { "" },
                if linux { "use-cgroups = true\n" } else { "" },
            ),
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
            cgroup_ready: linux,
            cores: 8,
        }
    }

    #[test]
    fn exact_linux_and_darwin_observations_are_accepted() {
        assert!(observe(&source(System::X8664Linux), System::X8664Linux).is_ok());
        assert!(observe(&source(System::Aarch64Darwin), System::Aarch64Darwin).is_ok());
    }

    #[test]
    fn managed_daemon_command_accepts_only_direct_or_exact_rosetta_execution() {
        assert!(is_managed_daemon_command(
            b"/opt/pkg/nix/current/bin/nix-daemon\0--daemon\0"
        ));
        assert!(is_managed_daemon_command(
            b"/run/rosetta/rosetta\0/opt/pkg/nix/current/bin/nix-daemon\0nix-daemon\0--daemon\0"
        ));
        assert!(!is_managed_daemon_command(
            b"/run/rosetta/rosetta\0/tmp/nix-daemon\0nix-daemon\0--daemon\0"
        ));
        assert!(!is_managed_daemon_command(
            b"/run/rosetta/rosetta\0/opt/pkg/nix/current/bin/nix-daemon\0nix-daemon\0--daemon\0--extra\0"
        ));
    }

    #[test]
    fn config_widening_and_missing_cgroup_refuse() {
        let mut bad_config = source(System::X8664Linux);
        bad_config.config = bad_config
            .config
            .replace("sandbox = true", "sandbox = false");
        assert!(observe(&bad_config, System::X8664Linux).is_err());

        let mut no_cgroup = source(System::X8664Linux);
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
    fn missing_or_unexpected_builder_members_refuse() {
        let mut missing = source(System::Aarch64Darwin);
        missing.accounts.accounts.pop();
        assert!(observe(&missing, System::Aarch64Darwin).is_err());

        let mut omitted_member = source(System::Aarch64Darwin);
        omitted_member.accounts.explicit_members.remove("_nixbld32");
        assert!(observe(&omitted_member, System::Aarch64Darwin).is_err());

        let mut unexpected = source(System::Aarch64Darwin);
        unexpected
            .accounts
            .explicit_members
            .insert("wheel-user".to_owned());
        assert!(observe(&unexpected, System::Aarch64Darwin).is_err());

        let mut root_builder = source(System::Aarch64Darwin);
        root_builder.accounts.accounts[0].uid = 0;
        assert!(observe(&root_builder, System::Aarch64Darwin).is_err());

        let mut aliased_builder = source(System::Aarch64Darwin);
        let aliased_uid = aliased_builder.accounts.accounts[0].uid;
        aliased_builder.accounts.accounts.push(ObservedAccount {
            name: "pkg-nix-broker".to_owned(),
            uid: aliased_uid,
            primary_gid: 30001,
            home: "/var/empty".to_owned(),
            shell: "/usr/sbin/nologin".to_owned(),
        });
        assert!(observe(&aliased_builder, System::Aarch64Darwin).is_err());

        let mut extra_primary_member = source(System::Aarch64Darwin);
        extra_primary_member
            .accounts
            .accounts
            .push(ObservedAccount {
                name: "unexpected".to_owned(),
                uid: 40000,
                primary_gid: 30000,
                home: "/var/empty".to_owned(),
                shell: "/usr/sbin/nologin".to_owned(),
            });
        assert!(observe(&extra_primary_member, System::Aarch64Darwin).is_err());

        let mut login_builder = source(System::Aarch64Darwin);
        login_builder.accounts.accounts[0].shell = "/bin/sh".to_owned();
        assert!(observe(&login_builder, System::Aarch64Darwin).is_err());
    }
}
