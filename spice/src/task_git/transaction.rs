use super::*;

pub(super) fn materialize_and_update(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    action: &str,
) -> Result<(), Error> {
    materialize_and_update_with_hooks(root, target, expected, action, true, None)
}

/// Materialize a task-owned synthesized commit and advance the branch by CAS.
///
/// Completion has already enforced possession, ancestry, footprint, and tree
/// admission. Its unreferenced commit cannot acquire repository-hook admission
/// before this update, so this one owned transition must not invoke hooks that
/// guard operator-authored reference changes.
pub(super) fn materialize_and_update_owned(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    action: &str,
) -> Result<(), Error> {
    materialize_and_update_with_hooks(root, target, expected, action, false, None)
}

pub(super) fn materialize_release(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    snapshot: &snapshot::Captured,
) -> Result<(), Error> {
    materialize_and_update_with_hooks(
        root,
        target,
        expected,
        "return dirty lane to published history",
        true,
        Some(snapshot),
    )
}

fn materialize_and_update_with_hooks(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    action: &str,
    hooks: bool,
    dirty: Option<&snapshot::Captured>,
) -> Result<(), Error> {
    let marker = transaction_path(root)?;
    write_pending(
        &marker,
        &Pending::Move {
            expected: expected.to_owned(),
            target: target.to_owned(),
            dirty: dirty.map(|snapshot| snapshot.rollback().clone()),
        },
    )?;
    if let Some(snapshot) = dirty {
        // Make saved untracked paths visible to read-tree's removal logic.
        let indexed = run(root, &["read-tree", snapshot.commit()])
            .map_err(|error| restore_move_failure(root, error))?;
        if !indexed.success() {
            let error = Error::fault(
                "git::snapshot-index-failed",
                indexed.detail("install release snapshot index"),
            );
            return Err(restore_move_failure(root, error));
        }
    }
    let materialized = run(root, &["read-tree", "--reset", "-u", target])
        .map_err(|error| restore_move_failure(root, error))?;
    if !materialized.success() {
        let detail = materialized.detail(action);
        recover_pending(root)?;
        return Err(Error::fault("git::materialize-failed", detail));
    }
    let reference = required_read(
        root,
        &["symbolic-ref", "--quiet", "HEAD"],
        "resolve HEAD ref",
    )
    .map_err(|error| restore_move_failure(root, error))?;
    let updated = update_ref(root, &reference, target, expected, hooks)
        .map_err(|error| restore_move_failure(root, error))?;
    let current = required_read(root, &["rev-parse", "HEAD"], "read updated HEAD")
        .map_err(|error| restore_move_failure(root, error))?;
    if updated.success() || current == target {
        remove_marker(&marker)?;
        return Ok(());
    }
    let detail = updated.detail(action);
    recover_pending(root)?;
    let combined = format!("{}\n{}", updated.stdout, updated.stderr).to_ascii_lowercase();
    let reason = if combined.contains("cannot lock ref")
        && combined.contains(" is at ")
        && combined.contains(" but expected ")
    {
        "git::ref-race"
    } else {
        "git::ref-update-refused"
    };
    Err(Error::refusal(
        reason,
        detail,
        concat!(
            "inspect the restored clean checkout and retry the ",
            "same task boundary",
        ),
    ))
}

pub(super) fn materialize_resolution(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    tree: &str,
    label: &str,
) -> Result<(), Error> {
    let marker = transaction_path(root)?;
    write_pending(
        &marker,
        &Pending::Resolution {
            expected: expected.to_owned(),
            target: target.to_owned(),
            tree: tree.to_owned(),
        },
    )?;
    let materialized = run(root, &["read-tree", "--reset", "-u", target])?;
    if !materialized.success() {
        let detail = materialized.detail("materialize admitted task resolution");
        recover_pending(root)?;
        return Err(Error::fault("git::materialize-failed", detail));
    }
    let reference = required_read(
        root,
        &["symbolic-ref", "--quiet", "HEAD"],
        "resolve resolution HEAD ref",
    )?;
    let updated = update_ref(root, &reference, target, expected, false)?;
    let current = required_read(root, &["rev-parse", "HEAD"], "read resolution HEAD")?;
    if updated.success() || current == target {
        clear_merge_state(root)?;
        remove_marker(&marker)?;
        return Ok(());
    }
    let detail = updated.detail("advance branch to admitted task resolution");
    recover_pending(root)?;
    Err(Error::refusal(
        "git::resolution-ref-race",
        detail,
        format!("inspect the retained resolution, then retry task completion for {label}"),
    ))
}

/// Advance the lane to an unpublished integration and retire its merge.
///
/// Only the branch and the merge controls move. The index and worktree keep the
/// sealed resolution, so whatever the integration held at the baseline stays
/// staged against it.
pub(super) fn materialize_integration(
    root: Worktree<'_>,
    target: &str,
    expected: &str,
    label: &str,
) -> Result<(), Error> {
    let marker = transaction_path(root)?;
    write_pending(
        &marker,
        &Pending::Integration {
            expected: expected.to_owned(),
            target: target.to_owned(),
        },
    )?;
    let reference = required_read(
        root,
        &["symbolic-ref", "--quiet", "HEAD"],
        "resolve integration HEAD ref",
    )?;
    let updated = update_ref(root, &reference, target, expected, false)?;
    let current = required_read(root, &["rev-parse", "HEAD"], "read integration HEAD")?;
    if updated.success() || current == target {
        clear_merge_state(root)?;
        return remove_marker(&marker);
    }
    let detail = updated.detail("advance branch to unpublished task integration");
    recover_pending(root)?;
    Err(Error::refusal(
        "git::resolution-ref-race",
        detail,
        format!("inspect the retained resolution, then retry task completion for {label}"),
    ))
}

fn update_ref(
    root: Worktree<'_>,
    reference: &str,
    target: &str,
    expected: &str,
    hooks: bool,
) -> Result<GitOutput, Error> {
    if hooks {
        run(root, &["update-ref", reference, target, expected])
    } else {
        run(
            root,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "update-ref",
                reference,
                target,
                expected,
            ],
        )
    }
}

fn restore_move_failure(root: Worktree<'_>, error: Error) -> Error {
    match recover_pending(root) {
        Ok(()) => error,
        Err(recovery) => recovery,
    }
}

pub(super) fn materialize_conflict(
    root: Worktree<'_>,
    merged_tree: &str,
    records: &[String],
    agent: &str,
    upstream: &str,
    message: &str,
) -> Result<(), Error> {
    let previous = snapshot_merge_state(root)?;
    let marker = transaction_path(root)?;
    write_pending(
        &marker,
        &Pending::Conflict {
            agent: agent.to_owned(),
            upstream: upstream.to_owned(),
            previous,
        },
    )?;
    let result = install_conflict(root, merged_tree, records, agent, upstream, message);
    if let Err(error) = result {
        recover_pending(root)?;
        return Err(error);
    }
    remove_marker(&marker)
}

fn install_conflict(
    root: Worktree<'_>,
    merged_tree: &str,
    records: &[String],
    agent: &str,
    upstream: &str,
    message: &str,
) -> Result<(), Error> {
    let parsed = records
        .iter()
        .map(|record| parse_conflict_record(record))
        .collect::<Result<Vec<_>, _>>()?;
    let materialized = run(root, &["read-tree", "--reset", "-u", merged_tree])?;
    if !materialized.success() {
        return Err(Error::fault(
            "git::conflict-materialize-failed",
            materialized.detail("materialize conflicted merge tree"),
        ));
    }
    let paths = parsed
        .iter()
        .map(|(_, path)| path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for path in paths {
        let removed = run(root, &["update-index", "--force-remove", "--", path])?;
        if !removed.success() {
            return Err(Error::fault(
                "git::conflict-index-failed",
                removed.detail(&format!("prepare conflict index for {path}")),
            ));
        }
    }
    let index = parsed
        .iter()
        .map(|(metadata, path)| format!("{metadata}\t{path}\0"))
        .collect::<String>();
    let staged = run_with_input(
        root,
        &["update-index", "-z", "--index-info"],
        index.as_bytes(),
    )?;
    if !staged.success() {
        return Err(Error::fault(
            "git::conflict-index-failed",
            staged.detail("install conflict index stages"),
        ));
    }
    write_git_state(root, "ORIG_HEAD", &format!("{agent}\n"))?;
    write_git_state(root, "MERGE_MODE", "no-ff\n")?;
    write_git_state(root, "MERGE_MSG", &format!("{message}\n"))?;
    write_git_state(root, "MERGE_HEAD", &format!("{upstream}\n"))
}

fn parse_conflict_record(record: &str) -> Result<(String, String), Error> {
    let (metadata, path) = record.split_once('\t').ok_or_else(|| {
        Error::fault(
            "git::conflict-record-invalid",
            format!("malformed record {record:?}"),
        )
    })?;
    if metadata.split_whitespace().count() != 3 || path.is_empty() {
        return Err(Error::fault(
            "git::conflict-record-invalid",
            format!("malformed record {record:?}"),
        ));
    }
    Ok((metadata.to_owned(), path.to_owned()))
}

pub(super) fn conflict_error(label: &str, upstream: &str) -> Error {
    Error::refusal(
        "git::recoverable-conflict",
        format!(
            concat!(
                "task {label} overlaps baseline {upstream}; Git is ",
                "paused with MERGE_HEAD and complete index stages",
            ),
            label = label,
            upstream = upstream
        ),
        format!(
            concat!(
                "resolve every conflicted path, git add the ",
                "resolution, then run `spice task done --validation ",
                "\"<evidence>\"` while MERGE_HEAD exists; do not create ",
                "the merge with git commit because task completion owns it ",
                "for {label}",
            ),
            label = label
        ),
    )
}

pub(super) fn require_no_open_merge(root: Worktree<'_>, label: &str) -> Result<(), Error> {
    if let Some(upstream) = try_read(root, &["rev-parse", "--verify", "MERGE_HEAD"])? {
        return Err(conflict_error(label, &upstream));
    }
    Ok(())
}

/// Refuse a dirty lane with recovery's generic advice.
pub(super) fn require_clean(root: Worktree<'_>, action: &str) -> Result<(), Error> {
    require_clean_or(
        root,
        action,
        "commit or clear every reported path before retrying",
    )
}

/// Refuse a dirty lane with advice for the caller's own route.
///
/// An open merge overrides `remedy`: its paths are a merge to resolve, and
/// committing it by hand would bypass the completion that owns it.
pub(super) fn require_clean_or(
    root: Worktree<'_>,
    action: &str,
    remedy: &str,
) -> Result<(), Error> {
    let paths = dirty_paths(root)?;
    if paths.is_empty() {
        return Ok(());
    }
    let merging = try_read(root, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])?.is_some();
    let remedy = if merging {
        concat!(
            "a Git merge is open: resolve and git add its paths without committing, ",
            "then retry task completion under the claim that owns it, ",
            "or run git merge --abort to drop it",
        )
    } else {
        remedy
    };
    Err(Error::refusal(
        "git::worktree-dirty",
        format!("{action} requires a clean worktree:\n{}", paths.rendered()),
        remedy,
    ))
}

pub(super) fn is_ancestor(
    root: Worktree<'_>,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, Error> {
    let output = run(root, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::fault(
            "git::ancestry-failed",
            output.detail("inspect commit ancestry"),
        )),
    }
}

pub(super) fn merge_first_parent(
    root: Worktree<'_>,
    commit: &str,
) -> Result<Option<String>, Error> {
    let line = required_read(
        root,
        &["rev-list", "--parents", "-n", "1", commit],
        "read merge parents",
    )?;
    let parents = line.split_whitespace().skip(1).collect::<Vec<_>>();
    Ok((parents.len() >= 2).then(|| parents[0].to_owned()))
}

pub(super) fn merge_second_parent(
    root: Worktree<'_>,
    commit: &str,
) -> Result<Option<String>, Error> {
    let line = required_read(
        root,
        &["rev-list", "--parents", "-n", "1", commit],
        "read merge parents",
    )?;
    let parents = line.split_whitespace().skip(1).collect::<Vec<_>>();
    Ok((parents.len() >= 2).then(|| parents[1].to_owned()))
}

pub(super) fn recovered_lane_head(
    root: Worktree<'_>,
    head: &str,
    meta: Metadata<'_>,
) -> Result<Option<String>, Error> {
    let Some(message) = try_read(root, &["show", "-s", "--format=%B", head])? else {
        return Ok(None);
    };
    if !landing_message_matches(&message, meta)? {
        return Ok(None);
    }
    let Some(candidate) = merge_second_parent(root, head)? else {
        return Ok(None);
    };
    Ok(is_ancestor(root, &candidate, head)?.then_some(candidate))
}

fn landing_message_matches(message: &str, meta: Metadata<'_>) -> Result<bool, Error> {
    let message = message.trim_end_matches(['\r', '\n']);
    let lines = message.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let Some(trailer_separator) = start.checked_sub(1) else {
        return Ok(false);
    };
    let narrative = &lines[..trailer_separator];
    let Some(body_separator) = narrative.iter().rposition(|line| line.trim().is_empty()) else {
        return Ok(false);
    };
    let body = &narrative[body_separator + 1..];
    let pitch = crate::commit_message::fold_prose(meta.pitch);
    if body.len() != pitch.len()
        || body
            .iter()
            .zip(&pitch)
            .any(|(line, folded)| line.strip_suffix('\r').unwrap_or(line) != folded)
    {
        return Ok(false);
    }
    let mut fields = BTreeMap::new();
    for line in &lines[start..] {
        let Some((name, value)) = line.strip_suffix('\r').unwrap_or(line).split_once(": ") else {
            return Ok(false);
        };
        if matches!(name, "Task-Phase" | "Task-Stamp" | "Task-Topic")
            && fields.insert(name, value).is_some()
        {
            return Err(Error::fault(
                "git::landing-evidence-invalid",
                format!("duplicate {name} trailer"),
            ));
        }
    }
    Ok(landing_fields_match(&fields, meta))
}

fn landing_fields_match(fields: &BTreeMap<&str, &str>, meta: Metadata<'_>) -> bool {
    landing_task_stamp(fields) == Some(task_stamp(meta.label))
        && trailer_matches(fields, "Task-Phase", meta.phase)
        && trailer_matches(fields, "Task-Topic", meta.topic)
}

fn landing_task_stamp<'a>(fields: &BTreeMap<&str, &'a str>) -> Option<&'a str> {
    fields.get("Task-Stamp").copied()
}

fn trailer_matches(fields: &BTreeMap<&str, &str>, name: &str, expected: &str) -> bool {
    if expected.is_empty() {
        !fields.contains_key(name)
    } else {
        fields.get(name) == Some(&expected)
    }
}

fn task_stamp(label: &str) -> &str {
    label.rsplit_once('-').map_or(label, |(_, stamp)| stamp)
}

pub(super) fn compose_message(meta: Metadata<'_>) -> Result<String, Error> {
    wire::pitch::admit(meta.pitch).map_err(|error| {
        Error::refusal(
            "git::task-pitch-unavailable",
            error.reason(),
            error.remedy(),
        )
    })?;
    let prefix = if meta.topic.is_empty() {
        String::new()
    } else {
        let topic = meta.topic.trim_start_matches('/');
        let scope = topic.split_once('/').map_or(topic, |(scope, _)| scope);
        format!("{scope}: ")
    };
    let phase = if meta.phase == "todo" || meta.phase.is_empty() {
        String::new()
    } else {
        format!(" ({})", meta.phase)
    };
    let title = if meta.title.trim().is_empty() {
        meta.label
    } else {
        meta.title.trim()
    };
    let mut trailers = vec![
        ("Task-Phase", meta.phase),
        ("Task-Stamp", task_stamp(meta.label)),
        ("Task-Topic", meta.topic),
    ];
    trailers.retain(|(_, value)| !value.is_empty());
    trailers.sort_unstable_by_key(|(name, _)| *name);
    let trailers = trailers
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "{prefix}{title} {}{phase}\n\n{}\n\n{trailers}",
        meta.label,
        crate::commit_message::fold_prose(meta.pitch).join("\n")
    ))
}

pub(super) fn recover_pending(root: Worktree<'_>) -> Result<(), Error> {
    let marker = transaction_path(root)?;
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => {
            return Err(filesystem_fault(
                "read task Git transaction",
                &marker,
                error,
            ));
        }
    };
    let pending: Pending = serde_json::from_slice(&bytes).map_err(|error| {
        Error::fault(
            "git::transaction-invalid",
            format!("{}: {error}", marker.display()),
        )
    })?;
    match pending {
        Pending::Move {
            expected,
            target,
            dirty,
        } => {
            recover_move(root, &expected, &target, dirty.as_ref())?;
        }
        Pending::Conflict {
            agent,
            upstream,
            previous,
        } => recover_conflict(root, &agent, &upstream, &previous)?,
        Pending::Resolution {
            expected,
            target,
            tree,
        } => recover_resolution(root, &expected, &target, &tree)?,
        Pending::Integration { expected, target } => {
            recover_integration(root, &expected, &target)?;
        }
        Pending::Snapshot { commit } => return snapshot::resume(root, &commit),
        Pending::SnapshotStarted { replay } => return replay.recover(root, &marker),
        Pending::SnapshotCleanup { cleanup } => return cleanup.finish(root, &marker),
    }
    remove_marker(&marker)
}

fn recover_resolution(
    root: Worktree<'_>,
    expected: &str,
    target: &str,
    tree: &str,
) -> Result<(), Error> {
    let current = required_read(root, &["rev-parse", "HEAD"], "recover task resolution")?;
    if current == target {
        let restored = run(root, &["read-tree", "--reset", "-u", target])?;
        if !restored.success() {
            return Err(Error::fault(
                "git::transaction-recovery-failed",
                restored.detail("restore committed task resolution"),
            ));
        }
        return clear_merge_state(root);
    }
    if current != expected {
        return Err(Error::refusal(
            "git::transaction-head-moved",
            format!("task resolution expected {expected} or {target}, but HEAD is {current}"),
            "inspect HEAD and the retained merge state before retrying task completion",
        ));
    }
    let restored = run(root, &["read-tree", "--reset", "-u", tree])?;
    if restored.success() {
        Ok(())
    } else {
        Err(Error::fault(
            "git::transaction-recovery-failed",
            restored.detail("restore staged task resolution"),
        ))
    }
}

fn recover_integration(root: Worktree<'_>, expected: &str, target: &str) -> Result<(), Error> {
    let current = required_read(root, &["rev-parse", "HEAD"], "recover task integration")?;
    if current == target {
        return clear_merge_state(root);
    }
    if current == expected {
        return Ok(());
    }
    Err(Error::refusal(
        "git::transaction-head-moved",
        format!("task integration expected {expected} or {target}, but HEAD is {current}"),
        "inspect HEAD and the retained merge state before retrying task completion",
    ))
}

fn recover_move(
    root: Worktree<'_>,
    expected: &str,
    target: &str,
    dirty: Option<&snapshot::Rollback>,
) -> Result<(), Error> {
    let current = required_read(root, &["rev-parse", "HEAD"], "recover task Git transaction")?;
    if current == expected
        && let Some(dirty) = dirty
    {
        return snapshot::restore(root, dirty);
    }
    let restore = if current == target { target } else { &current };
    let restored = run(root, &["read-tree", "--reset", "-u", restore])?;
    if !restored.success() {
        return Err(Error::fault(
            "git::transaction-recovery-failed",
            restored.detail("restore task boundary tree"),
        ));
    }
    if current != expected && current != target {
        return Err(Error::refusal(
            "git::transaction-head-moved",
            format!(
                concat!(
                    "task boundary expected {expected} or ",
                    "{target}, but HEAD is {current}",
                ),
                expected = expected,
                target = target,
                current = current,
            ),
            concat!(
                "inspect the restored current HEAD before ",
                "retrying the task boundary",
            ),
        ));
    }
    Ok(())
}

fn recover_conflict(
    root: Worktree<'_>,
    agent: &str,
    upstream: &str,
    previous: &BTreeMap<String, Option<String>>,
) -> Result<(), Error> {
    if try_read(root, &["rev-parse", "--verify", "MERGE_HEAD"])?.as_deref() == Some(upstream) {
        return Ok(());
    }
    let current = required_read(root, &["rev-parse", "HEAD"], "recover conflict transaction")?;
    if current != agent {
        if is_ancestor(root, agent, &current)? && is_ancestor(root, upstream, &current)? {
            return Ok(());
        }
        return Err(Error::refusal(
            "git::transaction-head-moved",
            format!(
                concat!(
                    "conflict recovery expected {agent}, but HEAD ",
                    "is {current}",
                ),
                agent = agent,
                current = current
            ),
            concat!(
                "inspect the current HEAD before retrying the ",
                "task boundary",
            ),
        ));
    }
    let restored = run(root, &["read-tree", "--reset", "-u", agent])?;
    if !restored.success() {
        return Err(Error::fault(
            "git::transaction-recovery-failed",
            restored.detail("restore pre-conflict tree"),
        ));
    }
    restore_merge_state(root, previous)
}

fn snapshot_merge_state(root: Worktree<'_>) -> Result<BTreeMap<String, Option<String>>, Error> {
    MERGE_STATE_FILES
        .iter()
        .map(|name| {
            let path = git_state_path(root, name)?;
            let content = match fs::read_to_string(&path) {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(filesystem_fault("snapshot Git state", &path, error));
                }
            };
            Ok(((*name).to_owned(), content))
        })
        .collect()
}

fn restore_merge_state(
    root: Worktree<'_>,
    previous: &BTreeMap<String, Option<String>>,
) -> Result<(), Error> {
    for name in MERGE_STATE_FILES {
        let path = git_state_path(root, name)?;
        match previous.get(*name).and_then(Option::as_ref) {
            Some(content) => fs::write(&path, content)
                .map_err(|error| filesystem_fault("restore Git state", &path, error))?,
            None => match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(filesystem_fault("clear partial Git state", &path, error));
                }
            },
        }
    }
    Ok(())
}

pub(super) fn clear_merge_state(root: Worktree<'_>) -> Result<(), Error> {
    for name in MERGE_STATE_FILES {
        let path = git_state_path(root, name)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(filesystem_fault("clear admitted merge state", &path, error)),
        }
    }
    Ok(())
}

fn write_git_state(root: Worktree<'_>, name: &str, content: &str) -> Result<(), Error> {
    let path = git_state_path(root, name)?;
    fs::write(&path, content).map_err(|error| filesystem_fault("write Git state", &path, error))
}

pub(super) fn git_state_path(root: Worktree<'_>, name: &str) -> Result<PathBuf, Error> {
    let written = required_read(
        root,
        &["rev-parse", "--git-path", name],
        "resolve Git state path",
    )?;
    let path = PathBuf::from(written);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// `index` as a `GIT_INDEX_FILE` value Git accepts: the worktree root is
/// canonical, so on Windows a joined state path is verbatim, which Git for
/// Windows cannot create a lock beside.
pub(super) fn git_index_spelling<'a>(
    index: &'a Path,
    code: &'static str,
) -> Result<std::borrow::Cow<'a, str>, Error> {
    crate::repository::git_spelling(index)
        .map_err(|reason| Error::fault(code, format!("{} {reason}", index.display())))
}

pub(super) fn transaction_path(root: Worktree<'_>) -> Result<PathBuf, Error> {
    git_state_path(root, TRANSACTION_FILE)
}

pub(super) fn write_pending(path: &Path, pending: &Pending) -> Result<(), Error> {
    let bytes = serde_json::to_vec(pending).map_err(|error| {
        Error::fault(
            "git::transaction-invalid",
            format!("encode transaction: {error}"),
        )
    })?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|error| filesystem_fault("stage task Git transaction", &temporary, error))?;
    fs::rename(&temporary, path)
        .map_err(|error| filesystem_fault("publish task Git transaction", path, error))
}

pub(super) fn remove_marker(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(filesystem_fault("retire task Git transaction", path, error)),
    }
}

pub(super) fn filesystem_fault(action: &str, path: &Path, error: std::io::Error) -> Error {
    Error::fault(
        "git::transaction-filesystem",
        format!("could not {action} at {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PITCH: &str = "Automatic landings need durable task rationale.";

    fn landing_metadata() -> Metadata<'static> {
        Metadata {
            label: "GIT-1kQb17Ytx",
            title: "Finish task landing trailer cleanup",
            pitch: PITCH,
            topic: "/task/git",
            phase: "todo",
            actor: "lane:00000000-0000-4000-8000-000000000123",
        }
    }

    #[test]
    fn landing_message_emits_one_canonical_trailer_block() {
        let message = compose_message(landing_metadata()).expect("admitted pitch");
        assert_eq!(
            message,
            concat!(
                "task: Finish task landing trailer cleanup GIT-1kQb17Ytx\n\n",
                "Automatic landings need durable task rationale.\n\n",
                "Task-Phase: todo\n",
                "Task-Stamp: 1kQb17Ytx\n",
                "Task-Topic: /task/git",
            )
        );
        assert!(!message.contains("00000000-0000-4000-8000-000000000123"));
    }

    #[test]
    fn a_long_landing_pitch_folds_within_the_wrap_limit_and_reads_back() {
        let pitch = concat!(
            "Design pass operations belong beside the task lifecycle they inspect and settle, ",
            "not in a two-command top-level namespace."
        );
        let meta = Metadata {
            pitch,
            ..landing_metadata()
        };
        let message = compose_message(meta).expect("admitted pitch");
        let lines = message.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 8, "the pitch paragraph spans two lines");
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= crate::commit_message::WRAP_LIMIT)
        );
        assert!(landing_message_matches(&message, meta).unwrap());
        let truncated = message.replace(" namespace.", "");
        assert!(!landing_message_matches(&truncated, meta).unwrap());
    }

    #[test]
    fn an_unbroken_admitted_pitch_splits_within_the_wrap_limit_and_reads_back() {
        for width in [101, wire::pitch::PITCH_CHARS] {
            let pitch = "é".repeat(width);
            wire::pitch::admit(&pitch).expect("admitted pitch");
            let meta = Metadata {
                pitch: &pitch,
                ..landing_metadata()
            };
            let message = compose_message(meta).expect("admitted pitch");
            let lines = message.lines().collect::<Vec<_>>();
            assert!(
                lines
                    .iter()
                    .all(|line| line.chars().count() <= crate::commit_message::WRAP_LIMIT)
            );
            assert_eq!(lines[2..lines.len() - 4].concat(), pitch);
            assert!(landing_message_matches(&message, meta).unwrap());
        }
    }

    #[test]
    fn landing_reader_accepts_only_the_canonical_terminal_stamp() {
        let meta = landing_metadata();
        let canonical = concat!(
            "task: Independently authored landing\n\n",
            "Automatic landings need durable task rationale.\n\n",
            "Task-Phase: todo\n",
            "Task-Stamp: 1kQb17Ytx\n",
            "Task-Topic: /task/git\n",
        );
        assert!(landing_message_matches(canonical, meta).unwrap());

        let mismatched = canonical.replace("Task-Stamp: 1kQb17Ytx", "Task-Stamp: 2kQb17Ytx");
        assert!(!landing_message_matches(&mismatched, meta).unwrap());
        let prefixed = canonical.replace("Task-Stamp: 1kQb17Ytx", "Task-Stamp: GIT-1kQb17Ytx");
        assert!(!landing_message_matches(&prefixed, meta).unwrap());
        let moved_pitch = canonical.replace(
            "Automatic landings need durable task rationale.",
            "Different rationale.",
        );
        assert!(!landing_message_matches(&moved_pitch, meta).unwrap());
    }

    #[test]
    fn landing_reader_uses_one_unambiguous_terminal_block() {
        let meta = landing_metadata();
        let body_only = concat!(
            "Task-Phase: todo\n",
            "Task-Stamp: 1kQb17Ytx\n",
            "Task-Topic: /task/git\n\n",
            "ordinary final paragraph",
        );
        assert!(!landing_message_matches(body_only, meta).unwrap());

        let duplicate = concat!(
            "task: Duplicate landing\n\n",
            "Automatic landings need durable task rationale.\n\n",
            "Task-Phase: todo\n",
            "Task-Stamp: 1kQb17Ytx\n",
            "Task-Stamp: 1kQb17Ytx\n",
            "Task-Topic: /task/git",
        );
        assert!(landing_message_matches(duplicate, meta).is_err());
    }

    #[test]
    fn landing_subject_uses_the_topic_root_as_its_scope() {
        let message = compose_message(Metadata {
            label: "GATE-1kQb17Ytx",
            title: "Refresh topic contracts",
            pitch: PITCH,
            topic: "/gate/dialect/steer",
            phase: "todo",
            actor: "lane:00000000-0000-4000-8000-000000000123",
        })
        .expect("admitted pitch");
        assert_eq!(
            message.lines().next(),
            Some("gate: Refresh topic contracts GATE-1kQb17Ytx")
        );

        let message = compose_message(Metadata {
            label: "GATE-1kQb17Ytx",
            title: "Refresh topic contracts",
            pitch: PITCH,
            topic: "/gate",
            phase: "todo",
            actor: "lane:00000000-0000-4000-8000-000000000123",
        })
        .expect("admitted pitch");
        assert_eq!(
            message.lines().next(),
            Some("gate: Refresh topic contracts GATE-1kQb17Ytx")
        );
    }

    #[test]
    fn landing_message_refuses_an_unavailable_pitch() {
        let mut meta = landing_metadata();
        meta.pitch = "";
        let error = compose_message(meta).expect_err("missing pitch must refuse");
        assert_eq!(error.reason(), "git::task-pitch-unavailable");
        assert_eq!(error.class(), ErrorClass::Refusal);
    }
}
