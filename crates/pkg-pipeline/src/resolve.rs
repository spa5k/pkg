use std::fmt;

use pkg_core::{PackageSelector, System};
use pkg_index::IndexDocument;
use pkg_nix::{NixAdapter, VerifiedNixpkgsSource};
use pkg_resolver::{ResolveError, ResolvedPackagePlan, resolve_package};

/// All selectors resolved without realizing a store path.
#[derive(Debug)]
pub struct ResolvedInstall {
    targets: Vec<ResolvedPackagePlan>,
}

impl ResolvedInstall {
    /// Returns targets in caller-specified desired-state order.
    #[must_use]
    pub fn targets(&self) -> &[ResolvedPackagePlan] {
        &self.targets
    }
}

/// One target in a multi-selector resolve batch failed closed.
#[derive(Debug)]
pub struct ResolveBatchError {
    target_index: usize,
    source: Option<ResolveError>,
}

impl ResolveBatchError {
    /// Returns the zero-based desired-state position, never raw selector text.
    #[must_use]
    pub const fn target_index(&self) -> usize {
        self.target_index
    }
    /// Returns the redacted resolver error, or `None` for an empty batch.
    #[must_use]
    pub const fn source(&self) -> Option<ResolveError> {
        self.source
    }
}

impl fmt::Display for ResolveBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "install resolve target {} refused",
            self.target_index
        )
    }
}
impl std::error::Error for ResolveBatchError {}

/// Resolves every selector against one authenticated source without mutation.
pub fn resolve_install(
    selectors: &[PackageSelector],
    source: &VerifiedNixpkgsSource,
    system: System,
    index: Option<&IndexDocument>,
    adapter: &dyn NixAdapter,
) -> Result<ResolvedInstall, ResolveBatchError> {
    if selectors.is_empty() {
        return Err(ResolveBatchError {
            target_index: 0,
            source: None,
        });
    }
    let targets = selectors
        .iter()
        .enumerate()
        .map(|(target_index, selector)| {
            resolve_package(selector, source, system, index, adapter).map_err(|source| {
                ResolveBatchError {
                    target_index,
                    source: Some(source),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedInstall { targets })
}
