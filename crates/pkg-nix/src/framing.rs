//! Exact product framing for the CLI↔broker and broker↔helper channels.

use std::fmt;
use std::str::FromStr;

use pkg_core::channel::{ChannelSequence, PolicyVersion, SourceRevision};
use pkg_core::identity::{OutputName, StorePath};
use pkg_core::selector::{OutputSelection, PackageSelector, SelectorId, SelectorInput};
use pkg_core::state::Digest;
use pkg_core::version::{PackageVersion, VersionBound, VersionPreference, VersionRange};
use serde::{Deserialize, Serialize};

use crate::broker::{BrokerOperationKind, OperationHandle, OperationStatus};
use crate::catalog::{
    CatalogInfoLookup, CatalogInfoReport, CatalogInfoRequest, CatalogPackageInfo,
    CatalogPackageSummary, CatalogSearchReport, CatalogSearchRequest,
};
use crate::maintenance::{
    GenerationId, MaintenanceCapability, RemoveRootSetRequest, RepairMode, RepairOutcomeKind,
    RepairPathOutcome, RepairStorePathsReport, RepairStorePathsRequest, RootSet,
    RootSetAttestationRequest, RootSetEntry, RootSetIntent, RootSetPublicationRequest,
    RootSetReport, RootSetTransitionIntent, RootSetTransitionReport, RootSetTransitionRequest,
    VerifiedRepairScope,
};
use crate::{
    ApprovalSource, BuildCacheErrorCode, BuildPreview, BuildProgressEstimate, BuildReadiness,
    BuildReport, CacheDownloadClosure, CachePathObservation, DerivationPlanReport,
    EvaluateDerivationRequest, GcReport, InstallEvidence, JsonCodec, NixpkgsPin,
    NixpkgsSourceErrorCode, PathInfoReport, RootName, RootNixFailure, RootNixOperation,
    RootNixRequest, RootNixResponse, RootRef, RootRepairPlanProof, RootRepairPlanRequest,
    SubstituteReport, System, VerifyReport, VerifyRequest, VersionInfo,
};
use crate::{MethodKind, NixAdapterErrorCode};
use serde_json::value::RawValue;

const MAGIC: [u8; 4] = *b"PKG1";
const PROTOCOL_VERSION: u16 = 1;
const HEADER_BYTES: usize = 20;
const MAX_CLI_FRAME_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Root Helper payload cap. It matches the largest existing typed DTO codec.
pub const HELPER_FRAME_PAYLOAD_LIMIT: usize = JsonCodec::PRODUCTION_LIMIT + 1024 * 1024;
const MAX_ROOT_SET_WIRE_ENTRIES: usize = 4096;
const MAX_BUILD_SELECTOR_WIRE_ENTRIES: usize = 4096;
const MAX_CATALOG_INFO_WIRE_ENTRIES: usize = 256;

const CHANNEL_CLI_BROKER: u8 = 1;
const CHANNEL_BROKER_HELPER: u8 = 2;

/// Stable product-frame decoding failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameErrorCode {
    /// The frame exceeded its fixed byte ceiling.
    FrameTooLarge,
    /// The header was truncated or carried the wrong magic.
    MalformedHeader,
    /// The protocol version is not the single supported version.
    UnsupportedVersion,
    /// Channel, method, or request id violated the closed grammar.
    UnsupportedMessage,
    /// Declared and actual payload lengths differed.
    LengthMismatch,
    /// The JSON body was malformed, extended, or invalid after promotion.
    InvalidPayload,
}

/// Redacted product-frame error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameError {
    code: FrameErrorCode,
}

impl FrameError {
    const fn new(code: FrameErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable frame failure category.
    #[must_use]
    pub const fn code(self) -> FrameErrorCode {
        self.code
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "product frame refused: {:?}", self.code)
    }
}

impl std::error::Error for FrameError {}

/// Closed controls on the CLI-to-broker channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliBrokerRequest {
    /// Begin one closed operation class.
    Begin(BrokerOperationKind),
    /// Poll one opaque operation handle.
    Poll(OperationHandle),
    /// Cancel one opaque operation handle.
    Cancel(OperationHandle),
    /// Complete one operation after its local state commit succeeds.
    Complete(OperationHandle),
    /// Query the pinned managed runtime under an authorized operation.
    Version(OperationHandle),
    /// Verify exact pkg ownership through the privileged helper.
    VerifyManagedOwnership(OperationHandle),
    /// Evaluate one validated derivation request under a resolve operation.
    EvaluateDerivation(OperationHandle, EvaluateDerivationRequest),
    /// Query validated metadata for one promoted store path.
    PathInfo(OperationHandle, StorePath),
    /// Attempt substitution for one promoted store path.
    Substitute(OperationHandle, StorePath),
    /// Approve the exact private plan already held under a build operation.
    ApproveBuild(OperationHandle, BuildApprovalRequest),
    /// Verify one validated closed request.
    Verify(OperationHandle, VerifyRequest),
    /// Collect unreachable paths using the managed root set.
    Gc(OperationHandle),
    /// Fetch the sanitized preview of a broker-held private build plan.
    GetBuildPreview(OperationHandle),
    /// Prepare a private build plan from typed selector intent and broker authority.
    PrepareBuild(OperationHandle, Vec<PackageSelector>),
    /// Execute the exact approved private plan retained under one build handle.
    ExecuteBuild(OperationHandle, Digest),
    /// Publish a complete generation root intent after successful build execution.
    PublishBuildRoots(OperationHandle, RootSetIntent),
    /// Derive a fresh generation root set from an authenticated source generation.
    TransitionGenerationRoots(OperationHandle, RootSetTransitionIntent),
    /// Remove one authenticated caller-owned generation root set under GC admission.
    RemoveGenerationRoots(OperationHandle, GenerationId),
    /// Wait for and retain exclusive broker GC admission on this operation.
    AcquireGc(OperationHandle),
    /// Fetch broker-produced post-build evidence before root publication.
    GetInstallEvidence(OperationHandle),
    /// Attest an already durable generation after restart or lost acknowledgement.
    AttestGenerationRoots(OperationHandle, GenerationId),
    /// Run cache-first acquisition from broker-retained channel authority.
    AcquireInstall(OperationHandle, Vec<PackageSelector>),
    /// Refresh the broker-owned signed channel and native index.
    RefreshChannel(OperationHandle, ChannelRefreshMode),
    /// Search the broker-owned verified native index.
    SearchCatalog(OperationHandle, CatalogSearchRequest),
    /// Inspect packages from one broker-owned verified native index snapshot.
    InfoCatalog(OperationHandle, Vec<CatalogInfoRequest>),
    /// Verify or cache-repair one caller-owned rooted generation.
    RepairGeneration(OperationHandle, RepairGenerationRequest),
}

/// One sanitized, authority-produced cache download counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallDownloadProgress {
    selector: SelectorInput,
    done: u64,
    total: u64,
}

impl InstallDownloadProgress {
    /// Constructs a bounded counter for one product selector.
    ///
    /// The total must be nonzero and completed bytes cannot exceed it.
    pub fn new(selector: SelectorInput, done: u64, total: u64) -> Result<Self, FrameError> {
        if total == 0 || done > total {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        Ok(Self {
            selector,
            done,
            total,
        })
    }

    /// Returns the product selector. No Nix path crosses this boundary.
    #[must_use]
    pub const fn selector(&self) -> &SelectorInput {
        &self.selector
    }

    /// Returns the authenticated completed-byte count.
    #[must_use]
    pub const fn done(&self) -> u64 {
        self.done
    }

    /// Returns the authenticated total-byte count for this counter.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }
}

/// Closed responses on the broker-to-CLI channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliBrokerResponse {
    /// A fresh caller-bound operation was opened.
    Started(OperationHandle),
    /// Current sanitized operation state.
    Status(OperationStatus),
    /// Cancellation was accepted and admission released.
    Cancelled,
    /// Completion was accepted and admission released.
    Completed,
    /// Validated pinned managed-runtime version information.
    Version(VersionInfo),
    /// Whether the privileged helper authenticated every managed runtime asset.
    ManagedOwnership(bool),
    /// Validated derivation evaluation result.
    DerivationPlan(DerivationPlanReport),
    /// Validated path metadata result.
    PathInfo(PathInfoReport),
    /// Validated substitution result.
    Substitute(SubstituteReport),
    /// The exact private plan was durably approved for this operation.
    BuildApproved,
    /// Validated verification result.
    Verify(VerifyReport),
    /// Validated garbage-collection result.
    Gc(GcReport),
    /// Sanitized view of the broker-held private build plan.
    BuildPreview(BuildPreview),
    /// Sanitized view returned after authenticated preparation succeeds.
    BuildPrepared(BuildPreview),
    /// Stable redacted refusal from authenticated build preparation.
    BuildPreparationRefused(BuildPreparationErrorCode),
    /// Validated report from broker-owned build execution.
    BuildExecuted(BuildReport),
    /// Sanitized best-effort live estimate for method 19.
    BuildExecutionProgress(BuildProgressEstimate),
    /// Stable redacted refusal from broker-owned build execution.
    BuildExecutionRefused(BuildExecutionErrorCode),
    /// Durable root publication completed for the authenticated generation.
    BuildRootsPublished(RootSetReport),
    /// Stable redacted refusal from protected root publication.
    BuildRootPublicationRefused(BuildRootPublicationErrorCode),
    /// Authenticated generation root transition completed.
    GenerationRootsTransitioned(RootSetTransitionReport),
    /// Stable redacted refusal from protected generation root transition.
    GenerationRootTransitionRefused(GenerationRootTransitionErrorCode),
    /// One authenticated generation root set was removed or already absent.
    GenerationRootsRemoved,
    /// Stable redacted refusal from protected generation root removal.
    GenerationRootRemovalRefused(GenerationRootRemovalErrorCode),
    /// Exclusive broker GC admission is retained until completion or cancellation.
    GcAdmissionAcquired,
    /// Private post-build evidence for crash-safe lifecycle assembly.
    InstallEvidence(InstallEvidence),
    /// Exact helper-attested receipt for an already durable generation.
    GenerationRootsAttested(RootSetReport),
    /// Stable redacted refusal from protected root attestation.
    GenerationRootAttestationRefused(GenerationRootAttestationErrorCode),
    /// Every selected output was acquired and retained as private evidence.
    InstallAcquired,
    /// At least one selected output requires the approved local-build path.
    InstallBuildRequired,
    /// Stable redacted refusal from cache-first acquisition.
    InstallAcquisitionRefused(CacheInstallErrorCode),
    /// Intermediate authenticated download progress for method 26.
    InstallDownloadProgress(InstallDownloadProgress),
    /// Signed channel and native index refresh completed atomically.
    ChannelRefreshed(ChannelRefreshReport),
    /// Signed channel refresh was refused without changing live authority.
    ChannelRefreshRefused(ChannelRefreshErrorCode),
    /// Ranked product metadata from the broker-owned verified index.
    CatalogSearch(CatalogSearchReport),
    /// Product metadata lookups from one broker-owned verified index snapshot.
    CatalogInfo(Vec<CatalogInfoReport>),
    /// The authenticated native index was unavailable or refused search.
    CatalogSearchRefused,
    /// The authenticated native index was unavailable or refused info lookup.
    CatalogInfoRefused,
    /// Sanitized outcome from one generation repair transaction.
    RepairGeneration(RepairGenerationReport),
    /// Stable redacted refusal from generation repair.
    RepairGenerationRefused(RepairGenerationErrorCode),
    /// Redacted adapter failure for one exposed typed method.
    AdapterFailure(MethodKind, NixAdapterErrorCode),
}

/// Stable authenticated-build preparation refusal categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPreparationErrorCode {
    /// Linux Determinate host facts or macOS managed configuration were unavailable.
    HostRefused,
    /// The typed selector batch was invalid under the verified channel.
    IntentRefused,
    /// Source, resolution, cache classification, or plan construction refused.
    PlanningRefused,
    /// The caller-bound broker handle would not retain this preparation.
    BrokerRefused,
}

impl BuildPreparationErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostRefused => "host_refused",
            Self::IntentRefused => "intent_refused",
            Self::PlanningRefused => "planning_refused",
            Self::BrokerRefused => "broker_refused",
        }
    }
}

/// Closed caller intent for one rooted-generation repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairGenerationRequest {
    generation: GenerationId,
    verify_only: bool,
    approval: Option<BuildApprovalRequest>,
}

impl RepairGenerationRequest {
    /// Constructs path-free repair intent. The broker and helper derive every path.
    #[must_use]
    pub const fn new(generation: GenerationId, verify_only: bool) -> Self {
        Self {
            generation,
            verify_only,
            approval: None,
        }
    }

    /// Constructs a mutating continuation for one displayed repair plan.
    #[must_use]
    pub const fn with_approval(generation: GenerationId, approval: BuildApprovalRequest) -> Self {
        Self {
            generation,
            verify_only: false,
            approval: Some(approval),
        }
    }

    /// Returns the caller-selected rooted generation identity.
    #[must_use]
    pub const fn generation(&self) -> &GenerationId {
        &self.generation
    }

    /// Returns whether mutation is forbidden for this request.
    #[must_use]
    pub const fn verify_only(&self) -> bool {
        self.verify_only
    }

    /// Returns the optional explicit approval pointer.
    #[must_use]
    pub const fn approval(&self) -> Option<&BuildApprovalRequest> {
        self.approval.as_ref()
    }
}

/// Public, path-free result category for one repair transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairGenerationStatus {
    /// The complete closure verified clean without mutation.
    Clean,
    /// Verify-only mode found one or more damaged paths.
    DamageDetected,
    /// Cache-only repair restored the complete closure.
    RepairedFromCache,
    /// An explicitly approved local rebuild restored the complete closure.
    RepairedByBuild,
    /// Cache misses remain and an approved rebuild is required.
    NeedsApproval,
}

/// Sanitized repair report. Raw paths and helper outcomes remain private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairGenerationReport {
    status: RepairGenerationStatus,
    damaged_paths: u32,
    build_preview: Option<BuildPreview>,
}

impl RepairGenerationReport {
    /// Constructs a report while enforcing status/count consistency.
    pub fn new(status: RepairGenerationStatus, damaged_paths: u32) -> Result<Self, FrameError> {
        let expects_damage = matches!(
            status,
            RepairGenerationStatus::DamageDetected | RepairGenerationStatus::NeedsApproval
        );
        if expects_damage != (damaged_paths > 0) || status == RepairGenerationStatus::NeedsApproval
        {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        Ok(Self {
            status,
            damaged_paths,
            build_preview: None,
        })
    }

    /// Constructs the approval-required outcome with its sanitized preview.
    pub fn needs_approval(
        damaged_paths: u32,
        build_preview: BuildPreview,
    ) -> Result<Self, FrameError> {
        if damaged_paths == 0 || !build_preview.is_repair_approval() {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        Ok(Self {
            status: RepairGenerationStatus::NeedsApproval,
            damaged_paths,
            build_preview: Some(build_preview),
        })
    }

    /// Returns the sanitized terminal category.
    #[must_use]
    pub const fn status(&self) -> RepairGenerationStatus {
        self.status
    }

    /// Returns only the number of paths that remain damaged.
    #[must_use]
    pub const fn damaged_paths(&self) -> u32 {
        self.damaged_paths
    }

    /// Returns the sanitized repair-build preview when approval is required.
    #[must_use]
    pub const fn build_preview(&self) -> Option<&BuildPreview> {
        self.build_preview.as_ref()
    }
}

/// Closed, redacted production repair failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairGenerationErrorCode {
    /// The generation or derived repair scope was invalid.
    InvalidScope,
    /// Read-only verification failed or returned inconsistent evidence.
    VerifyFailed,
    /// Broker admission was unavailable or invalid.
    AdmissionFailed,
    /// The privileged helper refused or failed the closed operation.
    HelperFailed,
    /// Durable repair journaling failed.
    JournalFailed,
    /// Damage remained after a purported repair.
    StillDamaged,
    /// A fresh explicit local-build approval is required.
    FreshApprovalRequired,
    /// The production repair authority was unavailable.
    AuthorityUnavailable,
}

impl RepairGenerationErrorCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidScope => "invalid-scope",
            Self::VerifyFailed => "verify-failed",
            Self::AdmissionFailed => "admission-failed",
            Self::HelperFailed => "helper-failed",
            Self::JournalFailed => "journal-failed",
            Self::StillDamaged => "still-damaged",
            Self::FreshApprovalRequired => "fresh-approval-required",
            Self::AuthorityUnavailable => "authority-unavailable",
        }
    }
}

/// Public result of one broker-owned authenticated channel refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRefreshReport {
    updated: bool,
    sequence: ChannelSequence,
}

/// Closed user intent for one authenticated metadata refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRefreshMode {
    /// Authenticate and publish the newest channel and index.
    Apply,
    /// Authenticate and report change without publishing it.
    Check,
    /// Perform the normal network refresh even when metadata is current.
    Force,
}

impl ChannelRefreshMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Check => "check",
            Self::Force => "force",
        }
    }
}

impl ChannelRefreshReport {
    /// Constructs a sanitized refresh result from broker-owned authority.
    #[must_use]
    pub const fn new(updated: bool, sequence: ChannelSequence) -> Self {
        Self { updated, sequence }
    }

    /// Whether a newer authenticated channel/index pair became current.
    #[must_use]
    pub const fn updated(self) -> bool {
        self.updated
    }

    /// Current authenticated channel sequence after the refresh.
    #[must_use]
    pub const fn sequence(self) -> ChannelSequence {
        self.sequence
    }
}

/// Stable sanitized failure categories for authenticated channel refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRefreshErrorCode {
    /// Authenticated repository bytes could not be acquired.
    Network,
    /// Signed metadata, target, rollback, or index verification refused.
    Verification,
    /// Another refresh owns the durable channel writer lease.
    Busy,
    /// Durable state or atomic authority publication is unavailable.
    ServiceUnavailable,
}

/// Stable public failure categories for cache-first acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheInstallErrorCode {
    /// The handle, operation class, or retained intent was invalid.
    InvalidIntent,
    /// Authenticated resolution, substitution, or verification failed.
    AcquisitionFailed,
    /// Lifecycle cancellation won while acquisition was in flight.
    Cancelled,
    /// Broker-owned channel or acquisition authority was unavailable.
    AuthorityUnavailable,
}

impl CacheInstallErrorCode {
    /// Returns the closed wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIntent => "invalid-intent",
            Self::AcquisitionFailed => "acquisition-failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority-unavailable",
        }
    }
}

/// Stable redacted build-execution refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildExecutionErrorCode {
    /// The exact private approval was absent, stale, consumed, or mismatched.
    ApprovalUnavailable,
    /// Admission-time replanning no longer matched the approved plan.
    ApprovalInvalidated,
    /// The broker-owned disk/load preflight refused execution.
    ResourcePreflightFailed,
    /// Managed build execution failed or returned inconsistent outputs.
    ExecutionFailed,
    /// Operation lifecycle cancellation stopped admission or execution.
    Cancelled,
    /// Other private authority state refused the request without more detail.
    AuthorityUnavailable,
}

/// Stable redacted generation-root transition refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRootTransitionErrorCode {
    /// The ownerless path-free transition intent was invalid for this operation.
    InvalidIntent,
    /// The fixed privileged helper failed to derive or publish the destination root set.
    TransitionFailed,
    /// Operation lifecycle cancellation stopped the transition.
    Cancelled,
    /// Other private authority state refused the request without more detail.
    AuthorityUnavailable,
}

/// Stable redacted generation-root removal refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRootRemovalErrorCode {
    /// The handle or typed generation did not authorize a GC mutation.
    InvalidIntent,
    /// The fixed privileged helper failed to remove the root set.
    RemovalFailed,
    /// Operation lifecycle cancellation stopped the removal.
    Cancelled,
    /// Admission or private authority was unavailable without more detail.
    AuthorityUnavailable,
}

/// Stable redacted generation-root attestation refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRootAttestationErrorCode {
    /// The handle or typed generation did not authorize attestation.
    InvalidIntent,
    /// The helper could not attest the exact durable root set.
    AttestationFailed,
    /// Operation lifecycle cancellation stopped attestation.
    Cancelled,
    /// Admission or private authority was unavailable without more detail.
    AuthorityUnavailable,
}

impl GenerationRootAttestationErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIntent => "invalid_intent",
            Self::AttestationFailed => "attestation_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

impl GenerationRootRemovalErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIntent => "invalid_intent",
            Self::RemovalFailed => "removal_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

impl GenerationRootTransitionErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIntent => "invalid_intent",
            Self::TransitionFailed => "transition_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

impl BuildExecutionErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalUnavailable => "approval_unavailable",
            Self::ApprovalInvalidated => "approval_invalidated",
            Self::ResourcePreflightFailed => "resource_preflight_failed",
            Self::ExecutionFailed => "execution_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

/// Closed protected-root-publication refusal categories exposed to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildRootPublicationErrorCode {
    /// The handle, generation, or root entries did not match retained authority.
    InvalidRootIntent,
    /// The privileged helper failed to publish the complete durable root set.
    PublicationFailed,
    /// Operation lifecycle cancellation stopped or superseded publication.
    Cancelled,
    /// The broker has no authenticated root-publishing authority available.
    AuthorityUnavailable,
}

impl BuildRootPublicationErrorCode {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRootIntent => "invalid_root_intent",
            Self::PublicationFailed => "publication_failed",
            Self::Cancelled => "cancelled",
            Self::AuthorityUnavailable => "authority_unavailable",
        }
    }
}

/// Closed user-approval pointer. It carries no receipt, target, derivation, or
/// runtime option; the broker resolves the digest only against its private plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildApprovalRequest {
    build_plan_digest: Digest,
    source: ApprovalSource,
}

impl BuildApprovalRequest {
    /// Constructs an approval pointer for one displayed private-plan digest.
    #[must_use]
    pub const fn new(build_plan_digest: Digest, source: ApprovalSource) -> Self {
        Self {
            build_plan_digest,
            source,
        }
    }

    /// Returns the exact displayed plan digest.
    #[must_use]
    pub const fn build_plan_digest(&self) -> Digest {
        self.build_plan_digest
    }

    /// Returns the explicit one-operation approval source.
    #[must_use]
    pub const fn source(&self) -> ApprovalSource {
        self.source
    }
}

/// Closed privileged requests on the broker-to-helper channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerHelperRequest {
    /// Atomically publish a complete root set.
    PublishRootSet(RootSetPublicationRequest),
    /// Remove exactly one generation root set.
    RemoveRootSet(RemoveRootSetRequest),
    /// Ask the helper to issue a capability for a verified scope.
    IssueRepairCapability(VerifiedRepairScope),
    /// Redeem one opaque capability for fixed repair execution.
    RepairStorePaths(RepairStorePathsRequest),
    /// Derive a new root set only from names in a durable source generation.
    TransitionRootSet(RootSetTransitionRequest),
    /// Attest one durable generation without accepting names or store paths.
    AttestRootSet(RootSetAttestationRequest),
    /// Load durable generation roots only for authenticated broker repair planning.
    LoadRepairRootSet(RootSetAttestationRequest),
    /// Verify exact managed-runtime ownership against an authenticated manifest digest.
    VerifyManagedOwnership(Digest),
    /// Execute one fixed typed Nix operation in the privileged helper.
    RootNix(RootNixRequest),
}

/// Closed privileged responses on the helper-to-broker channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerHelperResponse {
    /// A generation root set was durably published.
    RootSetPublished(RootSetReport),
    /// One generation root set was removed or already absent.
    RootSetRemoved,
    /// A fresh opaque repair capability was issued.
    RepairCapabilityIssued(MaintenanceCapability),
    /// Fixed repair execution completed with sanitized outcomes.
    RepairCompleted(RepairStorePathsReport),
    /// A path-free generation transition was durably published.
    RootSetTransitioned(RootSetTransitionReport),
    /// One durable generation was reconstructed and exactly attested.
    RootSetAttested(RootSetReport),
    /// Exact durable roots returned only to the authenticated broker.
    RepairRootSetLoaded(RootSet),
    /// Whether every managed runtime asset was authenticated.
    ManagedOwnership(bool),
    /// Return one fixed typed Nix result from the privileged helper.
    RootNix(Box<RootNixResponse>),
}

/// Exact V1 product frame codec.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProductFrameCodec;

impl ProductFrameCodec {
    /// Encodes one closed CLI-to-broker lifecycle request.
    pub fn encode_cli_request(
        request_id: u64,
        request: &CliBrokerRequest,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match request {
            CliBrokerRequest::Begin(kind) => (
                1,
                encode_json(&BeginWire {
                    operation: operation_name(*kind),
                })?,
            ),
            CliBrokerRequest::Poll(handle) => (
                2,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Cancel(handle) => (
                3,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Complete(handle) => (
                4,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::Version(handle) => (
                10,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::EvaluateDerivation(handle, request) => (
                11,
                encode_handle_body(handle, &request.encode().map_err(adapter_payload)?)?,
            ),
            CliBrokerRequest::PathInfo(handle, path) => (
                12,
                encode_json(&HandlePathWire {
                    handle: handle.as_str(),
                    path: path.as_str(),
                })?,
            ),
            CliBrokerRequest::Substitute(handle, path) => (
                13,
                encode_json(&HandlePathWire {
                    handle: handle.as_str(),
                    path: path.as_str(),
                })?,
            ),
            CliBrokerRequest::ApproveBuild(handle, approval) => (
                14,
                encode_json(&BuildApprovalWire {
                    handle: handle.as_str(),
                    build_plan_digest: approval.build_plan_digest.to_string(),
                    source: approval_source_name(approval.source),
                })?,
            ),
            CliBrokerRequest::Verify(handle, request) => (
                15,
                encode_handle_body(handle, &request.encode().map_err(adapter_payload)?)?,
            ),
            CliBrokerRequest::Gc(handle) => (
                16,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::GetBuildPreview(handle) => (
                17,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::PrepareBuild(handle, selectors) => {
                validate_prepare_selectors(selectors)?;
                (
                    18,
                    encode_json(&PrepareBuildWire {
                        handle: handle.as_str(),
                        selectors: selectors.iter().map(SelectorWire::from).collect(),
                    })?,
                )
            }
            CliBrokerRequest::ExecuteBuild(handle, digest) => (
                19,
                encode_json(&BuildExecutionWire {
                    handle: handle.as_str(),
                    build_plan_digest: digest.to_string(),
                })?,
            ),
            CliBrokerRequest::PublishBuildRoots(handle, intent) => (
                20,
                encode_json(&BuildRootIntentWire {
                    handle: handle.as_str(),
                    source_generation: intent.source_generation().map(GenerationId::as_str),
                    generation: intent.generation().as_str(),
                    entries: intent
                        .entries()
                        .iter()
                        .map(|entry| RootSetEntryWire {
                            name: entry.name().as_str(),
                            target: entry.target().as_str(),
                        })
                        .collect(),
                    added_names: intent.added_names().iter().map(RootName::as_str).collect(),
                })?,
            ),
            CliBrokerRequest::TransitionGenerationRoots(handle, intent) => (
                21,
                encode_json(&GenerationRootTransitionWire {
                    handle: handle.as_str(),
                    source_generation: intent.source_generation().as_str(),
                    destination_generation: intent.destination_generation().as_str(),
                    retained_names: intent
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                })?,
            ),
            CliBrokerRequest::RemoveGenerationRoots(handle, generation) => (
                22,
                encode_json(&GenerationRootRemovalWire {
                    handle: handle.as_str(),
                    generation: generation.as_str(),
                })?,
            ),
            CliBrokerRequest::AcquireGc(handle) => (
                23,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::GetInstallEvidence(handle) => (
                24,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerRequest::AttestGenerationRoots(handle, generation) => (
                25,
                encode_json(&GenerationRootRemovalWire {
                    handle: handle.as_str(),
                    generation: generation.as_str(),
                })?,
            ),
            CliBrokerRequest::AcquireInstall(handle, selectors) => {
                validate_prepare_selectors(selectors)?;
                (
                    26,
                    encode_json(&PrepareBuildWire {
                        handle: handle.as_str(),
                        selectors: selectors.iter().map(SelectorWire::from).collect(),
                    })?,
                )
            }
            CliBrokerRequest::RefreshChannel(handle, mode) => (
                27,
                encode_json(&ChannelRefreshRequestWire {
                    handle: handle.as_str(),
                    mode: mode.as_str(),
                })?,
            ),
            CliBrokerRequest::SearchCatalog(handle, request) => (
                28,
                encode_json(&CatalogSearchRequestWire {
                    handle: handle.as_str(),
                    query: request.query(),
                    limit: request.limit(),
                    exact: request.exact(),
                    license: request.license(),
                })?,
            ),
            CliBrokerRequest::InfoCatalog(handle, requests) => {
                if requests.is_empty() || requests.len() > MAX_CATALOG_INFO_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                (
                    29,
                    encode_json(&CatalogInfoRequestWire {
                        handle: handle.as_str(),
                        selectors: requests.iter().map(CatalogInfoRequest::selector).collect(),
                    })?,
                )
            }
            CliBrokerRequest::RepairGeneration(handle, request) => (
                30,
                encode_json(&RepairGenerationRequestWire {
                    handle: handle.as_str(),
                    generation: request.generation().as_str(),
                    verify_only: request.verify_only(),
                    build_plan_digest: request
                        .approval()
                        .map(|approval| approval.build_plan_digest().to_string()),
                    approval_source: request
                        .approval()
                        .map(|approval| approval_source_name(approval.source())),
                })?,
            ),
            CliBrokerRequest::VerifyManagedOwnership(handle) => (
                31,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
        };
        encode_frame(CHANNEL_CLI_BROKER, method, request_id, &payload)
    }

    /// Decodes one exact CLI-to-broker lifecycle request.
    pub fn decode_cli_request(bytes: &[u8]) -> Result<(u64, CliBrokerRequest), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_CLI_BROKER)?;
        let request = match frame.method {
            1 => {
                let wire: BeginOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::Begin(parse_operation(&wire.operation)?)
            }
            2 | 3 | 4 | 10 | 16 | 17 | 23 | 24 | 31 => {
                let wire: HandleOwnedWire = decode_json(frame.payload)?;
                let handle = parse_handle(&wire.handle)?;
                match frame.method {
                    2 => CliBrokerRequest::Poll(handle),
                    3 => CliBrokerRequest::Cancel(handle),
                    4 => CliBrokerRequest::Complete(handle),
                    10 => CliBrokerRequest::Version(handle),
                    16 => CliBrokerRequest::Gc(handle),
                    17 => CliBrokerRequest::GetBuildPreview(handle),
                    23 => CliBrokerRequest::AcquireGc(handle),
                    24 => CliBrokerRequest::GetInstallEvidence(handle),
                    31 => CliBrokerRequest::VerifyManagedOwnership(handle),
                    _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
                }
            }
            27 => {
                let wire: ChannelRefreshRequestOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::RefreshChannel(
                    parse_handle(&wire.handle)?,
                    parse_channel_refresh_mode(&wire.mode)?,
                )
            }
            11 | 15 => {
                let (handle, body) = decode_handle_body(frame.payload)?;
                let codec = JsonCodec::default();
                match frame.method {
                    11 => CliBrokerRequest::EvaluateDerivation(
                        handle,
                        EvaluateDerivationRequest::decode(&codec, body.get().as_bytes())
                            .map_err(adapter_payload)?,
                    ),
                    15 => CliBrokerRequest::Verify(
                        handle,
                        VerifyRequest::decode(&codec, body.get().as_bytes())
                            .map_err(adapter_payload)?,
                    ),
                    _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
                }
            }
            12 | 13 => {
                let wire: HandlePathOwnedWire = decode_json(frame.payload)?;
                let handle = parse_handle(&wire.handle)?;
                let path = StorePath::new(&wire.path).map_err(adapter_payload)?;
                if frame.method == 12 {
                    CliBrokerRequest::PathInfo(handle, path)
                } else {
                    CliBrokerRequest::Substitute(handle, path)
                }
            }
            14 => {
                let wire: BuildApprovalOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::ApproveBuild(
                    parse_handle(&wire.handle)?,
                    BuildApprovalRequest::new(
                        Digest::from_str(&wire.build_plan_digest)
                            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                        parse_approval_source(&wire.source)?,
                    ),
                )
            }
            18 | 26 => {
                let wire: PrepareBuildOwnedWire = decode_json(frame.payload)?;
                if wire.selectors.is_empty()
                    || wire.selectors.len() > MAX_BUILD_SELECTOR_WIRE_ENTRIES
                {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                let handle = parse_handle(&wire.handle)?;
                let selectors = wire
                    .selectors
                    .into_iter()
                    .map(SelectorOwnedWire::promote)
                    .collect::<Result<Vec<_>, _>>()?;
                if frame.method == 18 {
                    CliBrokerRequest::PrepareBuild(handle, selectors)
                } else {
                    CliBrokerRequest::AcquireInstall(handle, selectors)
                }
            }
            19 => {
                let wire: BuildExecutionOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::ExecuteBuild(
                    parse_handle(&wire.handle)?,
                    Digest::from_str(&wire.build_plan_digest)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                )
            }
            20 => {
                let wire: BuildRootIntentOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::PublishBuildRoots(parse_handle(&wire.handle)?, wire.promote()?)
            }
            21 => {
                let wire: GenerationRootTransitionOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::TransitionGenerationRoots(
                    parse_handle(&wire.handle)?,
                    wire.promote()?,
                )
            }
            22 => {
                let wire: GenerationRootRemovalOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::RemoveGenerationRoots(
                    parse_handle(&wire.handle)?,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                )
            }
            25 => {
                let wire: GenerationRootRemovalOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::AttestGenerationRoots(
                    parse_handle(&wire.handle)?,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                )
            }
            28 => {
                let wire: CatalogSearchRequestOwnedWire = decode_json(frame.payload)?;
                CliBrokerRequest::SearchCatalog(
                    parse_handle(&wire.handle)?,
                    CatalogSearchRequest::new(
                        &wire.query,
                        wire.limit,
                        wire.exact,
                        wire.license.as_deref(),
                    )
                    .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?,
                )
            }
            29 => {
                let wire: CatalogInfoRequestOwnedWire = decode_json(frame.payload)?;
                if wire.selectors.is_empty() || wire.selectors.len() > MAX_CATALOG_INFO_WIRE_ENTRIES
                {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                CliBrokerRequest::InfoCatalog(
                    parse_handle(&wire.handle)?,
                    wire.selectors
                        .into_iter()
                        .map(|selector| {
                            CatalogInfoRequest::new(&selector)
                                .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            30 => {
                let wire: RepairGenerationRequestOwnedWire = decode_json(frame.payload)?;
                let generation = GenerationId::new(&wire.generation)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                let request = match (wire.build_plan_digest, wire.approval_source) {
                    (None, None) => RepairGenerationRequest::new(generation, wire.verify_only),
                    (Some(digest), Some(source)) if !wire.verify_only => {
                        RepairGenerationRequest::with_approval(
                            generation,
                            BuildApprovalRequest::new(
                                Digest::from_str(&digest)
                                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                                parse_approval_source(&source)?,
                            ),
                        )
                    }
                    _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
                };
                CliBrokerRequest::RepairGeneration(parse_handle(&wire.handle)?, request)
            }
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, request))
    }

    /// Encodes one closed broker-to-CLI lifecycle response.
    pub fn encode_cli_response(
        request_id: u64,
        response: &CliBrokerResponse,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match response {
            CliBrokerResponse::Started(handle) => (
                1,
                encode_json(&HandleWire {
                    handle: handle.as_str(),
                })?,
            ),
            CliBrokerResponse::Status(status) => (
                2,
                encode_json(&StatusWire {
                    status: status_name(*status),
                })?,
            ),
            CliBrokerResponse::Cancelled => (3, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Completed => (4, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Version(report) => (
                10,
                report
                    .encode()
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            CliBrokerResponse::ManagedOwnership(verified) => (
                31,
                encode_json(&ManagedOwnershipWire {
                    verified: *verified,
                })?,
            ),
            CliBrokerResponse::DerivationPlan(report) => {
                (11, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::PathInfo(report) => (12, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::Substitute(report) => {
                (13, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::BuildApproved => (14, encode_json(&EmptyWire {})?),
            CliBrokerResponse::Verify(report) => (15, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::Gc(report) => (16, report.encode().map_err(adapter_payload)?),
            CliBrokerResponse::BuildPreview(preview) => (17, encode_build_preview(preview)?),
            CliBrokerResponse::BuildPrepared(preview) => (18, encode_build_preview(preview)?),
            CliBrokerResponse::BuildPreparationRefused(code) => (
                18,
                encode_json(&BuildPreparationFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::BuildExecuted(report) => {
                (19, report.encode().map_err(adapter_payload)?)
            }
            CliBrokerResponse::BuildExecutionProgress(progress) => (
                19,
                encode_json(&BuildExecutionProgressWire {
                    progress_millionths: progress.millionths(),
                })?,
            ),
            CliBrokerResponse::BuildExecutionRefused(code) => (
                19,
                encode_json(&BuildExecutionFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::BuildRootsPublished(report) => (
                20,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            CliBrokerResponse::BuildRootPublicationRefused(code) => (
                20,
                encode_json(&BuildRootPublicationFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::GenerationRootsTransitioned(report) => (
                21,
                encode_json(&RootSetTransitionReportWire {
                    reference: report.root_set().reference().as_str(),
                    entry_count: report.root_set().entry_count(),
                    retained_names: report
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            CliBrokerResponse::GenerationRootTransitionRefused(code) => (
                21,
                encode_json(&GenerationRootTransitionFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::GenerationRootsRemoved => (22, encode_json(&EmptyWire {})?),
            CliBrokerResponse::GenerationRootRemovalRefused(code) => (
                22,
                encode_json(&GenerationRootRemovalFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::GcAdmissionAcquired => (23, encode_json(&EmptyWire {})?),
            CliBrokerResponse::InstallEvidence(evidence) => (
                24,
                evidence
                    .to_json_bytes()
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            CliBrokerResponse::GenerationRootsAttested(report) => (
                25,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            CliBrokerResponse::GenerationRootAttestationRefused(code) => (
                25,
                encode_json(&GenerationRootAttestationFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::InstallAcquired => (
                26,
                encode_json(&InstallAcquisitionWire {
                    outcome: "acquired",
                })?,
            ),
            CliBrokerResponse::InstallBuildRequired => (
                26,
                encode_json(&InstallAcquisitionWire {
                    outcome: "build-required",
                })?,
            ),
            CliBrokerResponse::InstallAcquisitionRefused(code) => (
                26,
                encode_json(&InstallAcquisitionFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::InstallDownloadProgress(progress) => (
                26,
                encode_json(&InstallDownloadProgressWire {
                    selector: progress.selector().as_str(),
                    done: progress.done(),
                    total: progress.total(),
                })?,
            ),
            CliBrokerResponse::ChannelRefreshed(report) => (
                27,
                encode_json(&ChannelRefreshWire {
                    updated: report.updated(),
                    sequence: report.sequence().get().get(),
                })?,
            ),
            CliBrokerResponse::ChannelRefreshRefused(code) => (
                27,
                encode_json(&ChannelRefreshFailureWire {
                    error: channel_refresh_error_name(*code),
                })?,
            ),
            CliBrokerResponse::CatalogSearch(report) => (
                28,
                encode_json(&CatalogSearchResponseWire::from_report(report))?,
            ),
            CliBrokerResponse::CatalogInfo(reports) => {
                if reports.is_empty() || reports.len() > MAX_CATALOG_INFO_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                (
                    29,
                    encode_json(
                        &reports
                            .iter()
                            .map(CatalogInfoResponseWire::from_report)
                            .collect::<Vec<_>>(),
                    )?,
                )
            }
            CliBrokerResponse::CatalogSearchRefused => (
                28,
                encode_json(&CatalogQueryFailureWire {
                    error: "unavailable",
                })?,
            ),
            CliBrokerResponse::CatalogInfoRefused => (
                29,
                encode_json(&CatalogQueryFailureWire {
                    error: "unavailable",
                })?,
            ),
            CliBrokerResponse::RepairGeneration(report) => (
                30,
                encode_json(&RepairGenerationReportWire {
                    status: repair_generation_status_name(report.status()),
                    damaged_paths: report.damaged_paths(),
                    build_preview: report.build_preview(),
                })?,
            ),
            CliBrokerResponse::RepairGenerationRefused(code) => (
                30,
                encode_json(&RepairGenerationFailureWire {
                    error: code.as_str(),
                })?,
            ),
            CliBrokerResponse::AdapterFailure(method, code) => (
                cli_adapter_method(*method)
                    .ok_or_else(|| FrameError::new(FrameErrorCode::UnsupportedMessage))?,
                encode_json(&AdapterFailureWire {
                    error: code.as_str(),
                })?,
            ),
        };
        encode_frame(CHANNEL_CLI_BROKER, method, request_id, &payload)
    }

    /// Decodes one exact broker-to-CLI lifecycle response.
    pub fn decode_cli_response(bytes: &[u8]) -> Result<(u64, CliBrokerResponse), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_CLI_BROKER)?;
        if let Some(method) = adapter_method(frame.method)
            && let Some(code) = decode_adapter_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::AdapterFailure(method, code),
            ));
        }
        if frame.method == 19
            && let Some(code) = decode_build_execution_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildExecutionRefused(code),
            ));
        }
        if frame.method == 18
            && let Some(code) = decode_build_preparation_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildPreparationRefused(code),
            ));
        }
        if frame.method == 19
            && let Some(progress) = decode_build_execution_progress(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildExecutionProgress(progress),
            ));
        }
        if frame.method == 20
            && let Some(code) = decode_build_root_publication_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::BuildRootPublicationRefused(code),
            ));
        }
        if frame.method == 21
            && let Some(code) = decode_generation_root_transition_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::GenerationRootTransitionRefused(code),
            ));
        }
        if frame.method == 22
            && let Some(code) = decode_generation_root_removal_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::GenerationRootRemovalRefused(code),
            ));
        }
        if frame.method == 25
            && let Some(code) = decode_generation_root_attestation_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::GenerationRootAttestationRefused(code),
            ));
        }
        if frame.method == 30
            && let Some(code) = decode_repair_generation_failure(frame.payload)?
        {
            return Ok((
                frame.request_id,
                CliBrokerResponse::RepairGenerationRefused(code),
            ));
        }
        let response = match frame.method {
            1 => {
                let wire: HandleOwnedWire = decode_json(frame.payload)?;
                CliBrokerResponse::Started(parse_handle(&wire.handle)?)
            }
            2 => {
                let wire: StatusOwnedWire = decode_json(frame.payload)?;
                CliBrokerResponse::Status(parse_status(&wire.status)?)
            }
            3 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::Cancelled
            }
            4 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::Completed
            }
            10 => CliBrokerResponse::Version(
                VersionInfo::decode(&JsonCodec::default(), frame.payload)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            31 => {
                let wire: ManagedOwnershipWire = decode_json(frame.payload)?;
                CliBrokerResponse::ManagedOwnership(wire.verified)
            }
            11 => CliBrokerResponse::DerivationPlan(
                DerivationPlanReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            12 => CliBrokerResponse::PathInfo(
                PathInfoReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            13 => CliBrokerResponse::Substitute(
                SubstituteReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            14 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::BuildApproved
            }
            15 => CliBrokerResponse::Verify(
                VerifyReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            16 => CliBrokerResponse::Gc(
                GcReport::decode(&JsonCodec::default(), frame.payload).map_err(adapter_payload)?,
            ),
            17 => CliBrokerResponse::BuildPreview(decode_build_preview(frame.payload)?),
            18 => CliBrokerResponse::BuildPrepared(decode_build_preview(frame.payload)?),
            19 => CliBrokerResponse::BuildExecuted(
                BuildReport::decode(&JsonCodec::default(), frame.payload)
                    .map_err(adapter_payload)?,
            ),
            20 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                CliBrokerResponse::BuildRootsPublished(RootSetReport::new(
                    reference,
                    wire.entry_count,
                    parse_mapping_digest(&wire.mapping_digest)?,
                ))
            }
            21 => CliBrokerResponse::GenerationRootsTransitioned(
                decode_json::<RootSetTransitionReportOwnedWire>(frame.payload)?.promote()?,
            ),
            22 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::GenerationRootsRemoved
            }
            23 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                CliBrokerResponse::GcAdmissionAcquired
            }
            24 => CliBrokerResponse::InstallEvidence(
                InstallEvidence::from_json_bytes(frame.payload)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ),
            25 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                CliBrokerResponse::GenerationRootsAttested(RootSetReport::new(
                    reference,
                    wire.entry_count,
                    parse_mapping_digest(&wire.mapping_digest)?,
                ))
            }
            26 => {
                if let Some(code) = decode_install_acquisition_failure(frame.payload)? {
                    CliBrokerResponse::InstallAcquisitionRefused(code)
                } else if let Some(progress) = decode_install_download_progress(frame.payload)? {
                    CliBrokerResponse::InstallDownloadProgress(progress)
                } else {
                    let wire: InstallAcquisitionOwnedWire = decode_json(frame.payload)?;
                    match wire.outcome.as_str() {
                        "acquired" => CliBrokerResponse::InstallAcquired,
                        "build-required" => CliBrokerResponse::InstallBuildRequired,
                        _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
                    }
                }
            }
            27 => {
                if let Some(code) = decode_channel_refresh_failure(frame.payload)? {
                    CliBrokerResponse::ChannelRefreshRefused(code)
                } else {
                    let wire: ChannelRefreshOwnedWire = decode_json(frame.payload)?;
                    let sequence = ChannelSequence::from_u64(wire.sequence)
                        .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
                    CliBrokerResponse::ChannelRefreshed(ChannelRefreshReport::new(
                        wire.updated,
                        sequence,
                    ))
                }
            }
            28 => {
                if decode_catalog_query_failure(frame.payload)? {
                    CliBrokerResponse::CatalogSearchRefused
                } else {
                    CliBrokerResponse::CatalogSearch(
                        decode_json::<CatalogSearchResponseOwnedWire>(frame.payload)?.promote()?,
                    )
                }
            }
            29 => {
                if decode_catalog_query_failure(frame.payload)? {
                    CliBrokerResponse::CatalogInfoRefused
                } else {
                    let wires = decode_json::<Vec<CatalogInfoResponseOwnedWire>>(frame.payload)?;
                    if wires.is_empty() || wires.len() > MAX_CATALOG_INFO_WIRE_ENTRIES {
                        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                    }
                    CliBrokerResponse::CatalogInfo(
                        wires
                            .into_iter()
                            .map(CatalogInfoResponseOwnedWire::promote)
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
            }
            30 => {
                let wire: RepairGenerationReportOwnedWire = decode_json(frame.payload)?;
                let status = parse_repair_generation_status(&wire.status)?;
                let report = match (status, wire.build_preview) {
                    (RepairGenerationStatus::NeedsApproval, Some(preview)) => {
                        preview
                            .validate()
                            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                        RepairGenerationReport::needs_approval(wire.damaged_paths, preview)?
                    }
                    (RepairGenerationStatus::NeedsApproval, None) | (_, Some(_)) => {
                        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                    }
                    (_, None) => RepairGenerationReport::new(status, wire.damaged_paths)?,
                };
                CliBrokerResponse::RepairGeneration(report)
            }
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, response))
    }

    /// Encodes one closed broker-to-helper request.
    pub fn encode_helper_request(
        request_id: u64,
        request: &BrokerHelperRequest,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match request {
            BrokerHelperRequest::PublishRootSet(request) => (
                1,
                encode_json(&RootSetPublicationWire::from_request(request))?,
            ),
            BrokerHelperRequest::RemoveRootSet(request) => (
                2,
                encode_json(&RemoveRootSetWire {
                    owner_uid: request.owner_uid(),
                    generation: request.generation().as_str(),
                })?,
            ),
            BrokerHelperRequest::IssueRepairCapability(scope) => {
                (3, encode_json(&RepairScopeWire::from_scope(scope))?)
            }
            BrokerHelperRequest::RepairStorePaths(request) => (
                4,
                encode_json(&CapabilityWire {
                    capability: request.capability().as_str(),
                })?,
            ),
            BrokerHelperRequest::TransitionRootSet(request) => (
                5,
                encode_json(&RootSetTransitionWire {
                    owner_uid: request.owner_uid(),
                    source_generation: request.source_generation().as_str(),
                    destination_generation: request.destination_generation().as_str(),
                    retained_names: request
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                })?,
            ),
            BrokerHelperRequest::AttestRootSet(request) => (
                6,
                encode_json(&RemoveRootSetWire {
                    owner_uid: request.owner_uid(),
                    generation: request.generation().as_str(),
                })?,
            ),
            BrokerHelperRequest::LoadRepairRootSet(request) => (
                7,
                encode_json(&RemoveRootSetWire {
                    owner_uid: request.owner_uid(),
                    generation: request.generation().as_str(),
                })?,
            ),
            BrokerHelperRequest::VerifyManagedOwnership(digest) => (
                8,
                encode_json(&ManifestDigestWire {
                    asset_manifest_digest: digest.to_string(),
                })?,
            ),
            BrokerHelperRequest::RootNix(request) => (
                request.operation().method_id(),
                encode_root_nix_request(request)?,
            ),
        };
        encode_frame(CHANNEL_BROKER_HELPER, method, request_id, &payload)
    }

    /// Decodes one exact broker-to-helper request.
    pub fn decode_helper_request(bytes: &[u8]) -> Result<(u64, BrokerHelperRequest), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_BROKER_HELPER)?;
        let request = match frame.method {
            1 => BrokerHelperRequest::PublishRootSet(
                decode_json::<RootSetPublicationOwnedWire>(frame.payload)?.promote()?,
            ),
            2 => {
                let wire: RemoveRootSetOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::RemoveRootSet(RemoveRootSetRequest::new(
                    wire.owner_uid,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            }
            3 => BrokerHelperRequest::IssueRepairCapability(
                decode_json::<RepairScopeOwnedWire>(frame.payload)?.promote()?,
            ),
            4 => {
                let wire: CapabilityOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::RepairStorePaths(RepairStorePathsRequest::new(
                    parse_capability(&wire.capability)?,
                ))
            }
            5 => BrokerHelperRequest::TransitionRootSet(
                decode_json::<RootSetTransitionOwnedWire>(frame.payload)?.promote()?,
            ),
            6 => {
                let wire: RemoveRootSetOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::AttestRootSet(RootSetAttestationRequest::new(
                    wire.owner_uid,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            }
            7 => {
                let wire: RemoveRootSetOwnedWire = decode_json(frame.payload)?;
                BrokerHelperRequest::LoadRepairRootSet(RootSetAttestationRequest::new(
                    wire.owner_uid,
                    GenerationId::new(&wire.generation)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            }
            8 => {
                let wire: ManifestDigestWire = decode_json(frame.payload)?;
                BrokerHelperRequest::VerifyManagedOwnership(parse_mapping_digest(
                    &wire.asset_manifest_digest,
                )?)
            }
            20..=32 => BrokerHelperRequest::RootNix(decode_root_nix_request(
                RootNixOperation::from_method_id(frame.method)
                    .ok_or_else(|| FrameError::new(FrameErrorCode::UnsupportedMessage))?,
                frame.payload,
            )?),
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, request))
    }

    /// Encodes one closed helper-to-broker response.
    pub fn encode_helper_response(
        request_id: u64,
        response: &BrokerHelperResponse,
    ) -> Result<Vec<u8>, FrameError> {
        let (method, payload) = match response {
            BrokerHelperResponse::RootSetPublished(report) => (
                1,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            BrokerHelperResponse::RootSetRemoved => (2, encode_json(&EmptyWire {})?),
            BrokerHelperResponse::RepairCapabilityIssued(capability) => (
                3,
                encode_json(&CapabilityWire {
                    capability: capability.as_str(),
                })?,
            ),
            BrokerHelperResponse::RepairCompleted(report) => {
                (4, encode_json(&RepairReportWire::from_report(report))?)
            }
            BrokerHelperResponse::RootSetTransitioned(report) => (
                5,
                encode_json(&RootSetTransitionReportWire {
                    reference: report.root_set().reference().as_str(),
                    entry_count: report.root_set().entry_count(),
                    retained_names: report
                        .retained_names()
                        .iter()
                        .map(RootName::as_str)
                        .collect(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            BrokerHelperResponse::RootSetAttested(report) => (
                6,
                encode_json(&RootSetReportWire {
                    reference: report.reference().as_str(),
                    entry_count: report.entry_count(),
                    mapping_digest: report.mapping_digest().to_string(),
                })?,
            ),
            BrokerHelperResponse::RepairRootSetLoaded(root_set) => {
                (7, encode_json(&RootSetWire::from_root_set(root_set))?)
            }
            BrokerHelperResponse::ManagedOwnership(verified) => (
                8,
                encode_json(&ManagedOwnershipWire {
                    verified: *verified,
                })?,
            ),
            BrokerHelperResponse::RootNix(response) => (
                response.operation().method_id(),
                encode_root_nix_response(response)?,
            ),
        };
        encode_frame(CHANNEL_BROKER_HELPER, method, request_id, &payload)
    }

    /// Decodes one exact helper-to-broker response.
    pub fn decode_helper_response(bytes: &[u8]) -> Result<(u64, BrokerHelperResponse), FrameError> {
        let frame = decode_frame(bytes, CHANNEL_BROKER_HELPER)?;
        let response = match frame.method {
            1 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                BrokerHelperResponse::RootSetPublished(RootSetReport::new(
                    reference,
                    wire.entry_count,
                    parse_mapping_digest(&wire.mapping_digest)?,
                ))
            }
            2 => {
                let _: EmptyWire = decode_json(frame.payload)?;
                BrokerHelperResponse::RootSetRemoved
            }
            3 => {
                let wire: CapabilityOwnedWire = decode_json(frame.payload)?;
                BrokerHelperResponse::RepairCapabilityIssued(parse_capability(&wire.capability)?)
            }
            4 => BrokerHelperResponse::RepairCompleted(
                decode_json::<RepairReportOwnedWire>(frame.payload)?.promote()?,
            ),
            5 => BrokerHelperResponse::RootSetTransitioned(
                decode_json::<RootSetTransitionReportOwnedWire>(frame.payload)?.promote()?,
            ),
            6 => {
                let wire: RootSetReportOwnedWire = decode_json(frame.payload)?;
                let reference = RootRef::new(&wire.reference)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                if wire.entry_count == 0 || wire.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
                    return Err(FrameError::new(FrameErrorCode::InvalidPayload));
                }
                BrokerHelperResponse::RootSetAttested(RootSetReport::new(
                    reference,
                    wire.entry_count,
                    parse_mapping_digest(&wire.mapping_digest)?,
                ))
            }
            7 => BrokerHelperResponse::RepairRootSetLoaded(
                decode_json::<RootSetOwnedWire>(frame.payload)?.promote()?,
            ),
            8 => {
                let wire: ManagedOwnershipWire = decode_json(frame.payload)?;
                BrokerHelperResponse::ManagedOwnership(wire.verified)
            }
            20..=32 => BrokerHelperResponse::RootNix(Box::new(decode_root_nix_response(
                RootNixOperation::from_method_id(frame.method)
                    .ok_or_else(|| FrameError::new(FrameErrorCode::UnsupportedMessage))?,
                frame.payload,
            )?)),
            _ => return Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
        };
        Ok((frame.request_id, response))
    }
}

const ROOT_NIX_SUCCESS: u8 = 0;
const ROOT_NIX_ADAPTER_ERROR: u8 = 1;
const ROOT_NIX_CACHE_ERROR: u8 = 2;
const ROOT_NIX_NIXPKGS_ERROR: u8 = 3;
const ROOT_NIX_BUSY: u8 = 4;
const ROOT_NIX_INACTIVE: u8 = 5;
const ROOT_NIX_BUILD_PROGRESS: u8 = 6;
const MAX_ROOT_NIX_PATHS: usize = 16_384;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootNixPathsWire<'a> {
    paths: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootNixPathsOwnedWire {
    paths: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootNixPinWire<'a> {
    revision: &'a str,
    nar_hash: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootNixPinOwnedWire {
    revision: String,
    nar_hash: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootNixReadinessWire {
    sandbox_enabled: bool,
    sandbox_fallback: bool,
    build_users_ready: bool,
    use_cgroups_enabled: bool,
    cgroup_v2_ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootRepairPlanWire<'a> {
    paths: Vec<&'a str>,
    policy_version: u64,
    system: &'a str,
    readiness: RootNixReadinessWire,
    host_cores: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootRepairPlanOwnedWire {
    paths: Vec<String>,
    policy_version: u64,
    system: String,
    readiness: RootNixReadinessWire,
    host_cores: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheObservationWire<'a> {
    path: &'a str,
    download_bytes: Option<u64>,
    nar_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CacheObservationOwnedWire {
    path: String,
    download_bytes: Option<u64>,
    nar_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheClosureWire<'a> {
    root: &'a str,
    paths: Vec<CacheObservationWire<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CacheClosureOwnedWire {
    root: String,
    paths: Vec<CacheObservationOwnedWire>,
}

fn encode_root_nix_request(request: &RootNixRequest) -> Result<Vec<u8>, FrameError> {
    match request {
        RootNixRequest::Version | RootNixRequest::Gc => Ok(Vec::new()),
        RootNixRequest::Evaluate(request) => request.encode().map_err(adapter_payload),
        RootNixRequest::PathInfo(path) | RootNixRequest::Substitute(path) => {
            encode_root_paths(std::slice::from_ref(path))
        }
        RootNixRequest::SubstituteMany(paths)
        | RootNixRequest::CacheInspect(paths)
        | RootNixRequest::CacheInspectClosures(paths)
        | RootNixRequest::ClosureForRoots(paths) => encode_root_paths(paths),
        RootNixRequest::Build(request) => request.encode().map_err(adapter_payload),
        RootNixRequest::Verify(request) => request.encode().map_err(adapter_payload),
        RootNixRequest::NixpkgsMetadata(pin) => encode_json(&RootNixPinWire {
            revision: pin.revision().as_str(),
            nar_hash: pin.nar_hash().as_str(),
        }),
        RootNixRequest::RepairPlan(request) => encode_json(&RootRepairPlanWire {
            paths: request.damaged().iter().map(StorePath::as_str).collect(),
            policy_version: request.policy_version().get().get(),
            system: request.system().as_str(),
            readiness: readiness_wire(request.readiness()),
            host_cores: request.host_cores(),
        }),
    }
}

fn decode_root_nix_request(
    operation: RootNixOperation,
    payload: &[u8],
) -> Result<RootNixRequest, FrameError> {
    let codec = JsonCodec::production();
    match operation {
        RootNixOperation::Version => {
            require_empty(payload)?;
            Ok(RootNixRequest::Version)
        }
        RootNixOperation::Evaluate => Ok(RootNixRequest::Evaluate(
            EvaluateDerivationRequest::decode(&codec, payload).map_err(adapter_payload)?,
        )),
        RootNixOperation::PathInfo => Ok(RootNixRequest::PathInfo(decode_one_path(payload)?)),
        RootNixOperation::Substitute => Ok(RootNixRequest::Substitute(decode_one_path(payload)?)),
        RootNixOperation::SubstituteMany => {
            Ok(RootNixRequest::SubstituteMany(decode_root_paths(payload)?))
        }
        RootNixOperation::Build => Ok(RootNixRequest::Build(
            crate::BuildRequest::decode(&codec, payload).map_err(adapter_payload)?,
        )),
        RootNixOperation::Verify => Ok(RootNixRequest::Verify(
            VerifyRequest::decode(&codec, payload).map_err(adapter_payload)?,
        )),
        RootNixOperation::Gc => {
            require_empty(payload)?;
            Ok(RootNixRequest::Gc)
        }
        RootNixOperation::CacheInspect => {
            Ok(RootNixRequest::CacheInspect(decode_root_paths(payload)?))
        }
        RootNixOperation::CacheInspectClosures => Ok(RootNixRequest::CacheInspectClosures(
            decode_root_paths(payload)?,
        )),
        RootNixOperation::NixpkgsMetadata => {
            let pin: RootNixPinOwnedWire = decode_json(payload)?;
            Ok(RootNixRequest::NixpkgsMetadata(
                NixpkgsPin::new(&pin.revision, &pin.nar_hash)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ))
        }
        RootNixOperation::ClosureForRoots => {
            Ok(RootNixRequest::ClosureForRoots(decode_root_paths(payload)?))
        }
        RootNixOperation::RepairPlan => {
            let wire: RootRepairPlanOwnedWire = decode_json(payload)?;
            let paths = promote_paths(wire.paths)?;
            let policy_version = PolicyVersion::from_u64(wire.policy_version)
                .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
            let system = System::from_str(&wire.system)
                .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
            let readiness = BuildReadiness::new(
                wire.readiness.sandbox_enabled,
                wire.readiness.sandbox_fallback,
                wire.readiness.build_users_ready,
                wire.readiness.use_cgroups_enabled,
                wire.readiness.cgroup_v2_ready,
            );
            Ok(RootNixRequest::RepairPlan(
                RootRepairPlanRequest::new(
                    paths,
                    policy_version,
                    system,
                    readiness,
                    wire.host_cores,
                )
                .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?,
            ))
        }
    }
}

fn encode_root_nix_response(response: &RootNixResponse) -> Result<Vec<u8>, FrameError> {
    let body = match response {
        RootNixResponse::Version(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::Evaluate(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::PathInfo(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::Substitute(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::SubstituteMany(values) => encode_chunks(
            values
                .iter()
                .map(|value| value.encode().map_err(adapter_payload))
                .collect::<Result<Vec<_>, _>>()?,
        )?,
        RootNixResponse::BuildProgress(value) => {
            let mut bytes = vec![ROOT_NIX_BUILD_PROGRESS];
            bytes.extend_from_slice(&value.millionths().to_be_bytes());
            return Ok(bytes);
        }
        RootNixResponse::Build(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::Verify(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::Gc(value) => value.encode().map_err(adapter_payload)?,
        RootNixResponse::CacheInspect(values) => encode_cache_observations(values)?,
        RootNixResponse::CacheInspectClosures(values) => encode_cache_closures(values)?,
        RootNixResponse::NixpkgsMetadata(bytes) => bytes.clone(),
        RootNixResponse::ClosureForRoots(paths) => encode_root_paths(paths)?,
        RootNixResponse::RepairPlan(proof) => proof
            .preview()
            .to_json_bytes()
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
        RootNixResponse::Failed { failure, .. } => return encode_root_nix_failure(*failure),
    };
    let mut payload = Vec::with_capacity(body.len() + 1);
    payload.push(ROOT_NIX_SUCCESS);
    payload.extend_from_slice(&body);
    Ok(payload)
}

fn decode_root_nix_response(
    operation: RootNixOperation,
    payload: &[u8],
) -> Result<RootNixResponse, FrameError> {
    let (&status, body) = payload
        .split_first()
        .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
    if status != ROOT_NIX_SUCCESS {
        return decode_root_nix_non_success(operation, status, body);
    }
    let codec = JsonCodec::production();
    match operation {
        RootNixOperation::Version => Ok(RootNixResponse::Version(
            VersionInfo::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::Evaluate => Ok(RootNixResponse::Evaluate(
            DerivationPlanReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::PathInfo => Ok(RootNixResponse::PathInfo(
            PathInfoReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::Substitute => Ok(RootNixResponse::Substitute(
            SubstituteReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::SubstituteMany => Ok(RootNixResponse::SubstituteMany(
            decode_chunks(body)?
                .into_iter()
                .map(|chunk| SubstituteReport::decode(&codec, chunk).map_err(adapter_payload))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        RootNixOperation::Build => Ok(RootNixResponse::Build(
            BuildReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::Verify => Ok(RootNixResponse::Verify(
            VerifyReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::Gc => Ok(RootNixResponse::Gc(
            GcReport::decode(&codec, body).map_err(adapter_payload)?,
        )),
        RootNixOperation::CacheInspect => Ok(RootNixResponse::CacheInspect(
            decode_cache_observations(body)?,
        )),
        RootNixOperation::CacheInspectClosures => Ok(RootNixResponse::CacheInspectClosures(
            decode_cache_closures(body)?,
        )),
        RootNixOperation::NixpkgsMetadata => Ok(RootNixResponse::NixpkgsMetadata(body.to_vec())),
        RootNixOperation::ClosureForRoots => {
            Ok(RootNixResponse::ClosureForRoots(decode_root_paths(body)?))
        }
        RootNixOperation::RepairPlan => Ok(RootNixResponse::RepairPlan(
            RootRepairPlanProof::new(
                BuildPreview::from_json_bytes(body)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
            )
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?,
        )),
    }
}

fn encode_root_nix_failure(failure: RootNixFailure) -> Result<Vec<u8>, FrameError> {
    let (status, code) = match failure {
        RootNixFailure::Adapter(code) => (ROOT_NIX_ADAPTER_ERROR, adapter_error_code(code)),
        RootNixFailure::Cache(code) => (ROOT_NIX_CACHE_ERROR, cache_error_code(code)),
        RootNixFailure::Nixpkgs(code) => (ROOT_NIX_NIXPKGS_ERROR, nixpkgs_error_code(code)),
        RootNixFailure::Busy => return Ok(vec![ROOT_NIX_BUSY]),
        RootNixFailure::Inactive => return Ok(vec![ROOT_NIX_INACTIVE]),
    };
    Ok(vec![status, code])
}

fn decode_root_nix_non_success(
    operation: RootNixOperation,
    status: u8,
    body: &[u8],
) -> Result<RootNixResponse, FrameError> {
    if status == ROOT_NIX_BUILD_PROGRESS {
        if operation != RootNixOperation::Build || body.len() != 4 {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let value = u32::from_be_bytes(
            body.try_into()
                .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
        );
        return Ok(RootNixResponse::BuildProgress(
            BuildProgressEstimate::new(value).map_err(adapter_payload)?,
        ));
    }
    let failure = match status {
        ROOT_NIX_ADAPTER_ERROR if body.len() == 1 => {
            RootNixFailure::Adapter(parse_adapter_error_byte(body[0])?)
        }
        ROOT_NIX_CACHE_ERROR if body.len() == 1 => {
            RootNixFailure::Cache(parse_cache_error_code(body[0])?)
        }
        ROOT_NIX_NIXPKGS_ERROR if body.len() == 1 => {
            RootNixFailure::Nixpkgs(parse_nixpkgs_error_code(body[0])?)
        }
        ROOT_NIX_BUSY if body.is_empty() => RootNixFailure::Busy,
        ROOT_NIX_INACTIVE if body.is_empty() => RootNixFailure::Inactive,
        _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    };
    Ok(RootNixResponse::Failed { operation, failure })
}

const fn readiness_wire(readiness: &BuildReadiness) -> RootNixReadinessWire {
    RootNixReadinessWire {
        sandbox_enabled: readiness.sandbox_enabled(),
        sandbox_fallback: readiness.sandbox_fallback(),
        build_users_ready: readiness.build_users_ready(),
        use_cgroups_enabled: readiness.use_cgroups_enabled(),
        cgroup_v2_ready: readiness.cgroup_v2_ready(),
    }
}

fn encode_root_paths(paths: &[StorePath]) -> Result<Vec<u8>, FrameError> {
    validate_paths(paths)?;
    encode_json(&RootNixPathsWire {
        paths: paths.iter().map(StorePath::as_str).collect(),
    })
}

fn decode_one_path(payload: &[u8]) -> Result<StorePath, FrameError> {
    let mut paths = decode_root_paths(payload)?;
    if paths.len() != 1 {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    paths
        .pop()
        .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn decode_root_paths(payload: &[u8]) -> Result<Vec<StorePath>, FrameError> {
    let wire: RootNixPathsOwnedWire = decode_json(payload)?;
    promote_paths(wire.paths)
}

fn promote_paths(paths: Vec<String>) -> Result<Vec<StorePath>, FrameError> {
    let paths = paths
        .into_iter()
        .map(|path| {
            StorePath::new(&path).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_paths(&paths)?;
    Ok(paths)
}

fn validate_paths(paths: &[StorePath]) -> Result<(), FrameError> {
    validate_path_names(paths.iter().map(StorePath::as_str))
}

fn validate_path_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<(), FrameError> {
    let mut names = names.into_iter().collect::<Vec<_>>();
    if names.is_empty() || names.len() > MAX_ROOT_NIX_PATHS {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    Ok(())
}

fn encode_cache_observations(values: &[CachePathObservation]) -> Result<Vec<u8>, FrameError> {
    validate_path_names(values.iter().map(|value| value.path().as_str()))?;
    encode_json(
        &values
            .iter()
            .map(|value| CacheObservationWire {
                path: value.path().as_str(),
                download_bytes: value.download_bytes(),
                nar_bytes: value.nar_bytes(),
            })
            .collect::<Vec<_>>(),
    )
}

fn decode_cache_observations(payload: &[u8]) -> Result<Vec<CachePathObservation>, FrameError> {
    let wires: Vec<CacheObservationOwnedWire> = decode_json(payload)?;
    let values = wires
        .into_iter()
        .map(|wire| promote_cache_observation(&wire))
        .collect::<Result<Vec<_>, _>>()?;
    validate_path_names(values.iter().map(|value| value.path().as_str()))?;
    Ok(values)
}

fn promote_cache_observation(
    wire: &CacheObservationOwnedWire,
) -> Result<CachePathObservation, FrameError> {
    let path =
        StorePath::new(&wire.path).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
    match (wire.download_bytes, wire.nar_bytes) {
        (Some(download), Some(nar)) => Ok(CachePathObservation::hit(path, download, nar)),
        (None, None) => Ok(CachePathObservation::miss(path)),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn encode_cache_closures(values: &[CacheDownloadClosure]) -> Result<Vec<u8>, FrameError> {
    validate_path_names(values.iter().map(|value| value.root().as_str()))?;
    encode_json(
        &values
            .iter()
            .map(|value| CacheClosureWire {
                root: value.root().as_str(),
                paths: value
                    .paths()
                    .iter()
                    .map(|path| CacheObservationWire {
                        path: path.path().as_str(),
                        download_bytes: path.download_bytes(),
                        nar_bytes: path.nar_bytes(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>(),
    )
}

fn decode_cache_closures(payload: &[u8]) -> Result<Vec<CacheDownloadClosure>, FrameError> {
    let wires: Vec<CacheClosureOwnedWire> = decode_json(payload)?;
    if wires.len() > MAX_ROOT_NIX_PATHS {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    let mut total = 0_usize;
    let values = wires
        .into_iter()
        .map(|wire| {
            total = total
                .checked_add(wire.paths.len())
                .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
            if total > MAX_ROOT_NIX_PATHS {
                return Err(FrameError::new(FrameErrorCode::InvalidPayload));
            }
            let root = StorePath::new(&wire.root)
                .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
            let paths = wire
                .paths
                .into_iter()
                .map(|wire| promote_cache_observation(&wire))
                .collect::<Result<Vec<_>, _>>()?;
            CacheDownloadClosure::new(root, paths)
                .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_path_names(values.iter().map(|value| value.root().as_str()))?;
    Ok(values)
}

fn encode_chunks(chunks: Vec<Vec<u8>>) -> Result<Vec<u8>, FrameError> {
    if chunks.is_empty() || chunks.len() > MAX_ROOT_NIX_PATHS {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    let mut body = Vec::new();
    body.extend_from_slice(
        &u32::try_from(chunks.len())
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?
            .to_be_bytes(),
    );
    for chunk in chunks {
        body.extend_from_slice(
            &u32::try_from(chunk.len())
                .map_err(|_| FrameError::new(FrameErrorCode::FrameTooLarge))?
                .to_be_bytes(),
        );
        body.extend_from_slice(&chunk);
        if body.len() > HELPER_FRAME_PAYLOAD_LIMIT {
            return Err(FrameError::new(FrameErrorCode::FrameTooLarge));
        }
    }
    Ok(body)
}

fn decode_chunks(mut body: &[u8]) -> Result<Vec<&[u8]>, FrameError> {
    let count = take_u32(&mut body)? as usize;
    if count == 0 || count > MAX_ROOT_NIX_PATHS {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    let mut chunks = Vec::with_capacity(count);
    for _ in 0..count {
        let length = take_u32(&mut body)? as usize;
        if length == 0 || length > body.len() {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let (chunk, rest) = body.split_at(length);
        chunks.push(chunk);
        body = rest;
    }
    if !body.is_empty() {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    Ok(chunks)
}

fn take_u32(bytes: &mut &[u8]) -> Result<u32, FrameError> {
    if bytes.len() < 4 {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    let (value, rest) = bytes.split_at(4);
    *bytes = rest;
    Ok(u32::from_be_bytes(value.try_into().map_err(|_| {
        FrameError::new(FrameErrorCode::InvalidPayload)
    })?))
}

const fn require_empty(payload: &[u8]) -> Result<(), FrameError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

const fn adapter_error_code(code: NixAdapterErrorCode) -> u8 {
    match code {
        NixAdapterErrorCode::UnexpectedCall => 1,
        NixAdapterErrorCode::OversizedInput => 2,
        NixAdapterErrorCode::MalformedPayload => 3,
        NixAdapterErrorCode::UnsupportedSchemaVersion => 4,
        NixAdapterErrorCode::UnsupportedUpstreamFormat => 5,
        NixAdapterErrorCode::ValidationFailure => 6,
        NixAdapterErrorCode::Timeout => 7,
        NixAdapterErrorCode::Unavailable => 8,
        NixAdapterErrorCode::TrustFailure => 9,
        NixAdapterErrorCode::IntegrityFailure => 10,
        NixAdapterErrorCode::PermissionDenied => 11,
        NixAdapterErrorCode::OperationFailed => 12,
    }
}

const fn parse_adapter_error_byte(code: u8) -> Result<NixAdapterErrorCode, FrameError> {
    match code {
        1 => Ok(NixAdapterErrorCode::UnexpectedCall),
        2 => Ok(NixAdapterErrorCode::OversizedInput),
        3 => Ok(NixAdapterErrorCode::MalformedPayload),
        4 => Ok(NixAdapterErrorCode::UnsupportedSchemaVersion),
        5 => Ok(NixAdapterErrorCode::UnsupportedUpstreamFormat),
        6 => Ok(NixAdapterErrorCode::ValidationFailure),
        7 => Ok(NixAdapterErrorCode::Timeout),
        8 => Ok(NixAdapterErrorCode::Unavailable),
        9 => Ok(NixAdapterErrorCode::TrustFailure),
        10 => Ok(NixAdapterErrorCode::IntegrityFailure),
        11 => Ok(NixAdapterErrorCode::PermissionDenied),
        12 => Ok(NixAdapterErrorCode::OperationFailed),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn cache_error_code(code: BuildCacheErrorCode) -> u8 {
    match code {
        BuildCacheErrorCode::InvalidSubject => 1,
        BuildCacheErrorCode::ProbeFailed => 2,
        BuildCacheErrorCode::NoBuildRequired => 3,
        BuildCacheErrorCode::InvalidEvidence => 4,
    }
}

const fn parse_cache_error_code(code: u8) -> Result<BuildCacheErrorCode, FrameError> {
    match code {
        1 => Ok(BuildCacheErrorCode::InvalidSubject),
        2 => Ok(BuildCacheErrorCode::ProbeFailed),
        3 => Ok(BuildCacheErrorCode::NoBuildRequired),
        4 => Ok(BuildCacheErrorCode::InvalidEvidence),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn nixpkgs_error_code(code: NixpkgsSourceErrorCode) -> u8 {
    match code {
        NixpkgsSourceErrorCode::InvalidVerifiedPin => 1,
        NixpkgsSourceErrorCode::RunnerFailure => 2,
        NixpkgsSourceErrorCode::MetadataTooLarge => 3,
        NixpkgsSourceErrorCode::MalformedMetadata => 4,
        NixpkgsSourceErrorCode::IdentityMismatch => 5,
        NixpkgsSourceErrorCode::InvalidSourcePath => 6,
    }
}

const fn parse_nixpkgs_error_code(code: u8) -> Result<NixpkgsSourceErrorCode, FrameError> {
    match code {
        1 => Ok(NixpkgsSourceErrorCode::InvalidVerifiedPin),
        2 => Ok(NixpkgsSourceErrorCode::RunnerFailure),
        3 => Ok(NixpkgsSourceErrorCode::MetadataTooLarge),
        4 => Ok(NixpkgsSourceErrorCode::MalformedMetadata),
        5 => Ok(NixpkgsSourceErrorCode::IdentityMismatch),
        6 => Ok(NixpkgsSourceErrorCode::InvalidSourcePath),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

struct DecodedFrame<'a> {
    method: u8,
    request_id: u64,
    payload: &'a [u8],
}

fn encode_frame(
    channel: u8,
    method: u8,
    request_id: u64,
    payload: &[u8],
) -> Result<Vec<u8>, FrameError> {
    let limit = channel_payload_limit(channel)?;
    if request_id == 0 || payload.len() > limit {
        return Err(FrameError::new(if payload.len() > limit {
            FrameErrorCode::FrameTooLarge
        } else {
            FrameErrorCode::UnsupportedMessage
        }));
    }
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| FrameError::new(FrameErrorCode::FrameTooLarge))?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    frame.push(channel);
    frame.push(method);
    frame.extend_from_slice(&request_id.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_frame(bytes: &[u8], expected_channel: u8) -> Result<DecodedFrame<'_>, FrameError> {
    if bytes.len() < HEADER_BYTES {
        return Err(FrameError::new(FrameErrorCode::MalformedHeader));
    }
    let limit = channel_payload_limit(expected_channel)?;
    if bytes.len() > HEADER_BYTES + limit {
        return Err(FrameError::new(FrameErrorCode::FrameTooLarge));
    }
    if bytes[..4] != MAGIC {
        return Err(FrameError::new(FrameErrorCode::MalformedHeader));
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(FrameError::new(FrameErrorCode::UnsupportedVersion));
    }
    if bytes[6] != expected_channel || bytes[7] == 0 {
        return Err(FrameError::new(FrameErrorCode::UnsupportedMessage));
    }
    let request_id = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| FrameError::new(FrameErrorCode::MalformedHeader))?,
    );
    if request_id == 0 {
        return Err(FrameError::new(FrameErrorCode::UnsupportedMessage));
    }
    let payload_len = u32::from_be_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| FrameError::new(FrameErrorCode::MalformedHeader))?,
    ) as usize;
    if payload_len > limit {
        return Err(FrameError::new(FrameErrorCode::FrameTooLarge));
    }
    if bytes.len() != HEADER_BYTES + payload_len {
        return Err(FrameError::new(FrameErrorCode::LengthMismatch));
    }
    Ok(DecodedFrame {
        method: bytes[7],
        request_id,
        payload: &bytes[HEADER_BYTES..],
    })
}

const fn channel_payload_limit(channel: u8) -> Result<usize, FrameError> {
    match channel {
        CHANNEL_CLI_BROKER => Ok(MAX_CLI_FRAME_PAYLOAD_BYTES),
        CHANNEL_BROKER_HELPER => Ok(HELPER_FRAME_PAYLOAD_LIMIT),
        _ => Err(FrameError::new(FrameErrorCode::UnsupportedMessage)),
    }
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, FrameError> {
    serde_json::to_vec(value).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn decode_json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, FrameError> {
    serde_json::from_slice(bytes).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn encode_build_preview(preview: &BuildPreview) -> Result<Vec<u8>, FrameError> {
    if !preview.is_build() {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    preview
        .to_json_bytes()
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

fn decode_build_preview(bytes: &[u8]) -> Result<BuildPreview, FrameError> {
    let preview = BuildPreview::from_json_bytes(bytes)
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
    if !preview.is_build() {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    Ok(preview)
}

fn adapter_payload<T>(_: T) -> FrameError {
    FrameError::new(FrameErrorCode::InvalidPayload)
}

fn validate_prepare_selectors(selectors: &[PackageSelector]) -> Result<(), FrameError> {
    if selectors.is_empty()
        || selectors.len() > MAX_BUILD_SELECTOR_WIRE_ENTRIES
        || selectors.iter().any(|selector| {
            selector.attribute().is_some()
                || selector.pin_state().is_pinned()
                || !matches!(selector.source_revision(), SourceRevision::CurrentChannel)
        })
    {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    } else {
        Ok(())
    }
}

const fn cli_adapter_method(method: MethodKind) -> Option<u8> {
    match method {
        MethodKind::Version => Some(10),
        MethodKind::EvaluateDerivation => Some(11),
        MethodKind::PathInfo => Some(12),
        MethodKind::Substitute => Some(13),
        MethodKind::Build => None,
        MethodKind::Verify => Some(15),
        MethodKind::Gc => Some(16),
    }
}

const fn adapter_method(method: u8) -> Option<MethodKind> {
    match method {
        10 => Some(MethodKind::Version),
        11 => Some(MethodKind::EvaluateDerivation),
        12 => Some(MethodKind::PathInfo),
        13 => Some(MethodKind::Substitute),
        15 => Some(MethodKind::Verify),
        16 => Some(MethodKind::Gc),
        _ => None,
    }
}

fn decode_adapter_failure(bytes: &[u8]) -> Result<Option<NixAdapterErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<AdapterFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_adapter_error_code(&wire.error).map(Some)
}

fn decode_build_execution_failure(
    bytes: &[u8],
) -> Result<Option<BuildExecutionErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildExecutionFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_build_execution_error_code(&wire.error).map(Some)
}

fn decode_build_preparation_failure(
    bytes: &[u8],
) -> Result<Option<BuildPreparationErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildPreparationFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_build_preparation_error_code(&wire.error).map(Some)
}

fn decode_build_root_publication_failure(
    bytes: &[u8],
) -> Result<Option<BuildRootPublicationErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildRootPublicationFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_build_root_publication_error_code(&wire.error).map(Some)
}

fn decode_generation_root_transition_failure(
    bytes: &[u8],
) -> Result<Option<GenerationRootTransitionErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<GenerationRootTransitionFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_generation_root_transition_error_code(&wire.error).map(Some)
}

fn decode_generation_root_removal_failure(
    bytes: &[u8],
) -> Result<Option<GenerationRootRemovalErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<GenerationRootRemovalFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_generation_root_removal_error_code(&wire.error).map(Some)
}

fn decode_generation_root_attestation_failure(
    bytes: &[u8],
) -> Result<Option<GenerationRootAttestationErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<GenerationRootAttestationFailureOwnedWire>(bytes)
    else {
        return Ok(None);
    };
    parse_generation_root_attestation_error_code(&wire.error).map(Some)
}

fn decode_install_acquisition_failure(
    bytes: &[u8],
) -> Result<Option<CacheInstallErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<InstallAcquisitionFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_cache_install_error_code(&wire.error).map(Some)
}

fn decode_channel_refresh_failure(
    bytes: &[u8],
) -> Result<Option<ChannelRefreshErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<ChannelRefreshFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_channel_refresh_error(&wire.error).map(Some)
}

fn decode_catalog_query_failure(bytes: &[u8]) -> Result<bool, FrameError> {
    let Ok(wire) = serde_json::from_slice::<CatalogQueryFailureOwnedWire>(bytes) else {
        return Ok(false);
    };
    if wire.error != "unavailable" {
        return Err(FrameError::new(FrameErrorCode::InvalidPayload));
    }
    Ok(true)
}

fn decode_repair_generation_failure(
    bytes: &[u8],
) -> Result<Option<RepairGenerationErrorCode>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<RepairGenerationFailureOwnedWire>(bytes) else {
        return Ok(None);
    };
    parse_repair_generation_error(&wire.error).map(Some)
}

const fn repair_generation_status_name(status: RepairGenerationStatus) -> &'static str {
    match status {
        RepairGenerationStatus::Clean => "clean",
        RepairGenerationStatus::DamageDetected => "damage-detected",
        RepairGenerationStatus::RepairedFromCache => "repaired-from-cache",
        RepairGenerationStatus::RepairedByBuild => "repaired-by-build",
        RepairGenerationStatus::NeedsApproval => "needs-approval",
    }
}

fn parse_repair_generation_status(value: &str) -> Result<RepairGenerationStatus, FrameError> {
    match value {
        "clean" => Ok(RepairGenerationStatus::Clean),
        "damage-detected" => Ok(RepairGenerationStatus::DamageDetected),
        "repaired-from-cache" => Ok(RepairGenerationStatus::RepairedFromCache),
        "repaired-by-build" => Ok(RepairGenerationStatus::RepairedByBuild),
        "needs-approval" => Ok(RepairGenerationStatus::NeedsApproval),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_repair_generation_error(value: &str) -> Result<RepairGenerationErrorCode, FrameError> {
    match value {
        "invalid-scope" => Ok(RepairGenerationErrorCode::InvalidScope),
        "verify-failed" => Ok(RepairGenerationErrorCode::VerifyFailed),
        "admission-failed" => Ok(RepairGenerationErrorCode::AdmissionFailed),
        "helper-failed" => Ok(RepairGenerationErrorCode::HelperFailed),
        "journal-failed" => Ok(RepairGenerationErrorCode::JournalFailed),
        "still-damaged" => Ok(RepairGenerationErrorCode::StillDamaged),
        "fresh-approval-required" => Ok(RepairGenerationErrorCode::FreshApprovalRequired),
        "authority-unavailable" => Ok(RepairGenerationErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn channel_refresh_error_name(code: ChannelRefreshErrorCode) -> &'static str {
    match code {
        ChannelRefreshErrorCode::Network => "network",
        ChannelRefreshErrorCode::Verification => "verification",
        ChannelRefreshErrorCode::Busy => "busy",
        ChannelRefreshErrorCode::ServiceUnavailable => "service-unavailable",
    }
}

fn parse_channel_refresh_error(value: &str) -> Result<ChannelRefreshErrorCode, FrameError> {
    match value {
        "network" => Ok(ChannelRefreshErrorCode::Network),
        "verification" => Ok(ChannelRefreshErrorCode::Verification),
        "busy" => Ok(ChannelRefreshErrorCode::Busy),
        "service-unavailable" => Ok(ChannelRefreshErrorCode::ServiceUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_channel_refresh_mode(value: &str) -> Result<ChannelRefreshMode, FrameError> {
    match value {
        "apply" => Ok(ChannelRefreshMode::Apply),
        "check" => Ok(ChannelRefreshMode::Check),
        "force" => Ok(ChannelRefreshMode::Force),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn decode_install_download_progress(
    bytes: &[u8],
) -> Result<Option<InstallDownloadProgress>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<InstallDownloadProgressOwnedWire>(bytes) else {
        return Ok(None);
    };
    InstallDownloadProgress::new(
        SelectorInput::new(&wire.selector)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
        wire.done,
        wire.total,
    )
    .map(Some)
}

fn decode_build_execution_progress(
    bytes: &[u8],
) -> Result<Option<BuildProgressEstimate>, FrameError> {
    let Ok(wire) = serde_json::from_slice::<BuildExecutionProgressOwnedWire>(bytes) else {
        return Ok(None);
    };
    BuildProgressEstimate::new(wire.progress_millionths)
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
        .map(Some)
}

fn parse_cache_install_error_code(value: &str) -> Result<CacheInstallErrorCode, FrameError> {
    match value {
        "invalid-intent" => Ok(CacheInstallErrorCode::InvalidIntent),
        "acquisition-failed" => Ok(CacheInstallErrorCode::AcquisitionFailed),
        "cancelled" => Ok(CacheInstallErrorCode::Cancelled),
        "authority-unavailable" => Ok(CacheInstallErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_build_execution_error_code(value: &str) -> Result<BuildExecutionErrorCode, FrameError> {
    match value {
        "approval_unavailable" => Ok(BuildExecutionErrorCode::ApprovalUnavailable),
        "approval_invalidated" => Ok(BuildExecutionErrorCode::ApprovalInvalidated),
        "resource_preflight_failed" => Ok(BuildExecutionErrorCode::ResourcePreflightFailed),
        "execution_failed" => Ok(BuildExecutionErrorCode::ExecutionFailed),
        "cancelled" => Ok(BuildExecutionErrorCode::Cancelled),
        "authority_unavailable" => Ok(BuildExecutionErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_build_preparation_error_code(
    value: &str,
) -> Result<BuildPreparationErrorCode, FrameError> {
    match value {
        "host_refused" => Ok(BuildPreparationErrorCode::HostRefused),
        "intent_refused" => Ok(BuildPreparationErrorCode::IntentRefused),
        "planning_refused" => Ok(BuildPreparationErrorCode::PlanningRefused),
        "broker_refused" => Ok(BuildPreparationErrorCode::BrokerRefused),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_build_root_publication_error_code(
    value: &str,
) -> Result<BuildRootPublicationErrorCode, FrameError> {
    match value {
        "invalid_root_intent" => Ok(BuildRootPublicationErrorCode::InvalidRootIntent),
        "publication_failed" => Ok(BuildRootPublicationErrorCode::PublicationFailed),
        "cancelled" => Ok(BuildRootPublicationErrorCode::Cancelled),
        "authority_unavailable" => Ok(BuildRootPublicationErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_generation_root_transition_error_code(
    value: &str,
) -> Result<GenerationRootTransitionErrorCode, FrameError> {
    match value {
        "invalid_intent" => Ok(GenerationRootTransitionErrorCode::InvalidIntent),
        "transition_failed" => Ok(GenerationRootTransitionErrorCode::TransitionFailed),
        "cancelled" => Ok(GenerationRootTransitionErrorCode::Cancelled),
        "authority_unavailable" => Ok(GenerationRootTransitionErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_generation_root_removal_error_code(
    value: &str,
) -> Result<GenerationRootRemovalErrorCode, FrameError> {
    match value {
        "invalid_intent" => Ok(GenerationRootRemovalErrorCode::InvalidIntent),
        "removal_failed" => Ok(GenerationRootRemovalErrorCode::RemovalFailed),
        "cancelled" => Ok(GenerationRootRemovalErrorCode::Cancelled),
        "authority_unavailable" => Ok(GenerationRootRemovalErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_generation_root_attestation_error_code(
    value: &str,
) -> Result<GenerationRootAttestationErrorCode, FrameError> {
    match value {
        "invalid_intent" => Ok(GenerationRootAttestationErrorCode::InvalidIntent),
        "attestation_failed" => Ok(GenerationRootAttestationErrorCode::AttestationFailed),
        "cancelled" => Ok(GenerationRootAttestationErrorCode::Cancelled),
        "authority_unavailable" => Ok(GenerationRootAttestationErrorCode::AuthorityUnavailable),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_adapter_error_code(value: &str) -> Result<NixAdapterErrorCode, FrameError> {
    match value {
        "unexpected_call" => Ok(NixAdapterErrorCode::UnexpectedCall),
        "oversized_input" => Ok(NixAdapterErrorCode::OversizedInput),
        "malformed_payload" => Ok(NixAdapterErrorCode::MalformedPayload),
        "unsupported_schema_version" => Ok(NixAdapterErrorCode::UnsupportedSchemaVersion),
        "unsupported_upstream_format" => Ok(NixAdapterErrorCode::UnsupportedUpstreamFormat),
        "validation_failure" => Ok(NixAdapterErrorCode::ValidationFailure),
        "timeout" => Ok(NixAdapterErrorCode::Timeout),
        "unavailable" => Ok(NixAdapterErrorCode::Unavailable),
        "trust_failure" => Ok(NixAdapterErrorCode::TrustFailure),
        "integrity_failure" => Ok(NixAdapterErrorCode::IntegrityFailure),
        "permission_denied" => Ok(NixAdapterErrorCode::PermissionDenied),
        "operation_failed" => Ok(NixAdapterErrorCode::OperationFailed),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn encode_handle_body(handle: &OperationHandle, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    let body: Box<RawValue> = decode_json(body)?;
    encode_json(&HandleBodyWire {
        handle: handle.as_str(),
        request: &body,
    })
}

fn decode_handle_body(bytes: &[u8]) -> Result<(OperationHandle, Box<RawValue>), FrameError> {
    let wire: HandleBodyOwnedWire = decode_json(bytes)?;
    Ok((parse_handle(&wire.handle)?, wire.request))
}

const fn operation_name(kind: BrokerOperationKind) -> &'static str {
    match kind {
        BrokerOperationKind::Doctor => "doctor",
        BrokerOperationKind::Refresh => "refresh",
        BrokerOperationKind::Resolve => "resolve",
        BrokerOperationKind::Acquire => "acquire",
        BrokerOperationKind::Build => "build",
        BrokerOperationKind::Activate => "activate",
        BrokerOperationKind::Gc => "gc",
        BrokerOperationKind::Repair => "repair",
    }
}

fn parse_operation(value: &str) -> Result<BrokerOperationKind, FrameError> {
    match value {
        "doctor" => Ok(BrokerOperationKind::Doctor),
        "refresh" => Ok(BrokerOperationKind::Refresh),
        "resolve" => Ok(BrokerOperationKind::Resolve),
        "acquire" => Ok(BrokerOperationKind::Acquire),
        "build" => Ok(BrokerOperationKind::Build),
        "activate" => Ok(BrokerOperationKind::Activate),
        "gc" => Ok(BrokerOperationKind::Gc),
        "repair" => Ok(BrokerOperationKind::Repair),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn status_name(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Completed => "completed",
        OperationStatus::Cancelled => "cancelled",
    }
}

fn parse_status(value: &str) -> Result<OperationStatus, FrameError> {
    match value {
        "running" => Ok(OperationStatus::Running),
        "completed" => Ok(OperationStatus::Completed),
        "cancelled" => Ok(OperationStatus::Cancelled),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

const fn approval_source_name(source: ApprovalSource) -> &'static str {
    source.as_str()
}

fn parse_approval_source(value: &str) -> Result<ApprovalSource, FrameError> {
    match value {
        "interactive" => Ok(ApprovalSource::Interactive),
        "yes" => Ok(ApprovalSource::AssumeYes),
        _ => Err(FrameError::new(FrameErrorCode::InvalidPayload)),
    }
}

fn parse_handle(value: &str) -> Result<OperationHandle, FrameError> {
    let tail = value.strip_prefix("op_").unwrap_or_default();
    if tail.len() == 64
        && tail
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(OperationHandle(value.to_owned()))
    } else {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

fn parse_capability(value: &str) -> Result<MaintenanceCapability, FrameError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(MaintenanceCapability(value.to_owned()))
    } else {
        Err(FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BeginWire<'a> {
    operation: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginOwnedWire {
    operation: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandleWire<'a> {
    handle: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleOwnedWire {
    handle: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelRefreshWire {
    updated: bool,
    sequence: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ChannelRefreshRequestWire<'a> {
    handle: &'a str,
    mode: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelRefreshRequestOwnedWire {
    handle: String,
    mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelRefreshOwnedWire {
    updated: bool,
    sequence: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ChannelRefreshFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelRefreshFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSearchRequestWire<'a> {
    handle: &'a str,
    query: &'a str,
    limit: u16,
    exact: bool,
    license: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSearchRequestOwnedWire {
    handle: String,
    query: String,
    limit: u16,
    exact: bool,
    license: Option<String>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogInfoRequestWire<'a> {
    handle: &'a str,
    selectors: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogInfoRequestOwnedWire {
    handle: String,
    selectors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSummaryWire<'a> {
    package: &'a str,
    name: &'a str,
    version: &'a str,
    description: &'a str,
    licenses: &'a [String],
    available: bool,
    broken: bool,
}

impl<'a> From<&'a CatalogPackageSummary> for CatalogSummaryWire<'a> {
    fn from(value: &'a CatalogPackageSummary) -> Self {
        Self {
            package: value.package(),
            name: value.name(),
            version: value.version(),
            description: value.description(),
            licenses: value.licenses(),
            available: value.available(),
            broken: value.broken(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSummaryOwnedWire {
    package: String,
    name: String,
    version: String,
    description: String,
    licenses: Vec<String>,
    available: bool,
    broken: bool,
}

impl CatalogSummaryOwnedWire {
    fn promote(self) -> Result<CatalogPackageSummary, FrameError> {
        CatalogPackageSummary::new(
            &self.package,
            &self.name,
            &self.version,
            &self.description,
            self.licenses,
            self.available,
            self.broken,
        )
        .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSearchResponseWire<'a> {
    sequence: u64,
    generated_at: &'a str,
    results: Vec<CatalogSummaryWire<'a>>,
}

impl<'a> CatalogSearchResponseWire<'a> {
    fn from_report(report: &'a CatalogSearchReport) -> Self {
        Self {
            sequence: report.sequence().get().get(),
            generated_at: report.generated_at(),
            results: report
                .results()
                .iter()
                .map(CatalogSummaryWire::from)
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogSearchResponseOwnedWire {
    sequence: u64,
    generated_at: String,
    results: Vec<CatalogSummaryOwnedWire>,
}

impl CatalogSearchResponseOwnedWire {
    fn promote(self) -> Result<CatalogSearchReport, FrameError> {
        let sequence = ChannelSequence::from_u64(self.sequence)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let results = self
            .results
            .into_iter()
            .map(CatalogSummaryOwnedWire::promote)
            .collect::<Result<Vec<_>, _>>()?;
        CatalogSearchReport::new(sequence, &self.generated_at, results)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogPackageInfoWire<'a> {
    summary: CatalogSummaryWire<'a>,
    homepage: &'a str,
    outputs: &'a [String],
    platforms: &'a [String],
    catalog_revision: &'a str,
    catalog_generated_at: &'a str,
}

impl<'a> From<&'a CatalogPackageInfo> for CatalogPackageInfoWire<'a> {
    fn from(value: &'a CatalogPackageInfo) -> Self {
        Self {
            summary: CatalogSummaryWire::from(value.summary()),
            homepage: value.homepage(),
            outputs: value.outputs(),
            platforms: value.platforms(),
            catalog_revision: value.catalog_revision(),
            catalog_generated_at: value.catalog_generated_at(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogPackageInfoOwnedWire {
    summary: CatalogSummaryOwnedWire,
    homepage: String,
    outputs: Vec<String>,
    platforms: Vec<String>,
    catalog_revision: String,
    catalog_generated_at: String,
}

impl CatalogPackageInfoOwnedWire {
    fn promote(self) -> Result<CatalogPackageInfo, FrameError> {
        CatalogPackageInfo::new(
            self.summary.promote()?,
            &self.homepage,
            self.outputs,
            self.platforms,
            &self.catalog_revision,
            &self.catalog_generated_at,
        )
        .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogInfoResponseWire<'a> {
    sequence: u64,
    status: &'static str,
    package: Option<CatalogPackageInfoWire<'a>>,
    candidates: Vec<CatalogSummaryWire<'a>>,
}

impl<'a> CatalogInfoResponseWire<'a> {
    fn from_report(report: &'a CatalogInfoReport) -> Self {
        match report.lookup() {
            CatalogInfoLookup::Found(package) => Self {
                sequence: report.sequence().get().get(),
                status: "found",
                package: Some(CatalogPackageInfoWire::from(package.as_ref())),
                candidates: Vec::new(),
            },
            CatalogInfoLookup::Ambiguous(candidates) => Self {
                sequence: report.sequence().get().get(),
                status: "ambiguous",
                package: None,
                candidates: candidates.iter().map(CatalogSummaryWire::from).collect(),
            },
            CatalogInfoLookup::NotFound => Self {
                sequence: report.sequence().get().get(),
                status: "not-found",
                package: None,
                candidates: Vec::new(),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogInfoResponseOwnedWire {
    sequence: u64,
    status: String,
    package: Option<CatalogPackageInfoOwnedWire>,
    candidates: Vec<CatalogSummaryOwnedWire>,
}

impl CatalogInfoResponseOwnedWire {
    fn promote(self) -> Result<CatalogInfoReport, FrameError> {
        let sequence = ChannelSequence::from_u64(self.sequence)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let lookup = match self.status.as_str() {
            "found" if self.candidates.is_empty() => CatalogInfoLookup::Found(Box::new(
                self.package
                    .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?
                    .promote()?,
            )),
            "ambiguous" if self.package.is_none() && !self.candidates.is_empty() => {
                CatalogInfoLookup::Ambiguous(
                    self.candidates
                        .into_iter()
                        .map(CatalogSummaryOwnedWire::promote)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            "not-found" if self.package.is_none() && self.candidates.is_empty() => {
                CatalogInfoLookup::NotFound
            }
            _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
        };
        CatalogInfoReport::new(sequence, lookup)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogQueryFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogQueryFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AdapterFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutionFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutionFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BuildPreparationFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildPreparationFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct BuildRootPublicationFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRootPublicationFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootTransitionFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootTransitionFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootRemovalFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootRemovalFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootAttestationFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationRootAttestationFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct InstallAcquisitionWire<'a> {
    outcome: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallAcquisitionOwnedWire {
    outcome: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct InstallAcquisitionFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallAcquisitionFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallDownloadProgressWire<'a> {
    selector: &'a str,
    done: u64,
    total: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallDownloadProgressOwnedWire {
    selector: String,
    done: u64,
    total: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionProgressWire {
    progress_millionths: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionProgressOwnedWire {
    progress_millionths: u32,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandleBodyWire<'a> {
    handle: &'a str,
    request: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleBodyOwnedWire {
    handle: String,
    request: Box<RawValue>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct HandlePathWire<'a> {
    handle: &'a str,
    path: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandlePathOwnedWire {
    handle: String,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildApprovalWire<'a> {
    handle: &'a str,
    build_plan_digest: String,
    source: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildApprovalOwnedWire {
    handle: String,
    build_plan_digest: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareBuildWire<'a> {
    handle: &'a str,
    selectors: Vec<SelectorWire<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareBuildOwnedWire {
    handle: String,
    selectors: Vec<SelectorOwnedWire>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionWire<'a> {
    handle: &'a str,
    build_plan_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildExecutionOwnedWire {
    handle: String,
    build_plan_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildRootIntentWire<'a> {
    handle: &'a str,
    source_generation: Option<&'a str>,
    generation: &'a str,
    entries: Vec<RootSetEntryWire<'a>>,
    added_names: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildRootIntentOwnedWire {
    handle: String,
    source_generation: Option<String>,
    generation: String,
    entries: Vec<RootSetEntryOwnedWire>,
    added_names: Vec<String>,
}

impl BuildRootIntentOwnedWire {
    fn promote(self) -> Result<RootSetIntent, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let entries = self
            .entries
            .into_iter()
            .map(|entry| {
                Ok(RootSetEntry::new(
                    RootName::new(&entry.name)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                    StorePath::new(&entry.target)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        let mut added_names = self
            .added_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        added_names.sort();
        match self.source_generation {
            Some(source) => RootSetIntent::from_source(
                GenerationId::new(&source)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                generation,
                entries,
                added_names,
            ),
            None => RootSetIntent::new(generation, entries).and_then(|intent| {
                if intent.added_names() == added_names {
                    Ok(intent)
                } else {
                    Err(crate::MaintenanceError::backend_failure())
                }
            }),
        }
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootTransitionWire<'a> {
    handle: &'a str,
    source_generation: &'a str,
    destination_generation: &'a str,
    retained_names: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootTransitionOwnedWire {
    handle: String,
    source_generation: String,
    destination_generation: String,
    retained_names: Vec<String>,
}

impl GenerationRootTransitionOwnedWire {
    fn promote(self) -> Result<RootSetTransitionIntent, FrameError> {
        let source_generation = GenerationId::new(&self.source_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let destination_generation = GenerationId::new(&self.destination_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let retained_names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RootSetTransitionIntent::new(source_generation, destination_generation, retained_names)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootRemovalWire<'a> {
    handle: &'a str,
    generation: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationRootRemovalOwnedWire {
    handle: String,
    generation: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairGenerationRequestWire<'a> {
    handle: &'a str,
    generation: &'a str,
    verify_only: bool,
    build_plan_digest: Option<String>,
    approval_source: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairGenerationRequestOwnedWire {
    handle: String,
    generation: String,
    verify_only: bool,
    build_plan_digest: Option<String>,
    approval_source: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairGenerationReportWire<'a> {
    status: &'a str,
    damaged_paths: u32,
    build_preview: Option<&'a BuildPreview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairGenerationReportOwnedWire {
    status: String,
    damaged_paths: u32,
    build_preview: Option<BuildPreview>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RepairGenerationFailureWire<'a> {
    error: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairGenerationFailureOwnedWire {
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectorWire<'a> {
    id: &'a str,
    selector: &'a str,
    version_preference: VersionPreferenceWire<'a>,
    outputs: Option<Vec<&'a str>>,
}

impl<'a> From<&'a PackageSelector> for SelectorWire<'a> {
    fn from(selector: &'a PackageSelector) -> Self {
        Self {
            id: selector.id().as_str(),
            selector: selector.selector().as_str(),
            version_preference: VersionPreferenceWire::from(selector.version_preference()),
            outputs: selector
                .outputs()
                .explicit_outputs()
                .map(|outputs| outputs.iter().map(OutputName::as_str).collect()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectorOwnedWire {
    id: String,
    selector: String,
    version_preference: VersionPreferenceOwnedWire,
    outputs: Option<Vec<String>>,
}

impl SelectorOwnedWire {
    fn promote(self) -> Result<PackageSelector, FrameError> {
        let outputs = match self.outputs {
            None => OutputSelection::default_selection(),
            Some(outputs) => OutputSelection::explicit(
                outputs
                    .into_iter()
                    .map(|output| OutputName::new(&output).map_err(adapter_payload))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .map_err(adapter_payload)?,
        };
        Ok(PackageSelector::new(
            SelectorId::new(&self.id).map_err(adapter_payload)?,
            SelectorInput::new(&self.selector).map_err(adapter_payload)?,
            self.version_preference.promote()?,
            outputs,
            SourceRevision::CurrentChannel,
        ))
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum VersionPreferenceWire<'a> {
    Any,
    Exact {
        version: &'a str,
    },
    Min {
        version: &'a str,
    },
    Range {
        lower: Option<VersionBoundWire<'a>>,
        upper: Option<VersionBoundWire<'a>>,
    },
}

impl<'a> From<&'a VersionPreference> for VersionPreferenceWire<'a> {
    fn from(preference: &'a VersionPreference) -> Self {
        match preference {
            VersionPreference::Any => Self::Any,
            VersionPreference::Exact(version) => Self::Exact {
                version: version.as_str(),
            },
            VersionPreference::Minimum(version) => Self::Min {
                version: version.as_str(),
            },
            VersionPreference::Range(range) => Self::Range {
                lower: range.lower().map(VersionBoundWire::from),
                upper: range.upper().map(VersionBoundWire::from),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum VersionPreferenceOwnedWire {
    Any,
    Exact {
        version: String,
    },
    Min {
        version: String,
    },
    Range {
        lower: Option<VersionBoundOwnedWire>,
        upper: Option<VersionBoundOwnedWire>,
    },
}

impl VersionPreferenceOwnedWire {
    fn promote(self) -> Result<VersionPreference, FrameError> {
        Ok(match self {
            Self::Any => VersionPreference::Any,
            Self::Exact { version } => VersionPreference::Exact(PackageVersion::new(version)),
            Self::Min { version } => VersionPreference::Minimum(PackageVersion::new(version)),
            Self::Range { lower, upper } => VersionPreference::Range(
                VersionRange::new(
                    lower.map(VersionBoundOwnedWire::promote).transpose()?,
                    upper.map(VersionBoundOwnedWire::promote).transpose()?,
                )
                .map_err(adapter_payload)?,
            ),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBoundWire<'a> {
    version: &'a str,
    inclusive: bool,
}

impl<'a> From<&'a VersionBound> for VersionBoundWire<'a> {
    fn from(bound: &'a VersionBound) -> Self {
        Self {
            version: bound.version().as_str(),
            inclusive: bound.is_inclusive(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBoundOwnedWire {
    version: String,
    inclusive: bool,
}

impl VersionBoundOwnedWire {
    fn promote(self) -> Result<VersionBound, FrameError> {
        let version = PackageVersion::new(self.version);
        Ok(if self.inclusive {
            VersionBound::inclusive(version)
        } else {
            VersionBound::exclusive(version)
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyWire {}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedOwnershipWire {
    verified: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestDigestWire {
    asset_manifest_digest: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StatusWire<'a> {
    status: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusOwnedWire {
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetWire<'a> {
    owner_uid: u32,
    generation: &'a str,
    entries: Vec<RootSetEntryWire<'a>>,
}

impl<'a> RootSetWire<'a> {
    fn from_root_set(root_set: &'a RootSet) -> Self {
        Self {
            owner_uid: root_set.owner_uid(),
            generation: root_set.generation().as_str(),
            entries: root_set
                .entries()
                .iter()
                .map(|entry| RootSetEntryWire {
                    name: entry.name().as_str(),
                    target: entry.target().as_str(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetPublicationWire<'a> {
    owner_uid: u32,
    source_generation: Option<&'a str>,
    generation: &'a str,
    entries: Vec<RootSetEntryWire<'a>>,
    added_names: Vec<&'a str>,
}

impl<'a> RootSetPublicationWire<'a> {
    fn from_request(request: &'a RootSetPublicationRequest) -> Self {
        let root_set = request.root_set();
        Self {
            owner_uid: root_set.owner_uid(),
            source_generation: request.source_generation().map(GenerationId::as_str),
            generation: root_set.generation().as_str(),
            entries: root_set
                .entries()
                .iter()
                .map(|entry| RootSetEntryWire {
                    name: entry.name().as_str(),
                    target: entry.target().as_str(),
                })
                .collect(),
            added_names: request.added_names().iter().map(RootName::as_str).collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RootSetEntryWire<'a> {
    name: &'a str,
    target: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetOwnedWire {
    owner_uid: u32,
    generation: String,
    entries: Vec<RootSetEntryOwnedWire>,
}

impl RootSetOwnedWire {
    fn promote(self) -> Result<RootSet, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let entries = self
            .entries
            .into_iter()
            .map(|entry| {
                Ok(RootSetEntry::new(
                    RootName::new(&entry.name)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                    StorePath::new(&entry.target)
                        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?,
                ))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        RootSet::new(self.owner_uid, generation, entries)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetPublicationOwnedWire {
    owner_uid: u32,
    source_generation: Option<String>,
    generation: String,
    entries: Vec<RootSetEntryOwnedWire>,
    added_names: Vec<String>,
}

impl RootSetPublicationOwnedWire {
    fn promote(self) -> Result<RootSetPublicationRequest, FrameError> {
        let source_generation = self
            .source_generation
            .map(|source| {
                GenerationId::new(&source)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .transpose()?;
        let added_names = self
            .added_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RootSetPublicationRequest::new(
            RootSetOwnedWire {
                owner_uid: self.owner_uid,
                generation: self.generation,
                entries: self.entries,
            }
            .promote()?,
            source_generation,
            added_names,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootSetEntryOwnedWire {
    name: String,
    target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveRootSetWire<'a> {
    owner_uid: u32,
    generation: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveRootSetOwnedWire {
    owner_uid: u32,
    generation: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionWire<'a> {
    owner_uid: u32,
    source_generation: &'a str,
    destination_generation: &'a str,
    retained_names: Vec<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionOwnedWire {
    owner_uid: u32,
    source_generation: String,
    destination_generation: String,
    retained_names: Vec<String>,
}

impl RootSetTransitionOwnedWire {
    fn promote(self) -> Result<RootSetTransitionRequest, FrameError> {
        let source_generation = GenerationId::new(&self.source_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let destination_generation = GenerationId::new(&self.destination_generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let retained_names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RootSetTransitionRequest::new(
            self.owner_uid,
            source_generation,
            destination_generation,
            retained_names,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairScopeWire<'a> {
    owner_uid: u32,
    generation: &'a str,
    paths: Vec<&'a str>,
    build_plan_digest: Option<String>,
    policy_version: u64,
    mode: &'a str,
}

impl<'a> RepairScopeWire<'a> {
    fn from_scope(scope: &'a VerifiedRepairScope) -> Self {
        Self {
            owner_uid: scope.owner_uid(),
            generation: scope.generation().as_str(),
            paths: scope.paths().iter().map(StorePath::as_str).collect(),
            build_plan_digest: scope.build_plan_digest().map(|digest| digest.to_string()),
            policy_version: scope.policy_version().get().get(),
            mode: match scope.mode() {
                RepairMode::CacheOnly => "cacheOnly",
                RepairMode::Build => "build",
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairScopeOwnedWire {
    owner_uid: u32,
    generation: String,
    paths: Vec<String>,
    build_plan_digest: Option<String>,
    policy_version: u64,
    mode: String,
}

impl RepairScopeOwnedWire {
    fn promote(self) -> Result<VerifiedRepairScope, FrameError> {
        let generation = GenerationId::new(&self.generation)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let paths = self
            .paths
            .into_iter()
            .map(|path| {
                StorePath::new(&path).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let digest = self
            .build_plan_digest
            .map(|digest| {
                Digest::from_str(&digest)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .transpose()?;
        let policy_version = PolicyVersion::from_u64(self.policy_version)
            .ok_or_else(|| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let mode = match self.mode.as_str() {
            "cacheOnly" => RepairMode::CacheOnly,
            "build" => RepairMode::Build,
            _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
        };
        VerifiedRepairScope::new(
            self.owner_uid,
            generation,
            paths,
            digest,
            policy_version,
            mode,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityWire<'a> {
    capability: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityOwnedWire {
    capability: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetReportWire<'a> {
    reference: &'a str,
    entry_count: usize,
    mapping_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetReportOwnedWire {
    reference: String,
    entry_count: usize,
    mapping_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionReportWire<'a> {
    reference: &'a str,
    entry_count: usize,
    retained_names: Vec<&'a str>,
    mapping_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootSetTransitionReportOwnedWire {
    reference: String,
    entry_count: usize,
    retained_names: Vec<String>,
    mapping_digest: String,
}

impl RootSetTransitionReportOwnedWire {
    fn promote(self) -> Result<RootSetTransitionReport, FrameError> {
        if self.entry_count == 0 || self.entry_count > MAX_ROOT_SET_WIRE_ENTRIES {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let reference = RootRef::new(&self.reference)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        let names = self
            .retained_names
            .into_iter()
            .map(|name| {
                RootName::new(&name).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mapping_digest = Digest::from_str(&self.mapping_digest)
            .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
        RootSetTransitionReport::new(
            RootSetReport::new(reference, self.entry_count, mapping_digest),
            names,
            mapping_digest,
        )
        .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
    }
}

fn parse_mapping_digest(value: &str) -> Result<Digest, FrameError> {
    Digest::from_str(value).map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairReportWire<'a> {
    mode: &'a str,
    outcomes: Vec<RepairOutcomeWire<'a>>,
}

impl<'a> RepairReportWire<'a> {
    fn from_report(report: &'a RepairStorePathsReport) -> Self {
        Self {
            mode: match report.mode() {
                RepairMode::CacheOnly => "cacheOnly",
                RepairMode::Build => "build",
            },
            outcomes: report
                .outcomes()
                .iter()
                .map(|outcome| RepairOutcomeWire {
                    path: outcome.path().as_str(),
                    kind: match outcome.kind() {
                        RepairOutcomeKind::Restored => "restored",
                        RepairOutcomeKind::Unchanged => "unchanged",
                        RepairOutcomeKind::CacheMiss => "cacheMiss",
                    },
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RepairOutcomeWire<'a> {
    path: &'a str,
    kind: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepairReportOwnedWire {
    mode: String,
    outcomes: Vec<RepairOutcomeOwnedWire>,
}

impl RepairReportOwnedWire {
    fn promote(self) -> Result<RepairStorePathsReport, FrameError> {
        if self.outcomes.is_empty() || self.outcomes.len() > MAX_ROOT_SET_WIRE_ENTRIES {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        let mode = match self.mode.as_str() {
            "cacheOnly" => RepairMode::CacheOnly,
            "build" => RepairMode::Build,
            _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
        };
        let mut outcomes = self
            .outcomes
            .into_iter()
            .map(|outcome| {
                let path = StorePath::new(&outcome.path)
                    .map_err(|_| FrameError::new(FrameErrorCode::InvalidPayload))?;
                let kind = match outcome.kind.as_str() {
                    "restored" => RepairOutcomeKind::Restored,
                    "unchanged" => RepairOutcomeKind::Unchanged,
                    "cacheMiss" => RepairOutcomeKind::CacheMiss,
                    _ => return Err(FrameError::new(FrameErrorCode::InvalidPayload)),
                };
                Ok(RepairPathOutcome::new(path, kind))
            })
            .collect::<Result<Vec<_>, FrameError>>()?;
        outcomes.sort_by(|left, right| left.path().as_str().cmp(right.path().as_str()));
        if outcomes
            .windows(2)
            .any(|pair| pair[0].path() == pair[1].path())
        {
            return Err(FrameError::new(FrameErrorCode::InvalidPayload));
        }
        Ok(RepairStorePathsReport::new(mode, outcomes))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairOutcomeOwnedWire {
    path: String,
    kind: String,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroU64};

    use pkg_core::state::body_digest;

    use super::*;
    use crate::{
        AcceptedFormats, AttributePath, BuildApprovalReceipt, BuildOutput, BuildOutputProvenance,
        BuildStatus, DerivationPath, DerivedOutputTarget, EvaluatedDerivation, FormatVersion,
        GcStatus, NarHash, NarIntegrity, NixVersion, NixpkgsRevision, OperationId, PackageVersion,
        PathVerifyResult, Signature, SubstituteOutcome, TrustStatus, VerifyMode,
    };

    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn path(name: &str) -> StorePath {
        StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
    }

    fn nar_hash() -> NarHash {
        NarHash::new("sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=").unwrap()
    }

    fn evaluation_request() -> EvaluateDerivationRequest {
        EvaluateDerivationRequest::new(
            AttributePath::new("hello").unwrap(),
            System::X8664Linux,
            NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            nar_hash(),
            OutputSelection::default_selection(),
        )
        .unwrap()
    }

    fn build_request() -> crate::BuildRequest {
        crate::BuildRequest::new(
            vec![
                DerivedOutputTarget::new(
                    DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello.drv"))
                        .unwrap(),
                    vec![OutputName::new("out").unwrap()],
                )
                .unwrap(),
            ],
            System::X8664Linux,
            BuildApprovalReceipt::new(
                OperationId::new("op-framing").unwrap(),
                Digest::from_bytes([0x42; 32]),
                PolicyVersion::from_u64(7).unwrap(),
            ),
        )
        .unwrap()
    }

    fn repair_request() -> RootRepairPlanRequest {
        RootRepairPlanRequest::new(
            vec![path("hello")],
            PolicyVersion::from_u64(7).unwrap(),
            System::X8664Linux,
            BuildReadiness::new(true, false, true, true, true),
            8,
        )
        .unwrap()
    }

    fn derivation_report() -> DerivationPlanReport {
        let derivation =
            DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-hello.drv")).unwrap();
        let output = OutputName::new("out").unwrap();
        DerivationPlanReport::new(
            4,
            derivation.clone(),
            vec![output.clone()],
            vec![
                EvaluatedDerivation::new(
                    derivation,
                    "hello-1.0".into(),
                    System::X8664Linux,
                    BTreeMap::from([(output, path("hello"))]),
                    Digest::from_bytes([1; 32]),
                    false,
                )
                .unwrap(),
            ],
            Digest::from_bytes([2; 32]),
            "hello".into(),
            PackageVersion::new("1.0"),
        )
        .unwrap()
    }

    fn path_info_report() -> PathInfoReport {
        PathInfoReport::new(
            path("hello"),
            nar_hash(),
            vec![Signature::new("cache:BBBBBBBB").unwrap()],
            vec![path("glibc")],
            None,
            1024,
            4096,
        )
        .unwrap()
    }

    fn verify_report() -> VerifyReport {
        VerifyReport::new(vec![PathVerifyResult::new(
            path("hello"),
            NarIntegrity::Intact,
            TrustStatus::Trusted,
        )])
        .unwrap()
    }

    fn repair_proof() -> RootRepairPlanProof {
        let preview = BuildPreview::from_json_bytes(
            br#"{"schemaVersion":1,"purpose":"repair","platform":{"os":"linux","arch":"x86_64"},"policyVersion":7,"buildPlanDigest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","targets":[{"selector":"repair-1","packageName":"hello-1.0","version":"installed","outputsToInstall":["out"],"localBuildRequired":true}],"build":{"count":1,"names":["hello-1.0"],"hasFixedOutput":false},"cache":{"knownDownloadBytes":0,"knownContentBytes":0},"unknownLocalOutputs":1,"estimates":{"approxBuildMinutes":null,"approxNewDiskBytes":null,"approxTotalClosureBytes":null},"readiness":{"sandboxed":true,"buildIsolationReady":true,"nativeBuild":true,"resourceBoundary":{"isolation":"sandbox","perBuildResourceCap":false,"notice":"Repair builds run sandboxed. pkg fixes repair parallelism to one build job, admits one machine-global build operation, and applies no hard per-build memory/CPU/IO cap. Determinate controls other daemon limits."}},"approvalRequired":true}"#,
        )
        .unwrap();
        RootRepairPlanProof::new(preview).unwrap()
    }

    fn normal_build_preview() -> BuildPreview {
        let mut value = repair_proof().preview().to_json_value().unwrap();
        value["purpose"] = serde_json::json!("build");
        value["readiness"]["resourceBoundary"]["notice"] = serde_json::json!(
            "Builds run sandboxed. Determinate controls daemon limits and build parallelism. pkg admits one machine-global build operation and applies no hard per-build memory/CPU/IO cap."
        );
        BuildPreview::from_json_bytes(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn root_set() -> RootSet {
        RootSet::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("hello-1.0"),
            )],
        )
        .unwrap()
    }

    fn root_publication() -> RootSetPublicationRequest {
        let root_set = root_set();
        let added_names = root_set
            .entries()
            .iter()
            .map(|entry| entry.name().clone())
            .collect();
        RootSetPublicationRequest::new(root_set, None, added_names).unwrap()
    }

    #[test]
    fn build_preparation_method_round_trips_success_and_closed_refusals() {
        let success = CliBrokerResponse::BuildPrepared(normal_build_preview());
        let encoded = ProductFrameCodec::encode_cli_response(18, &success).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((18, success))
        );

        for code in [
            BuildPreparationErrorCode::HostRefused,
            BuildPreparationErrorCode::IntentRefused,
            BuildPreparationErrorCode::PlanningRefused,
            BuildPreparationErrorCode::BrokerRefused,
        ] {
            let refusal = CliBrokerResponse::BuildPreparationRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(18, &refusal).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((18, refusal))
            );
        }

        for payload in [
            br#"{"error":"unknown"}"#.as_slice(),
            br#"{"error":"host_refused","detail":"forbidden"}"#.as_slice(),
            br#"{"error":1}"#.as_slice(),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, 18, 18, payload).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Err(FrameError::new(FrameErrorCode::InvalidPayload))
            );
        }
    }

    #[test]
    fn both_channels_round_trip_with_exact_headers() {
        let cli = CliBrokerRequest::Begin(BrokerOperationKind::Repair);
        let encoded = ProductFrameCodec::encode_cli_request(7, &cli).unwrap();
        assert_eq!(&encoded[..4], b"PKG1");
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((7, cli))
        );

        let handle = OperationHandle(format!("op_{}", "a".repeat(64)));
        let approval = CliBrokerRequest::ApproveBuild(
            handle,
            BuildApprovalRequest::new(Digest::from_bytes([0x42; 32]), ApprovalSource::Interactive),
        );
        let encoded = ProductFrameCodec::encode_cli_request(8, &approval).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("buildPlanDigest"));
        for forbidden in ["receipt", "derivation", "target", "substituter", "maxJobs"] {
            assert!(!wire.contains(forbidden), "approval exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((8, approval))
        );

        let encoded =
            ProductFrameCodec::encode_cli_response(8, &CliBrokerResponse::BuildApproved).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((8, CliBrokerResponse::BuildApproved))
        );
        let handle = OperationHandle(format!("op_{}", "b".repeat(64)));
        let selector = PackageSelector::new(
            SelectorId::new("sel_ripgrep").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Minimum(PackageVersion::new("14.0")),
            OutputSelection::explicit(vec![OutputName::new("out").unwrap()]).unwrap(),
            SourceRevision::CurrentChannel,
        );
        let prepare = CliBrokerRequest::PrepareBuild(handle, vec![selector]);
        let encoded = ProductFrameCodec::encode_cli_request(10, &prepare).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["source", "revision", "attribute", "pinned", "nixpkgs"] {
            assert!(!wire.contains(forbidden), "prepare exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((10, prepare))
        );

        let resolved = PackageSelector::new(
            SelectorId::new("sel_resolved").unwrap(),
            SelectorInput::new("ripgrep").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        )
        .with_attribute(pkg_core::AttributePath::new("ripgrep").unwrap())
        .unwrap();
        assert_eq!(
            ProductFrameCodec::encode_cli_request(
                10,
                &CliBrokerRequest::PrepareBuild(
                    OperationHandle(format!("op_{}", "b".repeat(64))),
                    vec![resolved],
                ),
            ),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let empty = encode_frame(
            CHANNEL_CLI_BROKER,
            18,
            10,
            format!(r#"{{"handle":"op_{}","selectors":[]}}"#, "b".repeat(64)).as_bytes(),
        )
        .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&empty),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let execute = CliBrokerRequest::ExecuteBuild(
            OperationHandle(format!("op_{}", "c".repeat(64))),
            Digest::from_bytes([0x24; 32]),
        );
        let encoded = ProductFrameCodec::encode_cli_request(11, &execute).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("buildPlanDigest"));
        for forbidden in ["estimate", "resource", "receipt", "target", "derivation"] {
            assert!(!wire.contains(forbidden), "execute exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((11, execute))
        );
        for code in [
            BuildExecutionErrorCode::ApprovalUnavailable,
            BuildExecutionErrorCode::ApprovalInvalidated,
            BuildExecutionErrorCode::ResourcePreflightFailed,
            BuildExecutionErrorCode::ExecutionFailed,
            BuildExecutionErrorCode::Cancelled,
            BuildExecutionErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::BuildExecutionRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(11, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((11, response))
            );
        }
        let report = BuildReport::new(
            BuildStatus::Built,
            vec![BuildOutput::new(
                path("built-1.0"),
                BuildOutputProvenance::LocalBuild,
            )],
        )
        .unwrap();
        let response = CliBrokerResponse::BuildExecuted(report);
        let encoded = ProductFrameCodec::encode_cli_response(11, &response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((11, response))
        );
        let response =
            CliBrokerResponse::BuildExecutionProgress(BuildProgressEstimate::new(420_000).unwrap());
        let encoded = ProductFrameCodec::encode_cli_response(11, &response).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains(r#""progressMillionths":420000"#));
        for forbidden in ["/nix/", "drv", "path", "system", "argv"] {
            assert!(
                !wire.contains(forbidden),
                "build progress exposed {forbidden}"
            );
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((11, response))
        );
        for payload in [
            br#"{"progressMillionths":0}"#.as_slice(),
            br#"{"progressMillionths":1000000}"#.as_slice(),
            br#"{"progressMillionths":1,"path":"/nix/store/x"}"#.as_slice(),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, 19, 11, payload).unwrap();
            assert!(ProductFrameCodec::decode_cli_response(&encoded).is_err());
        }

        let root_intent = RootSetIntent::new(
            GenerationId::new("gen-0007").unwrap(),
            vec![RootSetEntry::new(
                RootName::new("hello-out").unwrap(),
                path("built-1.0"),
            )],
        )
        .unwrap();
        let publish = CliBrokerRequest::PublishBuildRoots(
            OperationHandle(format!("op_{}", "d".repeat(64))),
            root_intent,
        );
        let encoded = ProductFrameCodec::encode_cli_request(12, &publish).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("generation"));
        assert!(wire.contains("entries"));
        assert!(!wire.contains("ownerUid"));
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((12, publish))
        );

        let published = CliBrokerResponse::BuildRootsPublished(RootSetReport::new(
            RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
            1,
            Digest::from_bytes([0x50; 32]),
        ));
        let encoded = ProductFrameCodec::encode_cli_response(12, &published).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((12, published))
        );
        for code in [
            BuildRootPublicationErrorCode::InvalidRootIntent,
            BuildRootPublicationErrorCode::PublicationFailed,
            BuildRootPublicationErrorCode::Cancelled,
            BuildRootPublicationErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::BuildRootPublicationRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(12, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((12, response))
            );
        }
        let extended = encode_frame(
            CHANNEL_CLI_BROKER,
            20,
            12,
            br#"{"error":"future_extension"}"#,
        )
        .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&extended),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let transition = CliBrokerRequest::TransitionGenerationRoots(
            OperationHandle(format!("op_{}", "e".repeat(64))),
            RootSetTransitionIntent::new(
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                vec![RootName::new("hello-out").unwrap()],
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_request(13, &transition).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("sourceGeneration"));
        assert!(wire.contains("destinationGeneration"));
        assert!(wire.contains("retainedNames"));
        for forbidden in ["ownerUid", "/nix/store", "target"] {
            assert!(!wire.contains(forbidden), "transition exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((13, transition))
        );
        let transitioned = CliBrokerResponse::GenerationRootsTransitioned(
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                    Digest::from_bytes([0x51; 32]),
                ),
                vec![RootName::new("hello-out").unwrap()],
                Digest::from_bytes([0x51; 32]),
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_response(13, &transitioned).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((13, transitioned))
        );
        for code in [
            GenerationRootTransitionErrorCode::InvalidIntent,
            GenerationRootTransitionErrorCode::TransitionFailed,
            GenerationRootTransitionErrorCode::Cancelled,
            GenerationRootTransitionErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::GenerationRootTransitionRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(13, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((13, response))
            );
        }
        let removal = CliBrokerRequest::RemoveGenerationRoots(
            OperationHandle(format!("op_{}", "9".repeat(64))),
            GenerationId::new("gen-0007").unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_request(14, &removal).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("generation"));
        for forbidden in ["ownerUid", "/nix/", "path", "target"] {
            assert!(!wire.contains(forbidden), "removal exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((14, removal))
        );
        let encoded =
            ProductFrameCodec::encode_cli_response(14, &CliBrokerResponse::GenerationRootsRemoved)
                .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((14, CliBrokerResponse::GenerationRootsRemoved))
        );
        for code in [
            GenerationRootRemovalErrorCode::InvalidIntent,
            GenerationRootRemovalErrorCode::RemovalFailed,
            GenerationRootRemovalErrorCode::Cancelled,
            GenerationRootRemovalErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::GenerationRootRemovalRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(14, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((14, response))
            );
        }
        let acquire_gc =
            CliBrokerRequest::AcquireGc(OperationHandle(format!("op_{}", "a".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_request(15, &acquire_gc).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["ownerUid", "generation", "/nix/", "path", "target"] {
            assert!(
                !wire.contains(forbidden),
                "GC admission exposed {forbidden}"
            );
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((15, acquire_gc))
        );
        let encoded =
            ProductFrameCodec::encode_cli_response(15, &CliBrokerResponse::GcAdmissionAcquired)
                .unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((15, CliBrokerResponse::GcAdmissionAcquired))
        );
        let install_evidence =
            CliBrokerRequest::GetInstallEvidence(OperationHandle(format!("op_{}", "c".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_request(16, &install_evidence).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["selector", "revision", "derivation", "path", "target"] {
            assert!(
                !wire.contains(forbidden),
                "evidence request exposed {forbidden}"
            );
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((16, install_evidence))
        );
        let attestation = CliBrokerRequest::AttestGenerationRoots(
            OperationHandle(format!("op_{}", "d".repeat(64))),
            GenerationId::new("gen-0007").unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_request(17, &attestation).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("generation"));
        for forbidden in ["ownerUid", "/nix/", "path", "target", "entries"] {
            assert!(!wire.contains(forbidden), "attestation exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((17, attestation))
        );
        let attested = CliBrokerResponse::GenerationRootsAttested(RootSetReport::new(
            RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
            1,
            Digest::from_bytes([0x53; 32]),
        ));
        let encoded = ProductFrameCodec::encode_cli_response(17, &attested).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((17, attested))
        );
        for code in [
            GenerationRootAttestationErrorCode::InvalidIntent,
            GenerationRootAttestationErrorCode::AttestationFailed,
            GenerationRootAttestationErrorCode::Cancelled,
            GenerationRootAttestationErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::GenerationRootAttestationRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(17, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((17, response))
            );
        }
        let selector = PackageSelector::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
        );
        let acquisition = CliBrokerRequest::AcquireInstall(
            OperationHandle(format!("op_{}", "e".repeat(64))),
            vec![selector],
        );
        let encoded = ProductFrameCodec::encode_cli_request(18, &acquisition).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["ownerUid", "/nix/", "derivation", "substituter", "key"] {
            assert!(!wire.contains(forbidden), "acquisition exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((18, acquisition))
        );
        for response in [
            CliBrokerResponse::InstallAcquired,
            CliBrokerResponse::InstallBuildRequired,
        ] {
            let encoded = ProductFrameCodec::encode_cli_response(18, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((18, response))
            );
        }
        let progress = CliBrokerResponse::InstallDownloadProgress(
            InstallDownloadProgress::new(SelectorInput::new("hello").unwrap(), 7, 11).unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_response(18, &progress).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains(r#""selector":"hello""#));
        assert!(!wire.contains("/nix/"));
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((18, progress))
        );
        for payload in [
            br#"{"selector":"hello","done":12,"total":11}"#.as_slice(),
            br#"{"selector":"hello","done":0,"total":0}"#.as_slice(),
            br#"{"selector":"hello","done":1,"total":2,"path":"/nix/store/x"}"#.as_slice(),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, 26, 18, payload).unwrap();
            assert!(ProductFrameCodec::decode_cli_response(&encoded).is_err());
        }
        for code in [
            CacheInstallErrorCode::InvalidIntent,
            CacheInstallErrorCode::AcquisitionFailed,
            CacheInstallErrorCode::Cancelled,
            CacheInstallErrorCode::AuthorityUnavailable,
        ] {
            let response = CliBrokerResponse::InstallAcquisitionRefused(code);
            let encoded = ProductFrameCodec::encode_cli_response(18, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((18, response))
            );
        }
        let complete =
            CliBrokerRequest::Complete(OperationHandle(format!("op_{}", "f".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_request(19, &complete).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((19, complete))
        );
        let encoded =
            ProductFrameCodec::encode_cli_response(19, &CliBrokerResponse::Completed).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((19, CliBrokerResponse::Completed))
        );

        let helper = BrokerHelperRequest::PublishRootSet(root_publication());
        let encoded = ProductFrameCodec::encode_helper_request(9, &helper).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((9, helper))
        );

        let transition = BrokerHelperRequest::TransitionRootSet(
            RootSetTransitionRequest::new(
                1001,
                GenerationId::new("gen-0007").unwrap(),
                GenerationId::new("gen-0008").unwrap(),
                vec![RootName::new("hello-out").unwrap()],
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_helper_request(10, &transition).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("sourceGeneration"));
        assert!(wire.contains("destinationGeneration"));
        assert!(wire.contains("retainedNames"));
        assert!(!wire.contains("/nix/store"));
        assert!(!wire.contains("target"));
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((10, transition))
        );

        let transitioned = BrokerHelperResponse::RootSetTransitioned(
            RootSetTransitionReport::new(
                RootSetReport::new(
                    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0008").unwrap(),
                    1,
                    Digest::from_bytes([0x52; 32]),
                ),
                vec![RootName::new("hello-out").unwrap()],
                Digest::from_bytes([0x52; 32]),
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_helper_response(10, &transitioned).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((10, transitioned))
        );
        let attestation = BrokerHelperRequest::AttestRootSet(RootSetAttestationRequest::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
        ));
        let encoded = ProductFrameCodec::encode_helper_request(11, &attestation).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("ownerUid"));
        assert!(wire.contains("generation"));
        for forbidden in ["/nix/", "path", "target", "entries"] {
            assert!(
                !wire.contains(forbidden),
                "helper attestation exposed {forbidden}"
            );
        }
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((11, attestation))
        );
        let attested = BrokerHelperResponse::RootSetAttested(RootSetReport::new(
            RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap(),
            1,
            Digest::from_bytes([0x54; 32]),
        ));
        let encoded = ProductFrameCodec::encode_helper_response(11, &attested).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((11, attested))
        );

        let started = CliBrokerResponse::Started(OperationHandle(format!("op_{}", "0".repeat(64))));
        let encoded = ProductFrameCodec::encode_cli_response(11, &started).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((11, started))
        );

        let handle = OperationHandle(format!("op_{}", "2".repeat(64)));
        let version_request = CliBrokerRequest::Version(handle);
        let encoded = ProductFrameCodec::encode_cli_request(13, &version_request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((13, version_request))
        );

        let version_response = CliBrokerResponse::Version(VersionInfo::new(
            NixVersion::new("2.34.8").unwrap(),
            AcceptedFormats::new(FormatVersion::new(1).unwrap()),
        ));
        let encoded = ProductFrameCodec::encode_cli_response(14, &version_response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((14, version_response))
        );

        let adapter_failure = CliBrokerResponse::AdapterFailure(
            MethodKind::Substitute,
            NixAdapterErrorCode::TrustFailure,
        );
        let encoded = ProductFrameCodec::encode_cli_response(15, &adapter_failure).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((15, adapter_failure))
        );

        let issued =
            BrokerHelperResponse::RepairCapabilityIssued(MaintenanceCapability("1".repeat(64)));
        let encoded = ProductFrameCodec::encode_helper_response(12, &issued).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((12, issued))
        );
    }

    #[test]
    fn managed_ownership_frames_are_path_free_and_strict() {
        let request = CliBrokerRequest::VerifyManagedOwnership(OperationHandle(format!(
            "op_{}",
            "7".repeat(64)
        )));
        let encoded = ProductFrameCodec::encode_cli_request(41, &request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((41, request))
        );
        let response = CliBrokerResponse::ManagedOwnership(true);
        let encoded = ProductFrameCodec::encode_cli_response(41, &response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((41, response))
        );

        let digest = Digest::from_bytes([0x5a; 32]);
        let request = BrokerHelperRequest::VerifyManagedOwnership(digest);
        let encoded = ProductFrameCodec::encode_helper_request(42, &request).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["path", "url", "command", "option", "trust"] {
            assert!(!wire.contains(forbidden));
        }
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((42, request))
        );
        let response = BrokerHelperResponse::ManagedOwnership(false);
        let encoded = ProductFrameCodec::encode_helper_response(42, &response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((42, response))
        );
    }

    #[test]
    fn typed_adapter_envelopes_preserve_nested_strictness_and_promote_paths() {
        let handle = format!("op_{}", "3".repeat(64));
        let duplicate_nested =
            format!(r#"{{"handle":"{handle}","request":{{"schemaVersion":1,"schemaVersion":1}}}}"#);
        let encoded =
            encode_frame(CHANNEL_CLI_BROKER, 11, 91, duplicate_nested.as_bytes()).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );

        let arbitrary_path = format!(r#"{{"handle":"{handle}","path":"/tmp/not-store"}}"#);
        let encoded = encode_frame(CHANNEL_CLI_BROKER, 12, 92, arbitrary_path.as_bytes()).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Err(FrameError::new(FrameErrorCode::InvalidPayload))
        );
    }

    #[test]
    fn repair_scope_round_trip_preserves_every_binding() {
        let scope = VerifiedRepairScope::new(
            1001,
            GenerationId::new("gen-0007").unwrap(),
            [path("hello-1.0"), path("glibc-2.39")],
            Some(body_digest(b"plan")),
            PolicyVersion::new(NonZeroU64::new(3).unwrap()),
            RepairMode::Build,
        )
        .unwrap();
        let request = BrokerHelperRequest::IssueRepairCapability(scope);
        let encoded = ProductFrameCodec::encode_helper_request(1, &request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((1, request))
        );
    }

    #[test]
    fn root_nix_complete_grammar_round_trips_all_requests_results_and_failures() {
        let pin = NixpkgsPin::new(
            "0123456789abcdef0123456789abcdef01234567",
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .unwrap();
        let requests = [
            RootNixRequest::Version,
            RootNixRequest::Evaluate(evaluation_request()),
            RootNixRequest::PathInfo(path("hello-1.0")),
            RootNixRequest::Substitute(path("hello-1.0")),
            RootNixRequest::SubstituteMany(vec![path("hello-1.0"), path("glibc-2.39")]),
            RootNixRequest::Build(build_request()),
            RootNixRequest::Verify(
                VerifyRequest::new(vec![path("hello")], VerifyMode::Recursive).unwrap(),
            ),
            RootNixRequest::Gc,
            RootNixRequest::CacheInspect(vec![path("hello-1.0")]),
            RootNixRequest::CacheInspectClosures(vec![path("hello-1.0")]),
            RootNixRequest::NixpkgsMetadata(pin),
            RootNixRequest::ClosureForRoots(vec![path("hello-1.0")]),
            RootNixRequest::RepairPlan(repair_request()),
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let request_id = index as u64 + 1;
            let request = BrokerHelperRequest::RootNix(request);
            let encoded = ProductFrameCodec::encode_helper_request(request_id, &request).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_helper_request(&encoded),
                Ok((request_id, request))
            );
        }

        let observation = CachePathObservation::hit(path("hello"), 1024, 4096);
        let proof = repair_proof();
        assert_eq!(
            proof.preview().build_plan_digest(),
            proof.digest().to_string().replacen("sha256-", "sha256:", 1)
        );
        let responses = [
            RootNixResponse::Version(VersionInfo::new(
                NixVersion::new("2.34.8").unwrap(),
                AcceptedFormats::new(FormatVersion::new(1).unwrap()),
            )),
            RootNixResponse::Evaluate(derivation_report()),
            RootNixResponse::PathInfo(path_info_report()),
            RootNixResponse::Substitute(
                SubstituteReport::miss(path("hello"), SubstituteOutcome::NoBinaryAvailable)
                    .unwrap(),
            ),
            RootNixResponse::SubstituteMany(vec![
                SubstituteReport::miss(path("hello"), SubstituteOutcome::NoBinaryAvailable)
                    .unwrap(),
            ]),
            RootNixResponse::Build(
                BuildReport::new(
                    BuildStatus::Built,
                    vec![BuildOutput::new(
                        path("hello"),
                        BuildOutputProvenance::LocalBuild,
                    )],
                )
                .unwrap(),
            ),
            RootNixResponse::Verify(verify_report()),
            RootNixResponse::Gc(
                GcReport::new(GcStatus::Collected, vec![path("old")], 1024).unwrap(),
            ),
            RootNixResponse::CacheInspect(vec![observation.clone()]),
            RootNixResponse::CacheInspectClosures(vec![
                CacheDownloadClosure::new(path("hello"), vec![observation]).unwrap(),
            ]),
            RootNixResponse::NixpkgsMetadata(br#"{"locked":{}}"#.to_vec()),
            RootNixResponse::ClosureForRoots(vec![path("hello"), path("glibc")]),
            RootNixResponse::RepairPlan(proof),
        ];
        for (index, response) in responses.into_iter().enumerate() {
            let request_id = index as u64 + 40;
            let response = BrokerHelperResponse::RootNix(Box::new(response));
            let encoded = ProductFrameCodec::encode_helper_response(request_id, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_helper_response(&encoded),
                Ok((request_id, response))
            );
        }

        for failure in [
            RootNixFailure::Adapter(NixAdapterErrorCode::Timeout),
            RootNixFailure::Cache(BuildCacheErrorCode::ProbeFailed),
            RootNixFailure::Nixpkgs(NixpkgsSourceErrorCode::RunnerFailure),
            RootNixFailure::Busy,
            RootNixFailure::Inactive,
        ] {
            let response = BrokerHelperResponse::RootNix(Box::new(RootNixResponse::Failed {
                operation: RootNixOperation::Version,
                failure,
            }));
            let encoded = ProductFrameCodec::encode_helper_response(77, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_helper_response(&encoded),
                Ok((77, response))
            );
        }
        let progress = BrokerHelperResponse::RootNix(Box::new(RootNixResponse::BuildProgress(
            BuildProgressEstimate::new(123).unwrap(),
        )));
        let encoded = ProductFrameCodec::encode_helper_response(78, &progress).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_response(&encoded),
            Ok((78, progress))
        );
    }

    #[test]
    fn preview_purpose_is_bound_to_each_enclosing_operation() {
        let repair = repair_proof().preview().clone();
        let build = normal_build_preview();
        let repair_notice =
            repair.to_json_value().unwrap()["readiness"]["resourceBoundary"]["notice"].clone();
        let mut repair_for_normal_frame = build.to_json_value().unwrap();
        repair_for_normal_frame["purpose"] = serde_json::json!("repair");
        repair_for_normal_frame["readiness"]["resourceBoundary"]["notice"] = repair_notice;
        let repair_for_normal_frame =
            BuildPreview::from_json_bytes(&serde_json::to_vec(&repair_for_normal_frame).unwrap())
                .unwrap();

        let proof = repair_proof();
        assert!(repair_request().accepts(&proof));
        let other_system = RootRepairPlanRequest::new(
            vec![path("hello")],
            PolicyVersion::from_u64(7).unwrap(),
            System::Aarch64Linux,
            BuildReadiness::new(true, false, true, true, true),
            8,
        )
        .unwrap();
        assert!(!other_system.accepts(&proof));

        assert!(RootRepairPlanProof::new(build.clone()).is_none());
        let mut root_payload = vec![ROOT_NIX_SUCCESS];
        root_payload.extend_from_slice(&build.to_json_bytes().unwrap());
        let root_frame = encode_frame(
            CHANNEL_BROKER_HELPER,
            RootNixOperation::RepairPlan.method_id(),
            71,
            &root_payload,
        )
        .unwrap();
        assert!(ProductFrameCodec::decode_helper_response(&root_frame).is_err());

        assert!(RepairGenerationReport::needs_approval(1, build.clone()).is_err());
        let repair_report = encode_json(&serde_json::json!({
            "status": repair_generation_status_name(RepairGenerationStatus::NeedsApproval),
            "damagedPaths": 1,
            "buildPreview": build.to_json_value().unwrap()
        }))
        .unwrap();
        let repair_frame = encode_frame(CHANNEL_CLI_BROKER, 30, 72, &repair_report).unwrap();
        assert!(ProductFrameCodec::decode_cli_response(&repair_frame).is_err());

        for response in [
            CliBrokerResponse::BuildPreview(repair_for_normal_frame.clone()),
            CliBrokerResponse::BuildPrepared(repair_for_normal_frame.clone()),
        ] {
            assert!(ProductFrameCodec::encode_cli_response(73, &response).is_err());
        }
        for method in [17, 18] {
            let frame = encode_frame(
                CHANNEL_CLI_BROKER,
                method,
                74,
                &repair_for_normal_frame.to_json_bytes().unwrap(),
            )
            .unwrap();
            assert!(ProductFrameCodec::decode_cli_response(&frame).is_err());
        }

        let mut impossible_cache_only = build.to_json_value().unwrap();
        impossible_cache_only["purpose"] = serde_json::json!("repair");
        impossible_cache_only["readiness"]["resourceBoundary"]["notice"] =
            repair.to_json_value().unwrap()["readiness"]["resourceBoundary"]["notice"].clone();
        impossible_cache_only["targets"][0]["localBuildRequired"] = serde_json::json!(false);
        impossible_cache_only["build"]["count"] = serde_json::json!(0);
        impossible_cache_only["build"]["names"] = serde_json::json!([]);
        impossible_cache_only["unknownLocalOutputs"] = serde_json::json!(0);
        impossible_cache_only["approvalRequired"] = serde_json::json!(false);
        let impossible_cache_only =
            BuildPreview::from_json_bytes(&serde_json::to_vec(&impossible_cache_only).unwrap())
                .unwrap();
        assert!(RootRepairPlanProof::new(impossible_cache_only.clone()).is_none());
        assert!(RepairGenerationReport::needs_approval(1, impossible_cache_only.clone()).is_err());

        let mut root_payload = vec![ROOT_NIX_SUCCESS];
        root_payload.extend_from_slice(&impossible_cache_only.to_json_bytes().unwrap());
        let root_frame = encode_frame(
            CHANNEL_BROKER_HELPER,
            RootNixOperation::RepairPlan.method_id(),
            75,
            &root_payload,
        )
        .unwrap();
        assert!(ProductFrameCodec::decode_helper_response(&root_frame).is_err());

        let report = encode_json(&serde_json::json!({
            "status": repair_generation_status_name(RepairGenerationStatus::NeedsApproval),
            "damagedPaths": 1,
            "buildPreview": impossible_cache_only.to_json_value().unwrap()
        }))
        .unwrap();
        let report_frame = encode_frame(CHANNEL_CLI_BROKER, 30, 76, &report).unwrap();
        assert!(ProductFrameCodec::decode_cli_response(&report_frame).is_err());
    }

    #[test]
    fn root_nix_path_lists_reject_empty_duplicate_invalid_and_excessive_values() {
        for payload in [
            br#"{"paths":[]}"#.to_vec(),
            format!(
                r#"{{"paths":["{}","{}"]}}"#,
                path("same").as_str(),
                path("same").as_str()
            )
            .into_bytes(),
            br#"{"paths":["/tmp/not-store"]}"#.to_vec(),
        ] {
            let frame = encode_frame(
                CHANNEL_BROKER_HELPER,
                RootNixOperation::SubstituteMany.method_id(),
                90,
                &payload,
            )
            .unwrap();
            assert!(ProductFrameCodec::decode_helper_request(&frame).is_err());
        }

        let excessive_paths = (0..=MAX_ROOT_NIX_PATHS)
            .map(|_| path("hello-1.0"))
            .collect::<Vec<_>>();
        let excessive = serde_json::to_vec(&RootNixPathsWire {
            paths: excessive_paths.iter().map(StorePath::as_str).collect(),
        })
        .unwrap();
        let frame = encode_frame(
            CHANNEL_BROKER_HELPER,
            RootNixOperation::SubstituteMany.method_id(),
            91,
            &excessive,
        )
        .unwrap();
        assert!(ProductFrameCodec::decode_helper_request(&frame).is_err());

        let duplicate = CachePathObservation::miss(path("same"));
        let response =
            BrokerHelperResponse::RootNix(Box::new(RootNixResponse::CacheInspect(vec![
                duplicate.clone(),
                duplicate,
            ])));
        assert!(ProductFrameCodec::encode_helper_response(92, &response).is_err());
    }

    #[test]
    fn root_nix_result_kind_is_exact_and_helper_cap_includes_report_overhead() {
        let wrong_body = GcReport::new(GcStatus::RefusedUnderLease, vec![], 0)
            .unwrap()
            .encode()
            .unwrap();
        let mut payload = vec![ROOT_NIX_SUCCESS];
        payload.extend_from_slice(&wrong_body);
        let frame = encode_frame(
            CHANNEL_BROKER_HELPER,
            RootNixOperation::Version.method_id(),
            92,
            &payload,
        )
        .unwrap();
        assert!(ProductFrameCodec::decode_helper_response(&frame).is_err());

        let metadata = vec![b' '; HELPER_FRAME_PAYLOAD_LIMIT - 1];
        let response =
            BrokerHelperResponse::RootNix(Box::new(RootNixResponse::NixpkgsMetadata(metadata)));
        let frame = ProductFrameCodec::encode_helper_response(93, &response).unwrap();
        assert_eq!(frame.len(), HEADER_BYTES + HELPER_FRAME_PAYLOAD_LIMIT);

        let oversized = BrokerHelperResponse::RootNix(Box::new(RootNixResponse::NixpkgsMetadata(
            vec![b' '; HELPER_FRAME_PAYLOAD_LIMIT],
        )));
        assert_eq!(
            ProductFrameCodec::encode_helper_response(94, &oversized),
            Err(FrameError::new(FrameErrorCode::FrameTooLarge))
        );
    }

    #[test]
    fn framing_rejects_wrong_channel_version_length_and_extension() {
        let request = CliBrokerRequest::Begin(BrokerOperationKind::Doctor);
        let encoded = ProductFrameCodec::encode_cli_request(1, &request).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded)
                .unwrap_err()
                .code(),
            FrameErrorCode::UnsupportedMessage
        );
        let mut bad_version = encoded.clone();
        bad_version[5] = 2;
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&bad_version)
                .unwrap_err()
                .code(),
            FrameErrorCode::UnsupportedVersion
        );
        let mut bad_length = encoded;
        bad_length.push(b' ');
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&bad_length)
                .unwrap_err()
                .code(),
            FrameErrorCode::LengthMismatch
        );
        let payload = br#"{"operation":"doctor","argv":[]}"#;
        let extended = encode_frame(CHANNEL_CLI_BROKER, 1, 1, payload).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&extended)
                .unwrap_err()
                .code(),
            FrameErrorCode::InvalidPayload
        );
    }

    #[test]
    fn repair_generation_frames_keep_paths_and_nix_controls_private() {
        let request = CliBrokerRequest::RepairGeneration(
            OperationHandle(format!("op_{}", "8".repeat(64))),
            RepairGenerationRequest::new(GenerationId::new("gen-0042").unwrap(), true),
        );
        let encoded = ProductFrameCodec::encode_cli_request(30, &request).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("gen-0042"));
        assert!(wire.contains("verifyOnly"));
        for forbidden in [
            "/nix/",
            "path",
            "derivation",
            "installable",
            "substituter",
            "trusted-public-key",
            "max-jobs",
            "builders",
            "argv",
        ] {
            assert!(!wire.contains(forbidden), "repair exposed {forbidden}");
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((30, request))
        );

        let approved = CliBrokerRequest::RepairGeneration(
            OperationHandle(format!("op_{}", "9".repeat(64))),
            RepairGenerationRequest::with_approval(
                GenerationId::new("gen-0042").unwrap(),
                BuildApprovalRequest::new(body_digest(b"repair plan"), ApprovalSource::Interactive),
            ),
        );
        let encoded = ProductFrameCodec::encode_cli_request(31, &approved).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(wire.contains("buildPlanDigest"));
        assert!(wire.contains("interactive"));
        assert!(!wire.contains("/nix/"));
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((31, approved))
        );

        let responses = [
            CliBrokerResponse::RepairGeneration(
                RepairGenerationReport::new(RepairGenerationStatus::Clean, 0).unwrap(),
            ),
            CliBrokerResponse::RepairGeneration(
                RepairGenerationReport::new(RepairGenerationStatus::DamageDetected, 3).unwrap(),
            ),
            CliBrokerResponse::RepairGenerationRefused(
                RepairGenerationErrorCode::FreshApprovalRequired,
            ),
        ];
        for response in responses {
            let encoded = ProductFrameCodec::encode_cli_response(30, &response).unwrap();
            let wire = String::from_utf8_lossy(&encoded);
            assert!(!wire.contains("/nix/"));
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((30, response))
            );
        }

        for payload in [
            br#"{"status":"clean","damagedPaths":1}"#.as_slice(),
            br#"{"status":"damage-detected","damagedPaths":0}"#.as_slice(),
            br#"{"handle":"op_x","generation":"gen-0042","verifyOnly":true,"path":"/nix/store/x"}"#
                .as_slice(),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, 30, 30, payload).unwrap();
            assert!(
                ProductFrameCodec::decode_cli_request(&encoded).is_err()
                    || ProductFrameCodec::decode_cli_response(&encoded).is_err()
            );
        }

        let load = BrokerHelperRequest::LoadRepairRootSet(RootSetAttestationRequest::new(
            1000,
            GenerationId::new("gen-0042").unwrap(),
        ));
        let encoded = ProductFrameCodec::encode_helper_request(7, &load).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        assert!(!wire.contains("/nix/store"));
        assert_eq!(
            ProductFrameCodec::decode_helper_request(&encoded),
            Ok((7, load))
        );
    }

    #[test]
    fn channel_refresh_frame_contains_only_handle_and_sanitized_result() {
        for mode in [
            ChannelRefreshMode::Apply,
            ChannelRefreshMode::Check,
            ChannelRefreshMode::Force,
        ] {
            let request = CliBrokerRequest::RefreshChannel(
                OperationHandle(format!("op_{}", "9".repeat(64))),
                mode,
            );
            let encoded = ProductFrameCodec::encode_cli_request(27, &request).unwrap();
            let wire = String::from_utf8_lossy(&encoded);
            for forbidden in [
                "url",
                "target",
                "system",
                "root",
                "key",
                "descriptor",
                "index",
            ] {
                assert!(!wire.contains(forbidden), "refresh exposed {forbidden}");
            }
            assert!(wire.contains(mode.as_str()));
            assert_eq!(
                ProductFrameCodec::decode_cli_request(&encoded),
                Ok((27, request))
            );
        }

        let mut responses = vec![CliBrokerResponse::ChannelRefreshed(
            ChannelRefreshReport::new(true, ChannelSequence::from_u64(42).unwrap()),
        )];
        responses.extend(
            [
                ChannelRefreshErrorCode::Network,
                ChannelRefreshErrorCode::Verification,
                ChannelRefreshErrorCode::Busy,
                ChannelRefreshErrorCode::ServiceUnavailable,
            ]
            .map(CliBrokerResponse::ChannelRefreshRefused),
        );
        for response in responses {
            let encoded = ProductFrameCodec::encode_cli_response(27, &response).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_cli_response(&encoded),
                Ok((27, response))
            );
        }
        for payload in [
            br#"{"updated":true,"sequence":0}"#.as_slice(),
            br#"{"updated":true,"sequence":42,"url":"https://example.invalid"}"#.as_slice(),
            br#"{"error":"unknown"}"#.as_slice(),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, 27, 27, payload).unwrap();
            assert!(ProductFrameCodec::decode_cli_response(&encoded).is_err());
        }
        let invalid_request = encode_frame(
            CHANNEL_CLI_BROKER,
            27,
            27,
            br#"{"handle":"op_9999999999999999999999999999999999999999999999999999999999999999","mode":"unsafe"}"#,
        )
        .unwrap();
        assert!(ProductFrameCodec::decode_cli_request(&invalid_request).is_err());
    }

    #[test]
    fn catalog_frames_are_bounded_product_metadata_only() {
        let handle = OperationHandle(format!("op_{}", "8".repeat(64)));
        let request = CliBrokerRequest::SearchCatalog(
            handle.clone(),
            CatalogSearchRequest::new("ripgrep", 25, false, Some("MIT")).unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_request(28, &request).unwrap();
        let wire = String::from_utf8_lossy(&encoded);
        for forbidden in ["url", "system", "index", "nixpkgs", "store", "target"] {
            assert!(
                !wire.contains(forbidden),
                "catalog request exposed {forbidden}"
            );
        }
        assert_eq!(
            ProductFrameCodec::decode_cli_request(&encoded),
            Ok((28, request))
        );

        let summary = CatalogPackageSummary::new(
            "ripgrep",
            "ripgrep",
            "14.1.1",
            "fast search",
            vec![String::from("MIT")],
            true,
            false,
        )
        .unwrap();
        let search = CliBrokerResponse::CatalogSearch(
            CatalogSearchReport::new(
                ChannelSequence::from_u64(42).unwrap(),
                "2026-08-19T00:00:00Z",
                vec![summary.clone()],
            )
            .unwrap(),
        );
        let encoded = ProductFrameCodec::encode_cli_response(28, &search).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((28, search))
        );

        let info = CatalogPackageInfo::new(
            summary,
            "https://example.invalid/ripgrep",
            vec![String::from("out")],
            vec![String::from("macos-apple-silicon")],
            "0123456789abcdef0123456789abcdef01234567",
            "2026-08-12T00:00:00Z",
        )
        .unwrap();
        let response = CliBrokerResponse::CatalogInfo(vec![
            CatalogInfoReport::new(
                ChannelSequence::from_u64(42).unwrap(),
                CatalogInfoLookup::Found(Box::new(info)),
            )
            .unwrap(),
        ]);
        let encoded = ProductFrameCodec::encode_cli_response(29, &response).unwrap();
        assert_eq!(
            ProductFrameCodec::decode_cli_response(&encoded),
            Ok((29, response))
        );

        for (method, payload) in [
            (
                28,
                br#"{"handle":"bad","query":"ripgrep","limit":0,"exact":false,"license":null}"#
                    .as_slice(),
            ),
            (
                29,
                br#"{"handle":"bad","selectors":["bad\nselector"]}"#.as_slice(),
            ),
            (28, br#"{"sequence":0,"results":[]}"#.as_slice()),
            (
                29,
                br#"[{"sequence":42,"status":"found","package":null,"candidates":[]}]"#.as_slice(),
            ),
        ] {
            let encoded = encode_frame(CHANNEL_CLI_BROKER, method, 30, payload).unwrap();
            assert!(
                ProductFrameCodec::decode_cli_request(&encoded).is_err()
                    || ProductFrameCodec::decode_cli_response(&encoded).is_err()
            );
        }
        assert!(
            ProductFrameCodec::encode_cli_request(
                31,
                &CliBrokerRequest::InfoCatalog(handle, Vec::new()),
            )
            .is_err()
        );
        assert!(
            ProductFrameCodec::encode_cli_response(
                31,
                &CliBrokerResponse::CatalogInfo(Vec::new()),
            )
            .is_err()
        );
    }

    #[test]
    fn helper_grammar_rejects_raw_paths_options_and_forged_capabilities() {
        for payload in [
            br#"{"capability":"/nix/store/raw"}"#.as_slice(),
            br#"{"capability":"--substituters"}"#.as_slice(),
            br#"{"capability":"github:NixOS/nixpkgs"}"#.as_slice(),
            br#"{"capability":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#
                .as_slice(),
        ] {
            let frame = encode_frame(CHANNEL_BROKER_HELPER, 4, 1, payload).unwrap();
            assert_eq!(
                ProductFrameCodec::decode_helper_request(&frame)
                    .unwrap_err()
                    .code(),
                FrameErrorCode::InvalidPayload
            );
        }
    }

    #[test]
    fn arbitrary_short_frames_never_panic_or_decode_as_valid() {
        for length in 0..HEADER_BYTES {
            for fill in [0_u8, 0x41, 0xff] {
                let bytes = vec![fill; length];
                assert!(ProductFrameCodec::decode_cli_request(&bytes).is_err());
                assert!(ProductFrameCodec::decode_helper_request(&bytes).is_err());
                assert!(ProductFrameCodec::decode_cli_response(&bytes).is_err());
                assert!(ProductFrameCodec::decode_helper_response(&bytes).is_err());
            }
        }
    }
}
