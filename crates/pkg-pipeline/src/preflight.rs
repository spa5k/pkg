use std::fmt;

use pkg_core::{OutputName, SelectorId, StorePath};

use crate::ResolvedInstall;

/// One exact expected output selected during mutation-free preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOutput {
    selector_id: SelectorId,
    output: OutputName,
    store_path: StorePath,
}

impl PlannedOutput {
    /// Returns the stable desired-state selector id.
    #[must_use]
    pub const fn selector_id(&self) -> &SelectorId {
        &self.selector_id
    }
    /// Returns the selected Nix output name.
    #[must_use]
    pub const fn output(&self) -> &OutputName {
        &self.output
    }
    /// Returns the expected, not-yet-trusted store identity.
    #[must_use]
    pub const fn store_path(&self) -> &StorePath {
        &self.store_path
    }
}

/// Cache-only PR-19 preflight result; local-build preview is extended by PR-26.
#[derive(Debug)]
pub struct PreflightInstall {
    outputs: Vec<PlannedOutput>,
}

impl PreflightInstall {
    /// Returns exact selected outputs in selector/output order.
    #[must_use]
    pub fn outputs(&self) -> &[PlannedOutput] {
        &self.outputs
    }
}

/// Evaluated plans were internally inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightError;
impl fmt::Display for PreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("install preflight refused inconsistent outputs")
    }
}
impl std::error::Error for PreflightError {}

/// Extracts exact selected outputs without touching the store or cache.
pub fn preflight_cache_only(
    resolved: &ResolvedInstall,
) -> Result<PreflightInstall, PreflightError> {
    let mut outputs = Vec::new();
    for target in resolved.targets() {
        let root = target
            .plan()
            .derivations()
            .iter()
            .find(|derivation| derivation.derivation() == target.plan().root())
            .ok_or(PreflightError)?;
        for output in target.plan().outputs_to_install() {
            let store_path = root.outputs().get(output).ok_or(PreflightError)?.clone();
            outputs.push(PlannedOutput {
                selector_id: target.selector().id().clone(),
                output: output.clone(),
                store_path,
            });
        }
    }
    outputs.sort_by(|left, right| {
        left.selector_id
            .as_str()
            .cmp(right.selector_id.as_str())
            .then_with(|| left.output.as_str().cmp(right.output.as_str()))
    });
    if outputs.is_empty() {
        return Err(PreflightError);
    }
    Ok(PreflightInstall { outputs })
}
