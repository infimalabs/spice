//! Replaying a released task's parked commits onto a claimant's lane.
//!
//! Parked commits are cherry-picked in memory, oldest first, onto whatever HEAD
//! the claim fast-forwarded to, and the lane then moves once through the
//! ordinary materialize-then-CAS update. The first conflict hands the rest of
//! the replay to Git's own cherry-pick sequencer in the worktree, so the
//! claimant finishes it with the commands Git already documents.

use super::*;

use std::collections::BTreeSet;

use wire::intent::{ClaimTransplant, TransplantConflict, TransplantedCommit};

const CONFLICT_REMEDY: &str = concat!(
    "resolve the conflicted paths and git add them, then run ",
    "`git cherry-pick --continue` to replay the remaining parked commits; ",
    "`git cherry-pick --skip` drops this commit and `git cherry-pick --abort` ",
    "drops it and every remaining one",
);

pub(super) struct Replayed {
    pub(super) head: String,
    pub(super) report: ClaimTransplant,
}

/// Replay `commits` onto `onto`, stopping in the worktree at the first conflict.
pub(super) fn replay(
    root: Worktree<'_>,
    commits: &[String],
    snapshot: Option<&str>,
    onto: &str,
) -> Result<Replayed, Error> {
    let mut head = onto.to_owned();
    let mut report = ClaimTransplant {
        onto: onto.to_owned(),
        applied: Vec::new(),
        present: Vec::new(),
        missing: Vec::new(),
        conflict: None,
        snapshot: snapshot.map(str::to_owned),
    };
    for (index, commit) in commits.iter().enumerate() {
        require_object(root, commit)?;
        if is_ancestor(root, commit, &head)? {
            report.present.push(commit.clone());
            continue;
        }
        // The first parent is the commit's own change for a merge too, which
        // is what a lane that merged upstream into its work parked.
        let parent = format!("{commit}^1");
        let picked = merge_tree_from(root, Some(&parent), &head, commit)?;
        if !picked.conflicts.is_empty() {
            advance(root, &head, onto)?;
            let mut remaining = Vec::new();
            for later in &commits[index + 1..] {
                require_object(root, later)?;
                remaining.push(later.clone());
            }
            pause(root, commit, &remaining)?;
            report.conflict = Some(TransplantConflict {
                commit: commit.clone(),
                paths: conflicted_paths(&picked.conflicts),
                remaining,
                remedy: CONFLICT_REMEDY.into(),
            });
            if let Some(snapshot) = snapshot {
                snapshot::defer(root, snapshot)?;
                let conflict = report.conflict.as_mut().expect("replay just paused");
                conflict.remedy = format!(
                    "{}; after the commit replay finishes, retry task completion to restore \
                     pending snapshot {snapshot} as uncommitted work",
                    conflict.remedy,
                )
                .into();
            }
            return Ok(Replayed { head, report });
        }
        let tree = required_read(
            root,
            &["rev-parse", &format!("{head}^{{tree}}")],
            "read lane tree",
        )?;
        if picked.tree == tree {
            report.present.push(commit.clone());
            continue;
        }
        let created = recommit(root, &picked.tree, &head, commit)?;
        report.applied.push(TransplantedCommit {
            parked: commit.clone(),
            commit: created.clone(),
        });
        head = created;
    }
    advance(root, &head, onto)?;
    if let Some(snapshot) = snapshot {
        report.conflict = snapshot::apply(root, snapshot)?;
    }
    Ok(Replayed { head, report })
}

/// Drop any paused replay and return the lane to `before`.
pub(super) fn abandon(root: Worktree<'_>, before: &str, action: &str) -> Result<(), Error> {
    let marker = transaction_path(root)?;
    let deferred = match fs::read(&marker) {
        Ok(bytes) => {
            let pending: Pending = serde_json::from_slice(&bytes)
                .map_err(|error| Error::fault("git::transaction-invalid", error.to_string()))?;
            match pending {
                Pending::Snapshot { .. } => true,
                Pending::SnapshotStarted { .. } | Pending::SnapshotCleanup { .. } => {
                    return Err(Error::refusal(
                        "git::snapshot-recovery-required",
                        "snapshot restoration has pending recovery; its content, HEAD and controls were retained instead of rolling them back",
                        "resolve the Git failure, then run `spice task capture HANDLE` for the same task to settle recovery and adopt the retained work; resolve and git add any conflicts first",
                    ));
                }
                _ => {
                    return Err(Error::refusal(
                        "git::transaction-state-moved",
                        "claim rollback found a different pending Git transaction",
                        "recover the recorded transaction before retrying the claim; no rollback was performed",
                    ));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(filesystem_fault("read replay marker", &marker, error)),
    };
    if try_read(
        root,
        &["rev-parse", "--verify", "--quiet", "CHERRY_PICK_HEAD"],
    )?
    .is_some()
    {
        let aborted = run(root, &["cherry-pick", "--abort"])?;
        if !aborted.success() {
            return Err(Error::fault(
                "git::transplant-abort-failed",
                aborted.detail("abort the paused parked commit replay"),
            ));
        }
    }
    if deferred {
        remove_marker(&marker)?;
    }
    let current = required_read(root, &["rev-parse", "HEAD"], "read replayed lane HEAD")?;
    if current != before {
        materialize_and_update(root, before, &current, action)?;
    }
    let cleared = run(root, &["read-tree", "--reset", "-u", before])?;
    if !cleared.success() {
        return Err(Error::fault(
            "git::transplant-rollback-failed",
            cleared.detail(action),
        ));
    }
    Ok(())
}

pub(super) fn require_object(root: Worktree<'_>, commit: &str) -> Result<(), Error> {
    if run(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?.success() {
        Ok(())
    } else {
        Err(Error::refusal(
            "git::parked-object-missing",
            format!("Git no longer holds parked object {commit}"),
            "recover the exact parked object before claiming this task; no work was replayed",
        ))
    }
}

fn advance(root: Worktree<'_>, head: &str, onto: &str) -> Result<(), Error> {
    if head == onto {
        return Ok(());
    }
    materialize_and_update(root, head, onto, "transplant parked task commits")
}

/// Recreate one parked commit on `parent`, keeping its author and message.
///
/// The committer date is the original's too, so a claim retried over the same
/// lane HEAD names the same commits.
fn recommit(root: Worktree<'_>, tree: &str, parent: &str, commit: &str) -> Result<String, Error> {
    let identity = required_read(
        root,
        &[
            "show",
            "-s",
            "--format=%an%x00%ae%x00%aI%x00%cI%x00%B",
            commit,
        ],
        "read parked commit identity",
    )?;
    let fields = identity.splitn(5, '\0').collect::<Vec<_>>();
    let [name, email, authored, committed, message] = fields[..] else {
        return Err(Error::fault(
            "git::parked-commit-invalid",
            format!("parked commit {commit} returned malformed identity {identity:?}"),
        ));
    };
    let created = run_with_environment(
        root,
        &["commit-tree", tree, "-p", parent, "-m", message],
        &[
            ("GIT_AUTHOR_NAME", name),
            ("GIT_AUTHOR_EMAIL", email),
            ("GIT_AUTHOR_DATE", authored),
            ("GIT_COMMITTER_DATE", committed),
        ],
    )?;
    let id = created.stdout.trim();
    if !created.success() || id.is_empty() {
        return Err(Error::fault(
            "git::transplant-commit-failed",
            created.detail(&format!("replay parked commit {commit}")),
        ));
    }
    Ok(id.to_owned())
}

/// Leave Git's cherry-pick of `commit`, then `remaining`, paused on its conflict.
fn pause(root: Worktree<'_>, commit: &str, remaining: &[String]) -> Result<(), Error> {
    let mut args = vec!["cherry-pick", "-m", "1", "--empty=drop", commit];
    args.extend(remaining.iter().map(String::as_str));
    let picked = run(root, &args)?;
    let paused = try_read(
        root,
        &["rev-parse", "--verify", "--quiet", "CHERRY_PICK_HEAD"],
    )?;
    if picked.success() || paused.is_none() {
        return Err(Error::fault(
            "git::transplant-pause-failed",
            picked.detail(&format!("pause the conflicted replay of {commit}")),
        ));
    }
    Ok(())
}

fn conflicted_paths(records: &[String]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| record.split_once('\t'))
        .map(|(_, path)| path.to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
