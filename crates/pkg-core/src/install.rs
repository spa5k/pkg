//! Atomic addition of resolved packages to coherent desired and locked state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::lifecycle::{LifecycleError, LifecycleState};
use crate::state::{LockEntry, LockedState, Manifest, ManifestEntry};
use crate::{ChannelSequence, PackageSelector, SelectorId, System};

/// One resolved and verified package ready to enter lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPackage {
    selector: PackageSelector,
    lock_entry: LockEntry,
    added_at: String,
    origin: String,
}

impl InstallPackage {
    /// Binds resolved intent to one exact verified realization and audit metadata.
    pub fn new(
        selector: PackageSelector,
        lock_entry: LockEntry,
        added_at: impl Into<String>,
        origin: impl Into<String>,
    ) -> Result<Self, InstallEditError> {
        let attribute = selector
            .attribute()
            .ok_or(InstallEditError::UnresolvedSelector)?;
        if attribute != lock_entry.attribute()
            || selector.id().as_str().is_empty()
            || lock_entry.realization().outputs_to_install().is_empty()
        {
            return Err(InstallEditError::BindingMismatch);
        }
        let added_at = added_at.into();
        let origin = origin.into();
        ManifestEntry::from_install(&selector, added_at.clone(), origin.clone())
            .map_err(|_| InstallEditError::InvalidMetadata)?;
        Ok(Self {
            selector,
            lock_entry,
            added_at,
            origin,
        })
    }

    /// Stable selector id being added.
    #[must_use]
    pub const fn id(&self) -> &SelectorId {
        self.selector.id()
    }
    /// Resolved desired selector.
    #[must_use]
    pub const fn selector(&self) -> &PackageSelector {
        &self.selector
    }
    /// Exact verified lock entry.
    #[must_use]
    pub const fn lock_entry(&self) -> &LockEntry {
        &self.lock_entry
    }
}

/// Successful atomic install edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    state: LifecycleState,
    added: Vec<SelectorId>,
}

impl InstallResult {
    /// Coherent next desired and locked state.
    #[must_use]
    pub const fn state(&self) -> &LifecycleState {
        &self.state
    }
    /// Added selector ids in caller order.
    #[must_use]
    pub fn added(&self) -> &[SelectorId] {
        &self.added
    }
    /// Consumes the edit and returns the next state.
    #[must_use]
    pub fn into_state(self) -> LifecycleState {
        self.state
    }
}

/// Stable install-state edit refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallEditError {
    /// At least one package is required.
    EmptyRequest,
    /// A selector was not resolved to an attribute.
    UnresolvedSelector,
    /// Desired and exact lock identities disagreed.
    BindingMismatch,
    /// Timestamp or origin metadata violated the durable schema.
    InvalidMetadata,
    /// An id or user selector appeared more than once.
    DuplicateTarget,
    /// A requested package is already installed.
    AlreadyInstalled,
    /// Package platform differed from the target lifecycle platform.
    SystemMismatch,
    /// The resulting state violated a lifecycle invariant.
    InvalidState,
}

impl fmt::Display for InstallEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "install state edit refused: {self:?}")
    }
}
impl std::error::Error for InstallEditError {}

/// Adds every package atomically, preserving existing exact locks unchanged.
pub fn install_packages(
    current: Option<LifecycleState>,
    channel_seq: ChannelSequence,
    system: System,
    uid: u32,
    packages: Vec<InstallPackage>,
) -> Result<InstallResult, InstallEditError> {
    if packages.is_empty() {
        return Err(InstallEditError::EmptyRequest);
    }
    let ids = packages
        .iter()
        .map(|package| package.id().clone())
        .collect::<BTreeSet<_>>();
    let selectors = packages
        .iter()
        .map(|package| package.selector.selector().clone())
        .collect::<BTreeSet<_>>();
    if ids.len() != packages.len() || selectors.len() != packages.len() {
        return Err(InstallEditError::DuplicateTarget);
    }
    if packages
        .iter()
        .any(|package| package.lock_entry.realization().system() != system)
    {
        return Err(InstallEditError::SystemMismatch);
    }

    let (mut manifest_entries, mut lock_entries) = match current {
        Some(state) => {
            if state.manifest().uid() != uid
                || state.locked().uid() != uid
                || state.locked().system() != system
            {
                return Err(InstallEditError::InvalidState);
            }
            let existing_ids = state
                .manifest()
                .entries()
                .iter()
                .map(ManifestEntry::id)
                .collect::<BTreeSet<_>>();
            let existing_selectors = state
                .manifest()
                .entries()
                .iter()
                .map(ManifestEntry::selector)
                .collect::<BTreeSet<_>>();
            if packages.iter().any(|package| {
                existing_ids.contains(package.id())
                    || existing_selectors.contains(package.selector.selector())
            }) {
                return Err(InstallEditError::AlreadyInstalled);
            }
            (
                state.manifest().entries().to_vec(),
                state.locked().entries().clone(),
            )
        }
        None => (Vec::new(), BTreeMap::new()),
    };

    let added = packages
        .iter()
        .map(|package| package.id().clone())
        .collect::<Vec<_>>();
    for package in packages {
        let entry =
            ManifestEntry::from_install(&package.selector, package.added_at, package.origin)
                .map_err(|_| InstallEditError::InvalidMetadata)?;
        lock_entries.insert(package.selector.id().clone(), package.lock_entry);
        manifest_entries.push(entry);
    }
    let manifest = Manifest::from_lifecycle_parts(channel_seq, uid, manifest_entries);
    let locked = LockedState::from_lifecycle_parts(channel_seq, system, uid, lock_entries);
    let state = LifecycleState::new(manifest, locked).map_err(map_lifecycle_error)?;
    Ok(InstallResult { state, added })
}

const fn map_lifecycle_error(_: LifecycleError) -> InstallEditError {
    InstallEditError::InvalidState
}

#[cfg(test)]
mod tests;
