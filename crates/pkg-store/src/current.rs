use std::fmt;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use pkg_core::identity::StorePath;
use pkg_nix::{GenerationId, MaintenanceAdapter};

use crate::activate::{ActivationPlan, verify_activation};
use crate::roots::{PreparedRootSet, RootError, publish_root_set};

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
        sync_dir(&self.state_root)?;
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

    fn revalidate(&self) -> Result<(), CurrentError> {
        Self::open(&self.trusted_root, &self.state_root, self.owner_uid).map(|_| ())
    }
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
    mut observe: impl FnMut(ActivationEvent),
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
    if prepared_roots.request().owner_uid() != layout.owner_uid
        || prepared_roots.request().generation() != generation
        || prepared_roots
            .output_roots()
            .into_iter()
            .map(StorePath::as_str)
            .ne(plan.output_roots().iter().map(StorePath::as_str))
    {
        return Err(CurrentError::RootPublication);
    }
    verify_activation(&staging, plan).map_err(|_| CurrentError::UnsafePath)?;

    publish_root_set(prepared_roots, helper)?;
    observe(ActivationEvent::Rooted);
    fs::rename(&staging, &retained)?;
    sync_dir(&activations)?;
    observe(ActivationEvent::ForestRetained);
    layout.switch_current(generation, nonce)?;
    observe(ActivationEvent::Activated);
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
            |event| events.push(event),
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
                |_| {}
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
                |_| {}
            ),
            Err(CurrentError::RootPublication)
        ));
        assert!(activations.join("gen-0001.staging").is_dir());
        assert_eq!(layout.current_generation().unwrap(), None);
    }
}
