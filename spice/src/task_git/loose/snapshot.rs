use std::hash::{DefaultHasher, Hash, Hasher};

use gix::bstr::{BStr, ByteSlice as _};

use super::*;

pub(super) fn read(root: &Path) -> Result<LooseWork, Error> {
    let repository = open(root)?;
    let head = repository
        .head()
        .map_err(|error| fault("git::head-unreadable", "read HEAD", error))?;
    let branch = head
        .referent_name()
        .map(|name| {
            name.shorten()
                .to_str()
                .map(str::to_owned)
                .map_err(|error| Error::fault("git::branch-invalid", error.to_string()))
        })
        .transpose()?;
    let head = head
        .into_peeled_id()
        .map_err(|error| fault("git::head-unreadable", "peel HEAD", error))?
        .detach();
    let head_commit = repository
        .find_commit(head)
        .map_err(|error| fault("git::head-unreadable", "read HEAD commit", error))?;
    let head_tree = head_commit
        .tree_id()
        .map_err(|error| fault("git::head-unreadable", "read HEAD tree", error))?
        .detach();
    let (dirty, dirty_fingerprint) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dirty_state(&repository, root, head_tree)
    }))
    .map_err(|_| {
        Error::fault(
            "git::index-invalid",
            "gix could not decode the worktree index",
        )
    })??;
    let baseline = branch
        .as_deref()
        .map(|branch| integration_baseline(&repository, branch))
        .transpose()?
        .flatten();
    Ok(LooseWork {
        head: head.to_string(),
        dirty,
        dirty_fingerprint,
        unlanded: unlanded(&repository, head, baseline.as_deref())?,
        operation: operation(&repository),
    })
}

fn open(root: &Path) -> Result<gix::Repository, Error> {
    let mut permissions = gix::open::Permissions::all();
    // The resident process can itself have Spice's child-process pairing
    // shadow. Native inspection ignores command-scope config just as
    // `git_shadow::declared_pairing()` strips it before asking Git.
    permissions.config.env = false;
    gix::open_opts(root, gix::open::Options::default().permissions(permissions))
        .map_err(|error| fault("git::repository-unreadable", "open worktree", error))
}

fn dirty_state(
    repository: &gix::Repository,
    root: &Path,
    head_tree: gix::ObjectId,
) -> Result<(Vec<String>, u64), Error> {
    let mut staged = Vec::new();
    let mut worktree = Vec::new();
    let status = repository
        .status(gix::progress::Discard)
        .map_err(|error| fault("git::status-unreadable", "prepare worktree status", error))?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .head_tree(head_tree)
        .into_iter(Vec::<gix::bstr::BString>::new())
        .map_err(|error| fault("git::status-unreadable", "read worktree status", error))?;
    for item in status {
        match item
            .map_err(|error| fault("git::status-unreadable", "read worktree status", error))?
        {
            gix::status::Item::TreeIndex(change) => {
                let path = utf8_path(change.location())?;
                staged.push((path, staged_fingerprint(&change)));
            }
            gix::status::Item::IndexWorktree(item) => {
                let Some(summary) = item.summary() else {
                    continue;
                };
                let path = utf8_path(item.rela_path())?;
                let mut state = DefaultHasher::new();
                std::mem::discriminant(&summary).hash(&mut state);
                worktree.push((
                    path,
                    state.finish(),
                    matches!(summary, gix::status::index_worktree::iter::Summary::Added),
                ));
            }
        }
    }
    staged.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    worktree.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    let mut hasher = DefaultHasher::new();
    for item in &staged {
        item.hash(&mut hasher);
    }
    for (path, state, untracked) in &worktree {
        (path, state, untracked).hash(&mut hasher);
        hash_worktree_path(root, path, &mut hasher)?;
    }
    let mut dirty = staged
        .iter()
        .map(|(path, _)| path.clone())
        .chain(worktree.iter().map(|(path, _, _)| path.clone()))
        .collect::<Vec<_>>();
    dirty.sort();
    dirty.dedup();
    Ok((dirty, hasher.finish()))
}

fn staged_fingerprint(change: &gix::diff::index::Change) -> u64 {
    use gix::diff::index::Change;

    let mut hasher = DefaultHasher::new();
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => (b'a', location, entry_mode.bits(), id).hash(&mut hasher),
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => (b'd', location, entry_mode.bits(), id).hash(&mut hasher),
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => (
            b'm',
            location,
            previous_entry_mode.bits(),
            previous_id,
            entry_mode.bits(),
            id,
        )
            .hash(&mut hasher),
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            location,
            entry_mode,
            id,
            copy,
            ..
        } => (
            b'r',
            source_location,
            source_entry_mode.bits(),
            source_id,
            location,
            entry_mode.bits(),
            id,
            copy,
        )
            .hash(&mut hasher),
    }
    hasher.finish()
}

fn integration_baseline(
    repository: &gix::Repository,
    branch: &str,
) -> Result<Option<String>, Error> {
    let config = repository.config_snapshot();
    let remote = config_value(&config, &format!("branch.{branch}.remote"))?;
    let merge = config_value(&config, &format!("branch.{branch}.merge"))?;
    let pairing = match (remote, merge) {
        (None, None) => None,
        (Some(remote), Some(merge)) => Some((remote, merge)),
        (remote, _) => {
            let (present, missing) = if remote.is_some() {
                ("remote", "merge")
            } else {
                ("merge", "remote")
            };
            return Err(Error::fault(
                "git::pairing-invalid",
                format!("branch {branch} declares a {present} but no {missing}"),
            ));
        }
    };
    let Some((remote, merge)) = pairing else {
        return Ok(has_remote(repository, "origin").then(|| "refs/remotes/origin/HEAD".to_owned()));
    };
    let branch = merge.strip_prefix("refs/heads/").ok_or_else(|| {
        Error::fault(
            "git::pairing-invalid",
            format!("branch.{branch}.merge is {merge}, which names no branch"),
        )
    })?;
    if remote == "." {
        return Ok(Some(format!("refs/heads/{branch}")));
    }
    if !has_remote(repository, &remote) {
        return Err(Error::fault(
            "git::remote-invalid",
            format!("branch tracks remote {remote}, which this repository does not define"),
        ));
    }
    Ok(Some(format!("refs/remotes/{remote}/{branch}")))
}

fn config_value(config: &gix::config::Snapshot<'_>, key: &str) -> Result<Option<String>, Error> {
    config
        .string(key)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|error| Error::fault("git::pairing-invalid", error.to_string()))
        })
        .transpose()
}

fn has_remote(repository: &gix::Repository, name: &str) -> bool {
    repository
        .remote_names()
        .iter()
        .any(|candidate| candidate.as_bstr() == name.as_bytes().as_bstr())
}

fn unlanded(
    repository: &gix::Repository,
    head: gix::ObjectId,
    baseline: Option<&str>,
) -> Result<Unlanded, Error> {
    let Some(baseline) = baseline else {
        return Ok(Unlanded::default());
    };
    let baseline = repository
        .rev_parse_single(baseline.as_bytes().as_bstr())
        .map_err(|error| fault("git::baseline-invalid", "read integration baseline", error))?
        .detach();
    let walk = repository
        .rev_walk([head])
        .with_hidden([baseline])
        .all()
        .map_err(|error| fault("git::ahead-invalid", "count unlanded commits", error))?;
    let mut total = 0_usize;
    for commit in walk {
        commit.map_err(|error| fault("git::ahead-invalid", "count unlanded commits", error))?;
        total = total.saturating_add(1);
    }
    Ok(Unlanded {
        total,
        rows: Vec::new(),
        baseline: Some(baseline.to_string()),
    })
}

fn operation(repository: &gix::Repository) -> Option<&'static str> {
    use gix::state::InProgress;

    match repository.state()? {
        InProgress::Merge => Some("merge"),
        InProgress::Rebase
        | InProgress::RebaseInteractive
        | InProgress::ApplyMailbox
        | InProgress::ApplyMailboxRebase => Some("rebase"),
        InProgress::CherryPick | InProgress::CherryPickSequence => Some("cherry-pick"),
        InProgress::Revert | InProgress::RevertSequence => Some("revert"),
        InProgress::Bisect => None,
    }
}

fn utf8_path(path: &BStr) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .map_err(|error| Error::fault("git::path-list-invalid", error.to_string()))
}

fn fault(reason: &'static str, action: &'static str, error: impl std::fmt::Display) -> Error {
    Error::fault(reason, format!("{action}: {error}"))
}
