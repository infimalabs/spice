"""Git integration bound to task boundaries — invisible to the agent.

Agents never pull and never push directly. Git is touched in exactly two
places, both owned by the task control plane:

* **claim** (`prepare_for_claim`): fast-forward the local tree to the current
  baseline so new work starts from the latest shared state, then the claim
  records that point-in-time commit.
* **phase completion** (`integrate_and_publish`): merge the completing
  agent's work with the baseline and publish a baseline-first merge, then
  record the agent commit and the always-present merge commit. A real content
  conflict is the one and only thing surfaced to the agent — framed as an
  overlap with the baseline, never as a sync with an upstream.

The default baseline is the current branch's real upstream (for example
``origin/main``). When no remote exists (local-only trees, or test harnesses)
every operation degrades to a safe no-op that still records the local HEAD,
so the captured review record holds without a remote.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from spice.errors import SpiceError
from spice.tasks import config

GIT_NETWORK_TIMEOUT_SECONDS = 30
TASK_GIT_SSH_COMMAND = "ssh -o BatchMode=yes -o ConnectTimeout=5"
_NETWORK_COMMANDS = {"fetch", "push"}


class MergeConflict(SpiceError):
    """A real content conflict the agent must resolve before the phase closes."""


@dataclass
class SyncResult:
    notes: list[str] = field(default_factory=list)
    uda_args: list[str] = field(default_factory=list)


def _run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = _control_plane_git_env()
    command = ["git", "-C", str(repo_root), *args]
    kwargs = {
        "capture_output": True,
        "check": False,
        "env": env,
        "text": True,
    }
    if args and args[0] in _NETWORK_COMMANDS:
        kwargs["timeout"] = GIT_NETWORK_TIMEOUT_SECONDS
    try:
        return subprocess.run(command, **kwargs)
    except subprocess.TimeoutExpired as exc:
        return subprocess.CompletedProcess(
            command,
            124,
            stdout=_timeout_text(exc.stdout),
            stderr=(
                _timeout_text(exc.stderr)
                + f"git {args[0]} timed out after {GIT_NETWORK_TIMEOUT_SECONDS}s\n"
            ),
        )


def _control_plane_git_env() -> dict[str, str]:
    env = dict(os.environ)
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GIT_SSH_COMMAND"] = TASK_GIT_SSH_COMMAND
    return env


def _timeout_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def _read(repo_root: Path, *args: str) -> str:
    completed = _run(repo_root, *args)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def _resolve_target(repo_root: Path) -> tuple[str, str] | None:
    """Return ``(remote, baseline_ref)`` for this worktree's task baseline,
    or ``None`` when the configured remote is absent (local-only).

    The current branch upstream is authoritative. Missing upstream in a
    remote-backed worktree is a setup error.
    """
    upstream = branch_upstream_target(repo_root)
    if upstream is not None:
        return upstream
    if not _read(repo_root, "remote"):
        return None
    raise SpiceError(
        "cannot resolve task baseline: current branch has no upstream; "
        "run `spice dev install-hooks` to configure branch tracking"
    )


def branch_upstream_target(repo_root: Path) -> tuple[str, str] | None:
    # The lane's own tracking (branch.<lane>.merge) is the single source of
    # truth — and it stays readable under the agent shadow: the shadow's
    # self-merge lives in *system* scope, so `git config --get` returns the
    # native *worktree* value (the real upstream branch). The remote is `origin`
    # by convention (branch.<lane>.remote is poisoned to `.` by the shadow's
    # command-scope pair, so it cannot be trusted). origin/HEAD is only a
    # backstop when the lane has no tracking configured.
    if _run(repo_root, "remote", "get-url", "origin").returncode != 0:
        return None
    branch = _read(repo_root, "symbolic-ref", "--quiet", "--short", "HEAD")
    prefix = "refs/heads/"
    merge = (
        _read(repo_root, "config", "--get", f"branch.{branch}.merge") if branch else ""
    )
    if merge.startswith(prefix):
        return "origin", f"origin/{merge[len(prefix) :]}"
    return _origin_head_backstop_target(repo_root)


def _origin_head_backstop_target(repo_root: Path) -> tuple[str, str]:
    head_ref = _read(repo_root, "symbolic-ref", "refs/remotes/origin/HEAD")
    prefix = "refs/remotes/"
    if not head_ref.startswith(prefix):
        raise SpiceError(
            "the lane has no branch.<lane>.merge and origin/HEAD is unset; run "
            "`git remote set-head origin --auto` or configure branch tracking so "
            "the task baseline can resolve the integration branch"
        )
    return "origin", head_ref[len(prefix) :]


def _is_ancestor(repo_root: Path, ancestor: str, descendant: str) -> bool:
    return (
        _run(repo_root, "merge-base", "--is-ancestor", ancestor, descendant).returncode
        == 0
    )


def _parents(repo_root: Path, commit: str) -> list[str]:
    line = _read(repo_root, "rev-list", "--parents", "-n", "1", commit)
    parts = line.split()
    return parts[1:]


def _is_merge_with_first_parent(repo_root: Path, commit: str, parent: str) -> bool:
    """True when ``commit`` is a merge with ``parent`` as its mainline."""
    parents = _parents(repo_root, commit)
    return len(parents) >= 2 and parents[0] == parent


def _worktree_dirty(repo_root: Path) -> bool:
    return _read(repo_root, "status", "--porcelain") != ""


def commits_ahead_of_baseline(repo_root: Path | None = None) -> int:
    """Count local commits ahead of the task baseline.

    This is exactly the quantity ``prepare_for_claim`` refuses to start new
    work over: commits on HEAD not yet recorded by a completed task. With no
    configured remote there is no baseline to be ahead of, so the count is 0.
    """
    root = repo_root or config.repo_root()
    resolved = _resolve_target(root)
    if resolved is None:
        return 0
    _, baseline = resolved
    ahead = _read(root, "rev-list", "--count", f"{baseline}..HEAD")
    try:
        return int(ahead)
    except ValueError:
        return 0


def prepare_for_claim(repo_root: Path | None = None) -> SyncResult:
    """Fast-forward-only update to the current baseline before a claim records
    HEAD.

    Requires a clean tree with zero commits ahead of the baseline; anything
    else is an anomaly we refuse rather than paper over. With no configured
    remote this is a no-op and the claim simply records the local HEAD.
    """
    root = repo_root or config.repo_root()
    resolved = _resolve_target(root)
    if resolved is None:
        return SyncResult()
    remote, baseline = resolved
    if _worktree_dirty(root):
        raise SpiceError(
            "cannot start new work: commit or clear the working tree first"
        )
    ahead = _read(root, "rev-list", "--count", f"{baseline}..HEAD")
    if ahead and ahead != "0":
        raise SpiceError(
            f"cannot start new work: the branch has {ahead} local commit(s) "
            "not yet recorded by a completed task; capture or clear them first"
        )
    before = _read(root, "rev-parse", "HEAD")
    _run(root, "fetch", remote)
    if not _read(root, "rev-parse", baseline):
        raise SpiceError(f"baseline {baseline} not found on remote {remote}")
    completed = _run(root, "merge", "--ff-only", baseline)
    if completed.returncode != 0:
        raise SpiceError(
            "cannot start new work: the working tree could not be brought to the "
            "current baseline cleanly; resolve local git state first"
        )
    after = _read(root, "rev-parse", "HEAD")
    notes = ["updated working tree to the current baseline"] if after != before else []
    return SyncResult(notes=notes)


def fast_forward_if_safe(repo_root: Path | None = None) -> SyncResult:
    """Bring the tree up to the current baseline when, and only when, it is
    safe.

    Lenient sibling of :func:`prepare_for_claim` for activation: it applies
    the same rules (clean tree, zero commits ahead, fast-forward-only) but
    never raises — any reason it cannot fast-forward (dirty, diverged, no
    remote, unresolved target) is simply a silent no-op, so activation always
    succeeds.
    """
    root = repo_root or config.repo_root()
    try:
        resolved = _resolve_target(root)
    except SpiceError:
        return SyncResult()
    if resolved is None:
        return SyncResult()
    remote, baseline = resolved
    if _worktree_dirty(root):
        return SyncResult()
    ahead = _read(root, "rev-list", "--count", f"{baseline}..HEAD")
    if ahead and ahead != "0":
        return SyncResult()
    before = _read(root, "rev-parse", "HEAD")
    _run(root, "fetch", remote)
    if not _read(root, "rev-parse", baseline):
        return SyncResult()
    if _run(root, "merge", "--ff-only", baseline).returncode != 0:
        return SyncResult()
    after = _read(root, "rev-parse", "HEAD")
    notes = ["updated working tree to the current baseline"] if after != before else []
    return SyncResult(notes=notes)


def integrate_and_publish(
    label: str,
    repo_root: Path | None = None,
    *,
    meta: dict[str, str] | None = None,
) -> SyncResult:
    """Integrate the completing agent's work with the baseline and publish it.

    Always lands a merge commit with the baseline as first parent and the
    agent's last commit as second parent (``--no-ff`` semantics), captures
    both for review, and pushes. The merge commit message is composed from
    harvested task and git facts. A real content conflict raises
    :class:`MergeConflict` with the tree left mid-merge for the agent to
    resolve and commit. A resolution that still contains conflict markers is
    refused before anything publishes. With no configured remote this records
    the local HEAD and performs no network or history mutation.
    """
    root = repo_root or config.repo_root()
    agent_head = _read(root, "rev-parse", "HEAD")
    resolved = _resolve_target(root)
    if resolved is None:
        return SyncResult(uda_args=_capture(agent_head, agent_head, "", ""))
    remote, baseline = resolved

    upstream_head = _fetch_upstream_head(root, remote, baseline)
    if agent_head == upstream_head:
        # Nothing to integrate; the baseline already holds this state.
        return SyncResult(
            uda_args=_capture(agent_head, agent_head, baseline, upstream_head)
        )

    message = _compose_message(label, meta)
    merge_head = _integrate_task_work(
        root,
        baseline=baseline,
        label=label,
        agent_head=agent_head,
        upstream_head=upstream_head,
        message=message,
    )
    merge_head, upstream_head = _publish_integrated_task(
        root,
        remote=remote,
        baseline=baseline,
        label=label,
        merge_head=merge_head,
        upstream_head=upstream_head,
        message=message,
    )
    return SyncResult(
        uda_args=_capture(agent_head, merge_head, baseline, upstream_head)
    )


def _fetch_upstream_head(repo_root: Path, remote: str, baseline: str) -> str:
    _run(repo_root, "fetch", remote)
    upstream_head = _read(repo_root, "rev-parse", baseline)
    if not upstream_head:
        raise SpiceError(f"baseline {baseline} not found on remote {remote}")
    return upstream_head


def _integrate_task_work(
    repo_root: Path,
    *,
    baseline: str,
    label: str,
    agent_head: str,
    upstream_head: str,
    message: str,
) -> str:
    if _is_ancestor(repo_root, upstream_head, "HEAD"):
        return _integrate_already_contains_baseline(
            repo_root, label, agent_head, upstream_head, message
        )
    return _integrate_advanced_baseline(
        repo_root,
        baseline=baseline,
        label=label,
        agent_head=agent_head,
        upstream_head=upstream_head,
        message=message,
    )


def _integrate_already_contains_baseline(
    repo_root: Path, label: str, agent_head: str, upstream_head: str, message: str
) -> str:
    # The baseline contributes no new tree content, but first-parent history
    # still needs the baseline as mainline for generated merges.
    if _is_merge_with_first_parent(repo_root, "HEAD", upstream_head):
        return agent_head
    return _synthesize_and_fast_forward(
        repo_root, agent_head, upstream_head, agent_head, message, label=label
    )


def _integrate_advanced_baseline(
    repo_root: Path,
    *,
    baseline: str,
    label: str,
    agent_head: str,
    upstream_head: str,
    message: str,
) -> str:
    # Merge into the index without committing, then synthesize the generated
    # task merge with the baseline as mainline. A real conflict stays in the
    # worktree for the agent to resolve; retrying wraps that resolution in the
    # same generated merge shape.
    merge = _run(repo_root, "merge", "--no-ff", "--no-commit", "-m", message, baseline)
    if merge.returncode != 0:
        raise MergeConflict(_merge_conflict_recovery(label, repo_root))
    merged_tree = _read(repo_root, "write-tree")
    if not merged_tree:
        raise SpiceError("could not write merged tree")
    _clear_temporary_merge_state(repo_root, action="clear merge state")
    return _synthesize_and_fast_forward(
        repo_root, merged_tree, upstream_head, agent_head, message, label=label
    )


def _publish_integrated_task(
    repo_root: Path,
    *,
    remote: str,
    baseline: str,
    label: str,
    merge_head: str,
    upstream_head: str,
    message: str,
) -> tuple[str, str]:
    flagged = _conflict_marker_paths(repo_root, upstream_head, merge_head)
    if flagged:
        raise SpiceError(_conflict_marker_refusal(label, flagged))
    branch = baseline.split("/", 1)[1]
    return _publish_task_merge(
        repo_root,
        remote=remote,
        baseline=baseline,
        branch=branch,
        label=label,
        merge_head=merge_head,
        upstream_head=upstream_head,
        message=message,
    )


def _publish_task_merge(
    repo_root: Path,
    *,
    remote: str,
    baseline: str,
    branch: str,
    label: str,
    merge_head: str,
    upstream_head: str,
    message: str,
) -> tuple[str, str]:
    push = _run(repo_root, "push", remote, f"{merge_head}:{branch}")
    if push.returncode == 0:
        return merge_head, upstream_head
    if not _is_non_fast_forward_push(push):
        raise SpiceError(_fail(f"publish task work to {baseline}", push))
    return _retry_publish_after_race(
        repo_root,
        remote=remote,
        baseline=baseline,
        branch=branch,
        label=label,
        merge_head=merge_head,
        previous_upstream_head=upstream_head,
        message=message,
        first_push=push,
    )


def _retry_publish_after_race(
    repo_root: Path,
    *,
    remote: str,
    baseline: str,
    branch: str,
    label: str,
    merge_head: str,
    previous_upstream_head: str,
    message: str,
    first_push: subprocess.CompletedProcess[str],
) -> tuple[str, str]:
    fetch = _run(repo_root, "fetch", remote)
    if fetch.returncode != 0:
        raise SpiceError(_publish_race_recovery(label, remote, baseline, first_push))
    fresh_upstream_head = _read(repo_root, "rev-parse", baseline)
    if not fresh_upstream_head or fresh_upstream_head == previous_upstream_head:
        raise SpiceError(_publish_race_recovery(label, remote, baseline, first_push))
    if fresh_upstream_head == merge_head:
        return merge_head, fresh_upstream_head

    merge = _run(repo_root, "merge", "--no-ff", "--no-commit", "-m", message, baseline)
    if merge.returncode != 0:
        raise MergeConflict(_merge_conflict_recovery(label, repo_root))
    merged_tree = _read(repo_root, "write-tree")
    if not merged_tree:
        raise SpiceError("could not write publish-race merged tree")
    _clear_temporary_merge_state(repo_root, action="clear publish-race merge state")
    retry_head = _synthesize_and_fast_forward(
        repo_root,
        merged_tree,
        fresh_upstream_head,
        merge_head,
        message,
        label=label,
    )
    flagged = _conflict_marker_paths(repo_root, fresh_upstream_head, retry_head)
    if flagged:
        raise SpiceError(_conflict_marker_refusal(label, flagged))
    retry_push = _run(repo_root, "push", remote, f"{retry_head}:{branch}")
    if retry_push.returncode != 0:
        if _is_non_fast_forward_push(retry_push):
            raise SpiceError(
                _publish_race_recovery(label, remote, baseline, retry_push)
            )
        raise SpiceError(_fail(f"publish task work to {baseline}", retry_push))
    return retry_head, fresh_upstream_head


def _is_non_fast_forward_push(completed: subprocess.CompletedProcess[str]) -> bool:
    output = (completed.stdout + "\n" + completed.stderr).lower()
    return (
        "non-fast-forward" in output
        or "fetch first" in output
        or "stale info" in output
    )


def _clear_temporary_merge_state(repo_root: Path, *, action: str) -> None:
    abort = _run(repo_root, "merge", "--abort")
    if abort.returncode == 0:
        return
    if _read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
        raise SpiceError(_fail(action, abort))
    reset = _run(repo_root, "reset", "--hard", "HEAD")
    if reset.returncode != 0:
        raise SpiceError(_fail(f"{action} after missing MERGE_HEAD", reset))


def _synthesize_and_fast_forward(
    repo_root: Path,
    treeish: str,
    first_parent: str,
    second_parent: str,
    message: str,
    *,
    label: str,
) -> str:
    merge_head = _synthesize_merge(
        repo_root, treeish, first_parent, second_parent, message
    )
    expected_head = _read(repo_root, "rev-parse", "HEAD")
    ff = _run(repo_root, "merge", "--ff-only", merge_head)
    if ff.returncode != 0:
        if _is_head_ref_lock_race(ff):
            raise SpiceError(
                _head_ref_lock_race_recovery(
                    label,
                    expected_head=expected_head,
                    current_head=_read(repo_root, "rev-parse", "HEAD"),
                    completed=ff,
                )
            )
        raise SpiceError(_fail("advance branch to merge commit", ff))
    return merge_head


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
    lines.extend(["git output:", _fail("advance branch to merge commit", completed)])
    return "\n".join(lines)


def _conflict_marker_paths(repo_root: Path, baseline: str, treeish: str) -> list[str]:
    """Changed files in ``treeish`` that still carry leftover conflict markers.

    A file is flagged only when it contains both an opening ``<<<<<<<`` and a
    closing ``>>>>>>>`` line, so documents that legitimately use a bare
    ``=======`` underline never trip the guard.
    """
    changed = [
        line
        for line in _read(
            repo_root, "diff", "--name-only", baseline, treeish
        ).splitlines()
        if line
    ]
    if not changed:
        return []

    def marked(pattern: str) -> set[str]:
        listing = _read(repo_root, "grep", "-l", "-E", pattern, treeish, "--", *changed)
        return {line.split(":", 1)[1] for line in listing.splitlines() if ":" in line}

    return sorted(marked(r"^<{7}( |$)") & marked(r"^>{7}( |$)"))


def _conflict_marker_refusal(label: str, paths: list[str]) -> str:
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


def _publish_race_recovery(
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
        _fail(f"publish task work to {baseline}", completed),
    ]
    return "\n".join(lines)


def _merge_conflict_recovery(label: str, repo_root: Path) -> str:
    conflicts = _conflict_paths(repo_root)
    if not _read(repo_root, "rev-parse", "--verify", "MERGE_HEAD"):
        return _merge_conflict_marker_recovery(label, repo_root, conflicts)
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
    label: str, repo_root: Path, conflicts: list[str]
) -> str:
    marker_paths = _working_tree_conflict_marker_paths(repo_root)
    paths = conflicts or marker_paths
    _, baseline = branch_upstream_target(repo_root) or ("", "<baseline>")
    lines = [
        "your changes overlap with the current baseline; git left conflict "
        "markers without an open MERGE_HEAD",
    ]
    if paths:
        lines.append("conflict-marker files:")
        lines.extend(f"  {path}" for path in paths)
    else:
        lines.append("conflict-marker files: run `git status --short`")
    add_paths = _shell_join(paths) if paths else "<files>"
    lines.extend(
        [
            "do not use plain `git commit`; no MERGE_HEAD exists to supply the "
            "baseline parent",
            "next commands:",
            "  git status --short",
            "  git rev-parse --verify MERGE_HEAD  # expected to fail here",
            "  edit the files above and remove every marker line",
            f"  git add -- {add_paths}",
            "  merge_commit=$(git commit-tree $(git write-tree) "
            f'-p HEAD -p {baseline} -m "Resolve baseline overlap for {label}")',
            '  git update-ref refs/heads/$(git branch --show-current) "$merge_commit"',
            f'  spice task done {label} --validation "..."',
        ]
    )
    return "\n".join(lines)


def _conflict_paths(repo_root: Path) -> list[str]:
    output = _read(repo_root, "diff", "--name-only", "--diff-filter=U")
    return [line for line in output.splitlines() if line]


def _working_tree_conflict_marker_paths(repo_root: Path) -> list[str]:
    changed: set[str] = set(_conflict_paths(repo_root))
    for args in (("diff", "--name-only"), ("diff", "--cached", "--name-only")):
        changed.update(line for line in _read(repo_root, *args).splitlines() if line)
    if not changed:
        return []

    def marked(pattern: str) -> set[str]:
        listing = _read(repo_root, "grep", "-l", "-E", pattern, "--", *sorted(changed))
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
    tree = _read(repo_root, "rev-parse", f"{treeish}^{{tree}}")
    completed = _run(
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
        raise SpiceError(_fail("synthesize merge commit", completed))
    return completed.stdout.strip()


def _capture(
    agent_head: str, merge_head: str, upstream: str, upstream_head: str
) -> list[str]:
    args = [
        f"done_head:{agent_head}",
        f"done_merge_head:{merge_head}",
        f"done_ref:{merge_head}",
    ]
    if upstream:
        args.append(f"done_upstream:{upstream}")
    if upstream_head:
        args.append(f"done_upstream_head:{upstream_head}")
    return args


def _compose_message(label: str, meta: dict[str, str] | None) -> str:
    """Build a terse merge message from task facts.

    Trailers are ``Key: value`` lines (git-trailer parseable) so the
    integration record stays cheap to harvest later. The agent never reads
    this; it lives on the shared baseline for review.
    """
    meta = meta or {}
    title = (meta.get("title") or "").strip()
    subject = title if title else f"Integrate {label}"
    lines = [subject]

    trailers = [("Task", label)]
    for key, value in (
        ("Task-Session", meta.get("actor")),
        ("Task-Phase", meta.get("phase")),
        ("Task-Project", meta.get("project")),
    ):
        if value:
            trailers.append((key, value))
    lines += ["", *(f"{key}: {value}" for key, value in sorted(trailers))]
    return "\n".join(lines)


def _fail(action: str, completed: subprocess.CompletedProcess[str]) -> str:
    detail = "\n".join(
        part.strip()
        for part in (completed.stdout, completed.stderr)
        if part and part.strip()
    )
    suffix = f"\n{detail}" if detail else ""
    return f"could not {action} (git exit {completed.returncode}){suffix}"
