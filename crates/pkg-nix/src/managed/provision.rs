//! Fetch, verify, stage, and receipt-last commit a product-managed Nix runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, chown, lchown, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lzma_rust2::XzReader;
use pkg_channel::{TrustedRoot, VerifiedChannel};
use sha2::{Digest as _, Sha256};
use tar::{Archive, EntryType};

use super::daemon::{DaemonErrorCode, ManagedDaemon};
use super::detect::{DetectionDisposition, DetectionReport, detect_unmanaged_nix};
use super::installer_bundle::{VerifiedRuntimeBundle, load_installer_bundle};
use super::ownership::{
    ManagedArtifact, ManagedArtifactKind, ManagedGroupBindings, OwnershipExpectation,
    decode_ownership_asset_manifest, encode_ownership_receipt, ownership_receipt_path,
    verify_with_owner_uid,
};
use crate::{Digest, NixVersion, System};

const MAX_RUNTIME_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ASSET_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_UNCOMPRESSED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4096;
static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

/// An authenticated pair of TUF target identities needed for provisioning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionSpec {
    descriptor_sha256: [u8; 32],
    system: System,
    nix_version: NixVersion,
    runtime_target: String,
    runtime_sha256: Digest,
    asset_manifest_target: String,
    asset_manifest_sha256: Digest,
}

impl ProvisionSpec {
    /// Promotes a verified channel into the narrow immutable provisioning input.
    pub fn from_verified_channel(
        channel: &VerifiedChannel,
        system: System,
    ) -> Result<Self, ProvisionError> {
        let descriptor = channel.descriptor();
        let runtime = descriptor.runtime();
        let expected_runtime = format!("nix/{}/{system}.tar.xz", descriptor.nix_version());
        let expected_manifest = format!("nix/{}/{system}.assets.json", descriptor.nix_version());
        if runtime.target() != expected_runtime
            || runtime.asset_manifest_target() != expected_manifest
        {
            return Err(ProvisionError::new(
                ProvisionErrorCode::InvalidAuthenticatedInput,
            ));
        }
        Ok(Self {
            descriptor_sha256: channel.descriptor_sha256(),
            system,
            nix_version: NixVersion::new(descriptor.nix_version())
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?,
            runtime_target: runtime.target().to_owned(),
            runtime_sha256: parse_raw_sha256(runtime.sha256())?,
            asset_manifest_target: runtime.asset_manifest_target().to_owned(),
            asset_manifest_sha256: parse_raw_sha256(runtime.asset_manifest_sha256())?,
        })
    }

    /// Returns the target native system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the authenticated Nix version.
    #[must_use]
    pub const fn nix_version(&self) -> &NixVersion {
        &self.nix_version
    }
}

/// Read-only source of already authenticated channel targets.
///
/// The source receives only exact target names promoted by [`ProvisionSpec`].
/// It cannot supply URLs, hashes, or other policy knobs to the provisioner.
trait RuntimeSource: Send + Sync {
    /// Returns the verified descriptor identity that authorized this source.
    fn descriptor_sha256(&self) -> [u8; 32];

    /// Opens one exact authenticated target as a bounded streaming reader.
    fn open_target(&self, target: &str) -> Result<Box<dyn Read + Send>, ProvisionError>;

    /// Commits the descriptor rollback floor after the installed runtime and
    /// ownership receipt have both verified.
    #[cfg(test)]
    fn commit_accepted_channel(&self) -> Result<(), ProvisionError>;
}

impl RuntimeSource for VerifiedRuntimeBundle {
    fn descriptor_sha256(&self) -> [u8; 32] {
        VerifiedRuntimeBundle::descriptor_sha256(self)
    }

    fn open_target(&self, target: &str) -> Result<Box<dyn Read + Send>, ProvisionError> {
        VerifiedRuntimeBundle::open_target(self, target)
            .map(|file| Box::new(file) as Box<dyn Read + Send>)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))
    }

    #[cfg(test)]
    fn commit_accepted_channel(&self) -> Result<(), ProvisionError> {
        VerifiedRuntimeBundle::commit_accepted_channel(self)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))
    }
}

/// Public inputs that do not contain authenticated target handles or Nix controls.
pub struct InstallerProvisionRequest<'a> {
    /// Fixed offline release bundle root containing `metadata/` and `targets/`.
    pub bundle_root: &'a Path,
    /// Existing private state directory for TUF metadata and rollback memory.
    pub datastore: &'a Path,
    /// Filesystem root to install beneath. Production callers pass `/`.
    pub installation_root: &'a Path,
    /// Existing private scratch parent used before the privileged commit.
    pub scratch_parent: &'a Path,
    /// Native host system established by the platform installer.
    pub system: System,
    /// Host-local gids for the two stable signed group roles.
    pub groups: ManagedGroupBindings,
}

/// Opaque authenticated installer bundle retained in private snapshots.
///
/// This value exposes no target reader, repository handle, datastore writer,
/// Nix option, or arbitrary path. Consuming it is the only way to provision
/// the exact bundle identity authenticated before platform mutation.
pub struct AuthenticatedInstallerBundle {
    source: VerifiedRuntimeBundle,
    spec: ProvisionSpec,
    installation_root: PathBuf,
    scratch_parent: PathBuf,
    groups: ManagedGroupBindings,
}

impl std::fmt::Debug for AuthenticatedInstallerBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedInstallerBundle")
            .field("system", &self.spec.system)
            .finish_non_exhaustive()
    }
}

/// Inputs for one fail-closed provisioning attempt.
pub struct ProvisionRequest<'a> {
    /// Filesystem root to install beneath. Production callers pass `/`.
    pub installation_root: &'a Path,
    /// Existing private scratch parent used only before the privileged commit.
    pub scratch_parent: &'a Path,
    /// Immutable authenticated runtime identity.
    pub spec: &'a ProvisionSpec,
    /// Host-local gids for the two stable signed group roles.
    pub groups: ManagedGroupBindings,
}

/// Successful managed-runtime installation summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedRuntime {
    system: System,
    nix_version: NixVersion,
    artifact_count: usize,
}

/// Successful installer bootstrap result with the authenticated host index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedBootstrap {
    runtime: ProvisionedRuntime,
    index: Vec<u8>,
}

/// Pending authenticated runtime installation owned by its provisioning transaction.
///
/// Dropping this value rolls back the daemon and every path created by this
/// attempt. A successful platform installer must explicitly call [`Self::commit`].
pub struct ProvisionedBootstrapTransaction<'a> {
    bootstrap: Option<ProvisionedBootstrap>,
    rollback: Option<RuntimeRollback>,
    source: Option<VerifiedRuntimeBundle>,
    channel_committed: bool,
    daemon: &'a dyn ManagedDaemon,
}

impl std::fmt::Debug for ProvisionedBootstrapTransaction<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProvisionedBootstrapTransaction")
            .field("pending", &self.rollback.is_some())
            .finish_non_exhaustive()
    }
}

impl ProvisionedBootstrapTransaction<'_> {
    /// Commits platform ownership of the installed runtime and returns its report.
    ///
    /// # Errors
    ///
    /// Returns a closed failure if the transaction was already consumed.
    pub fn commit(mut self) -> Result<ProvisionedBootstrap, ProvisionError> {
        self.commit_channel()?;
        self.finalize()
    }

    /// Persists the authenticated channel floor while retaining rollback ownership.
    ///
    /// # Errors
    ///
    /// Returns `ChannelStateFailed` without consuming rollback ownership when
    /// the durable floor cannot be committed.
    pub fn commit_channel(&mut self) -> Result<(), ProvisionError> {
        if self.channel_committed {
            return Ok(());
        }
        self.source
            .as_ref()
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))?
            .commit_accepted_channel()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))?;
        self.source = None;
        self.channel_committed = true;
        Ok(())
    }

    /// Finalizes a channel-committed transaction after platform receipt publication.
    ///
    /// # Errors
    ///
    /// Returns a closed failure if the channel floor was not committed or the
    /// transaction was already consumed. Rollback remains automatic on error.
    pub fn finalize(mut self) -> Result<ProvisionedBootstrap, ProvisionError> {
        if !self.channel_committed {
            return Err(ProvisionError::new(ProvisionErrorCode::ChannelStateFailed));
        }
        let bootstrap = self
            .bootstrap
            .take()
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
        self.rollback = None;
        Ok(bootstrap)
    }

    /// Rolls back the daemon and every path created by this exact attempt.
    ///
    /// # Errors
    ///
    /// Returns `RollbackFailed` if any exact reverse operation fails.
    pub fn rollback(mut self) -> Result<(), ProvisionError> {
        self.bootstrap = None;
        self.rollback
            .take()
            .map_or(Ok(()), |rollback| rollback.execute(self.daemon))
    }
}

impl Drop for ProvisionedBootstrapTransaction<'_> {
    fn drop(&mut self) {
        if let Some(rollback) = self.rollback.take() {
            let _ = rollback.execute(self.daemon);
        }
    }
}

impl ProvisionedBootstrap {
    /// Returns the verified managed-runtime result.
    #[must_use]
    pub const fn runtime(&self) -> &ProvisionedRuntime {
        &self.runtime
    }

    /// Returns the exact authenticated compressed index bytes for this host.
    #[must_use]
    pub fn index(&self) -> &[u8] {
        &self.index
    }

    /// Consumes the result into its runtime report and authenticated index.
    #[must_use]
    pub fn into_parts(self) -> (ProvisionedRuntime, Vec<u8>) {
        (self.runtime, self.index)
    }
}

impl ProvisionedRuntime {
    /// Returns the installed native system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the installed exact Nix version.
    #[must_use]
    pub const fn nix_version(&self) -> &NixVersion {
        &self.nix_version
    }

    /// Returns the number of signed static artifacts installed.
    #[must_use]
    pub const fn artifact_count(&self) -> usize {
        self.artifact_count
    }
}

/// Stable fail-closed provisioning failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionErrorCode {
    /// Verified channel values could not be promoted into the provisioning contract.
    InvalidAuthenticatedInput,
    /// Clean-host detection found foreign, ambiguous, or unauthenticated Nix state.
    ExistingNixRefused,
    /// An authenticated target could not be fetched completely.
    FetchFailed,
    /// A fetched target exceeded its fixed size bound.
    TargetTooLarge,
    /// Fetched bytes did not match authenticated SHA-256 metadata.
    TargetHashMismatch,
    /// The signed static asset manifest was invalid.
    InvalidAssetManifest,
    /// The runtime archive violated its signed allowlist or resource limits.
    InvalidArchive,
    /// The destination tree could not be inspected without following unsafe state.
    UnsafeDestination,
    /// Static artifacts could not be committed exactly as declared.
    InstallFailed,
    /// The managed daemon failed to start or answer its health check.
    DaemonFailed,
    /// The signed manifest or ownership receipt could not be installed atomically.
    ReceiptFailed,
    /// The authenticated descriptor rollback floor could not be committed.
    ChannelStateFailed,
    /// Best-effort rollback could not remove every artifact created by this attempt.
    RollbackFailed,
}

/// Redacted provisioning error with an optional closed daemon subcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvisionError {
    code: ProvisionErrorCode,
    daemon_code: Option<DaemonErrorCode>,
}

impl ProvisionError {
    const fn new(code: ProvisionErrorCode) -> Self {
        Self {
            code,
            daemon_code: None,
        }
    }

    const fn daemon(code: DaemonErrorCode) -> Self {
        Self {
            code: ProvisionErrorCode::DaemonFailed,
            daemon_code: Some(code),
        }
    }

    /// Returns the stable top-level failure category.
    #[must_use]
    pub const fn code(self) -> ProvisionErrorCode {
        self.code
    }

    /// Returns the closed daemon subcode when activation failed.
    #[must_use]
    pub const fn daemon_code(self) -> Option<DaemonErrorCode> {
        self.daemon_code
    }
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "managed Nix provisioning failed: {:?}",
            self.code
        )
    }
}

impl std::error::Error for ProvisionError {}

/// Authenticates an offline installer bundle and provisions its managed runtime.
///
/// Runtime readers and rollback-state mutation stay private to this one-shot
/// transaction. The accepted descriptor floor is committed only after the
/// installed ownership receipt verifies.
pub async fn provision_managed_nix_from_bundle(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
    daemon: &dyn ManagedDaemon,
) -> Result<ProvisionedBootstrap, ProvisionError> {
    let bundle = authenticate_installer_bundle(trusted_root, request).await?;
    provision_authenticated_installer_bundle(bundle, request, daemon)
}

/// Authenticates and snapshots one fixed installer bundle before host mutation.
///
/// The returned capability is opaque and single-use. It retains the datastore
/// writer lease and the private unlinked target snapshots until provisioning.
pub async fn authenticate_installer_bundle(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    let source = load_installer_bundle(
        trusted_root,
        request.bundle_root,
        request.datastore,
        request.system,
    )
    .await
    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?;
    let spec = ProvisionSpec::from_verified_channel(source.channel(), source.system())?;
    let provision_request = ProvisionRequest {
        installation_root: request.installation_root,
        scratch_parent: request.scratch_parent,
        spec: &spec,
        groups: request.groups,
    };
    let (path_entries, environment_keys) = current_host_inputs();
    require_host_state(
        &provision_request,
        &path_entries,
        &environment_keys,
        HostStatePolicy::Strict,
    )?;
    Ok(AuthenticatedInstallerBundle {
        source,
        spec,
        installation_root: request.installation_root.to_path_buf(),
        scratch_parent: request.scratch_parent.to_path_buf(),
        groups: request.groups,
    })
}

/// Authenticates an installer bundle from a synchronous privileged entry point.
///
/// This function refuses to nest a Tokio runtime. Async callers must use
/// [`authenticate_installer_bundle`] directly.
pub fn authenticate_installer_bundle_blocking(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    refuse_nested_runtime()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?;
    runtime.block_on(authenticate_installer_bundle(trusted_root, request))
}

/// Consumes a previously authenticated private bundle and provisions it.
///
/// # Errors
///
/// Returns a closed provisioning error if the request no longer matches the
/// authenticated host identity or any install, readiness, receipt, or rollback
/// operation fails.
pub fn provision_authenticated_installer_bundle(
    bundle: AuthenticatedInstallerBundle,
    request: &InstallerProvisionRequest<'_>,
    daemon: &dyn ManagedDaemon,
) -> Result<ProvisionedBootstrap, ProvisionError> {
    provision_authenticated_installer_bundle_transaction(bundle, request, daemon)?.commit()
}

/// Provisions an authenticated bundle as an explicit rollback-owned transaction.
///
/// The transaction must remain pending until the platform installation has
/// completed. Dropping it or calling `rollback` removes only this attempt.
pub fn provision_authenticated_installer_bundle_transaction<'a>(
    mut bundle: AuthenticatedInstallerBundle,
    request: &InstallerProvisionRequest<'_>,
    daemon: &'a dyn ManagedDaemon,
) -> Result<ProvisionedBootstrapTransaction<'a>, ProvisionError> {
    if request.system != bundle.spec.system
        || bundle.source.system() != bundle.spec.system
        || request.installation_root != bundle.installation_root
        || request.scratch_parent != bundle.scratch_parent
        || request.groups != bundle.groups
    {
        return Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ));
    }
    let index = bundle
        .source
        .take_index()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    let provision_request = ProvisionRequest {
        installation_root: request.installation_root,
        scratch_parent: request.scratch_parent,
        spec: &bundle.spec,
        groups: request.groups,
    };
    let (path_entries, environment_keys) = current_host_inputs();
    let (runtime, rollback) = provision_with_owner_policy(
        &provision_request,
        &bundle.source,
        daemon,
        0,
        &path_entries,
        &environment_keys,
        HostStatePolicy::FixedPlatformPrerequisites,
    )?;
    Ok(ProvisionedBootstrapTransaction {
        bootstrap: Some(ProvisionedBootstrap { runtime, index }),
        rollback: Some(rollback),
        source: Some(bundle.source),
        channel_committed: false,
        daemon,
    })
}

/// Runs the one-shot installer transaction from a synchronous privileged entry point.
///
/// This function refuses to nest a Tokio runtime. Async callers must use
/// [`provision_managed_nix_from_bundle`] directly.
pub fn provision_managed_nix_from_bundle_blocking(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
    daemon: &dyn ManagedDaemon,
) -> Result<ProvisionedBootstrap, ProvisionError> {
    refuse_nested_runtime()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?;
    runtime.block_on(provision_managed_nix_from_bundle(
        trusted_root,
        request,
        daemon,
    ))
}

fn refuse_nested_runtime() -> Result<(), ProvisionError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn provision_with_owner(
    request: &ProvisionRequest<'_>,
    source: &dyn RuntimeSource,
    daemon: &dyn ManagedDaemon,
    required_owner_uid: u32,
    path_entries: &[PathBuf],
    environment_keys: &[std::ffi::OsString],
) -> Result<ProvisionedRuntime, ProvisionError> {
    let (runtime, rollback) = provision_with_owner_policy(
        request,
        source,
        daemon,
        required_owner_uid,
        path_entries,
        environment_keys,
        HostStatePolicy::Strict,
    )?;
    if let Err(error) = source.commit_accepted_channel() {
        return match rollback.execute(daemon) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(rollback_error),
        };
    }
    Ok(runtime)
}

#[derive(Clone, Copy)]
enum HostStatePolicy {
    Strict,
    FixedPlatformPrerequisites,
}

fn provision_with_owner_policy(
    request: &ProvisionRequest<'_>,
    source: &dyn RuntimeSource,
    daemon: &dyn ManagedDaemon,
    required_owner_uid: u32,
    path_entries: &[PathBuf],
    environment_keys: &[std::ffi::OsString],
    host_state_policy: HostStatePolicy,
) -> Result<(ProvisionedRuntime, RuntimeRollback), ProvisionError> {
    if source.descriptor_sha256() != request.spec.descriptor_sha256 {
        return Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ));
    }
    require_host_state(request, path_entries, environment_keys, host_state_policy)?;
    validate_private_directory(request.scratch_parent, required_owner_uid)?;
    validate_private_directory(request.installation_root, required_owner_uid)?;

    let workspace = ScratchWorkspace::new(request.scratch_parent)?;
    let archive_path = workspace.path.join("runtime.tar.xz");
    fetch_target(
        source,
        &request.spec.runtime_target,
        request.spec.runtime_sha256,
        MAX_RUNTIME_ARCHIVE_BYTES,
        &archive_path,
    )?;
    let manifest_path = workspace.path.join("assets.json");
    fetch_target(
        source,
        &request.spec.asset_manifest_target,
        request.spec.asset_manifest_sha256,
        MAX_ASSET_MANIFEST_BYTES,
        &manifest_path,
    )?;
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    let expectation = decode_ownership_asset_manifest(
        &manifest_bytes,
        request.spec.system,
        &request.spec.nix_version,
        request.spec.asset_manifest_sha256,
        request.groups,
    )
    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAssetManifest))?;

    let staging = workspace.path.join("staging");
    create_private_directory(&staging)?;
    extract_exact_archive(&archive_path, &staging, &expectation)?;

    // The privileged scan is deliberately repeated immediately before the
    // first installation mutation. Downloads and archive parsing cannot make
    // a previously dirty host become trusted.
    require_host_state(request, path_entries, environment_keys, host_state_policy)?;
    let mut transaction = InstallTransaction::new(request.installation_root, daemon);
    let result = (|| {
        transaction.install_artifacts(&staging, &expectation, required_owner_uid)?;
        transaction.install_manifest(request.spec.system, &manifest_bytes, required_owner_uid)?;
        daemon
            .start(
                request.installation_root,
                request.spec.system,
                &request.spec.nix_version,
            )
            .map_err(|error| ProvisionError::daemon(error.code()))?;
        transaction.daemon_started = true;
        daemon
            .ping_store()
            .map_err(|error| ProvisionError::daemon(error.code()))?;
        transaction.install_receipt(&expectation, required_owner_uid)?;
        verify_with_owner_uid(request.installation_root, &expectation, required_owner_uid)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        let report = ProvisionedRuntime {
            system: request.spec.system,
            nix_version: request.spec.nix_version.clone(),
            artifact_count: expectation.artifacts().len(),
        };
        let rollback = transaction.detach_rollback();
        Ok((report, rollback))
    })();
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            if transaction.rollback().is_err() {
                Err(ProvisionError::new(ProvisionErrorCode::RollbackFailed))
            } else {
                Err(error)
            }
        }
    }
}

fn require_host_state(
    request: &ProvisionRequest<'_>,
    path_entries: &[PathBuf],
    environment_keys: &[std::ffi::OsString],
    policy: HostStatePolicy,
) -> Result<(), ProvisionError> {
    let report = detect_unmanaged_nix(
        request.installation_root,
        request.spec.system,
        path_entries,
        environment_keys,
    );
    if report.disposition() == DetectionDisposition::Clean
        || matches!(policy, HostStatePolicy::FixedPlatformPrerequisites)
            && has_only_fixed_platform_prerequisites(&report, request.spec.system)
    {
        Ok(())
    } else {
        Err(ProvisionError::new(ProvisionErrorCode::ExistingNixRefused))
    }
}

fn has_only_fixed_platform_prerequisites(report: &DetectionReport, system: System) -> bool {
    report.findings().iter().all(|finding| {
        matches!(
            finding.id(),
            "NIX_ROOT" | "NIX_VAR" | "NIXBLD_USERS" | "NIXBLD_GROUP"
        ) || matches!(
            (system, finding.id()),
            (
                System::X8664Linux | System::Aarch64Linux,
                "GETENT_NIXBLD_USER" | "GETENT_NIXBLD_GROUP"
            ) | (
                System::X8664Darwin | System::Aarch64Darwin,
                "DSCL_NIXBLD_USER" | "DSCL_NIXBLD_GROUP" | "SYNTHETIC_CONF_NIX"
            )
        )
    })
}

fn current_host_inputs() -> (Vec<PathBuf>, Vec<std::ffi::OsString>) {
    let path_entries = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let environment_keys = std::env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
    (path_entries, environment_keys)
}

fn parse_raw_sha256(value: &str) -> Result<Digest, ProvisionError> {
    format!("sha256:{value}")
        .parse()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))
}

fn fetch_target(
    source: &dyn RuntimeSource,
    target: &str,
    expected_digest: Digest,
    max_bytes: u64,
    destination: &Path,
) -> Result<(), ProvisionError> {
    let mut reader = source
        .open_target(target)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > max_bytes {
            return Err(ProvisionError::new(ProvisionErrorCode::TargetTooLarge));
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    }
    file.sync_all()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    if Digest::from_bytes(hasher.finalize().into()) != expected_digest {
        return Err(ProvisionError::new(ProvisionErrorCode::TargetHashMismatch));
    }
    Ok(())
}

fn extract_exact_archive(
    archive_path: &Path,
    staging: &Path,
    expectation: &OwnershipExpectation,
) -> Result<(), ProvisionError> {
    let expected: BTreeMap<String, &ManagedArtifact> = expectation
        .artifacts()
        .iter()
        .map(|artifact| (artifact.path().to_string_lossy().into_owned(), artifact))
        .collect();
    let file = File::open(archive_path)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    let decoder = XzReader::new(file, false);
    let bounded = BoundedReader::new(decoder, MAX_UNCOMPRESSED_BYTES);
    let mut archive = Archive::new(bounded);
    let mut seen = BTreeSet::new();
    let entries = archive
        .entries()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
        }
        let mut entry =
            entry.map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
        let relative = canonical_archive_path(
            &entry
                .path()
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?,
        )?;
        let absolute = format!("/{}", relative.to_string_lossy());
        let artifact = expected
            .get(&absolute)
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
        if !seen.insert(absolute) {
            return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
        }
        let destination = staging.join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
        }
        match artifact.kind() {
            ManagedArtifactKind::File if entry.header().entry_type().is_file() => {
                extract_file(&mut entry, &destination, artifact)?;
            }
            ManagedArtifactKind::Directory if entry.header().entry_type().is_dir() => {
                fs::create_dir_all(&destination)
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
            }
            ManagedArtifactKind::Symlink if entry.header().entry_type() == EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?
                    .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
                if target.as_ref() != Path::new(artifact.target().unwrap_or_default()) {
                    return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
                }
                symlink(target.as_ref(), &destination)
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
            }
            _ => return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive)),
        }
    }
    let mut bounded = archive.into_inner();
    let mut tail = [0_u8; 8192];
    loop {
        let count = bounded
            .read(&mut tail)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
        if count == 0 {
            break;
        }
        if tail[..count].iter().any(|byte| *byte != 0) {
            return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
        }
    }
    if bounded.exceeded {
        return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
    }
    for artifact in expectation.artifacts() {
        if artifact.kind() != ManagedArtifactKind::Directory
            && !seen.contains(&artifact.path().to_string_lossy().into_owned())
        {
            return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
        }
    }
    Ok(())
}

fn extract_file<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    destination: &Path,
    artifact: &ManagedArtifact,
) -> Result<(), ProvisionError> {
    let expected_size = artifact
        .size()
        .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    if entry.size() != expected_size {
        return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    let mut hasher = Sha256::new();
    let mut remaining = expected_size;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = entry
            .read(&mut buffer[..requested])
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
        if count == 0 {
            return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
        }
        remaining -= count as u64;
        hasher.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    }
    let expected_digest = artifact
        .sha256()
        .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidArchive))?;
    if Digest::from_bytes(hasher.finalize().into()) != expected_digest {
        return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
    }
    Ok(())
}

fn canonical_archive_path(path: &Path) -> Result<PathBuf, ProvisionError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
    }
    let mut canonical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => canonical.push(value),
            _ => return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive)),
        }
    }
    if canonical.as_os_str().is_empty() || canonical.to_str().is_none() {
        return Err(ProvisionError::new(ProvisionErrorCode::InvalidArchive));
    }
    Ok(canonical)
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
        let limit =
            usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = self.inner.read(&mut buffer[..limit])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

struct ScratchWorkspace {
    path: PathBuf,
}

impl ScratchWorkspace {
    fn new(parent: &Path) -> Result<Self, ProvisionError> {
        let serial = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("pkg-provision-{}-{serial}", std::process::id()));
        create_private_directory(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ScratchWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct InstallTransaction<'a> {
    root: &'a Path,
    daemon: &'a dyn ManagedDaemon,
    created: Vec<PathBuf>,
    daemon_started: bool,
    committed: bool,
}

struct RuntimeRollback {
    created: Vec<PathBuf>,
    daemon_started: bool,
}

impl RuntimeRollback {
    fn execute(mut self, daemon: &dyn ManagedDaemon) -> Result<(), ProvisionError> {
        let mut failed = self.daemon_started && daemon.stop().is_err();
        for path in self.created.drain(..).rev() {
            let result = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir(&path),
                Ok(_) => fs::remove_file(&path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
            failed |= result.is_err();
        }
        if failed {
            Err(ProvisionError::new(ProvisionErrorCode::RollbackFailed))
        } else {
            Ok(())
        }
    }
}

impl<'a> InstallTransaction<'a> {
    fn new(root: &'a Path, daemon: &'a dyn ManagedDaemon) -> Self {
        Self {
            root,
            daemon,
            created: Vec::new(),
            daemon_started: false,
            committed: false,
        }
    }

    fn install_artifacts(
        &mut self,
        staging: &Path,
        expectation: &OwnershipExpectation,
        owner_uid: u32,
    ) -> Result<(), ProvisionError> {
        let mut artifacts: Vec<&ManagedArtifact> = expectation.artifacts().iter().collect();
        artifacts.sort_by_key(|artifact| artifact.path().components().count());
        for artifact in artifacts {
            let destination = rooted(self.root, artifact.path());
            ensure_safe_parent(self.root, &destination, owner_uid)?;
            let gid = expectation.groups().gid_for(artifact.group());
            match artifact.kind() {
                ManagedArtifactKind::Directory => {
                    if let Ok(metadata) = fs::symlink_metadata(&destination) {
                        let expected_mode = artifact.mode().unwrap_or(0o700);
                        if !metadata.file_type().is_dir()
                            || metadata.uid() != owner_uid
                            || metadata.gid() != gid
                            || metadata.mode() & 0o7777 != expected_mode
                        {
                            return Err(ProvisionError::new(ProvisionErrorCode::InstallFailed));
                        }
                        continue;
                    }
                    fs::create_dir(&destination)
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    self.created.push(destination.clone());
                    chown(&destination, Some(owner_uid), Some(gid))
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    fs::set_permissions(
                        &destination,
                        fs::Permissions::from_mode(artifact.mode().unwrap_or(0o700)),
                    )
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                }
                ManagedArtifactKind::File => {
                    let source = rooted(staging, artifact.path());
                    let mut input = File::open(source)
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&destination)
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    self.created.push(destination.clone());
                    std::io::copy(&mut input, &mut output)
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    output
                        .sync_all()
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    chown(&destination, Some(owner_uid), Some(gid))
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    fs::set_permissions(
                        &destination,
                        fs::Permissions::from_mode(artifact.mode().unwrap_or(0o400)),
                    )
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                }
                ManagedArtifactKind::Symlink => {
                    symlink(artifact.target().unwrap_or_default(), &destination)
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                    self.created.push(destination.clone());
                    lchown(&destination, Some(owner_uid), Some(gid))
                        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InstallFailed))?;
                }
            }
        }
        Ok(())
    }

    fn install_manifest(
        &mut self,
        system: System,
        bytes: &[u8],
        owner_uid: u32,
    ) -> Result<(), ProvisionError> {
        let path = rooted(self.root, asset_manifest_path(system));
        self.install_metadata_file(&path, bytes, owner_uid)
    }

    fn install_receipt(
        &mut self,
        expectation: &OwnershipExpectation,
        owner_uid: u32,
    ) -> Result<(), ProvisionError> {
        let bytes = encode_ownership_receipt(expectation)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        let path = rooted(self.root, ownership_receipt_path(expectation.system()));
        self.install_metadata_file(&path, &bytes, owner_uid)
    }

    fn install_metadata_file(
        &mut self,
        path: &Path,
        bytes: &[u8],
        owner_uid: u32,
    ) -> Result<(), ProvisionError> {
        let parent = path
            .parent()
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        if fs::symlink_metadata(parent).is_err() {
            fs::create_dir(parent)
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
            self.created.push(parent.to_path_buf());
            let metadata_gid = if owner_uid == 0 {
                0
            } else {
                fs::metadata(self.root)
                    .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?
                    .gid()
            };
            chown(parent, Some(owner_uid), Some(metadata_gid))
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        }
        ensure_safe_parent(self.root, path, owner_uid)?;
        if fs::symlink_metadata(path).is_ok() {
            return Err(ProvisionError::new(ProvisionErrorCode::ReceiptFailed));
        }
        let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        self.created.push(temporary.clone());
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        let metadata_gid = if owner_uid == 0 {
            0
        } else {
            fs::metadata(self.root)
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?
                .gid()
        };
        chown(&temporary, Some(owner_uid), Some(metadata_gid))
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        fs::hard_link(&temporary, path)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        self.created.push(path.to_path_buf());
        fs::remove_file(&temporary)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))?;
        let temporary_index = self.created.len() - 2;
        self.created.remove(temporary_index);
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), ()> {
        let mut failed = false;
        if self.daemon_started && self.daemon.stop().is_err() {
            failed = true;
        }
        for path in self.created.iter().rev() {
            let result = match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir(path),
                Ok(_) => fs::remove_file(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
            if result.is_err() {
                failed = true;
            }
        }
        self.created.clear();
        if failed { Err(()) } else { Ok(()) }
    }

    fn detach_rollback(&mut self) -> RuntimeRollback {
        self.committed = true;
        RuntimeRollback {
            created: std::mem::take(&mut self.created),
            daemon_started: self.daemon_started,
        }
    }
}

impl Drop for InstallTransaction<'_> {
    fn drop(&mut self) {
        if !self.committed && !self.created.is_empty() {
            let _ = self.rollback();
        }
    }
}

fn asset_manifest_path(system: System) -> &'static Path {
    if matches!(system, System::X8664Linux | System::Aarch64Linux) {
        Path::new("/var/lib/pkg/managed-nix/assets-v1.json")
    } else {
        Path::new("/Library/Application Support/pkg/managed-nix/assets-v1.json")
    }
}

fn rooted(root: &Path, absolute: &Path) -> PathBuf {
    absolute
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

fn create_private_directory(path: &Path) -> Result<(), ProvisionError> {
    fs::create_dir(path).map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))
}

fn validate_private_directory(path: &Path, owner_uid: u32) -> Result<(), ProvisionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    if !metadata.file_type().is_dir() || metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0
    {
        return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
    }
    Ok(())
}

fn ensure_safe_parent(
    root: &Path,
    destination: &Path,
    owner_uid: u32,
) -> Result<(), ProvisionError> {
    let parent = destination
        .parent()
        .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(value) = component else {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        };
        current.push(value);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != owner_uid
            || metadata.mode() & 0o002 != 0
        {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use lzma_rust2::{XzOptions, XzWriter};
    use pkg_core::state::body_digest;
    use tempfile::TempDir;

    use super::*;
    use crate::managed::daemon::{DaemonError, DaemonErrorCode};
    use crate::managed::ownership::{ManagedGroup, encode_ownership_asset_manifest};

    const RUNTIME_PATH: &str = "/opt/pkg/nix/2.24.10/bin/nix";
    const RUNTIME_BYTES: &[u8] = b"fixture managed nix\n";

    #[tokio::test]
    async fn blocking_entry_point_refuses_a_nested_runtime() {
        assert_eq!(
            refuse_nested_runtime().map_err(ProvisionError::code),
            Err(ProvisionErrorCode::InvalidAuthenticatedInput)
        );
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

    struct FakeDaemon {
        fail_ping: bool,
        started: AtomicBool,
        stopped: AtomicBool,
    }

    impl FakeDaemon {
        fn healthy() -> Self {
            Self {
                fail_ping: false,
                started: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            }
        }
    }

    impl ManagedDaemon for FakeDaemon {
        fn start(
            &self,
            _installation_root: &Path,
            _system: System,
            _version: &NixVersion,
        ) -> Result<(), DaemonError> {
            self.started.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn ping_store(&self) -> Result<(), DaemonError> {
            if self.fail_ping {
                Err(DaemonError::new(DaemonErrorCode::ReadinessFailed))
            } else {
                Ok(())
            }
        }

        fn stop(&self) -> Result<(), DaemonError> {
            self.stopped.store(true, Ordering::Relaxed);
            Ok(())
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
                ManagedArtifact::directory("/opt/pkg", ManagedGroup::Broker, 0o750).unwrap(),
                ManagedArtifact::directory("/opt/pkg/nix", ManagedGroup::Broker, 0o750).unwrap(),
                ManagedArtifact::directory("/opt/pkg/nix/2.24.10", ManagedGroup::Broker, 0o750)
                    .unwrap(),
                ManagedArtifact::directory("/opt/pkg/nix/2.24.10/bin", ManagedGroup::Broker, 0o750)
                    .unwrap(),
                ManagedArtifact::file(
                    RUNTIME_PATH,
                    ManagedGroup::Broker,
                    0o550,
                    RUNTIME_BYTES.len() as u64,
                    body_digest(RUNTIME_BYTES),
                )
                .unwrap(),
                ManagedArtifact::directory("/var/lib/pkg", ManagedGroup::Broker, 0o700).unwrap(),
            ];
            let version = NixVersion::new("2.24.10").unwrap();
            let manifest =
                encode_ownership_asset_manifest(System::X8664Linux, &version, &artifacts).unwrap();
            let archive = archive_with_file("opt/pkg/nix/2.24.10/bin/nix", RUNTIME_BYTES);
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
                targets: BTreeMap::from([(runtime_target, archive), (manifest_target, manifest)]),
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
        let writer = archive.into_inner().unwrap();
        writer.finish().unwrap()
    }

    #[test]
    fn fixture_runtime_is_verified_activated_and_receipted() {
        let fixture = Fixture::new();
        let daemon = FakeDaemon::healthy();
        let report = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &daemon,
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(report.system(), System::X8664Linux);
        assert_eq!(report.nix_version().as_str(), "2.24.10");
        assert_eq!(
            fs::read(rooted(&fixture.root, Path::new(RUNTIME_PATH))).unwrap(),
            RUNTIME_BYTES
        );
        assert!(rooted(&fixture.root, ownership_receipt_path(System::X8664Linux)).is_file());
        assert!(daemon.started.load(Ordering::Relaxed));
        assert_eq!(fixture.source.opens.load(Ordering::Relaxed), 2);
        assert_eq!(fixture.source.commits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn prepared_platform_directory_is_reused_only_when_signed_metadata_matches() {
        let fixture = Fixture::new();
        let nix_root = fixture.root.join("nix");
        fs::create_dir(&nix_root).unwrap();
        fs::set_permissions(&nix_root, fs::Permissions::from_mode(0o755)).unwrap();
        let daemon = FakeDaemon::healthy();
        let (report, rollback) = provision_with_owner_policy(
            &fixture.request(),
            &fixture.source,
            &daemon,
            fixture.owner_uid,
            &[],
            &[],
            HostStatePolicy::FixedPlatformPrerequisites,
        )
        .unwrap();

        assert_eq!(report.system(), System::X8664Linux);
        assert!(nix_root.is_dir());
        assert_eq!(fixture.source.commits.load(Ordering::Relaxed), 0);
        rollback.execute(&daemon).unwrap();
        assert!(nix_root.is_dir());
        assert!(!rooted(&fixture.root, Path::new(RUNTIME_PATH)).exists());
        assert!(daemon.stopped.load(Ordering::Relaxed));
    }

    #[test]
    fn prepared_platform_directory_with_wrong_mode_is_refused() {
        let fixture = Fixture::new();
        let nix_root = fixture.root.join("nix");
        fs::create_dir(&nix_root).unwrap();
        fs::set_permissions(&nix_root, fs::Permissions::from_mode(0o777)).unwrap();
        let result = provision_with_owner_policy(
            &fixture.request(),
            &fixture.source,
            &FakeDaemon::healthy(),
            fixture.owner_uid,
            &[],
            &[],
            HostStatePolicy::FixedPlatformPrerequisites,
        );

        assert_eq!(
            result.map(|_| ()).map_err(ProvisionError::code),
            Err(ProvisionErrorCode::InstallFailed)
        );
        assert!(!rooted(&fixture.root, Path::new(RUNTIME_PATH)).exists());
    }

    #[test]
    fn prepared_platform_policy_rejects_unexpected_nix_evidence() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("nix")).unwrap();
        fs::create_dir(fixture.root.join("etc")).unwrap();
        fs::create_dir(fixture.root.join("etc/nix")).unwrap();

        let error = require_host_state(
            &fixture.request(),
            &[],
            &[],
            HostStatePolicy::FixedPlatformPrerequisites,
        )
        .unwrap_err();

        assert_eq!(error.code(), ProvisionErrorCode::ExistingNixRefused);
    }

    #[test]
    fn hash_mismatch_refuses_before_installation() {
        let mut fixture = Fixture::new();
        fixture.spec.runtime_sha256 = body_digest(b"different");
        let error = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &FakeDaemon::healthy(),
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), ProvisionErrorCode::TargetHashMismatch);
        assert_eq!(fixture.source.commits.load(Ordering::Relaxed), 0);
        assert!(!fixture.root.join("nix").exists());
    }

    #[test]
    fn descriptor_mismatch_refuses_before_fetch_or_installation() {
        let mut fixture = Fixture::new();
        fixture.source.descriptor_sha256 = [0x24; 32];
        let error = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &FakeDaemon::healthy(),
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), ProvisionErrorCode::InvalidAuthenticatedInput);
        assert_eq!(fixture.source.opens.load(Ordering::Relaxed), 0);
        assert_eq!(fixture.source.commits.load(Ordering::Relaxed), 0);
        assert!(!fixture.root.join("nix").exists());
    }

    #[test]
    fn channel_state_failure_rolls_back_verified_runtime_and_receipt() {
        let mut fixture = Fixture::new();
        fixture.source.fail_commit = true;
        let daemon = FakeDaemon::healthy();
        let error = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &daemon,
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), ProvisionErrorCode::ChannelStateFailed);
        assert_eq!(fixture.source.commits.load(Ordering::Relaxed), 1);
        assert!(daemon.stopped.load(Ordering::Relaxed));
        assert!(!fixture.root.join("nix").exists());
        assert!(!rooted(&fixture.root, ownership_receipt_path(System::X8664Linux)).exists());
    }

    #[test]
    fn daemon_readiness_failure_rolls_back_every_created_asset() {
        let fixture = Fixture::new();
        let daemon = FakeDaemon {
            fail_ping: true,
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        };
        let error = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &daemon,
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), ProvisionErrorCode::DaemonFailed);
        assert_eq!(error.daemon_code(), Some(DaemonErrorCode::ReadinessFailed));
        assert!(daemon.stopped.load(Ordering::Relaxed));
        assert!(!fixture.root.join("nix").exists());
        assert!(!fixture.root.join("opt/pkg").exists());
        assert!(!fixture.root.join("var/lib/pkg").exists());
    }

    #[test]
    fn unmanaged_nix_refuses_before_fetch() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("nix")).unwrap();
        let error = provision_with_owner(
            &fixture.request(),
            &fixture.source,
            &FakeDaemon::healthy(),
            fixture.owner_uid,
            &[],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), ProvisionErrorCode::ExistingNixRefused);
        assert_eq!(fixture.source.opens.load(Ordering::Relaxed), 0);
    }
}
