//! A snapshot's staged partition: the released index tree, named by a message
//! trailer so the snapshot keeps the single parent every replay picks against.

use super::*;

use std::collections::BTreeSet;

const SUBJECT: &str = "Spice parked worktree snapshot (uncommitted work)";
/// Message trailer naming the index tree the released lane had staged.
const TRAILER: &str = "Spice-Index: ";
/// `update-index --index-info` removes a path given this mode and object.
const REMOVED: &str = "0 0000000000000000000000000000000000000000";

/// The snapshot message, naming the index tree when the lane has one.
///
/// The tree is written from the scratch copy of the lane index, so capture
/// never rewrites the lane's own index file.
pub(super) fn message(
    root: Worktree<'_>,
    indexed: bool,
    environment: &[(&str, &str)],
) -> Result<String, Error> {
    if !indexed {
        return Ok(SUBJECT.to_owned());
    }
    let tree = checked(
        run_with_environment(root, &["write-tree"], environment)?,
        "write snapshot index tree",
    )?;
    Ok(format!("{SUBJECT}\n\n{TRAILER}{tree}"))
}

/// Return a restored snapshot's staged partition to the index.
///
/// The no-commit pick stages the whole delta. The index restarts at HEAD and
/// takes each path the release had staged, so unstaged edits stay unstaged
/// and untracked files stay untracked. A staged path that HEAD changed since
/// the capture keeps its merged worktree content staged. A snapshot without
/// the trailer stays fully staged.
pub(super) fn restore(root: Worktree<'_>, commit: &str) -> Result<(), Error> {
    let message = required_read(
        root,
        &["show", "--no-patch", "--format=%B", commit],
        "read snapshot message",
    )?;
    let Some(tree) = message.lines().find_map(|line| line.strip_prefix(TRAILER)) else {
        return Ok(());
    };
    let base = format!("{commit}^1");
    let moved = raw(
        root,
        &["diff", "-z", "--name-only", "--no-renames", &base, "HEAD"],
        "read paths HEAD changed since the snapshot",
    )?;
    let moved = moved.split('\0').collect::<BTreeSet<_>>();
    let staged = raw(
        root,
        &[
            "diff",
            "-z",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            &base,
            tree,
        ],
        "read the snapshot staged paths",
    )?;
    let mut entries = String::new();
    let mut merged = String::new();
    let mut fields = staged.split('\0');
    while let (Some(header), Some(path)) = (fields.next(), fields.next()) {
        if moved.contains(path) {
            merged.push_str(path);
            merged.push('\0');
            continue;
        }
        let mut columns = header.trim_start_matches(':').split(' ');
        let (Some(_), Some(mode), Some(_), Some(object)) = (
            columns.next(),
            columns.next(),
            columns.next(),
            columns.next(),
        ) else {
            return Err(Error::fault(
                "git::snapshot-index-invalid",
                format!("snapshot staged path {path} has no raw diff header"),
            ));
        };
        let entry = if object.bytes().all(|byte| byte == b'0') {
            REMOVED.to_owned()
        } else {
            format!("{mode} {object}")
        };
        entries.push_str(&format!("{entry}\t{path}\0"));
    }
    checked(
        run(root, &["reset", "--quiet"])?,
        "unstage restored snapshot",
    )?;
    checked(
        run_with_input(
            root,
            &["update-index", "-z", "--index-info"],
            entries.as_bytes(),
        )?,
        "restage snapshot paths",
    )?;
    checked(
        run_with_input(
            root,
            &["update-index", "-z", "--add", "--remove", "--stdin"],
            merged.as_bytes(),
        )?,
        "restage merged snapshot paths",
    )?;
    Ok(())
}

fn raw(root: Worktree<'_>, args: &[&str], action: &str) -> Result<String, Error> {
    let output = run(root, args)?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(Error::fault("git::snapshot-failed", output.detail(action)))
    }
}
