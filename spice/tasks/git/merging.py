"""Merge materialization and the prose an agent gets when integration refuses.

Two concerns that always travel together: how a task's work is combined with
the baseline (synthesized merges, conflict trees written into the working tree,
HEAD updated under lock races), and what the agent is told when that cannot
finish cleanly (conflict refusals, out-of-scope landings, publish races, the
recovery recipes for each).

This is the middle of `spice.tasks.git`: it stands on `plumbing` and is driven by
`boundaries`. The division against `boundaries` is *when* versus *how* — that
module owns the three moments Git is touched, this one owns carrying a single
integration through and reporting it. The refusal text sits here rather than
beside its caller because a recovery recipe is only writable next to the
machinery whose failure it describes.

Refusals here follow the tree-wide repair-first rule; see `spice.errors`.
"""

from __future__ import annotations

import shlex
import subprocess
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import atomic_write_text
from spice.studies.suiteseam import run_suite_seam_gate
from spice.tasks import config, identity
from spice.tasks.git import plumbing


MERGE_STATE_FILES = ("ORIG_HEAD", "MERGE_MODE", "MERGE_MSG", "MERGE_HEAD")


def parse_merge_tree_output(output: str) -> tuple[str, list[str]]:
    fields = output.split("\0")
    tree = fields[0].strip() if fields else ""
    return tree, [field for field in fields[1:] if field]


def materialize_merge_conflict(
    repo_root: Path,
    *,
    merged_tree: str,
    conflict_records: list[str],
    agent_head: str,
    upstream_head: str,
    message: str,
) -> None:
    """Install merge-tree's result as a complete, recoverable merge state.

    MERGE_HEAD is written last. Until then every failure rolls the index and
    working tree back to ``agent_head``; after it exists, the marker blobs,
    higher index stages, and both parents are all present. Bytecode cleanup
    after each tree move is best-effort and never raises, so an undeletable
    cache cannot interrupt installing or rolling back the merge state.
    """
    if not merged_tree or not conflict_records:
        raise SpiceError("conflicted merge-tree result was incomplete")
    parsed: list[tuple[str, str]] = []
    for record in conflict_records:
        metadata, separator, path = record.partition("\t")
        if not separator or len(metadata.split()) != 3 or not path:
            raise SpiceError("conflicted merge-tree index record was malformed")
        parsed.append((metadata, path))

    try:
        previous_state = _snapshot_merge_state(repo_root)
    except OSError as error:
        raise SpiceError(f"could not snapshot pre-merge state: {error}") from error
    try:
        materialize = plumbing.run(repo_root, "read-tree", "--reset", "-u", merged_tree)
        if materialize.returncode != 0:
            raise SpiceError(
                plumbing.fail("materialize conflicted merge tree", materialize)
            )
        plumbing.purge_stale_bytecode(repo_root, agent_head, merged_tree)
        for path in sorted({path for _, path in parsed}):
            removed = plumbing.run(
                repo_root, "update-index", "--force-remove", "--", path
            )
            if removed.returncode != 0:
                raise SpiceError(
                    plumbing.fail(f"prepare conflict index for {path}", removed)
                )
        index_info = "".join(f"{metadata}\t{path}\0" for metadata, path in parsed)
        staged = plumbing.run_with_input(
            repo_root, "update-index", "-z", "--index-info", input_text=index_info
        )
        if staged.returncode != 0:
            raise SpiceError(plumbing.fail("install conflict index stages", staged))

        _write_git_state(repo_root, "ORIG_HEAD", f"{agent_head}\n")
        _write_git_state(repo_root, "MERGE_MODE", "no-ff\n")
        _write_git_state(repo_root, "MERGE_MSG", f"{message}\n")
        _write_git_state(repo_root, "MERGE_HEAD", f"{upstream_head}\n")
    except (OSError, SpiceError) as error:
        state_error: OSError | None = None
        try:
            _restore_merge_state(previous_state)
        except OSError as restore_error:
            state_error = restore_error
        restored = plumbing.run(repo_root, "read-tree", "--reset", "-u", agent_head)
        if restored.returncode != 0:
            raise SpiceError(plumbing.fail("restore pre-merge tree", restored))
        plumbing.purge_stale_bytecode(repo_root, merged_tree, agent_head)
        if state_error is not None:
            raise SpiceError(
                f"could not restore pre-merge metadata: {state_error}"
            ) from state_error
        if isinstance(error, SpiceError):
            raise
        raise SpiceError(
            f"could not install recoverable merge state: {error}"
        ) from error


def _git_state_path(repo_root: Path, name: str) -> Path:
    value = plumbing.read(repo_root, "rev-parse", "--git-path", name)
    if not value:
        raise SpiceError(f"could not resolve git state path for {name}")
    path = Path(value)
    return path if path.is_absolute() else repo_root / path


def _write_git_state(repo_root: Path, name: str, content: str) -> None:
    atomic_write_text(_git_state_path(repo_root, name), content)


def _snapshot_merge_state(repo_root: Path) -> dict[Path, bytes | None]:
    snapshot: dict[Path, bytes | None] = {}
    for name in MERGE_STATE_FILES:
        path = _git_state_path(repo_root, name)
        snapshot[path] = path.read_bytes() if path.exists() else None
    return snapshot


def _restore_merge_state(snapshot: dict[Path, bytes | None]) -> None:
    """Restore Git-owned binary metadata exactly during transaction rollback."""
    for path, content in snapshot.items():
        if content is None:
            path.unlink(missing_ok=True)
        else:
            path.write_bytes(content)


def tree_of(repo_root: Path, ref: str) -> str:
    return plumbing.read(repo_root, "rev-parse", f"{ref}^{{tree}}")


def _collapse_to_first_parent(repo_root: Path, first_parent: str, *, label: str) -> str:
    """Fast-forward the branch to a tree-equivalent descendant baseline."""
    expected_head = plumbing.read(repo_root, "rev-parse", "HEAD")
    _materialize_and_update_head(
        repo_root,
        new_head=first_parent,
        expected_head=expected_head,
        label=label,
        action="fast-forward tree-same phase onto baseline",
    )
    return first_parent


def synthesize_and_fast_forward(
    repo_root: Path,
    treeish: str,
    first_parent: str,
    second_parent: str,
    message: str,
    *,
    label: str,
) -> str:
    # A tree-equal descendant baseline already contains the phase commits, so a
    # fast-forward avoids an empty merge. Divergent tree-equal histories are
    # different: resetting would abandon the local commit line (and can be
    # blocked by the reference guard), so preserve both lines in an ancestry
    # merge even though the resulting tree itself is unchanged.
    if tree_of(repo_root, treeish) == tree_of(
        repo_root, first_parent
    ) and plumbing.is_ancestor(repo_root, "HEAD", first_parent):
        return _collapse_to_first_parent(repo_root, first_parent, label=label)
    merge_head = _synthesize_merge(
        repo_root, treeish, first_parent, second_parent, message
    )
    expected_head = plumbing.read(repo_root, "rev-parse", "HEAD")
    _materialize_and_update_head(
        repo_root,
        new_head=merge_head,
        expected_head=expected_head,
        label=label,
        action="advance branch to merge commit",
    )
    return merge_head


def _materialize_and_update_head(
    repo_root: Path,
    *,
    new_head: str,
    expected_head: str,
    label: str,
    action: str,
) -> None:
    """Advance checked-out HEAD without exposing a half-applied ref update.

    The complete target tree reaches the index and working tree before the
    branch compare-and-swap. If that ref transaction fails, read-tree restores
    the actual current HEAD without invoking another ref hook. Thus command
    success has the target tree and parent record together, while every
    handled failure returns to a clean pre-transaction commit. Bytecode
    cleanup along the way is best-effort and never raises, so an undeletable
    cache cannot strand the transaction between those two outcomes.
    """
    current_ref = plumbing.read(repo_root, "symbolic-ref", "--quiet", "HEAD")
    if not current_ref:
        raise SpiceError(f"cannot {action}: HEAD is detached")
    materialize = plumbing.run(repo_root, "read-tree", "--reset", "-u", new_head)
    if materialize.returncode != 0:
        restored = plumbing.run(repo_root, "read-tree", "--reset", "-u", expected_head)
        if restored.returncode != 0:
            raise SpiceError(
                plumbing.fail(f"restore tree after failed {action}", restored)
            )
        plumbing.purge_stale_bytecode(repo_root, expected_head, new_head)
        raise SpiceError(plumbing.fail(action, materialize))
    plumbing.purge_stale_bytecode(repo_root, expected_head, new_head)

    update = plumbing.run(repo_root, "update-ref", current_ref, new_head, expected_head)
    current_head = plumbing.read(repo_root, "rev-parse", "HEAD")
    if update.returncode == 0 or current_head == new_head:
        return

    restore_head = current_head or expected_head
    restored = plumbing.run(repo_root, "read-tree", "--reset", "-u", restore_head)
    if restored.returncode != 0:
        raise SpiceError(plumbing.fail(f"restore tree after failed {action}", restored))
    plumbing.purge_stale_bytecode(repo_root, new_head, restore_head)
    if _is_head_ref_lock_race(update):
        raise SpiceError(
            _head_ref_lock_race_recovery(
                label,
                expected_head=expected_head,
                current_head=current_head,
                completed=update,
            )
        )
    raise SpiceError(plumbing.fail(action, update))


def _is_head_ref_lock_race(completed: subprocess.CompletedProcess[str]) -> bool:
    output = (completed.stdout + "\n" + completed.stderr).lower()
    return (
        "cannot lock ref" in output
        and " is at " in output
        and " but expected " in output
    )


def _head_ref_lock_race_recovery(
    label: str,
    *,
    expected_head: str,
    current_head: str,
    completed: subprocess.CompletedProcess[str],
) -> str:
    lines = [
        "HEAD moved while spice was advancing the generated task commit; "
        "task state was not advanced",
        "spice did not intentionally change the index or working tree after "
        "Git reported the ref-lock race; inspect the preserved state and retry "
        "from current HEAD",
        "next commands:",
        "  git status --short",
        "  git rev-parse HEAD",
        f'  spice task done {label} --validation "..."',
    ]
    if expected_head:
        lines.append(f"expected_head={expected_head}")
    if current_head:
        lines.append(f"current_head={current_head}")
    lines.extend(
        ["git output:", plumbing.fail("advance branch to merge commit", completed)]
    )
    return "\n".join(lines)


def conflict_marker_paths(repo_root: Path, baseline: str, treeish: str) -> list[str]:
    """Changed files in ``treeish`` that still carry leftover conflict markers.

    A file is flagged only when it contains both an opening ``<<<<<<<`` and a
    closing ``>>>>>>>`` line, so documents that legitimately use a bare
    ``=======`` underline never trip the guard.
    """
    changed = [
        line
        for line in plumbing.read(
            repo_root, "diff", "--name-only", baseline, treeish
        ).splitlines()
        if line
    ]
    if not changed:
        return []

    def marked(pattern: str) -> set[str]:
        listing = plumbing.read(
            repo_root, "grep", "-l", "-E", pattern, treeish, "--", *changed
        )
        return {line.split(":", 1)[1] for line in listing.splitlines() if ":" in line}

    return sorted(marked(r"^<{7}( |$)") & marked(r"^>{7}( |$)"))


def conflict_marker_refusal(label: str, paths: list[str]) -> str:
    joined = " ".join(paths)
    lines = [
        "refusing to publish: committed files still contain conflict markers:",
        *(f"  {path}" for path in paths),
        "next commands:",
        "  edit the files above and remove every leftover marker line",
        f"  git add -- {joined}",
        "  git commit --amend --no-edit",
        f'  spice task done {label} --validation "..."',
    ]
    return "\n".join(lines)


def _task_footprint_paths(
    repo_root: Path, upstream_head: str, agent_head: str
) -> set[str]:
    """Paths touched by the lane's own non-merge commits since the baseline.

    Merges are excluded on purpose: an overlap-resolution merge makes the
    baseline an ancestor of the lane head, and its combined diff would
    dissolve the footprint into every path the resolution happened to touch.
    """
    listing = plumbing.read(
        repo_root,
        "log",
        "--no-renames",
        "--no-merges",
        "--format=",
        "--name-only",
        f"{upstream_head}..{agent_head}",
    )
    return {line for line in listing.splitlines() if line}


def refuse_red_suite_landing(
    repo_root: Path, *, label: str, upstream_head: str, agent_head: str
) -> None:
    """Refuse a landing that reaches the whole suite while the whole suite is red.

    HEAD already holds the integrated tree, so this is the one moment the
    branch's real content can be tested before it becomes the branch. A lane's
    own tree cannot stand in for it: the lane verified a subset of the tests,
    against a baseline the other lanes have since moved.
    """
    run_suite_seam_gate(
        repo_root,
        sorted(_task_footprint_paths(repo_root, upstream_head, agent_head)),
        label=label,
    )


def refuse_out_of_scope_landing(
    repo_root: Path,
    *,
    label: str,
    upstream_head: str,
    merge_head: str,
    agent_head: str,
) -> None:
    """Refuse a landing whose first-parent diff leaves the task's footprint.

    Every path the landing changes against the baseline must come from one of
    the task's own commits; anything else is peer work on the shared branch
    that a hand-made overlap resolution would silently overwrite.
    """
    landed = {
        line
        for line in plumbing.read(
            repo_root,
            "diff",
            "--no-renames",
            "--name-only",
            upstream_head,
            merge_head,
        ).splitlines()
        if line
    }
    footprint = _task_footprint_paths(repo_root, upstream_head, agent_head)
    drifted = sorted(landed - footprint)
    if drifted:
        raise SpiceError(
            _out_of_scope_refusal(repo_root, label, upstream_head, drifted)
        )


def _out_of_scope_refusal(
    repo_root: Path, label: str, upstream_head: str, paths: list[str]
) -> str:
    present = [
        path for path in paths if _path_exists_at(repo_root, upstream_head, path)
    ]
    present_set = set(present)
    absent = [path for path in paths if path not in present_set]
    lines = [
        "refusing to publish: the landing rewrites baseline paths outside "
        f"the commits of {label}:",
        *(f"  {path}" for path in paths),
        "these paths carry peer work already landed on the shared branch; "
        "publishing would silently overwrite it",
        "next commands:",
    ]
    if present:
        lines.append(f"  git checkout {upstream_head} -- {_shell_join(present)}")
    if absent:
        lines.append(f"  git rm -- {_shell_join(absent)}")
    lines.extend(
        [
            f'  git commit -m "Restore baseline content for {label}"',
            f'  spice task done {label} --validation "..."',
            "an intentional change to any path above must land as its own commit "
            "on this branch so the task owns it; then rerun spice task done",
        ]
    )
    return "\n".join(lines)


def _path_exists_at(repo_root: Path, treeish: str, path: str) -> bool:
    return (
        plumbing.run(repo_root, "cat-file", "-e", f"{treeish}:{path}").returncode == 0
    )


def publish_race_recovery(
    label: str,
    remote: str,
    baseline: str,
    completed: subprocess.CompletedProcess[str],
) -> str:
    lines = [
        f"{baseline} advanced while publishing task work; the task state was "
        "not advanced",
        "next commands:",
        f"  git fetch {remote}",
        f"  git merge {baseline}",
        f'  spice task done {label} --validation "..."',
        "git push output:",
        plumbing.fail(f"publish task work to {baseline}", completed),
    ]
    return "\n".join(lines)


def merge_conflict_recovery(
    label: str, repo_root: Path, merged_upstream_head: str
) -> str:
    conflicts = _conflict_paths(repo_root)
    if not plumbing.read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
        return _merge_conflict_marker_recovery(
            label, repo_root, conflicts, merged_upstream_head
        )
    lines = [
        "your changes overlap with the current baseline; git is paused in a "
        "merge state",
    ]
    if conflicts:
        lines.append("conflicting files:")
        lines.extend(f"  {path}" for path in conflicts)
    else:
        lines.append("conflicting files: run `git status --short`")
    add_paths = _shell_join(conflicts) if conflicts else "<files>"
    lines.extend(
        [
            "keep the merge state open; do not run `git merge --abort`",
            "baseline-side hunks are peer work already landed on the shared "
            "branch; fold them into the resolution — never keep only this "
            "task's side",
            "commit while MERGE_HEAD exists so the baseline becomes a parent",
            "next commands:",
            "  git status --short",
            "  git rev-parse --verify MERGE_HEAD",
            "  edit the conflicting files above",
            f"  git add -- {add_paths}",
            f'  git commit -m "Resolve baseline overlap for {label}"',
            f'  spice task done {label} --validation "..."',
        ]
    )
    return "\n".join(lines)


def _merge_conflict_marker_recovery(
    label: str,
    repo_root: Path,
    conflicts: list[str],
    merged_upstream_head: str,
) -> str:
    marker_paths = _working_tree_conflict_marker_paths(repo_root)
    paths = conflicts or marker_paths
    lines = [
        "your changes overlap with the current baseline; git left conflict "
        "markers without an open MERGE_HEAD",
    ]
    if paths:
        lines.append("conflict-marker files:")
        lines.extend(f"  {path}" for path in paths)
    else:
        lines.append("conflict-marker files: run `git status --short`")
    lines.extend(
        [
            "do not use plain `git commit`; no MERGE_HEAD exists to supply the "
            "baseline parent",
            "baseline-side hunks are peer work already landed on the shared "
            "branch; fold them into the resolution — never keep only this "
            "task's side",
            "with no MERGE_HEAD the auto-merged peer changes were never staged, "
            "so stage the whole merged tree — not only the conflict — or "
            "`git write-tree` records only this task's side and drops them",
            "next commands:",
            "  git status --short",
            "  git rev-parse --verify MERGE_HEAD  # expected to fail here",
            "  edit the files above and remove every marker line",
            "  git add -A  # stage every merged path, peer changes included",
            "  merge_commit=$(git commit-tree $(git write-tree) "
            f"-p HEAD -p {merged_upstream_head} "
            f'-m "Resolve baseline overlap for {label}")',
            '  git update-ref refs/heads/$(git branch --show-current) "$merge_commit"',
            f'  spice task done {label} --validation "..."',
        ]
    )
    return "\n".join(lines)


def _conflict_paths(repo_root: Path) -> list[str]:
    output = plumbing.read(repo_root, "diff", "--name-only", "--diff-filter=U")
    return [line for line in output.splitlines() if line]


def _working_tree_conflict_marker_paths(repo_root: Path) -> list[str]:
    changed: set[str] = set(_conflict_paths(repo_root))
    for args in (("diff", "--name-only"), ("diff", "--cached", "--name-only")):
        changed.update(
            line for line in plumbing.read(repo_root, *args).splitlines() if line
        )
    if not changed:
        return []

    def marked(pattern: str) -> set[str]:
        listing = plumbing.read(
            repo_root, "grep", "-l", "-E", pattern, "--", *sorted(changed)
        )
        return {line for line in listing.splitlines() if line}

    return sorted(marked(r"^<{7}( |$)") & marked(r"^>{7}( |$)"))


def _shell_join(values: list[str]) -> str:
    return shlex.join(values)


def _synthesize_merge(
    repo_root: Path,
    treeish: str,
    first_parent: str,
    second_parent: str,
    message: str,
) -> str:
    """A uniform merge commit carrying ``treeish`` with explicit parent order."""
    tree = tree_of(repo_root, treeish)
    completed = plumbing.run(
        repo_root,
        "commit-tree",
        tree,
        "-p",
        first_parent,
        "-p",
        second_parent,
        "-m",
        message,
    )
    if completed.returncode != 0 or not completed.stdout.strip():
        raise SpiceError(plumbing.fail("synthesize merge commit", completed))
    return completed.stdout.strip()


def capture(
    agent_head: str,
    merge_head: str,
    upstream: str,
    upstream_head: str,
    local_commits: int,
) -> list[str]:
    args = [
        f"done_head:{agent_head}",
        f"done_merge_head:{merge_head}",
        f"done_ref:{merge_head}",
        f"done_local_commits:{local_commits}",
    ]
    if upstream:
        args.append(f"done_upstream:{upstream}")
    if upstream_head:
        args.append(f"done_upstream_head:{upstream_head}")
    return args


def compose_message(label: str, meta: dict[str, str] | None) -> str:
    """Build a terse merge message from task facts.

    The subject is a freeform, lossy projection of the task — ``<project-stem>:
    <title> <handle>`` for the implied todo phase, with `` (<phase>)`` appended
    for every other phase — and an empty free-text body. The sorted ``Task-*``
    trailers (git-trailer parseable) are the canonical record, with
    ``Task-Key`` carrying the stable incepted key. The agent never reads this;
    it lives on the shared baseline for review.
    """
    meta = meta or {}
    project = (meta.get("project") or "").strip()
    phase = (meta.get("phase") or "").strip()
    title = (meta.get("title") or "").strip()
    prefix = f"{config.project_stem(project)}: " if project else ""
    subject = " ".join(part for part in (title, label) if part)
    phase_suffix = f" ({phase})" if phase and phase.casefold() != "todo" else ""
    lines = [f"{prefix}{subject}{phase_suffix}"]

    try:
        incepted: str | None = identity.incepted_of_handle(label)
    except SpiceError:
        incepted = None
    structured = [
        (key, value)
        for key, value in (
            ("Task-Key", incepted),
            ("Task-Phase", phase),
            ("Task-Project", project),
            ("Task-Session", meta.get("actor")),
        )
        if value
    ]
    trailers = [f"{key}: {value}" for key, value in sorted(structured)]
    if trailers:
        lines += ["", *trailers]
    return "\n".join(lines)
