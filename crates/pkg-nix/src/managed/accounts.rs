//! Closed, bounded observation of the host account directory used by Nix builders.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use pkg_core::System;

const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// One account returned by the trusted platform directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildAccount {
    name: String,
    uid: u32,
    primary_gid: u32,
    home: String,
    shell: String,
}

impl BuildAccount {
    /// Account name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Numeric user identity.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// Numeric primary group identity.
    #[must_use]
    pub const fn primary_gid(&self) -> u32 {
        self.primary_gid
    }

    /// Configured home directory.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Configured login shell.
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.shell
    }
}

/// Bounded account-directory evidence needed for build-user readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildAccountDirectory {
    group_gid: u32,
    explicit_members: BTreeSet<String>,
    accounts: Vec<BuildAccount>,
}

impl BuildAccountDirectory {
    /// Numeric identity of the `nixbld` group.
    #[must_use]
    pub const fn group_gid(&self) -> u32 {
        self.group_gid
    }

    /// Explicit member names recorded on the `nixbld` group.
    #[must_use]
    pub const fn explicit_members(&self) -> &BTreeSet<String> {
        &self.explicit_members
    }

    /// All accounts returned by the platform directory.
    #[must_use]
    pub fn accounts(&self) -> &[BuildAccount] {
        &self.accounts
    }
}

/// Redacted account-directory observation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildAccountError;

impl fmt::Display for BuildAccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("managed build account observation failed")
    }
}

impl Error for BuildAccountError {}

/// Enumerates the platform's configured system-search account view through
/// fixed absolute tools.
///
/// # Errors
///
/// Fails closed when a command is absent, exceeds its time/output bound, exits
/// unsuccessfully, or returns malformed/duplicate account data.
pub fn observe_build_accounts(system: System) -> Result<BuildAccountDirectory, BuildAccountError> {
    match system {
        System::X8664Linux | System::Aarch64Linux => observe_linux(),
        System::X8664Darwin | System::Aarch64Darwin => observe_darwin(),
    }
}

fn observe_linux() -> Result<BuildAccountDirectory, BuildAccountError> {
    let passwd = run_bounded(Path::new("/usr/bin/getent"), &["passwd"])?;
    let group = run_bounded(Path::new("/usr/bin/getent"), &["group", "nixbld"])?;
    let accounts = parse_linux_passwd(&passwd)?;
    let (group_gid, explicit_members) = parse_linux_group(&group)?;
    Ok(BuildAccountDirectory {
        group_gid,
        explicit_members,
        accounts,
    })
}

fn observe_darwin() -> Result<BuildAccountDirectory, BuildAccountError> {
    let uids = run_bounded(
        Path::new("/usr/bin/dscl"),
        &["/Search", "-list", "/Users", "UniqueID"],
    )?;
    let gids = run_bounded(
        Path::new("/usr/bin/dscl"),
        &["/Search", "-list", "/Users", "PrimaryGroupID"],
    )?;
    let homes = run_bounded(
        Path::new("/usr/bin/dscl"),
        &["/Search", "-list", "/Users", "NFSHomeDirectory"],
    )?;
    let shells = run_bounded(
        Path::new("/usr/bin/dscl"),
        &["/Search", "-list", "/Users", "UserShell"],
    )?;
    let group = run_bounded(
        Path::new("/usr/bin/dscl"),
        &[
            "/Search",
            "-read",
            "/Groups/nixbld",
            "PrimaryGroupID",
            "GroupMembership",
        ],
    )?;
    let accounts = parse_darwin_accounts(&uids, &gids, &homes, &shells)?;
    let (group_gid, explicit_members) = parse_darwin_group(&group)?;
    Ok(BuildAccountDirectory {
        group_gid,
        explicit_members,
        accounts,
    })
}

fn parse_linux_passwd(bytes: &[u8]) -> Result<Vec<BuildAccount>, BuildAccountError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BuildAccountError)?;
    let mut accounts = Vec::new();
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 7 || fields[0].is_empty() || !names.insert(fields[0].to_owned()) {
            return Err(BuildAccountError);
        }
        accounts.push(BuildAccount {
            name: fields[0].to_owned(),
            uid: fields[2].parse().map_err(|_| BuildAccountError)?,
            primary_gid: fields[3].parse().map_err(|_| BuildAccountError)?,
            home: fields[5].to_owned(),
            shell: fields[6].to_owned(),
        });
    }
    if accounts.is_empty() {
        return Err(BuildAccountError);
    }
    Ok(accounts)
}

fn parse_linux_group(bytes: &[u8]) -> Result<(u32, BTreeSet<String>), BuildAccountError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BuildAccountError)?;
    let mut lines = text.lines();
    let fields = lines
        .next()
        .ok_or(BuildAccountError)?
        .split(':')
        .collect::<Vec<_>>();
    if fields.len() != 4 || fields[0] != "nixbld" || lines.next().is_some() {
        return Err(BuildAccountError);
    }
    let gid = fields[2].parse().map_err(|_| BuildAccountError)?;
    let members = parse_member_list(fields[3].split(',').filter(|member| !member.is_empty()))?;
    Ok((gid, members))
}

fn parse_darwin_accounts(
    uid_bytes: &[u8],
    gid_bytes: &[u8],
    home_bytes: &[u8],
    shell_bytes: &[u8],
) -> Result<Vec<BuildAccount>, BuildAccountError> {
    let uids = parse_dscl_pairs(uid_bytes)?;
    let gids = parse_dscl_pairs(gid_bytes)?;
    let homes = parse_dscl_text_pairs(home_bytes)?;
    let shells = parse_dscl_text_pairs(shell_bytes)?;
    if uids.keys().ne(gids.keys()) || uids.keys().ne(homes.keys()) || uids.keys().ne(shells.keys())
    {
        return Err(BuildAccountError);
    }
    uids.into_iter()
        .map(|(name, uid)| {
            let primary_gid = gids.get(&name).copied().ok_or(BuildAccountError)?;
            let home = homes.get(&name).cloned().ok_or(BuildAccountError)?;
            let shell = shells.get(&name).cloned().ok_or(BuildAccountError)?;
            Ok(BuildAccount {
                name,
                uid,
                primary_gid,
                home,
                shell,
            })
        })
        .collect()
}

fn parse_dscl_text_pairs(bytes: &[u8]) -> Result<BTreeMap<String, String>, BuildAccountError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BuildAccountError)?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once(char::is_whitespace)
            .ok_or(BuildAccountError)?;
        let value = value.trim();
        if name.is_empty()
            || value.is_empty()
            || values.insert(name.to_owned(), value.to_owned()).is_some()
        {
            return Err(BuildAccountError);
        }
    }
    if values.is_empty() {
        return Err(BuildAccountError);
    }
    Ok(values)
}

fn parse_dscl_pairs(bytes: &[u8]) -> Result<BTreeMap<String, u32>, BuildAccountError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BuildAccountError)?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let name = fields.next().ok_or(BuildAccountError)?;
        let value = fields.next().ok_or(BuildAccountError)?;
        let value = value
            .parse::<u32>()
            .or_else(|_| value.parse::<i32>().map(i32::cast_unsigned))
            .map_err(|_| BuildAccountError)?;
        if fields.next().is_some() || values.insert(name.to_owned(), value).is_some() {
            return Err(BuildAccountError);
        }
    }
    if values.is_empty() {
        return Err(BuildAccountError);
    }
    Ok(values)
}

fn parse_darwin_group(bytes: &[u8]) -> Result<(u32, BTreeSet<String>), BuildAccountError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BuildAccountError)?;
    let mut gid = None;
    let mut members = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("PrimaryGroupID:") {
            if gid
                .replace(value.trim().parse().map_err(|_| BuildAccountError)?)
                .is_some()
            {
                return Err(BuildAccountError);
            }
        } else if let Some(value) = line.strip_prefix("GroupMembership:") {
            if members
                .replace(parse_member_list(value.split_whitespace())?)
                .is_some()
            {
                return Err(BuildAccountError);
            }
        } else if !line.trim().is_empty() {
            return Err(BuildAccountError);
        }
    }
    Ok((
        gid.ok_or(BuildAccountError)?,
        members.ok_or(BuildAccountError)?,
    ))
}

fn parse_member_list<'a>(
    members: impl Iterator<Item = &'a str>,
) -> Result<BTreeSet<String>, BuildAccountError> {
    let mut result = BTreeSet::new();
    for member in members {
        if member.is_empty() || !result.insert(member.to_owned()) {
            return Err(BuildAccountError);
        }
    }
    Ok(result)
}

fn run_bounded(program: &Path, arguments: &[&str]) -> Result<Vec<u8>, BuildAccountError> {
    if !program.is_absolute() || !program.exists() {
        return Err(BuildAccountError);
    }
    let mut child = Command::new(program)
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| BuildAccountError)?;
    let stdout = child.stdout.take().ok_or(BuildAccountError)?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes);
        (result, bytes)
    });
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (read_result, bytes) = reader.join().map_err(|_| BuildAccountError)?;
    if !status.is_some_and(|status| status.success())
        || read_result.is_err()
        || bytes.len() as u64 > MAX_OUTPUT_BYTES
    {
        return Err(BuildAccountError);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_directory_parsers_retain_uid_gid_and_explicit_members() {
        let accounts = parse_linux_passwd(
            b"root:x:0:0:root:/root:/bin/sh\nnixbld1:x:30001:30000::/var/empty:/bin/false\n",
        )
        .unwrap();
        assert_eq!(accounts[1].uid(), 30001);
        assert_eq!(accounts[1].primary_gid(), 30000);
        assert_eq!(accounts[1].home(), "/var/empty");
        assert_eq!(accounts[1].shell(), "/bin/false");
        let (gid, members) = parse_linux_group(b"nixbld:x:30000:nixbld1\n").unwrap();
        assert_eq!(gid, 30000);
        assert_eq!(members, BTreeSet::from(["nixbld1".to_owned()]));
    }

    #[test]
    fn darwin_directory_parsers_join_accounts_and_group_evidence() {
        let accounts = parse_darwin_accounts(
            b"_nixbld1 301\nroot 0\n",
            b"_nixbld1 300\nroot 0\n",
            b"_nixbld1 /var/empty\nroot /var/root\n",
            b"_nixbld1 /usr/bin/false\nroot /bin/sh\n",
        )
        .unwrap();
        assert_eq!(accounts[0].name(), "_nixbld1");
        let (gid, members) =
            parse_darwin_group(b"PrimaryGroupID: 300\nGroupMembership: _nixbld1\n").unwrap();
        assert_eq!(gid, 300);
        assert!(members.contains("_nixbld1"));
    }

    #[test]
    fn malformed_duplicate_and_mismatched_directories_refuse() {
        assert_eq!(
            parse_dscl_pairs(b"nobody -2\n").unwrap()["nobody"],
            u32::MAX - 1
        );
        assert!(parse_linux_passwd(b"nixbld1:x:1:2\n").is_err());
        assert!(parse_linux_group(b"wheel:x:0:root\n").is_err());
        assert!(
            parse_darwin_accounts(b"a 1\n", b"b 2\n", b"a /var/empty\n", b"a /bin/false\n")
                .is_err()
        );
        assert!(parse_darwin_group(b"PrimaryGroupID: 300\n").is_err());
    }
}
