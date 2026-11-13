//! Repository source cited by commit, path, and line range, read in process
//! from the Git object store that owns its bytes.

use std::path::Path;

use wire::rooms::{GitObjectId, GitSource, LineRange, RoomSourceUnavailable};

/// The Git repository at `git`, a worktree or Git directory, without
/// consulting ambient Git configuration.
pub(crate) fn open(git: &Path) -> Result<gix::Repository, RoomSourceUnavailable> {
    gix::open_opts(git, gix::open::Options::isolated())
        .map_err(|_| RoomSourceUnavailable::Unreadable {})
}

/// `path` at `commit`, linked as `lines` of the blob found there, when the
/// object store holds that commit, a file at that path, and those lines.
pub(crate) fn link(
    repository: &gix::Repository,
    commit: GitObjectId,
    path: &str,
    lines: Option<LineRange>,
) -> Result<GitSource, RoomSourceUnavailable> {
    let (blob, length) = blob_at(repository, &commit, path)?;
    if lines.is_some_and(|lines| lines.last() > length) {
        return Err(RoomSourceUnavailable::Missing {});
    }
    Ok(GitSource {
        commit,
        path: path.to_owned(),
        blob,
        lines,
    })
}

/// Whether `source` still names the blob and lines it linked.
pub(crate) fn check(
    repository: &gix::Repository,
    source: &GitSource,
) -> Result<(), RoomSourceUnavailable> {
    let linked = link(
        repository,
        source.commit.clone(),
        &source.path,
        source.lines,
    )?;
    if linked.blob == source.blob {
        Ok(())
    } else {
        Err(RoomSourceUnavailable::Changed {})
    }
}

/// The blob `commit`'s tree holds at `path`, and its line count.
fn blob_at(
    repository: &gix::Repository,
    commit: &GitObjectId,
    path: &str,
) -> Result<(GitObjectId, u32), RoomSourceUnavailable> {
    let id = gix::ObjectId::from_hex(commit.as_str().as_bytes())
        .map_err(|_| RoomSourceUnavailable::Missing {})?;
    let commit = repository
        .try_find_object(id)
        .map_err(unreadable)?
        .ok_or(RoomSourceUnavailable::Missing {})?
        .try_into_commit()
        .map_err(|_| RoomSourceUnavailable::Missing {})?;
    let entry = commit
        .tree()
        .map_err(unreadable)?
        .lookup_entry_by_path(path)
        .map_err(unreadable)?
        .filter(|entry| entry.mode().is_blob())
        .ok_or(RoomSourceUnavailable::Missing {})?;
    let blob = repository
        .find_blob(entry.object_id())
        .map_err(unreadable)?;
    let id = entry.object_id().to_string().parse().map_err(unreadable)?;
    Ok((id, line_count(&blob.data)))
}

fn unreadable<E>(_: E) -> RoomSourceUnavailable {
    RoomSourceUnavailable::Unreadable {}
}

/// Lines in `data`; a final line without a newline still counts.
fn line_count(data: &[u8]) -> u32 {
    let breaks = data.iter().filter(|byte| **byte == b'\n').count();
    let count = breaks + usize::from(data.last().is_some_and(|byte| *byte != b'\n'));
    u32::try_from(count).unwrap_or(u32::MAX)
}
