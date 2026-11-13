//! What the live worktree holds that one selected tree does not.
//!
//! A surface that builds or tests a [`SelectedTree`] runs a private copy of
//! the index or a commit, never the worktree. An agent reading its output
//! must learn that from the output itself, so the surface reports which
//! snapshot it built and which worktree paths that snapshot left out. The
//! report is measured against the materialized root, never assumed from the
//! selector.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use super::{SelectedFileMode, SelectedLink, SelectedTree, TreeError};

/// How many left-out paths the rendered notice names before counting the rest.
const NAMED_PATHS: usize = 24;

/// One selected tree compared with the live worktree it was taken from.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeDivergence {
    snapshot: String,
    left_out: Vec<LeftOutPath>,
}

/// One worktree path whose live content the snapshot does not carry.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeftOutPath {
    path: String,
    state: LeftOutState,
}

/// Why the snapshot does not carry a worktree path's live content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftOutState {
    /// The worktree bytes or mode differ from the snapshot's entry.
    Modified,
    /// The snapshot carries the path; the worktree no longer has it.
    Missing,
    /// The worktree has a non-ignored file the snapshot does not carry.
    Untracked,
}

impl LeftOutState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::Untracked => "untracked",
        }
    }
}

impl LeftOutPath {
    /// Worktree-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Why the snapshot does not carry this path's live content.
    #[must_use]
    pub const fn state(&self) -> LeftOutState {
        self.state
    }
}

impl WorktreeDivergence {
    /// Which snapshot the surface built.
    #[must_use]
    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }

    /// Worktree paths the snapshot left out, sorted by path.
    #[must_use]
    pub fn left_out(&self) -> &[LeftOutPath] {
        &self.left_out
    }

    /// Whether the snapshot carries exactly the worktree's non-ignored content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left_out.is_empty()
    }

    /// The paths line of the notice: `PATH (STATE), ...` with a bounded tail.
    #[must_use]
    pub fn left_out_summary(&self) -> String {
        let mut text = String::new();
        for (index, path) in self.left_out.iter().take(NAMED_PATHS).enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            text.push_str(&path.path);
            text.push_str(" (");
            text.push_str(path.state.as_str());
            text.push(')');
        }
        let rest = self.left_out.len().saturating_sub(NAMED_PATHS);
        if rest > 0 {
            text.push_str(&format!(", and {rest} more"));
        }
        text
    }
}

impl fmt::Display for WorktreeDivergence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ran {} from a private copy, not this worktree; left out: {}",
            self.snapshot,
            self.left_out_summary()
        )
    }
}

impl SelectedTree {
    /// Measure which worktree paths this materialized snapshot left out.
    ///
    /// Every selected path is compared byte-for-byte and by executable mode
    /// against the live worktree, and every tracked symbolic link by its
    /// recorded target; every non-ignored worktree file outside the selection
    /// is untracked. Nested repositories are skipped the way Git skips them.
    ///
    /// # Errors
    ///
    /// Returns the repository, ignore-rule, or filesystem failure that kept
    /// the comparison from completing; a surface must not report a partial
    /// answer as the whole.
    pub fn worktree_divergence(&self) -> Result<WorktreeDivergence, TreeError> {
        let worktree = self.repository();
        let mut left_out = Vec::new();
        for (path, mode) in self.files.paths.iter().zip(&self.files.modes) {
            if let Some(state) = self.selected_path_state(worktree, path, *mode)? {
                left_out.push(LeftOutPath {
                    path: path.clone(),
                    state,
                });
            }
        }
        for link in &self.files.links {
            if let Some(state) = link_state(worktree, link)? {
                left_out.push(LeftOutPath {
                    path: link.path.clone(),
                    state,
                });
            }
        }
        let selected: BTreeSet<&str> = self
            .files
            .paths
            .iter()
            .map(String::as_str)
            .chain(self.files.links.iter().map(|link| link.path.as_str()))
            .collect();
        for path in untracked_paths(worktree, &selected)? {
            left_out.push(LeftOutPath {
                path,
                state: LeftOutState::Untracked,
            });
        }
        left_out.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(WorktreeDivergence {
            snapshot: self.snapshot_description(),
            left_out,
        })
    }

    fn snapshot_description(&self) -> String {
        match (&self.staged_authority, &self.source) {
            (Some(authority), _) => format!(
                "the staged index (tree {})",
                short_identity(authority.candidate_tree_identity())
            ),
            (None, super::source::SelectionSeal::Revision { before, revision }) => format!(
                "revision {revision} (commit {})",
                short_identity(&before.resolved_id().to_string())
            ),
            (None, super::source::SelectionSeal::Index { .. }) => "the staged index".to_owned(),
        }
    }

    fn selected_path_state(
        &self,
        worktree: &Path,
        path: &str,
        mode: SelectedFileMode,
    ) -> Result<Option<LeftOutState>, TreeError> {
        let live = worktree.join(path);
        let metadata = match fs::symlink_metadata(&live) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(LeftOutState::Missing));
            }
            Err(error) => return Err(TreeError::io("inspecting worktree path", &live, error)),
        };
        if !metadata.file_type().is_file() || !mode_matches(&metadata, mode) {
            return Ok(Some(LeftOutState::Modified));
        }
        let selected = self.root().join(path);
        let selected_bytes = fs::read(&selected)
            .map_err(|error| TreeError::io("reading selected path", &selected, error))?;
        let live_bytes = fs::read(&live)
            .map_err(|error| TreeError::io("reading worktree path", &live, error))?;
        Ok((selected_bytes != live_bytes).then_some(LeftOutState::Modified))
    }
}

fn short_identity(identity: &str) -> &str {
    identity.get(..12).unwrap_or(identity)
}

/// Compare one tracked link with the worktree without following either.
///
/// A checkout made without link support holds the target as a regular file's
/// bytes, which is the same record and compares the same way.
fn link_state(worktree: &Path, link: &SelectedLink) -> Result<Option<LeftOutState>, TreeError> {
    let live = worktree.join(&link.path);
    let metadata = match fs::symlink_metadata(&live) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(LeftOutState::Missing));
        }
        Err(error) => return Err(TreeError::io("inspecting worktree path", &live, error)),
    };
    let target = if metadata.file_type().is_symlink() {
        fs::read_link(&live).map(|target| target.into_os_string().into_encoded_bytes())
    } else if metadata.file_type().is_file() {
        fs::read(&live)
    } else {
        return Ok(Some(LeftOutState::Modified));
    }
    .map_err(|error| TreeError::io("reading worktree path", &live, error))?;
    Ok((target != link.target).then_some(LeftOutState::Modified))
}

#[cfg(target_family = "unix")]
fn mode_matches(metadata: &fs::Metadata, mode: SelectedFileMode) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let executable = metadata.permissions().mode() & 0o111 != 0;
    executable == matches!(mode, SelectedFileMode::Executable)
}

#[cfg(not(target_family = "unix"))]
fn mode_matches(_metadata: &fs::Metadata, _mode: SelectedFileMode) -> bool {
    true
}

/// Every non-ignored worktree file outside `selected`, in directory order.
fn untracked_paths(worktree: &Path, selected: &BTreeSet<&str>) -> Result<Vec<String>, TreeError> {
    let repo = super::open_repository(worktree)?;
    let index = repo
        .index_or_empty()
        .map_err(|source| TreeError::Repository {
            path: worktree.to_path_buf(),
            evidence: format!("cannot open the index for ignore rules: {source}"),
        })?;
    let mut excludes = repo
        .excludes(
            &index,
            None,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(|source| TreeError::Repository {
            path: worktree.to_path_buf(),
            evidence: format!("cannot assemble ignore rules: {source}"),
        })?;
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let directory = worktree.join(&relative);
        let entries = fs::read_dir(&directory)
            .map_err(|error| TreeError::io("listing worktree directory", &directory, error))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| TreeError::io("listing worktree directory", &directory, error))?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let file_type = entry.file_type().map_err(|error| {
                TreeError::io("inspecting worktree entry", &entry.path(), error)
            })?;
            let entry_relative = relative.join(&name);
            let Some(entry_text) = entry_relative.to_str() else {
                continue;
            };
            let mode = if file_type.is_dir() {
                gix::index::entry::Mode::DIR
            } else if file_type.is_symlink() {
                gix::index::entry::Mode::SYMLINK
            } else {
                gix::index::entry::Mode::FILE
            };
            let excluded = excludes
                .at_path(&entry_relative, Some(mode))
                .map_err(|error| TreeError::io("matching ignore rules", &entry.path(), error))?
                .is_excluded();
            if excluded {
                continue;
            }
            if file_type.is_dir() {
                if !entry.path().join(".git").exists() {
                    pending.push(entry_relative);
                }
            } else if !selected.contains(entry_text) {
                found.push(entry_text.to_owned());
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::Selector;
    use crate::test_repository::{self, Fixture};

    fn staged_fixture(label: &str) -> Fixture {
        let fixture = Fixture::repository(label);
        fixture.write(".gitignore", "target/\n");
        fixture.write("src/lib.rs", "pub fn lib() {}\n");
        fixture.write("src/edited.rs", "pub fn edited() {}\n");
        fixture.write("src/deleted.rs", "pub fn deleted() {}\n");
        test_repository::stage_all(fixture.path());
        test_repository::commit(fixture.path(), "opening");
        fixture
    }

    #[test]
    fn a_worktree_matching_its_snapshot_leaves_nothing_out() {
        let fixture = staged_fixture("divergence-clean");
        fixture.write("target/debug.bin", "ignored\n");
        let tree = Selector::staged().materialize(fixture.path()).unwrap();

        let divergence = tree.worktree_divergence().unwrap();

        assert!(divergence.is_empty(), "{divergence:?}");
        assert!(divergence.snapshot().starts_with("the staged index (tree "));
    }

    #[test]
    fn unstaged_edits_deletions_and_untracked_files_are_named_with_their_state() {
        let fixture = staged_fixture("divergence-edited");
        let tree = Selector::staged().materialize(fixture.path()).unwrap();
        fixture.write("src/edited.rs", "pub fn edited() { changed() }\n");
        fs::remove_file(fixture.join("src/deleted.rs")).unwrap();
        fixture.write("src/new.rs", "pub fn new() {}\n");
        fixture.write("target/debug.bin", "ignored\n");

        let divergence = tree.worktree_divergence().unwrap();

        let observed = divergence
            .left_out()
            .iter()
            .map(|path| (path.path(), path.state()))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                ("src/deleted.rs", LeftOutState::Missing),
                ("src/edited.rs", LeftOutState::Modified),
                ("src/new.rs", LeftOutState::Untracked),
            ]
        );
        assert_eq!(
            divergence.left_out_summary(),
            "src/deleted.rs (missing), src/edited.rs (modified), src/new.rs (untracked)"
        );
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn an_unstaged_mode_change_is_a_modification() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = staged_fixture("divergence-mode");
        let tree = Selector::staged().materialize(fixture.path()).unwrap();
        fs::set_permissions(
            fixture.join("src/lib.rs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let divergence = tree.worktree_divergence().unwrap();

        assert_eq!(
            divergence.left_out(),
            &[LeftOutPath {
                path: "src/lib.rs".to_owned(),
                state: LeftOutState::Modified,
            }]
        );
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn a_tracked_link_is_compared_by_its_target_and_never_reported_untracked() {
        use std::os::unix::fs::symlink;

        let fixture = staged_fixture("divergence-link");
        symlink("src/lib.rs", fixture.join("entry.rs")).unwrap();
        test_repository::stage(fixture.path(), &["entry.rs"]);
        let tree = Selector::staged().materialize(fixture.path()).unwrap();

        assert!(tree.worktree_divergence().unwrap().is_empty());

        fs::remove_file(fixture.join("entry.rs")).unwrap();
        symlink("src/edited.rs", fixture.join("entry.rs")).unwrap();

        assert_eq!(
            tree.worktree_divergence().unwrap().left_out(),
            &[LeftOutPath {
                path: "entry.rs".to_owned(),
                state: LeftOutState::Modified,
            }]
        );
    }

    #[test]
    fn a_revision_snapshot_names_its_commit_and_the_index_only_edits() {
        let fixture = staged_fixture("divergence-revision");
        let commit = test_repository::commit(fixture.path(), "second");
        fixture.write("src/staged.rs", "pub fn staged() {}\n");
        test_repository::stage(fixture.path(), &["src/staged.rs"]);
        let tree = Selector::revision(commit.clone())
            .materialize(fixture.path())
            .unwrap();

        let divergence = tree.worktree_divergence().unwrap();

        assert_eq!(
            divergence.snapshot(),
            format!("revision {commit} (commit {})", &commit[..12])
        );
        assert_eq!(
            divergence.left_out(),
            &[LeftOutPath {
                path: "src/staged.rs".to_owned(),
                state: LeftOutState::Untracked,
            }]
        );
    }
}
