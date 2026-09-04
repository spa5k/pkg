//! Fetch, verify, stage, and receipt-last commit a product-managed Nix runtime.
#![expect(
    dead_code,
    reason = "DN-19 deletes these unreachable managed-Nix provisioning internals (plans/determinate-nix-stacked-prs.md)"
)]

use std::fmt;
use std::fs;
use std::io::{Read, Seek};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pkg_channel::{TrustedRoot, VerifiedChannel};
use tempfile::TempPath;
use url::Url;

use super::detect::{DetectionDisposition, DetectionReport, FindingKind, detect_unmanaged_nix};
use super::installer_bundle::{
    BundleEnvironment, DatastoreOwner, VerifiedRuntimeBundle, load_installer_bundle,
};
use super::ownership::{
    ManagedGroupBindings, OwnershipExpectation, decode_ownership_asset_manifest,
    verify_ownership_receipt_against_manifest, verify_with_owner_uid,
};

use super::runtime_archive::MAX_ARCHIVE_ENTRIES;
use crate::{Digest, NixVersion, System, render_managed_build_nix_conf};

const DETERMINATE_STAGING_PREFIX: &str = ".determinate-installer-";
const DETERMINATE_STAGING_SUFFIX_LENGTH: usize = 16;
const MAX_DETERMINATE_STAGING_ENTRIES: usize = 8;
const MAX_ASSET_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_INSTALLER_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const PROVISION_WORKSPACE_NAME: &str = "pkg-provision";
const MAX_PROVISION_WORKSPACE_ENTRIES: usize = MAX_ARCHIVE_ENTRIES + 8;

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
        self.persist_accepted_channel()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))
    }
}

/// A fixed installer repository selected by product code.
#[derive(Clone, Copy)]
pub enum InstallerRepository<'a> {
    /// A local release directory containing `metadata/` and `targets/`.
    Bundle(&'a Path),
    /// Immutable HTTPS release endpoints compiled into the installer.
    Remote {
        /// TUF metadata directory.
        metadata_url: &'a Url,
        /// TUF target directory.
        targets_url: &'a Url,
    },
}

/// Public inputs that do not contain authenticated target handles or Nix controls.
pub struct InstallerProvisionRequest<'a> {
    /// Fixed local or product-compiled authenticated repository.
    pub repository: InstallerRepository<'a>,
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

/// Exact managed-Nix configuration derived from authenticated channel policy.
///
/// Callers cannot construct this value. Platform installers may only consume
/// the bytes already promoted by the bundle authentication boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedManagedNixConfig {
    system: System,
    contents: String,
}

/// Exact product binaries read from fixed targets in the authenticated bundle.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedInstallerPayloads {
    system: System,
    root_helper: Arc<[u8]>,
    broker: Arc<[u8]>,
    product_cli: Arc<[u8]>,
}

impl AuthenticatedInstallerPayloads {
    /// Returns the native system bound to these payloads.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the exact authenticated root-helper bytes.
    #[must_use]
    pub fn root_helper(&self) -> &[u8] {
        &self.root_helper
    }

    /// Returns the exact authenticated broker bytes.
    #[must_use]
    pub fn broker(&self) -> &[u8] {
        &self.broker
    }

    /// Returns the exact authenticated public CLI bytes.
    #[must_use]
    pub fn product_cli(&self) -> &[u8] {
        &self.product_cli
    }
}

impl std::fmt::Debug for AuthenticatedInstallerPayloads {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedInstallerPayloads")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedManagedNixConfig {
    /// Returns the native system bound to these configuration bytes.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the exact authenticated bytes for atomic installation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.contents.as_bytes()
    }
}

impl std::fmt::Debug for AuthenticatedManagedNixConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedManagedNixConfig")
            .field("system", &self.system)
            .finish_non_exhaustive()
    }
}

/// Opaque authenticated installer bundle retained in private snapshots.
///
/// This value exposes no target reader, repository handle, datastore writer,
/// Nix option, or arbitrary path. Consuming it is the only way to provision
/// the exact bundle identity authenticated before platform mutation.
pub struct AuthenticatedInstallerBundle {
    source: VerifiedRuntimeBundle,
    base_nix: AuthenticatedBaseNix,
    managed_nix_config: AuthenticatedManagedNixConfig,
    installer_payloads: AuthenticatedInstallerPayloads,
    installation_root: PathBuf,
    scratch_parent: PathBuf,
    groups: ManagedGroupBindings,
}

enum AuthenticatedBaseNix {
    Managed(Box<AuthenticatedManagedBaseNix>),
    Determinate,
}

struct AuthenticatedManagedBaseNix {
    spec: ProvisionSpec,
    ownership: OwnershipExpectation,
}

impl AuthenticatedInstallerBundle {
    fn identity(&self) -> AuthenticatedInstallerIdentity {
        AuthenticatedInstallerIdentity {
            base_nix: match &self.base_nix {
                AuthenticatedBaseNix::Managed(managed) => AuthenticatedBaseNixIdentity::Managed {
                    spec: managed.spec.clone(),
                    ownership: managed.ownership.clone(),
                },
                AuthenticatedBaseNix::Determinate => AuthenticatedBaseNixIdentity::Determinate {
                    descriptor_sha256: self.source.descriptor_sha256(),
                    installer: self.source.determinate_installer_identity(),
                },
            },
            config: self.managed_nix_config.clone(),
            payloads: self.installer_payloads.clone(),
        }
    }

    /// Returns the native system authenticated by this closed bundle.
    #[must_use]
    pub const fn system(&self) -> System {
        self.source.system()
    }

    /// Returns the authenticated descriptor digest used as the product release identity.
    #[must_use]
    pub const fn release_identity_digest(&self) -> Digest {
        Digest::from_bytes(self.source.descriptor_sha256())
    }

    /// Returns the exact authenticated configuration for the platform backend.
    #[must_use]
    pub const fn managed_nix_config(&self) -> &AuthenticatedManagedNixConfig {
        &self.managed_nix_config
    }

    /// Returns the exact product binaries authenticated by the bundle.
    #[must_use]
    pub const fn installer_payloads(&self) -> &AuthenticatedInstallerPayloads {
        &self.installer_payloads
    }

    /// Returns the exact authenticated managed-runtime asset-manifest digest.
    pub fn asset_manifest_digest(&self) -> Result<Digest, ProvisionError> {
        match &self.base_nix {
            AuthenticatedBaseNix::Managed(managed) => Ok(managed.spec.asset_manifest_sha256),
            AuthenticatedBaseNix::Determinate => Err(ProvisionError::new(
                ProvisionErrorCode::InvalidAuthenticatedInput,
            )),
        }
    }

    /// Returns the exact authenticated runtime ownership expectation.
    pub fn ownership_expectation(&self) -> Result<&OwnershipExpectation, ProvisionError> {
        match &self.base_nix {
            AuthenticatedBaseNix::Managed(managed) => Ok(&managed.ownership),
            AuthenticatedBaseNix::Determinate => Err(ProvisionError::new(
                ProvisionErrorCode::InvalidAuthenticatedInput,
            )),
        }
    }

    /// Materializes the fixed authenticated Determinate installer once in a
    /// private root-owned directory. The file is removed when the returned
    /// capability is dropped.
    pub fn stage_determinate_installer(
        &mut self,
        directory: &Path,
    ) -> Result<StagedDeterminateInstaller, ProvisionError> {
        if !matches!(&self.base_nix, AuthenticatedBaseNix::Determinate) {
            return Err(ProvisionError::new(
                ProvisionErrorCode::InvalidAuthenticatedInput,
            ));
        }
        validate_private_directory(directory, 0)?;
        reconcile_determinate_installer_staging_at(directory, 0, 0)?;
        let (mut source, length, sha256) = self
            .source
            .take_determinate_installer()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?;
        source
            .seek(std::io::SeekFrom::Start(0))
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        let mut file = tempfile::Builder::new()
            .prefix(DETERMINATE_STAGING_PREFIX)
            .rand_bytes(DETERMINATE_STAGING_SUFFIX_LENGTH)
            .tempfile_in(directory)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        std::io::copy(&mut source, file.as_file_mut())
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        file.as_file_mut()
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        file.as_file_mut()
            .sync_all()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
        if file
            .as_file()
            .metadata()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?
            .len()
            != length
        {
            return Err(ProvisionError::new(ProvisionErrorCode::FetchFailed));
        }
        Ok(StagedDeterminateInstaller {
            path: file.into_temp_path(),
            length,
            sha256,
        })
    }

    /// Commits the authenticated release rollback floor after installation.
    pub fn commit_authenticated_channel(&mut self) -> Result<(), ProvisionError> {
        self.source
            .commit_accepted_channel()
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::ChannelStateFailed))
    }

    /// Returns the fixed authenticated Determinate executable identity without
    /// exposing its private snapshot or a filesystem path.
    pub fn determinate_installer_identity(&self) -> Result<(u64, Digest), ProvisionError> {
        if !matches!(&self.base_nix, AuthenticatedBaseNix::Determinate) {
            return Err(ProvisionError::new(
                ProvisionErrorCode::InvalidAuthenticatedInput,
            ));
        }
        self.source
            .determinate_installer_identity()
            .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))
    }
}

/// One private, authenticated Determinate installer executable.
pub struct StagedDeterminateInstaller {
    path: TempPath,
    length: u64,
    sha256: Digest,
}

impl StagedDeterminateInstaller {
    /// Returns the private executable path for the closed process adapter.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the authenticated executable length.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the authenticated executable digest.
    #[must_use]
    pub const fn sha256(&self) -> Digest {
        self.sha256
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AuthenticatedInstallerIdentity {
    base_nix: AuthenticatedBaseNixIdentity,
    config: AuthenticatedManagedNixConfig,
    payloads: AuthenticatedInstallerPayloads,
}

#[derive(Debug, PartialEq, Eq)]
enum AuthenticatedBaseNixIdentity {
    Managed {
        spec: ProvisionSpec,
        ownership: OwnershipExpectation,
    },
    Determinate {
        descriptor_sha256: [u8; 32],
        installer: Option<(u64, Digest)>,
    },
}

impl std::fmt::Debug for AuthenticatedInstallerBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedInstallerBundle")
            .field("system", &self.source.system())
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
    /// The signed manifest or ownership receipt could not be installed atomically.
    ReceiptFailed,
    /// The authenticated descriptor rollback floor could not be committed.
    ChannelStateFailed,
    /// Best-effort rollback could not remove every artifact created by this attempt.
    RollbackFailed,
}

/// Redacted provisioning failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvisionError {
    code: ProvisionErrorCode,
}

impl ProvisionError {
    const fn new(code: ProvisionErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable top-level failure category.
    #[must_use]
    pub const fn code(self) -> ProvisionErrorCode {
        self.code
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

/// Authenticates and snapshots one fixed installer bundle before host mutation.
///
/// The returned capability is opaque and single-use. It retains the datastore
/// writer lease and the private unlinked target snapshots until provisioning.
pub async fn authenticate_installer_bundle(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    let bundle = load_authenticated_installer_bundle(trusted_root, request).await?;
    if matches!(bundle.base_nix, AuthenticatedBaseNix::Determinate) {
        let (path_entries, environment_keys) = current_host_inputs();
        let report = detect_unmanaged_nix(
            request.installation_root,
            request.system,
            &path_entries,
            &environment_keys,
        );
        if report.disposition() != DetectionDisposition::Clean {
            return Err(ProvisionError::new(ProvisionErrorCode::ExistingNixRefused));
        }
    }
    Ok(bundle)
}

async fn load_authenticated_installer_bundle(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    load_authenticated_installer_bundle_with_owner(
        trusted_root,
        request,
        Some(DatastoreOwner::current()),
    )
    .await
}

async fn load_authenticated_installer_bundle_with_owner(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
    datastore_owner: Option<DatastoreOwner>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    let source = load_installer_bundle(
        trusted_root,
        request.repository,
        request.datastore,
        BundleEnvironment {
            host: request.system,
            datastore_owner,
            clock: Arc::new(pkg_core::SystemClock),
        },
    )
    .await
    .map_err(|e| { eprintln!("debug: load_installer_bundle failed: {e:?}"); ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput) })?;
    let installer_payloads = load_authenticated_installer_payloads(&source, source.system())
        .map_err(|e| { eprintln!("debug: load_payloads failed: {e:?}"); e })?;
    let managed_nix_config = AuthenticatedManagedNixConfig {
        system: source.system(),
        contents: render_managed_build_nix_conf(
            source.system(),
            source.channel().descriptor().cache(),
        )
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?,
    };
    let base_nix = match source.system() {
        System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin => {
            AuthenticatedBaseNix::Determinate
        }
        System::X8664Darwin => {
            let spec = ProvisionSpec::from_verified_channel(source.channel(), source.system())?;
            let manifest_bytes = read_target_bytes(
                &source,
                &spec.asset_manifest_target,
                MAX_ASSET_MANIFEST_BYTES,
            )?;
            let ownership = decode_ownership_asset_manifest(
                &manifest_bytes,
                spec.system,
                &spec.nix_version,
                spec.asset_manifest_sha256,
                request.groups,
            )
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAssetManifest))?;
            AuthenticatedBaseNix::Managed(Box::new(AuthenticatedManagedBaseNix { spec, ownership }))
        }
    };
    Ok(AuthenticatedInstallerBundle {
        source,
        base_nix,
        managed_nix_config,
        installer_payloads,
        installation_root: request.installation_root.to_path_buf(),
        scratch_parent: request.scratch_parent.to_path_buf(),
        groups: request.groups,
    })
}

/// Reauthenticates the same installer bundle in a different private datastore.
///
/// The first strict authentication capability is consumed. Any change to its
/// authenticated identity or fixed host request fails closed. This does not
/// repeat the strict clean-host scan because the platform transaction has now
/// created its fixed prerequisites. Provisioning reopens those prerequisites
/// with `HostStatePolicy::FixedPlatformPrerequisites` before runtime mutation.
/// The final datastore and every state file must have the verified non-root
/// broker owner before they can be used or published.
pub async fn reauthenticate_installer_bundle(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
    authenticated: AuthenticatedInstallerBundle,
    broker_uid: u32,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    if request.system != authenticated.source.system()
        || request.installation_root != authenticated.installation_root
        || request.scratch_parent != authenticated.scratch_parent
        || request.groups != authenticated.groups
    {
        return Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ));
    }
    let owner = DatastoreOwner::new(broker_uid, request.groups.broker_gid())
        .ok_or_else(|| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))?;
    let original_identity = authenticated.identity();
    let source = authenticated.source;
    // Release the original datastore lease before reopening it for the broker.
    drop(source);
    let replacement =
        load_authenticated_installer_bundle_with_owner(trusted_root, request, Some(owner)).await?;
    if original_identity != replacement.identity() {
        return Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ));
    }
    Ok(replacement)
}

fn read_target_bytes(
    source: &dyn RuntimeSource,
    target: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, ProvisionError> {
    let mut reader = source
        .open_target(target)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?
        .take(max_bytes.saturating_add(1));
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::FetchFailed))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ProvisionError::new(ProvisionErrorCode::TargetTooLarge));
    }
    Ok(bytes)
}

fn load_authenticated_installer_payloads(
    source: &dyn RuntimeSource,
    system: System,
) -> Result<AuthenticatedInstallerPayloads, ProvisionError> {
    let read = |name: &str| {
        read_target_bytes(
            source,
            &format!("installer/{system}/{name}"),
            MAX_INSTALLER_BINARY_BYTES,
        )
    };
    let root_helper = read("pkg-root-helper")?;
    let broker = read("pkg-nix-broker")?;
    let product_cli = read("pkg")?;
    if [
        root_helper.as_slice(),
        broker.as_slice(),
        product_cli.as_slice(),
    ]
    .into_iter()
    .any(<[u8]>::is_empty)
    {
        return Err(ProvisionError::new(
            ProvisionErrorCode::InvalidAuthenticatedInput,
        ));
    }
    Ok(AuthenticatedInstallerPayloads {
        system,
        root_helper: Arc::from(root_helper),
        broker: Arc::from(broker),
        product_cli: Arc::from(product_cli),
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
    let runtime = installer_runtime()?;
    runtime.block_on(authenticate_installer_bundle(trusted_root, request))
}

/// Loads authenticated release bytes without authorizing a host mutation.
///
/// This is for recovery code that must authenticate the identity bound into a
/// durable journal before it can remove an interrupted transaction. The caller
/// must perform a privileged host-state preflight before starting a new mutation.
///
/// # Errors
///
/// Returns a stable error for invalid release data or a nested Tokio runtime.
pub fn load_authenticated_installer_bundle_blocking(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    refuse_nested_runtime()?;
    let runtime = installer_runtime()?;
    runtime.block_on(load_authenticated_installer_bundle(trusted_root, request))
}

/// Reauthenticates a strictly authenticated bundle from a synchronous entry point.
pub fn reauthenticate_installer_bundle_blocking(
    trusted_root: TrustedRoot,
    request: &InstallerProvisionRequest<'_>,
    authenticated: AuthenticatedInstallerBundle,
    broker_uid: u32,
) -> Result<AuthenticatedInstallerBundle, ProvisionError> {
    refuse_nested_runtime()?;
    let runtime = installer_runtime()?;
    runtime.block_on(reauthenticate_installer_bundle(
        trusted_root,
        request,
        authenticated,
        broker_uid,
    ))
}
fn installer_runtime() -> Result<tokio::runtime::Runtime, ProvisionError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))
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

/// Verifies that all detected Nix evidence belongs to the exact authenticated install.
///
/// This is the privileged platform recheck used immediately before mutation.
/// A clean host is not an existing install and is therefore rejected here.
///
/// # Errors
///
/// Returns `ExistingNixRefused` for any ambiguous, foreign, unreadable, or
/// receipt-mismatched state.
pub fn verify_authenticated_managed_install(
    root: &Path,
    expectation: &OwnershipExpectation,
    path_entries: &[PathBuf],
    environment_keys: &[std::ffi::OsString],
) -> Result<(), ProvisionError> {
    let report = detect_unmanaged_nix(root, expectation.system(), path_entries, environment_keys);
    if authenticated_managed_install_matches(root, &report, expectation, 0) {
        Ok(())
    } else {
        Err(ProvisionError::new(ProvisionErrorCode::ExistingNixRefused))
    }
}

/// Verifies the installed runtime from a root-owned receipt and authenticated manifest facts.
///
/// This entry point is for the privileged helper. It does not accept user
/// paths or environment values, and it refuses all foreign or ambiguous host
/// evidence before it authenticates the receipt and every declared artifact.
pub fn verify_authenticated_managed_install_from_receipt(
    root: &Path,
    system: System,
    nix_version: &NixVersion,
    asset_manifest_digest: Digest,
    groups: ManagedGroupBindings,
) -> Result<(), ProvisionError> {
    let report = detect_unmanaged_nix(root, system, &[], &[]);
    if has_only_authenticated_managed_install_evidence(&report, system)
        && verify_ownership_receipt_against_manifest(
            root,
            system,
            nix_version,
            asset_manifest_digest,
            groups,
        )
        .is_ok()
    {
        Ok(())
    } else {
        Err(ProvisionError::new(ProvisionErrorCode::ExistingNixRefused))
    }
}

fn authenticated_managed_install_matches(
    root: &Path,
    report: &DetectionReport,
    expectation: &OwnershipExpectation,
    required_owner_uid: u32,
) -> bool {
    has_only_authenticated_managed_install_evidence(report, expectation.system())
        && verify_with_owner_uid(root, expectation, required_owner_uid).is_ok()
}

fn has_only_authenticated_managed_install_evidence(
    report: &DetectionReport,
    system: System,
) -> bool {
    !report.findings().is_empty()
        && report.findings().iter().all(|finding| {
            finding.kind() != FindingKind::Ambiguous
                && (matches!(
                    finding.id(),
                    "NIX_ROOT"
                        | "NIX_STORE_POPULATED"
                        | "NIX_STORE_EMPTY"
                        | "NIX_VAR"
                        | "NIX_DAEMON_SOCKET"
                        | "NIX_DB"
                        | "NIX_PROFILES"
                        | "ETC_NIX_DIR"
                        | "NIX_CONF"
                        | "PKG_BROKER_CONFIGURATION"
                        | "NIXBLD_USERS"
                        | "NIXBLD_GROUP"
                        | "PKG_OWNERSHIP_MARKER"
                        | "PKG_OWNERSHIP_RECEIPT"
                ) || matches!(
                    (system, finding.id()),
                    (
                        System::X8664Linux | System::Aarch64Linux,
                        "GETENT_NIXBLD_USER" | "GETENT_NIXBLD_GROUP"
                    ) | (
                        System::X8664Darwin | System::Aarch64Darwin,
                        "NIX_ROOT_SYMLINK"
                            | "DSCL_NIXBLD_USER"
                            | "DSCL_NIXBLD_GROUP"
                            | "SYNTHETIC_CONF_NIX"
                            | "FSTAB_NIX"
                    )
                ))
        })
}

fn has_only_fixed_platform_prerequisites(report: &DetectionReport, system: System) -> bool {
    report.findings().iter().all(|finding| {
        finding.kind() != FindingKind::Ambiguous
            && (matches!(
                finding.id(),
                "NIX_ROOT" | "NIX_STORE_EMPTY" | "NIX_VAR" | "NIXBLD_USERS" | "NIXBLD_GROUP"
            ) || matches!(
                (system, finding.id()),
                (
                    System::X8664Linux | System::Aarch64Linux,
                    "GETENT_NIXBLD_USER" | "GETENT_NIXBLD_GROUP"
                ) | (
                    System::X8664Darwin | System::Aarch64Darwin,
                    "DSCL_NIXBLD_USER" | "DSCL_NIXBLD_GROUP" | "SYNTHETIC_CONF_NIX"
                )
            ))
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
    format!("sha256-{value}")
        .parse()
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::InvalidAuthenticatedInput))
}

struct AttemptPath {
    path: PathBuf,
    device: u64,
    inode: u64,
    directory: bool,
}

fn remove_attempt_paths(paths: &mut Vec<AttemptPath>) -> bool {
    let mut failed = false;
    for entry in paths.iter().filter(|entry| entry.directory) {
        match fs::symlink_metadata(&entry.path) {
            Ok(metadata)
                if metadata.dev() == entry.device
                    && metadata.ino() == entry.inode
                    && metadata.file_type().is_dir() =>
            {
                failed |= fs::set_permissions(
                    &entry.path,
                    fs::Permissions::from_mode(metadata.mode() & 0o7777 | 0o700),
                )
                .is_err();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => failed = true,
        }
    }
    for entry in paths.drain(..).rev() {
        let result = match fs::symlink_metadata(&entry.path) {
            Ok(metadata)
                if metadata.dev() == entry.device
                    && metadata.ino() == entry.inode
                    && metadata.file_type().is_dir() == entry.directory =>
            {
                if entry.directory {
                    fs::remove_dir(&entry.path)
                } else {
                    fs::remove_file(&entry.path)
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) | Err(_) => Err(std::io::Error::other("attempt-owned path identity changed")),
        };
        failed |= result.is_err();
    }
    failed
}

/// Durably publishes directory mutations by syncing the parent directory.
fn sync_directory(directory: &Path) -> Result<(), ProvisionError> {
    fs::File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::ReceiptFailed))
}

#[cfg(test)]
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

/// Removes only exact product-created Determinate staging files.
///
/// The caller must first authenticate the directory against the same expected
/// owner. Every accepted entry is a private, single-link regular file.
#[expect(
    clippy::similar_names,
    reason = "owner_uid and owner_gid are one fixed ownership pair"
)]
fn reconcile_determinate_installer_staging_at(
    directory: &Path,
    owner_uid: u32,
    owner_gid: u32,
) -> Result<(), ProvisionError> {
    let mut paths = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    for entry in entries {
        let entry =
            entry.map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
        if paths.len() >= MAX_DETERMINATE_STAGING_ENTRIES {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        };
        let suffix = name.strip_prefix(DETERMINATE_STAGING_PREFIX);
        let Some(suffix) = suffix else {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        };
        if suffix.len() != DETERMINATE_STAGING_SUFFIX_LENGTH
            || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
        if !metadata.file_type().is_file()
            || metadata.uid() != owner_uid
            || metadata.gid() != owner_gid
            || !matches!(metadata.mode() & 0o7777, 0o600 | 0o700)
            || metadata.nlink() != 1
        {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        }
        paths.push(entry.path());
    }
    for path in paths {
        fs::remove_file(path)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    }
    sync_directory(directory)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))
}

/// Refuses when the fixed production provisioning workspace already exists.
///
/// Call this before a durable install journal records ownership of the next
/// provisioning attempt.
pub fn verify_provision_workspace_absent(scratch_parent: &Path) -> Result<(), ProvisionError> {
    verify_provision_workspace_absent_with_owner(scratch_parent, 0)
}

fn verify_provision_workspace_absent_with_owner(
    scratch_parent: &Path,
    owner_uid: u32,
) -> Result<(), ProvisionError> {
    match fs::symlink_metadata(scratch_parent) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The platform transaction creates this fixed parent. Provisioning
            // reopens and validates it before ScratchWorkspace creates a child.
            return Ok(());
        }
        Ok(_) => {}
        Err(_) => {
            return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
        }
    }
    validate_private_directory(scratch_parent, owner_uid)?;
    match fs::symlink_metadata(scratch_parent.join(PROVISION_WORKSPACE_NAME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) | Err(_) => Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination)),
    }
}

/// Removes a fixed interrupted provisioning workspace after journal proof.
///
/// The caller must first authenticate durable proof that this product attempt
/// observed the workspace as absent before it created the install intent.
pub fn recover_interrupted_provision_workspace(
    scratch_parent: &Path,
) -> Result<bool, ProvisionError> {
    recover_interrupted_provision_workspace_with_owner(scratch_parent, 0)
}

fn recover_interrupted_provision_workspace_with_owner(
    scratch_parent: &Path,
    owner_uid: u32,
) -> Result<bool, ProvisionError> {
    if matches!(
        fs::symlink_metadata(scratch_parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return Ok(false);
    }
    let Some(mut paths) = capture_provision_workspace(scratch_parent, owner_uid)? else {
        return Ok(false);
    };
    if remove_attempt_paths(&mut paths) {
        return Err(ProvisionError::new(ProvisionErrorCode::RollbackFailed));
    }
    sync_directory(scratch_parent)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::RollbackFailed))?;
    Ok(true)
}

fn capture_provision_workspace(
    scratch_parent: &Path,
    owner_uid: u32,
) -> Result<Option<Vec<AttemptPath>>, ProvisionError> {
    validate_private_directory(scratch_parent, owner_uid)?;
    let parent_metadata = fs::symlink_metadata(scratch_parent)
        .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
    let workspace = scratch_parent.join(PROVISION_WORKSPACE_NAME);
    let workspace_metadata = match fs::symlink_metadata(&workspace) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination)),
    };
    if !workspace_metadata.file_type().is_dir()
        || workspace_metadata.uid() != owner_uid
        || workspace_metadata.mode() & 0o7777 != 0o700
        || workspace_metadata.dev() != parent_metadata.dev()
    {
        return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
    }

    let mut paths = vec![AttemptPath {
        path: workspace.clone(),
        device: workspace_metadata.dev(),
        inode: workspace_metadata.ino(),
        directory: true,
    }];
    let mut directories = vec![workspace];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)
            .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?
        {
            let path = entry
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?
                .path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| ProvisionError::new(ProvisionErrorCode::UnsafeDestination))?;
            let file_type = metadata.file_type();
            let directory = file_type.is_dir();
            if metadata.dev() != workspace_metadata.dev()
                || metadata.uid() != owner_uid
                || !(directory || file_type.is_file() || file_type.is_symlink())
                || paths.len() >= MAX_PROVISION_WORKSPACE_ENTRIES
            {
                return Err(ProvisionError::new(ProvisionErrorCode::UnsafeDestination));
            }
            paths.push(AttemptPath {
                path: path.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
                directory,
            });
            if directory {
                directories.push(path);
            }
        }
    }
    Ok(Some(paths))
}

#[cfg(test)]
mod tests;
