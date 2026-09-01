use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use pkg_core::identity::StorePath;
use pkg_nix::{GenerationId, MaintenanceAdapter, RootSetReport, RootSetTransitionReport};

use crate::activate::{ActivationPlan, verify_activation};
use crate::roots::{PreparedRootSet, RootError, publish_root_set};

/// Fixed receipt proving that pkg created or safely adopted an empty state root.
pub const STATE_OWNERSHIP_MARKER_NAME: &str = ".pkg-state-v1";
/// Exact marker bytes. The marker is private user-owned state, not secret authority.
pub const STATE_OWNERSHIP_MARKER_BYTES: &[u8] = b"pkg-state-v1\n";

/// A validated product-owned state layout rooted beneath a trusted boundary.
#[derive(Debug, Clone)]
pub struct StateLayout {
    trusted_root: PathBuf,
    state_root: PathBuf,
    owner_uid: u32,
}

/// State-layout or atomic-current operation failure.
#[derive(Debug)]
pub enum CurrentError {
    /// A path escaped the trusted root or used an unexpected file type.
    UnsafePath,
    /// Ownership or writable permission bits violated the state policy.
    UnsafePermissions,
    /// The requested retained forest does not exist as a real directory.
    MissingForest,
    /// Root publication did not become durable, so activation was not attempted.
    RootPublication,
    /// `current` may already name the new generation; recovery must finish forward.
    PostActivation,
    /// The observed recovery evidence cannot occur under the transaction contract.
    InvalidRecoveryState,
    /// A bounded filesystem operation failed.
    Filesystem(std::io::Error),
}

impl fmt::Display for CurrentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePath => f.write_str("unsafe state path refused"),
            Self::UnsafePermissions => f.write_str("unsafe state permissions refused"),
            Self::MissingForest => f.write_str("retained activation forest is missing"),
            Self::RootPublication => f.write_str("activation root publication failed"),
            Self::PostActivation => f.write_str("activation requires forward recovery"),
            Self::InvalidRecoveryState => f.write_str("invalid activation recovery state"),
            Self::Filesystem(_) => f.write_str("state filesystem operation failed"),
        }
    }
}

impl std::error::Error for CurrentError {}

impl From<std::io::Error> for CurrentError {
    fn from(error: std::io::Error) -> Self {
        Self::Filesystem(error)
    }
}

impl From<RootError> for CurrentError {
    fn from(_: RootError) -> Self {
        Self::RootPublication
    }
}

/// Durability landmarks emitted in their required order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationEvent {
    /// The helper has durably published every selected output root.
    Rooted,
    /// The staged forest has been atomically retained as `gen-<id>`.
    ForestRetained,
    /// The atomic `current` swap has completed.
    Activated,
}

/// Filesystem/journal evidence used to classify an interrupted activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryEvidence {
    /// The immutable generation record and its snapshots verify.
    pub prepared: bool,
    /// The complete per-output root set is present.
    pub rooted: bool,
    /// `current` actually names this generation and its forest verifies.
    pub current_is_generation: bool,
    /// The hash-chained journal contains this operation's committed row.
    pub committed: bool,
}

/// Required idempotent recovery action for one interrupted generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Delete the unreachable record, snapshots, and staging/retained forest.
    DiscardPrepared,
    /// Remove roots first via the helper, then delete unreachable user state.
    DiscardRooted,
    /// Restore mutable views from snapshots and append the committed row.
    FinishActivated,
    /// The durable transaction is already complete.
    None,
}

/// Classifies only the four reachable transaction states and fails closed otherwise.
pub const fn classify_recovery(evidence: RecoveryEvidence) -> Result<RecoveryAction, CurrentError> {
    match evidence {
        RecoveryEvidence {
            prepared: true,
            rooted: false,
            current_is_generation: false,
            committed: false,
        } => Ok(RecoveryAction::DiscardPrepared),
        RecoveryEvidence {
            prepared: true,
            rooted: true,
            current_is_generation: false,
            committed: false,
        } => Ok(RecoveryAction::DiscardRooted),
        RecoveryEvidence {
            prepared: true,
            rooted: true,
            current_is_generation: true,
            committed: false,
        } => Ok(RecoveryAction::FinishActivated),
        RecoveryEvidence {
            prepared: true,
            rooted: true,
            current_is_generation: true,
            committed: true,
        } => Ok(RecoveryAction::None),
        _ => Err(CurrentError::InvalidRecoveryState),
    }
}

impl StateLayout {
    /// Idempotently creates the fixed private per-user state tree beneath an
    /// existing trusted ownership boundary, then validates the complete path.
    ///
    /// Existing components are never chmodded or replaced. Missing components
    /// are created at `0700`; any symlink, foreign owner, writable group/world
    /// bit, non-directory, or path escape fails closed.
    pub fn initialize(
        trusted_root: &Path,
        state_root: &Path,
        owner_uid: u32,
    ) -> Result<Self, CurrentError> {
        if !trusted_root.is_absolute()
            || state_root == trusted_root
            || !state_root.starts_with(trusted_root)
        {
            return Err(CurrentError::UnsafePath);
        }
        validate_component(trusted_root, owner_uid)?;
        let relative = state_root
            .strip_prefix(trusted_root)
            .map_err(|_| CurrentError::UnsafePath)?;
        let mut cursor = trusted_root.to_path_buf();
        let mut state_root_created = false;
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(CurrentError::UnsafePath);
            };
            cursor.push(component);
            let created = ensure_private_directory(&cursor, owner_uid)?;
            if cursor == state_root {
                state_root_created = created;
            }
        }
        ensure_state_ownership_marker(state_root, owner_uid, state_root_created)?;
        for relative in [
            "generations",
            "journal",
            "activations",
            "run",
            "cache",
            "logs",
        ] {
            ensure_private_directory(&state_root.join(relative), owner_uid)?;
        }
        sync_dir(state_root)?;
        Self::open(trusted_root, state_root, owner_uid)
    }

    /// Validates every existing component from a trusted ownership boundary.
    pub fn open(
        trusted_root: &Path,
        state_root: &Path,
        owner_uid: u32,
    ) -> Result<Self, CurrentError> {
        if !trusted_root.is_absolute() || !state_root.starts_with(trusted_root) {
            return Err(CurrentError::UnsafePath);
        }
        validate_component(trusted_root, owner_uid)?;
        let relative = state_root
            .strip_prefix(trusted_root)
            .map_err(|_| CurrentError::UnsafePath)?;
        let mut cursor = trusted_root.to_path_buf();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(CurrentError::UnsafePath);
            };
            cursor.push(component);
            validate_component(&cursor, owner_uid)?;
        }
        Ok(Self {
            trusted_root: trusted_root.to_path_buf(),
            state_root: state_root.to_path_buf(),
            owner_uid,
        })
    }

    /// Atomically publishes `activations/gen-<id>` as the relative current target.
    pub fn switch_current(
        &self,
        generation: &GenerationId,
        nonce: &str,
    ) -> Result<(), CurrentError> {
        self.revalidate()?;
        validate_nonce(nonce)?;
        let relative_target = PathBuf::from("activations").join(generation.as_str());
        let forest = self.state_root.join(&relative_target);
        let metadata = fs::symlink_metadata(&forest).map_err(|_| CurrentError::MissingForest)?;
        if !metadata.file_type().is_dir() {
            return Err(CurrentError::MissingForest);
        }
        let temporary = self.state_root.join(format!("current.tmp.{nonce}"));
        if fs::symlink_metadata(&temporary).is_ok() {
            return Err(CurrentError::UnsafePath);
        }
        symlink(&relative_target, &temporary)?;
        sync_dir(&self.state_root)?;
        fs::rename(&temporary, self.state_root.join("current"))?;
        sync_dir(&self.state_root).map_err(|_| CurrentError::PostActivation)?;
        Ok(())
    }

    /// Reads and validates the exact relative current target, if present.
    pub fn current_generation(&self) -> Result<Option<GenerationId>, CurrentError> {
        self.revalidate()?;
        let current = self.state_root.join("current");
        match fs::symlink_metadata(&current) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(metadata) if !metadata.file_type().is_symlink() => Err(CurrentError::UnsafePath),
            Ok(_) => {
                let target = fs::read_link(current)?;
                let mut components = target.components();
                if components.next() != Some(Component::Normal("activations".as_ref())) {
                    return Err(CurrentError::UnsafePath);
                }
                let Some(Component::Normal(id)) = components.next() else {
                    return Err(CurrentError::UnsafePath);
                };
                if components.next().is_some() {
                    return Err(CurrentError::UnsafePath);
                }
                let id = id.to_str().ok_or(CurrentError::UnsafePath)?;
                GenerationId::new(id)
                    .map(Some)
                    .map_err(|_| CurrentError::UnsafePath)
            }
        }
    }

    /// Returns the validated per-user state root for durable state modules.
    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Returns the authenticated owner expected for every state component.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// Re-runs ownership, mode, and symlink-component validation.
    pub fn validate(&self) -> Result<(), CurrentError> {
        self.revalidate()
    }

    fn revalidate(&self) -> Result<(), CurrentError> {
        Self::open(&self.trusted_root, &self.state_root, self.owner_uid).map(|_| ())
    }
}

fn ensure_private_directory(path: &Path, owner_uid: u32) -> Result<bool, CurrentError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_component(path, owner_uid).map(|()| false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            let created = match builder.create(path) {
                Ok(()) => {
                    let parent = path.parent().ok_or(CurrentError::UnsafePath)?;
                    sync_dir(parent)?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error.into()),
            };
            validate_component(path, owner_uid)?;
            Ok(created)
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_state_ownership_marker(
    state_root: &Path,
    owner_uid: u32,
    state_root_created: bool,
) -> Result<(), CurrentError> {
    let marker = state_root.join(STATE_OWNERSHIP_MARKER_NAME);
    match fs::symlink_metadata(&marker) {
        Ok(_) => return validate_state_ownership_marker(&marker, owner_uid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if !state_root_created {
        // V1 is the first supported state format. There is no markerless
        // legacy format to migrate; inferring ownership from directory names
        // or contents would let an unrelated pre-existing tree be adopted.
        // Even an empty tree is refused because emptiness and marker creation
        // cannot be made atomic against its owning user at this layer.
        return Err(CurrentError::UnsafePath);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    match options.open(&marker) {
        Ok(mut file) => {
            file.write_all(STATE_OWNERSHIP_MARKER_BYTES)?;
            file.sync_all()?;
            sync_dir(state_root)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_state_ownership_marker(&marker, owner_uid)
}

fn validate_state_ownership_marker(path: &Path, owner_uid: u32) -> Result<(), CurrentError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != STATE_OWNERSHIP_MARKER_BYTES.len() as u64
        || fs::read(path)? != STATE_OWNERSHIP_MARKER_BYTES
    {
        return Err(CurrentError::UnsafePath);
    }
    Ok(())
}

/// Publishes roots, retains the staged forest, then atomically switches `current`.
///
/// The caller must have durably written the generation record and snapshots
/// before calling. A returned error never causes this function to move
/// `current`; recovery classifies any durable roots/forest left behind.
pub fn activate_rooted_generation(
    layout: &StateLayout,
    generation: &GenerationId,
    plan: &ActivationPlan,
    prepared_roots: &PreparedRootSet,
    helper: &dyn MaintenanceAdapter,
    nonce: &str,
    observe: impl FnMut(ActivationEvent) -> Result<(), CurrentError>,
) -> Result<(), CurrentError> {
    activate_generation(
        layout,
        generation,
        plan,
        Some(prepared_roots),
        helper,
        nonce,
        observe,
    )
}

/// Activates either a rooted nonempty generation or a root-free empty one.
///
/// `prepared_roots` must be `None` exactly when the activation plan has no
/// selected outputs. The empty set is trivially durable and never crosses the
/// privileged helper's deliberately nonempty root-publication grammar.
pub fn activate_generation(
    layout: &StateLayout,
    generation: &GenerationId,
    plan: &ActivationPlan,
    prepared_roots: Option<&PreparedRootSet>,
    helper: &dyn MaintenanceAdapter,
    nonce: &str,
    observe: impl FnMut(ActivationEvent) -> Result<(), CurrentError>,
) -> Result<(), CurrentError> {
    activate_generation_with(
        layout,
        generation,
        plan,
        prepared_roots,
        nonce,
        observe,
        || {
            if let Some(prepared_roots) = prepared_roots {
                publish_root_set(prepared_roots, helper)?;
            }
            Ok(())
        },
    )
}

/// Activates after the authenticated broker has already transitioned destination roots.
pub fn activate_transitioned_generation(
    layout: &StateLayout,
    generation: &GenerationId,
    plan: &ActivationPlan,
    prepared_roots: Option<&PreparedRootSet>,
    report: Option<&RootSetTransitionReport>,
    nonce: &str,
    observe: impl FnMut(ActivationEvent) -> Result<(), CurrentError>,
) -> Result<(), CurrentError> {
    match (prepared_roots, report) {
        (Some(prepared_roots), Some(report)) => {
            let expected = format!(
                "/nix/var/nix/gcroots/pkg/users/{}/{}",
                layout.owner_uid(),
                generation.as_str()
            );
            if report.root_set().reference().as_str() != expected
                || report.root_set().entry_count() != prepared_roots.request().entries().len()
                || !report.retained_names().iter().eq(prepared_roots
                    .request()
                    .entries()
                    .iter()
                    .map(pkg_nix::RootSetEntry::name))
                || report.mapping_digest() != prepared_roots.request().mapping_digest()
            {
                return Err(CurrentError::RootPublication);
            }
        }
        (None, None) if plan.output_roots().is_empty() => {}
        _ => return Err(CurrentError::RootPublication),
    }
    activate_generation_with(
        layout,
        generation,
        plan,
        prepared_roots,
        nonce,
        observe,
        || Ok(()),
    )
}

/// Activates after the authenticated broker has already published initial roots.
pub fn activate_published_generation(
    layout: &StateLayout,
    generation: &GenerationId,
    plan: &ActivationPlan,
    prepared_roots: Option<&PreparedRootSet>,
    report: Option<&RootSetReport>,
    nonce: &str,
    observe: impl FnMut(ActivationEvent) -> Result<(), CurrentError>,
) -> Result<(), CurrentError> {
    match (prepared_roots, report) {
        (Some(prepared_roots), Some(report)) => {
            let expected = format!(
                "/nix/var/nix/gcroots/pkg/users/{}/{}",
                layout.owner_uid(),
                generation.as_str()
            );
            if report.reference().as_str() != expected
                || report.entry_count() != prepared_roots.request().entries().len()
                || report.mapping_digest() != prepared_roots.request().mapping_digest()
            {
                return Err(CurrentError::RootPublication);
            }
        }
        (None, None) if plan.output_roots().is_empty() => {}
        _ => return Err(CurrentError::RootPublication),
    }
    activate_generation_with(
        layout,
        generation,
        plan,
        prepared_roots,
        nonce,
        observe,
        || Ok(()),
    )
}

fn activate_generation_with(
    layout: &StateLayout,
    generation: &GenerationId,
    plan: &ActivationPlan,
    prepared_roots: Option<&PreparedRootSet>,
    nonce: &str,
    mut observe: impl FnMut(ActivationEvent) -> Result<(), CurrentError>,
    publish: impl FnOnce() -> Result<(), RootError>,
) -> Result<(), CurrentError> {
    layout.revalidate()?;
    let activations = layout.state_root.join("activations");
    validate_component(&activations, layout.owner_uid)?;
    let staging = activations.join(format!("{}.staging", generation.as_str()));
    let retained = activations.join(generation.as_str());
    let staging_metadata =
        fs::symlink_metadata(&staging).map_err(|_| CurrentError::MissingForest)?;
    if !staging_metadata.file_type().is_dir() || fs::symlink_metadata(&retained).is_ok() {
        return Err(CurrentError::UnsafePath);
    }
    match prepared_roots {
        Some(prepared_roots)
            if prepared_roots.request().owner_uid() == layout.owner_uid
                && prepared_roots.request().generation() == generation
                && prepared_roots
                    .output_roots()
                    .into_iter()
                    .map(StorePath::as_str)
                    .eq(plan.output_roots().iter().map(StorePath::as_str)) => {}
        None if plan.output_roots().is_empty() => {}
        _ => return Err(CurrentError::RootPublication),
    }
    verify_activation(&staging, plan).map_err(|_| CurrentError::UnsafePath)?;

    publish()?;
    observe(ActivationEvent::Rooted)?;
    fs::rename(&staging, &retained)?;
    sync_dir(&activations)?;
    observe(ActivationEvent::ForestRetained)?;
    layout.switch_current(generation, nonce)?;
    observe(ActivationEvent::Activated)?;
    Ok(())
}

fn validate_component(path: &Path, owner_uid: u32) -> Result<(), CurrentError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(CurrentError::UnsafePath);
    }
    if metadata.uid() != owner_uid || metadata.permissions().mode() & 0o022 != 0 {
        return Err(CurrentError::UnsafePermissions);
    }
    Ok(())
}

fn validate_nonce(nonce: &str) -> Result<(), CurrentError> {
    if nonce.is_empty()
        || nonce.len() > 64
        || !nonce.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(CurrentError::UnsafePath);
    }
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), CurrentError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
