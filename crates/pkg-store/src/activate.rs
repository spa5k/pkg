use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};

use pkg_core::identity::StorePath;
use pkg_core::state::{CollisionPolicy, Digest};
use sha2::{Digest as _, Sha256};

/// One verified store output to expose in the activation forest.
#[derive(Debug, Clone)]
pub struct ActivationInput {
    store_path: StorePath,
}

impl ActivationInput {
    /// Uses the validated store path as both identity and read-only source.
    #[must_use]
    pub const fn new(store_path: StorePath) -> Self {
        Self { store_path }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Provider {
    output: StorePath,
    target: PathBuf,
}

/// One deterministic leaf in an activation forest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForestEntry {
    relative_path: PathBuf,
    target: PathBuf,
}

impl ForestEntry {
    /// Returns the safe relative path exposed to the user.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the absolute store target.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }
}

/// One deterministic collision decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    relative_path: PathBuf,
    winner: StorePath,
    losers: Vec<StorePath>,
}

impl Collision {
    /// Returns the colliding activation path.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the deterministically selected output provider.
    #[must_use]
    pub const fn winner(&self) -> &StorePath {
        &self.winner
    }

    /// Returns the other output providers in deterministic order.
    #[must_use]
    pub fn losers(&self) -> &[StorePath] {
        &self.losers
    }
}

/// Verified result of staging a forest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPlan {
    tree_digest: Digest,
    output_roots: Vec<StorePath>,
    entries: Vec<ForestEntry>,
    collisions: Vec<Collision>,
}

impl ActivationPlan {
    /// Digest over sorted `relative-path NUL absolute-target NUL` entries.
    #[must_use]
    pub const fn tree_digest(&self) -> Digest {
        self.tree_digest
    }

    /// Returns every selected output root in canonical store-path order.
    #[must_use]
    pub fn output_roots(&self) -> &[StorePath] {
        &self.output_roots
    }

    /// Number of leaf links in the forest.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Deterministically sorted forest entries.
    #[must_use]
    pub fn entries(&self) -> &[ForestEntry] {
        &self.entries
    }

    /// Recorded collision decisions.
    #[must_use]
    pub fn collisions(&self) -> &[Collision] {
        &self.collisions
    }
}

/// Activation staging or verification failure.
#[derive(Debug)]
pub enum ActivationError {
    /// A source or destination path violated the closed filesystem grammar.
    UnsafePath,
    /// Two providers had a file-vs-directory conflict.
    StructuralConflict,
    /// The selected collision policy refused an overlapping leaf.
    Collision,
    /// Existing state or a staged forest did not match its deterministic plan.
    IntegrityMismatch,
    /// A bounded filesystem operation failed.
    Filesystem(std::io::Error),
}

impl fmt::Display for ActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePath => f.write_str("unsafe activation path refused"),
            Self::StructuralConflict => f.write_str("activation file/directory conflict"),
            Self::Collision => f.write_str("activation path collision"),
            Self::IntegrityMismatch => f.write_str("activation forest integrity mismatch"),
            Self::Filesystem(_) => f.write_str("activation filesystem operation failed"),
        }
    }
}

impl std::error::Error for ActivationError {}

impl From<std::io::Error> for ActivationError {
    fn from(error: std::io::Error) -> Self {
        Self::Filesystem(error)
    }
}

/// Materializes and fsyncs a deterministic symlink forest without invoking Nix.
pub fn stage_activation(
    staging: &Path,
    inputs: &[ActivationInput],
    collision_policy: CollisionPolicy,
) -> Result<ActivationPlan, ActivationError> {
    let sources = inputs
        .iter()
        .map(|input| {
            (
                input.store_path.clone(),
                PathBuf::from(input.store_path.as_str()),
            )
        })
        .collect::<Vec<_>>();
    stage_from_sources(staging, &sources, collision_policy)
}

pub(crate) fn stage_from_sources(
    staging: &Path,
    sources: &[(StorePath, PathBuf)],
    collision_policy: CollisionPolicy,
) -> Result<ActivationPlan, ActivationError> {
    if fs::symlink_metadata(staging).is_ok() {
        return Err(ActivationError::UnsafePath);
    }
    let mut providers: BTreeMap<PathBuf, Vec<Provider>> = BTreeMap::new();
    let mut directories = BTreeMap::<PathBuf, StorePath>::new();
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let mut output_roots = ordered
        .iter()
        .map(|(output, _)| output.clone())
        .collect::<Vec<_>>();
    output_roots.dedup();
    for (output, source) in ordered {
        let metadata = fs::symlink_metadata(&source)?;
        if !metadata.file_type().is_dir() {
            return Err(ActivationError::UnsafePath);
        }
        walk_output(
            &source,
            Path::new(""),
            &output,
            &mut providers,
            &mut directories,
        )?;
    }
    for path in providers.keys() {
        if directories.contains_key(path)
            || path
                .ancestors()
                .skip(1)
                .any(|parent| providers.contains_key(parent))
        {
            return Err(ActivationError::StructuralConflict);
        }
    }

    let mut entries = Vec::new();
    let mut collisions = Vec::new();
    for (relative_path, choices) in providers {
        let winner_index = match (collision_policy, choices.len()) {
            (_, 1) => 0,
            (CollisionPolicy::Abort, _) => return Err(ActivationError::Collision),
            (CollisionPolicy::KeepFirst, _) => 0,
            (CollisionPolicy::KeepLast, count) => count - 1,
        };
        let winner = choices[winner_index].clone();
        if choices.len() > 1 {
            collisions.push(Collision {
                relative_path: relative_path.clone(),
                winner: winner.output.clone(),
                losers: choices
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != winner_index)
                    .map(|(_, provider)| provider.output.clone())
                    .collect(),
            });
        }
        entries.push(ForestEntry {
            relative_path,
            target: winner.target,
        });
    }

    fs::create_dir(staging)?;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o700))?;
    for entry in &entries {
        let destination = staging.join(&entry.relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        symlink(&entry.target, &destination)?;
    }
    sync_tree(staging)?;
    let tree_digest = digest_entries(&entries);
    let plan = ActivationPlan {
        tree_digest,
        output_roots,
        entries,
        collisions,
    };
    verify_activation(staging, &plan)?;
    Ok(plan)
}

fn walk_output(
    source_root: &Path,
    relative: &Path,
    output: &StorePath,
    providers: &mut BTreeMap<PathBuf, Vec<Provider>>,
    directories: &mut BTreeMap<PathBuf, StorePath>,
) -> Result<(), ActivationError> {
    let current = source_root.join(relative);
    let mut children = fs::read_dir(&current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let name = child.file_name();
        let child_relative = relative.join(name);
        validate_relative(&child_relative)?;
        let kind = child.file_type()?;
        if kind.is_dir() {
            directories
                .entry(child_relative.clone())
                .or_insert_with(|| output.clone());
            walk_output(source_root, &child_relative, output, providers, directories)?;
        } else if kind.is_file() || kind.is_symlink() {
            providers
                .entry(child_relative.clone())
                .or_default()
                .push(Provider {
                    output: output.clone(),
                    target: PathBuf::from(output.as_str()).join(child_relative),
                });
        } else {
            return Err(ActivationError::UnsafePath);
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), ActivationError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ActivationError::UnsafePath);
    }
    Ok(())
}

/// Recomputes every leaf and the tree digest without following forest symlinks.
pub fn verify_activation(tree: &Path, plan: &ActivationPlan) -> Result<(), ActivationError> {
    for entry in &plan.entries {
        let path = tree.join(&entry.relative_path);
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| ActivationError::IntegrityMismatch)?;
        if !metadata.file_type().is_symlink()
            || fs::read_link(&path).map_err(|_| ActivationError::IntegrityMismatch)? != entry.target
        {
            return Err(ActivationError::IntegrityMismatch);
        }
    }
    let observed = scan_forest(tree)?;
    if observed != plan.entries || digest_entries(&observed) != plan.tree_digest {
        return Err(ActivationError::IntegrityMismatch);
    }
    Ok(())
}

/// Verifies a retained forest from only its persisted generation metadata.
pub fn verify_recorded_activation(
    tree: &Path,
    expected_digest: Digest,
    expected_entries: u64,
    output_roots: &[StorePath],
) -> Result<(), ActivationError> {
    let entries = scan_forest(tree)?;
    if u64::try_from(entries.len()).ok() != Some(expected_entries)
        || digest_entries(&entries) != expected_digest
        || entries.iter().any(|entry| {
            !entry.target.is_absolute()
                || !output_roots
                    .iter()
                    .any(|root| entry.target.starts_with(root.as_str()))
        })
    {
        return Err(ActivationError::IntegrityMismatch);
    }
    Ok(())
}

/// Inspects an already-staged forest without following links.
///
/// This recovery/integration constructor accepts only absolute leaf targets
/// beneath one of the persisted output roots. An empty root set is accepted
/// only for an actually empty forest.
pub fn inspect_staged_activation(
    tree: &Path,
    mut output_roots: Vec<StorePath>,
) -> Result<ActivationPlan, ActivationError> {
    output_roots.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    output_roots.dedup_by(|left, right| left == right);
    let entries = scan_forest(tree)?;
    if (output_roots.is_empty() && !entries.is_empty())
        || entries.iter().any(|entry| {
            !entry.target.is_absolute()
                || !output_roots
                    .iter()
                    .any(|root| entry.target.starts_with(root.as_str()))
        })
    {
        return Err(ActivationError::UnsafePath);
    }
    Ok(ActivationPlan {
        tree_digest: digest_entries(&entries),
        output_roots,
        entries,
        collisions: Vec::new(),
    })
}

fn scan_forest(tree: &Path) -> Result<Vec<ForestEntry>, ActivationError> {
    fn visit(
        root: &Path,
        relative: &Path,
        out: &mut Vec<ForestEntry>,
    ) -> Result<(), ActivationError> {
        let mut children = fs::read_dir(root.join(relative))?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let child_relative = relative.join(child.file_name());
            let kind = child.file_type()?;
            if kind.is_dir() {
                visit(root, &child_relative, out)?;
            } else if kind.is_symlink() {
                out.push(ForestEntry {
                    relative_path: child_relative,
                    target: fs::read_link(child.path())?,
                });
            } else {
                return Err(ActivationError::IntegrityMismatch);
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(tree, Path::new(""), &mut entries)?;
    Ok(entries)
}

fn digest_entries(entries: &[ForestEntry]) -> Digest {
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.relative_path.as_os_str().as_encoded_bytes());
        hasher.update(b"\0");
        hasher.update(entry.target.as_os_str().as_encoded_bytes());
        hasher.update(b"\0");
    }
    Digest::from_bytes(hasher.finalize().into())
}

fn sync_tree(root: &Path) -> Result<(), ActivationError> {
    let mut dirs = vec![root.to_path_buf()];
    let mut index = 0;
    while index < dirs.len() {
        for child in fs::read_dir(&dirs[index])? {
            let child = child?;
            if child.file_type()?.is_dir() {
                dirs.push(child.path());
            }
        }
        index += 1;
    }
    for dir in dirs.into_iter().rev() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use tempfile::TempDir;

    fn store(name: &str) -> StorePath {
        StorePath::new(&format!(
            "/nix/store/00000000000000000000000000000000-{name}"
        ))
        .unwrap()
    }

    #[test]
    fn keeps_non_conflicting_files_and_records_deterministic_winner() {
        let temp = TempDir::new().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir_all(a.join("bin")).unwrap();
        fs::create_dir_all(b.join("bin")).unwrap();
        fs::write(a.join("bin/tool"), b"a").unwrap();
        fs::write(a.join("bin/only-a"), b"a").unwrap();
        fs::write(b.join("bin/tool"), b"b").unwrap();
        fs::write(b.join("bin/only-b"), b"b").unwrap();
        let sources = vec![(store("a"), a), (store("b"), b)];
        let plan = stage_from_sources(
            &temp.path().join("stage"),
            &sources,
            CollisionPolicy::KeepLast,
        )
        .unwrap();
        assert_eq!(plan.entry_count(), 3);
        assert_eq!(plan.collisions().len(), 1);
        assert!(
            plan.entries()
                .iter()
                .any(|entry| entry.relative_path() == Path::new("bin/only-a"))
        );
        assert!(
            plan.entries()
                .iter()
                .any(|entry| entry.relative_path() == Path::new("bin/only-b"))
        );
    }

    #[test]
    fn aborts_collision_without_creating_staging() {
        let temp = TempDir::new().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("same"), b"a").unwrap();
        fs::write(b.join("same"), b"b").unwrap();
        let stage = temp.path().join("stage");
        let error = stage_from_sources(
            &stage,
            &[(store("a"), a), (store("b"), b)],
            CollisionPolicy::Abort,
        )
        .unwrap_err();
        assert!(matches!(error, ActivationError::Collision));
        assert!(!stage.exists());
    }

    #[test]
    fn detects_repointed_leaf() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("tool"), b"x").unwrap();
        let stage = temp.path().join("stage");
        let plan =
            stage_from_sources(&stage, &[(store("a"), source)], CollisionPolicy::Abort).unwrap();
        fs::remove_file(stage.join("tool")).unwrap();
        symlink("/tmp/evil", stage.join("tool")).unwrap();
        assert!(matches!(
            verify_activation(&stage, &plan),
            Err(ActivationError::IntegrityMismatch)
        ));
        let _ = Digest::from_str(&plan.tree_digest().to_string()).unwrap();
    }

    #[test]
    fn treats_source_symlink_as_a_leaf_without_following_it() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        symlink("/tmp/outside", source.join("link")).unwrap();
        let stage = temp.path().join("stage");
        let expected = store("a");
        let plan = stage_from_sources(
            &stage,
            &[(expected.clone(), source)],
            CollisionPolicy::Abort,
        )
        .unwrap();
        assert_eq!(plan.entry_count(), 1);
        assert_eq!(
            fs::read_link(stage.join("link")).unwrap(),
            PathBuf::from(expected.as_str()).join("link")
        );
    }

    #[test]
    fn rejects_file_directory_conflicts_before_writing() {
        let temp = TempDir::new().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(b.join("bin")).unwrap();
        fs::write(a.join("bin"), b"file").unwrap();
        fs::write(b.join("bin/tool"), b"tool").unwrap();
        let stage = temp.path().join("stage");
        assert!(matches!(
            stage_from_sources(
                &stage,
                &[(store("a"), a), (store("b"), b)],
                CollisionPolicy::KeepLast
            ),
            Err(ActivationError::StructuralConflict)
        ));
        assert!(!stage.exists());
    }
}
