//! Immutable Git tree selection for gate runs.
//!
//! A gate never measures the checkout. The staged selector captures one parsed
//! index plus the exact `HEAD` tree that gives staged renames their lineage,
//! while a revision selector resolves one immutable commit identity. Both
//! sources pass through the same path, mode, object, and materialization checks
//! before a private read-only root is returned to the caller, named together
//! with the repository it was taken from so that an arm which must observe that
//! repository cannot be pointed at a different one. A tracked symbolic link is
//! part of the tree's identity and of its worktree comparison, but nothing
//! follows it: the private root carries regular files only.
//!
//! Each materialization is a real directory of real bytes below the temporary
//! directory. Retained staged slots preserve unchanged source timestamps.
//! Every source-consuming child inherits the slot's lock description, so a
//! dropped or killed gate cannot expose its compiler's source to a refill.
//! Ephemeral roots are reclaimed only when no consumer holds their lock;
//! abandoned-workspace recovery applies the same kernel possession check.

use std::path::{Path, PathBuf};

use gix::objs::WriteTo as _;

use crate::state::RenameMap;

/// Stable gate-owned policy for deciding that a staged delete/add pair is a move.
const STAGED_RENAME_SIMILARITY: f32 = 0.5;
/// Zero is the plumbing API's explicit unlimited-candidate setting.
const STAGED_RENAME_LIMIT: usize = 0;

mod consumer;
mod divergence;
mod error;
mod materialize;
mod source;
mod workspace;

pub(crate) use consumer::spawn as spawn_consumer;
pub use divergence::{LeftOutPath, LeftOutState, WorktreeDivergence};
pub use error::TreeError;
use materialize::{
    create_private_directory_tree, create_private_workspace, make_immutable, make_removable,
    materialize_items, private_directory, verify_immutable, write_private_source,
};
use source::{IndexSeal, RevisionSeal, SelectionSeal, head_seal};
use workspace::BuildRoot;

/// Which immutable Git source a gate should measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// The repository index and its `HEAD` rename base as captured at selection time.
    Staged,
    /// The one commit named by this revision at selection time.
    Revision(String),
}

impl Selector {
    /// Select the staged index.
    #[must_use]
    pub const fn staged() -> Self {
        Self::Staged
    }

    /// Select one named revision.
    #[must_use]
    pub fn revision(revision: impl Into<String>) -> Self {
        Self::Revision(revision.into())
    }

    /// Materialize this source below a private immutable root.
    ///
    /// The returned root owns no checkout state and remains valid until the
    /// [`SelectedTree`] is dropped. Index and reference sources, including the
    /// staged index's `HEAD` rename base, are rechecked after every selected
    /// object has been copied, so a moving source cannot publish a partial or
    /// mixed tree or mismatched rename lineage.
    ///
    /// # Errors
    ///
    /// Returns the named repository, revision, index, path, mode, object, or
    /// filesystem failure that prevented an immutable tree from being formed.
    pub fn materialize(&self, repository: &Path) -> Result<SelectedTree, TreeError> {
        self.materialize_with(repository, |_| {})
    }

    fn materialize_with(
        &self,
        repository: &Path,
        after_capture: impl FnOnce(&Path),
    ) -> Result<SelectedTree, TreeError> {
        let repo = open_repository(repository)?;
        // The staged selection is the tree gates compile, so it is the one
        // worth refilling in place: unchanged files keep the modification
        // times the artifacts beside them were built against. A revision
        // selection is read for planning and comparison rather than built,
        // and stays ephemeral.
        let mut build = match self {
            Self::Staged => BuildRoot::retained(repository)?,
            Self::Revision(_) => BuildRoot::create()?,
        };

        let (entries, source, renames, staged_authority, publishes_state) = match self {
            Self::Staged => {
                let before = IndexSeal::capture(&repo)?;
                let head = head_seal(&repo)?;
                after_capture(&build.workspace);
                let head_commit = head
                    .as_ref()
                    .map(|seal| seal.resolve(&repo, "HEAD", &build.workspace))
                    .transpose()?;
                let index = before.open(&repo, &build.workspace)?;
                let entries = index_items(&index)?;
                materialize_items(&repo, &build.root, &entries.files)?;
                let candidate_tree = write_index_tree(&repo, &entries)?;
                let staged_diff = staged_diff(&repo, head_commit, &index)?;
                let authority = StagedAuthority {
                    entries: entries.clone(),
                    candidate_tree,
                    rename_base: head_commit,
                    renames: staged_diff.renames.clone(),
                    changes: staged_diff.changes,
                };
                (
                    entries,
                    SelectionSeal::Index { before, head },
                    staged_diff.renames,
                    Some(authority),
                    true,
                )
            }
            Self::Revision(revision) => {
                let before = RevisionSeal::capture(&repo, revision)?;
                after_capture(&build.workspace);
                let commit_id = before.resolve(&repo, revision, &build.workspace)?;
                let entries = revision_items(&repo, revision, commit_id)?;
                materialize_items(&repo, &build.root, &entries.files)?;
                (
                    entries,
                    SelectionSeal::Revision {
                        before,
                        revision: revision.clone(),
                    },
                    RenameMap::default(),
                    None,
                    false,
                )
            }
        };

        make_immutable(&build.root)?;
        verify_immutable(&build.root, &entries.files)?;
        let links = selected_links(&repo, &entries.links)?;
        source.verify(&repo)?;
        let files = SelectedFiles {
            paths: entries.files.iter().map(|item| item.path.clone()).collect(),
            modes: entries.files.iter().map(|item| item.mode.into()).collect(),
            links,
        };
        Ok(build.finish(
            repository,
            source,
            files,
            renames,
            staged_authority,
            publishes_state,
        ))
    }
}

/// Semantic authority carried by one staged index independently of its on-disk encoding.
///
/// Canonical entries retain path, object, and executable mode, and every
/// symbolic link its path and target object. Rename authority additionally
/// retains the exact `HEAD` commit used as the comparison base, the complete
/// detected old-to-new map, and every changed path derived from that same
/// comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedAuthority {
    entries: Entries,
    candidate_tree: String,
    rename_base: Option<gix::ObjectId>,
    renames: RenameMap,
    changes: Vec<String>,
}

/// One semantic component that differs between staged authority captures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagedAuthorityDifference {
    /// A canonical path, object identity, or executable mode moved.
    Tree,
    /// The exact `HEAD` commit used for staged rename detection moved.
    RenameBase,
    /// The canonical staged rename map moved.
    RenameLineage,
}

impl StagedAuthority {
    /// Git tree object identity for this exact staged entry inventory.
    #[must_use]
    pub fn candidate_tree_identity(&self) -> &str {
        &self.candidate_tree
    }

    /// Exact commit used as the staged rename and ancestry baseline.
    #[must_use]
    pub fn baseline_identity(&self) -> Option<String> {
        self.rename_base.map(|identity| identity.to_string())
    }

    /// Changed paths derived from this exact index and rename baseline.
    #[must_use]
    pub fn changed_paths(&self) -> &[String] {
        &self.changes
    }

    /// Name every semantic component that differs in another staged capture.
    #[must_use]
    pub fn differences(&self, observed: &Self) -> Vec<StagedAuthorityDifference> {
        let mut differences = Vec::new();
        if self.entries != observed.entries {
            differences.push(StagedAuthorityDifference::Tree);
        }
        if self.rename_base != observed.rename_base {
            differences.push(StagedAuthorityDifference::RenameBase);
        }
        if self.renames != observed.renames {
            differences.push(StagedAuthorityDifference::RenameLineage);
        }
        differences
    }
}

/// Result of checking a retained source at a boundary allowed to recapture a
/// semantic-equivalent Git index representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshableSourceState {
    /// The retained byte-sensitive source seal remains current.
    Current,
    /// Only the index seal reported movement; semantic recapture is required.
    IndexChanged,
}

/// One private materialization held through its gate and source consumers.
///
/// Dropping the tree retires its parent lease. Inherited child descriptions
/// continue to exclude refills and reclamation until their last close.
#[derive(Debug)]
pub struct SelectedTree {
    workspace: PathBuf,
    root: PathBuf,
    repository: PathBuf,
    source: SelectionSeal,
    files: SelectedFiles,
    renames: RenameMap,
    staged_authority: Option<StagedAuthority>,
    publishes_state: bool,
    retained: bool,
    possession: Option<consumer::ConsumerLease>,
}

/// Git file mode retained for one regular selected-tree entry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SelectedFileMode {
    /// Git regular blob mode `100644`.
    Regular,
    /// Git executable blob mode `100755`.
    Executable,
}

#[derive(Debug)]
struct SelectedFiles {
    paths: Vec<String>,
    modes: Vec<SelectedFileMode>,
    links: Vec<SelectedLink>,
}

/// One tracked symbolic link: its path and the target bytes Git recorded.
#[derive(Debug)]
struct SelectedLink {
    path: String,
    target: Vec<u8>,
}

impl SelectedTree {
    /// Root beneath which every selected path is normalized and relative.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Repository this selection was materialized from.
    ///
    /// Carried with the tree because the root alone cannot name it: the root is
    /// a private copy in a scratch directory, and anything that must observe the
    /// live repository the identity was taken from has no other way to reach the
    /// right one. A caller that supplied it separately could supply a different
    /// one, and the observation would be labelled with a tree it never saw.
    #[must_use]
    pub fn repository(&self) -> &Path {
        &self.repository
    }

    /// Gate-owned writable workspace that contains this immutable selection.
    ///
    /// Observation scratch belongs below this directory so it is protected by
    /// the same possession and abandoned-workspace recovery as the selected
    /// tree. Callers must never place observation state in the source
    /// repository.
    pub(crate) fn private_workspace(&self) -> &Path {
        &self.workspace
    }

    /// Materialize the exact `HEAD` commit captured beside a staged index.
    ///
    /// The retained object identity, rather than a fresh `HEAD` resolution,
    /// makes this safe for ancestry-dependent inventories acquired after the
    /// selected tree itself was sealed. Revision selections and unborn staged
    /// repositories have no staged base.
    ///
    /// # Errors
    ///
    /// Returns the same repository, object, path, or filesystem failures as
    /// [`Selector::materialize`].
    pub fn materialize_staged_base(&self) -> Result<Option<Self>, TreeError> {
        let Some(base) = self
            .staged_authority
            .as_ref()
            .and_then(|authority| authority.rename_base)
        else {
            return Ok(None);
        };
        Selector::revision(base.to_string())
            .materialize(&self.repository)
            .map(Some)
    }

    /// Prove that the live Git source has not moved since materialization.
    ///
    /// The staged seal retains its live index watch, so a write followed by a
    /// byte-for-byte restoration still counts as movement rather than silently
    /// becoming a fresh authority with the old digest.
    ///
    /// # Errors
    ///
    /// Returns the exact index, `HEAD`, revision, or repository movement that
    /// invalidated this selected source.
    pub fn verify_source(&self) -> Result<(), TreeError> {
        let repository = open_repository(&self.repository)?;
        self.source.verify(&repository)
    }

    /// Refresh a byte-level staged seal only when gate-owned observation rewrote
    /// index cache data without changing any staged semantics.
    ///
    /// Canonical entries, candidate tree, changed paths, the exact `HEAD`
    /// rename base, and rename lineage are recaptured before accepting the new
    /// seal. A current seal and every revision selection remain untouched.
    pub(crate) fn refresh_staged_seal(&mut self) -> Result<(), TreeError> {
        let Some(expected) = self.staged_authority.as_ref() else {
            return self.verify_source();
        };
        let repository = open_repository(&self.repository)?;
        match self.source.verify(&repository) {
            Ok(()) => return Ok(()),
            Err(TreeError::IndexChanged { .. }) => {}
            Err(error) => return Err(error),
        }
        let before = IndexSeal::capture(&repository)?;
        let head = head_seal(&repository)?;
        let scratch = BuildRoot::create()?;
        let head_commit = head
            .as_ref()
            .map(|seal| seal.resolve(&repository, "HEAD", &scratch.workspace))
            .transpose()?;
        let index = before.open(&repository, &scratch.workspace)?;
        let entries = index_items(&index)?;
        let candidate_tree = write_index_tree(&repository, &entries)?;
        let staged_diff = staged_diff(&repository, head_commit, &index)?;
        let observed = StagedAuthority {
            entries,
            candidate_tree,
            rename_base: head_commit,
            renames: staged_diff.renames,
            changes: staged_diff.changes,
        };
        if !expected.differences(&observed).is_empty() {
            return Err(TreeError::IndexChanged {
                path: repository.index_path(),
            });
        }
        self.source = SelectionSeal::Index { before, head };
        Ok(())
    }

    /// Check this source while preserving index movement as a recapture signal.
    ///
    /// Revision and rename-base movement remains an error. Only an index seal
    /// failure is classified for the enclosing pre-commit reservation to
    /// compare against a fresh semantic staged authority.
    pub(crate) fn verify_refreshable_source(&self) -> Result<RefreshableSourceState, TreeError> {
        let repository = open_repository(&self.repository)?;
        match self.source.verify(&repository) {
            Ok(()) => Ok(RefreshableSourceState::Current),
            Err(TreeError::IndexChanged { .. }) => Ok(RefreshableSourceState::IndexChanged),
            Err(error) => Err(error),
        }
    }

    /// Canonical regular paths carried by this exact selected source.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.files.paths
    }

    /// Git mode for one canonical selected path.
    #[must_use]
    pub fn mode(&self, path: &str) -> Option<SelectedFileMode> {
        self.files
            .paths
            .binary_search_by(|candidate| candidate.as_str().cmp(path))
            .ok()
            .map(|index| self.files.modes[index])
    }

    /// Canonical old-to-new paths detected from this exact selected source.
    #[must_use]
    pub(crate) const fn renames(&self) -> &RenameMap {
        &self.renames
    }

    /// Canonical staged entries and their exact rename lineage.
    #[must_use]
    pub(crate) const fn staged_authority(&self) -> Option<&StagedAuthority> {
        self.staged_authority.as_ref()
    }

    pub(crate) fn staged_changes(&self) -> Option<&[String]> {
        self.staged_authority
            .as_ref()
            .map(StagedAuthority::changed_paths)
    }

    /// Whether acceptance of this source may publish current gate state.
    #[must_use]
    pub(crate) const fn publishes_state(&self) -> bool {
        self.publishes_state
    }
}

fn write_index_tree(repo: &gix::Repository, entries: &Entries) -> Result<String, TreeError> {
    let mut editor = repo
        .edit_tree(gix::ObjectId::empty_tree(repo.object_hash()))
        .map_err(|source| TreeError::Repository {
            path: repository_path(repo),
            evidence: format!("cannot initialize staged candidate tree: {source}"),
        })?;
    let files = entries.files.iter().map(|item| {
        let kind = match item.mode {
            FileMode::Regular => gix::object::tree::EntryKind::Blob,
            FileMode::Executable => gix::object::tree::EntryKind::BlobExecutable,
        };
        (item.path.as_str(), kind, item.id)
    });
    let links = entries.links.iter().map(|link| {
        (
            link.path.as_str(),
            gix::object::tree::EntryKind::Link,
            link.id,
        )
    });
    for (path, kind, id) in files.chain(links) {
        editor
            .upsert(path, kind, id)
            .map_err(|source| TreeError::Repository {
                path: repository_path(repo),
                evidence: format!("cannot add {path:?} to staged candidate tree: {source}"),
            })?;
    }
    // The identity is evidence, not an object the caller repository needs to
    // retain. The high-level editor writes every synthesized directory tree to
    // the repository object database; detach it and hash the canonical tree
    // encodings in memory instead. The sealed index supplies each normalized
    // path, mode, and leaf identity; callers that execute selected bytes admit
    // those objects separately before using the identity.
    editor
        .detach()
        .write(|tree| {
            let mut encoded = Vec::with_capacity(tree.size() as usize);
            tree.write_to(&mut encoded)
                .map_err(|error| error.to_string())?;
            gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Tree, &encoded)
                .map_err(|error| error.to_string())
        })
        .map(|identity| identity.to_string())
        .map_err(|source| TreeError::Repository {
            path: repository_path(repo),
            evidence: format!("cannot identify staged candidate tree: {source}"),
        })
}

impl Drop for SelectedTree {
    fn drop(&mut self) {
        if let Some(possession) = self.possession.take() {
            possession.retire(&self.workspace, self.retained);
        }
    }
}

/// The Git directories one live repository publishes durable state below.
///
/// Both are named because a linked worktree has two: its own directory and the
/// common directory every linked worktree shares, which is where one authority
/// lives. A caller
/// that resolved only one of them would either give every worktree a private
/// ledger or give them all one worktree's imports.
pub(crate) fn git_directories(repository: &Path) -> Result<(PathBuf, PathBuf), TreeError> {
    let repo = open_repository(repository)?;
    Ok((repo.path().to_path_buf(), repo.common_dir().to_path_buf()))
}

/// Derive the exact tree identity represented by one staged index without
/// locking or writing the caller repository.
///
/// The index and any split-index companions are sealed and copied into a
/// private gate workspace before parsing. Synthesized tree objects are hashed
/// in memory, and the source seal is rechecked before the identity is returned.
pub fn capture_staged_candidate_tree(
    repository: &Path,
    index: Option<&Path>,
) -> Result<String, TreeError> {
    let repo = open_repository(repository)?;
    let index = index.map_or_else(|| repo.index_path(), Path::to_path_buf);
    let seal = IndexSeal::capture_at(index)?;
    let build = BuildRoot::create()?;
    let captured = seal.open(&repo, &build.workspace)?;
    let entries = index_items(&captured)?;
    let identity = write_index_tree(&repo, &entries)?;
    seal.verify()?;
    Ok(identity)
}

/// Capture the one stable set of regular paths named by the current Git index.
///
/// The index sources are sealed before they are opened and rechecked after
/// every entry is admitted, just as staged-tree materialization does. Object
/// identities are deliberately discarded: a live contract measures current
/// checkout bytes, while the index supplies only the closed set of path names.
pub(crate) fn tracked_paths(repository: &Path) -> Result<Vec<String>, TreeError> {
    let repo = open_repository(repository)?;
    let seal = IndexSeal::capture(&repo)?;
    let build = BuildRoot::create()?;
    let index = seal.open(&repo, &build.workspace)?;
    let entries = index_items(&index)?;
    seal.verify()?;
    Ok(entries.files.into_iter().map(|item| item.path).collect())
}

pub(crate) fn open_repository(path: &Path) -> Result<gix::Repository, TreeError> {
    let options = gix::open::Options::isolated()
        .strict_config(true)
        .config_overrides([
            "gitoxide.objects.noReplace=true",
            "index.skipHash=false",
            "diff.algorithm=myers",
        ]);
    let repository = gix::open_opts(path, options).map_err(|source| TreeError::Repository {
        path: path.to_path_buf(),
        evidence: source.to_string(),
    })?;
    // gix reads only the files backend; a reftable repository opens but
    // cannot instantiate `HEAD`, so name the format and its remedy up front.
    let storage = repository
        .config_snapshot()
        .string("extensions.refStorage")
        .map(|value| value.to_string());
    match storage {
        None => Ok(repository),
        Some(format) if format.eq_ignore_ascii_case("files") => Ok(repository),
        Some(format) => Err(TreeError::ReferenceStorage {
            path: path.to_path_buf(),
            format,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileMode {
    Regular,
    Executable,
}

impl From<FileMode> for SelectedFileMode {
    fn from(value: FileMode) -> Self {
        match value {
            FileMode::Regular => Self::Regular,
            FileMode::Executable => Self::Executable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    path: String,
    id: gix::ObjectId,
    mode: FileMode,
}

/// One tracked symbolic link, whose blob holds its target.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    path: String,
    id: gix::ObjectId,
}

/// Every entry one index or commit tree names, in path order.
///
/// Files are the bytes a private root carries. Links are identity only: they
/// enter the candidate tree and the worktree comparison, and no gate reads
/// through one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Entries {
    files: Vec<Item>,
    links: Vec<Link>,
}

struct StagedDiff {
    renames: RenameMap,
    changes: Vec<String>,
}

fn staged_diff(
    repo: &gix::Repository,
    head_commit: Option<gix::ObjectId>,
    index: &gix::index::State,
) -> Result<StagedDiff, TreeError> {
    let Some(head_commit) = head_commit else {
        return Ok(StagedDiff {
            renames: RenameMap::default(),
            changes: index
                .entries()
                .iter()
                .map(|entry| normalized_path(entry.path(index)))
                .collect::<Result<_, _>>()?,
        });
    };
    let tree = commit_tree_id(repo, "HEAD", head_commit)?;
    let tree_index = repo
        .index_from_tree(&tree)
        .map_err(|source| TreeError::Repository {
            path: repository_path(repo),
            evidence: format!("cannot expand sealed HEAD for rename detection: {source}"),
        })?;
    let mut resource_cache =
        repo.diff_resource_cache_for_tree_diff()
            .map_err(|source| TreeError::Repository {
                path: repository_path(repo),
                evidence: format!("cannot initialize staged rename detection: {source}"),
            })?;
    let mut pathspec = gix::pathspec::Search::from_specs(
        std::iter::empty::<gix::pathspec::Pattern>(),
        None,
        repo.workdir().unwrap_or_else(|| repo.git_dir()),
    )
    .map_err(|source| TreeError::Repository {
        path: repository_path(repo),
        evidence: format!("cannot initialize the staged rename path matcher: {source}"),
    })?;
    let mut pairs = Vec::new();
    let mut changes = Vec::new();
    gix_diff::index(
        &tree_index,
        index,
        |change| {
            match change {
                gix_diff::index::ChangeRef::Rewrite {
                    source_location,
                    location,
                    copy: false,
                    ..
                } => {
                    let source = normalized_path(source_location.as_ref())?;
                    let destination = normalized_path(location.as_ref())?;
                    pairs.push((source.clone(), destination.clone()));
                    changes.extend([source, destination]);
                }
                change => changes.push(normalized_path(change.location().as_ref())?),
            }
            Ok::<_, TreeError>(std::ops::ControlFlow::Continue(()))
        },
        Some(gix_diff::index::RewriteOptions {
            resource_cache: &mut resource_cache,
            find: repo,
            rewrites: gix_diff::Rewrites {
                copies: None,
                percentage: Some(STAGED_RENAME_SIMILARITY),
                limit: STAGED_RENAME_LIMIT,
                track_empty: false,
            },
        }),
        &mut pathspec,
        &mut |_, _, _, _| true,
    )
    .map_err(|source| TreeError::Repository {
        path: repository_path(repo),
        evidence: format!("cannot capture staged renames from the sealed index: {source}"),
    })?;
    let renames = RenameMap::new(pairs).map_err(|source| TreeError::Repository {
        path: repository_path(repo),
        evidence: format!("staged rename map is invalid: {source}"),
    })?;
    changes.sort();
    changes.dedup();
    Ok(StagedDiff { renames, changes })
}

fn repository_path(repo: &gix::Repository) -> PathBuf {
    repo.workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

fn index_items(index: &gix::index::File) -> Result<Entries, TreeError> {
    let mut entries = Entries::default();
    for entry in index.entries() {
        let path = normalized_path(entry.path(index))?;
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            return Err(TreeError::UnmergedIndex {
                path,
                stage: entry.stage_raw(),
            });
        }
        let mode = if entry.mode == gix::index::entry::Mode::FILE {
            FileMode::Regular
        } else if entry.mode == gix::index::entry::Mode::FILE_EXECUTABLE {
            FileMode::Executable
        } else if entry.mode == gix::index::entry::Mode::SYMLINK {
            entries.links.push(Link { path, id: entry.id });
            continue;
        } else if entry.mode == gix::index::entry::Mode::COMMIT {
            return Err(TreeError::Submodule { path });
        } else {
            return Err(TreeError::UnsupportedMode {
                path,
                mode: entry.mode.bits(),
            });
        };
        entries.files.push(Item {
            path,
            id: entry.id,
            mode,
        });
    }
    normalize_entries(entries)
}

fn resolve_revision(repo: &gix::Repository, revision: &str) -> Result<gix::ObjectId, TreeError> {
    revision_spec(repo, revision)?
        .single()
        .map(gix::Id::detach)
        .ok_or_else(|| TreeError::RevisionUnresolved {
            revision: revision.to_owned(),
            evidence: "the revision names a range rather than one object".to_owned(),
        })
}

fn revision_spec<'repo>(
    repo: &'repo gix::Repository,
    revision: &str,
) -> Result<gix::revision::Spec<'repo>, TreeError> {
    refuse_ambiguous_reference(repo, revision)?;
    let options = gix::revision::spec::parse::Options {
        refs_hint: gix::revision::spec::parse::RefsHint::Fail,
        object_kind_hint: None,
    };
    gix::revision::Spec::from_bstr(revision, repo, options).map_err(|source| {
        TreeError::RevisionUnresolved {
            revision: revision.to_owned(),
            evidence: source.to_string(),
        }
    })
}

fn refuse_ambiguous_reference(repo: &gix::Repository, revision: &str) -> Result<(), TreeError> {
    let atom_end = revision
        .char_indices()
        .find_map(|(index, character)| "~^:@".contains(character).then_some(index))
        .unwrap_or(revision.len());
    let atom = &revision[..atom_end];
    if atom.is_empty() || atom == "HEAD" || atom.starts_with("refs/") {
        return Ok(());
    }

    let candidates = [
        format!("refs/{atom}"),
        format!("refs/tags/{atom}"),
        format!("refs/heads/{atom}"),
        format!("refs/remotes/{atom}"),
        format!("refs/remotes/{atom}/HEAD"),
    ];
    let mut matched = Vec::new();
    for candidate in candidates {
        match repo.try_find_reference(&candidate) {
            Ok(Some(_)) => matched.push(candidate),
            Ok(None) => {}
            Err(source) => {
                return Err(TreeError::RevisionUnresolved {
                    revision: revision.to_owned(),
                    evidence: source.to_string(),
                });
            }
        }
    }
    if matched.len() > 1 {
        return Err(TreeError::RevisionUnresolved {
            revision: revision.to_owned(),
            evidence: format!(
                "the reference name is ambiguous among {}",
                matched.join(", ")
            ),
        });
    }
    Ok(())
}

fn revision_items(
    repo: &gix::Repository,
    revision: &str,
    commit_id: gix::ObjectId,
) -> Result<Entries, TreeError> {
    let tree_id = commit_tree_id(repo, revision, commit_id)?;
    let mut entries = Entries::default();
    collect_tree(repo, tree_id, "", &mut entries)?;
    normalize_entries(entries)
}

fn commit_tree_id(
    repo: &gix::Repository,
    revision: &str,
    commit_id: gix::ObjectId,
) -> Result<gix::ObjectId, TreeError> {
    let commit = verified_object(repo, commit_id, revision)?;
    if commit.kind != gix::objs::Kind::Commit {
        return Err(TreeError::RevisionNotCommit {
            revision: revision.to_owned(),
            actual: commit.kind.to_string(),
        });
    }
    let commit = commit
        .try_into_commit()
        .map_err(|source| TreeError::RevisionNotCommit {
            revision: revision.to_owned(),
            actual: source.actual.to_string(),
        })?;
    commit
        .tree_id()
        .map(gix::Id::detach)
        .map_err(|source| TreeError::MalformedObject {
            object: commit_id.to_string(),
            evidence: source.to_string(),
        })
}

fn collect_tree(
    repo: &gix::Repository,
    tree_id: gix::ObjectId,
    prefix: &str,
    collected: &mut Entries,
) -> Result<(), TreeError> {
    let label = if prefix.is_empty() { "." } else { prefix };
    let object = verified_object(repo, tree_id, label)?;
    if object.kind != gix::objs::Kind::Tree {
        return Err(TreeError::UnexpectedObject {
            path: label.to_owned(),
            object: tree_id.to_string(),
            expected: "tree",
            actual: object.kind.to_string(),
        });
    }
    let tree = object
        .try_into_tree()
        .map_err(|source| TreeError::UnexpectedObject {
            path: label.to_owned(),
            object: tree_id.to_string(),
            expected: "tree",
            actual: source.actual.to_string(),
        })?;
    let entries = tree
        .iter()
        .map(|entry| {
            entry
                .map(|entry| (entry.filename().to_vec(), entry.object_id(), entry.mode()))
                .map_err(|source| TreeError::MalformedObject {
                    object: tree_id.to_string(),
                    evidence: source.to_string(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (name, id, mode) in entries {
        let name = normalized_component(&name)?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        match mode.value() {
            0o040000 => collect_tree(repo, id, &path, collected)?,
            0o100644 => collected.files.push(Item {
                path,
                id,
                mode: FileMode::Regular,
            }),
            0o100755 => collected.files.push(Item {
                path,
                id,
                mode: FileMode::Executable,
            }),
            0o120000 => collected.links.push(Link { path, id }),
            0o160000 => return Err(TreeError::Submodule { path }),
            unsupported => {
                return Err(TreeError::UnsupportedMode {
                    path,
                    mode: u32::from(unsupported),
                });
            }
        }
    }
    Ok(())
}

fn normalize_entries(mut entries: Entries) -> Result<Entries, TreeError> {
    entries
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    entries
        .links
        .sort_by(|left, right| left.path.cmp(&right.path));
    let mut paths = entries
        .files
        .iter()
        .map(|item| item.path.as_str())
        .chain(entries.links.iter().map(|link| link.path.as_str()))
        .collect::<Vec<_>>();
    paths.sort_unstable();
    for pair in paths.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        if left == right
            || right
                .strip_prefix(left)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return Err(TreeError::PathCollision {
                first: left.to_owned(),
                second: right.to_owned(),
            });
        }
    }
    Ok(entries)
}

/// Read each link's recorded target so the worktree comparison can name drift.
fn selected_links(repo: &gix::Repository, links: &[Link]) -> Result<Vec<SelectedLink>, TreeError> {
    links
        .iter()
        .map(|link| {
            let object = verified_object(repo, link.id, &link.path)?;
            if object.kind != gix::objs::Kind::Blob {
                return Err(TreeError::UnexpectedObject {
                    path: link.path.clone(),
                    object: link.id.to_string(),
                    expected: "blob",
                    actual: object.kind.to_string(),
                });
            }
            Ok(SelectedLink {
                path: link.path.clone(),
                target: object.detach().data,
            })
        })
        .collect()
}

fn normalized_path(path: &[u8]) -> Result<String, TreeError> {
    let path = std::str::from_utf8(path).map_err(|_| TreeError::UnnameablePath {
        path: String::from_utf8_lossy(path).into_owned(),
    })?;
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains(['\\', '\n', '\r', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(TreeError::UnnameablePath {
            path: path.to_owned(),
        });
    }
    Ok(path.to_owned())
}

pub(crate) fn normalized_component(component: &[u8]) -> Result<String, TreeError> {
    let component = std::str::from_utf8(component).map_err(|_| TreeError::UnnameablePath {
        path: String::from_utf8_lossy(component).into_owned(),
    })?;
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '\\', '\n', '\r', '\0'])
    {
        return Err(TreeError::UnnameablePath {
            path: component.to_owned(),
        });
    }
    Ok(component.to_owned())
}

pub(crate) fn verified_object<'repo>(
    repo: &'repo gix::Repository,
    id: gix::ObjectId,
    path: &str,
) -> Result<gix::Object<'repo>, TreeError> {
    let object = repo
        .find_object(id)
        .map_err(|source| TreeError::MissingObject {
            path: path.to_owned(),
            object: id.to_string(),
            evidence: source.to_string(),
        })?;
    let actual =
        gix::objs::compute_hash(id.kind(), object.kind, &object.data).map_err(|source| {
            TreeError::MalformedObject {
                object: id.to_string(),
                evidence: source.to_string(),
            }
        })?;
    if actual != id {
        return Err(TreeError::ObjectChanged {
            path: path.to_owned(),
            expected: id.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_repository;
    use std::collections::BTreeMap;
    use std::fs;

    // Only Unix exposes the inode and change time that distinguish a
    // byte-identical replacement; a Windows copy keeps the length and mtime.
    #[cfg(unix)]
    #[test]
    fn byte_identical_index_replacement_refuses_the_captured_source() {
        let fixture = test_repository::Fixture::repository("tree-index-replacement");
        fixture.write("selected.txt", b"selected\n");
        test_repository::stage_all(fixture.path());
        let index = fixture.join(".git/index");

        let error = Selector::staged()
            .materialize_with(fixture.path(), |_| {
                let replacement = fixture.join(".git/index.replacement");
                fs::copy(&index, &replacement).expect("copy identical index bytes");
                fs::rename(replacement, &index).expect("replace captured index");
            })
            .expect_err("index write-restore must move source identity");

        assert!(matches!(error, TreeError::IndexChanged { .. }));
    }

    #[test]
    fn staged_tree_identity_is_hashed_without_writing_repository_objects() {
        let fixture = test_repository::Fixture::repository("tree-object-isolation");
        fixture.write("base.txt", b"base\n");
        test_repository::stage_all(fixture.path());
        test_repository::commit(fixture.path(), "base");
        fixture.write("nested/new.txt", b"novel staged bytes\n");
        test_repository::stage(fixture.path(), &["nested/new.txt"]);

        let objects = fixture.join(".git/objects");
        let before = file_snapshot(&objects);
        let selected = Selector::staged()
            .materialize(fixture.path())
            .expect("materialize novel staged tree");
        let identity = selected
            .staged_authority()
            .expect("staged authority")
            .candidate_tree_identity();

        assert_eq!(file_snapshot(&objects), before);
        assert!(
            !objects.join(&identity[..2]).join(&identity[2..]).exists(),
            "candidate tree identity must not be persisted in the caller object database"
        );
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn a_tracked_link_enters_the_candidate_tree_exactly_as_git_commits_it() {
        let fixture = test_repository::Fixture::repository("tree-link-identity");
        fixture.write("target.txt", b"target\n");
        std::os::unix::fs::symlink("target.txt", fixture.join("linked.txt"))
            .expect("create fixture symlink");
        test_repository::stage_all(fixture.path());

        let selected = Selector::staged()
            .materialize(fixture.path())
            .expect("materialize a staged link");
        let identity = selected
            .staged_authority()
            .expect("staged authority")
            .candidate_tree_identity()
            .to_owned();
        drop(selected);

        test_repository::commit(fixture.path(), "tracked link");
        let committed = gix::open(fixture.path())
            .expect("open fixture repository")
            .head_commit()
            .expect("read fixture head")
            .tree_id()
            .expect("read committed tree")
            .to_string();
        assert_eq!(identity, committed);
    }

    #[test]
    fn unresolved_index_refusal_preserves_every_conflict_stage_byte() {
        let fixture = test_repository::Fixture::repository("tree-conflict-preservation");
        fixture.write("conflicted.txt", b"base\n");
        test_repository::stage_all(fixture.path());
        let head = test_repository::commit(fixture.path(), "base");
        test_repository::install_conflict_stages(fixture.path(), "conflicted.txt");
        for (name, value) in [
            ("ORIG_HEAD", format!("{head}\n")),
            ("MERGE_HEAD", format!("{head}\n")),
            ("MERGE_MODE", "no-ff\n".to_owned()),
            ("MERGE_MSG", "allocator conflict\n".to_owned()),
        ] {
            fs::write(fixture.join(format!(".git/{name}")), value).expect("write merge control");
        }
        let before = file_snapshot(&fixture.join(".git"));

        let refusal = Selector::staged()
            .materialize(fixture.path())
            .expect_err("an unresolved index cannot become a selected tree");

        assert!(matches!(
            refusal,
            TreeError::UnmergedIndex { path, stage: 1 } if path == "conflicted.txt"
        ));
        assert_eq!(file_snapshot(&fixture.join(".git")), before);
    }

    #[test]
    fn linked_worktree_ref_movement_preserves_staged_source_authority() {
        let fixture = test_repository::Fixture::repository("tree-linked-movement");
        fixture.write("selected.txt", b"base\n");
        test_repository::stage_all(fixture.path());
        test_repository::commit(fixture.path(), "base");
        let linked = fixture.path().with_extension("linked");
        test_repository::create_linked_worktree(fixture.path(), &linked, "peer");
        fixture.write("selected.txt", b"staged\n");
        test_repository::stage(fixture.path(), &["selected.txt"]);

        let selected = Selector::staged()
            .materialize_with(fixture.path(), |_| {
                fs::write(linked.join("peer.txt"), b"first\n").unwrap();
                test_repository::stage(&linked, &["peer.txt"]);
                test_repository::commit(&linked, "peer movement during capture");
            })
            .expect("linked ref movement is outside staged source authority");
        assert_eq!(
            fs::read(selected.root().join("selected.txt")).unwrap(),
            b"staged\n"
        );

        fs::write(linked.join("peer.txt"), b"second\n").unwrap();
        test_repository::stage(&linked, &["peer.txt"]);
        test_repository::commit(&linked, "peer movement during verification");
        selected
            .verify_source()
            .expect("linked ref movement is outside retained source authority");
        drop(selected);
        test_repository::remove_linked_worktree(fixture.path(), &linked);
    }

    fn file_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut pending = vec![PathBuf::new()];
        let mut observed = BTreeMap::new();
        while let Some(relative) = pending.pop() {
            let mut entries = fs::read_dir(root.join(&relative))
                .expect("read object directory")
                .collect::<Result<Vec<_>, _>>()
                .expect("read object entry");
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let path = relative.join(entry.file_name());
                if entry.file_type().expect("inspect object entry").is_dir() {
                    pending.push(path);
                } else {
                    observed.insert(
                        path.to_string_lossy().into_owned(),
                        fs::read(entry.path()).expect("read object bytes"),
                    );
                }
            }
        }
        observed
    }
}
