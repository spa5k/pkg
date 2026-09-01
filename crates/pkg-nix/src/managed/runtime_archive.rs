//! Shared producer and consumer contract for upstream Nix binary archives.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use lzma_rust2::XzReader;
use sha2::{Digest as _, Sha256};
use tar::{Archive, EntryType};

use super::ownership::{ManagedArtifact, ManagedGroup, encode_ownership_asset_manifest};
use crate::{Digest, NixVersion, System};

pub const MAX_ARCHIVE_ENTRIES: usize = 4096;
pub const MAX_REGISTRATION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNCOMPRESSED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const UPSTREAM_INSTALLER_FILES: [&str; 6] = [
    "install",
    "create-darwin-volume.sh",
    "install-darwin-multi-user.sh",
    "install-systemd-multi-user.sh",
    "install-freebsd-multi-user.sh",
    "install-multi-user",
];

/// Stable failures returned while deriving a managed-runtime asset manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeManifestErrorCode {
    /// The archive could not be opened or decoded completely.
    InvalidArchive,
    /// An archive member is outside the fixed upstream layout.
    InvalidMember,
    /// The installed artifact set violates the managed ownership contract.
    InvalidArtifact,
}

/// Redacted upstream-runtime manifest generation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeManifestError {
    code: RuntimeManifestErrorCode,
}

impl RuntimeManifestError {
    const fn new(code: RuntimeManifestErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn code(self) -> RuntimeManifestErrorCode {
        self.code
    }
}

impl fmt::Display for RuntimeManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "managed runtime manifest failed: {:?}",
            self.code
        )
    }
}

impl std::error::Error for RuntimeManifestError {}

pub enum UpstreamArchiveMember {
    Registration,
    Installer,
    Store(PathBuf),
}

struct RuntimeExecutable {
    path: PathBuf,
    size: u64,
    digest: Digest,
}

pub fn classify_upstream_archive_member(
    path: &Path,
    system: System,
    version: &NixVersion,
) -> Result<UpstreamArchiveMember, RuntimeManifestError> {
    let archived = canonical_archive_path(path)?;
    let prefix = PathBuf::from(format!("nix-{}-{}", version.as_str(), system.as_str()));
    let relative = archived
        .strip_prefix(prefix)
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember))?;
    if relative == Path::new(".reginfo") {
        return Ok(UpstreamArchiveMember::Registration);
    }
    if relative
        .to_str()
        .is_some_and(|member| UPSTREAM_INSTALLER_FILES.contains(&member))
    {
        return Ok(UpstreamArchiveMember::Installer);
    }
    let store = relative
        .strip_prefix("store")
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember))?;
    if store.as_os_str().is_empty() {
        return Err(RuntimeManifestError::new(
            RuntimeManifestErrorCode::InvalidMember,
        ));
    }
    Ok(UpstreamArchiveMember::Store(store.to_path_buf()))
}

/// Builds the canonical ownership manifest for one exact upstream Nix archive.
///
/// The function does not run an upstream installer script. It maps only the
/// authenticated `store/` tree, requires `.reginfo`, and adds the fixed product
/// runtime links used by the managed services.
///
/// # Errors
///
/// Returns a closed error when the archive is malformed, exceeds a fixed
/// bound, contains a foreign member, or cannot produce a valid ownership set.
pub fn build_upstream_runtime_asset_manifest(
    archive_path: &Path,
    system: System,
    version: &NixVersion,
) -> Result<Vec<u8>, RuntimeManifestError> {
    let file = File::open(archive_path)
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))?;
    let decoder = XzReader::new(file, false);
    let bounded = BoundedReader::new(decoder, MAX_UNCOMPRESSED_BYTES);
    let mut archive = Archive::new(bounded);
    let mut artifacts = base_artifacts(version)?;
    let mut registration_seen = false;
    let mut store_paths = BTreeSet::new();
    let mut runtime_executable = None;
    let mut runtime_aliases = BTreeMap::new();
    let entries = archive
        .entries()
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeManifestError::new(
                RuntimeManifestErrorCode::InvalidArchive,
            ));
        }
        let mut entry = entry
            .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))?;
        let path = entry
            .path()
            .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember))?;
        match classify_upstream_archive_member(&path, system, version)? {
            UpstreamArchiveMember::Registration => {
                if registration_seen
                    || !entry.header().entry_type().is_file()
                    || entry.size() == 0
                    || entry.size() > MAX_REGISTRATION_BYTES
                {
                    return Err(RuntimeManifestError::new(
                        RuntimeManifestErrorCode::InvalidArchive,
                    ));
                }
                drain_entry(&mut entry)?;
                registration_seen = true;
            }
            UpstreamArchiveMember::Installer => {
                if !entry.header().entry_type().is_file() {
                    return Err(RuntimeManifestError::new(
                        RuntimeManifestErrorCode::InvalidMember,
                    ));
                }
                drain_entry(&mut entry)?;
            }
            UpstreamArchiveMember::Store(relative) => {
                let installed = Path::new("/nix/store").join(relative);
                let installed_string = installed
                    .to_str()
                    .ok_or_else(|| {
                        RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember)
                    })?
                    .to_owned();
                if !store_paths.insert(installed_string.clone()) {
                    return Err(RuntimeManifestError::new(
                        RuntimeManifestErrorCode::InvalidMember,
                    ));
                }
                let mode = entry.header().mode().map_err(|_| {
                    RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember)
                })? & 0o7777;
                let entry_type = entry.header().entry_type();
                let artifact = match entry_type {
                    kind if kind.is_dir() => {
                        ManagedArtifact::directory(installed_string, ManagedGroup::Root, mode)
                    }
                    kind if kind.is_file() => {
                        let size = entry.size();
                        let digest = digest_entry(&mut entry)?;
                        if is_runtime_bin(&installed, "nix") {
                            if runtime_executable
                                .replace(RuntimeExecutable {
                                    path: installed.clone(),
                                    size,
                                    digest,
                                })
                                .is_some()
                            {
                                return Err(RuntimeManifestError::new(
                                    RuntimeManifestErrorCode::InvalidArtifact,
                                ));
                            }
                        } else if is_runtime_bin(&installed, "nix-store")
                            || is_runtime_bin(&installed, "nix-daemon")
                        {
                            return Err(RuntimeManifestError::new(
                                RuntimeManifestErrorCode::InvalidArtifact,
                            ));
                        }
                        ManagedArtifact::file(
                            installed_string,
                            ManagedGroup::Root,
                            mode,
                            size,
                            digest,
                        )
                    }
                    EntryType::Symlink => {
                        let target = entry
                            .link_name()
                            .map_err(|_| {
                                RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember)
                            })?
                            .ok_or_else(|| {
                                RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember)
                            })?;
                        let target = target.to_str().ok_or_else(|| {
                            RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidMember)
                        })?;
                        for name in ["nix-store", "nix-daemon"] {
                            if is_runtime_bin(&installed, name)
                                && runtime_aliases
                                    .insert(name, (installed.clone(), target.to_owned()))
                                    .is_some()
                            {
                                return Err(RuntimeManifestError::new(
                                    RuntimeManifestErrorCode::InvalidArtifact,
                                ));
                            }
                        }
                        ManagedArtifact::symlink(installed_string, ManagedGroup::Root, target)
                    }
                    _ => {
                        return Err(RuntimeManifestError::new(
                            RuntimeManifestErrorCode::InvalidMember,
                        ));
                    }
                }
                .map_err(|_| {
                    RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact)
                })?;
                artifacts.push(artifact);
            }
        }
    }
    let mut bounded = archive.into_inner();
    let mut tail = [0_u8; 8192];
    loop {
        let count = bounded
            .read(&mut tail)
            .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))?;
        if count == 0 {
            break;
        }
        if tail[..count].iter().any(|byte| *byte != 0) {
            return Err(RuntimeManifestError::new(
                RuntimeManifestErrorCode::InvalidArchive,
            ));
        }
    }
    if bounded.exceeded || !registration_seen {
        return Err(RuntimeManifestError::new(
            RuntimeManifestErrorCode::InvalidArchive,
        ));
    }
    add_runtime_facade(
        &mut artifacts,
        runtime_executable.as_ref(),
        &runtime_aliases,
        version,
    )?;
    encode_ownership_asset_manifest(system, version, &artifacts)
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))
}

fn base_artifacts(version: &NixVersion) -> Result<Vec<ManagedArtifact>, RuntimeManifestError> {
    let version_root = format!("/opt/pkg/nix/{}", version.as_str());
    [
        ManagedArtifact::directory("/nix", ManagedGroup::Root, 0o755),
        ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775),
        ManagedArtifact::directory("/opt/pkg", ManagedGroup::Root, 0o755),
        ManagedArtifact::directory("/opt/pkg/nix", ManagedGroup::Broker, 0o750),
        ManagedArtifact::directory(&version_root, ManagedGroup::Broker, 0o750),
        ManagedArtifact::directory(format!("{version_root}/bin"), ManagedGroup::Broker, 0o750),
    ]
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))
}

fn add_runtime_facade(
    artifacts: &mut Vec<ManagedArtifact>,
    executable: Option<&RuntimeExecutable>,
    aliases: &BTreeMap<&str, (PathBuf, String)>,
    version: &NixVersion,
) -> Result<(), RuntimeManifestError> {
    let executable = executable
        .ok_or_else(|| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))?;
    let executable_parent = executable
        .path
        .parent()
        .ok_or_else(|| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))?;
    if aliases.len() != 2
        || ["nix-store", "nix-daemon"].iter().any(|name| {
            !aliases.get(name).is_some_and(|(path, target)| {
                path.parent() == Some(executable_parent) && target == "nix"
            })
        })
    {
        return Err(RuntimeManifestError::new(
            RuntimeManifestErrorCode::InvalidArtifact,
        ));
    }
    for name in ["nix", "nix-store", "nix-daemon"] {
        artifacts.push(
            ManagedArtifact::file(
                format!("/opt/pkg/nix/{}/bin/{name}", version.as_str()),
                ManagedGroup::Broker,
                0o550,
                executable.size,
                executable.digest,
            )
            .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))?,
        );
    }
    artifacts.push(
        ManagedArtifact::symlink(
            "/opt/pkg/nix/current",
            ManagedGroup::Broker,
            version.as_str(),
        )
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArtifact))?,
    );
    Ok(())
}

pub fn is_runtime_bin(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|value| value == name)
        && path.parent().is_some_and(|parent| parent.ends_with("bin"))
}

fn canonical_archive_path(path: &Path) -> Result<PathBuf, RuntimeManifestError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(RuntimeManifestError::new(
            RuntimeManifestErrorCode::InvalidMember,
        ));
    }
    let mut canonical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => canonical.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RuntimeManifestError::new(
                    RuntimeManifestErrorCode::InvalidMember,
                ));
            }
        }
    }
    if canonical.as_os_str().is_empty() || canonical.to_str().is_none() {
        return Err(RuntimeManifestError::new(
            RuntimeManifestErrorCode::InvalidMember,
        ));
    }
    Ok(canonical)
}

fn drain_entry<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<(), RuntimeManifestError> {
    std::io::copy(entry, &mut std::io::sink())
        .map(|_| ())
        .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))
}

fn digest_entry<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<Digest, RuntimeManifestError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = entry
            .read(&mut buffer)
            .map_err(|_| RuntimeManifestError::new(RuntimeManifestErrorCode::InvalidArchive))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(Digest::from_bytes(hasher.finalize().into()))
}

struct BoundedReader<R> {
    inner: R,
    remaining: u64,
    exceeded: bool,
}

impl<R> BoundedReader<R> {
    const fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
            exceeded: false,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            let mut probe = [0_u8; 1];
            let count = self.inner.read(&mut probe)?;
            self.exceeded = count != 0;
            return Ok(0);
        }
        let allowed =
            usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    use lzma_rust2::{XzOptions, XzWriter};

    use super::*;
    use crate::managed::ownership::{
        ManagedArtifactKind, ManagedGroupBindings, decode_ownership_asset_manifest,
    };

    static NEXT_ARCHIVE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn official_archive_maps_to_a_decodable_runtime_manifest() {
        let archive = fixture_archive(false, true);
        let version = NixVersion::new("2.34.8").unwrap();
        let bytes = build_upstream_runtime_asset_manifest(&archive, System::Aarch64Linux, &version)
            .unwrap();
        let digest = Digest::from_bytes(Sha256::digest(&bytes).into());
        let expectation = decode_ownership_asset_manifest(
            &bytes,
            System::Aarch64Linux,
            &version,
            digest,
            ManagedGroupBindings::new(1001, 1002).unwrap(),
        )
        .unwrap();
        for path in [
            "/opt/pkg/nix/2.34.8/bin/nix",
            "/opt/pkg/nix/2.34.8/bin/nix-store",
            "/opt/pkg/nix/2.34.8/bin/nix-daemon",
            "/opt/pkg/nix/current",
        ] {
            assert!(expectation.artifacts().iter().any(|artifact| {
                artifact.path() == Path::new(path)
                    && if path.ends_with("current") {
                        artifact.kind() == ManagedArtifactKind::Symlink
                    } else {
                        artifact.kind() == ManagedArtifactKind::File
                    }
            }));
        }
        fs::remove_file(archive).unwrap();
    }

    #[test]
    fn foreign_archive_member_is_rejected() {
        let archive = fixture_archive(true, true);
        let error = build_upstream_runtime_asset_manifest(
            &archive,
            System::Aarch64Linux,
            &NixVersion::new("2.34.8").unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code(), RuntimeManifestErrorCode::InvalidMember);
        fs::remove_file(archive).unwrap();
    }

    #[test]
    fn missing_registration_is_rejected() {
        let archive = fixture_archive(false, false);
        let error = build_upstream_runtime_asset_manifest(
            &archive,
            System::Aarch64Linux,
            &NixVersion::new("2.34.8").unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.code(), RuntimeManifestErrorCode::InvalidArchive);
        fs::remove_file(archive).unwrap();
    }

    #[test]
    #[ignore = "requires PKG_TEST_NIX_ARCHIVE with the official Nix 2.34.8 aarch64-linux tarball"]
    fn official_nix_2_34_8_archive_is_representable() {
        let archive = std::env::var_os("PKG_TEST_NIX_ARCHIVE")
            .map(PathBuf::from)
            .expect("PKG_TEST_NIX_ARCHIVE must be set");
        let version = NixVersion::new("2.34.8").unwrap();
        let bytes = build_upstream_runtime_asset_manifest(&archive, System::Aarch64Linux, &version)
            .unwrap();
        let digest = Digest::from_bytes(Sha256::digest(&bytes).into());
        let expectation = decode_ownership_asset_manifest(
            &bytes,
            System::Aarch64Linux,
            &version,
            digest,
            ManagedGroupBindings::new(1001, 1002).unwrap(),
        )
        .unwrap();
        assert!(expectation.artifacts().len() > 2_000);
        assert!(expectation.artifacts().len() < MAX_ARCHIVE_ENTRIES);
    }

    fn fixture_archive(include_foreign_member: bool, include_registration: bool) -> PathBuf {
        let serial = NEXT_ARCHIVE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pkg-runtime-manifest-{}-{serial}.tar.xz",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        let writer = XzWriter::new(file, XzOptions::with_preset(1)).unwrap();
        let mut archive = tar::Builder::new(writer);
        let prefix = "nix-2.34.8-aarch64-linux";
        append_file(
            &mut archive,
            &format!("{prefix}/install"),
            0o755,
            b"#!/bin/sh\n",
        );
        if include_registration {
            append_file(
                &mut archive,
                &format!("{prefix}/.reginfo"),
                0o600,
                b"registration\n",
            );
        }
        let store = format!("{prefix}/store/{}-nix-2.34.8", "a".repeat(32));
        append_dir(&mut archive, &format!("{store}/"), 0o555);
        append_dir(&mut archive, &format!("{store}/bin/"), 0o555);
        append_file(&mut archive, &format!("{store}/bin/nix"), 0o555, b"nix");
        for binary in ["nix-store", "nix-daemon"] {
            append_symlink(&mut archive, &format!("{store}/bin/{binary}"), "nix");
        }
        if include_foreign_member {
            append_file(&mut archive, "foreign/file", 0o444, b"foreign");
        }
        let writer = archive.into_inner().unwrap();
        writer.finish().unwrap();
        path
    }

    fn append_dir<W: std::io::Write>(archive: &mut tar::Builder<W>, path: &str, mode: u32) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(EntryType::Directory);
        header.set_path(path).unwrap();
        header.set_size(0);
        header.set_mode(mode);
        header.set_cksum();
        archive.append(&header, Cursor::new([])).unwrap();
    }

    fn append_file<W: std::io::Write>(
        archive: &mut tar::Builder<W>,
        path: &str,
        mode: u32,
        bytes: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive.append(&header, bytes).unwrap();
    }

    fn append_symlink<W: std::io::Write>(archive: &mut tar::Builder<W>, path: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_path(path).unwrap();
        header.set_link_name(target).unwrap();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        archive.append(&header, Cursor::new([])).unwrap();
    }
}
