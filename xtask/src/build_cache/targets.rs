//! Admission of Cargo-tagged target roots below their owning worktrees.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::{
    BuildCacheContext, BuildCacheDisposition, BuildCacheError, BuildCacheRootKind,
    BuildCacheRootReport, inventory_root, malformed_root,
};

/// One Cargo target directory bound to the worktree that owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCacheTargetDirectory {
    /// Worktree boundary the target directory must remain below.
    pub worktree: PathBuf,
    /// Cargo-reported target directory.
    pub directory: PathBuf,
}

impl BuildCacheTargetDirectory {
    /// Bind one Cargo-reported target directory to its owning worktree.
    #[must_use]
    pub fn new(worktree: impl Into<PathBuf>, directory: impl Into<PathBuf>) -> Self {
        Self {
            worktree: worktree.into(),
            directory: directory.into(),
        }
    }
}

pub(super) fn inventory_target_roots(
    context: &BuildCacheContext,
    roots: &mut Vec<BuildCacheRootReport>,
    current_roots: &mut BTreeSet<PathBuf>,
    requested_live_roots: &mut BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, BuildCacheError> {
    let mut current_target = None;
    if let Some(target) = &context.target_directory
        && let Some(root) = inventory_target_root(target)?
    {
        if root.disposition == BuildCacheDisposition::Kept {
            current_target = Some(root.path.clone());
            current_roots.insert(root.path.clone());
        }
        insert_distinct_root(roots, root);
    }
    for target in &context.live_target_directories {
        if let Some(root) = inventory_target_root(target)? {
            if root.disposition == BuildCacheDisposition::Kept {
                requested_live_roots.insert(root.path.clone());
            }
            insert_distinct_root(roots, root);
        }
    }
    Ok(current_target)
}

pub(super) fn inventory_target_root(
    target: &BuildCacheTargetDirectory,
) -> Result<Option<BuildCacheRootReport>, BuildCacheError> {
    let worktree = fs::canonicalize(&target.worktree).map_err(|source| BuildCacheError::Io {
        path: target.worktree.clone(),
        source,
    })?;
    let metadata = match fs::symlink_metadata(&target.directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(BuildCacheError::Io {
                path: target.directory.clone(),
                source,
            });
        }
    };
    if !metadata.file_type().is_dir() {
        return Ok(Some(malformed_root(
            &target.directory,
            &metadata,
            BuildCacheRootKind::WorktreeTarget,
            &worktree,
            "worktree target root is not a real directory",
        )));
    }
    let canonical = fs::canonicalize(&target.directory).map_err(|source| BuildCacheError::Io {
        path: target.directory.clone(),
        source,
    })?;
    if canonical == worktree || !canonical.starts_with(&worktree) {
        return Ok(Some(malformed_root(
            &canonical,
            &metadata,
            BuildCacheRootKind::WorktreeTarget,
            &worktree,
            "target root is outside its owning worktree",
        )));
    }
    inventory_root(&canonical, BuildCacheRootKind::WorktreeTarget, &worktree).map(Some)
}

pub(super) fn insert_distinct_root(
    roots: &mut Vec<BuildCacheRootReport>,
    root: BuildCacheRootReport,
) {
    if !roots.iter().any(|existing| existing.path == root.path) {
        roots.push(root);
    }
}
