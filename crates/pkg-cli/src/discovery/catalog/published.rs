//! Package lists that the NixOS channels publish.
//!
//! Each channel release has a `git-revision` file and a `packages.json.br`
//! file with the metadata of every package. The NixOS build service writes
//! the list on `x86_64-linux`. Reading it takes seconds. A local evaluation
//! of the same data takes minutes.

use std::io::BufReader;
use std::path::Path;

use rusqlite::{Connection, Statement, params};
use serde::de::{DeserializeSeed, Deserializer as _, IgnoredAny, MapAccess, Visitor};

use crate::nix::{Outcome, SourceIdentity};

use super::{CatalogError, native::Snapshot};

const CHANNELS: &str = "https://channels.nixos.org";

/// The system that the NixOS build service writes the lists on.
const LIST_SYSTEM: &str = "x86_64-linux";

/// The reason code for a package that the list builds only for other
/// systems. The list comes from an evaluation on another system, so Nix
/// makes the final check at install.
pub const OTHER_SYSTEMS: &str = "other-systems";

/// The NixOS channel of `source`, such as `nixpkgs-unstable` for
/// `github:NixOS/nixpkgs/nixpkgs-unstable`.
#[must_use]
pub fn channel(source: &str) -> Option<&str> {
    let mut parts = source.strip_prefix("github:")?.split('/');
    let (owner, repository, branch) = (parts.next()?, parts.next()?, parts.next()?);
    let channel = owner.eq_ignore_ascii_case("nixos")
        && repository.eq_ignore_ascii_case("nixpkgs")
        && parts.next().is_none()
        && (branch.starts_with("nixos-") || branch.starts_with("nixpkgs-"))
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    channel.then_some(branch)
}

/// Prepare search data from the package list of `channel`.
///
/// Return `false` when the channel publishes no list. Then the caller
/// evaluates the source on this computer.
/// # Errors
///
/// Returns an error when the download fails or is stopped, the list is not
/// valid, or the cache cannot be written.
pub(super) fn prepare(
    cache_dir: &Path,
    source: &str,
    system: &str,
    channel: &str,
) -> Result<bool, CatalogError> {
    std::fs::create_dir_all(cache_dir).map_err(CatalogError::cache_write)?;
    let scratch = tempfile::Builder::new()
        .prefix(".pkg-list-")
        .tempdir_in(cache_dir)
        .map_err(CatalogError::cache_write)?;
    // The channel URL redirects to one release. The list must come from
    // the release that names the revision.
    let revision_file = scratch.path().join("git-revision");
    let Some(release) = fetch(
        &format!("{CHANNELS}/{channel}/git-revision"),
        &revision_file,
        60,
    )?
    else {
        return Ok(false);
    };
    let revision = std::fs::read_to_string(&revision_file).map_err(CatalogError::cache_write)?;
    let revision = revision.trim();
    let directory = release.strip_suffix("/git-revision");
    let (Some(directory), true) = (
        directory,
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
    ) else {
        return Err(CatalogError::Source(format!(
            "the {channel} channel names no valid revision"
        )));
    };
    let identity = identity(source, revision);
    if Snapshot::prepared(cache_dir, source, system, &identity).is_none() {
        crate::output::status("Downloading search data… Nixpkgs");
        let list = scratch.path().join("packages.json.br");
        if fetch(&format!("{directory}/packages.json.br"), &list, 300)?.is_none() {
            return Ok(false);
        }
        crate::output::status("Reading search data… Nixpkgs");
        Snapshot::from_list(cache_dir, source, system, &identity, |connection| {
            read(&list, connection, system)
        })?;
    }
    super::local::mark_checked(cache_dir, source, system, &identity)?;
    Ok(true)
}

/// The identity of `source` at `revision`, in the locked form that Nix
/// reports for a GitHub source.
fn identity(source: &str, revision: &str) -> SourceIdentity {
    let base = source.rsplit_once('/').map_or(source, |(base, _)| base);
    let locked = format!("{base}/{revision}");
    SourceIdentity {
        reference: source.to_string(),
        display: super::source_display(&locked),
        locked_url: Some(locked),
        revision: Some(revision.to_string()),
    }
}

/// Download `url` to `destination`. Return the URL after redirects, or
/// `None` when the server has no such file.
fn fetch(url: &str, destination: &Path, seconds: u32) -> Result<Option<String>, CatalogError> {
    let destination = destination
        .to_str()
        .ok_or_else(|| CatalogError::cache_write("cache paths must be UTF-8"))?;
    let seconds = seconds.to_string();
    let (outcome, stdout) = crate::nix::run_direct_captured(
        Path::new("curl"),
        &[
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-sSL",
            "--max-time",
            &seconds,
            "-o",
            destination,
            "-w",
            "%{http_code} %{url_effective}",
            url,
        ],
    )
    .map_err(|error| match error.cancelled {
        Some(signal) => CatalogError::Interrupted(signal),
        None => CatalogError::Source(format!("cannot start curl: {error}")),
    })?;
    match outcome {
        Outcome::Success => {}
        Outcome::Interrupted { signal } => return Err(CatalogError::Interrupted(signal)),
        Outcome::Failed { stderr, .. } => {
            return Err(CatalogError::Source(format!(
                "unable to download {url}: {}",
                stderr.trim()
            )));
        }
    }
    let (code, location) = stdout
        .trim()
        .split_once(' ')
        .unwrap_or_else(|| (stdout.trim(), url));
    match code {
        "200" => Ok(Some(location.to_string())),
        "403" | "404" => Ok(None),
        code => Err(CatalogError::Source(format!(
            "unable to download {url}: HTTP error {code}"
        ))),
    }
}

/// Read a Brotli-compressed package list into the snapshot tables.
fn read(list: &Path, connection: &mut Connection, system: &str) -> Result<(), CatalogError> {
    let file = std::fs::File::open(list).map_err(CatalogError::cache_write)?;
    let decoder = brotli_decompressor::Decompressor::new(file, 1 << 16);
    let mut deserializer =
        serde_json::Deserializer::from_reader(BufReader::with_capacity(1 << 16, decoder));
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction
            .prepare("INSERT OR REPLACE INTO packages VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")?;
        let mut rows = Rows {
            insert: &mut insert,
            system,
            version: None,
        };
        (&mut deserializer)
            .deserialize_map(ListVisitor(&mut rows))
            .and_then(|()| deserializer.end())
            .map_err(|error| {
                CatalogError::Source(format!("the package list is not valid: {error}"))
            })?;
        if rows.version != Some(2) {
            return Err(CatalogError::Source(String::from(
                "the package list has an unknown format version",
            )));
        }
    }
    Ok(transaction.commit()?)
}

struct Rows<'a, 'b> {
    insert: &'a mut Statement<'b>,
    system: &'a str,
    version: Option<u64>,
}

impl Rows<'_, '_> {
    fn insert(&mut self, key: &str, entry: &Entry) -> rusqlite::Result<()> {
        let attribute = format!("legacyPackages.{}.{key}", self.system);
        let top_level = !key.contains('.');
        let printable = crate::output::printable;
        self.insert.execute(params![
            attribute,
            key,
            printable(&entry.pname),
            printable(&entry.version),
            printable(entry.meta.description.as_deref().unwrap_or_default()),
            // Main commands come from top-level packages only, as in a
            // local evaluation.
            entry.meta.main_program.as_deref().filter(|_| top_level),
            key.rsplit('.').next().unwrap_or(key),
            reason(entry, self.system),
        ])?;
        Ok(())
    }
}

/// Why a package cannot install on `system`, in the order of a local
/// evaluation. The list states the platform support and the `broken` flag
/// exactly only for its own system. That includes the 32-bit packages that
/// it builds for `i686-linux`, because they install on `x86_64-linux` too.
/// On other systems, Nix decides whether a package is broken.
fn reason(entry: &Entry, system: &str) -> Option<&'static str> {
    let meta = &entry.meta;
    let exact = system == LIST_SYSTEM;
    if exact {
        if meta.unsupported == Some(true) {
            return Some("unsupported-platform");
        }
    } else {
        let listed = meta
            .platforms
            .as_ref()
            .is_none_or(|platforms| platforms.iter().any(|platform| platform.is(system)));
        if !listed
            || meta
                .bad_platforms
                .iter()
                .any(|platform| platform.is(system))
        {
            return Some(OTHER_SYSTEMS);
        }
    }
    [
        (meta.unfree, "unfree"),
        (meta.broken.filter(|_| exact), "broken"),
        (meta.insecure, "insecure"),
    ]
    .into_iter()
    .find_map(|(flag, code)| (flag == Some(true)).then_some(code))
}

#[derive(serde::Deserialize)]
struct Entry {
    pname: String,
    version: String,
    #[serde(default)]
    meta: Meta,
}

#[derive(Default, serde::Deserialize)]
struct Meta {
    description: Option<String>,
    #[serde(rename = "mainProgram")]
    main_program: Option<String>,
    platforms: Option<Vec<Platform>>,
    #[serde(rename = "badPlatforms", default)]
    bad_platforms: Vec<Platform>,
    unsupported: Option<bool>,
    unfree: Option<bool>,
    broken: Option<bool>,
    insecure: Option<bool>,
}

/// One platform entry: a system name, or a pattern that pkg does not
/// match.
enum Platform {
    System(String),
    Pattern,
}

impl Platform {
    fn is(&self, system: &str) -> bool {
        matches!(self, Self::System(name) if name == system)
    }
}

impl<'de> serde::Deserialize<'de> for Platform {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PlatformVisitor;
        impl<'de> Visitor<'de> for PlatformVisitor {
            type Value = Platform;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a system name or a platform pattern")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Platform, E> {
                Ok(Platform::System(value.to_string()))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Platform, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Platform::Pattern)
            }
        }
        deserializer.deserialize_any(PlatformVisitor)
    }
}

/// The top-level object: `packages` and `version`.
struct ListVisitor<'r, 'a, 'b>(&'r mut Rows<'a, 'b>);

impl<'de> Visitor<'de> for ListVisitor<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a package list")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "packages" => map.next_value_seed(PackagesSeed(&mut *self.0))?,
                // The list writes the version as a string, such as "2".
                "version" => {
                    self.0.version = match map.next_value()? {
                        serde_json::Value::String(text) => text.parse().ok(),
                        value => value.as_u64(),
                    };
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// The `packages` object, stored one entry at a time.
struct PackagesSeed<'r, 'a, 'b>(&'r mut Rows<'a, 'b>);

impl<'de> DeserializeSeed<'de> for PackagesSeed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for PackagesSeed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("package entries")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let entry: Entry = map.next_value()?;
            self.0
                .insert(&key, &entry)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nixos_channel_branches_have_lists() {
        for (source, expected) in [
            (
                "github:NixOS/nixpkgs/nixpkgs-unstable",
                Some("nixpkgs-unstable"),
            ),
            ("github:nixos/nixpkgs/nixos-25.05", Some("nixos-25.05")),
            (
                "github:NixOS/nixpkgs/nixpkgs-25.05-darwin",
                Some("nixpkgs-25.05-darwin"),
            ),
            ("github:NixOS/nixpkgs/master", None),
            ("github:NixOS/nixpkgs", None),
            ("github:NixOS/nixpkgs/nixpkgs-unstable?dir=x", None),
            ("github:example/nixpkgs/nixpkgs-unstable", None),
            ("path:/tmp/nixpkgs", None),
        ] {
            assert_eq!(channel(source), expected, "{source}");
        }
    }

    #[test]
    fn identities_use_the_locked_github_form() {
        let identity = identity("github:NixOS/nixpkgs/nixpkgs-unstable", "abc");
        assert_eq!(
            identity.locked_url.as_deref(),
            Some("github:NixOS/nixpkgs/abc")
        );
        assert_eq!(identity.revision.as_deref(), Some("abc"));
    }

    #[test]
    fn the_flags_apply_on_the_list_system_and_platforms_elsewhere() {
        let entry = |meta: serde_json::Value| -> Entry {
            serde_json::from_value(serde_json::json!({
                "pname": "p", "version": "1", "system": "x86_64-linux", "meta": meta,
            }))
            .unwrap()
        };
        let i686 = entry(serde_json::json!({"platforms": ["i686-linux"]}));
        let darwin_only = entry(serde_json::json!({
            "platforms": ["aarch64-darwin"], "unsupported": true,
        }));
        let bad_on_darwin = entry(serde_json::json!({"badPlatforms": ["aarch64-darwin"]}));
        let unfree = entry(serde_json::json!({"unfree": true}));
        let broken = entry(serde_json::json!({"broken": true}));
        for (entry, linux, darwin) in [
            (&i686, None, Some(OTHER_SYSTEMS)),
            (&darwin_only, Some("unsupported-platform"), None),
            (&bad_on_darwin, None, Some(OTHER_SYSTEMS)),
            (&unfree, Some("unfree"), Some("unfree")),
            (&broken, Some("broken"), None),
        ] {
            assert_eq!(reason(entry, "x86_64-linux"), linux);
            assert_eq!(reason(entry, "aarch64-darwin"), darwin);
        }
    }
}
