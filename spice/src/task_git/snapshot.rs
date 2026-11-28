//! Worktree snapshots are commit objects, never references or admitted lane commits.

use super::*;

use std::sync::atomic::{AtomicU64, Ordering};

use wire::intent::TransplantConflict;

mod index;
mod started;
pub(super) use started::Started;

/// Original index bytes are kept separately only until release admission settles.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rollback {
    pub(super) commit: String,
    index: Option<PathBuf>,
}

#[derive(Debug)]
pub(super) struct Captured {
    rollback: Rollback,
    /// The worktree's transaction marker, resolved when the snapshot was taken.
    marker: Option<PathBuf>,
}

impl Captured {
    pub(super) fn commit(&self) -> &str {
        &self.rollback.commit
    }

    pub(super) fn rollback(&self) -> &Rollback {
        &self.rollback
    }

    pub(super) fn restore(&self, root: Worktree<'_>) -> Result<(), Error> {
        restore(root, &self.rollback)
    }
}

impl Drop for Captured {
    fn drop(&mut self) {
        // A failed recovery still owns its backup through the durable marker.
        if self.marker.as_ref().is_some_and(|path| path.exists()) {
            return;
        }
        if let Some(index) = &self.rollback.index {
            let _ = fs::remove_file(index);
        }
    }
}

pub(super) fn require_releasable(root: Worktree<'_>) -> Result<(), Error> {
    require_no_open_merge(root, "task release")?;
    require_no_replay(root)?;
    let unresolved = required_read(
        root,
        &[
            "--no-optional-locks",
            "diff",
            "--name-only",
            "--diff-filter=U",
        ],
        "inspect unresolved snapshot paths",
    )?;
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(Error::refusal(
            "git::worktree-dirty",
            format!("task release has unresolved paths:\n{unresolved}"),
            "resolve and stage every conflicted path before releasing the task",
        ))
    }
}

fn require_no_replay(root: Worktree<'_>) -> Result<(), Error> {
    let pick = try_read(
        root,
        &["rev-parse", "--verify", "--quiet", "CHERRY_PICK_HEAD"],
    )?;
    if pick.is_some() || git_state_path(root, "sequencer")?.exists() {
        Err(Error::refusal(
            "git::parked-replay-paused",
            "parked commits are still in Git's cherry-pick sequence",
            "resolve the paused replay and run git cherry-pick --continue, --skip, or --abort first",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn capture(root: Worktree<'_>, head: &str) -> Result<Option<Captured>, Error> {
    if dirty_paths(root)?.is_empty() {
        return Ok(None);
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "spice-task-snapshot-index-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    );
    let build = git_state_path(root, &name)?;
    let saved = build.with_extension("original");
    let index = git_state_path(root, "index")?;
    let original = match fs::copy(&index, &saved) {
        Ok(_) => Some(saved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(filesystem_fault("save release index", &saved, error)),
    };
    let created = (|| {
        if let Some(original) = &original {
            fs::copy(original, &build)
                .map_err(|error| filesystem_fault("copy snapshot index", &build, error))?;
        }
        let path = transaction::git_index_spelling(&build, "git::snapshot-index-path")?;
        let environment = [("GIT_INDEX_FILE", path.as_ref())];
        if original.is_none() {
            checked(
                run_with_environment(root, &["read-tree", head], &environment)?,
                "initialize snapshot index",
            )?;
        }
        let message = index::message(root, original.is_some(), &environment)?;
        checked(
            run_with_environment(root, &["add", "-A", "--", "."], &environment)?,
            "capture dirty worktree",
        )?;
        let tree = checked(
            run_with_environment(root, &["write-tree"], &environment)?,
            "write snapshot tree",
        )?;
        checked(
            run(root, &["commit-tree", &tree, "-p", head, "-m", &message])?,
            "write dangling worktree snapshot",
        )
    })();
    let _ = fs::remove_file(&build);
    match created {
        Ok(commit) => Ok(Some(Captured {
            rollback: Rollback {
                commit,
                index: original,
            },
            marker: transaction_path(root).ok(),
        })),
        Err(error) => {
            if let Some(original) = original {
                let _ = fs::remove_file(original);
            }
            Err(error)
        }
    }
}

fn checked(output: GitOutput, action: &str) -> Result<String, Error> {
    if output.success() {
        Ok(output.stdout.trim().to_owned())
    } else {
        Err(Error::fault("git::snapshot-failed", output.detail(action)))
    }
}

pub(super) fn restore(root: Worktree<'_>, rollback: &Rollback) -> Result<(), Error> {
    checked(
        run(root, &["read-tree", "--reset", "-u", &rollback.commit])?,
        "restore dirty release worktree",
    )?;
    let index = git_state_path(root, "index")?;
    if let Some(saved) = &rollback.index {
        fs::copy(saved, &index)
            .map_err(|error| filesystem_fault("restore original release index", &index, error))?;
    } else {
        fs::remove_file(&index)
            .map_err(|error| filesystem_fault("restore absent release index", &index, error))?;
    }
    Ok(())
}

/// Apply only the snapshot's delta; neither a successful pick nor a conflict moves HEAD.
pub(super) fn apply(root: Worktree<'_>, commit: &str) -> Result<Option<TransplantConflict>, Error> {
    require_releasable(root)?;
    require_foreign_controls_absent(root)?;
    if Controls::read(root)? != Controls::default() {
        return Err(snapshot_state_refusal(
            "snapshot restoration found pre-existing Git operation controls",
        ));
    }
    transplant::require_object(root, commit)?;
    let head = required_read(root, &["rev-parse", "HEAD"], "read snapshot replay HEAD")?;
    // Persist the clean starting authority before Git can install any content.
    Started::begin(root, commit, &head)?;
    let picked = run(root, &["cherry-pick", "--no-commit", commit])?;
    let paths = if picked.success() {
        String::new()
    } else {
        required_read(
            root,
            &["diff", "--name-only", "--diff-filter=U"],
            "read snapshot conflicts",
        )?
    };
    let pending = Pending::SnapshotCleanup {
        cleanup: Cleanup {
            commit: commit.to_owned(),
            head,
            controls: Controls::read(root)?,
            failure: (!picked.success() && paths.is_empty())
                .then(|| picked.detail("restore uncommitted task snapshot")),
        },
    };
    let marker = transaction_path(root)?;
    write_pending(&marker, &pending)?;
    let Pending::SnapshotCleanup { cleanup } = pending else {
        unreachable!("constructed snapshot cleanup")
    };
    cleanup.finish(root, &marker)?;
    if picked.success() {
        index::restore(root, commit)?;
        return Ok(None);
    }
    Ok(Some(TransplantConflict {
        commit: commit.to_owned(),
        paths: paths.lines().map(str::to_owned).collect(),
        remaining: Vec::new(),
        remedy: "resolve and git add the snapshot conflicts; commit any remaining delta through the ordinary pre-commit gate, or retry task completion if the resolved tree is already HEAD; never commit the snapshot object itself".into(),
    }))
}

/// A real-commit conflict precedes the dirty delta; retain it until that replay settles.
pub(super) fn defer(root: Worktree<'_>, commit: &str) -> Result<(), Error> {
    write_pending(
        &transaction_path(root)?,
        &Pending::Snapshot {
            commit: commit.to_owned(),
        },
    )
}

pub(super) fn resume(root: Worktree<'_>, commit: &str) -> Result<(), Error> {
    require_no_replay(root)?;
    require_no_open_merge(root, "restore task snapshot")?;
    require_clean_or(
        root,
        "restore task snapshot after parked replay",
        &format!(
            concat!(
                "commit the reported paths through the pre-commit gate so pending ",
                "snapshot {commit} can restore on top of them, then retry",
            ),
            commit = commit
        ),
    )?;
    let conflict = apply(root, commit)?;
    if let Some(conflict) = conflict {
        Err(Error::refusal(
            "git::snapshot-conflict",
            format!(
                "snapshot {} has unresolved paths: {}",
                commit,
                conflict.paths.join(", ")
            ),
            conflict.remedy,
        ))
    } else {
        Ok(())
    }
}

/// The owned no-commit operation has returned; recovery only retires its
/// controls, never replays its delta or rewrites the claimant's index.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cleanup {
    commit: String,
    head: String,
    controls: Controls,
    failure: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Controls {
    message: Option<String>,
    auto_merge: Option<String>,
}

impl Controls {
    fn read(root: Worktree<'_>) -> Result<Self, Error> {
        Ok(Self {
            message: read_control(root, "MERGE_MSG")?,
            auto_merge: read_reference(root, "AUTO_MERGE")?,
        })
    }

    fn admits_cleanup(&self, current: &Self) -> bool {
        // A quit interrupted after deleting one control can resume. A newly
        // introduced or replaced control is not owned by this handoff.
        (current.message.is_none() || current.message == self.message)
            && (current.auto_merge.is_none() || current.auto_merge == self.auto_merge)
    }
}

impl Cleanup {
    pub(super) fn installed(&self) -> bool {
        self.failure.is_none()
    }

    pub(super) fn settle_controls(&self, root: Worktree<'_>) -> Result<(), Error> {
        let head = required_read(root, &["rev-parse", "HEAD"], "check snapshot cleanup HEAD")?;
        if head != self.head {
            return Err(snapshot_state_refusal(format!(
                "snapshot {} cleanup expected HEAD {}, found {head}",
                self.commit, self.head
            )));
        }
        require_foreign_controls_absent(root)?;
        let controls = Controls::read(root)?;
        if !self.controls.admits_cleanup(&controls) {
            return Err(snapshot_state_refusal(format!(
                "snapshot {} cleanup found changed Git operation controls",
                self.commit
            )));
        }
        if controls != Controls::default() {
            // Unlike reset, quit leaves staged content and unresolved index
            // stages intact. Only the sealed synthetic operation may be quit.
            checked(
                run(root, &["merge", "--quit"])?,
                "finish owned snapshot replay controls; retry the task boundary to recover",
            )?;
        }
        if Controls::read(root)? != Controls::default() {
            return Err(snapshot_state_refusal(format!(
                "snapshot {} still has operation controls after cleanup",
                self.commit
            )));
        }
        Ok(())
    }

    pub(super) fn finish(&self, root: Worktree<'_>, marker: &Path) -> Result<(), Error> {
        self.settle_controls(root)?;
        if let Some(failure) = &self.failure {
            // Cleanup is complete, but the pick was not. Keep its input for a
            // clean-worktree retry rather than claiming the snapshot landed.
            defer(root, &self.commit)?;
            return Err(Error::refusal(
                "git::snapshot-restore-failed",
                failure.clone(),
                "preserve claimant content, resolve the Git failure, and retry the task boundary; the snapshot remains pending",
            ));
        }
        remove_marker(marker)
    }
}

fn read_control(root: Worktree<'_>, name: &str) -> Result<Option<String>, Error> {
    let path = git_state_path(root, name)?;
    read_control_path(&path)
}

fn read_control_path(path: &Path) -> Result<Option<String>, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fs::read_to_string(path)
            .map(Some)
            .map_err(|error| filesystem_fault("read snapshot operation control", path, error)),
        Ok(_) => Err(snapshot_state_refusal(format!(
            "{} is not a regular Git operation control",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(filesystem_fault(
            "inspect snapshot operation control",
            path,
            error,
        )),
    }
}

fn read_reference(root: Worktree<'_>, name: &str) -> Result<Option<String>, Error> {
    let loose = read_control(root, name)?;
    let value = run(root, &["rev-parse", "--verify", "--quiet", name])?;
    if value.success() {
        let symbolic = run(root, &["symbolic-ref", "--quiet", name])?;
        if symbolic.success()
            || loose
                .as_ref()
                .is_some_and(|text| text.trim() != value.stdout.trim())
        {
            return Err(snapshot_state_refusal(format!(
                "snapshot reference {name} is not a direct object identity"
            )));
        }
        if symbolic.status != 1 {
            return Err(Error::fault(
                "git::command-failed",
                symbolic.detail("inspect snapshot reference"),
            ));
        }
        return Ok(Some(value.stdout));
    }
    if value.status == 1 && loose.is_none() {
        return Ok(None);
    }
    Err(snapshot_state_refusal(
        value.detail("read snapshot reference"),
    ))
}

fn require_foreign_controls_absent(root: Worktree<'_>) -> Result<(), Error> {
    for name in [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        if read_reference(root, name)?.is_some() {
            return Err(snapshot_state_refusal(format!(
                "snapshot replay cannot own Git operation control {name}"
            )));
        }
    }
    for name in ["MERGE_MODE", "sequencer", "rebase-merge", "rebase-apply"] {
        let path = git_state_path(root, name)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(snapshot_state_refusal(format!(
                    "snapshot replay cannot own Git operation control {name}"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(filesystem_fault(
                    "inspect foreign Git operation control",
                    &path,
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn snapshot_state_refusal(message: impl Into<String>) -> Error {
    Error::refusal(
        "git::snapshot-state-moved",
        message,
        "preserve the pending snapshot and claimant content; inspect the changed Git operation before retrying the task boundary",
    )
}

#[cfg(test)]
mod tests;
