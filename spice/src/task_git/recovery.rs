//! Lossless local quarantine for an explicitly recovered dirty handoff.
//!
//! This module owns only the Git/filesystem half of recovery. It snapshots one
//! exact dirty generation, makes that generation clean by touching only named
//! paths and the directories they leave empty, advances through
//! [`super::prepare_claim_observed`], and can either
//! restore the pre-claim generation or replay it on the observed synchronized
//! HEAD. Board possession is deliberately left to the command transaction that
//! drives these operations.

use super::*;

use std::collections::BTreeSet;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use wire::{Actor, CanonicalUuid, Phase, Seq, TaskHandle, TaskStamp};

/// Active recovery root beneath this worktree's canonical authority tree.
/// Retained generations live under `recovery/quarantine/KIND/TRANSACTION`.
const RECOVERY_DIRECTORY: &str = "authority/worktree/recovery/current";
const MANIFEST_FILE: &str = "manifest.json";
const OBJECT_DIRECTORY: &str = "objects";
const RECOVERY_VERSION: u32 = 1;

mod entries;
mod replay;
mod restore;
mod storage;
use entries::*;
pub use replay::replay_recovery;
use replay::*;
pub use restore::{RestoreOutcome, restore_recovery};
use storage::*;

#[cfg(test)]
mod tests;

/// Opaque identity of one durable dirty-handoff quarantine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recovery {
    transaction: String,
}

impl Recovery {
    /// Content address of the sealed original generation.
    #[must_use]
    pub fn transaction(&self) -> &str {
        &self.transaction
    }

    /// Whether text has the one spelling used by recovery content addresses.
    #[must_use]
    pub fn is_transaction(text: &str) -> bool {
        text.len() == 40
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

/// Exact board claim one synchronized quarantine is allowed to open.
///
/// This intent is written before the board mutation crosses its journal
/// boundary.  On restart the board's current heads therefore decide whether
/// the transaction is still pre-claim and must roll back, or is the exact
/// post-claim generation that may replay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryClaim {
    /// Actor authenticated for the claim.
    pub actor: Actor,
    /// Durable identity of the one named task.
    pub task: TaskStamp,
    /// Public handle sealed for an atomically created recovery target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<TaskHandle>,
    /// Whether the target is created in the same journal transaction as its claim.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
    /// Statement head witnessed before the claim.
    pub statement: Seq,
    /// Workflow head witnessed before the claim.
    pub workflow: Seq,
    /// Workflow phase witnessed before the claim.
    pub phase: Phase,
    /// Claim head witnessed before the claim, absent only for never-held work.
    pub claim: Option<Seq>,
    /// Lane bound to the claim.
    pub lane: CanonicalUuid,
    /// Occupant bound to the lane.
    pub occupant: CanonicalUuid,
    /// Resident epoch that may commit the claim.
    pub epoch: CanonicalUuid,
    /// Synchronized Git boundary the claim must journal.
    pub boundary: GitBoundary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RecoveryState {
    Captured,
    Quarantined,
    Syncing {
        boundary: GitBoundary,
    },
    Synchronized {
        boundary: GitBoundary,
    },
    /// A forward restore journals the lane HEAD it replays onto, beside the
    /// synchronized boundary when one was reached.
    Restoring {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundary: Option<GitBoundary>,
        target: String,
    },
    Replaying {
        boundary: GitBoundary,
    },
    Conflict {
        boundary: GitBoundary,
        detail: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    transaction: String,
    state: RecoveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim: Option<RecoveryClaim>,
    payload: Payload,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    repository: RepositorySeal,
    reference: String,
    head: String,
    configuration: String,
    index: StoredBlob,
    index_listing: String,
    staged_patch: StoredBlob,
    unstaged_patch: StoredBlob,
    staged_paths: Vec<String>,
    unstaged_paths: Vec<String>,
    tracked: Vec<PathState>,
    untracked: Vec<PathState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RepositorySeal {
    worktree: String,
    git: String,
    common: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBlob {
    digest: String,
    length: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PathState {
    path: String,
    entry: Option<StoredEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    kind: EntryKind,
    mode: u32,
    content: StoredBlob,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EntryKind {
    Regular,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedEntry {
    kind: EntryKind,
    mode: u32,
    digest: String,
    length: u64,
}

/// Return the current private recovery, if this worktree owns one.
pub fn pending_recovery(repository: &Repository) -> Result<Option<Recovery>, Error> {
    let root = recovery_root(repository)?;
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            let manifest = read_manifest(&root)?;
            verify_manifest_identity(repository, &manifest)?;
            Ok(Some(recovery(&manifest)))
        }
        Ok(_) => Err(recovery_refusal(
            "git::recovery-state-unsafe",
            format!("{} is not a private directory", root.display()),
            "move the unexpected recovery path aside and retry",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(recovery_filesystem("inspect recovery state", &root, error)),
    }
}

/// Seal and remove exactly one dirty generation from the visible lane.
pub fn quarantine_for_recovery(
    repository: &Repository,
    ambient: &Ambient,
) -> Result<Recovery, Error> {
    if let Some(recovery) = pending_recovery(repository)? {
        return resume_quarantine(repository, recovery);
    }
    let worktree = Worktree::new(repository.worktree_root(), ambient);
    let root = create_recovery_root(repository)?;
    let payload = match capture_payload(repository, worktree, &root) {
        Ok(payload) => payload,
        Err(error) => {
            discard_incomplete(&root);
            return Err(error);
        }
    };
    let transaction = digest_json(&payload)?;
    let mut manifest = Manifest {
        version: RECOVERY_VERSION,
        transaction,
        state: RecoveryState::Captured,
        claim: None,
        payload,
    };
    write_manifest(&root, &manifest)?;
    if let Err(error) = verify_original(repository, worktree, &root, &manifest) {
        // Nothing has left the lane yet, so the sealed copy has no purpose.
        discard_incomplete(&root);
        return Err(error);
    }
    if let Err(error) = clean_manifest_paths(worktree, &manifest.payload) {
        // Cleaning may already have removed sealed paths. Put the sealed
        // generation back so a refused quarantine leaves the lane as it found
        // it; a failed rollback keeps the store for `task capture --restore`.
        return Err(
            match restore_recovery(repository, ambient, &recovery(&manifest)) {
                Ok(_) => error,
                Err(rollback) => rollback,
            },
        );
    }
    manifest.state = RecoveryState::Quarantined;
    write_manifest(&root, &manifest)?;
    Ok(recovery(&manifest))
}

fn resume_quarantine(repository: &Repository, recovery: Recovery) -> Result<Recovery, Error> {
    let root = recovery_root(repository)?;
    let manifest = read_expected(&root, &recovery)?;
    match manifest.state {
        RecoveryState::Captured => Err(recovery_refusal(
            "git::recovery-capture-interrupted",
            format!("recovery {} stopped while cleaning", recovery.transaction),
            "retry explicit recovery to restore the sealed original state first",
        )),
        RecoveryState::Quarantined
        | RecoveryState::Syncing { .. }
        | RecoveryState::Synchronized { .. }
        | RecoveryState::Restoring { .. }
        | RecoveryState::Replaying { .. }
        | RecoveryState::Conflict { .. } => Ok(recovery),
    }
}

/// Durably bind a synchronized quarantine to one exact board claim intent.
pub fn bind_recovery_claim(
    repository: &Repository,
    recovery: &Recovery,
    claim: RecoveryClaim,
) -> Result<(), Error> {
    let root = recovery_root(repository)?;
    let mut manifest = read_expected(&root, recovery)?;
    let boundary = synchronized_boundary(&manifest)?;
    if boundary != claim.boundary {
        return Err(recovery_refusal(
            "git::recovery-claim-boundary-mismatch",
            format!(
                "recovery synchronized at {boundary:?}, but claim intent names {:?}",
                claim.boundary
            ),
            "retain the quarantine and retry only against its exact synchronized boundary",
        ));
    }
    match &manifest.claim {
        Some(existing) if existing == &claim => return Ok(()),
        Some(existing) => {
            return Err(recovery_refusal(
                "git::recovery-claim-ambiguous",
                format!("sealed claim {existing:?}; requested claim {claim:?}"),
                "retain the quarantine and resume only its originally sealed task claim",
            ));
        }
        None => {}
    }
    manifest.claim = Some(claim);
    write_manifest(&root, &manifest)
}

/// Read the exact board claim intent sealed beside a recovery transaction.
pub fn recovery_claim(
    repository: &Repository,
    recovery: &Recovery,
) -> Result<Option<RecoveryClaim>, Error> {
    let root = recovery_root(repository)?;
    let manifest = read_expected(&root, recovery)?;
    let Some(claim) = manifest.claim.clone() else {
        return Ok(None);
    };
    let boundary = synchronized_boundary(&manifest)?;
    if boundary != claim.boundary {
        return Err(recovery_refusal(
            "git::recovery-claim-boundary-mismatch",
            format!(
                "recovery state names {boundary:?}, but sealed claim names {:?}",
                claim.boundary
            ),
            "retain the quarantine and inspect its manifest before retrying",
        ));
    }
    Ok(Some(claim))
}

fn synchronized_boundary(manifest: &Manifest) -> Result<GitBoundary, Error> {
    match &manifest.state {
        RecoveryState::Synchronized { boundary }
        | RecoveryState::Restoring {
            boundary: Some(boundary),
            ..
        }
        | RecoveryState::Replaying { boundary }
        | RecoveryState::Conflict { boundary, .. } => Ok(boundary.clone()),
        RecoveryState::Captured
        | RecoveryState::Quarantined
        | RecoveryState::Syncing { .. }
        | RecoveryState::Restoring { boundary: None, .. } => Err(recovery_refusal(
            "git::recovery-claim-not-synchronized",
            "the quarantine has no completed synchronized boundary",
            "restore the pre-claim generation or complete synchronization before claiming",
        )),
    }
}

/// Advance the quarantined clean lane through the ordinary claim boundary.
pub fn synchronize_recovery(
    repository: &Repository,
    ambient: &Ambient,
    recovery: &Recovery,
) -> Result<GitBoundary, Error> {
    let worktree = Worktree::new(repository.worktree_root(), ambient);
    let root = recovery_root(repository)?;
    let manifest = read_expected(&root, recovery)?;
    verify_synchronization_identity(repository, worktree, &manifest)?;
    match manifest.state.clone() {
        RecoveryState::Synchronized { boundary }
        | RecoveryState::Replaying { boundary }
        | RecoveryState::Conflict { boundary, .. } => return Ok(boundary),
        RecoveryState::Restoring { .. } => {
            return Err(recovery_refusal(
                "git::recovery-restore-in-progress",
                "the sealed generation is being restored onto the lane HEAD",
                "resume the restore with `spice task capture --restore`",
            ));
        }
        RecoveryState::Syncing { boundary } => {
            return resume_synchronization(repository, worktree, &root, manifest, boundary);
        }
        RecoveryState::Captured => {
            return Err(interrupted_capture(recovery));
        }
        RecoveryState::Quarantined => {}
    }
    verify_text(
        "pre-synchronization HEAD",
        &manifest.payload.head,
        &current_head(worktree)?,
    )?;
    require_clean(worktree, "task recovery synchronization")?;
    let sealed = manifest.clone();
    let mut journaled = manifest;
    let boundary = prepare_claim_observed(
        repository,
        ambient,
        LooseCommits::Adopt,
        || verify_synchronization_identity(repository, worktree, &sealed),
        |boundary| {
            journaled.state = RecoveryState::Syncing {
                boundary: boundary.clone(),
            };
            write_manifest(&root, &journaled)
        },
    )?;
    journaled.state = RecoveryState::Synchronized {
        boundary: boundary.clone(),
    };
    write_manifest(&root, &journaled)?;
    Ok(boundary)
}

fn verify_synchronization_identity(
    repository: &Repository,
    worktree: Worktree<'_>,
    manifest: &Manifest,
) -> Result<(), Error> {
    verify_manifest_identity(repository, manifest)?;
    verify_text(
        "symbolic ref",
        &manifest.payload.reference,
        &required_read(
            worktree,
            &["symbolic-ref", "HEAD"],
            "verify synchronization symbolic HEAD",
        )?,
    )?;
    verify_text(
        "configuration",
        &manifest.payload.configuration,
        &configuration(worktree)?,
    )
}

fn resume_synchronization(
    repository: &Repository,
    worktree: Worktree<'_>,
    root: &Path,
    mut manifest: Manifest,
    boundary: GitBoundary,
) -> Result<GitBoundary, Error> {
    recover_pending(worktree)?;
    let head = current_head(worktree)?;
    if head != manifest.payload.head && head != boundary.head {
        return Err(recovery_refusal(
            "git::recovery-sync-ambiguous",
            format!(
                "recovery expected HEAD {} or {}, but observed {head}",
                manifest.payload.head, boundary.head
            ),
            "inspect the retained quarantine and moved branch before retrying",
        ));
    }
    if head == manifest.payload.head {
        let sealed = manifest.clone();
        let resumed = prepare_claim_observed(
            repository,
            worktree.ambient,
            LooseCommits::Adopt,
            || verify_synchronization_identity(repository, worktree, &sealed),
            |_| Ok(()),
        )?;
        if resumed != boundary {
            return Err(recovery_refusal(
                "git::recovery-baseline-moved",
                format!("sealed boundary {boundary:?}; resumed boundary {resumed:?}"),
                "restore the quarantine and retry against one stable baseline",
            ));
        }
    }
    manifest.state = RecoveryState::Synchronized {
        boundary: boundary.clone(),
    };
    write_manifest(root, &manifest)?;
    Ok(boundary)
}

fn capture_payload(
    repository: &Repository,
    worktree: Worktree<'_>,
    root: &Path,
) -> Result<Payload, Error> {
    refuse_unsupported_tree_entries(worktree)?;
    let dirty = dirty_paths(worktree)?;
    for path in dirty.all() {
        validate_relative(path)?;
    }
    let tracked_paths = union_paths(&dirty.staged, &dirty.unstaged);
    if tracked_paths.is_empty()
        && dirty.untracked.is_empty()
        && !has_snapshot_recovery(repository, worktree.ambient)?
    {
        return Err(recovery_refusal(
            "git::recovery-clean",
            "explicit dirty-handoff recovery found no dirty paths",
            "use ordinary task claim on this clean lane",
        ));
    }
    let index_path = git_state_path(worktree, "index")?;
    let index_bytes = read_required(&index_path, "capture exact Git index")?;
    Ok(Payload {
        repository: repository_seal(repository)?,
        reference: required_read(worktree, &["symbolic-ref", "HEAD"], "read symbolic HEAD")?,
        head: current_head(worktree)?,
        configuration: configuration(worktree)?,
        index: store_blob(root, &index_bytes)?,
        index_listing: index_listing(worktree)?,
        staged_patch: store_git_output(
            root,
            worktree,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--cached",
                "HEAD",
            ],
            "capture staged patch",
        )?,
        unstaged_patch: store_git_output(
            root,
            worktree,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--src-prefix=a/",
                "--dst-prefix=b/",
            ],
            "capture unstaged patch",
        )?,
        staged_paths: dirty.staged,
        unstaged_paths: dirty.unstaged,
        tracked: capture_paths(worktree, root, &tracked_paths, true)?,
        untracked: capture_paths(worktree, root, &dirty.untracked, false)?,
    })
}

fn refuse_unsupported_tree_entries(worktree: Worktree<'_>) -> Result<(), Error> {
    let mut pending = vec![worktree.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            recovery_filesystem("enumerate recovery worktree", &directory, error)
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                recovery_filesystem("read recovery worktree entry", &directory, error)
            })?;
            inspect_tree_entry(worktree, &entry.path(), &mut pending)?;
        }
    }
    Ok(())
}

fn inspect_tree_entry(
    worktree: Worktree<'_>,
    path: &Path,
    pending: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    let relative = path.strip_prefix(&*worktree).map_err(|error| {
        recovery_refusal(
            "git::recovery-path-escape",
            error.to_string(),
            "inspect the worktree root before retrying",
        )
    })?;
    if relative == Path::new(".git") {
        return Ok(());
    }
    let written = path_text(relative)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| recovery_filesystem("inspect recovery worktree entry", path, error))?;
    let kind = metadata.file_type();
    if kind.is_file() || kind.is_symlink() {
        return Ok(());
    }
    if ignored_path(worktree, &written)? {
        return Ok(());
    }
    if kind.is_dir() {
        if fs::symlink_metadata(path.join(".git")).is_ok() {
            return Err(recovery_refusal(
                "git::recovery-nested-repository",
                format!("unignored directory {written:?} contains nested Git authority"),
                "move the nested repository out of the handoff before retrying",
            ));
        }
        pending.push(path.to_path_buf());
        return Ok(());
    }
    Err(recovery_refusal(
        "git::recovery-file-kind",
        format!("unignored path {written:?} is a socket, device, or FIFO"),
        "replace unsupported filesystem objects with regular files before retrying",
    ))
}

fn ignored_path(worktree: Worktree<'_>, path: &str) -> Result<bool, Error> {
    let output = run(worktree, &["check-ignore", "--quiet", "--", path])?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::fault(
            "git::recovery-inspection-failed",
            output.detail(&format!("inspect ignore policy for {path}")),
        )),
    }
}

fn changed_paths(worktree: Worktree<'_>, args: &[&str]) -> Result<Vec<String>, Error> {
    let output = run(worktree, args)?;
    if !output.success() {
        return Err(Error::fault(
            "git::recovery-inspection-failed",
            output.detail("enumerate dirty paths"),
        ));
    }
    parse_nul_paths(&output.stdout)
}

fn parse_nul_paths(output: &str) -> Result<Vec<String>, Error> {
    if !output.is_empty() && !output.ends_with('\0') {
        return Err(Error::fault(
            "git::recovery-path-list-invalid",
            "Git returned an unterminated dirty path list",
        ));
    }
    let mut paths = output
        .split_terminator('\0')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for path in &paths {
        validate_relative(path)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn validate_relative(written: &str) -> Result<(), Error> {
    let path = Path::new(written);
    let admitted = !written.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if admitted {
        Ok(())
    } else {
        Err(recovery_refusal(
            "git::recovery-path-escape",
            format!("dirty path {written:?} is not a normalized repository-relative path"),
            "rename the path inside the repository before retrying",
        ))
    }
}

/// Name a sealed path beneath the worktree, refusing any existing parent that
/// is not a real directory. A missing parent admits the path: nothing beneath
/// it exists to traverse, and writing the path back creates it.
fn admitted_path(worktree: Worktree<'_>, written: &str) -> Result<PathBuf, Error> {
    validate_relative(written)?;
    let relative = Path::new(written);
    let mut cursor = worktree.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            unreachable!("validate_relative admitted only normal components");
        };
        cursor.push(name);
        let metadata = match fs::symlink_metadata(&cursor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(recovery_filesystem(
                    "inspect dirty path parent",
                    &cursor,
                    error,
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(recovery_refusal(
                "git::recovery-symlink-traversal",
                format!(
                    "dirty path {written:?} crosses unsafe parent {}",
                    cursor.display()
                ),
                "replace the parent with a repository-local directory before retrying",
            ));
        }
    }
    Ok(worktree.join(relative))
}

fn union_paths(left: &[String], right: &[String]) -> Vec<String> {
    left.iter()
        .chain(right)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn verify_original(
    repository: &Repository,
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &Manifest,
) -> Result<(), Error> {
    refuse_unsupported_tree_entries(worktree)?;
    verify_manifest_identity(repository, manifest)?;
    let payload = &manifest.payload;
    verify_text(
        "symbolic ref",
        &payload.reference,
        &required_read(worktree, &["symbolic-ref", "HEAD"], "verify symbolic HEAD")?,
    )?;
    verify_text("HEAD", &payload.head, &current_head(worktree)?)?;
    verify_text(
        "configuration",
        &payload.configuration,
        &configuration(worktree)?,
    )?;
    verify_index(worktree, root, payload)?;
    verify_path_sets(worktree, payload)?;
    verify_paths(worktree, root, &payload.tracked)?;
    verify_paths(worktree, root, &payload.untracked)
}

fn verify_manifest_identity(repository: &Repository, manifest: &Manifest) -> Result<(), Error> {
    if manifest.version != RECOVERY_VERSION {
        return Err(recovery_refusal(
            "git::recovery-version",
            format!("unsupported recovery manifest version {}", manifest.version),
            "use the Spice build that created this retained quarantine",
        ));
    }
    let expected = repository_seal(repository)?;
    if manifest.payload.repository != expected {
        return Err(recovery_refusal(
            "git::recovery-repository-moved",
            format!(
                "sealed {:?}; observed {expected:?}",
                manifest.payload.repository
            ),
            "run recovery only from the worktree that created the quarantine",
        ));
    }
    let computed = digest_json(&manifest.payload)?;
    verify_text("transaction digest", &manifest.transaction, &computed)
}

fn verify_index(worktree: Worktree<'_>, root: &Path, payload: &Payload) -> Result<(), Error> {
    let index_path = git_state_path(worktree, "index")?;
    let bytes = read_required(&index_path, "verify exact Git index")?;
    verify_blob_bytes(root, &payload.index, &bytes)?;
    verify_text(
        "index listing",
        &payload.index_listing,
        &index_listing(worktree)?,
    )
}

fn verify_path_sets(worktree: Worktree<'_>, payload: &Payload) -> Result<(), Error> {
    let dirty = dirty_paths(worktree)?;
    verify_list("staged paths", &payload.staged_paths, &dirty.staged)?;
    verify_list("unstaged paths", &payload.unstaged_paths, &dirty.unstaged)?;
    verify_list(
        "untracked paths",
        &path_names(&payload.untracked),
        &dirty.untracked,
    )
}

fn clean_manifest_paths(worktree: Worktree<'_>, payload: &Payload) -> Result<(), Error> {
    restore_tracked_to_head(worktree, &payload.tracked)?;
    for state in &payload.untracked {
        if !path_is_tracked(worktree, &state.path)? {
            quarantine_named_path(worktree, &state.path)?;
        }
    }
    require_quarantined(worktree)
}

/// Refuse unless the path listings a payload is sealed from are all empty.
///
/// `git status` is not asked: its untracked cache answers from a parent
/// directory's cached listing, which removing a file beneath a child does not
/// invalidate, so it can keep naming a directory this quarantine just emptied.
fn require_quarantined(worktree: Worktree<'_>) -> Result<(), Error> {
    let dirty = dirty_paths(worktree)?;
    if dirty.is_empty() {
        return Ok(());
    }
    Err(Error::refusal(
        "git::worktree-dirty",
        format!(
            "task recovery quarantine requires a clean worktree:\n{}",
            dirty.rendered()
        ),
        "commit or clear every reported path before retrying",
    ))
}

/// Remove one sealed untracked path and every parent directory that leaves
/// empty, so the quarantined lane holds no residue of the path.
///
/// Pruning stops at the first directory that will not go: one that still
/// holds something, or one the filesystem refuses to remove. A leftover empty
/// directory is residue the path listings never name.
fn quarantine_named_path(worktree: Worktree<'_>, written: &str) -> Result<(), Error> {
    remove_named_path(worktree, written)?;
    let mut parent = Path::new(written).parent();
    while let Some(relative) = parent.filter(|relative| !relative.as_os_str().is_empty()) {
        match fs::remove_dir(worktree.join(relative)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
        parent = relative.parent();
    }
    Ok(())
}

fn path_is_tracked(worktree: Worktree<'_>, path: &str) -> Result<bool, Error> {
    let output = run(worktree, &["ls-files", "--error-unmatch", "--", path])?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::fault(
            "git::recovery-inspection-failed",
            output.detail(&format!("inspect index ownership of {path}")),
        )),
    }
}

fn path_is_tracked_at_head(worktree: Worktree<'_>, path: &str) -> Result<bool, Error> {
    let output = run(
        worktree,
        &["ls-tree", "--name-only", "-z", "HEAD", "--", path],
    )?;
    if output.success() {
        Ok(!output.stdout.is_empty())
    } else {
        Err(Error::fault(
            "git::recovery-inspection-failed",
            output.detail(&format!("inspect HEAD ownership of {path}")),
        ))
    }
}

fn restore_tracked_to_head(worktree: Worktree<'_>, paths: &[PathState]) -> Result<(), Error> {
    let mut owned = vec![
        "restore".to_owned(),
        "--source=HEAD".to_owned(),
        "--staged".to_owned(),
        "--worktree".to_owned(),
        "--".to_owned(),
    ];
    let path_start = owned.len();
    for state in paths {
        if path_is_tracked(worktree, &state.path)?
            || path_is_tracked_at_head(worktree, &state.path)?
        {
            owned.push(state.path.clone());
        }
    }
    if owned.len() == path_start {
        return Ok(());
    }
    let args = owned.iter().map(String::as_str).collect::<Vec<_>>();
    let restored = run(worktree, &args)?;
    if restored.success() {
        Ok(())
    } else {
        Err(Error::fault(
            "git::recovery-clean-failed",
            restored.detail("restore manifest paths to HEAD"),
        ))
    }
}

fn remove_named_path(worktree: Worktree<'_>, written: &str) -> Result<(), Error> {
    let path = admitted_path(worktree, written)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
            fs::remove_file(&path)
                .map_err(|error| recovery_filesystem("quarantine named path", &path, error))
        }
        Ok(_) => Err(recovery_refusal(
            "git::recovery-file-kind",
            format!("named quarantine path {written:?} became a directory or special file"),
            "restore the sealed generation before retrying",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(recovery_filesystem(
            "inspect named quarantine path",
            &path,
            error,
        )),
    }
}

/// Restore the sealed worktree ignore files before exact rollback judges
/// concurrent drift. Cleaning may have returned those files to `HEAD`, making
/// paths that were intentionally outside the captured generation appear as
/// new untracked work under the wrong rules.
fn restore_ignore_paths(
    worktree: Worktree<'_>,
    root: &Path,
    payload: &Payload,
) -> Result<(), Error> {
    for state in payload.tracked.iter().chain(&payload.untracked) {
        if Path::new(&state.path)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(".gitignore")
        {
            restore_path(worktree, root, state)?;
        }
    }
    Ok(())
}

fn verify_rollback_authority(
    repository: &Repository,
    worktree: Worktree<'_>,
    manifest: &Manifest,
) -> Result<(), Error> {
    verify_manifest_identity(repository, manifest)?;
    verify_text(
        "symbolic ref",
        &manifest.payload.reference,
        &required_read(worktree, &["symbolic-ref", "HEAD"], "verify symbolic HEAD")?,
    )?;
    verify_text(
        "configuration",
        &manifest.payload.configuration,
        &configuration(worktree)?,
    )
}

fn verify_rollback_drift(worktree: Worktree<'_>, payload: &Payload) -> Result<(), Error> {
    refuse_unsealed_dirty_paths(
        worktree,
        payload,
        "git::recovery-rollback-drift",
        "preserve the retained quarantine and inspect concurrent lane edits",
    )
}

fn refuse_unsealed_dirty_paths(
    worktree: Worktree<'_>,
    payload: &Payload,
    reason: &'static str,
    remedy: &'static str,
) -> Result<(), Error> {
    let dirty = dirty_paths(worktree)?;
    let current_dirty = dirty.all();
    let admitted = union_paths(
        &path_names(&payload.tracked),
        &path_names(&payload.untracked),
    )
    .into_iter()
    .collect::<BTreeSet<_>>();
    let unexpected = current_dirty
        .into_iter()
        .filter(|path| !admitted.contains(*path))
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(recovery_refusal(
            reason,
            format!("unsealed dirty paths appeared: {}", unexpected.join(", ")),
            remedy,
        ))
    }
}
