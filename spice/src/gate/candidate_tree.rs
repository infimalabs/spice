//! Identity of the exact tree a staged commit would record.

use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;

use crate::repository::Repository;

use super::RunError;

/// Canonical Git object identity for a prospective staged commit tree.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CandidateTree(String);

impl CandidateTree {
    /// Canonical lowercase hexadecimal Git object identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CandidateTree {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(super) fn capture_candidate_tree(
    repository: &Repository,
    index: Option<&OsStr>,
) -> Result<CandidateTree, RunError> {
    let index = index.map(PathBuf::from).map(|index| {
        if index.is_absolute() {
            index
        } else {
            repository.worktree_root().join(index)
        }
    });
    // `git write-tree` may update the index cache-tree extension, so it takes
    // index.lock even under GIT_OPTIONAL_LOCKS=0. The gate tree engine instead
    // seals a private index copy and hashes every synthesized tree in memory.
    ::gate::tree::capture_staged_candidate_tree(repository.worktree_root(), index.as_deref())
        .map(CandidateTree)
        .map_err(|error| RunError::refusal("gate::candidate-tree-refused", error))
}

#[cfg(test)]
mod tests;
