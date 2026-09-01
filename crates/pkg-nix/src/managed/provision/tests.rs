//! Tests for the `provision` module.

use std::future;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use lzma_rust2::{XzOptions, XzWriter};
use pkg_core::state::body_digest;
use tempfile::TempDir;

use super::*;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, symlink};
use tar::EntryType;

use crate::managed::ownership::{ManagedArtifact, ManagedGroup};

#[test]
fn determinate_crash_staging_reconciles_only_exact_private_files() {
    let temporary = TempDir::new().unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let metadata = fs::metadata(temporary.path()).unwrap();
    let staged = temporary
        .path()
        .join(".determinate-installer-AbCdEf0123456789");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .unwrap();
    file.sync_all().unwrap();

    reconcile_determinate_installer_staging_at(temporary.path(), metadata.uid(), metadata.gid())
        .unwrap();
    assert!(!staged.exists());

    let unexpected = temporary.path().join("unrelated");
    fs::write(&unexpected, b"keep").unwrap();
    assert!(
        reconcile_determinate_installer_staging_at(
            temporary.path(),
            metadata.uid(),
            metadata.gid(),
        )
        .is_err()
    );
    assert_eq!(fs::read(&unexpected).unwrap(), b"keep");
    fs::remove_file(&unexpected).unwrap();

    let linked = temporary
        .path()
        .join(".determinate-installer-Link012345678901");
    std::os::unix::fs::symlink("missing", &linked).unwrap();
    assert!(
        reconcile_determinate_installer_staging_at(
            temporary.path(),
            metadata.uid(),
            metadata.gid(),
        )
        .is_err()
    );
    assert!(
        fs::symlink_metadata(linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn staged_determinate_installer_closes_writer_before_execution() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, b"#!/bin/sh\nprintf 'staged-ok\\n'\n").unwrap();
    file.as_file_mut()
        .set_permissions(fs::Permissions::from_mode(0o700))
        .unwrap();
    file.as_file_mut().sync_all().unwrap();
    let path = file.path().to_owned();
    let staged = StagedDeterminateInstaller {
        path: file.into_temp_path(),
        length: fs::metadata(&path).unwrap().len(),
        sha256: Digest::from_bytes([0; 32]),
    };

    let output = std::process::Command::new(staged.path()).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"staged-ok\n");
    assert!(path.exists());
    drop(staged);
    assert!(!path.exists());
}

#[test]
fn blocking_installer_runtime_enables_time() {
    let runtime = installer_runtime().unwrap();
    assert!(
        runtime
            .block_on(async {
                tokio::time::timeout(std::time::Duration::ZERO, future::pending::<()>()).await
            })
            .is_err()
    );
}

#[test]
fn raw_channel_sha256_uses_the_product_digest_prefix() {
    let value = "b4e565fe4db5c352547b8146488cab81db06be89301a4c67b081c76f1e457760";
    assert_eq!(
        parse_raw_sha256(value).unwrap().to_string(),
        format!("sha256-{value}")
    );
}
use crate::managed::ownership::encode_ownership_asset_manifest;

const RUNTIME_PATH: &str = "/nix/store/fixture-nix-2.24.10/bin/nix";
const RUNTIME_BYTES: &[u8] = b"fixture managed nix\n";

#[tokio::test]
async fn blocking_entry_point_refuses_a_nested_runtime() {
    assert_eq!(
        refuse_nested_runtime().map_err(ProvisionError::code),
        Err(ProvisionErrorCode::InvalidAuthenticatedInput)
    );
}

#[test]
fn reauthentication_requires_the_exact_authenticated_identity() {
    assert!(DatastoreOwner::new(0, 30_000).is_none());
    let fixture = Fixture::new();
    let expected_spec = fixture.spec.clone();
    let expected_config = AuthenticatedManagedNixConfig {
        system: expected_spec.system,
        contents: "fixed config".to_owned(),
    };
    let expected_payloads =
        load_authenticated_installer_payloads(&fixture.source, expected_spec.system).unwrap();
    let expected_ownership = fixture.expectation();

    let identity =
        |spec: &ProvisionSpec,
         config: &AuthenticatedManagedNixConfig,
         payloads: &AuthenticatedInstallerPayloads,
         ownership: &OwnershipExpectation| AuthenticatedInstallerIdentity {
            base_nix: AuthenticatedBaseNixIdentity::Managed {
                spec: spec.clone(),
                ownership: ownership.clone(),
            },
            config: config.clone(),
            payloads: payloads.clone(),
        };

    let expected_identity = identity(
        &expected_spec,
        &expected_config,
        &expected_payloads,
        &expected_ownership,
    );
    assert_eq!(expected_identity, expected_identity);
    let mut changed_spec = expected_spec.clone();
    changed_spec.runtime_sha256 = Digest::from_bytes([9; 32]);
    assert_ne!(
        expected_identity,
        identity(
            &changed_spec,
            &expected_config,
            &expected_payloads,
            &expected_ownership,
        )
    );
    let changed_config = AuthenticatedManagedNixConfig {
        system: expected_spec.system,
        contents: "changed config".to_owned(),
    };
    assert_ne!(
        expected_identity,
        identity(
            &expected_spec,
            &changed_config,
            &expected_payloads,
            &expected_ownership,
        )
    );
    let mut changed_payloads = expected_payloads.clone();
    changed_payloads.product_cli = Arc::from(b"changed pkg".as_slice());
    assert_ne!(
        expected_identity,
        identity(
            &expected_spec,
            &expected_config,
            &changed_payloads,
            &expected_ownership,
        )
    );
    let changed_ownership = OwnershipExpectation::new(
        expected_ownership.system(),
        expected_ownership.nix_version().clone(),
        expected_ownership.asset_manifest_digest(),
        ManagedGroupBindings::new(40_000, 40_001).unwrap(),
        expected_ownership.artifacts().to_vec(),
    )
    .unwrap();
    assert_ne!(
        expected_identity,
        identity(
            &expected_spec,
            &expected_config,
            &expected_payloads,
            &changed_ownership,
        )
    );
}

#[test]
fn interrupted_workspace_recovery_preserves_siblings_and_symlink_targets() {
    let temp = TempDir::new().unwrap();
    let scratch = temp.path().join("scratch");
    create_private_directory(&scratch).unwrap();
    let owner_uid = fs::metadata(&scratch).unwrap().uid();
    verify_provision_workspace_absent_with_owner(&scratch, owner_uid).unwrap();

    let workspace = scratch.join(PROVISION_WORKSPACE_NAME);
    let staging = workspace.join("staging");
    create_private_directory(&workspace).unwrap();
    create_private_directory(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o777)).unwrap();
    fs::write(staging.join("runtime"), b"runtime").unwrap();
    let external = temp.path().join("external");
    fs::write(&external, b"keep").unwrap();
    symlink(&external, staging.join("link")).unwrap();
    let sibling = scratch.join("keep");
    fs::write(&sibling, b"keep").unwrap();

    assert!(recover_interrupted_provision_workspace_with_owner(&scratch, owner_uid).unwrap());
    assert!(!workspace.exists());
    assert_eq!(fs::read(external).unwrap(), b"keep");
    assert_eq!(fs::read(sibling).unwrap(), b"keep");
    assert!(!recover_interrupted_provision_workspace_with_owner(&scratch, owner_uid).unwrap());
}

#[test]
fn absent_scratch_parent_proves_the_workspace_is_absent() {
    let temp = TempDir::new().unwrap();
    let scratch = temp.path().join("scratch");
    let owner_uid = fs::metadata(temp.path()).unwrap().uid();

    verify_provision_workspace_absent_with_owner(&scratch, owner_uid).unwrap();
    assert!(!recover_interrupted_provision_workspace_with_owner(&scratch, owner_uid).unwrap());
    assert!(!scratch.exists());

    symlink(temp.path(), &scratch).unwrap();
    assert_eq!(
        verify_provision_workspace_absent_with_owner(&scratch, owner_uid)
            .map_err(ProvisionError::code),
        Err(ProvisionErrorCode::UnsafeDestination)
    );
}

#[test]
fn workspace_recovery_refuses_symlinks_and_unsafe_modes() {
    let temp = TempDir::new().unwrap();
    let scratch = temp.path().join("scratch");
    create_private_directory(&scratch).unwrap();
    let owner_uid = fs::metadata(&scratch).unwrap().uid();
    let external = temp.path().join("external");
    create_private_directory(&external).unwrap();
    let workspace = scratch.join(PROVISION_WORKSPACE_NAME);

    symlink(&external, &workspace).unwrap();
    assert_eq!(
        recover_interrupted_provision_workspace_with_owner(&scratch, owner_uid)
            .map_err(ProvisionError::code),
        Err(ProvisionErrorCode::UnsafeDestination)
    );
    fs::remove_file(&workspace).unwrap();
    create_private_directory(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        recover_interrupted_provision_workspace_with_owner(&scratch, owner_uid)
            .map_err(ProvisionError::code),
        Err(ProvisionErrorCode::UnsafeDestination)
    );
    assert!(workspace.is_dir());
    assert!(external.is_dir());
}

struct FakeSource {
    descriptor_sha256: [u8; 32],
    targets: BTreeMap<String, Vec<u8>>,
    opens: AtomicUsize,
    commits: AtomicUsize,
    fail_commit: bool,
}

impl RuntimeSource for FakeSource {
    fn descriptor_sha256(&self) -> [u8; 32] {
        self.descriptor_sha256
    }

    fn open_target(&self, target: &str) -> Result<Box<dyn Read + Send>, ProvisionError> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        self.targets
            .get(target)
            .cloned()
            .map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>)
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::FetchFailed))
    }

    #[cfg(test)]
    fn commit_accepted_channel(&self) -> Result<(), ProvisionError> {
        self.commits.fetch_add(1, Ordering::Relaxed);
        if self.fail_commit {
            Err(ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))
        } else {
            Ok(())
        }
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    scratch: PathBuf,
    spec: ProvisionSpec,
    groups: ManagedGroupBindings,
    source: FakeSource,
    owner_uid: u32,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let scratch = temp.path().join("scratch");
        create_private_directory(&root).unwrap();
        create_private_directory(&scratch).unwrap();
        fs::create_dir(root.join("opt")).unwrap();
        fs::create_dir(root.join("var")).unwrap();
        fs::create_dir(root.join("var/lib")).unwrap();
        let metadata = fs::metadata(&root).unwrap();
        let owner_uid = metadata.uid();
        let groups = ManagedGroupBindings::same_gid_for_test(metadata.gid());
        let artifacts = vec![
            ManagedArtifact::directory("/nix", ManagedGroup::Broker, 0o755).unwrap(),
            ManagedArtifact::directory("/nix/store", ManagedGroup::BuildUsers, 0o1775).unwrap(),
            ManagedArtifact::directory(
                "/nix/store/fixture-nix-2.24.10",
                ManagedGroup::Broker,
                0o555,
            )
            .unwrap(),
            ManagedArtifact::directory(
                "/nix/store/fixture-nix-2.24.10/bin",
                ManagedGroup::Broker,
                0o555,
            )
            .unwrap(),
            ManagedArtifact::file(
                RUNTIME_PATH,
                ManagedGroup::Broker,
                0o555,
                RUNTIME_BYTES.len() as u64,
                body_digest(RUNTIME_BYTES),
            )
            .unwrap(),
            ManagedArtifact::symlink(
                "/nix/store/fixture-nix-2.24.10/bin/nix-store",
                ManagedGroup::Broker,
                "nix",
            )
            .unwrap(),
            ManagedArtifact::symlink(
                "/nix/store/fixture-nix-2.24.10/bin/nix-daemon",
                ManagedGroup::Broker,
                "nix",
            )
            .unwrap(),
            ManagedArtifact::directory("/opt/pkg", ManagedGroup::Broker, 0o755).unwrap(),
            ManagedArtifact::directory("/opt/pkg/nix", ManagedGroup::Broker, 0o750).unwrap(),
            ManagedArtifact::directory("/opt/pkg/nix/2.24.10", ManagedGroup::Broker, 0o750)
                .unwrap(),
            ManagedArtifact::directory("/opt/pkg/nix/2.24.10/bin", ManagedGroup::Broker, 0o750)
                .unwrap(),
            ManagedArtifact::file(
                "/opt/pkg/nix/2.24.10/bin/nix",
                ManagedGroup::Broker,
                0o550,
                RUNTIME_BYTES.len() as u64,
                body_digest(RUNTIME_BYTES),
            )
            .unwrap(),
            ManagedArtifact::file(
                "/opt/pkg/nix/2.24.10/bin/nix-store",
                ManagedGroup::Broker,
                0o550,
                RUNTIME_BYTES.len() as u64,
                body_digest(RUNTIME_BYTES),
            )
            .unwrap(),
            ManagedArtifact::file(
                "/opt/pkg/nix/2.24.10/bin/nix-daemon",
                ManagedGroup::Broker,
                0o550,
                RUNTIME_BYTES.len() as u64,
                body_digest(RUNTIME_BYTES),
            )
            .unwrap(),
            ManagedArtifact::symlink("/opt/pkg/nix/current", ManagedGroup::Broker, "2.24.10")
                .unwrap(),
            ManagedArtifact::directory("/var/lib/pkg", ManagedGroup::Broker, 0o700).unwrap(),
        ];
        let version = NixVersion::new("2.24.10").unwrap();
        let manifest =
            encode_ownership_asset_manifest(System::X8664Linux, &version, &artifacts).unwrap();
        let archive = archive_with_file(
            "nix-2.24.10-x86_64-linux/store/fixture-nix-2.24.10/bin/nix",
            RUNTIME_BYTES,
        );
        let runtime_target = "nix/2.24.10/x86_64-linux.tar.xz".to_string();
        let manifest_target = "nix/2.24.10/x86_64-linux.assets.json".to_string();
        let spec = ProvisionSpec {
            descriptor_sha256: [0x42; 32],
            system: System::X8664Linux,
            nix_version: version,
            runtime_target: runtime_target.clone(),
            runtime_sha256: body_digest(&archive),
            asset_manifest_target: manifest_target.clone(),
            asset_manifest_sha256: body_digest(&manifest),
        };
        let source = FakeSource {
            descriptor_sha256: spec.descriptor_sha256,
            targets: BTreeMap::from([
                (runtime_target, archive),
                (manifest_target, manifest),
                (
                    "installer/x86_64-linux/pkg-root-helper".to_owned(),
                    b"root-helper".to_vec(),
                ),
                (
                    "installer/x86_64-linux/pkg-nix-broker".to_owned(),
                    b"broker".to_vec(),
                ),
                ("installer/x86_64-linux/pkg".to_owned(), b"pkg-cli".to_vec()),
            ]),
            opens: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
            fail_commit: false,
        };
        Self {
            _temp: temp,
            root,
            scratch,
            spec,
            groups,
            source,
            owner_uid,
        }
    }

    fn request(&self) -> ProvisionRequest<'_> {
        ProvisionRequest {
            installation_root: &self.root,
            scratch_parent: &self.scratch,
            spec: &self.spec,
            groups: self.groups,
        }
    }

    fn expectation(&self) -> OwnershipExpectation {
        let bytes = self
            .source
            .targets
            .get(&self.spec.asset_manifest_target)
            .expect("fixture manifest");
        decode_ownership_asset_manifest(
            bytes,
            self.spec.system,
            &self.spec.nix_version,
            self.spec.asset_manifest_sha256,
            self.groups,
        )
        .expect("fixture expectation")
    }
}

#[test]
fn installer_payloads_use_only_fixed_authenticated_targets() {
    let fixture = Fixture::new();
    let payloads =
        load_authenticated_installer_payloads(&fixture.source, System::X8664Linux).unwrap();

    assert_eq!(payloads.system(), System::X8664Linux);
    assert_eq!(payloads.root_helper(), b"root-helper");
    assert_eq!(payloads.broker(), b"broker");
    assert_eq!(payloads.product_cli(), b"pkg-cli");
    assert_eq!(fixture.source.opens.load(Ordering::Relaxed), 3);
}

#[test]
fn missing_or_empty_installer_payload_refuses_authentication() {
    let mut fixture = Fixture::new();
    fixture.source.targets.insert(
        "installer/x86_64-linux/pkg-nix-broker".to_owned(),
        Vec::new(),
    );
    assert_eq!(
        load_authenticated_installer_payloads(&fixture.source, System::X8664Linux)
            .map_err(ProvisionError::code),
        Err(ProvisionErrorCode::InvalidAuthenticatedInput)
    );
    fixture.source.targets.remove("installer/x86_64-linux/pkg");
    assert_eq!(
        load_authenticated_installer_payloads(&fixture.source, System::X8664Linux)
            .map_err(ProvisionError::code),
        Err(ProvisionErrorCode::FetchFailed)
    );
}

fn archive_with_file(path: &str, bytes: &[u8]) -> Vec<u8> {
    let writer = XzWriter::new(Vec::new(), XzOptions::with_preset(1)).unwrap();
    let mut archive = tar::Builder::new(writer);
    let mut header = tar::Header::new_gnu();
    header.set_path(path).unwrap();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o550);
    header.set_cksum();
    archive.append(&header, bytes).unwrap();
    for name in ["nix-store", "nix-daemon"] {
        let mut alias = tar::Header::new_gnu();
        alias.set_entry_type(EntryType::Symlink);
        alias
            .set_path(format!(
                "nix-2.24.10-x86_64-linux/store/fixture-nix-2.24.10/bin/{name}"
            ))
            .unwrap();
        alias.set_link_name("nix").unwrap();
        alias.set_size(0);
        alias.set_mode(0o777);
        alias.set_cksum();
        archive.append(&alias, Cursor::new([])).unwrap();
    }
    let registration = b"fixture registration\n";
    let mut registration_header = tar::Header::new_gnu();
    registration_header
        .set_path("nix-2.24.10-x86_64-linux/.reginfo")
        .unwrap();
    registration_header.set_size(registration.len() as u64);
    registration_header.set_mode(0o600);
    registration_header.set_cksum();
    archive
        .append(&registration_header, registration.as_slice())
        .unwrap();
    let writer = archive.into_inner().unwrap();
    writer.finish().unwrap()
}
