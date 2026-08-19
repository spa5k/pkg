use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use pkg_core::state::{CollisionPolicy, Digest};
use pkg_core::{OutputName, SelectorId, StorePath};
use sha2::{Digest as _, Sha256};

/// One verified store output to expose in the activation forest.
#[derive(Debug, Clone)]
pub struct ActivationInput {
    store_path: StorePath,
    selector_id: Option<SelectorId>,
    output: Option<OutputName>,
}

impl ActivationInput {
    /// Uses the validated store path as both identity and read-only source.
    #[must_use]
    pub const fn new(store_path: StorePath) -> Self {
        Self {
            store_path,
            selector_id: None,
            output: None,
        }
    }

    /// Binds this source to its desired-state selector and output.
    #[must_use]
    pub const fn bound(selector_id: SelectorId, output: OutputName, store_path: StorePath) -> Self {
        Self {
            store_path,
            selector_id: Some(selector_id),
            output: Some(output),
        }
    }

    /// Returns the validated store source.
    #[must_use]
    pub const fn store_path(&self) -> &StorePath {
        &self.store_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Provider {
    output: StorePath,
    selector_id: Option<SelectorId>,
    output_name: Option<OutputName>,
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
    winner_selector: Option<SelectorId>,
    winner_output: Option<OutputName>,
    losers: Vec<StorePath>,
    loser_choices: Vec<(Option<SelectorId>, Option<OutputName>)>,
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

    /// Returns the winner's selector/output identity when supplied.
    #[must_use]
    pub fn winner_choice(&self) -> Option<(&SelectorId, &OutputName)> {
        self.winner_selector
            .as_ref()
            .zip(self.winner_output.as_ref())
    }

    /// Returns the other output providers in deterministic order.
    #[must_use]
    pub fn losers(&self) -> &[StorePath] {
        &self.losers
    }

    /// Returns all loser selector/output identities when supplied.
    #[must_use]
    pub fn loser_choices(&self) -> Option<Vec<(&SelectorId, &OutputName)>> {
        self.loser_choices
            .iter()
            .map(|(selector, output)| selector.as_ref().zip(output.as_ref()))
            .collect()
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
    let mut sources = inputs
        .iter()
        .map(|input| {
            (
                input.store_path.clone(),
                PathBuf::from(input.store_path.as_str()),
                input.selector_id.clone(),
                input.output.clone(),
            )
        })
        .collect::<Vec<_>>();
    sort_bound_sources(&mut sources);
    stage_ordered_sources(staging, &sources, collision_policy)
}

fn sort_bound_sources(
    sources: &mut [(StorePath, PathBuf, Option<SelectorId>, Option<OutputName>)],
) {
    sources.sort_by(|left, right| {
        left.2
            .as_ref()
            .map(SelectorId::as_str)
            .cmp(&right.2.as_ref().map(SelectorId::as_str))
            .then_with(|| {
                left.3
                    .as_ref()
                    .map(OutputName::as_str)
                    .cmp(&right.3.as_ref().map(OutputName::as_str))
            })
            .then_with(|| left.0.as_str().cmp(right.0.as_str()))
    });
}

#[cfg(test)]
pub(crate) fn stage_from_sources(
    staging: &Path,
    sources: &[(StorePath, PathBuf)],
    collision_policy: CollisionPolicy,
) -> Result<ActivationPlan, ActivationError> {
    let mut ordered = sources
        .iter()
        .map(|(output, source)| (output.clone(), source.clone(), None, None))
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    stage_ordered_sources(staging, &ordered, collision_policy)
}

fn stage_ordered_sources(
    staging: &Path,
    ordered: &[(StorePath, PathBuf, Option<SelectorId>, Option<OutputName>)],
    collision_policy: CollisionPolicy,
) -> Result<ActivationPlan, ActivationError> {
    if fs::symlink_metadata(staging).is_ok() {
        return Err(ActivationError::UnsafePath);
    }
    let mut providers: BTreeMap<PathBuf, Vec<Provider>> = BTreeMap::new();
    let mut directories = BTreeMap::<PathBuf, StorePath>::new();
    let mut output_roots = ordered
        .iter()
        .map(|(output, _, _, _)| output.clone())
        .collect::<Vec<_>>();
    output_roots.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    output_roots.dedup();
    for (output, source, selector_id, output_name) in ordered {
        let metadata = fs::symlink_metadata(source)?;
        if !metadata.file_type().is_dir() {
            return Err(ActivationError::UnsafePath);
        }
        walk_output(
            source,
            Path::new(""),
            output,
            selector_id.as_ref(),
            output_name.as_ref(),
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
                winner_selector: winner.selector_id.clone(),
                winner_output: winner.output_name.clone(),
                losers: choices
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != winner_index)
                    .map(|(_, provider)| provider.output.clone())
                    .collect(),
                loser_choices: choices
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != winner_index)
                    .map(|(_, provider)| {
                        (provider.selector_id.clone(), provider.output_name.clone())
                    })
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
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700).create(parent)?;
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
    selector_id: Option<&SelectorId>,
    output_name: Option<&OutputName>,
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
            walk_output(
                source_root,
                &child_relative,
                output,
                selector_id,
                output_name,
                providers,
                directories,
            )?;
        } else if kind.is_file() || kind.is_symlink() {
            providers
                .entry(child_relative.clone())
                .or_default()
                .push(Provider {
                    output: output.clone(),
                    selector_id: selector_id.cloned(),
                    output_name: output_name.cloned(),
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
    fn nested_activation_directories_are_private() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("share/locale/en/LC_MESSAGES")).unwrap();
        fs::write(
            source.join("share/locale/en/LC_MESSAGES/app.mo"),
            b"catalog",
        )
        .unwrap();
        let staging = temp.path().join("stage");

        stage_from_sources(
            &staging,
            &[(store("source"), source)],
            CollisionPolicy::Abort,
        )
        .unwrap();

        for relative in [
            "share",
            "share/locale",
            "share/locale/en",
            "share/locale/en/LC_MESSAGES",
        ] {
            assert_eq!(
                fs::metadata(staging.join(relative))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn bound_sources_record_selector_and_output_choices() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first");
        let last = temp.path().join("last");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&last).unwrap();
        fs::write(first.join("tool"), b"first").unwrap();
        fs::write(last.join("tool"), b"last").unwrap();
        let mut sources = vec![
            (
                store("last"),
                last,
                Some(SelectorId::new("sel_b").unwrap()),
                Some(OutputName::new("out").unwrap()),
            ),
            (
                store("first"),
                first,
                Some(SelectorId::new("sel_a").unwrap()),
                Some(OutputName::new("out").unwrap()),
            ),
        ];
        sort_bound_sources(&mut sources);
        let plan = stage_ordered_sources(
            &temp.path().join("stage"),
            &sources,
            CollisionPolicy::KeepLast,
        )
        .unwrap();
        let collision = &plan.collisions()[0];
        assert_eq!(
            collision
                .winner_choice()
                .map(|(selector, output)| (selector.as_str(), output.as_str())),
            Some(("sel_b", "out"))
        );
        assert_eq!(
            collision
                .loser_choices()
                .unwrap()
                .into_iter()
                .map(|(selector, output)| (selector.as_str(), output.as_str()))
                .collect::<Vec<_>>(),
            [("sel_a", "out")]
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
