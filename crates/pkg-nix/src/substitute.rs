//! Cache-only acquisition and post-substitution verification.
//!
//! The adapter owns cryptographic verification under immutable managed-Nix
//! configuration. This layer additionally binds the observed signature name
//! to the authenticated channel policy, runs recursive read-only integrity and
//! trust verification, and checks post-copy metadata against the substitution
//! receipt before returning a usable store object.

use std::fmt;

use pkg_channel::CachePolicy;

use crate::{
    NarHash, NarIntegrity, NixAdapter, NixAdapterError, NixAdapterErrorCode, PathInfoReport,
    Signature, StorePath, SubstituteOutcome, TrustStatus, VerifyMode, VerifyRequest,
};

/// A normal cache-availability result; local-build policy decides what follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMiss {
    /// The expected path was absent from configured substituters.
    Absent,
    /// The cache explicitly had no binary for the expected path.
    NoBinaryAvailable,
}

/// A substituted path accepted only after trust, integrity, and metadata checks.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedSubstitute {
    store_path: StorePath,
    nar_hash: NarHash,
    signatures: Vec<Signature>,
    references: Vec<StorePath>,
    nar_size: u64,
    closure_size: u64,
}

impl VerifiedSubstitute {
    /// Returns the verified store path.
    #[must_use]
    pub const fn store_path(&self) -> &StorePath {
        &self.store_path
    }
    /// Returns the recomputed, receipt-matched NAR hash.
    #[must_use]
    pub const fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }
    /// Returns cache signatures observed at substitution time.
    #[must_use]
    pub fn signatures(&self) -> &[Signature] {
        &self.signatures
    }
    /// Returns the verified path's direct references.
    #[must_use]
    pub fn references(&self) -> &[StorePath] {
        &self.references
    }
    /// Returns the verified NAR size.
    #[must_use]
    pub const fn nar_size(&self) -> u64 {
        self.nar_size
    }
    /// Returns the verified closure size reported after acquisition.
    #[must_use]
    pub const fn closure_size(&self) -> u64 {
        self.closure_size
    }
}

impl fmt::Debug for VerifiedSubstitute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedSubstitute")
            .field("nar_hash", &self.nar_hash)
            .field("signature_count", &self.signatures.len())
            .field("reference_count", &self.references.len())
            .field("nar_size", &self.nar_size)
            .field("closure_size", &self.closure_size)
            .finish_non_exhaustive()
    }
}

/// Cache-only acquisition result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstituteResult {
    /// A path was fetched and fully verified.
    Fetched(VerifiedSubstitute),
    /// No cache object was available; this is not a technical failure.
    Miss(CacheMiss),
}

/// Stable, redacted cache-acquisition failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteErrorCode {
    /// The adapter failed before a trustworthy result existed.
    AdapterFailure,
    /// No observed signature named an authenticated channel key.
    UnapprovedSignature,
    /// Recursive verification observed corrupt NAR contents.
    IntegrityFailure,
    /// Recursive verification did not consider the closure trusted.
    TrustFailure,
    /// Adapter reports disagreed about the requested path or cache metadata.
    MetadataMismatch,
}

/// A closed error that carries only stable categories, never paths or Nix output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstituteError {
    code: SubstituteErrorCode,
    adapter_code: Option<NixAdapterErrorCode>,
}

impl SubstituteError {
    const fn new(code: SubstituteErrorCode) -> Self {
        Self {
            code,
            adapter_code: None,
        }
    }

    const fn adapter(error: &NixAdapterError) -> Self {
        let code = match error.code() {
            NixAdapterErrorCode::TrustFailure => SubstituteErrorCode::TrustFailure,
            NixAdapterErrorCode::IntegrityFailure => SubstituteErrorCode::IntegrityFailure,
            _ => SubstituteErrorCode::AdapterFailure,
        };
        Self {
            code,
            adapter_code: Some(error.code()),
        }
    }

    /// Returns the stable public failure category.
    #[must_use]
    pub const fn code(self) -> SubstituteErrorCode {
        self.code
    }

    /// Returns the nested adapter category when the adapter originated failure.
    #[must_use]
    pub const fn adapter_code(self) -> Option<NixAdapterErrorCode> {
        self.adapter_code
    }
}

impl fmt::Display for SubstituteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "cache substitution refused: {:?}", self.code)
    }
}

impl std::error::Error for SubstituteError {}

/// Tries cache-only acquisition under the authenticated channel policy.
///
/// Cache absence returns [`SubstituteResult::Miss`]. Every fetched path must
/// have a substitution-time signature naming an authenticated key, pass
/// recursive read-only NAR/trust verification, and retain the same NAR hash
/// and signatures in post-copy path metadata.
pub fn acquire_substitute(
    store_path: &StorePath,
    cache_policy: &CachePolicy,
    adapter: &dyn NixAdapter,
) -> Result<SubstituteResult, SubstituteError> {
    acquire_with_trust(store_path, adapter, cache_policy.url(), |name| {
        cache_policy.admits_signature_name(name)
    })
}

fn acquire_with_trust(
    store_path: &StorePath,
    adapter: &dyn NixAdapter,
    expected_source_url: &str,
    admits_signature_name: impl Fn(&str) -> bool,
) -> Result<SubstituteResult, SubstituteError> {
    let substituted = adapter
        .substitute(store_path)
        .map_err(|error| SubstituteError::adapter(&error))?;
    if substituted.store_path() != store_path {
        return Err(SubstituteError::new(SubstituteErrorCode::MetadataMismatch));
    }
    match substituted.outcome() {
        SubstituteOutcome::AbsentFromSubstituters => {
            return Ok(SubstituteResult::Miss(CacheMiss::Absent));
        }
        SubstituteOutcome::NoBinaryAvailable => {
            return Ok(SubstituteResult::Miss(CacheMiss::NoBinaryAvailable));
        }
        SubstituteOutcome::Fetched => {}
    }
    let receipt = substituted
        .receipt()
        .ok_or_else(|| SubstituteError::new(SubstituteErrorCode::MetadataMismatch))?;
    if receipt.source_url() != expected_source_url {
        return Err(SubstituteError::new(SubstituteErrorCode::MetadataMismatch));
    }
    if !receipt
        .signatures()
        .iter()
        .any(|signature| admits_signature_name(signature.key_name()))
    {
        return Err(SubstituteError::new(
            SubstituteErrorCode::UnapprovedSignature,
        ));
    }

    let verify_request = VerifyRequest::new(vec![store_path.clone()], VerifyMode::Recursive)
        .map_err(|_| SubstituteError::new(SubstituteErrorCode::MetadataMismatch))?;
    let verification = adapter
        .verify(&verify_request)
        .map_err(|error| SubstituteError::adapter(&error))?;
    if !verification
        .results()
        .iter()
        .any(|result| result.path() == store_path)
    {
        return Err(SubstituteError::new(SubstituteErrorCode::MetadataMismatch));
    }
    if verification
        .results()
        .iter()
        .any(|result| result.nar_integrity() == NarIntegrity::Corrupt)
    {
        return Err(SubstituteError::new(SubstituteErrorCode::IntegrityFailure));
    }
    if verification
        .results()
        .iter()
        .any(|result| result.trust() == TrustStatus::Untrusted)
    {
        return Err(SubstituteError::new(SubstituteErrorCode::TrustFailure));
    }

    let info = adapter
        .path_info(store_path)
        .map_err(|error| SubstituteError::adapter(&error))?;
    validate_metadata(store_path, receipt.nar_hash(), receipt.signatures(), &info)?;
    Ok(SubstituteResult::Fetched(VerifiedSubstitute {
        store_path: store_path.clone(),
        nar_hash: info.nar_hash().clone(),
        signatures: receipt.signatures().to_vec(),
        references: info.references().to_vec(),
        nar_size: info.nar_size(),
        closure_size: info.closure_size(),
    }))
}

fn validate_metadata(
    requested: &StorePath,
    receipt_nar_hash: &NarHash,
    receipt_signatures: &[Signature],
    info: &PathInfoReport,
) -> Result<(), SubstituteError> {
    if info.store_path() != requested
        || info.nar_hash() != receipt_nar_hash
        || receipt_signatures
            .iter()
            .any(|signature| !info.signatures().contains(signature))
    {
        return Err(SubstituteError::new(SubstituteErrorCode::MetadataMismatch));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BuildPlanTarget, BuildReport, BuildRequest, DerivationPlanReport, Digest,
        EvaluateDerivationRequest, EvaluatedDerivation, GcReport, InstallEvidence, MethodKind,
        PathVerifyResult, SubstituteReceipt, SubstituteReport, VerifyReport, VersionInfo,
    };
    use pkg_core::{
        AttributePath, ChannelSequence, OutputName, OutputSelection, PackageVersion, PolicyVersion,
        SelectorId, SelectorInput, SourceRevision, System, VersionPreference,
    };
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use std::sync::Mutex;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const OTHER_NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn path() -> StorePath {
        StorePath::new(&format!("/nix/store/{HASH}-hello-2.12.2")).unwrap()
    }

    fn nar(value: &str) -> NarHash {
        NarHash::new(value).unwrap()
    }

    fn signature(name: &str) -> Signature {
        Signature::new(&format!("{name}:BBBBBBBB")).unwrap()
    }

    fn fetched() -> SubstituteReport {
        SubstituteReport::fetched(
            path(),
            SubstituteReceipt::new(
                "https://cache.nixos.org",
                nar(NAR),
                vec![signature("cache.nixos.org-1")],
            )
            .unwrap(),
        )
    }

    fn verified(integrity: NarIntegrity, trust: TrustStatus) -> VerifyReport {
        VerifyReport::new(vec![PathVerifyResult::new(path(), integrity, trust)]).unwrap()
    }

    fn info(nar_hash: NarHash) -> PathInfoReport {
        PathInfoReport::new(
            path(),
            nar_hash,
            vec![signature("cache.nixos.org-1")],
            vec![],
            Some(
                crate::DerivationPath::from_str(&format!("/nix/store/{HASH}-hello-2.12.2.drv"))
                    .unwrap(),
            ),
            100,
            100,
        )
        .unwrap()
    }

    fn target() -> BuildPlanTarget {
        let root = crate::DerivationPath::from_str(&format!("/nix/store/{HASH}-hello-2.12.2.drv"))
            .unwrap();
        let output_name = OutputName::new("out").unwrap();
        let mut outputs = BTreeMap::new();
        outputs.insert(output_name.clone(), path());
        let evaluated = EvaluatedDerivation::new(
            root.clone(),
            "hello-2.12.2".to_owned(),
            System::X8664Linux,
            outputs,
            Digest::from_bytes([8; 32]),
            false,
        )
        .unwrap();
        let plan = DerivationPlanReport::new(
            4,
            root,
            vec![output_name],
            vec![evaluated],
            Digest::from_bytes([9; 32]),
            "hello".to_owned(),
            PackageVersion::new("2.12.2"),
        )
        .unwrap();
        BuildPlanTarget::new(
            SelectorId::new("sel_hello").unwrap(),
            SelectorInput::new("hello").unwrap(),
            AttributePath::new("hello").unwrap(),
            VersionPreference::Any,
            OutputSelection::default_selection(),
            SourceRevision::CurrentChannel,
            plan,
        )
    }

    struct Adapter {
        substitute: Mutex<Option<Result<SubstituteReport, NixAdapterError>>>,
        verify: Mutex<Option<Result<VerifyReport, NixAdapterError>>>,
        path_info: Mutex<Option<Result<PathInfoReport, NixAdapterError>>>,
        calls: Mutex<Vec<MethodKind>>,
    }

    impl Adapter {
        fn new(
            substitute: Result<SubstituteReport, NixAdapterError>,
            verify: Option<Result<VerifyReport, NixAdapterError>>,
            path_info: Option<Result<PathInfoReport, NixAdapterError>>,
        ) -> Self {
            Self {
                substitute: Mutex::new(Some(substitute)),
                verify: Mutex::new(verify),
                path_info: Mutex::new(path_info),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn take<T>(
            slot: &Mutex<Option<Result<T, NixAdapterError>>>,
            kind: MethodKind,
        ) -> Result<T, NixAdapterError> {
            slot.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_else(|| Err(NixAdapterError::unexpected_extra_call(kind)))
        }

        fn record(&self, kind: MethodKind) {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(kind);
        }

        fn calls(&self) -> Vec<MethodKind> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl NixAdapter for Adapter {
        fn version(&self) -> Result<VersionInfo, NixAdapterError> {
            Err(NixAdapterError::unexpected_extra_call(MethodKind::Version))
        }

        fn evaluate_derivation(
            &self,
            _: &EvaluateDerivationRequest,
        ) -> Result<DerivationPlanReport, NixAdapterError> {
            Err(NixAdapterError::unexpected_extra_call(
                MethodKind::EvaluateDerivation,
            ))
        }

        fn path_info(&self, _: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
            self.record(MethodKind::PathInfo);
            Self::take(&self.path_info, MethodKind::PathInfo)
        }

        fn substitute(&self, _: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
            self.record(MethodKind::Substitute);
            Self::take(&self.substitute, MethodKind::Substitute)
        }

        fn build(&self, _: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
            Err(NixAdapterError::unexpected_extra_call(MethodKind::Build))
        }

        fn verify(&self, _: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
            self.record(MethodKind::Verify);
            Self::take(&self.verify, MethodKind::Verify)
        }

        fn gc(&self) -> Result<GcReport, NixAdapterError> {
            Err(NixAdapterError::unexpected_extra_call(MethodKind::Gc))
        }
    }

    #[test]
    fn cache_miss_is_normal_and_stops_after_substitute() {
        for (outcome, expected) in [
            (SubstituteOutcome::AbsentFromSubstituters, CacheMiss::Absent),
            (
                SubstituteOutcome::NoBinaryAvailable,
                CacheMiss::NoBinaryAvailable,
            ),
        ] {
            let adapter = Adapter::new(
                Ok(SubstituteReport::miss(path(), outcome).unwrap()),
                None,
                None,
            );
            assert_eq!(
                acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| true).unwrap(),
                SubstituteResult::Miss(expected)
            );
            assert_eq!(adapter.calls(), [MethodKind::Substitute]);
        }
    }

    #[test]
    fn fetched_path_requires_approved_signature_verify_then_matching_info() {
        let adapter = Adapter::new(
            Ok(fetched()),
            Some(Ok(verified(NarIntegrity::Intact, TrustStatus::Trusted))),
            Some(Ok(info(nar(NAR)))),
        );
        let result = acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |name| {
            name == "cache.nixos.org-1"
        })
        .unwrap();
        let SubstituteResult::Fetched(result) = result else {
            panic!("expected fetched result");
        };
        assert_eq!(result.store_path(), &path());
        assert_eq!(result.nar_hash(), &nar(NAR));
        assert_eq!(
            adapter.calls(),
            [
                MethodKind::Substitute,
                MethodKind::Verify,
                MethodKind::PathInfo
            ]
        );
        let debug = format!("{result:?}");
        assert!(!debug.contains("/nix/store"));
        assert!(!debug.contains(HASH));
    }

    #[test]
    fn verified_substitute_creates_cache_signed_install_evidence() {
        let acquire_adapter = Adapter::new(
            Ok(fetched()),
            Some(Ok(verified(NarIntegrity::Intact, TrustStatus::Trusted))),
            Some(Ok(info(nar(NAR)))),
        );
        let SubstituteResult::Fetched(substitute) = acquire_with_trust(
            &path(),
            &acquire_adapter,
            "https://cache.nixos.org",
            |name| name == "cache.nixos.org-1",
        )
        .unwrap() else {
            panic!("expected verified substitute");
        };
        let evidence_adapter = Adapter::new(
            Err(NixAdapterError::unexpected_extra_call(
                MethodKind::Substitute,
            )),
            None,
            Some(Ok(info(nar(NAR)))),
        );
        let evidence = InstallEvidence::from_cache_substitutes(
            Digest::from_bytes([3; 32]),
            ChannelSequence::from_u64(42).unwrap(),
            PolicyVersion::from_u64(7).unwrap(),
            crate::NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            nar(NAR),
            System::X8664Linux,
            vec![target()],
            vec![substitute],
            &evidence_adapter,
        )
        .unwrap();
        assert_eq!(evidence.targets().len(), 1);
        assert_eq!(evidence.targets()[0].acquired().len(), 1);
        assert_eq!(
            evidence.targets()[0].acquired()[0].provenance(),
            crate::BuildOutputProvenance::CacheSigned
        );
        assert_eq!(evidence_adapter.calls(), [MethodKind::PathInfo]);
    }

    #[test]
    fn cache_evidence_refuses_fresh_metadata_drift() {
        let acquire_adapter = Adapter::new(
            Ok(fetched()),
            Some(Ok(verified(NarIntegrity::Intact, TrustStatus::Trusted))),
            Some(Ok(info(nar(NAR)))),
        );
        let SubstituteResult::Fetched(substitute) = acquire_with_trust(
            &path(),
            &acquire_adapter,
            "https://cache.nixos.org",
            |name| name == "cache.nixos.org-1",
        )
        .unwrap() else {
            panic!("expected verified substitute");
        };
        let evidence_adapter = Adapter::new(
            Err(NixAdapterError::unexpected_extra_call(
                MethodKind::Substitute,
            )),
            None,
            Some(Ok(info(nar(OTHER_NAR)))),
        );
        assert!(
            InstallEvidence::from_cache_substitutes(
                Digest::from_bytes([3; 32]),
                ChannelSequence::from_u64(42).unwrap(),
                PolicyVersion::from_u64(7).unwrap(),
                crate::NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567",).unwrap(),
                nar(NAR),
                System::X8664Linux,
                vec![target()],
                vec![substitute],
                &evidence_adapter,
            )
            .is_err()
        );
    }

    #[test]
    fn unapproved_signature_and_corrupt_nar_fail_before_metadata_use() {
        let wrong_source = SubstituteReport::fetched(
            path(),
            SubstituteReceipt::new(
                "https://evil.invalid",
                nar(NAR),
                vec![signature("cache.nixos.org-1")],
            )
            .unwrap(),
        );
        let adapter = Adapter::new(Ok(wrong_source), None, None);
        assert_eq!(
            acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| true)
                .unwrap_err()
                .code(),
            SubstituteErrorCode::MetadataMismatch
        );
        assert_eq!(adapter.calls(), [MethodKind::Substitute]);

        let adapter = Adapter::new(Ok(fetched()), None, None);
        assert_eq!(
            acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| false)
                .unwrap_err()
                .code(),
            SubstituteErrorCode::UnapprovedSignature
        );
        assert_eq!(adapter.calls(), [MethodKind::Substitute]);

        let adapter = Adapter::new(
            Ok(fetched()),
            Some(Ok(verified(NarIntegrity::Corrupt, TrustStatus::Trusted))),
            None,
        );
        assert_eq!(
            acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| true)
                .unwrap_err()
                .code(),
            SubstituteErrorCode::IntegrityFailure
        );
        assert_eq!(
            adapter.calls(),
            [MethodKind::Substitute, MethodKind::Verify]
        );
    }

    #[test]
    fn metadata_or_adapter_trust_failure_never_returns_a_store_object() {
        let adapter = Adapter::new(
            Ok(fetched()),
            Some(Ok(verified(NarIntegrity::Intact, TrustStatus::Trusted))),
            Some(Ok(info(nar(OTHER_NAR)))),
        );
        assert_eq!(
            acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| true)
                .unwrap_err()
                .code(),
            SubstituteErrorCode::MetadataMismatch
        );

        let adapter = Adapter::new(Err(NixAdapterError::TrustFailure), None, None);
        let error =
            acquire_with_trust(&path(), &adapter, "https://cache.nixos.org", |_| true).unwrap_err();
        assert_eq!(error.code(), SubstituteErrorCode::TrustFailure);
        assert_eq!(
            error.adapter_code(),
            Some(NixAdapterErrorCode::TrustFailure)
        );
        assert!(!error.to_string().contains("/nix/store"));
    }
}
