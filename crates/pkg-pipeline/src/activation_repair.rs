//! Verification and reconstruction of a committed generation's activation links.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::str::FromStr;

use pkg_core::GenerationSnapshot;
use pkg_core::state::Digest;
use pkg_nix::GenerationId;
use pkg_store::{LeaseMode, StateLayout, StateLease, stage_activation, verify_recorded_activation};

use crate::activation_metadata::activation_inputs;
use crate::{CommitError, load_retained_history};

/// Verifies every activation link against its committed immutable generation.
/// This checks the local link tree, not the contents of the Nix store closure.
///
/// # Errors
/// Returns an error for unsafe state, an invalid lease, an unknown or unfinished
/// generation, or any missing, changed, or additional activation entry.
pub fn verify_generation_activation(
    layout: &StateLayout,
    lease: &StateLease,
    generation: &GenerationId,
) -> Result<(), CommitError> {
    let snapshot = retained_snapshot(layout, lease, generation)?;
    validate_directory(&layout.state_root().join("activations"), layout.owner_uid())?;
    verify_tree(
        &layout
            .state_root()
            .join(snapshot.generation().activation().tree_path()),
        &snapshot,
    )
}

/// Rebuilds damaged activation links from verified committed state.
///
/// The caller must first verify or repair the store closure through the broker.
/// A complete replacement is checked against the recorded digest before publish.
/// The non-atomic directory replacement can be retried after interruption.
///
/// # Errors
/// Returns an error for an invalid lease or state, failed reconstruction, a tree
/// that differs from the immutable record, or a filesystem failure.
pub fn repair_generation_activation(
    layout: &StateLayout,
    lease: &StateLease,
    generation: &GenerationId,
) -> Result<bool, CommitError> {
    if !lease.authorizes(layout, LeaseMode::Exclusive) {
        return Err(CommitError::LeaseRequired);
    }
    let snapshot = retained_snapshot(layout, lease, generation)?;
    let parent = layout.state_root().join("activations");
    validate_directory(&parent, layout.owner_uid())?;
    let tree = parent.join(generation.as_str());
    let staging = parent.join(format!("{}.staging", generation.as_str()));
    let backup = parent.join(format!("{}.repair", generation.as_str()));
    for path in [&tree, &staging, &backup] {
        if exists(path)?
            && fs::symlink_metadata(path)
                .map_err(|_| CommitError::StateIo)?
                .uid()
                != layout.owner_uid()
        {
            return Err(CommitError::StateIo);
        }
    }
    let restored = restore_interrupted_replacement(&tree, &backup)?;
    if verify_tree(&tree, &snapshot).is_ok() {
        remove_if_present(&backup)?;
        remove_if_present(&staging)?;
        sync_directory(&parent)?;
        return Ok(restored);
    }
    remove_if_present(&staging)?;
    stage_activation(
        &staging,
        &activation_inputs(snapshot.state()),
        snapshot.generation().activation().collision_policy(),
    )
    .map_err(|_| CommitError::StageMismatch)?;
    verify_tree(&staging, &snapshot)?;
    replace_forest(&tree, &staging, &backup)?;
    Ok(true)
}

fn retained_snapshot(
    layout: &StateLayout,
    lease: &StateLease,
    generation: &GenerationId,
) -> Result<GenerationSnapshot, CommitError> {
    load_retained_history(layout, lease)?
        .snapshots()
        .iter()
        .find(|snapshot| snapshot.generation().id() == generation.as_str())
        .cloned()
        .ok_or(CommitError::InvalidCandidate)
}

fn verify_tree(tree: &Path, snapshot: &GenerationSnapshot) -> Result<(), CommitError> {
    let activation = snapshot.generation().activation();
    let digest =
        Digest::from_str(activation.tree_digest()).map_err(|_| CommitError::InvalidCandidate)?;
    verify_recorded_activation(
        tree,
        digest,
        activation.entry_count(),
        activation.output_roots(),
    )
    .map_err(|_| CommitError::StageMismatch)
}

fn validate_directory(path: &Path, uid: u32) -> Result<(), CommitError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CommitError::StateIo)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o022 != 0 {
        return Err(CommitError::StateIo);
    }
    Ok(())
}

fn restore_interrupted_replacement(tree: &Path, backup: &Path) -> Result<bool, CommitError> {
    if !exists(tree)? && exists(backup)? {
        fs::rename(backup, tree).map_err(|_| CommitError::StateIo)?;
        sync_directory(tree.parent().ok_or(CommitError::StateIo)?)?;
        return Ok(true);
    }
    Ok(false)
}

fn replace_forest(tree: &Path, staging: &Path, backup: &Path) -> Result<(), CommitError> {
    let parent = tree.parent().ok_or(CommitError::StateIo)?;
    remove_if_present(backup)?;
    if exists(tree)? {
        fs::rename(tree, backup).map_err(|_| CommitError::StateIo)?;
        sync_directory(parent)?;
    }
    if fs::rename(staging, tree).is_err() {
        restore_interrupted_replacement(tree, backup)?;
        return Err(CommitError::StateIo);
    }
    sync_directory(parent)?;
    remove_if_present(backup)?;
    sync_directory(parent)
}

fn exists(path: &Path) -> Result<bool, CommitError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(CommitError::StateIo),
    }
}

fn remove_if_present(path: &Path) -> Result<(), CommitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).map_err(|_| CommitError::StateIo)
        }
        Ok(_) => fs::remove_file(path).map_err(|_| CommitError::StateIo),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CommitError::StateIo),
    }
}

fn sync_directory(path: &Path) -> Result<(), CommitError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| CommitError::StateIo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn replacement_and_interrupted_publish_keep_recovery_paths_bounded() {
        let temp = TempDir::new().unwrap();
        let tree = temp.path().join("gen-0001");
        let staging = temp.path().join("gen-0001.staging");
        let backup = temp.path().join("gen-0001.repair");
        fs::create_dir(&tree).unwrap();
        symlink(
            "/nix/store/00000000000000000000000000000000-old/bin/demo",
            tree.join("demo"),
        )
        .unwrap();
        fs::create_dir(&staging).unwrap();
        symlink(
            "/nix/store/00000000000000000000000000000000-fixed/bin/demo",
            staging.join("demo"),
        )
        .unwrap();
        replace_forest(&tree, &staging, &backup).unwrap();
        assert!(
            fs::read_link(tree.join("demo"))
                .unwrap()
                .to_str()
                .unwrap()
                .contains("-fixed/")
        );
        assert!(!exists(&backup).unwrap());
        // A crash after retaining the old tree is recovered before rebuilding.
        fs::rename(&tree, &backup).unwrap();
        assert!(restore_interrupted_replacement(&tree, &backup).unwrap());
        assert!(!restore_interrupted_replacement(&tree, &backup).unwrap());
        // Failed final rename restores the previous tree instead of losing it.
        assert_eq!(
            replace_forest(&tree, &staging, &backup),
            Err(CommitError::StateIo)
        );
        assert!(
            fs::read_link(tree.join("demo"))
                .unwrap()
                .to_str()
                .unwrap()
                .contains("-fixed/")
        );
    }

    #[test]
    fn cleanup_never_follows_an_unexpected_symlink() {
        let temp = TempDir::new().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "preserve").unwrap();
        let staging = temp.path().join("gen-0001.staging");
        symlink(&outside, &staging).unwrap();
        remove_if_present(&staging).unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("keep")).unwrap(),
            "preserve"
        );
    }
}
