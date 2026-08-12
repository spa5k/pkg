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
                    .map(|entry| entry.name()))
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
mod tests {
    use super::*;
    use crate::roots::{RootCandidate, prepare_root_set};
    use pkg_core::identity::{OutputName, StorePath};
    use pkg_core::selector::SelectorId;
    use pkg_core::state::CollisionPolicy;
    use pkg_nix::{InProcessHelper, InProcessPeer};
    use tempfile::Builder;

    #[test]
    fn swaps_current_as_an_exact_relative_symlink() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(state.join("activations")).unwrap();
        fs::set_permissions(state.join("activations"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(state.join("activations/gen-0001")).unwrap();
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        let generation = GenerationId::new("gen-0001").unwrap();
        layout.switch_current(&generation, "abc123").unwrap();
        assert_eq!(
            fs::read_link(state.join("current")).unwrap(),
            Path::new("activations/gen-0001")
        );
        assert_eq!(layout.current_generation().unwrap(), Some(generation));
    }

    #[test]
    fn empty_generation_activates_without_an_empty_root_request() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let activations = state.join("activations");
        fs::create_dir(&activations).unwrap();
        fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
        let generation = GenerationId::new("gen-0001").unwrap();
        let plan = crate::activate::stage_from_sources(
            &activations.join("gen-0001.staging"),
            &[],
            CollisionPolicy::Abort,
        )
        .unwrap();
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(uid);
        let mut events = Vec::new();
        activate_generation(
            &layout,
            &generation,
            &plan,
            None,
            &maintenance,
            "empty1",
            |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            events,
            [
                ActivationEvent::Rooted,
                ActivationEvent::ForestRetained,
                ActivationEvent::Activated
            ]
        );
        assert_eq!(layout.current_generation().unwrap(), Some(generation));
    }

    #[test]
    fn rejects_symlinked_or_world_writable_state_components() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            StateLayout::open(temp.path(), &real, uid),
            Err(CurrentError::UnsafePermissions)
        ));
        fs::remove_dir(&real).unwrap();
        fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(matches!(
            StateLayout::open(temp.path(), &link, uid),
            Err(CurrentError::UnsafePath)
        ));
    }

    #[test]
    fn initializes_only_the_fixed_private_state_tree() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("nested/state");
        let layout = StateLayout::initialize(temp.path(), &state, uid).unwrap();
        assert_eq!(layout.state_root(), state);
        for relative in [
            "",
            "generations",
            "journal",
            "activations",
            "run",
            "cache",
            "logs",
        ] {
            let metadata = fs::symlink_metadata(state.join(relative)).unwrap();
            assert!(metadata.file_type().is_dir());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        let marker = state.join(STATE_OWNERSHIP_MARKER_NAME);
        let metadata = fs::symlink_metadata(&marker).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(marker).unwrap(), STATE_OWNERSHIP_MARKER_BYTES);
        assert!(StateLayout::initialize(temp.path(), &state, uid).is_ok());

        let escaped = temp.path().parent().unwrap().join("pkg-escaped-state");
        assert!(matches!(
            StateLayout::initialize(temp.path(), &escaped, uid),
            Err(CurrentError::UnsafePath)
        ));
    }

    #[test]
    fn initialization_never_adopts_nonempty_unmarked_state() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(state.join("foreign"), b"keep").unwrap();

        assert!(matches!(
            StateLayout::initialize(temp.path(), &state, uid),
            Err(CurrentError::UnsafePath)
        ));
        assert_eq!(fs::read(state.join("foreign")).unwrap(), b"keep");
        assert!(!state.join(STATE_OWNERSHIP_MARKER_NAME).exists());

        fs::remove_file(state.join("foreign")).unwrap();
        assert!(matches!(
            StateLayout::initialize(temp.path(), &state, uid),
            Err(CurrentError::UnsafePath)
        ));
        assert!(!state.join(STATE_OWNERSHIP_MARKER_NAME).exists());
    }

    #[test]
    fn roots_and_retains_before_switching_current() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let activations = state.join("activations");
        fs::create_dir(&activations).unwrap();
        fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
        let generation = GenerationId::new("gen-0001").unwrap();
        let store_path = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
        let candidate = RootCandidate::new(
            SelectorId::new("sel_a").unwrap(),
            OutputName::new("out").unwrap(),
            store_path.clone(),
        );
        let roots = prepare_root_set(uid, generation.clone(), [candidate]).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("tool"), b"tool").unwrap();
        let plan = crate::activate::stage_from_sources(
            &activations.join("gen-0001.staging"),
            &[(store_path, source)],
            CollisionPolicy::Abort,
        )
        .unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let session = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        let maintenance = session.for_caller(uid);
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        let mut events = Vec::new();
        activate_rooted_generation(
            &layout,
            &generation,
            &plan,
            &roots,
            &maintenance,
            "n1",
            |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            events,
            [
                ActivationEvent::Rooted,
                ActivationEvent::ForestRetained,
                ActivationEvent::Activated
            ]
        );
        assert!(!activations.join("gen-0001.staging").exists());
        assert!(activations.join("gen-0001").is_dir());
        assert_eq!(layout.current_generation().unwrap(), Some(generation));
    }

    #[test]
    fn classifies_all_four_recovery_states_and_rejects_impossible_evidence() {
        let evidence = |rooted, current_is_generation, committed| RecoveryEvidence {
            prepared: true,
            rooted,
            current_is_generation,
            committed,
        };
        assert_eq!(
            classify_recovery(evidence(false, false, false)).unwrap(),
            RecoveryAction::DiscardPrepared
        );
        assert_eq!(
            classify_recovery(evidence(true, false, false)).unwrap(),
            RecoveryAction::DiscardRooted
        );
        assert_eq!(
            classify_recovery(evidence(true, true, false)).unwrap(),
            RecoveryAction::FinishActivated
        );
        assert_eq!(
            classify_recovery(evidence(true, true, true)).unwrap(),
            RecoveryAction::None
        );
        assert!(classify_recovery(evidence(false, true, false)).is_err());
        assert!(
            classify_recovery(RecoveryEvidence {
                prepared: false,
                rooted: false,
                current_is_generation: false,
                committed: false
            })
            .is_err()
        );
    }

    #[test]
    fn helper_refusal_never_retains_or_switches_the_forest() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        let activations = state.join("activations");
        fs::create_dir_all(&activations).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
        let generation = GenerationId::new("gen-0001").unwrap();
        let store_path = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
        let roots = prepare_root_set(
            uid,
            generation.clone(),
            [RootCandidate::new(
                SelectorId::new("sel_a").unwrap(),
                OutputName::new("out").unwrap(),
                store_path.clone(),
            )],
        )
        .unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("tool"), b"tool").unwrap();
        let plan = crate::activate::stage_from_sources(
            &activations.join("gen-0001.staging"),
            &[(store_path, source)],
            CollisionPolicy::Abort,
        )
        .unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let session = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap();
        let maintenance = session.for_caller(uid);
        helper.restart().unwrap();
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        assert!(matches!(
            activate_rooted_generation(
                &layout,
                &generation,
                &plan,
                &roots,
                &maintenance,
                "n1",
                |_| Ok(())
            ),
            Err(CurrentError::RootPublication)
        ));
        assert!(activations.join("gen-0001.staging").is_dir());
        assert!(!activations.join("gen-0001").exists());
        assert_eq!(layout.current_generation().unwrap(), None);
    }

    #[test]
    fn mismatched_root_set_is_refused_before_publication() {
        let temp = Builder::new().prefix("pkg-store-").tempdir_in(".").unwrap();
        let uid = fs::symlink_metadata(temp.path()).unwrap().uid();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = temp.path().join("state");
        let activations = state.join("activations");
        fs::create_dir_all(&activations).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&activations, fs::Permissions::from_mode(0o700)).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("tool"), b"tool").unwrap();
        let planned = StorePath::new("/nix/store/00000000000000000000000000000000-a").unwrap();
        let plan = crate::activate::stage_from_sources(
            &activations.join("gen-0001.staging"),
            &[(planned, source)],
            CollisionPolicy::Abort,
        )
        .unwrap();
        let generation = GenerationId::new("gen-0001").unwrap();
        let roots = prepare_root_set(
            uid,
            generation.clone(),
            [RootCandidate::new(
                SelectorId::new("sel_b").unwrap(),
                OutputName::new("out").unwrap(),
                StorePath::new("/nix/store/00000000000000000000000000000000-b").unwrap(),
            )],
        )
        .unwrap();
        let helper = InProcessHelper::new(991).unwrap();
        let maintenance = helper
            .connect(InProcessPeer::authenticated_uid(991))
            .unwrap()
            .for_caller(uid);
        let layout = StateLayout::open(temp.path(), &state, uid).unwrap();
        assert!(matches!(
            activate_rooted_generation(
                &layout,
                &generation,
                &plan,
                &roots,
                &maintenance,
                "n1",
                |_| Ok(())
            ),
            Err(CurrentError::RootPublication)
        ));
        assert!(activations.join("gen-0001.staging").is_dir());
        assert_eq!(layout.current_generation().unwrap(), None);
    }
}
