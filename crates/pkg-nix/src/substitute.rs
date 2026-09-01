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
mod tests;
